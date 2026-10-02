//! core-guardian: the privileged executor daemon.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use core_guardian::audit::AuditLog;
use core_guardian::policy::Policy;
use core_guardian::server::{AccessControl, acquire_listener, serve};
use core_guardian::{GuardianConfig, live_guardian};

#[derive(Parser)]
#[command(name = "core-guardian", version, about = "C.O.R.E. privileged executor")]
struct Cli {
    /// Configuration file.
    #[arg(long, default_value = "/etc/core/guardian.toml")]
    config: PathBuf,
    /// Never change the system; read-only actions still run.
    #[arg(long)]
    dry_run: bool,
    /// Override the socket path.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Validate the configuration, report missing tools and exit.
    #[arg(long)]
    check: bool,
    /// Increase log verbosity (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    core_protocol::logging::init(cli.verbose.max(1));
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// The policy file decides what root does on the model's behalf; it must not be
/// writable by anyone but root.
fn check_config_ownership(path: &Path) -> Result<(), String> {
    let Ok(meta) = std::fs::metadata(path) else { return Ok(()) };
    if meta.uid() != 0 {
        return Err(format!("{} must be owned by root", path.display()));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!("{} must not be writable by group or others", path.display()));
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), String> {
    let mut config = if cli.config.exists() {
        GuardianConfig::load(&cli.config)?
    } else {
        log::warn!("{} not found; using built-in defaults", cli.config.display());
        GuardianConfig::default()
    };
    config.dry_run |= cli.dry_run;
    if let Some(socket) = cli.socket {
        config.socket = socket;
    }

    if cli.check {
        return check(&config);
    }
    if is_root() {
        check_config_ownership(&cli.config)?;
    } else if !config.dry_run {
        return Err("core-guardian must run as root (or with --dry-run for development)".into());
    }

    let audit = match AuditLog::open(&config.audit_log) {
        Ok(a) => a,
        Err(e) if config.dry_run => {
            log::warn!(
                "audit log {} unavailable ({e}); continuing without it in dry-run mode",
                config.audit_log.display()
            );
            AuditLog::disabled()
        }
        Err(e) => return Err(format!("cannot open audit log {}: {e}", config.audit_log.display())),
    };
    if config.dry_run {
        log::warn!("DRY RUN: system changes will be simulated");
    }
    let access = AccessControl::from_config(&config);
    let listener =
        acquire_listener(&config).map_err(|e| format!("cannot listen on {}: {e}", config.socket.display()))?;
    let guardian = Arc::new(live_guardian(config, audit));
    serve(guardian, listener, access).map_err(|e| e.to_string())
}

fn check(config: &GuardianConfig) -> Result<(), String> {
    println!("socket:        {}", config.socket.display());
    println!("audit log:     {}", config.audit_log.display());
    println!("auto-approve:  up to {} risk", config.auto_approve);
    println!("package mgr:   {:?}", config.system.package_manager);
    println!("audio:         {:?}", config.system.audio);
    println!("network:       {:?}", config.system.network);
    println!("dry run:       {}", config.dry_run);
    let missing: Vec<String> =
        config.tools.iter().filter(|(_, p)| !p.exists()).map(|(name, p)| format!("{name} ({})", p.display())).collect();
    println!("tools missing: {}", if missing.is_empty() { "none".into() } else { missing.join(", ") });
    let caps = Policy::new(config).capabilities();
    let confirm: Vec<&str> = caps.iter().filter(|c| c.requires_confirmation).map(|c| c.action.as_str()).collect();
    println!("actions:       {} enabled, {} need confirmation ({})", caps.len(), confirm.len(), confirm.join(", "));
    Ok(())
}
