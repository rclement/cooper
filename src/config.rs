use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

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

impl FromStr for ApiType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "openai-completions" => Ok(ApiType::OpenaiCompletions),
            _ => Err(anyhow!(
                "unknown API type '{}'; supported: openai-completions",
                s
            )),
        }
    }
}

pub const API_TYPES: &[&str] = &["openai-completions"];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelConfig {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub base_url: String,
    #[serde(default)]
    pub api: ApiType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ContextConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// None = not set = all tools allowed; Some([]) = explicitly empty = no tools allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RawConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<HashMap<String, ProviderConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfig>,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub system_prompt: String,
    pub providers: HashMap<String, ProviderConfig>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub context: ContextConfig,
}

pub enum Scope {
    Global,
    Project,
}

fn scope_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Global => {
            let home =
                dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
            Ok(home.join(".cooper").join("settings.yml"))
        }
        Scope::Project => Ok(PathBuf::from("cooper.yml")),
    }
}

fn load_raw(path: &PathBuf) -> Result<RawConfig> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
}

fn merge_context(base: Option<ContextConfig>, over: Option<ContextConfig>) -> ContextConfig {
    match (base, over) {
        (None, None) => ContextConfig::default(),
        (Some(b), None) => b,
        (None, Some(o)) => o,
        (Some(b), Some(o)) => ContextConfig {
            files: if o.files.is_empty() { b.files } else { o.files },
            allowed_tools: o.allowed_tools.or(b.allowed_tools),
        },
    }
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
        context: Some(merge_context(base.context, over.context)),
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
        context: merged.context.unwrap_or_default(),
    })
}

/// Returns true if a provider with the given name already exists in the target scope's config file.
pub fn provider_exists_in_scope(name: &str, scope: &Scope) -> Result<bool> {
    let path = scope_path(scope)?;
    if !path.exists() {
        return Ok(false);
    }
    let raw = load_raw(&path)?;
    Ok(raw
        .providers
        .as_ref()
        .map(|p| p.contains_key(name))
        .unwrap_or(false))
}

/// Writes a provider into the target scope's config file, creating it if absent.
pub fn save_provider(name: &str, provider: ProviderConfig, scope: &Scope) -> Result<()> {
    let path = scope_path(scope)?;

    if let Scope::Global = scope {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating directory {}", dir.display()))?;
        }
    }

    let mut raw = if path.exists() {
        load_raw(&path)?
    } else {
        RawConfig::default()
    };

    raw.providers
        .get_or_insert_with(HashMap::new)
        .insert(name.to_string(), provider);

    let content = serde_yaml::to_string(&raw).context("serializing config")?;
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))
}
