use crate::provider::Provider;
use crate::types::{Message, OutputChunk, Role, SessionEvent, ToolSchema};
use anyhow::{Result, anyhow};
use web_time::Instant;

#[allow(async_fn_in_trait)]
pub trait ToolExecutor {
    fn schemas(&self) -> Vec<ToolSchema>;
    async fn execute(&self, name: &str, args_json: &str) -> Result<String>;
}

/// Per-turn hooks for session logging. Implement in the shell; pass `None` to skip.
pub trait SessionLogger {
    fn on_event(&mut self, event: &SessionEvent);
}

const MAX_TURNS: usize = 20;

/// Run one agentic turn against an existing message history.
/// Appends the assistant response (and any intermediate tool calls) to `messages`.
/// The caller must push the user message before calling this.
pub async fn run_turn(
    messages: &mut Vec<Message>,
    provider: &impl Provider,
    executor: &impl ToolExecutor,
    mut logger: Option<&mut dyn SessionLogger>,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
    let tool_schemas = executor.schemas();

    for _ in 0..MAX_TURNS {
        if let Some(ref mut l) = logger {
            l.on_event(&SessionEvent::Request { messages: messages.clone() });
        }

        let turn_start = Instant::now();
        let mut thinking_buf = String::new();
        let mut wrapped = |chunk: OutputChunk| {
            if let OutputChunk::Thinking { ref text } = chunk {
                thinking_buf.push_str(text);
            }
            on_chunk(chunk);
        };

        let (response, usage) = provider
            .call(messages.clone(), &tool_schemas, &mut wrapped)
            .await?;
        drop(wrapped);

        if let Some(ref mut l) = logger {
            l.on_event(&SessionEvent::Response {
                thinking: if thinking_buf.is_empty() { None } else { Some(thinking_buf.clone()) },
                message: response.clone(),
                duration_ms: turn_start.elapsed().as_millis() as u64,
                usage: usage.clone(),
            });
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
    provider: &impl Provider,
    executor: &impl ToolExecutor,
    logger: Option<&mut dyn SessionLogger>,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
    let mut messages = vec![
        Message::new(Role::System, system_prompt),
        Message::new(Role::User, prompt),
    ];
    run_turn(&mut messages, provider, executor, logger, on_chunk).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use crate::types::{Message, OutputChunk, Role, ToolCall, ToolSchema, Usage};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    // ── Test fixtures ─────────────────────────────────────────────────────────

    struct EchoExecutor;

    impl ToolExecutor for EchoExecutor {
        fn schemas(&self) -> Vec<ToolSchema> {
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
        fn on_event(&mut self, event: &crate::types::SessionEvent) {
            match event {
                crate::types::SessionEvent::Request { messages } => {
                    self.request_counts.push(messages.len());
                }
                crate::types::SessionEvent::Response { thinking, message, .. } => {
                    self.thinking_seen.push(thinking.is_some());
                    self.response_contents.push(message.content.clone());
                }
                _ => {}
            }
        }
    }

    // ── MockProvider ──────────────────────────────────────────────────────────

    struct MockResponse {
        message: Message,
        usage: Option<Usage>,
        thinking: Option<String>,
    }

    struct MockProvider {
        responses: RefCell<VecDeque<MockResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<MockResponse>) -> Self {
            Self { responses: RefCell::new(responses.into()) }
        }

        fn text(s: &str) -> Self {
            Self::new(vec![MockResponse {
                message: Message::new(Role::Assistant, s),
                usage: None,
                thinking: None,
            }])
        }
    }

    impl Provider for MockProvider {
        async fn call(
            &self,
            _messages: Vec<Message>,
            _tools: &[ToolSchema],
            on_chunk: &mut dyn FnMut(OutputChunk),
        ) -> Result<(Message, Option<Usage>)> {
            let resp = self
                .responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| anyhow!("MockProvider: no more responses"))?;
            if let Some(ref t) = resp.thinking {
                on_chunk(OutputChunk::Thinking { text: t.clone() });
            }
            if resp.message.tool_calls.is_none() && !resp.message.content.is_empty() {
                on_chunk(OutputChunk::Content { text: resp.message.content.clone() });
            }
            Ok((resp.message, resp.usage))
        }
    }

    // ── run_turn tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn run_turn_returns_content() {
        let provider = MockProvider::text("hello");
        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "hi"),
        ];
        let result = run_turn(&mut messages, &provider, &EchoExecutor, None, &mut |_| {})
            .await
            .unwrap();

        assert_eq!(result, "hello");
        assert_eq!(messages.len(), 3); // sys + user + assistant
        assert!(matches!(messages.last().unwrap().role, Role::Assistant));
    }

    #[tokio::test]
    async fn run_turn_executes_tool_then_returns() {
        let mut tool_msg = Message::new(Role::Assistant, "");
        tool_msg.tool_calls = Some(vec![ToolCall {
            id: "tc0".into(),
            name: "list_files".into(),
            arguments: "{}".into(),
        }]);
        let provider = MockProvider::new(vec![
            MockResponse { message: tool_msg, usage: None, thinking: None },
            MockResponse {
                message: Message::new(Role::Assistant, "done"),
                usage: None,
                thinking: None,
            },
        ]);

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "list"),
        ];
        let mut tool_calls_seen = vec![];
        let result = run_turn(
            &mut messages,
            &provider,
            &EchoExecutor,
            None,
            &mut |c| {
                if let OutputChunk::ToolCall { name, .. } = &c {
                    tool_calls_seen.push(name.clone());
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(result, "done");
        assert_eq!(tool_calls_seen, vec!["list_files"]);
    }

    #[tokio::test]
    async fn run_turn_with_logger() {
        let provider = MockProvider::new(vec![MockResponse {
            message: Message::new(Role::Assistant, "answer"),
            usage: None,
            thinking: Some("think".into()),
        }]);

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "hi"),
        ];
        let mut logger = RecordingLogger::new();
        run_turn(&mut messages, &provider, &EchoExecutor, Some(&mut logger), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(logger.request_counts.len(), 1);
        assert_eq!(logger.request_counts[0], 2); // sys + user
        assert_eq!(logger.thinking_seen, vec![true]);
    }

    #[tokio::test]
    async fn run_turn_max_turns_exceeded() {
        let mut responses = vec![];
        for _ in 0..=MAX_TURNS_FOR_TEST {
            let mut msg = Message::new(Role::Assistant, "");
            msg.tool_calls = Some(vec![ToolCall {
                id: "tc".into(),
                name: "list_files".into(),
                arguments: "{}".into(),
            }]);
            responses.push(MockResponse { message: msg, usage: None, thinking: None });
        }
        let provider = MockProvider::new(responses);

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "loop"),
        ];
        let result = run_turn(&mut messages, &provider, &EchoExecutor, None, &mut |_| {}).await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("exceeded") || msg.contains("turns"));
    }

    #[tokio::test]
    async fn run_convenience_wraps_run_turn() {
        let provider = MockProvider::text("result");
        let result = run(
            "user prompt".into(),
            "system prompt".into(),
            &provider,
            &EchoExecutor,
            None,
            &mut |_| {},
        )
        .await
        .unwrap();

        assert_eq!(result, "result");
    }

    #[tokio::test]
    async fn run_turn_tool_executor_error_captured() {
        struct FailExecutor;
        impl ToolExecutor for FailExecutor {
            fn schemas(&self) -> Vec<ToolSchema> { vec![] }
            async fn execute(&self, _: &str, _: &str) -> Result<String> {
                Err(anyhow!("tool failed"))
            }
        }

        let mut tool_msg = Message::new(Role::Assistant, "");
        tool_msg.tool_calls = Some(vec![ToolCall {
            id: "tc".into(),
            name: "list_files".into(),
            arguments: "{}".into(),
        }]);
        let provider = MockProvider::new(vec![
            MockResponse { message: tool_msg, usage: None, thinking: None },
            MockResponse {
                message: Message::new(Role::Assistant, "final"),
                usage: None,
                thinking: None,
            },
        ]);

        let mut messages = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "run"),
        ];
        let mut results_seen = vec![];
        let _ = run_turn(
            &mut messages,
            &provider,
            &FailExecutor,
            None,
            &mut |c| {
                if let OutputChunk::ToolResult { output, .. } = &c {
                    results_seen.push(output.clone());
                }
            },
        )
        .await
        .unwrap();

        assert!(results_seen.iter().any(|r| r.starts_with("error:")));
    }

    #[tokio::test]
    async fn run_turn_usage_chunk_emitted() {
        let provider = MockProvider::new(vec![MockResponse {
            message: Message::new(Role::Assistant, "hi"),
            usage: Some(Usage { prompt_tokens: 5, completion_tokens: 2, total_tokens: 7 }),
            thinking: None,
        }]);

        let mut messages = vec![Message::new(Role::User, "hi")];
        let mut usage_chunks = vec![];
        run_turn(
            &mut messages,
            &provider,
            &EchoExecutor,
            None,
            &mut |c| {
                if let OutputChunk::Usage { .. } = &c {
                    usage_chunks.push(c);
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(usage_chunks.len(), 1);
        assert!(matches!(&usage_chunks[0], OutputChunk::Usage { total_tokens: 7, .. }));
    }
}
