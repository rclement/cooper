use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ApiType {
    #[default]
    OpenaiCompletions,
}

impl fmt::Display for ApiType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiType::OpenaiCompletions => write!(f, "openai-completions"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub base_url: String,
    #[serde(default)]
    pub api: ApiType,
    pub api_key: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RawConfig {
    pub system_prompt: Option<String>,
    pub providers: Option<HashMap<String, ProviderConfig>>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub system_prompt: String,
    pub providers: HashMap<String, ProviderConfig>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
}

fn load_raw(path: &PathBuf) -> Result<RawConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
}

fn merge(base: RawConfig, over: RawConfig) -> RawConfig {
    RawConfig {
        system_prompt: over.system_prompt.or(base.system_prompt),
        providers: match (base.providers, over.providers) {
            (None, p) | (p, None) => p,
            (Some(mut b), Some(o)) => {
                b.extend(o);
                Some(b)
            }
        },
        default_provider: over.default_provider.or(base.default_provider),
        default_model: over.default_model.or(base.default_model),
    }
}

pub fn load() -> Result<ResolvedConfig> {
    let global_path = dirs::home_dir().map(|h| h.join(".cooper").join("settings.yml"));
    let global = match global_path.filter(|p| p.exists()) {
        Some(p) => load_raw(&p)?,
        None => RawConfig::default(),
    };

    let project_path = PathBuf::from("cooper.yml");
    let project = if project_path.exists() {
        load_raw(&project_path)?
    } else {
        RawConfig::default()
    };

    let merged = merge(global, project);

    Ok(ResolvedConfig {
        system_prompt: merged
            .system_prompt
            .unwrap_or_else(|| "You are a helpful AI assistant".to_string()),
        providers: merged.providers.unwrap_or_default(),
        default_provider: merged.default_provider,
        default_model: merged.default_model,
    })
}
