//! Multi-media mode — several sources answered by ONE backend call.
//!
//! A single source keeps its own pipeline (cache, per-kind prompt, video tiers).
//! This module owns everything specific to 2+ sources: the per-kind caps, the
//! materialization of every item under ONE work dir (backends take a single
//! add_dir, and the ffmpeg helpers all write fixed file names, so each item gets
//! its own subdirectory), and the labeled item block the multi prompts wrap.

use std::path::{Path, PathBuf};

use crate::backend::MediaCall;
use crate::config::Config;
use crate::core::DescribeOptions;
use crate::envelope::MediaKind;
use crate::ffmpeg::{self, ProbeInfo};
use crate::prompt;

/// Per-kind caps for one call. A still costs a fraction of what a clip costs, so
/// images are capped far higher than video and audio.
pub const MAX_IMAGES: usize = 10;
pub const MAX_VIDEOS: usize = 2;
pub const MAX_AUDIOS: usize = 2;

/// Frames sampled per video here. Deliberately below the single-video budget:
/// in multi mode every item shares one context window.
const VIDEO_FRAMES: u32 = 6;

/// One validated source, before materialization.
pub struct MultiItem {
    pub label: String,
    pub kind: MediaKind,
    pub path: PathBuf,
    pub sha: String,
    pub probe: ProbeInfo,
}

/// The assembled call plus what went wrong on the way.
pub struct MultiCall {
    pub call: MediaCall,
    pub warnings: Vec<String>,
    pub any_audio: bool,
}

/// Bracket label for position `i`. The caps keep this well inside A..Z.
pub fn label_for(i: usize) -> String {
    match u8::try_from(i) {
        Ok(n) if i < 26 => ((b'A' + n) as char).to_string(),
        _ => format!("S{}", i + 1),
    }
}

/// `Err(message)` when any kind is over its cap.
pub fn check_caps(items: &[MultiItem]) -> Result<(), String> {
    for (kind, max, name) in [
        (MediaKind::Image, MAX_IMAGES, "images"),
        (MediaKind::Video, MAX_VIDEOS, "videos"),
        (MediaKind::Audio, MAX_AUDIOS, "audio files"),
    ] {
        let n = items.iter().filter(|i| i.kind == kind).count();
        if n > max {
            return Err(format!(
                "too many {name}: {n} given, at most {max} per call \
                 (caps: {MAX_IMAGES} images, {MAX_VIDEOS} videos, {MAX_AUDIOS} audio)"
            ));
        }
    }
    Ok(())
}

/// Materialize every item under `work` and build the one call that covers them.
pub fn build_call(
    items: &[MultiItem],
    work: &Path,
    cfg: &Config,
    opts: &DescribeOptions,
    max_frames: u32,
) -> MultiCall {
    let mut warnings: Vec<String> = Vec::new();
    let mut full: Vec<String> = Vec::new();
    let mut vision: Vec<String> = Vec::new();
    let mut any_audio = false;

    for it in items {
        let label = &it.label;
        let dir = work.join(label);
        if std::fs::create_dir_all(&dir).is_err() {
            warnings.push(format!("item_skipped [{label}]: could not create work dir"));
            continue;
        }

        match it.kind {
            MediaKind::Image => {
                let p = match ffmpeg::sanitize_image(&it.path, &dir, &cfg.ffmpeg_bin) {
                    Some(p) => p,
                    None => match copy_into(&it.path, &dir) {
                        Some(p) => {
                            warnings.push(format!(
                                "gps-strip-skipped [{label}]: could not re-encode image; \
                                 original used"
                            ));
                            p
                        }
                        None => {
                            warnings
                                .push(format!("item_skipped [{label}]: could not stage image"));
                            continue;
                        }
                    },
                };
                let block = format!("[{label}] IMAGE\n- {}", p.display());
                full.push(block.clone());
                vision.push(block);
            }

            MediaKind::Video => {
                let dur = it.probe.duration.unwrap_or(1.0);
                let frames = ffmpeg::extract_frames_even(
                    &it.path,
                    &dir,
                    dur,
                    max_frames.min(VIDEO_FRAMES),
                    &cfg.ffmpeg_bin,
                );
                if frames.is_empty() {
                    warnings.push(format!("item_skipped [{label}]: no frames could be extracted"));
                    continue;
                }
                let audio = if it.probe.has_audio_stream {
                    ffmpeg::extract_audio(&it.path, &dir, &cfg.ffmpeg_bin)
                } else {
                    None
                };
                let head = format!(
                    "[{label}] VIDEO ({}, {} frames evenly spaced across it)",
                    fmt_dur(it.probe.duration),
                    frames.len()
                );
                let frame_lines = frames
                    .iter()
                    .map(|p| format!("- {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n");
                match &audio {
                    Some(a) => {
                        any_audio = true;
                        full.push(format!("{head}\n{frame_lines}\n- audio track: {}", a.display()));
                    }
                    None => full.push(format!("{head}\n{frame_lines}\n- audio track: none")),
                }
                // claude is vision-only: it never gets an audio path.
                vision.push(format!(
                    "{head}\n{frame_lines}\n- audio track: not available to this backend"
                ));
            }

            MediaKind::Audio => {
                let Some(p) = copy_into(&it.path, &dir) else {
                    warnings.push(format!("item_skipped [{label}]: could not stage audio"));
                    continue;
                };
                any_audio = true;
                full.push(format!(
                    "[{label}] AUDIO ({})\n- {}",
                    fmt_dur(it.probe.duration),
                    p.display()
                ));
                vision.push(format!(
                    "[{label}] AUDIO — not available to this backend, which cannot hear"
                ));
            }
        }
    }

    let items_full = full.join("\n");
    let items_vision = vision.join("\n");

    let (prompt_full, prompt_vision) = match &opts.prompt {
        Some(p) => (
            prompt::build_multi_override_prompt(&items_full, p),
            prompt::build_multi_override_prompt(&items_vision, p),
        ),
        None => (
            prompt::build_multi_prompt(&items_full, opts.want_transcript && any_audio, opts.translate),
            prompt::build_multi_prompt(&items_vision, false, opts.translate),
        ),
    };

    MultiCall {
        call: MediaCall {
            kind: "multi",
            add_dir: work.display().to_string(),
            prompt_full,
            prompt_vision,
            has_audio: any_audio,
        }
        .with_ask(opts.ask.as_deref()),
        warnings,
        any_audio,
    }
}

/// Copy a source into the work dir so ONE add_dir covers every item.
fn copy_into(src: &Path, dir: &Path) -> Option<PathBuf> {
    let dst = dir.join(src.file_name()?);
    std::fs::copy(src, &dst).ok()?;
    Some(dst)
}

fn fmt_dur(d: Option<f64>) -> String {
    match d {
        Some(d) => {
            let s = d.round() as i64;
            format!("{:02}:{:02}", s / 60, s % 60)
        }
        None => "duration unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: MediaKind, i: usize) -> MultiItem {
        MultiItem {
            label: label_for(i),
            kind,
            path: PathBuf::from("/tmp/x"),
            sha: String::new(),
            probe: ProbeInfo::default(),
        }
    }

    #[test]
    fn labels_are_letters() {
        assert_eq!(label_for(0), "A");
        assert_eq!(label_for(9), "J");
        assert_eq!(label_for(25), "Z");
        assert_eq!(label_for(26), "S27");
    }

    #[test]
    fn caps_allow_the_maximum_and_reject_one_more() {
        let full_set = || {
            let mut v: Vec<MultiItem> =
                (0..MAX_IMAGES).map(|i| item(MediaKind::Image, i)).collect();
            v.extend((0..MAX_VIDEOS).map(|i| item(MediaKind::Video, i)));
            v.extend((0..MAX_AUDIOS).map(|i| item(MediaKind::Audio, i)));
            v
        };
        assert!(check_caps(&full_set()).is_ok(), "a full legal set must pass");

        for (kind, name) in [
            (MediaKind::Image, "images"),
            (MediaKind::Video, "videos"),
            (MediaKind::Audio, "audio files"),
        ] {
            let mut over = full_set();
            over.push(item(kind, 0));
            let err = check_caps(&over).expect_err("one over the cap must fail");
            assert!(err.contains(name), "{err:?} should name {name}");
        }
    }
}
