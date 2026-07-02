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
