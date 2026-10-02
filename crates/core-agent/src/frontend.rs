//! The agent's interface to whatever presents it to the human (the console shell).

use std::path::Path;

use core_protocol::Risk;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent<'a> {
    /// The model is being consulted (step number from 1).
    Thinking { step: usize },
    /// The model's one-line reasoning.
    Thought(&'a str),
    /// A system action is about to run.
    ActionStarted { description: &'a str, risk: Risk },
    /// It finished. `detail` is a one-line summary of a failure.
    ActionFinished { description: &'a str, success: bool, detail: &'a str },
    /// Something the user should know (fallbacks, degraded modes).
    Notice(&'a str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmRequest<'a> {
    pub summary: &'a str,
    pub risk: Risk,
}

pub trait Frontend {
    fn event(&mut self, event: AgentEvent<'_>);

    /// Ask the human (never the model) to approve an action.
    fn confirm(&mut self, request: &ConfirmRequest<'_>) -> bool;

    /// Hand the terminal to an interactive program until it exits.
    fn launch(&mut self, program: &Path, args: &[String]) -> Result<i32, String>;

    /// Polled between steps; true aborts the current request.
    fn cancelled(&mut self) -> bool {
        false
    }
}
