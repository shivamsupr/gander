//! clap arg structs — the single CLI surface (PLAN.md §2).
//!
//! `describe` is the default form (bare `gander SOURCE [opts]`); `recall` is a
//! read-only cache-browse subcommand. stdout carries the result only; logs go to
//! stderr. No behavior lives here — `main.rs` dispatches on these structs.

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable text (default).
    Raw,
    /// Canonical JSON envelope (the agent contract).
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Model {
    // agy (Gemini)
    Pro,
    Flash,
    // claude
    Sonnet,
    Haiku,
    Opus,
    // codex (OpenAI)
    #[value(name = "gpt-5.5")]
    Gpt55,
    #[value(name = "gpt-5.4")]
    Gpt54,
    #[value(name = "gpt-5.4-mini")]
    Gpt54Mini,
}

/// Fallback model selector — `none` disables the second attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FallbackModel {
    Pro,
    Flash,
    Sonnet,
    Haiku,
    Opus,
    #[value(name = "gpt-5.5")]
    Gpt55,
    #[value(name = "gpt-5.4")]
    Gpt54,
    #[value(name = "gpt-5.4-mini")]
    Gpt54Mini,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Backend {
    Agy,
    Claude,
    Codex,
}

/// Fallback backend selector — `none` disables the second attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FallbackBackend {
    Agy,
    Claude,
    Codex,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Rating {
    Keep,
    Review,
    Cull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OrderBy {
    #[value(name = "updated_at")]
    UpdatedAt,
    #[value(name = "created_at")]
    CreatedAt,
    #[value(name = "rating")]
    Rating,
    #[value(name = "people_count")]
    PeopleCount,
    #[value(name = "duration_seconds")]
    DurationSeconds,
}

impl OrderBy {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderBy::UpdatedAt => "updated_at",
            OrderBy::CreatedAt => "created_at",
            OrderBy::Rating => "rating",
            OrderBy::PeopleCount => "people_count",
            OrderBy::DurationSeconds => "duration_seconds",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "gander",
    version,
    about = "Understand a local media file: transcript + structured description.",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Describe args (the default form when no subcommand is given).
    #[command(flatten)]
    pub describe: DescribeArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search previously analyzed media (read-only; no model call).
    Recall(RecallArgs),
    /// Inspect or reset the persisted defaults (~/.gander/config.toml).
    Config(ConfigArgs),
    /// Inspect or clear the result cache (~/.gander/media.db).
    Cache(CacheArgs),
}

#[derive(Debug, clap::Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub action: CacheAction,
}

#[derive(Debug, Subcommand)]
pub enum CacheAction {
    /// Print the cache DB path.
    Path {
        #[arg(long, value_name = "PATH")]
        db: Option<String>,
    },
    /// Remove cached entries: all of them, or just one SOURCE file.
    Clear {
        /// A media file to forget (by content hash). Omit to clear the whole cache.
        source: Option<String>,
        #[arg(long, value_name = "PATH")]
        db: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the config file path.
    Path,
    /// Print the current persisted config.
    Show,
    /// Delete the persisted config file.
    Clear,
}

#[derive(Debug, clap::Args)]
pub struct DescribeArgs {
    /// Local path to one image / video / audio file (no URLs).
    pub source: Option<String>,

    /// `json` emits the canonical envelope to stdout.
    #[arg(long, value_enum, default_value_t = OutputFormat::Raw)]
    pub output_format: OutputFormat,

    /// Primary model.
    #[arg(long, value_enum)]
    pub model: Option<Model>,

    /// Primary backend.
    #[arg(long, value_enum)]
    pub backend: Option<Backend>,

    /// Model for the fallback attempt (`none` = no fallback).
    #[arg(long, value_enum)]
    pub fallback_model: Option<FallbackModel>,

    /// Backend for the fallback attempt (`none` = no fallback).
    #[arg(long, value_enum)]
    pub fallback_backend: Option<FallbackBackend>,

    /// Skip speech transcription (visual fields only).
    #[arg(long)]
    pub no_transcript: bool,

    /// Do not produce an English translation block.
    #[arg(long)]
    pub no_translate: bool,

    /// Evenly-spaced frames per clip/chunk (clamped [1,64]).
    #[arg(long, value_name = "N")]
    pub max_frames: Option<u32>,

    /// Fixed-rate frame sampling, capped by --max-frames.
    #[arg(long, value_name = "RATE")]
    pub fps: Option<f64>,

    /// Segment length for the chunked tier.
    #[arg(long, value_name = "S")]
    pub chunk_length: Option<f64>,

    /// Cap on chunks; over-limit ⇒ widen segment length.
    #[arg(long, value_name = "N")]
    pub max_chunks: Option<u32>,

    /// Hard-reject videos longer than S (clean `failed`).
    #[arg(long, value_name = "S")]
    pub max_duration: Option<f64>,

    /// Ignore any cached row and recompute.
    #[arg(long)]
    pub force: bool,

    /// Keep the per-call temp workdir (path logged to stderr).
    #[arg(long)]
    pub keep_temp: bool,

    /// Restrict SOURCE to paths under DIR.
    #[arg(long, value_name = "DIR")]
    pub allowed_root: Option<String>,

    /// Override the cache DB location.
    #[arg(long, value_name = "PATH")]
    pub db: Option<String>,

    /// Per-backend wall-clock seconds.
    #[arg(long, value_name = "S")]
    pub timeout: Option<f64>,

    /// Health-probe the backends (ignores SOURCE).
    #[arg(long)]
    pub check: bool,

    /// Re-run the first-run interactive setup and rewrite ~/.gander/config.toml.
    #[arg(long)]
    pub reconfigure: bool,

    /// Ignore the persisted config file for this run.
    #[arg(long)]
    pub no_config: bool,
}

#[derive(Debug, clap::Args)]
pub struct RecallArgs {
    /// Full-text search (FTS5/BM25) over summary, description, transcript,
    /// translation, keywords and filename. Best-match order unless --order-by
    /// is given. FTS5 syntax (OR, NEAR, col:, term*) passes through.
    #[arg(long)]
    pub query: Option<String>,
    #[arg(long)]
    pub keyword: Option<String>,
    /// Exact substring match over description/summary/transcript.
    #[arg(long)]
    pub text: Option<String>,
    #[arg(long, value_enum)]
    pub rating: Option<Rating>,
    #[arg(long)]
    pub language: Option<String>,
    #[arg(long, value_enum)]
    pub kind: Option<MediaKind>,
    #[arg(long)]
    pub min_people: Option<i64>,
    #[arg(long)]
    pub min_duration: Option<f64>,

    /// Require a transcript.
    #[arg(long)]
    pub has_transcript: bool,
    /// Require no transcript.
    #[arg(long, conflicts_with = "has_transcript")]
    pub no_transcript: bool,

    /// Require audio.
    #[arg(long)]
    pub has_audio: bool,
    /// Require no audio.
    #[arg(long, conflicts_with = "has_audio")]
    pub no_audio: bool,

    #[arg(long)]
    pub chunked: bool,
    #[arg(long)]
    pub include_failed: bool,
    #[arg(long)]
    pub all_versions: bool,

    /// Sort column (default: updated_at, or best match with --query).
    #[arg(long, value_enum)]
    pub order_by: Option<OrderBy>,
    /// Ascending order (default: descending).
    #[arg(long)]
    pub asc: bool,
    #[arg(long, default_value_t = 20)]
    pub limit: i64,

    #[arg(long, value_name = "PATH")]
    pub db: Option<String>,

    #[arg(long, value_enum, default_value_t = OutputFormat::Raw)]
    pub output_format: OutputFormat,
}
