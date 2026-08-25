//! `describe_media()` — the orchestration pipeline.
//!
//! Owns validate → hash → cache lookup → probe → kind/tier routing → backend ladder
//! → parse → status decision → cache write. Never panics across the boundary.
//! Image/audio run one `run_ladder` call; video routes through `video::route_video`.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::backend::{
    model_for, run_ladder, AttemptRec, BackendKind, BackendResult, ErrorClass, MediaCall, Rung,
};
use crate::config::Config;
use crate::db;
use crate::envelope::{
    Attempt, BackendInfo, MediaKind, MediaMeta, MediaResult, Status, Structured, Technical,
};
use crate::ffmpeg::{self, ProbeInfo, WorkDir};
use crate::multi;
use crate::parse::{self, ParsedResult};
use crate::prompt::{build_prompt, PromptKind};
use crate::source::{self, SourceError};
use crate::video;
use rusqlite::Connection;

/// Knobs from the CLI describe form.
#[derive(Debug, Clone, Default)]
pub struct DescribeOptions {
    pub model: Option<String>,
    pub backend: Option<String>,
    pub fallback_model: Option<String>,
    pub fallback_backend: Option<String>,
    pub force: bool,
    pub want_transcript: bool,
    pub translate: bool,
    pub max_frames: Option<u32>,
    pub fps: Option<f64>,
    pub chunk_length: Option<f64>,
    pub max_chunks: Option<u32>,
    pub max_duration: Option<f64>,
    pub keep_temp: bool,
    /// Free-form caller question, answered as an `**Answer:**` line inside the
    /// Description. Bypasses the cache in both directions: a cached row would not
    /// contain the answer, and an ask-flavored description must not overwrite the
    /// canonical cached one.
    pub ask: Option<String>,
    /// Full prompt override: replaces the standard contract (only the media
    /// location preamble and the sentinel wrapper survive). The raw answer is
    /// returned in `description` with no structured parse. Bypasses the cache
    /// in both directions for the same reason as `ask`.
    pub prompt: Option<String>,
}

/// Resolve the primary + fallback rungs from flags/defaults. Defaults: primary
/// agy/pro, fallback agy/flash. A `none` on either fallback flag disables it.
fn resolve_rungs(opts: &DescribeOptions, cfg: &Config) -> (Rung, Option<Rung>) {
    let primary_kind = opts
        .backend
        .as_deref()
        .and_then(BackendKind::from_str)
        .unwrap_or(BackendKind::Agy);
    let primary_selector = opts
        .model
        .clone()
        .unwrap_or_else(|| cfg.model_default.clone());
    let primary = Rung {
        kind: primary_kind,
        model: model_for(primary_kind, &primary_selector),
    };

    let disabled = opts.fallback_backend.as_deref() == Some("none")
        || opts.fallback_model.as_deref() == Some("none");
    let fallback = if disabled {
        None
    } else {
        let kind = opts
            .fallback_backend
            .as_deref()
            .and_then(BackendKind::from_str)
            .unwrap_or(BackendKind::Agy);
        let selector = opts
            .fallback_model
            .clone()
            .unwrap_or_else(|| "flash".into());
        Some(Rung {
            kind,
            model: model_for(kind, &selector),
        })
    };
    (primary, fallback)
}

fn clamp(v: i64, lo: i64, hi: i64) -> i64 {
    v.max(lo).min(hi)
}

fn is_real_transcript(t: Option<&str>) -> bool {
    match t {
        None => false,
        Some(t) => {
            let t = t.trim();
            !t.is_empty() && t.to_lowercase() != "[no speech detected]"
        }
    }
}

/// Entry point for one OR MANY sources.
///
/// One source keeps the full single-file pipeline (cache, per-kind prompt, video
/// tiers). Two or more are answered by ONE backend call over the whole set, so
/// questions can compare items; the cache is bypassed both ways, because a set
/// answer is not the canonical row for any member.
pub fn describe_many(paths: &[String], opts: &DescribeOptions, cfg: &Config) -> MediaResult {
    match paths {
        [] => MediaResult::failed(None, "no source given".into(), "input", String::new(), "unknown"),
        [one] => describe_media(one, opts, cfg),
        _ => describe_set(paths, opts, cfg),
    }
}

/// The 2+ source path. Every failure here is decided BEFORE any backend call.
fn describe_set(paths: &[String], opts: &DescribeOptions, cfg: &Config) -> MediaResult {
    let max_frames = clamp(opts.max_frames.unwrap_or(cfg.max_frames) as i64, 1, 64) as u32;
    let eff_max_duration = opts.max_duration.or(cfg.max_duration_s);
    let all: Vec<String> = paths.to_vec();

    // 1) validate + hash + probe + sniff every source. Any bad source fails the
    // whole call: a partial set would silently answer a different question.
    let mut items: Vec<multi::MultiItem> = Vec::with_capacity(paths.len());
    for (i, raw) in paths.iter().enumerate() {
        let spath = match source::validate_path(raw, cfg.allowed_root.as_deref(), None) {
            Ok(p) => p,
            Err(e) => return with_sources(failed_from_source(raw, &e), &all),
        };
        let sha = match source::sha256_file(&spath) {
            Ok(s) => s,
            Err(e) => {
                return with_sources(
                    MediaResult::failed(
                        Some(spath.display().to_string()),
                        format!("hash failed: {e}"),
                        "unreadable",
                        String::new(),
                        "unknown",
                    ),
                    &all,
                )
            }
        };
        let probe = ffmpeg::ffprobe(&spath, &cfg.ffprobe_bin);
        let kind = match source::sniff_kind(&spath, &probe) {
            Ok(k) => k,
            Err(e) => return with_sources(failed_from_source(raw, &e), &all),
        };

        // A video that would normally be chunked cannot ride in a shared call.
        if kind == MediaKind::Video {
            if let Some(limit) = eff_max_duration {
                if probe.duration.unwrap_or(0.0) > limit {
                    return with_sources(
                        multi_failed(
                            &spath,
                            format!(
                                "duration {:.1}s exceeds --max-duration {limit:.0}s",
                                probe.duration.unwrap_or(0.0)
                            ),
                            "too_long",
                        ),
                        &all,
                    );
                }
            }
            if video::pick_tier(probe.duration, cfg) == video::VideoTier::Chunked {
                return with_sources(
                    multi_failed(
                        &spath,
                        format!(
                            "video longer than {:.0}s cannot be combined with other media: \
                             it needs chunking, which is one call per chunk",
                            cfg.batch_max_s
                        ),
                        "input",
                    ),
                    &all,
                );
            }
        }

        items.push(multi::MultiItem {
            label: multi::label_for(i),
            kind,
            path: spath,
            sha,
            probe,
        });
    }

    // 2) per-kind caps.
    if let Err(msg) = multi::check_caps(&items) {
        return with_sources(
            MediaResult::failed(
                items.first().map(|i| i.path.display().to_string()),
                msg,
                "input",
                String::new(),
                "multi",
            ),
            &all,
        );
    }

    // 3) set identity: the member hashes, in label order.
    let set_sha = source::sha256_of_hashes(&items.iter().map(|i| i.sha.clone()).collect::<Vec<_>>());

    let work = match WorkDir::new(opts.keep_temp, cfg.allowed_root.as_deref()) {
        Ok(w) => w,
        Err(e) => {
            return with_sources(
                MediaResult::failed(
                    items.first().map(|i| i.path.display().to_string()),
                    format!("could not create work dir: {e}"),
                    "input",
                    set_sha,
                    "multi",
                ),
                &all,
            )
        }
    };

    // 4) one call over the whole set.
    let mc = multi::build_call(&items, work.path(), cfg, opts, max_frames);
    let (primary, fallback) = resolve_rungs(opts, cfg);
    let br = run_ladder(&mc.call, &primary, fallback.as_ref(), cfg, None);

    let mut res = finish_set(&items, &set_sha, br, opts, mc.any_audio, mc.warnings);
    res.sources = items.iter().map(|i| i.path.display().to_string()).collect();
    res
}

/// Assemble the set result. Shares the parse + status rules with `finish()`;
/// only the media block differs, since no single ffprobe describes a set.
fn finish_set(
    items: &[multi::MultiItem],
    set_sha: &str,
    br: BackendResult,
    opts: &DescribeOptions,
    any_audio: bool,
    warnings: Vec<String>,
) -> MediaResult {
    let first = items[0].path.clone();
    let backend_info = BackendInfo {
        model_used: br.model.clone().unwrap_or_default(),
        backend_used: br.backend.map(String::from).unwrap_or_default(),
        attempts: br.attempts.iter().map(attempt_view).collect(),
    };
    // media{} describes ONE file, so a set only claims what holds for all of it.
    let media = MediaMeta {
        has_audio: any_audio,
        chunk_count: items.len() as u32,
        ..MediaMeta::default()
    };

    let mut base = MediaResult {
        status: Status::Ok,
        content_sha256: set_sha.to_string(),
        media_kind: "multi".to_string(),
        error: None,
        error_class: None,
        warnings,
        parse_ok: false,
        cached: false,
        summary: String::new(),
        description: String::new(),
        transcript: None,
        language: None,
        english_translation: None,
        structured: Some(Structured::default()),
        media,
        backend: backend_info,
        source_path: Some(first.display().to_string()),
        sources: Vec::new(),
        schema_version: crate::envelope::SCHEMA_VERSION.to_string(),
        tool_version: crate::envelope::TOOL_VERSION.to_string(),
    };

    if !br.ok {
        base.status = Status::Failed;
        base.error = Some(br.error);
        base.error_class = Some(if br.aborted_auth { "auth" } else { "backend" }.to_string());
        return base;
    }

    // --prompt: caller-defined reply shape, so no envelope parse (see finish()).
    if opts.prompt.is_some() {
        let answer =
            parse::extract_answer(&br.answer).unwrap_or_else(|| br.answer.trim().to_string());
        base.warnings
            .push("prompt_override: standard report skipped; raw answer in description".into());
        base.parse_ok = !answer.is_empty();
        if answer.is_empty() {
            base.status = Status::Failed;
            base.error = Some("prompt_override: empty answer".into());
            base.error_class = Some("backend".into());
        }
        base.summary = derive_summary(&answer);
        base.description = answer;
        return base;
    }

    let parsed = parse::parse_response(&br.answer);
    // decide_status only reads has_audio_stream off the probe, and treats Image
    // as never speech-capable — which is exactly right for an all-image set.
    let synthetic = ProbeInfo {
        has_audio_stream: any_audio,
        ..ProbeInfo::default()
    };
    let kind = if items.iter().all(|i| i.kind == MediaKind::Image) {
        MediaKind::Image
    } else {
        MediaKind::Video
    };
    let (status, transcript, language, translation, extra) =
        decide_status(kind, &synthetic, opts.want_transcript, &parsed, br.vision_only);
    base.warnings.extend(parsed.warnings.clone());
    base.warnings.extend(extra);

    if status == Status::Failed {
        let first_w = base.warnings.first().cloned().unwrap_or_else(|| "unknown".into());
        base.error = Some(format!("parse_failed: {first_w}"));
        base.error_class = Some("backend".to_string());
    }
    base.status = status;
    base.parse_ok = parsed.parse_ok;
    base.summary = derive_summary(&parsed.description);
    base.description = parsed.description.clone();
    base.transcript = transcript;
    base.language = language;
    base.english_translation = translation;
    base.structured = Some(structured_from_parsed(&parsed));
    base
}

fn multi_failed(spath: &Path, msg: String, class: &str) -> MediaResult {
    MediaResult::failed(
        Some(spath.display().to_string()),
        format!("{}: {msg}", spath.display()),
        class,
        String::new(),
        "multi",
    )
}

/// Failures before the call still report every source that was asked for.
fn with_sources(mut r: MediaResult, all: &[String]) -> MediaResult {
    r.sources = all.to_vec();
    r
}

pub fn describe_media(path: &str, opts: &DescribeOptions, cfg: &Config) -> MediaResult {
    let max_frames = clamp(opts.max_frames.unwrap_or(cfg.max_frames) as i64, 1, 64) as u32;
    let eff_max_duration = opts.max_duration.or(cfg.max_duration_s);

    // Cache connection — failures here must never sink the analysis (cache-less run).
    let mut conn: Option<Connection> = db::connect(&cfg.db_path, cfg.db_busy_timeout_ms).ok();

    // 1) validate + hash (no download).
    let spath = match source::validate_path(path, cfg.allowed_root.as_deref(), None) {
        Ok(p) => p,
        Err(e) => return failed_from_source(path, &e),
    };
    let sha = match source::sha256_file(&spath) {
        Ok(s) => s,
        Err(e) => {
            return MediaResult::failed(
                Some(spath.display().to_string()),
                format!("hash failed: {e}"),
                "unreadable",
                String::new(),
                "unknown",
            )
        }
    };

    // 2) cache lookup — return BEFORE probe on a hit (instant $0 read).
    if !opts.force && opts.ask.is_none() && opts.prompt.is_none() {
        if let Some(c) = &conn {
            if let Some(mut hit) = db::lookup(c, &sha) {
                hit.source_path = Some(spath.display().to_string());
                hit.sources = vec![spath.display().to_string()];
                return hit;
            }
        }
    }

    // 3) probe + sniff.
    let probe = ffmpeg::ffprobe(&spath, &cfg.ffprobe_bin);
    let kind = match source::sniff_kind(&spath, &probe) {
        Ok(k) => k,
        Err(e) => return failed_from_source(path, &e),
    };

    // 4a) chunked video cannot honor a raw prompt override: the merge synthesis
    // only sees per-chunk text, and returning the standard report instead would
    // silently ignore the caller. Hard reject before any backend call.
    if kind == MediaKind::Video
        && opts.prompt.is_some()
        && video::pick_tier(probe.duration, cfg) == video::VideoTier::Chunked
    {
        return MediaResult::failed(
            Some(spath.display().to_string()),
            format!(
                "prompt_not_supported: --prompt requires a single backend call; videos longer \
                 than {:.0}s are chunked",
                cfg.batch_max_s
            ),
            "input",
            sha,
            "video",
        );
    }

    // 4) max-duration hard reject (video only), before any backend call.
    if kind == MediaKind::Video {
        if let (Some(limit), Some(dur)) = (eff_max_duration, probe.duration) {
            if dur > limit {
                let mut res = MediaResult::failed(
                    Some(spath.display().to_string()),
                    format!("duration {dur:.1}s exceeds --max-duration {limit:.0}s"),
                    "too_long",
                    sha.clone(),
                    "video",
                );
                res.media = media_meta(kind, &probe, &spath, false, 0);
                maybe_write(&mut conn, &mut res, opts.force);
                return res;
            }
        }
    }

    let work = match WorkDir::new(opts.keep_temp, cfg.allowed_root.as_deref()) {
        Ok(w) => w,
        Err(e) => {
            return MediaResult::failed(
                Some(spath.display().to_string()),
                format!("could not create work dir: {e}"),
                "input",
                sha,
                kind.as_str(),
            )
        }
    };

    let mut warnings: Vec<String> = Vec::new();
    let (primary, fallback) = resolve_rungs(opts, cfg);
    let parent = spath.parent().unwrap_or(&spath).to_path_buf();

    let (br, chunk_count, chunked) = match kind {
        MediaKind::Image => {
            let call = image_call(&spath, cfg, work.path(), opts, &mut warnings)
                .with_ask(opts.ask.as_deref());
            (
                run_ladder(&call, &primary, fallback.as_ref(), cfg, None),
                0,
                false,
            )
        }
        MediaKind::Audio => {
            let call = audio_call(&spath, &probe, opts).with_ask(opts.ask.as_deref());
            (
                run_ladder(&call, &primary, fallback.as_ref(), cfg, None),
                1,
                false,
            )
        }
        MediaKind::Video => {
            let outcome = video::route_video(
                &spath,
                &parent,
                &probe,
                cfg,
                work.path(),
                opts,
                &primary,
                fallback.as_ref(),
                max_frames,
            );
            warnings.extend(outcome.warnings);
            (
                outcome.result,
                outcome.chunk_count,
                outcome.tier == video::VideoTier::Chunked,
            )
        }
    };

    let mut res = finish(
        &spath,
        &sha,
        &probe,
        kind,
        br,
        opts.want_transcript,
        chunk_count,
        chunked,
        warnings,
        opts.prompt.is_some(),
    );
    if opts.ask.is_none() && opts.prompt.is_none() {
        maybe_write(&mut conn, &mut res, opts.force);
    }
    res
}

/// Persist the result; a cache-write failure must never sink the analysis.
fn maybe_write(conn: &mut Option<Connection>, res: &mut MediaResult, force: bool) {
    if let Some(c) = conn {
        if let Err(e) = db::upsert(c, res, force) {
            res.warnings.push(format!("cache_write_failed: {e}"));
        }
    }
}

fn image_call(
    spath: &Path,
    cfg: &Config,
    work: &Path,
    opts: &DescribeOptions,
    warnings: &mut Vec<String>,
) -> MediaCall {
    let (img_path, add_dir) = match ffmpeg::sanitize_image(spath, work, &cfg.ffmpeg_bin) {
        Some(sanitized) => (sanitized.display().to_string(), work.display().to_string()),
        None => {
            warnings.push("gps-strip-skipped: could not re-encode image; original used".into());
            (
                spath.display().to_string(),
                spath.parent().unwrap_or(spath).display().to_string(),
            )
        }
    };
    let prompt = match &opts.prompt {
        Some(p) => crate::prompt::build_override_prompt(PromptKind::Image, p),
        None => build_prompt(PromptKind::Image, true, true, None),
    }
    .replace("MEDIA_PATH", &img_path);
    MediaCall {
        kind: "image",
        add_dir,
        prompt_full: prompt.clone(),
        prompt_vision: prompt,
        has_audio: false,
    }
}

fn audio_call(spath: &Path, probe: &ProbeInfo, opts: &DescribeOptions) -> MediaCall {
    let path = spath.display().to_string();
    let (full, vision) = match &opts.prompt {
        Some(p) => {
            let o = crate::prompt::build_override_prompt(PromptKind::Audio, p)
                .replace("MEDIA_PATH", &path);
            (o.clone(), o)
        }
        None => {
            let full = build_prompt(
                PromptKind::Audio,
                opts.want_transcript,
                opts.translate,
                None,
            )
            .replace("MEDIA_PATH", &path);
            // claude cannot hear audio → vision-only floor produces empty visual fields.
            let vision = build_prompt(PromptKind::Audio, false, opts.translate, None)
                .replace("MEDIA_PATH", &path);
            (full, vision)
        }
    };
    MediaCall {
        kind: "audio",
        add_dir: spath.parent().unwrap_or(spath).display().to_string(),
        prompt_full: full,
        prompt_vision: vision,
        has_audio: probe.has_audio_stream,
    }
}

// --------------------------------------------------------------------------- //
// Finish: parse + status + assemble
// --------------------------------------------------------------------------- //
#[allow(clippy::too_many_arguments)]
fn finish(
    spath: &Path,
    sha: &str,
    probe: &ProbeInfo,
    kind: MediaKind,
    br: BackendResult,
    want_transcript: bool,
    chunk_count: u32,
    chunked: bool,
    warnings: Vec<String>,
    prompt_override: bool,
) -> MediaResult {
    let media = media_meta(kind, probe, spath, chunked, chunk_count);
    let backend_info = BackendInfo {
        model_used: br.model.clone().unwrap_or_default(),
        backend_used: br.backend.map(String::from).unwrap_or_default(),
        attempts: br.attempts.iter().map(attempt_view).collect(),
    };

    if !br.ok {
        let error_class = if br.aborted_auth { "auth" } else { "backend" };
        return MediaResult {
            status: Status::Failed,
            content_sha256: sha.to_string(),
            media_kind: kind.as_str().to_string(),
            error: Some(br.error),
            error_class: Some(error_class.to_string()),
            warnings,
            parse_ok: false,
            cached: false,
            summary: String::new(),
            description: String::new(),
            transcript: None,
            language: None,
            english_translation: None,
            structured: Some(Structured::default()),
            media,
            backend: backend_info,
            source_path: Some(spath.display().to_string()),
            sources: vec![spath.display().to_string()],
            schema_version: crate::envelope::SCHEMA_VERSION.to_string(),
            tool_version: crate::envelope::TOOL_VERSION.to_string(),
        };
    }

    // --prompt override: the reply shape is caller-defined, so the envelope
    // parse and status rules do not apply. Return the sentinel-sliced answer
    // verbatim as the description.
    if prompt_override {
        let answer = parse::extract_answer(&br.answer)
            .unwrap_or_else(|| br.answer.trim().to_string());
        let mut all_warnings = warnings;
        all_warnings
            .push("prompt_override: standard report skipped; raw answer in description".into());
        let summary = derive_summary(&answer);
        return MediaResult {
            status: if answer.is_empty() {
                Status::Failed
            } else {
                Status::Ok
            },
            content_sha256: sha.to_string(),
            media_kind: kind.as_str().to_string(),
            error: if answer.is_empty() {
                Some("prompt_override: empty answer".into())
            } else {
                None
            },
            error_class: if answer.is_empty() {
                Some("backend".to_string())
            } else {
                None
            },
            warnings: all_warnings,
            parse_ok: !answer.is_empty(),
            cached: false,
            summary,
            description: answer,
            transcript: None,
            language: None,
            english_translation: None,
            structured: Some(Structured::default()),
            media,
            backend: backend_info,
            source_path: Some(spath.display().to_string()),
            sources: vec![spath.display().to_string()],
            schema_version: crate::envelope::SCHEMA_VERSION.to_string(),
            tool_version: crate::envelope::TOOL_VERSION.to_string(),
        };
    }

    let parsed = parse::parse_response(&br.answer);
    let (status, transcript, language, translation, extra) =
        decide_status(kind, probe, want_transcript, &parsed, br.vision_only);
    let mut all_warnings = warnings;
    all_warnings.extend(parsed.warnings.clone());
    all_warnings.extend(extra);

    let structured = structured_from_parsed(&parsed);
    let summary = derive_summary(&parsed.description);

    let (error, error_class) = if status == Status::Failed {
        let first = all_warnings
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".into());
        (
            Some(format!("parse_failed: {first}")),
            Some("backend".to_string()),
        )
    } else {
        (None, None)
    };

    MediaResult {
        status,
        content_sha256: sha.to_string(),
        media_kind: kind.as_str().to_string(),
        error,
        error_class,
        warnings: all_warnings,
        parse_ok: parsed.parse_ok,
        cached: false,
        summary,
        description: parsed.description.clone(),
        transcript,
        language,
        english_translation: translation,
        structured: Some(structured),
        media,
        backend: backend_info,
        source_path: Some(spath.display().to_string()),
        sources: vec![spath.display().to_string()],
        schema_version: crate::envelope::SCHEMA_VERSION.to_string(),
        tool_version: crate::envelope::TOOL_VERSION.to_string(),
    }
}

fn attempt_view(a: &AttemptRec) -> Attempt {
    let ok = a.error_class == ErrorClass::Ok;
    Attempt {
        backend: a.backend.to_string(),
        model: a.model.clone(),
        ok,
        error_class: if ok {
            None
        } else {
            Some(a.error_class.as_str().to_string())
        },
        elapsed_s: a.elapsed_s,
        chunk: a.chunk,
    }
}

fn media_meta(
    kind: MediaKind,
    probe: &ProbeInfo,
    path: &Path,
    chunked: bool,
    chunk_count: u32,
) -> MediaMeta {
    let (duration, codec, has_audio, width, height) = match kind {
        MediaKind::Image => (None, None, false, probe.width, probe.height),
        MediaKind::Audio => (
            probe.duration,
            probe.audio_codec.clone(),
            probe.has_audio_stream,
            None,
            None,
        ),
        MediaKind::Video => (
            probe.duration,
            probe.video_codec.clone(),
            probe.has_audio_stream,
            probe.width,
            probe.height,
        ),
    };
    let size = std::fs::metadata(path).ok().map(|m| m.len());
    MediaMeta {
        duration,
        width,
        height,
        codec,
        has_audio,
        size_bytes: size,
        chunked,
        chunk_count,
    }
}

fn structured_from_parsed(p: &ParsedResult) -> Structured {
    Structured {
        schema_version: p.schema_version.unwrap_or(3),
        language: p.language.clone(),
        language_confidence: p.language_confidence.clone(),
        has_speech: p.has_speech,
        rating: p.rating.clone(),
        cull_reason: p.cull_reason.clone(),
        technical: Technical {
            focus: p.technical.focus.clone(),
            exposure: p.technical.exposure.clone(),
            stability: p.technical.stability.clone(),
            motion_blur: p.technical.motion_blur.clone(),
        },
        lighting: p.lighting.clone(),
        time_of_day: p.time_of_day.clone(),
        dominant_color_palette: p.dominant_color_palette.clone(),
        dominant_colors: p.dominant_colors.clone(),
        audio_quality: p.audio_quality.clone(),
        people_count: p.people_count,
        keywords: p.keywords.clone(),
        shot_type: p.shot_type.clone(),
        notable_timestamp: p.notable_timestamp.clone(),
    }
}

fn derive_summary(description: &str) -> String {
    if description.is_empty() {
        return String::new();
    }
    if let Some(c) = summary_re().captures(description) {
        return truncate(c[1].trim(), 200);
    }
    if let Some(c) = scene_re().captures(description) {
        let scene = c[1].trim();
        let sent = split_first_sentence(scene);
        return truncate(sent.trim(), 200);
    }
    truncate(description.trim().lines().next().unwrap_or(""), 200)
}

fn split_first_sentence(s: &str) -> &str {
    // Split after the first sentence-ending punctuation followed by whitespace.
    let re = sentence_re();
    if let Some(m) = re.find(s) {
        &s[..m.start() + 1]
    } else {
        s
    }
}

fn decide_status(
    kind: MediaKind,
    probe: &ProbeInfo,
    want_transcript: bool,
    parsed: &ParsedResult,
    vision_only: bool,
) -> (
    Status,
    Option<String>,
    Option<String>,
    Option<String>,
    Vec<String>,
) {
    let mut extra: Vec<String> = Vec::new();
    let has_audio = probe.has_audio_stream;
    let speech_capable = kind != MediaKind::Image && has_audio && want_transcript;

    let mut transcript = parsed.transcript.clone();
    let mut translation = parsed.english_translation.clone();
    let mut language = parsed.language.clone();

    if vision_only && speech_capable {
        transcript = None;
        translation = None;
        language = None;
        extra.push("transcript unavailable: claude is vision-only (no audio modality)".into());
    }

    let desc_ok = !parsed.description.trim().is_empty();
    if !desc_ok {
        return (Status::Failed, transcript, language, translation, extra);
    }

    let transcript_present = is_real_transcript(transcript.as_deref());
    if transcript.is_none() {
        language = None;
        translation = None;
    }

    let na = kind == MediaKind::Image || !has_audio || !want_transcript;

    if vision_only && speech_capable {
        return (Status::Partial, transcript, language, translation, extra);
    }
    if !parsed.parse_ok {
        extra.push("parse_incomplete: structured fields may be defaulted".into());
        return (Status::Partial, transcript, language, translation, extra);
    }
    if transcript_present || na {
        return (Status::Ok, transcript, language, translation, extra);
    }
    extra.push("empty_transcript: speech-capable media produced no transcript".into());
    (Status::Partial, transcript, language, translation, extra)
}

// --------------------------------------------------------------------------- //
// Failure helpers + regexes
// --------------------------------------------------------------------------- //
fn failed_from_source(path: &str, e: &SourceError) -> MediaResult {
    MediaResult::failed(
        Some(path.to_string()),
        e.message.clone(),
        &e.error_class,
        String::new(),
        "unknown",
    )
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn summary_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\*\*Summary:\*\*\s*(.+)$").unwrap())
}

fn scene_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\*\*Scene:\*\*\s*(.+)$").unwrap())
}

fn sentence_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[.!?]\s").unwrap())
}
