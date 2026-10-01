//! Execution plans: the concrete, fully-specified steps an action expands to.
//!
//! A plan is data. It names tools by key (resolved to absolute paths from the config),
//! carries arguments as discrete argv entries, and is produced by the [`crate::planner`]
//! from a typed action. No shell is ever involved.

use std::path::PathBuf;
use std::time::Duration;

use core_protocol::choice::Signal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAs {
    Root,
    /// The uid/gid of the connected client (for per-user services such as PipeWire).
    Peer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandSpec {
    /// Key into the `[tools]` table, never a path supplied by the model.
    pub tool: &'static str,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Overrides the default command timeout.
    pub timeout: Option<Duration>,
    pub run_as: RunAs,
    /// Exit codes that count as success (some tools report state via exit codes).
    pub success_codes: &'static [i32],
    /// Keep only the first / last N lines of output.
    pub head_lines: Option<usize>,
    pub tail_lines: Option<usize>,
    /// Failure neither aborts the plan nor fails the action.
    pub optional: bool,
    /// Indices of `args` that hold secrets (redacted in reports and the audit log).
    pub secret_args: Vec<usize>,
}

impl CommandSpec {
    pub fn new(tool: &'static str, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        CommandSpec {
            tool,
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
            timeout: None,
            run_as: RunAs::Root,
            success_codes: &[0],
            head_lines: None,
            tail_lines: None,
            optional: false,
            secret_args: Vec::new(),
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn secret_arg(mut self, a: impl Into<String>) -> Self {
        self.secret_args.push(self.args.len());
        self.args.push(a.into());
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }

    pub fn as_peer(mut self) -> Self {
        self.run_as = RunAs::Peer;
        self
    }

    pub fn success_codes(mut self, codes: &'static [i32]) -> Self {
        self.success_codes = codes;
        self
    }

    pub fn head(mut self, n: usize) -> Self {
        self.head_lines = Some(n);
        self
    }

    pub fn tail(mut self, n: usize) -> Self {
        self.tail_lines = Some(n);
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// `tool arg1 arg2`, with secrets replaced.
    pub fn describe(&self) -> String {
        let mut parts = vec![self.tool.to_string()];
        for (i, a) in self.args.iter().enumerate() {
            if self.secret_args.contains(&i) {
                parts.push("<redacted>".into());
            } else if a.is_empty() || a.contains(char::is_whitespace) {
                parts.push(format!("'{a}'"));
            } else {
                parts.push(a.clone());
            }
        }
        parts.join(" ")
    }
}

/// Operations implemented directly in Rust instead of by running a program.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeOp {
    ListDir {
        path: PathBuf,
    },
    ReadFile {
        path: PathBuf,
        lines: usize,
        tail: bool,
    },
    SetBrightness {
        percent: u32,
    },
    Signal {
        pid: u32,
        signal: Signal,
    },
    /// Delete the configured swap file (never an arbitrary path).
    RemoveSwapFile,
    /// Ensure the swap file line is present in / absent from fstab.
    FstabSwap {
        present: bool,
    },
}

impl NativeOp {
    pub fn describe(&self) -> String {
        match self {
            NativeOp::ListDir { path } => format!("list {}", path.display()),
            NativeOp::ReadFile { path, lines, tail } => {
                format!("read {} lines of {}{}", lines, path.display(), if *tail { " (from the end)" } else { "" })
            }
            NativeOp::SetBrightness { percent } => format!("set backlight to {percent}%"),
            NativeOp::Signal { pid, signal } => format!("send SIG{} to {pid}", signal.as_str().to_uppercase()),
            NativeOp::RemoveSwapFile => "remove swap file".into(),
            NativeOp::FstabSwap { present: true } => "add swap file to /etc/fstab".into(),
            NativeOp::FstabSwap { present: false } => "remove swap file from /etc/fstab".into(),
        }
    }

    /// Whether the operation changes system state (skipped in dry-run mode).
    pub fn is_mutating(&self) -> bool {
        !matches!(self, NativeOp::ListDir { .. } | NativeOp::ReadFile { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Run(CommandSpec),
    Native(NativeOp),
}

impl Step {
    pub fn describe(&self) -> String {
        match self {
            Step::Run(c) => c.describe(),
            Step::Native(n) => n.describe(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn single(step: Step) -> Self {
        Plan { steps: vec![step] }
    }

    pub fn run(cmd: CommandSpec) -> Self {
        Plan::single(Step::Run(cmd))
    }

    pub fn native(op: NativeOp) -> Self {
        Plan::single(Step::Native(op))
    }

    pub fn then(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    pub fn then_run(self, cmd: CommandSpec) -> Self {
        self.then(Step::Run(cmd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_redacts_secrets_and_quotes_spaces() {
        let c =
            CommandSpec::new("nmcli", ["device", "wifi", "connect", "Home Net"]).arg("password").secret_arg("hunter22");
        assert_eq!(c.describe(), "nmcli device wifi connect 'Home Net' password <redacted>");
    }
}
