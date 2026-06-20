use crate::config::{
    normalize_model_ref, normalize_model_refs, AppConfig, ModelPrice, ModelRef, Provider,
};
use crate::proxy::{self, TokenUsage};
use crate::storage::{FusionRunDetails, FusionStepEntry, RequestLogStore};
use futures::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;
use tokio::time::{timeout, Duration};

pub const FUSION_MODEL_ID: &str = "nexus/fusion";
const DEFAULT_STAGE_MAX_TOKENS: u64 = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionErrorKind {
    BadRequest,
    Upstream,
}

#[derive(Debug, Clone)]
pub struct FusionError {
    kind: FusionErrorKind,
    message: String,
}

impl FusionError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: FusionErrorKind::BadRequest,
            message: message.into(),
        }
    }

    fn upstream(message: impl Into<String>) -> Self {
        Self {
            kind: FusionErrorKind::Upstream,
            message: message.into(),
        }
    }

    pub fn is_bad_request(&self) -> bool {
        self.kind == FusionErrorKind::BadRequest
    }
}

impl fmt::Display for FusionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FusionModelOverride {
    #[serde(default)]
    pub panel_models: Option<Vec<ModelRef>>,
    #[serde(default)]
    pub judge_model: Option<ModelRef>,
    #[serde(default)]
    pub final_model: Option<ModelRef>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FusionWorkbenchRequest {
    #[serde(default)]
    pub input_protocol: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub source_log_id: Option<i64>,
    #[serde(default)]
    pub nexus_fusion: Option<FusionModelOverride>,
}

#[derive(Debug, Clone)]
pub struct CompletedFusionRun {
    pub details: FusionRunDetails,
    pub usage: TokenUsage,
    pub final_content: String,
}

#[derive(Debug, Clone)]
struct ResolvedModel {
    model_ref: ModelRef,
    provider: Provider,
}

#[derive(Debug, Clone)]
struct ResolvedFusionModels {
    panels: Vec<ResolvedModel>,
    judge: ResolvedModel,
    final_model: ResolvedModel,
    timeout_secs: u64,
}

#[derive(Debug, Clone)]
struct ModelCallOutcome {
    target: ResolvedModel,
    content: Option<String>,
    error: Option<String>,
    usage: TokenUsage,
    latency_ms: u64,
    cost: f64,
}

#[derive(Debug, Default)]
struct RunTotals {
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost: f64,
    panel_count: u64,
}

impl RunTotals {
    fn record(&mut self, outcome: &ModelCallOutcome, role: &str) {
        if outcome.error.is_none() && role == "panel" {
            self.panel_count += 1;
        }
        self.prompt_tokens += outcome.usage.input_tokens;
        self.completion_tokens += outcome.usage.output_tokens;
        self.cache_read_tokens += outcome.usage.cache_read_tokens;
        self.cache_write_tokens += outcome.usage.cache_write_tokens;
        self.cost += outcome.cost;
    }

    fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.prompt_tokens,
            output_tokens: self.completion_tokens,
            cached_tokens: self.cache_read_tokens + self.cache_write_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
        }
    }

    fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

pub fn is_fusion_model(model: &str) -> bool {
    model.eq_ignore_ascii_case(FUSION_MODEL_ID)
}

pub fn openai_chat_response(content: &str, usage: TokenUsage) -> Value {
    json!({
        "id": format!("chatcmpl-fusion-{}", chrono::Utc::now().timestamp()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": FUSION_MODEL_ID,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": usage.input_tokens,
            "completion_tokens": usage.output_tokens,
            "total_tokens": usage.input_tokens + usage.output_tokens
        }
    })
}

pub fn anthropic_message_response(content: &str, usage: TokenUsage) -> Value {
    json!({
        "id": format!("msg_fusion_{}", chrono::Utc::now().timestamp()),
        "type": "message",
        "role": "assistant",
        "model": FUSION_MODEL_ID,
        "content": [{
            "type": "text",
            "text": content
        }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens
        }
    })
}

pub async fn run_workbench(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    request: FusionWorkbenchRequest,
) -> Result<CompletedFusionRun, FusionError> {
    let messages = if !request.messages.is_empty() {
        request.messages
    } else {
        vec![json!({
            "role": "user",
            "content": request.prompt
        })]
    };
    run_with_messages(
        client,
        store,
        config,
        normalize_protocol(&request.input_protocol),
        request.source_log_id,
        messages,
        request.nexus_fusion,
        None,
    )
    .await
}

pub async fn run_from_openai_request(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    body: &Value,
    source_log_id: Option<i64>,
) -> Result<CompletedFusionRun, FusionError> {
    reject_unsupported_request(body)?;
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| FusionError::bad_request("Fusion requires OpenAI chat messages"))?;
    let overrides = fusion_overrides_from_body(body);
    let final_max_tokens = requested_max_tokens(body);
    run_with_messages(
        client,
        store,
        config,
        "openai".to_string(),
        source_log_id,
        messages,
        overrides,
        final_max_tokens,
    )
    .await
}

pub async fn run_from_anthropic_request(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    body: &Value,
    source_log_id: Option<i64>,
) -> Result<CompletedFusionRun, FusionError> {
    reject_unsupported_request(body)?;
    let openai_body =
        proxy::anthropic_to_openai_chat_request(body).map_err(FusionError::bad_request)?;
    let messages = openai_body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| FusionError::bad_request("Fusion requires Anthropic messages"))?;
    let overrides = fusion_overrides_from_body(body);
    let final_max_tokens = requested_max_tokens(body);
    run_with_messages(
        client,
        store,
        config,
        "anthropic".to_string(),
        source_log_id,
        messages,
        overrides,
        final_max_tokens,
    )
    .await
}

fn reject_unsupported_request(body: &Value) -> Result<(), FusionError> {
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        return Err(FusionError::bad_request(
            "Fusion does not support streaming yet",
        ));
    }
    if tool_calling_requested(body) {
        return Err(FusionError::bad_request(
            "Fusion does not support tool calling yet",
        ));
    }
    reject_non_text_body_content(body).map_err(FusionError::bad_request)?;
    Ok(())
}

fn fusion_overrides_from_body(body: &Value) -> Option<FusionModelOverride> {
    body.get("nexus_fusion")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn requested_max_tokens(body: &Value) -> Option<u64> {
    body.get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn tool_calling_requested(body: &Value) -> bool {
    let tool_choice = body.get("tool_choice");
    if matches!(tool_choice, Some(Value::Null))
        || matches!(
            tool_choice.and_then(Value::as_str),
            Some(choice) if choice.eq_ignore_ascii_case("none")
        )
    {
        return false;
    }

    let tools_non_empty = match body.get("tools") {
        None | Some(Value::Null) => false,
        Some(Value::Array(items)) => !items.is_empty(),
        Some(_) => true,
    };
    if tools_non_empty {
        return true;
    }

    match tool_choice {
        None | Some(Value::Null) => false,
        Some(Value::String(choice)) => {
            !choice.eq_ignore_ascii_case("auto") && !choice.eq_ignore_ascii_case("none")
        }
        Some(Value::Object(_))
        | Some(Value::Array(_))
        | Some(Value::Bool(_))
        | Some(Value::Number(_)) => true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_with_messages(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    input_protocol: String,
    source_log_id: Option<i64>,
    messages: Vec<Value>,
    overrides: Option<FusionModelOverride>,
    final_max_tokens: Option<u64>,
) -> Result<CompletedFusionRun, FusionError> {
    let resolved = resolve_fusion_models(config, overrides).map_err(FusionError::bad_request)?;
    reject_non_text_messages(&messages).map_err(FusionError::bad_request)?;
    let run_id = store
        .create_fusion_run(source_log_id, &input_protocol)
        .await
        .map_err(FusionError::upstream)?;
    let started = Instant::now();
    let mut totals = RunTotals::default();

    let execution = execute_fusion(
        client,
        store,
        config,
        run_id,
        &resolved,
        &messages,
        final_max_tokens.unwrap_or(DEFAULT_STAGE_MAX_TOKENS),
        &mut totals,
    )
    .await;

    let duration_ms = started.elapsed().as_millis() as u64;
    match execution {
        Ok(final_content) => {
            store
                .finish_fusion_run(
                    run_id,
                    "succeeded",
                    duration_ms,
                    totals.panel_count,
                    totals.total_tokens(),
                    totals.cost,
                    Some(&final_content),
                    None,
                )
                .await
                .map_err(FusionError::upstream)?;
            let details = store
                .get_fusion_run(run_id)
                .await
                .map_err(FusionError::upstream)?
                .ok_or_else(|| {
                    FusionError::upstream(format!(
                        "Fusion run not found after completion: {run_id}"
                    ))
                })?;
            Ok(CompletedFusionRun {
                details,
                usage: totals.token_usage(),
                final_content,
            })
        }
        Err(error) => {
            store
                .finish_fusion_run(
                    run_id,
                    "failed",
                    duration_ms,
                    totals.panel_count,
                    totals.total_tokens(),
                    totals.cost,
                    None,
                    Some(&error),
                )
                .await
                .map_err(FusionError::upstream)?;
            Err(FusionError::upstream(error))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_fusion(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    run_id: i64,
    resolved: &ResolvedFusionModels,
    messages: &[Value],
    final_max_tokens: u64,
    totals: &mut RunTotals,
) -> Result<String, String> {
    let original_request = format_messages(messages);
    let panel_messages = with_system_message(
        messages,
        "You are one independent panel model in API Nexus Fusion. Answer the user's request directly. Prioritize correctness, concrete reasoning, and useful details. Do not mention that you are part of a panel.",
    );

    let panel_calls = resolved
        .panels
        .iter()
        .cloned()
        .map(|target| {
            call_model(
                client.clone(),
                config.clone(),
                target,
                panel_messages.clone(),
                resolved.timeout_secs,
                DEFAULT_STAGE_MAX_TOKENS,
            )
        })
        .collect::<Vec<_>>();
    let panel_outcomes = join_all(panel_calls).await;

    let mut successful_panels = Vec::new();
    let mut panel_errors = Vec::new();
    for outcome in panel_outcomes {
        record_step(store, run_id, "panel", &outcome, totals).await?;
        if let Some(content) = outcome.content.clone().filter(|_| outcome.error.is_none()) {
            successful_panels.push((outcome.target.model_ref.clone(), content));
        } else if let Some(error) = outcome.error {
            panel_errors.push(format!(
                "{} / {}: {}",
                outcome.target.provider.name, outcome.target.model_ref.model, error
            ));
        }
    }

    if successful_panels.is_empty() {
        let detail = if panel_errors.is_empty() {
            "No panel model returned content".to_string()
        } else {
            panel_errors.join("; ")
        };
        return Err(format!("All Fusion panel models failed: {detail}"));
    }

    let judge_messages = vec![
        json!({
            "role": "system",
            "content": "You are the API Nexus Fusion judge. Compare the panel outputs without writing the final answer. Return structured analysis with these sections: Consensus, Disagreements, Missing context or risks, Unique insights, Recommended synthesis."
        }),
        json!({
            "role": "user",
            "content": format!(
                "Original request:\n{}\n\nPanel outputs:\n{}",
                original_request,
                format_panel_outputs(&successful_panels)
            )
        }),
    ];
    let judge_outcome = call_model(
        client.clone(),
        config.clone(),
        resolved.judge.clone(),
        judge_messages,
        resolved.timeout_secs,
        DEFAULT_STAGE_MAX_TOKENS,
    )
    .await;
    record_step(store, run_id, "judge", &judge_outcome, totals).await?;
    let judge_content = judge_outcome
        .content
        .filter(|_| judge_outcome.error.is_none())
        .ok_or_else(|| {
            format!(
                "Fusion judge failed: {}",
                judge_outcome
                    .error
                    .unwrap_or_else(|| "empty judge response".to_string())
            )
        })?;

    let final_messages = vec![
        json!({
            "role": "system",
            "content": "You are API Nexus Fusion. Synthesize one final answer for the user from the original request, panel outputs, and judge analysis. Write naturally and do not expose hidden process unless it is directly useful."
        }),
        json!({
            "role": "user",
            "content": format!(
                "Original request:\n{}\n\nPanel outputs:\n{}\n\nJudge analysis:\n{}\n\nWrite the final answer now.",
                original_request,
                format_panel_outputs(&successful_panels),
                judge_content
            )
        }),
    ];
    let final_outcome = call_model(
        client.clone(),
        config.clone(),
        resolved.final_model.clone(),
        final_messages,
        resolved.timeout_secs,
        final_max_tokens,
    )
    .await;
    record_step(store, run_id, "final", &final_outcome, totals).await?;
    final_outcome
        .content
        .filter(|_| final_outcome.error.is_none())
        .ok_or_else(|| {
            format!(
                "Fusion final model failed: {}",
                final_outcome
                    .error
                    .unwrap_or_else(|| "empty final response".to_string())
            )
        })
}

async fn record_step(
    store: &Arc<RequestLogStore>,
    run_id: i64,
    role: &str,
    outcome: &ModelCallOutcome,
    totals: &mut RunTotals,
) -> Result<(), String> {
    totals.record(outcome, role);
    store
        .push_fusion_step(FusionStepEntry {
            id: 0,
            run_id,
            role: role.to_string(),
            provider_id: outcome.target.model_ref.provider_id.clone(),
            model: outcome.target.model_ref.model.clone(),
            status: if outcome.error.is_some() {
                "failed".to_string()
            } else {
                "succeeded".to_string()
            },
            latency_ms: outcome.latency_ms,
            prompt_tokens: outcome.usage.input_tokens,
            completion_tokens: outcome.usage.output_tokens,
            cost: outcome.cost,
            content: outcome.content.clone(),
            error: outcome.error.clone(),
        })
        .await?;
    Ok(())
}

async fn call_model(
    client: Client,
    config: AppConfig,
    target: ResolvedModel,
    messages: Vec<Value>,
    timeout_secs: u64,
    max_tokens: u64,
) -> ModelCallOutcome {
    let started = Instant::now();
    let result = timeout(
        Duration::from_secs(timeout_secs),
        call_model_inner(&client, &target, messages, max_tokens),
    )
    .await
    .unwrap_or_else(|_| Err(format!("Timed out after {timeout_secs}s")));
    let latency_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok((content, usage)) => {
            let cost = estimate_cost(&config, &target.provider, &target.model_ref.model, usage);
            ModelCallOutcome {
                target,
                content: Some(content),
                error: None,
                usage,
                latency_ms,
                cost,
            }
        }
        Err(error) => ModelCallOutcome {
            target,
            content: None,
            error: Some(error),
            usage: TokenUsage::default(),
            latency_ms,
            cost: 0.0,
        },
    }
}

async fn call_model_inner(
    client: &Client,
    target: &ResolvedModel,
    messages: Vec<Value>,
    max_tokens: u64,
) -> Result<(String, TokenUsage), String> {
    let openai_body = json!({
        "model": target.model_ref.model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": false
    });

    if proxy::is_anthropic_provider(&target.provider) {
        let body = proxy::openai_to_anthropic_request(&openai_body)?;
        let url = proxy::anthropic_upstream_url(&target.provider.base_url, "/v1/messages");
        let response = client
            .post(url)
            .header("content-type", "application/json")
            .header("x-api-key", &target.provider.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("HTTP {} - {}", status.as_u16(), text));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|error| format!("Failed to parse Anthropic response: {error}"))?;
        let usage = proxy::extract_token_usage(&body);
        let content = content_text(body.get("content").unwrap_or(&Value::Null));
        return Ok((non_empty_model_content(content)?, usage));
    }

    let url = proxy::openai_upstream_url(&target.provider.base_url, "/v1/chat/completions");
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!("Bearer {}", target.provider.api_key),
        )
        .json(&openai_body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("HTTP {} - {}", status.as_u16(), text));
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|error| format!("Failed to parse OpenAI response: {error}"))?;
    let usage = proxy::extract_token_usage(&body);
    let content = openai_response_text(&body);
    Ok((non_empty_model_content(content)?, usage))
}

fn non_empty_model_content(content: String) -> Result<String, String> {
    if content.trim().is_empty() {
        Err("empty model response".to_string())
    } else {
        Ok(content)
    }
}

fn resolve_fusion_models(
    config: &AppConfig,
    overrides: Option<FusionModelOverride>,
) -> Result<ResolvedFusionModels, String> {
    if !config.fusion.enabled {
        return Err("Fusion is disabled in settings".to_string());
    }

    let overrides = overrides.unwrap_or_default();
    let panel_refs = overrides
        .panel_models
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| config.fusion.panel_models.clone());
    let panel_refs = normalize_model_refs(panel_refs)
        .into_iter()
        .take(config.fusion.max_panel_models as usize)
        .collect::<Vec<_>>();
    if panel_refs.is_empty() {
        return Err("Fusion panel models are not configured".to_string());
    }

    let judge_ref = overrides
        .judge_model
        .and_then(normalize_model_ref)
        .or_else(|| config.fusion.judge_model.clone())
        .ok_or_else(|| "Fusion judge model is not configured".to_string())?;
    let final_ref = overrides
        .final_model
        .and_then(normalize_model_ref)
        .or_else(|| config.fusion.final_model.clone())
        .unwrap_or_else(|| judge_ref.clone());

    Ok(ResolvedFusionModels {
        panels: panel_refs
            .into_iter()
            .map(|model_ref| resolve_model_ref(config, model_ref))
            .collect::<Result<Vec<_>, _>>()?,
        judge: resolve_model_ref(config, judge_ref)?,
        final_model: resolve_model_ref(config, final_ref)?,
        timeout_secs: config.fusion.timeout_secs,
    })
}

fn resolve_model_ref(config: &AppConfig, model_ref: ModelRef) -> Result<ResolvedModel, String> {
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == model_ref.provider_id)
        .ok_or_else(|| format!("Fusion provider not found: {}", model_ref.provider_id))?;
    if !provider.enabled {
        return Err(format!("Fusion provider is disabled: {}", provider.name));
    }
    if !provider
        .models
        .iter()
        .any(|model| model == &model_ref.model)
    {
        return Err(format!(
            "Fusion model {} is not configured on provider {}",
            model_ref.model, provider.name
        ));
    }
    Ok(ResolvedModel {
        model_ref,
        provider: provider.clone(),
    })
}

fn reject_non_text_body_content(body: &Value) -> Result<(), String> {
    if let Some(system) = body.get("system") {
        reject_non_text_content(system)?;
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        reject_non_text_messages(messages)?;
    }
    Ok(())
}

fn reject_non_text_messages(messages: &[Value]) -> Result<(), String> {
    for message in messages {
        if let Some(content) = message.get("content") {
            reject_non_text_content(content)?;
        }
    }
    Ok(())
}

fn reject_non_text_content(content: &Value) -> Result<(), String> {
    match content {
        Value::Null | Value::String(_) => Ok(()),
        Value::Array(items) => {
            for item in items {
                let item_type = item.get("type").and_then(Value::as_str);
                if item_type != Some("text") {
                    return Err(
                        "Fusion does not support image/audio or non-text message content yet"
                            .to_string(),
                    );
                }
                if item.get("text").is_some_and(|text| !text.is_string()) {
                    return Err(
                        "Fusion does not support image/audio or non-text message content yet"
                            .to_string(),
                    );
                }
            }
            Ok(())
        }
        _ => Err("Fusion does not support image/audio or non-text message content yet".to_string()),
    }
}

fn with_system_message(messages: &[Value], system: &str) -> Vec<Value> {
    let mut output = vec![json!({
        "role": "system",
        "content": system
    })];
    output.extend_from_slice(messages);
    output
}

fn normalize_protocol(protocol: &str) -> String {
    if protocol.eq_ignore_ascii_case("anthropic") {
        "anthropic".to_string()
    } else {
        "openai".to_string()
    }
}

fn format_messages(messages: &[Value]) -> String {
    messages
        .iter()
        .map(|message| {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let text = content_text(message.get("content").unwrap_or(&Value::Null));
            format!("{role}: {text}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_panel_outputs(outputs: &[(ModelRef, String)]) -> String {
    outputs
        .iter()
        .enumerate()
        .map(|(index, (model_ref, content))| {
            format!(
                "Panel {} ({} / {}):\n{}",
                index + 1,
                model_ref.provider_id,
                model_ref.model,
                content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(Value::as_str) == Some("text") {
                    item.get("text").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn openai_response_text(body: &Value) -> String {
    let Some(message) = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
    else {
        return String::new();
    };
    content_text(message.get("content").unwrap_or(&Value::Null))
}

fn estimate_cost(config: &AppConfig, provider: &Provider, model: &str, usage: TokenUsage) -> f64 {
    let model_key = model.trim().to_ascii_lowercase();
    let provider_price = config.model_prices.iter().find(|price| {
        price.provider_id == provider.id && price.model.trim().eq_ignore_ascii_case(&model_key)
    });
    let wildcard_price = config.model_prices.iter().find(|price| {
        price.provider_id.trim().is_empty() && price.model.trim().eq_ignore_ascii_case(&model_key)
    });
    let Some(price) = provider_price.or(wildcard_price) else {
        return 0.0;
    };
    calculate_cost(provider, price, usage)
}

fn calculate_cost(provider: &Provider, price: &ModelPrice, usage: TokenUsage) -> f64 {
    let regular_input_tokens = if provider.protocol.eq_ignore_ascii_case("openai") {
        usage.input_tokens.saturating_sub(usage.cache_read_tokens)
    } else {
        usage.input_tokens
    };
    (regular_input_tokens as f64 / 1_000_000.0) * price.input_usd_per_million
        + (usage.output_tokens as f64 / 1_000_000.0) * price.output_usd_per_million
        + (usage.cache_read_tokens as f64 / 1_000_000.0)
            * price
                .cache_read_usd_per_million
                .max(price.cached_usd_per_million)
        + (usage.cache_write_tokens as f64 / 1_000_000.0) * price.cache_write_usd_per_million
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FusionConfig;

    #[test]
    fn openai_response_shape_contains_final_content() {
        let response = openai_chat_response(
            "done",
            TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                ..Default::default()
            },
        );
        assert_eq!(response["model"], FUSION_MODEL_ID);
        assert_eq!(response["choices"][0]["message"]["content"], "done");
        assert_eq!(response["usage"]["total_tokens"], 5);
    }

    #[test]
    fn rejects_non_text_message_content() {
        let body = json!({
            "model": FUSION_MODEL_ID,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,abc"}
                }]
            }]
        });

        let error = reject_unsupported_request(&body).unwrap_err();
        assert!(error.to_string().contains("non-text"));
    }

    #[test]
    fn allows_empty_tool_fields_from_generic_clients() {
        for body in [
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tools": []
            }),
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tools": [],
                "tool_choice": "auto"
            }),
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tool_choice": "none"
            }),
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tools": [{"type": "function", "function": {"name": "lookup"}}],
                "tool_choice": "none"
            }),
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tool_choice": null
            }),
        ] {
            reject_unsupported_request(&body).unwrap();
        }
    }

    #[test]
    fn rejects_actual_tool_calling_requests() {
        for body in [
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tools": [{"type": "function", "function": {"name": "lookup"}}]
            }),
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tools": [{"type": "function", "function": {"name": "lookup"}}],
                "tool_choice": "auto"
            }),
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tool_choice": "required"
            }),
            json!({
                "model": FUSION_MODEL_ID,
                "messages": [{"role": "user", "content": "ping"}],
                "tool_choice": {"type": "function", "function": {"name": "lookup"}}
            }),
        ] {
            let error = reject_unsupported_request(&body).unwrap_err();
            assert!(error.to_string().contains("tool calling"));
        }
    }

    #[test]
    fn resolves_final_model_to_judge_when_not_configured() {
        let config = AppConfig {
            providers: vec![Provider {
                id: "p1".to_string(),
                name: "Provider".to_string(),
                models: vec!["m1".to_string(), "m2".to_string()],
                enabled: true,
                ..Default::default()
            }],
            fusion: FusionConfig {
                panel_models: vec![ModelRef {
                    provider_id: "p1".to_string(),
                    model: "m1".to_string(),
                }],
                judge_model: Some(ModelRef {
                    provider_id: "p1".to_string(),
                    model: "m2".to_string(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let resolved = resolve_fusion_models(&config, None).unwrap();
        assert_eq!(resolved.final_model.model_ref.model, "m2");
    }
}
