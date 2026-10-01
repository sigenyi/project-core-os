use super::Collector;
use crate::snapshot::{Battery, Snapshot};
use crate::sysroot::Sysroot;

pub struct PowerCollector;

impl Collector for PowerCollector {
    fn name(&self) -> &'static str {
        "power"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        for supply in root.list("/sys/class/power_supply") {
            let base = format!("/sys/class/power_supply/{supply}");
            match root.read_trim(format!("{base}/type")).as_deref() {
                Some("Mains") => {
                    let online = root.read_trim(format!("{base}/online")).as_deref() == Some("1");
                    snap.power.ac_online = Some(snap.power.ac_online.unwrap_or(false) || online);
                }
                Some("Battery") if root.read_trim(format!("{base}/scope")).as_deref() != Some("Device") => {
                    snap.power.batteries.push(Battery {
                        capacity_pct: root.read_u64(format!("{base}/capacity")).map(|c| c.min(100) as u8),
                        status: root.read_trim(format!("{base}/status")).unwrap_or_else(|| "Unknown".into()),
                        name: supply,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }
}
