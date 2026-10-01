//! Typed, validated actions.
//!
//! [`ValidatedAction::from_intent`] is the gate between untrusted model output and
//! code that does things. Everything past this point works with strongly typed values
//! whose invariants (charsets, ranges, normalised paths) are guaranteed by construction.

use std::fmt;

use serde_json::{Map, Value};

use crate::catalog::{self, ActionSpec, ParamKind};
use crate::choice::{HardwareBus, LogPriority, ProcessSort, ServiceFilter, Signal, TelemetrySection};
use crate::intent::Intent;
use crate::risk::Risk;
use crate::validate;

/// Why an intent was refused before execution. The `Display` text is written for the
/// model: it is fed back as an observation so the model can correct itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    UnknownAction(String),
    UnknownParam { action: &'static str, param: String },
    MissingParam { action: &'static str, param: &'static str },
    InvalidParam { action: &'static str, param: &'static str, reason: String },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::UnknownAction(name) => {
                write!(f, "unknown action {name:?}; only the documented actions exist")
            }
            ValidationError::UnknownParam { action, param } => {
                let known: Vec<_> = catalog::find(action)
                    .map(|s| s.params.iter().map(|p| p.name).collect())
                    .unwrap_or_default();
                if known.is_empty() {
                    write!(f, "{action} takes no arguments, but {param:?} was given")
                } else {
                    write!(f, "{action} has no argument {param:?} (arguments: {})", known.join(", "))
                }
            }
            ValidationError::MissingParam { action, param } => {
                write!(f, "{action} requires the argument {param:?}")
            }
            ValidationError::InvalidParam { action, param, reason } => {
                write!(f, "{action}.{param}: {reason}")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

macro_rules! validated_string {
    ($(#[$meta:meta])* $name:ident, $check:path) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                $check(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

validated_string!(ServiceName, validate::service_name);
validated_string!(PackageName, validate::package_name);
validated_string!(PackageQuery, validate::package_query);
validated_string!(KernelModule, validate::kernel_module);
validated_string!(Hostname, validate::hostname);
validated_string!(HostTarget, validate::host_target);
validated_string!(Timezone, validate::timezone);
validated_string!(Ssid, validate::ssid);
validated_string!(Interface, validate::interface);
validated_string!(ProgramName, validate::program);

/// An absolute, normalised path without `..` components.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SafePath(String);

impl SafePath {
    pub fn new(value: impl AsRef<str>) -> Result<Self, String> {
        validate::normalize_path(value.as_ref()).map(SafePath)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SafePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<std::path::Path> for SafePath {
    fn as_ref(&self) -> &std::path::Path {
        std::path::Path::new(&self.0)
    }
}

/// A credential. Never printed by `Debug`/`Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Every action C.O.R.E. can perform, with validated arguments and defaults applied.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Respond { message: String },
    AskUser { question: String },
    GetTelemetry { section: TelemetrySection },
    LaunchProgram { program: ProgramName, args: Vec<String> },
    ListDirectory { path: SafePath },
    ReadFile { path: SafePath, lines: u32, tail: bool },
    ReadLogs { unit: Option<ServiceName>, priority: Option<LogPriority>, lines: u32 },
    ReadKernelLog { lines: u32, errors_only: bool },
    ServiceStatus { service: ServiceName },
    ListServices { state: ServiceFilter },
    DiskUsage,
    ListBlockDevices,
    ListHardware { bus: HardwareBus },
    ListProcesses { sort_by: ProcessSort, limit: u32 },
    NetworkStatus,
    WifiScan,
    PingHost { host: HostTarget, count: u32 },
    SearchPackages { query: PackageQuery },
    PackageInfo { package: PackageName },
    SetVolume { percent: u32 },
    SetMute { muted: bool },
    SetBrightness { percent: u32 },
    RestartService { service: ServiceName },
    StartService { service: ServiceName },
    StopService { service: ServiceName },
    EnableService { service: ServiceName, now: bool },
    DisableService { service: ServiceName, now: bool },
    KillProcess { pid: u32, signal: Signal },
    LoadKernelModule { module: KernelModule },
    UnloadKernelModule { module: KernelModule },
    WifiConnect { ssid: Ssid, passphrase: Option<Secret> },
    SetLink { interface: Interface, up: bool },
    SetHostname { hostname: Hostname },
    SetTimezone { timezone: Timezone },
    InstallPackage { package: PackageName },
    RemovePackage { package: PackageName },
    UpdateSystem,
    ConfigureSwap { size_mb: u32 },
    Reboot,
    Poweroff,
}

pub const DEFAULT_LINES: u32 = 50;

impl Action {
    /// One-line, human readable description (used for confirmations and the console).
    pub fn describe(&self) -> String {
        use Action::*;
        match self {
            Respond { .. } => "Reply to the user".into(),
            AskUser { .. } => "Ask the user a question".into(),
            GetTelemetry { section } => format!("Inspect live {section} telemetry"),
            LaunchProgram { program, args } if args.is_empty() => format!("Open {program}"),
            LaunchProgram { program, args } => format!("Open {program} {}", args.join(" ")),
            ListDirectory { path } => format!("List directory {path}"),
            ReadFile { path, .. } => format!("Read file {path}"),
            ReadLogs { unit: Some(u), .. } => format!("Read the journal for {u}"),
            ReadLogs { unit: None, .. } => "Read the system journal".into(),
            ReadKernelLog { .. } => "Read the kernel log".into(),
            ServiceStatus { service } => format!("Check the status of {service}"),
            ListServices { state } => format!("List {state} services"),
            DiskUsage => "Check disk usage".into(),
            ListBlockDevices => "List disks and partitions".into(),
            ListHardware { bus } => format!("List {} devices", bus.as_str().to_uppercase()),
            ListProcesses { sort_by, .. } => format!("List top processes by {sort_by}"),
            NetworkStatus => "Check network status".into(),
            WifiScan => "Scan for Wi-Fi networks".into(),
            PingHost { host, .. } => format!("Ping {host}"),
            SearchPackages { query } => format!("Search packages for \"{query}\""),
            PackageInfo { package } => format!("Look up package {package}"),
            SetVolume { percent } => format!("Set volume to {percent}%"),
            SetMute { muted: true } => "Mute audio".into(),
            SetMute { muted: false } => "Unmute audio".into(),
            SetBrightness { percent } => format!("Set screen brightness to {percent}%"),
            RestartService { service } => format!("Restart service {service}"),
            StartService { service } => format!("Start service {service}"),
            StopService { service } => format!("Stop service {service}"),
            EnableService { service, now: true } => format!("Enable and start service {service}"),
            EnableService { service, now: false } => format!("Enable service {service} at boot"),
            DisableService { service, now: true } => format!("Disable and stop service {service}"),
            DisableService { service, now: false } => format!("Disable service {service} at boot"),
            KillProcess { pid, signal } => format!("Send SIG{} to process {pid}", signal.as_str().to_uppercase()),
            LoadKernelModule { module } => format!("Load kernel module {module}"),
            UnloadKernelModule { module } => format!("Unload kernel module {module}"),
            WifiConnect { ssid, .. } => format!("Connect to Wi-Fi network \"{ssid}\""),
            SetLink { interface, up: true } => format!("Bring interface {interface} up"),
            SetLink { interface, up: false } => format!("Bring interface {interface} down"),
            SetHostname { hostname } => format!("Change hostname to {hostname}"),
            SetTimezone { timezone } => format!("Change time zone to {timezone}"),
            InstallPackage { package } => format!("Install package {package}"),
            RemovePackage { package } => format!("Remove package {package}"),
            UpdateSystem => "Upgrade all packages".into(),
            ConfigureSwap { size_mb: 0 } => "Remove the swap file".into(),
            ConfigureSwap { size_mb } => format!("Set up a {size_mb} MiB swap file"),
            Reboot => "Reboot the computer".into(),
            Poweroff => "Power off the computer".into(),
        }
    }
}

/// An [`Action`] together with its catalog entry and normalised arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedAction {
    pub spec: &'static ActionSpec,
    pub action: Action,
    /// Arguments after normalisation (defaults are *not* filled in). Contains secrets.
    pub args: Map<String, Value>,
}

impl ValidatedAction {
    pub fn name(&self) -> &'static str {
        self.spec.name
    }

    pub fn risk(&self) -> Risk {
        self.spec.risk
    }

    pub fn describe(&self) -> String {
        self.action.describe()
    }

    /// Arguments with every secret replaced, safe for logs and displays.
    pub fn redacted_args(&self) -> Value {
        let mut out = self.args.clone();
        for p in self.spec.params.iter().filter(|p| p.kind.is_secret()) {
            if let Some(v) = out.get_mut(p.name) {
                *v = Value::String("<redacted>".into());
            }
        }
        Value::Object(out)
    }

    /// The normalised intent (secrets included) for forwarding to the Guardian.
    pub fn to_intent(&self) -> Intent {
        Intent { thought: None, action: self.spec.name.to_string(), args: self.args.clone() }
    }

    /// Validate an untrusted intent.
    pub fn from_intent(intent: &Intent) -> Result<Self, ValidationError> {
        let spec = catalog::find(intent.action.trim())
            .ok_or_else(|| ValidationError::UnknownAction(intent.action.clone()))?;
        let mut a = Args::new(spec, &intent.args)?;
        use Action::*;
        let action = match spec.name {
            "respond" => Respond { message: a.req_text("message")? },
            "ask_user" => AskUser { question: a.req_text("question")? },
            "get_telemetry" => GetTelemetry { section: a.req_choice("section", TelemetrySection::parse)? },
            "launch_program" => LaunchProgram {
                program: a.req_typed("program", ProgramName::new)?,
                args: a.opt_arg_list("args")?.unwrap_or_default(),
            },
            "list_directory" => ListDirectory { path: a.req_path("path")? },
            "read_file" => ReadFile {
                path: a.req_path("path")?,
                lines: a.opt_u32("lines")?.unwrap_or(DEFAULT_LINES),
                tail: a.opt_bool("tail")?.unwrap_or(false),
            },
            "read_logs" => ReadLogs {
                unit: a.opt_typed("unit", ServiceName::new)?,
                priority: a.opt_choice("priority", LogPriority::parse)?,
                lines: a.opt_u32("lines")?.unwrap_or(DEFAULT_LINES),
            },
            "read_kernel_log" => ReadKernelLog {
                lines: a.opt_u32("lines")?.unwrap_or(DEFAULT_LINES),
                errors_only: a.opt_bool("errors_only")?.unwrap_or(false),
            },
            "service_status" => ServiceStatus { service: a.service()? },
            "list_services" => ListServices {
                state: a.opt_choice("state", ServiceFilter::parse)?.unwrap_or(ServiceFilter::Running),
            },
            "disk_usage" => DiskUsage,
            "list_block_devices" => ListBlockDevices,
            "list_hardware" => ListHardware { bus: a.req_choice("bus", HardwareBus::parse)? },
            "list_processes" => ListProcesses {
                sort_by: a.opt_choice("sort_by", ProcessSort::parse)?.unwrap_or(ProcessSort::Cpu),
                limit: a.opt_u32("limit")?.unwrap_or(15),
            },
            "network_status" => NetworkStatus,
            "wifi_scan" => WifiScan,
            "ping_host" => PingHost {
                host: a.req_typed("host", HostTarget::new)?,
                count: a.opt_u32("count")?.unwrap_or(3),
            },
            "search_packages" => SearchPackages { query: a.req_typed("query", PackageQuery::new)? },
            "package_info" => PackageInfo { package: a.package()? },
            "set_volume" => SetVolume { percent: a.req_u32("percent")? },
            "set_mute" => SetMute { muted: a.req_bool("muted")? },
            "set_brightness" => SetBrightness { percent: a.req_u32("percent")? },
            "restart_service" => RestartService { service: a.service()? },
            "start_service" => StartService { service: a.service()? },
            "stop_service" => StopService { service: a.service()? },
            "enable_service" => EnableService {
                service: a.service()?,
                now: a.opt_bool("now")?.unwrap_or(false),
            },
            "disable_service" => DisableService {
                service: a.service()?,
                now: a.opt_bool("now")?.unwrap_or(false),
            },
            "kill_process" => KillProcess {
                pid: a.req_u32("pid")?,
                signal: a.opt_choice("signal", Signal::parse)?.unwrap_or(Signal::Term),
            },
            "load_kernel_module" => LoadKernelModule { module: a.req_typed("module", KernelModule::new)? },
            "unload_kernel_module" => UnloadKernelModule { module: a.req_typed("module", KernelModule::new)? },
            "wifi_connect" => WifiConnect {
                ssid: a.req_typed("ssid", Ssid::new)?,
                passphrase: a.opt_secret("passphrase")?,
            },
            "set_link" => SetLink {
                interface: a.req_typed("interface", Interface::new)?,
                up: a.req_bool("up")?,
            },
            "set_hostname" => SetHostname { hostname: a.req_typed("hostname", Hostname::new)? },
            "set_timezone" => SetTimezone { timezone: a.req_typed("timezone", Timezone::new)? },
            "install_package" => InstallPackage { package: a.package()? },
            "remove_package" => RemovePackage { package: a.package()? },
            "update_system" => UpdateSystem,
            "configure_swap" => ConfigureSwap { size_mb: a.req_u32("size_mb")? },
            "reboot" => Reboot,
            "poweroff" => Poweroff,
            other => return Err(ValidationError::UnknownAction(other.to_string())),
        };
        Ok(ValidatedAction { spec, action, args: a.out })
    }
}

/// Argument accessor bound to one catalog entry. Performs JSON-type, range and
/// syntax checks and records the normalised value of every argument it reads.
struct Args<'a> {
    spec: &'static ActionSpec,
    input: &'a Map<String, Value>,
    out: Map<String, Value>,
}

impl<'a> Args<'a> {
    fn new(spec: &'static ActionSpec, input: &'a Map<String, Value>) -> Result<Self, ValidationError> {
        for key in input.keys() {
            if spec.param(key).is_none() {
                return Err(ValidationError::UnknownParam { action: spec.name, param: key.clone() });
            }
        }
        for p in spec.params.iter().filter(|p| p.required) {
            if input.get(p.name).is_none_or(Value::is_null) {
                return Err(ValidationError::MissingParam { action: spec.name, param: p.name });
            }
        }
        Ok(Args { spec, input, out: Map::new() })
    }

    fn param(&self, name: &'static str) -> &'static catalog::ParamSpec {
        self.spec
            .param(name)
            .unwrap_or_else(|| panic!("catalog entry {} lacks parameter {name}", self.spec.name))
    }

    fn invalid(&self, param: &'static str, reason: impl Into<String>) -> ValidationError {
        ValidationError::InvalidParam { action: self.spec.name, param, reason: reason.into() }
    }

    fn missing(&self, param: &'static str) -> ValidationError {
        ValidationError::MissingParam { action: self.spec.name, param }
    }

    fn raw(&self, name: &'static str) -> Option<&'a Value> {
        self.input.get(name).filter(|v| !v.is_null())
    }

    fn opt_str(&self, name: &'static str) -> Result<Option<&'a str>, ValidationError> {
        match self.raw(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(self.invalid(name, "must be a string")),
        }
    }

    fn opt_typed<T>(
        &mut self,
        name: &'static str,
        ctor: impl Fn(String) -> Result<T, String>,
    ) -> Result<Option<T>, ValidationError> {
        let Some(s) = self.opt_str(name)? else { return Ok(None) };
        let s = s.trim();
        let value = ctor(s.to_string()).map_err(|e| self.invalid(name, e))?;
        self.out.insert(name.into(), Value::String(s.to_string()));
        Ok(Some(value))
    }

    fn req_typed<T>(
        &mut self,
        name: &'static str,
        ctor: impl Fn(String) -> Result<T, String>,
    ) -> Result<T, ValidationError> {
        self.opt_typed(name, ctor)?.ok_or_else(|| self.missing(name))
    }

    fn service(&mut self) -> Result<ServiceName, ValidationError> {
        self.req_typed("service", ServiceName::new)
    }

    fn package(&mut self) -> Result<PackageName, ValidationError> {
        self.req_typed("package", PackageName::new)
    }

    fn req_path(&mut self, name: &'static str) -> Result<SafePath, ValidationError> {
        let raw = self.opt_str(name)?.ok_or_else(|| self.missing(name))?;
        let path = SafePath::new(raw.trim()).map_err(|e| self.invalid(name, e))?;
        self.out.insert(name.into(), Value::String(path.as_str().to_string()));
        Ok(path)
    }

    /// Free text is truncated rather than rejected: a slightly long reply is not
    /// worth a correction round-trip.
    fn req_text(&mut self, name: &'static str) -> Result<String, ValidationError> {
        let ParamKind::Text { max_len } = self.param(name).kind else {
            panic!("{}.{name} is not a text parameter", self.spec.name)
        };
        let raw = self.opt_str(name)?.ok_or_else(|| self.missing(name))?.trim();
        let mut text: String = raw.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect();
        if text.chars().count() > max_len {
            text = text.chars().take(max_len.saturating_sub(1)).collect::<String>() + "…";
        }
        validate::text(&text, max_len).map_err(|e| self.invalid(name, e))?;
        self.out.insert(name.into(), Value::String(text.clone()));
        Ok(text)
    }

    fn opt_secret(&mut self, name: &'static str) -> Result<Option<Secret>, ValidationError> {
        let Some(s) = self.opt_str(name)? else { return Ok(None) };
        if s.is_empty() {
            return Ok(None);
        }
        validate::wifi_passphrase(s).map_err(|e| self.invalid(name, e))?;
        self.out.insert(name.into(), Value::String(s.to_string()));
        Ok(Some(Secret(s.to_string())))
    }

    fn opt_choice<T>(&mut self, name: &'static str, parse: fn(&str) -> Option<T>) -> Result<Option<T>, ValidationError> {
        let Some(s) = self.opt_str(name)? else { return Ok(None) };
        let s = s.trim().to_ascii_lowercase();
        let value = parse(&s).ok_or_else(|| {
            let allowed = match self.param(name).kind {
                ParamKind::Choice(values) => values.join(", "),
                _ => String::new(),
            };
            self.invalid(name, format!("must be one of: {allowed}"))
        })?;
        self.out.insert(name.into(), Value::String(s));
        Ok(Some(value))
    }

    fn req_choice<T>(&mut self, name: &'static str, parse: fn(&str) -> Option<T>) -> Result<T, ValidationError> {
        self.opt_choice(name, parse)?.ok_or_else(|| self.missing(name))
    }

    /// Integers are accepted as JSON numbers, integral floats or numeric strings
    /// (backends without grammar support are sloppy), then range-checked.
    fn opt_int(&mut self, name: &'static str) -> Result<Option<i64>, ValidationError> {
        let Some(raw) = self.raw(name) else { return Ok(None) };
        let value = match raw {
            Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 1e15).map(|f| f as i64)),
            Value::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        }
        .ok_or_else(|| self.invalid(name, "must be an integer"))?;
        if let ParamKind::Integer { min, max } = self.param(name).kind {
            if value < min || value > max {
                return Err(self.invalid(name, format!("must be between {min} and {max}")));
            }
        }
        self.out.insert(name.into(), Value::from(value));
        Ok(Some(value))
    }

    fn opt_u32(&mut self, name: &'static str) -> Result<Option<u32>, ValidationError> {
        match self.opt_int(name)? {
            None => Ok(None),
            Some(v) => u32::try_from(v).map(Some).map_err(|_| self.invalid(name, "out of range")),
        }
    }

    fn req_u32(&mut self, name: &'static str) -> Result<u32, ValidationError> {
        self.opt_u32(name)?.ok_or_else(|| self.missing(name))
    }

    fn opt_bool(&mut self, name: &'static str) -> Result<Option<bool>, ValidationError> {
        let Some(raw) = self.raw(name) else { return Ok(None) };
        let value = match raw {
            Value::Bool(b) => Some(*b),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" => Some(true),
                "false" | "no" | "off" => Some(false),
                _ => None,
            },
            _ => None,
        }
        .ok_or_else(|| self.invalid(name, "must be true or false"))?;
        self.out.insert(name.into(), Value::Bool(value));
        Ok(Some(value))
    }

    fn req_bool(&mut self, name: &'static str) -> Result<bool, ValidationError> {
        self.opt_bool(name)?.ok_or_else(|| self.missing(name))
    }

    fn opt_arg_list(&mut self, name: &'static str) -> Result<Option<Vec<String>>, ValidationError> {
        let Some(raw) = self.raw(name) else { return Ok(None) };
        let ParamKind::ArgList { max_items } = self.param(name).kind else {
            panic!("{}.{name} is not an argument list", self.spec.name)
        };
        let items = raw.as_array().ok_or_else(|| self.invalid(name, "must be a list of strings"))?;
        if items.len() > max_items {
            return Err(self.invalid(name, format!("at most {max_items} items")));
        }
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let s = item.as_str().ok_or_else(|| self.invalid(name, "must be a list of strings"))?;
            validate::program_arg(s).map_err(|e| self.invalid(name, e))?;
            out.push(s.to_string());
        }
        self.out.insert(name.into(), Value::from(out.clone()));
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::catalog::CATALOG;

    fn validate(action: &str, args: Value) -> Result<ValidatedAction, ValidationError> {
        ValidatedAction::from_intent(&Intent::new(action, args))
    }

    #[test]
    fn every_catalog_example_validates() {
        for spec in CATALOG {
            let args: Value = serde_json::from_str(spec.example).unwrap();
            let v = validate(spec.name, args).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            assert_eq!(v.name(), spec.name);
            assert!(!v.describe().is_empty());
            // Normalised args survive a second validation unchanged.
            let again = ValidatedAction::from_intent(&v.to_intent()).unwrap();
            assert_eq!(again.args, v.args, "{}", spec.name);
        }
    }

    #[test]
    fn unknown_and_missing() {
        assert_eq!(validate("rm_rf", json!({})), Err(ValidationError::UnknownAction("rm_rf".into())));
        assert!(matches!(validate("reboot", json!({"force": true})), Err(ValidationError::UnknownParam { .. })));
        assert!(matches!(validate("restart_service", json!({})), Err(ValidationError::MissingParam { .. })));
        assert!(matches!(
            validate("restart_service", json!({"service": null})),
            Err(ValidationError::MissingParam { .. })
        ));
    }

    #[test]
    fn injection_attempts_are_rejected() {
        for bad in ["--now", "sshd; reboot", "$(id)", "a b", "../../etc"] {
            assert!(validate("restart_service", json!({"service": bad})).is_err(), "{bad}");
        }
        assert!(validate("install_package", json!({"package": "--overwrite=*"})).is_err());
        assert!(validate("read_file", json!({"path": "/etc/../etc/shadow"})).is_err());
        assert!(validate("ping_host", json!({"host": "-f"})).is_err());
    }

    #[test]
    fn ranges_and_types() {
        assert!(validate("set_volume", json!({"percent": 151})).is_err());
        assert!(validate("set_volume", json!({"percent": -1})).is_err());
        assert!(validate("set_volume", json!({"percent": "abc"})).is_err());
        let v = validate("set_volume", json!({"percent": "40"})).unwrap();
        assert_eq!(v.action, Action::SetVolume { percent: 40 });
        assert_eq!(v.args["percent"], json!(40));
        let v = validate("set_volume", json!({"percent": 40.0})).unwrap();
        assert_eq!(v.action, Action::SetVolume { percent: 40 });
        assert!(validate("kill_process", json!({"pid": 1})).is_err(), "pid 1 must be out of range");
        assert!(validate("set_mute", json!({"muted": "maybe"})).is_err());
    }

    #[test]
    fn defaults_are_applied() {
        let v = validate("read_logs", json!({})).unwrap();
        assert_eq!(v.action, Action::ReadLogs { unit: None, priority: None, lines: DEFAULT_LINES });
        let v = validate("kill_process", json!({"pid": 99})).unwrap();
        assert_eq!(v.action, Action::KillProcess { pid: 99, signal: Signal::Term });
    }

    #[test]
    fn paths_are_normalised() {
        let v = validate("list_directory", json!({"path": "/var//log/./"})).unwrap();
        assert_eq!(v.args["path"], json!("/var/log"));
    }

    #[test]
    fn long_text_is_truncated_not_rejected() {
        let v = validate("respond", json!({"message": "x".repeat(5000)})).unwrap();
        let Action::Respond { message } = v.action else { panic!() };
        assert_eq!(message.chars().count(), 1200);
        assert!(message.ends_with('…'));
    }

    #[test]
    fn secrets_are_redacted() {
        let v = validate("wifi_connect", json!({"ssid": "Home", "passphrase": "supersecret"})).unwrap();
        assert_eq!(v.redacted_args()["passphrase"], json!("<redacted>"));
        assert_eq!(v.to_intent().args["passphrase"], json!("supersecret"));
        assert!(!format!("{:?}", v.action).contains("supersecret"));
    }

    #[test]
    fn launch_program_args() {
        let v = validate("launch_program", json!({"program": "nano", "args": ["/tmp/x"]})).unwrap();
        assert_eq!(
            v.action,
            Action::LaunchProgram { program: ProgramName::new("nano").unwrap(), args: vec!["/tmp/x".into()] }
        );
        assert!(validate("launch_program", json!({"program": "/bin/sh"})).is_err());
        assert!(validate("launch_program", json!({"program": "nano", "args": "x"})).is_err());
        let too_many: Vec<String> = (0..17).map(|i| i.to_string()).collect();
        assert!(validate("launch_program", json!({"program": "nano", "args": too_many})).is_err());
    }

    #[test]
    fn error_messages_guide_the_model() {
        let e = validate("restart_service", json!({"name": "x"})).unwrap_err().to_string();
        assert!(e.contains("arguments: service"), "{e}");
        let e = validate("reboot", json!({"now": true})).unwrap_err().to_string();
        assert!(e.contains("takes no arguments"), "{e}");
    }
}
