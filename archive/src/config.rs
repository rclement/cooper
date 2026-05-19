use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub use cooper_core::ApiType;

pub const API_TYPES: &[&str] = &["openai-completions", "anthropic-messages"];

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

/// Controls which file is loaded as agent instructions, or disables the feature.
/// Serializes as a bare bool or string in YAML: `false`, `true`, or `"CLAUDE.md"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentInstructions {
    Enabled(bool),
    File(String),
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ContextConfig {
    /// None = not set = all tools allowed; Some([]) = explicitly empty = no tools allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_instructions: Option<AgentInstructions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// None = all skills allowed; Some([]) = no skills; Some([..]) = only listed skills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_skills: Option<Vec<String>>,
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

/// Expands `${VAR_NAME}` placeholders using the current process environment.
/// Unknown variables are replaced with an empty string.
fn expand_env_vars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(dollar) = rest.find("${") {
        out.push_str(&rest[..dollar]);
        rest = &rest[dollar + 2..];
        if let Some(close) = rest.find('}') {
            let var = &rest[..close];
            out.push_str(&std::env::var(var).unwrap_or_default());
            rest = &rest[close + 1..];
        } else {
            out.push_str("${");
        }
    }
    out.push_str(rest);
    out
}

fn load_raw(path: &PathBuf) -> Result<RawConfig> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let content = expand_env_vars(&content);
    serde_yaml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
}

fn merge_context(base: Option<ContextConfig>, over: Option<ContextConfig>) -> ContextConfig {
    match (base, over) {
        (None, None) => ContextConfig::default(),
        (Some(b), None) => b,
        (None, Some(o)) => o,
        (Some(b), Some(o)) => ContextConfig {
            agent_instructions: o.agent_instructions.or(b.agent_instructions),
            files: if o.files.is_empty() { b.files } else { o.files },
            allowed_tools: o.allowed_tools.or(b.allowed_tools),
            allowed_skills: o.allowed_skills.or(b.allowed_skills),
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
            .unwrap_or_else(|| cooper_core::system_prompt::DEFAULT.to_string()),
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

fn ensure_config_exists(_scope: &Scope, path: &PathBuf) -> Result<()> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let content = serde_yaml::to_string(&RawConfig::default())?;
        std::fs::write(path, content)?;
    }
    Ok(())
}

/// Read-modify-write a config file for the given scope, creating it if absent.
pub fn update_config<F>(scope: &Scope, f: F) -> Result<()>
where
    F: FnOnce(&mut RawConfig) -> Result<()>,
{
    let path = scope_path(scope)?;
    ensure_config_exists(scope, &path)?;
    let mut raw = load_raw(&path)?;
    f(&mut raw)?;
    let content = serde_yaml::to_string(&raw).context("serializing config")?;
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))
}

/// Load the raw (non-merged) config for a specific scope; returns default if the file is absent.
pub fn load_raw_scope(scope: &Scope) -> Result<RawConfig> {
    let path = scope_path(scope)?;
    if !path.exists() {
        return Ok(RawConfig::default());
    }
    load_raw(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Serialise tests that mutate HOME or cwd so they don't interfere.
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

    // ── expand_env_vars ───────────────────────────────────────────────────────

    #[test]
    fn expand_no_vars() {
        assert_eq!(expand_env_vars("plain text"), "plain text");
    }

    #[test]
    fn expand_known_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialised by ENV_LOCK.
        unsafe { std::env::set_var("COOPER_TEST_VAR", "hello") };
        let result = expand_env_vars("prefix ${COOPER_TEST_VAR} suffix");
        unsafe { std::env::remove_var("COOPER_TEST_VAR") };
        assert_eq!(result, "prefix hello suffix");
    }

    #[test]
    fn expand_unknown_var_becomes_empty() {
        let key = "COOPER_DEFINITELY_NOT_SET_XYZ123";
        // SAFETY: key is unique to this test; ENV_LOCK not needed since we just remove.
        let _ = unsafe { std::env::remove_var(key) };
        let result = expand_env_vars(&format!("a ${{{}}} b", key));
        assert_eq!(result, "a  b");
    }

    #[test]
    fn expand_unclosed_brace_preserved() {
        let result = expand_env_vars("hello ${unclosed");
        assert_eq!(result, "hello ${unclosed");
    }

    #[test]
    fn expand_multiple_vars() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialised by ENV_LOCK.
        unsafe {
            std::env::set_var("A_VAR", "foo");
            std::env::set_var("B_VAR", "bar");
        }
        let result = expand_env_vars("${A_VAR}+${B_VAR}");
        unsafe {
            std::env::remove_var("A_VAR");
            std::env::remove_var("B_VAR");
        }
        assert_eq!(result, "foo+bar");
    }

    // ── merge_context ─────────────────────────────────────────────────────────

    #[test]
    fn merge_context_both_none() {
        let c = merge_context(None, None);
        assert!(c.files.is_empty());
        assert!(c.allowed_tools.is_none());
    }

    #[test]
    fn merge_context_base_only() {
        let base = ContextConfig { files: vec!["a.md".into()], ..Default::default() };
        let c = merge_context(Some(base), None);
        assert_eq!(c.files, vec!["a.md"]);
    }

    #[test]
    fn merge_context_override_only() {
        let over = ContextConfig { files: vec!["b.md".into()], ..Default::default() };
        let c = merge_context(None, Some(over));
        assert_eq!(c.files, vec!["b.md"]);
    }

    #[test]
    fn merge_context_override_wins() {
        let base = ContextConfig {
            files: vec!["a.md".into()],
            allowed_tools: Some(vec!["read_file".into()]),
            ..Default::default()
        };
        let over = ContextConfig {
            files: vec!["b.md".into()],
            allowed_tools: None,
            ..Default::default()
        };
        let c = merge_context(Some(base.clone()), Some(over));
        // override has non-empty files → wins
        assert_eq!(c.files, vec!["b.md"]);
        // override allowed_tools is None → falls back to base
        assert_eq!(c.allowed_tools, Some(vec!["read_file".into()]));
    }

    #[test]
    fn merge_context_base_files_used_when_override_empty() {
        let base = ContextConfig { files: vec!["base.md".into()], ..Default::default() };
        let over = ContextConfig { files: vec![], ..Default::default() };
        let c = merge_context(Some(base), Some(over));
        assert_eq!(c.files, vec!["base.md"]);
    }

    // ── merge (RawConfig) ────────────────────────────────────────────────────

    #[test]
    fn merge_overrides_take_precedence() {
        let base = RawConfig {
            system_prompt: Some("base sys".into()),
            default_provider: Some("provider-a".into()),
            ..Default::default()
        };
        let over = RawConfig {
            system_prompt: Some("over sys".into()),
            default_model: Some("gpt-4".into()),
            ..Default::default()
        };
        let merged = merge(base, over);
        assert_eq!(merged.system_prompt, Some("over sys".into()));
        assert_eq!(merged.default_provider, Some("provider-a".into())); // from base
        assert_eq!(merged.default_model, Some("gpt-4".into()));
    }

    #[test]
    fn merge_providers_combined() {
        let mut base_providers = HashMap::new();
        base_providers.insert("a".to_string(), ProviderConfig {
            base_url: "http://a".into(), api: ApiType::default(), api_key: None, models: vec![],
        });
        let mut over_providers = HashMap::new();
        over_providers.insert("b".to_string(), ProviderConfig {
            base_url: "http://b".into(), api: ApiType::default(), api_key: None, models: vec![],
        });
        let base = RawConfig { providers: Some(base_providers), ..Default::default() };
        let over = RawConfig { providers: Some(over_providers), ..Default::default() };
        let merged = merge(base, over);
        let p = merged.providers.unwrap();
        assert!(p.contains_key("a"));
        assert!(p.contains_key("b"));
    }

    #[test]
    fn merge_providers_base_none() {
        let mut over_providers = HashMap::new();
        over_providers.insert("x".to_string(), ProviderConfig {
            base_url: "http://x".into(), api: ApiType::default(), api_key: None, models: vec![],
        });
        let base = RawConfig::default();
        let over = RawConfig { providers: Some(over_providers), ..Default::default() };
        let merged = merge(base, over);
        assert!(merged.providers.unwrap().contains_key("x"));
    }

    // ── load_raw_scope ────────────────────────────────────────────────────────

    #[test]
    fn load_raw_scope_missing_project_returns_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let raw = load_raw_scope(&Scope::Project).unwrap();
        assert!(raw.system_prompt.is_none());

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn load_raw_scope_existing_project_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let yaml = "system_prompt: custom\n";
        std::fs::write(tmp.path().join("cooper.yml"), yaml).unwrap();

        let raw = load_raw_scope(&Scope::Project).unwrap();
        assert_eq!(raw.system_prompt, Some("custom".into()));

        std::env::set_current_dir(prev).unwrap();
    }

    // ── save_provider + provider_exists_in_scope ──────────────────────────────

    #[test]
    fn save_and_check_provider_global() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();

        let provider = ProviderConfig {
            base_url: "http://localhost".into(),
            api: ApiType::OpenaiCompletions,
            api_key: Some("key".into()),
            models: vec![],
        };

        assert!(!provider_exists_in_scope("myp", &Scope::Global).unwrap());
        save_provider("myp", provider, &Scope::Global).unwrap();
        assert!(provider_exists_in_scope("myp", &Scope::Global).unwrap());
    }

    #[test]
    fn save_provider_idempotent_overwrites() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();

        let p1 = ProviderConfig { base_url: "http://a".into(), api: ApiType::default(), api_key: None, models: vec![] };
        let p2 = ProviderConfig { base_url: "http://b".into(), api: ApiType::default(), api_key: None, models: vec![] };
        save_provider("p", p1, &Scope::Global).unwrap();
        save_provider("p", p2, &Scope::Global).unwrap();

        let raw = load_raw_scope(&Scope::Global).unwrap();
        assert_eq!(raw.providers.unwrap()["p"].base_url, "http://b");
    }

    // ── update_config ─────────────────────────────────────────────────────────

    #[test]
    fn update_config_creates_file_if_absent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();

        update_config(&Scope::Global, |raw| {
            raw.default_model = Some("test-model".into());
            Ok(())
        }).unwrap();

        let raw = load_raw_scope(&Scope::Global).unwrap();
        assert_eq!(raw.default_model, Some("test-model".into()));
    }

    #[test]
    fn update_config_propagates_closure_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();

        let result = update_config(&Scope::Global, |_raw| {
            Err(anyhow::anyhow!("closure error"))
        });

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("closure error"));
    }

    // ── load (merged) ─────────────────────────────────────────────────────────

    #[test]
    fn load_falls_back_to_default_system_prompt() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp_home = TempHome::new();
        let tmp_dir = TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();
        // Change to a directory without a cooper.yml
        std::env::set_current_dir(tmp_dir.path()).unwrap();
        drop(tmp_home); // HOME is restored, but TempDir is still in scope

        // Re-set HOME to a fresh empty dir so no settings.yml exists
        let _home = TempHome::new();
        let cfg = load().unwrap();
        assert!(!cfg.system_prompt.is_empty());

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn load_with_global_settings_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let tmp_dir = TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp_dir.path()).unwrap();

        let settings_dir = _home._dir.path().join(".cooper");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(settings_dir.join("settings.yml"), "default_model: global-model\n").unwrap();

        let cfg = load().unwrap();
        assert_eq!(cfg.default_model, Some("global-model".into()));

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn load_project_overrides_global() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let tmp_dir = TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp_dir.path()).unwrap();

        let settings_dir = _home._dir.path().join(".cooper");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(settings_dir.join("settings.yml"), "default_provider: global\n").unwrap();
        std::fs::write(tmp_dir.path().join("cooper.yml"), "default_provider: project\n").unwrap();

        let cfg = load().unwrap();
        assert_eq!(cfg.default_provider, Some("project".into()));

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn merge_providers_over_none_returns_base() {
        let mut base_providers = HashMap::new();
        base_providers.insert("base-only".to_string(), ProviderConfig {
            base_url: "http://base".into(), api: ApiType::default(), api_key: None, models: vec![],
        });
        let base = RawConfig { providers: Some(base_providers), ..Default::default() };
        let over = RawConfig::default();
        let merged = merge(base, over);
        assert!(merged.providers.unwrap().contains_key("base-only"));
    }

    #[test]
    fn save_and_check_provider_project_scope() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let provider = ProviderConfig {
            base_url: "http://project-local".into(),
            api: ApiType::OpenaiCompletions,
            api_key: None,
            models: vec![],
        };

        assert!(!provider_exists_in_scope("proj-p", &Scope::Project).unwrap());
        save_provider("proj-p", provider, &Scope::Project).unwrap();
        assert!(provider_exists_in_scope("proj-p", &Scope::Project).unwrap());
        assert!(tmp.path().join("cooper.yml").exists());

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn update_config_project_scope_creates_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        update_config(&Scope::Project, |raw| {
            raw.default_model = Some("project-model".into());
            Ok(())
        }).unwrap();

        assert!(tmp.path().join("cooper.yml").exists());
        let raw = load_raw_scope(&Scope::Project).unwrap();
        assert_eq!(raw.default_model, Some("project-model".into()));

        std::env::set_current_dir(prev).unwrap();
    }
}
