//! codex adapter — `codex exec`, piped, agy-class modality. Resolved against the
//! live Codex v0.130.0 probe (see PLAN.md §4 / §8):
//!
//! - `exec` is the headless subcommand; the **prompt is positional** (no `-p`).
//! - `--yolo` = `approval:never` + `sandbox:danger-full-access` (full bypass);
//!   the file path lives inside the prompt (no `--add-dir`).
//! - `--skip-git-repo-check` since gander may run outside a git repo.
//! - `--color never` keeps stdout clean; reasoning effort pinned to `low` (the
//!   `xhigh` default is ~45s/image). Vision works ⇒ `supports_audio() == true`.
//! - Runs piped (no PTY); reads stdin, so the piped runner hands it `/dev/null`.

use std::collections::HashMap;

use crate::config::Config;

use super::AgentBackend;

pub struct Codex {
    pub bin: String,
}

impl AgentBackend for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    /// `codex exec --yolo --skip-git-repo-check --color never
    /// -c model_reasoning_effort="low" -m <model> <prompt>`. `model` is a concrete
    /// codex id (gpt-5.5 / gpt-5.4 / gpt-5.4-mini), resolved by `model_for`.
    fn build_argv(
        &self,
        prompt: &str,
        _add_dir: &str,
        model: &str,
        _timeout_s: f64,
    ) -> Vec<String> {
        let mut argv = vec![
            self.bin.clone(),
            "exec".into(),
            "--yolo".into(),
            "--skip-git-repo-check".into(),
            "--color".into(),
            "never".into(),
            "-c".into(),
            "model_reasoning_effort=\"low\"".into(),
        ];
        if !model.is_empty() {
            argv.push("-m".into());
            argv.push(model.to_string());
        }
        argv.push(prompt.to_string());
        argv
    }

    fn needs_pty(&self) -> bool {
        false
    }

    fn env(&self) -> HashMap<String, String> {
        // codex rides the user's ChatGPT login — strip nothing.
        std::env::vars().collect()
    }

    fn supports_audio(&self) -> bool {
        true
    }

    fn timeout_s(&self, cfg: &Config) -> f64 {
        cfg.print_timeout_s
    }
}
