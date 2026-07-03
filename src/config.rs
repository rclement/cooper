use std::collections::HashMap;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    pub default_provider: String,
    pub default_model: String,
    pub providers: HashMap<String, ProviderConfig>,
}

#[derive(Deserialize)]
pub struct ProviderConfig {
    pub provider_type: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<ModelConfig>,
}

#[derive(Deserialize)]
pub struct ModelConfig {
    pub id: String,
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    let path = dirs::home_dir()
        .ok_or("could not determine home directory")?
        .join(".cooper/settings.yml");
    let yaml_content = std::fs::read_to_string(path)?;
    let content: Config = serde_yaml::from_str(&yaml_content)?;
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `load()` resolves its path from the `HOME` env var (via `dirs::home_dir`),
    /// which is process-global, so tests that override it must not run concurrently.
    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Points `HOME` at a fresh temp dir for the duration of the guard, restoring
    /// the previous value on drop.
    struct HomeEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl HomeEnvGuard {
        fn set(path: &std::path::Path) -> Self {
            let lock = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var("HOME").ok();
            unsafe { std::env::set_var("HOME", path) };
            HomeEnvGuard {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for HomeEnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    #[test]
    fn load_parses_valid_settings_file() {
        let tmp_home = tempfile::tempdir().unwrap();
        let cooper_dir = tmp_home.path().join(".cooper");
        std::fs::create_dir_all(&cooper_dir).unwrap();
        std::fs::write(
            cooper_dir.join("settings.yml"),
            r#"
default_provider: openai
default_model: gpt-4
providers:
  openai:
    provider_type: openai-completions
    base_url: https://api.openai.com/v1
    api_key: sk-test
    models:
      - id: gpt-4
"#,
        )
        .unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        let config = load().unwrap();

        assert_eq!(config.default_provider, "openai");
        assert_eq!(config.default_model, "gpt-4");
        let provider = config.providers.get("openai").unwrap();
        assert_eq!(provider.provider_type, "openai-completions");
        assert_eq!(provider.models[0].id, "gpt-4");
    }

    #[test]
    fn load_fails_when_settings_file_is_missing() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        let result = load();

        match result {
            Err(e) => assert!(e.to_string().contains("No such file or directory")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn load_fails_on_invalid_yaml() {
        let tmp_home = tempfile::tempdir().unwrap();
        let cooper_dir = tmp_home.path().join(".cooper");
        std::fs::create_dir_all(&cooper_dir).unwrap();
        std::fs::write(cooper_dir.join("settings.yml"), "not: [valid: yaml").unwrap();
        let _guard = HomeEnvGuard::set(tmp_home.path());

        let result = load();

        assert!(result.is_err());
    }
}
