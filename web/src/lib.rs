use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cooper_core::agent::{self, AgentEventsHandler, AgentMessageChunk, Message, ToolCall, Usage};
use cooper_core::providers::openai_completions::OpenAICompletionsAPI;
use cooper_core::tools::{Tool, ToolSchema};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[derive(Deserialize)]
struct ContextFile {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct AgentConfig {
    base_url: String,
    api_key: String,
    model: String,
    #[serde(default)]
    system_prompt_template: Option<String>,
    #[serde(default)]
    agent_instructions: Option<String>,
    #[serde(default)]
    context_files: Vec<ContextFile>,
}

/// A tool registered from JS: `schema` describes it to the model, and
/// `execute_fn(args_json) -> Promise<string>` runs it. `js_sys::Function` is
/// cheap to clone (it's a ref-counted JS object handle), so each `run_prompt`
/// call can build its own `Box<dyn Tool>` registry from these without
/// disturbing the ones stored on `WasmAgent`.
#[derive(Clone)]
struct RegisteredTool {
    schema: ToolSchema,
    execute_fn: js_sys::Function,
}

/// Bridges the core's `Tool` trait to a JS-implemented tool. This is the
/// mechanism for *any* dynamically-registered tool, not just built-ins like
/// `fetch_url` — a future Python/Pyodide tool would just be a JS `execute_fn`
/// that calls into the Pyodide runtime.
struct JsTool {
    schema: ToolSchema,
    execute_fn: js_sys::Function,
}

#[async_trait::async_trait(?Send)]
impl Tool for JsTool {
    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String> {
        let args_json = serde_json::to_string(args).map_err(|e| e.to_string())?;
        let promise = self
            .execute_fn
            .call1(&JsValue::NULL, &JsValue::from_str(&args_json))
            .map_err(|e| js_error_to_string(&e))?;
        let promise: js_sys::Promise = promise
            .dyn_into()
            .map_err(|_| "tool did not return a promise".to_string())?;
        let result = wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .map_err(|e| js_error_to_string(&e))?;
        result
            .as_string()
            .ok_or_else(|| "tool did not resolve to a string".to_string())
    }
}

fn js_error_to_string(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(value, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

/// Browser-side Cooper agent. Constructed with a JSON config object; tools
/// are registered separately via `register_tool` since they carry a JS
/// callback, which isn't representable in the JSON config.
///
/// Holds the conversation history across `run_prompt` calls (in an
/// `Rc<RefCell<_>>` since `run_prompt` only borrows `&self`, but needs to
/// mutate history from inside the async block it returns), so successive
/// calls are follow-up turns in the same session rather than one-shot runs.
/// Call `reset` to start a fresh session with the same config/tools.
#[wasm_bindgen]
pub struct WasmAgent {
    config: AgentConfig,
    tools: Vec<RegisteredTool>,
    messages: Rc<RefCell<Vec<Message>>>,
}

#[wasm_bindgen]
impl WasmAgent {
    /// `config_json` fields: `base_url`, `api_key`, `model`, and optionally
    /// `system_prompt_template`, `agent_instructions`, and `context_files`
    /// (`[{ path, content }]`).
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str) -> Result<WasmAgent, JsValue> {
        let config: AgentConfig =
            serde_json::from_str(config_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(WasmAgent {
            config,
            tools: Vec::new(),
            messages: Rc::new(RefCell::new(Vec::new())),
        })
    }

    /// Clears the conversation history, so the next `run_prompt` call starts
    /// a brand-new session (fresh system prompt) instead of continuing the
    /// previous one.
    pub fn reset(&self) {
        self.messages.borrow_mut().clear();
    }

    /// Registers a tool available for the agent to call. `schema_json` must
    /// match `ToolSchema`'s JSON shape: `{ name, description, parameters:
    /// { <param>: { type, description, required } } }`. `execute_fn` is
    /// called with a JSON object of string arguments and must return (a
    /// promise resolving to) a string result; throwing/rejecting reports a
    /// tool error back to the agent.
    pub fn register_tool(
        &mut self,
        schema_json: &str,
        execute_fn: js_sys::Function,
    ) -> Result<(), JsValue> {
        let schema: ToolSchema =
            serde_json::from_str(schema_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.tools.push(RegisteredTool { schema, execute_fn });
        Ok(())
    }

    /// Runs one agentic-loop turn for `prompt`. `on_event` is called with a
    /// JSON string for each streamed event (see `EventDto`). The returned
    /// promise resolves with the final assistant message as a JSON string
    /// (see `MessageDto`), or rejects with an error string.
    pub fn run_prompt(&self, prompt: String, on_event: js_sys::Function) -> js_sys::Promise {
        let provider = OpenAICompletionsAPI::new(
            &self.config.base_url,
            &self.config.api_key,
            &self.config.model,
        );
        let system_prompt_template = self.config.system_prompt_template.clone();
        let agent_instructions = self.config.agent_instructions.clone();
        let context_files: HashMap<String, String> = self
            .config
            .context_files
            .iter()
            .map(|f| (f.path.clone(), f.content.clone()))
            .collect();
        let tool_registry: HashMap<String, Box<dyn Tool>> = self
            .tools
            .iter()
            .cloned()
            .map(|t| {
                let name = t.schema.name.clone();
                let tool: Box<dyn Tool> = Box::new(JsTool {
                    schema: t.schema,
                    execute_fn: t.execute_fn,
                });
                (name, tool)
            })
            .collect();
        let handler = JsEventHandler { on_event };
        let messages_state = Rc::clone(&self.messages);

        wasm_bindgen_futures::future_to_promise(async move {
            // Taken out (rather than borrowed across the `.await`s below) and
            // put back after, so a mid-turn error still leaves whatever
            // history was produced so far in place for the next call.
            let mut messages = messages_state.take();
            let result = agent::agent_loop_stream(
                &mut messages,
                &prompt,
                system_prompt_template,
                agent_instructions,
                &context_files,
                None,
                &tool_registry,
                &provider,
                &handler,
            )
            .await;
            messages_state.replace(messages);

            let result = result.map_err(|e| JsValue::from_str(&e.to_string()))?;

            serde_json::to_string(&MessageDto::from(&result))
                .map(|s| JsValue::from_str(&s))
                .map_err(|e| JsValue::from_str(&e.to_string()))
        })
    }
}

/// Forwards agent loop callbacks to a JS function as JSON-encoded `EventDto`s.
struct JsEventHandler {
    on_event: js_sys::Function,
}

impl JsEventHandler {
    fn emit(&self, event: &EventDto) {
        if let Ok(json) = serde_json::to_string(event) {
            let _ = self
                .on_event
                .call1(&JsValue::NULL, &JsValue::from_str(&json));
        }
    }
}

impl AgentEventsHandler for JsEventHandler {
    fn on_chunk(&self, chunk: &AgentMessageChunk) {
        self.emit(&EventDto::Chunk {
            text: chunk.text.clone(),
            reasoning: chunk.reasoning.clone(),
        });
    }

    fn on_complete(&self, usage: &Usage) {
        self.emit(&EventDto::Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        });
    }

    fn on_tool_call(&self, tool_call: &ToolCall) {
        self.emit(&EventDto::ToolCall {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: tool_call.arguments.clone(),
        });
    }

    fn on_tool_result(&self, tool_result: &Result<String, String>) {
        self.emit(&EventDto::ToolResult {
            result: tool_result.clone(),
        });
    }

    fn on_system_prompt(&self, system_prompt: &str) {
        self.emit(&EventDto::SystemPrompt {
            text: system_prompt.to_string(),
        });
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventDto {
    Chunk {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: HashMap<String, String>,
    },
    ToolResult {
        result: Result<String, String>,
    },
    SystemPrompt {
        text: String,
    },
}

#[derive(Serialize)]
struct MessageDto {
    text: Option<String>,
    reasoning: Option<String>,
}

impl From<&Message> for MessageDto {
    fn from(m: &Message) -> Self {
        match m {
            Message::Assistant {
                text, reasoning, ..
            } => MessageDto {
                text: text.clone(),
                reasoning: reasoning.clone(),
            },
            _ => MessageDto {
                text: None,
                reasoning: None,
            },
        }
    }
}
