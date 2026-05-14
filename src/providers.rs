use crate::config::{ApiType, ProviderConfig};
use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone)]
pub enum OutputChunk {
    SessionStart { provider: String, model: String },
    Thinking(String),
    Content(String),
    ToolCall { name: String, args: String },
    ToolResult { name: String, output: String },
}

pub async fn call(
    provider: &ProviderConfig,
    model: &str,
    messages: Vec<Message>,
    tools: &[Value],
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<Message> {
    match provider.api {
        ApiType::OpenaiCompletions => {
            openai_completions(provider, model, messages, tools, on_chunk).await
        }
    }
}

// ── OpenAI Chat Completions wire types ───────────────────────────────────────

fn message_to_value(m: Message) -> Value {
    match m.role {
        Role::Tool => serde_json::json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id.unwrap_or_default(),
            "content": m.content,
        }),
        Role::Assistant if m.tool_calls.is_some() => {
            let tcs: Vec<Value> = m
                .tool_calls
                .unwrap()
                .into_iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments,
                        }
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

#[derive(Serialize)]
struct OaiRequest {
    model: String,
    messages: Vec<Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    /// DeepSeek-R1 and some OpenRouter providers.
    reasoning_content: Option<String>,
    /// Ollama (qwen3.5 and similar).
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

// ── <think> tag parser ───────────────────────────────────────────────────────

/// Parses a stream of content strings, splitting off `<think>…</think>` blocks.
/// Handles tags that arrive split across multiple chunks.
#[derive(Default)]
struct ThinkParser {
    in_think: bool,
    partial_tag: String,
}

impl ThinkParser {
    fn feed(&mut self, text: &str) -> Vec<OutputChunk> {
        let mut chunks: Vec<OutputChunk> = Vec::new();
        let mut buf = String::new();

        let mut emit = |in_think: bool, s: String| {
            if s.is_empty() {
                return;
            }
            if in_think {
                chunks.push(OutputChunk::Thinking(s));
            } else {
                chunks.push(OutputChunk::Content(s));
            }
        };

        for ch in text.chars() {
            if !self.partial_tag.is_empty() {
                self.partial_tag.push(ch);
                if ch == '>' {
                    let tag = std::mem::take(&mut self.partial_tag);
                    match tag.as_str() {
                        "<think>" => {
                            emit(self.in_think, std::mem::take(&mut buf));
                            self.in_think = true;
                        }
                        "</think>" => {
                            emit(self.in_think, std::mem::take(&mut buf));
                            self.in_think = false;
                        }
                        other => {
                            buf.push_str(other);
                        }
                    }
                }
            } else if ch == '<' {
                self.partial_tag.push('<');
            } else {
                buf.push(ch);
            }
        }

        emit(self.in_think, buf);
        chunks
    }

    fn flush(&mut self) -> Vec<OutputChunk> {
        let tag = std::mem::take(&mut self.partial_tag);
        if tag.is_empty() {
            return vec![];
        }
        if self.in_think {
            vec![OutputChunk::Thinking(tag)]
        } else {
            vec![OutputChunk::Content(tag)]
        }
    }
}

// ── Streaming request ────────────────────────────────────────────────────────

async fn openai_completions(
    provider: &ProviderConfig,
    model: &str,
    messages: Vec<Message>,
    tools: &[Value],
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<Message> {
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let api_key = provider.api_key.as_deref().unwrap_or("");

    let oai_messages: Vec<Value> = messages.into_iter().map(message_to_value).collect();

    let mut stream = Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OaiRequest {
            model: model.to_string(),
            messages: oai_messages,
            stream: true,
            tools: tools.to_vec(),
        })
        .send()
        .await?
        .error_for_status()?
        .bytes_stream();

    let mut line_buf = String::new();
    let mut think_parser = ThinkParser::default();
    // full_content accumulates everything including raw tool call markup.
    // clean_content stops at the first inline tool call marker so we can use
    // it as the assistant message's text when returning tool calls.
    let mut full_content = String::new();
    let mut clean_content = String::new();
    let mut inline_tool_started = false;
    // index → (id, name, accumulated_arguments) for structured tool_calls.
    let mut pending: std::collections::BTreeMap<usize, (String, String, String)> =
        Default::default();

    'stream: while let Some(bytes) = stream.next().await {
        let raw = bytes?;
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

                let Some(choice) = chunk.choices.first() else {
                    continue;
                };

                // Accumulate structured tool call argument fragments.
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

                // Dedicated thinking fields.
                let thinking_text = choice
                    .delta
                    .reasoning_content
                    .as_deref()
                    .or(choice.delta.reasoning.as_deref())
                    .unwrap_or("");
                if !thinking_text.is_empty() {
                    on_chunk(OutputChunk::Thinking(thinking_text.to_string()));
                }

                // Main content — may contain inline <think> or tool call tags.
                if let Some(content) = &choice.delta.content {
                    if !content.is_empty() {
                        for out in think_parser.feed(content) {
                            match &out {
                                OutputChunk::Content(c) => {
                                    full_content.push_str(c);
                                    if !inline_tool_started {
                                        // Detect start of text-based tool call.
                                        let marker =
                                            c.find("<function=").or_else(|| c.find("<tool_call>"));
                                        if let Some(idx) = marker {
                                            let pre = c[..idx].to_string();
                                            clean_content.push_str(&pre);
                                            if !pre.is_empty() {
                                                on_chunk(OutputChunk::Content(pre));
                                            }
                                            inline_tool_started = true;
                                        } else {
                                            clean_content.push_str(c);
                                            on_chunk(out);
                                        }
                                    }
                                    // When inline_tool_started, accumulate in
                                    // full_content (already done) but don't emit.
                                }
                                OutputChunk::Thinking(_) => on_chunk(out),
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
        match &out {
            OutputChunk::Content(c) => {
                full_content.push_str(c);
                if !inline_tool_started {
                    clean_content.push_str(c);
                    on_chunk(out);
                }
            }
            OutputChunk::Thinking(_) => on_chunk(out),
            _ => {}
        }
    }

    if !pending.is_empty() {
        // Structured (OpenAI-format) tool calls.
        let tool_calls: Vec<ToolCall> = pending
            .into_values()
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments,
            })
            .collect();
        Ok(Message {
            role: Role::Assistant,
            content: clean_content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        })
    } else if let Some(inline_calls) = parse_inline_tool_calls(&full_content) {
        // Text-based tool calls embedded in the content.
        Ok(Message {
            role: Role::Assistant,
            content: clean_content,
            tool_calls: Some(inline_calls),
            tool_call_id: None,
        })
    } else {
        Ok(Message::new(Role::Assistant, full_content))
    }
}

// ── Inline tool call parser ───────────────────────────────────────────────────

/// Parses text-based tool call formats that some open-source models emit as
/// content instead of using the structured `tool_calls` API field.
///
/// Handles:
/// - `<function=NAME><parameter=K>V</parameter>…</function>` (Qwen/Hermes XML)
/// - `<tool_call>{"name":"…","arguments":{…}}</tool_call>` (Hermes JSON)
fn parse_inline_tool_calls(content: &str) -> Option<Vec<ToolCall>> {
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut rest = content;

    loop {
        if let Some(offset) = rest.find("<function=") {
            let tail = &rest[offset..]; // starts at "<function="
            let name_close = tail.find('>')?;
            let func_name = tail["<function=".len()..name_close].trim().to_string();

            let body_start = name_close + 1;
            let func_end = tail[body_start..].find("</function>")?;
            let func_body = &tail[body_start..body_start + func_end];

            let mut args = serde_json::Map::new();
            let mut pb = func_body;
            while let Some(pidx) = pb.find("<parameter=") {
                let pt = &pb[pidx..];
                let pclose = pt.find('>')?;
                let pname = pt["<parameter=".len()..pclose].trim().to_string();
                let val_start = pclose + 1;
                let val_end = pt[val_start..].find("</parameter>")?;
                let pval = pt[val_start..val_start + val_end].trim().to_string();
                args.insert(pname, serde_json::Value::String(pval));
                pb = &pt[val_start + val_end + "</parameter>".len()..];
            }

            tool_calls.push(ToolCall {
                id: format!("tc_{}", tool_calls.len()),
                name: func_name,
                arguments: serde_json::to_string(&serde_json::Value::Object(args))
                    .unwrap_or_else(|_| "{}".to_string()),
            });

            rest = &tail[body_start + func_end + "</function>".len()..];
            let rt = rest.trim_start();
            if rt.starts_with("</tool_call>") {
                let skip = rest.len() - rt.len() + "</tool_call>".len();
                rest = &rest[skip..];
            }
        } else if let Some(offset) = rest.find("<tool_call>") {
            let tail = &rest[offset + "<tool_call>".len()..];
            let end = tail.find("</tool_call>")?;
            let json_str = tail[..end].trim();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(name) = val["name"].as_str() {
                    let arguments = val
                        .get("arguments")
                        .map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string()))
                        .unwrap_or_else(|| "{}".to_string());
                    tool_calls.push(ToolCall {
                        id: format!("tc_{}", tool_calls.len()),
                        name: name.to_string(),
                        arguments,
                    });
                }
            }
            rest = &tail[end + "</tool_call>".len()..];
        } else {
            break;
        }
    }

    if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    }
}
