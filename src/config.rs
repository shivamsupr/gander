//! Single source of truth for paths, binary locations, exact model strings,
//! timeouts, sentinels, and the video-tier constants. Bin discovery is env →
//! PATH → name (no hardcoded paths or usernames).
//!
//! `Config::load()` resolves everything once per run. Per-setting precedence is
//! flag > `GANDER_*` env > (config file, M6) > built-in default.

use std::env;
use std::path::{Path, PathBuf};

// The sentinel contract every backend answer is wrapped in.
// `prompt.rs` embeds these literally; `parse.rs` slices on them.
pub const SENTINEL_BEGIN: &str = "===GANDER-BEGIN===";
pub const SENTINEL_END: &str = "===GANDER-END===";

// Exact agy model strings — a typo silently falls back to agy's default, so frozen.
pub const FLASH: &str = "Gemini 3.5 Flash (High)";
pub const PRO: &str = "Gemini 3.1 Pro (High)";
/// Chunk-prose synthesis runs on the cheapest reliable rung.
pub const SYNTH_MODEL: &str = FLASH;

// claude CLI model ids (a different namespace from agy).
pub const CLAUDE_SONNET: &str = "claude-sonnet-4-6";
pub const CLAUDE_HAIKU: &str = "claude-haiku-4-5";
pub const CLAUDE_OPUS: &str = "claude-opus-4-8";

// codex default model (Codex v0.130 reports `gpt-5.5`); we don't force `-m`.
pub const CODEX_DEFAULT_MODEL: &str = "gpt-5.5";

// Video-tier thresholds (half-open, no overlap).
pub const DIRECT_MAX_S: f64 = 30.0; // d <  DIRECT_MAX_S            -> DIRECT
pub const BATCH_MAX_S: f64 = 60.0; //  DIRECT_MAX_S <= d <= BATCH   -> SINGLE_BATCH ; d > BATCH -> CHUNKED
pub const CHUNK_LEN_S: f64 = 60.0;
pub const MAX_CHUNKS: u32 = 8;

// Frame sampling.
pub const MAX_FRAMES: u32 = 12;
pub const FRAME_LONG_EDGE: u32 = 1280;
pub const JPEG_QV: u32 = 3;

// Timeouts (seconds).
pub const PRINT_TIMEOUT_S: f64 = 300.0;
pub const CLAUDE_TIMEOUT_S: f64 = 180.0;
pub const LADDER_DEADLINE_S: f64 = 700.0;
pub const SYNTH_TIMEOUT_S: f64 = 120.0;
pub const CHECK_TIMEOUT_S: f64 = 45.0;

pub const DB_BUSY_TIMEOUT_MS: i64 = 5000;
pub const MODEL_DEFAULT: &str = "pro";

/// Resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    // binaries
    pub agy_bin: String,
    pub claude_bin: String,
    pub codex_bin: String,
    pub ffprobe_bin: String,
    pub ffmpeg_bin: String,
    // paths / security
    pub db_path: PathBuf,
    pub allowed_root: Option<PathBuf>,
    // model
    pub model_default: String,
    // timeouts
    pub print_timeout_s: f64,
    pub claude_timeout_s: f64,
    pub ladder_deadline_s: f64,
    pub synth_timeout_s: f64,
    pub check_timeout_s: f64,
    // video tiers
    pub direct_max_s: f64,
    pub batch_max_s: f64,
    pub chunk_len_s: f64,
    pub max_chunks: u32,
    pub max_duration_s: Option<f64>,
    // frame sampling
    pub max_frames: u32,
    pub frame_fps: Option<f64>,
    pub frame_long_edge: u32,
    pub jpeg_qv: u32,
    // db
    pub db_busy_timeout_ms: i64,
}

/// Overrides supplied by CLI flags (highest precedence).
#[derive(Debug, Default, Clone)]
pub struct ConfigOverrides {
    pub db_path: Option<String>,
    pub allowed_root: Option<String>,
    pub print_timeout_s: Option<f64>,
    pub max_duration_s: Option<f64>,
    pub chunk_len_s: Option<f64>,
    pub max_chunks: Option<u32>,
    pub max_frames: Option<u32>,
    pub frame_fps: Option<f64>,
    pub model_default: Option<String>,
}

impl Config {
    pub fn load(ov: &ConfigOverrides) -> Config {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        let db_path = ov
            .db_path
            .clone()
            .or_else(|| env::var("GANDER_DB_PATH").ok())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".gander").join("media.db"));

        let allowed_root = ov
            .allowed_root
            .clone()
            .or_else(|| env::var("GANDER_ALLOWED_ROOT").ok())
            .filter(|s| !s.is_empty())
            .map(|s| canonicalize_or(expanduser(&s)));

        Config {
            agy_bin: discover("GANDER_AGY_BIN", "agy"),
            claude_bin: discover("GANDER_CLAUDE_BIN", "claude"),
            codex_bin: discover("GANDER_CODEX_BIN", "codex"),
            ffprobe_bin: discover("GANDER_FFPROBE_BIN", "ffprobe"),
            ffmpeg_bin: discover("GANDER_FFMPEG_BIN", "ffmpeg"),
            db_path,
            allowed_root,
            model_default: ov
                .model_default
                .clone()
                .or_else(|| env::var("GANDER_MODEL_DEFAULT").ok())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| MODEL_DEFAULT.to_string()),
            print_timeout_s: ov
                .print_timeout_s
                .or_else(|| env_f64("GANDER_PRINT_TIMEOUT_S"))
                .unwrap_or(PRINT_TIMEOUT_S),
            claude_timeout_s: CLAUDE_TIMEOUT_S,
            ladder_deadline_s: LADDER_DEADLINE_S,
            synth_timeout_s: SYNTH_TIMEOUT_S,
            check_timeout_s: CHECK_TIMEOUT_S,
            direct_max_s: DIRECT_MAX_S,
            batch_max_s: BATCH_MAX_S,
            chunk_len_s: ov
                .chunk_len_s
                .or_else(|| env_f64("GANDER_CHUNK_LEN_S"))
                .unwrap_or(CHUNK_LEN_S),
            max_chunks: ov
                .max_chunks
                .or_else(|| env_u32("GANDER_MAX_CHUNKS"))
                .unwrap_or(MAX_CHUNKS),
            // unset by default -> no ceiling
            max_duration_s: ov
                .max_duration_s
                .or_else(|| env_f64("GANDER_MAX_DURATION_S")),
            max_frames: ov
                .max_frames
                .or_else(|| env_u32("GANDER_MAX_FRAMES"))
                .unwrap_or(MAX_FRAMES),
            frame_fps: ov.frame_fps.or_else(|| env_f64("GANDER_FRAME_FPS")),
            frame_long_edge: FRAME_LONG_EDGE,
            jpeg_qv: JPEG_QV,
            db_busy_timeout_ms: DB_BUSY_TIMEOUT_MS,
        }
    }
}

/// Resolve a binary: `$ENV` → `PATH` lookup → the bare name (let exec fail loudly).
fn discover(env_var: &str, name: &str) -> String {
    if let Ok(v) = env::var(env_var) {
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(p) = which(name) {
        return p;
    }
    name.to_string()
}

/// Minimal `which`: scan `PATH` for an executable file named `name`.
fn which(name: &str) -> Option<String> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let cand = dir.join(name);
        if is_executable_file(&cand) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

fn expanduser(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(s)
}

fn canonicalize_or(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

fn env_f64(name: &str) -> Option<f64> {
    env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn env_u32(name: &str) -> Option<u32> {
    env::var(name).ok().and_then(|v| v.trim().parse().ok())
}
