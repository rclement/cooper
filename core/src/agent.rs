use std::collections::HashMap;

use askama::Template;

use crate::providers::Provider;
use crate::tools;

/// ```askama
/// You are agent Cooper, a special AI agent harness.
///
/// {%- if let Some(agent_instructions) = agent_instructions %}
/// <agent-instructions>
/// {{ agent_instructions }}
/// </agent-instructions>
/// {%- endif %}
///
/// {%- if !context_files.is_empty() %}
/// <context>
/// {%- for (path, content) in context_files %}
/// <file path="{{ path }}">
/// {{ content }}
/// </file>
/// {%- endfor %}
/// </context>
/// {%- endif %}
///
/// Current date: {{ current_date }}
/// Current time: {{ current_time }}
/// {%- if let Some(current_working_dir) = current_working_dir %}
/// Current working directory: {{ current_working_dir }}
/// {%- endif %}
/// ```
#[derive(askama::Template)]
#[template(ext = "txt", in_doc = true)]
struct SystemPromptTemplate {
    agent_instructions: Option<String>,
    context_files: HashMap<String, String>,
    current_date: String,
    current_time: String,
    current_working_dir: Option<String>,
}

/// `current_working_dir` is caller-supplied rather than resolved here, since
/// the notion of a working directory doesn't exist in every environment this
/// core runs in (e.g. a browser tab has no filesystem/cwd).
fn build_system_prompt(
    agent_instructions: Option<String>,
    context_files: &HashMap<String, String>,
    current_working_dir: Option<String>,
) -> Result<String, askama::Error> {
    let now = chrono::Local::now();
    let template = SystemPromptTemplate {
        agent_instructions,
        context_files: context_files.clone(),
        current_date: now.format("%Y-%m-%d").to_string(),
        current_time: now.format("%H:%M:%S %z").to_string(),
        current_working_dir,
    };
    template.render()
}

#[derive(Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: HashMap<String, String>,
}

pub enum Message {
    System(String),
    User(String),
    Assistant {
        text: Option<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        result: Result<String, String>,
    },
}

pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Unknown(String),
}

pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

pub struct AgentMessageChunk {
    pub text: Option<String>,
    pub reasoning: Option<String>,
}

/// Not `Send + Sync`: a browser `Provider`/handler may wrap JS-bound values
/// (e.g. a callback `js_sys::Function`), which are single-threaded only.
pub trait AgentEventsHandler {
    fn on_chunk(&self, chunk: &AgentMessageChunk);
    fn on_complete(&self, _usage: &Usage) {}
    fn on_tool_call(&self, _tool_call: &ToolCall) {}
    fn on_tool_result(&self, _tool_result: &Result<String, String>) {}
}

pub async fn agent_loop_stream(
    user_prompt: &str,
    agent_instructions: Option<String>,
    context_files: &HashMap<String, String>,
    current_working_dir: Option<String>,
    tool_registry: &HashMap<String, Box<dyn tools::Tool>>,
    provider: &dyn Provider,
    handler: &dyn AgentEventsHandler,
) -> Result<Message, Box<dyn std::error::Error>> {
    let tool_schemas: Vec<tools::ToolSchema> = tool_registry.values().map(|t| t.schema()).collect();

    let system_prompt =
        build_system_prompt(agent_instructions, context_files, current_working_dir)?;
    let mut messages = vec![
        Message::System(system_prompt),
        Message::User(user_prompt.to_string()),
    ];

    loop {
        let (result, finish_reason) = provider
            .complete_stream(&messages, &tool_schemas, handler)
            .await?;

        match finish_reason {
            FinishReason::Stop => return Ok(result),
            FinishReason::ToolCalls => {}
            FinishReason::Length => return Err("response truncated: token limit reached".into()),
            FinishReason::Unknown(s) => {
                eprintln!("unknown finish reason: {}", s);
                return Ok(result);
            }
        }

        let tool_calls = match &result {
            Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => tool_calls.clone(),
            _ => return Ok(result),
        };

        messages.push(result);
        for tc in tool_calls {
            handler.on_tool_call(&tc);
            let tool_call_result = match tool_registry.get(&tc.name) {
                Some(tool) => tool.execute(&tc.arguments).await,
                None => Err(format!("tool not found: {}", tc.name)),
            };
            handler.on_tool_result(&tool_call_result);

            let tool_call_message = Message::Tool {
                call_id: tc.id.clone(),
                result: tool_call_result,
            };
            messages.push(tool_call_message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    type MockResponse = Box<dyn FnOnce() -> Result<(Message, FinishReason), String> + Send>;

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
            handler.on_complete(&Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });
            let f = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockProvider called more times than scripted");
            f().map_err(|e| e.into())
        }
    }

    /// Records every callback invocation so assertions can inspect what the
    /// agent loop reported without depending on stdout.
    #[derive(Default)]
    struct SpyHandler {
        tool_calls: Mutex<Vec<String>>,
        tool_results: Mutex<Vec<Result<String, String>>>,
        completes: Mutex<u32>,
    }

    impl AgentEventsHandler for SpyHandler {
        fn on_chunk(&self, _chunk: &AgentMessageChunk) {}

        fn on_complete(&self, _usage: &Usage) {
            *self.completes.lock().unwrap() += 1;
        }

        fn on_tool_call(&self, tool_call: &ToolCall) {
            self.tool_calls.lock().unwrap().push(tool_call.name.clone());
        }

        fn on_tool_result(&self, tool_result: &Result<String, String>) {
            self.tool_results.lock().unwrap().push(tool_result.clone());
        }
    }

    struct EchoTool;

    #[async_trait::async_trait]
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
        Message::Assistant {
            text: Some(text.to_string()),
            reasoning: None,
            tool_calls: vec![],
        }
    }

    #[test]
    fn build_system_prompt_includes_instructions_and_context() {
        let context_files = HashMap::from([("main.rs".to_string(), "fn main() {}".to_string())]);

        let prompt = build_system_prompt(
            Some("Be concise.".to_string()),
            &context_files,
            Some("/home/user/project".to_string()),
        )
        .unwrap();

        assert!(prompt.contains("<agent-instructions>\nBe concise.\n</agent-instructions>"));
        assert!(prompt.contains("<file path=\"main.rs\">\nfn main() {}\n</file>"));
        assert!(prompt.contains("Current working directory: /home/user/project"));
    }

    #[test]
    fn build_system_prompt_omits_empty_sections() {
        let prompt = build_system_prompt(None, &HashMap::new(), None).unwrap();

        assert!(!prompt.contains("<agent-instructions>"));
        assert!(!prompt.contains("<context>"));
        assert!(!prompt.contains("Current working directory"));
    }

    #[tokio::test]
    async fn agent_loop_stream_stops_on_first_response() {
        let provider = MockProvider::new(vec![Box::new(|| {
            Ok((assistant_text("done"), FinishReason::Stop))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            "hello",
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
    async fn agent_loop_stream_executes_tool_call_then_stops() {
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            name: "echo".to_string(),
            arguments: HashMap::from([("msg".to_string(), "hi".to_string())]),
        };
        let provider = MockProvider::new(vec![
            Box::new(move || {
                Ok((
                    Message::Assistant {
                        text: None,
                        reasoning: None,
                        tool_calls: vec![tool_call],
                    },
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|| Ok((assistant_text("final answer"), FinishReason::Stop))),
        ]);
        let handler = SpyHandler::default();
        let mut tool_registry: HashMap<String, Box<dyn tools::Tool>> = HashMap::new();
        tool_registry.insert("echo".to_string(), Box::new(EchoTool));

        let result = agent_loop_stream(
            "hello",
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
            *handler.tool_results.lock().unwrap(),
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
            Box::new(move || {
                Ok((
                    Message::Assistant {
                        text: None,
                        reasoning: None,
                        tool_calls: vec![tool_call],
                    },
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|| Ok((assistant_text("done"), FinishReason::Stop))),
        ]);
        let handler = SpyHandler::default();

        let _ = agent_loop_stream(
            "hello",
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
            *handler.tool_results.lock().unwrap(),
            vec![Err("tool not found: missing".to_string())]
        );
    }

    #[tokio::test]
    async fn agent_loop_stream_errors_on_length_finish_reason() {
        let provider = MockProvider::new(vec![Box::new(|| {
            Ok((assistant_text("truncated"), FinishReason::Length))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            "hello",
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
        let provider = MockProvider::new(vec![Box::new(|| {
            Ok((
                assistant_text("mystery"),
                FinishReason::Unknown("weird_reason".to_string()),
            ))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            "hello",
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
        let provider = MockProvider::new(vec![Box::new(|| {
            Ok((assistant_text("nothing to call"), FinishReason::ToolCalls))
        })]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            "hello",
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
            Box::new(move || {
                Ok((
                    Message::Assistant {
                        text: None,
                        reasoning: None,
                        tool_calls: vec![tool_call],
                    },
                    FinishReason::ToolCalls,
                ))
            }),
            Box::new(|| Ok((assistant_text("final answer"), FinishReason::Stop))),
        ]);
        let handler = DefaultCallbacksHandler;
        let mut tool_registry: HashMap<String, Box<dyn tools::Tool>> = HashMap::new();
        tool_registry.insert("echo".to_string(), Box::new(EchoTool));

        let result = agent_loop_stream(
            "hello",
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
        let provider = MockProvider::new(vec![Box::new(|| Err("connection refused".to_string()))]);
        let handler = SpyHandler::default();

        let result = agent_loop_stream(
            "hello",
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
}
