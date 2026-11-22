//! Backend engine: drive an agentic CLI and classify its outcome.
//!
//! - `pty` / `piped` — the two runners (agy needs a PTY, the others a pipe).
//! - `agy` / `claude` / `codex` — one small adapter each, behind `AgentBackend`.
//! - `classify_error` — the error taxonomy.
//! - `run_ladder` — the primary → fallback sequence.

pub mod agy;
pub mod claude;
pub mod codex;
pub mod piped;
pub mod pty;

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

use crate::config::Config;
pub use crate::models::model_for;
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
    /// The harness does not know the model name (renamed, removed, or not part
    /// of this account's plan).
    ModelUnknown,
    /// The backend's CLI is not on this machine (or not on `PATH`).
    NotInstalled,
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
            ErrorClass::ModelUnknown => "model_unknown",
            ErrorClass::NotInstalled => "not_installed",
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

/// How each harness says "I do not know that model". None of them fails loudly:
/// agy prints its model list and exits 0, claude and codex exit 0 with an error
/// line. Without this class a renamed model reads as `empty`.
///
/// - agy:    `invalid model selection ... is not recognized as a known model`
/// - claude: `[claude-code:unrecognized_model]`
/// - codex:  `The 'x' model is not supported when using Codex with ...`
fn model_unknown_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"invalid model selection|not recognized as a known model|unrecognized_model|model is not supported|model_not_found|unknown model",
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

/// `(class, short_detail)`. AUTH before OK; a clean answer beats every warning.
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
    if let Some(m) = model_unknown_re().find(&low) {
        return (
            ErrorClass::ModelUnknown,
            format!("the backend rejected the model: {:?}", m.as_str()),
        );
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
// Adapter contract — one trait, three small impls.
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

impl MediaCall {
    /// Append the `--ask` clause to both prompt variants. No-op when `ask` is None.
    pub fn with_ask(mut self, ask: Option<&str>) -> Self {
        if let Some(q) = ask {
            self.prompt_full = crate::prompt::append_ask(self.prompt_full, q);
            self.prompt_vision = crate::prompt::append_ask(self.prompt_vision, q);
        }
        self
    }
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

/// The full env minus the keys in `strip` (forces claude onto $0 Max OAuth).
pub fn env_minus(strip: &[&str]) -> HashMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| !strip.contains(&k.as_str()))
        .collect()
}

/// How many raw files to keep in the raw directory.
const RAW_KEEP: usize = 20;

/// Save what a backend printed on an attempt that gave no answer. Without this,
/// a failure is one word (`empty`, `fatal`) and the next one is just as opaque.
/// Best effort: a write error here must never change the result of the call.
fn keep_raw(
    dir: &std::path::Path,
    backend: &str,
    model: &str,
    ecls: ErrorClass,
    detail: &str,
    exit_code: Option<i32>,
    output: &str,
) {
    use std::time::{SystemTime, UNIX_EPOCH};
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{ms}-{backend}-{}.txt", ecls.as_str()));
    let body = format!(
        "backend: {backend}\nmodel: {model}\nerror_class: {}\ndetail: {detail}\n\
         exit_code: {exit_code:?}\n--- output ({} bytes) ---\n{output}\n",
        ecls.as_str(),
        output.len()
    );
    if std::fs::write(&path, body).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    // The file names start with a millisecond stamp, so name order is age order.
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    files.sort();
    let extra = files.len().saturating_sub(RAW_KEEP);
    for old in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
}

/// Is `bin` a program we can start? `Config` resolves each backend to an absolute
/// path when it finds it on `PATH`, and leaves the bare name when it does not.
pub fn installed(bin: &str) -> bool {
    let p = std::path::Path::new(bin);
    p.is_absolute() && p.exists()
}

/// What the caller should run next after a failed ladder. This line is how an
/// agent that never reads the README finds the diagnostic commands.
pub fn next_step(attempts: &[AttemptRec]) -> String {
    let unknown: Vec<&AttemptRec> = attempts
        .iter()
        .filter(|a| a.error_class == ErrorClass::ModelUnknown)
        .collect();
    let missing: Vec<&str> = attempts
        .iter()
        .filter(|a| a.error_class == ErrorClass::NotInstalled)
        .map(|a| a.backend)
        .collect();
    if let Some(b) = missing.first() {
        return format!(
            "the `{b}` CLI is not installed or not on PATH: choose a backend you have with \
             `--backend` (agy, claude, codex) or `gander --reconfigure`, or set GANDER_{}_BIN",
            b.to_uppercase()
        );
    }
    match unknown.first() {
        Some(a) => format!(
            "{} does not know the model {:?}: run `gander --models --backend {}` to list \
             the models it offers",
            a.backend, a.model, a.backend
        ),
        None => "Run `gander --check --deep` to test each media type on your configured \
                 backends"
            .to_string(),
    }
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
    // A missing CLI is the first thing a new user hits. Say so plainly, before
    // the runner turns it into an opaque exec error.
    if !installed(&argv[0]) {
        let detail = format!("{} not found", argv[0]);
        let att = AttemptRec {
            backend: be.name(),
            model: model.to_string(),
            error_class: ErrorClass::NotInstalled,
            error_detail: detail.clone(),
            elapsed_s: 0.0,
            answer_len: 0,
            chunk,
        };
        return OneRun {
            att,
            stdout: String::new(),
            ecls: ErrorClass::NotInstalled,
            detail,
            vision_only: !be.supports_audio(),
        };
    }
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
    if ecls != ErrorClass::Ok {
        if let Some(dir) = &cfg.raw_dir {
            keep_raw(dir, be.name(), model, ecls, &detail, raw.exit_code, &clean);
        }
    }
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
    let error = format!("all backends exhausted: {trail}. {}", next_step(&attempts));
    BackendResult {
        ok: false,
        backend: None,
        model: None,
        answer: String::new(),
        vision_only: false,
        attempts,
        aborted_auth: false,
        error,
    }
}

#[cfg(test)]
mod model_unknown_tests {
    use super::*;

    fn class(text: &str, answer_found: bool) -> ErrorClass {
        classify_error(text, Some(0), false, answer_found).0
    }

    #[test]
    fn each_harness_rejection_is_model_unknown() {
        // Captured from the real CLIs. All three exit 0.
        let agy = "error: invalid model selection (--model \"bogus\" --effort \"\"): model \
                   bogus is not recognized as a known model or custom model in settings\n\
                   Available models:\n  Gemini 4 Neon (High)\n";
        let claude = "[claude-code:unrecognized_model] {\"model\":\"bogus\"}";
        let codex = "ERROR: The 'gpt-5.5' model is not supported when using Codex with a \
                     ChatGPT account.";
        for out in [agy, claude, codex] {
            assert_eq!(class(out, false), ErrorClass::ModelUnknown, "{out}");
        }
    }

    #[test]
    fn a_real_answer_beats_a_model_warning() {
        // codex warns about missing metadata for a new model and still answers.
        let out = "warning: unknown model metadata\n===GANDER-BEGIN===";
        assert_eq!(class(out, true), ErrorClass::Ok);
        assert_eq!(class("", false), ErrorClass::Empty);
    }

    #[test]
    fn keep_raw_writes_the_output_and_prunes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..RAW_KEEP + 3 {
            keep_raw(
                dir.path(),
                "agy",
                "some-model",
                ErrorClass::Empty,
                "no answer block in output",
                Some(0),
                &format!("output number {i}"),
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        assert_eq!(files.len(), RAW_KEEP);
        let newest = std::fs::read_to_string(files.last().unwrap()).unwrap();
        assert!(newest.contains(&format!("output number {}", RAW_KEEP + 2)));
        assert!(newest.contains("error_class: empty"));
        assert!(newest.contains("model: some-model"));
    }

    #[test]
    fn next_step_names_the_models_command_for_an_unknown_model() {
        let att = |cls| AttemptRec {
            backend: "codex",
            model: "gpt-5.5".into(),
            error_class: cls,
            error_detail: String::new(),
            elapsed_s: 1.0,
            answer_len: 0,
            chunk: None,
        };
        let s = next_step(&[att(ErrorClass::ModelUnknown)]);
        assert!(s.contains("gander --models --backend codex"), "{s}");
        assert!(s.contains("gpt-5.5"), "{s}");
        let s = next_step(&[att(ErrorClass::Timeout)]);
        assert!(s.contains("gander --check --deep"), "{s}");
        let s = next_step(&[att(ErrorClass::NotInstalled)]);
        assert!(
            s.contains("not installed") && s.contains("GANDER_CODEX_BIN"),
            "{s}"
        );
        assert!(!installed("surely-not-a-real-binary"));
    }
}
