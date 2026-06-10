//! clap entry, dispatch, and exit-code mapping (PLAN.md §2 exit contract).
//!
//! Exit codes: 0 ok/partial · 1 unexpected · 2 usage · 3 input (`failed`) ·
//! 4 backend/auth. `partial` exits 0 — read the JSON `status`/`warnings`.

// Domain structs deliberately carry the full field set (ffprobe facts, config
// timeouts, debug excerpts) even where a field is not yet read, to keep the
// envelope contract stable and ease future features.
#![allow(dead_code)]

mod backend;
mod check;
mod cli;
mod config;
mod config_file;
mod core;
mod db;
mod envelope;
mod ffmpeg;
mod parse;
mod prompt;
mod source;
mod video;

use clap::Parser;
use std::process::ExitCode;

use cli::{Backend, Cli, Command, Model, OutputFormat};
use config::{Config, ConfigOverrides};
use core::DescribeOptions;
use envelope::{MediaResult, Status};

// Exit-code contract.
const EXIT_OK: u8 = 0;
const EXIT_UNEXPECTED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_INPUT: u8 = 3;
const EXIT_BACKEND: u8 = 4;

/// error_class values that map to EXIT_INPUT (a `failed` the caller caused).
const INPUT_CLASSES: &[&str] = &[
    "input",
    "not_a_file",
    "unreadable",
    "is_url",
    "outside_root",
    "too_long",
    "bad_kind",
];

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Recall(args)) => cmd_recall(args),
        Some(Command::Config(args)) => cmd_config(args),
        None => dispatch_describe(cli.describe),
    }
}

fn cmd_config(args: cli::ConfigArgs) -> ExitCode {
    use cli::ConfigAction::*;
    match args.action {
        Path => {
            println!("{}", config_file::config_path().display());
            ExitCode::from(EXIT_OK)
        }
        Show => match config_file::raw_text() {
            Some(text) => {
                print!("{text}");
                ExitCode::from(EXIT_OK)
            }
            None => {
                eprintln!(
                    "gander: no config at {} (run `gander --reconfigure`)",
                    config_file::config_path().display()
                );
                ExitCode::from(EXIT_OK)
            }
        },
        Clear => match config_file::clear() {
            Ok(true) => {
                eprintln!("gander: removed {}", config_file::config_path().display());
                ExitCode::from(EXIT_OK)
            }
            Ok(false) => {
                eprintln!("gander: no config to clear");
                ExitCode::from(EXIT_OK)
            }
            Err(e) => {
                eprintln!("gander: could not remove config: {e}");
                ExitCode::from(EXIT_UNEXPECTED)
            }
        },
    }
}

fn overrides_from(args: &cli::DescribeArgs) -> ConfigOverrides {
    ConfigOverrides {
        db_path: args.db.clone(),
        allowed_root: args.allowed_root.clone(),
        print_timeout_s: args.timeout,
        max_duration_s: args.max_duration,
        chunk_len_s: args.chunk_length,
        max_chunks: args.max_chunks,
        max_frames: args.max_frames,
        frame_fps: args.fps,
        model_default: args.model.map(model_str),
    }
}

fn dispatch_describe(args: cli::DescribeArgs) -> ExitCode {
    if args.reconfigure {
        // Re-run the first-run prompt and rewrite the config file.
        if config_file::maybe_first_run(args.no_config, true).is_none() {
            eprintln!("gander: --reconfigure needs an interactive terminal");
            return ExitCode::from(EXIT_USAGE);
        }
        return ExitCode::from(EXIT_OK);
    }
    if args.check {
        return cmd_check(&args);
    }
    let Some(source) = args.source.clone() else {
        eprintln!("gander: SOURCE is required (or use --check / --version / recall)");
        return ExitCode::from(EXIT_USAGE);
    };

    // A backend/model mismatch is a usage error (PLAN.md §4).
    if let Err(msg) = validate_pairs(&args) {
        eprintln!("gander: {msg}");
        return ExitCode::from(EXIT_USAGE);
    }

    // First-run: persist defaults if interactive and no config yet.
    config_file::maybe_first_run(args.no_config, false);
    let file = config_file::load(args.no_config);

    let cfg = Config::load(&overrides_from(&args));

    // Per-setting precedence: flag > GANDER_* env > config file > built-in.
    let model = pick(
        args.model.map(model_str),
        "GANDER_MODEL_DEFAULT",
        file.model,
        "pro",
    );
    let backend = pick(
        args.backend.map(backend_str),
        "GANDER_BACKEND_DEFAULT",
        file.backend,
        "agy",
    );
    let fb_model = pick(
        args.fallback_model.map(fallback_model_str),
        "GANDER_FALLBACK_MODEL_DEFAULT",
        file.fallback_model,
        "flash",
    );
    let fb_backend = pick(
        args.fallback_backend.map(fallback_backend_str),
        "GANDER_FALLBACK_BACKEND_DEFAULT",
        file.fallback_backend,
        "agy",
    );

    let opts = DescribeOptions {
        model: Some(model),
        backend: Some(backend),
        fallback_model: Some(fb_model),
        fallback_backend: Some(fb_backend),
        force: args.force,
        want_transcript: !args.no_transcript,
        translate: !args.no_translate,
        max_frames: args.max_frames,
        fps: args.fps,
        chunk_length: args.chunk_length,
        max_chunks: args.max_chunks,
        max_duration: args.max_duration,
        keep_temp: args.keep_temp,
    };

    let result = core::describe_media(&source, &opts, &cfg);

    match args.output_format {
        OutputFormat::Json => println!("{}", result.to_json_pretty()),
        OutputFormat::Raw => println!("{}", render_raw(&result)),
    }

    ExitCode::from(exit_for(&result))
}

/// flag > `GANDER_*` env > config file > built-in.
fn pick(flag: Option<String>, env_var: &str, file: Option<String>, builtin: &str) -> String {
    flag.or_else(|| std::env::var(env_var).ok().filter(|s| !s.is_empty()))
        .or(file)
        .unwrap_or_else(|| builtin.to_string())
}

// --------------------------------------------------------------------------- //
// recall + --check
// --------------------------------------------------------------------------- //
fn cmd_recall(args: cli::RecallArgs) -> ExitCode {
    let overrides = ConfigOverrides {
        db_path: args.db.clone(),
        ..Default::default()
    };
    let cfg = Config::load(&overrides);
    let conn = match db::connect(&cfg.db_path, cfg.db_busy_timeout_ms) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gander: cannot open db: {e}");
            return ExitCode::from(EXIT_UNEXPECTED);
        }
    };

    let filters = db::RecallFilters {
        keyword: args.keyword,
        text: args.text,
        rating: args.rating.map(|r| rating_str(r).to_string()),
        language: args.language,
        media_kind: args.kind.map(|k| media_kind_str(k).to_string()),
        min_people: args.min_people,
        min_duration: args.min_duration,
        has_transcript: tri(args.has_transcript, args.no_transcript),
        has_audio: tri(args.has_audio, args.no_audio),
        chunked: if args.chunked { Some(true) } else { None },
        include_failed: args.include_failed,
        all_versions: args.all_versions,
        order_by: args.order_by.as_str().to_string(),
        descending: !args.asc,
        limit: args.limit,
    };

    let rows = match db::recall(&conn, &filters) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gander: recall failed: {e}");
            return ExitCode::from(EXIT_UNEXPECTED);
        }
    };

    match args.output_format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into())
            )
        }
        OutputFormat::Raw => println!("{}", render_recall_table(&rows)),
    }
    ExitCode::from(EXIT_OK)
}

/// `--has-X` / `--no-X` → tri-state Option<bool>.
fn tri(yes: bool, no: bool) -> Option<bool> {
    if yes {
        Some(true)
    } else if no {
        Some(false)
    } else {
        None
    }
}

fn cmd_check(args: &cli::DescribeArgs) -> ExitCode {
    let cfg = Config::load(&overrides_from(args));
    let which = args.backend.map(|b| match b {
        Backend::Agy => backend::BackendKind::Agy,
        Backend::Claude => backend::BackendKind::Claude,
        Backend::Codex => backend::BackendKind::Codex,
    });
    let report = check::health_probe(&cfg, which);
    let all_ok = report
        .as_object()
        .map(|m| {
            m.values()
                .all(|v| v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false))
        })
        .unwrap_or(false);

    match args.output_format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
            )
        }
        OutputFormat::Raw => println!("{}", render_check(&report)),
    }
    ExitCode::from(if all_ok { EXIT_OK } else { EXIT_BACKEND })
}

fn render_recall_table(rows: &[serde_json::Value]) -> String {
    if rows.is_empty() {
        return "no matches".to_string();
    }
    let mut out = vec!["SHA       KIND   PEOPLE  LANG  RATING  SUMMARY".to_string()];
    for r in rows {
        let g = |k: &str| r.get(k).cloned().unwrap_or(serde_json::Value::Null);
        let sha = g("content_sha256")
            .as_str()
            .unwrap_or("")
            .chars()
            .take(8)
            .collect::<String>();
        let kind = g("media_kind")
            .as_str()
            .unwrap_or("")
            .chars()
            .take(6)
            .collect::<String>();
        let ppl = g("people_count")
            .as_i64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into());
        let lang = g("language").as_str().unwrap_or("—").to_string();
        let rating = g("rating").as_str().unwrap_or("—").to_string();
        let summary = g("summary")
            .as_str()
            .unwrap_or("")
            .chars()
            .take(50)
            .collect::<String>();
        out.push(format!(
            "{sha:<8}… {kind:<6} {ppl:<6}  {lang:<4}  {rating:<6}  {summary}"
        ));
    }
    out.join("\n")
}

fn render_check(report: &serde_json::Value) -> String {
    let mut out = Vec::new();
    let obj = match report.as_object() {
        Some(o) => o,
        None => return String::new(),
    };
    for name in ["agy", "claude", "codex"] {
        if let Some(v) = obj.get(name) {
            let icon = if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                "● ok"
            } else {
                "✖ down"
            };
            let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("");
            let lat = v.get("latency_s").and_then(|l| l.as_f64()).unwrap_or(0.0);
            out.push(format!("{name:<7} {icon:<7} {model:<26} {lat}s"));
        }
    }
    for name in ["ffprobe", "ffmpeg"] {
        if let Some(v) = obj.get(name) {
            let icon = if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                "● found"
            } else {
                "✖ missing"
            };
            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
            out.push(format!("{name:<7} {icon:<9} {path}"));
        }
    }
    out.join("\n")
}

fn exit_for(r: &MediaResult) -> u8 {
    match r.status {
        Status::Ok | Status::Partial => EXIT_OK,
        Status::Failed => {
            let is_input = r
                .error_class
                .as_deref()
                .map(|c| INPUT_CLASSES.contains(&c))
                .unwrap_or(false);
            if is_input {
                EXIT_INPUT
            } else {
                EXIT_BACKEND
            }
        }
    }
}

/// Each backend has its own model namespace; an explicit cross-namespace pair is a
/// usage error. Checks both the primary and (if set) the fallback pair.
fn validate_pairs(args: &cli::DescribeArgs) -> Result<(), String> {
    if let (Some(b), Some(m)) = (args.backend, args.model) {
        check_pair(&backend_str(b), &model_str(m))?;
    }
    if let (Some(b), Some(m)) = (args.fallback_backend, args.fallback_model) {
        let (bs, ms) = (fallback_backend_str(b), fallback_model_str(m));
        if bs != "none" && ms != "none" {
            check_pair(&bs, &ms)?;
        }
    }
    Ok(())
}

fn check_pair(backend: &str, model: &str) -> Result<(), String> {
    let ok = match backend {
        "agy" => matches!(model, "pro" | "flash"),
        "claude" => matches!(model, "sonnet" | "haiku" | "opus"),
        "codex" => matches!(model, "gpt-5.5" | "gpt-5.4" | "gpt-5.4-mini"),
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "backend {backend} is incompatible with model {model} \
             (agy: pro|flash; claude: sonnet|haiku|opus; codex: gpt-5.5|gpt-5.4|gpt-5.4-mini)"
        ))
    }
}

fn model_str(m: Model) -> String {
    match m {
        Model::Pro => "pro",
        Model::Flash => "flash",
        Model::Sonnet => "sonnet",
        Model::Haiku => "haiku",
        Model::Opus => "opus",
        Model::Gpt55 => "gpt-5.5",
        Model::Gpt54 => "gpt-5.4",
        Model::Gpt54Mini => "gpt-5.4-mini",
    }
    .to_string()
}

fn backend_str(b: Backend) -> String {
    match b {
        Backend::Agy => "agy",
        Backend::Claude => "claude",
        Backend::Codex => "codex",
    }
    .to_string()
}

fn fallback_model_str(m: cli::FallbackModel) -> String {
    use cli::FallbackModel::*;
    match m {
        Pro => "pro",
        Flash => "flash",
        Sonnet => "sonnet",
        Haiku => "haiku",
        Opus => "opus",
        Gpt55 => "gpt-5.5",
        Gpt54 => "gpt-5.4",
        Gpt54Mini => "gpt-5.4-mini",
        None => "none",
    }
    .to_string()
}

fn rating_str(r: cli::Rating) -> &'static str {
    match r {
        cli::Rating::Keep => "keep",
        cli::Rating::Review => "review",
        cli::Rating::Cull => "cull",
    }
}

fn media_kind_str(k: cli::MediaKind) -> &'static str {
    match k {
        cli::MediaKind::Image => "image",
        cli::MediaKind::Video => "video",
        cli::MediaKind::Audio => "audio",
    }
}

fn fallback_backend_str(b: cli::FallbackBackend) -> String {
    use cli::FallbackBackend::*;
    match b {
        Agy => "agy",
        Claude => "claude",
        Codex => "codex",
        None => "none",
    }
    .to_string()
}

/// Minimal human render (the full colored markdown render lands with recall in M6).
fn render_raw(r: &MediaResult) -> String {
    let mut out = String::new();
    let head = match r.status {
        Status::Ok => "● ok",
        Status::Partial => "◐ partial",
        Status::Failed => "✖ failed",
    };
    let sha = if r.content_sha256.is_empty() {
        "—".to_string()
    } else {
        format!("{}…", &r.content_sha256[..r.content_sha256.len().min(8)])
    };
    out.push_str(&format!("{head}  {}  sha {sha}", r.media_kind));
    if !r.backend.backend_used.is_empty() {
        out.push_str(&format!(
            "  ({}/{})",
            r.backend.backend_used,
            if r.backend.model_used.is_empty() {
                "—"
            } else {
                &r.backend.model_used
            }
        ));
    }

    if r.status == Status::Failed {
        out.push_str(&format!("\n\n{}", r.error.as_deref().unwrap_or("failed")));
        return out;
    }

    if !r.warnings.is_empty() {
        out.push_str("\n\nWARNINGS");
        for w in &r.warnings {
            out.push_str(&format!("\n  - {w}"));
        }
    }
    if !r.summary.is_empty() {
        out.push_str(&format!("\n\n{}", r.summary));
    }
    if let Some(t) = &r.transcript {
        out.push_str(&format!(
            "\n\nTRANSCRIPT ({})\n  {}",
            r.language.as_deref().unwrap_or("?"),
            t.replace('\n', "\n  ")
        ));
    }
    if !r.description.is_empty() {
        out.push_str(&format!(
            "\n\nDESCRIPTION\n  {}",
            r.description.replace('\n', "\n  ")
        ));
    }
    if let Some(s) = &r.structured {
        out.push_str("\n\nSTRUCTURED");
        out.push_str(&format!("\n  rating        {}", s.rating));
        out.push_str(&format!(
            "\n  shot_type     {:<16} people_count  {}",
            s.shot_type, s.people_count
        ));
        out.push_str(&format!(
            "\n  lighting      {:<16} time_of_day   {}",
            s.lighting, s.time_of_day
        ));
        out.push_str(&format!("\n  audio_quality {}", s.audio_quality));
        if !s.dominant_colors.is_empty() {
            out.push_str(&format!(
                "\n  colors        {}",
                s.dominant_colors.join(", ")
            ));
        }
        if !s.keywords.is_empty() {
            out.push_str(&format!("\n  keywords      {}", s.keywords.join(", ")));
        }
    }
    out.push_str(&format!(
        "\n\n  cached: {}   parse_ok: {}   attempts: {}",
        if r.cached { "yes" } else { "no" },
        if r.parse_ok { "yes" } else { "no" },
        r.backend.attempts.len()
    ));
    out
}
