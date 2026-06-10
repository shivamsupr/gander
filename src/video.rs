//! Video tiering + chunk orchestration + deterministic merge.
//!
//! `route_video` picks the tier from ffprobe duration, builds the frame/audio
//! extraction + backend calls, runs the ladder per chunk, and (for long video)
//! merges per-chunk results: deterministic structured/transcript fusion + ONE cheap
//! Flash prose-synthesis call. Returns a single `BackendResult` re-emitted as one
//! synthetic sentinel block, so `core.rs` handles every tier like a single call.
//!
//! NOTE: chunks run sequentially here. Bounded concurrency (std::thread + mpsc) is a
//! mechanical follow-up — the merge fold is order-independent and unaffected.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::backend::{run_ladder, AttemptRec, BackendKind, BackendResult, MediaCall, Rung};
use crate::config::{Config, FLASH, SENTINEL_BEGIN, SENTINEL_END, SYNTH_MODEL};
use crate::core::DescribeOptions;
use crate::envelope::{Structured, Technical};
use crate::ffmpeg::{self, ProbeInfo};
use crate::parse::{self, ParsedResult};
use crate::prompt::{build_merge_prompt, build_prompt, PromptKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoTier {
    Direct,
    SingleBatch,
    Chunked,
}

pub struct VideoOutcome {
    pub result: BackendResult,
    pub tier: VideoTier,
    pub chunk_count: u32,
    pub warnings: Vec<String>,
}

struct ChunkPiece {
    index: u32,
    t_start: f64,
    t_end: f64,
    parsed: Option<ParsedResult>,
    failed: bool,
}

impl ChunkPiece {
    fn range_label(&self) -> String {
        format!("{}-{}", fmt_ts(self.t_start), fmt_ts(self.t_end))
    }
}

/// Tier selection (half-open, no overlap).
pub fn pick_tier(duration: Option<f64>, cfg: &Config) -> VideoTier {
    match duration {
        None => VideoTier::Direct, // ffprobe couldn't read duration → let the backend handle it
        Some(d) if d < cfg.direct_max_s => VideoTier::Direct,
        Some(d) if d <= cfg.batch_max_s => VideoTier::SingleBatch,
        Some(_) => VideoTier::Chunked,
    }
}

fn fmt_ts(seconds: f64) -> String {
    let s = seconds.round() as i64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn frames_block(frame_paths: &[PathBuf]) -> String {
    frame_paths
        .iter()
        .map(|p| format!("- {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(clippy::too_many_arguments)]
fn build_frames_call(
    add_dir: &str,
    frame_paths: &[PathBuf],
    audio_path: Option<&Path>,
    offset_label: &str,
    want_transcript: bool,
    translate: bool,
    has_audio: bool,
) -> MediaCall {
    let bulleted = frames_block(frame_paths);

    let full = build_prompt(
        PromptKind::VideoFrames,
        want_transcript && audio_path.is_some(),
        translate,
        Some(offset_label),
    )
    .replace("FRAME_PATHS", &bulleted)
    .replace(
        "MEDIA_PATH",
        &audio_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no audio track)".into()),
    );

    // claude is vision-only: never transcribe, never reads audio.
    let vision = build_prompt(
        PromptKind::VideoFrames,
        false,
        translate,
        Some(offset_label),
    )
    .replace("FRAME_PATHS", &bulleted)
    .replace("MEDIA_PATH", "(no audio available to vision-only backend)");

    MediaCall {
        kind: "video",
        add_dir: add_dir.to_string(),
        prompt_full: full,
        prompt_vision: vision,
        has_audio,
    }
}

fn extract_frames(
    src: &Path,
    out_dir: &Path,
    duration: f64,
    src_fps: Option<f64>,
    cfg: &Config,
    max_frames: u32,
    fps: Option<f64>,
) -> Vec<PathBuf> {
    let (n, mode) = ffmpeg::plan_frame_count(duration, src_fps, max_frames, fps);
    if mode == "fps" {
        ffmpeg::extract_frames_fps(src, out_dir, fps.unwrap_or(1.0), n, &cfg.ffmpeg_bin)
    } else {
        ffmpeg::extract_frames_even(src, out_dir, duration, n, &cfg.ffmpeg_bin)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn route_video(
    spath: &Path,
    parent: &Path,
    probe: &ProbeInfo,
    cfg: &Config,
    work: &Path,
    opts: &DescribeOptions,
    primary: &Rung,
    fallback: Option<&Rung>,
    max_frames: u32,
) -> VideoOutcome {
    let d = probe.duration;
    let tier = pick_tier(d, cfg);
    let mut warnings: Vec<String> = Vec::new();
    let want_transcript = opts.want_transcript;
    let translate = opts.translate;
    let fps = opts.fps;
    let chunk_length = opts.chunk_length.unwrap_or(cfg.chunk_len_s);
    let max_chunks = opts.max_chunks.unwrap_or(cfg.max_chunks);

    match tier {
        // ---------------- DIRECT (<30s) ----------------
        VideoTier::Direct => {
            // The backend reads the whole file natively; claude (if reached) gets frames.
            let claude_frames = extract_frames(
                spath,
                work,
                d.unwrap_or(1.0),
                probe.src_fps,
                cfg,
                max_frames.min(6),
                fps,
            );
            let full = build_prompt(PromptKind::VideoDirect, want_transcript, translate, None)
                .replace("MEDIA_PATH", &spath.display().to_string());
            let vision = build_prompt(PromptKind::VideoFrames, false, translate, Some("00:00"))
                .replace("FRAME_PATHS", &frames_block(&claude_frames))
                .replace("MEDIA_PATH", "(no audio available to vision-only backend)");
            let call = MediaCall {
                kind: "video",
                add_dir: parent.display().to_string(),
                prompt_full: full,
                prompt_vision: vision,
                has_audio: probe.has_audio_stream,
            };
            let br = run_ladder(&call, primary, fallback, cfg, None);
            VideoOutcome {
                result: br,
                tier,
                chunk_count: 1,
                warnings,
            }
        }

        // ---------------- SINGLE BATCH (30..60s) ----------------
        VideoTier::SingleBatch => {
            let dur = d.unwrap_or(cfg.batch_max_s);
            let frames = extract_frames(spath, work, dur, probe.src_fps, cfg, max_frames, fps);
            let audio = if probe.has_audio_stream {
                ffmpeg::extract_audio(spath, work, &cfg.ffmpeg_bin)
            } else {
                None
            };
            let call = build_frames_call(
                &work.display().to_string(),
                &frames,
                audio.as_deref(),
                "00:00",
                want_transcript,
                translate,
                probe.has_audio_stream,
            );
            let br = run_ladder(&call, primary, fallback, cfg, None);
            VideoOutcome {
                result: br,
                tier,
                chunk_count: 1,
                warnings,
            }
        }

        // ---------------- CHUNKED (>60s) ----------------
        VideoTier::Chunked => {
            // Widen chunk length so total chunks <= max_chunks (widen, do not truncate).
            let mut eff_chunk_len = chunk_length;
            if let Some(d) = d {
                if (d / chunk_length).ceil() as u32 > max_chunks {
                    eff_chunk_len = (d / max_chunks as f64).ceil();
                    warnings.push(format!(
                        "chunk_len_widened: {}s -> {}s to fit --max-chunks {max_chunks}",
                        chunk_length as i64, eff_chunk_len as i64
                    ));
                }
            }

            let mut seg_paths = ffmpeg::segment_video(spath, work, eff_chunk_len, &cfg.ffmpeg_bin);
            if seg_paths.is_empty() {
                // segmentation failed → degrade to a DIRECT-style call on the whole file.
                warnings.push("segmentation_failed: analyzed whole file directly".into());
                let full = build_prompt(PromptKind::VideoDirect, want_transcript, translate, None)
                    .replace("MEDIA_PATH", &spath.display().to_string());
                let call = MediaCall {
                    kind: "video",
                    add_dir: parent.display().to_string(),
                    prompt_full: full.clone(),
                    prompt_vision: full,
                    has_audio: probe.has_audio_stream,
                };
                let br = run_ladder(&call, primary, fallback, cfg, None);
                return VideoOutcome {
                    result: br,
                    tier,
                    chunk_count: 1,
                    warnings,
                };
            }
            if seg_paths.len() as u32 > max_chunks {
                seg_paths.truncate(max_chunks as usize);
            }

            let chunk_count = seg_paths.len() as u32;
            let mut pieces: Vec<ChunkPiece> = Vec::new();
            let mut all_attempts: Vec<AttemptRec> = Vec::new();
            let mut any_vision_only = false;
            let mut offset = 0.0_f64;

            for (idx, cpath) in seg_paths.iter().enumerate() {
                let idx = idx as u32;
                let cprobe = ffmpeg::ffprobe(cpath, &cfg.ffprobe_bin);
                let cdur = cprobe.duration.unwrap_or(eff_chunk_len);
                let t_start = offset;
                let t_end = offset + cdur;
                offset = t_end;

                let sub = work.join(format!("c{idx:03}"));
                let _ = std::fs::create_dir_all(&sub);
                let frames =
                    extract_frames(cpath, &sub, cdur, cprobe.src_fps, cfg, max_frames, fps);
                let audio = if cprobe.has_audio_stream {
                    ffmpeg::extract_audio(cpath, &sub, &cfg.ffmpeg_bin)
                } else {
                    None
                };
                let call = build_frames_call(
                    &sub.display().to_string(),
                    &frames,
                    audio.as_deref(),
                    &fmt_ts(t_start),
                    want_transcript,
                    translate,
                    cprobe.has_audio_stream,
                );

                let br = run_ladder(&call, primary, fallback, cfg, Some(idx));
                all_attempts.extend(br.attempts.iter().cloned());

                if br.aborted_auth {
                    // auth is global → abort the whole video.
                    return VideoOutcome {
                        result: BackendResult {
                            ok: false,
                            backend: None,
                            model: None,
                            answer: String::new(),
                            vision_only: false,
                            attempts: all_attempts,
                            aborted_auth: true,
                            error: br.error,
                        },
                        tier,
                        chunk_count,
                        warnings,
                    };
                }

                if !br.ok {
                    let ec = br
                        .attempts
                        .last()
                        .map(|a| a.error_class.as_str())
                        .unwrap_or("unknown");
                    warnings.push(format!(
                        "chunk {idx} ({:.0}-{:.0}s) failed: {ec}",
                        t_start, t_end
                    ));
                    pieces.push(ChunkPiece {
                        index: idx,
                        t_start,
                        t_end,
                        parsed: None,
                        failed: true,
                    });
                    continue;
                }

                any_vision_only = any_vision_only || br.vision_only;
                pieces.push(ChunkPiece {
                    index: idx,
                    t_start,
                    t_end,
                    parsed: Some(parse::parse_response(&br.answer)),
                    failed: false,
                });
            }

            let result = merge_chunks(&pieces, all_attempts, any_vision_only, cfg, work);
            VideoOutcome {
                result,
                tier,
                chunk_count,
                warnings,
            }
        }
    }
}

// --------------------------------------------------------------------------- //
// Deterministic merges
// --------------------------------------------------------------------------- //
fn rating_severity(r: &str) -> i32 {
    match r {
        "keep" => 0,
        "cull" => 2,
        _ => 1,
    }
}

fn rating_by_sev(s: i32) -> &'static str {
    match s {
        0 => "keep",
        2 => "cull",
        _ => "review",
    }
}

const AQ_SEVERITY: &[&str] = &[
    "clear",
    "ambient",
    "music_only",
    "muffled",
    "noisy",
    "silent",
    "unclear",
];

fn agree_or(default: &str, vals: &[String]) -> String {
    let u: BTreeSet<&str> = vals
        .iter()
        .map(|s| s.as_str())
        .filter(|v| !v.is_empty() && *v != "unclear")
        .collect();
    match u.len() {
        0 => "unclear".into(),
        1 => u.into_iter().next().unwrap().to_string(),
        _ => default.to_string(),
    }
}

fn merge_structured(pieces: &[ChunkPiece]) -> Structured {
    let good: Vec<&ParsedResult> = pieces.iter().filter_map(|p| p.parsed.as_ref()).collect();
    if good.is_empty() {
        return Structured::default();
    }

    // keywords / dominant_colors: union, dedup (first-seen), capped.
    let keywords = union_capped(good.iter().map(|p| &p.keywords), 15);
    let dominant_colors = union_capped(good.iter().map(|p| &p.dominant_colors), 8);

    let counts: Vec<i64> = good.iter().map(|p| p.people_count).collect();
    let people_count = if counts.contains(&-1) {
        -1
    } else {
        counts.iter().copied().max().unwrap_or(0)
    };

    let max_sev = good
        .iter()
        .map(|p| rating_severity(&p.rating))
        .max()
        .unwrap_or(1);
    let rating = rating_by_sev(max_sev).to_string();
    let cull_reason = if rating == "cull" {
        good.iter()
            .filter(|p| p.rating == "cull" && !p.cull_reason.is_empty())
            .map(|p| p.cull_reason.clone())
            .next()
            .unwrap_or_default()
    } else {
        String::new()
    };

    let lighting = agree_or("varies", &collect(&good, |p| p.lighting.clone()));
    let shot_type = agree_or("varies", &collect(&good, |p| p.shot_type.clone()));
    let time_of_day = agree_or("unclear", &collect(&good, |p| p.time_of_day.clone()));

    let tech = |get: &dyn Fn(&parse::Technical) -> String, default: &str| -> String {
        let mut vals: BTreeSet<String> = good.iter().map(|p| get(&p.technical)).collect();
        vals.remove("unclear");
        match vals.len() {
            0 => "unclear".into(),
            1 => vals.into_iter().next().unwrap(),
            _ => default.into(),
        }
    };
    let technical = Technical {
        focus: tech(&|t| t.focus.clone(), "mixed"),
        exposure: tech(&|t| t.exposure.clone(), "mixed"),
        stability: tech(&|t| t.stability.clone(), "unclear"),
        motion_blur: tech(&|t| t.motion_blur.clone(), "mixed"),
    };

    // audio_quality: worst (most-degraded non-unclear) wins; silent only if all silent.
    let aq: Vec<String> = good
        .iter()
        .map(|p| p.audio_quality.clone())
        .filter(|s| !s.is_empty())
        .collect();
    let audio_quality = if aq.is_empty() {
        "unclear".into()
    } else if aq.iter().all(|x| x == "silent") {
        "silent".into()
    } else {
        let ranked: Vec<&String> = aq
            .iter()
            .filter(|x| AQ_SEVERITY.contains(&x.as_str()) && *x != "unclear")
            .collect();
        if let Some(worst) = ranked.iter().max_by_key(|x| {
            AQ_SEVERITY
                .iter()
                .position(|s| *s == x.as_str())
                .unwrap_or(0)
        }) {
            (**worst).clone()
        } else {
            agree_or("unclear", &aq)
        }
    };

    // language: most common non-"none"; confidence low if mixed.
    let langs: Vec<String> = good
        .iter()
        .filter_map(|p| p.language.clone())
        .filter(|l| l != "none")
        .collect();
    let language = mode(&langs);
    let distinct_langs: BTreeSet<&String> =
        good.iter().filter_map(|p| p.language.as_ref()).collect();
    let mixed_lang = distinct_langs.len() > 1;
    let language_confidence = Some(if mixed_lang {
        "low".to_string()
    } else {
        good[0]
            .language_confidence
            .clone()
            .unwrap_or_else(|| "medium".into())
    });

    let palette = good
        .iter()
        .map(|p| p.dominant_color_palette.clone())
        .find(|p| !p.is_empty() && p != "unclear")
        .unwrap_or_else(|| "unclear".into());

    // notable_timestamp: rebased absolute, from the most-severe chunk (ties → earliest).
    let mut sev_pieces: Vec<&ChunkPiece> = pieces.iter().filter(|p| p.parsed.is_some()).collect();
    sev_pieces.sort_by_key(|p| {
        let sev = p
            .parsed
            .as_ref()
            .map(|pr| rating_severity(&pr.rating))
            .unwrap_or(1);
        (-sev, p.index as i64)
    });
    let mut notable = String::new();
    for p in sev_pieces {
        let nts = &p.parsed.as_ref().unwrap().notable_timestamp;
        if !nts.is_empty() {
            notable = rebase_ts(nts, p.t_start);
            break;
        }
    }

    Structured {
        schema_version: 3,
        language: Some(language.clone().unwrap_or_else(|| "none".into())),
        language_confidence,
        has_speech: good.iter().any(|p| p.has_speech),
        rating,
        cull_reason,
        technical,
        lighting,
        time_of_day,
        dominant_color_palette: palette,
        dominant_colors,
        audio_quality,
        people_count,
        keywords,
        shot_type,
        notable_timestamp: notable,
    }
}

fn collect(good: &[&ParsedResult], get: impl Fn(&ParsedResult) -> String) -> Vec<String> {
    good.iter().map(|p| get(p)).collect()
}

fn union_capped<'a>(lists: impl Iterator<Item = &'a Vec<String>>, cap: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for list in lists {
        for item in list {
            let s = item.trim().to_lowercase();
            if !s.is_empty() && seen.insert(s.clone()) {
                out.push(s);
                if out.len() >= cap {
                    return out;
                }
            }
        }
    }
    out
}

fn mode(items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut best: Option<(&String, usize)> = None;
    for it in items {
        let count = items.iter().filter(|x| *x == it).count();
        match best {
            Some((_, c)) if c >= count => {}
            _ => best = Some((it, count)),
        }
    }
    best.map(|(s, _)| s.clone())
}

fn rebase_ts(rel: &str, t_start: f64) -> String {
    let Some((mm, ss)) = rel.split_once(':') else {
        return String::new();
    };
    match (mm.parse::<i64>(), ss.parse::<i64>()) {
        (Ok(m), Ok(s)) => fmt_ts((m * 60 + s) as f64 + t_start.round()),
        _ => String::new(),
    }
}

fn merge_transcript(pieces: &[ChunkPiece]) -> (Option<String>, Option<String>) {
    let mut ordered: Vec<&ChunkPiece> = pieces.iter().collect();
    ordered.sort_by_key(|p| p.index);
    let mut t_lines = Vec::new();
    let mut x_lines = Vec::new();
    let mut any_speech = false;
    for p in ordered {
        let rng = format!("[{}]", p.range_label());
        match &p.parsed {
            None => t_lines.push(format!("{rng} (segment unavailable)")),
            Some(_) if p.failed => t_lines.push(format!("{rng} (segment unavailable)")),
            Some(pr) => match &pr.transcript {
                Some(t) => {
                    any_speech = true;
                    t_lines.push(format!("{rng} {}", t.trim()));
                    if let Some(x) = &pr.english_translation {
                        x_lines.push(format!("{rng} {}", x.trim()));
                    }
                }
                None => t_lines.push(format!("{rng} [no speech detected]")),
            },
        }
    }
    if !any_speech {
        return (None, None);
    }
    (
        Some(t_lines.join("\n")),
        if x_lines.is_empty() {
            None
        } else {
            Some(x_lines.join("\n"))
        },
    )
}

// --------------------------------------------------------------------------- //
// Prose synthesis (one Flash text-only call; deterministic fallback)
// --------------------------------------------------------------------------- //
fn synthesize_prose(pieces: &[ChunkPiece], cfg: &Config, synth_dir: &Path) -> (String, String) {
    let mut ordered: Vec<&ChunkPiece> = pieces.iter().collect();
    ordered.sort_by_key(|p| p.index);

    let mut blocks = Vec::new();
    for p in &ordered {
        let Some(pr) = &p.parsed else { continue };
        if pr.description.trim().is_empty() {
            continue;
        }
        let ydump = format!(
            "rating: {}\nlighting: {}\nshot_type: {}",
            pr.rating, pr.lighting, pr.shot_type
        );
        blocks.push(format!(
            "### Segment [{}]\n```yaml\n{ydump}\n```\n{}\n",
            p.range_label(),
            pr.description.trim()
        ));
    }
    if blocks.is_empty() {
        return (String::new(), "**Scene:** unclear".into());
    }

    let merge_prompt = build_merge_prompt().replace("SEGMENTS_BLOCK", &blocks.join("\n"));
    let _ = SYNTH_MODEL; // == FLASH
    let call = MediaCall {
        kind: "video",
        add_dir: synth_dir.display().to_string(),
        prompt_full: merge_prompt.clone(),
        prompt_vision: merge_prompt,
        has_audio: false,
    };
    // Synthesis is always agy/Flash, regardless of the user's backend choice.
    let synth_rung = Rung {
        kind: BackendKind::Agy,
        model: FLASH.to_string(),
    };
    let br = run_ladder(&call, &synth_rung, None, cfg, None);
    if br.ok {
        let m = parse::parse_merge_response(&br.answer);
        if m.parse_ok && !m.description.is_empty() {
            return (m.summary, m.description);
        }
    }

    (fallback_summary(&ordered), fallback_prose(&ordered))
}

fn fallback_prose(ordered: &[&ChunkPiece]) -> String {
    let parts: Vec<String> = ordered
        .iter()
        .filter_map(|p| {
            p.parsed
                .as_ref()
                .filter(|pr| !pr.description.trim().is_empty())
                .map(|pr| format!("[{}] {}", p.range_label(), pr.description.trim()))
        })
        .collect();
    if parts.is_empty() {
        "**Scene:** unclear".into()
    } else {
        parts.join("\n\n")
    }
}

fn fallback_summary(ordered: &[&ChunkPiece]) -> String {
    for p in ordered {
        if let Some(pr) = &p.parsed {
            for line in pr.description.lines() {
                if line.trim().to_lowercase().starts_with("**scene:**") {
                    let rest = line
                        .split_once("**Scene:**")
                        .map(|(_, r)| r)
                        .unwrap_or("")
                        .trim();
                    return rest.chars().take(160).collect();
                }
            }
        }
    }
    "Multi-segment video.".into()
}

// --------------------------------------------------------------------------- //
// merge_chunks → one synthetic sentinel block
// --------------------------------------------------------------------------- //
fn merge_chunks(
    pieces: &[ChunkPiece],
    attempts: Vec<AttemptRec>,
    any_vision_only: bool,
    cfg: &Config,
    synth_dir: &Path,
) -> BackendResult {
    if !pieces.iter().any(|p| p.parsed.is_some()) {
        let trail = pieces
            .iter()
            .map(|p| format!("chunk{}:failed", p.index))
            .collect::<Vec<_>>()
            .join(";");
        return BackendResult {
            ok: false,
            backend: None,
            model: None,
            answer: String::new(),
            vision_only: false,
            attempts,
            aborted_auth: false,
            error: format!("all chunks failed: {trail}"),
        };
    }

    let structured = merge_structured(pieces);
    let (transcript, translation) = merge_transcript(pieces);
    let (summary, prose) = synthesize_prose(pieces, cfg, synth_dir);

    let yaml_block = serde_yaml::to_string(&structured)
        .unwrap_or_else(|_| "{}".into())
        .trim_end()
        .to_string();
    let summary_line = if summary.is_empty() {
        String::new()
    } else {
        format!("**Summary:** {summary}\n\n")
    };
    let answer = format!(
        "{begin}\n```yaml\n{yaml_block}\n```\n\n## Description\n{summary_line}{prose}\n\n\
         ```transcript\n{}\n```\n\n```translation\n{}\n```\n{end}\n",
        transcript.as_deref().unwrap_or("[no speech detected]"),
        translation.as_deref().unwrap_or("[not applicable]"),
        begin = SENTINEL_BEGIN,
        end = SENTINEL_END,
    );

    BackendResult {
        ok: true,
        backend: Some(if any_vision_only { "claude" } else { "agy" }),
        model: Some(if any_vision_only {
            "(merged: claude vision-only)".to_string()
        } else {
            "(merged: agy chunks + Flash synth)".to_string()
        }),
        answer,
        vision_only: any_vision_only,
        attempts,
        aborted_auth: false,
        error: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigOverrides;

    fn pr(
        rating: &str,
        kws: &[&str],
        people: i64,
        light: &str,
        shot: &str,
        transcript: Option<&str>,
        nts: &str,
    ) -> ParsedResult {
        ParsedResult {
            parse_ok: true,
            schema_version: Some(3),
            language: Some("en".into()),
            language_confidence: Some("high".into()),
            has_speech: transcript.is_some(),
            rating: rating.into(),
            cull_reason: if rating == "cull" {
                "bars".into()
            } else {
                String::new()
            },
            technical: parse::Technical {
                focus: "sharp".into(),
                exposure: "adequate".into(),
                stability: "smooth".into(),
                motion_blur: "clean".into(),
            },
            lighting: light.into(),
            time_of_day: "midday".into(),
            dominant_color_palette: "warm".into(),
            dominant_colors: vec!["amber".into()],
            audio_quality: "clear".into(),
            people_count: people,
            keywords: kws.iter().map(|s| s.to_string()).collect(),
            shot_type: shot.into(),
            notable_timestamp: nts.into(),
            description: "**Scene:** seg".into(),
            transcript: transcript.map(String::from),
            english_translation: None,
            warnings: vec![],
            raw_excerpt: None,
        }
    }

    fn piece(
        index: u32,
        t0: f64,
        t1: f64,
        parsed: Option<ParsedResult>,
        failed: bool,
    ) -> ChunkPiece {
        ChunkPiece {
            index,
            t_start: t0,
            t_end: t1,
            parsed,
            failed,
        }
    }

    #[test]
    fn pick_tier_boundaries() {
        let cfg = Config::load(&ConfigOverrides::default());
        assert_eq!(pick_tier(None, &cfg), VideoTier::Direct);
        assert_eq!(pick_tier(Some(29.9), &cfg), VideoTier::Direct);
        assert_eq!(pick_tier(Some(30.0), &cfg), VideoTier::SingleBatch);
        assert_eq!(pick_tier(Some(60.0), &cfg), VideoTier::SingleBatch);
        assert_eq!(pick_tier(Some(60.1), &cfg), VideoTier::Chunked);
    }

    #[test]
    fn merge_structured_rules() {
        let p0 = piece(
            0,
            0.0,
            60.0,
            Some(pr(
                "keep",
                &["a", "b"],
                2,
                "golden_hour",
                "wide",
                Some("Hello"),
                "00:05",
            )),
            false,
        );
        let p1 = piece(
            1,
            60.0,
            123.0,
            Some(pr("cull", &["b", "c"], 5, "night", "medium", None, "00:10")),
            false,
        );
        let p2 = piece(2, 123.0, 180.0, None, true);
        let ms = merge_structured(&[p0, p1, p2]);
        assert_eq!(ms.rating, "cull"); // most severe
        assert_eq!(ms.cull_reason, "bars");
        assert_eq!(ms.people_count, 5); // max
        assert_eq!(ms.keywords, vec!["a", "b", "c"]); // union, first-seen
        assert_eq!(ms.lighting, "varies"); // disagree
        assert_eq!(ms.shot_type, "varies");
        assert_eq!(ms.notable_timestamp, "01:10"); // cull chunk @60s + rel 00:10
    }

    #[test]
    fn merge_structured_caps_keywords() {
        let many: Vec<String> = (0..40).map(|i| format!("k{i}")).collect();
        let mut p = pr("keep", &[], 1, "golden_hour", "wide", None, "");
        p.keywords = many;
        let ms = merge_structured(&[piece(0, 0.0, 60.0, Some(p), false)]);
        assert!(ms.keywords.len() <= 15);
    }

    #[test]
    fn merge_people_many_dominates() {
        let p0 = piece(
            0,
            0.0,
            60.0,
            Some(pr("keep", &[], 3, "golden_hour", "wide", None, "")),
            false,
        );
        let p1 = piece(
            1,
            60.0,
            120.0,
            Some(pr("keep", &[], -1, "golden_hour", "wide", None, "")),
            false,
        );
        assert_eq!(merge_structured(&[p0, p1]).people_count, -1);
    }

    #[test]
    fn merge_transcript_format() {
        let p0 = piece(
            0,
            0.0,
            60.0,
            Some(pr("keep", &[], 1, "golden_hour", "wide", Some("Hello"), "")),
            false,
        );
        let p1 = piece(
            1,
            60.0,
            123.0,
            Some(pr("keep", &[], 1, "golden_hour", "wide", None, "")),
            false,
        );
        let p2 = piece(2, 123.0, 180.0, None, true);
        let (t, _x) = merge_transcript(&[p0, p1, p2]);
        let t = t.unwrap();
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "[00:00-01:00] Hello");
        assert_eq!(lines[1], "[01:00-02:03] [no speech detected]");
        assert_eq!(lines[2], "[02:03-03:00] (segment unavailable)");
    }

    #[test]
    fn merge_transcript_none_when_silent() {
        let p0 = piece(
            0,
            0.0,
            60.0,
            Some(pr("keep", &[], 1, "golden_hour", "wide", None, "")),
            false,
        );
        assert_eq!(merge_transcript(&[p0]), (None, None));
    }
}
