//! Against a real llama.cpp server: the generated grammar is accepted and sampling is
//! constrained by it. Run with tools/e2e/run.sh (or point CORE_E2E_LLAMA_URL at any
//! llama-server and pass `--ignored`).

use std::time::Duration;

use core_agent::backend::{ChatMessage, CompletionRequest, InferenceBackend, LlamaServer};
use core_protocol::grammar::{GrammarOptions, gbnf};
use core_protocol::{Intent, ValidatedAction, ValidationError};

fn server() -> LlamaServer {
    let url = std::env::var("CORE_E2E_LLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let mut s = LlamaServer::new(&url, None, Duration::from_secs(600));
    s.health().expect("llama-server must be running (see tools/e2e/run.sh)");
    s
}

#[test]
#[ignore = "needs a running llama-server; see tools/e2e/run.sh"]
fn llama_server_output_obeys_the_grammar() {
    let mut llama = server();
    // Only actions whose arguments are bounded, so even a model that knows nothing
    // must finish a complete intent within the token budget.
    let allowed = ["disk_usage", "reboot", "set_volume", "set_mute", "list_services", "list_hardware"];
    let grammar = gbnf(&GrammarOptions { actions: Some(&allowed), thought_max: 24 });
    for (i, request) in ["check disk usage", "reboot", "mute the sound", "list failed services"].iter().enumerate() {
        let messages = [
            ChatMessage::system("You are C.O.R.E. Answer with one JSON action."),
            ChatMessage::user(format!("REQUEST: {request}")),
        ];
        let out = llama
            .complete(&CompletionRequest {
                messages: &messages,
                grammar: Some(&grammar),
                max_tokens: 160,
                temperature: 0.3 + i as f32 * 0.2,
            })
            .expect("completion");
        let intent =
            Intent::parse(&out).unwrap_or_else(|e| panic!("grammar-constrained output must parse ({e}): {out}"));
        assert!(allowed.contains(&intent.action.as_str()), "action outside the grammar: {out}");
        match ValidatedAction::from_intent(&intent) {
            Ok(_) => {}
            // The grammar bounds digit count, not numeric range; the validator catches the rest.
            Err(ValidationError::InvalidParam { param: "percent", .. }) => {}
            Err(e) => panic!("grammar admitted an invalid intent ({e}): {out}"),
        }
    }
}

#[test]
#[ignore = "needs a running llama-server; see tools/e2e/run.sh"]
fn full_catalog_grammar_is_accepted() {
    let mut llama = server();
    let grammar = gbnf(&GrammarOptions::default());
    let messages = [ChatMessage::system("You are C.O.R.E."), ChatMessage::user("REQUEST: what is my IP address?")];
    // A tiny test model may run out of tokens inside a free-text field; what matters
    // is that the server accepted the grammar and the output is a valid prefix.
    let out = llama
        .complete(&CompletionRequest { messages: &messages, grammar: Some(&grammar), max_tokens: 64, temperature: 0.2 })
        .expect("llama-server rejected the full grammar");
    assert!(out.starts_with("{"), "{out}");
    assert!(out.contains("\"thought\""), "{out}");
}
