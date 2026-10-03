//! Inference backends: anything that turns a conversation into the next intent.

mod llama;
mod rescue;
mod script;

use std::fmt;

pub use llama::LlamaServer;
pub use rescue::{RescuePlanner, plan as rescue_plan};
pub use script::{BrokenScript, SCRIPT_DONE, ScriptedIntents};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage { role: Role::System, content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage { role: Role::User, content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage { role: Role::Assistant, content: content.into() }
    }
}

pub struct CompletionRequest<'a> {
    pub messages: &'a [ChatMessage],
    /// GBNF grammar constraining the output, if the backend supports it.
    pub grammar: Option<&'a str>,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// The backend could not be reached (server down, still loading).
    Unavailable(String),
    /// The backend answered but the answer was unusable.
    BadResponse(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Unavailable(e) => write!(f, "language model unavailable: {e}"),
            BackendError::BadResponse(e) => write!(f, "language model returned an unusable response: {e}"),
        }
    }
}

impl std::error::Error for BackendError {}

pub trait InferenceBackend: Send {
    /// Short description for status displays.
    fn name(&self) -> String;

    /// Produce the next assistant message (one intent JSON object).
    fn complete(&mut self, request: &CompletionRequest) -> Result<String, BackendError>;

    /// Cheap readiness probe.
    fn health(&mut self) -> Result<(), BackendError> {
        Ok(())
    }
}
