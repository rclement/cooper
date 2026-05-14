use crate::config::{API_TYPES, AgentInstructions, ApiType, ModelConfig, ProviderConfig, Scope};
use crate::providers::OutputChunk;
use crate::tools::{self, ToolRegistry};
use crate::{agent, config};
use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use console::style;
use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::str::FromStr;

#[derive(Parser)]
#[command(name = "cooper", about = "...damn good agent")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a single prompt against a model
    Prompt {
        /// The prompt to send
        prompt: String,
        /// Override the system prompt
        #[arg(long)]
        system_prompt: Option<String>,
        /// Provider to use
        #[arg(long)]
        provider: Option<String>,
        /// Model ID to use
        #[arg(long)]
        model: Option<String>,
        /// Disable loading agent instructions (AGENTS.md)
        #[arg(long, conflicts_with = "agent_instructions")]
        no_agent_instructions: bool,
        /// Load agent instructions from a custom file instead of AGENTS.md
        #[arg(long, value_name = "FILE")]
        agent_instructions: Option<String>,
    },
    /// Manage model providers
    Providers {
        #[command(subcommand)]
        subcommand: ProvidersCommand,
    },
    /// Manage settings
    Settings {
        #[command(subcommand)]
        subcommand: SettingsCommand,
    },
    /// Manage and run tools
    Tools {
        #[command(subcommand)]
        subcommand: ToolsCommand,
    },
}

#[derive(Subcommand)]
enum ProvidersCommand {
    /// List all configured providers
    List,
    /// Add a new provider (interactive if no args given)
    Add {
        /// Provider name
        #[arg(long)]
        name: Option<String>,
        /// Base URL of the provider API
        #[arg(long)]
        base_url: Option<String>,
        /// API type [default: openai-completions]
        #[arg(long)]
        api: Option<String>,
        /// Add a model by ID (can be given multiple times)
        #[arg(long = "model", value_name = "MODEL_ID")]
        models: Vec<String>,
        /// API key
        #[arg(long)]
        api_key: Option<String>,
        /// Store in ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// Show resolved settings (global + project merged)
    Show,
}

#[derive(Subcommand)]
enum ToolsCommand {
    /// List all available tools
    List,
    /// Run a tool with --param value arguments
    Run {
        /// Tool name
        tool_name: String,
        /// Parameters as --key value pairs
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        params: Vec<String>,
    },
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Prompt {
            prompt,
            system_prompt,
            provider,
            model,
            no_agent_instructions,
            agent_instructions,
        } => {
            let mut config = config::load()?;
            if no_agent_instructions {
                config.context.agent_instructions = Some(AgentInstructions::Enabled(false));
            } else if let Some(file) = agent_instructions {
                config.context.agent_instructions = Some(AgentInstructions::File(file));
            }
            let registry = ToolRegistry::load()?;
            let mut printer = PhasePrinter::default();
            agent::run(
                prompt,
                system_prompt,
                provider,
                model,
                &config,
                &registry,
                &mut |chunk| printer.print(chunk),
            )
            .await?;
            printer.finish();
        }

        Command::Providers { subcommand } => match subcommand {
            ProvidersCommand::List => {
                let config = config::load()?;
                if config.providers.is_empty() {
                    println!("No providers configured.");
                    return Ok(());
                }
                let mut names: Vec<&String> = config.providers.keys().collect();
                names.sort();
                for name in names {
                    let p = &config.providers[name];
                    let marker = if config.default_provider.as_deref() == Some(name) {
                        " (default)"
                    } else {
                        ""
                    };
                    println!("{}{}", name, marker);
                    println!("  base_url: {}", p.base_url);
                    println!("  api: {}", p.api);
                    if !p.models.is_empty() {
                        println!("  models:");
                        for m in &p.models {
                            println!("    - {}", m.id);
                        }
                    }
                    if p.api_key.is_some() {
                        println!("  api_key: (set)");
                    }
                }
            }

            ProvidersCommand::Add {
                name,
                base_url,
                api,
                models,
                api_key,
                project,
            } => {
                let scope = if project {
                    Scope::Project
                } else {
                    Scope::Global
                };
                providers_add(name, base_url, api, models, api_key, scope)?;
            }
        },

        Command::Tools { subcommand } => match subcommand {
            ToolsCommand::List => {
                let registry = ToolRegistry::load()?;
                for tool in tools::BUILTIN_TOOLS {
                    println!(
                        "{:<20} {}  {}",
                        style(tool.name).bold(),
                        tool.description,
                        style("[builtin]").dim()
                    );
                    for p in tool.params {
                        let req = if p.required { " (required)" } else { "" };
                        let def = p
                            .default
                            .map(|d| format!(" [default: {}]", d))
                            .unwrap_or_default();
                        println!("  --{:<18} <{}>{}{}", p.name, p.type_, req, def);
                    }
                }
                for tool in registry.custom_tools() {
                    let src = display_source(&tool.source);
                    println!(
                        "{:<20} {}  {}",
                        style(&tool.def.name).bold(),
                        tool.def.description,
                        style(format!("[{}]", src)).dim()
                    );
                    for (name, p) in &tool.def.parameters {
                        let req = if p.required { " (required)" } else { "" };
                        let def = p
                            .default
                            .as_ref()
                            .map(|d| format!(" [default: {}]", d))
                            .unwrap_or_default();
                        println!("  --{:<18} <{}>{}{}", name, p.param_type, req, def);
                    }
                }
            }
            ToolsCommand::Run { tool_name, params } => {
                let registry = ToolRegistry::load()?;
                let args = parse_tool_params(&params)?;
                let args_json = serde_json::to_string(&args)?;
                let output = registry.execute_json(&tool_name, &args_json).await?;
                print!("{}", output);
                if !output.ends_with('\n') {
                    println!();
                }
            }
        },

        Command::Settings {
            subcommand: SettingsCommand::Show,
        } => {
            let config = config::load()?;
            println!("system_prompt: {}", config.system_prompt);
            println!(
                "default_provider: {}",
                config.default_provider.as_deref().unwrap_or("(not set)")
            );
            println!(
                "default_model: {}",
                config.default_model.as_deref().unwrap_or("(not set)")
            );
            match &config.context.agent_instructions {
                None | Some(AgentInstructions::Enabled(true)) => {
                    println!("context.agent_instructions: AGENTS.md (default)")
                }
                Some(AgentInstructions::Enabled(false)) => {
                    println!("context.agent_instructions: disabled")
                }
                Some(AgentInstructions::File(f)) => {
                    println!("context.agent_instructions: {}", f)
                }
            }
            if config.context.files.is_empty() {
                println!("context.files: (none)");
            } else {
                println!("context.files:");
                for f in &config.context.files {
                    println!("  - {}", f);
                }
            }
            match &config.context.allowed_tools {
                None => println!("context.allowed_tools: (all)"),
                Some(tools) if tools.is_empty() => println!("context.allowed_tools: (none)"),
                Some(tools) => {
                    println!("context.allowed_tools:");
                    for t in tools {
                        println!("  - {}", t);
                    }
                }
            }
            if config.providers.is_empty() {
                println!("providers: (none)");
            } else {
                println!("providers:");
                let mut names: Vec<&String> = config.providers.keys().collect();
                names.sort();
                for name in names {
                    let p = &config.providers[name];
                    println!("  {}:", name);
                    println!("    base_url: {}", p.base_url);
                    println!("    api: {}", p.api);
                    if !p.models.is_empty() {
                        println!("    models:");
                        for m in &p.models {
                            println!("      - id: {}", m.id);
                        }
                    }
                    if p.api_key.is_some() {
                        println!("    api_key: (set)");
                    }
                }
            }
        }
    }

    Ok(())
}

fn providers_add(
    name: Option<String>,
    base_url: Option<String>,
    api: Option<String>,
    models: Vec<String>,
    api_key: Option<String>,
    scope: Scope,
) -> Result<()> {
    match name {
        None => {
            if let Some((provider_name, provider_cfg)) = providers_add_interactive(&scope)? {
                config::save_provider(&provider_name, provider_cfg, &scope)?;
                println!(
                    "Provider '{}' saved to {}.",
                    provider_name,
                    scope_label(&scope)
                );
            }
        }
        Some(name) => {
            let base_url = base_url
                .ok_or_else(|| anyhow!("--base-url is required in non-interactive mode"))?;
            let api_type = api
                .as_deref()
                .map(ApiType::from_str)
                .transpose()?
                .unwrap_or_default();
            let provider_cfg = ProviderConfig {
                base_url,
                api: api_type,
                models: models.into_iter().map(|id| ModelConfig { id }).collect(),
                api_key,
            };

            if config::provider_exists_in_scope(&name, &scope)? {
                return Err(anyhow!(
                    "provider '{}' already exists in {}",
                    name,
                    scope_label(&scope),
                ));
            }

            config::save_provider(&name, provider_cfg, &scope)?;
            println!("Provider '{}' saved to {}.", name, scope_label(&scope));
        }
    }
    Ok(())
}

/// Returns `None` if the user aborted (declined to overwrite an existing provider).
fn providers_add_interactive(scope: &Scope) -> Result<Option<(String, ProviderConfig)>> {
    let theme = ColorfulTheme::default();

    let name: String = Input::with_theme(&theme)
        .with_prompt("Provider name")
        .interact_text()?;

    // Check for duplicates right after getting the name so the user is not
    // forced to fill every field before learning the provider already exists.
    if config::provider_exists_in_scope(&name, scope)? {
        let overwrite = Confirm::with_theme(&theme)
            .with_prompt(format!(
                "Provider '{}' already exists in {}. Overwrite?",
                name,
                scope_label(scope)
            ))
            .default(false)
            .interact()?;
        if !overwrite {
            return Ok(None);
        }
    }

    let base_url: String = Input::with_theme(&theme)
        .with_prompt("Base URL")
        .interact_text()?;

    let api_idx = Select::with_theme(&theme)
        .with_prompt("API type")
        .items(API_TYPES)
        .default(0)
        .interact()?;
    let api = ApiType::from_str(API_TYPES[api_idx])?;

    let api_key: String = Input::with_theme(&theme)
        .with_prompt("API key (leave empty to skip)")
        .allow_empty(true)
        .interact_text()?;

    // Collect model IDs one at a time until the user leaves input empty.
    let mut models: Vec<ModelConfig> = Vec::new();
    loop {
        let prompt = if models.is_empty() {
            "Model ID (leave empty to skip)".to_string()
        } else {
            format!(
                "Another model ID (leave empty to stop, {} so far)",
                models.len()
            )
        };
        let model_id: String = Input::with_theme(&theme)
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text()?;
        if model_id.is_empty() {
            break;
        }
        models.push(ModelConfig { id: model_id });
    }

    Ok(Some((
        name,
        ProviderConfig {
            base_url,
            api,
            models,
            api_key: if api_key.is_empty() {
                None
            } else {
                Some(api_key)
            },
        },
    )))
}

fn parse_tool_params(raw: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let mut i = 0;
    while i < raw.len() {
        let key = raw[i]
            .strip_prefix("--")
            .ok_or_else(|| anyhow!("expected --param_name, got: {}", raw[i]))?;
        i += 1;
        let value = raw
            .get(i)
            .ok_or_else(|| anyhow!("missing value for --{}", key))?
            .clone();
        i += 1;
        map.insert(key.to_string(), value);
    }
    Ok(map)
}

fn display_source(source: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = source.strip_prefix(&cwd) {
            return rel.to_string_lossy().into_owned();
        }
    }
    source.to_string_lossy().into_owned()
}

fn scope_label(scope: &Scope) -> &'static str {
    match scope {
        Scope::Global => "global settings (~/.cooper/settings.yml)",
        Scope::Project => "project settings (cooper.yml)",
    }
}

// ── Streaming output display ──────────────────────────────────────────────────

#[derive(Default, PartialEq)]
enum Phase {
    #[default]
    Start,
    Thinking,
    Content,
}

/// Prints streamed output chunks with distinct styling per phase.
#[derive(Default)]
struct PhasePrinter {
    phase: Phase,
}

impl PhasePrinter {
    fn print(&mut self, chunk: OutputChunk) {
        let stdout = io::stdout();
        let mut out = stdout.lock();

        match chunk {
            OutputChunk::SessionStart {
                provider,
                model,
                agent_instructions,
                context_files,
                tools,
            } => {
                let _ = writeln!(out, "{}", style(format!("{} / {}", provider, model)).dim());
                let mut parts: Vec<String> = Vec::new();
                if let Some(path) = agent_instructions {
                    parts.push(format!("instructions: {}", path));
                }
                if !context_files.is_empty() {
                    parts.push(format!("files: {}", context_files.join(", ")));
                }
                match tools.as_deref() {
                    None => {}
                    Some([]) => parts.push("tools: (none)".to_string()),
                    Some(names) => parts.push(format!("tools: {}", names.join(", "))),
                }
                if !parts.is_empty() {
                    let _ = writeln!(out, "{}", style(parts.join("  ·  ")).dim());
                }
            }
            OutputChunk::Thinking(text) => {
                if self.phase != Phase::Thinking {
                    let _ = writeln!(out, "{}", style("thinking…").dim().italic());
                    self.phase = Phase::Thinking;
                }
                let _ = write!(out, "{}", style(&text).dim());
                let _ = out.flush();
            }
            OutputChunk::Content(text) => {
                if self.phase == Phase::Thinking {
                    let _ = writeln!(out);
                    let _ = writeln!(out, "{}", style("───").dim());
                }
                self.phase = Phase::Content;
                let _ = write!(out, "{}", text);
                let _ = out.flush();
            }
            OutputChunk::ToolCall { name, args } => {
                if self.phase == Phase::Thinking {
                    let _ = writeln!(out);
                }
                self.phase = Phase::Content;
                let _ = writeln!(
                    out,
                    "{} {}({})",
                    style("→").cyan().bold(),
                    style(&name).cyan(),
                    style(&args).dim()
                );
                let _ = out.flush();
            }
            OutputChunk::ToolResult { name, output } => {
                let lines = output.trim_end().lines().count();
                let preview = if lines <= 1 {
                    output.trim_end().to_string()
                } else {
                    format!("({} lines)", lines)
                };
                let _ = writeln!(
                    out,
                    "{} {} {}",
                    style("←").cyan(),
                    style(&name).dim(),
                    style(&preview).dim()
                );
                let _ = out.flush();
            }
        }
    }

    fn finish(&self) {
        // Ensure the prompt returns to a clean line regardless of whether the
        // response ended with a newline.
        println!();
    }
}
