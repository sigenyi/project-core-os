//! Running external programs safely.
//!
//! * no shell: the program is an absolute path from the config and arguments are argv
//! * a cleared environment with a fixed `PATH`, locale and no colour/pager
//! * stdin is `/dev/null`; cwd is `/`
//! * each command gets its own process group, killed as a whole on timeout
//! * output is captured concurrently (no pipe deadlocks) and capped, keeping the head
//!   and the tail, which is where errors usually are

use std::collections::VecDeque;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// A command ready to execute: program resolved, privileges decided.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// Drop to this uid/gid before exec (None = stay root).
    pub credentials: Option<(u32, u32)>,
    pub max_output: usize,
    /// Human-readable form with secrets redacted.
    pub display: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    /// Set when the program could not be started at all.
    pub spawn_error: Option<String>,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, cmd: &ResolvedCommand) -> CommandOutput;
}

const BASE_ENV: &[(&str, &str)] = &[
    ("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
    ("LANG", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("TERM", "dumb"),
    ("NO_COLOR", "1"),
    ("SYSTEMD_COLORS", "0"),
    ("SYSTEMD_PAGER", ""),
    ("SYSTEMD_LESS", ""),
    ("PAGER", "cat"),
];

/// Executes commands for real.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, cmd: &ResolvedCommand) -> CommandOutput {
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .env_clear()
            .envs(BASE_ENV.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir("/")
            .process_group(0);
        if let Some((uid, gid)) = cmd.credentials {
            // std clears supplementary groups when dropping from root.
            command.uid(uid).gid(gid);
            command.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
            command.env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path=/run/user/{uid}/bus"));
        } else {
            command.env("HOME", "/root");
        }
        command.envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                return CommandOutput {
                    spawn_error: Some(format!("cannot start {}: {e}", cmd.display)),
                    ..Default::default()
                };
            }
        };
        let pid = child.id() as i32;
        let half = (cmd.max_output / 2).max(1);
        let out_reader = capture(child.stdout.take().expect("piped"), half);
        let err_reader = capture(child.stderr.take().expect("piped"), half);

        let deadline = Instant::now() + cmd.timeout;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    timed_out = true;
                    // SAFETY: kill(2) on the child's process group; the group was
                    // created by process_group(0) and contains only its descendants.
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    break child.wait().ok();
                }
                Ok(None) => thread::sleep(Duration::from_millis(15)),
                Err(_) => break None,
            }
        };
        // Descendants that kept the pipes open would block the readers forever.
        // SAFETY: as above; harmless if the group no longer exists.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let (stdout, t1) = out_reader.join().unwrap_or_default();
        let (stderr, t2) = err_reader.join().unwrap_or_default();
        let exit_code = status.and_then(|s| s.code().or_else(|| s.signal().map(|sig| 128 + sig)));
        CommandOutput {
            exit_code,
            stdout: strip_ansi(&stdout),
            stderr: strip_ansi(&stderr),
            truncated: t1 || t2,
            timed_out,
            spawn_error: None,
        }
    }
}

/// Read a stream to the end on a thread, keeping at most `half` bytes of head and of tail.
fn capture(mut stream: impl Read + Send + 'static, half: usize) -> thread::JoinHandle<(String, bool)> {
    thread::spawn(move || {
        let mut head = Vec::new();
        let mut tail: VecDeque<u8> = VecDeque::new();
        let mut dropped = 0usize;
        let mut buf = [0u8; 8192];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut chunk = &buf[..n];
            if head.len() < half {
                let take = (half - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
                chunk = &chunk[take..];
            }
            tail.extend(chunk);
            while tail.len() > half {
                tail.pop_front();
                dropped += 1;
            }
        }
        let mut text = String::from_utf8_lossy(&head).into_owned();
        if dropped > 0 {
            text.push_str(&format!("\n[... {dropped} bytes omitted ...]\n"));
        }
        text.push_str(&String::from_utf8_lossy(tail.make_contiguous()));
        (text, dropped > 0)
    })
}

/// Remove ANSI escape sequences that slipped past NO_COLOR.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            } else {
                chars.next();
            }
            continue;
        }
        if c == '\r' {
            continue;
        }
        out.push(c);
    }
    out
}

/// Test double: returns scripted outputs keyed by command prefix.
#[derive(Default)]
pub struct ScriptedRunner {
    pub log: Mutex<Vec<String>>,
    /// (display prefix, output). First match wins; unmatched commands succeed silently.
    pub script: Vec<(String, CommandOutput)>,
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, cmd: &ResolvedCommand) -> CommandOutput {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(cmd.display.clone());
        self.script
            .iter()
            .find(|(prefix, _)| cmd.display.starts_with(prefix.as_str()))
            .map(|(_, out)| out.clone())
            .unwrap_or(CommandOutput { exit_code: Some(0), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(program: &str, args: &[&str], timeout_ms: u64, max_output: usize) -> ResolvedCommand {
        ResolvedCommand {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: vec![],
            timeout: Duration::from_millis(timeout_ms),
            credentials: None,
            max_output,
            display: format!("{program} {}", args.join(" ")),
        }
    }

    #[test]
    fn captures_output_and_exit_code() {
        let out = SystemRunner.run(&cmd("/bin/sh", &["-c", "echo out; echo err >&2; exit 3"], 5000, 1024));
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.stdout, "out\n");
        assert_eq!(out.stderr, "err\n");
        assert!(!out.timed_out && !out.truncated);
    }

    #[test]
    fn environment_is_scrubbed() {
        // SAFETY: test-only; no other threads in this test read the environment.
        unsafe { std::env::set_var("CORE_TEST_SECRET", "leak") };
        let out = SystemRunner.run(&cmd("/usr/bin/env", &[], 5000, 4096));
        assert!(!out.stdout.contains("CORE_TEST_SECRET"));
        assert!(out.stdout.contains("LC_ALL=C.UTF-8"));
    }

    #[test]
    fn timeout_kills_the_whole_process_group() {
        let started = Instant::now();
        let out = SystemRunner.run(&cmd("/bin/sh", &["-c", "sleep 30 & sleep 30"], 200, 1024));
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5), "background child must not keep pipes open");
    }

    #[test]
    fn output_is_capped_keeping_head_and_tail() {
        let out = SystemRunner.run(&cmd("/bin/sh", &["-c", "echo START; seq 1 100000; echo END"], 10_000, 200));
        assert!(out.truncated);
        assert!(out.stdout.starts_with("START"));
        assert!(out.stdout.trim_end().ends_with("END"));
        assert!(out.stdout.contains("bytes omitted"));
        assert!(out.stdout.len() < 400);
    }

    #[test]
    fn missing_program_reports_spawn_error() {
        let out = SystemRunner.run(&cmd("/nonexistent/tool", &[], 1000, 100));
        assert!(out.spawn_error.unwrap().contains("cannot start"));
    }

    #[test]
    fn ansi_is_stripped() {
        assert_eq!(strip_ansi("\u{1b}[1;31mred\u{1b}[0m text\r\n"), "red text\n");
    }
}
