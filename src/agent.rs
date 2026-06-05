use std::collections::HashMap;

use crate::providers::Provider;
use askama::Template;

/// === system prompt === ///

/// ```askama
/// You are agent Cooper, a special AI agent harness.
///
/// Current date: {{ current_date }}
/// Current time: {{ current_time }}
/// Current working directory: {{ current_working_dir }}
/// ```
#[derive(askama::Template)]
#[template(ext = "txt", in_doc = true)]
struct SystemPromptTemplate {
    current_date: String,
    current_time: String,
    current_working_dir: String,
}

fn build_system_prompt() -> Result<String, askama::Error> {
    let now = chrono::Local::now();
    let template = SystemPromptTemplate {
        current_date: now.format("%Y-%m-%d").to_string(),
        current_time: now.format("%H:%M:%S %z").to_string(),
        current_working_dir: std::env::current_dir()?.display().to_string(),
    };
    template.render()
}

/// === agent message types === ///

#[derive(Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: HashMap<String, String>,
}

pub enum Message {
    System(String),
    User(String),
    Assistant {
        text: Option<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        result: Result<String, String>,
    },
}

pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Unknown(String),
}

pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// === agent event handler === ///

pub struct DeltaChunk {
    pub text: Option<String>,
    pub reasoning: Option<String>,
}

pub trait ChunkHandler: Send + Sync {
    fn on_chunk(&self, chunk: &DeltaChunk);
    fn on_complete(&self, _usage: &Usage) {}
    fn on_tool_call(&self, _tool_call: &ToolCall) {}
    fn on_tool_result(&self, _tool_result: &Result<String, String>) {}
}

/// === tool type definitions === ///

pub enum ToolParameterTypeSchema {
    String,
    Number,
    Boolean,
}

pub struct ToolParameterSchema {
    pub param_type: ToolParameterTypeSchema,
    pub description: String,
    pub required: bool,
}

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

/// === built-in tools definitions === ///

struct ListFilesTool;

#[async_trait::async_trait]
impl Tool for ListFilesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_files".to_string(),
            description: "List files in a given directory".to_string(),
            parameters: HashMap::from([(
                "path".to_string(),
                ToolParameterSchema {
                    param_type: ToolParameterTypeSchema::String,
                    description: "Directory path".to_string(),
                    required: true,
                },
            )]),
        }
    }

    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String> {
        let path = args
            .get("path")
            .ok_or_else(|| "missing argument: path".to_string())?;
        let dir_list = std::fs::read_dir(path).map_err(|e| e.to_string())?;
        let filenames: Vec<String> = dir_list
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(filenames.join("\n"))
    }
}

struct ReadFileTool;

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".to_string(),
            description: "Read the content of a file".to_string(),
            parameters: HashMap::from([(
                "path".to_string(),
                ToolParameterSchema {
                    param_type: ToolParameterTypeSchema::String,
                    description: "File path".to_string(),
                    required: true,
                },
            )]),
        }
    }

    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String> {
        let path = args
            .get("path")
            .ok_or_else(|| "missing argument: path".to_string())?;

        let file_content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok(file_content)
    }
}

struct ExecCmdTool;

#[async_trait::async_trait]
impl Tool for ExecCmdTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "exec_cmd".to_string(),
            description: "Execute a shell command".to_string(),
            parameters: HashMap::from([(
                "command".to_string(),
                ToolParameterSchema {
                    param_type: ToolParameterTypeSchema::String,
                    description: "Shell command to execute".to_string(),
                    required: true,
                },
            )]),
        }
    }

    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String> {
        let command = args
            .get("command")
            .ok_or_else(|| "missing argument: command".to_string())?;

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|e| e.to_string())?;

        if !output.status.success() {
            return Err(format!(
                "exit code: {:?}, error: {}",
                output.status.code().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(stdout)
    }
}

/// === agentic loop with tool calling (streaming) === ///

pub async fn agent_loop_stream(
    user_prompt: &str,
    provider: &dyn Provider,
    handler: &dyn ChunkHandler,
) -> Result<Message, Box<dyn std::error::Error>> {
    let builtin_tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ListFilesTool),
        Box::new(ReadFileTool),
        Box::new(ExecCmdTool),
    ];
    let mut tool_registry: HashMap<String, Box<dyn Tool>> = HashMap::new();
    for tool in builtin_tools {
        tool_registry.insert(tool.schema().name.clone(), tool);
    }
    let tool_schemas: Vec<ToolSchema> = tool_registry.values().map(|t| t.schema()).collect();

    let system_prompt = build_system_prompt()?;
    let mut messages = vec![
        Message::System(system_prompt),
        Message::User(user_prompt.to_string()),
    ];

    loop {
        let (result, finish_reason) = provider
            .complete_stream(&messages, &tool_schemas, handler)
            .await?;

        match finish_reason {
            FinishReason::Stop => return Ok(result),
            FinishReason::ToolCalls => {}
            FinishReason::Length => return Err("response truncated: token limit reached".into()),
            FinishReason::Unknown(s) => {
                eprintln!("unknown finish reason: {}", s);
                return Ok(result);
            }
        }

        let tool_calls = match &result {
            Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => tool_calls.clone(),
            _ => return Ok(result),
        };

        messages.push(result);
        for tc in tool_calls {
            handler.on_tool_call(&tc);
            let tool_call_result = match tool_registry.get(&tc.name) {
                Some(tool) => tool.execute(&tc.arguments).await,
                None => Err(format!("tool not found: {}", tc.name)),
            };
            handler.on_tool_result(&tool_call_result);

            let tool_call_message = Message::Tool {
                call_id: tc.id.clone(),
                result: tool_call_result,
            };
            messages.push(tool_call_message);
        }
    }
}
