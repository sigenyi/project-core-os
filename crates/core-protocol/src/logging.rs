//! A tiny `log` backend for C.O.R.E. daemons and tools.
//!
//! Under systemd (detected via `JOURNAL_STREAM`) each line carries a `<N>` syslog
//! priority prefix so journald records the right level; otherwise lines are tagged
//! with a readable level name.

use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

struct Logger {
    journald: bool,
    level: LevelFilter,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = if self.journald {
            let prio = match record.level() {
                Level::Error => 3,
                Level::Warn => 4,
                Level::Info => 6,
                Level::Debug | Level::Trace => 7,
            };
            format!("<{prio}>{}\n", record.args())
        } else {
            format!("[{:<5}] {}: {}\n", record.level(), record.target(), record.args())
        };
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }

    fn flush(&self) {}
}

/// Install the logger. `verbosity` 0 = warnings, 1 = info, 2 = debug, 3+ = trace.
/// `CORE_LOG` (error|warn|info|debug|trace) overrides it.
pub fn init(verbosity: u8) {
    let mut level = match verbosity {
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    if let Some(env) = std::env::var("CORE_LOG").ok().and_then(|v| v.parse().ok()) {
        level = env;
    }
    let logger = Logger { journald: std::env::var_os("JOURNAL_STREAM").is_some(), level };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }
}
