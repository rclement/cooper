use crate::config::{ApiType, ProviderConfig};
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

pub async fn call(
    provider: &ProviderConfig,
    model: &str,
    messages: Vec<Message>,
) -> Result<Message> {
    match provider.api {
        ApiType::OpenaiCompletions => openai_completions(provider, model, messages).await,
    }
}

// Wire types for OpenAI Chat Completions — kept private to this module.

#[derive(Serialize, Deserialize)]
struct OaiMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct OaiRequest {
    model: String,
    messages: Vec<OaiMessage>,
}

#[derive(Deserialize)]
struct OaiResponse {
    choices: Vec<OaiChoice>,
}

#[derive(Deserialize)]
struct OaiChoice {
    message: OaiMessage,
}

async fn openai_completions(
    provider: &ProviderConfig,
    model: &str,
    messages: Vec<Message>,
) -> Result<Message> {
    let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));
    let api_key = provider.api_key.as_deref().unwrap_or("");

    let oai_messages: Vec<OaiMessage> = messages
        .into_iter()
        .map(|m| OaiMessage {
            role: match m.role {
                Role::System => "system".to_string(),
                Role::User => "user".to_string(),
                Role::Assistant => "assistant".to_string(),
            },
            content: m.content,
        })
        .collect();

    let response = Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OaiRequest {
            model: model.to_string(),
            messages: oai_messages,
        })
        .send()
        .await?
        .error_for_status()?
        .json::<OaiResponse>()
        .await?;

    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("empty response from provider"))?;

    Ok(Message {
        role: Role::Assistant,
        content: choice.message.content,
    })
}
