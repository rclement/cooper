use crate::types::{Message, OutputChunk, Role, ToolCall, Usage};
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8096;

// ── Wire serialisation ────────────────────────────────────────────────────────

/// Converts an OpenAI-format tool schema to Anthropic format.
/// OAI:       {"type":"function","function":{"name":"...","description":"...","parameters":{...}}}
/// Anthropic: {"name":"...","description":"...","input_schema":{...}}
fn tool_schema_to_anthropic(oai_schema: &Value) -> Value {
    let func = &oai_schema["function"];
    let mut result = serde_json::json!({
        "name": func["name"],
        "input_schema": func["parameters"],
    });
    if let Some(desc) = func.get("description") {
        result["description"] = desc.clone();
    }
    result
}

/// Converts the internal message list to Anthropic wire format.
/// Returns (system_prompt, messages_array).
///
/// Key differences from OpenAI:
/// - System message becomes a top-level field, not a messages entry.
/// - Assistant tool calls become content blocks of type `tool_use`.
/// - Tool results (Role::Tool) are grouped into a single user message as
///   `tool_result` content blocks — Anthropic does not allow a bare "tool" role.
fn messages_to_wire(messages: &[Message]) -> (Option<String>, Vec<Value>) {
    let mut system: Option<String> = None;
    let mut wire: Vec<Value> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let m = &messages[i];
        match m.role {
            Role::System => {
                system = Some(m.content.clone());
                i += 1;
            }
            Role::User => {
                wire.push(serde_json::json!({"role": "user", "content": m.content}));
                i += 1;
            }
            Role::Assistant => {
                if let Some(tcs) = &m.tool_calls {
                    let content: Vec<Value> = tcs
                        .iter()
                        .map(|tc| {
                            let input: Value = serde_json::from_str(&tc.arguments)
                                .unwrap_or(serde_json::json!({}));
                            serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": input,
                            })
                        })
                        .collect();
                    wire.push(serde_json::json!({"role": "assistant", "content": content}));
                } else {
                    wire.push(serde_json::json!({"role": "assistant", "content": m.content}));
                }
                i += 1;
            }
            Role::Tool => {
                // Consecutive tool results collapse into one user message.
                let mut tool_results: Vec<Value> = Vec::new();
                while i < messages.len() {
                    if let Role::Tool = messages[i].role {
                        tool_results.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": messages[i].tool_call_id.as_deref().unwrap_or(""),
                            "content": messages[i].content,
                        }));
                        i += 1;
                    } else {
                        break;
                    }
                }
                wire.push(serde_json::json!({"role": "user", "content": tool_results}));
            }
        }
    }

    (system, wire)
}

// ── Request type ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    stream: bool,
}

// ── SSE event types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart {
        message: MessageStartData,
    },
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: ContentDelta,
    },
    ContentBlockStop {
        #[allow(dead_code)]
        index: usize,
    },
    MessageDelta {
        usage: Option<MessageDeltaUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: AnthropicError,
    },
}

#[derive(Deserialize)]
struct MessageStartData {
    usage: Option<MessageStartUsage>,
}

#[derive(Deserialize)]
struct MessageStartUsage {
    input_tokens: u32,
}

#[derive(Deserialize)]
struct MessageDeltaUsage {
    output_tokens: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentDelta {
    TextDelta { text: String },
    ThinkingDelta { thinking: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Deserialize)]
struct AnthropicError {
    message: String,
}

// ── Streaming call ────────────────────────────────────────────────────────────

pub async fn call(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: Vec<Message>,
    tools: &[Value],
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<(Message, Option<Usage>)> {
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let anthropic_tools: Vec<Value> = tools.iter().map(tool_schema_to_anthropic).collect();
    let (system, wire_messages) = messages_to_wire(&messages);

    let mut stream = Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&AnthropicRequest {
            model: model.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system,
            messages: wire_messages,
            tools: anthropic_tools,
            stream: true,
        })
        .send()
        .await
        .map_err(|e| anyhow!("request failed: {}", e))?
        .error_for_status()
        .map_err(|e| anyhow!("HTTP error: {}", e))?
        .bytes_stream();

    // Per-block state: tool_use blocks accumulate args; text/thinking are emitted inline.
    let mut pending_tools: std::collections::BTreeMap<usize, (String, String, String)> =
        Default::default();
    let mut thinking_indices: std::collections::BTreeSet<usize> = Default::default();
    let mut full_content = String::new();
    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut line_buf = String::new();

    'stream: while let Some(bytes) = stream.next().await {
        let raw = bytes.map_err(|e| anyhow!("stream read error: {}", e))?;
        for ch in String::from_utf8_lossy(&raw).chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut line_buf);
                let line = line.trim();

                // Anthropic SSE uses both `event:` and `data:` lines; we only need data.
                let data = match line.strip_prefix("data: ") {
                    Some(d) => d,
                    None => continue,
                };

                let event: AnthropicEvent = match serde_json::from_str(data) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                match event {
                    AnthropicEvent::MessageStart { message } => {
                        if let Some(u) = message.usage {
                            input_tokens = u.input_tokens;
                        }
                    }
                    AnthropicEvent::ContentBlockStart {
                        index,
                        content_block,
                    } => match content_block {
                        ContentBlock::Text => {}
                        ContentBlock::Thinking => {
                            thinking_indices.insert(index);
                        }
                        ContentBlock::ToolUse { id, name } => {
                            pending_tools.insert(index, (id, name, String::new()));
                        }
                    },
                    AnthropicEvent::ContentBlockDelta { index, delta } => match delta {
                        ContentDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                full_content.push_str(&text);
                                on_chunk(OutputChunk::Content { text });
                            }
                        }
                        ContentDelta::ThinkingDelta { thinking } => {
                            if !thinking.is_empty() {
                                on_chunk(OutputChunk::Thinking { text: thinking });
                            }
                        }
                        ContentDelta::InputJsonDelta { partial_json } => {
                            if let Some(entry) = pending_tools.get_mut(&index) {
                                entry.2.push_str(&partial_json);
                            }
                        }
                    },
                    AnthropicEvent::ContentBlockStop { .. } => {}
                    AnthropicEvent::MessageDelta { usage } => {
                        if let Some(u) = usage {
                            output_tokens = u.output_tokens;
                        }
                    }
                    AnthropicEvent::MessageStop => {
                        break 'stream;
                    }
                    AnthropicEvent::Error { error } => {
                        return Err(anyhow!("Anthropic API error: {}", error.message));
                    }
                    AnthropicEvent::Ping => {}
                }
            } else {
                line_buf.push(ch);
            }
        }
    }

    let _ = thinking_indices; // tracked for potential future use

    let usage = if input_tokens > 0 || output_tokens > 0 {
        Some(Usage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            total_tokens: input_tokens + output_tokens,
        })
    } else {
        None
    };

    if !pending_tools.is_empty() {
        let tool_calls: Vec<ToolCall> = pending_tools
            .into_values()
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments,
            })
            .collect();
        return Ok((
            Message {
                role: Role::Assistant,
                content: full_content,
                tool_calls: Some(tool_calls),
                tool_call_id: None,
            },
            usage,
        ));
    }

    Ok((Message::new(Role::Assistant, full_content), usage))
}
