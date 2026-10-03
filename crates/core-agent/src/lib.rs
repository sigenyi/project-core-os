//! The C.O.R.E. agent: the unprivileged orchestrator between the human, the local
//! language model, the telemetry pipeline and the Guardian.
//!
//! Every external dependency is behind a trait so the loop can be tested and the
//! pieces swapped independently:
//!
//! | trait                        | production                 | alternatives            |
//! |------------------------------|----------------------------|-------------------------|
//! | [`backend::InferenceBackend`] | llama.cpp `llama-server`   | rescue planner, scripts |
//! | [`guardian::GuardianClient`]  | Unix socket                | in-process dry run      |
//! | [`telemetry::TelemetryProvider`] | `core-sensed` snapshot | live / static           |
//! | [`frontend::Frontend`]        | console shell              | test recorders          |
//! | [`voice::Transcriber`]        | whisper.cpp                |                         |

pub mod backend;
pub mod config;
pub mod files;
pub mod frontend;
pub mod guardian;
pub mod orchestrator;
pub mod prompt;
pub mod telemetry;
pub mod voice;

pub use config::AgentConfig;
pub use frontend::{AgentEvent, ConfirmRequest, Frontend};
pub use orchestrator::{Agent, Outcome};

use std::time::Duration;

use backend::{BrokenScript, InferenceBackend, LlamaServer, RescuePlanner, ScriptedIntents};
use config::BackendKind;

/// Build the inference backend described by the configuration.
pub fn backend_from_config(config: &AgentConfig) -> Box<dyn InferenceBackend> {
    match config.inference.backend {
        BackendKind::Llama => Box::new(LlamaServer::new(
            &config.inference.url,
            config.inference.model.clone(),
            Duration::from_secs(config.inference.timeout_secs),
        )),
        BackendKind::Rescue => Box::new(RescuePlanner),
        BackendKind::Script => match &config.inference.script {
            Some(path) => match ScriptedIntents::load(path) {
                Ok(script) => Box::new(script),
                Err(e) => Box::new(BrokenScript(e)),
            },
            None => Box::new(BrokenScript("the script backend needs inference.script (or --script)".into())),
        },
    }
}
