use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cooper_core::agent::{
    self, AgentEventsHandler, AgentMessageChunk, FinishReason, Message, ToolCall,
};
use cooper_core::providers::Provider;
use cooper_core::providers::openai_completions::OpenAICompletionsAPI;
use cooper_core::providers::openai_wire::{
    ApiCompletionRequest, ApiMessage, ApiStreamChunk, ApiTool, ChatStreamAccumulator,
};
use cooper_core::tools::{Tool, ToolSchema};
use futures_util::{FutureExt, StreamExt};
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
    /// Unused (and may be omitted) when a completion bridge is set — the
    /// local provider has no endpoint to talk to.
    #[serde(default)]
    base_url: String,
    #[serde(default)]
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

/// `Provider` backed by a JS completion function instead of HTTP — the
/// in-browser local-model path (wllama). The JS side is called as
/// `complete_fn(request_json, on_chunk) -> Promise<void>`, where
/// `request_json` is a standard OpenAI `chat.completions` request (wllama's
/// `createChatCompletion` accepts exactly this shape) and `on_chunk` must be
/// invoked with each streamed `chat.completion.chunk` as a JSON string.
///
/// Chunks flow through an unbounded channel because the `on_chunk` closure
/// handed to JS must be `'static`, while `handler` is a borrow — so the
/// closure only forwards strings, and this method drains them concurrently
/// with awaiting the completion promise (single-threaded wasm: chunks only
/// arrive while suspended at an `.await`, so nothing is lost).
struct JsBridgeProvider {
    model: String,
    complete_fn: js_sys::Function,
}

#[async_trait::async_trait(?Send)]
impl Provider for JsBridgeProvider {
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
            stream_options: None,
        };
        let request_json = serde_json::to_string(&request)?;

        let (tx, mut rx) = futures_channel::mpsc::unbounded::<String>();
        let on_chunk = Closure::wrap(Box::new(move |chunk: JsValue| {
            if let Some(json) = chunk.as_string() {
                let _ = tx.unbounded_send(json);
            }
        }) as Box<dyn FnMut(JsValue)>);

        let promise = self
            .complete_fn
            .call2(
                &JsValue::NULL,
                &JsValue::from_str(&request_json),
                on_chunk.as_ref().unchecked_ref(),
            )
            .map_err(|e| js_error_to_string(&e))?;
        let promise: js_sys::Promise = promise
            .dyn_into()
            .map_err(|_| "local provider bridge did not return a promise".to_string())?;

        let mut acc = ChatStreamAccumulator::new();
        let done = wasm_bindgen_futures::JsFuture::from(promise).fuse();
        futures_util::pin_mut!(done);
        loop {
            futures_util::select! {
                result = done => {
                    result.map_err(|e| js_error_to_string(&e))?;
                    break;
                }
                chunk_json = rx.next() => {
                    match chunk_json {
                        Some(json) => acc.push(&serde_json::from_str::<ApiStreamChunk>(&json)?, handler),
                        None => break,
                    }
                }
            }
        }
        // The promise can resolve with chunks still queued; flush them. The
        // closure (and with it the sender) is dropped only after this, so
        // `try_next` sees every chunk JS managed to emit.
        while let Ok(json) = rx.try_recv() {
            acc.push(&serde_json::from_str::<ApiStreamChunk>(&json)?, handler);
        }
        drop(on_chunk);

        acc.finish()
    }
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
    completion_bridge: Option<js_sys::Function>,
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
            completion_bridge: None,
        })
    }

    /// Routes completions through a JS function instead of the built-in
    /// OpenAI HTTP provider — the local (in-browser model) path. See
    /// `JsBridgeProvider` for the function's contract.
    pub fn set_completion_bridge(&mut self, complete_fn: js_sys::Function) {
        self.completion_bridge = Some(complete_fn);
    }

    /// Clears the conversation history, so the next `run_prompt` call starts
    /// a brand-new session (fresh system prompt) instead of continuing the
    /// previous one.
    pub fn reset(&self) {
        self.messages.borrow_mut().clear();
    }

    /// Snapshots the conversation history as JSON, so the caller can persist
    /// it (e.g. to IndexedDB) and later restore it with `import_history` to
    /// resume this session — even across a page reload, where this
    /// `WasmAgent` instance itself doesn't survive.
    pub fn export_history(&self) -> Result<String, JsValue> {
        serde_json::to_string(&*self.messages.borrow())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Replaces the conversation history with a previously exported
    /// snapshot, so the next `run_prompt` call continues that conversation
    /// instead of starting a new one.
    pub fn import_history(&self, history_json: &str) -> Result<(), JsValue> {
        let messages: Vec<Message> =
            serde_json::from_str(history_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        *self.messages.borrow_mut() = messages;
        Ok(())
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
        let provider: Box<dyn Provider> = match &self.completion_bridge {
            Some(complete_fn) => Box::new(JsBridgeProvider {
                model: self.config.model.clone(),
                complete_fn: complete_fn.clone(),
            }),
            None => Box::new(OpenAICompletionsAPI::new(
                &self.config.base_url,
                &self.config.api_key,
                &self.config.model,
            )),
        };
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
                provider.as_ref(),
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

    fn on_tool_call(&self, tool_call: &ToolCall) {
        self.emit(&EventDto::ToolCall {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: tool_call.arguments.clone(),
        });
    }

    fn on_message(&self, message: &Message) {
        self.emit(&EventDto::Message {
            message: message.clone(),
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
    ToolCall {
        id: String,
        name: String,
        arguments: HashMap<String, String>,
    },
    /// A finalized message, in the exact same JSON shape as one entry of the
    /// exported history — timings, usage and timestamp included — so the UI
    /// renders live events and replayed history through one code path.
    Message {
        message: Message,
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
