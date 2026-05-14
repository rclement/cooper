use crate::provider;
use crate::types::{Message, OutputChunk, Role};
use anyhow::{Result, anyhow};

#[allow(async_fn_in_trait)]
pub trait ToolExecutor {
    fn schemas(&self) -> Vec<serde_json::Value>;
    async fn execute(&self, name: &str, args_json: &str) -> Result<String>;
}

/// Per-turn hooks for session logging. Implement in the shell; pass `None` to skip.
pub trait SessionLogger {
    fn on_request(&mut self, messages: &[Message]);
    fn on_response(&mut self, thinking: Option<&str>, message: &Message);
}

const MAX_TURNS: usize = 20;

/// Run one agentic turn against an existing message history.
/// Appends the assistant response (and any intermediate tool calls) to `messages`.
/// The caller must push the user message before calling this.
pub async fn run_turn(
    messages: &mut Vec<Message>,
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

        let response = provider::call(
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
            );
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

pub async fn run(
    prompt: String,
    system_prompt: String,
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
    run_turn(&mut messages, base_url, api_key, model, executor, logger, on_chunk).await
}
