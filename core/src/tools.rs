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

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: HashMap<String, ToolParameterSchema>,
}

#[async_trait::async_trait(?Send)]
pub trait Tool {
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String>;
}
