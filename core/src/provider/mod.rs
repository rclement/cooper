pub mod anthropic_messages;
pub mod openai_completions;

use crate::types::{ApiType, Message, OutputChunk, ToolCall};
use anyhow::Result;
use serde_json::Value;

// ── ThinkParser ───────────────────────────────────────────────────────────────

/// Parses a stream of content strings, splitting off `<think>…</think>` blocks.
/// Handles tags that arrive split across multiple chunks.
#[derive(Default)]
pub struct ThinkParser {
    in_think: bool,
    partial_tag: String,
}

impl ThinkParser {
    pub fn feed(&mut self, text: &str) -> Vec<OutputChunk> {
        let mut chunks: Vec<OutputChunk> = Vec::new();
        let mut buf = String::new();

        let mut emit = |in_think: bool, s: String| {
            if s.is_empty() {
                return;
            }
            if in_think {
                chunks.push(OutputChunk::Thinking { text: s });
            } else {
                chunks.push(OutputChunk::Content { text: s });
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

    pub fn flush(&mut self) -> Vec<OutputChunk> {
        let tag = std::mem::take(&mut self.partial_tag);
        if tag.is_empty() {
            return vec![];
        }
        if self.in_think {
            vec![OutputChunk::Thinking { text: tag }]
        } else {
            vec![OutputChunk::Content { text: tag }]
        }
    }
}

// ── Inline tool call parser ───────────────────────────────────────────────────

/// Parses text-based tool call formats that some open-source models emit as
/// content instead of using the structured `tool_calls` API field.
///
/// Handles:
/// - `<function=NAME><parameter=K>V</parameter>…</function>` (Qwen/Hermes XML)
/// - `<tool_call>{"name":"…","arguments":{…}}</tool_call>` (Hermes JSON)
pub fn parse_inline_tool_calls(content: &str) -> Option<Vec<ToolCall>> {
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut rest = content;

    loop {
        if let Some(offset) = rest.find("<function=") {
            let tail = &rest[offset..];
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

// ── Dispatcher ────────────────────────────────────────────────────────────────

pub async fn call(
    api_type: &ApiType,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: Vec<Message>,
    tools: &[Value],
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<(Message, Option<crate::types::Usage>)> {
    match api_type {
        ApiType::OpenaiCompletions => {
            openai_completions::call(base_url, api_key, model, messages, tools, on_chunk).await
        }
        ApiType::AnthropicMessages => {
            anthropic_messages::call(base_url, api_key, model, messages, tools, on_chunk).await
        }
    }
}
