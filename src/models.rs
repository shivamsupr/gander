//! The one model table. Every model gander accepts is a row here; the CLI
//! choices, the pair check, the first-run picker and `model_for` all read it.
//!
//! To update a harness, edit its rows and nothing else. Check the rows against
//! the harness's own list:
//!
//! - agy:    `agy models`          (slug, then display name)
//! - codex:  `codex debug models`  (JSON; keep the `"visibility": "list"` rows)
//! - claude: no list command; `claude --model <alias>` shows the id it resolves to
//!
//! Last checked against the live lists on 2026-11-22.

use crate::backend::BackendKind;
use crate::config::{CLAUDE_FABLE, CLAUDE_HAIKU, CLAUDE_OPUS, CLAUDE_SONNET, FLASH, PRO};

/// `(selector, concrete)`: what the user types, and the string the harness gets.
type Row = (&'static str, &'static str);

/// agy. `pro` and `flash` are gander's own aliases; the rest are agy slugs passed
/// through unchanged. The first row is the default.
const AGY: &[Row] = &[
    ("pro", PRO),
    ("flash", FLASH),
    ("gemini-4-neon-low", "gemini-4-neon-low"),
    ("gemini-4-neon-medium", "gemini-4-neon-medium"),
    ("gemini-4-neon-high", "gemini-4-neon-high"),
    ("gemini-4-neon-xhigh", "gemini-4-neon-xhigh"),
    ("gemini-4-carbon-low", "gemini-4-carbon-low"),
    ("gemini-4-carbon-medium", "gemini-4-carbon-medium"),
    ("gemini-4-carbon-high", "gemini-4-carbon-high"),
    ("gemini-4-argon-low", "gemini-4-argon-low"),
    ("gemini-4-argon-medium", "gemini-4-argon-medium"),
    ("gemini-4-argon-high", "gemini-4-argon-high"),
    ("gemini-4-hydrogen-low", "gemini-4-hydrogen-low"),
    ("gemini-4-hydrogen-medium", "gemini-4-hydrogen-medium"),
    ("gemini-4-hydrogen-high", "gemini-4-hydrogen-high"),
    ("gemini-4-hydrogen-xhigh", "gemini-4-hydrogen-xhigh"),
    (
        "gemini-pro-orchestrator-neon",
        "gemini-pro-orchestrator-neon",
    ),
    ("gemini-pro-orchestrator", "gemini-pro-orchestrator"),
    (
        "gemini-pro-orchestrator-argon",
        "gemini-pro-orchestrator-argon",
    ),
    ("gemini-3.8-flash-low", "gemini-3.8-flash-low"),
    ("gemini-3.8-flash-medium", "gemini-3.8-flash-medium"),
    ("gemini-3.8-flash-high", "gemini-3.8-flash-high"),
    ("claude-opus-5-5-low", "claude-opus-5-5-low"),
    ("claude-opus-5-5-medium", "claude-opus-5-5-medium"),
    ("claude-opus-5-5-high", "claude-opus-5-5-high"),
    ("claude-sonnet-5-5-low", "claude-sonnet-5-5-low"),
    ("claude-sonnet-5-5-medium", "claude-sonnet-5-5-medium"),
    ("claude-sonnet-5-5-high", "claude-sonnet-5-5-high"),
    ("gpt-oss-120b-medium", "gpt-oss-120b-medium"),
];

/// claude. Aliases map to pinned ids. The first row is the default.
const CLAUDE: &[Row] = &[
    ("sonnet", CLAUDE_SONNET),
    ("haiku", CLAUDE_HAIKU),
    ("opus", CLAUDE_OPUS),
    ("fable", CLAUDE_FABLE),
];

/// codex. Slugs passed through unchanged. The first row is the default.
const CODEX: &[Row] = &[
    ("gpt-6.2-sol", "gpt-6.2-sol"),
    ("gpt-6.2-astra", "gpt-6.2-astra"),
    ("gpt-6.1-terra", "gpt-6.1-terra"),
    ("gpt-6.1-sol", "gpt-6.1-sol"),
    ("gpt-6-astra", "gpt-6-astra"),
    ("gpt-6-sol", "gpt-6-sol"),
    ("gpt-6-luna", "gpt-6-luna"),
    ("gpt-5.6-sol", "gpt-5.6-sol"),
    ("gpt-5.6-terra", "gpt-5.6-terra"),
    ("gpt-5.6-luna", "gpt-5.6-luna"),
];

/// The capability class of a model, read from its family name. The harnesses do
/// not publish this; the mapping is the fleet owner's (2026-11-22):
///
/// - `above-pro`: fable, astra
/// - `pro`:       neon, opus, sol
/// - `mid`:       terra, carbon
/// - `flash`:     hydrogen, luna, and Gemini Flash
///
/// `unknown` for every family not named above (argon, the orchestrators, sonnet,
/// haiku, gpt-oss). A new family stays `unknown` until someone classes it here.
pub fn class_of(concrete: &str) -> &'static str {
    let m = concrete.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if m.contains("orchestrator") {
        "unknown"
    } else if has(&["fable", "astra"]) {
        "above-pro"
    } else if has(&["neon", "opus"]) || m.ends_with("-sol") {
        "pro"
    } else if has(&["terra", "carbon"]) {
        "mid"
    } else if has(&["hydrogen", "luna", "flash"]) {
        "flash"
    } else {
        "unknown"
    }
}

/// `--models`: every accepted model, per backend, with its class. `only` limits
/// the listing to one backend.
pub fn listing(only: Option<BackendKind>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (name, kind) in [
        ("agy", BackendKind::Agy),
        ("claude", BackendKind::Claude),
        ("codex", BackendKind::Codex),
    ] {
        if only.is_some_and(|k| k != kind) {
            continue;
        }
        let list: Vec<serde_json::Value> = rows(kind)
            .iter()
            .enumerate()
            .map(|(i, r)| {
                serde_json::json!({
                    "name": r.0,
                    "runs": r.1,
                    "class": class_of(r.1),
                    "default": i == 0,
                })
            })
            .collect();
        out.insert(name.into(), serde_json::Value::Array(list));
    }
    serde_json::Value::Object(out)
}

fn rows(kind: BackendKind) -> &'static [Row] {
    match kind {
        BackendKind::Agy => AGY,
        BackendKind::Claude => CLAUDE,
        BackendKind::Codex => CODEX,
    }
}

/// The selectors valid for a backend name (first = that backend's default).
/// Empty for an unknown backend.
pub fn selectors_for(backend: &str) -> Vec<&'static str> {
    BackendKind::from_str(backend)
        .map(|k| rows(k).iter().map(|r| r.0).collect())
        .unwrap_or_default()
}

/// Every selector of every backend, in table order (the `--model` choices).
pub fn all_selectors() -> Vec<&'static str> {
    [AGY, CLAUDE, CODEX]
        .iter()
        .flat_map(|t| t.iter().map(|r| r.0))
        .collect()
}

/// Is `selector` one of `backend`'s models? An unknown backend accepts anything.
pub fn valid_pair(backend: &str, selector: &str) -> bool {
    match BackendKind::from_str(backend) {
        Some(k) => rows(k).iter().any(|r| r.0 == selector),
        None => true,
    }
}

/// Map a gander model selector to a backend's concrete model string. A selector
/// from another backend (e.g. the default `pro` with `--backend codex`) resolves
/// to that backend's default row.
pub fn model_for(kind: BackendKind, selector: &str) -> String {
    let t = rows(kind);
    if kind == BackendKind::Agy && selector == "auto" {
        return FLASH.to_string();
    }
    t.iter()
        .find(|r| r.0 == selector)
        .unwrap_or(&t[0])
        .1
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_slugs_resolve() {
        assert_eq!(model_for(BackendKind::Agy, "pro"), PRO);
        assert_eq!(model_for(BackendKind::Agy, "flash"), FLASH);
        assert_eq!(model_for(BackendKind::Agy, "auto"), FLASH);
        assert_eq!(
            model_for(BackendKind::Agy, "gemini-4-neon-medium"),
            "gemini-4-neon-medium"
        );
        assert_eq!(model_for(BackendKind::Claude, "fable"), CLAUDE_FABLE);
        assert_eq!(
            model_for(BackendKind::Codex, "gpt-6.1-terra"),
            "gpt-6.1-terra"
        );
    }

    #[test]
    fn foreign_selector_falls_to_backend_default() {
        assert_eq!(model_for(BackendKind::Agy, "sonnet"), PRO);
        assert_eq!(model_for(BackendKind::Claude, "pro"), CLAUDE_SONNET);
        assert_eq!(model_for(BackendKind::Codex, "pro"), "gpt-6.2-sol");
        // The dead pre-2026-11 codex ids must not be passed through any more.
        assert_eq!(model_for(BackendKind::Codex, "gpt-5.5"), "gpt-6.2-sol");
    }

    #[test]
    fn pairs_are_scoped_to_their_backend() {
        assert!(valid_pair("agy", "pro"));
        assert!(valid_pair("agy", "claude-opus-5-5-high"));
        assert!(!valid_pair("agy", "opus"));
        assert!(valid_pair("claude", "fable"));
        assert!(!valid_pair("claude", "claude-opus-5-5-high"));
        assert!(valid_pair("codex", "gpt-6.2-sol"));
        assert!(!valid_pair("codex", "gpt-5.5"));
        assert!(selectors_for("nonsense").is_empty());
    }

    #[test]
    fn classes_follow_the_family_name() {
        for (name, class) in [
            ("claude-fable-5-2", "above-pro"),
            ("gpt-6.2-astra", "above-pro"),
            ("gemini-4-neon-medium", "pro"),
            ("claude-opus-5-5", "pro"),
            ("claude-opus-5-5-high", "pro"),
            ("gpt-6.2-sol", "pro"),
            ("gpt-6.1-terra", "mid"),
            ("gemini-4-carbon-low", "mid"),
            ("gemini-4-hydrogen-xhigh", "flash"),
            ("gpt-6-luna", "flash"),
            ("Gemini 3.8 Flash (Medium)", "flash"),
            ("gemini-4-argon-low", "unknown"),
            ("gemini-pro-orchestrator-neon", "unknown"),
            ("claude-sonnet-5-5", "unknown"),
            ("claude-haiku-5-5", "unknown"),
            ("gpt-oss-120b-medium", "unknown"),
        ] {
            assert_eq!(class_of(name), class, "{name}");
        }
    }

    #[test]
    fn no_selector_belongs_to_two_backends() {
        let all = all_selectors();
        let mut seen = std::collections::HashSet::new();
        for s in &all {
            assert!(seen.insert(*s), "duplicate selector {s}");
            assert_ne!(*s, "none", "`none` is reserved for the fallback flags");
        }
    }
}
