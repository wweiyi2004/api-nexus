//! Anthropic Messages protocol adapter used by Claude Code Fusion clients.

use serde_json::{json, Value};

use crate::agentic::{ToolCall, ToolSpec};
use crate::proxy::{self, TokenUsage};
use crate::responses::{ClientTool, ClientToolKind};

#[derive(Clone, Debug)]
pub struct ParsedMessagesRequest {
    pub messages: Vec<Value>,
    pub client_tools: Vec<ClientTool>,
    pub max_output_tokens: u64,
    pub require_tool: bool,
}

pub fn parse_request(body: &Value) -> Result<ParsedMessagesRequest, String> {
    body.get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| "Anthropic request requires a model".to_string())?;
    let converted = proxy::anthropic_to_openai_chat_request(body)?;
    let messages = converted
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "Anthropic request requires messages".to_string())?;
    let client_tools = parse_client_tools(body.get("tools"))?;
    let require_tool = body
        .get("tool_choice")
        .and_then(Value::as_object)
        .and_then(|choice| choice.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "any" || kind == "tool");

    Ok(ParsedMessagesRequest {
        messages,
        client_tools,
        max_output_tokens: body
            .get("max_tokens")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .unwrap_or(2048),
        require_tool,
    })
}

fn parse_client_tools(value: Option<&Value>) -> Result<Vec<ClientTool>, String> {
    let Some(tools) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    tools
        .iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "Anthropic tool requires a name".to_string())?;
            Ok(ClientTool {
                exposed_name: name.to_string(),
                response_name: name.to_string(),
                namespace: None,
                kind: ClientToolKind::Function,
                spec: ToolSpec {
                    name: name.to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: tool
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                },
            })
        })
        .collect()
}

pub fn completed_message(
    model: &str,
    text: Option<&str>,
    tool_calls: &[ToolCall],
    usage: TokenUsage,
) -> Result<Value, String> {
    let mut content = Vec::new();
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        content.push(json!({"type": "text", "text": text}));
    }
    content.extend(tool_calls.iter().map(|call| {
        json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": call.arguments
        })
    }));
    if content.is_empty() {
        return Err("Anthropic Fusion turn produced no output".to_string());
    }

    Ok(json!({
        "id": format!("msg_fusion_{}", uuid::Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": if tool_calls.is_empty() { "end_turn" } else { "tool_use" },
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.input_tokens,
            "cache_creation_input_tokens": usage.cache_write_tokens,
            "cache_read_input_tokens": usage.cache_read_tokens,
            "output_tokens": usage.output_tokens
        }
    }))
}

pub fn sse_body(message: &Value) -> Result<String, String> {
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Anthropic message is missing id".to_string())?;
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "Anthropic message is missing model".to_string())?;
    let usage = message.get("usage").cloned().unwrap_or_else(|| json!({}));
    let mut frames = vec![(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": usage.get("input_tokens").and_then(Value::as_u64).unwrap_or_default(),
                    "cache_creation_input_tokens": usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or_default(),
                    "cache_read_input_tokens": usage.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or_default(),
                    "output_tokens": 0
                }
            }
        }),
    )];

    for (index, block) in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                frames.push((
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "text", "text": ""}
                    }),
                ));
                frames.push((
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {
                            "type": "text_delta",
                            "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                        }
                    }),
                ));
            }
            Some("tool_use") => {
                frames.push((
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": block.get("id").and_then(Value::as_str).unwrap_or_default(),
                            "name": block.get("name").and_then(Value::as_str).unwrap_or_default(),
                            "input": {}
                        }
                    }),
                ));
                frames.push((
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": block.get("input").cloned().unwrap_or_else(|| json!({})).to_string()
                        }
                    }),
                ));
            }
            _ => continue,
        }
        frames.push((
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));
    }

    frames.push((
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": message.get("stop_reason").and_then(Value::as_str).unwrap_or("end_turn"),
                "stop_sequence": null
            },
            "usage": {
                "output_tokens": usage.get("output_tokens").and_then(Value::as_u64).unwrap_or_default()
            }
        }),
    ));
    frames.push(("message_stop", json!({"type": "message_stop"})));

    Ok(frames
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_code_tools_and_tool_results() {
        let request = parse_request(&json!({
            "model": "nexus/fusion",
            "system": [{"type": "text", "text": "coding system"}],
            "messages": [
                {"role": "user", "content": "run it"},
                {"role": "assistant", "content": [{
                    "type": "tool_use", "id": "toolu_1", "name": "Bash",
                    "input": {"command": "Write-Output OK"}
                }]},
                {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "toolu_1", "content": "OK"
                }]}
            ],
            "tools": [{
                "name": "Bash", "description": "Run a command",
                "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}}
            }],
            "max_tokens": 32000,
            "stream": true
        }))
        .unwrap();

        assert_eq!(request.client_tools.len(), 1);
        assert_eq!(request.client_tools[0].exposed_name, "Bash");
        assert_eq!(request.max_output_tokens, 32000);
        assert!(request
            .messages
            .iter()
            .any(|message| message["role"] == "tool"));
    }

    #[test]
    fn serializes_tool_use_as_anthropic_json_and_sse() {
        let message = completed_message(
            "nexus/fusion",
            None,
            &[ToolCall {
                id: "toolu_1".into(),
                name: "Bash".into(),
                arguments: json!({"command": "Write-Output OK"}),
            }],
            TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(message["stop_reason"], "tool_use");
        assert_eq!(message["content"][0]["name"], "Bash");

        let sse = sse_body(&message).unwrap();
        assert!(sse.contains("event: message_start"));
        assert!(sse.contains("\"type\":\"input_json_delta\""));
        assert!(sse.contains("Write-Output OK"));
        assert!(sse.ends_with("\n\n"));
    }
}
