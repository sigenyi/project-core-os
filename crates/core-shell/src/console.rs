//! The console: C.O.R.E.'s entire user interface is a line of text.
//!
//! No windows, widgets or graphics stack. Output is plain lines with optional ANSI
//! colour; on the Linux virtual console (TERM=linux) only ASCII glyphs are used
//! because the kernel console fonts lack most symbols.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use core_agent::{AgentEvent, ConfirmRequest, Frontend};
use core_protocol::Risk;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

/// Set by SIGINT while a request is being processed.
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Route Ctrl-C to [`INTERRUPTED`] instead of killing the shell. (rustyline handles
/// Ctrl-C itself while reading a line.)
pub fn install_sigint_handler() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigint as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

struct Style {
    color: bool,
    ascii: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
    }
    fn dim(&self, t: &str) -> String {
        self.paint("2", t)
    }
    fn red(&self, t: &str) -> String {
        self.paint("31", t)
    }
    fn yellow(&self, t: &str) -> String {
        self.paint("33", t)
    }
    fn cyan(&self, t: &str) -> String {
        self.paint("36", t)
    }
    fn bold(&self, t: &str) -> String {
        self.paint("1", t)
    }
    fn arrow(&self) -> &'static str {
        if self.ascii { "->" } else { "→" }
    }
    fn cross(&self) -> &'static str {
        if self.ascii { "x" } else { "✗" }
    }
    fn warn(&self) -> &'static str {
        if self.ascii { "!" } else { "⚠" }
    }
}

pub struct Console {
    editor: Option<DefaultEditor>,
    style: Style,
    interactive: bool,
    pub verbose: bool,
    /// A transient status line ("thinking") is on screen.
    status_shown: bool,
    history_file: Option<std::path::PathBuf>,
}

impl Console {
    pub fn new() -> Self {
        let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        let term = std::env::var("TERM").unwrap_or_default();
        let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() && term != "dumb";
        let editor = if interactive { DefaultEditor::new().ok() } else { None };
        let history_file = std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share/core/history"));
        let mut console = Console {
            editor,
            style: Style { color, ascii: term == "linux" || term == "dumb" || term.is_empty() },
            interactive,
            verbose: false,
            status_shown: false,
            history_file,
        };
        if let (Some(ed), Some(file)) = (console.editor.as_mut(), console.history_file.as_ref()) {
            let _ = ed.load_history(file);
        }
        console
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Read one line. `None` means end of input (Ctrl-D / EOF).
    pub fn read_line(&mut self, prompt: &str) -> Option<String> {
        self.clear_status();
        match self.editor.as_mut() {
            Some(ed) => loop {
                match ed.readline(prompt) {
                    Ok(line) => {
                        if !line.trim().is_empty() {
                            let _ = ed.add_history_entry(line.as_str());
                        }
                        return Some(line);
                    }
                    Err(ReadlineError::Interrupted) => continue, // Ctrl-C clears the line
                    Err(_) => return None,
                }
            },
            None => {
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) | Err(_) => None,
                    Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
                }
            }
        }
    }

    pub fn save_history(&mut self) {
        if let (Some(ed), Some(file)) = (self.editor.as_mut(), self.history_file.as_ref()) {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = ed.save_history(file);
        }
    }

    pub fn prompt(&self) -> String {
        if self.style.ascii { "> ".into() } else { "› ".into() }
    }

    fn clear_status(&mut self) {
        if self.status_shown {
            print!("\r\x1b[2K");
            let _ = std::io::stdout().flush();
            self.status_shown = false;
        }
    }

    fn line(&mut self, text: &str) {
        self.clear_status();
        println!("{text}");
    }

    pub fn banner(&mut self, lines: &[String]) {
        self.line(&self.style.bold("C.O.R.E.  say what you need  (/help for commands)"));
        for l in lines {
            let l = self.style.dim(l);
            self.line(&l);
        }
    }

    pub fn reply(&mut self, text: &str) {
        self.line(text);
    }

    pub fn question(&mut self, text: &str) {
        let q = self.style.cyan(text);
        self.line(&q);
    }

    pub fn error(&mut self, text: &str) {
        let t = self.style.red(text);
        self.line(&t);
    }

    pub fn info(&mut self, text: &str) {
        let t = self.style.dim(text);
        self.line(&t);
    }

    pub fn notice(&mut self, text: &str) {
        let t = self.style.yellow(&format!("{} {text}", self.style.warn()));
        self.line(&t);
    }
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}

impl Frontend for Console {
    fn event(&mut self, event: AgentEvent<'_>) {
        match event {
            AgentEvent::Thinking { .. } => {
                if self.interactive {
                    self.clear_status();
                    print!("{}", self.style.dim("  thinking..."));
                    let _ = std::io::stdout().flush();
                    self.status_shown = true;
                }
            }
            AgentEvent::Thought(t) if self.verbose => {
                let t = self.style.dim(&format!("  ({t})"));
                self.line(&t);
            }
            AgentEvent::Thought(_) => {}
            AgentEvent::ActionStarted { description, .. } => {
                let t = format!("  {} {description}", self.style.arrow());
                self.line(&t);
            }
            AgentEvent::ActionFinished { success: false, detail, .. } => {
                let t = self.style.red(&format!("    {} {detail}", self.style.cross()));
                self.line(&t);
            }
            AgentEvent::ActionFinished { .. } => {}
            AgentEvent::Notice(n) => self.notice(n),
        }
    }

    fn confirm(&mut self, request: &ConfirmRequest<'_>) -> bool {
        let risk = match request.risk {
            Risk::High => self.style.red("high risk"),
            Risk::Medium => self.style.yellow("medium risk"),
            r => r.to_string(),
        };
        let head = format!("  {} {}  [{risk}]", self.style.warn(), self.style.bold(request.summary));
        self.line(&head);
        let answer = self.read_line("    Allow? [y/N] ").unwrap_or_default();
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    fn launch(&mut self, program: &Path, args: &[String]) -> Result<i32, String> {
        self.clear_status();
        // Caught signals revert to their defaults across exec, so the program gets a
        // normal Ctrl-C while the shell merely records it (and forgets it afterwards).
        let status =
            Command::new(program).args(args).status().map_err(|e| format!("cannot start {}: {e}", program.display()));
        INTERRUPTED.store(false, Ordering::SeqCst);
        status.map(|s| s.code().unwrap_or(-1))
    }

    fn cancelled(&mut self) -> bool {
        INTERRUPTED.swap(false, Ordering::SeqCst)
    }
}
