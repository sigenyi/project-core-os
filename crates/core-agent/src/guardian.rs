//! Talking to the Guardian.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use core_protocol::wire::{Capability, Request, Response, read_frame, write_frame};
use core_protocol::{Intent, PROTOCOL_VERSION};

/// The agent's view of the privileged executor. Implemented over the Unix socket in
/// production and in-process (dry-run) for development.
pub trait GuardianClient: Send {
    fn describe(&self) -> String;
    fn capabilities(&mut self) -> io::Result<Vec<Capability>>;
    fn execute(&mut self, id: u64, intent: &Intent) -> io::Result<Response>;
    fn confirm(&mut self, token: &str, approve: bool) -> io::Result<Response>;
}

pub struct SocketGuardian {
    path: PathBuf,
    stream: Option<UnixStream>,
}

impl SocketGuardian {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SocketGuardian { path: path.into(), stream: None }
    }

    fn connect(&mut self) -> io::Result<&mut UnixStream> {
        if self.stream.is_none() {
            let mut s = UnixStream::connect(&self.path).map_err(|e| {
                io::Error::new(e.kind(), format!("cannot reach the Guardian at {}: {e}", self.path.display()))
            })?;
            // Long enough for a large package installation; never wait forever.
            s.set_read_timeout(Some(Duration::from_secs(3600)))?;
            write_frame(
                &mut s,
                &Request::Hello {
                    client: format!("core-agent {}", env!("CARGO_PKG_VERSION")),
                    protocol: PROTOCOL_VERSION,
                },
            )?;
            match read_frame::<_, Response>(&mut s)? {
                Some(Response::Hello { protocol, .. }) if protocol == PROTOCOL_VERSION => {}
                Some(Response::Hello { protocol, .. }) => {
                    return Err(io::Error::other(format!(
                        "Guardian speaks protocol {protocol}, expected {PROTOCOL_VERSION}"
                    )));
                }
                Some(Response::Error { message }) => {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
                }
                other => return Err(io::Error::other(format!("unexpected handshake reply: {other:?}"))),
            }
            self.stream = Some(s);
        }
        Ok(self.stream.as_mut().expect("just connected"))
    }

    /// Send a request and read the reply. On error, says whether the request may have
    /// reached the Guardian (in which case it must not be sent again).
    fn try_call(&mut self, request: &Request) -> Result<Response, (io::Error, bool)> {
        let s = self.connect().map_err(|e| (e, false))?;
        // A write to a connection the Guardian already closed fails with EPIPE, so a
        // failed write means the request was not delivered.
        write_frame(s, request).map_err(|e| (e, false))?;
        match read_frame(s) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err((io::Error::new(io::ErrorKind::UnexpectedEof, "the Guardian closed the connection"), true)),
            Err(e) => Err((e, true)),
        }
    }

    fn call(&mut self, request: &Request) -> io::Result<Response> {
        match self.try_call(request) {
            Ok(r) => Ok(r),
            Err((e, delivered)) => {
                self.stream = None;
                // Retry once if the connection was stale (e.g. the Guardian restarted) and
                // the request never arrived. Never after delivery: the action may have
                // run. Never for confirmations: tokens die with their connection.
                if delivered || matches!(request, Request::Confirm { .. }) {
                    return Err(e);
                }
                self.try_call(request).map_err(|(e, _)| e)
            }
        }
    }
}

impl GuardianClient for SocketGuardian {
    fn describe(&self) -> String {
        format!("Guardian at {}", self.path.display())
    }

    fn capabilities(&mut self) -> io::Result<Vec<Capability>> {
        match self.call(&Request::Capabilities)? {
            Response::Capabilities { actions } => Ok(actions),
            other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
        }
    }

    fn execute(&mut self, id: u64, intent: &Intent) -> io::Result<Response> {
        self.call(&Request::Execute { id, intent: intent.clone() })
    }

    fn confirm(&mut self, token: &str, approve: bool) -> io::Result<Response> {
        self.call(&Request::Confirm { token: token.to_string(), approve })
    }
}
