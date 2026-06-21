//! Protocol-agnostic agentic tool-calling loop for Fusion.
//!
//! Built incrementally across Plan 1 (Tasks 1-6): types and helpers land with
//! their unit tests before a production consumer exists. The module-level
//! `dead_code` allow is removed in Task 6, once `run_tool_loop` is wired into
//! `fusion::call_model` and every item has a real caller.
use serde_json::{json, Value};

use crate::config::Provider;
use crate::proxy::{self, TokenUsage};

/// A tool definition advertised to a model, serialized per protocol.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolSpec {
    pub fn to_openai_tool(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.input_schema,
            }
        })
    }

    pub fn to_anthropic_tool(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "input_schema": self.input_schema,
        })
    }
}

/// A single tool invocation parsed out of a model response.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Executes a tool call and returns its textual result.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> Result<String, String>;
}

#[derive(Clone, Debug)]
pub enum ToolLoopOutcome {
    Text {
        content: String,
        usage: TokenUsage,
    },
    ClientToolCalls {
        text: Option<String>,
        calls: Vec<ToolCall>,
        usage: TokenUsage,
    },
}

/// Extract assistant text and tool calls from an OpenAI chat-completions
/// response (`choices[0].message.{content,tool_calls}`).
pub fn parse_openai_tool_calls(response: &Value) -> (Option<String>, Vec<ToolCall>) {
    let message = response
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"));
    let text = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .filter(|content| !content.is_empty())
        .map(str::to_string);
    let calls = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    let function = call.get("function")?;
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    Some(ToolCall {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: function.get("name").and_then(Value::as_str)?.to_string(),
                        arguments: serde_json::from_str(arguments).unwrap_or_else(|_| json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (text, calls)
}

/// Extract assistant text and tool calls from an Anthropic messages response
/// (`content[]` blocks: `text` and `tool_use{id,name,input}`).
pub fn parse_anthropic_tool_calls(response: &Value) -> (Option<String>, Vec<ToolCall>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    if let Some(blocks) = response.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                        text.push_str(chunk);
                    }
                }
                Some("tool_use") => calls.push(ToolCall {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                }),
                _ => {}
            }
        }
    }
    ((!text.is_empty()).then_some(text), calls)
}

/// Build an OpenAI chat-completions request with optional function tools.
pub fn openai_request_body(
    model: &str,
    messages: &[Value],
    max_tokens: u64,
    tools: &[ToolSpec],
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": false,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(ToolSpec::to_openai_tool).collect());
    }
    body
}

/// Build an Anthropic messages request with optional native tools.
pub fn anthropic_request_body(
    model: &str,
    system: Option<&str>,
    messages: &[Value],
    max_tokens: u64,
    tools: &[ToolSpec],
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": false,
    });
    if let Some(system) = system.filter(|value| !value.is_empty()) {
        body["system"] = Value::String(system.to_string());
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(ToolSpec::to_anthropic_tool).collect());
    }
    body
}

/// Echo the OpenAI assistant message and append one tool result per call.
pub fn openai_followup_messages(assistant: &Value, results: &[(ToolCall, String)]) -> Vec<Value> {
    let mut messages = Vec::with_capacity(results.len() + 1);
    messages.push(assistant.clone());
    messages.extend(results.iter().map(|(call, result)| {
        json!({
            "role": "tool",
            "tool_call_id": call.id,
            "content": result,
        })
    }));
    messages
}

/// Reconstruct an Anthropic assistant tool-use turn and its user tool results.
pub fn anthropic_followup_messages(results: &[(ToolCall, String)]) -> Vec<Value> {
    let tool_uses = results
        .iter()
        .map(|(call, _)| {
            json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.arguments,
            })
        })
        .collect::<Vec<_>>();
    let tool_results = results
        .iter()
        .map(|(call, result)| {
            json!({
                "type": "tool_result",
                "tool_use_id": call.id,
                "content": result,
            })
        })
        .collect::<Vec<_>>();
    vec![
        json!({"role": "assistant", "content": tool_uses}),
        json!({"role": "user", "content": tool_results}),
    ]
}

/// Drive one provider until it returns text without another tool request.
#[allow(clippy::too_many_arguments)]
pub async fn run_tool_loop(
    client: &reqwest::Client,
    provider: &Provider,
    model: &str,
    system: Option<&str>,
    messages: Vec<Value>,
    max_tokens: u64,
    tools: &[ToolSpec],
    executor: &dyn ToolExecutor,
    max_tool_calls: u32,
) -> Result<(String, TokenUsage), String> {
    run_tool_loop_with_required_tool(
        client,
        provider,
        model,
        system,
        messages,
        max_tokens,
        tools,
        executor,
        max_tool_calls,
        false,
    )
    .await
}

/// Variant used by on-demand Fusion when the caller requires a server tool.
#[allow(clippy::too_many_arguments)]
pub async fn run_tool_loop_with_required_tool(
    client: &reqwest::Client,
    provider: &Provider,
    model: &str,
    system: Option<&str>,
    messages: Vec<Value>,
    max_tokens: u64,
    tools: &[ToolSpec],
    executor: &dyn ToolExecutor,
    max_tool_calls: u32,
    require_tool: bool,
) -> Result<(String, TokenUsage), String> {
    match run_mixed_tool_loop(
        client,
        provider,
        model,
        system,
        messages,
        max_tokens,
        tools,
        executor,
        max_tool_calls,
        require_tool,
        &[],
    )
    .await?
    {
        ToolLoopOutcome::Text { content, usage } => Ok((content, usage)),
        ToolLoopOutcome::ClientToolCalls { .. } => {
            Err("unexpected client tool call in server-only loop".to_string())
        }
    }
}

/// Drive server tools internally while returning client-owned calls to the caller.
#[allow(clippy::too_many_arguments)]
pub async fn run_mixed_tool_loop(
    client: &reqwest::Client,
    provider: &Provider,
    model: &str,
    system: Option<&str>,
    mut messages: Vec<Value>,
    max_tokens: u64,
    tools: &[ToolSpec],
    executor: &dyn ToolExecutor,
    max_tool_calls: u32,
    require_tool: bool,
    client_tool_names: &[String],
) -> Result<ToolLoopOutcome, String> {
    let anthropic = proxy::is_anthropic_provider(provider);
    let mut usage = TokenUsage::default();
    let mut executed_calls = 0_u32;
    let mut best_text = None;

    loop {
        let mut request_body = if anthropic {
            anthropic_request_body(model, system, &messages, max_tokens, tools)
        } else {
            openai_request_body(model, &messages, max_tokens, tools)
        };
        if !anthropic && should_disable_thinking(provider, model) {
            request_body["thinking"] = json!({"type": "disabled"});
        }
        if !anthropic && !tools.is_empty() {
            request_body["parallel_tool_calls"] = Value::Bool(false);
        }
        if require_tool && executed_calls == 0 && !tools.is_empty() {
            request_body["tool_choice"] = if anthropic {
                json!({"type": "any"})
            } else {
                Value::String("required".to_string())
            };
        }
        let url = if anthropic {
            proxy::anthropic_upstream_url(&provider.base_url, "/v1/messages")
        } else {
            proxy::openai_upstream_url(&provider.base_url, "/v1/chat/completions")
        };
        let mut request = client.post(url).header("content-type", "application/json");
        request = if anthropic {
            request
                .header("x-api-key", &provider.api_key)
                .header("anthropic-version", "2023-06-01")
        } else {
            request.header("authorization", format!("Bearer {}", provider.api_key))
        };
        let response = request
            .json(&request_body)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let response_text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("HTTP {} - {}", status.as_u16(), response_text));
        }
        let response_body: Value = serde_json::from_str(&response_text).map_err(|error| {
            let protocol = if anthropic { "Anthropic" } else { "OpenAI" };
            format!("Failed to parse {protocol} response: {error}")
        })?;
        add_usage(&mut usage, proxy::extract_token_usage(&response_body));

        let (text, calls) = if anthropic {
            parse_anthropic_tool_calls(&response_body)
        } else {
            parse_openai_tool_calls(&response_body)
        };
        if text.is_some() {
            best_text = text;
        }
        if calls.is_empty() {
            return best_text
                .filter(|content| !content.trim().is_empty())
                .map(|content| ToolLoopOutcome::Text { content, usage })
                .ok_or_else(|| "empty model response".to_string());
        }

        let client_calls = calls
            .iter()
            .filter(|call| client_tool_names.iter().any(|name| name == &call.name))
            .cloned()
            .collect::<Vec<_>>();
        if !client_calls.is_empty() {
            if client_calls.len() != calls.len() {
                return Err(
                    "model returned server and client tool calls in one parallel batch".to_string(),
                );
            }
            return Ok(ToolLoopOutcome::ClientToolCalls {
                text: best_text,
                calls: client_calls,
                usage,
            });
        }

        let call_count = u32::try_from(calls.len()).unwrap_or(u32::MAX);
        if tools.is_empty()
            || executed_calls.saturating_add(call_count) > max_tool_calls
            || max_tool_calls == 0
        {
            return best_text
                .filter(|content| !content.trim().is_empty())
                .map(|content| ToolLoopOutcome::Text { content, usage })
                .ok_or_else(|| "exceeded max_tool_calls".to_string());
        }

        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            let result = executor
                .execute(&call)
                .await
                .unwrap_or_else(|error| format!("Tool error: {error}"));
            results.push((call, result));
        }
        executed_calls = executed_calls.saturating_add(call_count);

        if anthropic {
            messages.extend(anthropic_followup_messages(&results));
        } else {
            let assistant = response_body
                .get("choices")
                .and_then(|choices| choices.get(0))
                .and_then(|choice| choice.get("message"))
                .cloned()
                .ok_or_else(|| "OpenAI tool response is missing assistant message".to_string())?;
            messages.extend(openai_followup_messages(&assistant, &results));
        }
    }
}

fn should_disable_thinking(provider: &Provider, model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    if !model.starts_with("deepseek-v") {
        return false;
    }

    provider.name.eq_ignore_ascii_case("deepseek")
        || provider.id.eq_ignore_ascii_case("deepseek")
        || provider
            .base_url
            .to_ascii_lowercase()
            .contains("deepseek.com")
}

fn add_usage(total: &mut TokenUsage, round: TokenUsage) {
    total.input_tokens = total.input_tokens.saturating_add(round.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(round.output_tokens);
    total.cached_tokens = total.cached_tokens.saturating_add(round.cached_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(round.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(round.cache_write_tokens);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, routing::post, Json, Router};
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    #[test]
    fn tool_spec_serializes_to_openai_function_tool() {
        let spec = ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a URL".into(),
            input_schema: json!({"type":"object","properties":{"url":{"type":"string"}}}),
        };
        assert_eq!(
            spec.to_openai_tool(),
            json!({
                "type": "function",
                "function": {
                    "name": "web_fetch",
                    "description": "Fetch a URL",
                    "parameters": {"type":"object","properties":{"url":{"type":"string"}}}
                }
            })
        );
    }

    #[test]
    fn tool_spec_serializes_to_anthropic_tool() {
        let spec = ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a URL".into(),
            input_schema: json!({"type":"object","properties":{"url":{"type":"string"}}}),
        };
        assert_eq!(
            spec.to_anthropic_tool(),
            json!({
                "name": "web_fetch",
                "description": "Fetch a URL",
                "input_schema": {"type":"object","properties":{"url":{"type":"string"}}}
            })
        );
    }

    #[test]
    fn parses_openai_tool_calls() {
        let resp = json!({"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"call_1","type":"function","function":{"name":"web_fetch","arguments":"{\"url\":\"https://e.com\"}"}}]}}]});
        let (text, calls) = parse_openai_tool_calls(&resp);
        assert!(text.is_none());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "web_fetch");
        assert_eq!(calls[0].arguments, json!({"url":"https://e.com"}));
    }

    #[test]
    fn parses_openai_text_without_tool_calls() {
        let resp = json!({"choices":[{"message":{"content":"hello","tool_calls":null}}]});
        let (text, calls) = parse_openai_tool_calls(&resp);
        assert_eq!(text.as_deref(), Some("hello"));
        assert!(calls.is_empty());
    }

    #[test]
    fn parses_anthropic_tool_use() {
        let resp = json!({"content":[
            {"type":"text","text":"Let me check."},
            {"type":"tool_use","id":"tu_1","name":"web_fetch","input":{"url":"https://e.com"}}]});
        let (text, calls) = parse_anthropic_tool_calls(&resp);
        assert_eq!(text.as_deref(), Some("Let me check."));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "tu_1");
        assert_eq!(calls[0].arguments, json!({"url":"https://e.com"}));
    }

    fn fetch_spec() -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a URL".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"url": {"type": "string"}},
                "required": ["url"]
            }),
        }
    }

    #[test]
    fn request_bodies_inject_protocol_specific_tools() {
        let messages = vec![json!({"role": "user", "content": "hello"})];
        let spec = fetch_spec();
        let openai = openai_request_body("gpt", &messages, 123, std::slice::from_ref(&spec));
        assert_eq!(openai["tools"][0]["type"], "function");
        assert_eq!(openai["tools"][0]["function"]["name"], "web_fetch");
        assert_eq!(openai["messages"], json!(messages));

        let anthropic = anthropic_request_body(
            "claude",
            Some("be useful"),
            &messages,
            123,
            std::slice::from_ref(&spec),
        );
        assert_eq!(anthropic["tools"][0]["name"], "web_fetch");
        assert_eq!(anthropic["system"], "be useful");

        assert!(openai_request_body("gpt", &messages, 123, &[])
            .get("tools")
            .is_none());
        assert!(anthropic_request_body("claude", None, &messages, 123, &[])
            .get("tools")
            .is_none());
    }

    #[test]
    fn openai_followup_echoes_assistant_and_uses_tool_call_id() {
        let assistant = json!({
            "role": "assistant",
            "content": null,
            "reasoning_content": "I should fetch the page.",
            "tool_calls": [{"id": "call_1", "type": "function", "function": {
                "name": "web_fetch", "arguments": "{\"url\":\"https://e.com\"}"
            }}]
        });
        let call = ToolCall {
            id: "call_1".into(),
            name: "web_fetch".into(),
            arguments: json!({"url": "https://e.com"}),
        };
        let messages = openai_followup_messages(&assistant, &[(call, "page".into())]);
        assert_eq!(messages[0], assistant);
        assert_eq!(messages[0]["reasoning_content"], "I should fetch the page.");
        assert_eq!(
            messages[1],
            json!({
                "role": "tool", "tool_call_id": "call_1", "content": "page"
            })
        );
    }

    #[test]
    fn anthropic_followup_uses_tool_use_id() {
        let call = ToolCall {
            id: "tu_1".into(),
            name: "web_fetch".into(),
            arguments: json!({"url": "https://e.com"}),
        };
        let messages = anthropic_followup_messages(&[(call, "page".into())]);
        assert_eq!(messages[0]["content"][0]["type"], "tool_use");
        assert_eq!(
            messages[0]["content"][0]["input"],
            json!({"url": "https://e.com"})
        );
        assert_eq!(
            messages[1]["content"][0],
            json!({
                "type": "tool_result", "tool_use_id": "tu_1", "content": "page"
            })
        );
    }

    #[derive(Clone)]
    struct ModelServerState {
        responses: Arc<Vec<Value>>,
        request_index: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    async fn model_response(
        State(state): State<ModelServerState>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        state.requests.lock().unwrap().push(request);
        let index = state.request_index.fetch_add(1, Ordering::SeqCst);
        Json(
            state
                .responses
                .get(index)
                .cloned()
                .unwrap_or_else(|| json!({"error": "unexpected request"})),
        )
    }

    async fn spawn_model_server(responses: Vec<Value>) -> (String, ModelServerState) {
        let state = ModelServerState {
            responses: Arc::new(responses),
            request_index: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(model_response))
            .route("/v1/messages", post(model_response))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), state)
    }

    #[derive(Default)]
    struct RecordingExecutor {
        calls: Mutex<Vec<ToolCall>>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for RecordingExecutor {
        async fn execute(&self, call: &ToolCall) -> Result<String, String> {
            self.calls.lock().unwrap().push(call.clone());
            Ok("fetched page".into())
        }
    }

    fn provider(base_url: String, protocol: &str) -> Provider {
        Provider {
            id: "test".into(),
            name: "test".into(),
            protocol: protocol.into(),
            base_url,
            api_key: "secret".into(),
            models: vec!["model".into()],
            enabled: true,
            priority: 0,
        }
    }

    #[tokio::test]
    async fn tool_loop_returns_plain_completion_and_usage() {
        let (base_url, state) = spawn_model_server(vec![json!({
            "choices": [{"message": {"role": "assistant", "content": "answer"}}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 3}
        })])
        .await;
        let executor = RecordingExecutor::default();
        let result = run_tool_loop(
            &reqwest::Client::new(),
            &provider(base_url, "openai"),
            "model",
            None,
            vec![json!({"role": "user", "content": "question"})],
            100,
            &[],
            &executor,
            0,
        )
        .await
        .unwrap();

        assert_eq!(result.0, "answer");
        assert_eq!(result.1.input_tokens, 2);
        assert_eq!(result.1.output_tokens, 3);
        assert_eq!(state.request_index.load(Ordering::SeqCst), 1);
        assert!(executor.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tool_loop_executes_openai_call_and_appends_result() {
        let (base_url, state) = spawn_model_server(vec![
            json!({
                "choices": [{"message": {"role": "assistant", "content": null,
                    "tool_calls": [{"id": "call_1", "type": "function", "function": {
                        "name": "web_fetch", "arguments": "{\"url\":\"https://e.com\"}"
                    }}]
                }}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 1}
            }),
            json!({
                "choices": [{"message": {"role": "assistant", "content": "final answer"}}],
                "usage": {"prompt_tokens": 4, "completion_tokens": 3}
            }),
        ])
        .await;
        let executor = RecordingExecutor::default();
        let result = run_tool_loop(
            &reqwest::Client::new(),
            &provider(base_url, "openai"),
            "model",
            None,
            vec![json!({"role": "user", "content": "question"})],
            100,
            &[fetch_spec()],
            &executor,
            2,
        )
        .await
        .unwrap();

        assert_eq!(result.0, "final answer");
        assert_eq!(result.1.input_tokens, 6);
        assert_eq!(result.1.output_tokens, 4);
        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({"url": "https://e.com"}));
        drop(calls);
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1]["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(requests[1]["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(requests[1]["messages"][2]["content"], "fetched page");
    }

    #[tokio::test]
    async fn mixed_loop_returns_client_tool_without_executing_it() {
        let (base_url, _) = spawn_model_server(vec![json!({
            "choices": [{"message": {"role": "assistant", "content": null,
                "tool_calls": [{"id": "call_shell", "type": "function", "function": {
                    "name": "shell_command", "arguments": "{\"command\":\"rg --files\"}"
                }}]
            }}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1}
        })])
        .await;
        let executor = RecordingExecutor::default();
        let client_tool = ToolSpec {
            name: "shell_command".into(),
            description: "Run a command".into(),
            input_schema: json!({"type": "object"}),
        };
        let outcome = run_mixed_tool_loop(
            &reqwest::Client::new(),
            &provider(base_url, "openai"),
            "model",
            None,
            vec![json!({"role": "user", "content": "inspect"})],
            100,
            &[client_tool],
            &executor,
            0,
            false,
            &["shell_command".to_string()],
        )
        .await
        .unwrap();

        match outcome {
            ToolLoopOutcome::ClientToolCalls { calls, usage, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "shell_command");
                assert_eq!(calls[0].arguments, json!({"command": "rg --files"}));
                assert_eq!(usage.input_tokens, 2);
            }
            ToolLoopOutcome::Text { .. } => panic!("expected a client tool call"),
        }
        assert!(executor.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tool_loop_executes_anthropic_call_and_appends_result() {
        let (base_url, state) = spawn_model_server(vec![
            json!({
                "content": [{"type": "tool_use", "id": "tu_1", "name": "web_fetch",
                    "input": {"url": "https://e.com"}}],
                "usage": {"input_tokens": 2, "output_tokens": 1}
            }),
            json!({
                "content": [{"type": "text", "text": "anthropic answer"}],
                "usage": {"input_tokens": 4, "output_tokens": 3}
            }),
        ])
        .await;
        let executor = RecordingExecutor::default();
        let result = run_tool_loop(
            &reqwest::Client::new(),
            &provider(base_url, "anthropic"),
            "model",
            Some("system prompt"),
            vec![json!({"role": "user", "content": "question"})],
            100,
            &[fetch_spec()],
            &executor,
            2,
        )
        .await
        .unwrap();

        assert_eq!(result.0, "anthropic answer");
        assert_eq!(result.1.input_tokens, 6);
        assert_eq!(result.1.output_tokens, 4);
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests[0]["system"], "system prompt");
        assert_eq!(requests[0]["tools"][0]["name"], "web_fetch");
        assert_eq!(requests[1]["messages"][1]["content"][0]["id"], "tu_1");
        assert_eq!(
            requests[1]["messages"][2]["content"][0]["tool_use_id"],
            "tu_1"
        );
    }

    #[tokio::test]
    async fn tool_loop_with_no_tools_never_invokes_executor() {
        let (base_url, _) = spawn_model_server(vec![json!({
            "choices": [{"message": {"role": "assistant", "content": "direct"}}]
        })])
        .await;
        let executor = RecordingExecutor::default();
        let result = run_tool_loop(
            &reqwest::Client::new(),
            &provider(base_url, "openai"),
            "model",
            None,
            vec![],
            100,
            &[],
            &executor,
            0,
        )
        .await
        .unwrap();
        assert_eq!(result.0, "direct");
        assert!(executor.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn deepseek_v_models_disable_thinking_for_tool_loop() {
        let (base_url, state) = spawn_model_server(vec![json!({
            "choices": [{"message": {"role": "assistant", "content": "answer"}}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 3}
        })])
        .await;
        let executor = RecordingExecutor::default();
        let result = run_tool_loop(
            &reqwest::Client::new(),
            &Provider {
                id: "deepseek".into(),
                name: "DeepSeek".into(),
                protocol: "openai".into(),
                base_url,
                api_key: "secret".into(),
                models: vec!["deepseek-v4-flash".into()],
                enabled: true,
                priority: 0,
            },
            "deepseek-v4-flash",
            None,
            vec![json!({"role": "user", "content": "question"})],
            100,
            &[fetch_spec()],
            &executor,
            1,
        )
        .await
        .unwrap();

        assert_eq!(result.0, "answer");
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests[0]["thinking"], json!({"type": "disabled"}));
    }
}
