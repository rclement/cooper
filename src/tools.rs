use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

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

pub fn all_oai_schemas() -> Vec<Value> {
    BUILTIN_TOOLS.iter().map(oai_schema).collect()
}

pub fn execute_json(name: &str, args_json: &str) -> Result<String> {
    let tool = find(name).ok_or_else(|| anyhow!("unknown tool: {}", name))?;
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
    execute(tool, &args)
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
