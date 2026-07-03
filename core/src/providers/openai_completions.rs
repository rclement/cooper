use std::collections::HashMap;
use std::vec;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::agent::{AgentEventsHandler, AgentMessageChunk, FinishReason, Message, ToolCall, Usage};
use crate::providers::Provider;
use crate::tools::{ToolParameterTypeSchema, ToolSchema};

fn get_tool_param_type(param_type: &ToolParameterTypeSchema) -> &'static str {
    match param_type {
        ToolParameterTypeSchema::String => "string",
        ToolParameterTypeSchema::Number => "number",
        ToolParameterTypeSchema::Boolean => "boolean",
    }
}

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
    handler: &dyn AgentEventsHandler,
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
                        && (!result.text_buf.is_empty() || !content.trim().is_empty())
                    {
                        result.text_buf.push_str(content);
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
                        result.reasoning_buf.push_str(reasoning);
                        handler.on_chunk(&AgentMessageChunk {
                            text: None,
                            reasoning: Some(reasoning.to_string()),
                        });
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

#[async_trait(?Send)]
impl Provider for OpenAICompletionsAPI {
    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        handler: &dyn AgentEventsHandler,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolParameterSchema;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Records callbacks from `complete_stream` so tests can assert on
    /// streamed chunks and completion usage without touching stdout.
    #[derive(Default)]
    struct SpyHandler {
        text_chunks: std::sync::Mutex<Vec<String>>,
        reasoning_chunks: std::sync::Mutex<Vec<String>>,
        usages: std::sync::Mutex<Vec<(u64, u64, u64)>>,
    }

    impl AgentEventsHandler for SpyHandler {
        fn on_chunk(&self, chunk: &AgentMessageChunk) {
            if let Some(t) = &chunk.text {
                self.text_chunks.lock().unwrap().push(t.clone());
            }
            if let Some(r) = &chunk.reasoning {
                self.reasoning_chunks.lock().unwrap().push(r.clone());
            }
        }

        fn on_complete(&self, usage: &Usage) {
            self.usages.lock().unwrap().push((
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens,
            ));
        }
    }

    /// Joins raw SSE `data:` payloads (without the trailing `[DONE]`) into a
    /// single response body, mirroring what an OpenAI-compatible server sends.
    fn sse_body(data_lines: &[&str]) -> String {
        let mut body = String::new();
        for line in data_lines {
            body.push_str("data: ");
            body.push_str(line);
            body.push_str("\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    async fn mock_server_with_body(body: String) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "text/event-stream")
                    .append_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;
        server
    }

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

    #[tokio::test]
    async fn complete_stream_returns_text_and_usage_on_stop() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello","reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":null}],"usage":null}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":null,"content":null,"reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, finish_reason) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match message {
            Message::Assistant {
                text,
                reasoning,
                tool_calls,
            } => {
                assert_eq!(text.as_deref(), Some("Hello"));
                assert_eq!(reasoning, None);
                assert!(tool_calls.is_empty());
            }
            _ => panic!("expected assistant message"),
        }
        assert!(matches!(finish_reason, FinishReason::Stop));
        assert_eq!(
            *handler.text_chunks.lock().unwrap(),
            vec!["Hello".to_string()]
        );
        assert_eq!(*handler.usages.lock().unwrap(), vec![(10, 5, 15)]);
    }

    #[tokio::test]
    async fn complete_stream_drops_leading_whitespace_only_content_before_tool_call() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":"\n","reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":null}],"usage":null}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":null,"content":null,"reasoning":null,"reasoning_content":null,"tool_calls":[{"index":0,"id":"call-1","function":{"name":"get_weather","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, _) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match message {
            Message::Assistant { text, .. } => assert_eq!(text, None),
            _ => panic!("expected assistant message"),
        }
        assert!(handler.text_chunks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn complete_stream_preserves_whitespace_once_real_text_has_started() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":"First.","reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":null}],"usage":null}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":null,"content":"\n\n","reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":null}],"usage":null}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":null,"content":"Second.","reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":"stop"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, _) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match message {
            Message::Assistant { text, .. } => {
                assert_eq!(text.as_deref(), Some("First.\n\nSecond."))
            }
            _ => panic!("expected assistant message"),
        }
        assert_eq!(
            *handler.text_chunks.lock().unwrap(),
            vec![
                "First.".to_string(),
                "\n\n".to_string(),
                "Second.".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn complete_stream_captures_reasoning_field() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning":"thinking...","reasoning_content":null,"tool_calls":null},"finish_reason":"stop"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, _) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match message {
            Message::Assistant { reasoning, .. } => {
                assert_eq!(reasoning.as_deref(), Some("thinking..."))
            }
            _ => panic!("expected assistant message"),
        }
        assert_eq!(
            *handler.reasoning_chunks.lock().unwrap(),
            vec!["thinking...".to_string()]
        );
    }

    #[tokio::test]
    async fn complete_stream_falls_back_to_reasoning_content_field() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning":null,"reasoning_content":"thinking2","tool_calls":null},"finish_reason":"stop"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, _) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match message {
            Message::Assistant { reasoning, .. } => {
                assert_eq!(reasoning.as_deref(), Some("thinking2"))
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn complete_stream_accumulates_tool_call_across_deltas() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning":null,"reasoning_content":null,"tool_calls":[{"index":0,"id":"call-1","function":{"name":"get_weather","arguments":null}}]},"finish_reason":null}],"usage":null}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":null,"content":null,"reasoning":null,"reasoning_content":null,"tool_calls":[{"index":0,"id":null,"function":{"name":null,"arguments":"{\"loc"}}]},"finish_reason":null}],"usage":null}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":null,"content":null,"reasoning":null,"reasoning_content":null,"tool_calls":[{"index":0,"id":null,"function":{"name":null,"arguments":"\":\"paris\"}"}}]},"finish_reason":"tool_calls"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, finish_reason) = api.complete_stream(&[], &[], &handler).await.unwrap();

        assert!(matches!(finish_reason, FinishReason::ToolCalls));
        match message {
            Message::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "call-1");
                assert_eq!(tool_calls[0].name, "get_weather");
                assert_eq!(
                    tool_calls[0].arguments,
                    HashMap::from([("loc".to_string(), "paris".to_string())])
                );
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn complete_stream_errors_when_tool_call_arguments_are_not_a_string_map() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning":null,"reasoning_content":null,"tool_calls":[{"index":0,"id":"call-1","function":{"name":"get_weather","arguments":"{\"count\":5}"}}]},"finish_reason":"tool_calls"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let result = api.complete_stream(&[], &[], &handler).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn complete_stream_maps_function_call_finish_reason_to_tool_calls() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":"function_call"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (_, finish_reason) = api.complete_stream(&[], &[], &handler).await.unwrap();

        assert!(matches!(finish_reason, FinishReason::ToolCalls));
    }

    #[tokio::test]
    async fn complete_stream_maps_length_finish_reason() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":"length"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (_, finish_reason) = api.complete_stream(&[], &[], &handler).await.unwrap();

        assert!(matches!(finish_reason, FinishReason::Length));
    }

    #[tokio::test]
    async fn complete_stream_maps_unrecognized_finish_reason() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":"content_filter"}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (_, finish_reason) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match finish_reason {
            FinishReason::Unknown(s) => assert_eq!(s, "content_filter"),
            _ => panic!("expected unknown finish reason"),
        }
    }

    #[tokio::test]
    async fn complete_stream_defaults_to_unknown_when_no_finish_reason_sent() {
        let body = sse_body(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":"hi","reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":null}],"usage":null}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (_, finish_reason) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match finish_reason {
            FinishReason::Unknown(s) => assert_eq!(s, "none"),
            _ => panic!("expected unknown finish reason"),
        }
    }

    #[tokio::test]
    async fn complete_stream_errors_on_http_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let result = api.complete_stream(&[], &[], &handler).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn complete_stream_errors_when_stream_ends_without_done_marker() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"data: {"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":"hi","reasoning":null,"reasoning_content":null,"tool_calls":null},"finish_reason":null}],"usage":null}"#.to_string() + "\n\n",
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let result = api.complete_stream(&[], &[], &handler).await;

        match result {
            Err(e) => assert_eq!(e.to_string(), "stream ended without [Done]"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn complete_stream_errors_on_malformed_json_chunk() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("data: not-json\n\n", "text/event-stream"),
            )
            .mount(&server)
            .await;
        let api = OpenAICompletionsAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let result = api.complete_stream(&[], &[], &handler).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn complete_stream_errors_on_connection_failure() {
        let api = OpenAICompletionsAPI::new("http://127.0.0.1:1", "test-key", "test-model");
        let handler = SpyHandler::default();

        let result = api.complete_stream(&[], &[], &handler).await;

        assert!(result.is_err());
    }
}
