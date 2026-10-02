use std::collections::HashMap;
use std::ffi::CStr;
use std::net::{Ipv4Addr, Ipv6Addr};

use super::Collector;
use crate::snapshot::{InterfaceKind, NetInterface, Snapshot};
use crate::sysroot::Sysroot;

pub struct NetworkCollector;

impl Collector for NetworkCollector {
    fn name(&self) -> &'static str {
        "network"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let ipv4 = if root.is_live() { live_ipv4_addresses() } else { HashMap::new() };
        let ipv6 = root.read("/proc/net/if_inet6").map(|t| parse_if_inet6(&t)).unwrap_or_default();

        let net = &mut snap.network;
        net.interfaces = root
            .list("/sys/class/net")
            .into_iter()
            .filter(|n| root.exists(format!("/sys/class/net/{n}/operstate")))
            .map(|name| {
                let base = format!("/sys/class/net/{name}");
                let kind = interface_kind(root, &name, &base);
                NetInterface {
                    kind,
                    state: root.read_trim(format!("{base}/operstate")).unwrap_or_else(|| "unknown".into()),
                    carrier: root.read_trim(format!("{base}/carrier")).map(|c| c == "1"),
                    mac: root
                        .read_trim(format!("{base}/address"))
                        .filter(|m| kind != InterfaceKind::Loopback && m != "00:00:00:00:00:00"),
                    driver: root.link_name(format!("{base}/device/driver")),
                    ipv4: ipv4.get(&name).cloned().unwrap_or_default(),
                    ipv6: ipv6.get(&name).cloned().unwrap_or_default(),
                    name,
                }
            })
            .collect();

        if let Some((iface, gw)) = root.read("/proc/net/route").and_then(|t| parse_default_route(&t)) {
            net.default_interface = Some(iface);
            net.default_gateway = Some(gw.to_string());
        }

        let mut dns = root.read("/etc/resolv.conf").map(|t| parse_nameservers(&t)).unwrap_or_default();
        // systemd-resolved's stub hides the real upstream servers.
        if dns.iter().all(|s| s == "127.0.0.53" || s == "127.0.0.54") {
            if let Some(upstream) = root.read("/run/systemd/resolve/resolv.conf").map(|t| parse_nameservers(&t)) {
                if !upstream.is_empty() {
                    dns = upstream;
                }
            }
        }
        net.dns_servers = dns;
        Ok(())
    }
}

fn interface_kind(root: &Sysroot, name: &str, base: &str) -> InterfaceKind {
    if name == "lo" || root.read_trim(format!("{base}/type")).as_deref() == Some("772") {
        InterfaceKind::Loopback
    } else if root.exists(format!("{base}/wireless")) || root.exists(format!("{base}/phy80211")) {
        InterfaceKind::Wireless
    } else if root.exists(format!("{base}/bridge")) {
        InterfaceKind::Bridge
    } else if root.exists(format!("{base}/tun_flags")) || name.starts_with("wg") {
        InterfaceKind::Tunnel
    } else if root.exists(format!("{base}/device")) {
        InterfaceKind::Ethernet
    } else {
        InterfaceKind::Virtual
    }
}

/// Default IPv4 route from `/proc/net/route` (fields are little-endian hex).
pub(crate) fn parse_default_route(text: &str) -> Option<(String, Ipv4Addr)> {
    text.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 || f[1] != "00000000" {
            return None;
        }
        let flags = u16::from_str_radix(f[3], 16).ok()?;
        if flags & 0x1 == 0 {
            return None; // RTF_UP not set
        }
        // The kernel prints the network-order address as a native-endian u32, so the
        // native byte representation is the address in network order.
        let gw = u32::from_str_radix(f[2], 16).ok()?;
        Some((f[0].to_string(), Ipv4Addr::from(gw.to_ne_bytes())))
    })
}

/// Global IPv6 addresses per interface from `/proc/net/if_inet6`.
pub(crate) fn parse_if_inet6(text: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 6 || f[3] != "00" {
            continue; // only scope global
        }
        let (Ok(addr), Ok(prefix)) = (u128::from_str_radix(f[0], 16), u8::from_str_radix(f[2], 16)) else {
            continue;
        };
        out.entry(f[5].to_string()).or_default().push(format!("{}/{prefix}", Ipv6Addr::from(addr)));
    }
    out
}

pub(crate) fn parse_nameservers(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(String::from)
        .collect()
}

/// IPv4 addresses (CIDR notation) of every interface, via getifaddrs(3).
fn live_ipv4_addresses() -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    // SAFETY: getifaddrs allocates a linked list we only read and then free with
    // freeifaddrs. Every pointer is checked for NULL before being dereferenced, and
    // sockaddr pointers are only cast after checking sa_family == AF_INET.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let addr = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
                let prefix = if ifa.ifa_netmask.is_null() {
                    32
                } else {
                    u32::from_be((*(ifa.ifa_netmask as *const libc::sockaddr_in)).sin_addr.s_addr).count_ones()
                };
                let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                out.entry(name).or_default().push(format!("{ip}/{prefix}"));
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_route() {
        let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0
";
        let (iface, gw) = parse_default_route(text).unwrap();
        assert_eq!(iface, "wlan0");
        assert_eq!(gw, Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(parse_default_route("Iface\tDestination\n"), None);
    }

    #[test]
    fn ipv6_global_only() {
        let text = "\
fe80000000000000021a2bfffe3c4d5e 03 40 20 80    wlan0
2a0102030405060700000000000000aa 03 40 00 00    wlan0
00000000000000000000000000000001 01 80 10 80       lo
";
        let map = parse_if_inet6(text);
        assert_eq!(map["wlan0"], vec!["2a01:203:405:607::aa/64".to_string()]);
        assert!(!map.contains_key("lo"));
    }

    #[test]
    fn nameservers() {
        assert_eq!(
            parse_nameservers("# comment\nnameserver 1.1.1.1\nnameserver  9.9.9.9 \nsearch lan\n"),
            ["1.1.1.1", "9.9.9.9"]
        );
    }
}
