use crate::provider;
use crate::types::{ApiType, Message, OutputChunk, Role, Usage};
use anyhow::{Result, anyhow};

#[allow(async_fn_in_trait)]
pub trait ToolExecutor {
    fn schemas(&self) -> Vec<serde_json::Value>;
    async fn execute(&self, name: &str, args_json: &str) -> Result<String>;
}

/// Per-turn hooks for session logging. Implement in the shell; pass `None` to skip.
pub trait SessionLogger {
    fn on_request(&mut self, messages: &[Message]);
    fn on_response(&mut self, thinking: Option<&str>, message: &Message, usage: Option<&Usage>);
}

const MAX_TURNS: usize = 20;

/// Run one agentic turn against an existing message history.
/// Appends the assistant response (and any intermediate tool calls) to `messages`.
/// The caller must push the user message before calling this.
pub async fn run_turn(
    messages: &mut Vec<Message>,
    api_type: &ApiType,
    base_url: &str,
    api_key: &str,
    model: &str,
    executor: &impl ToolExecutor,
    mut logger: Option<&mut dyn SessionLogger>,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
    let tool_schemas = executor.schemas();

    for _ in 0..MAX_TURNS {
        if let Some(ref mut l) = logger {
            l.on_request(messages);
        }

        let mut thinking_buf = String::new();
        let mut wrapped = |chunk: OutputChunk| {
            if let OutputChunk::Thinking { ref text } = chunk {
                thinking_buf.push_str(text);
            }
            on_chunk(chunk);
        };

        let (response, usage) = provider::call(
            api_type,
            base_url,
            api_key,
            model,
            messages.clone(),
            &tool_schemas,
            &mut wrapped,
        )
        .await?;
        drop(wrapped);

        if let Some(ref mut l) = logger {
            l.on_response(
                if thinking_buf.is_empty() {
                    None
                } else {
                    Some(&thinking_buf)
                },
                &response,
                usage.as_ref(),
            );
        }

        if let Some(ref u) = usage {
            on_chunk(OutputChunk::Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            });
        }

        if let Some(tool_calls) = response.tool_calls.clone() {
            messages.push(response);
            for tc in tool_calls {
                on_chunk(OutputChunk::ToolCall {
                    name: tc.name.clone(),
                    args: tc.arguments.clone(),
                });
                let result = executor
                    .execute(&tc.name, &tc.arguments)
                    .await
                    .unwrap_or_else(|e| format!("error: {}", e));
                on_chunk(OutputChunk::ToolResult {
                    name: tc.name.clone(),
                    output: result.clone(),
                });
                messages.push(Message::tool_result(tc.id, result));
            }
        } else {
            let content = response.content.clone();
            messages.push(response);
            return Ok(content);
        }
    }

    Err(anyhow!(
        "agent loop exceeded {} turns without a final response",
        MAX_TURNS
    ))
}

/// Constant exposed for tests only.
#[cfg(test)]
pub const MAX_TURNS_FOR_TEST: usize = MAX_TURNS;

pub async fn run(
    prompt: String,
    system_prompt: String,
    api_type: &ApiType,
    base_url: &str,
    api_key: &str,
    model: &str,
    executor: &impl ToolExecutor,
    logger: Option<&mut dyn SessionLogger>,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
    let mut messages = vec![
        Message::new(Role::System, system_prompt),
        Message::new(Role::User, prompt),
    ];
    run_turn(
        &mut messages,
        api_type,
        base_url,
        api_key,
        model,
        executor,
        logger,
        on_chunk,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApiType, Message, OutputChunk, Role};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct EchoExecutor;

    impl ToolExecutor for EchoExecutor {
        fn schemas(&self) -> Vec<serde_json::Value> {
            vec![]
        }
        async fn execute(&self, _name: &str, _args: &str) -> Result<String> {
            Ok("tool output".to_string())
        }
    }

    struct RecordingLogger {
        request_counts: Vec<usize>,
        response_contents: Vec<String>,
        thinking_seen: Vec<bool>,
    }

    impl RecordingLogger {
        fn new() -> Self {
            Self { request_counts: vec![], response_contents: vec![], thinking_seen: vec![] }
        }
    }

    impl SessionLogger for RecordingLogger {
        fn on_request(&mut self, messages: &[Message]) {
            self.request_counts.push(messages.len());
        }
        fn on_response(&mut self, thinking: Option<&str>, msg: &Message, _: Option<&Usage>) {
            self.thinking_seen.push(thinking.is_some());
            self.response_contents.push(msg.content.clone());
        }
    }

    fn oai_text_sse(text: &str) -> String {
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}],\"usage\":null}}\ndata: [DONE]\n",
            text = text
        )
    }

    fn oai_tool_then_text_sse(text: &str) -> (String, String) {
        let tool_response = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"tc0\",\"function\":{{\"name\":\"list_files\",\"arguments\":\"{{}}\"}}}}]}}}}],\"usage\":null}}\ndata: [DONE]\n"
        );
        let text_response = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}],\"usage\":null}}\ndata: [DONE]\n",
            text = text
        );
        (tool_response, text_response)
    }

    #[tokio::test]
    async fn run_turn_returns_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("hello")))
            .mount(&server)
            .await;

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "hi"),
        ];
        let result = run_turn(
            &mut messages,
            &ApiType::OpenaiCompletions,
            &server.uri(),
            "key",
            "model",
            &EchoExecutor,
            None,
            &mut |_| {},
        ).await.unwrap();

        assert_eq!(result, "hello");
        assert_eq!(messages.len(), 3); // sys + user + assistant
        assert!(matches!(messages.last().unwrap().role, Role::Assistant));
    }

    #[tokio::test]
    async fn run_turn_executes_tool_then_returns() {
        let server = MockServer::start().await;
        let (tool_sse, text_sse) = oai_tool_then_text_sse("done");

        // First call returns tool request, second returns final text
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tool_sse))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(text_sse))
            .mount(&server)
            .await;

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "list"),
        ];
        let mut tool_calls_seen = vec![];
        let result = run_turn(
            &mut messages,
            &ApiType::OpenaiCompletions,
            &server.uri(),
            "key",
            "model",
            &EchoExecutor,
            None,
            &mut |c| {
                if let OutputChunk::ToolCall { name, .. } = &c {
                    tool_calls_seen.push(name.clone());
                }
            },
        ).await.unwrap();

        assert_eq!(result, "done");
        assert_eq!(tool_calls_seen, vec!["list_files"]);
    }

    #[tokio::test]
    async fn run_turn_with_logger() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}],\"usage\":null}\ndata: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}],\"usage\":null}\ndata: [DONE]\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "hi"),
        ];
        let mut logger = RecordingLogger::new();
        run_turn(
            &mut messages,
            &ApiType::OpenaiCompletions,
            &server.uri(),
            "key",
            "model",
            &EchoExecutor,
            Some(&mut logger),
            &mut |_| {},
        ).await.unwrap();

        assert_eq!(logger.request_counts.len(), 1);
        assert_eq!(logger.request_counts[0], 2); // sys + user
        assert_eq!(logger.thinking_seen, vec![true]); // thinking was seen
    }

    #[tokio::test]
    async fn run_turn_max_turns_exceeded() {
        let server = MockServer::start().await;
        let tool_sse = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{}\"}}]}}],\"usage\":null}\ndata: [DONE]\n";
        // Always return a tool call — agent will loop until MAX_TURNS
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tool_sse))
            .mount(&server)
            .await;

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "loop"),
        ];
        let result = run_turn(
            &mut messages,
            &ApiType::OpenaiCompletions,
            &server.uri(),
            "key",
            "model",
            &EchoExecutor,
            None,
            &mut |_| {},
        ).await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("exceeded") || msg.contains("turns"));
    }

    #[tokio::test]
    async fn run_convenience_wraps_run_turn() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("result")))
            .mount(&server)
            .await;

        let result = run(
            "user prompt".into(),
            "system prompt".into(),
            &ApiType::OpenaiCompletions,
            &server.uri(),
            "key",
            "model",
            &EchoExecutor,
            None,
            &mut |_| {},
        ).await.unwrap();

        assert_eq!(result, "result");
    }

    #[tokio::test]
    async fn run_turn_tool_executor_error_captured() {
        struct FailExecutor;
        impl ToolExecutor for FailExecutor {
            fn schemas(&self) -> Vec<serde_json::Value> { vec![] }
            async fn execute(&self, _: &str, _: &str) -> Result<String> {
                Err(anyhow!("tool failed"))
            }
        }

        let server = MockServer::start().await;
        let (tool_sse, text_sse) = oai_tool_then_text_sse("final");
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tool_sse))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(text_sse))
            .mount(&server)
            .await;

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "run"),
        ];
        let mut results_seen = vec![];
        let _ = run_turn(
            &mut messages,
            &ApiType::OpenaiCompletions,
            &server.uri(),
            "key",
            "model",
            &FailExecutor,
            None,
            &mut |c| {
                if let OutputChunk::ToolResult { output, .. } = &c {
                    results_seen.push(output.clone());
                }
            },
        ).await.unwrap();

        // Error from executor is captured as "error: ..." result, not propagated
        assert!(results_seen.iter().any(|r| r.starts_with("error:")));
    }

    #[tokio::test]
    async fn run_turn_usage_chunk_emitted() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\ndata: [DONE]\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mut messages = vec![Message::new(Role::User, "hi")];
        let mut usage_chunks = vec![];
        run_turn(
            &mut messages,
            &ApiType::OpenaiCompletions,
            &server.uri(),
            "key",
            "model",
            &EchoExecutor,
            None,
            &mut |c| {
                if let OutputChunk::Usage { .. } = &c {
                    usage_chunks.push(c);
                }
            },
        ).await.unwrap();

        assert_eq!(usage_chunks.len(), 1);
        assert!(matches!(&usage_chunks[0], OutputChunk::Usage { total_tokens: 7, .. }));
    }
}
