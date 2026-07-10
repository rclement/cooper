use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use cooper_core::agent;
use cooper_core::providers;

use crate::config;
use crate::sessions::{self, SessionRecord};
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

    fn on_tool_call(&self, tool_call: &agent::ToolCall) {
        println!("[tool call] {} {:?}\n", tool_call.name, tool_call.arguments)
    }

    fn on_message(&self, message: &agent::Message) {
        match message {
            agent::Message::Assistant {
                usage,
                reasoning_duration_ms,
                response_duration_ms,
                ..
            } => {
                if let Some(usage) = usage {
                    println!(
                        "\n\n[usage] prompt tokens = {}, completion tokens = {}, total tokens = {}\n",
                        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens,
                    );
                }
                if let Some(timing) = format_timing(*reasoning_duration_ms, *response_duration_ms) {
                    println!("{timing}\n");
                }
            }
            agent::Message::Tool {
                result,
                duration_ms,
                ..
            } => {
                let suffix = duration_ms
                    .map(|ms| format!(" ({})", format_duration(ms)))
                    .unwrap_or_default();
                match result {
                    Ok(output) => println!("[tool result]{suffix}\n{output}\n"),
                    Err(e) => println!("[tool error]{suffix}\n{e}\n"),
                }
            }
            _ => {}
        }
    }
}

/// Renders a duration the way a human reads it on a terminal: sub-second
/// durations as milliseconds (precise enough to matter), longer ones as
/// seconds with one decimal place (millisecond precision would just be noise).
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Builds the `[timing] reasoning = ..., response = ...` line from whichever
/// phases were actually timed, or `None` if neither was — a plain-text
/// response from a non-reasoning model has no reasoning phase to report, and
/// a message reconstructed from history before this metadata existed has
/// neither.
fn format_timing(reasoning_ms: Option<u64>, response_ms: Option<u64>) -> Option<String> {
    let parts: Vec<String> = [
        reasoning_ms.map(|ms| format!("reasoning = {}", format_duration(ms))),
        response_ms.map(|ms| format!("response = {}", format_duration(ms))),
    ]
    .into_iter()
    .flatten()
    .collect();

    if parts.is_empty() {
        None
    } else {
        Some(format!("[timing] {}", parts.join(", ")))
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
    /// Send a single one-shot prompt to the agent
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
    /// Start an interactive multi-turn conversation with the agent
    Chat {
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
        /// Resume a previously saved chat session by id (see `sessions list`)
        #[arg(long, short = 'r')]
        resume: Option<String>,
    },
    /// Manage saved chat sessions
    Sessions {
        #[command(subcommand)]
        action: SessionsCommand,
    },
    /// Serve the browser app (cross-origin isolated, with a same-origin git proxy)
    Web {
        /// Port to listen on
        #[arg(long, short = 'P', default_value_t = 8080)]
        port: u16,
        /// Address to bind — the default stays local-only; pass 0.0.0.0 to
        /// accept outside connections (e.g. from a container's port mapping)
        #[arg(long, short = 'H', default_value = "127.0.0.1")]
        host: String,
        /// Directory holding the web app (defaults to the `web/` directory of
        /// the checkout this binary was built from)
        #[arg(long, short = 'd')]
        dir: Option<std::path::PathBuf>,
    },
}

#[derive(clap::Subcommand)]
enum SessionsCommand {
    /// List saved chat sessions
    List,
    /// Print a saved session's full transcript
    Show {
        /// Session id
        id: String,
    },
}

/// Everything shared by `prompt` and `chat` to build one agent turn: the
/// resolved provider, the loaded agent-instructions/context-file content
/// (built once, reused on every turn of a `chat` session), the built-in
/// tool registry, and the current working directory.
struct AgentSetup {
    provider: Box<dyn providers::Provider>,
    provider_name: String,
    model_name: String,
    agent_instructions_content: Option<String>,
    context_files_content: HashMap<String, String>,
    tool_registry: HashMap<String, Box<dyn tools::Tool>>,
    current_working_dir: Option<String>,
}

fn setup_agent(
    provider_name: Option<String>,
    model_name: Option<String>,
    agent_instructions: Option<String>,
    context_files: Vec<String>,
) -> AgentSetup {
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

    let current_working_dir = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string());

    AgentSetup {
        provider,
        provider_name,
        model_name,
        agent_instructions_content,
        context_files_content,
        tool_registry,
        current_working_dir,
    }
}

async fn prompt_cmd(
    text: String,
    provider_name: Option<String>,
    model_name: Option<String>,
    agent_instructions: Option<String>,
    context_files: Vec<String>,
) {
    let setup = setup_agent(provider_name, model_name, agent_instructions, context_files);
    let chunk_handler = PrintHandler::new();
    let mut messages: Vec<agent::Message> = Vec::new();

    if let Err(e) = agent::agent_loop_stream(
        &mut messages,
        &text,
        None,
        setup.agent_instructions_content,
        &setup.context_files_content,
        setup.current_working_dir,
        &setup.tool_registry,
        setup.provider.as_ref(),
        &chunk_handler,
    )
    .await
    {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Prints a saved session's messages in the same `[reasoning]`/`[response]`/
/// `[tool call]`/`[tool result]` style `PrintHandler` uses live, so a
/// resumed or `sessions show`n session reads the same way a live run did.
fn print_transcript(messages: &[agent::Message]) {
    for message in messages {
        match message {
            agent::Message::System(text) => println!("[system]\n{text}\n"),
            agent::Message::User(text) => println!("[you] {text}\n"),
            agent::Message::Assistant {
                text,
                reasoning,
                tool_calls,
                reasoning_duration_ms,
                response_duration_ms,
                usage,
                ..
            } => {
                if let Some(r) = reasoning {
                    println!("[reasoning] {r}\n");
                }
                if let Some(t) = text {
                    println!("[response] {t}\n");
                }
                for tc in tool_calls {
                    println!("[tool call] {} {:?}\n", tc.name, tc.arguments);
                }
                if let Some(usage) = usage {
                    println!(
                        "[usage] prompt tokens = {}, completion tokens = {}, total tokens = {}\n",
                        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens,
                    );
                }
                if let Some(timing) = format_timing(*reasoning_duration_ms, *response_duration_ms) {
                    println!("{timing}\n");
                }
            }
            agent::Message::Tool {
                result,
                duration_ms,
                ..
            } => {
                let suffix = duration_ms
                    .map(|ms| format!(" ({})", format_duration(ms)))
                    .unwrap_or_default();
                match result {
                    Ok(output) => println!("[tool result]{suffix}\n{output}\n"),
                    Err(e) => println!("[tool error]{suffix}\n{e}\n"),
                }
            }
        }
    }
}

async fn chat_cmd(
    provider_name: Option<String>,
    model_name: Option<String>,
    agent_instructions: Option<String>,
    context_files: Vec<String>,
    resume: Option<String>,
) {
    let setup = setup_agent(provider_name, model_name, agent_instructions, context_files);
    let chunk_handler = PrintHandler::new();

    let mut session = match resume {
        Some(id) => match sessions::load(&id) {
            Ok(session) => {
                println!(
                    "Resuming session \"{}\" ({} messages so far)\n",
                    session.title,
                    session.history.len()
                );
                print_transcript(&session.history);
                session
            }
            Err(e) => {
                eprintln!("error loading session '{id}': {e}");
                std::process::exit(1);
            }
        },
        None => SessionRecord::new(setup.provider_name.clone(), setup.model_name.clone()),
    };

    println!(
        "Chat session started (id: {}). Type 'exit', 'quit', or an empty line to end it.",
        session.id
    );

    loop {
        print!("\n> ");
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => break, // EOF (e.g. Ctrl-D)
            Err(_) => break,
            Ok(_) => {}
        }

        let line = line.trim();
        if line.is_empty() || line == "exit" || line == "quit" {
            break;
        }

        if session.history.is_empty() && session.title.is_empty() {
            session.title = line.chars().take(80).collect();
        }

        if let Err(e) = agent::agent_loop_stream(
            &mut session.history,
            line,
            None,
            setup.agent_instructions_content.clone(),
            &setup.context_files_content,
            setup.current_working_dir.clone(),
            &setup.tool_registry,
            setup.provider.as_ref(),
            &chunk_handler,
        )
        .await
        {
            eprintln!("error: {e}");
            break;
        }

        session.touch();
        if let Err(e) = sessions::save(&session) {
            eprintln!("warning: failed to save session: {e}");
        }
    }
}

fn format_relative(now_ms: u64, ts_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(ts_ms) / 1000;
    if seconds < 60 {
        return "just now".to_string();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

fn sessions_list_cmd() {
    let saved = match sessions::list() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error listing sessions: {e}");
            std::process::exit(1);
        }
    };

    if saved.is_empty() {
        println!("No saved sessions yet. Start one with `cooper chat`.");
        return;
    }

    let now = sessions::now_unix();
    for session in &saved {
        let title = if session.title.is_empty() {
            "(untitled)"
        } else {
            &session.title
        };
        println!(
            "{}  {title:<40}  {} · {} · {} messages",
            session.id,
            format_relative(now, session.updated_at),
            session.model,
            session.history.len(),
        );
    }
}

fn sessions_show_cmd(id: String) {
    let session = match sessions::load(&id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error loading session '{id}': {e}");
            std::process::exit(1);
        }
    };

    let title = if session.title.is_empty() {
        "(untitled)"
    } else {
        &session.title
    };
    println!("Session: {title}");
    println!("Provider: {}  Model: {}\n", session.provider, session.model);
    print_transcript(&session.history);
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
        Command::Chat {
            provider,
            model,
            agent_instructions,
            context_file,
            resume,
        } => chat_cmd(provider, model, agent_instructions, context_file, resume).await,
        Command::Sessions { action } => match action {
            SessionsCommand::List => sessions_list_cmd(),
            SessionsCommand::Show { id } => sessions_show_cmd(id),
        },
        Command::Web { port, host, dir } => crate::web::web_cmd(host, port, dir).await,
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
    fn print_handler_reports_finalized_assistant_and_tool_messages_without_panicking() {
        let handler = PrintHandler::new();

        handler.on_tool_call(&agent::ToolCall {
            id: "1".to_string(),
            name: "echo".to_string(),
            arguments: HashMap::new(),
        });

        let mut assistant_reply = agent::Message::assistant(Some("done".to_string()), None, vec![]);
        if let agent::Message::Assistant {
            usage,
            reasoning_duration_ms,
            response_duration_ms,
            ..
        } = &mut assistant_reply
        {
            *usage = Some(agent::Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            });
            *reasoning_duration_ms = Some(1200);
            *response_duration_ms = Some(800);
        }
        handler.on_message(&assistant_reply);

        handler.on_message(&agent::Message::Tool {
            call_id: "1".to_string(),
            result: Ok("output".to_string()),
            duration_ms: Some(230),
            at_ms: None,
        });
        handler.on_message(&agent::Message::Tool {
            call_id: "1".to_string(),
            result: Err("failure".to_string()),
            duration_ms: None,
            at_ms: None,
        });
    }

    #[test]
    fn renders_session_age_the_way_a_human_scans_a_list() {
        let now = 1_000_000_000;
        let seconds = 1000;

        assert_eq!(format_relative(now, now - 30 * seconds), "just now");
        assert_eq!(format_relative(now, now - 5 * 60 * seconds), "5m ago");
        assert_eq!(format_relative(now, now - 3 * 3600 * seconds), "3h ago");
        assert_eq!(format_relative(now, now - 48 * 3600 * seconds), "2d ago");
    }

    #[test]
    fn a_session_saved_in_the_future_still_reads_as_just_now() {
        assert_eq!(format_relative(1000, 2000), "just now");
    }

    #[test]
    fn formats_timing_line_from_whichever_phases_were_measured() {
        assert_eq!(
            format_timing(Some(1200), Some(800)),
            Some("[timing] reasoning = 1.2s, response = 800ms".to_string())
        );
        assert_eq!(
            format_timing(None, Some(500)),
            Some("[timing] response = 500ms".to_string())
        );
        assert_eq!(format_timing(None, None), None);
    }
}
