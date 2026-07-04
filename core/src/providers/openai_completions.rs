use futures_util::StreamExt;

use async_trait::async_trait;

use crate::agent::{AgentEventsHandler, FinishReason, Message};
use crate::providers::Provider;
use crate::providers::openai_wire::{
    ApiCompletionRequest, ApiMessage, ApiStreamChunk, ApiStreamOptions, ApiTool,
    ChatStreamAccumulator,
};
use crate::tools::ToolSchema;

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

/// Splits the SSE framing into `data:` payloads and feeds each parsed chunk
/// into the accumulator; returns it once the `[DONE]` marker arrives.
async fn process_stream(
    response: reqwest::Response,
    handler: &dyn AgentEventsHandler,
) -> Result<ChatStreamAccumulator, Box<dyn std::error::Error>> {
    let mut stream = response.bytes_stream();
    let mut line_buf = String::new();
    let mut acc = ChatStreamAccumulator::new();

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
                return Ok(acc);
            }

            if let Some(json) = line.strip_prefix("data: ") {
                let chunk = serde_json::from_str::<ApiStreamChunk>(json)?;
                acc.push(&chunk, handler);
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

        let acc = process_stream(response, handler).await?;
        acc.finish(handler)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentMessageChunk, Usage};
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
