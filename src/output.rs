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
        /// Available skills (chat) or the single active skill (prompt --skill)
        skills: Vec<String>,
        /// Name of the skill pre-loaded into the system prompt (prompt --skill only)
        active_skill: Option<String>,
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
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
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
            cooper_core::OutputChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } => OutputChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            },
        }
    }
}
