//! Translate typed actions into concrete plans for the configured distribution.
//!
//! This is where distribution differences live (cpkg vs pacman vs apt, PipeWire vs
//! ALSA, systemd-networkd vs NetworkManager vs iwd). Every argument placed in a command has already passed the
//! validators in `core-protocol`; option parsing is additionally terminated with `--`
//! wherever the target tool supports it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use core_protocol::Action;
use core_protocol::choice::{HardwareBus, ProcessSort, ServiceFilter};

use crate::config::{AudioBackend, GuardianConfig, NetworkBackend, PackageManager};
use crate::plan::{CommandSpec, NativeOp, Plan, Step};

/// Facts about the running system the planner and policy need (abstracted for tests).
pub trait SystemProbe: Send + Sync {
    fn wireless_interfaces(&self) -> Vec<String>;
    /// Filesystem type holding `path` (e.g. "ext4", "btrfs").
    fn filesystem_type(&self, path: &Path) -> Option<String>;
    /// The canonical name of a unit, resolving aliases (`autovt@tty1` → `getty@tty1.service`).
    fn unit_id(&self, _unit: &str) -> Option<String> {
        None
    }
}

/// Reads the live system through sysfs/procfs and (read-only) systemctl.
pub struct LiveProbe {
    pub sysfs: PathBuf,
    pub procfs: PathBuf,
    pub systemctl: Option<PathBuf>,
}

impl SystemProbe for LiveProbe {
    fn wireless_interfaces(&self) -> Vec<String> {
        let net = self.sysfs.join("class/net");
        let mut out: Vec<String> = std::fs::read_dir(&net)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| net.join(n).join("wireless").exists() || net.join(n).join("phy80211").exists())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    fn filesystem_type(&self, path: &Path) -> Option<String> {
        let mounts = std::fs::read_to_string(self.procfs.join("mounts")).ok()?;
        mounts
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                (f.len() >= 3 && path.starts_with(f[1])).then(|| (f[1].len(), f[2].to_string()))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, fs)| fs)
    }

    fn unit_id(&self, unit: &str) -> Option<String> {
        let systemctl = self.systemctl.as_ref()?;
        let out = std::process::Command::new(systemctl)
            .args(["show", "--property=Id", "--value", "--", unit])
            .env_clear()
            .env("SYSTEMD_PAGER", "")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !id.is_empty()).then_some(id)
    }
}

pub struct Planner<'a> {
    config: &'a GuardianConfig,
    probe: &'a dyn SystemProbe,
}

/// Why Wi-Fi actions cannot be planned with systemd-networkd alone.
const NO_WIFI_DAEMON: &str = "Wi-Fi is not available: this system manages networks with systemd-networkd, and no Wi-Fi \
     daemon (iwd or wpa_supplicant) is configured. Wired networking is managed automatically.";

/// Exit codes of `systemctl status`: 0 running, 1-3 dead/failed/inactive (all informative).
const SYSTEMCTL_STATUS_OK: &[i32] = &[0, 1, 2, 3];

impl<'a> Planner<'a> {
    pub fn new(config: &'a GuardianConfig, probe: &'a dyn SystemProbe) -> Self {
        Planner { config, probe }
    }

    fn pkg_timeout(&self) -> Duration {
        Duration::from_secs(self.config.package_timeout_secs)
    }

    pub fn plan(&self, action: &Action) -> Result<Plan, String> {
        use Action::*;
        let plan = match action {
            Respond { .. }
            | AskUser { .. }
            | GetTelemetry { .. }
            | LaunchProgram { .. }
            | ListDirectory { .. }
            | ReadFile { .. } => {
                return Err("this action is handled by the agent, not the Guardian".into());
            }

            ReadLogs { unit, priority, lines } => {
                let mut c = CommandSpec::new("journalctl", ["--no-pager", "-o", "short-iso", "-b", "-n"])
                    .arg(lines.to_string());
                if let Some(p) = priority {
                    c = c.arg("-p").arg(p.as_str());
                }
                if let Some(u) = unit {
                    c = c.arg("-u").arg(u.as_str());
                }
                Plan::run(c)
            }
            ReadKernelLog { lines, errors_only } => {
                let mut c = CommandSpec::new("dmesg", ["--color=never", "--ctime"]);
                if *errors_only {
                    c = c.arg("--level=emerg,alert,crit,err");
                }
                Plan::run(c.tail(*lines as usize))
            }
            ServiceStatus { service } => Plan::run(
                CommandSpec::new("systemctl", ["status", "--no-pager", "--lines=15", "--"])
                    .arg(service.as_str())
                    .success_codes(SYSTEMCTL_STATUS_OK),
            ),
            ListServices { state } => {
                let args: &[&str] = match state {
                    ServiceFilter::Running => &["list-units", "--type=service", "--state=running"],
                    ServiceFilter::Failed => &["list-units", "--state=failed"],
                    ServiceFilter::Enabled => &["list-unit-files", "--type=service", "--state=enabled"],
                    ServiceFilter::All => &["list-units", "--type=service", "--all"],
                };
                Plan::run(
                    CommandSpec::new("systemctl", args.iter().copied()).arg("--no-pager").arg("--plain").head(120),
                )
            }
            DiskUsage => Plan::run(CommandSpec::new(
                "df",
                ["-h", "-x", "tmpfs", "-x", "devtmpfs", "-x", "efivarfs", "-x", "squashfs"],
            )),
            ListBlockDevices => Plan::run(CommandSpec::new("lsblk", ["-o", "NAME,SIZE,TYPE,FSTYPE,MOUNTPOINTS,MODEL"])),
            ListHardware { bus: HardwareBus::Pci } => Plan::run(CommandSpec::new("lspci", ["-nnk"])),
            ListHardware { bus: HardwareBus::Usb } => Plan::run(CommandSpec::new("lsusb", Vec::<String>::new())),
            ListProcesses { sort_by, limit } => {
                let sort = match sort_by {
                    ProcessSort::Cpu => "--sort=-%cpu",
                    ProcessSort::Memory => "--sort=-%mem",
                };
                Plan::run(
                    CommandSpec::new("ps", ["-eo", "pid,user,%cpu,%mem,etime,comm", sort]).head(*limit as usize + 1),
                )
            }
            NetworkStatus => match self.config.system.network {
                // networkd's own view (link state, "routable") and resolved's DNS
                // servers; /etc/resolv.conf only names resolved's stub there.
                NetworkBackend::Networkd => Plan::run(CommandSpec::new("networkctl", ["list", "--no-pager"]))
                    .then_run(CommandSpec::new("ip", ["route"]))
                    .then_run(CommandSpec::new("resolvectl", ["status", "--no-pager"]).head(40)),
                NetworkBackend::NetworkManager | NetworkBackend::Iwd => {
                    Plan::run(CommandSpec::new("ip", ["-brief", "address"]))
                        .then_run(CommandSpec::new("ip", ["route"]))
                        .then(Step::Native(NativeOp::ReadFixedFile { path: "/etc/resolv.conf", lines: 20 }))
                }
            },
            WifiScan => match self.config.system.network {
                NetworkBackend::Networkd => return Err(NO_WIFI_DAEMON.into()),
                NetworkBackend::NetworkManager => Plan::run(CommandSpec::new(
                    "nmcli",
                    ["-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "yes"],
                )),
                NetworkBackend::Iwd => {
                    let dev = self.wireless_interface()?;
                    Plan::run(CommandSpec::new("iwctl", ["station", &dev, "scan"]).optional())
                        .then_run(CommandSpec::new("iwctl", ["station", &dev, "get-networks"]))
                }
            },
            PingHost { host, count } => Plan::run(
                CommandSpec::new("ping", ["-c", &count.to_string(), "-W", "2", "--", host.as_str()])
                    .success_codes(&[0, 1]),
            ),
            SearchPackages { query } => {
                let words: Vec<&str> = query.as_str().split_whitespace().collect();
                let c = match self.config.system.package_manager {
                    // cpkg exits 0 with no output when nothing matches.
                    PackageManager::Cpkg => CommandSpec::new("cpkg", ["search", "--"]),
                    PackageManager::Pacman => CommandSpec::new("pacman", ["-Ss", "--"]),
                    PackageManager::Apt => CommandSpec::new("apt-cache", ["search", "--"]),
                    PackageManager::Dnf => CommandSpec::new("dnf", ["search", "--"]),
                    PackageManager::Zypper => CommandSpec::new("zypper", ["--non-interactive", "search", "--"]),
                    PackageManager::Apk => CommandSpec::new("apk", ["search"]),
                    PackageManager::Xbps => CommandSpec::new("xbps-query", ["-Rs"]),
                };
                // "No results" is an answer, not a failure.
                Plan::run(words.into_iter().fold(c, |c, w| c.arg(w)).success_codes(&[0, 1]).head(60))
            }
            PackageInfo { package } => {
                let c = match self.config.system.package_manager {
                    PackageManager::Cpkg => CommandSpec::new("cpkg", ["info", "--"]),
                    PackageManager::Pacman => CommandSpec::new("pacman", ["-Qi", "--"]),
                    PackageManager::Apt => CommandSpec::new("dpkg", ["-s", "--"]),
                    PackageManager::Dnf | PackageManager::Zypper => CommandSpec::new("rpm", ["-qi", "--"]),
                    PackageManager::Apk => CommandSpec::new("apk", ["info", "-a"]),
                    PackageManager::Xbps => CommandSpec::new("xbps-query", Vec::<String>::new()),
                };
                // Exit 1 means "not installed", which is exactly what was asked.
                Plan::run(c.arg(package.as_str()).success_codes(&[0, 1]))
            }

            SetVolume { percent } => Plan::run(match self.config.system.audio {
                AudioBackend::Wpctl => CommandSpec::new("wpctl", ["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@"])
                    .arg(format!("{:.2}", *percent as f64 / 100.0))
                    .as_peer(),
                AudioBackend::Pactl => CommandSpec::new("pactl", ["set-sink-volume", "@DEFAULT_SINK@"])
                    .arg(format!("{percent}%"))
                    .as_peer(),
                AudioBackend::Amixer => CommandSpec::new("amixer", ["-q", "sset", "Master"]).arg(format!("{percent}%")),
            }),
            SetMute { muted } => Plan::run(match self.config.system.audio {
                AudioBackend::Wpctl => {
                    CommandSpec::new("wpctl", ["set-mute", "@DEFAULT_AUDIO_SINK@", if *muted { "1" } else { "0" }])
                        .as_peer()
                }
                AudioBackend::Pactl => {
                    CommandSpec::new("pactl", ["set-sink-mute", "@DEFAULT_SINK@", if *muted { "1" } else { "0" }])
                        .as_peer()
                }
                AudioBackend::Amixer => {
                    CommandSpec::new("amixer", ["-q", "sset", "Master", if *muted { "mute" } else { "unmute" }])
                }
            }),
            SetBrightness { percent } => Plan::native(NativeOp::SetBrightness { percent: *percent }),

            RestartService { service } => self.systemctl_then_check(&["restart"], service.as_str()),
            StartService { service } => self.systemctl_then_check(&["start"], service.as_str()),
            StopService { service } => Plan::run(CommandSpec::new("systemctl", ["stop", "--"]).arg(service.as_str())),
            EnableService { service, now } => {
                let args: &[&str] = if *now { &["enable", "--now"] } else { &["enable"] };
                self.systemctl_then_check(args, service.as_str())
            }
            DisableService { service, now } => {
                let args: &[&str] = if *now { &["disable", "--now", "--"] } else { &["disable", "--"] };
                Plan::run(CommandSpec::new("systemctl", args.iter().copied()).arg(service.as_str()))
            }
            KillProcess { pid, signal } => Plan::native(NativeOp::Signal { pid: *pid, signal: *signal }),
            LoadKernelModule { module } => Plan::run(CommandSpec::new("modprobe", ["--"]).arg(module.as_str())),
            UnloadKernelModule { module } => Plan::run(CommandSpec::new("modprobe", ["-r", "--"]).arg(module.as_str())),
            WifiConnect { ssid, passphrase } => match self.config.system.network {
                NetworkBackend::Networkd => return Err(NO_WIFI_DAEMON.into()),
                NetworkBackend::NetworkManager => {
                    let mut c = CommandSpec::new("nmcli", ["device", "wifi", "connect"]).arg(ssid.as_str());
                    if let Some(p) = passphrase {
                        c = c.arg("password").secret_arg(p.expose());
                    }
                    Plan::run(c.timeout(Duration::from_secs(60)))
                }
                NetworkBackend::Iwd => {
                    let dev = self.wireless_interface()?;
                    let mut c = CommandSpec::new("iwctl", Vec::<String>::new());
                    if let Some(p) = passphrase {
                        c = c.arg("--passphrase").secret_arg(p.expose());
                    }
                    Plan::run(
                        c.arg("station").arg(dev).arg("connect").arg(ssid.as_str()).timeout(Duration::from_secs(60)),
                    )
                }
            },
            SetLink { interface, up } => Plan::run(CommandSpec::new(
                "ip",
                ["link", "set", "dev", interface.as_str(), if *up { "up" } else { "down" }],
            )),
            SetHostname { hostname } => {
                Plan::run(CommandSpec::new("hostnamectl", ["set-hostname", "--"]).arg(hostname.as_str()))
            }
            SetTimezone { timezone } => {
                Plan::run(CommandSpec::new("timedatectl", ["set-timezone", "--"]).arg(timezone.as_str()))
            }

            InstallPackage { package } => Plan::run(
                match self.config.system.package_manager {
                    PackageManager::Cpkg => CommandSpec::new("cpkg", ["install", "--"]),
                    PackageManager::Pacman => CommandSpec::new("pacman", ["-S", "--noconfirm", "--needed", "--"]),
                    PackageManager::Apt => {
                        CommandSpec::new("apt-get", ["install", "-y", "--"]).env("DEBIAN_FRONTEND", "noninteractive")
                    }
                    PackageManager::Dnf => CommandSpec::new("dnf", ["install", "-y", "--"]),
                    PackageManager::Zypper => CommandSpec::new("zypper", ["--non-interactive", "install", "--"]),
                    PackageManager::Apk => CommandSpec::new("apk", ["add"]),
                    PackageManager::Xbps => CommandSpec::new("xbps-install", ["-y"]),
                }
                .arg(package.as_str())
                .timeout(self.pkg_timeout())
                .tail(40),
            ),
            RemovePackage { package } => Plan::run(
                match self.config.system.package_manager {
                    // Never --force: cpkg refuses to remove what other packages need.
                    PackageManager::Cpkg => CommandSpec::new("cpkg", ["remove", "--"]),
                    PackageManager::Pacman => CommandSpec::new("pacman", ["-Rns", "--noconfirm", "--"]),
                    PackageManager::Apt => {
                        CommandSpec::new("apt-get", ["remove", "-y", "--"]).env("DEBIAN_FRONTEND", "noninteractive")
                    }
                    PackageManager::Dnf => CommandSpec::new("dnf", ["remove", "-y", "--"]),
                    PackageManager::Zypper => CommandSpec::new("zypper", ["--non-interactive", "remove", "--"]),
                    PackageManager::Apk => CommandSpec::new("apk", ["del"]),
                    PackageManager::Xbps => CommandSpec::new("xbps-remove", ["-y"]),
                }
                .arg(package.as_str())
                .timeout(self.pkg_timeout())
                .tail(40),
            ),
            UpdateSystem => {
                let t = self.pkg_timeout();
                match self.config.system.package_manager {
                    PackageManager::Cpkg => Plan::run(CommandSpec::new("cpkg", ["upgrade"]).timeout(t).tail(40)),
                    PackageManager::Pacman => {
                        Plan::run(CommandSpec::new("pacman", ["-Syu", "--noconfirm"]).timeout(t).tail(40))
                    }
                    PackageManager::Apt => Plan::run(CommandSpec::new("apt-get", ["update"]).timeout(t).tail(20))
                        .then_run(
                            CommandSpec::new("apt-get", ["upgrade", "-y"])
                                .env("DEBIAN_FRONTEND", "noninteractive")
                                .timeout(t)
                                .tail(40),
                        ),
                    PackageManager::Dnf => Plan::run(CommandSpec::new("dnf", ["upgrade", "-y"]).timeout(t).tail(40)),
                    PackageManager::Zypper => {
                        Plan::run(CommandSpec::new("zypper", ["--non-interactive", "update"]).timeout(t).tail(40))
                    }
                    PackageManager::Apk => Plan::run(CommandSpec::new("apk", ["update"]).timeout(t).tail(20))
                        .then_run(CommandSpec::new("apk", ["upgrade"]).timeout(t).tail(40)),
                    PackageManager::Xbps => {
                        Plan::run(CommandSpec::new("xbps-install", ["-Syu", "-y"]).timeout(t).tail(40))
                    }
                }
            }
            ConfigureSwap { size_mb } => self.swap_plan(*size_mb),
            Reboot => Plan::run(CommandSpec::new("systemctl", ["reboot"])),
            Poweroff => Plan::run(CommandSpec::new("systemctl", ["poweroff"])),
        };
        self.check_tools(&plan)?;
        Ok(plan)
    }

    /// Run a systemctl verb, then report the resulting state (informational).
    fn systemctl_then_check(&self, verb: &[&str], unit: &str) -> Plan {
        Plan::run(CommandSpec::new("systemctl", verb.iter().copied()).arg("--").arg(unit)).then_run(
            CommandSpec::new("systemctl", ["status", "--no-pager", "--lines=5", "--"])
                .arg(unit)
                .success_codes(SYSTEMCTL_STATUS_OK)
                .optional(),
        )
    }

    fn swap_plan(&self, size_mb: u32) -> Plan {
        let file = self.config.native.swapfile.display().to_string();
        let mut plan = Plan::run(CommandSpec::new("swapoff", [file.as_str()]).optional())
            .then(Step::Native(NativeOp::RemoveSwapFile));
        if size_mb == 0 {
            return plan.then(Step::Native(NativeOp::FstabSwap { present: false }));
        }
        let parent = self.config.native.swapfile.parent().unwrap_or(Path::new("/"));
        if self.probe.filesystem_type(parent).as_deref() == Some("btrfs") {
            // Btrfs swap files must be NOCOW and contiguous; btrfs-progs handles that.
            plan = plan.then_run(CommandSpec::new(
                "btrfs",
                ["filesystem", "mkswapfile", "--size", &format!("{size_mb}m"), "--", &file],
            ));
        } else {
            plan = plan
                .then_run(CommandSpec::new("fallocate", ["-l", &format!("{size_mb}MiB"), "--", &file]))
                .then_run(CommandSpec::new("mkswap", [file.as_str()]));
        }
        plan.then_run(CommandSpec::new("swapon", [file.as_str()]))
            .then(Step::Native(NativeOp::FstabSwap { present: true }))
    }

    fn wireless_interface(&self) -> Result<String, String> {
        self.probe.wireless_interfaces().into_iter().next().ok_or_else(|| {
            "no wireless interface exists (the Wi-Fi driver may not be loaded; check list_hardware and read_kernel_log)"
                .into()
        })
    }

    fn check_tools(&self, plan: &Plan) -> Result<(), String> {
        for step in &plan.steps {
            if let Step::Run(c) = step {
                if !self.config.tools.contains_key(c.tool) {
                    return Err(format!("the {} tool is not configured on this system", c.tool));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use core_protocol::{Intent, ValidatedAction};
    use serde_json::json;

    use super::*;

    struct FakeProbe {
        wifi: Vec<String>,
        fs: &'static str,
    }

    impl SystemProbe for FakeProbe {
        fn wireless_interfaces(&self) -> Vec<String> {
            self.wifi.clone()
        }
        fn filesystem_type(&self, _: &Path) -> Option<String> {
            Some(self.fs.into())
        }
    }

    fn plan_with(
        config: &GuardianConfig,
        probe: &FakeProbe,
        action: &str,
        args: serde_json::Value,
    ) -> Result<Vec<String>, String> {
        let v = ValidatedAction::from_intent(&Intent::new(action, args)).unwrap();
        Planner::new(config, probe).plan(&v.action).map(|p| p.steps.iter().map(Step::describe).collect())
    }

    fn plan(action: &str, args: serde_json::Value) -> Vec<String> {
        let probe = FakeProbe { wifi: vec!["wlan0".into()], fs: "ext4" };
        plan_with(&GuardianConfig::default(), &probe, action, args).unwrap()
    }

    #[test]
    fn every_guardian_action_plans_with_defaults() {
        // The defaults are C.O.R.E. OS: every action plans except Wi-Fi, which has
        // no daemon there and says so.
        let probe = FakeProbe { wifi: vec!["wlan0".into()], fs: "ext4" };
        let config = GuardianConfig::default();
        for spec in core_protocol::CATALOG.iter().filter(|s| s.executor == core_protocol::Executor::Guardian) {
            let args: serde_json::Value = serde_json::from_str(spec.example).unwrap();
            let result = plan_with(&config, &probe, spec.name, args);
            if spec.name.starts_with("wifi_") {
                assert!(result.unwrap_err().contains("no Wi-Fi daemon"), "{}", spec.name);
                continue;
            }
            let steps = result.unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            assert!(!steps.is_empty(), "{}", spec.name);
        }
        // With a Wi-Fi capable backend, every action plans.
        let mut nm = GuardianConfig::default();
        nm.system.network = NetworkBackend::NetworkManager;
        for spec in core_protocol::CATALOG.iter().filter(|s| s.executor == core_protocol::Executor::Guardian) {
            let args: serde_json::Value = serde_json::from_str(spec.example).unwrap();
            plan_with(&nm, &probe, spec.name, args).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
        }
    }

    #[test]
    fn cpkg_packages() {
        assert_eq!(plan("install_package", json!({"package": "w3m"})), ["cpkg install -- w3m"]);
        assert_eq!(plan("remove_package", json!({"package": "w3m"})), ["cpkg remove -- w3m"]);
        assert_eq!(plan("update_system", json!({})), ["cpkg upgrade"]);
        assert_eq!(plan("package_info", json!({"package": "nano"})), ["cpkg info -- nano"]);
        assert_eq!(plan("search_packages", json!({"query": "text editor"})), ["cpkg search -- text editor"]);
        // A name that looks like an option stays an operand.
        let probe = FakeProbe { wifi: vec![], fs: "ext4" };
        let v = ValidatedAction::from_intent(&Intent::new("install_package", json!({"package": "nano"}))).unwrap();
        let p = Planner::new(&GuardianConfig::default(), &probe).plan(&v.action).unwrap();
        let Step::Run(cmd) = &p.steps[0] else { panic!() };
        assert_eq!(cmd.timeout, Some(Duration::from_secs(GuardianConfig::default().package_timeout_secs)));
    }

    #[test]
    fn networkd_status_and_no_wifi() {
        assert_eq!(
            plan("network_status", json!({})),
            ["networkctl list --no-pager", "ip route", "resolvectl status --no-pager"]
        );
        let err = plan_with(
            &GuardianConfig::default(),
            &FakeProbe { wifi: vec!["wlan0".into()], fs: "ext4" },
            "wifi_connect",
            json!({"ssid": "Home", "passphrase": "hunter222"}),
        )
        .unwrap_err();
        assert!(err.contains("no Wi-Fi daemon") && !err.contains("hunter222"), "{err}");
    }

    #[test]
    fn services() {
        assert_eq!(
            plan("restart_service", json!({"service": "bluetooth"})),
            ["systemctl restart -- bluetooth", "systemctl status --no-pager --lines=5 -- bluetooth"]
        );
        assert_eq!(
            plan("disable_service", json!({"service": "cups", "now": true})),
            ["systemctl disable --now -- cups"]
        );
    }

    #[test]
    fn packages_per_distribution() {
        let probe = FakeProbe { wifi: vec![], fs: "ext4" };
        let mut pacman = GuardianConfig::default();
        pacman.system.package_manager = PackageManager::Pacman;
        assert_eq!(
            plan_with(&pacman, &probe, "install_package", json!({"package": "w3m"})).unwrap(),
            ["pacman -S --noconfirm --needed -- w3m"]
        );
        assert_eq!(
            plan_with(&pacman, &probe, "search_packages", json!({"query": "web browser"})).unwrap(),
            ["pacman -Ss -- web browser"]
        );
        let mut c = GuardianConfig::default();
        c.system.package_manager = PackageManager::Apt;
        assert_eq!(
            plan_with(&c, &probe, "install_package", json!({"package": "w3m"})).unwrap(),
            ["apt-get install -y -- w3m"]
        );
        assert_eq!(
            plan_with(&c, &probe, "update_system", json!({})).unwrap(),
            ["apt-get update", "apt-get upgrade -y"]
        );
    }

    #[test]
    fn wifi_backends_and_secrets() {
        let mut nm = GuardianConfig::default();
        nm.system.network = NetworkBackend::NetworkManager;
        assert_eq!(
            plan_with(
                &nm,
                &FakeProbe { wifi: vec!["wlan0".into()], fs: "ext4" },
                "wifi_connect",
                json!({"ssid": "Home Net", "passphrase": "hunter222"})
            )
            .unwrap(),
            ["nmcli device wifi connect 'Home Net' password <redacted>"]
        );
        let mut c = GuardianConfig::default();
        c.system.network = NetworkBackend::Iwd;
        let probe = FakeProbe { wifi: vec!["wlan0".into()], fs: "ext4" };
        assert_eq!(
            plan_with(&c, &probe, "wifi_connect", json!({"ssid": "Home", "passphrase": "hunter222"})).unwrap(),
            ["iwctl --passphrase <redacted> station wlan0 connect Home"]
        );
        let none = FakeProbe { wifi: vec![], fs: "ext4" };
        let err = plan_with(&c, &none, "wifi_scan", json!({})).unwrap_err();
        assert!(err.contains("no wireless interface"));
    }

    #[test]
    fn audio_runs_as_the_user_for_pipewire() {
        let v = ValidatedAction::from_intent(&Intent::new("set_volume", json!({"percent": 40}))).unwrap();
        let probe = FakeProbe { wifi: vec![], fs: "ext4" };
        let config = GuardianConfig::default();
        let plan = Planner::new(&config, &probe).plan(&v.action).unwrap();
        let Step::Run(cmd) = &plan.steps[0] else { panic!() };
        assert_eq!(cmd.describe(), "wpctl set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 0.40");
        assert_eq!(cmd.run_as, crate::plan::RunAs::Peer);
    }

    #[test]
    fn swap_plans() {
        assert_eq!(
            plan("configure_swap", json!({"size_mb": 4096})),
            [
                "swapoff /swapfile",
                "remove swap file",
                "fallocate -l 4096MiB -- /swapfile",
                "mkswap /swapfile",
                "swapon /swapfile",
                "add swap file to /etc/fstab"
            ]
        );
        let btrfs = FakeProbe { wifi: vec![], fs: "btrfs" };
        let steps = plan_with(&GuardianConfig::default(), &btrfs, "configure_swap", json!({"size_mb": 1024})).unwrap();
        assert_eq!(steps[2], "btrfs filesystem mkswapfile --size 1024m -- /swapfile");
        assert_eq!(
            plan("configure_swap", json!({"size_mb": 0})),
            ["swapoff /swapfile", "remove swap file", "remove swap file from /etc/fstab"]
        );
    }

    #[test]
    fn missing_tool_is_reported() {
        let mut c = GuardianConfig::default();
        c.tools.remove("lspci");
        let probe = FakeProbe { wifi: vec![], fs: "ext4" };
        let err = plan_with(&c, &probe, "list_hardware", json!({"bus": "pci"})).unwrap_err();
        assert!(err.contains("lspci"));
    }

    #[test]
    fn agent_actions_are_not_planned() {
        let probe = FakeProbe { wifi: vec![], fs: "ext4" };
        assert!(plan_with(&GuardianConfig::default(), &probe, "respond", json!({"message": "hi"})).is_err());
    }
}
