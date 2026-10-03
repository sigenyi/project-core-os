//! Show what the Guardian would do with an intent, without doing it.
//!
//! `core-guardian --plan INTENT` validates the intent, asks the policy for its
//! decision and expands the action into its plan, then prints all three as JSON.
//! Nothing is executed, no confirmation is asked, nothing is audited and no socket
//! is involved. The evaluation harness runs the printed command lines in a
//! disposable VM to test the planner against the real operating system; that is a
//! test of the plans, not of authorization (which only the running Guardian applies).

use core_protocol::{Intent, ValidatedAction};
use serde_json::{Value, json};

use crate::config::GuardianConfig;
use crate::plan::{RunAs, Step};
use crate::planner::{Planner, SystemProbe};
use crate::policy::{Decision, Policy};

/// The decision and plan for `intent`, as JSON. A plan that would carry a secret
/// (a Wi-Fi passphrase) is refused rather than printed.
pub fn preview(config: &GuardianConfig, probe: &dyn SystemProbe, intent: &Intent) -> Result<Value, String> {
    let v = ValidatedAction::from_intent(intent).map_err(|e| e.to_string())?;
    let decision = match Policy::with_probe(config, probe).authorize(&v) {
        Decision::Allow => json!({"decision": "allow"}),
        Decision::Confirm { risk } => json!({"decision": "confirm", "risk": risk}),
        Decision::Deny { reason } => {
            return Ok(json!({"action": v.name(), "policy": {"decision": "deny", "reason": reason}, "steps": []}));
        }
    };
    let plan = Planner::new(config, probe).plan(&v.action)?;
    let mut steps = Vec::new();
    for step in &plan.steps {
        match step {
            Step::Run(c) => {
                if !c.secret_args.is_empty() {
                    return Err(format!("the plan for {} carries a secret argument; it is not printed", v.name()));
                }
                let program = config.tools.get(c.tool).ok_or_else(|| format!("tool {} is not configured", c.tool))?;
                steps.push(json!({"run": {
                    "program": program,
                    "args": c.args,
                    "env": c.env,
                    "as": if c.run_as == RunAs::Peer { "peer" } else { "root" },
                    "success_codes": c.success_codes,
                    "optional": c.optional,
                }}));
            }
            Step::Native(op) => steps.push(json!({"native": op.describe()})),
        }
    }
    Ok(json!({"action": v.name(), "args": v.redacted_args(), "risk": v.risk(), "policy": decision, "steps": steps}))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    struct NoProbe;
    impl SystemProbe for NoProbe {
        fn wireless_interfaces(&self) -> Vec<String> {
            vec!["wlan0".into()]
        }
        fn filesystem_type(&self, _: &Path) -> Option<String> {
            Some("ext4".into())
        }
    }

    fn show(config: &GuardianConfig, action: &str, args: Value) -> Result<Value, String> {
        preview(config, &NoProbe, &Intent::new(action, args))
    }

    #[test]
    fn install_needs_confirmation_and_plans_cpkg() {
        let p = show(&GuardianConfig::default(), "install_package", json!({"package": "nano"})).unwrap();
        assert_eq!(p["policy"]["decision"], "confirm");
        assert_eq!(p["steps"][0]["run"]["program"], "/usr/bin/cpkg");
        assert_eq!(p["steps"][0]["run"]["args"], json!(["install", "--", "nano"]));
        assert_eq!(p["steps"][0]["run"]["as"], "root");
    }

    #[test]
    fn policy_denials_have_no_steps() {
        let p = show(&GuardianConfig::default(), "remove_package", json!({"package": "cpkg"})).unwrap();
        assert_eq!(p["policy"]["decision"], "deny");
        assert_eq!(p["steps"], json!([]));
        let p = show(&GuardianConfig::default(), "stop_service", json!({"service": "core-guardian"})).unwrap();
        assert_eq!(p["policy"]["decision"], "deny");
    }

    #[test]
    fn read_only_actions_are_allowed() {
        let p = show(&GuardianConfig::default(), "network_status", json!({})).unwrap();
        assert_eq!(p["policy"]["decision"], "allow");
        assert_eq!(p["steps"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn secrets_and_invalid_intents_are_refused() {
        let mut nm = GuardianConfig::default();
        nm.system.network = crate::config::NetworkBackend::NetworkManager;
        let err = show(&nm, "wifi_connect", json!({"ssid": "Home", "passphrase": "hunter222"})).unwrap_err();
        assert!(err.contains("secret") && !err.contains("hunter222"), "{err}");
        assert!(show(&GuardianConfig::default(), "rm_rf", json!({})).is_err());
        assert!(show(&GuardianConfig::default(), "install_package", json!({"package": "-rf"})).is_err());
    }
}
