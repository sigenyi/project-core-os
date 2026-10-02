use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// How dangerous an action is. Ordered: `Observe < Low < Medium < High`.
///
/// The Guardian's policy compares an action's risk against the configured
/// auto-approval ceiling; anything above it needs explicit human confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Read-only: inspects state, changes nothing.
    Observe,
    /// Minor and trivially reversible (volume, restarting a service).
    Low,
    /// Changes persistent configuration (enabling services, hostname, Wi-Fi).
    Medium,
    /// Can break the system or lose data (packages, swap, power state).
    High,
}

impl Risk {
    pub const ALL: [Risk; 4] = [Risk::Observe, Risk::Low, Risk::Medium, Risk::High];

    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Observe => "observe",
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

impl fmt::Display for Risk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Risk {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Risk::ALL
            .into_iter()
            .find(|r| r.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown risk level {s:?} (expected observe|low|medium|high)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_and_parsing() {
        assert!(Risk::Observe < Risk::Low && Risk::Low < Risk::Medium && Risk::Medium < Risk::High);
        assert_eq!("HIGH".parse::<Risk>().unwrap(), Risk::High);
        assert!("extreme".parse::<Risk>().is_err());
        assert_eq!(serde_json::to_string(&Risk::Medium).unwrap(), "\"medium\"");
    }
}
