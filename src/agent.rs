use crate::config::ResolvedConfig;
use crate::providers::{Message, OutputChunk, Role, call};
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;
use uuid::Uuid;

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

pub async fn run(
    prompt: String,
    system_prompt: Option<String>,
    provider_name: Option<String>,
    model_name: Option<String>,
    config: &ResolvedConfig,
    on_chunk: &mut dyn FnMut(OutputChunk),
) -> Result<String> {
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

    let system = system_prompt.unwrap_or_else(|| config.system_prompt.clone());

    let messages = vec![
        Message {
            role: Role::System,
            content: system,
        },
        Message {
            role: Role::User,
            content: prompt,
        },
    ];

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
    append(
        &path,
        &SessionEntry::Request {
            messages: messages.clone(),
        },
    )?;

    // Wrap the caller's callback to also accumulate thinking for session storage.
    let mut thinking_buf = String::new();
    let mut wrapped = |chunk: OutputChunk| {
        if let OutputChunk::Thinking(ref t) = chunk {
            thinking_buf.push_str(t);
        }
        on_chunk(chunk);
    };

    let start = Instant::now();
    let response = call(provider, &model, messages, &mut wrapped).await?;
    let duration_ms = start.elapsed().as_millis() as u64;

    append(
        &path,
        &SessionEntry::Response {
            thinking: if thinking_buf.is_empty() {
                None
            } else {
                Some(thinking_buf)
            },
            message: response.clone(),
            duration_ms,
        },
    )?;

    Ok(response.content)
}
