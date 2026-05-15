use crate::config::{AgentInstructions, ResolvedConfig};
use crate::output::OutputChunk;
use crate::skills::SkillRegistry;
use crate::tools::ToolRegistry;
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use cooper_core::{ApiType, Message, Role, SessionLogger, Usage};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
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
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
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

    fn on_response(&mut self, thinking: Option<&str>, message: &Message, usage: Option<&Usage>) {
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
                usage: usage.cloned(),
            },
        ) {
            eprintln!("warning: could not write session response: {}", e);
        }
    }
}

// ── Skill-aware executor ──────────────────────────────────────────────────────

/// Wraps the tool registry and skill registry for a single turn.
/// Intercepts `activate_skill` calls, returning the skill body as the tool
/// result and storing it so the session can patch the system prompt afterward.
struct SessionRegistry<'a> {
    tools: &'a ToolRegistry,
    skills: &'a SkillRegistry,
    activated: RefCell<Option<String>>, // skill body if activated this turn
}

impl<'a> SessionRegistry<'a> {
    fn new(tools: &'a ToolRegistry, skills: &'a SkillRegistry) -> Self {
        Self {
            tools,
            skills,
            activated: RefCell::new(None),
        }
    }

    fn take_activated(&self) -> Option<String> {
        self.activated.borrow_mut().take()
    }
}

/// Builds the `activate_skill` tool schema from the available skill registry.
/// Returns `None` when no skills are registered (tool should not be exposed).
pub fn activate_skill_schema(skills: &SkillRegistry) -> Option<serde_json::Value> {
    let skill_list = skills.all();
    if skill_list.is_empty() {
        return None;
    }
    let catalog = skill_list
        .iter()
        .map(|s| {
            if s.description.is_empty() {
                s.name.clone()
            } else {
                format!("- {}: {}", s.name, s.description)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let valid_names: Vec<serde_json::Value> = skill_list
        .iter()
        .map(|s| serde_json::Value::String(s.name.clone()))
        .collect();
    Some(serde_json::json!({
        "type": "function",
        "function": {
            "name": "activate_skill",
            "description": format!(
                "Load specialized instructions for the current task. \
                Call this BEFORE starting work whenever the user's request matches a skill's domain — do not wait to be asked explicitly.\n\n\
                Available skills:\n{}",
                catalog
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "Name of the skill to activate",
                        "enum": valid_names
                    }
                },
                "required": ["skill"]
            }
        }
    }))
}

impl<'a> cooper_core::ToolExecutor for SessionRegistry<'a> {
    fn schemas(&self) -> Vec<serde_json::Value> {
        let mut schemas = self.tools.all_oai_schemas();
        if let Some(schema) = activate_skill_schema(self.skills) {
            schemas.push(schema);
        }
        schemas
    }

    async fn execute(&self, name: &str, args_json: &str) -> anyhow::Result<String> {
        if name == "activate_skill" {
            let args: serde_json::Value =
                serde_json::from_str(args_json).context("parsing activate_skill arguments")?;
            let skill_name = args["skill"]
                .as_str()
                .ok_or_else(|| anyhow!("missing 'skill' parameter"))?;
            let skill = self
                .skills
                .find(skill_name)
                .ok_or_else(|| anyhow!("skill '{}' not found", skill_name))?;
            // Store raw body for system prompt patching (unchanged by wrapping below)
            *self.activated.borrow_mut() = Some(skill.system_prompt.clone());

            // Only bundled (directory-based) skills have a dedicated directory to scan.
            // Flat skills live directly in a shared skills/ folder — scanning their parent
            // would expose sibling skill files as resources, which is wrong.
            let is_bundled = skill.source.file_name().and_then(|n| n.to_str()) == Some("skill.md");
            let skill_dir = if is_bundled {
                skill.source.parent()
            } else {
                None
            };

            let resources: Vec<String> = skill_dir
                .and_then(|dir| std::fs::read_dir(dir).ok())
                .map(|entries| {
                    let mut files: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().is_file() && e.path() != skill.source)
                        .filter_map(|e| e.file_name().to_str().map(str::to_string))
                        .collect();
                    files.sort();
                    files
                })
                .unwrap_or_default();

            let mut result = format!("<skill_content name=\"{}\">", skill_name);

            if !skill.system_prompt.is_empty() {
                result.push('\n');
                result.push_str(skill.system_prompt.trim_end());
            }

            if let Some(dir) = skill_dir {
                result.push_str(&format!("\n\nSkill directory: {}", dir.display()));
            }

            if !resources.is_empty() {
                result.push_str("\n\n<skill_resources>");
                for file in &resources {
                    result.push_str(&format!("\n  <file>{}</file>", file));
                }
                result.push_str("\n</skill_resources>");
            }

            result.push_str("\n</skill_content>");
            return Ok(result);
        }
        self.tools.execute_json(name, args_json).await
    }
}

// ── Multi-turn chat session ───────────────────────────────────────────────────

pub struct Session {
    messages: Vec<Message>,
    api_type: ApiType,
    base_url: String,
    api_key: String,
    model: String,
    logger: FileSessionLogger,
}

impl Session {
    /// Set up provider, build the system prompt, emit `SessionStart`, and create the session log.
    pub async fn start(
        system_prompt: Option<String>,
        active_skill: Option<String>,
        provider_name: Option<String>,
        model_name: Option<String>,
        config: &ResolvedConfig,
        tool_registry: &ToolRegistry,
        skill_registry: &SkillRegistry,
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

        let resolved_tools: Vec<String> = match &config.context.allowed_tools {
            None => tool_registry.all_names(),
            Some(names) => names.clone(),
        };

        let skill_names: Vec<String> = skill_registry
            .all()
            .iter()
            .map(|s| s.name.clone())
            .collect();

        // Mirror the tools Option semantics: None = no skill system active (nothing to show),
        // Some(names) = skill system is in play (show names or "(none)" if filtered to empty).
        let skills_display: Option<Vec<String>> =
            if skill_names.is_empty() && config.context.allowed_skills.is_none() {
                None
            } else {
                Some(skill_names)
            };

        on_chunk(OutputChunk::SessionStart {
            provider: provider_key.clone(),
            model: model.clone(),
            agent_instructions: instructions_entry.map(|(p, _)| p.to_string()),
            context_files: config.context.files.clone(),
            tools: Some(resolved_tools),
            skills: skills_display,
            active_skill,
        });

        let mut system = system_prompt.unwrap_or_else(|| config.system_prompt.clone());

        if !skill_registry.all().is_empty() {
            let names: Vec<&str> = skill_registry
                .all()
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            system.push_str(&format!(
                "\n\nYou have access to skill modules ({}) via the `activate_skill` tool. \
                Activate the most relevant skill at the start of any task that matches its domain.",
                names.join(", ")
            ));
        }

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
            api_type: provider.api.clone(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone().unwrap_or_default(),
            model,
            logger: FileSessionLogger {
                path,
                turn_start: None,
            },
        })
    }

    /// Returns the current system prompt content.
    pub fn system_prompt(&self) -> &str {
        self.messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or("")
    }

    /// Inject or replace the skill block in the system prompt.
    /// Replaces an existing `<skill-instructions>` block if present, otherwise appends one.
    /// Passing an empty `body` removes an existing block without adding a new one.
    pub fn inject_skill(&mut self, body: &str) {
        let Some(sys) = self.messages.first_mut() else {
            return;
        };
        const OPEN: &str = "\n\n<skill-instructions>\n";
        const CLOSE: &str = "\n</skill-instructions>";
        if let Some(start) = sys.content.find(OPEN) {
            if let Some(rel) = sys.content[start + OPEN.len()..].find(CLOSE) {
                let end = start + OPEN.len() + rel + CLOSE.len();
                let new_block = if body.is_empty() {
                    String::new()
                } else {
                    format!("{}{}{}", OPEN, body.trim_end(), CLOSE)
                };
                sys.content = format!(
                    "{}{}{}",
                    &sys.content[..start],
                    new_block,
                    &sys.content[end..]
                );
                return;
            }
        }
        if !body.is_empty() {
            sys.content
                .push_str(&format!("{}{}{}", OPEN, body.trim_end(), CLOSE));
        }
    }

    /// Send a user message and stream the response, keeping history for follow-up turns.
    /// If the model calls `activate_skill`, the skill body is injected into the system
    /// prompt so subsequent turns have it persistently.
    pub async fn send(
        &mut self,
        input: String,
        tool_registry: &ToolRegistry,
        skill_registry: &SkillRegistry,
        on_chunk: &mut dyn FnMut(OutputChunk),
    ) -> Result<String> {
        self.messages.push(Message::new(Role::User, input));
        let executor = SessionRegistry::new(tool_registry, skill_registry);
        let mut wrapped = |c: cooper_core::OutputChunk| on_chunk(OutputChunk::from(c));
        let result = cooper_core::agent::run_turn(
            &mut self.messages,
            &self.api_type,
            &self.base_url,
            &self.api_key,
            &self.model,
            &executor,
            Some(&mut self.logger),
            &mut wrapped,
        )
        .await?;

        if let Some(body) = executor.take_activated() {
            if let Some(sys) = self.messages.first_mut() {
                sys.content.push_str(&format!(
                    "\n\n<skill-instructions>\n{}\n</skill-instructions>",
                    body.trim_end()
                ));
            }
        }

        Ok(result)
    }
}

// ── Single-shot entry point (used by `cooper prompt`) ────────────────────────

pub async fn run(
    prompt: String,
    system_prompt: Option<String>,
    active_skill: Option<String>,
    provider_name: Option<String>,
    model_name: Option<String>,
    config: &ResolvedConfig,
    registry: &ToolRegistry,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
    let empty_skills = SkillRegistry::empty();
    let mut session = Session::start(
        system_prompt,
        active_skill,
        provider_name,
        model_name,
        config,
        registry,
        &empty_skills,
        on_chunk,
    )
    .await?;
    session
        .send(prompt, registry, &empty_skills, on_chunk)
        .await
}
