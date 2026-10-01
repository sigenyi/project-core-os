use super::Collector;
use crate::snapshot::{Snapshot, ThermalZone};
use crate::sysroot::Sysroot;

pub struct ThermalCollector;

impl Collector for ThermalCollector {
    fn name(&self) -> &'static str {
        "thermal"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        snap.thermal = root
            .list("/sys/class/thermal")
            .into_iter()
            .filter(|z| z.starts_with("thermal_zone"))
            .filter_map(|zone| {
                let base = format!("/sys/class/thermal/{zone}");
                let millideg = root.read_i64(format!("{base}/temp"))?;
                let temp_c = millideg as f32 / 1000.0;
                // Disabled zones report 0 or absurd values.
                (temp_c > 0.0 && temp_c < 150.0)
                    .then(|| ThermalZone { name: root.read_trim(format!("{base}/type")).unwrap_or(zone), temp_c })
            })
            .collect();
        Ok(())
    }
}
