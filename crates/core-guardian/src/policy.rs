//! Authorisation: given a validated action, allow it, demand confirmation, or deny it.

use std::path::{Component, Path};

use core_protocol::wire::Capability;
use core_protocol::{Action, CATALOG, Executor, Risk, ValidatedAction};

use crate::config::GuardianConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Confirm { risk: Risk },
    Deny { reason: String },
}

pub struct Policy<'a> {
    config: &'a GuardianConfig,
}

impl<'a> Policy<'a> {
    pub fn new(config: &'a GuardianConfig) -> Self {
        Policy { config }
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
        let risk = self.effective_risk(name, v.risk());
        if self.needs_confirmation(name, risk) { Decision::Confirm { risk } } else { Decision::Allow }
    }

    /// Argument-dependent rules: protected services/packages, readable paths, pids.
    fn check_specifics(&self, action: &Action) -> Result<(), String> {
        use Action::*;
        match action {
            RestartService { service } | StopService { service } | DisableService { service, .. } => {
                if self.is_protected_service(service.as_str()) {
                    return Err(format!("{service} is a protected system service and cannot be stopped or restarted"));
                }
            }
            RemovePackage { package } if self.config.packages.protected.iter().any(|p| p == package.as_str()) => {
                return Err(format!("{package} is essential to the system and cannot be removed"));
            }
            ListDirectory { path } | ReadFile { path, .. } => self.check_readable(Path::new(path.as_str()))?,
            KillProcess { pid, .. } if *pid == std::process::id() => {
                return Err("refusing to signal the Guardian itself".into());
            }
            _ => {}
        }
        Ok(())
    }

    pub fn is_protected_service(&self, unit: &str) -> bool {
        let base = unit.strip_suffix(".service").unwrap_or(unit);
        self.config.services.protected.iter().any(|p| p == unit || p == base)
    }

    /// Path rules. Called on the requested path here and again on the canonical path
    /// at execution time.
    pub fn check_readable(&self, path: &Path) -> Result<(), String> {
        let p = &self.config.paths;
        let shown = path.display();
        if p.denied.iter().any(|d| path.starts_with(d)) {
            return Err(format!("{shown} is off limits (protected secrets)"));
        }
        for comp in path.components() {
            if let Component::Normal(name) = comp {
                let name = name.to_string_lossy();
                if p.denied_names.iter().any(|d| *d == name) {
                    return Err(format!("{shown} is off limits ({name} may contain secrets)"));
                }
            }
        }
        if let Some(file) = path.file_name().map(|f| f.to_string_lossy()) {
            if p.denied_suffixes.iter().any(|s| file.ends_with(s.as_str())) {
                return Err(format!("{shown} looks like a key or credential file"));
            }
        }
        if path != Path::new("/") && !p.readable.iter().any(|r| path.starts_with(r)) {
            let roots: Vec<String> = p.readable.iter().map(|r| r.display().to_string()).collect();
            return Err(format!("{shown} is outside the readable areas ({})", roots.join(", ")));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use core_protocol::Intent;
    use serde_json::json;

    use super::*;

    fn decide(config: &GuardianConfig, action: &str, args: serde_json::Value) -> Decision {
        let v = ValidatedAction::from_intent(&Intent::new(action, args)).unwrap();
        Policy::new(config).authorize(&v)
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
        // Inspecting a protected service is fine.
        assert_eq!(decide(&c, "service_status", json!({"service": "dbus"})), Decision::Allow);
        assert!(matches!(decide(&c, "remove_package", json!({"package": "glibc"})), Decision::Deny { .. }));
    }

    #[test]
    fn path_rules() {
        let c = GuardianConfig::default();
        let p = Policy::new(&c);
        assert!(p.check_readable(Path::new("/etc/fstab")).is_ok());
        assert!(p.check_readable(Path::new("/var/log/pacman.log")).is_ok());
        assert!(p.check_readable(Path::new("/")).is_ok());
        assert!(p.check_readable(Path::new("/etc/shadow")).is_err());
        assert!(p.check_readable(Path::new("/etc/ssh/ssh_host_ed25519_key")).is_err());
        assert!(p.check_readable(Path::new("/home/core/.ssh/config")).is_err());
        assert!(p.check_readable(Path::new("/home/core/certs/server.key")).is_err());
        assert!(p.check_readable(Path::new("/proc/1/environ")).is_err());
        assert!(p.check_readable(Path::new("/root/notes")).is_err());
        assert!(p.check_readable(Path::new("/dev/sda")).is_err());
        assert!(matches!(decide(&c, "read_file", json!({"path": "/etc/shadow"})), Decision::Deny { .. }));
    }

    #[test]
    fn capabilities_cover_guardian_actions_only() {
        let mut c = GuardianConfig::default();
        c.actions.disabled.push("reboot".into());
        let caps = Policy::new(&c).capabilities();
        assert!(caps.iter().all(|c| c.action != "respond" && c.action != "reboot"));
        let install = caps.iter().find(|c| c.action == "install_package").unwrap();
        assert!(install.requires_confirmation);
        let du = caps.iter().find(|c| c.action == "disk_usage").unwrap();
        assert!(!du.requires_confirmation);
    }
}
