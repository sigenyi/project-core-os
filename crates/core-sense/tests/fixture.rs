//! End-to-end perception test against a synthetic machine: a laptop whose Intel Wi-Fi
//! card has no driver because its firmware is missing.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use core_sense::collectors::{self, Collector, ServicesCollector};
use core_sense::snapshot::{InterfaceKind, Severity};
use core_sense::{Sensor, Sysroot, read_snapshot, summary, write_snapshot};

fn put(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn build_fixture(root: &Path) {
    put(root, "proc/sys/kernel/hostname", "core-laptop\n");
    put(root, "proc/sys/kernel/osrelease", "6.10.2-arch1-1\n");
    put(root, "proc/sys/kernel/arch", "x86_64\n");
    put(root, "etc/os-release", "NAME=\"C.O.R.E. OS\"\nPRETTY_NAME=\"C.O.R.E. OS 0.1\"\n");
    put(root, "proc/uptime", "7980.55 31000.10\n");
    put(
        root,
        "proc/cpuinfo",
        "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i7-1165G7\nflags\t\t: fpu vme\n\nprocessor\t: 1\nmodel name\t: Intel(R) Core(TM) i7-1165G7\n",
    );
    put(root, "proc/loadavg", "0.42 0.30 0.21 1/234 5678\n");
    put(
        root,
        "proc/meminfo",
        "MemTotal:       16303740 kB\nMemFree:  1000 kB\nMemAvailable:   12000000 kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n",
    );
    put(
        root,
        "proc/mounts",
        "/dev/nvme0n1p2 / ext4 rw,relatime 0 0\n/dev/nvme0n1p1 /boot vfat rw 0 0\nproc /proc proc rw 0 0\n",
    );
    put(root, "sys/block/nvme0n1/size", "500118192\n");
    put(root, "sys/block/nvme0n1/removable", "0\n");
    put(root, "sys/block/nvme0n1/queue/rotational", "0\n");
    put(root, "sys/block/nvme0n1/device/model", "Samsung SSD 980\n");
    put(root, "sys/block/loop0/size", "100\n");
    put(root, "sys/class/dmi/id/sys_vendor", "LENOVO\n");
    put(root, "sys/class/dmi/id/product_name", "20XW\n");

    // PCI: Wi-Fi without driver, GPU with driver.
    let wifi = "sys/devices/pci0000:00/0000:03:00.0";
    put(root, &format!("{wifi}/class"), "0x028000\n");
    put(root, &format!("{wifi}/vendor"), "0x8086\n");
    put(root, &format!("{wifi}/device"), "0x2723\n");
    let gpu = "sys/devices/pci0000:00/0000:00:02.0";
    put(root, &format!("{gpu}/class"), "0x030000\n");
    put(root, &format!("{gpu}/vendor"), "0x8086\n");
    put(root, &format!("{gpu}/device"), "0x9a49\n");
    fs::create_dir_all(root.join("sys/bus/pci/drivers/i915")).unwrap();
    symlink("../../../bus/pci/drivers/i915", root.join(gpu).join("driver")).unwrap();
    fs::create_dir_all(root.join("sys/bus/pci/devices")).unwrap();
    symlink(root.join(wifi), root.join("sys/bus/pci/devices/0000:03:00.0")).unwrap();
    symlink(root.join(gpu), root.join("sys/bus/pci/devices/0000:00:02.0")).unwrap();
    put(
        root,
        "usr/share/hwdata/pci.ids",
        "8086  Intel Corporation\n\t2723  Wi-Fi 6 AX200\n\t9a49  TigerLake-LP GT2 [Iris Xe Graphics]\n",
    );

    // Network: only loopback and a wired NIC that is down.
    put(root, "sys/class/net/lo/operstate", "unknown\n");
    put(root, "sys/class/net/lo/type", "772\n");
    put(root, "sys/class/net/eth0/operstate", "down\n");
    put(root, "sys/class/net/eth0/address", "aa:bb:cc:dd:ee:ff\n");
    put(root, "sys/class/net/eth0/type", "1\n");
    fs::create_dir_all(root.join("sys/class/net/eth0/device")).unwrap();
    put(root, "proc/net/route", "Iface\tDestination\tGateway\tFlags\n");
    put(root, "etc/resolv.conf", "nameserver 127.0.0.53\n");

    put(root, "sys/class/power_supply/AC/type", "Mains\n");
    put(root, "sys/class/power_supply/AC/online", "0\n");
    put(root, "sys/class/power_supply/BAT0/type", "Battery\n");
    put(root, "sys/class/power_supply/BAT0/capacity", "64\n");
    put(root, "sys/class/power_supply/BAT0/status", "Discharging\n");
    put(root, "sys/class/thermal/thermal_zone0/type", "x86_pkg_temp\n");
    put(root, "sys/class/thermal/thermal_zone0/temp", "51000\n");

    put(
        root,
        "dev/kmsg",
        "6,1,1000,-;Linux version 6.10.2\n3,2,5140900,-;iwlwifi 0000:03:00.0: Direct firmware load for iwlwifi-cc-a0-77.ucode failed with error -2\n",
    );
}

fn sensor(root: &Path) -> Sensor {
    let mut set: Vec<Box<dyn Collector>> =
        collectors::default_collectors().into_iter().filter(|c| c.name() != "services").collect();
    set.push(Box::new(ServicesCollector::Fixed(vec!["bluetooth.service".into()])));
    Sensor::with_collectors(Sysroot::at(root), set)
}

#[test]
fn perceives_a_broken_wifi_laptop() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let snap = sensor(dir.path()).snapshot();

    assert!(snap.errors.is_empty(), "{:?}", snap.errors);
    assert_eq!(snap.host.hostname, "core-laptop");
    assert_eq!(snap.host.os, "C.O.R.E. OS 0.1");
    assert_eq!(snap.host.product.as_deref(), Some("LENOVO 20XW"));
    assert_eq!(snap.host.virtualization, None);
    assert_eq!(snap.cpu.logical_cores, 2);
    assert_eq!(snap.cpu.model, "Intel(R) Core(TM) i7-1165G7");
    assert_eq!(snap.memory.total_mb, 15921);
    assert_eq!(snap.storage.len(), 2);
    assert_eq!(snap.block_devices.len(), 1, "loop devices are skipped");
    assert_eq!(snap.block_devices[0].size_mb, 244198);

    let eth = snap.network.interfaces.iter().find(|i| i.name == "eth0").unwrap();
    assert_eq!(eth.kind, InterfaceKind::Ethernet);
    assert_eq!(snap.network.dns_servers, ["127.0.0.53"]);

    let wifi = snap.devices.pci.iter().find(|d| d.slot == "0000:03:00.0").unwrap();
    assert_eq!(wifi.driver, None);
    assert_eq!(wifi.device.as_deref(), Some("Wi-Fi 6 AX200"));
    let gpu = snap.devices.pci.iter().find(|d| d.slot == "0000:00:02.0").unwrap();
    assert_eq!(gpu.driver.as_deref(), Some("i915"));

    assert_eq!(snap.power.ac_online, Some(false));
    assert_eq!(snap.power.batteries[0].capacity_pct, Some(64));
    assert_eq!(snap.services.failed, ["bluetooth.service"]);
    assert!(snap.kernel_log.available);
    assert_eq!(snap.kernel_log.notable.len(), 1);

    let messages: Vec<&str> = snap.insights.iter().map(|i| i.message.as_str()).collect();
    assert_eq!(snap.insights[0].severity, Severity::Error);
    assert!(messages.contains(&"iwlwifi: firmware failed to load"), "{messages:?}");
    assert!(
        messages.contains(&"Network controller Intel Corporation Wi-Fi 6 AX200 (0000:03:00.0) has no driver loaded"),
        "{messages:?}"
    );
    assert!(messages.contains(&"no network interface is connected"), "{messages:?}");
    assert!(messages.contains(&"bluetooth.service has failed"), "{messages:?}");
    assert!(!messages.contains(&"no swap is configured"), "16 GiB machines do not need a swap nag");

    let text = summary(&snap);
    assert!(text.contains("issues:\n- [error] kernel: iwlwifi: firmware failed to load"), "{text}");
    assert!(text.contains("power: on battery · BAT0 64% Discharging"), "{text}");
}

#[test]
fn snapshot_round_trips_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let snap = sensor(dir.path()).snapshot();
    let out = dir.path().join("run/core/telemetry.json");
    write_snapshot(&out, &snap, false).unwrap();
    assert_eq!(read_snapshot(&out).unwrap(), snap);
}

#[test]
fn empty_root_degrades_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let snap = Sensor::new(Sysroot::at(dir.path())).snapshot();
    let failed: Vec<&str> = snap.errors.iter().map(|e| e.collector.as_str()).collect();
    assert!(failed.contains(&"cpu") && failed.contains(&"memory") && failed.contains(&"kernel_log"), "{failed:?}");
    assert_eq!(snap.host.hostname, "localhost");
}

#[test]
fn live_snapshot_works_on_this_machine() {
    let snap = Sensor::new(Sysroot::live()).snapshot();
    assert!(snap.cpu.logical_cores >= 1);
    assert!(snap.memory.total_mb > 0);
    assert!(!summary(&snap).is_empty());
}
