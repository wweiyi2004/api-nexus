//! Protocol-agnostic agentic tool-calling loop for Fusion.
//!
//! Built incrementally across Plan 1 (Tasks 1-6): types and helpers land with
//! their unit tests before a production consumer exists. The module-level
//! `dead_code` allow is removed in Task 6, once `run_tool_loop` is wired into
//! `fusion::call_model` and every item has a real caller.
#![allow(dead_code)]

use serde_json::{json, Value};

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
