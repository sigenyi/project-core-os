//! Where the agent gets its picture of the machine.

use std::path::PathBuf;

use core_sense::{Sensor, Snapshot, Sysroot, age_secs, read_snapshot};

pub trait TelemetryProvider: Send {
    fn snapshot(&mut self) -> Snapshot;
}

/// Prefer the snapshot published by `core-sensed` (it has CAP_SYSLOG for the kernel
/// log); fall back to collecting live, unprivileged, when it is missing or stale.
pub struct PublishedTelemetry {
    path: PathBuf,
    max_age_secs: u64,
    sensor: Sensor,
}

impl PublishedTelemetry {
    pub fn new(path: impl Into<PathBuf>, max_age_secs: u64) -> Self {
        PublishedTelemetry { path: path.into(), max_age_secs, sensor: Sensor::new(Sysroot::live()) }
    }
}

impl TelemetryProvider for PublishedTelemetry {
    fn snapshot(&mut self) -> Snapshot {
        if age_secs(&self.path).is_some_and(|age| age <= self.max_age_secs) {
            match read_snapshot(&self.path) {
                Ok(s) => return s,
                Err(e) => log::warn!("ignoring unreadable telemetry {}: {e}", self.path.display()),
            }
        }
        self.sensor.snapshot()
    }
}

/// A fixed snapshot (tests, demos).
pub struct StaticTelemetry(pub Snapshot);

impl TelemetryProvider for StaticTelemetry {
    fn snapshot(&mut self) -> Snapshot {
        self.0.clone()
    }
}
