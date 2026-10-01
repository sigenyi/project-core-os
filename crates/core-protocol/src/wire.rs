//! Agent ⇄ Guardian wire protocol.
//!
//! Frames are a 4-byte big-endian length followed by a UTF-8 JSON document, carried
//! over a Unix stream socket. The Guardian never trusts the client's validation: an
//! `Execute` request carries the raw [`Intent`] and is validated again server-side.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::intent::Intent;
use crate::risk::Risk;

/// Upper bound on a single frame (1 MiB).
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello { client: String, protocol: u32 },
    /// Validate, authorise and (if allowed) execute an intent.
    Execute { id: u64, intent: Intent },
    /// Answer a pending confirmation. Must come from the same connection.
    Confirm { token: String, approve: bool },
    /// Which actions this Guardian will accept, with effective risk/confirmation.
    Capabilities,
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello { server: String, protocol: u32, dry_run: bool },
    Executed { id: u64, report: ExecutionReport },
    /// The action is allowed only with explicit human approval.
    ConfirmationRequired { id: u64, token: String, summary: String, risk: Risk, expires_in_secs: u64 },
    Rejected { id: u64, kind: RejectKind, reason: String },
    Capabilities { actions: Vec<Capability> },
    Pong,
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectKind {
    /// The intent failed validation (unknown action, bad argument).
    Invalid,
    /// Policy forbids it (disabled action, protected service, denied path).
    Denied,
    /// The user declined the confirmation.
    Declined,
    /// Unknown or expired confirmation token.
    Expired,
    RateLimited,
    /// This action is not executed by the Guardian (e.g. `respond`).
    NotPrivileged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub action: String,
    pub risk: Risk,
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub action: String,
    pub success: bool,
    pub steps: Vec<StepReport>,
    pub duration_ms: u64,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    /// What was done, e.g. `systemctl restart bluetooth.service`.
    pub description: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}

impl ExecutionReport {
    /// The first failing step, if any.
    pub fn failure(&self) -> Option<&StepReport> {
        self.steps.iter().find(|s| !s.success)
    }

    /// All output concatenated, step by step, for display or model observation.
    pub fn combined_output(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            if self.steps.len() > 1 {
                out.push_str(&format!("$ {}\n", step.description));
            }
            if !step.stdout.is_empty() {
                out.push_str(step.stdout.trim_end());
                out.push('\n');
            }
            if !step.stderr.is_empty() {
                out.push_str(step.stderr.trim_end());
                out.push('\n');
            }
            if step.timed_out {
                out.push_str("[timed out]\n");
            }
            if step.truncated {
                out.push_str("[output truncated]\n");
            }
        }
        out
    }
}

/// Write one length-prefixed JSON frame.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    let len = u32::try_from(body.len()).expect("MAX_FRAME fits in u32");
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()
}

/// Read one frame. Returns `Ok(None)` on a clean end-of-stream before a frame starts.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes exceeds limit")));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::json;

    use super::*;

    #[test]
    fn round_trip_frames() {
        let mut buf = Vec::new();
        let req = Request::Execute { id: 7, intent: Intent::new("disk_usage", json!({})) };
        write_frame(&mut buf, &req).unwrap();
        write_frame(&mut buf, &Request::Ping).unwrap();
        let mut cur = Cursor::new(buf);
        assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), Some(req));
        assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), Some(Request::Ping));
        assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), None);
    }

    #[test]
    fn oversized_frames_are_refused() {
        let mut cur = Cursor::new(((MAX_FRAME + 1) as u32).to_be_bytes().to_vec());
        assert!(read_frame::<_, Request>(&mut cur).is_err());
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Request::Ping).unwrap();
        buf.pop();
        assert!(read_frame::<_, Request>(&mut Cursor::new(buf)).is_err());
    }

    #[test]
    fn json_shape_is_tagged() {
        let v = serde_json::to_value(Response::Rejected { id: 1, kind: RejectKind::Denied, reason: "no".into() }).unwrap();
        assert_eq!(v, json!({"type": "rejected", "id": 1, "kind": "denied", "reason": "no"}));
    }

    #[test]
    fn combined_output_formats_steps() {
        let report = ExecutionReport {
            action: "configure_swap".into(),
            success: false,
            steps: vec![
                StepReport { description: "fallocate".into(), success: true, ..Default::default() },
                StepReport {
                    description: "mkswap".into(),
                    success: false,
                    exit_code: Some(1),
                    stderr: "mkswap: error\n".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(report.failure().unwrap().description, "mkswap");
        assert_eq!(report.combined_output(), "$ fallocate\n$ mkswap\nmkswap: error\n");
    }
}
