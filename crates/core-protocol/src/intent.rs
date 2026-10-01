//! The raw, untrusted structure the model emits.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A model's request to perform one action, before validation.
///
/// Wire format (enforced by the grammar, accepted leniently here):
/// `{"thought": "...", "action": "restart_service", "args": {"service": "bluetooth"}}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    /// The model's one-line reasoning. Informational only; never acted upon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<String>,
    pub action: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub args: Map<String, Value>,
}

fn null_as_empty<'de, D>(d: D) -> Result<Map<String, Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Map<String, Value>>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentParseError {
    /// No JSON object could be located in the text.
    NoJson,
    /// A JSON object was found but did not have the intent shape.
    Malformed(String),
}

impl fmt::Display for IntentParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntentParseError::NoJson => f.write_str("output contained no JSON object"),
            IntentParseError::Malformed(e) => write!(f, "output was not a valid intent: {e}"),
        }
    }
}

impl std::error::Error for IntentParseError {}

impl Intent {
    pub fn new(action: impl Into<String>, args: Value) -> Self {
        let args = match args {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        Intent { thought: None, action: action.into(), args }
    }

    pub fn with_thought(mut self, thought: impl Into<String>) -> Self {
        self.thought = Some(thought.into());
        self
    }

    /// Extract an intent from model output.
    ///
    /// With grammar-constrained sampling the output is exactly one JSON object, but
    /// backends without grammar support may wrap it in prose or Markdown fences, so the
    /// first balanced top-level object is located and parsed.
    pub fn parse(text: &str) -> Result<Intent, IntentParseError> {
        let json = extract_json_object(text).ok_or(IntentParseError::NoJson)?;
        let intent: Intent =
            serde_json::from_str(json).map_err(|e| IntentParseError::Malformed(e.to_string()))?;
        if intent.action.trim().is_empty() {
            return Err(IntentParseError::Malformed("\"action\" is empty".into()));
        }
        Ok(intent)
    }

    /// Compact JSON form (what the model would have produced).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("intent serialisation cannot fail")
    }
}

/// Find the first balanced `{...}` in `text`, honouring JSON string escapes.
pub fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_strict_output() {
        let i = Intent::parse(r#"{"thought":"wifi is down","action":"restart_service","args":{"service":"iwd"}}"#).unwrap();
        assert_eq!(i.action, "restart_service");
        assert_eq!(i.args["service"], json!("iwd"));
        assert_eq!(i.thought.as_deref(), Some("wifi is down"));
    }

    #[test]
    fn parses_wrapped_output() {
        let text = "Sure! Here is the command:\n```json\n{\"action\": \"disk_usage\", \"args\": {}}\n```\nLet me know.";
        let i = Intent::parse(text).unwrap();
        assert_eq!(i.action, "disk_usage");
        assert!(i.args.is_empty());
    }

    #[test]
    fn braces_inside_strings_do_not_confuse_extraction() {
        let text = r#"{"action":"respond","args":{"message":"use {curly} and \"quotes\" }"}} trailing"#;
        let i = Intent::parse(text).unwrap();
        assert_eq!(i.args["message"], json!("use {curly} and \"quotes\" }"));
    }

    #[test]
    fn missing_or_null_args_become_empty() {
        assert!(Intent::parse(r#"{"action":"reboot"}"#).unwrap().args.is_empty());
        assert!(Intent::parse(r#"{"action":"reboot","args":null}"#).unwrap().args.is_empty());
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Intent::parse("I cannot do that"), Err(IntentParseError::NoJson));
        assert!(matches!(Intent::parse(r#"{"foo":1}"#), Err(IntentParseError::Malformed(_))));
        assert!(matches!(Intent::parse(r#"{"action":""}"#), Err(IntentParseError::Malformed(_))));
        assert!(matches!(Intent::parse(r#"{"action":"x","args":[1]}"#), Err(IntentParseError::Malformed(_))));
        assert_eq!(Intent::parse(r#"{"action":"x""#), Err(IntentParseError::NoJson));
    }
}
