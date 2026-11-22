//! Persisted user defaults (`~/.gander/config.toml`) + the TTY-gated first-run
//! prompt. Holds the four selectors `model` / `backend` / `fallback_model` /
//! `fallback_backend`, and the optional `[classes]` table.
//!
//! Per-setting precedence (resolved in `main`): flag, then `GANDER_*` env, then the
//! config file, then the built-in default. The interactive prompt only fires on a real
//! TTY (stdin+stderr); a non-interactive run (the agent path) never prompts or writes.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FileDefaults {
    pub model: Option<String>,
    pub backend: Option<String>,
    pub fallback_model: Option<String>,
    pub fallback_backend: Option<String>,
    /// `[classes]`: class name → model family keywords. Applied on top of the
    /// built-in classes (see `models::Classes`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub classes: BTreeMap<String, Vec<String>>,
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gander")
        .join("config.toml")
}

/// Load persisted defaults. Missing/malformed file → empty defaults (a stderr warn,
/// never fatal). `no_config` ignores the file entirely for this run.
pub fn load(no_config: bool) -> FileDefaults {
    if no_config {
        return FileDefaults::default();
    }
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return FileDefaults::default();
    };
    match toml::from_str::<FileDefaults>(&text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[gander] ignoring malformed {}: {e}", path.display());
            FileDefaults::default()
        }
    }
}

/// On first run (no config file) and an interactive TTY, prompt once and persist.
/// Returns the freshly written defaults, or `None` if nothing was written (non-TTY,
/// `--no-config`, or the file already exists). `force` (= `--reconfigure`) always prompts.
pub fn maybe_first_run(no_config: bool, force: bool) -> Option<FileDefaults> {
    if no_config && !force {
        return None;
    }
    let path = config_path();
    if path.exists() && !force {
        return None;
    }
    // Only prompt on a real TTY — the agent path must never block.
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return None;
    }

    let mut d = prompt_defaults();
    // `--reconfigure` asks only for the four selectors. Keep the user's classes.
    d.classes = load(false).classes;
    if let Err(e) = write(&d) {
        eprintln!("[gander] could not write {}: {e}", path.display());
        return None;
    }
    eprintln!("[gander] saved defaults to {}", path.display());
    Some(d)
}

/// The models to offer for a backend, its default first. Asks the backend for
/// its live list and falls back to the built-in one.
fn models_for(backend: &str) -> Vec<String> {
    let Some(kind) = crate::backend::BackendKind::from_str(backend) else {
        return Vec::new();
    };
    let cfg = crate::config::Config::load(&Default::default());
    crate::models::names(kind, &cfg, true).0
}

/// Raw config file contents, or `None` if no file exists.
pub fn raw_text() -> Option<String> {
    std::fs::read_to_string(config_path()).ok()
}

/// Delete the config file. Returns `true` if a file was removed.
pub fn clear() -> std::io::Result<bool> {
    let path = config_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn prompt_defaults() -> FileDefaults {
    eprintln!("gander first-run — choose defaults (↑/↓ to move, Enter to select):");

    // Backend first, then a model of that backend → always a compatible pair.
    let backend = pick("Primary backend", &["agy", "claude", "codex"]);
    let model = pick_model("Primary model", &backend);

    let fb_backend = pick("Fallback backend", &["agy", "claude", "codex", "none"]);
    let fallback_model = if fb_backend == "none" {
        "none".to_string()
    } else {
        pick_model("Fallback model", &fb_backend)
    };

    FileDefaults {
        model: Some(model),
        backend: Some(backend),
        fallback_model: Some(fallback_model),
        fallback_backend: Some(fb_backend),
        classes: BTreeMap::new(),
    }
}

fn pick_model(label: &str, backend: &str) -> String {
    let models = models_for(backend);
    let refs: Vec<&str> = models.iter().map(String::as_str).collect();
    pick(label, &refs)
}

/// Arrow-key single-select over `choices` (the first is highlighted by default).
/// The interactive UI renders to stderr; on cancel/error it falls back to the first
/// choice. Only ever called behind the TTY gate in `maybe_first_run`.
fn pick(label: &str, choices: &[&str]) -> String {
    let fallback = choices.first().copied().unwrap_or("").to_string();
    match inquire::Select::new(label, choices.to_vec())
        .with_help_message("↑/↓ move · Enter select · type to filter")
        .prompt()
    {
        Ok(choice) => choice.to_string(),
        Err(_) => fallback,
    }
}

pub fn write(d: &FileDefaults) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        set_mode(parent, 0o700);
    }
    let text = toml::to_string_pretty(d).map_err(std::io::Error::other)?;
    std::fs::write(&path, text)?;
    set_mode(&path, 0o600);
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_table_round_trips_and_is_optional() {
        let d: FileDefaults = toml::from_str("model = \"pro\"\n").unwrap();
        assert!(d.classes.is_empty());
        // No empty `[classes]` header is written for a user who has none.
        assert!(!toml::to_string_pretty(&d).unwrap().contains("classes"));

        let d: FileDefaults =
            toml::from_str("model = \"pro\"\n[classes]\nmid = [\"sonnet\"]\n").unwrap();
        assert_eq!(d.classes["mid"], ["sonnet"]);
        let back: FileDefaults = toml::from_str(&toml::to_string_pretty(&d).unwrap()).unwrap();
        assert_eq!(back.classes["mid"], ["sonnet"]);
    }

    #[test]
    fn unknown_backend_offers_no_models() {
        assert!(models_for("nonsense").is_empty());
    }
}
