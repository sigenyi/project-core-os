//! Token-efficient renderings of a snapshot for the language model.
//!
//! The full JSON snapshot is several kilobytes; the summary is a dozen short lines.
//! The agent embeds the summary in every prompt and lets the model drill into any
//! section on demand with `get_telemetry`.

use serde_json::{Value, json};

use crate::snapshot::{InterfaceKind, Snapshot};
use core_protocol::time::human_duration;

fn gib(mb: u64) -> String {
    let g = mb as f64 / 1024.0;
    if g >= 10.0 { format!("{g:.0}") } else { format!("{g:.1}") }
}

/// A compact, line-oriented description of the machine's state.
pub fn summary(s: &Snapshot) -> String {
    let mut lines = Vec::new();

    let h = &s.host;
    let mut host =
        format!("host: {} · {} · kernel {} · up {}", h.hostname, h.os, h.kernel, human_duration(h.uptime_secs));
    if let Some(v) = &h.virtualization {
        host.push_str(&format!(" · virtual machine ({v})"));
    }
    lines.push(host);

    let c = &s.cpu;
    lines.push(format!(
        "cpu: {} ({} threads) · load {:.2} {:.2} {:.2}",
        c.model, c.logical_cores, c.load[0], c.load[1], c.load[2]
    ));

    let m = &s.memory;
    let swap = if m.swap_total_mb == 0 {
        "none".to_string()
    } else {
        format!("{}/{} GiB used", gib(m.swap_total_mb - m.swap_free_mb.min(m.swap_total_mb)), gib(m.swap_total_mb))
    };
    lines.push(format!(
        "memory: {}/{} GiB used ({}%) · swap {swap}",
        gib(m.total_mb - m.available_mb.min(m.total_mb)),
        gib(m.total_mb),
        m.used_pct()
    ));

    let disks: Vec<String> = s
        .storage
        .iter()
        .take(6)
        .map(|f| match (f.used_pct, f.size_mb) {
            (Some(p), Some(size)) => format!("{} {} {p}% of {} GiB", f.mount, f.fs_type, gib(size)),
            _ => format!("{} {}", f.mount, f.fs_type),
        })
        .collect();
    if !disks.is_empty() {
        lines.push(format!("disks: {}", disks.join(" · ")));
    }

    let n = &s.network;
    let mut net: Vec<String> = n
        .interfaces
        .iter()
        .filter(|i| i.kind != InterfaceKind::Loopback)
        .take(6)
        .map(|i| {
            let kind = format!("{:?}", i.kind).to_lowercase();
            let driver = i.driver.as_deref().map(|d| format!(", {d}")).unwrap_or_default();
            let addr = i.ipv4.first().map(|a| format!(" {a}")).unwrap_or_default();
            format!("{} ({kind}{driver}) {}{addr}", i.name, i.state)
        })
        .collect();
    if let Some(gw) = &n.default_gateway {
        net.push(format!("gateway {gw}"));
    }
    if !n.dns_servers.is_empty() {
        net.push(format!("dns {}", n.dns_servers.join(" ")));
    }
    lines.push(format!("network: {}", if net.is_empty() { "no interfaces".into() } else { net.join(" · ") }));

    let cards: Vec<&str> = s.audio.cards.iter().map(|c| c.name.as_str()).collect();
    lines.push(format!("audio: {}", if cards.is_empty() { "no sound card".into() } else { cards.join(", ") }));

    let p = &s.power;
    if p.ac_online.is_some() || !p.batteries.is_empty() {
        let mut parts = Vec::new();
        if let Some(ac) = p.ac_online {
            parts.push(if ac { "on AC".to_string() } else { "on battery".to_string() });
        }
        for b in &p.batteries {
            parts.push(format!(
                "{} {}% {}",
                b.name,
                b.capacity_pct.map(|c| c.to_string()).unwrap_or("?".into()),
                b.status
            ));
        }
        lines.push(format!("power: {}", parts.join(" · ")));
    }

    if let Some(hot) = s.thermal.iter().max_by(|a, b| a.temp_c.total_cmp(&b.temp_c)) {
        lines.push(format!("temperature: max {:.0}°C ({})", hot.temp_c, hot.name));
    }

    if s.services.checked {
        let failed = if s.services.failed.is_empty() { "none".to_string() } else { s.services.failed.join(", ") };
        lines.push(format!("failed services: {failed}"));
    }

    if s.insights.is_empty() {
        lines.push("issues: none detected".into());
    } else {
        lines.push("issues:".into());
        for i in &s.insights {
            let sev = format!("{:?}", i.severity).to_lowercase();
            let hint = i.hint.as_deref().map(|h| format!(" (hint: {h})")).unwrap_or_default();
            lines.push(format!("- [{sev}] {}: {}{hint}", i.subsystem, i.message));
        }
    }
    lines.join("\n")
}

/// One section of the snapshot as JSON, for `get_telemetry`.
pub fn section(s: &Snapshot, name: &str) -> Option<Value> {
    let v = match name {
        "summary" => Value::String(summary(s)),
        "host" => serde_json::to_value(&s.host).ok()?,
        "cpu" => serde_json::to_value(&s.cpu).ok()?,
        "memory" => serde_json::to_value(&s.memory).ok()?,
        "storage" => json!({"filesystems": s.storage, "block_devices": s.block_devices}),
        "network" => serde_json::to_value(&s.network).ok()?,
        "devices" => serde_json::to_value(&s.devices).ok()?,
        "audio" => serde_json::to_value(&s.audio).ok()?,
        "power" => serde_json::to_value(&s.power).ok()?,
        "thermal" => serde_json::to_value(&s.thermal).ok()?,
        "services" => serde_json::to_value(&s.services).ok()?,
        "kernel_log" => serde_json::to_value(&s.kernel_log).ok()?,
        "insights" => serde_json::to_value(&s.insights).ok()?,
        _ => return None,
    };
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::*;

    #[test]
    fn summary_mentions_key_facts() {
        let s = Snapshot {
            host: Host {
                hostname: "core".into(),
                os: "Arch Linux".into(),
                kernel: "6.10".into(),
                uptime_secs: 7980,
                virtualization: Some("qemu".into()),
                ..Default::default()
            },
            memory: Memory { total_mb: 16384, available_mb: 12288, swap_total_mb: 0, swap_free_mb: 0 },
            network: Network {
                interfaces: vec![NetInterface {
                    name: "wlan0".into(),
                    kind: InterfaceKind::Wireless,
                    state: "up".into(),
                    driver: Some("iwlwifi".into()),
                    ipv4: vec!["192.168.1.20/24".into()],
                    ..Default::default()
                }],
                default_gateway: Some("192.168.1.1".into()),
                dns_servers: vec!["1.1.1.1".into()],
                ..Default::default()
            },
            insights: vec![Insight {
                severity: Severity::Warning,
                subsystem: "services".into(),
                message: "cups.service has failed".into(),
                hint: None,
            }],
            ..Default::default()
        };
        let text = summary(&s);
        assert!(text.contains("host: core · Arch Linux · kernel 6.10 · up 2h 13m · virtual machine (qemu)"), "{text}");
        assert!(text.contains("memory: 4.0/16 GiB used (25%) · swap none"), "{text}");
        assert!(
            text.contains("wlan0 (wireless, iwlwifi) up 192.168.1.20/24 · gateway 192.168.1.1 · dns 1.1.1.1"),
            "{text}"
        );
        assert!(text.contains("- [warning] services: cups.service has failed"), "{text}");
    }

    #[test]
    fn sections() {
        let s = Snapshot::default();
        for name in core_protocol::choice::TelemetrySection::ALL {
            assert!(section(&s, name).is_some(), "{name}");
        }
        assert!(section(&s, "bogus").is_none());
    }
}
