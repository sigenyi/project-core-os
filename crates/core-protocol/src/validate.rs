//! Syntactic validators for every argument type the model can produce.
//!
//! These are the authoritative checks. The GBNF grammar (see [`crate::grammar`])
//! mirrors them so that well-behaved models rarely produce invalid values, but the
//! grammar is only an optimisation: the Guardian re-validates everything it receives.
//!
//! A recurring rule below is "must not start with `-`". Arguments are always passed to
//! programs as discrete argv entries (never through a shell), but a value beginning with
//! a dash could still be interpreted as an option by the target program.

use std::net::IpAddr;

pub type Result = std::result::Result<(), String>;

fn check_len(value: &str, min: usize, max: usize, what: &str) -> Result {
    let n = value.chars().count();
    if n < min {
        return Err(format!("{what} must be at least {min} characters"));
    }
    if n > max {
        return Err(format!("{what} must be at most {max} characters"));
    }
    Ok(())
}

fn check_charset(value: &str, what: &str, allowed: impl Fn(char) -> bool) -> Result {
    match value.chars().find(|c| !allowed(*c)) {
        Some(c) => Err(format!("{what} contains a forbidden character {c:?}")),
        None => Ok(()),
    }
}

fn check_first_alnum(value: &str, what: &str) -> Result {
    match value.chars().next() {
        Some(c) if c.is_ascii_alphanumeric() => Ok(()),
        _ => Err(format!("{what} must start with a letter or digit")),
    }
}

fn no_control(value: &str, what: &str, allow_newlines: bool) -> Result {
    check_charset(value, what, |c| {
        !c.is_control() || (allow_newlines && (c == '\n' || c == '\t'))
    })
}

/// Unit types the Guardian is willing to manage.
pub const UNIT_SUFFIXES: &[&str] = &[".service", ".socket", ".timer", ".target", ".path", ".mount"];

/// A systemd unit name such as `wpa_supplicant`, `bluetooth.service` or `getty@tty2.service`.
pub fn service_name(value: &str) -> Result {
    const WHAT: &str = "service name";
    check_len(value, 1, 128, WHAT)?;
    check_first_alnum(value, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_alphanumeric() || "_.:@-".contains(c))?;
    if value.contains("..") {
        return Err(format!("{WHAT} must not contain '..'"));
    }
    if let Some(dot) = value.rfind('.') {
        let suffix = &value[dot..];
        // Names like "systemd-networkd.service" have a type suffix; names like
        // "foo.bar" (no recognised suffix) are treated as service names by systemctl,
        // which would be surprising, so reject unknown suffixes.
        if !UNIT_SUFFIXES.contains(&suffix) {
            return Err(format!(
                "{WHAT} has unsupported unit type {suffix:?}; use one of {}",
                UNIT_SUFFIXES.join(", ")
            ));
        }
    }
    Ok(())
}

/// A distribution package name (`firefox`, `linux-firmware`, `python3.12`, `g++`).
pub fn package_name(value: &str) -> Result {
    const WHAT: &str = "package name";
    check_len(value, 1, 128, WHAT)?;
    check_first_alnum(value, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
}

/// A free-text package search query (`web browser`, `audio`).
pub fn package_query(value: &str) -> Result {
    const WHAT: &str = "search query";
    check_len(value, 1, 64, WHAT)?;
    check_first_alnum(value, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_alphanumeric() || " ._+-".contains(c))
}

/// A kernel module name (`iwlwifi`, `snd_hda_intel`, `snd-hda-intel`).
pub fn kernel_module(value: &str) -> Result {
    const WHAT: &str = "kernel module";
    check_len(value, 1, 64, WHAT)?;
    check_first_alnum(value, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_alphanumeric() || "_-".contains(c))
}

/// Normalise an absolute path: collapse `//` and `.` components, reject `..`.
///
/// Returns the normalised path. Symlinks are *not* resolved here (that requires the
/// filesystem); the Guardian canonicalises again before acting.
pub fn normalize_path(value: &str) -> std::result::Result<String, String> {
    const WHAT: &str = "path";
    check_len(value, 1, 4096, WHAT)?;
    if !value.starts_with('/') {
        return Err("path must be absolute (start with '/')".into());
    }
    no_control(value, WHAT, false)?;
    let mut parts: Vec<&str> = Vec::new();
    for comp in value.split('/') {
        match comp {
            "" | "." => {}
            ".." => return Err("path must not contain '..' components".into()),
            other => parts.push(other),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

pub fn path(value: &str) -> Result {
    normalize_path(value).map(|_| ())
}

/// A DNS hostname (RFC 1123), e.g. `core-laptop` or `nas.home.arpa`.
pub fn hostname(value: &str) -> Result {
    const WHAT: &str = "hostname";
    check_len(value, 1, 253, WHAT)?;
    for label in value.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("{WHAT} labels must be 1-63 characters"));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!("{WHAT} labels must not start or end with '-'"));
        }
        check_charset(label, WHAT, |c| c.is_ascii_alphanumeric() || c == '-')?;
    }
    Ok(())
}

/// A host to contact: hostname or literal IPv4/IPv6 address.
pub fn host_target(value: &str) -> Result {
    if value.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    hostname(value).map_err(|e| format!("{e} (or give an IP address)"))
}

/// An IANA time zone such as `Europe/Berlin` or `UTC`.
pub fn timezone(value: &str) -> Result {
    const WHAT: &str = "time zone";
    check_len(value, 1, 64, WHAT)?;
    check_first_alnum(value, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_alphanumeric() || "_+/-".contains(c))?;
    if value.contains("..") || value.contains("//") || value.ends_with('/') {
        return Err(format!("{WHAT} is malformed"));
    }
    Ok(())
}

/// A Wi-Fi network name (1-32 bytes, printable).
pub fn ssid(value: &str) -> Result {
    const WHAT: &str = "SSID";
    if value.is_empty() || value.len() > 32 {
        return Err(format!("{WHAT} must be 1-32 bytes"));
    }
    no_control(value, WHAT, false)?;
    if value.starts_with('-') {
        return Err(format!("{WHAT} must not start with '-'"));
    }
    Ok(())
}

/// A WPA passphrase (8-63 printable ASCII characters).
pub fn wifi_passphrase(value: &str) -> Result {
    const WHAT: &str = "passphrase";
    check_len(value, 8, 63, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_graphic() || c == ' ')
}

/// A network interface name (`wlan0`, `enp3s0`, `wlp2s0`).
pub fn interface(value: &str) -> Result {
    const WHAT: &str = "interface name";
    check_len(value, 1, 15, WHAT)?;
    check_first_alnum(value, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
}

/// A program name, resolved by the agent against its launch allowlist (`htop`, `nano`).
pub fn program(value: &str) -> Result {
    const WHAT: &str = "program name";
    check_len(value, 1, 64, WHAT)?;
    check_first_alnum(value, WHAT)?;
    check_charset(value, WHAT, |c| c.is_ascii_alphanumeric() || "_.+-".contains(c))
}

/// A single argument for an interactive user program.
pub fn program_arg(value: &str) -> Result {
    const WHAT: &str = "program argument";
    check_len(value, 1, 256, WHAT)?;
    no_control(value, WHAT, false)
}

/// Free text written by the model for the user (messages, questions).
pub fn text(value: &str, max_len: usize) -> Result {
    const WHAT: &str = "text";
    check_len(value, 1, max_len, WHAT)?;
    no_control(value, WHAT, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names() {
        for ok in ["wpa_supplicant", "bluetooth.service", "getty@tty2.service", "NetworkManager", "systemd-resolved.service", "fstrim.timer"] {
            assert!(service_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-foo", "--now", "foo bar", "a/b", "x.conf", "..service", "a;b", "$(reboot)"] {
            assert!(service_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn package_names() {
        for ok in ["firefox", "linux-firmware", "g++", "python3.12", "lib32-mesa", "NetworkManager"] {
            assert!(package_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-y", "--noconfirm", "a b", "a/b", "foo;rm", "../x"] {
            assert!(package_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(normalize_path("/etc//pacman.d/./mirrorlist").unwrap(), "/etc/pacman.d/mirrorlist");
        assert_eq!(normalize_path("/").unwrap(), "/");
        assert_eq!(normalize_path("/var/log/").unwrap(), "/var/log");
        assert!(normalize_path("etc/passwd").is_err());
        assert!(normalize_path("/etc/../root").is_err());
        assert!(normalize_path("/tmp/a\nb").is_err());
    }

    #[test]
    fn hostnames_and_hosts() {
        assert!(hostname("core-laptop").is_ok());
        assert!(hostname("nas.home.arpa").is_ok());
        assert!(hostname("-bad").is_err());
        assert!(hostname("bad-").is_err());
        assert!(hostname("a..b").is_err());
        assert!(hostname("under_score").is_err());
        assert!(host_target("1.1.1.1").is_ok());
        assert!(host_target("2606:4700::1111").is_ok());
        assert!(host_target("archlinux.org").is_ok());
        assert!(host_target("-c99").is_err());
    }

    #[test]
    fn timezones() {
        assert!(timezone("Europe/Berlin").is_ok());
        assert!(timezone("America/Argentina/Buenos_Aires").is_ok());
        assert!(timezone("UTC").is_ok());
        assert!(timezone("Etc/GMT+5").is_ok());
        assert!(timezone("../../etc/shadow").is_err());
        assert!(timezone("/etc/shadow").is_err());
    }

    #[test]
    fn wifi_values() {
        assert!(ssid("Home WiFi 5G").is_ok());
        assert!(ssid("-x").is_err());
        assert!(ssid(&"a".repeat(33)).is_err());
        assert!(wifi_passphrase("hunter22").is_ok());
        assert!(wifi_passphrase("short").is_err());
    }

    #[test]
    fn text_allows_newlines_but_not_other_controls() {
        assert!(text("line one\nline two", 100).is_ok());
        assert!(text("bell\u{7}", 100).is_err());
        assert!(text("", 100).is_err());
        assert!(text(&"x".repeat(101), 100).is_err());
    }
}
