use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use cooper_core::agent;
use cooper_core::providers;

use crate::config;
use crate::tools;

struct PrintHandler {
    reasoning: AtomicBool,
}

impl PrintHandler {
    fn new() -> Self {
        PrintHandler {
            reasoning: false.into(),
        }
    }
}

impl agent::AgentEventsHandler for PrintHandler {
    fn on_chunk(&self, chunk: &agent::AgentMessageChunk) {
        if let Some(t) = &chunk.text {
            if self.reasoning.load(Ordering::Relaxed) {
                self.reasoning.store(false, Ordering::Relaxed);
                print!("\n\n[response] ")
            }
            print!("{}", t);
        }
        if let Some(r) = &chunk.reasoning {
            if !self.reasoning.load(Ordering::Relaxed) {
                self.reasoning.store(true, Ordering::Relaxed);
                print!("\n\n[reasoning] ");
            }
            print!("{}", r);
        }
        let _ = std::io::stdout().flush();
    }

    fn on_complete(&self, usage: &agent::Usage) {
        println!(
            "\n\n[usage] prompt tokens = {}, completion tokens = {}, total tokens = {}\n",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens,
        );
    }

    fn on_tool_call(&self, tool_call: &agent::ToolCall) {
        println!("[tool call] {} {:?}\n", tool_call.name, tool_call.arguments)
    }

    fn on_tool_result(&self, tool_result: &Result<String, String>) {
        match tool_result {
            Ok(output) => println!("[tool result]\n{}\n", output),
            Err(e) => println!("[tool error]\n{}\n", e),
        }
    }
}

/// Agent Cooper is a special AI agent
#[derive(clap::Parser)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Send a prompt to the agent
    Prompt {
        /// Text to send
        text: String,
        /// Provider name
        #[arg(long, short = 'p')]
        provider: Option<String>,
        /// Model name
        #[arg(long, short = 'm')]
        model: Option<String>,
        /// Agent instructions filepath
        #[arg(long, short = 'i')]
        agent_instructions: Option<String>,
        /// Additional context files
        #[arg(long, short = 'c', num_args = 0..)]
        context_file: Vec<String>,
    },
}

async fn prompt_cmd(
    text: String,
    provider_name: Option<String>,
    model_name: Option<String>,
    agent_instructions: Option<String>,
    context_files: Vec<String>,
) {
    let config = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            std::process::exit(1);
        }
    };

    let provider_name = provider_name.unwrap_or_else(|| config.default_provider.clone());
    let model_name = model_name.unwrap_or(config.default_model);

    println!("provider: {provider_name}");
    println!("model: {model_name}");

    let provider_config = match config.providers.get(&provider_name) {
        Some(p) => p,
        None => {
            eprintln!("provider '{}' not found in config", provider_name);
            std::process::exit(1);
        }
    };

    if !provider_config.models.iter().any(|m| m.id == model_name) {
        eprintln!(
            "model '{}' not found in provider '{}'",
            model_name, provider_name
        );
        std::process::exit(1);
    }

    let provider: Box<dyn providers::Provider> = match provider_config.provider_type.as_str() {
        "openai-completions" => Box::new(providers::openai_completions::OpenAICompletionsAPI::new(
            &provider_config.base_url,
            &provider_config.api_key,
            &model_name,
        )),
        _ => {
            eprintln!("unknown provider type: {}", provider_config.provider_type);
            std::process::exit(1);
        }
    };

    let agent_instructions_content = if let Some(path) = agent_instructions {
        match std::fs::read_to_string(&path) {
            Ok(contents) => Some(contents),
            Err(e) => {
                eprintln!("failed to read agent instructions file '{path}': {e}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let context_files_content: HashMap<String, String> = context_files
        .iter()
        .filter_map(|path| match std::fs::read_to_string(path) {
            Ok(contents) => Some((path.clone(), contents)),
            Err(e) => {
                eprintln!("failed to read context file '{path}': {e}");
                None
            }
        })
        .collect();

    let builtin_tools: Vec<Box<dyn tools::Tool>> = vec![
        Box::new(tools::ListFilesTool),
        Box::new(tools::ReadFileTool),
        Box::new(tools::ExecCmdTool),
    ];
    let mut tool_registry: HashMap<String, Box<dyn tools::Tool>> = HashMap::new();
    for tool in builtin_tools {
        tool_registry.insert(tool.schema().name.clone(), tool);
    }

    let chunk_handler = PrintHandler::new();

    let current_working_dir = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string());

    match agent::agent_loop_stream(
        &text,
        agent_instructions_content,
        &context_files_content,
        current_working_dir,
        &tool_registry,
        provider.as_ref(),
        &chunk_handler,
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn run() {
    let args = Args::parse();

    match args.command {
        Command::Prompt {
            text,
            provider,
            model,
            agent_instructions,
            context_file,
        } => prompt_cmd(text, provider, model, agent_instructions, context_file).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent::AgentEventsHandler;

    #[test]
    fn print_handler_starts_in_response_mode() {
        let handler = PrintHandler::new();

        assert!(!handler.reasoning.load(Ordering::Relaxed));
    }

    #[test]
    fn print_handler_switches_to_reasoning_mode_on_reasoning_chunk() {
        let handler = PrintHandler::new();

        handler.on_chunk(&agent::AgentMessageChunk {
            text: None,
            reasoning: Some("thinking".to_string()),
        });

        assert!(handler.reasoning.load(Ordering::Relaxed));
    }

    #[test]
    fn print_handler_switches_back_to_response_mode_on_text_chunk() {
        let handler = PrintHandler::new();
        handler.on_chunk(&agent::AgentMessageChunk {
            text: None,
            reasoning: Some("thinking".to_string()),
        });

        handler.on_chunk(&agent::AgentMessageChunk {
            text: Some("answer".to_string()),
            reasoning: None,
        });

        assert!(!handler.reasoning.load(Ordering::Relaxed));
    }

    #[test]
    fn print_handler_ignores_empty_chunk() {
        let handler = PrintHandler::new();

        handler.on_chunk(&agent::AgentMessageChunk {
            text: None,
            reasoning: None,
        });

        assert!(!handler.reasoning.load(Ordering::Relaxed));
    }

    #[test]
    fn print_handler_callbacks_do_not_panic() {
        let handler = PrintHandler::new();

        handler.on_complete(&agent::Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        });
        handler.on_tool_call(&agent::ToolCall {
            id: "1".to_string(),
            name: "echo".to_string(),
            arguments: HashMap::new(),
        });
        handler.on_tool_result(&Ok("output".to_string()));
        handler.on_tool_result(&Err("failure".to_string()));
    }
}
