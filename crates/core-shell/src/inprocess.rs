//! Development mode: run the Guardian inside the shell, in dry-run mode.
//!
//! Lets anyone try C.O.R.E. on an ordinary Linux machine without root, a socket or a
//! systemd unit. Read-only actions run for real (as the current user); everything
//! that would change the system is simulated. The same policy, planner and
//! confirmation flow apply, so behaviour matches production.

use std::io;

use core_agent::guardian::GuardianClient;
use core_guardian::audit::AuditLog;
use core_guardian::{Guardian, GuardianConfig, Peer, Session, live_guardian};
use core_protocol::Intent;
use core_protocol::wire::{Capability, Request, Response};

pub struct InProcessGuardian {
    guardian: Guardian,
    session: Session,
}

impl InProcessGuardian {
    pub fn dry_run() -> Self {
        let config = GuardianConfig { dry_run: true, ..GuardianConfig::default() };
        let guardian = live_guardian(config, AuditLog::disabled());
        // SAFETY: getuid/getgid have no preconditions.
        let peer =
            Peer { uid: unsafe { libc::getuid() }, gid: unsafe { libc::getgid() }, pid: std::process::id() as i32 };
        let session = guardian.new_session(peer);
        InProcessGuardian { guardian, session }
    }
}

impl GuardianClient for InProcessGuardian {
    fn describe(&self) -> String {
        "in-process Guardian (dry run: changes are simulated)".into()
    }

    fn capabilities(&mut self) -> io::Result<Vec<Capability>> {
        match self.guardian.handle(&mut self.session, Request::Capabilities) {
            Response::Capabilities { actions } => Ok(actions),
            other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
        }
    }

    fn execute(&mut self, id: u64, intent: &Intent) -> io::Result<Response> {
        Ok(self.guardian.handle(&mut self.session, Request::Execute { id, intent: intent.clone() }))
    }

    fn confirm(&mut self, token: &str, approve: bool) -> io::Result<Response> {
        Ok(self.guardian.handle(&mut self.session, Request::Confirm { token: token.into(), approve }))
    }
}
