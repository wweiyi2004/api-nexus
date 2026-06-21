use crate::agentic::{self, ToolCall, ToolExecutor, ToolSpec};
use crate::config::{
    normalize_model_ref, normalize_model_refs, AppConfig, ModelPrice, ModelRef, Provider,
};
use crate::proxy::{self, TokenUsage};
use crate::responses::{ClientTool, ParsedResponsesRequest};
use crate::storage::{FusionRunDetails, FusionStepEntry, RequestLogStore};
use crate::web_tools::WebTools;
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
pub struct CompletedOnDemandRun {
    pub usage: TokenUsage,
    pub final_content: String,
}

#[derive(Debug, Clone)]
pub struct CompletedResponsesTurn {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
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
        true,
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
        true,
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
        true,
    )
    .await
}

pub async fn run_on_demand_from_openai_request(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    body: &Value,
) -> Result<CompletedOnDemandRun, FusionError> {
    reject_on_demand_request(body)?;
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| FusionError::bad_request("Fusion requires OpenAI chat messages"))?;
    run_on_demand_with_messages(
        client,
        store,
        config,
        "openai",
        messages,
        fusion_overrides_from_body(body),
        requested_max_tokens(body),
        openai_requires_tool(body),
    )
    .await
}

pub async fn run_on_demand_from_anthropic_request(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    body: &Value,
) -> Result<CompletedOnDemandRun, FusionError> {
    reject_on_demand_request(body)?;
    let openai_body =
        proxy::anthropic_to_openai_chat_request(body).map_err(FusionError::bad_request)?;
    let messages = openai_body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| FusionError::bad_request("Fusion requires Anthropic messages"))?;
    run_on_demand_with_messages(
        client,
        store,
        config,
        "anthropic",
        messages,
        fusion_overrides_from_body(body),
        requested_max_tokens(body),
        anthropic_requires_tool(body),
    )
    .await
}

pub async fn run_from_responses_request(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    request: ParsedResponsesRequest,
) -> Result<CompletedResponsesTurn, FusionError> {
    if config.fusion.mode == "on_demand" {
        run_on_demand_responses(client, store, config, request).await
    } else {
        run_forced_responses(client, store, config, request).await
    }
}

async fn run_forced_responses(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    request: ParsedResponsesRequest,
) -> Result<CompletedResponsesTurn, FusionError> {
    let analysis = run_with_messages(
        client,
        store,
        config,
        "responses".to_string(),
        None,
        request.messages.clone(),
        None,
        None,
        false,
    )
    .await?;
    let resolved = resolve_fusion_models(config, None).map_err(FusionError::bad_request)?;
    let mut final_messages = with_system_message(
        &request.messages,
        "You are API Nexus Fusion's final coding model. Use the judge analysis to continue the Codex turn. Call a client tool when repository inspection or modification is needed; otherwise answer the user directly.",
    );
    final_messages.push(json!({
        "role": "user",
        "content": format!("Fusion judge analysis:\n{}", analysis.final_content)
    }));
    let final_started = Instant::now();
    let outcome = run_client_capable_model(
        client,
        &resolved.final_model,
        final_messages,
        request.max_output_tokens,
        &[],
        &request.client_tools,
        &NO_TOOLS_EXECUTOR,
        0,
        request.require_tool,
    )
    .await;
    let final_latency_ms = final_started.elapsed().as_millis() as u64;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            store
                .push_fusion_step(FusionStepEntry {
                    id: 0,
                    run_id: analysis.details.run.id,
                    role: "final".to_string(),
                    provider_id: resolved.final_model.model_ref.provider_id.clone(),
                    model: resolved.final_model.model_ref.model.clone(),
                    status: "failed".to_string(),
                    latency_ms: final_latency_ms,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    cost: 0.0,
                    content: None,
                    error: Some(error.clone()),
                })
                .await
                .map_err(FusionError::upstream)?;
            store
                .finish_fusion_run(
                    analysis.details.run.id,
                    "failed",
                    analysis
                        .details
                        .run
                        .duration_ms
                        .saturating_add(final_latency_ms),
                    analysis.details.run.panel_count,
                    analysis.details.run.total_tokens,
                    analysis.details.run.estimated_cost,
                    None,
                    Some(&error),
                )
                .await
                .map_err(FusionError::upstream)?;
            return Err(FusionError::upstream(error));
        }
    };
    let final_usage = tool_loop_usage(&outcome);
    let final_cost = estimate_cost(
        config,
        &resolved.final_model.provider,
        &resolved.final_model.model_ref.model,
        final_usage,
    );
    let final_content = tool_loop_log_content(&outcome);
    store
        .push_fusion_step(FusionStepEntry {
            id: 0,
            run_id: analysis.details.run.id,
            role: "final".to_string(),
            provider_id: resolved.final_model.model_ref.provider_id.clone(),
            model: resolved.final_model.model_ref.model.clone(),
            status: "succeeded".to_string(),
            latency_ms: final_latency_ms,
            prompt_tokens: final_usage.input_tokens,
            completion_tokens: final_usage.output_tokens,
            cost: final_cost,
            content: Some(final_content.clone()),
            error: None,
        })
        .await
        .map_err(FusionError::upstream)?;
    store
        .finish_fusion_run(
            analysis.details.run.id,
            "succeeded",
            analysis
                .details
                .run
                .duration_ms
                .saturating_add(final_latency_ms),
            analysis.details.run.panel_count,
            analysis
                .details
                .run
                .total_tokens
                .saturating_add(final_usage.input_tokens)
                .saturating_add(final_usage.output_tokens),
            analysis.details.run.estimated_cost + final_cost,
            Some(&final_content),
            None,
        )
        .await
        .map_err(FusionError::upstream)?;
    Ok(completed_responses_turn(outcome, analysis.usage))
}

async fn run_on_demand_responses(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    request: ParsedResponsesRequest,
) -> Result<CompletedResponsesTurn, FusionError> {
    if !config.fusion.enabled {
        return Err(FusionError::bad_request("Fusion is disabled in settings"));
    }
    let outer_ref = config
        .fusion
        .outer_model
        .clone()
        .ok_or_else(|| FusionError::bad_request("Fusion outer model is not configured"))?;
    if is_fusion_model(&outer_ref.model) {
        return Err(FusionError::bad_request(
            "Fusion outer model cannot be nexus/fusion",
        ));
    }
    let outer = resolve_model_ref(config, outer_ref).map_err(FusionError::bad_request)?;
    let outer_messages = with_system_message(
        &request.messages,
        "You are the outer coding model for API Nexus Fusion. Answer the Codex turn directly or call a Codex client tool. Call the server-side fusion tool when independent panel analysis would materially improve correctness. Never return the fusion tool to the client.",
    );
    let executor = FusionToolExecutor {
        client: client.clone(),
        store: store.clone(),
        config: config.clone(),
        input_protocol: "responses".to_string(),
        messages: request.messages,
        overrides: None,
        usage: tokio::sync::Mutex::new(TokenUsage::default()),
    };
    let outcome = run_client_capable_model(
        client,
        &outer,
        outer_messages,
        request.max_output_tokens,
        &[fusion_tool_spec()],
        &request.client_tools,
        &executor,
        1,
        request.require_tool,
    )
    .await
    .map_err(FusionError::upstream)?;
    let mut server_usage = TokenUsage::default();
    add_token_usage(&mut server_usage, *executor.usage.lock().await);
    Ok(completed_responses_turn(outcome, server_usage))
}

#[allow(clippy::too_many_arguments)]
async fn run_client_capable_model(
    client: &Client,
    target: &ResolvedModel,
    messages: Vec<Value>,
    max_tokens: u64,
    server_tools: &[ToolSpec],
    client_tools: &[ClientTool],
    executor: &dyn ToolExecutor,
    max_server_tool_calls: u32,
    require_tool: bool,
) -> Result<agentic::ToolLoopOutcome, String> {
    let mut specs = server_tools.to_vec();
    specs.extend(client_tools.iter().map(|tool| tool.spec.clone()));
    let client_names = client_tools
        .iter()
        .map(|tool| tool.exposed_name.clone())
        .collect::<Vec<_>>();
    let (system, messages) = messages_for_provider(target, messages, max_tokens)?;
    agentic::run_mixed_tool_loop(
        client,
        &target.provider,
        &target.model_ref.model,
        system.as_deref(),
        messages,
        max_tokens,
        &specs,
        executor,
        max_server_tool_calls,
        require_tool,
        &client_names,
    )
    .await
}

fn completed_responses_turn(
    outcome: agentic::ToolLoopOutcome,
    mut accumulated_usage: TokenUsage,
) -> CompletedResponsesTurn {
    match outcome {
        agentic::ToolLoopOutcome::Text { content, usage } => {
            add_token_usage(&mut accumulated_usage, usage);
            CompletedResponsesTurn {
                text: Some(content),
                tool_calls: Vec::new(),
                usage: accumulated_usage,
            }
        }
        agentic::ToolLoopOutcome::ClientToolCalls { text, calls, usage } => {
            add_token_usage(&mut accumulated_usage, usage);
            CompletedResponsesTurn {
                text,
                tool_calls: calls,
                usage: accumulated_usage,
            }
        }
    }
}

fn tool_loop_usage(outcome: &agentic::ToolLoopOutcome) -> TokenUsage {
    match outcome {
        agentic::ToolLoopOutcome::Text { usage, .. }
        | agentic::ToolLoopOutcome::ClientToolCalls { usage, .. } => *usage,
    }
}

fn tool_loop_log_content(outcome: &agentic::ToolLoopOutcome) -> String {
    match outcome {
        agentic::ToolLoopOutcome::Text { content, .. } => content.clone(),
        agentic::ToolLoopOutcome::ClientToolCalls { text, calls, .. } => json!({
            "text": text,
            "tool_calls": calls.iter().map(|call| json!({
                "id": call.id,
                "name": call.name,
                "arguments": call.arguments
            })).collect::<Vec<_>>()
        })
        .to_string(),
    }
}

fn reject_on_demand_request(body: &Value) -> Result<(), FusionError> {
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        return Err(FusionError::bad_request(
            "Fusion does not support streaming yet",
        ));
    }
    reject_non_text_body_content(body).map_err(FusionError::bad_request)
}

fn openai_requires_tool(body: &Value) -> bool {
    match body.get("tool_choice") {
        Some(Value::String(choice)) => choice.eq_ignore_ascii_case("required"),
        Some(Value::Object(choice)) => choice
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .is_some_and(|name| name == "fusion"),
        _ => false,
    }
}

fn anthropic_requires_tool(body: &Value) -> bool {
    let Some(choice) = body.get("tool_choice") else {
        return false;
    };
    matches!(choice.get("type").and_then(Value::as_str), Some("any"))
        || (matches!(choice.get("type").and_then(Value::as_str), Some("tool"))
            && choice.get("name").and_then(Value::as_str) == Some("fusion"))
}

#[allow(clippy::too_many_arguments)]
async fn run_on_demand_with_messages(
    client: &Client,
    store: &Arc<RequestLogStore>,
    config: &AppConfig,
    input_protocol: &str,
    messages: Vec<Value>,
    overrides: Option<FusionModelOverride>,
    max_tokens: Option<u64>,
    require_tool: bool,
) -> Result<CompletedOnDemandRun, FusionError> {
    if !config.fusion.enabled {
        return Err(FusionError::bad_request("Fusion is disabled in settings"));
    }
    let outer_ref = config
        .fusion
        .outer_model
        .clone()
        .ok_or_else(|| FusionError::bad_request("Fusion outer model is not configured"))?;
    if is_fusion_model(&outer_ref.model) {
        return Err(FusionError::bad_request(
            "Fusion outer model cannot be nexus/fusion",
        ));
    }
    let outer = resolve_model_ref(config, outer_ref).map_err(FusionError::bad_request)?;
    reject_non_text_messages(&messages).map_err(FusionError::bad_request)?;

    let outer_messages = with_system_message(
        &messages,
        "You are the outer model for API Nexus Fusion. Answer the user directly. Call the fusion tool when independent panel analysis would materially improve correctness, breadth, or confidence. If you call it, use its analysis as evidence and then write the final answer yourself. Do not mention hidden orchestration.",
    );
    let executor = FusionToolExecutor {
        client: client.clone(),
        store: store.clone(),
        config: config.clone(),
        input_protocol: input_protocol.to_string(),
        messages,
        overrides,
        usage: tokio::sync::Mutex::new(TokenUsage::default()),
    };
    let spec = fusion_tool_spec();
    let max_tokens = max_tokens.unwrap_or(DEFAULT_STAGE_MAX_TOKENS);
    let (system, protocol_messages) = messages_for_provider(&outer, outer_messages, max_tokens)
        .map_err(FusionError::bad_request)?;
    let (final_content, mut usage) = agentic::run_tool_loop_with_required_tool(
        client,
        &outer.provider,
        &outer.model_ref.model,
        system.as_deref(),
        protocol_messages,
        max_tokens,
        &[spec],
        &executor,
        1,
        require_tool,
    )
    .await
    .map_err(FusionError::upstream)?;
    add_token_usage(&mut usage, *executor.usage.lock().await);
    Ok(CompletedOnDemandRun {
        usage,
        final_content,
    })
}

fn fusion_tool_spec() -> ToolSpec {
    ToolSpec {
        name: "fusion".into(),
        description: "Run multiple independent models and a judge to analyze the user's request."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "focus": {"type": "string", "description": "Optional analysis focus"}
            },
            "additionalProperties": false
        }),
    }
}

struct FusionToolExecutor {
    client: Client,
    store: Arc<RequestLogStore>,
    config: AppConfig,
    input_protocol: String,
    messages: Vec<Value>,
    overrides: Option<FusionModelOverride>,
    usage: tokio::sync::Mutex<TokenUsage>,
}

#[async_trait::async_trait]
impl ToolExecutor for FusionToolExecutor {
    async fn execute(&self, call: &ToolCall) -> Result<String, String> {
        if call.name != "fusion" {
            return Err(format!("Unknown server tool: {}", call.name));
        }
        let mut messages = self.messages.clone();
        if let Some(focus) = call
            .arguments
            .get("focus")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|focus| !focus.is_empty())
        {
            messages.push(json!({
                "role": "user",
                "content": format!("Additional analysis focus requested by the outer model: {focus}")
            }));
        }
        let run = run_with_messages(
            &self.client,
            &self.store,
            &self.config,
            self.input_protocol.clone(),
            None,
            messages,
            self.overrides.clone(),
            None,
            false,
        )
        .await
        .map_err(|error| error.to_string())?;
        add_token_usage(&mut *self.usage.lock().await, run.usage);
        Ok(run.final_content)
    }
}

fn add_token_usage(total: &mut TokenUsage, usage: TokenUsage) {
    total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
    total.cached_tokens = total.cached_tokens.saturating_add(usage.cached_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(usage.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(usage.cache_write_tokens);
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
    include_final: bool,
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
        include_final,
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
    include_final: bool,
) -> Result<String, String> {
    let web_tools = if config.fusion.enable_web_tools {
        config
            .fusion
            .web_search_daemon_url
            .as_deref()
            .map(|daemon_url| {
                WebTools::new(
                    client.clone(),
                    daemon_url,
                    config.fusion.web_search_limit,
                    config.fusion.web_fetch_max_chars,
                )
                .map(Arc::new)
            })
            .transpose()?
    } else {
        None
    };
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
                web_tools.clone(),
                config.fusion.max_tool_calls,
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
        web_tools,
        config.fusion.max_tool_calls,
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

    if !include_final {
        return Ok(judge_content);
    }

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
        None,
        0,
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

#[allow(clippy::too_many_arguments)]
async fn call_model(
    client: Client,
    config: AppConfig,
    target: ResolvedModel,
    messages: Vec<Value>,
    timeout_secs: u64,
    max_tokens: u64,
    web_tools: Option<Arc<WebTools>>,
    max_tool_calls: u32,
) -> ModelCallOutcome {
    let started = Instant::now();
    let result = timeout(
        Duration::from_secs(timeout_secs),
        call_model_inner(
            &client,
            &target,
            messages,
            max_tokens,
            web_tools.as_deref(),
            max_tool_calls,
        ),
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
    web_tools: Option<&WebTools>,
    max_tool_calls: u32,
) -> Result<(String, TokenUsage), String> {
    let tools = web_tools.map(WebTools::specs).unwrap_or_default();
    let executor: &dyn ToolExecutor = web_tools
        .map(|tools| tools as &dyn ToolExecutor)
        .unwrap_or(&NO_TOOLS_EXECUTOR);
    let effective_max_tool_calls = if tools.is_empty() { 0 } else { max_tool_calls };

    let (system, messages) = messages_for_provider(target, messages, max_tokens)?;

    agentic::run_tool_loop(
        client,
        &target.provider,
        &target.model_ref.model,
        system.as_deref(),
        messages,
        max_tokens,
        &tools,
        executor,
        effective_max_tool_calls,
    )
    .await
}

fn messages_for_provider(
    target: &ResolvedModel,
    messages: Vec<Value>,
    max_tokens: u64,
) -> Result<(Option<String>, Vec<Value>), String> {
    if proxy::is_anthropic_provider(&target.provider) {
        let openai_body = json!({
            "model": target.model_ref.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": false
        });
        let converted = proxy::openai_to_anthropic_request(&openai_body)?;
        let system = converted
            .get("system")
            .and_then(Value::as_str)
            .map(str::to_string);
        let messages = converted
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok((system, messages))
    } else {
        Ok((None, messages))
    }
}

struct NoToolsExecutor;

#[async_trait::async_trait]
impl ToolExecutor for NoToolsExecutor {
    async fn execute(&self, _call: &ToolCall) -> Result<String, String> {
        Err("No tools configured".to_string())
    }
}

static NO_TOOLS_EXECUTOR: NoToolsExecutor = NoToolsExecutor;

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
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    #[derive(Clone, Default)]
    struct WebIntegrationState {
        calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    async fn tool_calling_model(
        State(state): State<WebIntegrationState>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        state.requests.lock().unwrap().push(body);
        if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Json(json!({
                "choices": [{"message": {"role": "assistant", "content": null,
                    "tool_calls": [{"id": "search_1", "type": "function", "function": {
                        "name": "web_search", "arguments": "{\"query\":\"current docs\"}"
                    }}]
                }}]
            }))
        } else {
            Json(json!({
                "choices": [{"message": {"role": "assistant", "content": "answer with sources"}}]
            }))
        }
    }

    async fn search_daemon(Json(_body): Json<Value>) -> Json<Value> {
        Json(json!({
            "status": "ok",
            "data": {"query": "current docs", "results": [{
                "title": "Documentation", "url": "https://docs.example", "description": "Current reference"
            }]},
            "error": null
        }))
    }

    async fn spawn_test_router(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

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
    fn on_demand_recognizes_required_tool_choices() {
        assert!(openai_requires_tool(&json!({"tool_choice": "required"})));
        assert!(openai_requires_tool(&json!({
            "tool_choice": {"type": "function", "function": {"name": "fusion"}}
        })));
        assert!(!openai_requires_tool(&json!({"tool_choice": "auto"})));
        assert!(anthropic_requires_tool(
            &json!({"tool_choice": {"type": "any"}})
        ));
        assert!(anthropic_requires_tool(&json!({
            "tool_choice": {"type": "tool", "name": "fusion"}
        })));
        assert!(!anthropic_requires_tool(
            &json!({"tool_choice": {"type": "auto"}})
        ));
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

    #[tokio::test]
    async fn call_model_inner_routes_web_search_results_back_to_panel_model() {
        let state = WebIntegrationState::default();
        let model_url = spawn_test_router(
            Router::new()
                .route("/v1/chat/completions", post(tool_calling_model))
                .with_state(state.clone()),
        )
        .await;
        let daemon_url =
            spawn_test_router(Router::new().route("/search", post(search_daemon))).await;
        let client = reqwest::Client::new();
        let web_tools = WebTools::new(client.clone(), &daemon_url, 5, 30_000).unwrap();
        let target = ResolvedModel {
            model_ref: ModelRef {
                provider_id: "panel".into(),
                model: "panel-model".into(),
            },
            provider: Provider {
                id: "panel".into(),
                name: "Panel".into(),
                base_url: model_url,
                api_key: "secret".into(),
                models: vec!["panel-model".into()],
                enabled: true,
                ..Default::default()
            },
        };

        let (content, _) = call_model_inner(
            &client,
            &target,
            vec![json!({"role": "user", "content": "question"})],
            512,
            Some(&web_tools),
            4,
        )
        .await
        .unwrap();

        assert_eq!(content, "answer with sources");
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tools"][0]["function"]["name"], "web_search");
        assert!(requests[1]["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("https://docs.example"));
    }
}
