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

pub fn oai_schema(tool: &Tool) -> Value {
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
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": {
                "type": "object",
                "properties": Value::Object(properties),
                "required": required_params,
            }
        }
    })
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
    fn oai_schema(&self) -> Value {
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
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.def.name,
                "description": self.def.description,
                "parameters": {
                    "type": "object",
                    "properties": Value::Object(properties),
                    "required": required_params,
                }
            }
        })
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
    custom_tools: Vec<CustomTool>,
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

    pub fn all_oai_schemas(&self) -> Vec<Value> {
        let mut schemas: Vec<Value> = BUILTIN_TOOLS.iter().map(oai_schema).collect();
        schemas.extend(self.custom_tools.iter().map(|t| t.oai_schema()));
        schemas
    }

    pub fn schemas_for(&self, names: &[String]) -> Vec<Value> {
        let mut schemas = Vec::new();
        for tool in BUILTIN_TOOLS {
            if names.iter().any(|n| n == tool.name) {
                schemas.push(oai_schema(tool));
            }
        }
        for tool in &self.custom_tools {
            if names.iter().any(|n| n == &tool.def.name) {
                schemas.push(tool.oai_schema());
            }
        }
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

impl cooper_core::ToolExecutor for ToolRegistry {
    fn schemas(&self) -> Vec<serde_json::Value> {
        self.all_oai_schemas()
    }

    async fn execute(&self, name: &str, args_json: &str) -> anyhow::Result<String> {
        self.execute_json(name, args_json).await
    }
}
