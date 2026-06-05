use std::collections::HashMap;

use askama::Template;

use crate::providers::Provider;
use crate::tools;

/// === system prompt === ///

/// ```askama
/// You are agent Cooper, a special AI agent harness.
///
/// Current date: {{ current_date }}
/// Current time: {{ current_time }}
/// Current working directory: {{ current_working_dir }}
/// ```
#[derive(askama::Template)]
#[template(ext = "txt", in_doc = true)]
struct SystemPromptTemplate {
    current_date: String,
    current_time: String,
    current_working_dir: String,
}

fn build_system_prompt() -> Result<String, askama::Error> {
    let now = chrono::Local::now();
    let template = SystemPromptTemplate {
        current_date: now.format("%Y-%m-%d").to_string(),
        current_time: now.format("%H:%M:%S %z").to_string(),
        current_working_dir: std::env::current_dir()?.display().to_string(),
    };
    template.render()
}

/// === agent message types === ///

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

/// === agent event handler === ///

pub struct DeltaChunk {
    pub text: Option<String>,
    pub reasoning: Option<String>,
}

pub trait ChunkHandler: Send + Sync {
    fn on_chunk(&self, chunk: &DeltaChunk);
    fn on_complete(&self, _usage: &Usage) {}
    fn on_tool_call(&self, _tool_call: &ToolCall) {}
    fn on_tool_result(&self, _tool_result: &Result<String, String>) {}
}

/// === agentic loop with tool calling (streaming) === ///

pub async fn agent_loop_stream(
    user_prompt: &str,
    tool_registry: &HashMap<String, Box<dyn tools::Tool>>,
    provider: &dyn Provider,
    handler: &dyn ChunkHandler,
) -> Result<Message, Box<dyn std::error::Error>> {
    let tool_schemas: Vec<tools::ToolSchema> = tool_registry.values().map(|t| t.schema()).collect();

    let system_prompt = build_system_prompt()?;
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
