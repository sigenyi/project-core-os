use super::Collector;
use crate::snapshot::Snapshot;
use crate::sysroot::{Sysroot, key_values};

pub struct HostCollector;

impl Collector for HostCollector {
    fn name(&self) -> &'static str {
        "host"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let h = &mut snap.host;
        h.hostname = root
            .read_trim("/proc/sys/kernel/hostname")
            .or_else(|| root.read_trim("/etc/hostname"))
            .unwrap_or_else(|| "localhost".into());
        h.kernel = root.read_trim("/proc/sys/kernel/osrelease").unwrap_or_default();
        h.os = root
            .read("/etc/os-release")
            .or_else(|| root.read("/usr/lib/os-release"))
            .and_then(|t| key_values(&t, '=').find(|(k, _)| *k == "PRETTY_NAME").map(|(_, v)| v.to_string()))
            .unwrap_or_else(|| "Linux".into());
        h.arch = root
            .read_trim("/proc/sys/kernel/arch")
            .or_else(|| root.is_live().then(|| std::env::consts::ARCH.to_string()))
            .unwrap_or_default();
        h.uptime_secs = root
            .read("/proc/uptime")
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .map(|f| f as u64)
            .unwrap_or(0);
        h.timezone =
            root.link_target("/etc/localtime").and_then(|t| t.split_once("zoneinfo/").map(|(_, tz)| tz.to_string()));

        let vendor = root.read_trim("/sys/class/dmi/id/sys_vendor");
        let product = root.read_trim("/sys/class/dmi/id/product_name");
        h.product = match (&vendor, &product) {
            (Some(v), Some(p)) if p.starts_with(v.as_str()) => Some(p.clone()),
            (Some(v), Some(p)) => Some(format!("{v} {p}")),
            (Some(x), None) | (None, Some(x)) => Some(x.clone()),
            (None, None) => None,
        };
        let cpu_flags_hypervisor = root.read("/proc/cpuinfo").is_some_and(|c| {
            c.lines().any(|l| l.starts_with("flags") && l.split_whitespace().any(|f| f == "hypervisor"))
        });
        h.virtualization = detect_virtualization(vendor.as_deref(), product.as_deref(), cpu_flags_hypervisor);
        Ok(())
    }
}

fn detect_virtualization(vendor: Option<&str>, product: Option<&str>, hypervisor_flag: bool) -> Option<String> {
    let ident = format!("{} {}", vendor.unwrap_or(""), product.unwrap_or("")).to_lowercase();
    const KNOWN: &[(&str, &str)] = &[
        ("qemu", "qemu"),
        ("kvm", "kvm"),
        ("virtualbox", "virtualbox"),
        ("innotek", "virtualbox"),
        ("vmware", "vmware"),
        ("microsoft corporation virtual machine", "hyper-v"),
        ("xen", "xen"),
        ("parallels", "parallels"),
        ("bochs", "bochs"),
    ];
    KNOWN
        .iter()
        .find(|(needle, _)| ident.contains(needle))
        .map(|(_, name)| name.to_string())
        .or_else(|| hypervisor_flag.then(|| "unknown".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtualization_detection() {
        assert_eq!(
            detect_virtualization(Some("QEMU"), Some("Standard PC (Q35 + ICH9, 2009)"), true).as_deref(),
            Some("qemu")
        );
        assert_eq!(
            detect_virtualization(Some("Microsoft Corporation"), Some("Virtual Machine"), true).as_deref(),
            Some("hyper-v")
        );
        assert_eq!(detect_virtualization(Some("LENOVO"), Some("20XW"), false), None);
        assert_eq!(detect_virtualization(None, None, true).as_deref(), Some("unknown"));
    }
}
