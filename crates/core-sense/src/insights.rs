//! Insights: facts derived from raw telemetry.
//!
//! Small models reason far better over "the Wi-Fi card has no driver because its
//! firmware failed to load" than over a dump of sysfs. Each [`InsightRule`] turns raw
//! readings into a short, prioritised finding with a hint about what to do next.

use std::collections::HashSet;

use crate::snapshot::{Insight, InterfaceKind, Severity, Snapshot};

pub trait InsightRule: Send + Sync {
    fn evaluate(&self, snapshot: &Snapshot, out: &mut Vec<Insight>);
}

pub fn default_rules() -> Vec<Box<dyn InsightRule>> {
    vec![
        Box::new(MissingDrivers),
        Box::new(Connectivity),
        Box::new(DiskSpace),
        Box::new(MemoryPressure),
        Box::new(FailedServices),
        Box::new(KernelProblems),
        Box::new(Thermal),
        Box::new(BatteryLevel),
        Box::new(NoSound),
    ]
}

/// Most important insights kept in a snapshot.
pub const MAX_INSIGHTS: usize = 12;

/// Run every rule and order the findings most-severe first.
pub fn evaluate(snapshot: &Snapshot, rules: &[Box<dyn InsightRule>]) -> Vec<Insight> {
    let mut out = Vec::new();
    for rule in rules {
        rule.evaluate(snapshot, &mut out);
    }
    let mut seen = HashSet::new();
    out.retain(|i| seen.insert(i.message.clone()));
    out.sort_by_key(|i| std::cmp::Reverse(i.severity));
    out.truncate(MAX_INSIGHTS);
    out
}

fn insight(severity: Severity, subsystem: &str, message: String, hint: Option<&str>) -> Insight {
    Insight { severity, subsystem: subsystem.into(), message, hint: hint.map(String::from) }
}

/// Devices in classes that are useless without a driver, but have none bound.
pub struct MissingDrivers;

impl InsightRule for MissingDrivers {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        for d in s.devices.pci.iter().filter(|d| d.driver.is_none()) {
            let subsystem = match (d.class_code.get(..2).unwrap_or(""), d.class_code.as_str()) {
                ("02" | "0d", _) => "network",
                ("03", _) => "display",
                ("04", _) => "audio",
                ("01", _) => "storage",
                (_, "0c03") => "usb",
                _ => continue,
            };
            out.push(insight(
                Severity::Warning,
                subsystem,
                format!("{} {} ({}) has no driver loaded", d.class_name, d.label(), d.slot),
                Some("check read_kernel_log for firmware errors; a kernel module may need loading or firmware may be missing"),
            ));
        }
    }
}

pub struct Connectivity;

impl InsightRule for Connectivity {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        let net = &s.network;
        let physical: Vec<_> = net
            .interfaces
            .iter()
            .filter(|i| matches!(i.kind, InterfaceKind::Ethernet | InterfaceKind::Wireless))
            .collect();
        if physical.is_empty() {
            if !net.interfaces.is_empty() || !s.devices.pci.is_empty() {
                out.push(insight(
                    Severity::Warning,
                    "network",
                    "no network interface hardware is available".into(),
                    Some("look for network controllers without a driver"),
                ));
            }
            return;
        }
        if !physical.iter().any(|i| i.has_link()) {
            let wireless: Vec<&str> =
                physical.iter().filter(|i| i.kind == InterfaceKind::Wireless).map(|i| i.name.as_str()).collect();
            let hint = if wireless.is_empty() {
                "check the cable; set_link can bring an interface up"
            } else {
                "use wifi_scan then wifi_connect to join a network"
            };
            out.push(insight(Severity::Warning, "network", "no network interface is connected".into(), Some(hint)));
            return;
        }
        if net.default_gateway.is_none() {
            out.push(insight(
                Severity::Warning,
                "network",
                "a link is up but there is no default route (DHCP may have failed)".into(),
                Some("restart the network manager service and read its logs"),
            ));
        } else if net.dns_servers.is_empty() {
            out.push(insight(
                Severity::Warning,
                "network",
                "no DNS servers are configured; names will not resolve".into(),
                Some("restart the network manager or systemd-resolved"),
            ));
        }
    }
}

pub struct DiskSpace;

impl InsightRule for DiskSpace {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        for fs in &s.storage {
            if fs.mount == "/"
                && fs.read_only
                && !matches!(fs.fs_type.as_str(), "squashfs" | "erofs" | "overlay" | "iso9660")
            {
                out.push(insight(
                    Severity::Error,
                    "storage",
                    format!("root filesystem ({}) is mounted read-only", fs.device),
                    Some("this usually follows filesystem errors; check read_kernel_log"),
                ));
            }
            let Some(pct) = fs.used_pct else { continue };
            let severity = match pct {
                97.. => Severity::Critical,
                90.. => Severity::Warning,
                _ => continue,
            };
            out.push(insight(
                severity,
                "storage",
                format!("{} is {pct}% full ({} MiB free)", fs.mount, fs.avail_mb.unwrap_or(0)),
                Some("free space by removing unneeded packages or files"),
            ));
        }
    }
}

pub struct MemoryPressure;

impl InsightRule for MemoryPressure {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        let m = &s.memory;
        if m.total_mb == 0 {
            return;
        }
        let avail_pct = m.available_mb * 100 / m.total_mb;
        if avail_pct < 5 {
            out.push(insight(
                Severity::Error,
                "memory",
                format!("memory almost exhausted ({} MiB available)", m.available_mb),
                Some("list_processes sorted by memory"),
            ));
        } else if avail_pct < 10 {
            out.push(insight(
                Severity::Warning,
                "memory",
                format!("memory is low ({} MiB available)", m.available_mb),
                Some("list_processes sorted by memory"),
            ));
        }
        if m.swap_total_mb == 0 && m.total_mb < 8192 {
            out.push(insight(
                Severity::Info,
                "memory",
                "no swap is configured".into(),
                Some("configure_swap can add a swap file"),
            ));
        }
    }
}

pub struct FailedServices;

impl InsightRule for FailedServices {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        for unit in &s.services.failed {
            out.push(insight(
                Severity::Warning,
                "services",
                format!("{unit} has failed"),
                Some("service_status and read_logs show why"),
            ));
        }
    }
}

pub struct KernelProblems;

impl InsightRule for KernelProblems {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        let mut firmware_drivers = HashSet::new();
        for msg in &s.kernel_log.notable {
            let lower = msg.message.to_lowercase();
            let source = msg.message.split([' ', ':']).next().unwrap_or("kernel").to_string();
            if lower.contains("firmware")
                && ["failed", "error", "not found", "no suitable"].iter().any(|k| lower.contains(k))
            {
                if firmware_drivers.insert(source.clone()) {
                    out.push(insight(
                        Severity::Error,
                        "kernel",
                        format!("{source}: firmware failed to load"),
                        Some("the firmware package (e.g. linux-firmware) may be missing; install it and reload the module"),
                    ));
                }
            } else if ["bug:", "oops", "general protection", "kernel panic", "call trace"]
                .iter()
                .any(|k| lower.contains(k))
            {
                out.push(insight(
                    Severity::Error,
                    "kernel",
                    format!("kernel fault: {}", clip(&msg.message, 120)),
                    None,
                ));
            } else if lower.contains("i/o error") {
                out.push(insight(
                    Severity::Error,
                    "storage",
                    format!("disk I/O error: {}", clip(&msg.message, 120)),
                    Some("the disk may be failing; back up important data"),
                ));
            } else if lower.contains("killed process") || lower.contains("out of memory") {
                out.push(insight(
                    Severity::Warning,
                    "memory",
                    format!("out-of-memory kill: {}", clip(&msg.message, 120)),
                    Some("configure_swap or close programs"),
                ));
            } else if lower.contains("segfault") {
                out.push(insight(Severity::Info, "processes", format!("{source} crashed (segfault)"), None));
            }
        }
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { s.chars().take(max).collect::<String>() + "…" }
}

pub struct Thermal;

impl InsightRule for Thermal {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        let Some(hot) = s.thermal.iter().max_by(|a, b| a.temp_c.total_cmp(&b.temp_c)) else { return };
        let severity = match hot.temp_c {
            t if t >= 100.0 => Severity::Critical,
            t if t >= 90.0 => Severity::Warning,
            _ => return,
        };
        out.push(insight(
            severity,
            "thermal",
            format!("{} is at {:.0}°C", hot.name, hot.temp_c),
            Some("list_processes sorted by cpu"),
        ));
    }
}

pub struct BatteryLevel;

impl InsightRule for BatteryLevel {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        for b in s.power.batteries.iter().filter(|b| b.status == "Discharging") {
            let Some(pct) = b.capacity_pct else { continue };
            let severity = match pct {
                0..=10 => Severity::Error,
                11..=20 => Severity::Warning,
                _ => continue,
            };
            out.push(insight(severity, "power", format!("battery {} is at {pct}% and discharging", b.name), None));
        }
    }
}

pub struct NoSound;

impl InsightRule for NoSound {
    fn evaluate(&self, s: &Snapshot, out: &mut Vec<Insight>) {
        let has_audio_hw = s.devices.pci.iter().any(|d| d.class_code.starts_with("04"));
        if s.audio.cards.is_empty() && has_audio_hw {
            out.push(insight(
                Severity::Warning,
                "audio",
                "audio hardware is present but no sound card is registered".into(),
                Some("check read_kernel_log; the snd_hda_intel or snd_sof modules may need loading"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::*;

    fn base() -> Snapshot {
        Snapshot {
            network: Network {
                interfaces: vec![NetInterface {
                    name: "wlan0".into(),
                    kind: InterfaceKind::Wireless,
                    state: "up".into(),
                    carrier: Some(true),
                    ..Default::default()
                }],
                default_gateway: Some("192.168.1.1".into()),
                default_interface: Some("wlan0".into()),
                dns_servers: vec!["1.1.1.1".into()],
            },
            memory: Memory { total_mb: 16000, available_mb: 8000, swap_total_mb: 4096, swap_free_mb: 4096 },
            ..Default::default()
        }
    }

    fn run(s: &Snapshot) -> Vec<Insight> {
        evaluate(s, &default_rules())
    }

    #[test]
    fn healthy_system_has_no_insights() {
        assert!(run(&base()).is_empty());
    }

    #[test]
    fn disconnected_wifi() {
        let mut s = base();
        s.network.interfaces[0].state = "down".into();
        s.network.interfaces[0].carrier = None;
        let i = run(&s);
        assert_eq!(i.len(), 1);
        assert_eq!(i[0].message, "no network interface is connected");
        assert!(i[0].hint.as_deref().unwrap().contains("wifi_scan"));
    }

    #[test]
    fn missing_driver_and_firmware() {
        let mut s = base();
        s.devices.pci.push(PciDevice {
            slot: "0000:03:00.0".into(),
            class_code: "0280".into(),
            class_name: "Network controller".into(),
            vendor_id: "8086".into(),
            device_id: "2723".into(),
            vendor: Some("Intel Corporation".into()),
            device: Some("Wi-Fi 6 AX200".into()),
            driver: None,
        });
        s.kernel_log.notable.push(KernelMessage {
            level: 3,
            uptime_secs: 5.0,
            message: "iwlwifi 0000:03:00.0: Direct firmware load for iwlwifi-cc-a0-77.ucode failed with error -2"
                .into(),
            repeats: 1,
        });
        let i = run(&s);
        assert_eq!(i[0].severity, Severity::Error);
        assert_eq!(i[0].message, "iwlwifi: firmware failed to load");
        assert_eq!(
            i[1].message,
            "Network controller Intel Corporation Wi-Fi 6 AX200 (0000:03:00.0) has no driver loaded"
        );
    }

    #[test]
    fn disk_memory_thermal_battery() {
        let mut s = base();
        s.storage.push(Filesystem {
            mount: "/".into(),
            fs_type: "ext4".into(),
            used_pct: Some(98),
            avail_mb: Some(300),
            ..Default::default()
        });
        s.memory.available_mb = 300;
        s.memory.swap_total_mb = 0;
        s.memory.total_mb = 4000;
        s.thermal.push(ThermalZone { name: "x86_pkg_temp".into(), temp_c: 95.0 });
        s.power.batteries.push(Battery { name: "BAT0".into(), capacity_pct: Some(8), status: "Discharging".into() });
        let i = run(&s);
        let msgs: Vec<&str> = i.iter().map(|i| i.message.as_str()).collect();
        assert_eq!(i[0].severity, Severity::Critical);
        assert!(msgs.contains(&"/ is 98% full (300 MiB free)"));
        assert!(msgs.contains(&"memory is low (300 MiB available)"));
        assert!(msgs.contains(&"no swap is configured"));
        assert!(msgs.contains(&"x86_pkg_temp is at 95°C"));
        assert!(msgs.contains(&"battery BAT0 is at 8% and discharging"));
    }

    #[test]
    fn insights_are_capped_and_ordered() {
        let mut s = base();
        s.services.failed = (0..30).map(|n| format!("unit{n}.service")).collect();
        s.memory.available_mb = 100;
        let i = run(&s);
        assert_eq!(i.len(), MAX_INSIGHTS);
        assert_eq!(i[0].severity, Severity::Error);
    }
}
