//! llama.cpp `llama-server` backend.
//!
//! Uses the OpenAI-compatible chat endpoint so the server applies the model's own chat
//! template, and passes the GBNF grammar so sampling can only produce a valid intent.
//! Connections are local-only: proxies are ignored by design (offline-first).

use std::time::Duration;

use serde_json::{Value, json};

use super::{BackendError, CompletionRequest, InferenceBackend};

pub struct LlamaServer {
    base_url: String,
    model: Option<String>,
    agent: ureq::Agent,
}

impl LlamaServer {
    pub fn new(base_url: &str, model: Option<String>, timeout: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(timeout))
            .timeout_connect(Some(Duration::from_secs(3)))
            .http_status_as_error(false)
            .build()
            .into();
        LlamaServer { base_url: base_url.trim_end_matches('/').to_string(), model, agent }
    }

    pub fn request_body(&self, req: &CompletionRequest) -> Value {
        let messages: Vec<Value> =
            req.messages.iter().map(|m| json!({"role": m.role.as_str(), "content": m.content})).collect();
        let mut body = json!({
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
            "stream": false,
            "cache_prompt": true,
            // Hybrid "thinking" models must answer directly: the grammar starts at token one.
            "chat_template_kwargs": {"enable_thinking": false},
        });
        if let Some(g) = req.grammar {
            body["grammar"] = Value::from(g);
        }
        if let Some(m) = &self.model {
            body["model"] = Value::from(m.as_str());
        }
        body
    }
}

/// Pull the assistant text out of an OpenAI-style chat completion.
pub fn extract_content(response: &Value) -> Result<String, BackendError> {
    let choice = response
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| BackendError::BadResponse(format!("no choices in {}", clip(&response.to_string()))))?;
    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| BackendError::BadResponse("choice has no message content".into()))?;
    if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        log::warn!("model output hit max_tokens; it may be truncated");
    }
    Ok(content.to_string())
}

fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

impl InferenceBackend for LlamaServer {
    fn name(&self) -> String {
        format!("llama.cpp at {}", self.base_url)
    }

    fn complete(&mut self, req: &CompletionRequest) -> Result<String, BackendError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = self.request_body(req);
        let mut resp = self.agent.post(&url).send_json(&body).map_err(|e| BackendError::Unavailable(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| BackendError::BadResponse(e.to_string()))?;
        match status {
            200 => {}
            503 => return Err(BackendError::Unavailable("the model is still loading".into())),
            _ => return Err(BackendError::BadResponse(format!("HTTP {status}: {}", clip(&text)))),
        }
        let value: Value = serde_json::from_str(&text).map_err(|e| BackendError::BadResponse(e.to_string()))?;
        extract_content(&value)
    }

    fn health(&mut self) -> Result<(), BackendError> {
        let url = format!("{}/health", self.base_url);
        let resp = self.agent.get(&url).call().map_err(|e| BackendError::Unavailable(e.to_string()))?;
        match resp.status().as_u16() {
            200 => Ok(()),
            503 => Err(BackendError::Unavailable("the model is still loading".into())),
            s => Err(BackendError::Unavailable(format!("health check returned HTTP {s}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ChatMessage;
    use super::*;

    #[test]
    fn body_carries_grammar_and_messages() {
        let s = LlamaServer::new("http://127.0.0.1:8080/", Some("qwen".into()), Duration::from_secs(5));
        let msgs = [ChatMessage::system("sys"), ChatMessage::user("hi")];
        let body = s.request_body(&CompletionRequest {
            messages: &msgs,
            grammar: Some("root ::= \"x\""),
            max_tokens: 64,
            temperature: 0.1,
        });
        assert_eq!(body["messages"][1], json!({"role": "user", "content": "hi"}));
        assert_eq!(body["grammar"], json!("root ::= \"x\""));
        assert_eq!(body["model"], json!("qwen"));
        assert_eq!(body["max_tokens"], json!(64));
        assert_eq!(s.name(), "llama.cpp at http://127.0.0.1:8080");
    }

    #[test]
    fn content_extraction() {
        let ok = json!({"choices": [{"message": {"role": "assistant", "content": "{\"action\":\"x\"}"}, "finish_reason": "stop"}]});
        assert_eq!(extract_content(&ok).unwrap(), "{\"action\":\"x\"}");
        assert!(extract_content(&json!({"error": "boom"})).is_err());
        assert!(extract_content(&json!({"choices": [{"message": {}}]})).is_err());
    }

    #[test]
    fn unreachable_server_is_unavailable() {
        let mut s = LlamaServer::new("http://127.0.0.1:9", None, Duration::from_secs(2));
        assert!(matches!(s.health(), Err(BackendError::Unavailable(_))));
    }
}
