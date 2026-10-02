use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;

use super::Collector;
use crate::snapshot::{KernelMessage, Snapshot};
use crate::sysroot::Sysroot;

/// Notable kernel messages from `/dev/kmsg` (driver crashes, firmware failures, I/O
/// errors, OOM kills). Requires CAP_SYSLOG when `kernel.dmesg_restrict = 1`.
pub struct KernelLogCollector {
    pub max_messages: usize,
}

impl Default for KernelLogCollector {
    fn default() -> Self {
        KernelLogCollector { max_messages: 20 }
    }
}

impl Collector for KernelLogCollector {
    fn name(&self) -> &'static str {
        "kernel_log"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let text = read_kmsg(root)?;
        snap.kernel_log.available = true;
        snap.kernel_log.notable = parse_kmsg(&text, self.max_messages);
        Ok(())
    }
}

fn read_kmsg(root: &Sysroot) -> Result<String, String> {
    let path = root.path("/dev/kmsg");
    let mut file =
        OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path).map_err(|e| match e.kind() {
            ErrorKind::PermissionDenied => "kernel log needs CAP_SYSLOG".to_string(),
            _ => format!("cannot open {}: {e}", path.display()),
        })?;
    // /dev/kmsg returns exactly one record per read(); a regular file (fixtures)
    // returns arbitrary chunks. Both are handled by accumulating and splitting lines.
    let mut out = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > 8 * 1024 * 1024 {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            // EPIPE: records were overwritten while reading; keep going.
            Err(e) if e.raw_os_error() == Some(libc::EPIPE) => continue,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("reading kernel log: {e}")),
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Keywords that make a warning (or lower) message worth the model's attention.
const WARNING_KEYWORDS: &[&str] = &[
    "firmware",
    "failed",
    "failure",
    "error",
    "timeout",
    "timed out",
    "not found",
    "unable",
    "cannot",
    "oops",
    "bug:",
    "call trace",
    "i/o error",
    "hung",
    "throttl",
    "out of memory",
    "killed process",
    "taint",
];

/// Even informational messages matching these are notable.
const INFO_KEYWORDS: &[&str] = &["segfault", "link is down", "out of memory", "killed process", "general protection"];

/// Parse kmsg records (`pri,seq,usec,flags;message`) keeping the most recent notable ones.
pub fn parse_kmsg(text: &str, max: usize) -> Vec<KernelMessage> {
    let mut out: Vec<KernelMessage> = Vec::new();
    for line in text.lines() {
        if line.starts_with(' ') {
            continue; // continuation dictionary lines ("SUBSYSTEM=...")
        }
        let Some((prefix, message)) = line.split_once(';') else { continue };
        let mut fields = prefix.split(',');
        let Some(level) = fields.next().and_then(|p| p.parse::<u32>().ok()).map(|p| (p & 7) as u8) else {
            continue;
        };
        let usec: u64 = fields.nth(1).and_then(|t| t.parse().ok()).unwrap_or(0);
        let message = unescape_kmsg(message.trim());
        let lower = message.to_lowercase();
        let notable = level <= 3
            || (level == 4 && WARNING_KEYWORDS.iter().any(|k| lower.contains(k)))
            || INFO_KEYWORDS.iter().any(|k| lower.contains(k));
        if !notable {
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|m| m.message == message) {
            existing.repeats += 1;
            existing.uptime_secs = usec as f64 / 1e6;
            continue;
        }
        out.push(KernelMessage { level, uptime_secs: usec as f64 / 1e6, message, repeats: 1 });
    }
    out.sort_by(|a, b| a.uptime_secs.total_cmp(&b.uptime_secs));
    if out.len() > max {
        out.drain(..out.len() - max);
    }
    out
}

/// kmsg escapes non-printable bytes as `\xNN`.
fn unescape_kmsg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("\\x") {
        out.push_str(&rest[..pos]);
        let hex = rest.get(pos + 2..pos + 4).and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) if b.is_ascii_graphic() || b == b' ' => out.push(b as char),
            Some(_) => out.push(' '),
            None => out.push_str("\\x"),
        }
        rest = &rest[pos + if hex.is_some() { 4 } else { 2 }..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
6,1,0,-;Linux version 6.10.2-arch1-1
6,2,1200,-;NET: Registered PF_INET6 protocol family
3,3,5140900,-;iwlwifi 0000:03:00.0: Direct firmware load for iwlwifi-QuZ-a0-hr-b0-77.ucode failed with error -2
 SUBSYSTEM=pci
 DEVICE=+pci:0000:03:00.0
4,4,5141000,-;iwlwifi 0000:03:00.0: no suitable firmware found!
4,5,6000000,-;ACPI: some harmless warning
6,6,7000000,-;firefox[2412]: segfault at 0 ip 00007f sp 00007ff error 4
3,7,8000000,-;iwlwifi 0000:03:00.0: Direct firmware load for iwlwifi-QuZ-a0-hr-b0-77.ucode failed with error -2
6,8,9000000,-;e1000e 0000:00:1f.6 eth0: NIC Link is Down
";

    #[test]
    fn keeps_notable_messages_and_collapses_repeats() {
        let msgs = parse_kmsg(SAMPLE, 20);
        let texts: Vec<&str> = msgs.iter().map(|m| m.message.as_str()).collect();
        assert_eq!(
            texts,
            [
                "iwlwifi 0000:03:00.0: no suitable firmware found!",
                "firefox[2412]: segfault at 0 ip 00007f sp 00007ff error 4",
                "iwlwifi 0000:03:00.0: Direct firmware load for iwlwifi-QuZ-a0-hr-b0-77.ucode failed with error -2",
                "e1000e 0000:00:1f.6 eth0: NIC Link is Down",
            ]
        );
        assert_eq!(msgs[2].repeats, 2);
        assert_eq!(msgs[2].level, 3);
        assert!((msgs[2].uptime_secs - 8.0).abs() < 1e-9);
    }

    #[test]
    fn caps_message_count() {
        assert_eq!(parse_kmsg(SAMPLE, 2).len(), 2);
    }

    #[test]
    fn unescapes() {
        assert_eq!(unescape_kmsg(r"a\x20b\x0ac"), "a b c");
        assert_eq!(unescape_kmsg(r"trailing\x"), r"trailing\x");
    }
}
