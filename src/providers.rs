use crate::config::{ApiType, ProviderConfig};
use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// A single streamed chunk of output from a model, classified by phase.
#[derive(Debug, Clone)]
pub enum OutputChunk {
    /// Content produced during the model's reasoning/thinking phase.
    Thinking(String),
    /// Content produced in the final response phase.
    Content(String),
}

pub async fn call(
    provider: &ProviderConfig,
    model: &str,
    messages: Vec<Message>,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<Message> {
    match provider.api {
        ApiType::OpenaiCompletions => openai_completions(provider, model, messages, on_chunk).await,
    }
}

// ── OpenAI Chat Completions wire types (private) ─────────────────────────────

#[derive(Serialize, Deserialize)]
struct OaiMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct OaiRequest {
    model: String,
    messages: Vec<OaiMessage>,
    stream: bool,
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    /// Used by DeepSeek-R1 and some OpenRouter providers.
    reasoning_content: Option<String>,
    /// Used by Ollama for thinking-capable models (e.g. qwen3.5).
    reasoning: Option<String>,
}

// ── <think> tag parser ───────────────────────────────────────────────────────

/// Parses a stream of content strings, splitting off `<think>…</think>` blocks.
/// Handles tags that arrive split across multiple chunks.
#[derive(Default)]
struct ThinkParser {
    in_think: bool,
    partial_tag: String, // buffered characters when we're inside a `<…>` sequence
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
                // Inside a tag — accumulate until `>`.
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
                            // Not a special tag — treat as literal text.
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

    /// Call after the stream ends to flush any buffered partial tag.
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
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<Message> {
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let api_key = provider.api_key.as_deref().unwrap_or("");

    let oai_messages: Vec<OaiMessage> = messages
        .into_iter()
        .map(|m| OaiMessage {
            role: match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
            }
            .to_string(),
            content: m.content,
        })
        .collect();

    let mut stream = Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OaiRequest {
            model: model.to_string(),
            messages: oai_messages,
            stream: true,
        })
        .send()
        .await?
        .error_for_status()?
        .bytes_stream();

    let mut line_buf = String::new();
    let mut think_parser = ThinkParser::default();
    let mut full_content = String::new();

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

                // Providers that expose a dedicated reasoning field.
                // `reasoning_content`: DeepSeek-R1, some OpenRouter providers.
                // `reasoning`: Ollama (qwen3.5 and similar).
                let thinking_text = choice
                    .delta
                    .reasoning_content
                    .as_deref()
                    .or(choice.delta.reasoning.as_deref())
                    .unwrap_or("");
                if !thinking_text.is_empty() {
                    on_chunk(OutputChunk::Thinking(thinking_text.to_string()));
                }

                // Main content — may contain inline <think> tags.
                if let Some(content) = &choice.delta.content {
                    if !content.is_empty() {
                        for out in think_parser.feed(content) {
                            if let OutputChunk::Content(ref c) = out {
                                full_content.push_str(c);
                            }
                            on_chunk(out);
                        }
                    }
                }
            } else {
                line_buf.push(ch);
            }
        }
    }

    for out in think_parser.flush() {
        if let OutputChunk::Content(ref c) = out {
            full_content.push_str(c);
        }
        on_chunk(out);
    }

    Ok(Message {
        role: Role::Assistant,
        content: full_content,
    })
}
