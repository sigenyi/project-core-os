//! The Guardian over a real Unix socket: kernel peer credentials, framing, sessions.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use core_guardian::audit::AuditLog;
use core_guardian::planner::SystemProbe;
use core_guardian::runner::ScriptedRunner;
use core_guardian::server::{AccessControl, bind_socket, serve};
use core_guardian::{Guardian, GuardianConfig};
use core_protocol::wire::{RejectKind, Request, Response, read_frame, write_frame};
use core_protocol::{Intent, PROTOCOL_VERSION};
use serde_json::json;

struct Probe;
impl SystemProbe for Probe {
    fn wireless_interfaces(&self) -> Vec<String> {
        vec![]
    }
    fn filesystem_type(&self, _: &Path) -> Option<String> {
        None
    }
}

fn start(dir: &Path, audit: &Path) -> std::path::PathBuf {
    let sock = dir.join("guardian.sock");
    let mut config = GuardianConfig::default();
    for p in config.tools.values_mut() {
        *p = "/bin/true".into();
    }
    // SAFETY: getuid has no preconditions.
    let me = unsafe { libc::getuid() };
    let listener = bind_socket(&sock, 0o600, None).unwrap();
    let guardian = Arc::new(Guardian::new(
        config,
        Box::new(ScriptedRunner::default()),
        Box::new(Probe),
        AuditLog::open(audit).unwrap(),
    ));
    let access = AccessControl { uids: vec![me], gids: vec![] };
    thread::spawn(move || serve(guardian, listener, access));
    sock
}

fn call(stream: &mut UnixStream, req: &Request) -> Response {
    write_frame(stream, req).unwrap();
    read_frame(stream).unwrap().expect("response")
}

#[test]
fn full_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let audit = dir.path().join("audit.jsonl");
    let sock = start(dir.path(), &audit);
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    let Response::Hello { protocol, dry_run, .. } =
        call(&mut s, &Request::Hello { client: "test".into(), protocol: PROTOCOL_VERSION })
    else {
        panic!()
    };
    assert_eq!(protocol, PROTOCOL_VERSION);
    assert!(!dry_run);

    let Response::Capabilities { actions } = call(&mut s, &Request::Capabilities) else { panic!() };
    assert!(actions.iter().any(|a| a.action == "install_package" && a.requires_confirmation));

    let r =
        call(&mut s, &Request::Execute { id: 1, intent: Intent::new("restart_service", json!({"service": "iwd"})) });
    let Response::Executed { id: 1, report } = r else { panic!("{r:?}") };
    assert!(report.success);

    let r = call(&mut s, &Request::Execute { id: 2, intent: Intent::new("remove_package", json!({"package": "w3m"})) });
    let Response::ConfirmationRequired { id: 2, token, .. } = r else { panic!("{r:?}") };
    let r = call(&mut s, &Request::Confirm { token, approve: true });
    assert!(matches!(r, Response::Executed { id: 2, .. }), "{r:?}");

    let r = call(&mut s, &Request::Execute { id: 3, intent: Intent::new("restart_service", json!({"service": "-x"})) });
    assert!(matches!(r, Response::Rejected { id: 3, kind: RejectKind::Invalid, .. }), "{r:?}");

    // The audit log saw everything, with the peer identified by the kernel.
    drop(s);
    thread::sleep(Duration::from_millis(50));
    let log = std::fs::read_to_string(&audit).unwrap();
    let entries: Vec<serde_json::Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let decisions: Vec<&str> = entries.iter().map(|e| e["decision"].as_str().unwrap()).collect();
    assert_eq!(decisions, ["allowed", "confirmation_required", "confirmed", "invalid"]);
    assert_eq!(entries[0]["peer"]["pid"], json!(std::process::id()));
}

#[test]
fn garbage_frames_end_the_session_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start(dir.path(), &dir.path().join("audit.jsonl"));
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    use std::io::Write;
    s.write_all(&5u32.to_be_bytes()).unwrap();
    s.write_all(b"nope!").unwrap();
    let r: Response = read_frame(&mut s).unwrap().unwrap();
    assert!(matches!(r, Response::Error { .. }));
    // A fresh connection still works.
    let mut s2 = UnixStream::connect(&sock).unwrap();
    assert_eq!(call(&mut s2, &Request::Ping), Response::Pong);
}
