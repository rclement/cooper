use cooper_core::{ApiType, OutputChunk};
use wasm_bindgen::prelude::*;

mod session;
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

        let system_prompt = cooper_core::system_prompt::build(cooper_core::system_prompt::Options {
            base: cfg["system_prompt"]
                .as_str()
                .unwrap_or(cooper_core::system_prompt::DEFAULT)
                .to_string(),
            date: cfg["date"].as_str().map(str::to_string),
            cwd: None,
            skills: vec![],
            agent_instructions: cfg["agent_instructions"].as_str().map(str::to_string),
            context_files: cfg["context_files"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| {
                            Some(cooper_core::system_prompt::ContextFile {
                                path: f["path"].as_str()?.to_string(),
                                content: f["content"].as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        });

        Ok(CooperAgent {
            api_type,
            base_url: cfg["base_url"].as_str().unwrap_or("").to_string(),
            api_key: cfg["api_key"].as_str().unwrap_or("").to_string(),
            model: cfg["model"].as_str().unwrap_or("").to_string(),
            system_prompt,
        })
    }

    /// Runs the agentic loop for `prompt`.
    ///
    /// `on_chunk` is called with a JSON string for each output event.
    /// Returns a Promise that resolves to the final assistant text, or rejects
    /// with an error string.
    pub fn run_prompt(&self, prompt: String, on_chunk: js_sys::Function) -> js_sys::Promise {
        let api_type = self.api_type.clone();
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let system_prompt = self.system_prompt.clone();

        wasm_bindgen_futures::future_to_promise(async move {
            let registry = tools::ToolRegistry::new();
            let mut logger = session::WasmSessionLogger::new(&api_type.to_string(), &model);
            let provider = cooper_core::AnyProvider::new(&api_type, base_url, api_key, model);

            let mut cb = |chunk: OutputChunk| {
                let json = serde_json::to_string(&chunk).unwrap_or_default();
                let _ = on_chunk.call1(&JsValue::UNDEFINED, &JsValue::from_str(&json));
            };

            cooper_core::agent::run(prompt, system_prompt, &provider, &registry, Some(&mut logger), &mut cb)
                .await
                .map(|s| JsValue::from_str(&s))
                .map_err(|e| JsValue::from_str(&e.to_string()))
        })
    }
}
