use crate::session_logger::WasmSessionLogger;
use crate::tools::ToolRegistry;
use anyhow::Result;
use cooper_core::{ApiType, OutputChunk};

pub async fn run(
    prompt: String,
    system_prompt: String,
    api_type: ApiType,
    base_url: String,
    api_key: String,
    model: String,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
    let registry = ToolRegistry::new();
    let mut logger = WasmSessionLogger::new();
    cooper_core::agent::run(
        prompt,
        system_prompt,
        &api_type,
        &base_url,
        &api_key,
        &model,
        &registry,
        Some(&mut logger),
        on_chunk,
    )
    .await
}
