use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum ToolParameterTypeSchema {
    String,
    Number,
    Boolean,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolParameterSchema {
    pub param_type: ToolParameterTypeSchema,
    pub description: String,
    pub required: bool,
}

#[derive(Debug, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: HashMap<String, ToolParameterSchema>,
}

#[async_trait::async_trait]
pub trait Tool {
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String>;
}
