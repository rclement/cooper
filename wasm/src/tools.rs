use anyhow::anyhow;

// All native built-in tools (list_files, read_file, write_file, edit_file, execute_command)
// are disabled in the browser environment — they require filesystem and process APIs that
// WASM does not expose. Custom JS-based tools can be added in a future iteration.

pub struct ToolRegistry;

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry
    }
}

impl cooper_core::ToolExecutor for ToolRegistry {
    fn schemas(&self) -> Vec<cooper_core::ToolSchema> {
        vec![]
    }

    async fn execute(&self, name: &str, _args_json: &str) -> anyhow::Result<String> {
        Err(anyhow!(
            "tool '{}' is not available in the browser environment",
            name
        ))
    }
}
