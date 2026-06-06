use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
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
use tower_http::cors::{Any, CorsLayer};

use crate::config::{AppConfig, Provider};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const PROTOCOL_ANTHROPIC: &str = "anthropic";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
}

impl TokenUsage {
    fn is_empty(self) -> bool {
        self.input_tokens == 0 && self.output_tokens == 0 && self.cached_tokens == 0
    }

    fn absorb_max(&mut self, usage: TokenUsage) {
        self.input_tokens = self.input_tokens.max(usage.input_tokens);
        self.output_tokens = self.output_tokens.max(usage.output_tokens);
        self.cached_tokens = self.cached_tokens.max(usage.cached_tokens);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStats {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
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
    }
}

pub type TokenStatsState = Arc<RwLock<TokenStats>>;

#[derive(Clone)]
pub struct ProxyState {
    pub config: Arc<RwLock<AppConfig>>,
    pub client: Client,
    pub token_stats: TokenStatsState,
}

#[cfg(test)]
pub fn create_proxy_router(config: Arc<RwLock<AppConfig>>) -> Router {
    create_proxy_router_with_stats(config, Arc::new(RwLock::new(TokenStats::default())))
}

pub fn create_proxy_router_with_stats(
    config: Arc<RwLock<AppConfig>>,
    token_stats: TokenStatsState,
) -> Router {
    let state = ProxyState {
        config,
        token_stats,
        client: Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap(),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any);

    Router::new()
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

async fn health_handler() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "API Nexus",
        "version": "1.0.0"
    }))
}

async fn list_models_handler(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    let config = state.config.read().await;
    if let Some(response) = proxy_api_key_error(&headers, &config) {
        return response;
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
    headers.contains_key("anthropic-version") || headers.contains_key("x-api-key")
}

fn is_anthropic_provider(provider: &Provider) -> bool {
    provider.protocol.eq_ignore_ascii_case(PROTOCOL_ANTHROPIC)
}

fn provider_matches_model(provider: &Provider, model: &str) -> bool {
    provider.enabled && provider.models.iter().any(|m| m == model)
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

fn proxy_api_key_error(headers: &HeaderMap, config: &AppConfig) -> Option<Response> {
    if config.proxy_api_key.is_empty() {
        return None;
    }

    match incoming_api_key(headers) {
        Some(incoming_key) if incoming_key == config.proxy_api_key => None,
        _ => Some(auth_error_response()),
    }
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

fn first_u64_field(value: &Value, field_names: &[&str]) -> u64 {
    field_names
        .iter()
        .find_map(|field_name| value.get(*field_name).and_then(Value::as_u64))
        .unwrap_or_default()
}

fn sum_cache_token_fields(value: &Value) -> u64 {
    const CACHE_FIELDS: &[&str] = &[
        "cached_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "cache_read",
        "cache_creation",
        "cache_write",
    ];

    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, nested)| {
                let current = if CACHE_FIELDS.contains(&key.as_str()) {
                    nested.as_u64().unwrap_or_default()
                } else {
                    0
                };
                current + sum_cache_token_fields(nested)
            })
            .sum(),
        Value::Array(items) => items.iter().map(sum_cache_token_fields).sum(),
        _ => 0,
    }
}

fn extract_token_usage(value: &Value) -> TokenUsage {
    let usage = value
        .get("usage")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
        })
        .unwrap_or(value);

    TokenUsage {
        input_tokens: first_u64_field(usage, &["prompt_tokens", "input_tokens"]),
        output_tokens: first_u64_field(usage, &["completion_tokens", "output_tokens"]),
        cached_tokens: sum_cache_token_fields(usage),
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
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);

    if is_stream {
        let stream = async_stream::stream! {
            let mut upstream = resp.bytes_stream();
            let mut buffer = String::new();
            let mut usage = TokenUsage::default();

            while let Some(chunk) = upstream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(pos) = buffer.find("\n\n") {
                            let frame = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();
                            usage.absorb_max(extract_token_usage_from_sse_frame(&frame));
                        }

                        yield Ok::<Bytes, io::Error>(bytes);
                    }
                    Err(err) => {
                        yield Err(io::Error::other(err.to_string()));
                        return;
                    }
                }
            }

            if !buffer.trim().is_empty() {
                usage.absorb_max(extract_token_usage_from_sse_frame(&buffer));
            }

            record_token_usage(&token_stats, usage).await;
        };

        return Response::builder()
            .status(status)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from_stream(stream))
            .unwrap_or_else(|err| {
                bad_gateway(format!("Failed to build stream response: {}", err))
            });
    }

    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = match resp.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => return bad_gateway(format!("Failed to read upstream response: {}", err)),
    };

    if let Ok(body_json) = serde_json::from_slice::<Value>(&bytes) {
        record_token_usage(&token_stats, extract_token_usage(&body_json)).await;
    }

    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    } else {
        builder = builder.header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }

    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|err| bad_gateway(format!("Failed to build upstream response: {}", err)))
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

fn openai_to_anthropic_request(openai: &Value) -> Result<Value, String> {
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
            request[field] = value.clone();
        }
    }

    if let Some(stop) = openai.get("stop") {
        request["stop_sequences"] = match stop {
            Value::String(stop) => json!([stop]),
            Value::Array(_) => stop.clone(),
            _ => Value::Null,
        };
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
        request["tool_choice"] = match tool_choice {
            Value::String(choice) if choice == "required" => json!({"type": "any"}),
            Value::String(choice) if choice == "auto" => json!({"type": "auto"}),
            Value::String(choice) if choice == "none" => Value::Null,
            Value::Object(_) => {
                if let Some(name) = tool_choice
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                {
                    json!({"type": "tool", "name": name})
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        };
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
            "role": "user",
            "content": text_parts.join("\n")
        }));
    }

    openai_messages
}

fn anthropic_to_openai_chat_request(anthropic: &Value) -> Result<Value, String> {
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
            request[field] = value.clone();
        }
    }

    if let Some(stop_sequences) = anthropic.get("stop_sequences") {
        request["stop"] = stop_sequences.clone();
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
        request["tool_choice"] = match tool_choice {
            Value::Object(choice) => match choice.get("type").and_then(Value::as_str) {
                Some("auto") => Value::String("auto".to_string()),
                Some("any") => Value::String("required".to_string()),
                Some("tool") => choice
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| json!({"type": "function", "function": {"name": name}}))
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            },
            _ => Value::Null,
        };
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
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let stream = async_stream::stream! {
        let mut upstream = resp.bytes_stream();
        let mut buffer = String::new();
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
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(pos) = buffer.find("\n\n") {
                        let frame = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();
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
            "usage": {"output_tokens": 0}
        }))));
        yield Ok::<Bytes, io::Error>(Bytes::from(anthropic_sse_frame("message_stop", json!({
            "type": "message_stop"
        }))));

        record_token_usage(&token_stats, usage).await;
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
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let stream = async_stream::stream! {
        let mut upstream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut usage = TokenUsage::default();
        let mut response_id = format!("chatcmpl-anthropic-{}", unix_timestamp());
        let mut sent_done = false;

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(pos) = buffer.find("\n\n") {
                        let frame = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();
                        usage.absorb_max(extract_token_usage_from_sse_frame(&frame));
                        for output in anthropic_frame_to_openai_chunks(&frame, &mut response_id, &model) {
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

        if !buffer.trim().is_empty() {
            usage.absorb_max(extract_token_usage_from_sse_frame(&buffer));
            for output in anthropic_frame_to_openai_chunks(&buffer, &mut response_id, &model) {
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

async fn anthropic_handler(
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
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "Only POST is supported for this endpoint"
                }
            })),
        )
            .into_response();
    }

    let request_path = uri.path().to_string();

    let config = state.config.read().await;
    if let Some(response) = proxy_api_key_error(&headers, &config) {
        return response;
    }
    drop(config);

    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": format!("Invalid JSON: {}", e)
                    }
                })),
            )
                .into_response();
        }
    };

    let model = body_json
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_stream = body_json
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let config = state.config.read().await;
    let mut providers: Vec<_> = config
        .providers
        .iter()
        .filter(|provider| provider_matches_model(provider, &model))
        .cloned()
        .collect();
    drop(config);

    providers.sort_by_key(|provider| provider.priority);

    if providers.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "type": "error",
                "error": {
                    "type": "not_found_error",
                    "message": format!("No provider found for model: {}", model)
                }
            })),
        )
            .into_response();
    }

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

                    return passthrough_response(resp, is_stream, state.token_stats.clone()).await;
                }
                Err(err) => {
                    errors.push(format!("{}: {}", provider.name, err));
                    continue;
                }
            }
        }

        if request_path == "/v1/messages/count_tokens" {
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
                    )
                    .await;
                }

                match resp.json::<Value>().await {
                    Ok(openai_response) => {
                        record_token_usage(
                            &state.token_stats,
                            extract_token_usage(&openai_response),
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

    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": "All providers failed",
                "details": errors
            }
        })),
    )
        .into_response()
}

async fn proxy_handler(
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
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({
                "error": {
                    "message": "Only POST is supported for this endpoint",
                    "type": "invalid_request_error"
                }
            })),
        )
            .into_response();
    }

    let request_path = uri.path().to_string();

    let config = state.config.read().await;
    if let Some(response) = proxy_api_key_error(&headers, &config) {
        return response;
    }
    drop(config);

    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Invalid JSON: {}", e)})),
            )
                .into_response();
        }
    };

    let model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    let is_stream = body_json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let config = state.config.read().await;
    let mut providers: Vec<_> = config
        .providers
        .iter()
        .filter(|provider| {
            provider_matches_model(provider, &model)
                && (!is_anthropic_provider(provider) || request_path == "/v1/chat/completions")
        })
        .cloned()
        .collect();
    drop(config);

    providers.sort_by_key(|p| p.priority);

    if providers.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("No provider found for model: {}", model),
                    "type": "invalid_request_error"
                }
            })),
        )
            .into_response();
    }

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
                        )
                        .await;
                    }

                    match resp.json::<Value>().await {
                        Ok(anthropic_response) => {
                            record_token_usage(
                                &state.token_stats,
                                extract_token_usage(&anthropic_response),
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

                return passthrough_response(resp, is_stream, state.token_stats.clone()).await;
            }
            Err(e) => {
                errors.push(format!("{}: {}", provider.name, e));
                continue;
            }
        }
    }

    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": "All providers failed",
                "type": "server_error",
                "details": errors
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;
    use axum::routing::post;
    use std::net::SocketAddr;
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
                cached_tokens: 56
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
                cached_tokens: 52
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
        let proxy = create_proxy_router_with_stats(Arc::new(RwLock::new(config)), stats.clone());
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
}
