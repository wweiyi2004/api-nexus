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
}
