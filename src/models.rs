//! Model names: gander's aliases, the built-in lists, the live lists, and classes.
//!
//! gander does not decide which models exist. A harness's model list depends on
//! the harness version and on the account, so `--model` is passed through to the
//! harness as typed, and the harness accepts or rejects it. gander adds:
//!
//! - **aliases** — a few short names (`pro`, `flash`, `sonnet`, ...) that map to
//!   a concrete model;
//! - **the live list** — `gander --models` asks each harness what it offers
//!   (`agy models`, `codex debug models`; claude has no such command);
//! - **the built-in list** — a snapshot, used when the live list cannot be read
//!   and to spot a name that belongs to a different backend;
//! - **classes** — a coarse capability label per model family, from
//!   `[classes]` in `~/.gander/config.toml` on top of built-in defaults.
//!
//! Built-in lists last checked against the live lists on 2026-11-22.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::{backend_for, piped, BackendKind};
use crate::config::{
    Config, CLAUDE_FABLE, CLAUDE_HAIKU, CLAUDE_OPUS, CLAUDE_SONNET, FLASH, HARNESS_DEFAULT, PRO,
};

/// How long a harness may take to print its model list.
const LIST_TIMEOUT_S: f64 = 30.0;

pub const BACKENDS: [(&str, BackendKind); 3] = [
    ("agy", BackendKind::Agy),
    ("claude", BackendKind::Claude),
    ("codex", BackendKind::Codex),
];

/// `(alias, concrete)`. The first alias of a backend is its default.
type Alias = (&'static str, &'static str);

const AGY_ALIASES: &[Alias] = &[("pro", PRO), ("flash", FLASH)];
const CLAUDE_ALIASES: &[Alias] = &[
    ("sonnet", CLAUDE_SONNET),
    ("haiku", CLAUDE_HAIKU),
    ("opus", CLAUDE_OPUS),
    ("fable", CLAUDE_FABLE),
];
/// codex has one alias: `default` passes no `-m`, so codex uses the model from
/// the user's own codex config.
const CODEX_ALIASES: &[Alias] = &[("default", HARNESS_DEFAULT)];

/// Snapshot of `agy models` (slugs).
const AGY_BUILTIN: &[&str] = &[
    "gemini-4-neon-low",
    "gemini-4-neon-medium",
    "gemini-4-neon-high",
    "gemini-4-neon-xhigh",
    "gemini-4-carbon-low",
    "gemini-4-carbon-medium",
    "gemini-4-carbon-high",
    "gemini-4-argon-low",
    "gemini-4-argon-medium",
    "gemini-4-argon-high",
    "gemini-4-hydrogen-low",
    "gemini-4-hydrogen-medium",
    "gemini-4-hydrogen-high",
    "gemini-4-hydrogen-xhigh",
    "gemini-pro-orchestrator-neon",
    "gemini-pro-orchestrator",
    "gemini-pro-orchestrator-argon",
    "gemini-3.8-flash-low",
    "gemini-3.8-flash-medium",
    "gemini-3.8-flash-high",
    "claude-opus-5-5-low",
    "claude-opus-5-5-medium",
    "claude-opus-5-5-high",
    "claude-sonnet-5-5-low",
    "claude-sonnet-5-5-medium",
    "claude-sonnet-5-5-high",
    "gpt-oss-120b-medium",
];

/// claude has no list command, so its aliases are the whole built-in list.
const CLAUDE_BUILTIN: &[&str] = &[];

/// Snapshot of `codex debug models` (the `"visibility": "list"` rows).
const CODEX_BUILTIN: &[&str] = &[
    "gpt-6.2-sol",
    "gpt-6.2-astra",
    "gpt-6.1-terra",
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
];

fn aliases(kind: BackendKind) -> &'static [Alias] {
    match kind {
        BackendKind::Agy => AGY_ALIASES,
        BackendKind::Claude => CLAUDE_ALIASES,
        BackendKind::Codex => CODEX_ALIASES,
    }
}

fn builtin(kind: BackendKind) -> &'static [&'static str] {
    match kind {
        BackendKind::Agy => AGY_BUILTIN,
        BackendKind::Claude => CLAUDE_BUILTIN,
        BackendKind::Codex => CODEX_BUILTIN,
    }
}

/// Does gander know `selector` as a name of this backend (alias or built-in)?
fn known_to(kind: BackendKind, selector: &str) -> bool {
    aliases(kind).iter().any(|a| a.0 == selector) || builtin(kind).contains(&selector)
}

/// Is `selector` a name of some other backend, and not of this one? Such a name
/// can never work here, so it is never passed through.
fn foreign_to(kind: BackendKind, selector: &str) -> bool {
    !known_to(kind, selector)
        && BACKENDS
            .iter()
            .any(|(_, other)| *other != kind && known_to(*other, selector))
}

/// Can `selector` be used with `backend`? False only for a name that belongs to
/// a different backend. A name gander has never seen is allowed: the harness
/// decides. An unknown backend accepts anything.
pub fn valid_pair(backend: &str, selector: &str) -> bool {
    match BackendKind::from_str(backend) {
        Some(k) => !foreign_to(k, selector),
        None => true,
    }
}

/// Map a model selector to the string the harness gets.
///
/// - an alias of this backend → its concrete model;
/// - a name of another backend (the default `pro` with `--backend codex`) → this
///   backend's default;
/// - anything else → passed through unchanged.
pub fn model_for(kind: BackendKind, selector: &str) -> String {
    let al = aliases(kind);
    if kind == BackendKind::Agy && selector == "auto" {
        return FLASH.to_string();
    }
    if let Some(a) = al.iter().find(|a| a.0 == selector) {
        return a.1.to_string();
    }
    if selector.is_empty() || foreign_to(kind, selector) {
        return al[0].1.to_string();
    }
    selector.to_string()
}

// --------------------------------------------------------------------------- //
// Classes
// --------------------------------------------------------------------------- //

/// The built-in class of each model family. The harnesses publish no such
/// label; this is a judgment, and `[classes]` in the config file overrides it.
const DEFAULT_CLASSES: &[(&str, &[&str])] = &[
    ("above-pro", &["fable", "astra"]),
    ("pro", &["neon", "opus", "sol"]),
    ("mid", &["terra", "carbon"]),
    ("flash", &["hydrogen", "luna", "flash"]),
    // Listed so that `gemini-pro-orchestrator-neon` is not classed by its
    // `neon` suffix: an orchestrator is a different kind of model.
    ("unknown", &["orchestrator"]),
];

/// Family keyword → class.
#[derive(Debug, Clone)]
pub struct Classes(BTreeMap<String, String>);

impl Default for Classes {
    fn default() -> Self {
        Classes::with_overrides(&BTreeMap::new())
    }
}

impl Classes {
    /// The built-in classes, with `overrides` (class → family keywords, the
    /// `[classes]` config table) applied on top.
    pub fn with_overrides(overrides: &BTreeMap<String, Vec<String>>) -> Classes {
        let mut map = BTreeMap::new();
        for (class, words) in DEFAULT_CLASSES {
            for w in *words {
                map.insert((*w).to_string(), (*class).to_string());
            }
        }
        for (class, words) in overrides {
            for w in words {
                map.insert(w.to_lowercase(), class.clone());
            }
        }
        Classes(map)
    }

    /// The class of a model: that of the first word in its name that is a known
    /// family keyword, else `unknown`. Words are split on anything that is not a
    /// letter or digit, so `gpt-6.2-sol` is `gpt`, `6`, `2`, `sol`.
    pub fn of(&self, model: &str) -> &str {
        model
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .find_map(|w| self.0.get(w))
            .map(String::as_str)
            .unwrap_or("unknown")
    }
}

// --------------------------------------------------------------------------- //
// The live list
// --------------------------------------------------------------------------- //

/// Parse `agy models`: one `slug<TAB>Display Name` per line.
fn parse_agy_models(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(slug, _)| slug.trim().to_string())
        .filter(|s| !s.is_empty() && !s.contains(' '))
        .collect()
}

/// Parse `codex debug models`: `{"models": [{"slug": ..., "visibility": ...}]}`.
/// Only the rows codex itself shows in its picker (`"list"`) are kept.
fn parse_codex_models(out: &str) -> Vec<String> {
    let start = out.find('{').unwrap_or(0);
    let Ok(v) = serde_json::from_str::<Value>(&out[start..]) else {
        return Vec::new();
    };
    v.get("models")
        .and_then(Value::as_array)
        .map(|ms| {
            ms.iter()
                .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
                .filter_map(|m| m.get("slug").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Ask the harness for its model list. `None` when the harness has no list
/// command (claude), is not installed, fails, or prints nothing usable.
fn live(kind: BackendKind, cfg: &Config) -> Option<Vec<String>> {
    let be = backend_for(kind, cfg);
    type Parser = fn(&str) -> Vec<String>;
    let (argv, parse): (Vec<String>, Parser) = match kind {
        BackendKind::Agy => (vec![cfg.agy_bin.clone(), "models".into()], parse_agy_models),
        BackendKind::Codex => (
            vec![cfg.codex_bin.clone(), "debug".into(), "models".into()],
            parse_codex_models,
        ),
        BackendKind::Claude => return None,
    };
    let run = piped::run_piped(&argv, &be.env(), Duration::from_secs_f64(LIST_TIMEOUT_S)).ok()?;
    let found = parse(&run.stdout);
    (!found.is_empty()).then_some(found)
}

/// The model names of one backend, default first: its aliases, then the live
/// list (or the built-in one). The bool is true when the list is live.
pub fn names(kind: BackendKind, cfg: &Config, want_live: bool) -> (Vec<String>, bool) {
    let listed = if want_live { live(kind, cfg) } else { None };
    let is_live = listed.is_some();
    let listed = listed.unwrap_or_else(|| builtin(kind).iter().map(|s| (*s).to_string()).collect());
    let mut out: Vec<String> = aliases(kind).iter().map(|a| a.0.to_string()).collect();
    out.extend(listed);
    (out, is_live)
}

/// `--models`: what each backend accepts, with the class of each model.
pub fn listing(only: Option<BackendKind>, cfg: &Config, classes: &Classes) -> Value {
    let mut out = serde_json::Map::new();
    for (name, kind) in BACKENDS {
        if only.is_some_and(|k| k != kind) {
            continue;
        }
        let (all, is_live) = names(kind, cfg, true);
        let models: Vec<Value> = all
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let runs = model_for(kind, n);
                let class = if runs == HARNESS_DEFAULT {
                    "unknown"
                } else {
                    classes.of(&runs)
                };
                json!({ "name": n, "runs": runs, "class": class, "default": i == 0 })
            })
            .collect();
        let source = match (kind, is_live) {
            (_, true) => "live",
            (BackendKind::Claude, false) => "aliases",
            (_, false) => "builtin",
        };
        out.insert(name.into(), json!({ "source": source, "models": models }));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve() {
        assert_eq!(model_for(BackendKind::Agy, "pro"), PRO);
        assert_eq!(model_for(BackendKind::Agy, "flash"), FLASH);
        assert_eq!(model_for(BackendKind::Agy, "auto"), FLASH);
        assert_eq!(model_for(BackendKind::Claude, "fable"), CLAUDE_FABLE);
        assert_eq!(model_for(BackendKind::Codex, "default"), HARNESS_DEFAULT);
    }

    #[test]
    fn unseen_names_pass_through_unchanged() {
        // A model the harness added after this build must still be usable.
        assert_eq!(
            model_for(BackendKind::Agy, "gemini-5-xenon-low"),
            "gemini-5-xenon-low"
        );
        assert_eq!(
            model_for(BackendKind::Claude, "claude-opus-9"),
            "claude-opus-9"
        );
        assert_eq!(model_for(BackendKind::Codex, "gpt-7-sol"), "gpt-7-sol");
        assert!(valid_pair("codex", "gpt-7-sol"));
        assert!(valid_pair("nonsense", "anything"));
    }

    #[test]
    fn a_name_of_another_backend_falls_to_the_default() {
        assert_eq!(model_for(BackendKind::Agy, "sonnet"), PRO);
        assert_eq!(model_for(BackendKind::Claude, "pro"), CLAUDE_SONNET);
        assert_eq!(model_for(BackendKind::Codex, "pro"), HARNESS_DEFAULT);
        assert_eq!(model_for(BackendKind::Codex, ""), HARNESS_DEFAULT);
        assert!(!valid_pair("agy", "opus"));
        assert!(!valid_pair("claude", "gemini-4-neon-low"));
        assert!(!valid_pair("codex", "flash"));
        assert!(valid_pair("agy", "claude-opus-5-5-high"));
    }

    #[test]
    fn default_classes_follow_the_family_word() {
        let c = Classes::default();
        for (name, class) in [
            ("claude-fable-5-2", "above-pro"),
            ("gpt-6.2-astra", "above-pro"),
            ("gemini-4-neon-medium", "pro"),
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
            ("gpt-oss-120b-medium", "unknown"),
        ] {
            assert_eq!(c.of(name), class, "{name}");
        }
    }

    #[test]
    fn config_classes_override_and_extend() {
        let mut o = BTreeMap::new();
        o.insert(
            "mid".to_string(),
            vec!["Sonnet".to_string(), "argon".into()],
        );
        o.insert("pro".to_string(), vec!["terra".to_string()]);
        let c = Classes::with_overrides(&o);
        assert_eq!(c.of("claude-sonnet-5-5"), "mid");
        assert_eq!(c.of("gemini-4-argon-high"), "mid");
        assert_eq!(c.of("gpt-6.1-terra"), "pro");
        assert_eq!(c.of("gemini-4-neon-low"), "pro");
    }

    #[test]
    fn live_lists_parse() {
        let agy = "Fetching available models...\n\
                   gemini-4-neon-high\tGemini 4 Neon (High)\n\
                   gemini-3.8-flash-low\tGemini 3.8 Flash (Low)\n";
        assert_eq!(
            parse_agy_models(agy),
            ["gemini-4-neon-high", "gemini-3.8-flash-low"]
        );
        let codex = r#"{"models":[
            {"slug":"gpt-6.2-sol","visibility":"list"},
            {"slug":"gpt-reserve","visibility":"hide"},
            {"slug":"gpt-6-luna","visibility":"list"}]}"#;
        assert_eq!(parse_codex_models(codex), ["gpt-6.2-sol", "gpt-6-luna"]);
        assert!(parse_codex_models("not json").is_empty());
        assert!(parse_agy_models("error: not logged in").is_empty());
    }

    #[test]
    fn builtin_names_start_with_the_default_alias() {
        let cfg = Config::load(&Default::default());
        let (agy, live) = names(BackendKind::Agy, &cfg, false);
        assert!(!live);
        assert_eq!(agy[..2], ["pro", "flash"]);
        assert!(agy.contains(&"gemini-4-neon-medium".to_string()));
        let (claude, _) = names(BackendKind::Claude, &cfg, false);
        assert_eq!(claude, ["sonnet", "haiku", "opus", "fable"]);
        let (codex, _) = names(BackendKind::Codex, &cfg, false);
        assert_eq!(codex[0], "default");
    }
}
