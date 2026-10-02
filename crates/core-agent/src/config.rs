//! Agent configuration (`/etc/core/agent.toml`, overridable per user).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub inference: InferenceConfig,
    pub agent: LoopConfig,
    pub guardian: GuardianLink,
    pub telemetry: TelemetryConfig,
    pub programs: ProgramsConfig,
    /// What `read_file`/`list_directory` may show the model (on top of the user's own
    /// file permissions).
    pub files: core_protocol::paths::PathPolicy,
    pub voice: VoiceConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// llama.cpp's `llama-server` (OpenAI-compatible endpoint + GBNF grammar).
    Llama,
    /// Deterministic keyword planner; no model needed.
    Rescue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InferenceConfig {
    pub backend: BackendKind,
    pub url: String,
    /// Model name sent with requests (llama-server ignores it unless routing).
    pub model: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Context window of the loaded model, used to budget prompts.
    pub context_tokens: usize,
    pub timeout_secs: u64,
    /// Fall back to rescue mode when the model server is unreachable.
    pub fallback_to_rescue: bool,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        InferenceConfig {
            backend: BackendKind::Llama,
            url: "http://127.0.0.1:8080".into(),
            model: None,
            temperature: 0.2,
            max_tokens: 384,
            context_tokens: 8192,
            timeout_secs: 180,
            fallback_to_rescue: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoopConfig {
    /// Maximum model calls for one request.
    pub max_steps: usize,
    /// Consecutive failed actions before the agent stops and explains.
    pub max_consecutive_failures: usize,
    /// Previous exchanges kept as conversation memory.
    pub history_turns: usize,
    /// Characters of command output shown to the model per observation.
    pub observation_chars: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig { max_steps: 10, max_consecutive_failures: 3, history_turns: 4, observation_chars: 3000 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuardianLink {
    pub socket: PathBuf,
}

impl Default for GuardianLink {
    fn default() -> Self {
        GuardianLink { socket: core_protocol::DEFAULT_GUARDIAN_SOCKET.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Snapshot published by core-sensed.
    pub path: PathBuf,
    /// Older snapshots are replaced by a live collection.
    pub max_age_secs: u64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        TelemetryConfig { path: core_protocol::DEFAULT_TELEMETRY_PATH.into(), max_age_secs: 30 }
    }
}

/// Interactive programs `launch_program` may open (they run as the user).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProgramsConfig {
    /// Program name → absolute path. Entries whose binary is missing are ignored.
    pub allowed: BTreeMap<String, PathBuf>,
    /// Also allow any executable in these directories (minus `denied`).
    pub allow_installed: bool,
    pub search_path: Vec<PathBuf>,
    /// Never launched, even with `allow_installed` (shells, interpreters, privilege tools).
    pub denied: Vec<String>,
}

impl Default for ProgramsConfig {
    fn default() -> Self {
        let allowed = [
            "htop",
            "btop",
            "top",
            "nano",
            "vim",
            "nvim",
            "less",
            "man",
            "w3m",
            "lynx",
            "links",
            "elinks",
            "mc",
            "ranger",
            "nmtui",
            "alsamixer",
            "calcurse",
            "newsboat",
            "cmus",
            "mpv",
            "weechat",
            "irssi",
        ];
        let denied = [
            "sh",
            "bash",
            "zsh",
            "fish",
            "dash",
            "ksh",
            "csh",
            "tcsh",
            "busybox",
            "python",
            "python3",
            "perl",
            "ruby",
            "node",
            "lua",
            "php",
            "sudo",
            "su",
            "doas",
            "pkexec",
            "env",
            "xargs",
            "nohup",
            "setsid",
            "chroot",
            "curl",
            "wget",
            "nc",
            "ncat",
            "socat",
            "ssh",
            "scp",
            "sftp",
            "dd",
            "rm",
            "mkfs",
            "systemd-run",
        ];
        ProgramsConfig {
            allowed: allowed.iter().map(|p| (p.to_string(), PathBuf::from(format!("/usr/bin/{p}")))).collect(),
            allow_installed: false,
            search_path: vec!["/usr/bin".into(), "/usr/local/bin".into()],
            denied: denied.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl ProgramsConfig {
    /// Resolve a program name to an executable path, if it may be launched.
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        let denied =
            |n: &str| self.denied.iter().any(|d| d == n || n.starts_with(&format!("{d}.")) || n.starts_with("python"));
        if denied(name) {
            return None;
        }
        if let Some(p) = self.allowed.get(name).filter(|p| is_executable(p)) {
            return Some(p.clone());
        }
        if self.allow_installed {
            return self.search_path.iter().map(|d| d.join(name)).find(|p| is_executable(p));
        }
        None
    }

    /// Names that can currently be launched (installed allowlist entries).
    pub fn available(&self) -> Vec<String> {
        self.allowed.iter().filter(|(_, p)| is_executable(p)).map(|(n, _)| n.clone()).collect()
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriberKind {
    /// whisper.cpp `whisper-server` (model stays loaded).
    WhisperServer,
    /// whisper.cpp `whisper-cli` per utterance (no resident memory).
    WhisperCli,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceConfig {
    pub enabled: bool,
    pub transcriber: TranscriberKind,
    pub url: String,
    pub cli: PathBuf,
    pub model: PathBuf,
    /// Recording program (alsa-utils `arecord`).
    pub recorder: PathBuf,
    /// ALSA capture device.
    pub device: Option<String>,
    pub max_seconds: u32,
    pub language: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        VoiceConfig {
            enabled: true,
            transcriber: TranscriberKind::WhisperCli,
            url: "http://127.0.0.1:8081".into(),
            cli: "/usr/lib/core/whisper/bin/whisper-cli".into(),
            model: "/usr/share/core/models/whisper.bin".into(),
            recorder: "/usr/bin/arecord".into(),
            device: None,
            max_seconds: 30,
            language: "auto".into(),
        }
    }
}

impl AgentConfig {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = AgentConfig::from_toml("").unwrap();
        assert_eq!(c.inference.backend, BackendKind::Llama);
        let c = AgentConfig::from_toml("[inference]\nbackend = \"rescue\"\n[agent]\nmax_steps = 3\n").unwrap();
        assert_eq!(c.inference.backend, BackendKind::Rescue);
        assert_eq!(c.agent.max_steps, 3);
        assert!(AgentConfig::from_toml("[agent]\nbogus = 1").is_err());
    }

    #[test]
    fn shipped_config_is_valid() {
        let c = AgentConfig::from_toml(include_str!("../../../system/etc/core/agent.toml")).unwrap();
        assert_eq!(c.telemetry.path, PathBuf::from(core_protocol::DEFAULT_TELEMETRY_PATH));
        assert_eq!(c.guardian.socket, PathBuf::from(core_protocol::DEFAULT_GUARDIAN_SOCKET));
    }

    #[test]
    fn program_resolution() {
        let mut p = ProgramsConfig::default();
        p.allowed.insert("true".into(), "/bin/true".into());
        assert_eq!(p.resolve("true"), Some(PathBuf::from("/bin/true")));
        assert_eq!(p.resolve("bash"), None);
        assert_eq!(p.resolve("python3.12"), None);
        assert_eq!(p.resolve("ls"), None, "not allowlisted");
        p.allow_installed = true;
        p.search_path = vec!["/bin".into(), "/usr/bin".into()];
        assert!(p.resolve("ls").is_some());
        assert_eq!(p.resolve("sh"), None, "denied even when installed");
    }
}
