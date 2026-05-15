use super::{ThinkParser, parse_inline_tool_calls};
use crate::types::{Message, OutputChunk, Role, ToolCall, ToolSchema, Usage};
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

fn tool_schema_to_oai(schema: &ToolSchema) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": schema.name,
            "description": schema.description,
            "parameters": schema.parameters,
        }
    })
}

// ── Streaming call ────────────────────────────────────────────────────────────

pub async fn call(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: Vec<Message>,
    tools: &[ToolSchema],
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<(Message, Option<Usage>)> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let wire_messages: Vec<Value> = messages.iter().map(message_to_wire).collect();
    let tools_wire: Vec<Value> = tools.iter().map(tool_schema_to_oai).collect();

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
            tools: tools_wire,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, OutputChunk, Role};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── message_to_wire ───────────────────────────────────────────────────────

    #[test]
    fn wire_tool_role() {
        let m = Message::tool_result("id1", "result");
        let w = message_to_wire(&m);
        assert_eq!(w["role"], "tool");
        assert_eq!(w["tool_call_id"], "id1");
        assert_eq!(w["content"], "result");
    }

    #[test]
    fn wire_assistant_with_tool_calls() {
        let mut m = Message::new(Role::Assistant, "");
        m.tool_calls = Some(vec![crate::types::ToolCall {
            id: "tc0".into(),
            name: "search".into(),
            arguments: r#"{"q":"x"}"#.into(),
        }]);
        let w = message_to_wire(&m);
        assert_eq!(w["role"], "assistant");
        assert!(w["content"].is_null());
        let tcs = w["tool_calls"].as_array().unwrap();
        assert_eq!(tcs[0]["id"], "tc0");
        assert_eq!(tcs[0]["function"]["name"], "search");
    }

    #[test]
    fn wire_assistant_plain() {
        let m = Message::new(Role::Assistant, "hello");
        let w = message_to_wire(&m);
        assert_eq!(w["role"], "assistant");
        assert_eq!(w["content"], "hello");
    }

    #[test]
    fn wire_user() {
        let m = Message::new(Role::User, "hi");
        let w = message_to_wire(&m);
        assert_eq!(w["role"], "user");
        assert_eq!(w["content"], "hi");
    }

    #[test]
    fn wire_system() {
        let m = Message::new(Role::System, "be helpful");
        let w = message_to_wire(&m);
        assert_eq!(w["role"], "system");
        assert_eq!(w["content"], "be helpful");
    }

    // ── call (streaming) ──────────────────────────────────────────────────────

    fn sse(lines: &[&str]) -> String {
        let mut s: String = lines.iter().map(|l| format!("data: {}\n", l)).collect();
        s.push_str("data: [DONE]\n");
        s
    }

    #[tokio::test]
    async fn call_simple_text() {
        let server = MockServer::start().await;
        let body = sse(&[
            r#"{"choices":[{"delta":{"content":"hello"}}],"usage":null}"#,
            r#"{"choices":[{"delta":{}}],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mut chunks = vec![];
        let (msg, usage) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |c| chunks.push(c),
        ).await.unwrap();

        assert_eq!(msg.content, "hello");
        assert!(msg.tool_calls.is_none());
        let u = usage.unwrap();
        assert_eq!(u.prompt_tokens, 10);
        assert_eq!(u.completion_tokens, 3);
        assert_eq!(u.total_tokens, 13);
        assert!(chunks.iter().any(|c| matches!(c, OutputChunk::Content { text } if text == "hello")));
    }

    #[tokio::test]
    async fn call_tool_call_structured() {
        let server = MockServer::start().await;
        let body = sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc0","function":{"name":"search","arguments":""}}]}}],"usage":null}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":\"x\"}"}}]}}],"usage":null}"#,
            r#"{"choices":[{"delta":{}}],"usage":{"prompt_tokens":5,"completion_tokens":10,"total_tokens":15}}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (msg, _) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "search")],
            &[],
            &mut |_| {},
        ).await.unwrap();

        let tcs = msg.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "tc0");
        assert_eq!(tcs[0].name, "search");
        assert!(tcs[0].arguments.contains("q"));
    }

    #[tokio::test]
    async fn call_reasoning_content_emitted_as_thinking() {
        let server = MockServer::start().await;
        let body = sse(&[
            r#"{"choices":[{"delta":{"reasoning_content":"let me think"}}],"usage":null}"#,
            r#"{"choices":[{"delta":{"content":"answer"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mut chunks = vec![];
        let (msg, _) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "think")],
            &[],
            &mut |c| chunks.push(c),
        ).await.unwrap();

        assert_eq!(msg.content, "answer");
        assert!(chunks.iter().any(|c| matches!(c, OutputChunk::Thinking { text } if text == "let me think")));
    }

    #[tokio::test]
    async fn call_reasoning_field_fallback() {
        let server = MockServer::start().await;
        // Some providers use "reasoning" instead of "reasoning_content"
        let body = sse(&[
            r#"{"choices":[{"delta":{"reasoning":"alternative think"}}],"usage":null}"#,
            r#"{"choices":[{"delta":{"content":"out"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mut chunks = vec![];
        let _ = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |c| chunks.push(c),
        ).await.unwrap();

        assert!(chunks.iter().any(|c| matches!(c, OutputChunk::Thinking { .. })));
    }

    #[tokio::test]
    async fn call_inline_tool_call_xml() {
        let server = MockServer::start().await;
        let body = sse(&[
            r#"{"choices":[{"delta":{"content":"<function=ping></function>"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (msg, _) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |_| {},
        ).await.unwrap();

        assert!(msg.tool_calls.is_some());
        assert_eq!(msg.tool_calls.unwrap()[0].name, "ping");
    }

    #[tokio::test]
    async fn call_inline_tool_call_json() {
        let server = MockServer::start().await;
        let body = sse(&[
            r#"{"choices":[{"delta":{"content":"<tool_call>{\"name\":\"run\",\"arguments\":{\"cmd\":\"ls\"}}</tool_call>"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (msg, _) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |_| {},
        ).await.unwrap();

        let tcs = msg.tool_calls.unwrap();
        assert_eq!(tcs[0].name, "run");
    }

    #[tokio::test]
    async fn call_think_tag_in_content_parsed() {
        let server = MockServer::start().await;
        // Content contains <think> tags — ThinkParser should handle it
        let body = sse(&[
            r#"{"choices":[{"delta":{"content":"<think>reasoning</think>response"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mut chunks = vec![];
        let (msg, _) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |c| chunks.push(c),
        ).await.unwrap();

        assert_eq!(msg.content, "response");
        assert!(chunks.iter().any(|c| matches!(c, OutputChunk::Thinking { text } if text == "reasoning")));
    }

    #[tokio::test]
    async fn call_empty_choices_skipped() {
        let server = MockServer::start().await;
        let body = sse(&[
            r#"{"choices":[],"usage":null}"#,
            r#"{"choices":[{"delta":{"content":"ok"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (msg, _) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |_| {},
        ).await.unwrap();

        assert_eq!(msg.content, "ok");
    }

    #[tokio::test]
    async fn call_http_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Error"))
            .mount(&server)
            .await;

        let result = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |_| {},
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("HTTP error"));
    }

    #[tokio::test]
    async fn call_no_usage_when_absent() {
        let server = MockServer::start().await;
        let body = sse(&[
            r#"{"choices":[{"delta":{"content":"hi"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (_, usage) = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |_| {},
        ).await.unwrap();

        assert!(usage.is_none());
    }

    #[tokio::test]
    async fn call_content_before_inline_tool_emitted_cleanly() {
        let server = MockServer::start().await;
        // Preamble before the <function= marker should be emitted as content
        let body = sse(&[
            r#"{"choices":[{"delta":{"content":"preamble <function=ping></function>"}}],"usage":null}"#,
        ]);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mut chunks = vec![];
        let _ = call(
            &server.uri(), "key", "model",
            vec![Message::new(Role::User, "hi")],
            &[],
            &mut |c| chunks.push(c),
        ).await.unwrap();

        assert!(chunks.iter().any(|c| matches!(c, OutputChunk::Content { text } if text == "preamble ")));
    }
}
