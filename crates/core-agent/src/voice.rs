//! Voice input: record speech with ALSA and transcribe it locally with whisper.cpp.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::config::{TranscriberKind, VoiceConfig};

pub trait Transcriber: Send {
    fn transcribe(&mut self, wav: &[u8]) -> Result<String, String>;
}

/// whisper.cpp's `whisper-server` (`POST /inference`, multipart form).
pub struct WhisperServer {
    url: String,
    language: String,
    agent: ureq::Agent,
}

impl WhisperServer {
    pub fn new(url: &str, language: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .build()
            .into();
        WhisperServer { url: url.trim_end_matches('/').to_string(), language: language.to_string(), agent }
    }
}

/// Build a multipart/form-data body. Returns (content type, body).
pub fn multipart(fields: &[(&str, &str)], file_field: &str, file_name: &str, file: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("core-{}", crate::voice::boundary_suffix());
    let mut body = Vec::with_capacity(file.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{file_name}\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn boundary_suffix() -> String {
    format!("{:x}{:x}", std::process::id(), core_protocol::time::unix_now())
}

impl Transcriber for WhisperServer {
    fn transcribe(&mut self, wav: &[u8]) -> Result<String, String> {
        let (content_type, body) = multipart(
            &[("response_format", "json"), ("temperature", "0.0"), ("language", &self.language)],
            "file",
            "speech.wav",
            wav,
        );
        let mut resp = self
            .agent
            .post(&format!("{}/inference", self.url))
            .header("Content-Type", &content_type)
            .send(&body[..])
            .map_err(|e| format!("speech recogniser unavailable: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
        if status != 200 {
            return Err(format!("speech recogniser returned HTTP {status}: {text}"));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(clean_transcript(v.get("text").and_then(Value::as_str).unwrap_or("")))
    }
}

/// whisper.cpp's `whisper-cli`, run once per utterance (no resident model memory).
pub struct WhisperCli {
    program: PathBuf,
    model: PathBuf,
    language: String,
}

impl WhisperCli {
    pub fn new(program: &Path, model: &Path, language: &str) -> Self {
        WhisperCli { program: program.into(), model: model.into(), language: language.into() }
    }
}

impl Transcriber for WhisperCli {
    fn transcribe(&mut self, wav: &[u8]) -> Result<String, String> {
        let file = temp_wav_path();
        fs::write(&file, wav).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
        let out = Command::new(&self.program)
            .args(["-m"])
            .arg(&self.model)
            .args(["-f"])
            .arg(&file)
            .args(["-l", &self.language, "--no-timestamps", "--no-prints"])
            .stdin(Stdio::null())
            .output();
        let _ = fs::remove_file(&file);
        let out = out.map_err(|e| format!("cannot run {}: {e}", self.program.display()))?;
        if !out.status.success() {
            return Err(format!("whisper-cli failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(clean_transcript(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Whisper marks silence and noise with bracketed tags; drop them and tidy spacing.
pub fn clean_transcript(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn temp_wav_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("core-voice-{}-{}.wav", std::process::id(), core_protocol::time::unix_now()))
}

/// Push-to-talk recording with `arecord` (16 kHz mono 16-bit, what whisper expects).
pub struct Recording {
    child: Child,
    file: PathBuf,
}

pub fn start_recording(config: &VoiceConfig) -> Result<Recording, String> {
    let file = temp_wav_path();
    let mut cmd = Command::new(&config.recorder);
    cmd.args(["-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "wav", "-d"]).arg(config.max_seconds.to_string());
    if let Some(dev) = &config.device {
        cmd.args(["-D", dev]);
    }
    let child = cmd
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", config.recorder.display()))?;
    Ok(Recording { child, file })
}

impl Recording {
    /// Stop recording (SIGINT lets arecord finalise the WAV header) and return the audio.
    pub fn stop(self) -> Result<Vec<u8>, String> {
        // SAFETY: signalling our own child process.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        let out = self.child.wait_with_output().map_err(|e| e.to_string())?;
        let audio = fs::read(&self.file)
            .map_err(|_| format!("no audio was recorded: {}", String::from_utf8_lossy(&out.stderr).trim()));
        let _ = fs::remove_file(&self.file);
        let audio = audio?;
        if audio.len() <= 44 {
            return Err("the recording is empty; check the microphone".into());
        }
        Ok(audio)
    }
}

pub fn transcriber(config: &VoiceConfig) -> Box<dyn Transcriber> {
    match config.transcriber {
        TranscriberKind::WhisperServer => Box::new(WhisperServer::new(&config.url, &config.language)),
        TranscriberKind::WhisperCli => Box::new(WhisperCli::new(&config.cli, &config.model, &config.language)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_cleanup() {
        assert_eq!(clean_transcript(" [BLANK_AUDIO]  Restart   the wifi. (music) "), "Restart the wifi.");
        assert_eq!(clean_transcript("[inaudible]"), "");
    }

    #[test]
    fn multipart_layout() {
        let (ct, body) = multipart(&[("response_format", "json")], "file", "a.wav", b"RIFF");
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n"
        )));
        assert!(text.contains("filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n"));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    }
}
