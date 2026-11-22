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
//! `GANDER_*` env, then the config file), never the cache, and its own temp
//! database, so it leaves nothing behind in `recall`.
//!
//! The files in `assets/probe/` are generated with ffmpeg alone (see the README
//! there), so they are free to redistribute.

use std::time::Instant;

use serde_json::{json, Value};

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

/// `(ok, sees, hears, why_not)` for one answer.
fn judge(p: &Probe, r: &MediaResult) -> (bool, Option<bool>, Option<bool>, String) {
    let sees = p.must_see.then(|| sees_mark(r));
    let hears = p
        .must_hear
        .then(|| heard_share(r.transcript.as_deref()) >= HEARD_MIN);
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
            None | Some("") => {
                "no transcript: the backend that answered did not process the audio".to_string()
            }
            Some(_) => "the transcript does not match the spoken sentence".to_string(),
        });
    }
    (missing.is_empty(), sees, hears, missing.join("; "))
}

fn run_probe(p: &Probe, dir: &std::path::Path, opts: &DescribeOptions, cfg: &Config) -> Value {
    let path = dir.join(p.file);
    if let Err(e) = std::fs::write(&path, p.bytes) {
        return json!({ "ok": false, "detail": format!("could not write the probe file: {e}") });
    }
    let started = Instant::now();
    let mut tries = 0;
    loop {
        tries += 1;
        let r = core::describe_media(&path.display().to_string(), opts, cfg);
        let (ok, sees, hears, why) = judge(p, &r);
        if ok || tries >= TRIES {
            let attempts: Vec<String> = r
                .backend
                .attempts
                .iter()
                .map(|a| {
                    format!(
                        "{}/{}:{}",
                        a.backend,
                        a.model,
                        a.error_class.as_deref().unwrap_or("ok")
                    )
                })
                .collect();
            return json!({
                "ok": ok,
                "status": r.status.as_str(),
                "backend_used": r.backend.backend_used,
                "model_used": r.backend.model_used,
                "sees": sees,
                "hears": hears,
                "transcript": r.transcript,
                "detail": why,
                "tries": tries,
                "took_s": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
                "attempts": attempts,
            });
        }
    }
}

/// Run the probes named in `kinds` (any of `image`, `audio`, `video`), in order.
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
    let mut opts = opts.clone();
    opts.force = true;
    opts.ask = None;
    opts.prompt = None;
    opts.want_transcript = true;

    for p in PROBES.iter().filter(|p| kinds.iter().any(|k| k == p.kind)) {
        out.insert(p.kind.to_string(), run_probe(p, dir.path(), &opts, &cfg));
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
        assert!(judge(p, &seen).0);
        // An answer that never saw the file: a confident description, no number.
        let blind = result(Status::Ok, "A blue rectangle.", None);
        let (ok, sees, _, why) = judge(p, &blind);
        assert!(!ok);
        assert_eq!(sees, Some(false));
        assert!(why.contains("4172"));
    }

    #[test]
    fn audio_needs_a_matching_transcript_not_a_good_status() {
        let p = &PROBES[1];
        let exact = "This is the gander audio probe. The code word is copper seven four.";
        assert!(judge(p, &result(Status::Ok, "", Some(exact))).0);
        // One misheard word is still working audio.
        let near = "This is the Gander audio probe. The code word is kappa seven four.";
        assert!(judge(p, &result(Status::Ok, "", Some(near))).0);
        // The trap: a vision-only fallback answers `partial` with no transcript.
        let (ok, _, hears, why) = judge(p, &result(Status::Partial, "Audio file.", None));
        assert!(!ok);
        assert_eq!(hears, Some(false));
        assert!(why.contains("no transcript"));
        // A transcript of something else.
        assert!(
            !judge(
                p,
                &result(Status::Ok, "", Some("Hello there, how are you today?"))
            )
            .0
        );
    }

    #[test]
    fn video_needs_both() {
        let p = &PROBES[2];
        let t = "This is the gander audio probe. The code word is copper seven four.";
        assert!(judge(p, &result(Status::Ok, "Text: GANDER PROBE 4172", Some(t))).0);
        assert!(!judge(p, &result(Status::Partial, "Text: GANDER PROBE 4172", None)).0);
        assert!(!judge(p, &result(Status::Ok, "A blue screen.", Some(t))).0);
    }

    #[test]
    fn a_failed_status_reports_its_error() {
        let mut r = result(Status::Failed, "", None);
        r.error = Some("all backends exhausted: agy/x:empty".into());
        let (ok, _, _, why) = judge(&PROBES[0], &r);
        assert!(!ok);
        assert!(why.contains("all backends exhausted"));
    }
}
