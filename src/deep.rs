//! `--check --deep` — run three tiny bundled media files through the real
//! describe path and check the answers.
//!
//! `--check` alone sends a text prompt, so it proves a login and nothing more.
//! The media types fail independently: audio can be dead while images work, and
//! a fallback backend with no audio support then returns a confident `partial`
//! with no transcript. So the deep probe does not trust `status`. It checks the
//! content: the image and the video show a number, the audio and the video say a
//! sentence, and the answer must contain them.
//!
//! The probe uses the same primary and fallback as a real call (flags, then
//! `GANDER_*` env, then the config file), but it tests each of the two alone.
//! It never uses the cache, and it has its own temp database, so it leaves
//! nothing behind in `recall`. What a backend printed on a failed attempt is
//! kept in `~/.gander/probe-raw` (or `GANDER_RAW_DIR`).
//!
//! The files in `assets/probe/` are generated with ffmpeg alone (see the README
//! there), so they are free to redistribute.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::backend::{backend_for, BackendKind};
use crate::config::Config;
use crate::core::{self, DescribeOptions};
use crate::envelope::{MediaResult, Status};

const IMAGE: &[u8] = include_bytes!("../assets/probe/probe.png");
const AUDIO: &[u8] = include_bytes!("../assets/probe/probe.m4a");
const VIDEO: &[u8] = include_bytes!("../assets/probe/probe.mp4");

/// The number drawn on the image and on every video frame.
const SEEN_MARK: &str = "4172";
/// The sentence spoken in the audio file and in the video.
const SPOKEN: &str = "this is the gander audio probe the code word is copper seven four";
/// Share of the spoken words a transcript must contain. Below 1.0 on purpose:
/// a model that mishears one word of synthetic speech still has working audio.
const HEARD_MIN: f64 = 0.6;
/// One retry: a single empty answer from a harness is common and means nothing.
const TRIES: u32 = 2;
/// Seconds to wait before the retry.
const RETRY_WAIT_S: f64 = 45.0;

pub const KINDS: [&str; 3] = ["image", "audio", "video"];

struct Probe {
    kind: &'static str,
    file: &'static str,
    bytes: &'static [u8],
    must_see: bool,
    must_hear: bool,
}

const PROBES: [Probe; 3] = [
    Probe {
        kind: "image",
        file: "gander-probe.png",
        bytes: IMAGE,
        must_see: true,
        must_hear: false,
    },
    Probe {
        kind: "audio",
        file: "gander-probe.m4a",
        bytes: AUDIO,
        must_see: false,
        must_hear: true,
    },
    Probe {
        kind: "video",
        file: "gander-probe.mp4",
        bytes: VIDEO,
        must_see: true,
        must_hear: true,
    },
];

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Share of the spoken sentence's words that appear in `transcript`.
fn heard_share(transcript: Option<&str>) -> f64 {
    let got = words(transcript.unwrap_or(""));
    let want = words(SPOKEN);
    let hit = want.iter().filter(|w| got.contains(w)).count();
    hit as f64 / want.len() as f64
}

/// Does any text field of the answer contain the number shown in the picture?
fn sees_mark(r: &MediaResult) -> bool {
    r.to_json_value().to_string().contains(SEEN_MARK)
}

/// One backend and model to probe on its own.
struct Rung {
    /// `primary` or `fallback`.
    role: &'static str,
    backend: String,
    model: String,
    can_hear: bool,
}

/// The primary and the fallback of `opts`, as two separate single-try rungs.
///
/// A real call only reaches the fallback when the primary fails, so a probe that
/// follows the same path never tests the fallback while the primary is healthy,
/// and hides a dead primary when the fallback can cover for it. Each rung is
/// therefore probed alone.
fn rungs(opts: &DescribeOptions, cfg: &Config) -> Vec<Rung> {
    let rung = |role, backend: &str, model: &str| {
        let kind = BackendKind::from_str(backend).unwrap_or(BackendKind::Agy);
        Rung {
            role,
            backend: backend.to_string(),
            model: model.to_string(),
            can_hear: backend_for(kind, cfg).supports_audio(),
        }
    };
    let get = |v: &Option<String>, default: &str| v.clone().unwrap_or_else(|| default.to_string());
    let mut out = vec![rung(
        "primary",
        &get(&opts.backend, "agy"),
        &get(&opts.model, &cfg.model_default),
    )];
    let (fb, fm) = (
        get(&opts.fallback_backend, "agy"),
        get(&opts.fallback_model, "flash"),
    );
    let same = fb == out[0].backend && fm == out[0].model;
    if fb != "none" && fm != "none" && !same {
        out.push(rung("fallback", &fb, &fm));
    }
    out
}

/// `(ok, sees, hears, why_not)` for one answer. `can_hear` is false for a
/// backend with no audio support: its video answer is judged on the picture only.
fn judge(p: &Probe, r: &MediaResult, can_hear: bool) -> (bool, Option<bool>, Option<bool>, String) {
    let sees = p.must_see.then(|| sees_mark(r));
    let hears =
        (p.must_hear && can_hear).then(|| heard_share(r.transcript.as_deref()) >= HEARD_MIN);
    if r.status == Status::Failed {
        let why = r.error.clone().unwrap_or_else(|| "failed".into());
        return (false, sees, hears, why);
    }
    let mut missing = Vec::new();
    if sees == Some(false) {
        missing.push(format!(
            "the answer does not mention the number {SEEN_MARK} shown in the picture"
        ));
    }
    if hears == Some(false) {
        missing.push(match r.transcript.as_deref() {
            None | Some("") => "no transcript: the backend did not process the audio".to_string(),
            Some(_) => "the transcript does not match the spoken sentence".to_string(),
        });
    }
    (missing.is_empty(), sees, hears, missing.join("; "))
}

/// The `.txt` files in the raw directory.
fn raw_files(cfg: &Config) -> Vec<PathBuf> {
    let Some(dir) = &cfg.raw_dir else {
        return Vec::new();
    };
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect()
}

/// Probe one media file on one rung, with one retry after a pause.
fn run_probe(p: &Probe, path: &Path, rung: &Rung, opts: &DescribeOptions, cfg: &Config) -> Value {
    if !rung.can_hear && !p.must_see {
        return json!({
            "ok": true,
            "skipped": true,
            "backend": rung.backend,
            "model": rung.model,
            "detail": format!("{} cannot process audio", rung.backend),
        });
    }
    let mut opts = opts.clone();
    opts.backend = Some(rung.backend.clone());
    opts.model = Some(rung.model.clone());
    opts.fallback_model = Some("none".into());
    opts.fallback_backend = Some("none".into());

    let before = raw_files(cfg);
    let started = Instant::now();
    let mut tries = 0;
    loop {
        tries += 1;
        let r = core::describe_media(&path.display().to_string(), &opts, cfg);
        let (ok, sees, hears, why) = judge(p, &r, rung.can_hear);
        let error_class = r
            .backend
            .attempts
            .last()
            .and_then(|a| a.error_class.clone());
        if !ok && tries < TRIES && worth_a_retry(error_class.as_deref()) {
            std::thread::sleep(retry_wait());
            continue;
        }
        let note = (p.must_hear && !rung.can_hear)
            .then(|| format!("speech not checked: {} cannot process audio", rung.backend));
        // What the backend printed on the attempts that gave no answer.
        let raw: Vec<String> = if ok {
            Vec::new()
        } else {
            let mut new: Vec<PathBuf> = raw_files(cfg)
                .into_iter()
                .filter(|f| !before.contains(f))
                .collect();
            new.sort();
            new.iter().map(|f| f.display().to_string()).collect()
        };
        return json!({
            "ok": ok,
            "skipped": false,
            "backend": rung.backend,
            "model": rung.model,
            "model_used": r.backend.model_used,
            "status": r.status.as_str(),
            "error_class": error_class,
            "sees": sees,
            "hears": hears,
            "transcript": r.transcript,
            "detail": why,
            "note": note,
            "tries": tries,
            "took_s": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "raw": raw,
        });
    }
}

/// A retry cannot fix a missing CLI, a dead login, or an unknown model name.
fn worth_a_retry(error_class: Option<&str>) -> bool {
    !matches!(
        error_class,
        Some("not_installed" | "auth" | "model_unknown")
    )
}

/// The pause before the retry (`GANDER_PROBE_RETRY_WAIT_S`, default 45). An
/// empty answer is often a short rate limit; an instant retry hits it again.
fn retry_wait() -> Duration {
    let s = std::env::var("GANDER_PROBE_RETRY_WAIT_S")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(RETRY_WAIT_S);
    Duration::from_secs_f64(s)
}

/// Fold the rows of one media type into its verdict. It fails when a tested
/// rung fails, and also when no rung could be tested at all.
fn verdict(kind: &str, rows: Vec<(&'static str, Value)>) -> Value {
    let flag = |v: &Value, k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let tested = rows.iter().filter(|(_, v)| !flag(v, "skipped")).count();
    let all_ok = rows.iter().all(|(_, v)| flag(v, "ok"));
    let mut out = serde_json::Map::new();
    out.insert("ok".into(), json!(all_ok && tested > 0));
    if tested == 0 {
        out.insert(
            "detail".into(),
            json!(format!("no configured backend can process {kind}")),
        );
    }
    for (role, row) in rows {
        out.insert(role.into(), row);
    }
    Value::Object(out)
}

/// Run the probes named in `kinds` (any of `image`, `audio`, `video`), in order,
/// on the primary and on the fallback.
pub fn deep_probe(kinds: &[String], opts: &DescribeOptions, cfg: &Config) -> Value {
    let mut out = serde_json::Map::new();
    let dir = match tempfile::Builder::new().prefix("gander-probe-").tempdir() {
        Ok(d) => d,
        Err(e) => {
            for k in kinds {
                out.insert(
                    k.clone(),
                    json!({ "ok": false, "detail": format!("no temp dir: {e}") }),
                );
            }
            return Value::Object(out);
        }
    };
    // The probe must test the backends, not the cache or the path rules: fresh
    // database, no allowed-root (the probe files live in a temp dir), no cache.
    let mut cfg = cfg.clone();
    cfg.db_path = dir.path().join("probe.db");
    cfg.allowed_root = None;
    if cfg.raw_dir.is_none() {
        cfg.raw_dir = dirs::home_dir().map(|h| h.join(".gander").join("probe-raw"));
    }
    let mut opts = opts.clone();
    opts.force = true;
    opts.ask = None;
    opts.prompt = None;
    opts.want_transcript = true;

    let rungs = rungs(&opts, &cfg);
    for p in PROBES.iter().filter(|p| kinds.iter().any(|k| k == p.kind)) {
        let path = dir.path().join(p.file);
        if let Err(e) = std::fs::write(&path, p.bytes) {
            out.insert(
                p.kind.to_string(),
                json!({ "ok": false, "detail": format!("could not write the probe file: {e}") }),
            );
            continue;
        }
        let rows = rungs
            .iter()
            .map(|r| (r.role, run_probe(p, &path, r, &opts, &cfg)))
            .collect();
        out.insert(p.kind.to_string(), verdict(p.kind, rows));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(status: Status, description: &str, transcript: Option<&str>) -> MediaResult {
        let mut r = MediaResult::failed(None, String::new(), "backend", String::new(), "image");
        r.status = status;
        r.error = None;
        r.description = description.to_string();
        r.transcript = transcript.map(str::to_string);
        r
    }

    #[test]
    fn probe_files_are_bundled_and_small() {
        for p in &PROBES {
            assert!(p.bytes.len() > 1000, "{} is empty", p.kind);
            assert!(p.bytes.len() < 64 * 1024, "{} is too big to bundle", p.kind);
        }
        assert!(IMAGE.starts_with(b"\x89PNG"));
    }

    #[test]
    fn image_needs_the_number_in_the_answer() {
        let p = &PROBES[0];
        let seen = result(Status::Ok, "White text reads GANDER PROBE 4172.", None);
        assert!(judge(p, &seen, true).0);
        // An answer that never saw the file: a confident description, no number.
        let blind = result(Status::Ok, "A blue rectangle.", None);
        let (ok, sees, _, why) = judge(p, &blind, true);
        assert!(!ok);
        assert_eq!(sees, Some(false));
        assert!(why.contains("4172"));
    }

    #[test]
    fn audio_needs_a_matching_transcript_not_a_good_status() {
        let p = &PROBES[1];
        let exact = "This is the gander audio probe. The code word is copper seven four.";
        assert!(judge(p, &result(Status::Ok, "", Some(exact)), true).0);
        // One misheard word is still working audio.
        let near = "This is the Gander audio probe. The code word is kappa seven four.";
        assert!(judge(p, &result(Status::Ok, "", Some(near)), true).0);
        // The trap: a vision-only fallback answers `partial` with no transcript.
        let (ok, _, hears, why) = judge(p, &result(Status::Partial, "Audio file.", None), true);
        assert!(!ok);
        assert_eq!(hears, Some(false));
        assert!(why.contains("no transcript"));
        // A transcript of something else.
        let other = result(Status::Ok, "", Some("Hello there, how are you today?"));
        assert!(!judge(p, &other, true).0);
    }

    #[test]
    fn video_needs_both() {
        let p = &PROBES[2];
        let t = "This is the gander audio probe. The code word is copper seven four.";
        let both = result(Status::Ok, "Text: GANDER PROBE 4172", Some(t));
        let silent = result(Status::Partial, "Text: GANDER PROBE 4172", None);
        let blind = result(Status::Ok, "A blue screen.", Some(t));
        assert!(judge(p, &both, true).0);
        assert!(!judge(p, &silent, true).0);
        assert!(!judge(p, &blind, true).0);
        // A backend with no audio support is judged on the picture only.
        let (ok, sees, hears, _) = judge(p, &silent, false);
        assert!(ok);
        assert_eq!((sees, hears), (Some(true), None));
        assert!(!judge(p, &blind, false).0);
    }

    #[test]
    fn a_failed_status_reports_its_error() {
        let mut r = result(Status::Failed, "", None);
        r.error = Some("all backends exhausted: agy/x:empty".into());
        let (ok, _, _, why) = judge(&PROBES[0], &r, true);
        assert!(!ok);
        assert!(why.contains("all backends exhausted"));
    }

    fn opts(b: &str, m: &str, fb: &str, fm: &str) -> DescribeOptions {
        DescribeOptions {
            backend: Some(b.into()),
            model: Some(m.into()),
            fallback_backend: Some(fb.into()),
            fallback_model: Some(fm.into()),
            ..Default::default()
        }
    }

    #[test]
    fn primary_and_fallback_are_separate_rungs() {
        let cfg = Config::load(&Default::default());
        let r = rungs(&opts("agy", "pro", "claude", "opus"), &cfg);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[0].role, r[0].backend.as_str(), r[0].can_hear),
            ("primary", "agy", true)
        );
        assert_eq!(
            (r[1].role, r[1].backend.as_str(), r[1].can_hear),
            ("fallback", "claude", false)
        );
        // No fallback, or a fallback equal to the primary: one rung.
        assert_eq!(rungs(&opts("agy", "pro", "none", "flash"), &cfg).len(), 1);
        assert_eq!(rungs(&opts("agy", "pro", "agy", "none"), &cfg).len(), 1);
        assert_eq!(rungs(&opts("agy", "pro", "agy", "pro"), &cfg).len(), 1);
    }

    #[test]
    fn audio_on_a_backend_that_cannot_hear_is_skipped_not_run() {
        let cfg = Config::load(&Default::default());
        let r = rungs(&opts("claude", "sonnet", "none", "none"), &cfg);
        // No backend call happens here: the skip is decided before the call.
        let row = run_probe(
            &PROBES[1],
            Path::new("/nonexistent"),
            &r[0],
            &opts("claude", "sonnet", "none", "none"),
            &cfg,
        );
        assert_eq!(row["skipped"], true);
        assert_eq!(row["ok"], true);
    }

    #[test]
    fn only_failures_that_can_pass_next_time_are_retried() {
        for c in ["empty", "timeout", "capacity", "transient", "fatal"] {
            assert!(worth_a_retry(Some(c)), "{c}");
        }
        // No error class: the backend answered, but the content was wrong.
        assert!(worth_a_retry(None));
        for c in ["not_installed", "auth", "model_unknown"] {
            assert!(!worth_a_retry(Some(c)), "{c}");
        }
    }

    #[test]
    fn verdict_fails_on_a_failed_rung_and_when_nothing_was_tested() {
        let ok = json!({ "ok": true, "skipped": false });
        let bad = json!({ "ok": false, "skipped": false });
        let skip = json!({ "ok": true, "skipped": true });
        let v = verdict(
            "audio",
            vec![("primary", ok.clone()), ("fallback", skip.clone())],
        );
        assert_eq!(v["ok"], true);
        // The trap the probe exists for: a dead primary next to a fallback.
        let v = verdict("audio", vec![("primary", bad), ("fallback", ok)]);
        assert_eq!(v["ok"], false);
        let v = verdict("audio", vec![("primary", skip)]);
        assert_eq!(v["ok"], false);
        assert!(v["detail"]
            .as_str()
            .unwrap()
            .contains("no configured backend"));
    }
}
