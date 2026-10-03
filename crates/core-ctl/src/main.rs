//! core-ctl: administration and development tool for C.O.R.E.
//!
//! Inspect the action catalog and grammar, validate intents offline, drive the
//! Guardian directly (no model involved), read telemetry and the audit log, and run
//! `doctor` to check that every component of the system is healthy.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use core_agent::backend::{InferenceBackend, LlamaServer, RescuePlanner};
use core_agent::config::{BackendKind, TranscriberKind};
use core_agent::guardian::{GuardianClient, SocketGuardian};
use core_agent::telemetry::StaticTelemetry;
use core_agent::{Agent, AgentConfig};
use core_protocol::grammar::{GrammarOptions, gbnf};
use core_protocol::wire::Response;
use core_protocol::{CATALOG, Category, Intent, ValidatedAction};
use core_sense::{Sensor, Sysroot, age_secs, read_snapshot, section, summary};

#[derive(Parser)]
#[command(name = "core-ctl", version, about = "C.O.R.E. administration tool")]
struct Cli {
    /// Agent configuration (for socket paths, model URL, ...).
    #[arg(long, global = true, default_value = "/etc/core/agent.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the GBNF grammar that constrains the model.
    Grammar {
        /// Restrict to these actions (comma separated).
        #[arg(long, value_delimiter = ',')]
        actions: Vec<String>,
    },
    /// List every action with its risk and executor.
    Catalog {
        #[arg(long)]
        json: bool,
    },
    /// Print the system prompt the agent would use (asks the Guardian for its policy).
    Prompt,
    /// Validate an intent JSON offline and show its normalised form.
    Validate { intent: String },
    /// Show the action contract's version and fingerprint (docs/TRAINING.md).
    Contract {
        /// Print the canonical text the fingerprint is computed over.
        #[arg(long)]
        canonical: bool,
    },
    /// Send an intent straight to the Guardian (prompts for confirmation if needed).
    Exec {
        intent: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Show telemetry (live, or the published snapshot with --published).
    Telemetry {
        #[arg(long)]
        summary: bool,
        #[arg(long)]
        section: Option<String>,
        #[arg(long)]
        published: bool,
    },
    /// Show recent Guardian audit log entries.
    Audit {
        #[arg(short = 'n', default_value_t = 20)]
        lines: usize,
        #[arg(long, default_value = "/var/log/core/audit.jsonl")]
        path: PathBuf,
    },
    /// Check the health of every C.O.R.E. component.
    Doctor,
    /// Answer one request with the rescue planner and print the intent it chose.
    Rescue { request: Vec<String> },
}

fn main() -> ExitCode {
    // Behave like a normal Unix tool when piped into `head`: exit quietly on EPIPE.
    // SAFETY: restoring the default disposition of SIGPIPE before any threads exist.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    core_protocol::logging::init(0);
    let config = if cli.config.exists() {
        AgentConfig::load(&cli.config).unwrap_or_else(|e| {
            eprintln!("warning: {e}; using defaults");
            AgentConfig::default()
        })
    } else {
        AgentConfig::default()
    };
    match run(cli.command, &config) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cmd: Cmd, config: &AgentConfig) -> Result<bool, String> {
    match cmd {
        Cmd::Grammar { actions } => {
            let names: Vec<&str> = actions.iter().map(String::as_str).collect();
            for n in &names {
                core_protocol::catalog::find(n).ok_or_else(|| format!("unknown action {n}"))?;
            }
            let opts = GrammarOptions { actions: (!names.is_empty()).then_some(&names[..]), ..Default::default() };
            print!("{}", gbnf(&opts));
        }
        Cmd::Catalog { json } => catalog(json),
        Cmd::Prompt => {
            let guardian = SocketGuardian::new(&config.guardian.socket);
            let agent = Agent::new(
                config.clone(),
                Box::new(RescuePlanner),
                Box::new(guardian),
                Box::new(StaticTelemetry(Default::default())),
            );
            println!("{}", agent.system_prompt());
        }
        Cmd::Contract { canonical } => {
            if canonical {
                print!("{}", core_protocol::contract::canonical());
            } else {
                println!("contract:           {}", core_protocol::contract::CONTRACT_VERSION);
                println!("observation format: {}", core_protocol::contract::OBSERVATION_FORMAT_VERSION);
                println!("protocol:           {}", core_protocol::PROTOCOL_VERSION);
                println!("fingerprint:        {}", core_protocol::contract::fingerprint());
            }
        }
        Cmd::Validate { intent } => {
            let parsed = Intent::parse(&intent).map_err(|e| e.to_string())?;
            match ValidatedAction::from_intent(&parsed) {
                Ok(v) => {
                    println!("valid: {}", v.describe());
                    println!("risk:  {}", v.risk());
                    println!("args:  {}", v.redacted_args());
                }
                Err(e) => {
                    println!("invalid: {e}");
                    return Ok(false);
                }
            }
        }
        Cmd::Exec { intent, socket } => return exec(&intent, socket.as_deref().unwrap_or(&config.guardian.socket)),
        Cmd::Telemetry { summary: want_summary, section: sec, published } => {
            let snap = if published {
                read_snapshot(&config.telemetry.path)
                    .map_err(|e| format!("{}: {e}", config.telemetry.path.display()))?
            } else {
                Sensor::new(Sysroot::live()).snapshot()
            };
            if want_summary {
                println!("{}", summary(&snap));
            } else if let Some(name) = sec {
                let v = section(&snap, &name).ok_or_else(|| format!("unknown section {name}"))?;
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                println!("{}", serde_json::to_string_pretty(&snap).unwrap_or_default());
            }
        }
        Cmd::Audit { lines, path } => {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let all: Vec<&str> = text.lines().collect();
            for line in &all[all.len().saturating_sub(lines)..] {
                let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
                let ok = match v.get("success").and_then(|s| s.as_bool()) {
                    Some(true) => " ok",
                    Some(false) => " FAILED",
                    None => "",
                };
                println!(
                    "{} uid={} {:<22} {:<22}{ok} {}",
                    v["ts"].as_str().unwrap_or("?"),
                    v["peer"]["uid"],
                    v["action"].as_str().unwrap_or("-"),
                    v["decision"].as_str().unwrap_or("-"),
                    v.get("reason").and_then(|r| r.as_str()).unwrap_or("")
                );
            }
        }
        Cmd::Doctor => return Ok(doctor(config)),
        Cmd::Rescue { request } => {
            let (_, action, args) = core_agent::backend::rescue_plan(&request.join(" "), "");
            println!("{}", serde_json::json!({"action": action, "args": args}));
        }
    }
    Ok(true)
}

fn catalog(json: bool) {
    if json {
        let entries: Vec<serde_json::Value> = CATALOG
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "summary": s.summary,
                    "risk": s.risk,
                    "executor": s.executor,
                    "category": s.category,
                    "params": s.params.iter().map(|p| serde_json::json!({
                        "name": p.name, "type": p.kind.describe(), "required": p.required, "doc": p.doc
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries).unwrap_or_default());
        return;
    }
    for cat in Category::ALL {
        println!("{}", cat.title());
        for s in CATALOG.iter().filter(|s| s.category == cat) {
            let params: Vec<String> =
                s.params.iter().map(|p| format!("{}{}", p.name, if p.required { "" } else { "?" })).collect();
            println!(
                "  {:<22} {:<8} {:<9} {}({})",
                s.name,
                s.risk,
                format!("{:?}", s.executor).to_lowercase(),
                s.name,
                params.join(", ")
            );
        }
    }
}

fn exec(intent: &str, socket: &Path) -> Result<bool, String> {
    let intent = Intent::parse(intent).map_err(|e| e.to_string())?;
    let mut g = SocketGuardian::new(socket);
    let mut response = g.execute(1, &intent).map_err(|e| e.to_string())?;
    if let Response::ConfirmationRequired { token, summary, risk, .. } = &response {
        eprint!("{summary} [{risk} risk] - allow? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        let approve = matches!(answer.trim(), "y" | "Y" | "yes");
        response = g.confirm(token, approve).map_err(|e| e.to_string())?;
    }
    match response {
        Response::Executed { report, .. } => {
            print!("{}", report.combined_output());
            eprintln!(
                "{} ({} ms{})",
                if report.success { "succeeded" } else { "FAILED" },
                report.duration_ms,
                if report.dry_run { ", dry run" } else { "" }
            );
            Ok(report.success)
        }
        Response::Rejected { kind, reason, .. } => {
            eprintln!("rejected ({kind:?}): {reason}");
            Ok(false)
        }
        other => {
            eprintln!("unexpected response: {other:?}");
            Ok(false)
        }
    }
}

fn check(ok: bool, what: &str, detail: &str) -> bool {
    println!(
        "[{}] {what}{}",
        if ok { " ok " } else { "FAIL" },
        if detail.is_empty() { String::new() } else { format!(": {detail}") }
    );
    ok
}

fn doctor(config: &AgentConfig) -> bool {
    let mut healthy = true;

    let mut guardian = SocketGuardian::new(&config.guardian.socket);
    healthy &= match guardian.capabilities() {
        Ok(caps) => check(
            true,
            "guardian",
            &format!("{} actions available at {}", caps.len(), config.guardian.socket.display()),
        ),
        Err(e) => check(false, "guardian", &e.to_string()),
    };

    healthy &= match config.inference.backend {
        BackendKind::Rescue => check(true, "inference", "rescue mode configured (no model)"),
        BackendKind::Llama => {
            let mut llama =
                LlamaServer::new(&config.inference.url, config.inference.model.clone(), Duration::from_secs(5));
            match llama.health() {
                Ok(()) => check(true, "inference", &llama.name()),
                Err(e) => check(false, "inference", &e.to_string()),
            }
        }
    };

    let path = &config.telemetry.path;
    healthy &= match (age_secs(path), read_snapshot(path)) {
        (Some(age), Ok(snap)) => check(
            age <= config.telemetry.max_age_secs,
            "telemetry",
            &format!(
                "{} is {age}s old, {} insights, kernel log {}",
                path.display(),
                snap.insights.len(),
                if snap.kernel_log.available { "available" } else { "unavailable" }
            ),
        ),
        _ => check(false, "telemetry", &format!("{} missing; is core-sensed running?", path.display())),
    };

    let models: Vec<String> = std::fs::read_dir("/usr/share/core/models")
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let model_ok = check(models.iter().any(|m| m.ends_with(".gguf")), "language model file", &models.join(", "));
    if config.inference.backend == BackendKind::Llama {
        healthy &= model_ok;
    }

    if config.voice.enabled {
        let recorder = config.voice.recorder.exists();
        let transcriber = match config.voice.transcriber {
            TranscriberKind::WhisperCli => config.voice.cli.exists() && config.voice.model.exists(),
            TranscriberKind::WhisperServer => true,
        };
        check(
            recorder && transcriber,
            "voice",
            &format!("recorder {}, transcriber {}", present(recorder), present(transcriber)),
        );
    }
    println!("{}", if healthy { "C.O.R.E. is healthy." } else { "Some components need attention." });
    healthy
}

fn present(ok: bool) -> &'static str {
    if ok { "present" } else { "missing" }
}
