use crate::config::{
    API_TYPES, AgentInstructions, ApiType, ContextConfig, ModelConfig, ProviderConfig, RawConfig,
    ResolvedConfig, Scope,
};
use crate::output::OutputChunk;
use crate::skills::SkillRegistry;
use crate::tools::{self, ToolRegistry};
use crate::{agent, config};
use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use console::style;
use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
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
    /// Start an interactive chat session (default when no command is given)
    Chat {
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
    /// Run a single prompt against a model
    Prompt {
        /// The prompt to send
        prompt: String,
        /// Activate a skill's instructions for this prompt
        #[arg(long, value_name = "SKILL")]
        skill: Option<String>,
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
    /// Manage configuration
    Config {
        #[command(subcommand)]
        subcommand: ConfigCommand,
    },
    /// Manage model providers
    Providers {
        #[command(subcommand)]
        subcommand: ProvidersCommand,
    },
    /// Manage context settings
    Context {
        #[command(subcommand)]
        subcommand: ContextCommand,
    },
    /// Manage and run tools
    Tools {
        #[command(subcommand)]
        subcommand: ToolsCommand,
    },
    /// Manage and run skills
    Skills {
        #[command(subcommand)]
        subcommand: SkillsCommand,
    },
    /// Manage settings (alias for `config show`)
    #[command(hide = true)]
    Settings {
        #[command(subcommand)]
        subcommand: SettingsCommand,
    },
}

// ── Config subcommands ────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum ConfigCommand {
    /// Show configuration (resolved by default; --project for project-only)
    Show {
        /// Show only the project config (./cooper.yml), not the merged result
        #[arg(long)]
        project: bool,
    },
    /// Set a top-level configuration field
    Set {
        #[command(subcommand)]
        subcommand: ConfigSetCommand,
    },
    /// Unset a top-level configuration field
    Unset {
        #[command(subcommand)]
        subcommand: ConfigUnsetCommand,
    },
}

#[derive(Subcommand)]
enum ConfigSetCommand {
    /// Set the system prompt
    SystemPrompt {
        value: String,
        /// Store in ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Set the default provider
    DefaultProvider {
        name: String,
        /// Store in ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Set the default model
    DefaultModel {
        model_id: String,
        /// Store in ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
enum ConfigUnsetCommand {
    /// Unset the system prompt
    SystemPrompt {
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Unset the default provider
    DefaultProvider {
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Unset the default model
    DefaultModel {
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
}

// ── Providers subcommands ─────────────────────────────────────────────────────

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
    /// Remove a provider
    Remove {
        /// Provider name
        name: String,
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Interactively edit a provider (fields pre-filled with current values)
    Edit {
        /// Provider name
        name: String,
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Update specific fields of a provider non-interactively
    Set {
        /// Provider name
        name: String,
        /// New base URL
        #[arg(long)]
        base_url: Option<String>,
        /// New API type
        #[arg(long)]
        api: Option<String>,
        /// New API key (empty string to clear)
        #[arg(long)]
        api_key: Option<String>,
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Manage models for a provider
    Models {
        #[command(subcommand)]
        subcommand: ProvidersModelsCommand,
    },
}

#[derive(Subcommand)]
enum ProvidersModelsCommand {
    /// List models for a provider (from resolved config)
    List {
        /// Provider name
        provider: String,
    },
    /// Add a model ID to a provider
    Add {
        /// Provider name
        provider: String,
        /// Model ID to add
        model_id: String,
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
    /// Remove a model ID from a provider
    Remove {
        /// Provider name
        provider: String,
        /// Model ID to remove
        model_id: String,
        /// Target ./cooper.yml instead of global ~/.cooper/settings.yml
        #[arg(long)]
        project: bool,
    },
}

// ── Context subcommands ───────────────────────────────────────────────────────

#[derive(Subcommand)]
enum ContextCommand {
    /// Show resolved context (agent instructions, files, tools, skills)
    Show,
    /// Manage agent instructions setting
    AgentInstructions {
        #[command(subcommand)]
        subcommand: ContextAgentInstructionsCommand,
    },
    /// Manage context files
    Files {
        #[command(subcommand)]
        subcommand: ContextFilesCommand,
    },
    /// Manage allowed tools
    Tools {
        #[command(subcommand)]
        subcommand: ContextToolsCommand,
    },
    /// Manage allowed skills
    Skills {
        #[command(subcommand)]
        subcommand: ContextSkillsCommand,
    },
}

#[derive(Subcommand)]
enum ContextAgentInstructionsCommand {
    /// Enable agent instructions (load AGENTS.md)
    Enable {
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Disable agent instructions
    Disable {
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Load agent instructions from a specific file
    SetFile {
        path: String,
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum ContextFilesCommand {
    /// List context files (from resolved config)
    List {
        #[arg(long)]
        global: bool,
    },
    /// Add a file to the context
    Add {
        path: String,
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Remove a file from the context
    Remove {
        path: String,
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Clear all context files
    Clear {
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum ContextToolsCommand {
    /// List allowed tools (from resolved config)
    List {
        #[arg(long)]
        global: bool,
    },
    /// Allow a tool (transitions from all-allowed to an explicit list if needed)
    Allow {
        tool_name: String,
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Deny a tool (transitions from all-allowed to all-minus-this if needed)
    Deny {
        tool_name: String,
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Reset to allow all tools
    Reset {
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum ContextSkillsCommand {
    /// List allowed skills (from resolved config)
    List {
        #[arg(long)]
        global: bool,
    },
    /// Allow a skill (transitions from all-allowed to an explicit list if needed)
    Allow {
        skill_name: String,
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Deny a skill (transitions from all-allowed to all-minus-this if needed)
    Deny {
        skill_name: String,
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
    /// Reset to allow all skills
    Reset {
        /// Target global ~/.cooper/settings.yml instead of ./cooper.yml
        #[arg(long)]
        global: bool,
    },
}

// ── Existing subcommands (unchanged) ─────────────────────────────────────────

#[derive(Subcommand)]
enum SettingsCommand {
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

#[derive(Subcommand)]
enum SkillsCommand {
    /// List all available skills
    List,
}

// ── Main dispatch ─────────────────────────────────────────────────────────────

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Chat {
            system_prompt,
            provider,
            model,
            no_agent_instructions,
            agent_instructions,
        } => {
            run_chat(
                system_prompt,
                provider,
                model,
                no_agent_instructions,
                agent_instructions,
            )
            .await?;
        }

        Command::Prompt {
            prompt,
            skill,
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

            let (resolved_system, active_skill) = if let Some(skill_name) = skill {
                let skill_registry =
                    SkillRegistry::load_filtered(config.context.allowed_skills.as_deref())?;
                let skill = skill_registry
                    .find(&skill_name)
                    .ok_or_else(|| anyhow!("skill '{}' not found", skill_name))?;
                let mut s = system_prompt.unwrap_or_else(|| config.system_prompt.clone());
                if !skill.system_prompt.is_empty() {
                    s.push_str(&format!(
                        "\n\n<skill-instructions>\n{}\n</skill-instructions>",
                        skill.system_prompt.trim_end()
                    ));
                }
                (Some(s), Some(skill_name))
            } else {
                (system_prompt, None)
            };

            let registry = ToolRegistry::load()?;
            let mut printer = PhasePrinter::default();
            agent::run(
                prompt,
                resolved_system,
                active_skill,
                provider,
                model,
                &config,
                &registry,
                &mut |chunk| printer.print(chunk),
            )
            .await?;
            printer.finish();
        }

        Command::Config { subcommand } => match subcommand {
            ConfigCommand::Show { project } => {
                if project {
                    let raw = config::load_raw_scope(&Scope::Project)?;
                    display_raw_config(&raw, "project (./cooper.yml)");
                } else {
                    let cfg = config::load()?;
                    display_resolved_config(&cfg);
                }
            }
            ConfigCommand::Set { subcommand } => match subcommand {
                ConfigSetCommand::SystemPrompt { value, project } => {
                    let scope = config_scope(project);
                    config::update_config(&scope, |raw| {
                        raw.system_prompt = Some(value.clone());
                        Ok(())
                    })?;
                    println!("system_prompt set in {}.", scope_label(&scope));
                }
                ConfigSetCommand::DefaultProvider { name, project } => {
                    let scope = config_scope(project);
                    config::update_config(&scope, |raw| {
                        raw.default_provider = Some(name.clone());
                        Ok(())
                    })?;
                    println!(
                        "default_provider set to '{}' in {}.",
                        name,
                        scope_label(&scope)
                    );
                }
                ConfigSetCommand::DefaultModel { model_id, project } => {
                    let scope = config_scope(project);
                    config::update_config(&scope, |raw| {
                        raw.default_model = Some(model_id.clone());
                        Ok(())
                    })?;
                    println!(
                        "default_model set to '{}' in {}.",
                        model_id,
                        scope_label(&scope)
                    );
                }
            },
            ConfigCommand::Unset { subcommand } => match subcommand {
                ConfigUnsetCommand::SystemPrompt { project } => {
                    let scope = config_scope(project);
                    config::update_config(&scope, |raw| {
                        raw.system_prompt = None;
                        Ok(())
                    })?;
                    println!("system_prompt unset in {}.", scope_label(&scope));
                }
                ConfigUnsetCommand::DefaultProvider { project } => {
                    let scope = config_scope(project);
                    config::update_config(&scope, |raw| {
                        raw.default_provider = None;
                        Ok(())
                    })?;
                    println!("default_provider unset in {}.", scope_label(&scope));
                }
                ConfigUnsetCommand::DefaultModel { project } => {
                    let scope = config_scope(project);
                    config::update_config(&scope, |raw| {
                        raw.default_model = None;
                        Ok(())
                    })?;
                    println!("default_model unset in {}.", scope_label(&scope));
                }
            },
        },

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
                let scope = config_scope(project);
                providers_add(name, base_url, api, models, api_key, scope)?;
            }

            ProvidersCommand::Remove { name, project } => {
                let scope = config_scope(project);
                providers_remove(&name, scope)?;
            }

            ProvidersCommand::Edit { name, project } => {
                let scope = config_scope(project);
                providers_edit(&name, scope)?;
            }

            ProvidersCommand::Set {
                name,
                base_url,
                api,
                api_key,
                project,
            } => {
                let scope = config_scope(project);
                providers_set(&name, base_url, api, api_key, scope)?;
            }

            ProvidersCommand::Models { subcommand } => match subcommand {
                ProvidersModelsCommand::List { provider } => {
                    let cfg = config::load()?;
                    let p = cfg
                        .providers
                        .get(&provider)
                        .ok_or_else(|| anyhow!("provider '{}' not found", provider))?;
                    if p.models.is_empty() {
                        println!("No models configured for '{}'.", provider);
                    } else {
                        for m in &p.models {
                            println!("{}", m.id);
                        }
                    }
                }

                ProvidersModelsCommand::Add {
                    provider,
                    model_id,
                    project,
                } => {
                    let scope = config_scope(project);
                    let sl = scope_label(&scope);
                    config::update_config(&scope, |raw| {
                        let p = raw
                            .providers
                            .as_mut()
                            .and_then(|ps| ps.get_mut(&provider))
                            .ok_or_else(|| {
                                anyhow!("provider '{}' not found in {}", provider, sl)
                            })?;
                        if p.models.iter().any(|m| m.id == model_id) {
                            return Err(anyhow!(
                                "model '{}' already exists in provider '{}'",
                                model_id,
                                provider
                            ));
                        }
                        p.models.push(ModelConfig {
                            id: model_id.clone(),
                        });
                        Ok(())
                    })?;
                    println!(
                        "Model '{}' added to provider '{}' in {}.",
                        model_id, provider, sl
                    );
                }

                ProvidersModelsCommand::Remove {
                    provider,
                    model_id,
                    project,
                } => {
                    let scope = config_scope(project);
                    let sl = scope_label(&scope);
                    config::update_config(&scope, |raw| {
                        let p = raw
                            .providers
                            .as_mut()
                            .and_then(|ps| ps.get_mut(&provider))
                            .ok_or_else(|| {
                                anyhow!("provider '{}' not found in {}", provider, sl)
                            })?;
                        let len_before = p.models.len();
                        p.models.retain(|m| m.id != model_id);
                        if p.models.len() == len_before {
                            return Err(anyhow!(
                                "model '{}' not found in provider '{}'",
                                model_id,
                                provider
                            ));
                        }
                        Ok(())
                    })?;
                    println!(
                        "Model '{}' removed from provider '{}' in {}.",
                        model_id, provider, sl
                    );
                }
            },
        },

        Command::Context { subcommand } => match subcommand {
            ContextCommand::Show => {
                let cfg = config::load()?;
                let ctx = &cfg.context;
                match &ctx.agent_instructions {
                    None | Some(AgentInstructions::Enabled(true)) => {
                        println!("agent_instructions: AGENTS.md (default)")
                    }
                    Some(AgentInstructions::Enabled(false)) => {
                        println!("agent_instructions: disabled")
                    }
                    Some(AgentInstructions::File(f)) => {
                        println!("agent_instructions: {}", f)
                    }
                }
                if ctx.files.is_empty() {
                    println!("files: (none)");
                } else {
                    println!("files:");
                    for f in &ctx.files {
                        println!("  - {}", f);
                    }
                }
                match &ctx.allowed_tools {
                    None => println!("allowed_tools: (all)"),
                    Some(t) if t.is_empty() => println!("allowed_tools: (none)"),
                    Some(t) => {
                        println!("allowed_tools:");
                        for n in t {
                            println!("  - {}", n);
                        }
                    }
                }
                match &ctx.allowed_skills {
                    None => println!("allowed_skills: (all)"),
                    Some(s) if s.is_empty() => println!("allowed_skills: (none)"),
                    Some(s) => {
                        println!("allowed_skills:");
                        for n in s {
                            println!("  - {}", n);
                        }
                    }
                }
            }

            ContextCommand::AgentInstructions { subcommand } => match subcommand {
                ContextAgentInstructionsCommand::Enable { global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        raw.context
                            .get_or_insert_with(ContextConfig::default)
                            .agent_instructions = Some(AgentInstructions::Enabled(true));
                        Ok(())
                    })?;
                    println!("agent_instructions enabled in {}.", scope_label(&scope));
                }
                ContextAgentInstructionsCommand::Disable { global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        raw.context
                            .get_or_insert_with(ContextConfig::default)
                            .agent_instructions = Some(AgentInstructions::Enabled(false));
                        Ok(())
                    })?;
                    println!("agent_instructions disabled in {}.", scope_label(&scope));
                }
                ContextAgentInstructionsCommand::SetFile { path, global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        raw.context
                            .get_or_insert_with(ContextConfig::default)
                            .agent_instructions = Some(AgentInstructions::File(path.clone()));
                        Ok(())
                    })?;
                    println!(
                        "agent_instructions set to '{}' in {}.",
                        path,
                        scope_label(&scope)
                    );
                }
            },

            ContextCommand::Files { subcommand } => match subcommand {
                ContextFilesCommand::List { .. } => {
                    let cfg = config::load()?;
                    if cfg.context.files.is_empty() {
                        println!("(none)");
                    } else {
                        for f in &cfg.context.files {
                            println!("{}", f);
                        }
                    }
                }
                ContextFilesCommand::Add { path, global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        raw.context
                            .get_or_insert_with(ContextConfig::default)
                            .files
                            .push(path.clone());
                        Ok(())
                    })?;
                    println!("Added '{}' to files in {}.", path, scope_label(&scope));
                }
                ContextFilesCommand::Remove { path, global } => {
                    let scope = context_scope(global);
                    let sl = scope_label(&scope);
                    config::update_config(&scope, |raw| {
                        let files =
                            &mut raw.context.get_or_insert_with(ContextConfig::default).files;
                        let len_before = files.len();
                        files.retain(|f| f != &path);
                        if files.len() == len_before {
                            return Err(anyhow!("file '{}' not found in {}", path, sl));
                        }
                        Ok(())
                    })?;
                    println!("Removed '{}' from files in {}.", path, sl);
                }
                ContextFilesCommand::Clear { global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        raw.context
                            .get_or_insert_with(ContextConfig::default)
                            .files
                            .clear();
                        Ok(())
                    })?;
                    println!("Cleared files in {}.", scope_label(&scope));
                }
            },

            ContextCommand::Tools { subcommand } => match subcommand {
                ContextToolsCommand::List { .. } => {
                    let cfg = config::load()?;
                    match &cfg.context.allowed_tools {
                        None => println!("(all)"),
                        Some(t) if t.is_empty() => println!("(none)"),
                        Some(t) => {
                            for n in t {
                                println!("{}", n);
                            }
                        }
                    }
                }
                ContextToolsCommand::Allow { tool_name, global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        let allowed = &mut raw
                            .context
                            .get_or_insert_with(ContextConfig::default)
                            .allowed_tools;
                        match allowed {
                            None => *allowed = Some(vec![tool_name.clone()]),
                            Some(list) => {
                                if !list.contains(&tool_name) {
                                    list.push(tool_name.clone());
                                }
                            }
                        }
                        Ok(())
                    })?;
                    println!("Allowed tool '{}' in {}.", tool_name, scope_label(&scope));
                }
                ContextToolsCommand::Deny { tool_name, global } => {
                    let scope = context_scope(global);
                    let sl = scope_label(&scope);
                    config::update_config(&scope, |raw| {
                        let ctx = raw.context.get_or_insert_with(ContextConfig::default);
                        match &mut ctx.allowed_tools {
                            None => {
                                let registry = ToolRegistry::load()?;
                                let mut all_names = registry.all_names();
                                all_names.retain(|n| n != &tool_name);
                                ctx.allowed_tools = Some(all_names);
                            }
                            Some(list) if list.is_empty() => {
                                return Err(anyhow!(
                                    "no tools are currently allowed, nothing to deny"
                                ));
                            }
                            Some(list) => {
                                let len_before = list.len();
                                list.retain(|n| n != &tool_name);
                                if list.len() == len_before {
                                    return Err(anyhow!(
                                        "tool '{}' is not in the allowed list in {}",
                                        tool_name,
                                        sl
                                    ));
                                }
                            }
                        }
                        Ok(())
                    })?;
                    println!("Denied tool '{}' in {}.", tool_name, sl);
                }
                ContextToolsCommand::Reset { global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        raw.context
                            .get_or_insert_with(ContextConfig::default)
                            .allowed_tools = None;
                        Ok(())
                    })?;
                    println!("All tools allowed in {}.", scope_label(&scope));
                }
            },

            ContextCommand::Skills { subcommand } => match subcommand {
                ContextSkillsCommand::List { .. } => {
                    let cfg = config::load()?;
                    match &cfg.context.allowed_skills {
                        None => println!("(all)"),
                        Some(s) if s.is_empty() => println!("(none)"),
                        Some(s) => {
                            for n in s {
                                println!("{}", n);
                            }
                        }
                    }
                }
                ContextSkillsCommand::Allow { skill_name, global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        let allowed = &mut raw
                            .context
                            .get_or_insert_with(ContextConfig::default)
                            .allowed_skills;
                        match allowed {
                            None => *allowed = Some(vec![skill_name.clone()]),
                            Some(list) => {
                                if !list.contains(&skill_name) {
                                    list.push(skill_name.clone());
                                }
                            }
                        }
                        Ok(())
                    })?;
                    println!("Allowed skill '{}' in {}.", skill_name, scope_label(&scope));
                }
                ContextSkillsCommand::Deny { skill_name, global } => {
                    let scope = context_scope(global);
                    let sl = scope_label(&scope);
                    config::update_config(&scope, |raw| {
                        let ctx = raw.context.get_or_insert_with(ContextConfig::default);
                        match &mut ctx.allowed_skills {
                            None => {
                                let registry = SkillRegistry::load()?;
                                let mut all_names: Vec<String> =
                                    registry.all().iter().map(|s| s.name.clone()).collect();
                                all_names.retain(|n| n != &skill_name);
                                ctx.allowed_skills = Some(all_names);
                            }
                            Some(list) if list.is_empty() => {
                                return Err(anyhow!(
                                    "no skills are currently allowed, nothing to deny"
                                ));
                            }
                            Some(list) => {
                                let len_before = list.len();
                                list.retain(|n| n != &skill_name);
                                if list.len() == len_before {
                                    return Err(anyhow!(
                                        "skill '{}' is not in the allowed list in {}",
                                        skill_name,
                                        sl
                                    ));
                                }
                            }
                        }
                        Ok(())
                    })?;
                    println!("Denied skill '{}' in {}.", skill_name, sl);
                }
                ContextSkillsCommand::Reset { global } => {
                    let scope = context_scope(global);
                    config::update_config(&scope, |raw| {
                        raw.context
                            .get_or_insert_with(ContextConfig::default)
                            .allowed_skills = None;
                        Ok(())
                    })?;
                    println!("All skills allowed in {}.", scope_label(&scope));
                }
            },
        },

        Command::Tools { subcommand } => match subcommand {
            ToolsCommand::List => {
                let config = config::load()?;
                let registry = ToolRegistry::load()?;
                let skill_registry =
                    SkillRegistry::load_filtered(config.context.allowed_skills.as_deref())?;
                if let Some(schema) = agent::activate_skill_schema(&skill_registry) {
                    let f = &schema["function"];
                    let name = f["name"].as_str().unwrap_or("activate_skill");
                    let desc = f["description"].as_str().unwrap_or("");
                    println!("{:<20}  {}", style(name).bold(), style("[meta]").dim());
                    for line in desc.lines() {
                        println!("  {}", line);
                    }
                    if let Some(props) = f["parameters"]["properties"].as_object() {
                        for (pname, pval) in props {
                            let ptype = pval["type"].as_str().unwrap_or("string");
                            println!("  --{:<18} <{}>  (required)", pname, ptype);
                            if let Some(variants) = pval["enum"].as_array() {
                                let names: Vec<&str> =
                                    variants.iter().filter_map(|v| v.as_str()).collect();
                                println!("    enum: {}", names.join(", "));
                            }
                        }
                    }
                    println!();
                }
                let allowed_tools = config.context.allowed_tools.as_deref();
                for tool in tools::BUILTIN_TOOLS {
                    if allowed_tools.is_some_and(|a| !a.iter().any(|n| n == tool.name)) {
                        continue;
                    }
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
                    if allowed_tools.is_some_and(|a| !a.iter().any(|n| n == &tool.def.name)) {
                        continue;
                    }
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

        Command::Skills { subcommand } => match subcommand {
            SkillsCommand::List => {
                let config = config::load()?;
                let registry =
                    SkillRegistry::load_filtered(config.context.allowed_skills.as_deref())?;
                if registry.all().is_empty() {
                    println!("No skills found.");
                    return Ok(());
                }
                for skill in registry.all() {
                    let src = display_source(&skill.source);
                    println!(
                        "{:<20} {}  {}",
                        style(&skill.name).bold(),
                        skill.description,
                        style(format!("[{}]", src)).dim()
                    );
                }
            }
        },

        Command::Settings {
            subcommand: SettingsCommand::Show,
        } => {
            let cfg = config::load()?;
            display_resolved_config(&cfg);
        }
    }

    Ok(())
}

// ── Chat session ──────────────────────────────────────────────────────────────

async fn run_chat(
    system_prompt: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    no_agent_instructions: bool,
    agent_instructions: Option<String>,
) -> Result<()> {
    let mut config = config::load()?;
    if no_agent_instructions {
        config.context.agent_instructions = Some(AgentInstructions::Enabled(false));
    } else if let Some(file) = agent_instructions {
        config.context.agent_instructions = Some(AgentInstructions::File(file));
    }
    let tool_registry = ToolRegistry::load()?;
    let skill_registry = SkillRegistry::load_filtered(config.context.allowed_skills.as_deref())?;

    let mut start_printer = PhasePrinter::default();
    let mut session = agent::Session::start(
        system_prompt,
        None,
        provider,
        model,
        &config,
        &tool_registry,
        &skill_registry,
        &mut |chunk| start_printer.print(chunk),
    )
    .await?;

    let stdin = io::stdin();
    loop {
        print!("{} ", style(">").cyan().bold());
        io::stdout().flush()?;

        let mut line = String::new();
        let n = stdin.lock().read_line(&mut line)?;
        if n == 0 {
            break;
        }

        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }
        if matches!(input.as_str(), "/exit" | "/quit") {
            break;
        }

        if input == "/context" {
            let prompt = session.system_prompt();
            let stdout = io::stdout();
            let mut out = stdout.lock();
            let _ = writeln!(out, "{}", style("─── system prompt ───").dim());
            let _ = writeln!(out, "{}", prompt);
            let _ = writeln!(out, "{}", style("─────────────────────").dim());
            continue;
        }

        if let Some(skill_name) = input.strip_prefix("/skill:") {
            let skill_name = skill_name.trim();
            match skill_registry.find(skill_name) {
                Some(skill) => {
                    session.inject_skill(&skill.system_prompt);
                    println!(
                        "{}",
                        style(format!("Skill '{}' activated.", skill_name)).dim()
                    );
                }
                None => {
                    eprintln!("skill '{}' not found", skill_name);
                    let names: Vec<&str> = skill_registry
                        .all()
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect();
                    if names.is_empty() {
                        eprintln!("No skills are available.");
                    } else {
                        eprintln!("Available skills: {}", names.join(", "));
                    }
                }
            }
            continue;
        }

        let mut printer = PhasePrinter::default();
        session
            .send(input, &tool_registry, &skill_registry, &mut |chunk| {
                printer.print(chunk)
            })
            .await?;
        printer.finish();
    }

    println!("{}", style("bye").dim());
    Ok(())
}

// ── Provider helpers ──────────────────────────────────────────────────────────

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

fn providers_remove(name: &str, scope: Scope) -> Result<()> {
    let sl = scope_label(&scope);
    config::update_config(&scope, |raw| {
        let providers = raw
            .providers
            .as_mut()
            .ok_or_else(|| anyhow!("no providers configured in {}", sl))?;
        if providers.remove(name).is_none() {
            return Err(anyhow!("provider '{}' not found in {}", name, sl));
        }
        Ok(())
    })?;
    println!("Provider '{}' removed from {}.", name, sl);
    Ok(())
}

fn providers_edit(name: &str, scope: Scope) -> Result<()> {
    let raw = config::load_raw_scope(&scope)?;
    let current = raw
        .providers
        .as_ref()
        .and_then(|p| p.get(name))
        .ok_or_else(|| anyhow!("provider '{}' not found in {}", name, scope_label(&scope)))?
        .clone();

    let updated = providers_edit_interactive(name, &current)?;
    config::save_provider(name, updated, &scope)?;
    println!("Provider '{}' updated in {}.", name, scope_label(&scope));
    Ok(())
}

fn providers_edit_interactive(name: &str, current: &ProviderConfig) -> Result<ProviderConfig> {
    let theme = ColorfulTheme::default();
    println!("Editing provider '{}'", name);

    let base_url: String = Input::with_theme(&theme)
        .with_prompt("Base URL")
        .with_initial_text(&current.base_url)
        .interact_text()?;

    let current_api_str = current.api.to_string();
    let current_api_idx = API_TYPES
        .iter()
        .position(|&t| t == current_api_str)
        .unwrap_or(0);
    let api_idx = Select::with_theme(&theme)
        .with_prompt("API type")
        .items(API_TYPES)
        .default(current_api_idx)
        .interact()?;
    let api = ApiType::from_str(API_TYPES[api_idx])?;

    let current_key = current.api_key.as_deref().unwrap_or("");
    let api_key: String = Input::with_theme(&theme)
        .with_prompt("API key (leave empty to clear)")
        .with_initial_text(current_key)
        .allow_empty(true)
        .interact_text()?;

    let mut models: Vec<ModelConfig> = if !current.models.is_empty() {
        let model_ids: Vec<&str> = current.models.iter().map(|m| m.id.as_str()).collect();
        println!("  Current models: {}", model_ids.join(", "));
        let keep = Confirm::with_theme(&theme)
            .with_prompt("Keep existing models?")
            .default(true)
            .interact()?;
        if keep {
            current.models.clone()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

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

    Ok(ProviderConfig {
        base_url,
        api,
        models,
        api_key: if api_key.is_empty() {
            None
        } else {
            Some(api_key)
        },
    })
}

fn providers_set(
    name: &str,
    base_url: Option<String>,
    api: Option<String>,
    api_key: Option<String>,
    scope: Scope,
) -> Result<()> {
    if base_url.is_none() && api.is_none() && api_key.is_none() {
        return Err(anyhow!(
            "at least one of --base-url, --api, --api-key is required"
        ));
    }
    let sl = scope_label(&scope);
    config::update_config(&scope, |raw| {
        let p = raw
            .providers
            .as_mut()
            .and_then(|ps| ps.get_mut(name))
            .ok_or_else(|| anyhow!("provider '{}' not found in {}", name, sl))?;
        if let Some(url) = base_url {
            p.base_url = url;
        }
        if let Some(api_str) = api {
            p.api = ApiType::from_str(&api_str)?;
        }
        if let Some(key) = api_key {
            p.api_key = if key.is_empty() { None } else { Some(key) };
        }
        Ok(())
    })?;
    println!("Provider '{}' updated in {}.", name, sl);
    Ok(())
}

// ── Display helpers ───────────────────────────────────────────────────────────

fn display_resolved_config(config: &ResolvedConfig) {
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
    match &config.context.allowed_skills {
        None => println!("context.allowed_skills: (all)"),
        Some(skills) if skills.is_empty() => println!("context.allowed_skills: (none)"),
        Some(skills) => {
            println!("context.allowed_skills:");
            for s in skills {
                println!("  - {}", s);
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

fn display_raw_config(raw: &RawConfig, label: &str) {
    println!("# {}", label);
    println!(
        "system_prompt: {}",
        raw.system_prompt.as_deref().unwrap_or("(not set)")
    );
    println!(
        "default_provider: {}",
        raw.default_provider.as_deref().unwrap_or("(not set)")
    );
    println!(
        "default_model: {}",
        raw.default_model.as_deref().unwrap_or("(not set)")
    );
    match &raw.context {
        None => {
            println!("context.agent_instructions: (not set)");
            println!("context.files: (not set)");
            println!("context.allowed_tools: (not set)");
            println!("context.allowed_skills: (not set)");
        }
        Some(ctx) => {
            match &ctx.agent_instructions {
                None => println!("context.agent_instructions: (not set)"),
                Some(AgentInstructions::Enabled(true)) => {
                    println!("context.agent_instructions: enabled")
                }
                Some(AgentInstructions::Enabled(false)) => {
                    println!("context.agent_instructions: disabled")
                }
                Some(AgentInstructions::File(f)) => {
                    println!("context.agent_instructions: {}", f)
                }
            }
            if ctx.files.is_empty() {
                println!("context.files: (none)");
            } else {
                println!("context.files:");
                for f in &ctx.files {
                    println!("  - {}", f);
                }
            }
            match &ctx.allowed_tools {
                None => println!("context.allowed_tools: (not set / all)"),
                Some(t) if t.is_empty() => println!("context.allowed_tools: (none)"),
                Some(t) => {
                    println!("context.allowed_tools:");
                    for n in t {
                        println!("  - {}", n);
                    }
                }
            }
            match &ctx.allowed_skills {
                None => println!("context.allowed_skills: (not set / all)"),
                Some(s) if s.is_empty() => println!("context.allowed_skills: (none)"),
                Some(s) => {
                    println!("context.allowed_skills:");
                    for n in s {
                        println!("  - {}", n);
                    }
                }
            }
        }
    }
    match &raw.providers {
        None => println!("providers: (none)"),
        Some(providers) if providers.is_empty() => println!("providers: (none)"),
        Some(providers) => {
            println!("providers:");
            let mut names: Vec<&String> = providers.keys().collect();
            names.sort();
            for name in names {
                let p = &providers[name];
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

// ── Misc helpers ──────────────────────────────────────────────────────────────

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

/// Scope for config/providers commands: default global, --project for project.
fn config_scope(project: bool) -> Scope {
    if project {
        Scope::Project
    } else {
        Scope::Global
    }
}

/// Scope for context commands: default project, --global for global.
fn context_scope(global: bool) -> Scope {
    if global {
        Scope::Global
    } else {
        Scope::Project
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
                skills,
                active_skill,
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
                match (active_skill, skills.as_deref()) {
                    (Some(name), _) => parts.push(format!("skill: {}", name)),
                    (None, Some([])) => parts.push("skills: (none)".to_string()),
                    (None, Some(names)) => parts.push(format!("skills: {}", names.join(", "))),
                    (None, None) => {}
                }
                if !parts.is_empty() {
                    let _ = writeln!(out, "{}", style(parts.join("  ·  ")).dim());
                }
            }
            OutputChunk::Thinking { text } => {
                if self.phase != Phase::Thinking {
                    let _ = writeln!(out, "{}", style("thinking…").dim().italic());
                    self.phase = Phase::Thinking;
                }
                let _ = write!(out, "{}", style(&text).dim());
                let _ = out.flush();
            }
            OutputChunk::Content { text } => {
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
            OutputChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } => {
                let _ = writeln!(
                    out,
                    "{}",
                    style(format!(
                        "[{} in · {} out · {} total tokens]",
                        prompt_tokens, completion_tokens, total_tokens
                    ))
                    .dim()
                );
            }
        }
    }

    fn finish(&self) {
        println!();
    }
}
