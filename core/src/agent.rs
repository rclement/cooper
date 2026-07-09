use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use web_time::Instant;

use crate::providers::Provider;
use crate::tools;

const DEFAULT_SYSTEM_PROMPT_TEMPLATE: &str = r#"You are agent Cooper, a special AI agent harness.
{%- if agent_instructions %}

<agent-instructions>
{{ agent_instructions }}
</agent-instructions>
{%- endif %}
{%- if context_files %}

<context>
{%- for file in context_files %}
<file path="{{ file.path }}">
{{ file.content }}
</file>
{%- endfor %}
</context>
{%- endif %}

Current date: {{ current_date }}
Current time: {{ current_time }}
{%- if current_working_dir %}
Current working directory: {{ current_working_dir }}
{%- endif %}"#;

#[derive(Serialize)]
struct ContextFileView<'a> {
    path: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct SystemPromptContext<'a> {
    agent_instructions: Option<&'a str>,
    context_files: Vec<ContextFileView<'a>>,
    current_date: String,
    current_time: String,
    current_working_dir: Option<&'a str>,
}

fn build_system_prompt(
    system_prompt_template: Option<&str>,
    agent_instructions: Option<&str>,
    context_files: &HashMap<String, String>,
    current_working_dir: Option<&str>,
) -> Result<String, minijinja::Error> {
    let now = chrono::Local::now();
    let context = SystemPromptContext {
        agent_instructions,
        context_files: context_files
            .iter()
            .map(|(path, content)| ContextFileView { path, content })
            .collect(),
        current_date: now.format("%Y-%m-%d").to_string(),
        current_time: now.format("%H:%M:%S %z").to_string(),
        current_working_dir,
    };

    let mut env = minijinja::Environment::new();
    env.add_template(
        "system_prompt",
        system_prompt_template.unwrap_or(DEFAULT_SYSTEM_PROMPT_TEMPLATE),
    )?;
    env.get_template("system_prompt")?.render(context)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        text: Option<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<i64>,
    },
    Tool {
        call_id: String,
        result: Result<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<i64>,
    },
}

impl Message {
    pub fn assistant(
        text: Option<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Message::Assistant {
            text,
            reasoning,
            tool_calls,
            reasoning_duration_ms: None,
            response_duration_ms: None,
            usage: None,
            at_ms: None,
        }
    }
}

pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Unknown(String),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

pub struct AgentMessageChunk {
    pub text: Option<String>,
    pub reasoning: Option<String>,
}

pub trait AgentEventsHandler {
    fn on_chunk(&self, chunk: &AgentMessageChunk);
    fn on_tool_call(&self, _tool_call: &ToolCall) {}
    fn on_message(&self, _message: &Message) {}
}

#[allow(clippy::too_many_arguments)]
pub async fn agent_loop_stream(
    messages: &mut Vec<Message>,
    user_prompt: &str,
    system_prompt_template: Option<String>,
    agent_instructions: Option<String>,
    context_files: &HashMap<String, String>,
    current_working_dir: Option<String>,
    tool_registry: &HashMap<String, Box<dyn tools::Tool>>,
    provider: &dyn Provider,
    handler: &dyn AgentEventsHandler,
) -> Result<Message, Box<dyn std::error::Error>> {
    let tool_schemas: Vec<tools::ToolSchema> = tool_registry.values().map(|t| t.schema()).collect();

    if messages.is_empty() {
        let system_prompt = build_system_prompt(
            system_prompt_template.as_deref(),
            agent_instructions.as_deref(),
            context_files,
            current_working_dir.as_deref(),
        )?;
        let system_message = Message::System(system_prompt);
        handler.on_message(&system_message);
        messages.push(system_message);
    }
    messages.push(Message::User(user_prompt.to_string()));

    loop {
        let round_started_at_ms = chrono::Utc::now().timestamp_millis();
        let (mut result, finish_reason) = provider
            .complete_stream(messages.as_slice(), &tool_schemas, handler)
            .await?;

        if let Message::Assistant { at_ms, .. } = &mut result {
            *at_ms = Some(round_started_at_ms);
        }
        handler.on_message(&result);

        match finish_reason {
            FinishReason::Stop => {
                messages.push(result.clone());
                return Ok(result);
            }
            FinishReason::ToolCalls => {}
            FinishReason::Length => return Err("response truncated: token limit reached".into()),
            FinishReason::Unknown(s) => {
                eprintln!("unknown finish reason: {}", s);
                messages.push(result.clone());
                return Ok(result);
            }
        }

        let tool_calls = match &result {
            Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => tool_calls.clone(),
            _ => {
                messages.push(result.clone());
                return Ok(result);
            }
        };

        messages.push(result);
        for tc in tool_calls {
            handler.on_tool_call(&tc);
            let tool_started_at_ms = chrono::Utc::now().timestamp_millis();
            let tool_started = Instant::now();
            let tool_call_result = match tool_registry.get(&tc.name) {
                Some(tool) => tool.execute(&tc.arguments).await,
                None => Err(format!("tool not found: {}", tc.name)),
            };

            let tool_call_message = Message::Tool {
                call_id: tc.id.clone(),
                result: tool_call_result,
                duration_ms: Some(tool_started.elapsed().as_millis() as u64),
                at_ms: Some(tool_started_at_ms),
            };
            handler.on_message(&tool_call_message);
            messages.push(tool_call_message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// One scripted provider round: given the handler (so it can stream
    /// chunks first, like a real provider would), produce the round's final
    /// message and finish reason.
    type MockResponse =
        Box<dyn FnOnce(&dyn AgentEventsHandler) -> Result<(Message, FinishReason), String> + Send>;

    /// A provider stub that replays a fixed queue of responses, one per call,
    /// so tests can script exactly how the agent loop should react at each step.
    struct MockProvider {
        responses: Mutex<VecDeque<MockResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<MockResponse>) -> Self {
            MockProvider {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl Provider for MockProvider {
        async fn complete_stream(
            &self,
            _messages: &[Message],
            _tools: &[tools::ToolSchema],
            handler: &dyn AgentEventsHandler,
        ) -> Result<(Message, FinishReason), Box<dyn std::error::Error>> {
            let f = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockProvider called more times than scripted");
            f(handler).map_err(|e| e.into())
        }
    }

    /// Records every callback invocation so assertions can inspect what the
    /// agent loop reported without depending on stdout.
    #[derive(Default)]
    struct SpyHandler {
        tool_calls: Mutex<Vec<String>>,
        messages: Mutex<Vec<Message>>,
    }

    impl SpyHandler {
        /// The tool results announced via `on_message`, in order.
        fn tool_results(&self) -> Vec<Result<String, String>> {
            self.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(|m| match m {
                    Message::Tool { result, .. } => Some(result.clone()),
                    _ => None,
                })
                .collect()
        }

        /// The system prompts announced via `on_message`, in order.
        fn system_prompts(&self) -> Vec<String> {
            self.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(|m| match m {
                    Message::System(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    impl AgentEventsHandler for SpyHandler {
        fn on_chunk(&self, _chunk: &AgentMessageChunk) {}

        fn on_tool_call(&self, tool_call: &ToolCall) {
            self.tool_calls.lock().unwrap().push(tool_call.name.clone());
        }

        fn on_message(&self, message: &Message) {
            self.messages.lock().unwrap().push(message.clone());
        }
    }

    struct EchoTool;

    #[async_trait::async_trait(?Send)]
    impl tools::Tool for EchoTool {
        fn schema(&self) -> tools::ToolSchema {
            tools::ToolSchema {
                name: "echo".to_string(),
                description: "Echoes its arguments".to_string(),
                parameters: HashMap::new(),
            }
        }

        async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String> {
            Ok(format!("echoed:{:?}", args))
        }
    }

    fn assistant_text(text: &str) -> Message {
        Message::assistant(Some(text.to_string()), None, vec![])
    }

    #[test]
    fn build_system_prompt_includes_instructions_and_context() {
        let context_files = HashMap::from([("main.rs".to_string(), "fn main() {}".to_string())]);

        let prompt = build_system_prompt(
            None,
            Some("Be concise."),
            &context_files,
            Some("/home/user/project"),
        )
        .unwrap();

        assert!(prompt.contains("<agent-instructions>\nBe concise.\n</agent-instructions>"));
        assert!(prompt.contains("<file path=\"main.rs\">\nfn main() {}\n</file>"));
        assert!(prompt.contains("Current working directory: /home/user/project"));
    }

    #[test]
    fn build_system_prompt_omits_empty_sections() {
        let prompt = build_system_prompt(None, None, &HashMap::new(), None).unwrap();

        assert!(!prompt.contains("<agent-instructions>"));
        assert!(!prompt.contains("<context>"));
        assert!(!prompt.contains("Current working directory"));
    }

    #[test]
    fn build_system_prompt_uses_custom_template_when_provided() {
        let prompt = build_system_prompt(
            Some("Custom prompt. Instructions: {{ agent_instructions }}"),
            Some("be terse"),
            &HashMap::new(),
            None,
        )
        .unwrap();

        assert_eq!(prompt, "Custom prompt. Instructions: be terse");
    }

    #[test]
    fn build_system_prompt_errors_on_invalid_custom_template() {
        let err = build_system_prompt(Some("{% unknown_tag %}"), None, &HashMap::new(), None)
            .unwrap_err();

        assert!(!err.to_string().is_empty());
    }

    #[tokio::test]
    async fn agent_loop_stream_stops_on_first_response() {
        let provider = MockProvider::new(vec![Box::new(|_handler| {
            Ok((assistant_text("done"), FinishReason::Stop))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        match result {
            Message::Assistant { text, .. } => assert_eq!(text.as_deref(), Some("done")),
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn agent_loop_stream_reuses_history_on_follow_up_turn() {
        let provider = MockProvider::new(vec![
            Box::new(|_handler| Ok((assistant_text("first answer"), FinishReason::Stop))),
            Box::new(|_handler| Ok((assistant_text("second answer"), FinishReason::Stop))),
        ]);
        let handler = SpyHandler::default();
        let mut messages = Vec::new();

        agent_loop_stream(
            &mut messages,
            "first question",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        let result = agent_loop_stream(
            &mut messages,
            "second question",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        match result {
            Message::Assistant { text, .. } => assert_eq!(text.as_deref(), Some("second answer")),
            _ => panic!("expected assistant message"),
        }
        // System prompt is only built/announced once, on the first turn.
        assert_eq!(handler.system_prompts().len(), 1);
        // System + first user + first assistant + second user + second assistant.
        assert_eq!(messages.len(), 5);
        match &messages[1] {
            Message::User(text) => assert_eq!(text, "first question"),
            _ => panic!("expected first user message"),
        }
        match &messages[3] {
            Message::User(text) => assert_eq!(text, "second question"),
            _ => panic!("expected second user message"),
        }
    }

    #[tokio::test]
    async fn agent_loop_stream_reports_system_prompt_before_first_completion() {
        let provider = MockProvider::new(vec![Box::new(|_handler| {
            Ok((assistant_text("done"), FinishReason::Stop))
        })]);
        let handler = SpyHandler::default();

        agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            Some("Be concise.".to_string()),
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        let system_prompts = handler.system_prompts();
        assert_eq!(system_prompts.len(), 1);
        assert!(
            system_prompts[0].contains("<agent-instructions>\nBe concise.\n</agent-instructions>")
        );
    }

    #[tokio::test]
    async fn agent_loop_stream_executes_tool_call_then_stops() {
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            name: "echo".to_string(),
            arguments: HashMap::from([("msg".to_string(), "hi".to_string())]),
        };
        let provider = MockProvider::new(vec![
            Box::new(move |_handler| {
                Ok((
                    Message::assistant(None, None, vec![tool_call]),
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|_handler| Ok((assistant_text("final answer"), FinishReason::Stop))),
        ]);
        let handler = SpyHandler::default();
        let mut tool_registry: HashMap<String, Box<dyn tools::Tool>> = HashMap::new();
        tool_registry.insert("echo".to_string(), Box::new(EchoTool));

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &tool_registry,
            &provider,
            &handler,
        )
        .await
        .unwrap();

        match result {
            Message::Assistant { text, .. } => assert_eq!(text.as_deref(), Some("final answer")),
            _ => panic!("expected assistant message"),
        }
        assert_eq!(
            *handler.tool_calls.lock().unwrap(),
            vec!["echo".to_string()]
        );
        assert_eq!(
            handler.tool_results(),
            vec![Ok("echoed:{\"msg\": \"hi\"}".to_string())]
        );
    }

    #[tokio::test]
    async fn agent_loop_stream_reports_tool_not_found() {
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            name: "missing".to_string(),
            arguments: HashMap::new(),
        };
        let provider = MockProvider::new(vec![
            Box::new(move |_handler| {
                Ok((
                    Message::assistant(None, None, vec![tool_call]),
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|_handler| Ok((assistant_text("done"), FinishReason::Stop))),
        ]);
        let handler = SpyHandler::default();

        let _ = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        assert_eq!(
            handler.tool_results(),
            vec![Err("tool not found: missing".to_string())]
        );
    }

    #[tokio::test]
    async fn agent_loop_stream_errors_on_length_finish_reason() {
        let provider = MockProvider::new(vec![Box::new(|_handler| {
            Ok((assistant_text("truncated"), FinishReason::Length))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await;

        match result {
            Err(e) => assert_eq!(e.to_string(), "response truncated: token limit reached"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn agent_loop_stream_returns_on_unknown_finish_reason() {
        let provider = MockProvider::new(vec![Box::new(|_handler| {
            Ok((
                assistant_text("mystery"),
                FinishReason::Unknown("weird_reason".to_string()),
            ))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        match result {
            Message::Assistant { text, .. } => assert_eq!(text.as_deref(), Some("mystery")),
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn agent_loop_stream_returns_when_tool_calls_reason_but_no_tool_calls() {
        let provider = MockProvider::new(vec![Box::new(|_handler| {
            Ok((assistant_text("nothing to call"), FinishReason::ToolCalls))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        match result {
            Message::Assistant { text, .. } => assert_eq!(text.as_deref(), Some("nothing to call")),
            _ => panic!("expected assistant message"),
        }
        assert!(handler.tool_calls.lock().unwrap().is_empty());
    }

    /// Only implements the required `on_chunk` method, exercising the
    /// trait's default no-op bodies for the other callbacks.
    struct DefaultCallbacksHandler;

    impl AgentEventsHandler for DefaultCallbacksHandler {
        fn on_chunk(&self, _chunk: &AgentMessageChunk) {}
    }

    #[tokio::test]
    async fn agent_loop_stream_works_with_default_handler_callbacks() {
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            name: "echo".to_string(),
            arguments: HashMap::new(),
        };
        let provider = MockProvider::new(vec![
            Box::new(move |_handler| {
                Ok((
                    Message::assistant(None, None, vec![tool_call]),
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|_handler| Ok((assistant_text("final answer"), FinishReason::Stop))),
        ]);
        let handler = DefaultCallbacksHandler;
        let mut tool_registry: HashMap<String, Box<dyn tools::Tool>> = HashMap::new();
        tool_registry.insert("echo".to_string(), Box::new(EchoTool));

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &tool_registry,
            &provider,
            &handler,
        )
        .await
        .unwrap();

        match result {
            Message::Assistant { text, .. } => assert_eq!(text.as_deref(), Some("final answer")),
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn agent_loop_stream_propagates_provider_error() {
        let provider = MockProvider::new(vec![Box::new(|_handler| Err("connection refused".to_string()))]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await;

        match result {
            Err(e) => assert_eq!(e.to_string(), "connection refused"),
            Ok(_) => panic!("expected error"),
        }
    }

    // --- Observability metadata: every finalized message tells the user
    // when it happened, how long it took, and what it cost in tokens.
    // (Reasoning/response phase timing is measured while accumulating the
    // stream and tested next to that code — see openai_wire.rs.) ---

    #[tokio::test]
    async fn assistant_message_is_stamped_with_the_time_its_round_started() {
        let before = chrono::Utc::now().timestamp_millis();
        let provider = MockProvider::new(vec![Box::new(|_handler| {
            Ok((assistant_text("done"), FinishReason::Stop))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();
        let after = chrono::Utc::now().timestamp_millis();

        match result {
            Message::Assistant { at_ms, .. } => {
                let at_ms = at_ms.expect("round start time should be stamped");
                assert!(at_ms >= before && at_ms <= after);
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn provider_reported_usage_stays_on_the_assistant_message() {
        // The provider embeds usage while accumulating the stream (see
        // ChatStreamAccumulator::finish); the loop must pass it through
        // untouched so it lands in history.
        let provider = MockProvider::new(vec![Box::new(|_handler| {
            let message = Message::Assistant {
                text: Some("done".to_string()),
                reasoning: None,
                tool_calls: vec![],
                reasoning_duration_ms: None,
                response_duration_ms: None,
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
                at_ms: None,
            };
            Ok((message, FinishReason::Stop))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &HashMap::new(),
            &provider,
            &handler,
        )
        .await
        .unwrap();

        match result {
            Message::Assistant { usage, .. } => {
                let usage = usage.expect("usage should survive the loop");
                assert_eq!(usage.total_tokens, 15);
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn tool_result_message_records_execution_duration_and_start_time() {
        /// A tool slow enough that its measured duration can't be zero.
        struct SlowTool;

        #[async_trait::async_trait(?Send)]
        impl tools::Tool for SlowTool {
            fn schema(&self) -> tools::ToolSchema {
                tools::ToolSchema {
                    name: "slow".to_string(),
                    description: "Takes its time".to_string(),
                    parameters: HashMap::new(),
                }
            }

            async fn execute(&self, _args: &HashMap<String, String>) -> Result<String, String> {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Ok("finally".to_string())
            }
        }

        let tool_call = ToolCall {
            id: "call-1".to_string(),
            name: "slow".to_string(),
            arguments: HashMap::new(),
        };
        let provider = MockProvider::new(vec![
            Box::new(move |_handler: &dyn AgentEventsHandler| {
                Ok((
                    Message::assistant(None, None, vec![tool_call]),
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|_handler| Ok((assistant_text("done"), FinishReason::Stop))),
        ]);
        let handler = SpyHandler::default();
        let mut tool_registry: HashMap<String, Box<dyn tools::Tool>> = HashMap::new();
        tool_registry.insert("slow".to_string(), Box::new(SlowTool));

        let before = chrono::Utc::now().timestamp_millis();
        let mut messages = Vec::new();
        agent_loop_stream(
            &mut messages,
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &tool_registry,
            &provider,
            &handler,
        )
        .await
        .unwrap();
        let after = chrono::Utc::now().timestamp_millis();

        let tool_message = messages
            .iter()
            .find(|m| matches!(m, Message::Tool { .. }))
            .expect("history should contain the tool result");
        match tool_message {
            Message::Tool {
                duration_ms, at_ms, ..
            } => {
                assert!(duration_ms.expect("tool execution should be timed") >= 10);
                let at_ms = at_ms.expect("tool start time should be stamped");
                assert!(at_ms >= before && at_ms <= after);
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn every_finalized_message_is_announced_in_order() {
        // A first tool-calling turn produces four finalized messages: the
        // rendered system prompt, the assistant round requesting the tool,
        // the tool's result, and the final assistant answer. `on_message`
        // must report each one, in that order, so a UI can render the turn
        // as it unfolds. (The user prompt is never announced — the caller
        // wrote it themselves.)
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            name: "echo".to_string(),
            arguments: HashMap::new(),
        };
        let provider = MockProvider::new(vec![
            Box::new(move |_handler: &dyn AgentEventsHandler| {
                Ok((
                    Message::assistant(None, None, vec![tool_call]),
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|_handler| Ok((assistant_text("final answer"), FinishReason::Stop))),
        ]);
        let handler = SpyHandler::default();
        let mut tool_registry: HashMap<String, Box<dyn tools::Tool>> = HashMap::new();
        tool_registry.insert("echo".to_string(), Box::new(EchoTool));

        agent_loop_stream(
            &mut Vec::new(),
            "hello",
            None,
            None,
            &HashMap::new(),
            None,
            &tool_registry,
            &provider,
            &handler,
        )
        .await
        .unwrap();

        let announced = handler.messages.lock().unwrap();
        assert_eq!(announced.len(), 4);
        assert!(matches!(&announced[0], Message::System(_)));
        assert!(matches!(
            &announced[1],
            Message::Assistant { tool_calls, .. } if tool_calls.len() == 1
        ));
        assert!(matches!(&announced[2], Message::Tool { .. }));
        assert!(matches!(
            &announced[3],
            Message::Assistant { text: Some(t), .. } if t == "final answer"
        ));
    }

    #[test]
    fn history_saved_before_metadata_existed_still_loads() {
        // Sessions persisted by earlier versions have no timing/usage
        // fields at all — they must deserialize with the metadata absent,
        // not fail.
        let old_assistant = r#"{"Assistant":{"text":"hi","reasoning":null,"tool_calls":[]}}"#;
        let old_tool = r#"{"Tool":{"call_id":"call-1","result":{"Ok":"output"}}}"#;

        let assistant: Message = serde_json::from_str(old_assistant).unwrap();
        match assistant {
            Message::Assistant {
                reasoning_duration_ms,
                response_duration_ms,
                usage,
                at_ms,
                ..
            } => {
                assert_eq!(reasoning_duration_ms, None);
                assert_eq!(response_duration_ms, None);
                assert!(usage.is_none());
                assert_eq!(at_ms, None);
            }
            _ => panic!("expected assistant message"),
        }

        let tool: Message = serde_json::from_str(old_tool).unwrap();
        match tool {
            Message::Tool {
                duration_ms, at_ms, ..
            } => {
                assert_eq!(duration_ms, None);
                assert_eq!(at_ms, None);
            }
            _ => panic!("expected tool message"),
        }
    }
}
