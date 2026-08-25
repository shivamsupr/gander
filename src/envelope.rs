//! The canonical result object + JSON envelope.
//!
//! `MediaResult` is the domain result; `to_envelope()` is the SINGLE place envelope
//! field names + order are fixed. Every key is ALWAYS present (`null`/`""`/`0`/`[]`
//! for N/A) — enforced here by serde view structs whose field order is the contract.

use serde::{Deserialize, Serialize};

/// Cache-invalidation key (format `YYYY-MM-DD.N`, sortable). Bump with the prompt
/// schema whenever the YAML field set / parse logic changes.
pub const SCHEMA_VERSION: &str = "2026-06-08.1";
/// The int the model echoes inside the YAML block.
pub const STRUCTURED_SCHEMA_INT: i64 = 3;
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Partial,
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Partial => "partial",
            Status::Failed => "failed",
        }
    }
}

/// The four technical sub-fields (mirrors the YAML `technical:` map).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Technical {
    pub focus: String,
    pub exposure: String,
    pub stability: String,
    pub motion_blur: String,
}

impl Default for Technical {
    fn default() -> Self {
        Technical {
            focus: "unclear".into(),
            exposure: "unclear".into(),
            stability: "unclear".into(),
            motion_blur: "unclear".into(),
        }
    }
}

/// Mirrors the YAML schema the model emits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Structured {
    pub schema_version: i64,
    pub language: Option<String>,
    pub language_confidence: Option<String>,
    pub has_speech: bool,
    pub rating: String,
    pub cull_reason: String,
    pub technical: Technical,
    pub lighting: String,
    pub time_of_day: String,
    pub dominant_color_palette: String,
    pub dominant_colors: Vec<String>,
    pub audio_quality: String,
    pub people_count: i64,
    pub keywords: Vec<String>,
    pub shot_type: String,
    pub notable_timestamp: String,
}

impl Default for Structured {
    fn default() -> Self {
        Structured {
            schema_version: STRUCTURED_SCHEMA_INT,
            language: None,
            language_confidence: None,
            has_speech: false,
            rating: "review".into(),
            cull_reason: String::new(),
            technical: Technical::default(),
            lighting: "unclear".into(),
            time_of_day: "unclear".into(),
            dominant_color_palette: "unclear".into(),
            dominant_colors: Vec::new(),
            audio_quality: "unclear".into(),
            people_count: 0,
            keywords: Vec::new(),
            shot_type: "unclear".into(),
            notable_timestamp: String::new(),
        }
    }
}

/// The envelope `media{}` block + ffprobe-derived facts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaMeta {
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
    pub has_audio: bool,
    pub size_bytes: Option<u64>,
    pub chunked: bool,
    pub chunk_count: u32,
}

impl MediaMeta {
    pub fn wxh(&self) -> Option<String> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(format!("{w}x{h}")),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Attempt {
    pub backend: String,
    pub model: String,
    pub ok: bool,
    pub error_class: Option<String>,
    pub elapsed_s: f64,
    pub chunk: Option<u32>,
}

impl Default for Attempt {
    fn default() -> Self {
        Attempt {
            backend: String::new(),
            model: String::new(),
            ok: false,
            error_class: None,
            elapsed_s: 0.0,
            chunk: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendInfo {
    pub model_used: String,
    pub backend_used: String,
    pub attempts: Vec<Attempt>,
}

/// The canonical result. `to_envelope()` renders the JSON contract.
#[derive(Debug, Clone)]
pub struct MediaResult {
    pub status: Status,
    pub content_sha256: String,
    pub media_kind: String,
    pub error: Option<String>,
    pub error_class: Option<String>,
    pub warnings: Vec<String>,
    pub parse_ok: bool,
    pub cached: bool,
    pub summary: String,
    pub description: String,
    pub transcript: Option<String>,
    pub language: Option<String>,
    pub english_translation: Option<String>,
    pub structured: Option<Structured>,
    pub media: MediaMeta,
    pub backend: BackendInfo,
    pub source_path: Option<String>,
    /// Every analyzed source, in label order (`[A]`, `[B]`, …). One entry for a
    /// normal describe; several in multi-media mode, where `source_path` holds
    /// only the first and the description refers to items by label.
    pub sources: Vec<String>,
    pub schema_version: String,
    pub tool_version: String,
}

impl MediaResult {
    /// A clean `failed` envelope (input/probe/backend errors).
    pub fn failed(
        path: Option<String>,
        error: String,
        error_class: &str,
        content_sha256: String,
        media_kind: &str,
    ) -> MediaResult {
        MediaResult {
            status: Status::Failed,
            content_sha256,
            media_kind: media_kind.to_string(),
            error: Some(error),
            error_class: Some(error_class.to_string()),
            warnings: Vec::new(),
            parse_ok: false,
            cached: false,
            summary: String::new(),
            description: String::new(),
            transcript: None,
            language: None,
            english_translation: None,
            structured: None,
            media: MediaMeta::default(),
            backend: BackendInfo::default(),
            sources: path.iter().cloned().collect(),
            source_path: path,
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: TOOL_VERSION.to_string(),
        }
    }

    /// Build the canonical envelope view. `s` is the (possibly defaulted) structured
    /// block, borrowed so it outlives the returned view (failed results have none).
    fn build_envelope<'a>(&'a self, s: &'a Structured) -> Envelope<'a> {
        Envelope {
            status: self.status.as_str(),
            error: self.error.as_deref(),
            warnings: &self.warnings,
            parse_ok: self.parse_ok,
            media_kind: &self.media_kind,
            content_sha256: &self.content_sha256,
            cached: self.cached,
            summary: &self.summary,
            description: &self.description,
            transcript: self.transcript.as_deref(),
            language: self.language.as_deref(),
            english_translation: self.english_translation.as_deref(),
            structured: StructuredView::from(s),
            media: MediaView::from(&self.media),
            backend: BackendView::from(&self.backend),
            schema_version: &self.schema_version,
            tool_version: &self.tool_version,
            sources: &self.sources,
        }
    }

    pub fn to_json_pretty(&self) -> String {
        let s = self.structured.clone().unwrap_or_default();
        serde_json::to_string_pretty(&self.build_envelope(&s)).unwrap_or_else(|_| "{}".to_string())
    }

    /// Owned envelope JSON value (for the DB blob / recall, M4+).
    pub fn to_json_value(&self) -> serde_json::Value {
        let s = self.structured.clone().unwrap_or_default();
        serde_json::to_value(self.build_envelope(&s)).unwrap_or(serde_json::Value::Null)
    }
}

// --------------------------------------------------------------------------- //
// Serde "view" structs — field declaration order IS the envelope contract.
// --------------------------------------------------------------------------- //
#[derive(Serialize)]
pub struct Envelope<'a> {
    pub status: &'a str,
    pub error: Option<&'a str>,
    pub warnings: &'a [String],
    pub parse_ok: bool,
    pub media_kind: &'a str,
    pub content_sha256: &'a str,
    pub cached: bool,
    pub summary: &'a str,
    pub description: &'a str,
    pub transcript: Option<&'a str>,
    pub language: Option<&'a str>,
    pub english_translation: Option<&'a str>,
    pub structured: StructuredView<'a>,
    pub media: MediaView<'a>,
    pub backend: BackendView<'a>,
    pub schema_version: &'a str,
    pub tool_version: &'a str,
    /// Appended AFTER the original keys on purpose: adding a key is additive for
    /// consumers, moving one is not.
    pub sources: &'a [String],
}

#[derive(Serialize)]
pub struct TechnicalView<'a> {
    pub focus: &'a str,
    pub exposure: &'a str,
    pub stability: &'a str,
    pub motion_blur: &'a str,
}

#[derive(Serialize)]
pub struct StructuredView<'a> {
    pub rating: &'a str,
    pub cull_reason: &'a str,
    pub technical: TechnicalView<'a>,
    pub lighting: &'a str,
    pub time_of_day: &'a str,
    pub dominant_color_palette: &'a str,
    pub dominant_colors: &'a [String],
    pub audio_quality: &'a str,
    pub people_count: i64,
    pub keywords: &'a [String],
    pub shot_type: &'a str,
    pub notable_timestamp: &'a str,
}

impl<'a> From<&'a Structured> for StructuredView<'a> {
    fn from(s: &'a Structured) -> Self {
        StructuredView {
            rating: &s.rating,
            cull_reason: &s.cull_reason,
            technical: TechnicalView {
                focus: &s.technical.focus,
                exposure: &s.technical.exposure,
                stability: &s.technical.stability,
                motion_blur: &s.technical.motion_blur,
            },
            lighting: &s.lighting,
            time_of_day: &s.time_of_day,
            dominant_color_palette: &s.dominant_color_palette,
            dominant_colors: &s.dominant_colors,
            audio_quality: &s.audio_quality,
            people_count: s.people_count,
            keywords: &s.keywords,
            shot_type: &s.shot_type,
            notable_timestamp: &s.notable_timestamp,
        }
    }
}

#[derive(Serialize)]
pub struct MediaView<'a> {
    pub duration: Option<f64>,
    pub wxh: Option<String>,
    pub codec: Option<&'a str>,
    pub has_audio: bool,
    pub size: Option<u64>,
    pub chunked: bool,
    pub chunk_count: u32,
}

impl<'a> From<&'a MediaMeta> for MediaView<'a> {
    fn from(m: &'a MediaMeta) -> Self {
        MediaView {
            duration: m.duration,
            wxh: m.wxh(),
            codec: m.codec.as_deref(),
            has_audio: m.has_audio,
            size: m.size_bytes,
            chunked: m.chunked,
            chunk_count: m.chunk_count,
        }
    }
}

#[derive(Serialize)]
pub struct AttemptView<'a> {
    pub chunk: Option<u32>,
    pub model: &'a str,
    pub backend: &'a str,
    pub ok: bool,
    pub error_class: Option<&'a str>,
    pub elapsed_s: f64,
}

#[derive(Serialize)]
pub struct BackendView<'a> {
    pub model_used: &'a str,
    pub backend_used: &'a str,
    pub attempts: Vec<AttemptView<'a>>,
}

impl<'a> From<&'a BackendInfo> for BackendView<'a> {
    fn from(b: &'a BackendInfo) -> Self {
        BackendView {
            model_used: &b.model_used,
            backend_used: &b.backend_used,
            attempts: b
                .attempts
                .iter()
                .map(|a| AttemptView {
                    chunk: a.chunk,
                    model: &a.model,
                    backend: &a.backend,
                    ok: a.ok,
                    error_class: a.error_class.as_deref(),
                    elapsed_s: (a.elapsed_s * 10.0).round() / 10.0,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_top_level_key_present() {
        let r = MediaResult::failed(
            Some("/x.png".into()),
            "boom".into(),
            "input",
            "sha".into(),
            "image",
        );
        let v: serde_json::Value = serde_json::from_str(&r.to_json_pretty()).unwrap();
        for k in [
            "status",
            "error",
            "warnings",
            "parse_ok",
            "media_kind",
            "content_sha256",
            "cached",
            "summary",
            "description",
            "transcript",
            "language",
            "english_translation",
            "structured",
            "media",
            "backend",
            "schema_version",
            "tool_version",
        ] {
            assert!(v.get(k).is_some(), "missing key: {k}");
        }
        for k in [
            "rating",
            "cull_reason",
            "technical",
            "lighting",
            "time_of_day",
            "dominant_color_palette",
            "dominant_colors",
            "audio_quality",
            "people_count",
            "keywords",
            "shot_type",
            "notable_timestamp",
        ] {
            assert!(
                v["structured"].get(k).is_some(),
                "missing structured key: {k}"
            );
        }
        for k in [
            "duration",
            "wxh",
            "codec",
            "has_audio",
            "size",
            "chunked",
            "chunk_count",
        ] {
            assert!(v["media"].get(k).is_some(), "missing media key: {k}");
        }
        for k in ["model_used", "backend_used", "attempts"] {
            assert!(v["backend"].get(k).is_some(), "missing backend key: {k}");
        }
    }

    #[test]
    fn key_order_is_stable() {
        let r = MediaResult::failed(None, "x".into(), "input", String::new(), "image");
        let json = r.to_json_pretty();
        let status_at = json.find("\"status\"").unwrap();
        let media_kind_at = json.find("\"media_kind\"").unwrap();
        let tool_at = json.find("\"tool_version\"").unwrap();
        assert!(status_at < media_kind_at && media_kind_at < tool_at);
    }

    #[test]
    fn wxh_formats_dimensions() {
        let m = MediaMeta {
            width: Some(1280),
            height: Some(720),
            ..MediaMeta::default()
        };
        assert_eq!(m.wxh(), Some("1280x720".to_string()));
        assert_eq!(MediaMeta::default().wxh(), None);
    }
}
