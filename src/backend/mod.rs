//! Backend engine: drive an agentic CLI and classify its outcome.
//!
//! - `pty`   — the load-bearing PTY runner (M1).
//! - `agy`   — agy adapter: argv + env + PTY run (M2).
//! - error taxonomy `classify_error` lives here.
//!
//! The `AgentBackend` trait + primary/fallback ladder + claude/codex adapters land
//! in M3 on top of these pieces.

pub mod agy;
pub mod claude;
pub mod codex;
pub mod piped;
pub mod pty;

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

use crate::config::{Config, CLAUDE_HAIKU, CLAUDE_OPUS, CLAUDE_SONNET, FLASH, PRO};
use crate::parse;

/// Precisely classified backend outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Ok,
    Capacity,
    Timeout,
    Transient,
    Auth,
    Empty,
    Fatal,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::Ok => "ok",
            ErrorClass::Capacity => "capacity",
            ErrorClass::Timeout => "timeout",
            ErrorClass::Transient => "transient",
            ErrorClass::Auth => "auth",
            ErrorClass::Empty => "empty",
            ErrorClass::Fatal => "fatal",
        }
    }
}

/// One recorded attempt on the ladder.
#[derive(Debug, Clone)]
pub struct AttemptRec {
    pub backend: &'static str,
    pub model: String,
    pub error_class: ErrorClass,
    pub error_detail: String,
    pub elapsed_s: f64,
    pub answer_len: usize,
    pub chunk: Option<u32>,
}

/// The result of running one or more backend rungs for a single media call.
#[derive(Debug, Clone)]
pub struct BackendResult {
    pub ok: bool,
    pub backend: Option<&'static str>,
    pub model: Option<String>,
    /// Full raw stdout of the winning rung (parse slices it again).
    pub answer: String,
    pub vision_only: bool,
    pub attempts: Vec<AttemptRec>,
    pub aborted_auth: bool,
    pub error: String,
}

// --------------------------------------------------------------------------- //
// Error taxonomy — classify a backend outcome from its output + exit code.
// --------------------------------------------------------------------------- //
fn capacity_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"model_capacity_exhausted|\b429\b|\bcapacity\b|\boverloaded\b|resource[_ ]exhausted|rate.?limit|quota|try again later",
        )
        .unwrap()
    })
}

fn auth_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"not (logged in|authenticated)|\bunauthor|\b401\b|\b403\b|please (log ?in|sign ?in)|authentication (failed|required)|invalid (credentials|api key|token)|\boauth\b|login expired|reauthenticate|no active (account|session)",
        )
        .unwrap()
    })
}

fn transient_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\b50[0234]\b|internal (server )?error|service unavailable|connection (reset|refused|closed)|network|\beof\b|unexpected end|deadline exceeded|temporarily",
        )
        .unwrap()
    })
}

/// `(class, short_detail)`. AUTH before OK; a clean answer beats a capacity warning.
pub fn classify_error(
    text: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    answer_found: bool,
) -> (ErrorClass, String) {
    let low = text.to_lowercase();

    if let Some(m) = auth_re().find(&low) {
        return (ErrorClass::Auth, format!("auth signal: {:?}", m.as_str()));
    }
    if answer_found {
        return (ErrorClass::Ok, String::new());
    }
    if timed_out {
        return (ErrorClass::Timeout, "wall-clock kill".into());
    }
    if let Some(m) = capacity_re().find(&low) {
        return (
            ErrorClass::Capacity,
            format!("capacity signal: {:?}", m.as_str()),
        );
    }
    if let Some(m) = transient_re().find(&low) {
        return (
            ErrorClass::Transient,
            format!("transient signal: {:?}", m.as_str()),
        );
    }
    if exit_code == Some(0) || low.trim().is_empty() {
        return (ErrorClass::Empty, "no answer block in output".into());
    }
    let tail: String = low
        .chars()
        .rev()
        .take(160)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (
        ErrorClass::Fatal,
        format!("exit={exit_code:?}; tail={tail:?}"),
    )
}

// --------------------------------------------------------------------------- //
// Adapter contract — one trait, three small impls (PLAN.md §4).
// --------------------------------------------------------------------------- //
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Agy,
    Claude,
    Codex,
}

impl BackendKind {
    pub fn from_str(s: &str) -> Option<BackendKind> {
        match s {
            "agy" => Some(BackendKind::Agy),
            "claude" => Some(BackendKind::Claude),
            "codex" => Some(BackendKind::Codex),
            _ => None,
        }
    }
}

/// One rung of the ladder: a backend + the concrete model string it should run.
#[derive(Debug, Clone)]
pub struct Rung {
    pub kind: BackendKind,
    pub model: String,
}

/// One unit of work the ladder can run. Holds both prompt variants so the ladder
/// hands each backend the right one (claude is vision-only → no audio prompt).
#[derive(Debug, Clone)]
pub struct MediaCall {
    pub kind: &'static str,
    pub add_dir: String,
    pub prompt_full: String,   // agy / codex (full modality)
    pub prompt_vision: String, // claude (vision-only)
    pub has_audio: bool,
}

/// The pluggable backend contract. The ladder never touches backend specifics.
pub trait AgentBackend {
    fn name(&self) -> &'static str;
    fn build_argv(&self, prompt: &str, add_dir: &str, model: &str, timeout_s: f64) -> Vec<String>;
    fn needs_pty(&self) -> bool;
    fn env(&self) -> HashMap<String, String>;
    fn supports_audio(&self) -> bool;
    fn timeout_s(&self, cfg: &Config) -> f64;
}

pub fn backend_for(kind: BackendKind, cfg: &Config) -> Box<dyn AgentBackend> {
    match kind {
        BackendKind::Agy => Box::new(agy::Agy {
            bin: cfg.agy_bin.clone(),
        }),
        BackendKind::Claude => Box::new(claude::Claude {
            bin: cfg.claude_bin.clone(),
        }),
        BackendKind::Codex => Box::new(codex::Codex {
            bin: cfg.codex_bin.clone(),
        }),
    }
}

/// Map a gander model selector to a backend's concrete model string.
pub fn model_for(kind: BackendKind, selector: &str) -> String {
    match kind {
        BackendKind::Agy => match selector {
            "flash" | "auto" => FLASH,
            _ => PRO,
        }
        .to_string(),
        BackendKind::Claude => match selector {
            "haiku" => CLAUDE_HAIKU,
            "opus" => CLAUDE_OPUS,
            _ => CLAUDE_SONNET,
        }
        .to_string(),
        BackendKind::Codex => match selector {
            "gpt-5.5" | "gpt-5.4" | "gpt-5.4-mini" => selector.to_string(),
            // A non-codex selector (e.g. the default "pro") → codex's own default.
            _ => crate::config::CODEX_DEFAULT_MODEL.to_string(),
        },
    }
}

/// The full env minus the keys in `strip` (forces claude onto $0 Max OAuth).
pub fn env_minus(strip: &[&str]) -> HashMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| !strip.contains(&k.as_str()))
        .collect()
}

// --------------------------------------------------------------------------- //
// Run one rung + the primary→fallback ladder.
// --------------------------------------------------------------------------- //
struct OneRun {
    att: AttemptRec,
    stdout: String,
    ecls: ErrorClass,
    detail: String,
    vision_only: bool,
}

fn run_one(
    be: &dyn AgentBackend,
    model: &str,
    call: &MediaCall,
    cfg: &Config,
    chunk: Option<u32>,
) -> OneRun {
    let prompt = if be.supports_audio() {
        &call.prompt_full
    } else {
        &call.prompt_vision
    };
    let timeout = be.timeout_s(cfg);
    let argv = be.build_argv(prompt, &call.add_dir, model, timeout);
    let env = be.env();
    let dur = Duration::from_secs_f64(timeout);

    let raw = if be.needs_pty() {
        pty::run_pty(&argv, &env, None, dur)
    } else {
        piped::run_piped(&argv, &env, dur)
    };

    let raw = match raw {
        Ok(r) => r,
        Err(e) => {
            let att = AttemptRec {
                backend: be.name(),
                model: model.to_string(),
                error_class: ErrorClass::Fatal,
                error_detail: format!("exec error: {e}"),
                elapsed_s: 0.0,
                answer_len: 0,
                chunk,
            };
            return OneRun {
                att,
                stdout: String::new(),
                ecls: ErrorClass::Fatal,
                detail: format!("exec error: {e}"),
                vision_only: !be.supports_audio(),
            };
        }
    };

    let combined = format!("{}\n{}", raw.stdout, raw.stderr);
    let clean = parse::strip_ansi(&combined);
    let answer = parse::extract_answer(&raw.stdout);
    let (ecls, detail) = classify_error(&clean, raw.exit_code, raw.timed_out, answer.is_some());
    let att = AttemptRec {
        backend: be.name(),
        model: model.to_string(),
        error_class: ecls,
        error_detail: detail.clone(),
        elapsed_s: raw.duration.as_secs_f64(),
        answer_len: answer.as_deref().map(str::len).unwrap_or(0),
        chunk,
    };
    OneRun {
        att,
        stdout: raw.stdout,
        ecls,
        detail,
        vision_only: !be.supports_audio(),
    }
}

/// Run the primary rung; demote to the fallback on capacity/timeout/transient/
/// empty/unparseable; abort immediately on auth. `fallback = None` ⇒ single-shot.
pub fn run_ladder(
    call: &MediaCall,
    primary: &Rung,
    fallback: Option<&Rung>,
    cfg: &Config,
    chunk: Option<u32>,
) -> BackendResult {
    let mut attempts: Vec<AttemptRec> = Vec::new();
    let rungs: Vec<&Rung> = std::iter::once(primary).chain(fallback).collect();

    for rung in rungs {
        let be = backend_for(rung.kind, cfg);
        let run = run_one(&*be, &rung.model, call, cfg, chunk);
        attempts.push(run.att);

        match run.ecls {
            ErrorClass::Ok => {
                return BackendResult {
                    ok: true,
                    backend: Some(be.name()),
                    model: Some(rung.model.clone()),
                    answer: run.stdout,
                    vision_only: run.vision_only,
                    attempts,
                    aborted_auth: false,
                    error: String::new(),
                };
            }
            ErrorClass::Auth => {
                return BackendResult {
                    ok: false,
                    backend: None,
                    model: None,
                    answer: String::new(),
                    vision_only: false,
                    attempts,
                    aborted_auth: true,
                    error: format!(
                        "auth failure on {}/{}: {}. Re-login: run `{}` once interactively, \
                         and confirm the session is active.",
                        be.name(),
                        rung.model,
                        run.detail,
                        be.name()
                    ),
                };
            }
            // CAPACITY / TIMEOUT / TRANSIENT / EMPTY / FATAL -> demote.
            _ => {}
        }
    }

    let trail = attempts
        .iter()
        .map(|a| format!("{}/{}:{}", a.backend, a.model, a.error_class.as_str()))
        .collect::<Vec<_>>()
        .join("; ");
    BackendResult {
        ok: false,
        backend: None,
        model: None,
        answer: String::new(),
        vision_only: false,
        attempts,
        aborted_auth: false,
        error: format!("all backends exhausted: {trail}"),
    }
}
