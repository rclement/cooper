use cooper_core::{ApiType, OutputChunk};
use wasm_bindgen::prelude::*;

mod agent;
mod session_logger;
mod tools;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Browser-side Cooper agent. Constructed with a JSON config object and exposes
/// `run_prompt` which returns a JS Promise and streams chunks via a callback.
#[wasm_bindgen]
pub struct CooperAgent {
    api_type: ApiType,
    base_url: String,
    api_key: String,
    model: String,
    system_prompt: String,
}

#[wasm_bindgen]
impl CooperAgent {
    /// `config_json` fields: `base_url`, `api_key`, `model`, `system_prompt`,
    /// and optionally `api` (`"openai-completions"` or `"anthropic-messages"`).
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str) -> Result<CooperAgent, JsValue> {
        let cfg: serde_json::Value =
            serde_json::from_str(config_json).map_err(|e| JsValue::from_str(&e.to_string()))?;

        let api_type = cfg["api"]
            .as_str()
            .and_then(|s| s.parse::<ApiType>().ok())
            .unwrap_or_default();

        Ok(CooperAgent {
            api_type,
            base_url: cfg["base_url"].as_str().unwrap_or("").to_string(),
            api_key: cfg["api_key"].as_str().unwrap_or("").to_string(),
            model: cfg["model"].as_str().unwrap_or("").to_string(),
            system_prompt: cfg["system_prompt"]
                .as_str()
                .unwrap_or("You are a helpful AI assistant.")
                .to_string(),
        })
    }

    /// Runs the agentic loop for `prompt`.
    ///
    /// `on_chunk` is called with a JSON string for each output event:
    ///   `{"type":"content","text":"..."}` — streamed assistant text
    ///   `{"type":"thinking","text":"..."}` — reasoning/thinking tokens
    ///   `{"type":"tool_call","name":"...","args":"..."}` — tool invocation
    ///   `{"type":"tool_result","name":"...","output":"..."}` — tool output
    ///
    /// Returns a Promise that resolves to the final assistant text, or rejects
    /// with an error string.
    pub fn run_prompt(&self, prompt: String, on_chunk: js_sys::Function) -> js_sys::Promise {
        let api_type = self.api_type.clone();
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let system_prompt = self.system_prompt.clone();

        wasm_bindgen_futures::future_to_promise(async move {
            let mut cb = |chunk: OutputChunk| {
                let json = serde_json::to_string(&chunk).unwrap_or_default();
                let _ = on_chunk.call1(&JsValue::UNDEFINED, &JsValue::from_str(&json));
            };

            agent::run(
                prompt,
                system_prompt,
                api_type,
                base_url,
                api_key,
                model,
                &mut cb,
            )
            .await
            .map(|s| JsValue::from_str(&s))
            .map_err(|e| JsValue::from_str(&e.to_string()))
        })
    }
}
