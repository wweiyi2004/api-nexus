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
}
