//! claude adapter — piped subprocess, json-unwrap, vision-only floor.
//! Piped subprocess, JSON-unwrapped output, vision-only floor.
//!
//! claude has no audio modality → `supports_audio() == false`, which the pipeline
//! turns into `status="partial"` with `transcript=null` for speech-capable media.
//! We strip API-key env so claude runs on the $0 Max OAuth session.

use std::collections::HashMap;

use crate::config::Config;

use super::{env_minus, AgentBackend};

const STRIP_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDE_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];

pub struct Claude {
    pub bin: String,
}

impl AgentBackend for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    /// `claude -p <prompt> --model <m> --output-format json
    /// --permission-mode bypassPermissions`. No `--add-dir`; claude reads the path
    /// named in the prompt under bypassPermissions.
    fn build_argv(
        &self,
        prompt: &str,
        _add_dir: &str,
        model: &str,
        _timeout_s: f64,
    ) -> Vec<String> {
        vec![
            self.bin.clone(),
            "-p".into(),
            prompt.to_string(),
            "--model".into(),
            model.to_string(),
            "--output-format".into(),
            "json".into(),
            "--permission-mode".into(),
            "bypassPermissions".into(),
        ]
    }

    fn needs_pty(&self) -> bool {
        false
    }

    fn env(&self) -> HashMap<String, String> {
        env_minus(STRIP_KEYS)
    }

    fn supports_audio(&self) -> bool {
        false
    }

    fn timeout_s(&self, cfg: &Config) -> f64 {
        cfg.print_timeout_s.min(cfg.claude_timeout_s)
    }
}
