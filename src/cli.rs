use crate::{agent, config};
use anyhow::Result;
use clap::{Parser, Subcommand};

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
        /// Model to use
        #[arg(long)]
        model: Option<String>,
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
}

#[derive(Subcommand)]
enum ProvidersCommand {
    /// List all configured providers
    List,
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// Show resolved settings (global + project merged)
    Show,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Prompt { prompt, system_prompt, provider, model } => {
            let config = config::load()?;
            let response = agent::run(prompt, system_prompt, provider, model, &config).await?;
            println!("{}", response);
        }

        Command::Providers { subcommand: ProvidersCommand::List } => {
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
                if let Some(model) = &p.model {
                    println!("  model: {}", model);
                }
                if p.api_key.is_some() {
                    println!("  api_key: (set)");
                }
            }
        }

        Command::Settings { subcommand: SettingsCommand::Show } => {
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
                    if let Some(model) = &p.model {
                        println!("    model: {}", model);
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
