//! Carry out a plan step by step and report what happened.

use std::time::{Duration, Instant};

use core_protocol::wire::{ExecutionReport, StepReport};

use crate::config::GuardianConfig;
use crate::native;
use crate::plan::{CommandSpec, Plan, RunAs, Step};
use crate::runner::{CommandRunner, ResolvedCommand};

pub struct Executor<'a> {
    pub config: &'a GuardianConfig,
    pub runner: &'a dyn CommandRunner,
    /// Credentials of the requesting client, for steps that run as the user.
    pub peer: Option<(u32, u32)>,
    /// Simulate instead of executing (read-only plans still run for real).
    pub simulate: bool,
}

impl Executor<'_> {
    pub fn execute(&self, action: &str, plan: &Plan) -> ExecutionReport {
        let started = Instant::now();
        let mut steps = Vec::with_capacity(plan.steps.len());
        let mut success = true;
        for step in &plan.steps {
            let report = match step {
                Step::Run(cmd) if self.simulate => simulated(cmd.describe()),
                Step::Run(cmd) => self.run_command(cmd),
                Step::Native(op) if self.simulate && op.is_mutating() => simulated(op.describe()),
                Step::Native(op) => match native::execute(op, self.config) {
                    Ok(stdout) => {
                        StepReport { description: op.describe(), success: true, stdout, ..Default::default() }
                    }
                    Err(stderr) => {
                        StepReport { description: op.describe(), success: false, stderr, ..Default::default() }
                    }
                },
            };
            let optional = matches!(step, Step::Run(c) if c.optional);
            let failed = !report.success;
            steps.push(report);
            if failed && !optional {
                success = false;
                break;
            }
        }
        ExecutionReport {
            action: action.to_string(),
            success,
            steps,
            duration_ms: started.elapsed().as_millis() as u64,
            dry_run: self.simulate,
        }
    }

    fn run_command(&self, spec: &CommandSpec) -> StepReport {
        let description = spec.describe();
        let fail = |stderr: String| StepReport {
            description: description.clone(),
            success: false,
            stderr,
            ..Default::default()
        };
        let Some(program) = self.config.tools.get(spec.tool) else {
            return fail(format!("{} is not configured", spec.tool));
        };
        if !program.exists() {
            return fail(format!("{} is not installed (expected at {})", spec.tool, program.display()));
        }
        let credentials = match spec.run_as {
            RunAs::Root => None,
            RunAs::Peer => match self.peer {
                Some(c) => Some(c),
                None => return fail("this step must run as the requesting user, who is unknown".into()),
            },
        };
        let cmd = ResolvedCommand {
            program: program.clone(),
            args: spec.args.clone(),
            env: spec.env.clone(),
            timeout: spec.timeout.unwrap_or(Duration::from_secs(self.config.command_timeout_secs)),
            credentials,
            max_output: self.config.max_output_bytes,
            display: description.clone(),
        };
        let out = self.runner.run(&cmd);
        if let Some(err) = out.spawn_error {
            return fail(err);
        }
        let (stdout, cut) = limit_lines(&out.stdout, spec.head_lines, spec.tail_lines);
        let success = !out.timed_out && out.exit_code.is_some_and(|c| spec.success_codes.contains(&c));
        StepReport {
            description,
            success,
            exit_code: out.exit_code,
            stdout,
            stderr: out.stderr,
            truncated: out.truncated || cut,
            timed_out: out.timed_out,
        }
    }
}

fn simulated(description: String) -> StepReport {
    StepReport {
        stdout: format!("[dry-run] would run: {description}"),
        description,
        success: true,
        exit_code: Some(0),
        ..Default::default()
    }
}

/// Keep the first `head` and/or last `tail` lines. Returns whether anything was cut.
pub fn limit_lines(text: &str, head: Option<usize>, tail: Option<usize>) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: &[&str] = &lines;
    if let Some(h) = head {
        kept = &kept[..h.min(kept.len())];
    }
    if let Some(t) = tail {
        kept = &kept[kept.len().saturating_sub(t)..];
    }
    if kept.len() == lines.len() {
        return (text.to_string(), false);
    }
    let mut out = kept.join("\n");
    out.push('\n');
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::NativeOp;
    use crate::runner::{CommandOutput, ScriptedRunner};

    fn config() -> GuardianConfig {
        let mut c = GuardianConfig::default();
        // Point every tool at a binary that exists on any Linux box.
        for path in c.tools.values_mut() {
            *path = "/bin/true".into();
        }
        c
    }

    #[test]
    fn stops_at_first_required_failure() {
        let c = config();
        let runner = ScriptedRunner {
            script: vec![(
                "mkswap".into(),
                CommandOutput { exit_code: Some(1), stderr: "mkswap: bad".into(), ..Default::default() },
            )],
            ..Default::default()
        };
        let plan = Plan::run(CommandSpec::new("swapoff", ["/swapfile"]).optional())
            .then_run(CommandSpec::new("mkswap", ["/swapfile"]))
            .then_run(CommandSpec::new("swapon", ["/swapfile"]));
        let ex = Executor { config: &c, runner: &runner, peer: None, simulate: false };
        let report = ex.execute("configure_swap", &plan);
        assert!(!report.success);
        assert_eq!(report.steps.len(), 2);
        assert_eq!(report.failure().unwrap().stderr, "mkswap: bad");
        assert_eq!(runner.log.lock().unwrap().len(), 2, "swapon never ran");
    }

    #[test]
    fn optional_failures_do_not_fail_the_action() {
        let c = config();
        let runner = ScriptedRunner {
            script: vec![("swapoff".into(), CommandOutput { exit_code: Some(255), ..Default::default() })],
            ..Default::default()
        };
        let plan = Plan::run(CommandSpec::new("swapoff", ["/swapfile"]).optional())
            .then_run(CommandSpec::new("swapon", ["/x"]));
        let report = Executor { config: &c, runner: &runner, peer: None, simulate: false }.execute("x", &plan);
        assert!(report.success);
        assert!(!report.steps[0].success);
    }

    #[test]
    fn simulation_runs_nothing() {
        let c = config();
        let runner = ScriptedRunner::default();
        let plan = Plan::run(CommandSpec::new("pacman", ["-S", "w3m"]))
            .then(Step::Native(NativeOp::FstabSwap { present: true }));
        let report = Executor { config: &c, runner: &runner, peer: None, simulate: true }.execute("x", &plan);
        assert!(report.success && report.dry_run);
        assert!(runner.log.lock().unwrap().is_empty());
        assert_eq!(report.steps[0].stdout, "[dry-run] would run: pacman -S w3m");
    }

    #[test]
    fn success_codes_and_missing_tools() {
        let mut c = config();
        let runner = ScriptedRunner {
            script: vec![(
                "pacman -Qi".into(),
                CommandOutput {
                    exit_code: Some(1),
                    stderr: "error: package 'x' was not found".into(),
                    ..Default::default()
                },
            )],
            ..Default::default()
        };
        let ex = Executor { config: &c, runner: &runner, peer: None, simulate: false };
        let report =
            ex.execute("package_info", &Plan::run(CommandSpec::new("pacman", ["-Qi", "x"]).success_codes(&[0, 1])));
        assert!(report.success, "exit 1 = not installed, a valid answer");

        c.tools.insert("pacman".into(), "/nonexistent/pacman".into());
        let ex = Executor { config: &c, runner: &runner, peer: None, simulate: false };
        let report = ex.execute("x", &Plan::run(CommandSpec::new("pacman", ["-Qi", "x"])));
        assert!(report.steps[0].stderr.contains("not installed"));
    }

    #[test]
    fn peer_steps_need_a_peer() {
        let c = config();
        let runner = ScriptedRunner::default();
        let ex = Executor { config: &c, runner: &runner, peer: None, simulate: false };
        let report = ex.execute("set_volume", &Plan::run(CommandSpec::new("wpctl", ["x"]).as_peer()));
        assert!(!report.success);
    }

    #[test]
    fn line_limits() {
        assert_eq!(limit_lines("a\nb\nc\n", Some(2), None), ("a\nb\n".into(), true));
        assert_eq!(limit_lines("a\nb\nc\n", None, Some(1)), ("c\n".into(), true));
        assert_eq!(limit_lines("a\nb\n", Some(5), None), ("a\nb\n".into(), false));
    }
}
