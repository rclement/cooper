use crate::config::{AgentInstructions, ResolvedConfig};
use crate::providers::OutputChunk;
use crate::tools::ToolRegistry;
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use cooper_core::{Message, Role, SessionLogger};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;
use uuid::Uuid;

// ── Session logging ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionEntry {
    Session {
        id: String,
        provider: String,
        model: String,
        project: String,
        started_at: String,
    },
    Request {
        messages: Vec<Message>,
    },
    Response {
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        message: Message,
        duration_ms: u64,
    },
}

fn session_file(session_id: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let cwd = std::env::current_dir().context("getting current directory")?;
    let slug = cwd
        .to_string_lossy()
        .replace('/', "-")
        .trim_start_matches('-')
        .to_string();
    let dir = home.join(".cooper").join("sessions").join(slug);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating session directory {}", dir.display()))?;
    Ok(dir.join(format!("{}.jsonl", session_id)))
}

fn append(path: &PathBuf, entry: &SessionEntry) -> Result<()> {
    let line = serde_json::to_string(entry).context("serializing session entry")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening session file {}", path.display()))?;
    writeln!(file, "{}", line).context("writing session entry")
}

struct FileSessionLogger {
    path: PathBuf,
    turn_start: Option<Instant>,
}

impl SessionLogger for FileSessionLogger {
    fn on_request(&mut self, messages: &[Message]) {
        self.turn_start = Some(Instant::now());
        if let Err(e) = append(
            &self.path,
            &SessionEntry::Request {
                messages: messages.to_vec(),
            },
        ) {
            eprintln!("warning: could not write session request: {}", e);
        }
    }

    fn on_response(&mut self, thinking: Option<&str>, message: &Message) {
        let duration_ms = self
            .turn_start
            .take()
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);
        if let Err(e) = append(
            &self.path,
            &SessionEntry::Response {
                thinking: thinking.map(str::to_string),
                message: message.clone(),
                duration_ms,
            },
        ) {
            eprintln!("warning: could not write session response: {}", e);
        }
    }
}

// ── Multi-turn chat session ───────────────────────────────────────────────────

pub struct Session {
    messages: Vec<Message>,
    base_url: String,
    api_key: String,
    model: String,
    logger: FileSessionLogger,
}

impl Session {
    /// Set up provider, build the system prompt, emit `SessionStart`, and create the session log.
    pub async fn start(
        system_prompt: Option<String>,
        provider_name: Option<String>,
        model_name: Option<String>,
        config: &ResolvedConfig,
        registry: &ToolRegistry,
        on_chunk: &mut dyn FnMut(OutputChunk),
    ) -> Result<Self> {
        let provider_key = provider_name
            .or_else(|| config.default_provider.clone())
            .ok_or_else(|| {
                anyhow!("no provider specified; set default_provider in config or use --provider")
            })?;

        let provider = config
            .providers
            .get(&provider_key)
            .ok_or_else(|| anyhow!("provider '{}' not found in configuration", provider_key))?;

        let model = model_name
            .or_else(|| provider.models.first().map(|m| m.id.clone()))
            .or_else(|| config.default_model.clone())
            .ok_or_else(|| {
                anyhow!("no model specified; add a model to the provider config or use --model")
            })?;

        let instructions_entry = match &config.context.agent_instructions {
            None | Some(AgentInstructions::Enabled(true)) => Some(("AGENTS.md", false)),
            Some(AgentInstructions::Enabled(false)) => None,
            Some(AgentInstructions::File(name)) => Some((name.as_str(), true)),
        };

        let tool_schemas_for_display = match &config.context.allowed_tools {
            None => registry.all_oai_schemas(),
            Some(names) => registry.schemas_for(names),
        };
        drop(tool_schemas_for_display);

        on_chunk(OutputChunk::SessionStart {
            provider: provider_key.clone(),
            model: model.clone(),
            agent_instructions: instructions_entry.map(|(p, _)| p.to_string()),
            context_files: config.context.files.clone(),
            tools: config.context.allowed_tools.clone(),
        });

        let mut system = system_prompt.unwrap_or_else(|| config.system_prompt.clone());

        if let Some((path, warn_if_missing)) = instructions_entry {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    system.push_str(&format!(
                        "\n\n<agent-instructions>\n{}\n</agent-instructions>",
                        content.trim_end()
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if warn_if_missing {
                        eprintln!("warning: agent instructions file not found: {}", path);
                    }
                }
                Err(e) => eprintln!("warning: could not read agent instructions {}: {}", path, e),
            }
        }

        if !config.context.files.is_empty() {
            let mut file_context = String::new();
            for file_path in &config.context.files {
                match std::fs::read_to_string(file_path) {
                    Ok(content) => {
                        file_context.push_str(&format!(
                            "<file path=\"{}\">\n{}\n</file>\n",
                            file_path, content
                        ));
                    }
                    Err(e) => {
                        eprintln!("warning: could not read context file {}: {}", file_path, e);
                    }
                }
            }
            if !file_context.is_empty() {
                system.push_str(&format!("\n\n<context>\n{}</context>", file_context));
            }
        }

        let session_id = Uuid::new_v4().to_string();
        let cwd = std::env::current_dir().context("getting current directory")?;
        let path = session_file(&session_id)?;
        append(
            &path,
            &SessionEntry::Session {
                id: session_id,
                provider: provider_key,
                model: model.clone(),
                project: cwd.to_string_lossy().to_string(),
                started_at: Utc::now().to_rfc3339(),
            },
        )?;

        Ok(Session {
            messages: vec![Message::new(Role::System, system)],
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone().unwrap_or_default(),
            model,
            logger: FileSessionLogger {
                path,
                turn_start: None,
            },
        })
    }

    /// Send a user message and stream the response, keeping history for follow-up turns.
    pub async fn send(
        &mut self,
        input: String,
        registry: &ToolRegistry,
        on_chunk: &mut dyn FnMut(OutputChunk),
    ) -> Result<String> {
        self.messages.push(Message::new(Role::User, input));
        let mut wrapped = |c: cooper_core::OutputChunk| on_chunk(OutputChunk::from(c));
        cooper_core::agent::run_turn(
            &mut self.messages,
            &self.base_url,
            &self.api_key,
            &self.model,
            registry,
            Some(&mut self.logger),
            &mut wrapped,
        )
        .await
    }
}

// ── Single-shot entry point (used by `cooper prompt` and `cooper skills run`) ─

pub async fn run(
    prompt: String,
    system_prompt: Option<String>,
    provider_name: Option<String>,
    model_name: Option<String>,
    config: &ResolvedConfig,
    registry: &ToolRegistry,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
    let mut session =
        Session::start(system_prompt, provider_name, model_name, config, registry, on_chunk)
            .await?;
    session.send(prompt, registry, on_chunk).await
}
