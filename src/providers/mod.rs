pub mod openai_completions;
use crate::agent::{ChunkHandler, FinishReason, Message, ToolSchema};
use async_trait::async_trait;

#[async_trait]
pub trait Provider {
    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        handler: &dyn ChunkHandler,
    ) -> Result<(Message, FinishReason), Box<dyn std::error::Error>>;
}
