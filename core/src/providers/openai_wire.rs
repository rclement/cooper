//! OpenAI chat.completions wire types and stream accumulation, shared by
//! every transport that speaks this shape: the HTTP/SSE provider in
//! `openai_completions`, and the browser-side wllama bridge (which produces
//! the same chunk objects in-process, no HTTP involved).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentEventsHandler, AgentMessageChunk, FinishReason, Message, ToolCall, Usage};
use crate::tools::{ToolParameterTypeSchema, ToolSchema};

fn get_tool_param_type(param_type: &ToolParameterTypeSchema) -> &'static str {
    match param_type {
        ToolParameterTypeSchema::String => "string",
        ToolParameterTypeSchema::Number => "number",
        ToolParameterTypeSchema::Boolean => "boolean",
    }
}

#[derive(Serialize)]
pub struct ApiStreamOptions {
    pub include_usage: bool,
}

#[derive(Serialize)]
pub struct ApiMessage {
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
pub struct ApiTool {
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
pub struct ApiCompletionRequest {
    pub model: String,
    pub messages: Vec<ApiMessage>,
    pub tools: Vec<ApiTool>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<ApiStreamOptions>,
}

#[derive(Deserialize, Clone)]
pub struct ApiUsage {
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
    #[serde(default)]
    id: Option<String>,
    /// Optional because some producers (e.g. wllama) send the first delta of
    /// a tool call with only `id`/`type` set.
    #[serde(default)]
    function: Option<ApiStreamToolCallFunction>,
}

/// Unknown wire fields (`role`, `id`, choice `index`, ...) are ignored by
/// serde rather than modeled here; only what accumulation reads is kept.
#[derive(Deserialize)]
struct ApiStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ApiStreamToolCallDelta>>,
}

#[derive(Deserialize)]
struct ApiStreamChoice {
    delta: ApiStreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
pub struct ApiStreamChunk {
    #[serde(default)]
    choices: Vec<ApiStreamChoice>,
    #[serde(default)]
    usage: Option<ApiUsage>,
}

struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// Folds a sequence of `chat.completion.chunk` deltas into the final
/// assistant `Message`, emitting streaming events on the way. Transports
/// (SSE, wllama bridge, ...) parse their framing into `ApiStreamChunk`s and
/// feed them here, so the delta semantics live in exactly one place.
#[derive(Default)]
pub struct ChatStreamAccumulator {
    text_buf: String,
    reasoning_buf: String,
    usage: Option<ApiUsage>,
    tool_calls: HashMap<usize, ToolCallAcc>,
    finish_reason: Option<String>,
}

impl ChatStreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &ApiStreamChunk, handler: &dyn AgentEventsHandler) {
        if let Some(choice) = chunk.choices.first() {
            // Some providers emit a whitespace-only content delta (e.g. a
            // lone "\n") right before or after a tool call, as a
            // formatting artifact rather than real response text.
            // Drop it while no real text has been seen yet, so
            // `Message::Assistant.text` stays `None` for tool-only
            // turns; once real content is underway, keep whitespace
            // deltas as-is so intra-response formatting (blank lines
            // between paragraphs, etc.) isn't lost.
            if let Some(content) = &choice.delta.content
                && !content.is_empty()
                && (!self.text_buf.is_empty() || !content.trim().is_empty())
            {
                self.text_buf.push_str(content);
                handler.on_chunk(&AgentMessageChunk {
                    text: Some(content.clone()),
                    reasoning: None,
                });
            }

            let thinking = choice
                .delta
                .reasoning
                .as_deref()
                .or(choice.delta.reasoning_content.as_deref());
            if let Some(reasoning) = thinking
                && !reasoning.is_empty()
            {
                self.reasoning_buf.push_str(reasoning);
                handler.on_chunk(&AgentMessageChunk {
                    text: None,
                    reasoning: Some(reasoning.to_string()),
                });
            }

            if let Some(tool_calls) = &choice.delta.tool_calls {
                for tool_call in tool_calls {
                    let entry = self.tool_calls.entry(tool_call.index).or_insert(ToolCallAcc {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(tool_call_id) = &tool_call.id {
                        entry.id = tool_call_id.clone();
                    }
                    if let Some(function) = &tool_call.function {
                        if let Some(tool_call_name) = &function.name {
                            entry.name = tool_call_name.clone();
                        }
                        if let Some(tool_call_arg) = &function.arguments {
                            entry.arguments.push_str(tool_call_arg);
                        }
                    }
                }
            }

            if let Some(finish_reason) = &choice.finish_reason {
                self.finish_reason = Some(finish_reason.clone());
            }
        }

        if let Some(usage) = &chunk.usage {
            self.usage = Some(usage.clone());
        }
    }

    pub fn finish(
        self,
        handler: &dyn AgentEventsHandler,
    ) -> Result<(Message, FinishReason), Box<dyn std::error::Error>> {
        if let Some(u) = &self.usage {
            handler.on_complete(&Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            });
        }

        let mut sorted_tool_calls: Vec<(usize, ToolCallAcc)> = self.tool_calls.into_iter().collect();
        sorted_tool_calls.sort_by_key(|(index, _)| *index);

        let message = Message::Assistant {
            text: if self.text_buf.is_empty() {
                None
            } else {
                Some(self.text_buf)
            },
            reasoning: if self.reasoning_buf.is_empty() {
                None
            } else {
                Some(self.reasoning_buf)
            },
            tool_calls: sorted_tool_calls
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

        let finish_reason = match self.finish_reason.as_deref() {
            Some("stop") => FinishReason::Stop,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolCalls,
            Some("length") => FinishReason::Length,
            Some(other) => FinishReason::Unknown(other.to_string()),
            None => FinishReason::Unknown("none".to_string()),
        };

        Ok((message, finish_reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolParameterSchema;
    use std::collections::HashMap;

    #[test]
    fn get_tool_param_type_maps_all_variants() {
        assert_eq!(
            get_tool_param_type(&ToolParameterTypeSchema::String),
            "string"
        );
        assert_eq!(
            get_tool_param_type(&ToolParameterTypeSchema::Number),
            "number"
        );
        assert_eq!(
            get_tool_param_type(&ToolParameterTypeSchema::Boolean),
            "boolean"
        );
    }

    #[test]
    fn api_message_from_system_message() {
        let api_message = ApiMessage::from(&Message::System("sys prompt".to_string()));
        let value = serde_json::to_value(&api_message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({"role": "system", "content": "sys prompt"})
        );
    }

    #[test]
    fn api_message_from_user_message() {
        let api_message = ApiMessage::from(&Message::User("hi there".to_string()));
        let value = serde_json::to_value(&api_message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({"role": "user", "content": "hi there"})
        );
    }

    #[test]
    fn api_message_from_assistant_without_tool_calls() {
        let message = Message::Assistant {
            text: Some("answer".to_string()),
            reasoning: Some("thinking".to_string()),
            tool_calls: vec![],
        };
        let api_message = ApiMessage::from(&message);
        let value = serde_json::to_value(&api_message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({"role": "assistant", "content": "answer", "reasoning": "thinking"})
        );
    }

    #[test]
    fn api_message_from_assistant_with_tool_calls() {
        let message = Message::Assistant {
            text: None,
            reasoning: None,
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: HashMap::from([("msg".to_string(), "hi".to_string())]),
            }],
        };
        let api_message = ApiMessage::from(&message);
        let value = serde_json::to_value(&api_message).unwrap();

        assert_eq!(value["role"], "assistant");
        assert!(value.get("content").is_none());
        assert_eq!(value["tool_calls"][0]["id"], "call-1");
        assert_eq!(value["tool_calls"][0]["type"], "function");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "echo");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            "{\"msg\":\"hi\"}"
        );
    }

    #[test]
    fn api_message_from_tool_ok_result() {
        let message = Message::Tool {
            call_id: "call-1".to_string(),
            result: Ok("output".to_string()),
        };
        let api_message = ApiMessage::from(&message);
        let value = serde_json::to_value(&api_message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({"role": "tool", "content": "output", "tool_call_id": "call-1"})
        );
    }

    #[test]
    fn api_message_from_tool_err_result() {
        let message = Message::Tool {
            call_id: "call-1".to_string(),
            result: Err("boom".to_string()),
        };
        let api_message = ApiMessage::from(&message);
        let value = serde_json::to_value(&api_message).unwrap();

        assert_eq!(value["content"], "Error: boom");
    }

    #[test]
    fn api_tool_from_schema() {
        let schema = ToolSchema {
            name: "get_weather".to_string(),
            description: "Get the weather".to_string(),
            parameters: HashMap::from([
                (
                    "city".to_string(),
                    ToolParameterSchema {
                        param_type: ToolParameterTypeSchema::String,
                        description: "City name".to_string(),
                        required: true,
                    },
                ),
                (
                    "days".to_string(),
                    ToolParameterSchema {
                        param_type: ToolParameterTypeSchema::Number,
                        description: "Forecast days".to_string(),
                        required: false,
                    },
                ),
            ]),
        };

        let api_tool = ApiTool::from(&schema);
        let value = serde_json::to_value(&api_tool).unwrap();

        assert_eq!(value["type"], "function");
        assert_eq!(value["function"]["name"], "get_weather");
        assert_eq!(value["function"]["parameters"]["type"], "object");
        assert_eq!(
            value["function"]["parameters"]["properties"]["city"]["type"],
            "string"
        );
        assert_eq!(
            value["function"]["parameters"]["properties"]["days"]["type"],
            "number"
        );
        assert_eq!(
            value["function"]["parameters"]["required"],
            serde_json::json!(["city"])
        );
    }

    /// The wllama bridge feeds chunks that may omit `id`/`usage` and carry
    /// extra fields — deserialization must stay lenient.
    #[test]
    fn api_stream_chunk_deserializes_minimal_and_extra_fields() {
        let chunk: ApiStreamChunk = serde_json::from_str(
            r#"{"object":"chat.completion.chunk","choices":[{"delta":{"content":"hi"}}]}"#,
        )
        .unwrap();
        assert_eq!(chunk.choices.len(), 1);
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
        assert!(chunk.usage.is_none());
    }
}
