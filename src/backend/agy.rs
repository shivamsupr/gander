//! agy adapter — drives agy under a PTY with full modality (vision/audio/video).
//!
//! agy uses the user's Google login, so we strip NOTHING from the environment; we
//! pass argv as a vector (no shell string-concat) straight into the PTY runner.

use std::collections::HashMap;

use crate::config::Config;

use super::AgentBackend;

pub struct Agy {
    pub bin: String,
}

impl AgentBackend for Agy {
    fn name(&self) -> &'static str {
        "agy"
    }

    /// `agy -p <prompt> --model <m> --dangerously-skip-permissions --add-dir <dir>
    /// --print-timeout <s>s`. `--print-timeout` gives up AFTER our wall-clock does.
    fn build_argv(&self, prompt: &str, add_dir: &str, model: &str, timeout_s: f64) -> Vec<String> {
        let agy_print_to = (timeout_s as i64 - 20).max(30);
        vec![
            self.bin.clone(),
            "-p".into(),
            prompt.to_string(),
            "--model".into(),
            model.to_string(),
            "--dangerously-skip-permissions".into(),
            "--add-dir".into(),
            add_dir.to_string(),
            "--print-timeout".into(),
            format!("{agy_print_to}s"),
        ]
    }

    fn needs_pty(&self) -> bool {
        true
    }

    fn env(&self) -> HashMap<String, String> {
        // Strip nothing — agy rides the user's Google login.
        std::env::vars().collect()
    }

    fn supports_audio(&self) -> bool {
        true
    }

    fn timeout_s(&self, cfg: &Config) -> f64 {
        cfg.print_timeout_s
    }
}
