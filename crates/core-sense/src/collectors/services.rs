use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::Collector;
use crate::snapshot::Snapshot;
use crate::sysroot::Sysroot;

/// Failed systemd units.
pub enum ServicesCollector {
    /// Ask `systemctl` (live systems only).
    Systemctl { program: String, timeout: Duration },
    /// Fixed answer, for tests and non-systemd environments.
    Fixed(Vec<String>),
}

impl ServicesCollector {
    pub fn systemctl() -> Self {
        ServicesCollector::Systemctl { program: "systemctl".into(), timeout: Duration::from_secs(3) }
    }
}

impl Collector for ServicesCollector {
    fn name(&self) -> &'static str {
        "services"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let failed = match self {
            ServicesCollector::Fixed(units) => units.clone(),
            ServicesCollector::Systemctl { program, timeout } => {
                if !root.is_live() || !root.exists("/run/systemd/system") {
                    return Ok(()); // not booted with systemd: nothing to report
                }
                let out = run_with_timeout(
                    program,
                    &["list-units", "--state=failed", "--no-legend", "--plain", "--no-pager"],
                    *timeout,
                )?;
                parse_failed_units(&out)
            }
        };
        snap.services.checked = true;
        snap.services.failed = failed;
        Ok(())
    }
}

pub(crate) fn parse_failed_units(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim_start().trim_start_matches('●').split_whitespace().next())
        .map(String::from)
        .collect()
}

/// Run a short read-only probe, killing it if it exceeds `timeout`.
pub(crate) fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.by_ref().take(256 * 1024).read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} timed out"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    }
    reader.join().map_err(|_| "reader thread panicked".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_failed_units() {
        let text =
            "bluetooth.service loaded failed failed Bluetooth service\n● cups.service loaded failed failed CUPS\n";
        assert_eq!(parse_failed_units(text), ["bluetooth.service", "cups.service"]);
        assert!(parse_failed_units("").is_empty());
    }

    #[test]
    fn timeout_kills_slow_probe() {
        let err = run_with_timeout("sleep", &["5"], Duration::from_millis(100)).unwrap_err();
        assert!(err.contains("timed out"));
        assert_eq!(run_with_timeout("echo", &["hi"], Duration::from_secs(5)).unwrap(), "hi\n");
    }
}
