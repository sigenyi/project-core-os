//! The Guardian's request handling, independent of transport.
//!
//! [`Guardian::handle`] is the whole decision pipeline:
//! rate limit → validate → route → authorise → (confirm) → plan → execute → audit.
//! The socket server and the agent's in-process development mode both drive it.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use core_protocol::wire::{ExecutionReport, RejectKind, Request, Response, StepReport};
use core_protocol::{Executor as ActionExecutor, Intent, PROTOCOL_VERSION, Risk, ValidatedAction};
use serde_json::{Value, json};

use crate::audit::AuditLog;
use crate::config::GuardianConfig;
use crate::confirm::Confirmations;
use crate::executor::Executor;
use crate::planner::{Planner, SystemProbe};
use crate::policy::{Decision, Policy};
use crate::runner::CommandRunner;

/// Identity of a connected client, from the kernel (SO_PEERCRED), never from the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    pub uid: u32,
    pub gid: u32,
    pub pid: i32,
}

/// Per-connection state.
pub struct Session {
    pub peer: Peer,
    confirmations: Confirmations,
}

pub struct Guardian {
    config: GuardianConfig,
    runner: Box<dyn CommandRunner>,
    probe: Box<dyn SystemProbe>,
    audit: AuditLog,
    /// System changes are serialised: one at a time. Read-only actions do not wait.
    exec_lock: Mutex<()>,
    /// Recent request times per uid (shared by all of a user's connections).
    recent: Mutex<HashMap<u32, VecDeque<Instant>>>,
}

const MAX_PENDING_CONFIRMATIONS: usize = 4;

impl Guardian {
    pub fn new(
        config: GuardianConfig,
        runner: Box<dyn CommandRunner>,
        probe: Box<dyn SystemProbe>,
        audit: AuditLog,
    ) -> Self {
        Guardian { config, runner, probe, audit, exec_lock: Mutex::new(()), recent: Mutex::new(HashMap::new()) }
    }

    pub fn config(&self) -> &GuardianConfig {
        &self.config
    }

    pub fn new_session(&self, peer: Peer) -> Session {
        Session {
            peer,
            confirmations: Confirmations::new(
                Duration::from_secs(self.config.confirmation_timeout_secs),
                MAX_PENDING_CONFIRMATIONS,
            ),
        }
    }

    pub fn handle(&self, session: &mut Session, request: Request) -> Response {
        match request {
            Request::Hello { .. } => Response::Hello {
                server: format!("core-guardian {}", env!("CARGO_PKG_VERSION")),
                protocol: PROTOCOL_VERSION,
                dry_run: self.config.dry_run,
            },
            Request::Ping => Response::Pong,
            Request::Capabilities => Response::Capabilities { actions: Policy::new(&self.config).capabilities() },
            Request::Execute { id, intent } => self.execute(session, id, intent),
            Request::Confirm { token, approve } => self.confirm(session, &token, approve),
        }
    }

    /// Sliding one-minute window per uid, so reconnecting does not reset the limit.
    fn rate_limited(&self, uid: u32) -> bool {
        let window = Duration::from_secs(60);
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let times = recent.entry(uid).or_default();
        while times.front().is_some_and(|t| t.elapsed() > window) {
            times.pop_front();
        }
        if times.len() >= self.config.max_requests_per_minute as usize {
            return true;
        }
        times.push_back(Instant::now());
        false
    }

    fn execute(&self, session: &mut Session, id: u64, intent: Intent) -> Response {
        let peer = session.peer;
        if self.rate_limited(peer.uid) {
            self.audit_event(peer, id, json!({"action": clip(&intent.action), "decision": "rate_limited"}));
            return reject(id, RejectKind::RateLimited, "too many requests; slow down");
        }
        let v = match ValidatedAction::from_intent(&intent) {
            Ok(v) => v,
            Err(e) => {
                self.audit_event(
                    peer,
                    id,
                    json!({"action": clip(&intent.action), "decision": "invalid", "reason": e.to_string()}),
                );
                return reject(id, RejectKind::Invalid, e.to_string());
            }
        };
        if v.spec.executor != ActionExecutor::Guardian {
            return reject(id, RejectKind::NotPrivileged, format!("{} is handled by the agent", v.name()));
        }
        match Policy::with_probe(&self.config, self.probe.as_ref()).authorize(&v) {
            Decision::Deny { reason } => {
                self.audit_action(peer, id, &v, "denied", Some(&reason), None);
                reject(id, RejectKind::Denied, reason)
            }
            Decision::Confirm { risk } => {
                let summary = v.describe();
                self.audit_action(peer, id, &v, "confirmation_required", None, None);
                let token = session.confirmations.insert(id, v);
                Response::ConfirmationRequired {
                    id,
                    token,
                    summary,
                    risk,
                    expires_in_secs: self.config.confirmation_timeout_secs,
                }
            }
            Decision::Allow => self.run(peer, id, &v, "allowed"),
        }
    }

    fn confirm(&self, session: &mut Session, token: &str, approve: bool) -> Response {
        let peer = session.peer;
        let Some(pending) = session.confirmations.take(token) else {
            self.audit_event(peer, 0, json!({"decision": "confirmation_expired"}));
            return reject(0, RejectKind::Expired, "that confirmation expired or does not exist; ask again");
        };
        if !approve {
            self.audit_action(peer, pending.request_id, &pending.action, "declined", None, None);
            return reject(pending.request_id, RejectKind::Declined, "the user declined this action");
        }
        self.run(peer, pending.request_id, &pending.action, "confirmed")
    }

    fn run(&self, peer: Peer, id: u64, v: &ValidatedAction, decision: &str) -> Response {
        // Changes run one at a time; reads (bounded by command timeouts) never queue
        // behind a long package installation.
        let _serialised = (v.risk() > Risk::Observe).then(|| self.exec_lock.lock().unwrap_or_else(|e| e.into_inner()));
        let report = match Planner::new(&self.config, self.probe.as_ref()).plan(&v.action) {
            Ok(plan) => Executor {
                config: &self.config,
                runner: self.runner.as_ref(),
                peer: Some((peer.uid, peer.gid)),
                // Dry-run never changes the system, but read-only actions stay real.
                simulate: self.config.dry_run && v.risk() > Risk::Observe,
            }
            .execute(v.name(), &plan),
            Err(reason) => ExecutionReport {
                action: v.name().into(),
                success: false,
                steps: vec![StepReport {
                    description: "prepare".into(),
                    success: false,
                    stderr: reason,
                    ..Default::default()
                }],
                ..Default::default()
            },
        };
        self.audit_action(peer, id, v, decision, None, Some(&report));
        Response::Executed { id, report }
    }

    fn audit_action(
        &self,
        peer: Peer,
        id: u64,
        v: &ValidatedAction,
        decision: &str,
        reason: Option<&str>,
        report: Option<&ExecutionReport>,
    ) {
        let mut entry = json!({
            "action": v.name(),
            "args": v.redacted_args(),
            "risk": v.risk(),
            "decision": decision,
        });
        if let Some(r) = reason {
            entry["reason"] = Value::from(r);
        }
        if let Some(r) = report {
            entry["success"] = Value::from(r.success);
            entry["dry_run"] = Value::from(r.dry_run);
            entry["duration_ms"] = Value::from(r.duration_ms);
            entry["steps"] =
                r.steps.iter().map(|s| json!({"step": s.description, "ok": s.success, "exit": s.exit_code})).collect();
        }
        self.audit_event(peer, id, entry);
    }

    fn audit_event(&self, peer: Peer, id: u64, mut entry: Value) {
        entry["peer"] = json!({"uid": peer.uid, "pid": peer.pid});
        entry["request"] = Value::from(id);
        // Which action contract this entry was made under (trajectories built
        // from the log are pinned to it; docs/TRAINING.md).
        entry["contract"] = Value::from(core_protocol::contract::fingerprint());
        log::info!("{entry}");
        self.audit.record(entry);
    }
}

fn reject(id: u64, kind: RejectKind, reason: impl Into<String>) -> Response {
    Response::Rejected { id, kind, reason: reason.into() }
}

fn clip(s: &str) -> String {
    s.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::runner::{CommandOutput, ScriptedRunner};

    struct NoProbe;
    impl SystemProbe for NoProbe {
        fn wireless_interfaces(&self) -> Vec<String> {
            vec!["wlan0".into()]
        }
        fn filesystem_type(&self, _: &Path) -> Option<String> {
            Some("ext4".into())
        }
    }

    fn guardian(config: GuardianConfig, runner: ScriptedRunner) -> Guardian {
        Guardian::new(config, Box::new(runner), Box::new(NoProbe), AuditLog::disabled())
    }

    fn config() -> GuardianConfig {
        let mut c = GuardianConfig::default();
        for p in c.tools.values_mut() {
            *p = "/bin/true".into();
        }
        c
    }

    fn peer() -> Peer {
        Peer { uid: 1000, gid: 1000, pid: 42 }
    }

    fn exec(g: &Guardian, s: &mut Session, action: &str, args: serde_json::Value) -> Response {
        g.handle(s, Request::Execute { id: 1, intent: Intent::new(action, args) })
    }

    #[test]
    fn audit_entries_name_the_action_contract() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let g = Guardian::new(
            config(),
            Box::new(ScriptedRunner::default()),
            Box::new(NoProbe),
            AuditLog::open(&path).unwrap(),
        );
        let mut s = g.new_session(peer());
        exec(&g, &mut s, "restart_service", json!({"service": "bluetooth"}));
        exec(&g, &mut s, "install_package", json!({"package": "w3m"}));
        let text = std::fs::read_to_string(&path).unwrap();
        let entries: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(entries.len(), 2);
        for e in &entries {
            assert_eq!(e["contract"], core_protocol::contract::fingerprint(), "{e}");
        }
        assert_eq!(entries[1]["decision"], "confirmation_required");
    }

    #[test]
    fn low_risk_runs_immediately() {
        let g = guardian(config(), ScriptedRunner::default());
        let mut s = g.new_session(peer());
        let Response::Executed { report, .. } = exec(&g, &mut s, "restart_service", json!({"service": "bluetooth"}))
        else {
            panic!()
        };
        assert!(report.success);
        assert_eq!(report.steps[0].description, "systemctl restart -- bluetooth");
    }

    #[test]
    fn high_risk_needs_confirmation_from_the_same_session() {
        let g = guardian(config(), ScriptedRunner::default());
        let mut s = g.new_session(peer());
        let Response::ConfirmationRequired { token, summary, risk, .. } =
            exec(&g, &mut s, "install_package", json!({"package": "w3m"}))
        else {
            panic!()
        };
        assert_eq!(summary, "Install package w3m");
        assert_eq!(risk, Risk::High);

        // Another connection cannot redeem it.
        let mut other = g.new_session(peer());
        assert!(matches!(
            g.handle(&mut other, Request::Confirm { token: token.clone(), approve: true }),
            Response::Rejected { kind: RejectKind::Expired, .. }
        ));
        let Response::Executed { report, .. } =
            g.handle(&mut s, Request::Confirm { token: token.clone(), approve: true })
        else {
            panic!()
        };
        assert!(report.success);
        // Single use.
        assert!(matches!(
            g.handle(&mut s, Request::Confirm { token, approve: true }),
            Response::Rejected { kind: RejectKind::Expired, .. }
        ));
    }

    #[test]
    fn declining_runs_nothing() {
        let runner = ScriptedRunner::default();
        let g = guardian(config(), runner);
        let mut s = g.new_session(peer());
        let Response::ConfirmationRequired { token, .. } = exec(&g, &mut s, "reboot", json!({})) else { panic!() };
        assert!(matches!(
            g.handle(&mut s, Request::Confirm { token, approve: false }),
            Response::Rejected { kind: RejectKind::Declined, .. }
        ));
    }

    #[test]
    fn invalid_denied_and_agent_actions() {
        let g = guardian(config(), ScriptedRunner::default());
        let mut s = g.new_session(peer());
        assert!(matches!(
            exec(&g, &mut s, "format_disk", json!({})),
            Response::Rejected { kind: RejectKind::Invalid, .. }
        ));
        assert!(matches!(
            exec(&g, &mut s, "stop_service", json!({"service": "dbus"})),
            Response::Rejected { kind: RejectKind::Denied, .. }
        ));
        assert!(matches!(
            exec(&g, &mut s, "read_file", json!({"path": "/etc/fstab"})),
            Response::Rejected { kind: RejectKind::NotPrivileged, .. }
        ));
        assert!(matches!(
            exec(&g, &mut s, "respond", json!({"message": "hi"})),
            Response::Rejected { kind: RejectKind::NotPrivileged, .. }
        ));
    }

    #[test]
    fn failures_carry_stderr_for_self_correction() {
        let runner = ScriptedRunner {
            script: vec![(
                "systemctl restart".into(),
                CommandOutput {
                    exit_code: Some(5),
                    stderr: "Failed to restart foo.service: Unit foo.service not found.".into(),
                    ..Default::default()
                },
            )],
            ..Default::default()
        };
        let g = guardian(config(), runner);
        let mut s = g.new_session(peer());
        let Response::Executed { report, .. } = exec(&g, &mut s, "restart_service", json!({"service": "foo"})) else {
            panic!()
        };
        assert!(!report.success);
        assert!(report.combined_output().contains("Unit foo.service not found"));
    }

    #[test]
    fn dry_run_simulates_changes_but_reads_for_real() {
        let mut c = config();
        c.dry_run = true;
        c.auto_approve = Risk::High;
        let runner = ScriptedRunner {
            script: vec![(
                "df".into(),
                CommandOutput { exit_code: Some(0), stdout: "Filesystem Size\n".into(), ..Default::default() },
            )],
            ..Default::default()
        };
        let g = guardian(c, runner);
        let mut s = g.new_session(peer());
        let Response::Executed { report, .. } = exec(&g, &mut s, "install_package", json!({"package": "w3m"})) else {
            panic!()
        };
        assert!(report.dry_run && report.steps[0].stdout.starts_with("[dry-run]"));
        let Response::Executed { report, .. } = exec(&g, &mut s, "disk_usage", json!({})) else { panic!() };
        assert!(!report.dry_run);
        assert_eq!(report.steps[0].stdout, "Filesystem Size\n");
    }

    #[test]
    fn rate_limiting() {
        let mut c = config();
        c.max_requests_per_minute = 2;
        let g = guardian(c, ScriptedRunner::default());
        let mut s = g.new_session(peer());
        exec(&g, &mut s, "disk_usage", json!({}));
        exec(&g, &mut s, "disk_usage", json!({}));
        assert!(matches!(
            exec(&g, &mut s, "disk_usage", json!({})),
            Response::Rejected { kind: RejectKind::RateLimited, .. }
        ));
    }

    #[test]
    fn planning_errors_become_failed_reports() {
        let mut c = config();
        c.tools.remove("lsusb");
        let g = guardian(c, ScriptedRunner::default());
        let mut s = g.new_session(peer());
        let Response::Executed { report, .. } = exec(&g, &mut s, "list_hardware", json!({"bus": "usb"})) else {
            panic!()
        };
        assert!(!report.success);
        assert!(report.steps[0].stderr.contains("lsusb"));
    }
}
