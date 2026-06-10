//! `--check` — health-probe the backends with a trivial no-media prompt, plus
//! ffmpeg/ffprobe presence.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::{backend_for, classify_error, model_for, piped, pty, BackendKind, ErrorClass};
use crate::config::Config;
use crate::parse;
use crate::prompt::{build_prompt, PromptKind};

/// Probe `which` backend (or all three), plus ffmpeg/ffprobe when probing all.
pub fn health_probe(cfg: &Config, which: Option<BackendKind>) -> Value {
    let probe_prompt =
        build_prompt(PromptKind::Image, true, true, None).replace("MEDIA_PATH", "(none)");
    let add_dir = cfg
        .allowed_root
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".into())
        });

    let mut out = serde_json::Map::new();
    let kinds = match which {
        Some(k) => vec![k],
        None => vec![BackendKind::Agy, BackendKind::Claude, BackendKind::Codex],
    };
    for kind in kinds {
        out.insert(
            name_of(kind).to_string(),
            probe_backend(kind, cfg, &probe_prompt, &add_dir),
        );
    }
    if which.is_none() {
        out.insert("ffprobe".into(), bin_ok(&cfg.ffprobe_bin));
        out.insert("ffmpeg".into(), bin_ok(&cfg.ffmpeg_bin));
    }
    Value::Object(out)
}

fn name_of(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Agy => "agy",
        BackendKind::Claude => "claude",
        BackendKind::Codex => "codex",
    }
}

fn probe_backend(kind: BackendKind, cfg: &Config, prompt: &str, add_dir: &str) -> Value {
    let be = backend_for(kind, cfg);
    let model = model_for(kind, "flash");
    let timeout = cfg.check_timeout_s;
    let argv = be.build_argv(prompt, add_dir, &model, timeout);
    let env = be.env();
    let dur = Duration::from_secs_f64(timeout);

    let raw = if be.needs_pty() {
        pty::run_pty(&argv, &env, None, dur)
    } else {
        piped::run_piped(&argv, &env, dur)
    };

    match raw {
        Ok(r) => {
            let combined = format!("{}\n{}", r.stdout, r.stderr);
            let answer = parse::extract_answer(&r.stdout);
            let (ecls, detail) = classify_error(
                &parse::strip_ansi(&combined),
                r.exit_code,
                r.timed_out,
                answer.is_some(),
            );
            json!({
                "model": model,
                "ok": ecls == ErrorClass::Ok,
                "error_class": ecls.as_str(),
                "detail": detail,
                "latency_s": round1(r.duration.as_secs_f64()),
            })
        }
        Err(e) => json!({
            "model": model,
            "ok": false,
            "error_class": "fatal",
            "detail": format!("exec error: {e}"),
            "latency_s": 0.0,
        }),
    }
}

fn bin_ok(path: &str) -> Value {
    let p = Path::new(path);
    let ok = p.is_absolute() && p.exists();
    json!({ "ok": ok, "path": path })
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
