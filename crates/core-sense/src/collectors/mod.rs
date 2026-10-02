//! Collectors: each fills one part of the [`Snapshot`].
//!
//! A collector is a small, independent probe. New sources of perception (for example
//! eBPF-backed event counters) are added by implementing [`Collector`] and registering
//! it; nothing else in the pipeline changes.

mod audio;
mod cpu;
mod devices;
mod host;
mod kernel_log;
mod memory;
mod network;
mod power;
mod services;
mod storage;
mod thermal;

pub use audio::AudioCollector;
pub use cpu::CpuCollector;
pub use devices::{DevicesCollector, pci_class_name};
pub use host::HostCollector;
pub use kernel_log::{KernelLogCollector, parse_kmsg};
pub use memory::MemoryCollector;
pub use network::NetworkCollector;
pub use power::PowerCollector;
pub use services::ServicesCollector;
pub use storage::StorageCollector;
pub use thermal::ThermalCollector;

use crate::snapshot::Snapshot;
use crate::sysroot::Sysroot;

pub trait Collector: Send + Sync {
    /// Stable identifier, used in error reports.
    fn name(&self) -> &'static str;

    /// Fill the relevant part of `snapshot`. Returning an error records it in
    /// `snapshot.errors`; whatever was filled before the error is kept.
    fn collect(&self, root: &Sysroot, snapshot: &mut Snapshot) -> Result<(), String>;
}

/// The standard set of collectors.
pub fn default_collectors() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(HostCollector),
        Box::new(CpuCollector),
        Box::new(MemoryCollector),
        Box::new(StorageCollector),
        Box::new(NetworkCollector),
        Box::new(DevicesCollector::default()),
        Box::new(AudioCollector),
        Box::new(PowerCollector),
        Box::new(ThermalCollector),
        Box::new(ServicesCollector::systemctl()),
        Box::new(KernelLogCollector::default()),
    ]
}
