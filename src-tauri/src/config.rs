use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    pub model_routes: Vec<ModelRoute>,
    #[serde(default)]
    pub model_prices: Vec<ModelPrice>,
    #[serde(default = "default_usd_to_cny_rate")]
    pub usd_to_cny_rate: f64,
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,
    #[serde(default = "default_max_log_entries")]
    pub max_log_entries: usize,
    #[serde(default)]
    pub fusion: FusionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionConfig {
    #[serde(default = "default_fusion_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub panel_models: Vec<ModelRef>,
    #[serde(default)]
    pub judge_model: Option<ModelRef>,
    #[serde(default)]
    pub final_model: Option<ModelRef>,
    #[serde(default = "default_fusion_max_panel_models")]
    pub max_panel_models: u8,
    #[serde(default = "default_fusion_timeout_secs")]
    pub timeout_secs: u64,
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
pub struct ModelRoute {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrice {
    #[serde(default)]
    pub provider_id: String,
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
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub source_note: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub automatic: bool,
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

fn default_fusion_enabled() -> bool {
    true
}

fn default_fusion_max_panel_models() -> u8 {
    4
}

fn default_fusion_timeout_secs() -> u64 {
    120
}

fn default_provider_enabled() -> bool {
    true
}

fn default_provider_protocol() -> String {
    "openai".to_string()
}

struct KnownModelPrice {
    input_usd_per_million: f64,
    output_usd_per_million: f64,
    cache_read_usd_per_million: f64,
    cache_write_usd_per_million: f64,
    source_url: &'static str,
    source_note: &'static str,
    updated_at: &'static str,
}

fn usd_model_price(
    input_usd_per_million: f64,
    output_usd_per_million: f64,
    cache_read_usd_per_million: f64,
    cache_write_usd_per_million: f64,
    source_url: &'static str,
    source_note: &'static str,
    updated_at: &'static str,
) -> KnownModelPrice {
    KnownModelPrice {
        input_usd_per_million,
        output_usd_per_million,
        cache_read_usd_per_million,
        cache_write_usd_per_million,
        source_url,
        source_note,
        updated_at,
    }
}

fn cny_model_price(
    cny_per_million: [f64; 4],
    usd_to_cny_rate: f64,
    source_url: &'static str,
    source_note: &'static str,
    updated_at: &'static str,
) -> KnownModelPrice {
    let [input_cny_per_million, output_cny_per_million, cache_read_cny_per_million, cache_write_cny_per_million] =
        cny_per_million;

    usd_model_price(
        input_cny_per_million / usd_to_cny_rate,
        output_cny_per_million / usd_to_cny_rate,
        cache_read_cny_per_million / usd_to_cny_rate,
        cache_write_cny_per_million / usd_to_cny_rate,
        source_url,
        source_note,
        updated_at,
    )
}

fn ali_cache_read(input_cny_per_million: f64) -> f64 {
    // Aliyun Model Studio implicit context-cache reads are billed at 20% of input.
    // Explicit cache hits can be cheaper (10%), so users can override manually.
    input_cny_per_million * 0.2
}

fn ali_cache_write(input_cny_per_million: f64) -> f64 {
    // Aliyun explicit cache creation is billed at 125% of input.
    input_cny_per_million * 1.25
}

fn is_opencode_zen(base_url: &str) -> bool {
    base_url.contains("opencode.ai/zen")
}

fn is_aliyun_model_studio(base_url: &str) -> bool {
    base_url.contains("dashscope.aliyuncs.com")
        || base_url.contains("bailian")
        || base_url.contains("aliyun.com")
}

fn is_kimi_official(base_url: &str) -> bool {
    base_url.contains("moonshot")
        || base_url.contains("platform.kimi.com")
        || base_url.contains("api.kimi.com")
}

fn is_minimax_official(base_url: &str) -> bool {
    base_url.contains("api.minimaxi.com")
        || base_url.contains("api.minimax.io")
        || base_url.contains("platform.minimaxi.com")
}

fn deepseek_price(model_key: &str) -> Option<KnownModelPrice> {
    match model_key {
        "deepseek-v4-flash" => Some(usd_model_price(
            0.14,
            0.28,
            0.0028,
            0.0,
            "https://api-docs.deepseek.com/quick_start/pricing",
            "DeepSeek 官方美元价格",
            "2026-06-20",
        )),
        "deepseek-v4-pro" => Some(usd_model_price(
            0.435,
            0.87,
            0.003625,
            0.0,
            "https://api-docs.deepseek.com/quick_start/pricing",
            "DeepSeek 官方美元价格",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn openai_price(model_key: &str) -> Option<KnownModelPrice> {
    match model_key {
        "gpt-4o" => Some(usd_model_price(
            2.5,
            10.0,
            1.25,
            0.0,
            "https://developers.openai.com/api/docs/models/gpt-4o",
            "OpenAI 官方美元价格",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn gemini_price(model_key: &str) -> Option<KnownModelPrice> {
    match model_key {
        "gemini-2.5-flash" => Some(usd_model_price(
            0.3,
            2.5,
            0.03,
            0.0,
            "https://ai.google.dev/gemini-api/docs/pricing",
            "Gemini 标准文本价格",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn zhipu_price(model_key: &str, usd_to_cny_rate: f64) -> Option<KnownModelPrice> {
    match model_key {
        "glm-5.2" => Some(cny_model_price(
            [8.0, 28.0, 2.0, 0.0],
            usd_to_cny_rate,
            "https://open.bigmodel.cn/pricing",
            "智谱官方人民币价格，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        "glm-5.1" => Some(cny_model_price(
            [6.0, 24.0, 1.3, 0.0],
            usd_to_cny_rate,
            "https://open.bigmodel.cn/pricing",
            "智谱官方人民币价格；输入 <32K 档位，长上下文请手动调整",
            "2026-06-20",
        )),
        "glm-5" => Some(cny_model_price(
            [4.0, 18.0, 1.0, 0.0],
            usd_to_cny_rate,
            "https://open.bigmodel.cn/pricing",
            "智谱官方人民币价格；输入 <32K 档位，长上下文请手动调整",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn kimi_price(model_key: &str, usd_to_cny_rate: f64) -> Option<KnownModelPrice> {
    match model_key {
        "kimi-k2.5" => Some(cny_model_price(
            [4.0, 21.0, 0.7, 0.0],
            usd_to_cny_rate,
            "https://platform.kimi.com/docs/pricing/chat-k25.md",
            "Kimi 官方人民币价格，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        "kimi-k2.6" => Some(cny_model_price(
            [6.5, 27.0, 1.1, 0.0],
            usd_to_cny_rate,
            "https://platform.kimi.com/docs/pricing/chat-k26.md",
            "Kimi 官方人民币价格，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        "kimi-k2.7-code" => Some(cny_model_price(
            [6.5, 27.0, 1.3, 0.0],
            usd_to_cny_rate,
            "https://platform.kimi.com/docs/pricing/chat-k27-code.md",
            "Kimi 官方人民币价格，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        "kimi-k2.7-code-highspeed" => Some(cny_model_price(
            [13.0, 54.0, 2.6, 0.0],
            usd_to_cny_rate,
            "https://platform.kimi.com/docs/pricing/chat-k27-code.md",
            "Kimi HighSpeed 官方人民币价格，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn minimax_price(model_key: &str, usd_to_cny_rate: f64) -> Option<KnownModelPrice> {
    match model_key {
        "minimax-m3" => Some(cny_model_price(
            [2.1, 8.4, 0.42, 0.0],
            usd_to_cny_rate,
            "https://platform.minimaxi.com/docs/guides/pricing-paygo.md",
            "MiniMax 官方标准按量人民币价格，按当前 USD/CNY 设置折算；>512K 输入请手动调整",
            "2026-06-20",
        )),
        "minimax-m2.7" => Some(cny_model_price(
            [2.1, 8.4, 0.42, 2.625],
            usd_to_cny_rate,
            "https://platform.minimaxi.com/docs/guides/pricing-paygo.md",
            "MiniMax 官方标准按量人民币价格，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        "minimax-m2.5" => Some(cny_model_price(
            [2.1, 8.4, 0.21, 2.625],
            usd_to_cny_rate,
            "https://platform.minimaxi.com/docs/guides/pricing-paygo.md",
            "MiniMax 官方历史模型人民币价格，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn aliyun_model_studio_price(model_key: &str, usd_to_cny_rate: f64) -> Option<KnownModelPrice> {
    match model_key {
        "qwen3.7-max" => Some(cny_model_price(
            [12.0, 36.0, ali_cache_read(12.0), ali_cache_write(12.0)],
            usd_to_cny_rate,
            "https://help.aliyun.com/zh/model-studio/billing-for-model-studio",
            "阿里云百炼人民币价格，按当前 USD/CNY 设置折算；缓存读取按隐式缓存 20% 估算",
            "2026-06-20",
        )),
        "qwen3.7-plus" => Some(cny_model_price(
            [2.0, 8.0, ali_cache_read(2.0), ali_cache_write(2.0)],
            usd_to_cny_rate,
            "https://help.aliyun.com/zh/model-studio/billing-for-model-studio",
            "阿里云百炼人民币价格；输入 ≤256K 档位，缓存读取按隐式缓存 20% 估算",
            "2026-06-20",
        )),
        "qwen3.6-plus" => Some(cny_model_price(
            [2.0, 12.0, ali_cache_read(2.0), ali_cache_write(2.0)],
            usd_to_cny_rate,
            "https://help.aliyun.com/zh/model-studio/billing-for-model-studio",
            "阿里云百炼人民币价格；输入 ≤256K 档位，缓存读取按隐式缓存 20% 估算",
            "2026-06-20",
        )),
        "qwen3.5-plus" => Some(cny_model_price(
            [0.8, 4.8, ali_cache_read(0.8), ali_cache_write(0.8)],
            usd_to_cny_rate,
            "https://help.aliyun.com/zh/model-studio/billing-for-model-studio",
            "阿里云百炼人民币价格；输入 ≤128K 档位，缓存读取按隐式缓存 20% 估算",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn aliyun_third_party_price(model_key: &str, usd_to_cny_rate: f64) -> Option<KnownModelPrice> {
    match model_key {
        "mimo-v2.5-pro" | "xiaomi/mimo-v2.5-pro" => Some(cny_model_price(
            [7.0, 21.0, 0.0, 0.0],
            usd_to_cny_rate,
            "https://help.aliyun.com/zh/model-studio/billing-for-model-studio",
            "阿里云百炼 Xiaomi/MiMo 人民币价格；输入 ≤256K 档位，按当前 USD/CNY 设置折算",
            "2026-06-20",
        )),
        _ => None,
    }
}

fn public_reference_price(model_key: &str, usd_to_cny_rate: f64) -> Option<KnownModelPrice> {
    deepseek_price(model_key)
        .or_else(|| zhipu_price(model_key, usd_to_cny_rate))
        .or_else(|| kimi_price(model_key, usd_to_cny_rate))
        .or_else(|| minimax_price(model_key, usd_to_cny_rate))
        .or_else(|| aliyun_model_studio_price(model_key, usd_to_cny_rate))
        .or_else(|| aliyun_third_party_price(model_key, usd_to_cny_rate))
        .map(|mut price| {
            price.source_note =
                "OpenCode Zen 公称 zero-markup；按上游公开价估算，实际账单请以 Zen 为准";
            price
        })
}

fn official_model_price(
    provider: &Provider,
    model: &str,
    usd_to_cny_rate: f64,
) -> Option<ModelPrice> {
    let base_url = provider.base_url.to_ascii_lowercase();
    let model_key = model.to_ascii_lowercase();
    let known = if base_url.contains("api.deepseek.com") {
        deepseek_price(&model_key)
    } else if base_url.contains("api.openai.com") {
        openai_price(&model_key)
    } else if base_url.contains("generativelanguage.googleapis.com") {
        gemini_price(&model_key)
    } else if base_url.contains("open.bigmodel.cn") {
        zhipu_price(&model_key, usd_to_cny_rate)
    } else if is_kimi_official(&base_url) {
        kimi_price(&model_key, usd_to_cny_rate)
    } else if is_minimax_official(&base_url) {
        minimax_price(&model_key, usd_to_cny_rate)
    } else if is_aliyun_model_studio(&base_url) {
        aliyun_model_studio_price(&model_key, usd_to_cny_rate)
            .or_else(|| kimi_price(&model_key, usd_to_cny_rate))
            .or_else(|| minimax_price(&model_key, usd_to_cny_rate))
            .or_else(|| aliyun_third_party_price(&model_key, usd_to_cny_rate))
    } else if is_opencode_zen(&base_url) {
        public_reference_price(&model_key, usd_to_cny_rate)
    } else {
        None
    }?;

    Some(ModelPrice {
        provider_id: provider.id.clone(),
        model: model.to_string(),
        input_usd_per_million: known.input_usd_per_million,
        output_usd_per_million: known.output_usd_per_million,
        cached_usd_per_million: known.cache_read_usd_per_million,
        cache_read_usd_per_million: known.cache_read_usd_per_million,
        cache_write_usd_per_million: known.cache_write_usd_per_million,
        source_url: known.source_url.to_string(),
        source_note: known.source_note.to_string(),
        updated_at: known.updated_at.to_string(),
        automatic: true,
    })
}

fn reconcile_model_prices(config: &mut AppConfig) {
    let usd_to_cny_rate = config.usd_to_cny_rate;
    let manual_wildcards: HashSet<String> = config
        .model_prices
        .iter()
        .filter(|price| !price.automatic && price.provider_id.trim().is_empty())
        .map(|price| price.model.trim().to_ascii_lowercase())
        .filter(|model| !model.is_empty())
        .collect();
    let manual_exact: HashSet<(String, String)> = config
        .model_prices
        .iter()
        .filter(|price| !price.automatic && !price.provider_id.trim().is_empty())
        .map(|price| {
            (
                price.provider_id.clone(),
                price.model.trim().to_ascii_lowercase(),
            )
        })
        .collect();
    let providers_by_id: HashMap<&str, &Provider> = config
        .providers
        .iter()
        .map(|provider| (provider.id.as_str(), provider))
        .collect();
    let mut prices = Vec::new();
    let mut seen = HashSet::new();

    for mut price in config.model_prices.drain(..) {
        let model_key = price.model.trim().to_ascii_lowercase();
        if model_key.is_empty() {
            continue;
        }
        let key = (price.provider_id.clone(), model_key.clone());
        if seen.contains(&key) {
            continue;
        }

        if price.automatic {
            if manual_wildcards.contains(&model_key) || manual_exact.contains(&key) {
                continue;
            }
            let Some(provider) = providers_by_id.get(price.provider_id.as_str()) else {
                continue;
            };
            if !provider
                .models
                .iter()
                .any(|model| model.eq_ignore_ascii_case(&price.model))
            {
                continue;
            }
            let Some(current) = official_model_price(provider, &price.model, usd_to_cny_rate)
            else {
                continue;
            };
            price = current;
        }

        seen.insert(key);
        prices.push(price);
    }

    for provider in &config.providers {
        for model in &provider.models {
            let model_key = model.trim().to_ascii_lowercase();
            let key = (provider.id.clone(), model_key.clone());
            if model_key.is_empty() || manual_wildcards.contains(&model_key) || seen.contains(&key)
            {
                continue;
            }
            if let Some(price) = official_model_price(provider, model, usd_to_cny_rate) {
                seen.insert(key);
                prices.push(price);
            }
        }
    }

    config.model_prices = prices;
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
            model_routes: Vec::new(),
            model_prices: Vec::new(),
            usd_to_cny_rate: default_usd_to_cny_rate(),
            log_retention_days: default_log_retention_days(),
            max_log_entries: default_max_log_entries(),
            fusion: FusionConfig::default(),
        }
    }
}

impl Default for ModelRef {
    fn default() -> Self {
        Self {
            provider_id: String::new(),
            model: String::new(),
        }
    }
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            enabled: default_fusion_enabled(),
            panel_models: Vec::new(),
            judge_model: None,
            final_model: None,
            max_panel_models: default_fusion_max_panel_models(),
            timeout_secs: default_fusion_timeout_secs(),
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
    let mut provider_ids = HashSet::new();
    for provider in &mut config.providers {
        if provider.id.trim().is_empty() || !provider_ids.insert(provider.id.clone()) {
            provider.id = uuid::Uuid::new_v4().to_string();
            provider_ids.insert(provider.id.clone());
        }
    }

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
    if config.fusion.max_panel_models == 0 {
        config.fusion.max_panel_models = default_fusion_max_panel_models();
    }
    config.fusion.max_panel_models = config.fusion.max_panel_models.clamp(1, 8);
    if config.fusion.timeout_secs == 0 {
        config.fusion.timeout_secs = default_fusion_timeout_secs();
    }
    config.fusion.timeout_secs = config.fusion.timeout_secs.clamp(5, 600);
    config.fusion.panel_models = normalize_model_refs(config.fusion.panel_models);
    config.fusion.judge_model = normalize_optional_model_ref(config.fusion.judge_model);
    config.fusion.final_model = normalize_optional_model_ref(config.fusion.final_model);

    for price in &mut config.model_prices {
        if price.cache_read_usd_per_million <= 0.0 && price.cached_usd_per_million > 0.0 {
            price.cache_read_usd_per_million = price.cached_usd_per_million;
        }
        price.cached_usd_per_million = price.cache_read_usd_per_million;
    }
    reconcile_model_prices(&mut config);

    let mut configured_routes: HashMap<String, Vec<String>> = config
        .model_routes
        .drain(..)
        .filter(|route| !route.model.trim().is_empty())
        .map(|route| (route.model, route.provider_ids))
        .collect();
    let mut models = Vec::new();
    let mut seen_models = HashSet::new();
    for provider in &config.providers {
        for model in &provider.models {
            if !model.trim().is_empty() && seen_models.insert(model.clone()) {
                models.push(model.clone());
            }
        }
    }

    config.model_routes = models
        .into_iter()
        .map(|model| {
            let mut matching: Vec<_> = config
                .providers
                .iter()
                .enumerate()
                .filter(|(_, provider)| provider.models.iter().any(|item| item == &model))
                .collect();
            matching.sort_by_key(|(index, provider)| (provider.priority, *index));
            let matching_ids: HashSet<_> = matching
                .iter()
                .map(|(_, provider)| provider.id.as_str())
                .collect();
            let mut ordered = Vec::new();
            let mut seen = HashSet::new();

            for provider_id in configured_routes.remove(&model).unwrap_or_default() {
                if matching_ids.contains(provider_id.as_str()) && seen.insert(provider_id.clone()) {
                    ordered.push(provider_id);
                }
            }
            for (_, provider) in matching {
                if seen.insert(provider.id.clone()) {
                    ordered.push(provider.id.clone());
                }
            }

            ModelRoute {
                model,
                provider_ids: ordered,
            }
        })
        .collect();

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

pub(crate) fn normalize_model_ref(mut model_ref: ModelRef) -> Option<ModelRef> {
    model_ref.provider_id = model_ref.provider_id.trim().to_string();
    model_ref.model = model_ref.model.trim().to_string();
    if model_ref.provider_id.is_empty() || model_ref.model.is_empty() {
        None
    } else {
        Some(model_ref)
    }
}

pub(crate) fn normalize_optional_model_ref(model_ref: Option<ModelRef>) -> Option<ModelRef> {
    model_ref.and_then(normalize_model_ref)
}

pub(crate) fn normalize_model_refs(model_refs: Vec<ModelRef>) -> Vec<ModelRef> {
    let mut seen = HashSet::new();
    model_refs
        .into_iter()
        .filter_map(normalize_model_ref)
        .filter(|model_ref| seen.insert((model_ref.provider_id.clone(), model_ref.model.clone())))
        .collect()
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
        assert!(config.model_routes.is_empty());
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

    #[test]
    fn model_routes_migrate_from_global_priority_and_preserve_custom_order() {
        let mut config = AppConfig {
            providers: vec![
                Provider {
                    id: "first".to_string(),
                    models: vec!["shared".to_string(), "only-first".to_string()],
                    priority: 1,
                    ..Default::default()
                },
                Provider {
                    id: "second".to_string(),
                    models: vec!["shared".to_string()],
                    priority: 0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        config = normalize_config(config);
        assert_eq!(config.model_routes[0].model, "shared");
        assert_eq!(config.model_routes[0].provider_ids, ["second", "first"]);

        config.model_routes[0].provider_ids = vec!["first".to_string(), "second".to_string()];
        config = normalize_config(config);
        assert_eq!(config.model_routes[0].provider_ids, ["first", "second"]);
        assert_eq!(config.model_routes[1].provider_ids, ["first"]);
    }

    #[test]
    fn official_prices_are_provider_specific_and_manual_prices_win() {
        let mut config = AppConfig {
            providers: vec![Provider {
                id: "openai".to_string(),
                base_url: "https://api.openai.com".to_string(),
                models: vec!["gpt-4o".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        config = normalize_config(config);
        assert_eq!(config.model_prices.len(), 1);
        assert_eq!(config.model_prices[0].provider_id, "openai");
        assert_eq!(config.model_prices[0].input_usd_per_million, 2.5);
        assert!(config.model_prices[0].automatic);

        config.model_prices.push(ModelPrice {
            provider_id: "openai".to_string(),
            model: "gpt-4o".to_string(),
            input_usd_per_million: 1.0,
            output_usd_per_million: 2.0,
            cached_usd_per_million: 0.0,
            cache_read_usd_per_million: 0.0,
            cache_write_usd_per_million: 0.0,
            source_url: String::new(),
            source_note: String::new(),
            updated_at: String::new(),
            automatic: false,
        });
        config = normalize_config(config);

        assert_eq!(config.model_prices.len(), 1);
        assert_eq!(config.model_prices[0].input_usd_per_million, 1.0);
        assert!(!config.model_prices[0].automatic);
    }

    #[test]
    fn known_provider_prices_cover_current_presets() {
        let mut config = AppConfig {
            providers: vec![
                Provider {
                    id: "zhipu".to_string(),
                    base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
                    models: vec![
                        "glm-5.2".to_string(),
                        "glm-5.1".to_string(),
                        "glm-5".to_string(),
                    ],
                    ..Default::default()
                },
                Provider {
                    id: "kimi".to_string(),
                    base_url: "https://api.moonshot.cn/v1".to_string(),
                    models: vec![
                        "kimi-k2.5".to_string(),
                        "kimi-k2.6".to_string(),
                        "kimi-k2.7-code".to_string(),
                    ],
                    ..Default::default()
                },
                Provider {
                    id: "minimax".to_string(),
                    base_url: "https://api.minimaxi.com/v1".to_string(),
                    models: vec![
                        "minimax-m3".to_string(),
                        "minimax-m2.7".to_string(),
                        "minimax-m2.5".to_string(),
                    ],
                    ..Default::default()
                },
                Provider {
                    id: "aliyun".to_string(),
                    base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
                    models: vec![
                        "qwen3.7-max".to_string(),
                        "qwen3.7-plus".to_string(),
                        "qwen3.6-plus".to_string(),
                        "qwen3.5-plus".to_string(),
                        "mimo-v2.5-pro".to_string(),
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        config = normalize_config(config);

        for model in [
            "glm-5.2",
            "glm-5.1",
            "glm-5",
            "kimi-k2.5",
            "kimi-k2.6",
            "kimi-k2.7-code",
            "minimax-m3",
            "minimax-m2.7",
            "minimax-m2.5",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.6-plus",
            "qwen3.5-plus",
            "mimo-v2.5-pro",
        ] {
            assert!(
                config.model_prices.iter().any(|price| price.model == model),
                "missing automatic price for {model}"
            );
        }

        let glm_52 = config
            .model_prices
            .iter()
            .find(|price| price.model == "glm-5.2")
            .unwrap();
        assert_eq!(glm_52.input_usd_per_million, 8.0 / 7.2);
        assert_eq!(glm_52.output_usd_per_million, 28.0 / 7.2);

        let minimax_m27 = config
            .model_prices
            .iter()
            .find(|price| price.model == "minimax-m2.7")
            .unwrap();
        assert_eq!(minimax_m27.cache_write_usd_per_million, 2.625 / 7.2);

        let qwen = config
            .model_prices
            .iter()
            .find(|price| price.model == "qwen3.7-plus")
            .unwrap();
        assert_eq!(qwen.cache_read_usd_per_million, 0.4 / 7.2);
        assert_eq!(qwen.cache_write_usd_per_million, 2.5 / 7.2);
    }

    #[test]
    fn unknown_aggregator_models_do_not_get_guessed_prices() {
        let mut config = AppConfig {
            providers: vec![Provider {
                id: "aggregator".to_string(),
                base_url: "https://ai.soruxgpt.com".to_string(),
                models: vec!["gpt-5.5".to_string(), "claude-opus-4-8".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        config = normalize_config(config);
        assert!(config.model_prices.is_empty());
    }
}
