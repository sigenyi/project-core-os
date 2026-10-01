//! The action catalog: the complete, closed vocabulary of things the AI may ask for.
//!
//! This table is the single source of truth from which C.O.R.E. derives:
//! * the GBNF grammar that constrains the model's sampling ([`crate::grammar`]),
//! * the tool documentation embedded in the system prompt,
//! * the generic argument checks performed before typed validation ([`crate::action`]).
//!
//! Adding a capability means adding an entry here, a variant to [`crate::Action`], and a
//! handler in whichever component executes it. Nothing else needs to change.

use serde::Serialize;

use crate::choice::{HardwareBus, LogPriority, ProcessSort, ServiceFilter, Signal, TelemetrySection};
use crate::risk::Risk;

/// The value type of one action parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Free text for the user to read.
    Text {
        max_len: usize,
    },
    /// A credential. Redacted in audit logs and confirmations.
    Secret {
        min_len: usize,
        max_len: usize,
    },
    Integer {
        min: i64,
        max: i64,
    },
    Boolean,
    /// One of a fixed set of strings.
    Choice(&'static [&'static str]),
    ServiceName,
    PackageName,
    PackageQuery,
    KernelModule,
    Path,
    Hostname,
    HostTarget,
    Timezone,
    Ssid,
    Interface,
    Program,
    /// A list of program arguments.
    ArgList {
        max_items: usize,
    },
}

impl ParamKind {
    /// Short human/model readable type description used in prompt docs.
    pub fn describe(&self) -> String {
        match self {
            ParamKind::Text { max_len } => format!("text, max {max_len} chars"),
            ParamKind::Secret { min_len, max_len } => format!("secret, {min_len}-{max_len} chars"),
            ParamKind::Integer { min, max } => format!("integer {min}..{max}"),
            ParamKind::Boolean => "true|false".into(),
            ParamKind::Choice(values) => values.join("|"),
            ParamKind::ServiceName => "systemd unit name".into(),
            ParamKind::PackageName => "package name".into(),
            ParamKind::PackageQuery => "search words".into(),
            ParamKind::KernelModule => "kernel module name".into(),
            ParamKind::Path => "absolute path".into(),
            ParamKind::Hostname => "hostname".into(),
            ParamKind::HostTarget => "hostname or IP".into(),
            ParamKind::Timezone => "IANA zone like Europe/Paris".into(),
            ParamKind::Ssid => "Wi-Fi network name".into(),
            ParamKind::Interface => "network interface name".into(),
            ParamKind::Program => "program name".into(),
            ParamKind::ArgList { max_items } => format!("list of up to {max_items} strings"),
        }
    }

    pub fn is_secret(&self) -> bool {
        matches!(self, ParamKind::Secret { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub kind: ParamKind,
    pub required: bool,
    pub doc: &'static str,
}

/// Which component carries out an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Executor {
    /// Handled inside the unprivileged agent (conversation, telemetry, user programs).
    Agent,
    /// Forwarded to the privileged Guardian daemon.
    Guardian,
}

/// Grouping used to organise prompt documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Conversation,
    Inspect,
    Services,
    Packages,
    Network,
    Hardware,
    System,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Conversation,
        Category::Inspect,
        Category::Services,
        Category::Packages,
        Category::Network,
        Category::Hardware,
        Category::System,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::Conversation => "Conversation",
            Category::Inspect => "Inspect (read-only)",
            Category::Services => "Services",
            Category::Packages => "Packages",
            Category::Network => "Network",
            Category::Hardware => "Hardware",
            Category::System => "System",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub params: &'static [ParamSpec],
    pub risk: Risk,
    pub executor: Executor,
    pub category: Category,
    /// Ends the agent's turn (the user speaks next).
    pub terminal: bool,
    /// Example `args` object, used in documentation and tests.
    pub example: &'static str,
}

impl ActionSpec {
    pub fn param(&self, name: &str) -> Option<&'static ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }
}

const fn req(name: &'static str, kind: ParamKind, doc: &'static str) -> ParamSpec {
    ParamSpec { name, kind, required: true, doc }
}

const fn opt(name: &'static str, kind: ParamKind, doc: &'static str) -> ParamSpec {
    ParamSpec { name, kind, required: false, doc }
}

const SERVICE: ParamSpec = req("service", ParamKind::ServiceName, "unit, e.g. bluetooth or sshd.service");
const PACKAGE: ParamSpec = req("package", ParamKind::PackageName, "exact package name");
const MODULE: ParamSpec = req("module", ParamKind::KernelModule, "module name, e.g. iwlwifi");
const LINES: ParamSpec = opt("lines", ParamKind::Integer { min: 1, max: 500 }, "how many lines (default 50)");

macro_rules! action {
    (
        $name:literal, $cat:ident, $exec:ident, $risk:ident, $summary:literal,
        params: [$($p:expr),* $(,)?], example: $example:literal $(, terminal: $term:literal)?
    ) => {
        ActionSpec {
            name: $name,
            summary: $summary,
            params: &[$($p),*],
            risk: Risk::$risk,
            executor: Executor::$exec,
            category: Category::$cat,
            terminal: action!(@term $($term)?),
            example: $example,
        }
    };
    (@term) => { false };
    (@term $t:literal) => { $t };
}

/// Every action C.O.R.E. understands.
pub static CATALOG: &[ActionSpec] = &[
    // ---- conversation -------------------------------------------------------------
    action!("respond", Conversation, Agent, Observe,
        "Reply to the user and end your turn. Use it to answer, to report a finished task, or to explain why something cannot be done.",
        params: [req("message", ParamKind::Text { max_len: 1200 }, "what to tell the user; plain text, concise")],
        example: r#"{"message":"Done. Bluetooth is running again."}"#, terminal: true),
    action!("ask_user", Conversation, Agent, Observe,
        "Ask the user a clarifying question and wait for the answer. Only when the request is genuinely ambiguous.",
        params: [req("question", ParamKind::Text { max_len: 400 }, "one short question")],
        example: r#"{"question":"Which Wi-Fi network should I connect to?"}"#, terminal: true),
    action!("get_telemetry", Inspect, Agent, Observe,
        "Show detailed live system state for one section.",
        params: [req("section", ParamKind::Choice(TelemetrySection::ALL), "which section")],
        example: r#"{"section":"network"}"#),
    action!("launch_program", System, Agent, Low,
        "Open an interactive console program for the user; returns when they quit it.",
        params: [
            req("program", ParamKind::Program, "program name, e.g. htop, nano, w3m"),
            opt("args", ParamKind::ArgList { max_items: 16 }, "command line arguments"),
        ],
        example: r#"{"program":"nano","args":["/home/core/notes.txt"]}"#),
    // ---- inspect -------------------------------------------------------------------
    action!("list_directory", Inspect, Guardian, Observe,
        "List the entries of a directory.",
        params: [req("path", ParamKind::Path, "directory")],
        example: r#"{"path":"/etc/systemd/network"}"#),
    action!("read_file", Inspect, Guardian, Observe,
        "Read a text file (config files, logs). Secrets such as /etc/shadow are refused.",
        params: [
            req("path", ParamKind::Path, "file"),
            LINES,
            opt("tail", ParamKind::Boolean, "read the last lines instead of the first"),
        ],
        example: r#"{"path":"/etc/fstab"}"#),
    action!("read_logs", Inspect, Guardian, Observe,
        "Read the system journal, optionally for one unit and/or minimum priority.",
        params: [
            opt("unit", ParamKind::ServiceName, "only this unit"),
            opt("priority", ParamKind::Choice(LogPriority::ALL), "minimum priority"),
            LINES,
        ],
        example: r#"{"unit":"NetworkManager","priority":"warning","lines":40}"#),
    action!("read_kernel_log", Inspect, Guardian, Observe,
        "Read kernel messages (dmesg): driver, firmware and hardware errors.",
        params: [LINES, opt("errors_only", ParamKind::Boolean, "only errors and worse")],
        example: r#"{"errors_only":true}"#),
    action!("service_status", Services, Guardian, Observe,
        "Show whether a service is running, plus its recent log lines.",
        params: [SERVICE],
        example: r#"{"service":"bluetooth"}"#),
    action!("list_services", Services, Guardian, Observe,
        "List services by state.",
        params: [opt("state", ParamKind::Choice(ServiceFilter::ALL), "default running")],
        example: r#"{"state":"failed"}"#),
    action!("disk_usage", Inspect, Guardian, Observe,
        "Show free and used space of mounted filesystems.",
        params: [],
        example: r#"{}"#),
    action!("list_block_devices", Hardware, Guardian, Observe,
        "List disks and partitions with sizes, filesystems and mount points.",
        params: [],
        example: r#"{}"#),
    action!("list_hardware", Hardware, Guardian, Observe,
        "List PCI or USB devices with their kernel drivers.",
        params: [req("bus", ParamKind::Choice(HardwareBus::ALL), "which bus")],
        example: r#"{"bus":"pci"}"#),
    action!("list_processes", Inspect, Guardian, Observe,
        "List the top processes by CPU or memory use.",
        params: [
            opt("sort_by", ParamKind::Choice(ProcessSort::ALL), "default cpu"),
            opt("limit", ParamKind::Integer { min: 1, max: 50 }, "default 15"),
        ],
        example: r#"{"sort_by":"memory","limit":10}"#),
    action!("network_status", Network, Guardian, Observe,
        "Show interfaces, IP addresses, routes and DNS servers.",
        params: [],
        example: r#"{}"#),
    action!("wifi_scan", Network, Guardian, Observe,
        "Scan for Wi-Fi networks in range.",
        params: [],
        example: r#"{}"#),
    action!("ping_host", Network, Guardian, Observe,
        "Test reachability of a host.",
        params: [
            req("host", ParamKind::HostTarget, "hostname or IP"),
            opt("count", ParamKind::Integer { min: 1, max: 10 }, "packets, default 3"),
        ],
        example: r#"{"host":"1.1.1.1"}"#),
    action!("search_packages", Packages, Guardian, Observe,
        "Search the package repositories.",
        params: [req("query", ParamKind::PackageQuery, "keywords")],
        example: r#"{"query":"web browser"}"#),
    action!("package_info", Packages, Guardian, Observe,
        "Show whether a package is installed and its details.",
        params: [PACKAGE],
        example: r#"{"package":"firefox"}"#),
    // ---- low risk ------------------------------------------------------------------
    action!("set_volume", Hardware, Guardian, Low,
        "Set the main audio output volume.",
        params: [req("percent", ParamKind::Integer { min: 0, max: 150 }, "volume percent")],
        example: r#"{"percent":40}"#),
    action!("set_mute", Hardware, Guardian, Low,
        "Mute or unmute the main audio output.",
        params: [req("muted", ParamKind::Boolean, "true to mute")],
        example: r#"{"muted":false}"#),
    action!("set_brightness", Hardware, Guardian, Low,
        "Set the screen backlight brightness.",
        params: [req("percent", ParamKind::Integer { min: 1, max: 100 }, "brightness percent")],
        example: r#"{"percent":70}"#),
    action!("restart_service", Services, Guardian, Low,
        "Restart a service.",
        params: [SERVICE],
        example: r#"{"service":"wpa_supplicant"}"#),
    action!("start_service", Services, Guardian, Low,
        "Start a stopped service.",
        params: [SERVICE],
        example: r#"{"service":"bluetooth"}"#),
    // ---- medium risk ---------------------------------------------------------------
    action!("stop_service", Services, Guardian, Medium,
        "Stop a running service.",
        params: [SERVICE],
        example: r#"{"service":"cups"}"#),
    action!("enable_service", Services, Guardian, Medium,
        "Make a service start at boot.",
        params: [SERVICE, opt("now", ParamKind::Boolean, "also start it now")],
        example: r#"{"service":"bluetooth","now":true}"#),
    action!("disable_service", Services, Guardian, Medium,
        "Stop a service from starting at boot.",
        params: [SERVICE, opt("now", ParamKind::Boolean, "also stop it now")],
        example: r#"{"service":"cups","now":true}"#),
    action!("kill_process", System, Guardian, Medium,
        "Send a signal to a process.",
        params: [
            req("pid", ParamKind::Integer { min: 2, max: 4_194_304 }, "process id"),
            opt("signal", ParamKind::Choice(Signal::ALL), "default term"),
        ],
        example: r#"{"pid":4242,"signal":"term"}"#),
    action!("load_kernel_module", Hardware, Guardian, Medium,
        "Load a kernel module (driver).",
        params: [MODULE],
        example: r#"{"module":"btusb"}"#),
    action!("unload_kernel_module", Hardware, Guardian, Medium,
        "Unload a kernel module (driver).",
        params: [MODULE],
        example: r#"{"module":"btusb"}"#),
    action!("wifi_connect", Network, Guardian, Medium,
        "Connect to a Wi-Fi network.",
        params: [
            req("ssid", ParamKind::Ssid, "network name"),
            opt("passphrase", ParamKind::Secret { min_len: 8, max_len: 63 }, "password if the network is secured"),
        ],
        example: r#"{"ssid":"HomeNet","passphrase":"correct horse"}"#),
    action!("set_link", Network, Guardian, Medium,
        "Bring a network interface up or down.",
        params: [
            req("interface", ParamKind::Interface, "interface, e.g. wlan0"),
            req("up", ParamKind::Boolean, "true for up, false for down"),
        ],
        example: r#"{"interface":"wlan0","up":true}"#),
    action!("set_hostname", System, Guardian, Medium,
        "Change the machine's hostname.",
        params: [req("hostname", ParamKind::Hostname, "new hostname")],
        example: r#"{"hostname":"core-laptop"}"#),
    action!("set_timezone", System, Guardian, Medium,
        "Change the system time zone.",
        params: [req("timezone", ParamKind::Timezone, "IANA time zone")],
        example: r#"{"timezone":"Europe/Berlin"}"#),
    // ---- high risk -----------------------------------------------------------------
    action!("install_package", Packages, Guardian, High,
        "Install a package from the repositories.",
        params: [PACKAGE],
        example: r#"{"package":"w3m"}"#),
    action!("remove_package", Packages, Guardian, High,
        "Uninstall a package.",
        params: [PACKAGE],
        example: r#"{"package":"w3m"}"#),
    action!("update_system", Packages, Guardian, High,
        "Upgrade all installed packages.",
        params: [],
        example: r#"{}"#),
    action!("configure_swap", System, Guardian, High,
        "Create or resize the swap file (/swapfile); 0 removes it.",
        params: [req("size_mb", ParamKind::Integer { min: 0, max: 65536 }, "swap size in MiB")],
        example: r#"{"size_mb":4096}"#),
    action!("reboot", System, Guardian, High,
        "Restart the computer.",
        params: [],
        example: r#"{}"#),
    action!("poweroff", System, Guardian, High,
        "Shut down the computer.",
        params: [],
        example: r#"{}"#),
];

/// Look up an action by name.
pub fn find(name: &str) -> Option<&'static ActionSpec> {
    CATALOG.iter().find(|a| a.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn names_are_unique_snake_case() {
        let mut seen = HashSet::new();
        for spec in CATALOG {
            assert!(seen.insert(spec.name), "duplicate action {}", spec.name);
            assert!(spec.name.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{}", spec.name);
            let mut params = HashSet::new();
            for p in spec.params {
                assert!(params.insert(p.name), "duplicate param {}.{}", spec.name, p.name);
            }
        }
    }

    #[test]
    fn examples_are_json_objects_with_known_params() {
        for spec in CATALOG {
            let value: serde_json::Value =
                serde_json::from_str(spec.example).unwrap_or_else(|e| panic!("{}: bad example: {e}", spec.name));
            let obj = value.as_object().expect("example must be an object");
            for key in obj.keys() {
                assert!(spec.param(key).is_some(), "{}: example uses unknown param {key}", spec.name);
            }
            for p in spec.params.iter().filter(|p| p.required) {
                assert!(obj.contains_key(p.name), "{}: example misses required {}", spec.name, p.name);
            }
        }
    }

    #[test]
    fn only_conversation_actions_are_terminal() {
        for spec in CATALOG {
            assert_eq!(spec.terminal, spec.category == Category::Conversation, "{}", spec.name);
        }
    }

    #[test]
    fn privileged_mutations_are_never_observe() {
        for spec in CATALOG {
            let mutating = !matches!(spec.category, Category::Conversation | Category::Inspect)
                && !spec.name.starts_with("list_")
                && !matches!(
                    spec.name,
                    "service_status"
                        | "network_status"
                        | "wifi_scan"
                        | "ping_host"
                        | "search_packages"
                        | "package_info"
                );
            if mutating {
                assert!(spec.risk > Risk::Observe, "{} mutates but is marked observe", spec.name);
            }
        }
    }
}
