//! Trajectories: recorded episodes used to train and evaluate the model.
//!
//! One trajectory is one episode on a disposable machine: the task and its fixture,
//! the user's request, every step (the model's intent, what happened to it, the
//! observation the agent fed back) and the checks on the resulting system state.
//! Trajectories are stored one per line (JSON lines).
//!
//! Before a trajectory is stored it is [`sanitize`]d: secret arguments are redacted
//! (and their values scrubbed from all text), and identifying data in requests and
//! observations (MAC addresses, non-loopback IP addresses, user names in home
//! directories) is replaced by placeholders. [`validate`] then checks a record
//! against the dataset's pinned contract (`docs/TRAINING.md`): a record must be
//! sanitized, every intent must be a valid catalog action, and its contract
//! fingerprint must be the one the dataset pins.

use serde::{Deserialize, Serialize};

use crate::action::ValidatedAction;
use crate::catalog;
use crate::intent::Intent;

/// Version of this record format.
pub const SCHEMA_VERSION: u32 = 1;

/// What secret arguments are replaced with (also what the Guardian's audit log uses).
pub const REDACTED: &str = "<redacted>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Train,
    Dev,
    Heldout,
}

/// What produced the intents in an episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A task's reference solution.
    Reference,
    /// The deterministic rescue planner.
    Rescue,
    /// A teacher model (which one is recorded in the dataset manifest).
    Teacher,
    /// A student or candidate model under evaluation.
    Model,
    /// A person.
    Human,
}

/// What happened to a step's intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Handled by the agent itself (conversation, telemetry, user files).
    Agent,
    /// Executed by the Guardian without confirmation.
    Allowed,
    /// Confirmed by the human, then executed.
    Confirmed,
    /// The human declined it.
    Declined,
    /// Refused by the Guardian's policy.
    Denied,
    /// Rejected before reaching policy (invalid intent).
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub intent: Intent,
    pub disposition: Disposition,
    /// The observation text the agent fed back to the model.
    pub observation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub name: String,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Checks on the resulting system state, run after the episode.
    pub checks: Vec<Check>,
    /// The episode ran in a real (disposable) VM, so the checks saw a real system.
    /// Plans that were only previewed or dry-run prove nothing about the result.
    pub ran_in_vm: bool,
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        self.ran_in_vm && !self.checks.is_empty() && self.checks.iter().all(|c| c.passed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trajectory {
    pub schema: u32,
    /// The action contract's fingerprint (`contract::fingerprint`).
    pub contract: String,
    /// Unique per episode.
    pub episode: String,
    /// The task (fixture plus request) this episode ran.
    pub task: String,
    pub split: Split,
    pub source: Source,
    pub request: String,
    pub steps: Vec<Step>,
    pub outcome: Outcome,
}

/// Redact secrets and identifying data in place. Idempotent.
pub fn sanitize(t: &mut Trajectory) {
    // Collect secret values first, so they are scrubbed wherever they were echoed.
    let mut secrets: Vec<String> = Vec::new();
    for step in &mut t.steps {
        let Some(spec) = catalog::find(&step.intent.action) else { continue };
        for p in spec.params.iter().filter(|p| p.kind.is_secret()) {
            if let Some(v) = step.intent.args.get_mut(p.name) {
                if let Some(s) = v.as_str() {
                    if s != REDACTED && !s.is_empty() {
                        secrets.push(s.to_string());
                    }
                }
                *v = serde_json::Value::String(REDACTED.into());
            }
        }
    }
    let clean = |text: &str| -> String {
        let mut out = text.to_string();
        for s in &secrets {
            out = out.replace(s.as_str(), REDACTED);
        }
        scrub_identifiers(&out)
    };
    t.request = clean(&t.request);
    for step in &mut t.steps {
        step.observation = clean(&step.observation);
        if let Some(thought) = &step.intent.thought {
            step.intent.thought = Some(clean(thought));
        }
    }
}

/// Everything wrong with a record for a dataset pinned to `contract`. Empty means
/// the record is acceptable.
pub fn validate(t: &Trajectory, contract: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if t.schema != SCHEMA_VERSION {
        problems.push(format!("schema {} is not {SCHEMA_VERSION}", t.schema));
    }
    if !crate::contract::is_fingerprint(&t.contract) {
        problems.push(format!("contract {:?} is not a fingerprint", t.contract));
    } else if t.contract != contract {
        problems.push(format!("made with contract {}, the dataset pins {contract}", t.contract));
    }
    if t.episode.trim().is_empty() || t.task.trim().is_empty() {
        problems.push("episode and task must be named".into());
    }
    if t.request.trim().is_empty() {
        problems.push("the request is empty".into());
    }
    if t.steps.is_empty() {
        problems.push("no steps".into());
    }
    for (i, step) in t.steps.iter().enumerate() {
        if step.disposition == Disposition::Invalid {
            continue; // recorded on purpose: the model emitted something invalid
        }
        if let Err(e) = ValidatedAction::from_intent(&step.intent) {
            problems.push(format!("step {}: {e}", i + 1));
        }
    }
    let mut sanitized = t.clone();
    sanitize(&mut sanitized);
    if sanitized != *t {
        problems.push("not sanitized (secrets or identifying data remain)".into());
    }
    if t.outcome.checks.is_empty() {
        problems.push("no state checks".into());
    }
    problems
}

/// Replace MAC addresses, non-loopback IP addresses and user names in home
/// directories with placeholders.
pub fn scrub_identifiers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        out.push_str(&scrub_token(token));
        token.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '/' | '_' | '-' | '%') {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

fn scrub_token(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    // A token may carry a prefix length or port: 192.168.1.5/24, fe80::1%eth0.
    let (core, rest) = match token.find(['/', '%']) {
        Some(i) if !token.starts_with('/') => token.split_at(i),
        _ => (token, ""),
    };
    let core_trimmed = core.trim_end_matches(['.', ':']);
    let trailing = &core[core_trimmed.len()..];
    if is_mac(core_trimmed) {
        return format!("<mac>{trailing}{rest}");
    }
    if let Some(ip) = parse_ipv4(core_trimmed) {
        if ip[0] != 127 && ip != [0, 0, 0, 0] {
            return format!("<ip>{trailing}{rest}");
        }
    }
    if is_ipv6(core_trimmed) && core_trimmed != "::1" && core_trimmed != "::" {
        return format!("<ip>{trailing}{rest}");
    }
    scrub_home(token)
}

fn scrub_home(token: &str) -> String {
    let Some(i) = token.find("/home/") else { return token.to_string() };
    let after = &token[i + 6..];
    let end = after.find('/').unwrap_or(after.len());
    if end == 0 || &after[..end] == "<user>" {
        return token.to_string();
    }
    format!("{}/home/<user>{}", &token[..i], &after[end..])
}

fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut ip = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        ip[i] = p.parse().ok()?;
    }
    Some(ip)
}

/// IPv6 addresses, without confusing them with times (12:34:56) or MACs.
fn is_ipv6(s: &str) -> bool {
    let colons = s.matches(':').count();
    if colons < 2 || !s.bytes().all(|b| b.is_ascii_hexdigit() || b == b':') {
        return false;
    }
    if s.contains(":::") || s.split(':').any(|g| g.len() > 4) {
        return false;
    }
    s.contains("::") || colons == 7
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record() -> Trajectory {
        Trajectory {
            schema: SCHEMA_VERSION,
            contract: crate::contract::fingerprint().to_string(),
            episode: "ep-1".into(),
            task: "pkg-missing-nano".into(),
            split: Split::Train,
            source: Source::Reference,
            request: "install nano for me".into(),
            steps: vec![
                Step {
                    intent: Intent::new("install_package", json!({"package": "nano"})),
                    disposition: Disposition::Confirmed,
                    observation: "OBSERVATION install_package succeeded".into(),
                },
                Step {
                    intent: Intent::new("respond", json!({"message": "nano is installed"})),
                    disposition: Disposition::Agent,
                    observation: String::new(),
                },
            ],
            outcome: Outcome { checks: vec![Check { name: "nano runs".into(), passed: true }], ran_in_vm: true },
        }
    }

    #[test]
    fn a_clean_record_is_valid_and_round_trips() {
        let t = record();
        assert_eq!(validate(&t, crate::contract::fingerprint()), Vec::<String>::new());
        let line = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Trajectory>(&line).unwrap(), t);
        assert!(t.outcome.succeeded());
    }

    #[test]
    fn a_record_for_another_contract_is_rejected() {
        let mut t = record();
        t.contract = format!("sha256:{}", "0".repeat(64));
        let problems = validate(&t, crate::contract::fingerprint());
        assert!(problems.iter().any(|p| p.contains("the dataset pins")), "{problems:?}");
    }

    #[test]
    fn secrets_are_redacted_everywhere() {
        let mut t = record();
        t.steps[0] = Step {
            intent: Intent::new("wifi_connect", json!({"ssid": "Home", "passphrase": "hunter2222"}))
                .with_thought("connect with hunter2222"),
            disposition: Disposition::Confirmed,
            observation: "Error: wrong passphrase hunter2222".into(),
        };
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("not sanitized")));
        sanitize(&mut t);
        let line = serde_json::to_string(&t).unwrap();
        assert!(!line.contains("hunter2222"), "{line}");
        assert_eq!(t.steps[0].intent.args["passphrase"], REDACTED);
        assert_eq!(
            validate(&t, crate::contract::fingerprint()),
            Vec::<String>::new(),
            "redacted secrets still validate"
        );
    }

    #[test]
    fn identifying_data_is_scrubbed() {
        let text = "eth0 UP 52:54:00:12:34:56 10.0.2.15/24 fe80::5054:ff:fe12:3456/64 gw 192.168.1.1 dns 8.8.8.8.\n\
                    lo 127.0.0.1 ::1 at 12:34:56 on 2026-10-03, file /home/joel/notes.txt, v1.2.3 size 1.5";
        let s = scrub_identifiers(text);
        assert_eq!(
            s,
            "eth0 UP <mac> <ip>/24 <ip>/64 gw <ip> dns <ip>.\n\
             lo 127.0.0.1 ::1 at 12:34:56 on 2026-10-03, file /home/<user>/notes.txt, v1.2.3 size 1.5"
        );
        assert_eq!(scrub_identifiers(&s), s, "idempotent");
    }

    #[test]
    fn invalid_or_incomplete_records_are_reported() {
        let mut t = record();
        t.steps[0].intent = Intent::new("install_package", json!({"package": "-rf"}));
        t.outcome.checks.clear();
        let problems = validate(&t, crate::contract::fingerprint());
        assert!(problems.iter().any(|p| p.starts_with("step 1")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("no state checks")), "{problems:?}");
        // An invalid intent recorded as such is kept: models do emit them.
        let mut t = record();
        t.steps[0].intent = Intent::new("rm_rf", json!({}));
        t.steps[0].disposition = Disposition::Invalid;
        assert_eq!(validate(&t, crate::contract::fingerprint()), Vec::<String>::new());
    }

    #[test]
    fn success_needs_a_real_vm_and_passing_checks() {
        let mut t = record();
        t.outcome.ran_in_vm = false;
        assert!(!t.outcome.succeeded(), "a dry run or preview proves nothing about the result");
        let mut t = record();
        t.outcome.checks[0].passed = false;
        assert!(!t.outcome.succeeded());
    }
}
