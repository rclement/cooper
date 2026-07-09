pub mod openai_completions;
pub mod openai_wire;

use async_trait::async_trait;

use crate::agent;
use crate::tools;

#[async_trait(?Send)]
pub trait Provider {
    async fn complete_stream(
        &self,
        messages: &[agent::Message],
        tools: &[tools::ToolSchema],
        handler: &dyn agent::AgentEventsHandler,
    ) -> Result<(agent::Message, agent::FinishReason), Box<dyn std::error::Error>>;
}
