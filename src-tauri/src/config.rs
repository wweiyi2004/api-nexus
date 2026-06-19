use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,
    #[serde(default = "default_proxy_host")]
    pub proxy_host: String,
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
    #[serde(default)]
    pub proxy_api_key: String,
    #[serde(default)]
    pub model_aliases: Vec<ModelAlias>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_provider_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_provider_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
}

fn default_proxy_port() -> u16 {
    11434
}

fn default_proxy_host() -> String {
    "127.0.0.1".to_string()
}

fn default_auto_start() -> bool {
    true
}

fn default_provider_enabled() -> bool {
    true
}

fn default_provider_protocol() -> String {
    "openai".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            proxy_port: default_proxy_port(),
            proxy_host: default_proxy_host(),
            auto_start: default_auto_start(),
            proxy_api_key: String::new(),
            model_aliases: Vec::new(),
        }
    }
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            protocol: default_provider_protocol(),
            base_url: String::new(),
            api_key: String::new(),
            models: Vec::new(),
            enabled: default_provider_enabled(),
            priority: 0,
        }
    }
}

fn config_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("api-nexus");
    fs::create_dir_all(&path).ok();
    path.push("config.json");
    path
}

fn backup_invalid_config(path: &PathBuf) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let backup_path = path.with_file_name(format!("config.invalid.{}.json", timestamp));
    if let Err(err) = fs::copy(path, &backup_path) {
        log::error!("Failed to back up invalid config {:?}: {}", path, err);
    } else {
        log::error!("Invalid config backed up to {:?}", backup_path);
    }
}

pub fn generate_proxy_api_key() -> String {
    format!("sk-nexus-{}", uuid::Uuid::new_v4().simple())
}

fn read_config_or_default() -> AppConfig {
    let path = config_path();
    if !path.exists() {
        return AppConfig::default();
    }

    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(config) => config,
            Err(err) => {
                log::error!("Failed to parse config {:?}: {}", path, err);
                backup_invalid_config(&path);
                AppConfig::default()
            }
        },
        Err(err) => {
            log::error!("Failed to read config {:?}: {}", path, err);
            AppConfig::default()
        }
    }
}

pub fn load_config() -> AppConfig {
    let mut config = read_config_or_default();
    // An empty key disables proxy auth entirely, which lets any local process
    // or malicious web page use the configured upstream API keys.
    if config.proxy_api_key.trim().is_empty() {
        config.proxy_api_key = generate_proxy_api_key();
    }
    save_config(&config).ok();
    config
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = config_path();
    let content = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(&path, content).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_uses_backward_compatible_defaults() {
        let config: AppConfig = serde_json::from_str(
            r#"{
                "providers": [
                    {
                        "id": "openai",
                        "name": "OpenAI",
                        "base_url": "https://api.openai.com",
                        "api_key": "sk-test",
                        "models": ["gpt-4o"]
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(config.proxy_port, 11434);
        assert_eq!(config.proxy_host, "127.0.0.1");
        assert!(config.auto_start);
        assert_eq!(config.proxy_api_key, "");
        assert_eq!(config.providers[0].protocol, "openai");
        assert!(config.providers[0].enabled);
        assert_eq!(config.providers[0].priority, 0);
    }

    #[test]
    fn generated_proxy_api_keys_are_unique_and_prefixed() {
        let first = generate_proxy_api_key();
        let second = generate_proxy_api_key();

        assert!(first.starts_with("sk-nexus-"));
        assert!(first.len() > "sk-nexus-".len());
        assert_ne!(first, second);
    }
}
