use std::collections::HashMap;

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

pub struct ListFilesTool;

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

pub struct ReadFileTool;

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

pub struct ExecCmdTool;

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
