use std::collections::HashMap;

use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolParameterTypeSchema {
    String,
    Number,
    Boolean,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ToolParameterSchema {
    #[serde(rename = "type")]
    pub param_type: ToolParameterTypeSchema,
    pub description: String,
    pub required: bool,
}

/// `Deserialize` so a tool schema can be supplied at runtime rather than only
/// defined in Rust — e.g. a browser tool registered from JS (or eventually
/// Python via Pyodide) describes itself with the same JSON shape.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: HashMap<String, ToolParameterSchema>,
}

/// `?Send`: a browser-side tool may bridge to a JS (or Pyodide) callback,
/// i.e. a `js_sys::Function`, which is single-threaded only.
#[async_trait::async_trait(?Send)]
pub trait Tool {
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String>;
}
