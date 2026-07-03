use std::{collections::HashMap, process};

pub use cooper_core::tools::Tool;
use cooper_core::tools::{ToolParameterSchema, ToolParameterTypeSchema, ToolSchema};

pub struct ListFilesTool;

#[async_trait::async_trait(?Send)]
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

#[async_trait::async_trait(?Send)]
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

#[async_trait::async_trait(?Send)]
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

#[async_trait::async_trait(?Send)]
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

    #[test]
    fn listfilestool_schema_success() {
        let expected = ToolSchema {
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
        };

        let schema = ListFilesTool.schema();

        assert_eq!(schema, expected);
    }

    #[tokio::test]
    async fn listfilestool_execute_success() {
        let mut expected_filenames = vec!["a.txt", "b.txt"];
        expected_filenames.sort();

        let tmpdir = tempfile::tempdir().unwrap();
        let tmpdir_path = tmpdir.path();
        for filename in &expected_filenames {
            std::fs::write(tmpdir_path.join(filename), "content").unwrap();
        }

        let args = HashMap::from([(
            "path".to_string(),
            tmpdir_path.to_str().unwrap().to_string(),
        )]);
        let result = ListFilesTool.execute(&args).await.unwrap();
        let mut result_files = result.split('\n').collect::<Vec<&str>>();
        result_files.sort();

        assert_eq!(result_files, expected_filenames);
    }

    #[tokio::test]
    async fn listfilestool_execute_missing_path() {
        let args = HashMap::new();
        let err = ListFilesTool.execute(&args).await.unwrap_err();

        assert_eq!(err, "missing argument: path");
    }

    #[tokio::test]
    async fn readfiletool_execute_invalid_path() {
        let args = HashMap::from([("path".to_string(), "/nonexistent/path/to/file".to_string())]);
        let err = ReadFileTool.execute(&args).await.unwrap_err();

        assert!(err.contains("No such file or directory"));
    }

    #[tokio::test]
    async fn listfilestool_execute_invalid_path() {
        let args = HashMap::from([("path".to_string(), "/nonexistent/path/to/dir".to_string())]);
        let err = ListFilesTool.execute(&args).await.unwrap_err();

        assert!(err.contains("No such file or directory"));
    }

    #[test]
    fn execcmdtool_schema_success() {
        let expected = ToolSchema {
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
        };

        let schema = ExecCmdTool.schema();

        assert_eq!(schema, expected);
    }

    #[tokio::test]
    async fn execcmdtool_execute_success() {
        let args = HashMap::from([("command".to_string(), "echo hello".to_string())]);
        let result = ExecCmdTool.execute(&args).await.unwrap();

        assert_eq!(result, "hello\n");
    }

    #[tokio::test]
    async fn execcmdtool_execute_missing_command() {
        let args = HashMap::new();
        let err = ExecCmdTool.execute(&args).await.unwrap_err();

        assert_eq!(err, "missing argument: command");
    }

    #[tokio::test]
    async fn execcmdtool_execute_failure() {
        let args = HashMap::from([(
            "command".to_string(),
            "echo failed 1>&2; exit 1".to_string(),
        )]);
        let err = ExecCmdTool.execute(&args).await.unwrap_err();

        assert_eq!(err, "exit code: 1, error: failed\n");
    }

    #[test]
    fn customtool_schema_success() {
        let parameters = HashMap::from([(
            "name".to_string(),
            ToolParameterSchema {
                param_type: ToolParameterTypeSchema::String,
                description: "A name".to_string(),
                required: true,
            },
        )]);
        let tool = CustomTool::new("greet", "Greet someone", &parameters, &[]);

        let expected = ToolSchema {
            name: "greet".to_string(),
            description: "Greet someone".to_string(),
            parameters,
        };

        assert_eq!(tool.schema(), expected);
    }

    #[tokio::test]
    async fn customtool_execute_success_with_param_substitution() {
        let parameters = HashMap::from([(
            "name".to_string(),
            ToolParameterSchema {
                param_type: ToolParameterTypeSchema::String,
                description: "A name".to_string(),
                required: true,
            },
        )]);
        let commands = vec![vec!["echo".to_string(), "hello ${name}".to_string()]];
        let tool = CustomTool::new("greet", "Greet someone", &parameters, &commands);

        let args = HashMap::from([("name".to_string(), "world".to_string())]);
        let result = tool.execute(&args).await.unwrap();

        assert_eq!(result, "hello world\n");
    }

    #[tokio::test]
    async fn customtool_execute_pipeline_success() {
        let commands = vec![
            vec!["echo".to_string(), "hello world".to_string()],
            vec![
                "cut".to_string(),
                "-d".to_string(),
                " ".to_string(),
                "-f2".to_string(),
            ],
        ];
        let tool = CustomTool::new("pipeline", "Pipeline test", &HashMap::new(), &commands);

        let args = HashMap::new();
        let result = tool.execute(&args).await.unwrap();

        assert_eq!(result, "world\n");
    }

    #[tokio::test]
    async fn customtool_execute_missing_required_arg() {
        let parameters = HashMap::from([(
            "name".to_string(),
            ToolParameterSchema {
                param_type: ToolParameterTypeSchema::String,
                description: "A name".to_string(),
                required: true,
            },
        )]);
        let commands = vec![vec!["echo".to_string(), "${name}".to_string()]];
        let tool = CustomTool::new("greet", "Greet someone", &parameters, &commands);

        let args = HashMap::new();
        let err = tool.execute(&args).await.unwrap_err();

        assert_eq!(err, "missing required argument: name");
    }

    #[tokio::test]
    async fn customtool_execute_command_not_found() {
        let commands = vec![vec!["nonexistent-binary-xyz".to_string()]];
        let tool = CustomTool::new("bad", "Bad command", &HashMap::new(), &commands);

        let args = HashMap::new();
        let err = tool.execute(&args).await.unwrap_err();

        assert!(err.contains("No such file or directory"));
    }

    #[tokio::test]
    async fn customtool_execute_failure_exit_code() {
        let commands = vec![vec!["false".to_string()]];
        let tool = CustomTool::new("fail", "Fails", &HashMap::new(), &commands);

        let args = HashMap::new();
        let err = tool.execute(&args).await.unwrap_err();

        assert_eq!(err, "exit code: 1, error: ");
    }

    #[tokio::test]
    async fn customtool_execute_no_commands() {
        let tool = CustomTool::new("empty", "No commands", &HashMap::new(), &[]);

        let args = HashMap::new();
        let err = tool.execute(&args).await.unwrap_err();

        assert_eq!(err, "");
    }
}
