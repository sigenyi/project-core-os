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

/// Version of this record format. Version 2 added the outcome's verdict, reasons and
/// failed steps; version 1 records are rejected.
pub const SCHEMA_VERSION: u32 = 2;

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
    /// Required commands that failed while carrying out a step. Checks that pass
    /// afterwards do not make such an episode a success: it is a failed example.
    pub step_failures: Vec<StepFailure>,
    /// The verdict of whoever ran the episode (the evaluation runner), which also
    /// judges things the checks cannot see: the policy decision, a fixture that broke
    /// nothing, a refusal that ran anyway.
    pub verdict: Verdict,
    /// Why the verdict is `failed` (empty when it passed).
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepFailure {
    /// The step (1-based) whose command failed.
    pub step: usize,
    /// The command and how it failed.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Passed,
    Failed,
}

impl Outcome {
    /// A success only when everything agrees: a passed verdict, no failed step, in
    /// a real VM, with state checks that all passed.
    pub fn succeeded(&self) -> bool {
        self.verdict == Verdict::Passed
            && self.step_failures.is_empty()
            && self.ran_in_vm
            && !self.checks.is_empty()
            && self.checks.iter().all(|c| c.passed)
    }

    /// Ways the outcome contradicts itself or the steps it describes.
    fn contradictions(&self, steps: &[Step]) -> Vec<String> {
        let mut out = Vec::new();
        let executed = |d: Disposition| matches!(d, Disposition::Allowed | Disposition::Confirmed);
        if self.verdict == Verdict::Passed {
            if !self.step_failures.is_empty() {
                out.push("verdict passed, but a required step failed".into());
            }
            if self.checks.iter().any(|c| !c.passed) {
                out.push("verdict passed, but a state check failed".into());
            }
            if !self.ran_in_vm {
                out.push("verdict passed, but the episode did not run in a VM".into());
            }
            if !self.reasons.is_empty() {
                out.push("verdict passed, but it gives failure reasons".into());
            }
            if !steps.is_empty() && steps.iter().all(|s| s.disposition == Disposition::Invalid) {
                out.push("verdict passed, but no step was a valid action".into());
            }
        } else if self.reasons.is_empty() || self.reasons.iter().any(|r| r.trim().is_empty()) {
            out.push("verdict failed without a reason".into());
        }
        for f in &self.step_failures {
            match f.step.checked_sub(1).and_then(|i| steps.get(i)) {
                None => out.push(format!("step failure names step {}, the episode has {}", f.step, steps.len())),
                Some(step) if !executed(step.disposition) => out.push(format!(
                    "step failure names step {}, which was {:?} and never ran",
                    f.step, step.disposition
                )),
                Some(_) => {}
            }
        }
        // An executed step whose observation reports a failure must be recorded as one.
        for (i, step) in steps.iter().enumerate() {
            let reports_failure = step.observation.lines().next().is_some_and(|l| l.contains(": FAILED"));
            if executed(step.disposition) && reports_failure && !self.step_failures.iter().any(|f| f.step == i + 1) {
                out.push(format!("step {} reports a failure that the outcome does not record", i + 1));
            }
        }
        out
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
///
/// Run it on the raw episode, while secret arguments still hold their values: a
/// value that was redacted earlier (in an audit log, say) cannot be found where the
/// user or a program echoed it, and no validator can tell it is there.
pub fn sanitize(t: &mut Trajectory) {
    // Collect secret values first, so they are scrubbed wherever they were echoed.
    let mut secrets: Vec<String> = Vec::new();
    for step in &mut t.steps {
        let spec = catalog::find(&step.intent.action);
        for (name, v) in step.intent.args.iter_mut() {
            let secret = match spec {
                Some(spec) => spec.param(name).is_some_and(|p| p.kind.is_secret()),
                // Not a catalog action (recorded as invalid): judge by the name.
                None => looks_secret(name),
            };
            if !secret {
                continue;
            }
            // The argument is always redacted. Its value is scrubbed from text only
            // when it is long enough to be a real secret (catalog secrets have at
            // least 8 characters): a model's "y" must not be cut out of every word.
            let value = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            if value != REDACTED && value.chars().count() >= MIN_ECHOED_SECRET {
                secrets.push(value);
            }
            *v = serde_json::Value::String(REDACTED.into());
        }
    }
    // Longest first, so a secret that contains another is replaced whole.
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let unsecret = |text: &str| -> String {
        let mut out = text.to_string();
        for s in &secrets {
            out = out.replace(s.as_str(), REDACTED);
        }
        out
    };
    let clean = |text: &str| scrub_identifiers(&unsecret(text));
    t.request = clean(&t.request);
    for r in &mut t.outcome.reasons {
        *r = clean(r);
    }
    for f in &mut t.outcome.step_failures {
        f.detail = clean(&f.detail);
    }
    for c in &mut t.outcome.checks {
        c.name = clean(&c.name);
    }
    for step in &mut t.steps {
        step.observation = clean(&step.observation);
        if let Some(thought) = &step.intent.thought {
            step.intent.thought = Some(clean(thought));
        }
        // Arguments are scrubbed too. Free text gets placeholders; typed values
        // (a host to ping, a path) get documentation values of the same type, so
        // the intent still validates.
        let spec = catalog::find(&step.intent.action);
        for (name, v) in step.intent.args.iter_mut() {
            let free_text = match spec.and_then(|spec| spec.param(name)) {
                Some(p) => matches!(p.kind, catalog::ParamKind::Text { .. }),
                None => true,
            };
            let scrub = |text: &str| {
                if free_text { clean(text) } else { scrub_with(&unsecret(text), Style::DocumentationValues) }
            };
            scrub_value(v, &scrub);
        }
    }
}

/// Echoed values shorter than this are not scrubbed from text (see [`sanitize`]).
const MIN_ECHOED_SECRET: usize = 8;

/// Every string in an argument value, also inside lists (`launch_program`'s args).
fn scrub_value(v: &mut serde_json::Value, scrub: &dyn Fn(&str) -> String) {
    match v {
        serde_json::Value::String(text) if text != REDACTED => *text = scrub(text),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|i| scrub_value(i, scrub)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|i| scrub_value(i, scrub)),
        _ => {}
    }
}

/// Whether an argument of a non-catalog action holds a secret, judged by the name:
/// long secret words anywhere (`wifiPassword`, `api_token`), short ones only as a
/// whole word (`key`, `api-key`, `apiKey`; not `keyboard` or `monkey`).
fn looks_secret(name: &str) -> bool {
    const ANYWHERE: &[&str] = &["password", "passphrase", "passwd", "secret", "token", "credential"];
    const WHOLE_WORD: &[&str] = &["pass", "psk", "key", "apikey", "pin"];
    let lower = name.to_ascii_lowercase();
    if ANYWHERE.iter().any(|w| lower.contains(w)) {
        return true;
    }
    // Words split at punctuation and at camelCase humps.
    let mut words = vec![String::new()];
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            words.push(String::new());
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower {
            words.push(String::new());
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        words.last_mut().unwrap().push(c.to_ascii_lowercase());
    }
    words.iter().any(|w| WHOLE_WORD.contains(&w.as_str()))
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
    problems.extend(t.outcome.contradictions(&t.steps));
    problems
}

/// Replace MAC addresses, non-loopback IP addresses and user names in home
/// directories with placeholders (`<mac>`, `<ip>`, `/home/<user>`).
///
/// A four-part version number (6.10.5.1) cannot be told from an IPv4 address and
/// is replaced too: scrubbing errs on the side of removing data.
pub fn scrub_identifiers(text: &str) -> String {
    scrub_with(text, Style::Placeholders)
}

/// What identifying values are replaced with.
#[derive(Clone, Copy, PartialEq)]
enum Style {
    /// `<mac>`, `<ip>`, `<user>`: for text.
    Placeholders,
    /// Reserved documentation values that still parse (02:00:00:00:00:01,
    /// 192.0.2.1, 2001:db8::1, user): for typed arguments.
    DocumentationValues,
}

const DOC_MAC: &str = "02:00:00:00:00:01";
const DOC_IPV4: &str = "192.0.2.1";
const DOC_IPV6: &str = "2001:db8::1";

fn scrub_with(text: &str, style: Style) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '/' | '_' | '-' | '%') {
            token.push(c);
        } else {
            out.push_str(&scrub_token(&token, style));
            token.clear();
            out.push(c);
        }
    }
    out.push_str(&scrub_token(&token, style));
    out
}

/// A token is scrubbed segment by segment between slashes, so addresses inside URLs
/// (http://192.168.1.7/x) and with prefix lengths (10.0.2.15/24) are found.
fn scrub_token(token: &str, style: Style) -> String {
    if token.is_empty() {
        return String::new();
    }
    let joined: Vec<String> = token.split('/').map(|seg| scrub_segment(seg, style)).collect();
    scrub_home(&joined.join("/"), style)
}

fn scrub_segment(seg: &str, style: Style) -> String {
    // A zone index follows a link-local address: fe80::1%eth0.
    let (core, zone) = match seg.find('%') {
        Some(i) => seg.split_at(i),
        None => (seg, ""),
    };
    // Sentence punctuation after an address is not part of it. A trailing colon is
    // trimmed only when the text is not an address already: in 2001:4860:: the
    // colons are the address (zero compression), in "10.0.0.1:" they are not.
    // Colons come off one at a time, so "2001:4860:::" (an address and a colon)
    // still finds 2001:4860::.
    let mut trimmed = core.trim_end_matches('.');
    while !is_ipv6(trimmed) && trimmed.ends_with(':') {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    if !is_ipv6(trimmed) {
        trimmed = trimmed.trim_end_matches([':', '.']);
    }
    let trailing = &core[trimmed.len()..];
    let (mac, v4, v6) = match style {
        Style::Placeholders => ("<mac>", "<ip>", "<ip>"),
        Style::DocumentationValues => (DOC_MAC, DOC_IPV4, DOC_IPV6),
    };
    let replaced = if is_mac(trimmed) && trimmed != DOC_MAC {
        Some(mac.to_string())
    } else if parse_ipv4(trimmed).is_some_and(identifying_ipv4) {
        Some(v4.to_string())
    } else if let Some((addr, port)) = trimmed.rsplit_once(':').filter(|(a, p)| {
        // An IPv4 address with a port: 192.168.1.5:22.
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && parse_ipv4(a).is_some()
    }) {
        parse_ipv4(addr).is_some_and(identifying_ipv4).then(|| format!("{v4}:{port}"))
    } else if is_ipv6(trimmed) && identifying_ipv6(trimmed) {
        Some(v6.to_string())
    } else {
        None
    };
    match replaced {
        // A documentation address has no interface; its zone would not parse.
        Some(r) if style == Style::DocumentationValues => format!("{r}{trailing}"),
        Some(r) => format!("{r}{trailing}{zone}"),
        None => seg.to_string(),
    }
}

fn identifying_ipv4(ip: [u8; 4]) -> bool {
    ip[0] != 127 && ip != [0, 0, 0, 0] && !(ip[0] == 192 && ip[1] == 0 && ip[2] == 2)
}

/// Loopback, unspecified, IPv4-mapped loopback and documentation (2001:db8::/32)
/// addresses identify nobody, however they are spelled.
fn identifying_ipv6(s: &str) -> bool {
    let Ok(a) = s.parse::<std::net::Ipv6Addr>() else { return true };
    let documentation = a.segments()[0] == 0x2001 && a.segments()[1] == 0x0db8;
    let mapped_loopback = a.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback() || v4.is_unspecified());
    !(a.is_loopback() || a.is_unspecified() || documentation || mapped_loopback)
}

fn scrub_home(token: &str, style: Style) -> String {
    let Some(i) = token.find("/home/") else { return token.to_string() };
    let after = &token[i + 6..];
    let end = after.find('/').unwrap_or(after.len());
    let user = match style {
        Style::Placeholders => "<user>",
        Style::DocumentationValues => "user",
    };
    if end == 0 || &after[..end] == user {
        return token.to_string();
    }
    format!("{}/home/{user}{}", &token[..i], &after[end..])
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

/// IPv6 addresses, including IPv4-mapped ones (::ffff:10.1.2.3), without confusing
/// them with times (12:34:56) or MACs.
fn is_ipv6(s: &str) -> bool {
    if let Some((head, v4)) = s.rsplit_once(':').filter(|(_, v4)| v4.contains('.')) {
        return parse_ipv4(v4).is_some() && is_ipv6(&format!("{head}:0"));
    }
    let colons = s.matches(':').count();
    if colons < 2 || !s.bytes().all(|b| b.is_ascii_hexdigit() || b == b':') {
        return false;
    }
    if s.contains(":::") || s.matches("::").count() > 1 || s.split(':').any(|g| g.len() > 4) {
        return false;
    }
    // A colon at either end is only valid as part of "::" (2001:4860::, ::1).
    if (s.ends_with(':') && !s.ends_with("::")) || (s.starts_with(':') && !s.starts_with("::")) {
        return false;
    }
    if s.contains("::") {
        // The compressed zeros stand for at least one group.
        s.split(':').filter(|g| !g.is_empty()).count() <= 7
    } else {
        colons == 7
    }
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
            outcome: Outcome {
                checks: vec![Check { name: "nano runs".into(), passed: true }],
                ran_in_vm: true,
                step_failures: vec![],
                verdict: Verdict::Passed,
                reasons: vec![],
            },
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
    fn addresses_in_urls_with_ports_and_mapped_forms_are_scrubbed() {
        let text = "GET http://192.168.1.7/index.html from 192.168.1.5:22 dns 10.0.0.1:53 \
                    mapped ::ffff:10.1.2.3 local 127.0.0.1:631 doc 192.0.2.1";
        let s = scrub_identifiers(text);
        assert_eq!(
            s,
            "GET http://<ip>/index.html from <ip>:22 dns <ip>:53 mapped <ip> local 127.0.0.1:631 doc 192.0.2.1"
        );
        assert_eq!(scrub_identifiers(&s), s, "idempotent");
        // Known and accepted: a four-part version looks like an address.
        assert_eq!(scrub_identifiers("linux 6.10.5.1"), "linux <ip>");
        assert_eq!(scrub_identifiers("bc 1.07.1, bash 5.3"), "bc 1.07.1, bash 5.3");
    }

    #[test]
    fn ipv6_with_trailing_zero_compression_is_scrubbed() {
        let text = "dns 2001:4860::, gw 2001:4860::. via (2001:4860::) net 2001:4860::/32 ll fe80:: \
                    lo ::1 any :: doc 2001:db8:: 2001:db8::1 label 2001:4860::8888: time 12:34:56 ok";
        let s = scrub_identifiers(text);
        assert_eq!(
            s,
            "dns <ip>, gw <ip>. via (<ip>) net <ip>/32 ll <ip> \
             lo ::1 any :: doc 2001:db8:: 2001:db8::1 label <ip>: time 12:34:56 ok"
        );
        assert_eq!(scrub_identifiers(&s), s, "idempotent");
        // Not addresses: a single dangling colon, two compressions, a MAC, a time.
        for not_ip in ["2001:4860:", "1::2::3", "a:b", "12:34:56", "Note::"] {
            assert!(!is_ipv6(not_ip), "{not_ip}");
        }
        for ip in ["2001:4860::", "::", "::1", "fe80::", "2001:db8:0:0:0:0:0:1", "::ffff:10.1.2.3"] {
            assert!(is_ipv6(ip), "{ip}");
        }
        // A typed argument becomes a documentation address that still validates.
        let mut t = record();
        t.steps[0].intent = Intent::new("ping_host", json!({"host": "2001:4860::"}));
        assert!(ValidatedAction::from_intent(&t.steps[0].intent).is_ok(), "valid before");
        sanitize(&mut t);
        assert_eq!(t.steps[0].intent.args["host"], DOC_IPV6);
        assert_eq!(validate(&t, crate::contract::fingerprint()), Vec::<String>::new());
        let once = t.clone();
        sanitize(&mut t);
        assert_eq!(t, once, "sanitize is idempotent");
        // The documentation and loopback exemptions hold for typed arguments too.
        for kept in ["2001:db8::", "::1", "127.0.0.1", "192.0.2.7"] {
            let mut t = record();
            t.steps[0].intent = Intent::new("ping_host", json!({"host": kept}));
            sanitize(&mut t);
            assert_eq!(t.steps[0].intent.args["host"], kept);
        }
    }

    #[test]
    fn arguments_are_scrubbed_and_still_validate() {
        let mut t = record();
        t.steps[0].intent = Intent::new("ping_host", json!({"host": "192.168.1.9"}));
        t.steps[1].intent = Intent::new("respond", json!({"message": "192.168.1.9 at 52:54:00:12:34:56 answers"}));
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("not sanitized")));
        sanitize(&mut t);
        assert_eq!(t.steps[0].intent.args["host"], DOC_IPV4);
        assert_eq!(t.steps[1].intent.args["message"], "<ip> at <mac> answers");
        assert_eq!(validate(&t, crate::contract::fingerprint()), Vec::<String>::new());
        // A typed path keeps its shape too.
        let mut t = record();
        t.steps[0].intent = Intent::new("list_directory", json!({"path": "/home/joel/notes"}));
        sanitize(&mut t);
        assert_eq!(t.steps[0].intent.args["path"], "/home/user/notes");
        assert_eq!(validate(&t, crate::contract::fingerprint()), Vec::<String>::new());
    }

    #[test]
    fn list_arguments_are_scrubbed() {
        let mut t = record();
        t.steps[0].intent =
            Intent::new("launch_program", json!({"program": "w3m", "args": ["/home/joel/x", "http://192.168.1.9/"]}));
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("not sanitized")));
        sanitize(&mut t);
        assert_eq!(t.steps[0].intent.args["args"], json!(["/home/user/x", "http://192.0.2.1/"]));
        assert_eq!(validate(&t, crate::contract::fingerprint()), Vec::<String>::new());
    }

    #[test]
    fn short_or_misnamed_values_of_unknown_actions_do_not_wreck_text() {
        let mut t = record();
        t.request = "say yes to my keyboard box".into();
        t.steps[0].intent = Intent::new("wifi_conect", json!({"key": "y", "keyboard_layout": "us", "monkey": "a"}));
        t.steps[0].disposition = Disposition::Invalid;
        sanitize(&mut t);
        assert_eq!(t.request, "say yes to my keyboard box");
        assert_eq!(t.steps[0].intent.args["key"], REDACTED, "the argument itself is still redacted");
        assert_eq!(t.steps[0].intent.args["keyboard_layout"], "us");
        assert_eq!(t.steps[0].intent.args["monkey"], "a");
    }

    #[test]
    fn secret_names_are_recognised_in_any_spelling() {
        for name in ["password", "wifiPassword", "wifi_password", "api_token", "apiKey", "api-key", "key", "pin"] {
            assert!(looks_secret(name), "{name}");
        }
        for name in ["keyboard_layout", "monkey", "keyboard", "spinner", "passage_count", "network"] {
            assert!(!looks_secret(name), "{name}");
        }
        // A numeric secret is scrubbed from text too.
        let mut t = record();
        t.request = "my pin is 12345678".into();
        t.steps[0].intent = Intent::new("unlock", json!({"pin": 12345678, "wifiPassword": "hunter2222"}));
        t.steps[0].disposition = Disposition::Invalid;
        t.steps[0].observation = "hunter2222 accepted".into();
        sanitize(&mut t);
        let line = serde_json::to_string(&t).unwrap();
        assert!(!line.contains("12345678") && !line.contains("hunter2222"), "{line}");
    }

    #[test]
    fn a_link_local_host_becomes_a_valid_documentation_address() {
        let mut t = record();
        t.steps[0].intent = Intent::new("ping_host", json!({"host": "fe80::1%eth0"}));
        t.steps[0].disposition = Disposition::Invalid; // the zone does not validate before either
        sanitize(&mut t);
        assert_eq!(t.steps[0].intent.args["host"], DOC_IPV6);
        assert!(ValidatedAction::from_intent(&t.steps[0].intent).is_ok());
        assert_eq!(scrub_identifiers("via fe80::1%eth0"), "via <ip>%eth0", "text keeps the interface");
    }

    #[test]
    fn secrets_in_unknown_actions_are_redacted_by_name() {
        let mut t = record();
        t.request = "join Home, the password is hunter2222".into();
        t.steps[0].intent = Intent::new("join_wifi", json!({"network": "Home", "password": "hunter2222"}));
        t.steps[0].disposition = Disposition::Invalid;
        sanitize(&mut t);
        let line = serde_json::to_string(&t).unwrap();
        assert!(!line.contains("hunter2222"), "{line}");
        assert_eq!(t.steps[0].intent.args["network"], "Home");
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

    /// The case the first version got wrong: a required command failed, yet the
    /// state checks passed afterwards. That is a failed example, kept as one.
    #[test]
    fn a_failed_required_step_is_a_failed_episode_even_when_checks_pass() {
        let mut t = record();
        t.outcome.step_failures =
            vec![StepFailure { step: 1, detail: "/usr/bin/cpkg install -- nano exited 1".into() }];
        t.outcome.verdict = Verdict::Failed;
        t.outcome.reasons = vec!["solution step failed: /usr/bin/cpkg install -- nano exited 1".into()];
        assert!(t.outcome.checks.iter().all(|c| c.passed));
        assert!(!t.outcome.succeeded());
        assert_eq!(validate(&t, crate::contract::fingerprint()), Vec::<String>::new(), "a valid failed example");
        let line = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Trajectory>(&line).unwrap(), t, "round-trips");

        // Claiming success over a failed step, or failing without saying why, is rejected.
        let mut lie = t.clone();
        lie.outcome.verdict = Verdict::Passed;
        lie.outcome.reasons.clear();
        assert!(!lie.outcome.succeeded());
        let problems = validate(&lie, crate::contract::fingerprint());
        assert!(problems.iter().any(|p| p.contains("a required step failed")), "{problems:?}");
        let mut silent = t.clone();
        silent.outcome.reasons.clear();
        let problems = validate(&silent, crate::contract::fingerprint());
        assert!(problems.iter().any(|p| p.contains("without a reason")), "{problems:?}");
    }

    #[test]
    fn outcomes_that_contradict_their_steps_are_reported() {
        let fp = crate::contract::fingerprint();
        let has = |t: &Trajectory, what: &str| validate(t, fp).iter().any(|p| p.contains(what));
        // An executed step whose observation says FAILED, but no recorded failure.
        let mut t = record();
        t.steps[0].observation = "OBSERVATION (install_package: FAILED, exit code 1)\nno repository".into();
        assert!(has(&t, "reports a failure that the outcome does not record"));
        // Recorded as a failure, the same episode is a valid failed example.
        t.outcome.step_failures = vec![StepFailure { step: 1, detail: "cpkg install -- nano exited 1".into() }];
        t.outcome.verdict = Verdict::Failed;
        t.outcome.reasons = vec!["solution step failed".into()];
        assert_eq!(validate(&t, fp), Vec::<String>::new());
        // A failure attributed to a step that never ran.
        t.steps[0].disposition = Disposition::Denied;
        assert!(has(&t, "never ran"));
        // A blank reason is no reason.
        let mut t = record();
        t.outcome.verdict = Verdict::Failed;
        t.outcome.reasons = vec![" ".into()];
        assert!(has(&t, "without a reason"));
        // Passing with nothing but invalid actions.
        let mut t = record();
        for s in &mut t.steps {
            s.disposition = Disposition::Invalid;
        }
        assert!(has(&t, "no step was a valid action"));
        // A model that corrects an invalid action and then succeeds may pass.
        let mut t = record();
        t.steps.insert(
            0,
            Step {
                intent: Intent::new("install_pkg", json!({})),
                disposition: Disposition::Invalid,
                observation: "OBSERVATION (invalid action)".into(),
            },
        );
        assert_eq!(validate(&t, fp), Vec::<String>::new());
    }

    #[test]
    fn check_names_are_sanitized() {
        let mut t = record();
        t.outcome.checks[0].name = "192.168.1.9 answers".into();
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("not sanitized")));
        sanitize(&mut t);
        assert_eq!(t.outcome.checks[0].name, "<ip> answers");
    }

    #[test]
    fn ipv6_followed_by_a_colon_and_exemptions_in_any_spelling() {
        assert_eq!(scrub_identifiers("dns 2001:4860::: and fe80:::"), "dns <ip>: and <ip>:");
        for kept in ["2001:DB8::1", "2001:0db8::1", "0:0:0:0:0:0:0:1", "::ffff:127.0.0.1", "0::0"] {
            assert_eq!(scrub_identifiers(kept), kept, "{kept}");
            let mut t = record();
            t.steps[0].intent = Intent::new("ping_host", json!({"host": kept}));
            sanitize(&mut t);
            assert_eq!(t.steps[0].intent.args["host"], kept, "typed {kept} keeps its meaning");
        }
        let mut t = record();
        t.steps[0].intent = Intent::new("ping_host", json!({"host": "2001:4860::"}));
        t.request = "use 2001:4860:::".into();
        sanitize(&mut t);
        assert_eq!(t.request, "use <ip>:");
        let once = t.clone();
        sanitize(&mut t);
        assert_eq!(t, once, "idempotent");
    }

    #[test]
    fn outcome_contradictions_are_reported() {
        let mut t = record();
        t.outcome.checks[0].passed = false;
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("a state check failed")));
        let mut t = record();
        t.outcome.ran_in_vm = false;
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("did not run in a VM")));
        let mut t = record();
        t.outcome.verdict = Verdict::Failed;
        t.outcome.reasons = vec!["x".into()];
        t.outcome.step_failures = vec![StepFailure { step: 9, detail: "x".into() }];
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("names step 9")));
        // A verdict the runner did not give fails the check too, and so does a
        // record from schema 1, which had no verdict at all.
        let v1 = r#"{"schema":1,"contract":"x","episode":"e","task":"t","split":"train","source":"reference",
            "request":"r","steps":[],"outcome":{"checks":[],"ran_in_vm":true}}"#;
        assert!(serde_json::from_str::<Trajectory>(v1).is_err());
    }

    #[test]
    fn reasons_and_failure_details_are_sanitized() {
        let mut t = record();
        t.outcome.verdict = Verdict::Failed;
        t.outcome.reasons = vec!["ping 192.168.1.9 failed".into()];
        t.outcome.step_failures = vec![StepFailure { step: 1, detail: "/home/joel/x exited 1".into() }];
        assert!(validate(&t, crate::contract::fingerprint()).iter().any(|p| p.contains("not sanitized")));
        sanitize(&mut t);
        assert_eq!(t.outcome.reasons, ["ping <ip> failed"]);
        assert_eq!(t.outcome.step_failures[0].detail, "/home/<user>/x exited 1");
    }
}
