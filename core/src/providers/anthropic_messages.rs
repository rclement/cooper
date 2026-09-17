//! `Provider` for the Anthropic Messages API (`POST {base_url}/messages`),
//! streamed over SSE. The wire shapes and the stream-to-message folding
//! live in `anthropic_wire`; this file only speaks HTTP.

use futures_util::StreamExt;

use async_trait::async_trait;

use crate::agent::{AgentEventsHandler, FinishReason, Message};
use crate::providers::Provider;
use crate::providers::anthropic_wire::{
    ApiConversation, ApiMessagesRequest, ApiStreamEvent, ApiTool, MessagesStreamAccumulator,
};
use crate::tools::ToolSchema;

/// The API version every request pins, so a future revision of the
/// Messages API cannot silently change what this client parses.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// `max_tokens` is mandatory on every request. This ceiling is high enough
/// for a long answer yet accepted by every current model; `with_max_tokens`
/// raises it for models that allow more.
pub const DEFAULT_MAX_TOKENS: u32 = 16_384;

pub struct AnthropicMessagesAPI {
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl AnthropicMessagesAPI {
    /// `base_url` is the API root, e.g. `https://api.anthropic.com/v1`;
    /// `/messages` is appended per request.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        AnthropicMessagesAPI {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            client: reqwest::Client::new(),
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

/// Reads SSE off the response until `message_stop`. Each event is an
/// `event:` line followed by a `data:` line; the JSON in `data:` carries
/// the same `type`, so only `data:` lines are looked at.
async fn process_stream(
    response: reqwest::Response,
    handler: &dyn AgentEventsHandler,
) -> Result<MessagesStreamAccumulator, Box<dyn std::error::Error>> {
    let mut stream = response.bytes_stream();
    let mut line_buf = String::new();
    let mut acc = MessagesStreamAccumulator::new();

    while let Some(item) = stream.next().await {
        let chunk_bytes = item?;
        let chunk_str = std::str::from_utf8(&chunk_bytes)?;

        // delimit SSE lines, possibly split across network chunks
        line_buf.push_str(chunk_str);
        let mut parts = line_buf.split('\n').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                // last part, may be incomplete so keep it
                line_buf = part.to_string();
                break;
            }

            let line = part.trim();
            let Some(json) = line.strip_prefix("data: ") else {
                continue;
            };

            let event = serde_json::from_str::<ApiStreamEvent>(json)?;
            let is_last = matches!(event, ApiStreamEvent::MessageStop);
            acc.push(event, handler)?;
            if is_last {
                return Ok(acc);
            }
        }
    }

    Err("stream ended without message_stop".into())
}

#[async_trait(?Send)]
impl Provider for AnthropicMessagesAPI {
    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        handler: &dyn AgentEventsHandler,
    ) -> Result<(Message, FinishReason), Box<dyn std::error::Error>> {
        let conversation = ApiConversation::from_messages(messages);
        let request = ApiMessagesRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: conversation.system,
            messages: conversation.messages,
            tools: tools.iter().map(ApiTool::from).collect(),
            stream: true,
        };

        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            // Cooper's web app calls the API straight from the browser,
            // with a key the user typed into their own settings. Anthropic
            // blocks browser origins unless the caller opts in with this
            // header; it is harmless from the CLI.
            .header("anthropic-dangerous-direct-browser-access", "true")
            .json(&request)
            .send()
            .await?;

        // On failure the API explains itself in a JSON body
        // (`{"type":"error","error":{"type":..,"message":..}}`); the
        // status code alone would leave the user guessing.
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(body);
            return Err(format!("anthropic api returned {status}: {detail}").into());
        }

        let acc = process_stream(response, handler).await?;
        acc.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentMessageChunk;
    use std::collections::HashMap;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    struct SpyHandler {
        text_chunks: std::sync::Mutex<Vec<String>>,
        reasoning_chunks: std::sync::Mutex<Vec<String>>,
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
    }

    /// Renders events the way the real API frames them: an `event:` line
    /// naming the type, then the `data:` JSON, then a blank line.
    fn sse_body(events: &[&str]) -> String {
        let mut body = String::new();
        for json in events {
            let event_name = serde_json::from_str::<serde_json::Value>(json).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string();
            body.push_str(&format!("event: {event_name}\ndata: {json}\n\n"));
        }
        body
    }

    async fn mock_server_with_body(body: String) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "text/event-stream")
                    .append_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;
        server
    }

    const TEXT_REPLY: &[&str] = &[
        r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"ping"}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#,
        r#"{"type":"message_stop"}"#,
    ];

    #[tokio::test]
    async fn a_text_reply_is_streamed_and_returned_with_its_usage() {
        let server = mock_server_with_body(sse_body(TEXT_REPLY)).await;
        let api = AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, finish_reason) = api.complete_stream(&[], &[], &handler).await.unwrap();

        assert!(matches!(finish_reason, FinishReason::Stop));
        match message {
            Message::Assistant { text, usage, .. } => {
                assert_eq!(text.as_deref(), Some("Hello"));
                let usage = usage.expect("usage");
                assert_eq!(usage.prompt_tokens, 10);
                assert_eq!(usage.completion_tokens, 5);
                assert_eq!(usage.total_tokens, 15);
            }
            _ => panic!("expected assistant message"),
        }
        assert_eq!(
            *handler.text_chunks.lock().unwrap(),
            vec!["Hello".to_string()]
        );
    }

    #[tokio::test]
    async fn the_request_carries_the_api_key_version_and_browser_opt_in_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .and(header("anthropic-dangerous-direct-browser-access", "true"))
            .and(header("content-type", "application/json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse_body(TEXT_REPLY), "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let api = AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model");

        let result = api.complete_stream(&[], &[], &SpyHandler::default()).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn the_request_body_is_laid_out_the_way_the_messages_api_expects() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "model": "test-model",
                "max_tokens": 2048,
                "stream": true,
                "system": "be brief",
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "hi"}]}
                ],
                "tools": [{"name": "echo", "input_schema": {"type": "object"}}]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse_body(TEXT_REPLY), "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let api =
            AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model").with_max_tokens(2048);
        let messages = [
            Message::System("be brief".to_string()),
            Message::User("hi".to_string()),
        ];
        let tools = [ToolSchema {
            name: "echo".to_string(),
            description: "Echo".to_string(),
            parameters: HashMap::new(),
        }];

        let result = api
            .complete_stream(&messages, &tools, &SpyHandler::default())
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn a_tool_call_is_returned_with_its_arguments_split_across_events() {
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":3,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"loc"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\":\"paris\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model");

        let (message, finish_reason) = api
            .complete_stream(&[], &[], &SpyHandler::default())
            .await
            .unwrap();

        assert!(matches!(finish_reason, FinishReason::ToolCalls));
        match message {
            Message::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "toolu_1");
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
    async fn thinking_is_streamed_as_reasoning() {
        let body = sse_body(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"pondering"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model");
        let handler = SpyHandler::default();

        let (message, _) = api.complete_stream(&[], &[], &handler).await.unwrap();

        match message {
            Message::Assistant { reasoning, .. } => {
                assert_eq!(reasoning.as_deref(), Some("pondering"))
            }
            _ => panic!("expected assistant message"),
        }
        assert_eq!(
            *handler.reasoning_chunks.lock().unwrap(),
            vec!["pondering".to_string()]
        );
    }

    #[tokio::test]
    async fn an_http_error_status_reports_the_api_explanation() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_raw(
                r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        let api = AnthropicMessagesAPI::new(server.uri(), "bad-key", "test-model");

        let result = api.complete_stream(&[], &[], &SpyHandler::default()).await;

        match result {
            Err(e) => assert_eq!(
                e.to_string(),
                "anthropic api returned 401 Unauthorized: invalid x-api-key"
            ),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn a_stream_that_never_reaches_message_stop_is_an_error() {
        let body = sse_body(&TEXT_REPLY[..TEXT_REPLY.len() - 1]);
        let server = mock_server_with_body(body).await;
        let api = AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model");

        let result = api.complete_stream(&[], &[], &SpyHandler::default()).await;

        match result {
            Err(e) => assert_eq!(e.to_string(), "stream ended without message_stop"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn an_error_event_mid_stream_is_reported_with_the_api_message() {
        let body = sse_body(&[
            r#"{"type":"message_start","message":{"id":"msg_1"}}"#,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ]);
        let server = mock_server_with_body(body).await;
        let api = AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model");

        let result = api.complete_stream(&[], &[], &SpyHandler::default()).await;

        match result {
            Err(e) => assert_eq!(
                e.to_string(),
                "anthropic api error (overloaded_error): Overloaded"
            ),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn malformed_event_json_is_an_error() {
        let server = mock_server_with_body("event: x\ndata: not-json\n\n".to_string()).await;
        let api = AnthropicMessagesAPI::new(server.uri(), "test-key", "test-model");

        let result = api.complete_stream(&[], &[], &SpyHandler::default()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn an_unreachable_server_is_an_error() {
        let api = AnthropicMessagesAPI::new("http://127.0.0.1:1/v1", "test-key", "test-model");

        let result = api.complete_stream(&[], &[], &SpyHandler::default()).await;

        assert!(result.is_err());
    }
}
