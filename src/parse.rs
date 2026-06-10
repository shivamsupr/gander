//! The one tolerant parser: raw backend text → `ParsedResult`.
//!
//! Tolerant by design: any structural failure degrades to `parse_ok=false` with raw
//! text preserved — NEVER a panic. Handles agy PTY dumps (ANSI + chatter + prompt
//! echo) and `claude --output-format json` envelopes.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_yaml::Value as Yaml;

use crate::config::{SENTINEL_BEGIN, SENTINEL_END};

#[derive(Debug, Clone)]
pub struct ParsedResult {
    pub parse_ok: bool,
    pub schema_version: Option<i64>,
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
    pub description: String,
    pub transcript: Option<String>,
    pub english_translation: Option<String>,
    pub warnings: Vec<String>,
    pub raw_excerpt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Technical {
    pub focus: String,
    pub exposure: String,
    pub stability: String,
    pub motion_blur: String,
}

#[derive(Debug, Clone)]
pub struct MergeResult {
    pub summary: String,
    pub description: String,
    pub parse_ok: bool,
    pub warnings: Vec<String>,
}

// --------------------------------------------------------------------------- //
// Step 0 — claude unwrap
// --------------------------------------------------------------------------- //
/// `claude -p ... --output-format json` → `{"type":"result","result":"<text>"}`.
/// agy raw PTY text fails JSON parse and passes through untouched.
pub fn unwrap_claude_json(raw: &str) -> String {
    let s = raw.trim();
    if let Some(text) = try_result(s) {
        return text;
    }
    // tolerate leading non-JSON noise: try the largest {...} span
    if let (Some(a), Some(b)) = (s.find('{'), s.rfind('}')) {
        if b > a {
            if let Some(text) = try_result(&s[a..=b]) {
                return text;
            }
        }
    }
    raw.to_string()
}

fn try_result(s: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    v.get("result")?.as_str().map(String::from)
}

// --------------------------------------------------------------------------- //
// Step 1 — strip ANSI
// --------------------------------------------------------------------------- //
fn ansi_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?s)\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[PX^_].*?\x1b\\",
        )
        .unwrap()
    })
}

pub fn strip_ansi(s: &str) -> String {
    let stripped = ansi_re().replace_all(s, "");
    stripped.replace("\r\n", "\n").replace('\r', "\n")
}

// --------------------------------------------------------------------------- //
// Step 2 — slice sentinels (LAST well-ordered pair)
// --------------------------------------------------------------------------- //
pub fn slice_sentinels(s: &str) -> Option<String> {
    let e = s.rfind(SENTINEL_END)?;
    let b = s[..e].rfind(SENTINEL_BEGIN)?;
    let inner = s[b + SENTINEL_BEGIN.len()..e].trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

pub fn extract_answer(raw: &str) -> Option<String> {
    slice_sentinels(&strip_ansi(&unwrap_claude_json(raw)))
}

// --------------------------------------------------------------------------- //
// Step 3/4 — fence + section extraction
// --------------------------------------------------------------------------- //
pub fn extract_fence(body: &str, info: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r"(?s)```[ \t]*{}[ \t]*\r?\n(.*?)\r?\n```",
        regex::escape(info)
    ))
    .ok()?;
    re.captures(body).map(|c| c[1].to_string())
}

fn extract_yaml(body: &str) -> Option<String> {
    extract_fence(body, "yaml").or_else(|| extract_fence(body, "yml"))
}

/// `## Header` … up to the next ```` ``` ```` fence or end of text. The `regex` crate
/// has no lookahead, so the tail is scanned manually for the next fence.
pub fn extract_section(body: &str, header: &str) -> Option<String> {
    let start_re = Regex::new(&format!(r"(?m)^##\s+{}\s*\r?\n", regex::escape(header))).ok()?;
    let m = start_re.find(body)?;
    let rest = &body[m.end()..];
    // Section ends just before a newline that precedes a fence.
    let end = rest.find("\n```").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

fn load_yaml(block: Option<&str>) -> (Yaml, Vec<String>) {
    let empty = Yaml::Mapping(serde_yaml::Mapping::new());
    let Some(block) = block else {
        return (empty, vec!["yaml_fence_missing".into()]);
    };
    match serde_yaml::from_str::<Yaml>(block) {
        Ok(v @ Yaml::Mapping(_)) => (v, vec![]),
        Ok(_) => (empty, vec!["yaml_not_a_mapping".into()]),
        Err(_) => {
            // bare-colon scalar recovery, e.g. `dominant_color_palette: warm: amber`
            let fixed = recover_bare_colon(block);
            if let Ok(v @ Yaml::Mapping(_)) = serde_yaml::from_str::<Yaml>(&fixed) {
                return (v, vec!["yaml_recovered_bare_colon".into()]);
            }
            (empty, vec!["yaml_parse_failed".into()])
        }
    }
}

fn recover_bare_colon(block: &str) -> String {
    let re = Regex::new(r"(?m)^(dominant_color_palette):\s*(.+)$").unwrap();
    re.replace_all(block, |c: &regex::Captures| {
        format!("{}: \"{}\"", &c[1], c[2].trim())
    })
    .into_owned()
}

// --------------------------------------------------------------------------- //
// Steps 5/6 — transcript / translation normalization
// --------------------------------------------------------------------------- //
fn empty_transcript(s: &str) -> bool {
    matches!(
        s,
        "[no speech detected]" | "[transcription disabled]" | "" | "none" | "n/a"
    )
}

fn empty_translation(s: &str) -> bool {
    matches!(s, "[not applicable]" | "" | "none" | "n/a")
}

pub fn norm_transcript(s: Option<String>) -> Option<String> {
    let s = s?;
    if empty_transcript(&s.trim().to_lowercase()) {
        None
    } else {
        Some(s.trim().to_string())
    }
}

pub fn norm_translation(s: Option<String>) -> Option<String> {
    let s = s?;
    if empty_translation(&s.trim().to_lowercase()) {
        None
    } else {
        Some(s.trim().to_string())
    }
}

// --------------------------------------------------------------------------- //
// Coercion & validation
// --------------------------------------------------------------------------- //
fn enum_set(field: &str) -> &'static [&'static str] {
    match field {
        "rating" => &["keep", "review", "cull"],
        "focus" => &["sharp", "soft", "out_of_focus", "mixed", "unclear"],
        "exposure" => &["adequate", "under", "over", "strong", "unclear"],
        "stability" => &["smooth", "shaky", "handheld", "static", "unclear"],
        "motion_blur" => &["clean", "slight", "heavy", "unclear"],
        "lighting" => &[
            "bright_daylight",
            "golden_hour",
            "overcast",
            "indoor_artificial",
            "low_light",
            "night",
            "mixed",
            "varies",
            "unclear",
        ],
        "time_of_day" => &[
            "morning",
            "midday",
            "afternoon",
            "evening",
            "night",
            "golden_hour",
            "unclear",
        ],
        "audio_quality" => &[
            "clear",
            "noisy",
            "muffled",
            "music_only",
            "ambient",
            "silent",
            "unclear",
        ],
        "shot_type" => &[
            "close-up",
            "medium",
            "wide",
            "establishing",
            "aerial",
            "pov",
            "macro",
            "static-portrait",
            "varies",
            "unclear",
        ],
        _ => &[],
    }
}

const BIOMETRIC_KEYS: &[&str] = &[
    "faces",
    "face_count",
    "cluster_id",
    "bbox",
    "detection_quality",
    "speaker_count",
];

fn coerce_people_count(v: Option<&Yaml>) -> (i64, Vec<String>) {
    match v {
        Some(Yaml::Bool(_)) => (0, vec![]),
        Some(Yaml::Number(n)) if n.is_i64() || n.is_u64() => (n.as_i64().unwrap_or(0), vec![]),
        Some(other) => {
            let t = yaml_to_string(other).trim().to_lowercase();
            if matches!(t.as_str(), "many" | "crowd" | "uncountable" | "lots") {
                return (-1, vec![]);
            }
            if let Some(m) = Regex::new(r"-?\d+").unwrap().find(&t) {
                if let Ok(n) = m.as_str().parse::<i64>() {
                    return (n, vec![]);
                }
            }
            (0, vec!["people_count_uncoerced".into()])
        }
        None => (0, vec![]),
    }
}

fn coerce_enum(field: &str, v: Option<&Yaml>, default: &str) -> (String, Vec<String>) {
    let t = match v {
        Some(y) => yaml_to_string(y).trim().to_lowercase(),
        None => return (default.to_string(), vec![]),
    };
    if t.starts_with('<') && t.ends_with('>') {
        return (default.to_string(), vec![format!("placeholder:{field}")]);
    }
    if enum_set(field).contains(&t.as_str()) {
        (t, vec![])
    } else {
        (default.to_string(), vec![format!("enum_coerced:{field}")])
    }
}

fn coerce_list(v: Option<&Yaml>, cap: usize) -> Vec<String> {
    let parts: Vec<String> = match v {
        None | Some(Yaml::Null) => return vec![],
        Some(Yaml::String(s)) => Regex::new(r"[,\n]")
            .unwrap()
            .split(s)
            .map(|p| p.to_string())
            .collect(),
        Some(Yaml::Sequence(seq)) => seq.iter().map(yaml_to_string).collect(),
        Some(other) => vec![yaml_to_string(other)],
    };
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for item in parts {
        let s = item.trim().to_lowercase();
        if s.starts_with('<') && s.ends_with('>') {
            continue;
        }
        if !s.is_empty() && seen.insert(s.clone()) {
            out.push(s);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

/// Stringify a YAML scalar the way the model tends to emit it.
fn yaml_to_string(v: &Yaml) -> String {
    match v {
        Yaml::String(s) => s.clone(),
        Yaml::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        Yaml::Number(n) => n.to_string(),
        Yaml::Null => "None".into(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn yaml_get<'a>(map: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    map.get(key).filter(|v| !matches!(v, Yaml::Null))
}

fn yaml_truthy(v: Option<&Yaml>) -> bool {
    match v {
        Some(Yaml::Bool(b)) => *b,
        Some(Yaml::Number(n)) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        Some(Yaml::String(s)) => !s.is_empty(),
        _ => false,
    }
}

// --------------------------------------------------------------------------- //
// parse_response
// --------------------------------------------------------------------------- //
pub fn parse_response(raw: &str) -> ParsedResult {
    let mut warnings: Vec<String> = Vec::new();
    let text = strip_ansi(&unwrap_claude_json(raw));
    let inner = match slice_sentinels(&text) {
        Some(i) => i,
        None => {
            warnings.push("sentinels_not_found".into());
            let fallback: String = text.trim().chars().take(1024).collect();
            return empty_parsed(false, fallback, warnings, Some(truncate(&text, 8192)));
        }
    };

    let yaml_block = extract_yaml(&inner);
    let (data, ywarn) = load_yaml(yaml_block.as_deref());
    warnings.extend(ywarn);

    let description = extract_section(&inner, "Description").unwrap_or_default();
    let transcript = norm_transcript(extract_fence(&inner, "transcript"));
    let translation = norm_translation(extract_fence(&inner, "translation"));

    // ---- coerce scalars ----
    let (rating, w) = coerce_enum("rating", yaml_get(&data, "rating"), "review");
    if !w.is_empty() {
        warnings.push("rating_defaulted".into());
    }

    let tech_in = yaml_get(&data, "technical");
    let mut tech_field = |sub: &str| -> String {
        let v = tech_in.and_then(|t| yaml_get(t, sub));
        let (val, w) = coerce_enum(sub, v, "unclear");
        warnings.extend(w);
        val
    };
    let technical = Technical {
        focus: tech_field("focus"),
        exposure: tech_field("exposure"),
        stability: tech_field("stability"),
        motion_blur: tech_field("motion_blur"),
    };

    let (lighting, w) = coerce_enum("lighting", yaml_get(&data, "lighting"), "unclear");
    warnings.extend(w);
    let (time_of_day, w) = coerce_enum("time_of_day", yaml_get(&data, "time_of_day"), "unclear");
    warnings.extend(w);
    let (audio_quality, w) =
        coerce_enum("audio_quality", yaml_get(&data, "audio_quality"), "unclear");
    warnings.extend(w);
    let (shot_type, w) = coerce_enum("shot_type", yaml_get(&data, "shot_type"), "unclear");
    warnings.extend(w);

    let (people_count, w) = coerce_people_count(yaml_get(&data, "people_count"));
    warnings.extend(w);

    let keywords = coerce_list(yaml_get(&data, "keywords"), 15);
    let dominant_colors = coerce_list(yaml_get(&data, "dominant_colors"), 8);

    let mut palette = yaml_get(&data, "dominant_color_palette")
        .map(yaml_to_string)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unclear".into());
    if palette.starts_with('<') && palette.ends_with('>') {
        palette = "unclear".into();
    }

    // cull_reason consistency
    let mut cull_reason = yaml_get(&data, "cull_reason")
        .map(yaml_to_string)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if cull_reason.starts_with('<') && cull_reason.ends_with('>') {
        cull_reason.clear();
    }
    if rating == "cull" && cull_reason.is_empty() {
        warnings.push("cull_reason_missing".into());
    }
    if rating != "cull" {
        cull_reason.clear();
    }

    // schema_version mismatch (non-fatal)
    let mut schema_version = None;
    if let Some(sv) = yaml_get(&data, "schema_version") {
        match sv {
            Yaml::Number(n) if n.is_i64() || n.is_u64() => {
                let iv = n.as_i64().unwrap_or(0);
                schema_version = Some(iv);
                if iv != 3 {
                    warnings.push(format!("schema_version_mismatch:{iv}"));
                }
            }
            Yaml::String(s) if s.trim().parse::<i64>().is_ok() => {
                let iv: i64 = s.trim().parse().unwrap();
                schema_version = Some(iv);
                if iv != 3 {
                    warnings.push(format!("schema_version_mismatch:{iv}"));
                }
            }
            other => warnings.push(format!("schema_version_mismatch:{}", yaml_to_string(other))),
        }
    }

    // language: none/und/empty → None
    let mut language = None;
    if let Some(lr) = yaml_get(&data, "language") {
        let lt = yaml_to_string(lr).trim().to_lowercase();
        let placeholder = lt.starts_with('<') && lt.ends_with('>');
        if !lt.is_empty() && !matches!(lt.as_str(), "none" | "und" | "null" | "n/a") && !placeholder
        {
            language = Some(lt);
        }
    }
    let language_confidence = yaml_get(&data, "language_confidence")
        .map(|v| yaml_to_string(v).trim().to_lowercase())
        .filter(|s| !s.is_empty());

    let mut has_speech = yaml_truthy(yaml_get(&data, "has_speech"));
    if has_speech && transcript.is_none() {
        warnings.push("speech_claimed_but_empty".into());
    }
    if !has_speech && transcript.is_some() {
        has_speech = true;
        warnings.push("speech_recovered".into());
    }

    // notable_timestamp must look like MM:SS
    let mut nts = yaml_get(&data, "notable_timestamp")
        .map(yaml_to_string)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !nts.is_empty() && !ts_re().is_match(&nts) {
        nts.clear();
        warnings.push("timestamp_coerced".into());
    }

    // ensure no biometric keys leaked
    for k in BIOMETRIC_KEYS {
        if data.get(*k).is_some() {
            warnings.push(format!("ignored_obsolete_key:{k}"));
        }
    }

    let data_nonempty = matches!(&data, Yaml::Mapping(m) if !m.is_empty());
    let parse_ok = !inner.is_empty() && data_nonempty && !description.is_empty();

    ParsedResult {
        parse_ok,
        schema_version,
        language,
        language_confidence,
        has_speech,
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
        notable_timestamp: nts,
        description,
        transcript,
        english_translation: translation,
        warnings,
        raw_excerpt: if parse_ok {
            None
        } else {
            Some(truncate(&text, 8192))
        },
    }
}

fn ts_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{1,2}:\d{2}$").unwrap())
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn empty_parsed(
    parse_ok: bool,
    description: String,
    warnings: Vec<String>,
    raw_excerpt: Option<String>,
) -> ParsedResult {
    ParsedResult {
        parse_ok,
        schema_version: None,
        language: None,
        language_confidence: None,
        has_speech: false,
        rating: "review".into(),
        cull_reason: String::new(),
        technical: Technical {
            focus: "unclear".into(),
            exposure: "unclear".into(),
            stability: "unclear".into(),
            motion_blur: "unclear".into(),
        },
        lighting: "unclear".into(),
        time_of_day: "unclear".into(),
        dominant_color_palette: "unclear".into(),
        dominant_colors: vec![],
        audio_quality: "unclear".into(),
        people_count: 0,
        keywords: vec![],
        shot_type: "unclear".into(),
        notable_timestamp: String::new(),
        description,
        transcript: None,
        english_translation: None,
        warnings,
        raw_excerpt,
    }
}

// --------------------------------------------------------------------------- //
// parse_merge_response
// --------------------------------------------------------------------------- //
pub fn parse_merge_response(raw: &str) -> MergeResult {
    let stripped = strip_ansi(&unwrap_claude_json(raw));
    let text = match slice_sentinels(&stripped) {
        Some(t) => t,
        None => {
            return MergeResult {
                summary: String::new(),
                description: String::new(),
                parse_ok: false,
                warnings: vec!["merge_sentinels_not_found".into()],
            }
        }
    };
    let y = extract_yaml(&text).unwrap_or_default();
    let data: Yaml = serde_yaml::from_str(&y).unwrap_or(Yaml::Null);
    let summary = yaml_get(&data, "summary")
        .map(yaml_to_string)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let prose = extract_section(&text, "Description").unwrap_or_default();
    let ok = !prose.is_empty();
    MergeResult {
        summary,
        description: prose,
        parse_ok: ok,
        warnings: if ok {
            vec![]
        } else {
            vec!["merge_description_empty".into()]
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SENTINEL_BEGIN, SENTINEL_END};

    const GOOD_YAML: &str = "schema_version: 3
language: es
has_speech: true
rating: cull
cull_reason: color bars
technical:
  focus: sharp
  exposure: strong
  stability: smooth
  motion_blur: clean
lighting: golden_hour
time_of_day: afternoon
dominant_color_palette: warm savanna: amber, ochre
dominant_colors:
- amber
- ochre
audio_quality: clear
people_count: many
keywords:
- steel-beam
- safety
face_count: 3
shot_type: medium
notable_timestamp: 00:03";

    fn block(yaml: &str, desc: &str, transcript: &str, translation: &str) -> String {
        format!(
            "{b}\n```yaml\n{yaml}\n```\n\n## Description\n{desc}\n\n\
             ```transcript\n{transcript}\n```\n\n```translation\n{translation}\n```\n{e}",
            b = SENTINEL_BEGIN,
            e = SENTINEL_END,
        )
    }

    fn good() -> String {
        block(
            GOOD_YAML,
            "**Scene:** x",
            "[no speech detected]",
            "[not applicable]",
        )
    }

    #[test]
    fn last_pair_slicing_on_double_echo() {
        let echoed = format!(
            "noise\n{}\nmore\n{}",
            good(),
            block(
                GOOD_YAML,
                "**Scene:** real",
                "[no speech detected]",
                "[not applicable]"
            )
        );
        let r = parse_response(&format!("\x1b[32m{echoed}\x1b[0m"));
        assert!(r.parse_ok);
        assert!(
            r.description.contains("real"),
            "last block should win: {}",
            r.description
        );
    }

    #[test]
    fn claude_json_unwrap_in_parse() {
        let inner = block(GOOD_YAML, "**Scene:** x", "Hola", "[not applicable]");
        let raw = serde_json::json!({"type": "result", "result": inner}).to_string();
        let r = parse_response(&raw);
        assert_eq!(r.transcript.as_deref(), Some("Hola"));
    }

    #[test]
    fn palette_bare_colon_recovery() {
        let r = parse_response(&block(
            GOOD_YAML,
            "**Scene:** x",
            "Hola",
            "[not applicable]",
        ));
        assert_eq!(r.dominant_color_palette, "warm savanna: amber, ochre");
    }

    #[test]
    fn people_count_many_is_neg1() {
        let r = parse_response(&good());
        assert_eq!(r.people_count, -1);
    }

    #[test]
    fn biometric_keys_ignored() {
        let r = parse_response(&good());
        assert!(r.warnings.iter().any(|w| w.contains("face_count")));
    }

    #[test]
    fn empty_transcript_normalized() {
        let r = parse_response(&block(
            "schema_version: 3\nrating: keep\nhas_speech: false",
            "**Scene:** x",
            "[no speech detected]",
            "[not applicable]",
        ));
        assert!(r.transcript.is_none());
    }

    #[test]
    fn transcription_disabled_normalized() {
        let r = parse_response(&block(
            "schema_version: 3\nrating: keep",
            "**Scene:** x",
            "[transcription disabled]",
            "[not applicable]",
        ));
        assert!(r.transcript.is_none());
    }

    #[test]
    fn yml_fence_and_rating_default() {
        let body = block(
            "schema_version: 3\nrating: bogus\nhas_speech: false",
            "**Scene:** x",
            "[no speech detected]",
            "[not applicable]",
        )
        .replacen("```yaml", "```yml", 1);
        let r = parse_response(&body);
        assert_eq!(r.rating, "review");
        assert!(r.warnings.iter().any(|w| w == "rating_defaulted"));
    }

    #[test]
    fn no_sentinels_parse_fail() {
        let r = parse_response("just agy chatter, no answer block");
        assert!(!r.parse_ok);
        assert!(r.warnings.iter().any(|w| w == "sentinels_not_found"));
        assert!(!r.description.is_empty());
    }

    #[test]
    fn merge_parse_summary_only() {
        let raw = format!(
            "{b}\n```yaml\nsummary: A block party.\n```\n## Description\n**Scene:** street\n{e}",
            b = SENTINEL_BEGIN,
            e = SENTINEL_END,
        );
        let m = parse_merge_response(&raw);
        assert!(m.parse_ok);
        assert_eq!(m.summary, "A block party.");
        assert!(m.description.contains("street"));
    }

    #[test]
    fn strip_ansi_removes_color() {
        assert_eq!(strip_ansi("\x1b[32mhi\x1b[0m"), "hi");
    }

    #[test]
    fn unwrap_claude_passthrough_for_non_json() {
        assert_eq!(unwrap_claude_json("raw agy text"), "raw agy text");
    }
}
