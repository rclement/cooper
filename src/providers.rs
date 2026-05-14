/// CLI-extended output events. Includes `SessionStart` (CLI-only) plus all core
/// streaming chunks. The core variants use struct fields to match `cooper_core::OutputChunk`.
#[derive(Debug, Clone)]
pub enum OutputChunk {
    SessionStart {
        provider: String,
        model: String,
        /// None = disabled; Some(path) = file that will be loaded (may or may not exist)
        agent_instructions: Option<String>,
        context_files: Vec<String>,
        /// None = all tools; Some(list) = restricted set (empty = no tools)
        tools: Option<Vec<String>>,
    },
    Thinking {
        text: String,
    },
    Content {
        text: String,
    },
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        output: String,
    },
}

impl From<cooper_core::OutputChunk> for OutputChunk {
    fn from(c: cooper_core::OutputChunk) -> Self {
        match c {
            cooper_core::OutputChunk::Thinking { text } => OutputChunk::Thinking { text },
            cooper_core::OutputChunk::Content { text } => OutputChunk::Content { text },
            cooper_core::OutputChunk::ToolCall { name, args } => {
                OutputChunk::ToolCall { name, args }
            }
            cooper_core::OutputChunk::ToolResult { name, output } => {
                OutputChunk::ToolResult { name, output }
            }
        }
    }
}
