use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use web_time::Instant;

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
                ..
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
            Message::Tool {
                call_id, result, ..
            } => ApiMessage {
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
    #[serde(default)]
    function: Option<ApiStreamToolCallFunction>,
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamPhase {
    Reasoning,
    Response,
}

#[derive(Default)]
pub struct ChatStreamAccumulator {
    text_buf: String,
    reasoning_buf: String,
    usage: Option<ApiUsage>,
    tool_calls: HashMap<usize, ToolCallAcc>,
    finish_reason: Option<String>,
    current_phase: Option<(StreamPhase, Instant)>,
    reasoning_ms: Option<u64>,
    response_ms: Option<u64>,
}

impl ChatStreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    fn enter_phase(&mut self, phase: StreamPhase) {
        match self.current_phase {
            Some((current, _)) if current == phase => {} // already in it
            _ => {
                self.close_current_phase();
                self.current_phase = Some((phase, Instant::now()));
            }
        }
    }

    fn close_current_phase(&mut self) {
        if let Some((phase, started_at)) = self.current_phase.take() {
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            match phase {
                StreamPhase::Reasoning => self.reasoning_ms = Some(elapsed_ms),
                StreamPhase::Response => self.response_ms = Some(elapsed_ms),
            }
        }
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
                self.enter_phase(StreamPhase::Response);
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
                self.enter_phase(StreamPhase::Reasoning);
                self.reasoning_buf.push_str(reasoning);
                handler.on_chunk(&AgentMessageChunk {
                    text: None,
                    reasoning: Some(reasoning.to_string()),
                });
            }

            if let Some(tool_calls) = &choice.delta.tool_calls {
                for tool_call in tool_calls {
                    let entry = self
                        .tool_calls
                        .entry(tool_call.index)
                        .or_insert(ToolCallAcc {
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

    pub fn finish(mut self) -> Result<(Message, FinishReason), Box<dyn std::error::Error>> {
        self.close_current_phase();

        let mut sorted_tool_calls: Vec<(usize, ToolCallAcc)> =
            self.tool_calls.into_iter().collect();
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
            reasoning_duration_ms: self.reasoning_ms,
            response_duration_ms: self.response_ms,
            usage: self.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
            at_ms: None,
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
        let message = Message::assistant(
            Some("answer".to_string()),
            Some("thinking".to_string()),
            vec![],
        );
        let api_message = ApiMessage::from(&message);
        let value = serde_json::to_value(&api_message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({"role": "assistant", "content": "answer", "reasoning": "thinking"})
        );
    }

    #[test]
    fn api_message_from_assistant_with_tool_calls() {
        let message = Message::assistant(
            None,
            None,
            vec![ToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: HashMap::from([("msg".to_string(), "hi".to_string())]),
            }],
        );
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
            duration_ms: None,
            at_ms: None,
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
            duration_ms: None,
            at_ms: None,
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

    /// Accumulation tests drive `push` directly, the way a transport does;
    /// the handler is irrelevant to what's being asserted.
    struct NullHandler;

    impl AgentEventsHandler for NullHandler {
        fn on_chunk(&self, _chunk: &AgentMessageChunk) {}
    }

    fn reasoning_delta(text: &str) -> ApiStreamChunk {
        serde_json::from_str(&format!(
            r#"{{"choices":[{{"delta":{{"reasoning":"{text}"}}}}]}}"#
        ))
        .unwrap()
    }

    fn content_delta(text: &str) -> ApiStreamChunk {
        serde_json::from_str(&format!(
            r#"{{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn accumulated_message_records_how_long_reasoning_and_response_took() {
        // Feed deltas the way a reasoning model streams them: think for a
        // while, then write the answer. Each phase gets its own duration.
        let mut acc = ChatStreamAccumulator::new();

        acc.push(&reasoning_delta("thinking"), &NullHandler);
        std::thread::sleep(std::time::Duration::from_millis(20));
        acc.push(&content_delta("the answer"), &NullHandler);
        std::thread::sleep(std::time::Duration::from_millis(20));

        let (message, _) = acc.finish().unwrap();
        match message {
            Message::Assistant {
                reasoning_duration_ms,
                response_duration_ms,
                ..
            } => {
                assert!(reasoning_duration_ms.expect("reasoning should be timed") >= 10);
                assert!(response_duration_ms.expect("response should be timed") >= 10);
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn accumulated_message_without_reasoning_gets_no_reasoning_duration() {
        let mut acc = ChatStreamAccumulator::new();

        acc.push(&content_delta("plain answer"), &NullHandler);

        let (message, _) = acc.finish().unwrap();
        match message {
            Message::Assistant {
                reasoning_duration_ms,
                response_duration_ms,
                ..
            } => {
                assert_eq!(reasoning_duration_ms, None);
                assert!(response_duration_ms.is_some());
            }
            _ => panic!("expected assistant message"),
        }
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
