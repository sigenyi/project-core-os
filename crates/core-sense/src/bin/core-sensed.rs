//! core-sensed: the C.O.R.E. telemetry daemon.
//!
//! Periodically snapshots the machine and publishes it atomically to
//! `/run/core/telemetry.json` for the agent. `--once` prints a single snapshot,
//! which is handy for debugging perception on any Linux machine.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use core_protocol::DEFAULT_TELEMETRY_PATH;
use core_sense::{Sensor, Sysroot, summary, write_snapshot};

#[derive(Parser)]
#[command(name = "core-sensed", version, about = "C.O.R.E. telemetry daemon")]
struct Cli {
    /// Take one snapshot, print it to stdout and exit.
    #[arg(long)]
    once: bool,
    /// With --once: print the compact model-facing summary instead of JSON.
    #[arg(long, requires = "once")]
    summary: bool,
    /// Pretty-print JSON.
    #[arg(long)]
    pretty: bool,
    /// Where the daemon publishes snapshots.
    #[arg(long, default_value = DEFAULT_TELEMETRY_PATH)]
    output: PathBuf,
    /// Seconds between snapshots.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
    interval: u64,
    /// Read /proc, /sys, ... below this directory instead of / (for testing).
    #[arg(long, default_value = "/")]
    sysroot: PathBuf,
    /// Increase log verbosity (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() -> ExitCode {
    // Behave like a normal Unix tool when piped into `head`: exit quietly on EPIPE.
    // SAFETY: restoring the default disposition of SIGPIPE before any threads exist.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    core_protocol::logging::init(cli.verbose);
    let sensor = Sensor::new(Sysroot::at(&cli.sysroot));

    if cli.once {
        let snap = sensor.snapshot();
        let text = if cli.summary {
            summary(&snap)
        } else if cli.pretty {
            serde_json::to_string_pretty(&snap).expect("snapshot serialises")
        } else {
            serde_json::to_string(&snap).expect("snapshot serialises")
        };
        println!("{text}");
        return ExitCode::SUCCESS;
    }

    log::info!("publishing telemetry to {} every {}s", cli.output.display(), cli.interval);
    let mut known: HashSet<String> = HashSet::new();
    loop {
        let snap = sensor.snapshot();
        // Log newly appearing problems once, so the journal tells the story too.
        let current: HashSet<String> =
            snap.insights.iter().map(|i| format!("{}: {}", i.subsystem, i.message)).collect();
        for new in current.difference(&known) {
            log::warn!("detected: {new}");
        }
        for gone in known.difference(&current) {
            log::info!("resolved: {gone}");
        }
        known = current;
        if let Err(e) = write_snapshot(&cli.output, &snap, cli.pretty) {
            log::error!("cannot write {}: {e}", cli.output.display());
        }
        std::thread::sleep(Duration::from_secs(cli.interval));
    }
}
