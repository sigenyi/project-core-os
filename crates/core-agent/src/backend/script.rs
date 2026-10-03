//! Scripted intents: a model-free backend that replays a file of intents.
//!
//! Each line of the script is one intent, exactly as a model would emit it. Every
//! completion returns the next line, whatever the conversation says, so a test can
//! drive the real agent loop, the Guardian socket, confirmations and the audit log
//! without a language model (`docs/TRAINING.md`, D9). The intents are untrusted like
//! any model output: the agent validates them and the Guardian applies policy.
//!
//! Lines that are empty or start with `#` are skipped. When the script runs out, the
//! backend answers with a `respond` intent, so a request always ends.

use std::fs;
use std::path::{Path, PathBuf};

use super::{BackendError, CompletionRequest, InferenceBackend};

pub struct ScriptedIntents {
    path: PathBuf,
    lines: Vec<String>,
    next: usize,
}

/// What the backend answers once every scripted intent has been used.
pub const SCRIPT_DONE: &str = r#"{"action":"respond","args":{"message":"The script has no more steps."}}"#;

impl ScriptedIntents {
    pub fn from_lines(path: &Path, text: &str) -> Self {
        let lines =
            text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect();
        ScriptedIntents { path: path.to_path_buf(), lines, next: 0 }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text =
            fs::read_to_string(path).map_err(|e| format!("cannot read intent script {}: {e}", path.display()))?;
        Ok(Self::from_lines(path, &text))
    }
}

impl InferenceBackend for ScriptedIntents {
    fn name(&self) -> String {
        format!("scripted intents ({}, {} left)", self.path.display(), self.lines.len() - self.next)
    }

    fn complete(&mut self, _request: &CompletionRequest) -> Result<String, BackendError> {
        let line = self.lines.get(self.next).cloned().unwrap_or_else(|| SCRIPT_DONE.to_string());
        self.next = (self.next + 1).min(self.lines.len());
        Ok(line)
    }
}

/// A backend that only reports why its script could not be loaded. The error is a
/// bad response, not "unavailable", so the agent does not quietly fall back to the
/// rescue planner in the middle of a test.
pub struct BrokenScript(pub String);

impl InferenceBackend for BrokenScript {
    fn name(&self) -> String {
        "scripted intents (not loaded)".into()
    }

    fn complete(&mut self, _request: &CompletionRequest) -> Result<String, BackendError> {
        Err(BackendError::BadResponse(self.0.clone()))
    }

    fn health(&mut self) -> Result<(), BackendError> {
        Err(BackendError::BadResponse(self.0.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> CompletionRequest<'static> {
        CompletionRequest { messages: &[], grammar: None, max_tokens: 1, temperature: 0.0 }
    }

    #[test]
    fn replays_lines_in_order_then_says_it_is_done() {
        let mut s = ScriptedIntents::from_lines(
            Path::new("t"),
            "# comment\n{\"action\":\"disk_usage\",\"args\":{}}\n\n  {\"action\":\"respond\",\"args\":{\"message\":\"ok\"}}\n",
        );
        assert_eq!(s.complete(&req()).unwrap(), r#"{"action":"disk_usage","args":{}}"#);
        assert_eq!(s.complete(&req()).unwrap(), r#"{"action":"respond","args":{"message":"ok"}}"#);
        assert_eq!(s.complete(&req()).unwrap(), SCRIPT_DONE);
        assert_eq!(s.complete(&req()).unwrap(), SCRIPT_DONE);
    }

    #[test]
    fn a_missing_script_is_an_error_not_a_fallback() {
        let err = ScriptedIntents::load(Path::new("/nonexistent/script.jsonl")).err().unwrap();
        let mut b = BrokenScript(err);
        assert!(matches!(b.complete(&req()), Err(BackendError::BadResponse(_))));
        assert!(matches!(b.health(), Err(BackendError::BadResponse(_))));
    }
}
