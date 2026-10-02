//! Append-only JSON-lines audit log of every request and its outcome.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;

pub struct AuditLog {
    file: Mutex<Option<File>>,
}

impl AuditLog {
    /// Open (creating if needed) with owner-only permissions.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::DirBuilder::new().recursive(true).mode(0o750).create(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
        Ok(AuditLog { file: Mutex::new(Some(file)) })
    }

    /// A log that discards entries (tests, dev mode without a writable log).
    pub fn disabled() -> Self {
        AuditLog { file: Mutex::new(None) }
    }

    pub fn record(&self, mut entry: Value) {
        if let Value::Object(map) = &mut entry {
            map.insert("ts".into(), Value::String(core_protocol::time::now_rfc3339()));
        }
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = guard.as_mut() {
            let mut line = entry.to_string();
            line.push('\n');
            if let Err(e) = f.write_all(line.as_bytes()) {
                log::error!("audit log write failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;

    use super::*;

    #[test]
    fn appends_lines_with_timestamps_and_private_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log/audit.jsonl");
        let log = AuditLog::open(&path).unwrap();
        log.record(json!({"action": "a"}));
        log.record(json!({"action": "b"}));
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["action"], "b");
        assert!(lines[0]["ts"].as_str().unwrap().ends_with('Z'));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
