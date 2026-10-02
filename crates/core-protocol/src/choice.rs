//! Closed sets of string values ("enums") that actions accept.
//!
//! Each enum exposes `ALL`, the exact strings the model must emit; the catalog and
//! the grammar are generated from these lists so they can never drift apart.

macro_rules! choice_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [&'static str] = &[$($text),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

choice_enum!(
    /// Sections of the telemetry snapshot the agent can inspect on demand.
    TelemetrySection {
        Summary => "summary",
        Host => "host",
        Cpu => "cpu",
        Memory => "memory",
        Storage => "storage",
        Network => "network",
        Devices => "devices",
        Audio => "audio",
        Power => "power",
        Thermal => "thermal",
        Services => "services",
        KernelLog => "kernel_log",
        Insights => "insights",
    }
);

choice_enum!(
    /// syslog priorities understood by journalctl `-p`.
    LogPriority {
        Emerg => "emerg",
        Alert => "alert",
        Crit => "crit",
        Err => "err",
        Warning => "warning",
        Notice => "notice",
        Info => "info",
        Debug => "debug",
    }
);

choice_enum!(
    /// Which services `list_services` reports.
    ServiceFilter {
        Running => "running",
        Failed => "failed",
        Enabled => "enabled",
        All => "all",
    }
);

choice_enum!(
    ProcessSort {
        Cpu => "cpu",
        Memory => "memory",
    }
);

choice_enum!(
    /// Signals `kill_process` may send.
    Signal {
        Term => "term",
        Kill => "kill",
        Hup => "hup",
        Int => "int",
    }
);

choice_enum!(
    HardwareBus {
        Pci => "pci",
        Usb => "usb",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for s in Signal::ALL {
            assert_eq!(Signal::parse(s).unwrap().as_str(), *s);
        }
        assert_eq!(TelemetrySection::parse("kernel_log"), Some(TelemetrySection::KernelLog));
        assert_eq!(LogPriority::parse("fatal"), None);
    }
}
