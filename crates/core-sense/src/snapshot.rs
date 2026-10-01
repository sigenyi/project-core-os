//! The telemetry snapshot: everything C.O.R.E. knows about the machine at one instant.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub schema: u32,
    pub collected_at: String,
    pub collected_unix: u64,
    pub host: Host,
    pub cpu: Cpu,
    pub memory: Memory,
    pub storage: Vec<Filesystem>,
    pub block_devices: Vec<BlockDevice>,
    pub network: Network,
    pub devices: Devices,
    pub audio: Audio,
    pub power: Power,
    pub thermal: Vec<ThermalZone>,
    pub services: Services,
    pub kernel_log: KernelLog,
    /// Derived findings, most severe first.
    pub insights: Vec<Insight>,
    /// Collectors that failed, with the reason. Partial snapshots are normal.
    pub errors: Vec<CollectorError>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Host {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub uptime_secs: u64,
    pub timezone: Option<String>,
    /// Hardware vendor and model from DMI, e.g. "LENOVO ThinkPad X1".
    pub product: Option<String>,
    /// Hypervisor if running virtualised (qemu, kvm, virtualbox, vmware, hyper-v).
    pub virtualization: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cpu {
    pub model: String,
    pub logical_cores: u32,
    pub load: [f32; 3],
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub total_mb: u64,
    pub available_mb: u64,
    pub swap_total_mb: u64,
    pub swap_free_mb: u64,
}

impl Memory {
    pub fn used_pct(&self) -> u8 {
        if self.total_mb == 0 {
            return 0;
        }
        (100 - (self.available_mb * 100 / self.total_mb).min(100)) as u8
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filesystem {
    pub mount: String,
    pub device: String,
    pub fs_type: String,
    pub read_only: bool,
    pub size_mb: Option<u64>,
    pub avail_mb: Option<u64>,
    pub used_pct: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockDevice {
    pub name: String,
    pub size_mb: u64,
    pub removable: bool,
    pub rotational: bool,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceKind {
    Ethernet,
    Wireless,
    Loopback,
    Bridge,
    Tunnel,
    #[default]
    Virtual,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetInterface {
    pub name: String,
    pub kind: InterfaceKind,
    /// Kernel operstate: up, down, dormant, unknown, lowerlayerdown.
    pub state: String,
    pub carrier: Option<bool>,
    pub mac: Option<String>,
    pub driver: Option<String>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
}

impl NetInterface {
    /// Physical (ethernet/wireless) interface with a link.
    pub fn has_link(&self) -> bool {
        matches!(self.kind, InterfaceKind::Ethernet | InterfaceKind::Wireless)
            && (self.state == "up" || self.carrier == Some(true))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    pub interfaces: Vec<NetInterface>,
    pub default_gateway: Option<String>,
    pub default_interface: Option<String>,
    pub dns_servers: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PciDevice {
    pub slot: String,
    pub class_code: String,
    pub class_name: String,
    pub vendor_id: String,
    pub device_id: String,
    pub vendor: Option<String>,
    pub device: Option<String>,
    pub driver: Option<String>,
}

impl PciDevice {
    /// Human label: device name if known, else vendor + ids.
    pub fn label(&self) -> String {
        match (&self.vendor, &self.device) {
            (Some(v), Some(d)) => format!("{v} {d}"),
            (Some(v), None) => format!("{v} [{}:{}]", self.vendor_id, self.device_id),
            _ => format!("[{}:{}]", self.vendor_id, self.device_id),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsbDevice {
    pub path: String,
    pub vendor_id: String,
    pub product_id: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub drivers: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Devices {
    pub pci: Vec<PciDevice>,
    pub usb: Vec<UsbDevice>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundCard {
    pub index: u32,
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub cards: Vec<SoundCard>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Battery {
    pub name: String,
    pub capacity_pct: Option<u8>,
    /// Charging, Discharging, Full, Not charging, Unknown.
    pub status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Power {
    pub ac_online: Option<bool>,
    pub batteries: Vec<Battery>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThermalZone {
    pub name: String,
    pub temp_c: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Services {
    /// False when the service manager could not be queried.
    pub checked: bool,
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KernelMessage {
    /// syslog level 0 (emerg) .. 7 (debug).
    pub level: u8,
    /// Seconds since boot.
    pub uptime_secs: f64,
    pub message: String,
    /// How many identical messages were collapsed into this one.
    pub repeats: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KernelLog {
    pub available: bool,
    /// Most recent notable messages (errors, and warnings about firmware/drivers).
    pub notable: Vec<KernelMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    pub severity: Severity,
    pub subsystem: String,
    pub message: String,
    /// A suggested next step for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectorError {
    pub collector: String,
    pub error: String,
}
