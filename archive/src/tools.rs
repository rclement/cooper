use anyhow::{Context, Result, anyhow};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command as AsyncCommand;

// ── Built-in tools ────────────────────────────────────────────────────────────

pub struct ToolParam {
    pub name: &'static str,
    pub description: &'static str,
    pub type_: &'static str,
    pub required: bool,
    pub default: Option<&'static str>,
}

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub params: &'static [ToolParam],
}

pub const BUILTIN_TOOLS: &[Tool] = &[
    Tool {
        name: "list_files",
        description: "List files in a directory",
        params: &[ToolParam {
            name: "path",
            description: "Directory path",
            type_: "string",
            required: false,
            default: Some("."),
        }],
    },
    Tool {
        name: "read_file",
        description: "Read the content of a file",
        params: &[ToolParam {
            name: "path",
            description: "File path to read",
            type_: "string",
            required: true,
            default: None,
        }],
    },
    Tool {
        name: "write_file",
        description: "Write content to a file",
        params: &[
            ToolParam {
                name: "path",
                description: "File path to write",
                type_: "string",
                required: true,
                default: None,
            },
            ToolParam {
                name: "content",
                description: "Content to write",
                type_: "string",
                required: true,
                default: None,
            },
        ],
    },
    Tool {
        name: "edit_file",
        description: "Edit a file by replacing the first occurrence of a string",
        params: &[
            ToolParam {
                name: "path",
                description: "File path to edit",
                type_: "string",
                required: true,
                default: None,
            },
            ToolParam {
                name: "old",
                description: "Text to replace",
                type_: "string",
                required: true,
                default: None,
            },
            ToolParam {
                name: "new",
                description: "Replacement text",
                type_: "string",
                required: true,
                default: None,
            },
        ],
    },
    Tool {
        name: "execute_command",
        description: "Execute a shell command",
        params: &[ToolParam {
            name: "command",
            description: "Shell command to execute",
            type_: "string",
            required: true,
            default: None,
        }],
    },
];

pub fn find(name: &str) -> Option<&'static Tool> {
    BUILTIN_TOOLS.iter().find(|t| t.name == name)
}

pub fn tool_schema(tool: &Tool) -> cooper_core::ToolSchema {
    let mut properties = serde_json::Map::new();
    let mut required_params: Vec<Value> = Vec::new();
    for p in tool.params {
        properties.insert(
            p.name.to_string(),
            serde_json::json!({ "type": p.type_, "description": p.description }),
        );
        if p.required {
            required_params.push(serde_json::json!(p.name));
        }
    }
    cooper_core::ToolSchema {
        name: tool.name.to_string(),
        description: tool.description.to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": Value::Object(properties),
            "required": required_params,
        }),
    }
}

pub fn execute(tool: &Tool, args: &HashMap<String, String>) -> Result<String> {
    let mut params: HashMap<&str, String> = HashMap::new();
    for p in tool.params {
        if let Some(val) = args.get(p.name) {
            params.insert(p.name, val.clone());
        } else if let Some(default) = p.default {
            params.insert(p.name, default.to_string());
        } else if p.required {
            return Err(anyhow!("missing required parameter: --{}", p.name));
        }
    }

    match tool.name {
        "list_files" => {
            let path = &params["path"];
            let mut entries: Vec<String> = fs::read_dir(path)?
                .filter_map(|e| e.ok())
                .map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        format!("{}/", name)
                    } else {
                        name
                    }
                })
                .collect();
            entries.sort();
            Ok(entries.join("\n"))
        }
        "read_file" => {
            let path = &params["path"];
            let content = fs::read_to_string(path)?;
            Ok(content)
        }
        "write_file" => {
            let path = &params["path"];
            let content = &params["content"];
            if let Some(parent) = Path::new(path.as_str()).parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                }
            }
            fs::write(path, content)?;
            Ok(format!("Written to {}", path))
        }
        "edit_file" => {
            let path = &params["path"];
            let old = &params["old"];
            let new = &params["new"];
            let content = fs::read_to_string(path)?;
            if !content.contains(old.as_str()) {
                return Err(anyhow!("pattern not found in {}", path));
            }
            let updated = content.replacen(old.as_str(), new.as_str(), 1);
            fs::write(path, updated)?;
            Ok(format!("Edited {}", path))
        }
        "execute_command" => {
            let command = &params["command"];
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .output()?;
            let mut result = String::new();
            if !output.stdout.is_empty() {
                result.push_str(&String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                result.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            Ok(result)
        }
        _ => Err(anyhow!("unknown built-in tool: {}", tool.name)),
    }
}

// ── Custom tool types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct CustomToolParamDef {
    #[serde(rename = "type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    pub default: Option<String>,
    pub description: Option<String>,
}

// Deserializes from either a flat array (single command) or array of arrays (pipeline).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CommandSpec {
    Single(Vec<String>),
    Pipeline(Vec<Vec<String>>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomToolDef {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: IndexMap<String, CustomToolParamDef>,
    pub command: CommandSpec,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct CustomTool {
    pub def: CustomToolDef,
    pub source: PathBuf,
}

impl CustomTool {
    fn tool_schema(&self) -> cooper_core::ToolSchema {
        let mut properties = serde_json::Map::new();
        let mut required_params: Vec<Value> = Vec::new();
        for (name, p) in &self.def.parameters {
            let mut prop = serde_json::json!({ "type": p.param_type });
            if let Some(desc) = &p.description {
                prop["description"] = Value::String(desc.clone());
            }
            properties.insert(name.clone(), prop);
            if p.required {
                required_params.push(Value::String(name.clone()));
            }
        }
        cooper_core::ToolSchema {
            name: self.def.name.clone(),
            description: self.def.description.clone(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": Value::Object(properties),
                "required": required_params,
            }),
        }
    }

    pub async fn execute(&self, args: &HashMap<String, String>) -> Result<String> {
        // Resolve parameter values, applying defaults for omitted optional params.
        let mut params: HashMap<String, String> = HashMap::new();
        for (name, def) in &self.def.parameters {
            if let Some(val) = args.get(name) {
                params.insert(name.clone(), val.clone());
            } else if let Some(default) = &def.default {
                params.insert(name.clone(), default.clone());
            } else if def.required {
                return Err(anyhow!("missing required parameter: --{}", name));
            }
        }

        // Resolve env values: ${VAR} → OS environment variable.
        let env_map: HashMap<String, String> = self
            .def
            .env
            .iter()
            .map(|(k, v)| (k.clone(), resolve_env_vars(v)))
            .collect();

        // Combined substitution map: params take precedence over env vars.
        let mut vars = env_map.clone();
        vars.extend(params.clone());

        match &self.def.command {
            CommandSpec::Single(cmd) => run_command(cmd, &vars, &env_map, None).await,
            CommandSpec::Pipeline(stages) => {
                let mut stdin: Option<Vec<u8>> = None;
                for stage in stages {
                    let out = run_command(stage, &vars, &env_map, stdin.as_deref()).await?;
                    stdin = Some(out.into_bytes());
                }
                Ok(stdin
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default())
            }
        }
    }
}

// ── Substitution helpers ──────────────────────────────────────────────────────

// Replaces ${key} placeholders with values from `vars`. Unknown placeholders are left as-is.
fn substitute(s: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let key = &after[..end];
            if let Some(val) = vars.get(key) {
                result.push_str(val);
            } else {
                result.push_str("${");
                result.push_str(key);
                result.push('}');
            }
            rest = &after[end + 1..];
        } else {
            result.push_str("${");
            rest = after;
        }
    }
    result.push_str(rest);
    result
}

// Replaces ${VAR} with the value of OS environment variable VAR.
fn resolve_env_vars(s: &str) -> String {
    let mut result = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let var_name = &after[..end];
            result.push_str(&std::env::var(var_name).unwrap_or_default());
            rest = &after[end + 1..];
        } else {
            result.push_str("${");
            rest = after;
        }
    }
    result.push_str(rest);
    result
}

async fn run_command(
    cmd: &[String],
    vars: &HashMap<String, String>,
    env_map: &HashMap<String, String>,
    stdin_data: Option<&[u8]>,
) -> Result<String> {
    if cmd.is_empty() {
        return Err(anyhow!("empty command"));
    }
    let program = substitute(&cmd[0], vars);
    let args: Vec<String> = cmd[1..].iter().map(|s| substitute(s, vars)).collect();

    let mut child = AsyncCommand::new(&program)
        .args(&args)
        .envs(env_map)
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning command: {}", program))?;

    if let Some(data) = stdin_data {
        if let Some(mut stdin_handle) = child.stdin.take() {
            stdin_handle
                .write_all(data)
                .await
                .context("writing stdin to child process")?;
            // Drop closes the pipe, signalling EOF to the child.
        }
    }

    let output = child
        .wait_with_output()
        .await
        .context("waiting for child process")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "command '{}' exited with {}: {}",
            program,
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ── Tool discovery ────────────────────────────────────────────────────────────

fn load_tool_from_file(path: &Path) -> Result<CustomTool> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let def: CustomToolDef =
        serde_yaml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
    Ok(CustomTool {
        def,
        source: path.to_path_buf(),
    })
}

fn load_from_dir(dir: &Path) -> Result<Vec<CustomTool>> {
    let mut tools = Vec::new();

    let mut entries: Vec<_> = match fs::read_dir(dir) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(_) => return Ok(tools),
    };
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let result = if path.is_file() && path.extension().map_or(false, |e| e == "yml") {
            load_tool_from_file(&path)
        } else if path.is_dir() {
            let tool_yml = path.join("tool.yml");
            if tool_yml.exists() {
                load_tool_from_file(&tool_yml)
            } else {
                continue;
            }
        } else {
            continue;
        };

        match result {
            Ok(tool) => tools.push(tool),
            Err(e) => eprintln!("warning: skipping invalid tool definition: {}", e),
        }
    }

    Ok(tools)
}

// ── Tool registry ─────────────────────────────────────────────────────────────

pub struct ToolRegistry {
    pub(crate) custom_tools: Vec<CustomTool>,
}

impl ToolRegistry {
    /// Loads custom tools from global (~/.cooper/tools) and project (.agents/tools) directories.
    /// Project tools override globals of the same name. Errors if any custom tool name
    /// conflicts with a built-in.
    pub fn load() -> Result<Self> {
        let mut custom_tools: Vec<CustomTool> = Vec::new();

        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".cooper").join("tools");
            custom_tools.extend(load_from_dir(&global_dir)?);
        }

        let project_dir = PathBuf::from(".agents/tools");
        if project_dir.exists() {
            for tool in load_from_dir(&project_dir)? {
                custom_tools.retain(|t| t.def.name != tool.def.name);
                custom_tools.push(tool);
            }
        }

        for tool in &custom_tools {
            if BUILTIN_TOOLS.iter().any(|b| b.name == tool.def.name) {
                return Err(anyhow!(
                    "custom tool '{}' (from {}) conflicts with a built-in tool name; \
                     rename the tool or remove the file",
                    tool.def.name,
                    tool.source.display()
                ));
            }
        }

        Ok(Self { custom_tools })
    }

    pub fn all_names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILTIN_TOOLS.iter().map(|t| t.name.to_string()).collect();
        names.extend(self.custom_tools.iter().map(|t| t.def.name.clone()));
        names
    }

    pub fn schemas(&self) -> Vec<cooper_core::ToolSchema> {
        let mut schemas: Vec<cooper_core::ToolSchema> = BUILTIN_TOOLS.iter().map(tool_schema).collect();
        schemas.extend(self.custom_tools.iter().map(|t| t.tool_schema()));
        schemas
    }

    pub fn find_custom(&self, name: &str) -> Option<&CustomTool> {
        self.custom_tools.iter().find(|t| t.def.name == name)
    }

    pub async fn execute_json(&self, name: &str, args_json: &str) -> Result<String> {
        let raw: serde_json::Map<String, Value> = serde_json::from_str(args_json)
            .with_context(|| format!("parsing arguments for {}", name))?;
        let args: HashMap<String, String> = raw
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    match v {
                        Value::String(s) => s,
                        other => other.to_string(),
                    },
                )
            })
            .collect();

        if let Some(tool) = find(name) {
            return execute(tool, &args);
        }
        if let Some(tool) = self.find_custom(name) {
            return tool.execute(&args).await;
        }
        Err(anyhow!("unknown tool: {}", name))
    }

    pub fn custom_tools(&self) -> &[CustomTool] {
        &self.custom_tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    // ── substitute ────────────────────────────────────────────────────────────

    #[test]
    fn substitute_no_vars() {
        let vars = HashMap::new();
        assert_eq!(substitute("hello world", &vars), "hello world");
    }

    #[test]
    fn substitute_known_var() {
        let mut vars = HashMap::new();
        vars.insert("NAME".to_string(), "alice".to_string());
        assert_eq!(substitute("hello ${NAME}!", &vars), "hello alice!");
    }

    #[test]
    fn substitute_unknown_var_preserved() {
        let vars = HashMap::new();
        assert_eq!(substitute("${UNKNOWN}", &vars), "${UNKNOWN}");
    }

    #[test]
    fn substitute_unclosed_brace() {
        let vars = HashMap::new();
        assert_eq!(substitute("hello ${unclosed", &vars), "hello ${unclosed");
    }

    #[test]
    fn substitute_multiple_vars() {
        let mut vars = HashMap::new();
        vars.insert("A".to_string(), "1".to_string());
        vars.insert("B".to_string(), "2".to_string());
        assert_eq!(substitute("${A}+${B}", &vars), "1+2");
    }

    // ── resolve_env_vars ──────────────────────────────────────────────────────

    #[test]
    fn resolve_env_vars_known() {
        // SAFETY: unique key, test is isolated.
        unsafe { std::env::set_var("COOPER_TOOLS_TEST", "resolved") };
        let result = resolve_env_vars("val=${COOPER_TOOLS_TEST}");
        unsafe { std::env::remove_var("COOPER_TOOLS_TEST") };
        assert_eq!(result, "val=resolved");
    }

    #[test]
    fn resolve_env_vars_unknown_becomes_empty() {
        unsafe { std::env::remove_var("COOPER_TOOLS_MISSING_XYZ") };
        let result = resolve_env_vars("${COOPER_TOOLS_MISSING_XYZ}");
        assert_eq!(result, "");
    }

    #[test]
    fn resolve_env_vars_unclosed() {
        let result = resolve_env_vars("${abc");
        assert_eq!(result, "${abc");
    }

    // ── tool_schema ───────────────────────────────────────────────────────────

    #[test]
    fn tool_schema_builtin_read_file() {
        let tool = find("read_file").unwrap();
        let schema = tool_schema(tool);
        assert_eq!(schema.name, "read_file");
        let required = schema.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "path"));
    }

    #[test]
    fn tool_schema_optional_param_not_in_required() {
        let tool = find("list_files").unwrap();
        let schema = tool_schema(tool);
        // path has a default, so it's optional
        let required = schema.parameters["required"].as_array().unwrap();
        assert!(!required.iter().any(|r| r == "path"));
        // but it still appears in properties
        let props = schema.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("path"));
    }

    // ── find ──────────────────────────────────────────────────────────────────

    #[test]
    fn find_existing_tool() {
        assert!(find("list_files").is_some());
        assert!(find("read_file").is_some());
        assert!(find("write_file").is_some());
        assert!(find("edit_file").is_some());
        assert!(find("execute_command").is_some());
    }

    #[test]
    fn find_missing_tool() {
        assert!(find("nonexistent_tool").is_none());
    }

    // ── execute (built-ins) ───────────────────────────────────────────────────

    #[test]
    fn execute_list_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let tool = find("list_files").unwrap();
        let mut args = HashMap::new();
        args.insert("path".to_string(), tmp.path().to_string_lossy().into_owned());
        let result = execute(tool, &args).unwrap();
        assert!(result.contains("a.txt"));
        assert!(result.contains("b.txt"));
        assert!(result.contains("subdir/"));
    }

    #[test]
    fn execute_list_files_default_path() {
        let tool = find("list_files").unwrap();
        let args = HashMap::new(); // uses default "."
        // Just ensure it doesn't error (we're in the project root during tests)
        assert!(execute(tool, &args).is_ok());
    }

    #[test]
    fn execute_read_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        std::fs::write(&path, "file content").unwrap();

        let tool = find("read_file").unwrap();
        let mut args = HashMap::new();
        args.insert("path".to_string(), path.to_string_lossy().into_owned());
        let result = execute(tool, &args).unwrap();
        assert_eq!(result, "file content");
    }

    #[test]
    fn execute_read_file_missing_param_errors() {
        let tool = find("read_file").unwrap();
        let args = HashMap::new(); // path is required
        let result = execute(tool, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path"));
    }

    #[test]
    fn execute_write_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.txt");

        let tool = find("write_file").unwrap();
        let mut args = HashMap::new();
        args.insert("path".to_string(), path.to_string_lossy().into_owned());
        args.insert("content".to_string(), "written".to_string());
        let result = execute(tool, &args).unwrap();
        assert!(result.contains("Written"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "written");
    }

    #[test]
    fn execute_write_file_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("deep").join("nested").join("file.txt");

        let tool = find("write_file").unwrap();
        let mut args = HashMap::new();
        args.insert("path".to_string(), path.to_string_lossy().into_owned());
        args.insert("content".to_string(), "nested".to_string());
        execute(tool, &args).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn execute_edit_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("edit.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = find("edit_file").unwrap();
        let mut args = HashMap::new();
        args.insert("path".to_string(), path.to_string_lossy().into_owned());
        args.insert("old".to_string(), "world".to_string());
        args.insert("new".to_string(), "rust".to_string());
        let result = execute(tool, &args).unwrap();
        assert!(result.contains("Edited"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust");
    }

    #[test]
    fn execute_edit_file_pattern_not_found() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("edit.txt");
        std::fs::write(&path, "hello").unwrap();

        let tool = find("edit_file").unwrap();
        let mut args = HashMap::new();
        args.insert("path".to_string(), path.to_string_lossy().into_owned());
        args.insert("old".to_string(), "NOTFOUND".to_string());
        args.insert("new".to_string(), "x".to_string());
        let result = execute(tool, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pattern not found"));
    }

    #[test]
    fn execute_command_success() {
        let tool = find("execute_command").unwrap();
        let mut args = HashMap::new();
        args.insert("command".to_string(), "echo hello".to_string());
        let result = execute(tool, &args).unwrap();
        assert!(result.contains("hello"));
    }

    #[test]
    fn execute_command_captures_stderr() {
        let tool = find("execute_command").unwrap();
        let mut args = HashMap::new();
        args.insert("command".to_string(), "echo error >&2".to_string());
        let result = execute(tool, &args).unwrap();
        assert!(result.contains("error"));
    }

    #[test]
    fn execute_unknown_builtin_errors() {
        let tool = Tool { name: "fake_tool", description: "", params: &[] };
        let args = HashMap::new();
        let result = execute(&tool, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown built-in tool"));
    }

    // ── CustomTool::execute ───────────────────────────────────────────────────

    fn make_custom_tool(name: &str, command: Vec<String>, parameters: IndexMap<String, CustomToolParamDef>) -> CustomTool {
        CustomTool {
            def: CustomToolDef {
                name: name.to_string(),
                description: "test tool".to_string(),
                parameters,
                command: CommandSpec::Single(command),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        }
    }

    #[tokio::test]
    async fn custom_tool_single_command() {
        let tool = make_custom_tool("greet", vec!["echo".into(), "hi".into()], IndexMap::new());
        let args = HashMap::new();
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("hi"));
    }

    #[tokio::test]
    async fn custom_tool_with_param_substitution() {
        let mut params = IndexMap::new();
        params.insert("name".to_string(), CustomToolParamDef {
            param_type: "string".into(),
            required: true,
            default: None,
            description: None,
        });
        let tool = make_custom_tool("greet", vec!["echo".into(), "${name}".into()], params);
        let mut args = HashMap::new();
        args.insert("name".to_string(), "world".to_string());
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("world"));
    }

    #[tokio::test]
    async fn custom_tool_missing_required_param_errors() {
        let mut params = IndexMap::new();
        params.insert("required_param".to_string(), CustomToolParamDef {
            param_type: "string".into(),
            required: true,
            default: None,
            description: None,
        });
        let tool = make_custom_tool("t", vec!["echo".into()], params);
        let result = tool.execute(&HashMap::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("required_param"));
    }

    #[tokio::test]
    async fn custom_tool_default_param_used() {
        let mut params = IndexMap::new();
        params.insert("msg".to_string(), CustomToolParamDef {
            param_type: "string".into(),
            required: false,
            default: Some("default-msg".into()),
            description: None,
        });
        let tool = make_custom_tool("t", vec!["echo".into(), "${msg}".into()], params);
        let result = tool.execute(&HashMap::new()).await.unwrap();
        assert!(result.contains("default-msg"));
    }

    #[tokio::test]
    async fn custom_tool_pipeline() {
        let tool = CustomTool {
            def: CustomToolDef {
                name: "pipe".into(),
                description: "pipeline".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Pipeline(vec![
                    vec!["echo".into(), "hello world".into()],
                    vec!["tr".into(), "a-z".into(), "A-Z".into()],
                ]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let result = tool.execute(&HashMap::new()).await.unwrap();
        assert!(result.contains("HELLO WORLD"));
    }

    #[tokio::test]
    async fn custom_tool_command_failure_errors() {
        let tool = make_custom_tool("fail", vec!["sh".into(), "-c".into(), "exit 1".into()], IndexMap::new());
        let result = tool.execute(&HashMap::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exited with"));
    }

    #[tokio::test]
    async fn custom_tool_empty_command_errors() {
        let tool = CustomTool {
            def: CustomToolDef {
                name: "empty".into(),
                description: "".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Single(vec![]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let result = tool.execute(&HashMap::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty command"));
    }

    // ── load_from_dir (custom tools) ──────────────────────────────────────────

    #[test]
    fn load_custom_tools_from_dir_empty() {
        let tmp = TempDir::new().unwrap();
        let tools = load_from_dir(tmp.path()).unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn load_custom_tools_from_dir_nonexistent() {
        let tools = load_from_dir(std::path::Path::new("/nonexistent/tools")).unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn load_custom_tools_from_dir_yml_file() {
        let tmp = TempDir::new().unwrap();
        let yaml = "name: mytool\ndescription: does stuff\ncommand: [echo, hi]\n";
        std::fs::write(tmp.path().join("mytool.yml"), yaml).unwrap();
        let tools = load_from_dir(tmp.path()).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].def.name, "mytool");
    }

    #[test]
    fn load_custom_tools_from_dir_bundled() {
        let tmp = TempDir::new().unwrap();
        let tool_dir = tmp.path().join("my-tool");
        std::fs::create_dir(&tool_dir).unwrap();
        let yaml = "name: bundled\ndescription: bundled tool\ncommand: [echo, ok]\n";
        std::fs::write(tool_dir.join("tool.yml"), yaml).unwrap();
        let tools = load_from_dir(tmp.path()).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].def.name, "bundled");
    }

    #[test]
    fn load_custom_tools_dir_no_tool_yml_ignored() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("notool");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("other.yml"), "whatever").unwrap();
        let tools = load_from_dir(tmp.path()).unwrap();
        assert!(tools.is_empty());
    }

    // ── ToolRegistry ──────────────────────────────────────────────────────────

    #[test]
    fn registry_all_names_includes_builtins() {
        let reg = ToolRegistry { custom_tools: vec![] };
        let names = reg.all_names();
        assert!(names.contains(&"list_files".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"execute_command".to_string()));
    }

    #[test]
    fn registry_all_names_includes_custom() {
        let tool = CustomTool {
            def: CustomToolDef {
                name: "my_custom".into(),
                description: "".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Single(vec!["echo".into()]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let reg = ToolRegistry { custom_tools: vec![tool] };
        assert!(reg.all_names().contains(&"my_custom".to_string()));
    }

    #[test]
    fn registry_find_custom() {
        let tool = CustomTool {
            def: CustomToolDef {
                name: "special".into(),
                description: "".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Single(vec!["echo".into()]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let reg = ToolRegistry { custom_tools: vec![tool] };
        assert!(reg.find_custom("special").is_some());
        assert!(reg.find_custom("nonexistent").is_none());
    }

    #[tokio::test]
    async fn registry_execute_json_builtin() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("t.txt");
        std::fs::write(&file, "content").unwrap();
        let reg = ToolRegistry { custom_tools: vec![] };
        let args = serde_json::json!({"path": file.to_str().unwrap()}).to_string();
        let result = reg.execute_json("read_file", &args).await.unwrap();
        assert_eq!(result, "content");
    }

    #[tokio::test]
    async fn registry_execute_json_unknown_tool_errors() {
        let reg = ToolRegistry { custom_tools: vec![] };
        let result = reg.execute_json("totally_unknown", "{}").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
    }

    #[tokio::test]
    async fn registry_execute_json_bad_args_errors() {
        let reg = ToolRegistry { custom_tools: vec![] };
        let result = reg.execute_json("read_file", "not json").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn registry_execute_json_non_string_value_stringified() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("t.txt");
        std::fs::write(&file, "hi").unwrap();
        let reg = ToolRegistry { custom_tools: vec![] };
        // path passed as a JSON string should work
        let args = format!("{{\"path\": \"{}\"}}", file.to_str().unwrap());
        let result = reg.execute_json("read_file", &args).await.unwrap();
        assert_eq!(result, "hi");
    }

    #[test]
    fn registry_load_errors_on_builtin_name_conflict() {
        let tmp = TempDir::new().unwrap();
        // Create a custom tool with a builtin name
        let yaml = "name: read_file\ndescription: conflict\ncommand: [echo]\n";
        std::fs::write(tmp.path().join("read_file.yml"), yaml).unwrap();
        let tools = load_from_dir(tmp.path()).unwrap();
        // Simulate the conflict check from ToolRegistry::load
        let has_conflict = tools.iter().any(|t| BUILTIN_TOOLS.iter().any(|b| b.name == t.def.name));
        assert!(has_conflict);
    }

    #[test]
    fn custom_tool_tool_schema() {
        let mut params = IndexMap::new();
        params.insert("query".to_string(), CustomToolParamDef {
            param_type: "string".into(),
            required: true,
            default: None,
            description: Some("search query".into()),
        });
        let tool = CustomTool {
            def: CustomToolDef {
                name: "search".into(),
                description: "searches".into(),
                parameters: params,
                command: CommandSpec::Single(vec!["echo".into()]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let schema = tool.tool_schema();
        assert_eq!(schema.name, "search");
        let required = schema.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "query"));
        assert_eq!(schema.parameters["properties"]["query"]["description"], "search query");
    }

    // ── ToolRegistry::custom_tools() accessor ─────────────────────────────────

    #[test]
    fn registry_custom_tools_accessor() {
        let tool = CustomTool {
            def: CustomToolDef {
                name: "acc".into(),
                description: "".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Single(vec!["echo".into()]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let reg = ToolRegistry { custom_tools: vec![tool] };
        assert_eq!(reg.custom_tools().len(), 1);
        assert_eq!(reg.custom_tools()[0].def.name, "acc");
    }

    // ── ToolRegistry::load() ──────────────────────────────────────────────────

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempHome {
        _dir: TempDir,
        orig: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let orig = std::env::var("HOME").ok();
            unsafe { std::env::set_var("HOME", dir.path()) };
            Self { _dir: dir, orig }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            unsafe {
                match &self.orig {
                    Some(h) => std::env::set_var("HOME", h),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    #[test]
    fn registry_load_no_tools_succeeds() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let reg = ToolRegistry::load().unwrap();
        assert!(reg.custom_tools().is_empty());

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn registry_load_global_tool_included() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let tools_dir = _home._dir.path().join(".cooper").join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(
            tools_dir.join("my_tool.yml"),
            "name: my_tool\ndescription: test\ncommand: [echo, hi]\n",
        ).unwrap();

        let reg = ToolRegistry::load().unwrap();
        assert!(reg.all_names().contains(&"my_tool".to_string()));

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn registry_load_project_tool_overrides_global() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let global_dir = _home._dir.path().join(".cooper").join("tools");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::write(
            global_dir.join("shared.yml"),
            "name: shared\ndescription: global version\ncommand: [echo, global]\n",
        ).unwrap();

        let project_dir = tmp_cwd.path().join(".agents").join("tools");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("shared.yml"),
            "name: shared\ndescription: project version\ncommand: [echo, project]\n",
        ).unwrap();

        let reg = ToolRegistry::load().unwrap();
        let shared_count = reg.custom_tools().iter().filter(|t| t.def.name == "shared").count();
        assert_eq!(shared_count, 1);
        assert_eq!(reg.find_custom("shared").unwrap().def.description, "project version");

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn registry_schemas_includes_custom() {
        let tool = CustomTool {
            def: CustomToolDef {
                name: "custom_schema".into(),
                description: "custom description".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Single(vec!["echo".into()]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let reg = ToolRegistry { custom_tools: vec![tool] };
        let schemas = reg.schemas();
        assert_eq!(schemas.len(), BUILTIN_TOOLS.len() + 1);
        let custom = schemas.last().unwrap();
        assert_eq!(custom.name, "custom_schema");
    }

    #[tokio::test]
    async fn registry_execute_json_dispatches_to_custom_tool() {
        let tool = make_custom_tool("my_echo", vec!["echo".into(), "custom-output".into()], IndexMap::new());
        let reg = ToolRegistry { custom_tools: vec![tool] };
        let result = reg.execute_json("my_echo", "{}").await.unwrap();
        assert!(result.contains("custom-output"));
    }

    #[tokio::test]
    async fn registry_execute_json_integer_value_coerced_to_string() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("num.txt");
        let reg = ToolRegistry { custom_tools: vec![] };
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "content": 42
        }).to_string();
        let result = reg.execute_json("write_file", &args).await.unwrap();
        assert!(result.contains("Written"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "42");
    }

    #[test]
    fn load_from_dir_skips_non_yml_non_dir_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "not a tool").unwrap();
        std::fs::write(tmp.path().join("config.json"), "{\"key\": \"val\"}").unwrap();
        let tools = load_from_dir(tmp.path()).unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn registry_load_builtin_conflict_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let tools_dir = _home._dir.path().join(".cooper").join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(
            tools_dir.join("read_file.yml"),
            "name: read_file\ndescription: conflict\ncommand: [echo]\n",
        ).unwrap();

        let result = ToolRegistry::load();
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("conflicts with a built-in"));

        std::env::set_current_dir(prev).unwrap();
    }

    // ── load_from_dir skips invalid definitions ───────────────────────────────

    #[test]
    fn load_from_dir_skips_invalid_yaml_with_warning() {
        let tmp = TempDir::new().unwrap();
        // Missing required fields (name, command) — serde will reject this
        std::fs::write(tmp.path().join("bad.yml"), "foo: bar\n").unwrap();
        let tools = load_from_dir(tmp.path()).unwrap();
        assert!(tools.is_empty());
    }

    // ── CustomTool env vars ───────────────────────────────────────────────────

    #[tokio::test]
    async fn custom_tool_env_var_injected_into_command() {
        let mut env = HashMap::new();
        env.insert("COOPER_TEST_GREETING".to_string(), "hello-from-env".to_string());
        let tool = CustomTool {
            def: CustomToolDef {
                name: "env-tool".into(),
                description: "".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Single(vec![
                    "sh".into(), "-c".into(), "echo $COOPER_TEST_GREETING".into(),
                ]),
                env,
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let result = tool.execute(&HashMap::new()).await.unwrap();
        assert!(result.trim().contains("hello-from-env"));
    }

    // ── Empty pipeline ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn custom_tool_empty_pipeline_returns_empty_string() {
        let tool = CustomTool {
            def: CustomToolDef {
                name: "empty-pipe".into(),
                description: "".into(),
                parameters: IndexMap::new(),
                command: CommandSpec::Pipeline(vec![]),
                env: HashMap::new(),
            },
            source: std::path::PathBuf::from("test.yml"),
        };
        let result = tool.execute(&HashMap::new()).await.unwrap();
        assert_eq!(result, "");
    }
}

impl cooper_core::ToolExecutor for ToolRegistry {
    fn schemas(&self) -> Vec<cooper_core::ToolSchema> {
        self.schemas()
    }

    async fn execute(&self, name: &str, args_json: &str) -> anyhow::Result<String> {
        self.execute_json(name, args_json).await
    }
}
