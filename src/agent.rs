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

        let agent_instructions = if let Some((path, warn_if_missing)) = instructions_entry {
            match std::fs::read_to_string(path) {
                Ok(content) => Some(content),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if warn_if_missing {
                        eprintln!("warning: agent instructions file not found: {}", path);
                    }
                    None
                }
                Err(e) => {
                    eprintln!("warning: could not read agent instructions {}: {}", path, e);
                    None
                }
            }
        } else {
            None
        };

        let context_files = config
            .context
            .files
            .iter()
            .filter_map(|path| match std::fs::read_to_string(path) {
                Ok(content) => Some(cooper_core::system_prompt::ContextFile {
                    path: path.clone(),
                    content,
                }),
                Err(e) => {
                    eprintln!("warning: could not read context file {}: {}", path, e);
                    None
                }
            })
            .collect();

        let cwd = std::env::current_dir().context("getting current directory")?;

        let system = cooper_core::system_prompt::build(cooper_core::system_prompt::Options {
            base: system_prompt.unwrap_or_else(|| config.system_prompt.clone()),
            date: Some(Utc::now().format("%Y-%m-%d").to_string()),
            cwd: Some(cwd.display().to_string()),
            skills: skill_registry
                .all()
                .iter()
                .map(|s| cooper_core::system_prompt::SkillInfo {
                    name: s.name.clone(),
                    description: s.description.clone(),
                })
                .collect(),
            agent_instructions,
            context_files,
        });

        let session_id = Uuid::new_v4().to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiType, ContextConfig, ModelConfig, ProviderConfig, ResolvedConfig};
    use crate::skills::SkillRegistry;
    use crate::tools::ToolRegistry;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempHome {
        _dir: TempDir,
        orig: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let orig = std::env::var("HOME").ok();
            // SAFETY: serialised by ENV_LOCK — no concurrent env reads in these tests.
            unsafe { std::env::set_var("HOME", dir.path()) };
            Self { _dir: dir, orig }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            // SAFETY: serialised by ENV_LOCK.
            unsafe {
                match &self.orig {
                    Some(h) => std::env::set_var("HOME", h),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    fn make_config(base_url: &str) -> ResolvedConfig {
        let mut providers = HashMap::new();
        providers.insert("test".to_string(), ProviderConfig {
            base_url: base_url.to_string(),
            api: ApiType::OpenaiCompletions,
            api_key: Some("key".to_string()),
            models: vec![ModelConfig { id: "gpt-test".to_string() }],
        });
        ResolvedConfig {
            system_prompt: "You are helpful.".to_string(),
            providers,
            default_provider: Some("test".to_string()),
            default_model: None,
            context: ContextConfig::default(),
        }
    }

    fn oai_text_sse(text: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}],\"usage\":null}}\ndata: [DONE]\n")
    }

    // ── Session::inject_skill ─────────────────────────────────────────────────

    fn dummy_session(system: &str) -> Session {
        Session {
            messages: vec![cooper_core::Message::new(cooper_core::Role::System, system)],
            api_type: ApiType::OpenaiCompletions,
            base_url: "http://localhost".into(),
            api_key: "key".into(),
            model: "model".into(),
            logger: FileSessionLogger {
                path: std::path::PathBuf::from("/dev/null"),
                turn_start: None,
            },
        }
    }

    #[test]
    fn system_prompt_returns_first_message_content() {
        let s = dummy_session("be helpful");
        assert_eq!(s.system_prompt(), "be helpful");
    }

    #[test]
    fn inject_skill_appends_block() {
        let mut s = dummy_session("base");
        s.inject_skill("skill instructions");
        assert!(s.system_prompt().contains("<skill-instructions>"));
        assert!(s.system_prompt().contains("skill instructions"));
        assert!(s.system_prompt().contains("</skill-instructions>"));
    }

    #[test]
    fn inject_skill_replaces_existing_block() {
        let mut s = dummy_session("base");
        s.inject_skill("first");
        s.inject_skill("second");
        let prompt = s.system_prompt();
        assert!(!prompt.contains("first"));
        assert!(prompt.contains("second"));
        // Only one block present
        assert_eq!(prompt.matches("<skill-instructions>").count(), 1);
    }

    #[test]
    fn inject_skill_empty_body_removes_block() {
        let mut s = dummy_session("base");
        s.inject_skill("some skill");
        s.inject_skill("");
        let prompt = s.system_prompt();
        assert!(!prompt.contains("<skill-instructions>"));
    }

    #[test]
    fn inject_skill_empty_body_no_block_is_noop() {
        let mut s = dummy_session("base");
        s.inject_skill(""); // no block to remove — should not panic or append
        assert_eq!(s.system_prompt(), "base");
    }

    #[test]
    fn inject_skill_trims_trailing_whitespace() {
        let mut s = dummy_session("base");
        s.inject_skill("content   \n\n");
        let prompt = s.system_prompt();
        // trim_end should have removed trailing whitespace before </skill-instructions>
        assert!(!prompt.contains("content   \n\n</skill-instructions>"));
        assert!(prompt.contains("content"));
    }

    // ── activate_skill_schema ─────────────────────────────────────────────────

    #[test]
    fn activate_skill_schema_empty_registry_returns_none() {
        let reg = SkillRegistry::empty();
        assert!(activate_skill_schema(&reg).is_none());
    }

    #[test]
    fn activate_skill_schema_with_skills() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("coding.md"),
            "---\nname: coding\ndescription: helps with code\n---\nDo code.",
        ).unwrap();
        let skills = crate::skills::SkillRegistry {
            skills: crate::skills::load_from_dir_pub(tmp.path()).unwrap(),
        };
        let schema = activate_skill_schema(&skills).unwrap();
        assert_eq!(schema["function"]["name"], "activate_skill");
        let variants = schema["function"]["parameters"]["properties"]["skill"]["enum"].as_array().unwrap();
        assert!(variants.iter().any(|v| v == "coding"));
    }

    #[test]
    fn activate_skill_schema_skill_with_description_formatted() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("review.md"),
            "---\nname: review\ndescription: code review\n---\nReview.",
        ).unwrap();
        let skills = crate::skills::SkillRegistry {
            skills: crate::skills::load_from_dir_pub(tmp.path()).unwrap(),
        };
        let schema = activate_skill_schema(&skills).unwrap();
        let desc = schema["function"]["description"].as_str().unwrap();
        assert!(desc.contains("review: code review"));
    }

    #[test]
    fn activate_skill_schema_skill_without_description() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("bare.md"), "bare content").unwrap();
        let skills = crate::skills::SkillRegistry {
            skills: crate::skills::load_from_dir_pub(tmp.path()).unwrap(),
        };
        let schema = activate_skill_schema(&skills).unwrap();
        let desc = schema["function"]["description"].as_str().unwrap();
        // No description means just the name, not "name: "
        assert!(desc.contains("bare"));
        assert!(!desc.contains("bare: "));
    }

    // ── Session::start ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn session_start_success() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;
        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let mut chunks = vec![];
        let session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |c| chunks.push(c),
        ).await.unwrap();

        assert!(session.system_prompt().contains("You are helpful"));
        // SessionStart chunk emitted
        assert!(chunks.iter().any(|c| matches!(c, OutputChunk::SessionStart { .. })));
    }

    #[tokio::test]
    async fn session_start_no_provider_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let mut config = make_config("http://localhost");
        config.default_provider = None;
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let result = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await;

        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("provider"));
    }

    #[tokio::test]
    async fn session_start_unknown_provider_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let config = make_config("http://localhost");
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let result = Session::start(
            None, None, Some("nonexistent".into()), None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await;

        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn session_start_no_model_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let mut config = make_config("http://localhost");
        // Remove model from provider
        config.providers.get_mut("test").unwrap().models = vec![];
        config.default_model = None;
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let result = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await;

        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("model"));
    }

    // ── Session::send ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn session_send_returns_content() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("world")))
            .mount(&server)
            .await;

        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();
        let mut session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        let result = session.send("hello".into(), &tools, &skills, &mut |_| {}).await.unwrap();
        assert_eq!(result, "world");
    }

    #[tokio::test]
    async fn session_send_skill_activation_injects_system_prompt() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;

        // First call returns activate_skill tool call
        let activate_sse = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc0\",\"function\":{\"name\":\"activate_skill\",\"arguments\":\"{\\\"skill\\\":\\\"coding\\\"}\"}}]}}],\"usage\":null}\ndata: [DONE]\n";
        // Second call returns final text
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(activate_sse))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("coded")))
            .mount(&server)
            .await;

        let tmp_skills = TempDir::new().unwrap();
        std::fs::write(tmp_skills.path().join("coding.md"), "---\nname: coding\n---\nWrite great code.").unwrap();
        let skill_list = crate::skills::load_from_dir_pub(tmp_skills.path()).unwrap();
        let skills = SkillRegistry { skills: skill_list };

        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let mut session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        let result = session.send("write code".into(), &tools, &skills, &mut |_| {}).await.unwrap();
        assert_eq!(result, "coded");
        // Skill instructions injected into system prompt after activation
        assert!(session.system_prompt().contains("<skill-instructions>"));
        assert!(session.system_prompt().contains("Write great code"));
    }

    // ── inject_skill edge case ────────────────────────────────────────────────

    #[test]
    fn inject_skill_no_messages_is_noop() {
        let mut s = Session {
            messages: vec![],
            api_type: ApiType::OpenaiCompletions,
            base_url: "http://localhost".into(),
            api_key: "key".into(),
            model: "model".into(),
            logger: FileSessionLogger {
                path: std::path::PathBuf::from("/dev/null"),
                turn_start: None,
            },
        };
        s.inject_skill("should not panic");
        assert!(s.messages.is_empty());
    }

    // ── Session::start — AgentInstructions variants ───────────────────────────

    #[tokio::test]
    async fn session_start_agent_instructions_disabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let mut config = make_config("http://localhost");
        config.context.agent_instructions =
            Some(crate::config::AgentInstructions::Enabled(false));
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let mut chunks = vec![];
        let session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |c| chunks.push(c),
        ).await.unwrap();

        // instructions_entry is None → no agent-instructions block in system prompt
        assert!(!session.system_prompt().contains("<agent-instructions>"));
    }

    #[tokio::test]
    async fn session_start_agent_instructions_enabled_true() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let mut config = make_config("http://localhost");
        config.context.agent_instructions =
            Some(crate::config::AgentInstructions::Enabled(true));
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        // AGENTS.md doesn't exist in temp home — should succeed without warning
        let session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        assert!(!session.system_prompt().is_empty());
    }

    #[tokio::test]
    async fn session_start_agent_instructions_file_exists() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        std::fs::write(tmp_cwd.path().join("CUSTOM.md"), "Custom agent instructions").unwrap();

        let mut config = make_config("http://localhost");
        config.context.agent_instructions =
            Some(crate::config::AgentInstructions::File("CUSTOM.md".into()));
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        assert!(session.system_prompt().contains("Custom agent instructions"));

        std::env::set_current_dir(prev).unwrap();
    }

    #[tokio::test]
    async fn session_start_agent_instructions_file_missing_warns_and_continues() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let mut config = make_config("http://localhost");
        config.context.agent_instructions =
            Some(crate::config::AgentInstructions::File("nonexistent.md".into()));
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        // Should succeed despite missing file — just warns to stderr
        let session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        assert!(!session.system_prompt().is_empty());
        assert!(!session.system_prompt().contains("<agent-instructions>"));

        std::env::set_current_dir(prev).unwrap();
    }

    // ── Session::start — context files ────────────────────────────────────────

    #[tokio::test]
    async fn session_start_context_file_included_in_system_prompt() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let tmp_ctx = TempDir::new().unwrap();
        let ctx_file = tmp_ctx.path().join("context.md");
        std::fs::write(&ctx_file, "important context content").unwrap();

        let mut config = make_config("http://localhost");
        config.context.files = vec![ctx_file.to_string_lossy().into_owned()];
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        assert!(session.system_prompt().contains("important context content"));
    }

    #[tokio::test]
    async fn session_start_missing_context_file_warns_and_continues() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let mut config = make_config("http://localhost");
        config.context.files = vec!["/tmp/cooper_test_nonexistent_ctx_file.md".into()];
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        // Should succeed even with a missing context file
        let session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        assert!(!session.system_prompt().is_empty());
    }

    // ── Session::start — model and tools overrides ────────────────────────────

    #[tokio::test]
    async fn session_start_explicit_model_name_overrides_provider() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;
        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let mut chunk_model = String::new();
        Session::start(
            None, None, None, Some("explicit-model".into()),
            &config, &tools, &skills,
            &mut |c| {
                if let OutputChunk::SessionStart { model, .. } = &c {
                    chunk_model = model.clone();
                }
            },
        ).await.unwrap();

        assert_eq!(chunk_model, "explicit-model");
    }

    #[tokio::test]
    async fn session_start_with_allowed_tools_restricts_list() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let mut config = make_config("http://localhost");
        config.context.allowed_tools = Some(vec!["read_file".into()]);
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();

        let mut tools_in_chunk: Option<Vec<String>> = None;
        Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |c| {
                if let OutputChunk::SessionStart { tools, .. } = c {
                    tools_in_chunk = tools;
                }
            },
        ).await.unwrap();

        let list = tools_in_chunk.unwrap();
        assert_eq!(list, vec!["read_file"]);
    }

    #[tokio::test]
    async fn session_start_skills_display_with_allowed_skills_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let mut config = make_config("http://localhost");
        // allowed_skills = Some([]) → skills system is active but nothing allowed
        config.context.allowed_skills = Some(vec![]);
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty(); // no skills loaded

        let mut skills_in_chunk: Option<Option<Vec<String>>> = None;
        Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |c| {
                if let OutputChunk::SessionStart { skills, .. } = c {
                    skills_in_chunk = Some(skills);
                }
            },
        ).await.unwrap();

        // skills_display = Some([]) because allowed_skills is Some
        assert!(matches!(skills_in_chunk, Some(Some(ref v)) if v.is_empty()));
    }

    // ── activate_skill error paths ────────────────────────────────────────────

    fn activate_sse_with_args(args_json: &str) -> String {
        let escaped = args_json.replace('"', "\\\"");
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"tc0\",\"function\":{{\"name\":\"activate_skill\",\"arguments\":\"{escaped}\"}}}}]}}}}],\"usage\":null}}\ndata: [DONE]\n"
        )
    }

    #[tokio::test]
    async fn activate_skill_missing_param_captured_as_error_result() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(activate_sse_with_args("{}")))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("done")))
            .mount(&server)
            .await;

        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();
        let mut session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        let mut tool_results = vec![];
        let result = session.send(
            "test".into(), &tools, &skills,
            &mut |c| {
                if let OutputChunk::ToolResult { output, .. } = c {
                    tool_results.push(output);
                }
            },
        ).await.unwrap();

        assert_eq!(result, "done");
        assert!(tool_results.iter().any(|r| r.contains("missing") || r.contains("error")));
    }

    #[tokio::test]
    async fn activate_skill_not_found_captured_as_error_result() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_string(activate_sse_with_args("{\"skill\":\"nonexistent\"}")))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("done")))
            .mount(&server)
            .await;

        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();
        let mut session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        let mut tool_results = vec![];
        let result = session.send(
            "test".into(), &tools, &skills,
            &mut |c| {
                if let OutputChunk::ToolResult { output, .. } = c {
                    tool_results.push(output);
                }
            },
        ).await.unwrap();

        assert_eq!(result, "done");
        assert!(tool_results.iter().any(|r| r.contains("nonexistent") || r.contains("not found") || r.contains("error")));
    }

    #[tokio::test]
    async fn activate_skill_bundled_skill_includes_resources() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;

        // Build a bundled skill: tmp/my-skill/skill.md + tmp/my-skill/guide.md
        let tmp_skills = TempDir::new().unwrap();
        let bundled_dir = tmp_skills.path().join("my-skill");
        std::fs::create_dir(&bundled_dir).unwrap();
        std::fs::write(bundled_dir.join("skill.md"), "---\nname: my-skill\n---\nBundled content").unwrap();
        std::fs::write(bundled_dir.join("guide.md"), "extra resource").unwrap();

        let skill_list = crate::skills::load_from_dir_pub(tmp_skills.path()).unwrap();
        let skills = SkillRegistry { skills: skill_list };

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_string(activate_sse_with_args("{\"skill\":\"my-skill\"}")))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("ok")))
            .mount(&server)
            .await;

        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let mut session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        let mut tool_results = vec![];
        session.send(
            "activate".into(), &tools, &skills,
            &mut |c| {
                if let OutputChunk::ToolResult { output, .. } = c {
                    tool_results.push(output);
                }
            },
        ).await.unwrap();

        let combined = tool_results.join("");
        assert!(combined.contains("Skill directory:"));
        assert!(combined.contains("<skill_resources>"));
        assert!(combined.contains("guide.md"));
    }

    #[tokio::test]
    async fn activate_skill_empty_body_omits_content_section() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;

        let tmp_skills = TempDir::new().unwrap();
        // Skill with empty body (frontmatter only)
        std::fs::write(tmp_skills.path().join("empty.md"), "---\nname: empty\n---\n").unwrap();
        let skill_list = crate::skills::load_from_dir_pub(tmp_skills.path()).unwrap();
        let skills = SkillRegistry { skills: skill_list };

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_string(activate_sse_with_args("{\"skill\":\"empty\"}")))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("ok")))
            .mount(&server)
            .await;

        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let mut session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        let mut tool_results = vec![];
        session.send(
            "activate".into(), &tools, &skills,
            &mut |c| {
                if let OutputChunk::ToolResult { output, .. } = c {
                    tool_results.push(output);
                }
            },
        ).await.unwrap();

        let combined = tool_results.join("");
        assert!(combined.contains("<skill_content name=\"empty\">"));
        assert!(combined.contains("</skill_content>"));
    }

    // ── SessionRegistry fallback (regular tool call) ──────────────────────────

    #[tokio::test]
    async fn session_send_regular_tool_call_uses_fallback_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("data.txt");
        std::fs::write(&file, "tool-content").unwrap();

        let escaped_path = file.to_str().unwrap().replace('"', "\\\"");
        let tool_call_sse = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"tc1\",\"function\":{{\"name\":\"read_file\",\"arguments\":\"{{\\\"path\\\":\\\"{escaped_path}\\\"}}\"}}}}]}}}}],\"usage\":null}}\ndata: [DONE]\n"
        );

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tool_call_sse))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("done")))
            .mount(&server)
            .await;

        let config = make_config(&server.uri());
        let tools = ToolRegistry { custom_tools: vec![] };
        let skills = SkillRegistry::empty();
        let mut session = Session::start(
            None, None, None, None,
            &config, &tools, &skills,
            &mut |_| {},
        ).await.unwrap();

        let mut tool_results = vec![];
        let result = session.send(
            "read that file".into(), &tools, &skills,
            &mut |c| {
                if let OutputChunk::ToolResult { output, .. } = c {
                    tool_results.push(output);
                }
            },
        ).await.unwrap();

        assert_eq!(result, "done");
        assert!(tool_results.iter().any(|r| r.contains("tool-content")));
    }

    // ── run() public function ─────────────────────────────────────────────────

    #[tokio::test]
    async fn run_fn_returns_model_response() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oai_text_sse("run-result")))
            .mount(&server)
            .await;

        let config = make_config(&server.uri());
        let registry = ToolRegistry { custom_tools: vec![] };
        let mut chunks = vec![];

        let result = super::run(
            "my prompt".into(),
            None,
            None,
            None,
            None,
            &config,
            &registry,
            &mut |c| chunks.push(c),
        ).await.unwrap();

        assert_eq!(result, "run-result");
        assert!(chunks.iter().any(|c| matches!(c, OutputChunk::SessionStart { .. })));
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
