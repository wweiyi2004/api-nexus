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
    pub proxy_api_keys: Vec<ProxyApiKey>,
    #[serde(default)]
    pub model_aliases: Vec<ModelAlias>,
    #[serde(default)]
    pub model_prices: Vec<ModelPrice>,
    #[serde(default = "default_usd_to_cny_rate")]
    pub usd_to_cny_rate: f64,
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,
    #[serde(default = "default_max_log_entries")]
    pub max_log_entries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyApiKey {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub key: String,
    #[serde(default = "default_provider_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrice {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub input_usd_per_million: f64,
    #[serde(default)]
    pub output_usd_per_million: f64,
    #[serde(default)]
    pub cached_usd_per_million: f64,
    #[serde(default)]
    pub cache_read_usd_per_million: f64,
    #[serde(default)]
    pub cache_write_usd_per_million: f64,
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

fn default_usd_to_cny_rate() -> f64 {
    7.2
}

fn default_log_retention_days() -> u32 {
    30
}

fn default_max_log_entries() -> usize {
    10_000
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
            proxy_api_keys: Vec::new(),
            model_aliases: Vec::new(),
            model_prices: Vec::new(),
            usd_to_cny_rate: default_usd_to_cny_rate(),
            log_retention_days: default_log_retention_days(),
            max_log_entries: default_max_log_entries(),
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

pub fn app_data_dir() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("api-nexus");
    fs::create_dir_all(&path).ok();
    path
}

fn config_path() -> PathBuf {
    let mut path = app_data_dir();
    path.push("config.json");
    path
}

pub fn database_path() -> PathBuf {
    app_data_dir().join("api-nexus.sqlite3")
}

fn secrets_path() -> PathBuf {
    app_data_dir().join("secrets.dpapi")
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

pub fn normalize_config(mut config: AppConfig) -> AppConfig {
    if config.usd_to_cny_rate <= 0.0 {
        config.usd_to_cny_rate = default_usd_to_cny_rate();
    }
    if config.log_retention_days == 0 {
        config.log_retention_days = default_log_retention_days();
    }
    config.log_retention_days = config.log_retention_days.clamp(1, 3650);
    if config.max_log_entries == 0 {
        config.max_log_entries = default_max_log_entries();
    }
    config.max_log_entries = config.max_log_entries.clamp(100, 1_000_000);

    for price in &mut config.model_prices {
        if price.cache_read_usd_per_million <= 0.0 && price.cached_usd_per_million > 0.0 {
            price.cache_read_usd_per_million = price.cached_usd_per_million;
        }
        price.cached_usd_per_million = price.cache_read_usd_per_million;
    }

    if config.proxy_api_keys.is_empty() {
        if config.proxy_api_key.trim().is_empty() {
            config.proxy_api_key = generate_proxy_api_key();
        }
        config.proxy_api_keys.push(ProxyApiKey {
            id: uuid::Uuid::new_v4().to_string(),
            name: "默认密钥".to_string(),
            key: config.proxy_api_key.clone(),
            enabled: true,
        });
    }

    for (index, key) in config.proxy_api_keys.iter_mut().enumerate() {
        if key.id.trim().is_empty() {
            key.id = uuid::Uuid::new_v4().to_string();
        }
        if key.name.trim().is_empty() {
            key.name = format!("密钥 {}", index + 1);
        }
        if key.key.trim().is_empty() {
            key.key = generate_proxy_api_key();
        }
    }

    if !config.proxy_api_keys.iter().any(|key| key.enabled) {
        if let Some(first_key) = config.proxy_api_keys.first_mut() {
            first_key.enabled = true;
        }
    }

    if let Some(first_enabled_key) = config
        .proxy_api_keys
        .iter()
        .find(|key| key.enabled && !key.key.trim().is_empty())
    {
        config.proxy_api_key = first_enabled_key.key.clone();
    }

    config
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
    if let Err(error) = crate::security::hydrate_config_secrets(&mut config, &secrets_path()) {
        log::error!("Failed to load secure API keys: {}", error);
    }
    let config = normalize_config(config);
    save_config(&config).ok();
    config
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = config_path();
    crate::security::save_config_secrets(config, &secrets_path())?;
    let redacted = crate::security::redacted_config(config);
    let content = serde_json::to_string_pretty(&redacted).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content).map_err(|e| e.to_string())?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    fs::rename(temporary, path).map_err(|e| e.to_string())
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
        assert!(config.proxy_api_keys.is_empty());
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
