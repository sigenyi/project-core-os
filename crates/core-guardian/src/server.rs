//! Unix socket transport: accept connections, authenticate peers via the kernel,
//! and run one session per connection.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use core_protocol::wire::{Request, Response, read_frame, write_frame};

use crate::config::GuardianConfig;
use crate::service::{Guardian, Peer};

/// Who may connect: root, listed uids, and members of listed groups.
#[derive(Debug, Clone, Default)]
pub struct AccessControl {
    pub uids: Vec<u32>,
    pub gids: Vec<u32>,
}

impl AccessControl {
    pub fn from_config(config: &GuardianConfig) -> Self {
        let gids = config
            .allowed_groups
            .iter()
            .filter_map(|g| {
                let gid = group_id(g);
                if gid.is_none() {
                    log::warn!("allowed group {g:?} does not exist");
                }
                gid
            })
            .collect();
        AccessControl { uids: config.allowed_uids.clone(), gids }
    }

    pub fn permits(&self, peer: &Peer, groups: &[u32]) -> bool {
        peer.uid == 0
            || self.uids.contains(&peer.uid)
            || self.gids.contains(&peer.gid)
            || groups.iter().any(|g| self.gids.contains(g))
    }
}

pub fn group_id(name: &str) -> Option<u32> {
    let c = CString::new(name).ok()?;
    // SAFETY: getgrnam returns NULL or a pointer to static storage that we read
    // immediately; called during single-threaded startup.
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    (!gr.is_null()).then(|| unsafe { (*gr).gr_gid })
}

/// Kernel-verified credentials of the process on the other end of `stream`.
pub fn peer_credentials(stream: &UnixStream) -> io::Result<(Peer, Vec<u32>)> {
    let fd = stream.as_raw_fd();
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most `len` bytes into `cred`.
    let rc = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut groups = vec![0u32; 256];
    let mut glen = (groups.len() * std::mem::size_of::<u32>()) as libc::socklen_t;
    // SAFETY: as above; SO_PEERGROUPS fills an array of gid_t (u32 on Linux).
    let rc =
        unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERGROUPS, groups.as_mut_ptr().cast(), &mut glen) };
    if rc == 0 {
        groups.truncate(glen as usize / std::mem::size_of::<u32>());
    } else {
        groups.clear();
    }
    Ok((Peer { uid: cred.uid, gid: cred.gid, pid: cred.pid }, groups))
}

/// A listener passed by systemd socket activation (fd 3), if any.
fn systemd_listener() -> Option<UnixListener> {
    let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    let fds: i32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    if pid != std::process::id() || fds < 1 {
        return None;
    }
    // SAFETY: systemd guarantees fd 3 is the listening socket it passed us.
    Some(unsafe { UnixListener::from_raw_fd(3) })
}

/// Use the socket-activated listener or bind the configured path.
pub fn acquire_listener(config: &GuardianConfig) -> io::Result<UnixListener> {
    if let Some(listener) = systemd_listener() {
        log::info!("using socket passed by systemd");
        return Ok(listener);
    }
    bind_socket(&config.socket, config.socket_mode, config.socket_group.as_deref())
}

pub fn bind_socket(path: &Path, mode: u32, group: Option<&str>) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(_) => {}
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    if let Some(gid) = group.and_then(group_id) {
        std::os::unix::fs::chown(path, None, Some(gid))?;
    }
    log::info!("listening on {}", path.display());
    Ok(listener)
}

struct ConnectionSlot(Arc<AtomicUsize>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accept connections forever.
pub fn serve(guardian: Arc<Guardian>, listener: UnixListener, access: AccessControl) -> io::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    let max = guardian.config().max_connections.max(1);
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept failed: {e}");
                continue;
            }
        };
        let (peer, groups) = match peer_credentials(&stream) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("cannot read peer credentials: {e}");
                continue;
            }
        };
        if !access.permits(&peer, &groups) {
            log::warn!("refused connection from uid {} (pid {})", peer.uid, peer.pid);
            let _ = write_frame(&mut stream, &Response::Error { message: "not authorised".into() });
            continue;
        }
        if active.fetch_add(1, Ordering::SeqCst) >= max {
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = write_frame(&mut stream, &Response::Error { message: "too many connections".into() });
            continue;
        }
        let slot = ConnectionSlot(active.clone());
        let g = guardian.clone();
        thread::spawn(move || {
            let _slot = slot;
            log::debug!("session opened for uid {} pid {}", peer.uid, peer.pid);
            handle_connection(&g, stream, peer);
            log::debug!("session closed for pid {}", peer.pid);
        });
    }
    Ok(())
}

fn handle_connection(guardian: &Guardian, mut stream: UnixStream, peer: Peer) {
    let mut session = guardian.new_session(peer);
    loop {
        let request: Request = match read_frame(&mut stream) {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                let _ = write_frame(&mut stream, &Response::Error { message: format!("bad request: {e}") });
                return;
            }
        };
        let response = guardian.handle(&mut session, request);
        if write_frame(&mut stream, &response).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_control() {
        let acl = AccessControl { uids: vec![1001], gids: vec![50] };
        let p = |uid, gid| Peer { uid, gid, pid: 1 };
        assert!(acl.permits(&p(0, 0), &[]));
        assert!(acl.permits(&p(1001, 1001), &[]));
        assert!(acl.permits(&p(1002, 50), &[]));
        assert!(acl.permits(&p(1003, 1003), &[1003, 50]));
        assert!(!acl.permits(&p(1004, 1004), &[1004]));
    }

    #[test]
    fn group_lookup() {
        assert_eq!(group_id("root"), Some(0));
        assert_eq!(group_id("no-such-group-core"), None);
    }
}
