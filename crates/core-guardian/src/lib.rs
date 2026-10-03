//! The Guardian: C.O.R.E.'s privileged executor.
//!
//! The language model is treated as untrusted. It cannot type commands; it can only
//! emit an intent naming one catalogued action. The Guardian is the only component
//! running as root, and it:
//!
//! 1. authenticates the client with kernel peer credentials,
//! 2. re-validates the intent into a typed action ([`core_protocol::ValidatedAction`]),
//! 3. applies [`policy`] (disabled actions, protected services/paths, risk ceilings),
//! 4. parks risky actions until a human confirms them ([`confirm`]),
//! 5. expands the action into a fixed [`plan`] for this distribution ([`planner`]),
//! 6. runs it without a shell, with timeouts and output caps ([`runner`]),
//! 7. records everything in an append-only [`audit`] log.

pub mod audit;
pub mod config;
pub mod confirm;
pub mod executor;
pub mod native;
pub mod plan;
pub mod planner;
pub mod policy;
pub mod preview;
pub mod runner;
pub mod server;
pub mod service;

pub use config::GuardianConfig;
pub use service::{Guardian, Peer, Session};

use planner::LiveProbe;
use runner::SystemRunner;

/// A Guardian wired to the real system.
pub fn live_guardian(config: GuardianConfig, audit: audit::AuditLog) -> Guardian {
    let probe = LiveProbe {
        sysfs: config.native.sysfs.clone(),
        procfs: config.native.procfs.clone(),
        systemctl: config.tools.get("systemctl").cloned(),
    };
    Guardian::new(config, Box::new(SystemRunner), Box::new(probe), audit)
}
