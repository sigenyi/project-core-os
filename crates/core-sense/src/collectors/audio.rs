use super::Collector;
use crate::snapshot::{Snapshot, SoundCard};
use crate::sysroot::Sysroot;

pub struct AudioCollector;

impl Collector for AudioCollector {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        // No /proc/asound at all means the ALSA core is not loaded: zero cards.
        snap.audio.cards = root.read("/proc/asound/cards").map(|t| parse_cards(&t)).unwrap_or_default();
        Ok(())
    }
}

/// Parse `/proc/asound/cards`:
/// ` 0 [PCH            ]: HDA-Intel - HDA Intel PCH`
pub(crate) fn parse_cards(text: &str) -> Vec<SoundCard> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (index, rest) = line.split_once(' ')?;
            let index: u32 = index.parse().ok()?;
            let rest = rest.trim_start().strip_prefix('[')?;
            let (id, rest) = rest.split_once(']')?;
            let desc = rest.trim_start().strip_prefix(':')?.trim();
            let name = desc.split_once(" - ").map(|(_, n)| n).unwrap_or(desc);
            Some(SoundCard { index, id: id.trim().to_string(), name: name.trim().to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cards() {
        let text = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
                      HDA Intel PCH at 0xf7f10000 irq 33
 1 [NVidia         ]: HDA-Intel - HDA NVidia
                      HDA NVidia at 0xf7080000 irq 17
";
        let cards = parse_cards(text);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0], SoundCard { index: 0, id: "PCH".into(), name: "HDA Intel PCH".into() });
        assert_eq!(cards[1].id, "NVidia");
        assert!(parse_cards("--- no soundcards ---").is_empty());
    }
}
