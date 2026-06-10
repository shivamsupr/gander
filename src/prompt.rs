//! Prompt construction — the single source of truth for text sent to backends.
//!
//! Every prompt (image, audio, video-direct, video-frames, merge) forces the SAME
//! byte-level sentinel envelope, so `parse.rs` never branches on backend or kind.
//!
//! HARD RULE: every template here is APOSTROPHE-FREE. Path placeholders
//! (`MEDIA_PATH`, `FRAME_PATHS`, `OFFSET_LABEL`) are left literal for the caller to
//! substitute, so templates stay path-independent.

use crate::config::{SENTINEL_BEGIN, SENTINEL_END};

/// Bump on ANY change to the YAML field set, fences, or sentinels.
pub const PROMPT_SCHEMA_VERSION: i64 = 3;

/// Kind selector for `build_prompt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Image,
    Audio,
    VideoDirect,
    VideoFrames,
}

const INTRO_IMAGE: &str = "\
The media is a single IMAGE file located at: MEDIA_PATH
Read the image. There is no audio and no motion. Set has_speech to false, language to none,
audio_quality to silent, stability to static, motion_blur to clean, and notable_timestamp to
an empty string. In the transcript block emit [no speech detected] and in the translation
block emit [not applicable]. If the image contains visible written text, report it inside the
Description Subjects line, not in the transcript block. Describe what is visibly present.";

const INTRO_AUDIO: &str = "\
The media is an AUDIO file located at: MEDIA_PATH
Listen to the full audio. There are no visual frames. Set focus, exposure, lighting,
time_of_day, and shot_type to unclear, set dominant_colors to an empty list, and set
people_count to the number of distinct speakers you hear (0 if none, -1 if a crowd).
Transcribe any speech VERBATIM in the original spoken language inside the transcript block
and set language to its ISO 639-1 code. Profile the audio (number of speakers, accent, tone,
background sounds, music) in the Description Subjects and Mood lines. Set notable_timestamp to
the MM:SS of the most salient audio moment, or an empty string. {TRANSCRIPT_CLAUSE}
If there is no speech, set has_speech to false, language to none, and emit [no speech detected].";

const INTRO_VIDEO_DIRECT: &str = "\
The media is a short VIDEO file located at: MEDIA_PATH
Read the video natively. Examine the visual frames across the FULL duration together with the
audio track. If there is spoken language, transcribe it VERBATIM in the original spoken
language inside the transcript block, set has_speech to true, and set language to its
ISO 639-1 code. {TRANSCRIPT_CLAUSE} If there is no speech, set has_speech to false, language
to none, audio_quality to its true value (silent only if there is no audio track at all), and
emit [no speech detected]. Use notable_timestamp to mark the single most salient moment as MM:SS.
Describe how the scene changes over time in the Description Action line.";

const INTRO_VIDEO_FRAMES: &str = "\
The media is a VIDEO segment provided as several still FRAMES plus one AUDIO track.
The frames are evenly spaced in time across the segment, in chronological order:
FRAME_PATHS
The audio track for the SAME segment is located at: MEDIA_PATH
Treat the frames as a time-ordered sample of one continuous clip and the audio as its
soundtrack. Reason about motion and change by comparing consecutive frames. If there is
spoken language on the audio track, transcribe it VERBATIM in the original spoken language
inside the transcript block, set has_speech to true, and set language to its ISO 639-1 code.
{TRANSCRIPT_CLAUSE} If there is no speech, set has_speech to false, language to none, and emit
[no speech detected]. This segment begins at time OFFSET_LABEL in the source video; express
notable_timestamp relative to the start of THIS segment as MM:SS (or an empty string).
Pick the single best concrete value for lighting and shot_type even if they vary slightly.";

fn contract() -> String {
    format!(
        "\
You are a media-understanding analyst. Analyze the media described above and produce ONE
structured report. Output ONLY the report, wrapped EXACTLY between the two sentinel lines
shown below. Do not write anything after the closing sentinel.

Rules:
- Emit the sentinels on their own lines, exactly: {begin} and {end}
- Inside, emit exactly three fenced blocks with these info strings: a yaml block, a
  transcript block, and a translation block, plus one prose section titled ## Description.
- Use the literal word unclear for any field you cannot determine. Never omit a key.
- Keep every value on the field it belongs to. Do not invent people, sounds, or text.
- For people_count use a whole number: 0 if none, the exact count if you can count them,
  or -1 if there are too many to count.
- keywords are 5 to 12 lowercase hyphenated tags. dominant_colors are 3 to 6 lowercase
  color words. Pick ONE concrete value for lighting, shot_type, and time_of_day.
- Apostrophes are allowed inside transcript and translation content only.

Rating guidance:
- keep: technically sound and useful. In focus, well exposed, stable, with a clear subject
  or usable speech. Worth retaining for downstream use without review.
- review: borderline or context-dependent. Partially usable, minor technical issues,
  ambiguous content, or a judgment call a human should make. When in doubt, choose review.
- cull: not useful. Test patterns, accidental captures, severely degraded media, duplicates,
  blank or corrupt frames, or content with no retainable value. Give a one-clause cull_reason.

Emit this exact skeleton, filling in real values:

{begin}
```yaml
schema_version: 3
language: <iso-639-1 code, or none if no speech>
language_confidence: <high|medium|low>
has_speech: <true|false>
rating: <keep|review|cull>
cull_reason: <one short clause, or empty string if not cull>
technical:
  focus: <sharp|soft|out_of_focus|mixed|unclear>
  exposure: <adequate|under|over|strong|unclear>
  stability: <smooth|shaky|handheld|static|unclear>
  motion_blur: <clean|slight|heavy|unclear>
lighting: <bright_daylight|golden_hour|overcast|indoor_artificial|low_light|night|mixed|unclear>
time_of_day: <morning|midday|afternoon|evening|night|golden_hour|unclear>
dominant_color_palette: <one short phrase>
dominant_colors:
- <color word>
- <color word>
- <color word>
audio_quality: <clear|noisy|muffled|music_only|ambient|silent|unclear>
people_count: <integer, 0 if none, -1 if too many to count>
keywords:
- <lowercase-hyphenated-tag>
- <lowercase-hyphenated-tag>
- <lowercase-hyphenated-tag>
- <lowercase-hyphenated-tag>
- <lowercase-hyphenated-tag>
shot_type: <close-up|medium|wide|establishing|aerial|pov|macro|static-portrait|unclear>
notable_timestamp: <MM:SS of the most salient moment, or empty string>
```

## Description

**Scene:** <where this is, setting, environment, one to three sentences>
**Subjects:** <who or what is present, count and appearance>
**Action:** <what happens over time>
**Mood:** <tone, atmosphere>
**Shot type:** <framing and camera behavior in plain words>
**Use cases:** <one to three concrete downstream uses>

```transcript
<verbatim speech in the original language, or the literal text [no speech detected]>
```

```translation
<English translation of the transcript, or the literal text [not applicable]>
```
{end}",
        begin = SENTINEL_BEGIN,
        end = SENTINEL_END,
    )
}

fn transcript_clause(want_transcript: bool, translate: bool) -> &'static str {
    if !want_transcript {
        "Do not transcribe. There is no audio available to you. Emit \
         [transcription disabled] in the transcript block and [not applicable] \
         in the translation block."
    } else if translate {
        "Then provide a faithful English translation in the translation block; \
         if the speech is already English, repeat it there."
    } else {
        "Leave the translation block as [not applicable]."
    }
}

/// Assemble a kind-specific intro + the shared contract.
pub fn build_prompt(
    kind: PromptKind,
    want_transcript: bool,
    translate: bool,
    offset_label: Option<&str>,
) -> String {
    let intro_template = match kind {
        PromptKind::Image => INTRO_IMAGE,
        PromptKind::Audio => INTRO_AUDIO,
        PromptKind::VideoDirect => INTRO_VIDEO_DIRECT,
        PromptKind::VideoFrames => INTRO_VIDEO_FRAMES,
    };

    let mut intro = intro_template.replace(
        "{TRANSCRIPT_CLAUSE}",
        transcript_clause(want_transcript, translate),
    );
    if kind == PromptKind::VideoFrames {
        intro = intro.replace("OFFSET_LABEL", offset_label.unwrap_or("00:00"));
    }

    format!("{}\n\n{}", intro.trim_end(), contract())
}

// --------------------------------------------------------------------------- //
// Chunk-merge synthesis prompt
// --------------------------------------------------------------------------- //
/// The merge-synthesis template. `SEGMENTS_BLOCK` is substituted by the caller.
pub fn build_merge_prompt() -> String {
    format!(
        "\
You are a media-understanding editor. Below are N ordered segment descriptions of ONE
continuous video, in chronological order. Each segment has a time range label, a yaml facts
block, and a prose description. Fuse them into ONE coherent overall description of the whole
video and ONE single-sentence summary. Do not analyze any media yourself; work only from the
segment text given. Do not invent events that are not in the segments. Where segments differ
(lighting, shot type), describe the progression rather than picking one. Refer to time using
the segment range labels when an event is localized.

Output ONLY the report, wrapped EXACTLY between the sentinel lines. Emit a yaml block with a
single key summary (one sentence, at most about 160 characters), then the prose section.
Do not emit a transcript or translation block.

Here are the N ordered segments:

SEGMENTS_BLOCK

Emit exactly this, filling in real values:

{begin}
```yaml
summary: <one sentence overall summary of the whole video>
```

## Description

**Scene:** <overall setting across the whole video>
**Subjects:** <who or what appears across the video>
**Action:** <how the video progresses from start to finish, referencing time ranges>
**Mood:** <overall tone>
**Shot type:** <how framing and camera behavior change across the video>
**Use cases:** <one to three concrete downstream uses>
{end}",
        begin = SENTINEL_BEGIN,
        end = SENTINEL_END,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: &[PromptKind] = &[
        PromptKind::Image,
        PromptKind::Audio,
        PromptKind::VideoDirect,
        PromptKind::VideoFrames,
    ];

    #[test]
    fn templates_apostrophe_free() {
        for &k in KINDS {
            for wt in [true, false] {
                for tr in [true, false] {
                    let p = build_prompt(k, wt, tr, Some("00:00"));
                    assert!(!p.contains('\''), "{k:?} wt={wt} tr={tr} has an apostrophe");
                }
            }
        }
        assert!(!build_merge_prompt().contains('\''));
    }

    #[test]
    fn sentinels_and_fences_present() {
        for &k in KINDS {
            let p = build_prompt(k, true, true, Some("00:00"));
            assert!(p.contains(SENTINEL_BEGIN) && p.contains(SENTINEL_END));
            assert!(
                p.contains("```yaml")
                    && p.contains("```transcript")
                    && p.contains("```translation")
            );
            assert!(p.contains("## Description"));
        }
    }

    #[test]
    fn placeholders_left_for_caller() {
        let vf = build_prompt(PromptKind::VideoFrames, true, true, Some("01:00"));
        assert!(vf.contains("FRAME_PATHS"));
        assert!(vf.contains("MEDIA_PATH"));
        assert!(!vf.contains("OFFSET_LABEL") && vf.contains("01:00"));
        assert!(!vf.contains("{TRANSCRIPT_CLAUSE}"));
    }

    #[test]
    fn no_transcript_clause() {
        let p = build_prompt(PromptKind::VideoFrames, false, true, Some("00:00"));
        assert!(p.contains("transcription disabled"));
    }
}
