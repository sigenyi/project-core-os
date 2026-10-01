//! Rescue mode: a deterministic, keyword-driven planner.
//!
//! When the language model cannot run (not enough memory, corrupt model file, the
//! inference service crashed) the machine must still be operable, if only to fix the
//! problem. The rescue planner understands short imperative commands ("restart
//! NetworkManager", "volume 40", "install linux-firmware") and speaks the same intent
//! protocol as the model, so everything downstream (validation, policy, confirmation,
//! audit) is identical. It is also a convenient driver for testing the control loop.

use serde_json::{Value, json};

use super::{BackendError, CompletionRequest, InferenceBackend, Role};
use crate::prompt::{OBSERVATION_PREFIX, REQUEST_PREFIX, STATE_HEADER};

#[derive(Default)]
pub struct RescuePlanner;

pub const HELP: &str = "Rescue mode (no language model) understands short commands: \
status · volume 40 · mute/unmute · brightness 70 · restart|start|stop|enable|disable <service> · \
status of <service> · logs [for <unit>] · kernel errors · install|remove <package> · search <words> · \
update system · disk usage · disks · network · wifi scan · connect to <ssid> password <pass> · ping <host> · \
processes · list <dir> · read <file> · swap <size> GB · pci|usb devices · load|unload module <name> · \
hostname <name> · timezone <Area/City> · open <program> · reboot · shutdown";

impl InferenceBackend for RescuePlanner {
    fn name(&self) -> String {
        "rescue mode (no language model)".into()
    }

    fn complete(&mut self, request: &CompletionRequest) -> Result<String, BackendError> {
        let last = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .ok_or_else(|| BackendError::BadResponse("no user message".into()))?;
        let (thought, action, args) = if last.content.starts_with(OBSERVATION_PREFIX) {
            summarise_observation(&last.content)
        } else {
            let (state, req) = split_request(&last.content);
            plan(req, state)
        };
        Ok(json!({"thought": thought, "action": action, "args": args}).to_string())
    }
}

fn split_request(content: &str) -> (&str, &str) {
    match content.rsplit_once(REQUEST_PREFIX) {
        Some((before, req)) => (before.strip_prefix(STATE_HEADER).unwrap_or(before).trim(), req.trim()),
        None => ("", content.trim()),
    }
}

fn summarise_observation(obs: &str) -> (String, &'static str, Value) {
    let (header, body) = obs.split_once('\n').unwrap_or((obs, ""));
    let body = body.lines().filter(|l| !l.starts_with("Find the cause in this error")).collect::<Vec<_>>().join("\n");
    let body: String = body.trim().chars().take(900).collect();
    let message = if header.contains("FAILED") || header.contains("DENIED") || header.contains("invalid") {
        format!("That did not work. {}{}", header.trim_start_matches(OBSERVATION_PREFIX).trim(), with_body(&body))
    } else if header.contains("declined") {
        "Okay, I did not do it.".to_string()
    } else {
        format!("Done{}", if body.is_empty() { ".".to_string() } else { format!(":\n{body}") })
    };
    ("Report the result.".into(), "respond", json!({"message": message}))
}

fn with_body(body: &str) -> String {
    if body.is_empty() { String::new() } else { format!("\n{body}") }
}

fn respond(text: impl Into<String>) -> (String, &'static str, Value) {
    ("Answer directly.".into(), "respond", json!({"message": text.into()}))
}

fn act(action: &'static str, args: Value) -> (String, &'static str, Value) {
    (format!("Rescue rule matched {action}."), action, args)
}

/// First number in the text.
fn number(text: &str) -> Option<u64> {
    text.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty()).and_then(|s| s.parse().ok())
}

/// The word following any of `keys` (skipping filler words), in its original case.
fn word_after(original: &str, keys: &[&str]) -> Option<String> {
    let words: Vec<&str> = original.split_whitespace().collect();
    let filler = ["the", "a", "an", "to", "for", "of", "package", "service", "program", "app", "module", "please"];
    for (i, w) in words.iter().enumerate() {
        if keys.contains(&w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric())) {
            return words[i + 1..]
                .iter()
                .map(|w| w.trim_matches(|c: char| ",.!?;\"'".contains(c)))
                .find(|w| !w.is_empty() && !filler.contains(&w.to_lowercase().as_str()))
                .map(String::from);
        }
    }
    None
}

/// Everything after the first of `keys`.
fn rest_after(original: &str, keys: &[&str]) -> Option<String> {
    let lower = original.to_lowercase();
    keys.iter().find_map(|k| {
        let pos = lower.find(&format!("{k} "))?;
        let rest = original[pos + k.len()..].trim().trim_end_matches(['.', '!', '?']);
        (!rest.is_empty()).then(|| rest.to_string())
    })
}

fn has(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.split(|c: char| !c.is_alphanumeric() && c != '-').any(|t| t == *w))
}

fn has_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| text.contains(p))
}

pub fn plan(request: &str, state: &str) -> (String, &'static str, Value) {
    let t = request.to_lowercase();
    let t = t.trim();

    if t.is_empty() || has(t, &["help"]) || has_phrase(t, &["what can you do"]) {
        return respond(HELP);
    }
    if has_phrase(t, &["system status", "how is the system", "health"]) || t == "status" || t == "info" {
        return respond(if state.is_empty() { "No system state is available.".to_string() } else { state.to_string() });
    }

    // Power: before services, so "restart the computer" is not a service restart.
    let machine = has(t, &["computer", "system", "machine", "pc", "laptop"]);
    if has(t, &["reboot"]) || (has(t, &["restart"]) && machine && !has(t, &["service"])) {
        return act("reboot", json!({}));
    }
    if has_phrase(t, &["shut down", "shutdown", "power off", "poweroff"]) || (has_phrase(t, &["turn off"]) && machine) {
        return act("poweroff", json!({}));
    }
    if has(t, &["update", "upgrade"]) && (machine || has(t, &["packages", "everything", "all"])) {
        return act("update_system", json!({}));
    }

    // Audio and display.
    if has(t, &["unmute"]) {
        return act("set_mute", json!({"muted": false}));
    }
    if has(t, &["mute", "silence"]) {
        return act("set_mute", json!({"muted": true}));
    }
    if has(t, &["volume", "louder", "quieter", "sound"]) {
        if let Some(n) = number(t) {
            return act("set_volume", json!({"percent": n.min(150)}));
        }
    }
    if has(t, &["brightness", "brighter", "dimmer", "backlight"]) {
        if let Some(n) = number(t) {
            return act("set_brightness", json!({"percent": n.clamp(1, 100)}));
        }
    }

    // Packages.
    if has(t, &["installed"]) {
        if let Some(p) = word_after(request, &["is", "check"]) {
            return act("package_info", json!({"package": p.to_lowercase()}));
        }
    }
    if has(t, &["uninstall", "remove"]) {
        if let Some(p) = word_after(request, &["uninstall", "remove"]) {
            return act("remove_package", json!({"package": p}));
        }
    }
    if has(t, &["install"]) {
        if let Some(p) = word_after(request, &["install"]) {
            return act("install_package", json!({"package": p}));
        }
    }
    if has(t, &["search", "find"]) && !t.contains('/') {
        if let Some(q) = rest_after(request, &["search for", "search", "find"]) {
            return act("search_packages", json!({"query": q}));
        }
    }

    // Kernel modules (before services: "load module" is not a service start).
    if has(t, &["module", "driver"]) {
        if has(t, &["unload"]) {
            if let Some(m) = word_after(request, &["unload"]) {
                return act("unload_kernel_module", json!({"module": m}));
            }
        } else if has(t, &["load"]) {
            if let Some(m) = word_after(request, &["load"]) {
                return act("load_kernel_module", json!({"module": m}));
            }
        }
    }

    // Logs (before services: "logs for sshd" is not a status query).
    if has_phrase(t, &["kernel", "dmesg", "driver error", "hardware error"]) {
        return act("read_kernel_log", json!({"errors_only": has(t, &["error", "errors"]), "lines": 40}));
    }
    if has(t, &["log", "logs", "journal"]) {
        return match word_after(request, &["for", "of", "from"]) {
            Some(unit) => act("read_logs", json!({"unit": unit, "lines": 40})),
            None => act("read_logs", json!({"priority": "warning", "lines": 40})),
        };
    }

    // Services.
    for (verb, action) in [
        ("restart", "restart_service"),
        ("start", "start_service"),
        ("stop", "stop_service"),
        ("enable", "enable_service"),
        ("disable", "disable_service"),
    ] {
        if has(t, &[verb]) {
            if let Some(s) = word_after(request, &[verb]) {
                return act(action, json!({"service": s}));
            }
        }
    }
    if has(t, &["status"]) {
        if let Some(s) = word_after(request, &["of", "status"]) {
            return act("service_status", json!({"service": s}));
        }
    }
    if has_phrase(t, &["failed services", "failed units", "what failed"]) {
        return act("list_services", json!({"state": "failed"}));
    }

    // Network.
    if has(t, &["ping"]) {
        if let Some(h) = word_after(request, &["ping"]) {
            return act("ping_host", json!({"host": h}));
        }
    }
    if has(t, &["connect"]) {
        if let Some(ssid) = word_after(request, &["to", "connect"]) {
            let pass = word_after(request, &["password", "passphrase", "key"]);
            return act("wifi_connect", json!({"ssid": ssid, "passphrase": pass}));
        }
    }
    if has(t, &["wifi", "wi-fi", "wireless"]) && has(t, &["scan", "networks", "available", "list"]) {
        return act("wifi_scan", json!({}));
    }
    if has(t, &["network", "internet", "ip", "connection", "online", "wifi", "dns"]) {
        return act("network_status", json!({}));
    }

    // Storage, files, processes, hardware.
    if has(t, &["swap"]) {
        if let Some(n) = number(t) {
            let mb = if has(t, &["mb", "mib", "megabytes"]) { n } else { n * 1024 };
            return act("configure_swap", json!({"size_mb": mb.min(65536)}));
        }
        if has(t, &["remove", "disable", "delete"]) {
            return act("configure_swap", json!({"size_mb": 0}));
        }
    }
    if let Some(path) = request.split_whitespace().find(|w| w.starts_with('/')) {
        let path = path.trim_end_matches(['.', ',', '?', '!']);
        if has(t, &["read", "show", "cat", "open", "view", "print"]) && !has(t, &["list", "ls", "directory", "folder"])
        {
            return act("read_file", json!({"path": path, "lines": 60}));
        }
        return act("list_directory", json!({"path": path}));
    }
    if has(t, &["disk", "disks", "space", "storage"]) {
        if has(t, &["partitions", "drives", "disks"]) && !has(t, &["space", "usage", "free"]) {
            return act("list_block_devices", json!({}));
        }
        return act("disk_usage", json!({}));
    }
    if has(t, &["process", "processes", "running", "cpu", "memory", "ram"]) {
        let by = if has(t, &["memory", "ram"]) { "memory" } else { "cpu" };
        return act("list_processes", json!({"sort_by": by, "limit": 15}));
    }
    if has(t, &["usb"]) {
        return act("list_hardware", json!({"bus": "usb"}));
    }
    if has(t, &["pci", "hardware", "devices"]) {
        return act("list_hardware", json!({"bus": "pci"}));
    }
    if has(t, &["hostname"]) {
        if let Some(h) = word_after(request, &["to", "hostname"]) {
            return act("set_hostname", json!({"hostname": h}));
        }
    }
    if has(t, &["timezone"]) || has_phrase(t, &["time zone"]) {
        if let Some(z) = request.split_whitespace().find(|w| w.contains('/') || *w == "UTC") {
            return act("set_timezone", json!({"timezone": z.trim_end_matches(['.', '!'])}));
        }
    }
    if has(t, &["open", "launch", "run"]) {
        if let Some(p) = word_after(request, &["open", "launch", "run"]) {
            return act("launch_program", json!({"program": p.to_lowercase()}));
        }
    }

    respond(format!("I could not understand that in rescue mode. {HELP}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChatMessage;

    fn intent(text: &str) -> (String, Value) {
        let (_, action, args) = plan(text, "host: core");
        (action.to_string(), args)
    }

    #[test]
    fn understands_common_commands() {
        let cases: &[(&str, &str, Value)] = &[
            ("set the volume to 40", "set_volume", json!({"percent": 40})),
            ("mute", "set_mute", json!({"muted": true})),
            ("unmute please", "set_mute", json!({"muted": false})),
            ("brightness 70", "set_brightness", json!({"percent": 70})),
            ("restart NetworkManager", "restart_service", json!({"service": "NetworkManager"})),
            ("restart the computer", "reboot", json!({})),
            ("shut down", "poweroff", json!({})),
            ("install the package firefox", "install_package", json!({"package": "firefox"})),
            ("uninstall w3m", "remove_package", json!({"package": "w3m"})),
            ("search for web browser", "search_packages", json!({"query": "web browser"})),
            ("update the system", "update_system", json!({})),
            ("how much disk space do I have", "disk_usage", json!({})),
            ("is my internet working?", "network_status", json!({})),
            ("scan wifi networks", "wifi_scan", json!({})),
            (
                "connect to HomeNet password hunter22",
                "wifi_connect",
                json!({"ssid": "HomeNet", "passphrase": "hunter22"}),
            ),
            ("ping 1.1.1.1", "ping_host", json!({"host": "1.1.1.1"})),
            ("show logs for sshd", "read_logs", json!({"unit": "sshd", "lines": 40})),
            ("any kernel errors?", "read_kernel_log", json!({"errors_only": true, "lines": 40})),
            ("what is using memory", "list_processes", json!({"sort_by": "memory", "limit": 15})),
            ("list /etc/systemd", "list_directory", json!({"path": "/etc/systemd"})),
            ("show /etc/fstab", "read_file", json!({"path": "/etc/fstab", "lines": 60})),
            ("add 4 GB of swap", "configure_swap", json!({"size_mb": 4096})),
            ("load module btusb", "load_kernel_module", json!({"module": "btusb"})),
            ("status of bluetooth", "service_status", json!({"service": "bluetooth"})),
            ("set timezone to Europe/Berlin", "set_timezone", json!({"timezone": "Europe/Berlin"})),
            ("open htop", "launch_program", json!({"program": "htop"})),
            ("show usb devices", "list_hardware", json!({"bus": "usb"})),
        ];
        for (text, action, args) in cases {
            assert_eq!(intent(text), (action.to_string(), args.clone()), "{text}");
        }
    }

    #[test]
    fn every_rule_output_validates() {
        // Rule output must pass the same validation as model output.
        for text in ["volume 40", "restart bluetooth", "install w3m", "list /etc", "swap 2 GB", "ping example.org"] {
            let (thought, action, args) = plan(text, "");
            let raw = json!({"thought": thought, "action": action, "args": args}).to_string();
            let intent = core_protocol::Intent::parse(&raw).unwrap();
            core_protocol::ValidatedAction::from_intent(&intent).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    #[test]
    fn status_reports_state_and_unknown_gets_help() {
        assert_eq!(intent("status").1, json!({"message": "host: core"}));
        let (action, args) = intent("compose a symphony");
        assert_eq!(action, "respond");
        assert!(args["message"].as_str().unwrap().contains("rescue mode"));
    }

    #[test]
    fn observations_become_replies() {
        let mut r = RescuePlanner;
        let msgs = [
            ChatMessage::system("sys"),
            ChatMessage::user("SYSTEM STATE (x):\nstate\n\nREQUEST: restart foo"),
            ChatMessage::assistant("{}"),
            ChatMessage::user(
                "OBSERVATION (restart_service: FAILED, exit code 5)\nUnit foo.service not found.\nFind the cause in this error, then try a different approach or explain the problem to the user.",
            ),
        ];
        let out = r
            .complete(&CompletionRequest { messages: &msgs, grammar: None, max_tokens: 10, temperature: 0.0 })
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["action"], "respond");
        let msg = v["args"]["message"].as_str().unwrap();
        assert!(
            msg.starts_with("That did not work. (restart_service: FAILED, exit code 5)\nUnit foo.service not found."),
            "{msg}"
        );
        assert!(!msg.contains("Find the cause"));
    }
}
