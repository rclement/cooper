use std::{collections::HashMap, process};

/// === tool type definitions === ///

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

        let output = process::Command::new("sh")
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

pub struct CustomTool {
    name: String,
    description: String,
    parameters: HashMap<String, ToolParameterSchema>,
    commands: Vec<Vec<String>>,
}

impl CustomTool {
    pub fn new(
        name: &str,
        description: &str,
        parameters: &HashMap<String, ToolParameterSchema>,
        commands: &[Vec<String>],
    ) -> Self {
        CustomTool {
            name: name.to_string(),
            description: description.to_string(),
            parameters: parameters.clone(),
            commands: commands.to_vec(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for CustomTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    async fn execute(&self, args: &HashMap<String, String>) -> Result<String, String> {
        fn substitute_param(s: &str, params: &HashMap<String, String>) -> String {
            let mut result = s.to_string();
            for (k, v) in params {
                result = result.replace(&format!("${{{}}}", k), v);
            }
            result
        }

        let param_values: HashMap<String, String> = args
            .iter()
            .filter(|(k, _)| self.parameters.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (name, param) in &self.parameters {
            if param.required && !param_values.contains_key(name) {
                return Err(format!("missing required argument: {}", name));
            }
        }

        let mut previous_child: Option<process::Child> = None;
        for command in self.commands.iter() {
            let program = substitute_param(&command[0], &param_values);
            let program_args: Vec<String> = command[1..]
                .iter()
                .map(|a| substitute_param(a, &param_values))
                .collect();

            let stdin = match previous_child.as_mut() {
                Some(child) => process::Stdio::from(child.stdout.take().unwrap()),
                None => process::Stdio::inherit(),
            };
            let stdout = process::Stdio::piped();

            let child = process::Command::new(program)
                .args(&program_args)
                .stdin(stdin)
                .stdout(stdout)
                .spawn()
                .map_err(|e| e.to_string())?;

            previous_child = Some(child);
        }
        let output = previous_child
            .ok_or("".to_string())?
            .wait_with_output()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn readfiletool_schema_success() {
        let expected = ToolSchema {
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
        };

        let schema = ReadFileTool.schema();

        assert_eq!(schema, expected);
    }

    #[tokio::test]
    async fn readfiletool_execute_success() {
        let content = "some content";
        let mut tmpfile = tempfile::NamedTempFile::new().unwrap();
        tmpfile.write_all(content.as_bytes()).unwrap();

        let args = HashMap::from([(
            "path".to_string(),
            tmpfile.path().to_str().unwrap().to_string(),
        )]);
        let result = ReadFileTool.execute(&args).await.unwrap();

        assert_eq!(result, content);
    }

    #[tokio::test]
    async fn readfiletool_execute_missing_path() {
        let args = HashMap::new();
        let err = ReadFileTool.execute(&args).await.unwrap_err();

        assert_eq!(err, "missing argument: path");
    }
}
