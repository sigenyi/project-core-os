//! C.O.R.E. perception: what the AI knows about the machine before it acts.
//!
//! A [`Sensor`] runs a set of [`collectors::Collector`]s against a [`Sysroot`] and
//! derives [`insights`]. The `core-sensed` daemon publishes the resulting [`Snapshot`]
//! to `/run/core-sense/telemetry.json`; the agent embeds a compact [`summary`] of it in every
//! prompt. Collectors read procfs/sysfs directly rather than shelling out, so
//! perception needs no privileges beyond CAP_SYSLOG for the kernel log.

pub mod collectors;
pub mod insights;
pub mod snapshot;
pub mod summary;
pub mod sysroot;

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

pub use collectors::Collector;
pub use insights::InsightRule;
pub use snapshot::Snapshot;
pub use summary::{section, summary};
pub use sysroot::Sysroot;

pub struct Sensor {
    root: Sysroot,
    collectors: Vec<Box<dyn Collector>>,
    rules: Vec<Box<dyn InsightRule>>,
}

impl Sensor {
    /// Standard collectors and rules against `root`.
    pub fn new(root: Sysroot) -> Self {
        Sensor { root, collectors: collectors::default_collectors(), rules: insights::default_rules() }
    }

    /// Custom collectors (standard insight rules).
    pub fn with_collectors(root: Sysroot, collectors: Vec<Box<dyn Collector>>) -> Self {
        Sensor { root, collectors, rules: insights::default_rules() }
    }

    pub fn root(&self) -> &Sysroot {
        &self.root
    }

    /// Take a snapshot. Never fails: a failing collector is recorded in `errors`.
    pub fn snapshot(&self) -> Snapshot {
        let mut snap = Snapshot {
            schema: snapshot::SCHEMA_VERSION,
            collected_at: core_protocol::time::now_rfc3339(),
            collected_unix: core_protocol::time::unix_now(),
            ..Default::default()
        };
        for c in &self.collectors {
            if let Err(error) = c.collect(&self.root, &mut snap) {
                log::debug!("collector {} failed: {error}", c.name());
                snap.errors.push(snapshot::CollectorError { collector: c.name().into(), error });
            }
        }
        snap.insights = insights::evaluate(&snap, &self.rules);
        snap
    }
}

/// Load a snapshot written by `core-sensed`.
pub fn read_snapshot(path: impl AsRef<Path>) -> io::Result<Snapshot> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Seconds since a file was last modified.
pub fn age_secs(path: impl AsRef<Path>) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok().map(|d| d.as_secs())
}

/// Write a snapshot atomically (temp file + rename) so readers never see a partial file.
pub fn write_snapshot(path: impl AsRef<Path>, snap: &Snapshot, pretty: bool) -> io::Result<()> {
    let path = path.as_ref();
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("telemetry")));
    {
        let mut f = fs::File::create(&tmp)?;
        let body = if pretty { serde_json::to_vec_pretty(snap) } else { serde_json::to_vec(snap) }
            .map_err(io::Error::other)?;
        f.write_all(&body)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}
