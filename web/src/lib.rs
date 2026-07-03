use std::collections::HashMap;

use cooper_core::agent::{self, AgentEventsHandler, AgentMessageChunk, Message, ToolCall, Usage};
use cooper_core::providers::openai_completions::OpenAICompletionsAPI;
use cooper_core::tools::Tool;
use serde::{Deserialize, Serialize};
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

/// Browser-side Cooper agent. Constructed with a JSON config object; there are
/// no built-in tools yet (fs/process tools are native-only, browser-safe
/// tools land in a later step) so the loop always runs tool-free for now.
#[wasm_bindgen]
pub struct WasmAgent {
    config: AgentConfig,
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
        Ok(WasmAgent { config })
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
        let tool_registry: HashMap<String, Box<dyn Tool>> = HashMap::new();
        let handler = JsEventHandler { on_event };

        wasm_bindgen_futures::future_to_promise(async move {
            let result = agent::agent_loop_stream(
                &prompt,
                system_prompt_template,
                agent_instructions,
                &context_files,
                None,
                &tool_registry,
                &provider,
                &handler,
            )
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

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
