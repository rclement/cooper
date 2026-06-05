use std::collections::HashMap;
use std::vec;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::agent::{ChunkHandler, DeltaChunk, FinishReason, Message, ToolCall, Usage};
use crate::providers::Provider;
use crate::tools::{ToolParameterTypeSchema, ToolSchema};

/// === utility functions === ///

fn get_tool_param_type(param_type: &ToolParameterTypeSchema) -> &'static str {
    match param_type {
        ToolParameterTypeSchema::String => "string",
        ToolParameterTypeSchema::Number => "number",
        ToolParameterTypeSchema::Boolean => "boolean",
    }
}

/// === api payload schemas === ///

#[derive(Serialize)]
struct ApiStreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ApiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl From<&Message> for ApiMessage {
    fn from(m: &Message) -> Self {
        match m {
            Message::System(text) => ApiMessage {
                role: "system".to_string(),
                content: Some(text.clone()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
            },
            Message::User(text) => ApiMessage {
                role: "user".to_string(),
                content: Some(text.clone()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
            },
            Message::Assistant {
                text,
                reasoning,
                tool_calls,
            } => ApiMessage {
                role: "assistant".to_string(),
                content: text.clone(),
                reasoning: reasoning.clone(),
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        tool_calls
                            .iter()
                            .map(|tc| ApiToolCall {
                                id: tc.id.clone(),
                                function: ApiToolCallFunction {
                                    name: tc.name.clone(),
                                    arguments: serde_json::to_string(&tc.arguments)
                                        .unwrap_or_default(),
                                },
                                tool_call_type: "function".to_string(),
                            })
                            .collect(),
                    )
                },
                tool_call_id: None,
            },
            Message::Tool { call_id, result } => ApiMessage {
                role: "tool".to_string(),
                content: Some(result.clone().unwrap_or_else(|e| format!("Error: {e}"))),
                reasoning: None,
                tool_calls: None,
                tool_call_id: Some(call_id.clone()),
            },
        }
    }
}

#[derive(Serialize)]
struct ApiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ApiToolCall {
    id: String,
    function: ApiToolCallFunction,
    #[serde(rename = "type")]
    tool_call_type: String,
}

#[derive(Serialize)]
struct ApiToolParamProperty {
    #[serde(rename = "type")]
    param_type: String,
    description: String,
}

#[derive(Serialize)]
struct ApiToolParameters {
    #[serde(rename = "type")]
    object_type: String,
    properties: HashMap<String, ApiToolParamProperty>,
    required: Vec<String>,
}

#[derive(Serialize)]
struct ApiToolFunction {
    name: String,
    description: String,
    parameters: ApiToolParameters,
}

#[derive(Serialize)]
struct ApiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ApiToolFunction,
}

impl From<&ToolSchema> for ApiTool {
    fn from(t: &ToolSchema) -> Self {
        ApiTool {
            tool_type: "function".to_string(),
            function: ApiToolFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: ApiToolParameters {
                    object_type: "object".to_string(),
                    properties: t
                        .parameters
                        .iter()
                        .map(|p| {
                            (
                                p.0.clone(),
                                ApiToolParamProperty {
                                    param_type: get_tool_param_type(&p.1.param_type).to_string(),
                                    description: p.1.description.clone(),
                                },
                            )
                        })
                        .collect(),
                    required: t
                        .parameters
                        .iter()
                        .filter(|p| p.1.required)
                        .map(|p| p.0.clone())
                        .collect(),
                },
            },
        }
    }
}

#[derive(Serialize)]
struct ApiCompletionRequest {
    model: String,
    messages: Vec<ApiMessage>,
    tools: Vec<ApiTool>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<ApiStreamOptions>,
}

#[derive(Deserialize)]
struct ApiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Deserialize)]
struct ApiStreamToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ApiStreamToolCallDelta {
    index: usize,
    id: Option<String>,
    function: ApiStreamToolCallFunction,
}

#[derive(Deserialize)]
struct ApiStreamDelta {
    role: Option<String>,
    content: Option<String>,
    reasoning: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ApiStreamToolCallDelta>>,
}

#[derive(Deserialize)]
struct ApiStreamChoice {
    index: u64,
    delta: ApiStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ApiStreamChunk {
    id: String,
    choices: Vec<ApiStreamChoice>,
    usage: Option<ApiUsage>,
}

pub struct OpenAICompletionsAPI {
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAICompletionsAPI {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        OpenAICompletionsAPI {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

struct StreamResult {
    text_buf: String,
    reasoning_buf: String,
    usage_buf: Option<ApiUsage>,
    tool_call_buf: Vec<(usize, ToolCallAcc)>,
    finish_reason_buf: Option<String>,
}

async fn process_stream(
    response: reqwest::Response,
    handler: &dyn ChunkHandler,
) -> Result<StreamResult, Box<dyn std::error::Error>> {
    let mut stream = response.bytes_stream();
    let mut line_buf = String::new();
    let mut tool_call_buf: HashMap<usize, ToolCallAcc> = HashMap::new();
    let mut result = StreamResult {
        text_buf: String::new(),
        reasoning_buf: String::new(),
        usage_buf: None,
        tool_call_buf: vec![],
        finish_reason_buf: None,
    };

    while let Some(item) = stream.next().await {
        let chunk_bytes = item?;
        let chunk_str = std::str::from_utf8(&chunk_bytes)?;

        // delimit SSE events, possible across multiple chunks
        line_buf.push_str(chunk_str);
        let mut parts = line_buf.split("\n").peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                // last part, may be incomplete so keep it
                line_buf = part.to_string();
                break;
            }

            let line = part.trim();
            if line.is_empty() {
                continue;
            }

            if line == "data: [DONE]" {
                if let Some(u) = result.usage_buf.take() {
                    let usage = Usage {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        total_tokens: u.total_tokens,
                    };
                    handler.on_complete(&usage);
                }

                let mut sorted_tool_calls: Vec<(usize, ToolCallAcc)> =
                    tool_call_buf.into_iter().collect::<Vec<_>>();
                sorted_tool_calls.sort_by_key(|(index, _)| *index);
                result.tool_call_buf = sorted_tool_calls;

                return Ok(result);
            }

            if let Some(json) = line.strip_prefix("data: ") {
                let delta = serde_json::from_str::<ApiStreamChunk>(json)?;
                if let Some(choice) = delta.choices.first() {
                    if let Some(content) = &choice.delta.content {
                        if !content.is_empty() {
                            result.text_buf.push_str(content);
                            handler.on_chunk(&DeltaChunk {
                                text: Some(content.clone()),
                                reasoning: None,
                            });
                        }
                    }

                    let thinking = choice
                        .delta
                        .reasoning
                        .as_deref()
                        .or(choice.delta.reasoning_content.as_deref());
                    if let Some(reasoning) = thinking {
                        if !reasoning.is_empty() {
                            result.reasoning_buf.push_str(reasoning);
                            handler.on_chunk(&DeltaChunk {
                                text: None,
                                reasoning: Some(reasoning.to_string()),
                            });
                        }
                    }

                    if let Some(tool_calls) = &choice.delta.tool_calls {
                        for tool_call in tool_calls {
                            let entry =
                                tool_call_buf.entry(tool_call.index).or_insert(ToolCallAcc {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                });
                            if let Some(tool_call_id) = &tool_call.id {
                                entry.id = tool_call_id.clone();
                            }
                            if let Some(tool_call_name) = &tool_call.function.name {
                                entry.name = tool_call_name.clone();
                            }
                            if let Some(tool_call_arg) = &tool_call.function.arguments {
                                entry.arguments.push_str(tool_call_arg);
                            }
                        }
                    }

                    if let Some(finish_reason) = &choice.finish_reason {
                        result.finish_reason_buf = Some(finish_reason.clone());
                    }
                }

                if let Some(usage) = delta.usage {
                    result.usage_buf = Some(usage);
                }
            }
        }
    }

    Err("stream ended without [Done]".into())
}

#[async_trait]
impl Provider for OpenAICompletionsAPI {
    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        handler: &dyn ChunkHandler,
    ) -> Result<(Message, FinishReason), Box<dyn std::error::Error>> {
        let request = ApiCompletionRequest {
            model: self.model.clone(),
            messages: messages.iter().map(ApiMessage::from).collect(),
            tools: tools.iter().map(ApiTool::from).collect(),
            stream: true,
            stream_options: Some(ApiStreamOptions {
                include_usage: true,
            }),
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let stream_result = process_stream(response, handler).await?;

        let new_message = Message::Assistant {
            text: if stream_result.text_buf.is_empty() {
                None
            } else {
                Some(stream_result.text_buf)
            },
            reasoning: if stream_result.reasoning_buf.is_empty() {
                None
            } else {
                Some(stream_result.reasoning_buf)
            },
            tool_calls: stream_result
                .tool_call_buf
                .into_iter()
                .map(
                    |(_, tool_call_acc)| -> Result<ToolCall, serde_json::Error> {
                        Ok(ToolCall {
                            id: tool_call_acc.id,
                            name: tool_call_acc.name,
                            arguments: serde_json::from_str::<HashMap<String, String>>(
                                &tool_call_acc.arguments,
                            )?,
                        })
                    },
                )
                .collect::<Result<Vec<_>, _>>()?,
        };

        let finish_reason = match stream_result.finish_reason_buf.as_deref() {
            Some("stop") => FinishReason::Stop,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolCalls,
            Some("length") => FinishReason::Length,
            Some(other) => FinishReason::Unknown(other.to_string()),
            None => FinishReason::Unknown("none".to_string()),
        };

        Ok((new_message, finish_reason))
    }
}
