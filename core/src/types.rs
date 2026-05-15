use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ApiType {
    #[default]
    OpenaiCompletions,
    AnthropicMessages,
}

impl fmt::Display for ApiType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiType::OpenaiCompletions => write!(f, "openai-completions"),
            ApiType::AnthropicMessages => write!(f, "anthropic-messages"),
        }
    }
}

impl FromStr for ApiType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "openai-completions" => Ok(ApiType::OpenaiCompletions),
            "anthropic-messages" => Ok(ApiType::AnthropicMessages),
            _ => Err(anyhow!(
                "unknown API type '{}'; supported: openai-completions, anthropic-messages",
                s
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Message {
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(id.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Provider-agnostic tool schema. Each provider translates this into its own
/// wire format internally — no OAI-specific JSON leaks into core.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: serde_json::Value,
}

/// Struct variants produce `{"type":"content","text":"..."}` JSON — suitable for both
/// the WASM JS callback and CLI pattern matching.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputChunk {
    /// Emitted once at the start of a session with the resolved configuration.
    SessionStart {
        provider: String,
        model: String,
        /// `None` = disabled; `Some(path)` = file that will be loaded
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_instructions: Option<String>,
        context_files: Vec<String>,
        /// `None` = all tools; `Some(list)` = restricted set
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
        /// `None` = skill system inactive; `Some(list)` = active (may be empty if all filtered out)
        #[serde(skip_serializing_if = "Option::is_none")]
        skills: Option<Vec<String>>,
        /// Name of the skill pre-loaded into the system prompt (`--skill` flag)
        #[serde(skip_serializing_if = "Option::is_none")]
        active_skill: Option<String>,
    },
    Content {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        output: String,
    },
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    },
}

/// Canonical session lifecycle events. Storage backends receive these via `SessionLogger::on_event`
/// and may enrich them with backend-specific metadata (e.g. `session_id`, `timestamp`) when persisting.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    SessionStart {
        id: String,
        provider: String,
        model: String,
        project: String,
        started_at: String,
    },
    Request {
        messages: Vec<Message>,
    },
    Response {
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        message: Message,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_type_display() {
        assert_eq!(ApiType::OpenaiCompletions.to_string(), "openai-completions");
        assert_eq!(ApiType::AnthropicMessages.to_string(), "anthropic-messages");
    }

    #[test]
    fn api_type_from_str_valid() {
        assert_eq!(
            "openai-completions".parse::<ApiType>().unwrap(),
            ApiType::OpenaiCompletions
        );
        assert_eq!(
            "anthropic-messages".parse::<ApiType>().unwrap(),
            ApiType::AnthropicMessages
        );
    }

    #[test]
    fn api_type_from_str_invalid() {
        assert!("unknown".parse::<ApiType>().is_err());
        assert!("".parse::<ApiType>().is_err());
    }

    #[test]
    fn api_type_default() {
        assert_eq!(ApiType::default(), ApiType::OpenaiCompletions);
    }

    #[test]
    fn message_new() {
        let m = Message::new(Role::User, "hello");
        assert!(matches!(m.role, Role::User));
        assert_eq!(m.content, "hello");
        assert!(m.tool_calls.is_none());
        assert!(m.tool_call_id.is_none());
    }

    #[test]
    fn message_new_accepts_owned_string() {
        let s = "owned".to_string();
        let m = Message::new(Role::System, s);
        assert_eq!(m.content, "owned");
    }

    #[test]
    fn message_tool_result() {
        let m = Message::tool_result("id-1", "result");
        assert!(matches!(m.role, Role::Tool));
        assert_eq!(m.content, "result");
        assert_eq!(m.tool_call_id, Some("id-1".to_string()));
        assert!(m.tool_calls.is_none());
    }

    #[test]
    fn output_chunk_serde_roundtrip() {
        let cases: Vec<OutputChunk> = vec![
            OutputChunk::Content { text: "hi".into() },
            OutputChunk::Thinking { text: "think".into() },
            OutputChunk::ToolCall { name: "f".into(), args: "{}".into() },
            OutputChunk::ToolResult { name: "f".into(), output: "ok".into() },
            OutputChunk::Usage { prompt_tokens: 1, completion_tokens: 2, total_tokens: 3 },
        ];
        for chunk in cases {
            let json = serde_json::to_string(&chunk).unwrap();
            let back: OutputChunk = serde_json::from_str(&json).unwrap();
            assert_eq!(chunk, back);
        }
    }

    #[test]
    fn output_chunk_type_tags() {
        let json = serde_json::to_string(&OutputChunk::Content { text: "x".into() }).unwrap();
        assert!(json.contains("\"type\":\"content\""));
        let json = serde_json::to_string(&OutputChunk::Thinking { text: "x".into() }).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        let json = serde_json::to_string(&OutputChunk::ToolCall { name: "f".into(), args: "{}".into() }).unwrap();
        assert!(json.contains("\"type\":\"tool_call\""));
        let json = serde_json::to_string(&OutputChunk::ToolResult { name: "f".into(), output: "o".into() }).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        let json = serde_json::to_string(&OutputChunk::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }).unwrap();
        assert!(json.contains("\"type\":\"usage\""));
    }

    #[test]
    fn message_serialization_skips_none_fields() {
        let m = Message::new(Role::User, "hi");
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("tool_call_id"));
    }

    #[test]
    fn usage_struct() {
        let u = Usage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 };
        assert_eq!(u.total_tokens, u.prompt_tokens + u.completion_tokens);
    }
}
