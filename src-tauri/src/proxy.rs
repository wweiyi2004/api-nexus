use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::anthropic;
use crate::config::{AppConfig, Provider};
use crate::fusion;
use crate::responses;
pub use crate::storage::RequestLogEntry;
use crate::storage::RequestLogStore;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const PROTOCOL_ANTHROPIC: &str = "anthropic";
const MAX_STORED_REQUEST_BODY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl TokenUsage {
    fn is_empty(self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cached_tokens == 0
            && self.cache_read_tokens == 0
            && self.cache_write_tokens == 0
    }

    fn absorb_max(&mut self, usage: TokenUsage) {
        self.input_tokens = self.input_tokens.max(usage.input_tokens);
        self.output_tokens = self.output_tokens.max(usage.output_tokens);
        self.cached_tokens = self.cached_tokens.max(usage.cached_tokens);
        self.cache_read_tokens = self.cache_read_tokens.max(usage.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.max(usage.cache_write_tokens);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStats {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl TokenStats {
    fn record(&mut self, usage: TokenUsage) {
        if usage.is_empty() {
            return;
        }

        self.request_count += 1;
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cached_tokens += usage.cached_tokens;
        self.cache_read_tokens += usage.cache_read_tokens;
        self.cache_write_tokens += usage.cache_write_tokens;
    }
}

pub type TokenStatsState = Arc<RwLock<TokenStats>>;

pub type RequestLogState = Arc<RequestLogStore>;

pub async fn push_request_log(state: &RequestLogState, entry: RequestLogEntry) {
    if let Err(error) = state.push(entry).await {
        log::error!("Failed to persist request log: {}", error);
    }
}

#[allow(clippy::too_many_arguments)]
async fn log_request_with_body(
    logs: &RequestLogState,
    ctx: &RequestLogContext,
    usage: TokenUsage,
    error: Option<String>,
) {
    let entry = RequestLogEntry {
        id: 0,
        timestamp: chrono::Utc::now().timestamp(),
        method: ctx.method.to_string(),
        path: ctx.path.clone(),
        model: ctx.model.clone(),
        provider: ctx.provider.clone(),
        provider_id: ctx.provider_id.clone(),
        api_key_name: ctx.api_key_name.clone(),
        status: ctx.status,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        duration_ms: ctx.started.elapsed().as_millis() as u64,
        error,
        request_body: ctx.request_body.clone(),
    };
    push_request_log(logs, entry).await;
}

#[derive(Clone)]
struct RequestLogContext {
    method: &'static str,
    path: String,
    model: String,
    provider: String,
    provider_id: String,
    api_key_name: String,
    status: u16,
    started: std::time::Instant,
    request_body: Option<String>,
}

/// Which upstream API surface a proxy handler speaks. Used to render
/// client-visible error bodies in the dialect the caller expects, so the
/// OpenAI and Anthropic handlers can share their flow.
#[derive(Clone, Copy)]
enum ApiDialect {
    OpenAI,
    Anthropic,
}

/// Semantic class of a proxy error, mapped to each dialect's concrete `type`
/// string by [`ApiDialect::error_type`]. Centralizes the OpenAI/Anthropic
/// differences that used to be duplicated across both handlers.
#[derive(Clone, Copy)]
enum ErrorKind {
    /// 400/405 — invalid request. Both dialects use `invalid_request_error`.
    BadRequest,
    /// 404 — no matching provider. OpenAI keeps `invalid_request_error`,
    /// Anthropic uses `not_found_error`.
    NotFound,
    /// 5xx — upstream/server failure. OpenAI uses `server_error`, Anthropic
    /// uses `api_error`.
    Upstream,
}

impl ApiDialect {
    fn error_type(self, kind: ErrorKind) -> &'static str {
        match (self, kind) {
            (_, ErrorKind::BadRequest) => "invalid_request_error",
            (ApiDialect::OpenAI, ErrorKind::NotFound) => "invalid_request_error",
            (ApiDialect::Anthropic, ErrorKind::NotFound) => "not_found_error",
            (ApiDialect::OpenAI, ErrorKind::Upstream) => "server_error",
            (ApiDialect::Anthropic, ErrorKind::Upstream) => "api_error",
        }
    }

    /// Build the JSON error body in this dialect's shape. OpenAI nests under
    /// `error.{message,type}`; Anthropic wraps that in a top-level
    /// `{"type":"error", ...}` envelope. The one shape NOT produced here is
    /// OpenAI's invalid-JSON body, which is a flat `{"error": "<msg>"}` string
    /// and stays inlined at its call site.
    fn error_body(self, kind: ErrorKind, message: &str, details: Option<&[String]>) -> Value {
        let error_type = self.error_type(kind);
        match self {
            ApiDialect::OpenAI => {
                let mut error = json!({ "message": message, "type": error_type });
                if let Some(details) = details {
                    error["details"] = json!(details);
                }
                json!({ "error": error })
            }
            ApiDialect::Anthropic => {
                let mut error = json!({ "type": error_type, "message": message });
                if let Some(details) = details {
                    error["details"] = json!(details);
                }
                json!({ "type": "error", "error": error })
            }
        }
    }

    fn error_response(
        self,
        status: StatusCode,
        kind: ErrorKind,
        message: &str,
        details: Option<&[String]>,
    ) -> Response {
        (status, Json(self.error_body(kind, message, details))).into_response()
    }
}

#[derive(Clone)]
pub struct ProxyState {
    pub config: Arc<RwLock<AppConfig>>,
    pub client: Client,
    pub token_stats: TokenStatsState,
    pub request_logs: RequestLogState,
}

#[cfg(test)]
pub fn create_proxy_router(config: Arc<RwLock<AppConfig>>) -> Router {
    create_proxy_router_with_stats(
        config,
        Arc::new(RwLock::new(TokenStats::default())),
        Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap()),
    )
}

pub fn create_proxy_router_with_stats(
    config: Arc<RwLock<AppConfig>>,
    token_stats: TokenStatsState,
    request_logs: RequestLogState,
) -> Router {
    let state = ProxyState {
        config,
        token_stats,
        request_logs,
        // No total request timeout: it would cut off long streaming responses.
        // Stalls are guarded by the read timeout, which resets on every chunk.
        client: Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(300))
            .pool_idle_timeout(std::time::Duration::from_secs(600))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .unwrap(),
    };

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            is_loopback_origin(origin)
        }))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("x-api-key"),
            HeaderName::from_static("anthropic-version"),
            HeaderName::from_static("anthropic-beta"),
        ]);

    Router::new()
        .route("/v1/responses", any(responses_handler))
        .route("/v1/chat/completions", any(proxy_handler))
        .route("/v1/completions", any(proxy_handler))
        .route("/v1/embeddings", any(proxy_handler))
        .route("/v1/messages", any(anthropic_handler))
        .route("/v1/messages/count_tokens", any(anthropic_handler))
        .route("/v1/models", get(list_models_handler))
        .route("/health", get(health_handler))
        .with_state(state)
        .layer(cors)
}

fn is_loopback_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    [
        "http://localhost",
        "https://localhost",
        "http://127.0.0.1",
        "https://127.0.0.1",
        "http://[::1]",
        "https://[::1]",
    ]
    .iter()
    .any(|prefix| {
        origin
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'))
    })
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "API Nexus",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn list_models_handler(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    let config = state.config.read().await;
    if let Err(response) = authorize_proxy_request(&headers, &config) {
        return *response;
    }

    let mut models: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for provider in &config.providers {
        if !provider.enabled {
            continue;
        }
        for model in &provider.models {
            if seen.insert(model.clone()) {
                models.push(json!({
                    "id": model,
                    "object": "model",
                    "owned_by": provider.name,
                }));
            }
        }
    }

    if is_anthropic_client(&headers) {
        let first_id = models.first().and_then(|model| model.get("id")).cloned();
        let last_id = models.last().and_then(|model| model.get("id")).cloned();
        let anthropic_models: Vec<Value> = models
            .into_iter()
            .map(|model| {
                let id = model.get("id").cloned().unwrap_or(Value::Null);
                let display_name = id.as_str().unwrap_or_default().to_string();
                json!({
                    "id": id,
                    "type": "model",
                    "display_name": display_name,
                    "created_at": "1970-01-01T00:00:00Z"
                })
            })
            .collect();

        return Json(json!({
            "data": anthropic_models,
            "has_more": false,
            "first_id": first_id,
            "last_id": last_id
        }))
        .into_response();
    }

    Json(json!({
        "object": "list",
        "data": models
    }))
    .into_response()
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") {
        Some(token.trim())
    } else {
        None
    }
}

fn x_api_key(headers: &HeaderMap) -> Option<&str> {
    headers.get("x-api-key")?.to_str().ok().map(str::trim)
}

fn incoming_api_key(headers: &HeaderMap) -> Option<&str> {
    bearer_token(headers).or_else(|| x_api_key(headers))
}

fn is_anthropic_client(headers: &HeaderMap) -> bool {
    headers.contains_key("anthropic-version")
}

fn resolve_model_alias<'a>(config: &'a AppConfig, model: &'a str) -> &'a str {
    if model.is_empty() {
        return model;
    }
    for alias in &config.model_aliases {
        if alias.alias.eq_ignore_ascii_case(model) {
            return &alias.model;
        }
    }
    model
}

pub(crate) fn is_anthropic_provider(provider: &Provider) -> bool {
    provider.protocol.eq_ignore_ascii_case(PROTOCOL_ANTHROPIC)
}

fn provider_matches_model(provider: &Provider, model: &str) -> bool {
    provider.enabled && provider.models.iter().any(|m| m == model)
}

fn sort_providers_for_model(config: &AppConfig, model: &str, providers: &mut [Provider]) {
    let route_positions: BTreeMap<&str, usize> = config
        .model_routes
        .iter()
        .find(|route| route.model == model)
        .map(|route| {
            route
                .provider_ids
                .iter()
                .enumerate()
                .map(|(index, provider_id)| (provider_id.as_str(), index))
                .collect()
        })
        .unwrap_or_default();

    providers.sort_by_key(|provider| {
        (
            route_positions
                .get(provider.id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            provider.priority,
        )
    });
}

fn base_url_has_path(base_url: &str) -> bool {
    let trimmed = base_url.trim_end_matches('/');
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);

    without_scheme.contains('/')
}

fn base_url_ends_with_version_segment(base_url: &str) -> bool {
    let Some(segment) = base_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
    else {
        return false;
    };

    let lower = segment.to_ascii_lowercase();
    lower == "v1"
        || lower == "v1beta"
        || lower == "v3"
        || lower == "v4"
        || lower.chars().all(|ch| ch.is_ascii_digit())
}

fn strip_standard_v1_prefix(path: &str) -> &str {
    let trimmed = path.trim_start_matches('/');
    trimmed.strip_prefix("v1/").unwrap_or(trimmed)
}

fn join_upstream_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub(crate) fn openai_upstream_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let requested = path.trim_start_matches('/');
    let without_v1 = strip_standard_v1_prefix(path);

    if base.ends_with(requested) || base.ends_with(without_v1) {
        return base.to_string();
    }

    if base_url_has_path(base) {
        join_upstream_url(base, without_v1)
    } else {
        join_upstream_url(base, requested)
    }
}

pub(crate) fn anthropic_upstream_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let requested = path.trim_start_matches('/');
    let without_v1 = strip_standard_v1_prefix(path);

    if base.ends_with(requested) || base.ends_with(without_v1) {
        return base.to_string();
    }

    if base_url_ends_with_version_segment(base) {
        join_upstream_url(base, without_v1)
    } else {
        join_upstream_url(base, requested)
    }
}

fn auth_error_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": "Invalid API key",
                "type": "authentication_error"
            }
        })),
    )
        .into_response()
}

fn authorize_proxy_request(
    headers: &HeaderMap,
    config: &AppConfig,
) -> Result<String, Box<Response>> {
    let mut active_keys: Vec<(&str, &str)> = config
        .proxy_api_keys
        .iter()
        .filter(|key| key.enabled && !key.key.trim().is_empty())
        .map(|key| (key.key.as_str(), key.name.as_str()))
        .collect();

    if active_keys.is_empty() && !config.proxy_api_key.trim().is_empty() {
        active_keys.push((config.proxy_api_key.as_str(), "默认密钥"));
    }

    if active_keys.is_empty() {
        return Ok("未验证".to_string());
    }

    let Some(incoming_key) = incoming_api_key(headers) else {
        return Err(Box::new(auth_error_response()));
    };

    active_keys
        .into_iter()
        .find_map(|(key, name)| {
            if incoming_key == key {
                Some(name.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| Box::new(auth_error_response()))
}

fn bad_gateway(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": message.into(),
                "type": "server_error"
            }
        })),
    )
        .into_response()
}

fn replay_request_body(path: &str, body: &Value) -> Option<String> {
    if !matches!(
        path,
        "/v1/chat/completions" | "/v1/messages" | "/v1/responses"
    ) {
        return None;
    }
    let body_text = body.to_string();
    if body_text.len() > MAX_STORED_REQUEST_BODY_BYTES {
        None
    } else {
        Some(body_text)
    }
}

fn first_u64_field(value: &Value, field_names: &[&str]) -> u64 {
    field_names
        .iter()
        .find_map(|field_name| value.get(*field_name).and_then(Value::as_u64))
        .unwrap_or_default()
}

fn sum_named_token_fields(value: &Value, field_names: &[&str]) -> u64 {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, nested)| {
                let current = if field_names.contains(&key.as_str()) {
                    nested.as_u64().unwrap_or_default()
                } else {
                    0
                };
                current + sum_named_token_fields(nested, field_names)
            })
            .sum(),
        Value::Array(items) => items
            .iter()
            .map(|item| sum_named_token_fields(item, field_names))
            .sum(),
        _ => 0,
    }
}

pub(crate) fn extract_token_usage(value: &Value) -> TokenUsage {
    let usage = value
        .get("usage")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
        })
        .unwrap_or(value);

    let cache_read_tokens = sum_named_token_fields(
        usage,
        &[
            "cached_tokens",
            "cache_read_input_tokens",
            "cache_read_tokens",
            "cache_read",
        ],
    );
    let cache_write_tokens = sum_named_token_fields(
        usage,
        &[
            "cache_creation_input_tokens",
            "cache_write_tokens",
            "cache_creation",
            "cache_write",
        ],
    );

    TokenUsage {
        input_tokens: first_u64_field(usage, &["prompt_tokens", "input_tokens"]),
        output_tokens: first_u64_field(usage, &["completion_tokens", "output_tokens"]),
        cached_tokens: cache_read_tokens + cache_write_tokens,
        cache_read_tokens,
        cache_write_tokens,
    }
}

fn extract_token_usage_from_sse_frame(frame: &str) -> TokenUsage {
    let (_, data) = parse_sse_frame(frame);
    let Some(data) = data else {
        return TokenUsage::default();
    };
    if data.trim() == "[DONE]" {
        return TokenUsage::default();
    }

    serde_json::from_str::<Value>(&data)
        .map(|value| extract_token_usage(&value))
        .unwrap_or_default()
}

async fn record_token_usage(token_stats: &TokenStatsState, usage: TokenUsage) {
    if usage.is_empty() {
        return;
    }

    token_stats.write().await.record(usage);
}

async fn passthrough_response(
    resp: reqwest::Response,
    is_stream: bool,
    token_stats: TokenStatsState,
    request_logs: RequestLogState,
    log_context: RequestLogContext,
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();

    let stream = async_stream::stream! {
        let mut upstream = resp.bytes_stream();
        let mut buffer = Vec::new();
        let mut usage = TokenUsage::default();
        let mut accumulated = Vec::new();

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    if is_stream {
                        buffer.extend_from_slice(&bytes);
                        while let Some(frame) = take_sse_frame(&mut buffer) {
                            usage.absorb_max(extract_token_usage_from_sse_frame(&frame));
                        }
                    } else {
                        accumulated.extend_from_slice(&bytes);
                    }

                    yield Ok::<Bytes, io::Error>(bytes);
                }
                Err(err) => {
                    yield Err(io::Error::other(err.to_string()));
                    return;
                }
            }
        }

        if is_stream {
            if !buffer.iter().all(u8::is_ascii_whitespace) {
                let frame = String::from_utf8_lossy(&buffer);
                usage.absorb_max(extract_token_usage_from_sse_frame(&frame));
            }
            record_token_usage(&token_stats, usage).await;
            log_request_with_body(&request_logs, &log_context, usage, None).await;
        } else if let Ok(body_json) = serde_json::from_slice::<Value>(&accumulated) {
            let usage = extract_token_usage(&body_json);
            record_token_usage(&token_stats, usage).await;
            log_request_with_body(&request_logs, &log_context, usage, None).await;
        } else {
            log_request_with_body(
                &request_logs,
                &log_context,
                TokenUsage::default(),
                Some("Failed to parse response usage".to_string()),
            )
            .await;
        }
    };

    let mut builder = Response::builder().status(status);
    if is_stream {
        builder = builder
            .header(
                header::CONTENT_TYPE,
                content_type.unwrap_or_else(|| HeaderValue::from_static("text/event-stream")),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    } else if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    } else {
        builder = builder.header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }

    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|err| bad_gateway(format!("Failed to build stream response: {}", err)))
}

fn copy_optional_header(
    mut req: reqwest::RequestBuilder,
    incoming_headers: &HeaderMap,
    header_name: &'static str,
) -> reqwest::RequestBuilder {
    if let Some(value) = incoming_headers.get(header_name) {
        req = req.header(header_name, value);
    }
    req
}

fn anthropic_request(
    state: &ProxyState,
    provider: &Provider,
    path: &str,
    headers: &HeaderMap,
    body_json: &Value,
) -> reqwest::RequestBuilder {
    let url = anthropic_upstream_url(&provider.base_url, path);
    let version = headers
        .get("anthropic-version")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(ANTHROPIC_VERSION);

    let mut req = state
        .client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-api-key", provider.api_key.clone())
        .header("anthropic-version", version);

    req = copy_optional_header(req, headers, "anthropic-beta");

    if let Some(ua) = headers.get(header::USER_AGENT) {
        req = req.header(header::USER_AGENT, ua);
    }

    req.json(body_json)
}

fn openai_request(
    state: &ProxyState,
    provider: &Provider,
    path: &str,
    headers: &HeaderMap,
    body_json: &Value,
) -> reqwest::RequestBuilder {
    let url = openai_upstream_url(&provider.base_url, path);
    let mut req = state
        .client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", provider.api_key),
        );

    if let Some(ua) = headers.get(header::USER_AGENT) {
        req = req.header(header::USER_AGENT, ua);
    }

    req.json(body_json)
}

fn extract_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn normalize_anthropic_content(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(blocks) => Value::Array(
            blocks
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        Some(json!({
                            "type": "text",
                            "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                        }))
                    } else {
                        None
                    }
                })
                .collect(),
        ),
        _ => Value::String(String::new()),
    }
}

pub(crate) fn openai_to_anthropic_request(openai: &Value) -> Result<Value, String> {
    let model = openai
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing model".to_string())?;
    let messages = openai
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Missing messages".to_string())?;

    let mut system_parts = Vec::new();
    let mut anthropic_messages = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let content = message.get("content").unwrap_or(&Value::Null);

        match role {
            "system" | "developer" => {
                let text = extract_content_text(content);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "assistant" => {
                anthropic_messages.push(json!({
                    "role": "assistant",
                    "content": normalize_anthropic_content(content)
                }));
            }
            _ => {
                anthropic_messages.push(json!({
                    "role": "user",
                    "content": normalize_anthropic_content(content)
                }));
            }
        }
    }

    if anthropic_messages.is_empty() {
        anthropic_messages.push(json!({"role": "user", "content": ""}));
    }

    let max_tokens = openai
        .get("max_tokens")
        .or_else(|| openai.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(1024);

    let mut request = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": anthropic_messages
    });

    if !system_parts.is_empty() {
        request["system"] = Value::String(system_parts.join("\n\n"));
    }

    for field in ["temperature", "top_p", "top_k", "stream"] {
        if let Some(value) = openai.get(field) {
            if !value.is_null() {
                request[field] = value.clone();
            }
        }
    }

    if let Some(stop) = openai.get("stop") {
        if !stop.is_null() {
            match stop {
                Value::String(stop) => {
                    request["stop_sequences"] = json!([stop]);
                }
                Value::Array(_) => {
                    request["stop_sequences"] = stop.clone();
                }
                _ => {}
            }
        }
    }

    if let Some(tools) = openai.get("tools").and_then(Value::as_array) {
        let anthropic_tools: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function")?;
                let name = function.get("name")?.as_str()?;
                Some(json!({
                    "name": name,
                    "description": function.get("description").and_then(Value::as_str).unwrap_or_default(),
                    "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                }))
            })
            .collect();

        if !anthropic_tools.is_empty() {
            request["tools"] = Value::Array(anthropic_tools);
        }
    }

    if let Some(tool_choice) = openai.get("tool_choice") {
        if !tool_choice.is_null() {
            match tool_choice {
                Value::String(choice) if choice == "required" => {
                    request["tool_choice"] = json!({"type": "any"});
                }
                Value::String(choice) if choice == "auto" => {
                    request["tool_choice"] = json!({"type": "auto"});
                }
                Value::String(choice) if choice == "none" => {
                    // Anthropic has no "none" tool_choice; omit the field.
                }
                Value::Object(_) => {
                    if let Some(name) = tool_choice
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                    {
                        request["tool_choice"] = json!({"type": "tool", "name": name});
                    }
                }
                _ => {}
            }
        }
    }

    Ok(request)
}

fn anthropic_message_to_openai_messages(message: &Value) -> Vec<Value> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let content = message.get("content").unwrap_or(&Value::Null);

    if role == "assistant" {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        match content {
            Value::String(text) => text_parts.push(text.clone()),
            Value::Array(blocks) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                text_parts.push(text.to_string());
                            }
                        }
                        Some("tool_use") => {
                            let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let arguments = block
                                .get("input")
                                .map(Value::to_string)
                                .unwrap_or_else(|| "{}".to_string());
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": arguments
                                }
                            }));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        let mut openai_message = json!({
            "role": "assistant",
            "content": text_parts.join("\n")
        });

        if !tool_calls.is_empty() {
            openai_message["tool_calls"] = Value::Array(tool_calls);
        }

        return vec![openai_message];
    }

    let mut openai_messages = Vec::new();
    let mut text_parts = Vec::new();

    match content {
        Value::String(text) => {
            text_parts.push(text.clone());
        }
        Value::Array(blocks) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            text_parts.push(text.to_string());
                        }
                    }
                    Some("tool_result") => {
                        if !text_parts.is_empty() {
                            openai_messages.push(json!({
                                "role": "user",
                                "content": text_parts.join("\n")
                            }));
                            text_parts.clear();
                        }

                        let tool_content = block
                            .get("content")
                            .map(extract_content_text)
                            .unwrap_or_default();
                        let tool_call_id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        openai_messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": tool_content
                        }));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    if !text_parts.is_empty() || openai_messages.is_empty() {
        openai_messages.push(json!({
            "role": if role == "system" { "system" } else { "user" },
            "content": text_parts.join("\n")
        }));
    }

    openai_messages
}

pub(crate) fn anthropic_to_openai_chat_request(anthropic: &Value) -> Result<Value, String> {
    let model = anthropic
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing model".to_string())?;
    let messages = anthropic
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Missing messages".to_string())?;

    let mut openai_messages = Vec::new();

    if let Some(system) = anthropic.get("system") {
        let text = extract_content_text(system);
        if !text.is_empty() {
            openai_messages.push(json!({
                "role": "system",
                "content": text
            }));
        }
    }

    for message in messages {
        openai_messages.extend(anthropic_message_to_openai_messages(message));
    }

    if openai_messages.is_empty() {
        openai_messages.push(json!({"role": "user", "content": ""}));
    }

    let mut request = json!({
        "model": model,
        "messages": openai_messages
    });

    if let Some(max_tokens) = anthropic.get("max_tokens") {
        request["max_tokens"] = max_tokens.clone();
    }

    for field in ["temperature", "top_p", "stream"] {
        if let Some(value) = anthropic.get(field) {
            if !value.is_null() {
                request[field] = value.clone();
            }
        }
    }

    if let Some(stop_sequences) = anthropic.get("stop_sequences") {
        if !stop_sequences.is_null() {
            request["stop"] = stop_sequences.clone();
        }
    }

    if let Some(tools) = anthropic.get("tools").and_then(Value::as_array) {
        let openai_tools: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": tool.get("description").and_then(Value::as_str).unwrap_or_default(),
                        "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                    }
                }))
            })
            .collect();

        if !openai_tools.is_empty() {
            request["tools"] = Value::Array(openai_tools);
        }
    }

    if let Some(tool_choice) = anthropic.get("tool_choice") {
        if !tool_choice.is_null() {
            let mapped = match tool_choice {
                Value::Object(choice) => match choice.get("type").and_then(Value::as_str) {
                    Some("auto") => Some(Value::String("auto".to_string())),
                    Some("any") => Some(Value::String("required".to_string())),
                    Some("tool") => choice
                        .get("name")
                        .and_then(Value::as_str)
                        .map(|name| json!({"type": "function", "function": {"name": name}})),
                    _ => None,
                },
                _ => None,
            };
            if let Some(value) = mapped {
                request["tool_choice"] = value;
            }
        }
    }

    Ok(request)
}

fn unix_timestamp() -> i64 {
    chrono::Utc::now().timestamp()
}

fn anthropic_stop_reason_to_openai(reason: Option<&str>) -> Option<&'static str> {
    match reason {
        Some("max_tokens") => Some("length"),
        Some("tool_use") => Some("tool_calls"),
        Some("end_turn") | Some("stop_sequence") => Some("stop"),
        Some(_) => Some("stop"),
        None => None,
    }
}

fn anthropic_text_and_tool_calls(content: &Value) -> (String, Vec<Value>) {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    if let Some(blocks) = content.as_array() {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        text_parts.push(text.to_string());
                    }
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let arguments = block
                        .get("input")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".to_string());
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    (text_parts.join(""), tool_calls)
}

fn anthropic_to_openai_response(anthropic: Value, model: &str) -> Value {
    let (content, tool_calls) = anthropic_text_and_tool_calls(&anthropic["content"]);
    let mut message = json!({
        "role": "assistant",
        "content": if content.is_empty() && !tool_calls.is_empty() { Value::Null } else { Value::String(content) }
    });

    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    let input_tokens = anthropic["usage"]["input_tokens"]
        .as_u64()
        .unwrap_or_default();
    let output_tokens = anthropic["usage"]["output_tokens"]
        .as_u64()
        .unwrap_or_default();

    json!({
        "id": anthropic.get("id").and_then(Value::as_str).unwrap_or("chatcmpl-anthropic"),
        "object": "chat.completion",
        "created": unix_timestamp(),
        "model": anthropic.get("model").and_then(Value::as_str).unwrap_or(model),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": anthropic_stop_reason_to_openai(anthropic.get("stop_reason").and_then(Value::as_str))
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    })
}

fn openai_finish_reason_to_anthropic(reason: Option<&str>) -> &'static str {
    match reason {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ => "end_turn",
    }
}

fn openai_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({"arguments": arguments}))
}

fn openai_to_anthropic_response(openai: Value, request_model: &str) -> Value {
    let choice = openai
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .cloned()
        .unwrap_or_default();
    let message = choice.get("message").cloned().unwrap_or_default();
    let mut content_blocks = Vec::new();

    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            content_blocks.push(json!({"type": "text", "text": text}));
        }
    } else if let Some(parts) = message.get("content").and_then(Value::as_array) {
        for part in parts {
            if part.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    content_blocks.push(json!({"type": "text", "text": text}));
                }
            }
        }
    }

    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let function = tool_call.get("function").unwrap_or(&Value::Null);
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            content_blocks.push(json!({
                "type": "tool_use",
                "id": tool_call.get("id").and_then(Value::as_str).unwrap_or_default(),
                "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
                "input": openai_tool_arguments(arguments)
            }));
        }
    }

    if content_blocks.is_empty() {
        content_blocks.push(json!({"type": "text", "text": ""}));
    }

    let input_tokens = openai["usage"]["prompt_tokens"]
        .as_u64()
        .unwrap_or_default();
    let output_tokens = openai["usage"]["completion_tokens"]
        .as_u64()
        .unwrap_or_default();

    json!({
        "id": openai.get("id").and_then(Value::as_str).unwrap_or("msg_openai"),
        "type": "message",
        "role": "assistant",
        "model": openai.get("model").and_then(Value::as_str).unwrap_or(request_model),
        "content": content_blocks,
        "stop_reason": openai_finish_reason_to_anthropic(choice.get("finish_reason").and_then(Value::as_str)),
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens
        }
    })
}

fn estimate_anthropic_tokens(value: &Value) -> u64 {
    fn count_text_chars(value: &Value) -> usize {
        match value {
            Value::String(text) => text.chars().count(),
            Value::Array(items) => items.iter().map(count_text_chars).sum(),
            Value::Object(map) => map.values().map(count_text_chars).sum(),
            _ => 0,
        }
    }

    let chars = count_text_chars(value);
    chars.div_ceil(4).max(1) as u64
}

fn parse_sse_frame(frame: &str) -> (Option<String>, Option<String>) {
    let mut event = None;
    let mut data_lines = Vec::new();

    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }

    let data = if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    };

    (event, data)
}

/// Removes and decodes one complete SSE frame. Network chunks may split a
/// UTF-8 code point, so decoding must happen only after a frame delimiter has
/// been found in the accumulated byte buffer.
fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<String> {
    fn end_of_line(bytes: &[u8]) -> Option<usize> {
        if bytes.starts_with(b"\r\n") {
            Some(2)
        } else if bytes.starts_with(b"\r") || bytes.starts_with(b"\n") {
            Some(1)
        } else {
            None
        }
    }

    let mut delimiter = None;
    for index in 0..buffer.len() {
        if let Some(first_len) = end_of_line(&buffer[index..]) {
            if let Some(second_len) = end_of_line(&buffer[index + first_len..]) {
                delimiter = Some((index, first_len + second_len));
                break;
            }
        }
    }

    let (frame_end, delimiter_len) = delimiter?;
    let frame = buffer.drain(..frame_end).collect::<Vec<_>>();
    buffer.drain(..delimiter_len);
    Some(String::from_utf8_lossy(&frame).into_owned())
}

fn openai_stream_chunk(id: &str, model: &str, delta: Value, finish_reason: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": unix_timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }]
        })
    )
}

fn anthropic_sse_frame(event: &str, data: Value) -> String {
    format!("event: {}\ndata: {}\n\n", event, data)
}

#[derive(Default)]
struct OpenAiToolDelta {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

async fn openai_stream_to_anthropic_response(
    resp: reqwest::Response,
    model: String,
    token_stats: TokenStatsState,
    request_logs: RequestLogState,
    log_context: RequestLogContext,
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let stream = async_stream::stream! {
        let mut upstream = resp.bytes_stream();
        let mut buffer = Vec::new();
        let mut usage = TokenUsage::default();
        let mut response_id = format!("msg_openai_{}", unix_timestamp());
        let mut sent_start = false;
        let mut text_block_open = false;
        let mut text_block_index = 0_u64;
        let mut next_block_index = 0_u64;
        let mut finish_reason: Option<String> = None;
        let mut tool_deltas: BTreeMap<u64, OpenAiToolDelta> = BTreeMap::new();
        let mut openai_done = false;

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while let Some(frame) = take_sse_frame(&mut buffer) {
                        let (_, data) = parse_sse_frame(&frame);
                        let Some(data) = data else {
                            continue;
                        };
                        if data.trim() == "[DONE]" {
                            openai_done = true;
                            break;
                        }

                        let Ok(chunk_json) = serde_json::from_str::<Value>(&data) else {
                            continue;
                        };
                        usage.absorb_max(extract_token_usage(&chunk_json));

                        if let Some(id) = chunk_json.get("id").and_then(Value::as_str) {
                            response_id = id.to_string();
                        }

                        if !sent_start {
                            sent_start = true;
                            yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("message_start", json!({
                                "type": "message_start",
                                "message": {
                                    "id": response_id,
                                    "type": "message",
                                    "role": "assistant",
                                    "model": model,
                                    "content": [],
                                    "stop_reason": null,
                                    "stop_sequence": null,
                                    "usage": {"input_tokens": 0, "output_tokens": 0}
                                }
                            }))));
                        }

                        let Some(choice) = chunk_json
                            .get("choices")
                            .and_then(Value::as_array)
                            .and_then(|choices| choices.first())
                        else {
                            continue;
                        };

                        let delta = choice.get("delta").unwrap_or(&Value::Null);
                        if let Some(content) = delta.get("content").and_then(Value::as_str) {
                            if !content.is_empty() {
                                if !text_block_open {
                                    text_block_open = true;
                                    text_block_index = next_block_index;
                                    next_block_index += 1;
                                    yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("content_block_start", json!({
                                        "type": "content_block_start",
                                        "index": text_block_index,
                                        "content_block": {"type": "text", "text": ""}
                                    }))));
                                }
                                yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("content_block_delta", json!({
                                    "type": "content_block_delta",
                                    "index": text_block_index,
                                    "delta": {"type": "text_delta", "text": content}
                                }))));
                            }
                        }

                        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                            for tool_call in tool_calls {
                                let index = tool_call
                                    .get("index")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(tool_deltas.len() as u64);
                                let entry = tool_deltas.entry(index).or_default();
                                if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                                    entry.id = Some(id.to_string());
                                }
                                if let Some(function) = tool_call.get("function") {
                                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                                        entry.name = Some(name.to_string());
                                    }
                                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                                        entry.arguments.push_str(arguments);
                                    }
                                }
                            }
                        }

                        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                            finish_reason = Some(reason.to_string());
                        }
                    }

                    if openai_done {
                        break;
                    }
                }
                Err(err) => {
                    yield Err(io::Error::other(err.to_string()));
                    return;
                }
            }
        }

        if !sent_start {
            yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("message_start", json!({
                "type": "message_start",
                "message": {
                    "id": response_id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            }))));
        }

        if text_block_open {
            yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("content_block_stop", json!({
                "type": "content_block_stop",
                "index": text_block_index
            }))));
        }

        for (tool_index, tool_delta) in tool_deltas {
            let block_index = next_block_index;
            next_block_index += 1;
            let id = tool_delta
                .id
                .unwrap_or_else(|| format!("toolu_{}", tool_index));
            let name = tool_delta.name.unwrap_or_else(|| "tool".to_string());
            yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("content_block_start", json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": {}
                }
            }))));

            if !tool_delta.arguments.is_empty() {
                yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("content_block_delta", json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": tool_delta.arguments
                    }
                }))));
            }

            yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("content_block_stop", json!({
                "type": "content_block_stop",
                "index": block_index
            }))));
        }

        yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("message_delta", json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": openai_finish_reason_to_anthropic(finish_reason.as_deref()),
                "stop_sequence": null
            },
            "usage": {"output_tokens": usage.output_tokens}
        }))));
        yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("message_stop", json!({
            "type": "message_stop"
        }))));

        record_token_usage(&token_stats, usage).await;
        log_request_with_body(&request_logs, &log_context, usage, None).await;
    };

    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        )
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .body(Body::from_stream(stream))
        .unwrap_or_else(|err| bad_gateway(format!("Failed to build stream response: {}", err)))
}

fn anthropic_frame_to_openai_chunks(
    frame: &str,
    response_id: &mut String,
    model: &str,
    tool_indices: &mut BTreeMap<u64, u64>,
    next_tool_index: &mut u64,
) -> Vec<String> {
    let (event, data) = parse_sse_frame(frame);
    let Some(data) = data else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&data) else {
        return Vec::new();
    };

    match event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
    {
        Some("message_start") => {
            if let Some(id) = value["message"]["id"].as_str() {
                *response_id = id.to_string();
            }
            vec![openai_stream_chunk(
                response_id,
                model,
                json!({"role": "assistant"}),
                Value::Null,
            )]
        }
        Some("content_block_start") => {
            let content_block = &value["content_block"];
            if content_block.get("type").and_then(Value::as_str) != Some("tool_use") {
                return Vec::new();
            }
            let block_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
            let tool_index = *next_tool_index;
            *next_tool_index += 1;
            tool_indices.insert(block_index, tool_index);
            let id = content_block
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("toolu_unknown");
            let name = content_block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            vec![openai_stream_chunk(
                response_id,
                model,
                json!({
                    "tool_calls": [{
                        "index": tool_index,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""}
                    }]
                }),
                Value::Null,
            )]
        }
        Some("content_block_delta") => {
            let delta = &value["delta"];
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    let text = delta
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    vec![openai_stream_chunk(
                        response_id,
                        model,
                        json!({"content": text}),
                        Value::Null,
                    )]
                }
                Some("input_json_delta") => {
                    let block_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let Some(tool_index) = tool_indices.get(&block_index).copied() else {
                        return Vec::new();
                    };
                    let arguments = delta
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    vec![openai_stream_chunk(
                        response_id,
                        model,
                        json!({
                            "tool_calls": [{
                                "index": tool_index,
                                "function": {"arguments": arguments}
                            }]
                        }),
                        Value::Null,
                    )]
                }
                _ => Vec::new(),
            }
        }
        Some("message_delta") => {
            let finish_reason = anthropic_stop_reason_to_openai(
                value["delta"].get("stop_reason").and_then(Value::as_str),
            )
            .map(|reason| Value::String(reason.to_string()))
            .unwrap_or(Value::Null);
            vec![openai_stream_chunk(
                response_id,
                model,
                json!({}),
                finish_reason,
            )]
        }
        Some("message_stop") => vec!["data: [DONE]\n\n".to_string()],
        _ => Vec::new(),
    }
}

async fn anthropic_stream_to_openai_response(
    resp: reqwest::Response,
    model: String,
    token_stats: TokenStatsState,
    request_logs: RequestLogState,
    log_context: RequestLogContext,
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let stream = async_stream::stream! {
        let mut upstream = resp.bytes_stream();
        let mut buffer = Vec::new();
        let mut usage = TokenUsage::default();
        let mut response_id = format!("chatcmpl-anthropic-{}", unix_timestamp());
        let mut sent_done = false;
        let mut tool_indices = BTreeMap::new();
        let mut next_tool_index = 0_u64;

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while let Some(frame) = take_sse_frame(&mut buffer) {
                        usage.absorb_max(extract_token_usage_from_sse_frame(&frame));
                        for output in anthropic_frame_to_openai_chunks(
                            &frame,
                            &mut response_id,
                            &model,
                            &mut tool_indices,
                            &mut next_tool_index,
                        ) {
                            if output.trim() == "data: [DONE]" {
                                sent_done = true;
                            }
                            yield Ok::<Bytes, io::Error>(Bytes::from(output));
                        }
                    }
                }
                Err(err) => {
                    yield Err(io::Error::other(err.to_string()));
                    return;
                }
            }
        }

        if !buffer.iter().all(u8::is_ascii_whitespace) {
            let frame = String::from_utf8_lossy(&buffer);
            usage.absorb_max(extract_token_usage_from_sse_frame(&frame));
            for output in anthropic_frame_to_openai_chunks(
                &frame,
                &mut response_id,
                &model,
                &mut tool_indices,
                &mut next_tool_index,
            ) {
                if output.trim() == "data: [DONE]" {
                    sent_done = true;
                }
                yield Ok::<Bytes, io::Error>(Bytes::from(output));
            }
        }

        if !sent_done {
            yield Ok::<Bytes, io::Error>(Bytes::from_static(b"data: [DONE]\n\n"));
        }

        record_token_usage(&token_stats, usage).await;
        log_request_with_body(&request_logs, &log_context, usage, None).await;
    };

    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        )
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .body(Body::from_stream(stream))
        .unwrap_or_else(|err| bad_gateway(format!("Failed to build stream response: {}", err)))
}

/// Everything a proxy handler needs after the shared preamble: auth identity,
/// parsed body, resolved model, stream flag, and the loggable request snapshot.
struct Prepared {
    api_key_name: String,
    body_json: Value,
    model: String,
    is_stream: bool,
    request_body_for_log: Option<String>,
    request_path: String,
    started: std::time::Instant,
}

/// Shared preamble for both proxy handlers: reject non-POST, authorize, parse
/// the JSON body, resolve the model alias, and capture stream/log metadata.
/// On any failure it returns the early `Response` already rendered in the
/// caller's `dialect` (the OPTIONS preflight is surfaced the same way).
async fn prepare_request(
    state: &ProxyState,
    dialect: ApiDialect,
    method: &Method,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    body: &axum::body::Bytes,
) -> Result<Prepared, Response> {
    if *method == Method::OPTIONS {
        return Err(StatusCode::NO_CONTENT.into_response());
    }
    if *method != Method::POST {
        return Err(dialect.error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            ErrorKind::BadRequest,
            "Only POST is supported for this endpoint",
            None,
        ));
    }

    let request_path = uri.path().to_string();
    let started = std::time::Instant::now();

    let config = state.config.read().await;
    let api_key_name = match authorize_proxy_request(headers, &config) {
        Ok(name) => name,
        Err(response) => return Err(*response),
    };

    let mut body_json: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(e) => {
            // OpenAI uses a flat `{"error": "<msg>"}` string here; Anthropic
            // uses the standard nested envelope.
            let response = match dialect {
                ApiDialect::OpenAI => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("Invalid JSON: {}", e) })),
                )
                    .into_response(),
                ApiDialect::Anthropic => dialect.error_response(
                    StatusCode::BAD_REQUEST,
                    ErrorKind::BadRequest,
                    &format!("Invalid JSON: {}", e),
                    None,
                ),
            };
            return Err(response);
        }
    };

    let raw_model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let model = resolve_model_alias(&config, &raw_model).to_string();
    drop(config);

    if model != raw_model {
        body_json["model"] = Value::String(model.clone());
    }

    let is_stream = body_json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    let request_body_for_log = replay_request_body(&request_path, &body_json);

    Ok(Prepared {
        api_key_name,
        body_json,
        model,
        is_stream,
        request_body_for_log,
        request_path,
        started,
    })
}

/// If `prepared.model` is a fusion model, run the fusion pipeline for this
/// dialect and return its (already-logged) response. Returns `None` when the
/// model is not a fusion model, leaving normal provider routing to the caller.
async fn try_fusion(
    state: &ProxyState,
    dialect: ApiDialect,
    prepared: &Prepared,
) -> Option<Response> {
    if !fusion::is_fusion_model(&prepared.model) {
        return None;
    }

    let expected_path = match dialect {
        ApiDialect::OpenAI => "/v1/chat/completions",
        ApiDialect::Anthropic => "/v1/messages",
    };

    let fusion_log = |status: u16| RequestLogContext {
        method: "POST",
        path: prepared.request_path.clone(),
        model: prepared.model.clone(),
        provider: "Fusion".to_string(),
        provider_id: "nexus-fusion".to_string(),
        api_key_name: prepared.api_key_name.clone(),
        status,
        started: prepared.started,
        request_body: prepared.request_body_for_log.clone(),
    };

    if matches!(dialect, ApiDialect::Anthropic)
        && prepared.request_path == "/v1/messages/count_tokens"
    {
        log_request_with_body(
            &state.request_logs,
            &fusion_log(200),
            TokenUsage::default(),
            None,
        )
        .await;
        return Some(
            Json(json!({
                "input_tokens": estimate_anthropic_tokens(&prepared.body_json)
            }))
            .into_response(),
        );
    }

    if prepared.request_path != expected_path {
        let message = format!("Fusion is only supported on {}", expected_path);
        log_request_with_body(
            &state.request_logs,
            &fusion_log(400),
            TokenUsage::default(),
            Some(message.clone()),
        )
        .await;
        return Some(dialect.error_response(
            StatusCode::BAD_REQUEST,
            ErrorKind::BadRequest,
            &message,
            None,
        ));
    }

    let config = state.config.read().await.clone();
    if matches!(dialect, ApiDialect::Anthropic) {
        let result = fusion::run_from_anthropic_client_request(
            &state.client,
            &state.request_logs,
            &config,
            &prepared.body_json,
        )
        .await;
        return Some(match result {
            Ok(turn) => {
                let message = match anthropic::completed_message(
                    &prepared.model,
                    turn.text.as_deref(),
                    &turn.tool_calls,
                    turn.usage,
                ) {
                    Ok(message) => message,
                    Err(error) => {
                        log_request_with_body(
                            &state.request_logs,
                            &fusion_log(502),
                            TokenUsage::default(),
                            Some(error.clone()),
                        )
                        .await;
                        return Some(dialect.error_response(
                            StatusCode::BAD_GATEWAY,
                            ErrorKind::Upstream,
                            &error,
                            None,
                        ));
                    }
                };
                record_token_usage(&state.token_stats, turn.usage).await;
                log_request_with_body(&state.request_logs, &fusion_log(200), turn.usage, None)
                    .await;
                if prepared.is_stream {
                    match anthropic::sse_body(&message) {
                        Ok(body) => Response::builder()
                            .status(StatusCode::OK)
                            .header(
                                header::CONTENT_TYPE,
                                HeaderValue::from_static("text/event-stream"),
                            )
                            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
                            .body(Body::from(body))
                            .unwrap_or_else(|error| {
                                dialect.error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    ErrorKind::Upstream,
                                    &format!("Failed to build Fusion stream: {error}"),
                                    None,
                                )
                            }),
                        Err(error) => dialect.error_response(
                            StatusCode::BAD_GATEWAY,
                            ErrorKind::Upstream,
                            &error,
                            None,
                        ),
                    }
                } else {
                    Json(message).into_response()
                }
            }
            Err(error) => {
                let status = if error.is_bad_request() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::BAD_GATEWAY
                };
                let message = error.to_string();
                log_request_with_body(
                    &state.request_logs,
                    &fusion_log(status.as_u16()),
                    TokenUsage::default(),
                    Some(message.clone()),
                )
                .await;
                dialect.error_response(
                    status,
                    if status == StatusCode::BAD_REQUEST {
                        ErrorKind::BadRequest
                    } else {
                        ErrorKind::Upstream
                    },
                    &message,
                    None,
                )
            }
        });
    }

    let run_result = if config.fusion.mode == "on_demand" {
        match dialect {
            ApiDialect::OpenAI => fusion::run_on_demand_from_openai_request(
                &state.client,
                &state.request_logs,
                &config,
                &prepared.body_json,
            )
            .await
            .map(|run| (run.final_content, run.usage)),
            ApiDialect::Anthropic => fusion::run_on_demand_from_anthropic_request(
                &state.client,
                &state.request_logs,
                &config,
                &prepared.body_json,
            )
            .await
            .map(|run| (run.final_content, run.usage)),
        }
    } else {
        match dialect {
            ApiDialect::OpenAI => fusion::run_from_openai_request(
                &state.client,
                &state.request_logs,
                &config,
                &prepared.body_json,
                None,
            )
            .await
            .map(|run| (run.final_content, run.usage)),
            ApiDialect::Anthropic => fusion::run_from_anthropic_request(
                &state.client,
                &state.request_logs,
                &config,
                &prepared.body_json,
                None,
            )
            .await
            .map(|run| (run.final_content, run.usage)),
        }
    };

    match run_result {
        Ok((final_content, usage)) => {
            record_token_usage(&state.token_stats, usage).await;
            log_request_with_body(&state.request_logs, &fusion_log(200), usage, None).await;
            let body = match dialect {
                ApiDialect::OpenAI => fusion::openai_chat_response(&final_content, usage),
                ApiDialect::Anthropic => fusion::anthropic_message_response(&final_content, usage),
            };
            Some(Json(body).into_response())
        }
        Err(error) => {
            let status = if error.is_bad_request() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            };
            let message = error.to_string();
            log_request_with_body(
                &state.request_logs,
                &fusion_log(status.as_u16()),
                TokenUsage::default(),
                Some(message.clone()),
            )
            .await;
            let kind = if status == StatusCode::BAD_REQUEST {
                ErrorKind::BadRequest
            } else {
                ErrorKind::Upstream
            };
            Some(dialect.error_response(status, kind, &message, None))
        }
    }
}

/// Pick and order the providers eligible for this request, or return the
/// dialect's "no provider" 404 (already logged) when none match.
async fn select_providers(
    state: &ProxyState,
    dialect: ApiDialect,
    prepared: &Prepared,
) -> Result<Vec<Provider>, Response> {
    let config = state.config.read().await;
    let mut providers: Vec<Provider> = config
        .providers
        .iter()
        .filter(|provider| {
            provider_matches_model(provider, &prepared.model)
                && match dialect {
                    // An OpenAI-entry request only routes to an Anthropic
                    // provider on the chat-completions path.
                    ApiDialect::OpenAI => {
                        !is_anthropic_provider(provider)
                            || prepared.request_path == "/v1/chat/completions"
                    }
                    ApiDialect::Anthropic => true,
                }
        })
        .cloned()
        .collect();
    sort_providers_for_model(&config, &prepared.model, &mut providers);
    drop(config);

    if providers.is_empty() {
        let message = format!("No provider found for model: {}", prepared.model);
        log_request_with_body(
            &state.request_logs,
            &RequestLogContext {
                method: "POST",
                path: prepared.request_path.clone(),
                model: prepared.model.clone(),
                provider: String::new(),
                provider_id: String::new(),
                api_key_name: prepared.api_key_name.clone(),
                status: 404,
                started: prepared.started,
                request_body: prepared.request_body_for_log.clone(),
            },
            TokenUsage::default(),
            Some(message.clone()),
        )
        .await;
        return Err(dialect.error_response(
            StatusCode::NOT_FOUND,
            ErrorKind::NotFound,
            &message,
            None,
        ));
    }

    Ok(providers)
}

fn responses_error_response(status: StatusCode, error_type: &str, message: &str) -> Response {
    (status, Json(responses::error_body(message, error_type))).into_response()
}

async fn responses_handler(
    State(state): State<ProxyState>,
    method: Method,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    if method == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    if method != Method::POST {
        return responses_error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request_error",
            "Only POST is supported for this endpoint",
        );
    }

    let started = std::time::Instant::now();
    let config = state.config.read().await;
    let api_key_name = match authorize_proxy_request(&headers, &config) {
        Ok(name) => name,
        Err(_) => {
            return responses_error_response(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Invalid or missing API key",
            )
        }
    };
    let mut body_json: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return responses_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!("Invalid JSON: {error}"),
            )
        }
    };
    let raw_model = body_json
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model = resolve_model_alias(&config, raw_model).to_string();
    let config = config.clone();
    if model != raw_model {
        body_json["model"] = Value::String(model.clone());
    }
    if !fusion::is_fusion_model(&model) {
        return responses_error_response(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "The Responses endpoint currently supports only nexus/fusion",
        );
    }
    let parsed = match responses::parse_request(&body_json) {
        Ok(request) => request,
        Err(error) => {
            return responses_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &error,
            )
        }
    };
    let response_model = parsed.model.clone();
    let stream = parsed.stream;
    let client_tools = parsed.client_tools.clone();
    let request_body = replay_request_body(uri.path(), &body_json);
    let log_context = RequestLogContext {
        method: "POST",
        path: uri.path().to_string(),
        model: model.clone(),
        provider: "Fusion".to_string(),
        provider_id: "nexus-fusion".to_string(),
        api_key_name,
        status: 200,
        started,
        request_body,
    };

    match fusion::run_from_responses_request(&state.client, &state.request_logs, &config, parsed)
        .await
    {
        Ok(turn) => {
            let response_body = match responses::completed_response(
                &response_model,
                turn.text.as_deref(),
                &turn.tool_calls,
                &client_tools,
                turn.usage,
            ) {
                Ok(body) => body,
                Err(error) => {
                    return responses_error_response(
                        StatusCode::BAD_GATEWAY,
                        "server_error",
                        &error,
                    )
                }
            };
            record_token_usage(&state.token_stats, turn.usage).await;
            log_request_with_body(&state.request_logs, &log_context, turn.usage, None).await;
            if stream {
                match responses::sse_body(&response_body) {
                    Ok(body) => Response::builder()
                        .status(StatusCode::OK)
                        .header(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/event-stream"),
                        )
                        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
                        .body(Body::from(body))
                        .unwrap_or_else(|error| {
                            responses_error_response(
                                StatusCode::BAD_GATEWAY,
                                "server_error",
                                &format!("Failed to build Responses stream: {error}"),
                            )
                        }),
                    Err(error) => {
                        responses_error_response(StatusCode::BAD_GATEWAY, "server_error", &error)
                    }
                }
            } else {
                Json(response_body).into_response()
            }
        }
        Err(error) => {
            let status = if error.is_bad_request() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            };
            let message = error.to_string();
            let mut failed_log = log_context;
            failed_log.status = status.as_u16();
            log_request_with_body(
                &state.request_logs,
                &failed_log,
                TokenUsage::default(),
                Some(message.clone()),
            )
            .await;
            responses_error_response(
                status,
                if status == StatusCode::BAD_REQUEST {
                    "invalid_request_error"
                } else {
                    "server_error"
                },
                &message,
            )
        }
    }
}

async fn anthropic_handler(
    State(state): State<ProxyState>,
    method: Method,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    let dialect = ApiDialect::Anthropic;
    let prepared = match prepare_request(&state, dialect, &method, &headers, &uri, &body).await {
        Ok(prepared) => prepared,
        Err(response) => return response,
    };

    if let Some(response) = try_fusion(&state, dialect, &prepared).await {
        return response;
    }

    let providers = match select_providers(&state, dialect, &prepared).await {
        Ok(providers) => providers,
        Err(response) => return response,
    };

    let Prepared {
        api_key_name,
        body_json,
        model,
        is_stream,
        request_body_for_log,
        request_path,
        started,
    } = prepared;

    let mut errors = Vec::new();

    for provider in &providers {
        if is_anthropic_provider(provider) {
            match anthropic_request(&state, provider, &request_path, &headers, &body_json)
                .send()
                .await
            {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status().as_u16();
                        let text = resp.text().await.unwrap_or_default();
                        errors.push(format!("{}: HTTP {} - {}", provider.name, status, text));
                        continue;
                    }

                    let status = resp.status().as_u16();
                    return passthrough_response(
                        resp,
                        is_stream,
                        state.token_stats.clone(),
                        state.request_logs.clone(),
                        RequestLogContext {
                            method: "POST",
                            path: request_path.clone(),
                            model: model.clone(),
                            provider: provider.name.clone(),
                            provider_id: provider.id.clone(),
                            api_key_name: api_key_name.clone(),
                            status,
                            started,
                            request_body: request_body_for_log.clone(),
                        },
                    )
                    .await;
                }
                Err(err) => {
                    errors.push(format!("{}: {}", provider.name, err));
                    continue;
                }
            }
        }

        if request_path == "/v1/messages/count_tokens" {
            log_request_with_body(
                &state.request_logs,
                &RequestLogContext {
                    method: "POST",
                    path: request_path.clone(),
                    model: model.clone(),
                    provider: provider.name.clone(),
                    provider_id: provider.id.clone(),
                    api_key_name: api_key_name.clone(),
                    status: 200,
                    started,
                    request_body: request_body_for_log.clone(),
                },
                TokenUsage::default(),
                None,
            )
            .await;
            return Json(json!({
                "input_tokens": estimate_anthropic_tokens(&body_json)
            }))
            .into_response();
        }

        if request_path != "/v1/messages" {
            errors.push(format!(
                "{}: Unsupported Anthropic path {}",
                provider.name, request_path
            ));
            continue;
        }

        let openai_body = match anthropic_to_openai_chat_request(&body_json) {
            Ok(value) => value,
            Err(err) => {
                errors.push(format!("{}: {}", provider.name, err));
                continue;
            }
        };

        match openai_request(
            &state,
            provider,
            "/v1/chat/completions",
            &headers,
            &openai_body,
        )
        .send()
        .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status().as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    errors.push(format!("{}: HTTP {} - {}", provider.name, status, text));
                    continue;
                }

                if is_stream {
                    return openai_stream_to_anthropic_response(
                        resp,
                        model.clone(),
                        state.token_stats.clone(),
                        state.request_logs.clone(),
                        RequestLogContext {
                            method: "POST",
                            path: request_path.clone(),
                            model: model.clone(),
                            provider: provider.name.clone(),
                            provider_id: provider.id.clone(),
                            api_key_name: api_key_name.clone(),
                            status: 200,
                            started,
                            request_body: request_body_for_log.clone(),
                        },
                    )
                    .await;
                }

                match resp.json::<Value>().await {
                    Ok(openai_response) => {
                        let usage = extract_token_usage(&openai_response);
                        record_token_usage(&state.token_stats, usage).await;
                        log_request_with_body(
                            &state.request_logs,
                            &RequestLogContext {
                                method: "POST",
                                path: request_path.clone(),
                                model: model.clone(),
                                provider: provider.name.clone(),
                                provider_id: provider.id.clone(),
                                api_key_name: api_key_name.clone(),
                                status: 200,
                                started,
                                request_body: request_body_for_log.clone(),
                            },
                            usage,
                            None,
                        )
                        .await;
                        return Json(openai_to_anthropic_response(openai_response, &model))
                            .into_response();
                    }
                    Err(err) => {
                        errors.push(format!(
                            "{}: Failed to parse OpenAI response: {}",
                            provider.name, err
                        ));
                        continue;
                    }
                }
            }
            Err(err) => {
                errors.push(format!("{}: {}", provider.name, err));
                continue;
            }
        }
    }

    log_request_with_body(
        &state.request_logs,
        &RequestLogContext {
            method: "POST",
            path: request_path.clone(),
            model: model.clone(),
            provider: String::new(),
            provider_id: String::new(),
            api_key_name: api_key_name.clone(),
            status: 502,
            started,
            request_body: request_body_for_log.clone(),
        },
        TokenUsage::default(),
        Some(errors.join("; ")),
    )
    .await;

    dialect.error_response(
        StatusCode::BAD_GATEWAY,
        ErrorKind::Upstream,
        "All providers failed",
        Some(&errors),
    )
}

async fn proxy_handler(
    State(state): State<ProxyState>,
    method: Method,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    let dialect = ApiDialect::OpenAI;
    let prepared = match prepare_request(&state, dialect, &method, &headers, &uri, &body).await {
        Ok(prepared) => prepared,
        Err(response) => return response,
    };

    if let Some(response) = try_fusion(&state, dialect, &prepared).await {
        return response;
    }

    let providers = match select_providers(&state, dialect, &prepared).await {
        Ok(providers) => providers,
        Err(response) => return response,
    };

    let Prepared {
        api_key_name,
        body_json,
        model,
        is_stream,
        request_body_for_log,
        request_path,
        started,
    } = prepared;

    let mut errors = Vec::new();

    for provider in &providers {
        if is_anthropic_provider(provider) {
            let anthropic_body = match openai_to_anthropic_request(&body_json) {
                Ok(value) => value,
                Err(err) => {
                    errors.push(format!("{}: {}", provider.name, err));
                    continue;
                }
            };

            match anthropic_request(&state, provider, "/v1/messages", &headers, &anthropic_body)
                .send()
                .await
            {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status().as_u16();
                        let text = resp.text().await.unwrap_or_default();
                        errors.push(format!("{}: HTTP {} - {}", provider.name, status, text));
                        continue;
                    }

                    if is_stream {
                        return anthropic_stream_to_openai_response(
                            resp,
                            model.clone(),
                            state.token_stats.clone(),
                            state.request_logs.clone(),
                            RequestLogContext {
                                method: "POST",
                                path: request_path.clone(),
                                model: model.clone(),
                                provider: provider.name.clone(),
                                provider_id: provider.id.clone(),
                                api_key_name: api_key_name.clone(),
                                status: 200,
                                started,
                                request_body: request_body_for_log.clone(),
                            },
                        )
                        .await;
                    }

                    match resp.json::<Value>().await {
                        Ok(anthropic_response) => {
                            let usage = extract_token_usage(&anthropic_response);
                            record_token_usage(&state.token_stats, usage).await;
                            log_request_with_body(
                                &state.request_logs,
                                &RequestLogContext {
                                    method: "POST",
                                    path: request_path.clone(),
                                    model: model.clone(),
                                    provider: provider.name.clone(),
                                    provider_id: provider.id.clone(),
                                    api_key_name: api_key_name.clone(),
                                    status: 200,
                                    started,
                                    request_body: request_body_for_log.clone(),
                                },
                                usage,
                                None,
                            )
                            .await;
                            return Json(anthropic_to_openai_response(anthropic_response, &model))
                                .into_response();
                        }
                        Err(err) => {
                            errors.push(format!(
                                "{}: Failed to parse Anthropic response: {}",
                                provider.name, err
                            ));
                            continue;
                        }
                    }
                }
                Err(err) => {
                    errors.push(format!("{}: {}", provider.name, err));
                    continue;
                }
            }
        }

        let url = openai_upstream_url(&provider.base_url, &request_path);

        let mut req = state
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", provider.api_key));

        if let Some(ua) = headers.get(header::USER_AGENT) {
            req = req.header(header::USER_AGENT, ua);
        }

        match req.json(&body_json).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status().as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    errors.push(format!("{}: HTTP {} - {}", provider.name, status, text));
                    continue;
                }

                let status = resp.status().as_u16();
                return passthrough_response(
                    resp,
                    is_stream,
                    state.token_stats.clone(),
                    state.request_logs.clone(),
                    RequestLogContext {
                        method: "POST",
                        path: request_path.clone(),
                        model: model.clone(),
                        provider: provider.name.clone(),
                        provider_id: provider.id.clone(),
                        api_key_name: api_key_name.clone(),
                        status,
                        started,
                        request_body: request_body_for_log.clone(),
                    },
                )
                .await;
            }
            Err(e) => {
                errors.push(format!("{}: {}", provider.name, e));
                continue;
            }
        }
    }

    log_request_with_body(
        &state.request_logs,
        &RequestLogContext {
            method: "POST",
            path: request_path.clone(),
            model: model.clone(),
            provider: String::new(),
            provider_id: String::new(),
            api_key_name: api_key_name.clone(),
            status: 502,
            started,
            request_body: request_body_for_log.clone(),
        },
        TokenUsage::default(),
        Some(errors.join("; ")),
    )
    .await;

    dialect.error_response(
        StatusCode::BAD_GATEWAY,
        ErrorKind::Upstream,
        "All providers failed",
        Some(&errors),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FusionConfig, ModelRef, ModelRoute, Provider};
    use axum::routing::post;
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    async fn spawn_router(router: Router) -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn provider(base_url: String, priority: i32) -> Provider {
        Provider {
            id: format!("provider-{}", priority),
            name: format!("Provider {}", priority),
            protocol: "openai".to_string(),
            base_url,
            api_key: "upstream-key".to_string(),
            models: vec!["test-model".to_string()],
            enabled: true,
            priority,
        }
    }

    fn anthropic_provider(base_url: String, priority: i32) -> Provider {
        Provider {
            protocol: "anthropic".to_string(),
            ..provider(base_url, priority)
        }
    }

    fn fusion_model_ref(model: &str) -> ModelRef {
        ModelRef {
            provider_id: "fusion-provider".to_string(),
            model: model.to_string(),
        }
    }

    fn fusion_config() -> FusionConfig {
        FusionConfig {
            panel_models: vec![fusion_model_ref("panel-a"), fusion_model_ref("panel-b")],
            judge_model: Some(fusion_model_ref("judge")),
            final_model: Some(fusion_model_ref("final")),
            timeout_secs: 10,
            ..Default::default()
        }
    }

    fn on_demand_fusion_config() -> FusionConfig {
        FusionConfig {
            mode: "on_demand".to_string(),
            outer_model: Some(fusion_model_ref("outer")),
            ..fusion_config()
        }
    }

    #[test]
    fn replay_request_body_only_keeps_small_replayable_requests() {
        assert!(replay_request_body(
            "/v1/chat/completions",
            &json!({"model": "test-model", "messages": []})
        )
        .is_some());
        assert!(replay_request_body(
            "/v1/messages",
            &json!({"model": "test-model", "messages": []})
        )
        .is_some());
        assert!(replay_request_body(
            "/v1/embeddings",
            &json!({"model": "test-model", "input": "secret"})
        )
        .is_none());
        assert!(replay_request_body(
            "/v1/chat/completions",
            &json!({"model": "test-model", "messages": [{"role": "user", "content": "x".repeat(MAX_STORED_REQUEST_BODY_BYTES)}]})
        )
        .is_none());
    }

    #[test]
    fn provider_order_is_independent_for_each_model() {
        let first = Provider {
            id: "first".to_string(),
            models: vec!["model-a".to_string(), "model-b".to_string()],
            ..provider("https://first.example".to_string(), 0)
        };
        let second = Provider {
            id: "second".to_string(),
            models: vec!["model-a".to_string(), "model-b".to_string()],
            ..provider("https://second.example".to_string(), 1)
        };
        let config = AppConfig {
            providers: vec![first.clone(), second.clone()],
            model_routes: vec![
                ModelRoute {
                    model: "model-a".to_string(),
                    provider_ids: vec!["second".to_string(), "first".to_string()],
                },
                ModelRoute {
                    model: "model-b".to_string(),
                    provider_ids: vec!["first".to_string(), "second".to_string()],
                },
            ],
            ..Default::default()
        };

        let mut model_a = vec![first.clone(), second.clone()];
        sort_providers_for_model(&config, "model-a", &mut model_a);
        assert_eq!(
            model_a
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );

        let mut model_b = vec![first, second];
        sort_providers_for_model(&config, "model-b", &mut model_b);
        assert_eq!(
            model_b
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn token_usage_is_extracted_from_common_provider_shapes() {
        let openai_usage = extract_token_usage(&json!({
            "usage": {
                "prompt_tokens": 120,
                "completion_tokens": 34,
                "prompt_tokens_details": {"cached_tokens": 56}
            }
        }));
        assert_eq!(
            openai_usage,
            TokenUsage {
                input_tokens: 120,
                output_tokens: 34,
                cached_tokens: 56,
                cache_read_tokens: 56,
                cache_write_tokens: 0,
            }
        );

        let anthropic_usage = extract_token_usage(&json!({
            "usage": {
                "input_tokens": 80,
                "output_tokens": 20,
                "cache_creation_input_tokens": 12,
                "cache_read_input_tokens": 40
            }
        }));
        assert_eq!(
            anthropic_usage,
            TokenUsage {
                input_tokens: 80,
                output_tokens: 20,
                cached_tokens: 52,
                cache_read_tokens: 40,
                cache_write_tokens: 12,
            }
        );
    }

    #[test]
    fn openai_upstream_url_supports_root_and_prefixed_base_urls() {
        assert_eq!(
            openai_upstream_url("https://api.openai.com", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            openai_upstream_url("https://api.moonshot.ai/v1", "/v1/chat/completions"),
            "https://api.moonshot.ai/v1/chat/completions"
        );
        assert_eq!(
            openai_upstream_url(
                "https://generativelanguage.googleapis.com/v1beta/openai/",
                "/v1/chat/completions"
            ),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        assert_eq!(
            openai_upstream_url(
                "https://ark.cn-beijing.volces.com/api/v3",
                "/v1/chat/completions"
            ),
            "https://ark.cn-beijing.volces.com/api/v3/chat/completions"
        );
        assert_eq!(
            openai_upstream_url(
                "https://ark.cn-beijing.volces.com/api/coding/v3",
                "/v1/chat/completions"
            ),
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions"
        );
        assert_eq!(
            openai_upstream_url("https://open.bigmodel.cn/api/paas/v4/", "/v1/models"),
            "https://open.bigmodel.cn/api/paas/v4/models"
        );
        assert_eq!(
            openai_upstream_url(
                "https://api.example.com/v1/chat/completions",
                "/v1/chat/completions"
            ),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn anthropic_upstream_url_preserves_vendor_prefixes() {
        assert_eq!(
            anthropic_upstream_url("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_upstream_url("https://api.deepseek.com/anthropic", "/v1/messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_upstream_url("https://api.example.com/anthropic/v1", "/v1/messages"),
            "https://api.example.com/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_upstream_url(
                "https://api.deepseek.com/anthropic",
                "/v1/messages/count_tokens"
            ),
            "https://api.deepseek.com/anthropic/v1/messages/count_tokens"
        );
        assert_eq!(
            anthropic_upstream_url("https://api.example.com/anthropic/v1beta", "/v1/messages"),
            "https://api.example.com/anthropic/v1beta/messages"
        );
    }

    #[test]
    fn sse_frames_support_crlf_and_split_utf8() {
        let payload = "data: {\"text\":\"中\"}\r\n\r\n".as_bytes();
        let character_start = payload
            .windows("中".len())
            .position(|bytes| bytes == "中".as_bytes())
            .unwrap();
        let mut buffer = payload[..character_start + 1].to_vec();
        assert!(take_sse_frame(&mut buffer).is_none());

        buffer.extend_from_slice(&payload[character_start + 1..]);
        let frame = take_sse_frame(&mut buffer).unwrap();
        assert_eq!(frame, "data: {\"text\":\"中\"}");
        assert!(buffer.is_empty());
    }

    #[test]
    fn anthropic_stream_tool_use_becomes_openai_tool_deltas() {
        let mut response_id = "chatcmpl-test".to_string();
        let mut tool_indices = BTreeMap::new();
        let mut next_tool_index = 0;
        let start = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,",
            "\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"lookup\",\"input\":{}}}"
        );
        let delta = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,",
            "\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"test\\\"}\"}}"
        );

        let start_output = anthropic_frame_to_openai_chunks(
            start,
            &mut response_id,
            "test-model",
            &mut tool_indices,
            &mut next_tool_index,
        );
        let delta_output = anthropic_frame_to_openai_chunks(
            delta,
            &mut response_id,
            "test-model",
            &mut tool_indices,
            &mut next_tool_index,
        );

        assert_eq!(next_tool_index, 1);
        let (_, start_data) = parse_sse_frame(&start_output[0]);
        let start_json: Value = serde_json::from_str(&start_data.unwrap()).unwrap();
        assert_eq!(
            start_json["choices"][0]["delta"]["tool_calls"][0]["id"],
            "toolu_1"
        );
        assert_eq!(
            start_json["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "lookup"
        );

        let (_, delta_data) = parse_sse_frame(&delta_output[0]);
        let delta_json: Value = serde_json::from_str(&delta_data.unwrap()).unwrap();
        assert_eq!(
            delta_json["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"q\":\"test\"}"
        );
    }

    #[tokio::test]
    async fn stream_responses_are_forwarded_without_reencoding() {
        let upstream_body = "data: {\"id\":\"chunk\"}\n\ndata: [DONE]\n\n";
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || async move {
                ([(header::CONTENT_TYPE, "text/event-stream")], upstream_body).into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;

        let config = AppConfig {
            providers: vec![provider(format!("http://{}", upstream_addr), 0)],
            proxy_api_key: "proxy-key".to_string(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .bearer_auth("proxy-key")
            .json(&json!({
                "model": "test-model",
                "stream": true,
                "messages": []
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.text().await.unwrap(), upstream_body);

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn opencode_requests_reach_volcengine_coding_endpoint() {
        let upstream = Router::new().route(
            "/api/coding/v3/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "volcengine-coding-response",
                    "object": "chat.completion",
                    "model": "test-model",
                    "choices": []
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![provider(
                format!("http://{}/api/coding/v3", upstream_addr),
                0,
            )],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "test-model",
                "messages": [{"role": "user", "content": "hello"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["id"], "volcengine-coding-response");

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn model_list_requires_proxy_key_when_configured() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            proxy_api_key: "proxy-key".to_string(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;
        let client = reqwest::Client::new();

        let unauthorized = client
            .get(format!("http://{}/v1/models", proxy_addr))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status().as_u16(), 401);

        let authorized = client
            .get(format!("http://{}/v1/models", proxy_addr))
            .bearer_auth("proxy-key")
            .send()
            .await
            .unwrap();
        assert_eq!(authorized.status().as_u16(), 200);
        let body: Value = authorized.json().await.unwrap();
        assert_eq!(body["data"][0]["id"], "test-model");

        proxy_task.abort();
    }

    #[tokio::test]
    async fn token_usage_is_recorded_for_passthrough_responses() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "chatcmpl_usage",
                    "object": "chat.completion",
                    "model": "test-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 11,
                        "completion_tokens": 7,
                        "total_tokens": 18,
                        "prompt_tokens_details": {"cached_tokens": 5}
                    }
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let stats = Arc::new(RwLock::new(TokenStats::default()));
        let config = AppConfig {
            providers: vec![provider(format!("http://{}", upstream_addr), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router_with_stats(
            Arc::new(RwLock::new(config)),
            stats.clone(),
            Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap()),
        );
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "test-model",
                "messages": []
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let _: Value = response.json().await.unwrap();

        let stats = stats.read().await;
        assert_eq!(stats.request_count, 1);
        assert_eq!(stats.input_tokens, 11);
        assert_eq!(stats.output_tokens, 7);
        assert_eq!(stats.cached_tokens, 5);

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn openai_fusion_model_runs_panel_judge_and_final_steps() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or_default();
                Json(json!({
                    "id": format!("chatcmpl-{model}"),
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": format!("answer from {model}")},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let store = Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap());
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router_with_stats(
            Arc::new(RwLock::new(config)),
            Arc::new(RwLock::new(TokenStats::default())),
            store.clone(),
        );
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "tools": [],
                "tool_choice": "auto",
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["model"], "nexus/fusion");
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "answer from final"
        );

        let runs = store.list_fusion_runs().await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "succeeded");
        assert_eq!(runs[0].panel_count, 2);
        let details = store.get_fusion_run(runs[0].id).await.unwrap().unwrap();
        assert_eq!(details.steps.len(), 4);
        assert_eq!(
            details
                .steps
                .iter()
                .map(|step| step.role.as_str())
                .collect::<Vec<_>>(),
            ["panel", "panel", "judge", "final"]
        );
        let logs = store.list().await;
        assert_eq!(logs[0].model, "nexus/fusion");
        assert!(logs[0]
            .request_body
            .as_deref()
            .unwrap_or_default()
            .contains("\"nexus/fusion\""));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn on_demand_outer_model_can_decline_fusion() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or_default();
                Json(json!({
                    "choices": [{"message": {"role": "assistant", "content": format!("direct from {model}")}}],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                }))
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let store = Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap());
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                    "outer".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: on_demand_fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router_with_stats(
            Arc::new(RwLock::new(config)),
            Arc::new(RwLock::new(TokenStats::default())),
            store.clone(),
        );
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "messages": [{"role": "user", "content": "simple question"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "direct from outer"
        );
        assert!(store.list_fusion_runs().await.unwrap().is_empty());
        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn on_demand_required_tool_runs_fusion_then_outer_synthesis() {
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_handler = captured_requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    captured.lock().unwrap().push(body.clone());
                    let model = body["model"].as_str().unwrap_or_default();
                    let has_tool_result = body["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| message["role"] == "tool")
                    });
                    let message = if model == "outer" && !has_tool_result {
                        json!({"role": "assistant", "content": null, "tool_calls": [{
                            "id": "fusion_1", "type": "function", "function": {
                                "name": "fusion", "arguments": "{\"focus\":\"verify carefully\"}"
                            }
                        }]})
                    } else if model == "outer" {
                        json!({"role": "assistant", "content": "outer synthesized answer"})
                    } else {
                        json!({"role": "assistant", "content": format!("analysis from {model}")})
                    };
                    Json(json!({
                        "choices": [{"message": message}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                    }))
                }
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let store = Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap());
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                    "outer".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: on_demand_fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router_with_stats(
            Arc::new(RwLock::new(config)),
            Arc::new(RwLock::new(TokenStats::default())),
            store.clone(),
        );
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "tool_choice": "required",
                "messages": [{"role": "user", "content": "complex question"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "outer synthesized answer"
        );
        {
            let requests = captured_requests.lock().unwrap();
            let first_outer = requests
                .iter()
                .find(|request| request["model"] == "outer")
                .unwrap();
            assert_eq!(first_outer["tool_choice"], "required");
            assert!(requests
                .iter()
                .filter(|request| request["model"] != "outer")
                .all(|request| request.get("tools").is_none()));
            assert!(!requests.iter().any(|request| request["model"] == "final"));
        }
        let runs = store.list_fusion_runs().await.unwrap();
        assert_eq!(runs.len(), 1);
        let details = store.get_fusion_run(runs[0].id).await.unwrap().unwrap();
        assert_eq!(
            details
                .steps
                .iter()
                .map(|step| step.role.as_str())
                .collect::<Vec<_>>(),
            ["panel", "panel", "judge"]
        );
        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn responses_fusion_round_trips_a_codex_function_call() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or_default();
                let has_tool_output = body["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| message["role"] == "tool")
                });
                let message = if model == "final" && !has_tool_output {
                    json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_shell_1",
                            "type": "function",
                            "function": {
                                "name": "shell_command",
                                "arguments": "{\"command\":\"rg --files\"}"
                            }
                        }]
                    })
                } else if model == "final" {
                    json!({"role": "assistant", "content": "Repository inspected."})
                } else {
                    json!({"role": "assistant", "content": format!("analysis from {model}")})
                };
                Json(json!({
                    "choices": [{"message": message}],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                }))
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let store = Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap());
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router_with_stats(
            Arc::new(RwLock::new(config)),
            Arc::new(RwLock::new(TokenStats::default())),
            store,
        );
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;
        let client = reqwest::Client::new();
        let tool = json!({
            "type": "function",
            "name": "shell_command",
            "description": "Run a shell command",
            "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }
        });

        let first = client
            .post(format!("http://{}/v1/responses", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "instructions": "You are a coding agent.",
                "input": [{"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "Inspect this repository"}
                ]}],
                "tools": [tool.clone()],
                "tool_choice": "auto",
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body: Value = first.json().await.unwrap();
        assert_eq!(first_body["object"], "response");
        assert_eq!(first_body["output"][0]["type"], "function_call");
        assert_eq!(first_body["output"][0]["call_id"], "call_shell_1");

        let second = client
            .post(format!("http://{}/v1/responses", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "instructions": "You are a coding agent.",
                "input": [
                    {"type": "message", "role": "user", "content": [
                        {"type": "input_text", "text": "Inspect this repository"}
                    ]},
                    first_body["output"][0].clone(),
                    {"type": "function_call_output", "call_id": "call_shell_1", "output": "README.md"}
                ],
                "tools": [tool],
                "tool_choice": "auto",
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second_body: Value = second.json().await.unwrap();
        assert_eq!(second_body["output"][0]["type"], "message");
        assert_eq!(
            second_body["output"][0]["content"][0]["text"],
            "Repository inspected."
        );

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn responses_fusion_stream_contains_completed_event() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or_default();
                Json(json!({
                    "choices": [{"message": {
                        "role": "assistant", "content": format!("answer from {model}")
                    }}],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                }))
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/responses", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "input": "ping",
                "tools": [],
                "tool_choice": "auto",
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("event: response.output_item.done\n"));
        assert!(body.contains("event: response.completed\n"));
        assert!(body.ends_with("\n\n"));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn responses_on_demand_executes_fusion_server_tool() {
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_handler = captured_requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    captured.lock().unwrap().push(body.clone());
                    let model = body["model"].as_str().unwrap_or_default();
                    let has_fusion_result = body["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| {
                            message["role"] == "tool" && message["tool_call_id"] == "call_fusion"
                        })
                    });
                    let message = if model == "outer" && !has_fusion_result {
                        json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_fusion",
                                "type": "function",
                                "function": {"name": "fusion", "arguments": "{}"}
                            }]
                        })
                    } else if model == "outer" {
                        json!({"role": "assistant", "content": "outer used fusion"})
                    } else {
                        json!({"role": "assistant", "content": format!("analysis from {model}")})
                    };
                    Json(json!({
                        "choices": [{"message": message}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                    }))
                }
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                    "outer".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: on_demand_fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/responses", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "input": "analyze carefully",
                "tools": [{
                    "type": "function",
                    "name": "shell_command",
                    "description": "Run a command",
                    "parameters": {"type": "object", "properties": {}}
                }],
                "tool_choice": "auto",
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["output"][0]["content"][0]["text"], "outer used fusion");
        let requests = captured_requests.lock().unwrap();
        let first_outer = requests
            .iter()
            .find(|request| request["model"] == "outer")
            .unwrap();
        let tool_names = first_outer["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"fusion"));
        assert!(tool_names.contains(&"shell_command"));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires a locally installed Codex CLI"]
    async fn codex_cli_can_use_fusion_responses_provider() {
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_handler = captured_requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    captured.lock().unwrap().push(body.clone());
                    let model = body["model"].as_str().unwrap_or_default();
                    let has_tool_output = body["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| message["role"] == "tool")
                    });
                    let has_structured_tool_history =
                        body["messages"].as_array().is_some_and(|messages| {
                            messages.iter().any(|message| {
                                message["role"] == "tool"
                                    || message
                                        .get("tool_calls")
                                        .and_then(Value::as_array)
                                        .is_some_and(|calls| !calls.is_empty())
                            })
                        });
                    if model != "final" && has_structured_tool_history {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": {
                                "message": "reasoning_content must be passed back"
                            }})),
                        )
                            .into_response();
                    }
                    let message = if model == "final" && !has_tool_output {
                        json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_codex_e2e",
                                "type": "function",
                                "function": {
                                    "name": "shell_command",
                                    "arguments": "{\"command\":\"Write-Output CODEX_TOOL_OK\"}"
                                }
                            }]
                        })
                    } else {
                        json!({"role": "assistant", "content": "OK"})
                    };
                    Json(json!({
                        "choices": [{"message": message}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 1}
                    }))
                    .into_response()
                }
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            proxy_api_key: "sk-codex-e2e".to_string(),
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;
        let provider_config = format!(
            "model_providers.api_nexus={{name=\"API Nexus\",base_url=\"http://{proxy_addr}/v1\",env_key=\"API_NEXUS_TEST_KEY\",wire_api=\"responses\"}}"
        );

        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("cmd");
            command.args(["/C", "codex"]);
            command
        } else {
            tokio::process::Command::new("codex")
        };
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            command
                .args([
                    "exec",
                    "--ignore-user-config",
                    "--ephemeral",
                    "--skip-git-repo-check",
                    "--json",
                    "-c",
                    "model=\"nexus/fusion\"",
                    "-c",
                    "model_provider=\"api_nexus\"",
                    "-c",
                    &provider_config,
                    "Complete the requested tool step, then reply with exactly OK.",
                ])
                .env("API_NEXUS_TEST_KEY", "sk-codex-e2e")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("Codex CLI timed out")
        .expect("failed to launch Codex CLI");

        assert!(
            output.status.success(),
            "Codex failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("\"type\":\"agent_message\""));
        assert!(stdout.contains("\"text\":\"OK\""));
        assert!(stdout.contains("CODEX_TOOL_OK"));
        let requests = captured_requests.lock().unwrap();
        assert!(requests.iter().any(|request| {
            request["model"] == "final"
                && request["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message["role"] == "tool"
                            && message["content"]
                                .as_str()
                                .is_some_and(|content| content.contains("CODEX_TOOL_OK"))
                    })
                })
        }));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires a locally installed Claude Code CLI"]
    async fn claude_code_cli_can_use_fusion_messages_provider() {
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_handler = captured_requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    captured.lock().unwrap().push(body.clone());
                    let model = body["model"].as_str().unwrap_or_default();
                    let has_tool_output = body["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| message["role"] == "tool")
                    });
                    let message = if model == "final" && !has_tool_output {
                        json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "toolu_claude_e2e",
                                "type": "function",
                                "function": {
                                    "name": "Bash",
                                    "arguments": "{\"command\":\"printf CLAUDE_TOOL_OK\"}"
                                }
                            }]
                        })
                    } else if model == "final" {
                        json!({"role": "assistant", "content": "OK"})
                    } else {
                        json!({"role": "assistant", "content": format!("analysis from {model}")})
                    };
                    Json(json!({
                        "choices": [{"message": message}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 1}
                    }))
                }
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            proxy_api_key: "sk-claude-e2e".to_string(),
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("cmd");
            command.args(["/C", "claude"]);
            command
        } else {
            tokio::process::Command::new("claude")
        };
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            command
                .args([
                    "-p",
                    "Complete the requested tool step, then reply with exactly OK.",
                    "--model",
                    "nexus/fusion",
                    "--output-format",
                    "json",
                    "--dangerously-skip-permissions",
                    "--no-session-persistence",
                ])
                .env("ANTHROPIC_BASE_URL", format!("http://{proxy_addr}"))
                .env("ANTHROPIC_AUTH_TOKEN", "sk-claude-e2e")
                .env("ANTHROPIC_MODEL", "nexus/fusion")
                .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
                .env_remove("ANTHROPIC_API_KEY")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("Claude Code CLI timed out")
        .expect("failed to launch Claude Code CLI");

        assert!(
            output.status.success(),
            "Claude Code failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("\"result\":\"OK\""), "stdout={stdout}");
        let requests = captured_requests.lock().unwrap();
        assert!(requests.iter().any(|request| {
            request["model"] == "final"
                && request["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message["role"] == "tool"
                            && message["content"]
                                .as_str()
                                .is_some_and(|content| content.contains("CLAUDE_TOOL_OK"))
                    })
                })
        }));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn openai_fusion_final_inherits_requested_max_tokens() {
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_handler = captured_requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    let model = body["model"].as_str().unwrap_or_default().to_string();
                    captured.lock().unwrap().push(body);
                    Json(json!({
                        "id": format!("chatcmpl-{model}"),
                        "object": "chat.completion",
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": format!("answer from {model}")},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
                    }))
                    .into_response()
                }
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "max_completion_tokens": 333,
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let _: Value = response.json().await.unwrap();

        let requests = captured_requests.lock().unwrap();
        let max_tokens_for = |model: &str| {
            requests
                .iter()
                .find(|body| body["model"] == model)
                .and_then(|body| body["max_tokens"].as_u64())
                .unwrap()
        };
        assert_eq!(max_tokens_for("panel-a"), 2048);
        assert_eq!(max_tokens_for("judge"), 2048);
        assert_eq!(max_tokens_for("final"), 333);

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn fusion_continues_when_one_panel_model_fails() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                if model == "panel-b" {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({"error": {"message": "panel failed"}})),
                    )
                        .into_response();
                }
                Json(json!({
                    "id": format!("chatcmpl-{model}"),
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": format!("answer from {model}")},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let store = Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap());
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router_with_stats(
            Arc::new(RwLock::new(config)),
            Arc::new(RwLock::new(TokenStats::default())),
            store.clone(),
        );
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "answer from final"
        );

        let runs = store.list_fusion_runs().await.unwrap();
        assert_eq!(runs[0].status, "succeeded");
        assert_eq!(runs[0].panel_count, 1);
        let details = store.get_fusion_run(runs[0].id).await.unwrap().unwrap();
        assert_eq!(details.steps.len(), 4);
        assert_eq!(
            details
                .steps
                .iter()
                .filter(|step| step.role == "panel" && step.status == "failed")
                .count(),
            1
        );
        assert!(details
            .steps
            .iter()
            .any(|step| step.role == "final" && step.status == "succeeded"));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn fusion_fails_when_all_panel_models_fail() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                if model.starts_with("panel-") {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({"error": {"message": format!("{model} failed")}})),
                    )
                        .into_response();
                }
                Json(json!({
                    "id": format!("chatcmpl-{model}"),
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": format!("answer from {model}")},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let store = Arc::new(RequestLogStore::open_in_memory(1000, 30).unwrap());
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router_with_stats(
            Arc::new(RwLock::new(config)),
            Arc::new(RwLock::new(TokenStats::default())),
            store.clone(),
        );
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 502);
        let body: Value = response.json().await.unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("All Fusion panel models failed"));

        let runs = store.list_fusion_runs().await.unwrap();
        assert_eq!(runs[0].status, "failed");
        assert_eq!(runs[0].panel_count, 0);
        let details = store.get_fusion_run(runs[0].id).await.unwrap().unwrap();
        assert_eq!(details.steps.len(), 2);
        assert!(details
            .steps
            .iter()
            .all(|step| step.role == "panel" && step.status == "failed"));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_fusion_model_returns_anthropic_message() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or_default();
                Json(json!({
                    "id": format!("chatcmpl-{model}"),
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": format!("answer from {model}")},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "nexus/fusion",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["type"], "message");
        assert_eq!(body["model"], "nexus/fusion");
        assert_eq!(body["content"][0]["text"], "answer from final");

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_fusion_stream_round_trips_a_claude_code_tool() {
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_handler = captured_requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    captured.lock().unwrap().push(body.clone());
                    let model = body["model"].as_str().unwrap_or_default();
                    let has_tool_output = body["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| message["role"] == "tool")
                    });
                    let has_structured_tool_history =
                        body["messages"].as_array().is_some_and(|messages| {
                            messages.iter().any(|message| {
                                message["role"] == "tool"
                                    || message
                                        .get("tool_calls")
                                        .and_then(Value::as_array)
                                        .is_some_and(|calls| !calls.is_empty())
                            })
                        });
                    if model != "final" && has_structured_tool_history {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": {
                                "message": "reasoning_content must be passed back"
                            }})),
                        )
                            .into_response();
                    }
                    let message = if model == "final" && !has_tool_output {
                        json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "toolu_bash_1",
                                "type": "function",
                                "function": {
                                    "name": "Bash",
                                    "arguments": "{\"command\":\"Write-Output CLAUDE_TOOL_OK\"}"
                                }
                            }]
                        })
                    } else if model == "final" {
                        json!({"role": "assistant", "content": "Repository inspected."})
                    } else {
                        json!({"role": "assistant", "content": format!("analysis from {model}")})
                    };
                    Json(json!({
                        "choices": [{"message": message}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                    }))
                    .into_response()
                }
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;
        let client = reqwest::Client::new();
        let tool = json!({
            "name": "Bash",
            "description": "Run a shell command",
            "input_schema": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }
        });

        let first = client
            .post(format!("http://{}/v1/messages?beta=true", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "nexus/fusion",
                "system": [{"type": "text", "text": "You are Claude Code."}],
                "messages": [{"role": "user", "content": "Inspect the repository"}],
                "tools": [tool.clone()],
                "max_tokens": 32000,
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let first_sse = first.text().await.unwrap();
        assert!(first_sse.contains("event: message_start"));
        assert!(first_sse.contains("\"stop_reason\":\"tool_use\""));
        assert!(first_sse.contains("\"name\":\"Bash\""));
        assert!(first_sse.contains("CLAUDE_TOOL_OK"));
        let analysis_calls_after_first = captured_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["model"] != "final")
            .count();

        let second = client
            .post(format!("http://{}/v1/messages?beta=true", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "nexus/fusion",
                "system": [{"type": "text", "text": "You are Claude Code."}],
                "messages": [
                    {"role": "user", "content": "Inspect the repository"},
                    {"role": "assistant", "content": [{
                        "type": "tool_use", "id": "toolu_bash_1", "name": "Bash",
                        "input": {"command": "Write-Output CLAUDE_TOOL_OK"}
                    }]},
                    {"role": "user", "content": [{
                        "type": "tool_result", "tool_use_id": "toolu_bash_1",
                        "content": "CLAUDE_TOOL_OK"
                    }]}
                ],
                "tools": [tool],
                "max_tokens": 32000,
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second_sse = second.text().await.unwrap();
        assert!(second_sse.contains("Repository inspected."));
        assert!(second_sse.contains("\"stop_reason\":\"end_turn\""));

        let requests = captured_requests.lock().unwrap();
        let first_final = requests
            .iter()
            .find(|request| {
                request["model"] == "final"
                    && request["messages"].as_array().is_some_and(|messages| {
                        !messages.iter().any(|message| message["role"] == "tool")
                    })
            })
            .unwrap();
        assert_eq!(first_final["tools"][0]["function"]["name"], "Bash");
        assert!(requests.iter().any(|request| {
            request["model"] == "final"
                && request["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message["role"] == "tool" && message["content"] == "CLAUDE_TOOL_OK"
                    })
                })
        }));
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["model"] != "final")
                .count(),
            analysis_calls_after_first,
            "tool-result turns must not rerun panel/judge models"
        );

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_fusion_count_tokens_does_not_require_an_upstream_route() {
        let config = AppConfig {
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages/count_tokens", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "nexus/fusion",
                "messages": [{"role": "user", "content": "count this"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert!(body["input_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0));

        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_on_demand_fusion_can_return_a_claude_client_tool() {
        let captured_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_handler = captured_requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    let has_tool_output = body["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| message["role"] == "tool")
                    });
                    captured.lock().unwrap().push(body);
                    let message = if has_tool_output {
                        json!({"role": "assistant", "content": "Directory inspected."})
                    } else {
                        json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "toolu_outer_bash",
                                "type": "function",
                                "function": {
                                    "name": "Bash",
                                    "arguments": "{\"command\":\"pwd\"}"
                                }
                            }]
                        })
                    };
                    Json(json!({
                        "choices": [{"message": message}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 1}
                    }))
                }
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                    "outer".to_string(),
                ],
                ..provider(format!("http://{}", upstream_addr), 0)
            }],
            fusion: on_demand_fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "nexus/fusion",
                "messages": [{"role": "user", "content": "Inspect the directory"}],
                "tools": [{
                    "name": "Bash",
                    "description": "Run a shell command",
                    "input_schema": {"type": "object", "properties": {}}
                }],
                "max_tokens": 1024,
                "stream": true
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let sse = response.text().await.unwrap();
        assert!(sse.contains("toolu_outer_bash"));
        assert!(sse.contains("\"name\":\"Bash\""));

        let followup = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "nexus/fusion",
                "messages": [
                    {"role": "user", "content": "Inspect the directory"},
                    {"role": "assistant", "content": [{
                        "type": "tool_use", "id": "toolu_outer_bash", "name": "Bash",
                        "input": {"command": "pwd"}
                    }]},
                    {"role": "user", "content": [{
                        "type": "tool_result", "tool_use_id": "toolu_outer_bash",
                        "content": "C:/project"
                    }]}
                ],
                "tools": [{
                    "name": "Bash",
                    "description": "Run a shell command",
                    "input_schema": {"type": "object", "properties": {}}
                }],
                "max_tokens": 1024,
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(followup.status(), StatusCode::OK);
        assert!(followup
            .text()
            .await
            .unwrap()
            .contains("Directory inspected."));

        let requests = captured_requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let tool_names = |request: &Value| -> Vec<String> {
            request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert!(tool_names(&requests[0]).iter().any(|name| name == "fusion"));
        assert!(tool_names(&requests[0]).iter().any(|name| name == "Bash"));
        assert!(!tool_names(&requests[1]).iter().any(|name| name == "fusion"));
        assert!(tool_names(&requests[1]).iter().any(|name| name == "Bash"));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn fusion_streaming_requests_return_clear_error() {
        let config = AppConfig {
            providers: vec![Provider {
                id: "fusion-provider".to_string(),
                models: vec![
                    "panel-a".to_string(),
                    "panel-b".to_string(),
                    "judge".to_string(),
                    "final".to_string(),
                ],
                ..provider("http://127.0.0.1:9".to_string(), 0)
            }],
            fusion: fusion_config(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "nexus/fusion",
                "stream": true,
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 400);
        let body: Value = response.json().await.unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("streaming"));

        proxy_task.abort();
    }

    #[tokio::test]
    async fn falls_back_to_next_provider_on_upstream_error() {
        let failing_upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "first provider failed"})),
                )
                    .into_response()
            }),
        );
        let working_upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "ok-from-second-provider",
                    "choices": []
                }))
                .into_response()
            }),
        );

        let (failing_addr, failing_task) = spawn_router(failing_upstream).await;
        let (working_addr, working_task) = spawn_router(working_upstream).await;
        let config = AppConfig {
            providers: vec![
                provider(format!("http://{}", failing_addr), 0),
                provider(format!("http://{}", working_addr), 1),
            ],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "test-model",
                "messages": []
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["id"], "ok-from-second-provider");

        failing_task.abort();
        working_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_native_messages_are_forwarded_with_anthropic_headers() {
        let upstream = Router::new().route(
            "/v1/messages",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(
                    headers
                        .get("x-api-key")
                        .and_then(|value| value.to_str().ok()),
                    Some("upstream-key")
                );
                assert_eq!(
                    headers
                        .get("anthropic-version")
                        .and_then(|value| value.to_str().ok()),
                    Some(ANTHROPIC_VERSION)
                );
                Json(json!({
                    "id": "msg_native",
                    "type": "message",
                    "role": "assistant",
                    "model": body["model"],
                    "content": [{"type": "text", "text": "native ok"}],
                    "stop_reason": "end_turn",
                    "stop_sequence": null,
                    "usage": {"input_tokens": 1, "output_tokens": 2}
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![anthropic_provider(format!("http://{}", upstream_addr), 0)],
            proxy_api_key: "proxy-key".to_string(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", proxy_addr))
            .header("x-api-key", "proxy-key")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "test-model",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hello"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["content"][0]["text"], "native ok");

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_messages_can_route_to_openai_provider() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(
                    headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok()),
                    Some("Bearer upstream-key")
                );
                assert_eq!(body["model"], "test-model");
                assert_eq!(body["messages"][0]["role"], "system");
                assert_eq!(body["messages"][0]["content"], "Be terse.");
                assert_eq!(body["messages"][1]["role"], "user");
                assert_eq!(body["messages"][1]["content"], "ping");
                Json(json!({
                    "id": "chatcmpl_converted",
                    "object": "chat.completion",
                    "model": body["model"],
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "pong"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![provider(format!("http://{}", upstream_addr), 0)],
            proxy_api_key: "proxy-key".to_string(),
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", proxy_addr))
            .header("x-api-key", "proxy-key")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "test-model",
                "max_tokens": 16,
                "system": "Be terse.",
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["type"], "message");
        assert_eq!(body["content"][0]["text"], "pong");
        assert_eq!(body["usage"]["input_tokens"], 3);
        assert_eq!(body["usage"]["output_tokens"], 1);

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_streaming_messages_can_route_to_openai_provider() {
        let upstream_body = concat!(
            "data: {\"id\":\"chatcmpl_stream\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl_stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl_stream\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || async move {
                ([(header::CONTENT_TYPE, "text/event-stream")], upstream_body).into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![provider(format!("http://{}", upstream_addr), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "test-model",
                "max_tokens": 16,
                "stream": true,
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("event: message_start"));
        assert!(body.contains("\"type\":\"text_delta\""));
        assert!(body.contains("\"text\":\"pong\""));
        assert!(body.contains("event: message_stop"));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn anthropic_count_tokens_is_estimated_for_openai_provider() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages/count_tokens", proxy_addr))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": "test-model",
                "messages": [{"role": "user", "content": "hello world"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert!(body["input_tokens"].as_u64().unwrap() > 0);

        proxy_task.abort();
    }

    #[tokio::test]
    async fn openai_chat_can_route_to_anthropic_provider() {
        let upstream = Router::new().route(
            "/v1/messages",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(
                    headers
                        .get("x-api-key")
                        .and_then(|value| value.to_str().ok()),
                    Some("upstream-key")
                );
                assert_eq!(body["system"], "Be terse.");
                assert_eq!(body["messages"][0]["role"], "user");
                assert_eq!(body["messages"][0]["content"], "ping");
                Json(json!({
                    "id": "msg_converted",
                    "type": "message",
                    "role": "assistant",
                    "model": body["model"],
                    "content": [{"type": "text", "text": "pong"}],
                    "stop_reason": "end_turn",
                    "stop_sequence": null,
                    "usage": {"input_tokens": 3, "output_tokens": 1}
                }))
                .into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![anthropic_provider(format!("http://{}", upstream_addr), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "test-model",
                "messages": [
                    {"role": "system", "content": "Be terse."},
                    {"role": "user", "content": "ping"}
                ]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["content"], "pong");
        assert_eq!(body["usage"]["total_tokens"], 4);

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn openai_streaming_chat_can_route_to_anthropic_provider() {
        let upstream_body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stream\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test-model\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || async move {
                ([(header::CONTENT_TYPE, "text/event-stream")], upstream_body).into_response()
            }),
        );
        let (upstream_addr, upstream_task) = spawn_router(upstream).await;
        let config = AppConfig {
            providers: vec![anthropic_provider(format!("http://{}", upstream_addr), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", proxy_addr))
            .json(&json!({
                "model": "test-model",
                "stream": true,
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("\"object\":\"chat.completion.chunk\""));
        assert!(body.contains("\"content\":\"pong\""));
        assert!(body.contains("data: [DONE]"));

        upstream_task.abort();
        proxy_task.abort();
    }

    #[tokio::test]
    async fn cors_preflight_is_accepted() {
        let config = AppConfig::default();
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .request(
                Method::OPTIONS,
                format!("http://{}/v1/chat/completions", proxy_addr),
            )
            .header(header::ORIGIN, "http://localhost:3000")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .send()
            .await
            .unwrap();

        assert!(response.status().is_success());
        assert!(response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));

        proxy_task.abort();
    }

    #[tokio::test]
    async fn cors_preflight_rejects_non_loopback_origins() {
        let proxy = create_proxy_router(Arc::new(RwLock::new(AppConfig::default())));
        let (proxy_addr, proxy_task) = spawn_router(proxy).await;
        let response = reqwest::Client::new()
            .request(
                Method::OPTIONS,
                format!("http://{}/v1/chat/completions", proxy_addr),
            )
            .header(header::ORIGIN, "https://example.com")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .send()
            .await
            .unwrap();

        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        proxy_task.abort();
    }

    #[test]
    fn openai_to_anthropic_omits_null_optional_fields() {
        let request = openai_to_anthropic_request(&json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 16,
            "temperature": null,
            "stop": null,
            "tool_choice": "none"
        }))
        .unwrap();

        assert!(request.get("temperature").is_none() || !request["temperature"].is_null());
        assert!(
            request.get("stop_sequences").is_none(),
            "stop_sequences should be omitted when stop is null, got: {:?}",
            request.get("stop_sequences")
        );
        assert!(
            request.get("tool_choice").is_none(),
            "tool_choice should be omitted when 'none', got: {:?}",
            request.get("tool_choice")
        );
    }

    #[test]
    fn anthropic_to_openai_omits_null_optional_fields() {
        let request = anthropic_to_openai_chat_request(&json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 16,
            "temperature": null,
            "stop_sequences": null,
            "tool_choice": null
        }))
        .unwrap();

        assert!(
            request.get("stop").is_none(),
            "stop should be omitted when stop_sequences is null, got: {:?}",
            request.get("stop")
        );
        assert!(
            request.get("tool_choice").is_none(),
            "tool_choice should be omitted when null, got: {:?}",
            request.get("tool_choice")
        );
    }

    // Characterization tests pinning the exact error-body shapes of both
    // handlers. These intentionally assert the *asymmetries* between the
    // OpenAI and Anthropic dialects so the upcoming handler refactor cannot
    // silently change a client-visible response.

    #[tokio::test]
    async fn openai_method_not_allowed_returns_openai_error_shape() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .get(format!("http://{}/v1/chat/completions", addr))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 405);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body,
            json!({
                "error": {
                    "message": "Only POST is supported for this endpoint",
                    "type": "invalid_request_error"
                }
            })
        );
        task.abort();
    }

    #[tokio::test]
    async fn anthropic_method_not_allowed_returns_anthropic_error_shape() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .get(format!("http://{}/v1/messages", addr))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 405);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body,
            json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "Only POST is supported for this endpoint"
                }
            })
        );
        task.abort();
    }

    #[tokio::test]
    async fn openai_invalid_json_returns_flat_error_string() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", addr))
            .body("not json")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 400);
        let body: Value = response.json().await.unwrap();
        // OpenAI dialect: the invalid-JSON body is a *flat* string, unlike
        // every other OpenAI error which nests under `error.{message,type}`.
        assert!(
            body["error"].is_string(),
            "expected flat string error, got {body}"
        );
        assert!(body["error"].as_str().unwrap().starts_with("Invalid JSON"));
        task.abort();
    }

    #[tokio::test]
    async fn anthropic_invalid_json_returns_nested_error_object() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", addr))
            .body("not json")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 400);
        let body: Value = response.json().await.unwrap();
        // Anthropic dialect: invalid-JSON is nested, not flat.
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Invalid JSON"));
        task.abort();
    }

    #[tokio::test]
    async fn openai_missing_provider_uses_invalid_request_type() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", addr))
            .json(&json!({"model": "no-such-model", "messages": []}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 404);
        let body: Value = response.json().await.unwrap();
        assert!(
            body.get("type").is_none(),
            "OpenAI body has no top-level type"
        );
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(
            body["error"]["message"],
            "No provider found for model: no-such-model"
        );
        task.abort();
    }

    #[tokio::test]
    async fn anthropic_missing_provider_uses_not_found_type() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", addr))
            .json(&json!({"model": "no-such-model", "messages": []}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 404);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["type"], "error");
        // Anthropic uses a distinct not_found_error type here.
        assert_eq!(body["error"]["type"], "not_found_error");
        assert_eq!(
            body["error"]["message"],
            "No provider found for model: no-such-model"
        );
        task.abort();
    }

    #[tokio::test]
    async fn openai_all_providers_failed_uses_server_error_type() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", addr))
            .json(&json!({"model": "test-model", "messages": []}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 502);
        let body: Value = response.json().await.unwrap();
        assert!(
            body.get("type").is_none(),
            "OpenAI body has no top-level type"
        );
        assert_eq!(body["error"]["message"], "All providers failed");
        assert_eq!(body["error"]["type"], "server_error");
        assert!(body["error"]["details"].is_array());
        task.abort();
    }

    #[tokio::test]
    async fn anthropic_all_providers_failed_uses_api_error_type() {
        let config = AppConfig {
            providers: vec![provider("http://127.0.0.1:9".to_string(), 0)],
            ..Default::default()
        };
        let proxy = create_proxy_router(Arc::new(RwLock::new(config)));
        let (addr, task) = spawn_router(proxy).await;

        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", addr))
            .json(&json!({
                "model": "test-model",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 16
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 502);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["message"], "All providers failed");
        // Anthropic uses api_error where OpenAI uses server_error.
        assert_eq!(body["error"]["type"], "api_error");
        assert!(body["error"]["details"].is_array());
        task.abort();
    }
}
