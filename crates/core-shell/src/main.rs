//! core-shell: the C.O.R.E. login shell.
//!
//! Replaces bash as the user's shell. The whole interface is a text prompt (typed or,
//! with `/voice`, spoken). Requests go to the local agent; system changes go through
//! the Guardian; risky ones are confirmed here, by the human, never by the model.

mod console;
mod inprocess;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use clap::{Parser, ValueEnum};
use console::{Console, INTERRUPTED, install_sigint_handler};
use core_agent::backend::RescuePlanner;
use core_agent::config::BackendKind;
use core_agent::guardian::{GuardianClient, SocketGuardian};
use core_agent::telemetry::PublishedTelemetry;
use core_agent::{Agent, AgentConfig, Outcome, backend_from_config, voice};
use inprocess::InProcessGuardian;

#[derive(Clone, Copy, ValueEnum)]
enum BackendArg {
    Llama,
    Rescue,
    /// Replay intents from --script (testing without a model).
    Script,
}

#[derive(Parser)]
#[command(name = "core-shell", version, about = "C.O.R.E. conversational shell")]
struct Cli {
    /// Agent configuration file.
    #[arg(long, default_value = "/etc/core/agent.toml")]
    config: PathBuf,
    /// Development mode: in-process dry-run Guardian; needs no root and no services.
    #[arg(long)]
    dev: bool,
    /// Override the inference backend.
    #[arg(long, value_enum)]
    backend: Option<BackendArg>,
    /// Intent script for `--backend script` (one intent per line).
    #[arg(long)]
    script: Option<PathBuf>,
    /// Override the llama-server URL.
    #[arg(long)]
    llama_url: Option<String>,
    /// Override the Guardian socket.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Handle one request and exit.
    #[arg(short = 'c', long = "command")]
    command: Option<String>,
    /// Increase log verbosity (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

const HELP: &str =
    "Just type (or say) what you want, e.g. \"my wifi is not working\" or \"install a text web browser\".
Commands:
  /voice, /v     speak a request (push to talk)
  /status        show what the system currently knows about the machine
  /verbose       show or hide the AI's reasoning
  /rescue        use the rule-based rescue mode instead of the language model
  /model         go back to the language model
  /clear         forget the conversation and clear the screen
  /prompt        show the system prompt (debugging)
  /grammar       show the output grammar (debugging)
  /exit          log out
Ctrl-C cancels the current request after the running step.";

fn load_config(cli: &Cli) -> AgentConfig {
    let mut config = if cli.config.exists() {
        AgentConfig::load(&cli.config).unwrap_or_else(|e| {
            eprintln!("warning: {e}; using defaults");
            AgentConfig::default()
        })
    } else {
        AgentConfig::default()
    };
    if let Some(url) = &cli.llama_url {
        config.inference.url = url.clone();
    }
    match cli.backend {
        Some(BackendArg::Llama) => config.inference.backend = BackendKind::Llama,
        Some(BackendArg::Rescue) => config.inference.backend = BackendKind::Rescue,
        Some(BackendArg::Script) => config.inference.backend = BackendKind::Script,
        None => {}
    }
    if let Some(script) = &cli.script {
        config.inference.script = Some(script.clone());
    }
    if let Some(socket) = &cli.socket {
        config.guardian.socket = socket.clone();
    }
    config
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    core_protocol::logging::init(cli.verbose);
    let config = load_config(&cli);

    let guardian: Box<dyn GuardianClient> = if cli.dev {
        Box::new(InProcessGuardian::dry_run())
    } else {
        Box::new(SocketGuardian::new(&config.guardian.socket))
    };
    let telemetry = Box::new(PublishedTelemetry::new(&config.telemetry.path, config.telemetry.max_age_secs));
    let mut agent = Agent::new(config.clone(), backend_from_config(&config), guardian, telemetry);
    let mut console = Console::new();

    if let Some(request) = &cli.command {
        return match process(&mut agent, &mut console, request) {
            true => ExitCode::SUCCESS,
            false => ExitCode::FAILURE,
        };
    }

    install_sigint_handler();
    let model_state = match agent.backend_health() {
        Ok(()) => "ready".to_string(),
        Err(e) if config.inference.fallback_to_rescue => format!("{e}; rescue mode will answer meanwhile"),
        Err(e) => e.to_string(),
    };
    console.banner(&[
        format!("model: {} ({model_state})", agent.backend_name()),
        format!("executor: {}", agent.guardian_name()),
    ]);

    loop {
        let prompt = console.prompt();
        let Some(line) = console.read_line(&prompt) else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request = match line.strip_prefix('/') {
            Some(cmd) => match builtin(cmd.trim(), &mut agent, &mut console, &config) {
                Builtin::Exit => break,
                Builtin::Done => continue,
                Builtin::Request(text) => text,
            },
            None => line.to_string(),
        };
        process(&mut agent, &mut console, &request);
    }
    console.save_history();
    ExitCode::SUCCESS
}

/// Run one request; true if it ended in an answer.
fn process(agent: &mut Agent, console: &mut Console, request: &str) -> bool {
    INTERRUPTED.store(false, Ordering::SeqCst);
    match agent.handle(request, console) {
        Outcome::Reply(m) => {
            console.reply(&m);
            true
        }
        Outcome::Question(q) => {
            console.question(&q);
            true
        }
        Outcome::Failed(m) => {
            console.error(&m);
            false
        }
        Outcome::Cancelled => {
            console.info("Cancelled.");
            false
        }
    }
}

enum Builtin {
    Exit,
    Done,
    Request(String),
}

fn builtin(cmd: &str, agent: &mut Agent, console: &mut Console, config: &AgentConfig) -> Builtin {
    match cmd {
        "exit" | "logout" | "quit" => return Builtin::Exit,
        "help" | "?" => console.info(HELP),
        "status" => console.reply(&agent.telemetry_summary()),
        "verbose" => {
            console.verbose = !console.verbose;
            console.info(if console.verbose { "Showing reasoning." } else { "Hiding reasoning." });
        }
        "rescue" => {
            agent.set_backend(Box::new(RescuePlanner));
            console.notice("Rescue mode: simple commands only. /model returns to the language model.");
        }
        "model" => {
            let mut c = config.clone();
            c.inference.backend = BackendKind::Llama;
            agent.set_backend(backend_from_config(&c));
            match agent.backend_health() {
                Ok(()) => console.info(&format!("Using {}.", agent.backend_name())),
                Err(e) => console.notice(&e.to_string()),
            }
        }
        "clear" => {
            agent.reset();
            if console.is_interactive() {
                print!("\x1b[2J\x1b[H");
            }
        }
        "prompt" => console.reply(agent.system_prompt()),
        "grammar" => console.reply(agent.grammar()),
        "voice" | "v" => {
            if let Some(text) = listen(console, config) {
                return Builtin::Request(text);
            }
        }
        other => console.error(&format!("Unknown command /{other}. Try /help.")),
    }
    Builtin::Done
}

fn listen(console: &mut Console, config: &AgentConfig) -> Option<String> {
    if !config.voice.enabled {
        console.error("Voice input is disabled in agent.toml.");
        return None;
    }
    let recording = match voice::start_recording(&config.voice) {
        Ok(r) => r,
        Err(e) => {
            console.error(&e);
            return None;
        }
    };
    console.info(&format!("Listening (up to {}s)... press Enter when you are done.", config.voice.max_seconds));
    let _ = console.read_line("");
    let audio = match recording.stop() {
        Ok(a) => a,
        Err(e) => {
            console.error(&e);
            return None;
        }
    };
    console.info("Transcribing...");
    match voice::transcriber(&config.voice).transcribe(&audio) {
        Ok(text) if !text.is_empty() => {
            console.info(&format!("You said: {text}"));
            Some(text)
        }
        Ok(_) => {
            console.error("I did not catch that. Try again with /voice.");
            None
        }
        Err(e) => {
            console.error(&e);
            None
        }
    }
}
