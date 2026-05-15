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
        /// None = no skills; Some(list) = restricted set (empty = no skills allowed)
        skills: Option<Vec<String>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_thinking() {
        let c = OutputChunk::from(cooper_core::OutputChunk::Thinking { text: "t".into() });
        assert!(matches!(c, OutputChunk::Thinking { text } if text == "t"));
    }

    #[test]
    fn from_content() {
        let c = OutputChunk::from(cooper_core::OutputChunk::Content { text: "hi".into() });
        assert!(matches!(c, OutputChunk::Content { text } if text == "hi"));
    }

    #[test]
    fn from_tool_call() {
        let c = OutputChunk::from(cooper_core::OutputChunk::ToolCall { name: "f".into(), args: "{}".into() });
        assert!(matches!(c, OutputChunk::ToolCall { name, args } if name == "f" && args == "{}"));
    }

    #[test]
    fn from_tool_result() {
        let c = OutputChunk::from(cooper_core::OutputChunk::ToolResult { name: "f".into(), output: "ok".into() });
        assert!(matches!(c, OutputChunk::ToolResult { name, output } if name == "f" && output == "ok"));
    }

    #[test]
    fn from_usage() {
        let c = OutputChunk::from(cooper_core::OutputChunk::Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        });
        assert!(matches!(c, OutputChunk::Usage { prompt_tokens: 1, completion_tokens: 2, total_tokens: 3 }));
    }
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
