use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Mutex;

use super::Collector;
use crate::snapshot::{PciDevice, Snapshot, UsbDevice};
use crate::sysroot::Sysroot;

type IdPair = (String, String);
type Names = (Option<String>, Option<String>);

/// PCI and USB device inventory, including which kernel driver (if any) is bound.
pub struct DevicesCollector {
    /// Candidate locations of the pci.ids database (relative to the sysroot).
    pci_ids: Vec<PathBuf>,
    /// Name lookups are cached: pci.ids is ~1.5 MB and devices rarely change.
    cache: Mutex<HashMap<IdPair, Names>>,
}

impl Default for DevicesCollector {
    fn default() -> Self {
        DevicesCollector {
            pci_ids: vec!["/usr/share/hwdata/pci.ids".into(), "/usr/share/misc/pci.ids".into()],
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Collector for DevicesCollector {
    fn name(&self) -> &'static str {
        "devices"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let mut pci: Vec<PciDevice> = root
            .list("/sys/bus/pci/devices")
            .into_iter()
            .map(|slot| {
                let base = format!("/sys/bus/pci/devices/{slot}");
                let hex = |file: &str| {
                    root.read_trim(format!("{base}/{file}"))
                        .map(|v| v.trim_start_matches("0x").to_string())
                        .unwrap_or_default()
                };
                let class = hex("class");
                let class_code = class.get(..4).unwrap_or(&class).to_string();
                PciDevice {
                    class_name: pci_class_name(&class_code).to_string(),
                    class_code,
                    vendor_id: hex("vendor"),
                    device_id: hex("device"),
                    driver: root.link_name(format!("{base}/driver")),
                    vendor: None,
                    device: None,
                    slot,
                }
            })
            .collect();
        self.name_pci_devices(root, &mut pci);
        snap.devices.pci = pci;
        snap.devices.usb = usb_devices(root);
        Ok(())
    }
}

impl DevicesCollector {
    fn name_pci_devices(&self, root: &Sysroot, devices: &mut [PciDevice]) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let missing: HashSet<IdPair> = devices
            .iter()
            .map(|d| (d.vendor_id.clone(), d.device_id.clone()))
            .filter(|k| !cache.contains_key(k))
            .collect();
        if !missing.is_empty() {
            let found = self
                .pci_ids
                .iter()
                .find_map(|p| File::open(root.path(p)).ok())
                .map(|f| lookup_pci_ids(BufReader::new(f), &missing))
                .unwrap_or_default();
            for key in missing {
                let names = found.get(&key).cloned().unwrap_or((None, None));
                cache.insert(key, names);
            }
        }
        for d in devices {
            if let Some((vendor, device)) = cache.get(&(d.vendor_id.clone(), d.device_id.clone())) {
                d.vendor = vendor.clone();
                d.device = device.clone();
            }
        }
    }
}

/// Stream through a pci.ids database resolving only the requested (vendor, device) ids.
pub(crate) fn lookup_pci_ids(reader: impl BufRead, wanted: &HashSet<IdPair>) -> HashMap<IdPair, Names> {
    let vendors: HashSet<&str> = wanted.iter().map(|(v, _)| v.as_str()).collect();
    let mut out = HashMap::new();
    let mut current: Option<(String, String)> = None; // (vendor id, vendor name)
    for line in reader.lines().map_while(Result::ok) {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if line.starts_with("C ") {
            break; // device class section begins; vendors are done
        }
        if let Some(rest) = line.strip_prefix('\t') {
            if rest.starts_with('\t') {
                continue; // subsystem line
            }
            let Some((vid, vname)) = &current else { continue };
            if let Some((did, dname)) = rest.split_once("  ") {
                let key = (vid.clone(), did.trim().to_lowercase());
                if wanted.contains(&key) {
                    out.insert(key, (Some(vname.clone()), Some(dname.trim().to_string())));
                }
            }
        } else if let Some((vid, vname)) = line.split_once("  ") {
            let vid = vid.trim().to_lowercase();
            current = vendors.contains(vid.as_str()).then(|| (vid.clone(), vname.trim().to_string()));
            if let Some((v, name)) = &current {
                for key in wanted.iter().filter(|(wv, _)| wv == v) {
                    out.entry(key.clone()).or_insert((Some(name.clone()), None));
                }
            }
        }
    }
    out
}

/// PCI class (2 hex digits) or class+subclass (4 hex digits) to a readable name.
pub fn pci_class_name(code: &str) -> &'static str {
    match code {
        "0100" => "SCSI storage controller",
        "0101" => "IDE interface",
        "0106" => "SATA controller",
        "0107" => "Serial Attached SCSI controller",
        "0108" => "Non-Volatile memory controller",
        "0200" => "Ethernet controller",
        "0280" => "Network controller",
        "0300" => "VGA compatible controller",
        "0302" => "3D controller",
        "0380" => "Display controller",
        "0401" => "Multimedia audio controller",
        "0403" => "Audio device",
        "0480" => "Multimedia controller",
        "0600" => "Host bridge",
        "0601" => "ISA bridge",
        "0604" => "PCI bridge",
        "0c03" => "USB controller",
        "0c05" => "SMBus",
        "0d11" => "Bluetooth",
        "0d80" => "Wireless controller",
        _ => match code.get(..2).unwrap_or("") {
            "00" => "Unclassified device",
            "01" => "Mass storage controller",
            "02" => "Network controller",
            "03" => "Display controller",
            "04" => "Multimedia controller",
            "05" => "Memory controller",
            "06" => "Bridge",
            "07" => "Communication controller",
            "08" => "Generic system peripheral",
            "09" => "Input device controller",
            "0a" => "Docking station",
            "0b" => "Processor",
            "0c" => "Serial bus controller",
            "0d" => "Wireless controller",
            "0e" => "Intelligent controller",
            "0f" => "Satellite communications controller",
            "10" => "Encryption controller",
            "11" => "Signal processing controller",
            "12" => "Processing accelerator",
            "13" => "Non-essential instrumentation",
            "40" => "Coprocessor",
            _ => "Unknown device",
        },
    }
}

fn usb_devices(root: &Sysroot) -> Vec<UsbDevice> {
    let entries = root.list("/sys/bus/usb/devices");
    entries
        .iter()
        .filter(|n| !n.contains(':') && !n.starts_with("usb"))
        .map(|name| {
            let base = format!("/sys/bus/usb/devices/{name}");
            let prefix = format!("{name}:");
            let mut drivers: Vec<String> = entries
                .iter()
                .filter(|e| e.starts_with(&prefix))
                .filter_map(|e| root.link_name(format!("/sys/bus/usb/devices/{e}/driver")))
                .collect();
            drivers.sort();
            drivers.dedup();
            UsbDevice {
                path: name.clone(),
                vendor_id: root.read_trim(format!("{base}/idVendor")).unwrap_or_default(),
                product_id: root.read_trim(format!("{base}/idProduct")).unwrap_or_default(),
                manufacturer: root.read_trim(format!("{base}/manufacturer")),
                product: root.read_trim(format!("{base}/product")),
                drivers,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &str = "\
# pci.ids sample
8086  Intel Corporation
\t2723  Wi-Fi 6 AX200
\t\t8086 0084  Wi-Fi 6 AX200NGW
\t9a49  TigerLake-LP GT2 [Iris Xe Graphics]
10ec  Realtek Semiconductor Co., Ltd.
\t8168  RTL8111/8168/8211/8411 PCI Express Gigabit Ethernet Controller
C 00  Unclassified device
\t00  Non-VGA unclassified device
";

    #[test]
    fn looks_up_only_wanted_ids() {
        let wanted: HashSet<IdPair> =
            [("8086".to_string(), "2723".to_string()), ("10ec".to_string(), "ffff".to_string())].into();
        let found = lookup_pci_ids(IDS.as_bytes(), &wanted);
        assert_eq!(
            found[&("8086".into(), "2723".into())],
            (Some("Intel Corporation".into()), Some("Wi-Fi 6 AX200".into()))
        );
        // Unknown device of a known vendor still gets the vendor name.
        assert_eq!(found[&("10ec".into(), "ffff".into())], (Some("Realtek Semiconductor Co., Ltd.".into()), None));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn class_names() {
        assert_eq!(pci_class_name("0280"), "Network controller");
        assert_eq!(pci_class_name("0281"), "Network controller");
        assert_eq!(pci_class_name("0403"), "Audio device");
        assert_eq!(pci_class_name("zz"), "Unknown device");
    }
}
