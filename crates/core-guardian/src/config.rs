//! Guardian configuration (`/etc/core/guardian.toml`).
//!
//! Every field has a safe default, so an empty file is a valid configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use core_protocol::Risk;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuardianConfig {
    /// Socket path used when not socket-activated by systemd.
    pub socket: PathBuf,
    /// Permission bits for a self-created socket.
    pub socket_mode: u32,
    /// Group owning a self-created socket.
    pub socket_group: Option<String>,
    /// Users (by uid) allowed to talk to the Guardian. Root is always allowed.
    pub allowed_uids: Vec<u32>,
    /// Groups whose members may talk to the Guardian.
    pub allowed_groups: Vec<String>,
    pub audit_log: PathBuf,
    /// Plan and audit everything but execute nothing.
    pub dry_run: bool,
    /// Highest risk executed without human confirmation.
    pub auto_approve: Risk,
    pub confirmation_timeout_secs: u64,
    pub command_timeout_secs: u64,
    /// Timeout for package manager operations (downloads can be slow).
    pub package_timeout_secs: u64,
    /// Output kept per command (head and tail are preserved).
    pub max_output_bytes: usize,
    pub max_requests_per_minute: u32,
    pub max_connections: usize,
    pub system: SystemConfig,
    pub paths: PathPolicy,
    pub services: ServicePolicy,
    pub packages: PackagePolicy,
    pub actions: ActionPolicy,
    /// Absolute paths of every external program the Guardian may run.
    pub tools: BTreeMap<String, PathBuf>,
    pub native: NativeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Pacman,
    Apt,
    Dnf,
    Zypper,
    Apk,
    Xbps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioBackend {
    /// PipeWire (runs as the requesting user, whose session owns the audio server).
    Wpctl,
    /// PulseAudio or pipewire-pulse (runs as the requesting user).
    Pactl,
    /// Plain ALSA mixer (runs as root).
    Amixer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkBackend {
    NetworkManager,
    Iwd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SystemConfig {
    pub package_manager: PackageManager,
    pub audio: AudioBackend,
    pub network: NetworkBackend,
}

impl Default for SystemConfig {
    fn default() -> Self {
        SystemConfig {
            package_manager: PackageManager::Pacman,
            audio: AudioBackend::Wpctl,
            network: NetworkBackend::NetworkManager,
        }
    }
}

/// Which files `read_file`/`list_directory` may touch. Checked on the canonical path
/// (after resolving symlinks), so a link cannot smuggle in a denied file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathPolicy {
    /// Allowed prefixes.
    pub readable: Vec<PathBuf>,
    /// Denied prefixes (win over `readable`).
    pub denied: Vec<PathBuf>,
    /// Denied file or directory names anywhere in the path.
    pub denied_names: Vec<String>,
    /// Denied file name suffixes.
    pub denied_suffixes: Vec<String>,
}

impl Default for PathPolicy {
    fn default() -> Self {
        let paths = |v: &[&str]| v.iter().map(PathBuf::from).collect();
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        PathPolicy {
            readable: paths(&[
                "/etc",
                "/var/log",
                "/proc",
                "/sys",
                "/home",
                "/usr/share",
                "/usr/lib",
                "/boot",
                "/run",
                "/tmp",
                "/opt",
                "/srv",
                "/var/lib",
                "/var/cache",
            ]),
            denied: paths(&[
                "/etc/shadow",
                "/etc/shadow-",
                "/etc/gshadow",
                "/etc/gshadow-",
                "/etc/sudoers",
                "/etc/sudoers.d",
                "/etc/ssh",
                "/etc/core/secrets",
                "/etc/NetworkManager/system-connections",
                "/etc/wpa_supplicant",
                "/var/lib/iwd",
                "/var/lib/NetworkManager",
                "/var/lib/systemd/credential.secret",
                "/run/credentials",
                "/proc/kcore",
                "/root",
            ]),
            denied_names: strings(&[
                ".ssh",
                ".gnupg",
                ".password-store",
                ".netrc",
                ".pgpass",
                "environ",
                "mem",
                "pagemap",
                "kcore",
            ]),
            denied_suffixes: strings(&[".key", ".pem", ".p12", ".pfx", "_rsa", "_ed25519", "_ecdsa", "_dsa", ".kdbx"]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServicePolicy {
    /// Units that may be inspected but never stopped, restarted or disabled.
    pub protected: Vec<String>,
}

impl Default for ServicePolicy {
    fn default() -> Self {
        let protected = [
            "core-guardian",
            "core-guardian.socket",
            "core-sensed",
            "core-inference",
            "dbus",
            "dbus-broker",
            "dbus.socket",
            "systemd-journald",
            "systemd-logind",
            "systemd-udevd",
            "getty@tty1",
            "polkit",
        ];
        ServicePolicy { protected: protected.iter().map(|s| s.to_string()).collect() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackagePolicy {
    /// Packages that may never be removed.
    pub protected: Vec<String>,
}

impl Default for PackagePolicy {
    fn default() -> Self {
        let protected = [
            "base",
            "linux",
            "linux-lts",
            "linux-zen",
            "linux-hardened",
            "linux-firmware",
            "systemd",
            "systemd-libs",
            "glibc",
            "pacman",
            "bash",
            "coreutils",
            "util-linux",
            "filesystem",
            "core-os",
            "llama.cpp",
            "sudo",
            "apt",
            "dpkg",
            "dnf",
            "rpm",
            "libc6",
            "musl",
            "apk-tools",
            "busybox",
        ];
        PackagePolicy { protected: protected.iter().map(|s| s.to_string()).collect() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActionPolicy {
    /// Actions refused outright.
    pub disabled: Vec<String>,
    /// Actions that always need confirmation regardless of risk.
    pub always_confirm: Vec<String>,
    /// Actions that never need confirmation (use with great care).
    pub never_confirm: Vec<String>,
    /// Per-action risk overrides.
    pub risk: BTreeMap<String, Risk>,
}

/// Locations used by natively implemented operations (overridable for tests).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NativeConfig {
    pub sysfs: PathBuf,
    pub procfs: PathBuf,
    pub fstab: PathBuf,
    pub swapfile: PathBuf,
}

impl Default for NativeConfig {
    fn default() -> Self {
        NativeConfig {
            sysfs: "/sys".into(),
            procfs: "/proc".into(),
            fstab: "/etc/fstab".into(),
            swapfile: "/swapfile".into(),
        }
    }
}

pub fn default_tools() -> BTreeMap<String, PathBuf> {
    let tools: &[(&str, &str)] = &[
        ("systemctl", "/usr/bin/systemctl"),
        ("journalctl", "/usr/bin/journalctl"),
        ("dmesg", "/usr/bin/dmesg"),
        ("df", "/usr/bin/df"),
        ("lsblk", "/usr/bin/lsblk"),
        ("lspci", "/usr/bin/lspci"),
        ("lsusb", "/usr/bin/lsusb"),
        ("ps", "/usr/bin/ps"),
        ("ip", "/usr/bin/ip"),
        ("ping", "/usr/bin/ping"),
        ("nmcli", "/usr/bin/nmcli"),
        ("iwctl", "/usr/bin/iwctl"),
        ("wpctl", "/usr/bin/wpctl"),
        ("pactl", "/usr/bin/pactl"),
        ("amixer", "/usr/bin/amixer"),
        ("modprobe", "/usr/bin/modprobe"),
        ("hostnamectl", "/usr/bin/hostnamectl"),
        ("timedatectl", "/usr/bin/timedatectl"),
        ("swapon", "/usr/bin/swapon"),
        ("swapoff", "/usr/bin/swapoff"),
        ("mkswap", "/usr/bin/mkswap"),
        ("fallocate", "/usr/bin/fallocate"),
        ("btrfs", "/usr/bin/btrfs"),
        ("pacman", "/usr/bin/pacman"),
        ("apt-get", "/usr/bin/apt-get"),
        ("apt-cache", "/usr/bin/apt-cache"),
        ("dpkg", "/usr/bin/dpkg"),
        ("dnf", "/usr/bin/dnf"),
        ("rpm", "/usr/bin/rpm"),
        ("zypper", "/usr/bin/zypper"),
        ("apk", "/sbin/apk"),
        ("xbps-install", "/usr/bin/xbps-install"),
        ("xbps-remove", "/usr/bin/xbps-remove"),
        ("xbps-query", "/usr/bin/xbps-query"),
    ];
    tools.iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect()
}

impl Default for GuardianConfig {
    fn default() -> Self {
        GuardianConfig {
            socket: core_protocol::DEFAULT_GUARDIAN_SOCKET.into(),
            socket_mode: 0o660,
            socket_group: Some("core".into()),
            allowed_uids: Vec::new(),
            allowed_groups: vec!["core".into()],
            audit_log: "/var/log/core/audit.jsonl".into(),
            dry_run: false,
            auto_approve: Risk::Low,
            confirmation_timeout_secs: 120,
            command_timeout_secs: 60,
            package_timeout_secs: 1800,
            max_output_bytes: 16 * 1024,
            max_requests_per_minute: 60,
            max_connections: 8,
            system: SystemConfig::default(),
            paths: PathPolicy::default(),
            services: ServicePolicy::default(),
            packages: PackagePolicy::default(),
            actions: ActionPolicy::default(),
            tools: default_tools(),
            native: NativeConfig::default(),
        }
    }
}

impl GuardianConfig {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let mut config: GuardianConfig = toml::from_str(text).map_err(|e| e.to_string())?;
        // A config that names a few tools extends the defaults rather than replacing them.
        let mut tools = default_tools();
        tools.append(&mut config.tools);
        config.tools = tools;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn validate(&self) -> Result<(), String> {
        for name in self
            .actions
            .disabled
            .iter()
            .chain(&self.actions.always_confirm)
            .chain(&self.actions.never_confirm)
            .chain(self.actions.risk.keys())
        {
            if core_protocol::catalog::find(name).is_none() {
                return Err(format!("unknown action {name:?} in [actions]"));
            }
        }
        for (name, path) in &self.tools {
            if !path.is_absolute() {
                return Err(format!("tool {name} must have an absolute path"));
            }
        }
        if self.socket_mode & 0o007 != 0 {
            return Err("socket_mode must not grant access to other users".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_valid() {
        let c = GuardianConfig::from_toml("").unwrap();
        assert_eq!(c.auto_approve, Risk::Low);
        assert!(c.tools.contains_key("systemctl"));
    }

    #[test]
    fn partial_config_overrides() {
        let c = GuardianConfig::from_toml(
            r#"
            auto_approve = "observe"
            dry_run = true
            [system]
            package_manager = "apt"
            [actions]
            disabled = ["poweroff"]
            risk = { set_volume = "observe" }
            [tools]
            systemctl = "/bin/systemctl"
            "#,
        )
        .unwrap();
        assert_eq!(c.auto_approve, Risk::Observe);
        assert_eq!(c.system.package_manager, PackageManager::Apt);
        assert_eq!(c.tools["systemctl"], PathBuf::from("/bin/systemctl"));
        assert!(c.tools.contains_key("journalctl"), "defaults kept");
        assert_eq!(c.actions.risk["set_volume"], Risk::Observe);
    }

    #[test]
    fn shipped_config_is_valid() {
        let c = GuardianConfig::from_toml(include_str!("../../../system/etc/core/guardian.toml")).unwrap();
        assert_eq!(c.auto_approve, Risk::Low);
        assert!(c.services.protected.iter().any(|s| s == "core-guardian"));
    }

    #[test]
    fn rejects_mistakes() {
        assert!(GuardianConfig::from_toml("[actions]\ndisabled = [\"rm_rf\"]").is_err());
        assert!(GuardianConfig::from_toml("typo_field = 1").is_err());
        assert!(GuardianConfig::from_toml("[tools]\nfoo = \"relative/bin\"").is_err());
        assert!(GuardianConfig::from_toml("socket_mode = 0o666").is_err());
    }
}
