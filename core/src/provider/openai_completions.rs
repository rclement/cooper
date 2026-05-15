use super::{ThinkParser, parse_inline_tool_calls};
use crate::types::{Message, OutputChunk, Role, ToolCall, Usage};
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Wire serialisation ────────────────────────────────────────────────────────

fn message_to_wire(m: &Message) -> Value {
    match m.role {
        Role::Tool => serde_json::json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id.as_deref().unwrap_or(""),
            "content": m.content,
        }),
        Role::Assistant if m.tool_calls.is_some() => {
            let tcs: Vec<Value> = m
                .tool_calls
                .as_ref()
                .unwrap()
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments }
                    })
                })
                .collect();
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": tcs,
            })
        }
        _ => {
            let role_str = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            serde_json::json!({ "role": role_str, "content": m.content })
        }
    }
}

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct OaiRequest {
    model: String,
    messages: Vec<Value>,
    stream: bool,
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
}

#[derive(Deserialize)]
struct StreamUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    usage: Option<StreamUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    /// DeepSeek-R1 and some OpenRouter providers.
    reasoning_content: Option<String>,
    /// Ollama (qwen3 and similar).
    reasoning: Option<String>,
    tool_calls: Option<Vec<PartialToolCallDelta>>,
}

#[derive(Deserialize)]
struct PartialToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<PartialFunctionDelta>,
}

#[derive(Deserialize, Default)]
struct PartialFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
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
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let wire_messages: Vec<Value> = messages.iter().map(message_to_wire).collect();

    let mut stream = Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OaiRequest {
            model: model.to_string(),
            messages: wire_messages,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            tools: tools.to_vec(),
        })
        .send()
        .await
        .map_err(|e| anyhow!("request failed: {}", e))?
        .error_for_status()
        .map_err(|e| anyhow!("HTTP error: {}", e))?
        .bytes_stream();

    let mut line_buf = String::new();
    let mut think_parser = ThinkParser::default();
    let mut full_content = String::new();
    let mut clean_content = String::new();
    let mut inline_tool_started = false;
    let mut pending: std::collections::BTreeMap<usize, (String, String, String)> =
        Default::default();
    let mut last_usage: Option<StreamUsage> = None;

    'stream: while let Some(bytes) = stream.next().await {
        let raw = bytes.map_err(|e| anyhow!("stream read error: {}", e))?;
        for ch in String::from_utf8_lossy(&raw).chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut line_buf);
                let line = line.trim();

                let data = match line.strip_prefix("data: ") {
                    Some(d) => d,
                    None => continue,
                };

                if data == "[DONE]" {
                    break 'stream;
                }

                let chunk: StreamChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                if let Some(u) = chunk.usage {
                    last_usage = Some(u);
                }

                let Some(choice) = chunk.choices.first() else {
                    continue;
                };

                if let Some(tcs) = &choice.delta.tool_calls {
                    for tc in tcs {
                        let entry = pending.entry(tc.index).or_default();
                        if let Some(id) = &tc.id {
                            entry.0 = id.clone();
                        }
                        if let Some(f) = &tc.function {
                            if let Some(name) = &f.name {
                                entry.1.push_str(name);
                            }
                            if let Some(args) = &f.arguments {
                                entry.2.push_str(args);
                            }
                        }
                    }
                }

                let thinking_text = choice
                    .delta
                    .reasoning_content
                    .as_deref()
                    .or(choice.delta.reasoning.as_deref())
                    .unwrap_or("");
                if !thinking_text.is_empty() {
                    on_chunk(OutputChunk::Thinking {
                        text: thinking_text.to_string(),
                    });
                }

                if let Some(content) = &choice.delta.content {
                    if !content.is_empty() {
                        for out in think_parser.feed(content) {
                            match out {
                                OutputChunk::Content { text: ref c } => {
                                    full_content.push_str(c);
                                    if !inline_tool_started {
                                        let marker =
                                            c.find("<function=").or_else(|| c.find("<tool_call>"));
                                        if let Some(idx) = marker {
                                            let pre = c[..idx].to_string();
                                            clean_content.push_str(&pre);
                                            if !pre.is_empty() {
                                                on_chunk(OutputChunk::Content { text: pre });
                                            }
                                            inline_tool_started = true;
                                        } else {
                                            clean_content.push_str(c);
                                            on_chunk(out);
                                        }
                                    }
                                }
                                OutputChunk::Thinking { .. } => on_chunk(out),
                                _ => {}
                            }
                        }
                    }
                }
            } else {
                line_buf.push(ch);
            }
        }
    }

    for out in think_parser.flush() {
        match out {
            OutputChunk::Content { text: ref c } => {
                full_content.push_str(c);
                if !inline_tool_started {
                    clean_content.push_str(c);
                    on_chunk(out);
                }
            }
            OutputChunk::Thinking { .. } => on_chunk(out),
            _ => {}
        }
    }

    let usage = last_usage.map(|u| Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });

    if !pending.is_empty() {
        let tool_calls: Vec<ToolCall> = pending
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
                content: clean_content,
                tool_calls: Some(tool_calls),
                tool_call_id: None,
            },
            usage,
        ));
    }

    if let Some(inline_calls) = parse_inline_tool_calls(&full_content) {
        return Ok((
            Message {
                role: Role::Assistant,
                content: clean_content,
                tool_calls: Some(inline_calls),
                tool_call_id: None,
            },
            usage,
        ));
    }

    Ok((Message::new(Role::Assistant, full_content), usage))
}
