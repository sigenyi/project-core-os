//! The autonomous control loop.
//!
//! For each user request:
//!
//! 1. assemble context: conversation memory + live telemetry summary + the request
//! 2. ask the model for one intent (grammar-constrained to enabled actions)
//! 3. validate it; route it to the agent itself or to the Guardian
//! 4. turn the result into an OBSERVATION and loop, so failures are diagnosed and
//!    retried with a different approach, until the model responds to the user
//!
//! The loop is bounded (steps, consecutive failures) and refuses to re-run an action
//! that already failed with identical arguments. When it gives up, the model is
//! constrained to a final `respond` so the user always gets an explanation.

use std::collections::{HashSet, VecDeque};
use std::io;

use core_protocol::grammar::{AGENT_THOUGHT_MAX, GrammarOptions, gbnf};
use core_protocol::wire::{Capability, RejectKind, Response};
use core_protocol::{Action, CATALOG, Executor, Intent, Risk, ValidatedAction};

use crate::backend::{BackendError, ChatMessage, CompletionRequest, InferenceBackend, RescuePlanner};
use crate::config::AgentConfig;
use crate::frontend::{AgentEvent, ConfirmRequest, Frontend};
use crate::guardian::GuardianClient;
use crate::prompt::{self, ActionDoc, Exchange, OBSERVATION_PREFIX, PromptBuilder};
use crate::telemetry::TelemetryProvider;

/// How a request ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The model answered the user.
    Reply(String),
    /// The model needs more information from the user.
    Question(String),
    /// The agent could not complete the request; the text explains why.
    Failed(String),
    Cancelled,
}

pub struct Agent {
    config: AgentConfig,
    backend: Box<dyn InferenceBackend>,
    rescue: RescuePlanner,
    guardian: Box<dyn GuardianClient>,
    telemetry: Box<dyn TelemetryProvider>,
    docs: Vec<ActionDoc>,
    prompt: PromptBuilder,
    grammar: String,
    respond_grammar: String,
    history: VecDeque<Exchange>,
    next_id: u64,
}

/// Default capabilities when the Guardian cannot be asked (it will still enforce its
/// real policy when actions arrive).
fn assumed_capabilities() -> Vec<Capability> {
    CATALOG
        .iter()
        .filter(|s| s.executor == Executor::Guardian)
        .map(|s| Capability { action: s.name.into(), risk: s.risk, requires_confirmation: s.risk > Risk::Low })
        .collect()
}

fn action_docs(caps: &[Capability], config: &AgentConfig) -> Vec<ActionDoc> {
    let programs = config.programs.available();
    CATALOG
        .iter()
        .filter_map(|spec| match spec.executor {
            Executor::Agent if spec.name == "launch_program" => {
                (!programs.is_empty() || config.programs.allow_installed).then(|| ActionDoc {
                    spec,
                    requires_confirmation: false,
                    note: (!programs.is_empty()).then(|| format!("Installed: {}.", programs.join(", "))),
                })
            }
            Executor::Agent => Some(ActionDoc { spec, requires_confirmation: false, note: None }),
            Executor::Guardian => caps.iter().find(|c| c.action == spec.name).map(|c| ActionDoc {
                spec,
                requires_confirmation: c.requires_confirmation,
                note: None,
            }),
        })
        .collect()
}

/// The intent re-serialised canonically (catalog key order), as the model should write it.
fn canonical(thought: Option<&str>, v: &ValidatedAction) -> String {
    format!(
        r#"{{"thought":{},"action":"{}","args":{}}}"#,
        serde_json::Value::from(thought.unwrap_or("")),
        v.name(),
        v.spec.render_args(&v.args)
    )
}

/// What one step produced.
struct StepResult {
    observation: String,
    success: bool,
}

impl Agent {
    pub fn new(
        config: AgentConfig,
        backend: Box<dyn InferenceBackend>,
        mut guardian: Box<dyn GuardianClient>,
        telemetry: Box<dyn TelemetryProvider>,
    ) -> Self {
        let caps = guardian.capabilities().unwrap_or_else(|e| {
            log::warn!("could not ask the Guardian for its capabilities ({e}); assuming defaults");
            assumed_capabilities()
        });
        let docs = action_docs(&caps, &config);
        let names: Vec<&str> = docs.iter().map(|d| d.spec.name).collect();
        let grammar = gbnf(&GrammarOptions { actions: Some(&names), thought_max: AGENT_THOUGHT_MAX });
        let respond_grammar = gbnf(&GrammarOptions { actions: Some(&["respond"]), thought_max: AGENT_THOUGHT_MAX });
        let prompt = PromptBuilder::new(&docs, config.inference.context_tokens, config.inference.max_tokens as usize);
        Agent {
            config,
            backend,
            rescue: RescuePlanner,
            guardian,
            telemetry,
            docs,
            prompt,
            grammar,
            respond_grammar,
            history: VecDeque::new(),
            next_id: 1,
        }
    }

    pub fn backend_name(&self) -> String {
        self.backend.name()
    }

    pub fn guardian_name(&self) -> String {
        self.guardian.describe()
    }

    pub fn set_backend(&mut self, backend: Box<dyn InferenceBackend>) {
        self.backend = backend;
    }

    pub fn backend_health(&mut self) -> Result<(), BackendError> {
        self.backend.health()
    }

    /// Forget the conversation.
    pub fn reset(&mut self) {
        self.history.clear();
    }

    pub fn action_names(&self) -> Vec<&'static str> {
        self.docs.iter().map(|d| d.spec.name).collect()
    }

    pub fn system_prompt(&self) -> &str {
        self.prompt.system_prompt()
    }

    pub fn grammar(&self) -> &str {
        &self.grammar
    }

    pub fn telemetry_summary(&mut self) -> String {
        core_sense::summary(&self.telemetry.snapshot())
    }

    fn remember(&mut self, request: &str, reply: String) {
        self.history.push_back(Exchange { request: request.to_string(), reply });
        while self.history.len() > self.config.agent.history_turns {
            self.history.pop_front();
        }
    }

    fn infer(
        &mut self,
        messages: &[ChatMessage],
        grammar: &str,
        rescue: &mut bool,
        ui: &mut dyn Frontend,
    ) -> Result<String, BackendError> {
        let req = CompletionRequest {
            messages,
            grammar: Some(grammar),
            max_tokens: self.config.inference.max_tokens,
            temperature: self.config.inference.temperature,
        };
        if *rescue {
            return self.rescue.complete(&req);
        }
        match self.backend.complete(&req) {
            Err(BackendError::Unavailable(e)) if self.config.inference.fallback_to_rescue => {
                ui.event(AgentEvent::Notice(&format!("Language model unavailable ({e}); using rescue mode.")));
                *rescue = true;
                self.rescue.complete(&req)
            }
            other => other,
        }
    }

    /// Handle one user request to completion.
    pub fn handle(&mut self, request: &str, ui: &mut dyn Frontend) -> Outcome {
        let request = request.trim();
        let state = core_sense::summary(&self.telemetry.snapshot());
        let history: Vec<Exchange> = self.history.iter().cloned().collect();
        let mut transcript: Vec<ChatMessage> = Vec::new();
        let mut failed: HashSet<String> = HashSet::new();
        let mut consecutive_failures = 0;
        let mut rescue = false;

        for step in 1..=self.config.agent.max_steps {
            if ui.cancelled() {
                return Outcome::Cancelled;
            }
            ui.event(AgentEvent::Thinking { step });
            let messages = self.prompt.build(&history, &state, request, &transcript);
            let grammar = self.grammar.clone();
            let raw = match self.infer(&messages, &grammar, &mut rescue, ui) {
                Ok(raw) => raw,
                Err(e) => return Outcome::Failed(format!("I cannot think right now: {e}")),
            };

            let parsed = Intent::parse(&raw).map_err(|e| e.to_string()).and_then(|intent| {
                ValidatedAction::from_intent(&intent).map(|v| (intent, v)).map_err(|e| e.to_string())
            });
            let (intent, action) = match parsed {
                Ok(ok) => ok,
                Err(error) => {
                    transcript.push(ChatMessage::assistant(prompt::clip_middle(&raw, 600)));
                    transcript.push(ChatMessage::user(prompt::observe_rejection("", RejectKind::Invalid, &error)));
                    consecutive_failures += 1;
                    if consecutive_failures >= self.config.agent.max_consecutive_failures {
                        break;
                    }
                    continue;
                }
            };
            if let Some(t) = intent.thought.as_deref().filter(|t| !t.is_empty()) {
                ui.event(AgentEvent::Thought(t));
            }
            let reply_json = canonical(intent.thought.as_deref(), &action);
            transcript.push(ChatMessage::assistant(reply_json.clone()));

            match &action.action {
                Action::Respond { message } => {
                    let message = message.clone();
                    self.remember(request, reply_json);
                    return Outcome::Reply(message);
                }
                Action::AskUser { question } => {
                    let question = question.clone();
                    self.remember(request, reply_json);
                    return Outcome::Question(question);
                }
                _ => {}
            }

            let key = format!("{}{}", action.name(), action.spec.render_args(&action.args));
            let result = if failed.contains(&key) {
                StepResult {
                    observation: format!(
                        "{OBSERVATION_PREFIX} (repeat)\nThis exact action already failed in this task. Choose a different approach or respond to the user."
                    ),
                    success: false,
                }
            } else {
                self.perform(&intent, &action, ui)
            };
            transcript.push(ChatMessage::user(result.observation));
            if result.success {
                consecutive_failures = 0;
            } else {
                failed.insert(key);
                consecutive_failures += 1;
                if consecutive_failures >= self.config.agent.max_consecutive_failures {
                    break;
                }
            }
        }

        // Out of steps or retries: make the model explain, constrained to `respond`.
        transcript.push(ChatMessage::user(format!(
            "{OBSERVATION_PREFIX} (limit reached)\nStop now. Respond to the user: say what you tried, what failed, and what they could do next."
        )));
        let messages = self.prompt.build(&history, &state, request, &transcript);
        let grammar = self.respond_grammar.clone();
        let fallback =
            "I could not complete that. Several attempts failed; check the logs or try rephrasing.".to_string();
        let message = self
            .infer(&messages, &grammar, &mut rescue, ui)
            .ok()
            .and_then(|raw| Intent::parse(&raw).ok())
            .and_then(|i| ValidatedAction::from_intent(&i).ok())
            .and_then(|v| match v.action {
                Action::Respond { message } => Some(message),
                _ => None,
            })
            .unwrap_or(fallback);
        self.remember(
            request,
            format!(
                r#"{{"thought":"","action":"respond","args":{{"message":{}}}}}"#,
                serde_json::Value::from(message.as_str())
            ),
        );
        Outcome::Failed(message)
    }

    fn perform(&mut self, intent: &Intent, action: &ValidatedAction, ui: &mut dyn Frontend) -> StepResult {
        let max = self.config.agent.observation_chars;
        match &action.action {
            Action::GetTelemetry { section } => {
                let snap = self.telemetry.snapshot();
                let body = core_sense::section(&snap, section.as_str())
                    .map(|v| match v {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| "unknown section".into());
                StepResult {
                    observation: prompt::observe_text(&format!("get_telemetry {section}"), &body, max),
                    success: true,
                }
            }
            Action::LaunchProgram { program, args } => self.launch(program.as_str(), args, ui),
            Action::ListDirectory { path } => {
                let result = crate::files::list_directory(path.as_ref(), &self.config.files);
                self.local_result(&action.describe(), action.name(), result, ui)
            }
            Action::ReadFile { path, lines, tail } => {
                let result = crate::files::read_file(path.as_ref(), *lines as usize, *tail, &self.config.files);
                self.local_result(&action.describe(), action.name(), result, ui)
            }
            _ => self.privileged(intent, action, ui),
        }
    }

    /// Report an action the agent performed itself (reads) like a Guardian action.
    fn local_result(
        &self,
        description: &str,
        action: &str,
        result: Result<String, String>,
        ui: &mut dyn Frontend,
    ) -> StepResult {
        ui.event(AgentEvent::ActionStarted { description, risk: Risk::Observe });
        let max = self.config.agent.observation_chars;
        match result {
            Ok(body) => {
                ui.event(AgentEvent::ActionFinished { description, success: true, detail: "" });
                StepResult {
                    observation: prompt::observe_text(&format!("{action}: succeeded"), &body, max),
                    success: true,
                }
            }
            Err(e) => {
                ui.event(AgentEvent::ActionFinished { description, success: false, detail: &e });
                StepResult {
                    observation: format!("{OBSERVATION_PREFIX} ({action}: FAILED)\n{e}\n{}", prompt::GUIDE_FAILED),
                    success: false,
                }
            }
        }
    }

    fn launch(&mut self, program: &str, args: &[String], ui: &mut dyn Frontend) -> StepResult {
        let fail = |observation: String| StepResult { observation, success: false };
        let Some(path) = self.config.programs.resolve(program) else {
            let available = self.config.programs.available().join(", ");
            return fail(format!(
                "{OBSERVATION_PREFIX} (launch_program: FAILED)\n{program} is not installed or not allowed. Available programs: {available}. It may need install_package first."
            ));
        };
        // Options can make programs run commands (vim -c, less +!) and URLs make them
        // contact the network (a way to leak data), so let the human decide.
        if args.iter().any(|a| a.starts_with('-') || a.starts_with('+') || a.contains("://")) {
            let summary = format!("Open {program} {}", args.join(" "));
            if !ui.confirm(&ConfirmRequest { summary: &summary, risk: Risk::Medium }) {
                return fail(prompt::observe_rejection("launch_program", RejectKind::Declined, ""));
            }
        }
        let description = format!("Open {program}");
        ui.event(AgentEvent::ActionStarted { description: &description, risk: Risk::Low });
        match ui.launch(&path, args) {
            Ok(code) => {
                ui.event(AgentEvent::ActionFinished { description: &description, success: true, detail: "" });
                StepResult {
                    observation: format!(
                        "{OBSERVATION_PREFIX} (launch_program: the user used {program} and closed it, exit code {code})"
                    ),
                    success: true,
                }
            }
            Err(e) => {
                ui.event(AgentEvent::ActionFinished { description: &description, success: false, detail: &e });
                fail(format!("{OBSERVATION_PREFIX} (launch_program: FAILED)\n{e}"))
            }
        }
    }

    fn privileged(&mut self, intent: &Intent, action: &ValidatedAction, ui: &mut dyn Frontend) -> StepResult {
        let id = self.next_id;
        self.next_id += 1;
        let description = action.describe();
        ui.event(AgentEvent::ActionStarted { description: &description, risk: action.risk() });
        let mut wire_intent = action.to_intent();
        wire_intent.thought = intent.thought.clone();
        let response = self.guardian.execute(id, &wire_intent).and_then(|r| match r {
            Response::ConfirmationRequired { token, summary, risk, .. } => {
                let approved = ui.confirm(&ConfirmRequest { summary: &summary, risk });
                self.guardian.confirm(&token, approved)
            }
            other => Ok(other),
        });
        let result = self.observe(action.name(), response);
        let detail = if result.success { String::new() } else { prompt::failure_detail(&result.observation) };
        ui.event(AgentEvent::ActionFinished { description: &description, success: result.success, detail: &detail });
        result
    }

    fn observe(&self, action: &str, response: io::Result<Response>) -> StepResult {
        let max = self.config.agent.observation_chars;
        match response {
            Ok(Response::Executed { report, .. }) => {
                StepResult { observation: prompt::observe_report(&report, max), success: report.success }
            }
            Ok(Response::Rejected { kind, reason, .. }) => {
                StepResult { observation: prompt::observe_rejection(action, kind, &reason), success: false }
            }
            Ok(Response::Error { message }) => StepResult {
                observation: format!("{OBSERVATION_PREFIX} ({action}: Guardian error)\n{message}"),
                success: false,
            },
            Ok(other) => StepResult {
                observation: format!("{OBSERVATION_PREFIX} ({action}: unexpected reply)\n{other:?}"),
                success: false,
            },
            Err(e) => StepResult {
                observation: format!(
                    "{OBSERVATION_PREFIX} ({action}: FAILED)\nThe Guardian service is unreachable: {e}. System actions are impossible until it runs again; tell the user."
                ),
                success: false,
            },
        }
    }
}
