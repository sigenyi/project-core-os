use super::Collector;
use crate::snapshot::Snapshot;
use crate::sysroot::{Sysroot, key_values};

pub struct MemoryCollector;

impl Collector for MemoryCollector {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let meminfo = root.read("/proc/meminfo").ok_or("cannot read /proc/meminfo")?;
        let kb = |key: &str| -> u64 {
            key_values(&meminfo, ':')
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.split_whitespace().next()?.parse().ok())
                .unwrap_or(0)
        };
        let m = &mut snap.memory;
        m.total_mb = kb("MemTotal") / 1024;
        m.available_mb = match kb("MemAvailable") {
            0 => (kb("MemFree") + kb("Buffers") + kb("Cached")) / 1024,
            v => v / 1024,
        };
        m.swap_total_mb = kb("SwapTotal") / 1024;
        m.swap_free_mb = kb("SwapFree") / 1024;
        Ok(())
    }
}
