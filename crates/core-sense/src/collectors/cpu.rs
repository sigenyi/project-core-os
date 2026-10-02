use super::Collector;
use crate::snapshot::Snapshot;
use crate::sysroot::Sysroot;

pub struct CpuCollector;

impl Collector for CpuCollector {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let cpuinfo = root.read("/proc/cpuinfo").ok_or("cannot read /proc/cpuinfo")?;
        snap.cpu.model = ["model name", "Hardware", "Model", "cpu model", "uarch"]
            .iter()
            .find_map(|key| {
                cpuinfo.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    (k.trim() == *key && !v.trim().is_empty()).then(|| v.trim().to_string())
                })
            })
            .unwrap_or_else(|| "unknown CPU".into());
        let counted =
            cpuinfo.lines().filter(|l| l.split(':').next().is_some_and(|k| k.trim() == "processor")).count() as u32;
        snap.cpu.logical_cores = if counted > 0 {
            counted
        } else {
            root.read_trim("/sys/devices/system/cpu/online").map(|s| count_cpu_list(&s)).unwrap_or(1)
        };
        if let Some(load) = root.read("/proc/loadavg") {
            for (slot, value) in snap.cpu.load.iter_mut().zip(load.split_whitespace()) {
                *slot = value.parse().unwrap_or(0.0);
            }
        }
        Ok(())
    }
}

/// Count CPUs in a kernel CPU list such as `0-3,6,8-9`.
fn count_cpu_list(list: &str) -> u32 {
    list.split(',')
        .filter_map(|part| match part.split_once('-') {
            Some((a, b)) => Some(b.trim().parse::<u32>().ok()? - a.trim().parse::<u32>().ok()? + 1),
            None => part.trim().parse::<u32>().ok().map(|_| 1),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists() {
        assert_eq!(count_cpu_list("0-7"), 8);
        assert_eq!(count_cpu_list("0-3,6,8-9"), 7);
        assert_eq!(count_cpu_list("0"), 1);
    }
}
