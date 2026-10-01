//! Which files the AI may look at.
//!
//! Reads (`read_file`, `list_directory`) run in the unprivileged agent with the user's
//! own permissions, so the kernel already prevents access to anything the user
//! cannot read. This policy is a second layer: it keeps credentials the user *can*
//! read (their SSH keys, saved Wi-Fi passwords) out of the model's context, and keeps
//! away from kernel interfaces whose reads block or never end. Checks are made on the
//! canonical path, after symlinks are resolved.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
                "/media",
                "/mnt",
            ]),
            denied: paths(&[
                // credentials
                "/etc/shadow",
                "/etc/shadow-",
                "/etc/gshadow",
                "/etc/gshadow-",
                "/etc/sudoers",
                "/etc/sudoers.d",
                "/etc/ssh",
                "/etc/wireguard",
                "/etc/core/secrets",
                "/etc/NetworkManager/system-connections",
                "/run/NetworkManager/system-connections",
                "/etc/wpa_supplicant",
                "/var/lib/iwd",
                "/var/lib/NetworkManager",
                "/var/lib/bluetooth",
                "/var/lib/systemd/credential.secret",
                "/run/credentials",
                "/root",
                // kernel interfaces that block, stream forever or expose memory
                "/proc/kcore",
                "/proc/kmsg",
                "/proc/sysrq-trigger",
                "/sys/kernel/debug",
                "/sys/kernel/tracing",
                "/sys/firmware/efi/efivars",
            ]),
            denied_names: strings(&[
                ".ssh",
                ".gnupg",
                ".password-store",
                ".netrc",
                ".pgpass",
                ".git-credentials",
                ".docker",
                "keyrings",
                "environ",
                "mem",
                "pagemap",
                "kcore",
                "kmsg",
                "trace_pipe",
                "trace_pipe_raw",
            ]),
            denied_suffixes: strings(&[".key", ".pem", ".p12", ".pfx", "_rsa", "_ed25519", "_ecdsa", "_dsa", ".kdbx"]),
        }
    }
}

impl PathPolicy {
    /// Whether `path` (which should already be canonical) may be read.
    pub fn check_readable(&self, path: &Path) -> Result<(), String> {
        let shown = path.display();
        if self.denied.iter().any(|d| path.starts_with(d)) {
            return Err(format!("{shown} is off limits (credentials or kernel interface)"));
        }
        for comp in path.components() {
            if let Component::Normal(name) = comp {
                let name = name.to_string_lossy();
                if self.denied_names.iter().any(|d| *d == name) {
                    return Err(format!("{shown} is off limits ({name} may contain secrets)"));
                }
            }
        }
        if let Some(file) = path.file_name().map(|f| f.to_string_lossy()) {
            if self.denied_suffixes.iter().any(|s| file.ends_with(s.as_str())) {
                return Err(format!("{shown} looks like a key or credential file"));
            }
        }
        if path != Path::new("/") && !self.readable.iter().any(|r| path.starts_with(r)) {
            let roots: Vec<String> = self.readable.iter().map(|r| r.display().to_string()).collect();
            return Err(format!("{shown} is outside the readable areas ({})", roots.join(", ")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rules() {
        let p = PathPolicy::default();
        for ok in ["/", "/etc/fstab", "/var/log/pacman.log", "/home/core/notes.txt", "/proc/cpuinfo"] {
            assert!(p.check_readable(Path::new(ok)).is_ok(), "{ok}");
        }
        for bad in [
            "/etc/shadow",
            "/etc/ssh/ssh_host_ed25519_key",
            "/etc/wireguard/wg0.conf",
            "/var/lib/bluetooth/AA:BB/CC:DD/info",
            "/run/NetworkManager/system-connections/home.nmconnection",
            "/home/core/.ssh/config",
            "/home/core/certs/server.key",
            "/home/core/.local/share/keyrings/login.keyring",
            "/proc/1/environ",
            "/proc/kmsg",
            "/sys/kernel/tracing/trace_pipe",
            "/root/notes",
            "/dev/sda",
        ] {
            assert!(p.check_readable(Path::new(bad)).is_err(), "{bad}");
        }
    }
}
