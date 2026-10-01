//! The autonomous control loop end to end, with a scripted model, a fake Guardian
//! and a recording frontend.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use core_agent::backend::{BackendError, ChatMessage, CompletionRequest, InferenceBackend};
use core_agent::guardian::GuardianClient;
use core_agent::telemetry::StaticTelemetry;
use core_agent::{Agent, AgentConfig, AgentEvent, ConfirmRequest, Frontend, Outcome};
use core_protocol::wire::{Capability, ExecutionReport, RejectKind, Response, StepReport};
use core_protocol::{CATALOG, Executor, Intent, Risk};
use core_sense::Snapshot;
use core_sense::snapshot::{Host, Insight, Severity};

/// One model call: the messages it saw and the grammar it was given.
type Call = (Vec<ChatMessage>, Option<String>);

#[derive(Clone, Default)]
struct Calls(Arc<Mutex<Vec<Call>>>);

struct ScriptedModel {
    outputs: VecDeque<Result<String, BackendError>>,
    calls: Calls,
}

impl InferenceBackend for ScriptedModel {
    fn name(&self) -> String {
        "scripted".into()
    }
    fn complete(&mut self, req: &CompletionRequest) -> Result<String, BackendError> {
        self.calls.0.lock().unwrap().push((req.messages.to_vec(), req.grammar.map(String::from)));
        self.outputs.pop_front().unwrap_or_else(|| {
            Ok(r#"{"thought":"","action":"respond","args":{"message":"(script exhausted)"}}"#.into())
        })
    }
}

#[derive(Clone, Default)]
struct GuardianLog(Arc<Mutex<Vec<String>>>);

/// (substring of the intent JSON, how the fake Guardian answers); first match wins.
#[derive(Clone)]
enum Behaviour {
    Succeed(&'static str),
    Fail(&'static str),
    NeedConfirm,
    Deny(&'static str),
}

struct FakeGuardian {
    rules: Vec<(&'static str, Behaviour)>,
    disabled: Vec<&'static str>,
    log: GuardianLog,
    pending: Option<(u64, String)>,
    unreachable: bool,
}

fn report(action: &str, success: bool, stdout: &str, stderr: &str) -> ExecutionReport {
    ExecutionReport {
        action: action.into(),
        success,
        steps: vec![StepReport {
            description: action.into(),
            success,
            exit_code: Some(if success { 0 } else { 1 }),
            stdout: stdout.into(),
            stderr: stderr.into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

impl GuardianClient for FakeGuardian {
    fn describe(&self) -> String {
        "fake".into()
    }
    fn capabilities(&mut self) -> io::Result<Vec<Capability>> {
        Ok(CATALOG
            .iter()
            .filter(|s| s.executor == Executor::Guardian && !self.disabled.contains(&s.name))
            .map(|s| Capability { action: s.name.into(), risk: s.risk, requires_confirmation: s.risk > Risk::Low })
            .collect())
    }
    fn execute(&mut self, id: u64, intent: &Intent) -> io::Result<Response> {
        if self.unreachable {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no socket"));
        }
        self.log.0.lock().unwrap().push(intent.to_json());
        let json = intent.to_json();
        let behaviour = self.rules.iter().find(|(pattern, _)| json.contains(pattern)).map(|(_, b)| b.clone());
        Ok(match behaviour.unwrap_or(Behaviour::Succeed("")) {
            Behaviour::Succeed(out) => Response::Executed { id, report: report(&intent.action, true, out, "") },
            Behaviour::Fail(err) => Response::Executed { id, report: report(&intent.action, false, "", err) },
            Behaviour::Deny(reason) => Response::Rejected { id, kind: RejectKind::Denied, reason: reason.into() },
            Behaviour::NeedConfirm => {
                self.pending = Some((id, intent.action.clone()));
                Response::ConfirmationRequired {
                    id,
                    token: "tok".into(),
                    summary: format!("do {}", intent.action),
                    risk: Risk::High,
                    expires_in_secs: 60,
                }
            }
        })
    }
    fn confirm(&mut self, token: &str, approve: bool) -> io::Result<Response> {
        assert_eq!(token, "tok");
        let (id, action) = self.pending.take().expect("pending confirmation");
        self.log.0.lock().unwrap().push(format!("confirm {action} {approve}"));
        Ok(if approve {
            Response::Executed { id, report: report(&action, true, "installed", "") }
        } else {
            Response::Rejected { id, kind: RejectKind::Declined, reason: "declined".into() }
        })
    }
}

#[derive(Default)]
struct Recorder {
    events: Vec<String>,
    approve: bool,
    confirmations: Vec<String>,
}

impl Frontend for Recorder {
    fn event(&mut self, e: AgentEvent<'_>) {
        self.events.push(format!("{e:?}"));
    }
    fn confirm(&mut self, r: &ConfirmRequest<'_>) -> bool {
        self.confirmations.push(r.summary.to_string());
        self.approve
    }
    fn launch(&mut self, program: &Path, _args: &[String]) -> Result<i32, String> {
        self.events.push(format!("launched {}", program.display()));
        Ok(0)
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        host: Host { hostname: "core-test".into(), os: "C.O.R.E. OS".into(), ..Default::default() },
        insights: vec![Insight {
            severity: Severity::Warning,
            subsystem: "audio".into(),
            message: "muted".into(),
            hint: None,
        }],
        ..Default::default()
    }
}

struct Harness {
    agent: Agent,
    calls: Calls,
    guardian: GuardianLog,
}

fn harness(outputs: &[&str], rules: Vec<(&'static str, Behaviour)>) -> Harness {
    harness_with(outputs.iter().map(|o| Ok(o.to_string())).collect(), rules, vec![], false, AgentConfig::default())
}

fn harness_with(
    outputs: Vec<Result<String, BackendError>>,
    rules: Vec<(&'static str, Behaviour)>,
    disabled: Vec<&'static str>,
    unreachable: bool,
    config: AgentConfig,
) -> Harness {
    let calls = Calls::default();
    let log = GuardianLog::default();
    let model = ScriptedModel { outputs: outputs.into(), calls: calls.clone() };
    let guardian = FakeGuardian { rules, disabled, log: log.clone(), pending: None, unreachable };
    let agent = Agent::new(config, Box::new(model), Box::new(guardian), Box::new(StaticTelemetry(snapshot())));
    Harness { agent, calls, guardian: log }
}

fn last_user_message(calls: &Calls, call: usize) -> String {
    let c = calls.0.lock().unwrap();
    c[call].0.iter().rev().find(|m| m.role == core_agent::backend::Role::User).unwrap().content.clone()
}

#[test]
fn simple_request_executes_and_replies() {
    let mut h = harness(
        &[
            r#"{"thought":"Set volume.","action":"set_volume","args":{"percent":30}}"#,
            r#"{"thought":"Done.","action":"respond","args":{"message":"Volume is 30%."}}"#,
        ],
        vec![],
    );
    let mut ui = Recorder::default();
    assert_eq!(h.agent.handle("volume to 30 please", &mut ui), Outcome::Reply("Volume is 30%.".into()));
    assert_eq!(
        h.guardian.0.lock().unwrap().as_slice(),
        [r#"{"thought":"Set volume.","action":"set_volume","args":{"percent":30}}"#]
    );
    // The first prompt carries the telemetry summary and the request.
    let first = last_user_message(&h.calls, 0);
    assert!(first.contains("host: core-test") && first.contains("- [warning] audio: muted"), "{first}");
    assert!(first.ends_with("REQUEST: volume to 30 please"));
    // The second carries the observation.
    assert!(last_user_message(&h.calls, 1).starts_with("OBSERVATION (set_volume: succeeded)"));
    assert!(ui.events.iter().any(|e| e.contains("ActionStarted") && e.contains("Set volume to 30%")));
}

#[test]
fn failures_feed_back_and_the_model_self_corrects() {
    let mut h = harness(
        &[
            r#"{"thought":"Install it.","action":"install_package","args":{"package":"chromium-browser"}}"#,
            r#"{"thought":"Name differs; search.","action":"search_packages","args":{"query":"chromium"}}"#,
            r#"{"thought":"Use the found name.","action":"install_package","args":{"package":"chromium"}}"#,
            r#"{"thought":"Installed.","action":"respond","args":{"message":"Chromium is installed."}}"#,
        ],
        vec![
            ("chromium-browser", Behaviour::Fail("error: target not found: chromium-browser")),
            ("search_packages", Behaviour::Succeed("extra/chromium 130.0 A web browser")),
            ("install_package", Behaviour::NeedConfirm),
        ],
    );
    let mut ui = Recorder { approve: true, ..Default::default() };
    let outcome = h.agent.handle("install a web browser", &mut ui);
    assert_eq!(outcome, Outcome::Reply("Chromium is installed.".into()));
    assert!(last_user_message(&h.calls, 1).contains("error: target not found: chromium-browser"));
    assert!(last_user_message(&h.calls, 2).contains("extra/chromium"));
    assert!(last_user_message(&h.calls, 3).starts_with("OBSERVATION (install_package: succeeded)"));
    assert_eq!(ui.confirmations, ["do install_package"]);
}

#[test]
fn errors_reach_the_model_verbatim() {
    let mut h = harness(
        &[
            r#"{"thought":"Restart.","action":"restart_service","args":{"service":"wpa_supplicant"}}"#,
            r#"{"thought":"Wrong unit; this system uses iwd.","action":"restart_service","args":{"service":"iwd"}}"#,
            r#"{"thought":"ok","action":"respond","args":{"message":"Restarted iwd."}}"#,
        ],
        vec![(
            "restart_service",
            Behaviour::Fail("Failed to restart wpa_supplicant.service: Unit wpa_supplicant.service not found."),
        )],
    );
    let mut ui = Recorder::default();
    h.agent.handle("fix my wifi", &mut ui);
    let obs = last_user_message(&h.calls, 1);
    assert!(obs.starts_with("OBSERVATION (restart_service: FAILED, exit code 1)"), "{obs}");
    assert!(obs.contains("Unit wpa_supplicant.service not found."));
    assert!(ui.events.iter().any(|e| e.contains("ActionFinished") && e.contains("success: false")));
}

#[test]
fn confirmation_is_asked_of_the_human_and_declines_are_observed() {
    let mut h = harness(
        &[
            r#"{"thought":"Reboot.","action":"reboot","args":{}}"#,
            r#"{"thought":"User said no.","action":"respond","args":{"message":"Okay, not rebooting."}}"#,
        ],
        vec![("reboot", Behaviour::NeedConfirm)],
    );
    let mut ui = Recorder { approve: false, ..Default::default() };
    assert_eq!(h.agent.handle("reboot", &mut ui), Outcome::Reply("Okay, not rebooting.".into()));
    assert_eq!(ui.confirmations, ["do reboot"]);
    assert_eq!(h.guardian.0.lock().unwrap().last().unwrap(), "confirm reboot false");
    assert!(last_user_message(&h.calls, 1).contains("the user declined"));
}

#[test]
fn identical_failed_actions_are_not_rerun_and_limits_force_a_reply() {
    let same = r#"{"thought":"again","action":"start_service","args":{"service":"bluetooth"}}"#;
    let mut h = harness(
        &[
            same,
            same,
            same,
            r#"{"thought":"explain","action":"respond","args":{"message":"Bluetooth will not start; the adapter seems missing."}}"#,
        ],
        vec![("start_service", Behaviour::Fail("Job for bluetooth.service failed."))],
    );
    let mut ui = Recorder::default();
    let outcome = h.agent.handle("start bluetooth", &mut ui);
    assert_eq!(outcome, Outcome::Failed("Bluetooth will not start; the adapter seems missing.".into()));
    assert_eq!(h.guardian.0.lock().unwrap().len(), 1, "the repeat was never sent to the Guardian");
    assert!(last_user_message(&h.calls, 2).contains("already failed"));
    // The final call may only produce `respond`.
    let calls = h.calls.0.lock().unwrap();
    let final_grammar = calls.last().unwrap().1.as_deref().unwrap();
    assert!(final_grammar.contains("action ::= a-respond\n"), "{final_grammar}");
}

#[test]
fn policy_denials_are_observed() {
    let mut h = harness(
        &[
            r#"{"thought":"x","action":"read_file","args":{"path":"/etc/shadow"}}"#,
            r#"{"thought":"x","action":"respond","args":{"message":"That file is protected."}}"#,
        ],
        vec![("read_file", Behaviour::Deny("/etc/shadow is off limits (protected secrets)"))],
    );
    let mut ui = Recorder::default();
    h.agent.handle("show me the password file", &mut ui);
    let obs = last_user_message(&h.calls, 1);
    assert!(obs.starts_with("OBSERVATION (read_file: DENIED by system policy)\n/etc/shadow is off limits"), "{obs}");
}

#[test]
fn invalid_output_is_corrected() {
    let mut h = harness(
        &[
            "I think you should restart the service.",
            r#"{"thought":"x","action":"restart_service","args":{"name":"cups"}}"#,
            r#"{"thought":"ok","action":"respond","args":{"message":"Fine."}}"#,
        ],
        vec![],
    );
    let mut ui = Recorder::default();
    assert_eq!(h.agent.handle("hi", &mut ui), Outcome::Reply("Fine.".into()));
    assert!(last_user_message(&h.calls, 1).contains("no JSON object"));
    let second = last_user_message(&h.calls, 2);
    assert!(second.contains("restart_service has no argument \"name\" (arguments: service)"), "{second}");
}

#[test]
fn unavailable_model_falls_back_to_rescue_mode() {
    let mut h = harness_with(
        vec![Err(BackendError::Unavailable("connection refused".into()))],
        vec![],
        vec![],
        false,
        AgentConfig::default(),
    );
    let mut ui = Recorder::default();
    let outcome = h.agent.handle("volume 40", &mut ui);
    assert!(matches!(outcome, Outcome::Reply(ref m) if m.starts_with("Done")), "{outcome:?}");
    assert!(ui.events.iter().any(|e| e.contains("rescue mode")));
    assert!(h.guardian.0.lock().unwrap()[0].contains(r#""percent":40"#));
}

#[test]
fn disabled_actions_are_absent_from_grammar_and_prompt() {
    let h = harness_with(vec![], vec![], vec!["reboot", "poweroff"], false, AgentConfig::default());
    assert!(!h.agent.grammar().contains("a-reboot"));
    assert!(!h.agent.system_prompt().contains("reboot("));
    assert!(h.agent.grammar().contains("a-restart-service"));
}

#[test]
fn unreachable_guardian_is_explained() {
    let mut h = harness_with(
        vec![
            Ok(r#"{"thought":"x","action":"disk_usage","args":{}}"#.into()),
            Ok(r#"{"thought":"x","action":"respond","args":{"message":"The Guardian is down."}}"#.into()),
        ],
        vec![],
        vec![],
        true,
        AgentConfig::default(),
    );
    let mut ui = Recorder::default();
    h.agent.handle("disk space?", &mut ui);
    assert!(last_user_message(&h.calls, 1).contains("Guardian service is unreachable"));
}

#[test]
fn telemetry_and_conversation_memory() {
    let mut h = harness(
        &[
            r#"{"thought":"x","action":"get_telemetry","args":{"section":"host"}}"#,
            r#"{"thought":"x","action":"respond","args":{"message":"You are on core-test."}}"#,
            r#"{"thought":"x","action":"respond","args":{"message":"Still core-test."}}"#,
        ],
        vec![],
    );
    let mut ui = Recorder::default();
    h.agent.handle("what machine is this", &mut ui);
    assert!(last_user_message(&h.calls, 1).contains("\"hostname\":\"core-test\""));
    h.agent.handle("and again?", &mut ui);
    let calls = h.calls.0.lock().unwrap();
    let third = &calls[2].0;
    assert_eq!(third[1].content, "REQUEST: what machine is this");
    assert!(third[2].content.contains("You are on core-test."));
}

#[test]
fn launching_programs_runs_allowlisted_binaries_only() {
    let mut config = AgentConfig::default();
    config.programs.allowed.insert("truth".into(), "/bin/true".into());
    let mut h = harness_with(
        vec![
            Ok(r#"{"thought":"x","action":"launch_program","args":{"program":"bash"}}"#.into()),
            Ok(r#"{"thought":"x","action":"launch_program","args":{"program":"truth"}}"#.into()),
            Ok(r#"{"thought":"x","action":"respond","args":{"message":"ok"}}"#.into()),
        ],
        vec![],
        vec![],
        false,
        config,
    );
    let mut ui = Recorder::default();
    h.agent.handle("open something", &mut ui);
    assert!(last_user_message(&h.calls, 1).contains("bash is not installed or not allowed"));
    assert!(ui.events.iter().any(|e| e == "launched /bin/true"));
    assert!(h.guardian.0.lock().unwrap().is_empty(), "launching never involves the Guardian");
}
