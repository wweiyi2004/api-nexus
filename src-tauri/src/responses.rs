//! OpenAI Responses protocol adapter used by Codex custom model providers.

use serde_json::{json, Value};

use crate::agentic::{ToolCall, ToolSpec};
use crate::proxy::TokenUsage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientToolKind {
    Function,
    Custom,
}

#[derive(Clone, Debug)]
pub struct ClientTool {
    pub exposed_name: String,
    pub response_name: String,
    pub namespace: Option<String>,
    pub kind: ClientToolKind,
    pub spec: ToolSpec,
}

#[derive(Clone, Debug)]
pub struct ParsedResponsesRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub client_tools: Vec<ClientTool>,
    pub stream: bool,
    pub max_output_tokens: u64,
    pub require_tool: bool,
}

pub fn parse_request(body: &Value) -> Result<ParsedResponsesRequest, String> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| "Responses request requires a model".to_string())?
        .to_string();
    let client_tools = parse_client_tools(body.get("tools"))?;
    let mut messages = Vec::new();
    if let Some(instructions) = body
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        messages.push(json!({"role": "system", "content": instructions}));
    }
    parse_input(body.get("input"), &client_tools, &mut messages)?;
    if messages.is_empty() {
        return Err("Responses request requires text input".to_string());
    }

    let require_tool = match body.get("tool_choice") {
        Some(Value::String(choice)) => choice.eq_ignore_ascii_case("required"),
        Some(Value::Object(choice)) => choice
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "required" || kind == "function" || kind == "custom"),
        _ => false,
    };

    Ok(ParsedResponsesRequest {
        model,
        messages,
        client_tools,
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        max_output_tokens: body
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .unwrap_or(2048),
        require_tool,
    })
}

fn parse_input(
    input: Option<&Value>,
    tools: &[ClientTool],
    messages: &mut Vec<Value>,
) -> Result<(), String> {
    match input {
        Some(Value::String(text)) => {
            messages.push(json!({"role": "user", "content": text}));
            Ok(())
        }
        Some(Value::Array(items)) => {
            for item in items {
                parse_input_item(item, tools, messages)?;
            }
            Ok(())
        }
        Some(Value::Null) | None => Ok(()),
        Some(_) => Err("Responses input must be a string or item array".to_string()),
    }
}

fn parse_input_item(
    item: &Value,
    tools: &[ClientTool],
    messages: &mut Vec<Value>,
) -> Result<(), String> {
    match item.get("type").and_then(Value::as_str) {
        Some("message") | None if item.get("role").is_some() => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let text = response_content_text(item.get("content").unwrap_or(&Value::Null))?;
            if !text.is_empty() {
                messages.push(json!({"role": role, "content": text}));
            }
            Ok(())
        }
        Some("function_call") | Some("custom_tool_call") => {
            let call_id = required_item_string(item, "call_id")?;
            let response_name = required_item_string(item, "name")?;
            let namespace = item.get("namespace").and_then(Value::as_str);
            let exposed_name = find_exposed_name(tools, response_name, namespace)
                .unwrap_or_else(|| response_name.to_string());
            let arguments = if item.get("type").and_then(Value::as_str) == Some("custom_tool_call")
            {
                json!({"input": item.get("input").and_then(Value::as_str).unwrap_or_default()})
                    .to_string()
            } else {
                item.get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}")
                    .to_string()
            };
            messages.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {"name": exposed_name, "arguments": arguments}
                }]
            }));
            Ok(())
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            let call_id = required_item_string(item, "call_id")?;
            let output = response_content_text(item.get("output").unwrap_or(&Value::Null))?;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output
            }));
            Ok(())
        }
        Some("reasoning") => Ok(()),
        Some(kind) => Err(format!("Unsupported Responses input item: {kind}")),
        None => Err("Responses input item is missing type".to_string()),
    }
}

fn response_content_text(content: &Value) -> Result<String, String> {
    match content {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => {
            let mut text = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("input_text") | Some("output_text") | Some("text") => {
                        if let Some(value) = part.get("text").and_then(Value::as_str) {
                            text.push(value.to_string());
                        }
                    }
                    Some("input_image") | Some("input_file") => {
                        return Err(
                            "Fusion Responses currently supports text input only".to_string()
                        )
                    }
                    Some(_) | None => {}
                }
            }
            Ok(text.join("\n"))
        }
        other => Ok(other.to_string()),
    }
}

fn required_item_string<'a>(item: &'a Value, field: &str) -> Result<&'a str, String> {
    item.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Responses item requires {field}"))
}

fn parse_client_tools(value: Option<&Value>) -> Result<Vec<ClientTool>, String> {
    let Some(tools) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut parsed = Vec::new();
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => parsed.push(parse_function_tool(tool, None)?),
            Some("custom") => parsed.push(parse_custom_tool(tool, None)?),
            Some("namespace") => {
                let namespace = required_item_string(tool, "name")?;
                if let Some(children) = tool.get("tools").and_then(Value::as_array) {
                    for child in children {
                        match child.get("type").and_then(Value::as_str) {
                            Some("function") => {
                                parsed.push(parse_function_tool(child, Some(namespace))?)
                            }
                            Some("custom") => {
                                parsed.push(parse_custom_tool(child, Some(namespace))?)
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Hosted OpenAI tools cannot be executed by a generic inner provider.
            Some(_) | None => {}
        }
    }
    Ok(parsed)
}

fn parse_function_tool(tool: &Value, namespace: Option<&str>) -> Result<ClientTool, String> {
    let response_name = required_item_string(tool, "name")?;
    let exposed_name = exposed_tool_name(namespace, response_name);
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input_schema = tool
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    Ok(ClientTool {
        exposed_name: exposed_name.clone(),
        response_name: response_name.to_string(),
        namespace: namespace.map(str::to_string),
        kind: ClientToolKind::Function,
        spec: ToolSpec {
            name: exposed_name,
            description,
            input_schema,
        },
    })
}

fn parse_custom_tool(tool: &Value, namespace: Option<&str>) -> Result<ClientTool, String> {
    let response_name = required_item_string(tool, "name")?;
    let exposed_name = exposed_tool_name(namespace, response_name);
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(ClientTool {
        exposed_name: exposed_name.clone(),
        response_name: response_name.to_string(),
        namespace: namespace.map(str::to_string),
        kind: ClientToolKind::Custom,
        spec: ToolSpec {
            name: exposed_name,
            description,
            input_schema: json!({
                "type": "object",
                "properties": {"input": {"type": "string"}},
                "required": ["input"],
                "additionalProperties": false
            }),
        },
    })
}

fn exposed_tool_name(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) => format!("{namespace}__{name}"),
        None => name.to_string(),
    }
}

fn find_exposed_name(
    tools: &[ClientTool],
    response_name: &str,
    namespace: Option<&str>,
) -> Option<String> {
    tools
        .iter()
        .find(|tool| tool.response_name == response_name && tool.namespace.as_deref() == namespace)
        .map(|tool| tool.exposed_name.clone())
}

pub fn client_tool_output_item(call: &ToolCall, tools: &[ClientTool]) -> Result<Value, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.exposed_name == call.name)
        .ok_or_else(|| format!("Unknown client tool returned by model: {}", call.name))?;
    let mut item = match tool.kind {
        ClientToolKind::Function => json!({
            "id": format!("fc_{}", call.id),
            "type": "function_call",
            "status": "completed",
            "call_id": call.id,
            "name": tool.response_name,
            "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string())
        }),
        ClientToolKind::Custom => json!({
            "id": format!("ctc_{}", call.id),
            "type": "custom_tool_call",
            "status": "completed",
            "call_id": call.id,
            "name": tool.response_name,
            "input": call.arguments.get("input").and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| call.arguments.to_string())
        }),
    };
    if let Some(namespace) = &tool.namespace {
        item["namespace"] = Value::String(namespace.clone());
    }
    Ok(item)
}

pub fn completed_response(
    model: &str,
    text: Option<&str>,
    tool_calls: &[ToolCall],
    client_tools: &[ClientTool],
    usage: TokenUsage,
) -> Result<Value, String> {
    let response_id = format!("resp_fusion_{}", uuid::Uuid::new_v4().simple());
    let mut output = Vec::new();
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        output.push(json!({
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text, "annotations": []}]
        }));
    }
    for call in tool_calls {
        output.push(client_tool_output_item(call, client_tools)?);
    }
    if output.is_empty() {
        return Err("Responses turn produced no output".to_string());
    }
    Ok(json!({
        "id": response_id,
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": "completed",
        "error": null,
        "incomplete_details": null,
        "instructions": null,
        "model": model,
        "output": output,
        "parallel_tool_calls": false,
        "previous_response_id": null,
        "reasoning": {"effort": null, "summary": null},
        "store": false,
        "tool_choice": "auto",
        "tools": [],
        "usage": {
            "input_tokens": usage.input_tokens,
            "input_tokens_details": {"cached_tokens": usage.cache_read_tokens},
            "output_tokens": usage.output_tokens,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": usage.input_tokens.saturating_add(usage.output_tokens)
        },
        "metadata": {}
    }))
}

pub fn error_body(message: &str, error_type: &str) -> Value {
    json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": null,
            "code": null
        }
    })
}

pub fn sse_body(response: &Value) -> Result<String, String> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Responses body is missing id".to_string())?;
    let mut events = vec![json!({
        "type": "response.created",
        "response": {"id": response_id}
    })];
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for (output_index, item) in output.iter().enumerate() {
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": item
            }));
        }
    }
    events.push(json!({"type": "response.completed", "response": response}));

    let mut body = String::new();
    for event in events {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        body.push_str("event: ");
        body.push_str(event_type);
        body.push('\n');
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_request_fixture() -> Value {
        json!({
            "model": "nexus/fusion",
            "instructions": "You are a coding agent.",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "Inspect the repository"}
                ]},
                {"type": "function_call", "call_id": "call_1", "name": "shell_command",
                    "arguments": "{\"command\":\"rg --files\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "README.md"}
            ],
            "tools": [
                {"type": "function", "name": "shell_command", "description": "Run a command",
                    "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}},
                {"type": "custom", "name": "apply_patch", "description": "Apply a patch",
                    "format": {"type": "grammar"}},
                {"type": "namespace", "name": "multi_agent_v1", "tools": [
                    {"type": "function", "name": "spawn_agent", "description": "Spawn",
                        "parameters": {"type": "object", "properties": {"message": {"type": "string"}}}}
                ]},
                {"type": "web_search", "external_web_access": false}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "stream": true
        })
    }

    #[test]
    fn parses_codex_messages_and_tool_history() {
        let request = parse_request(&codex_request_fixture()).unwrap();
        assert_eq!(request.model, "nexus/fusion");
        assert!(request.stream);
        assert_eq!(request.messages[0]["role"], "system");
        assert_eq!(request.messages[1]["content"], "Inspect the repository");
        assert_eq!(
            request.messages[2]["tool_calls"][0]["function"]["name"],
            "shell_command"
        );
        assert_eq!(request.messages[3]["tool_call_id"], "call_1");
        assert_eq!(request.messages[3]["content"], "README.md");
    }

    #[test]
    fn converts_function_custom_and_namespace_tools() {
        let request = parse_request(&codex_request_fixture()).unwrap();
        assert_eq!(request.client_tools.len(), 3);
        assert_eq!(request.client_tools[0].exposed_name, "shell_command");
        assert_eq!(request.client_tools[1].kind, ClientToolKind::Custom);
        assert_eq!(
            request.client_tools[2].exposed_name,
            "multi_agent_v1__spawn_agent"
        );
    }

    #[test]
    fn restores_responses_function_and_custom_call_items() {
        let request = parse_request(&codex_request_fixture()).unwrap();
        let function = client_tool_output_item(
            &ToolCall {
                id: "call_2".into(),
                name: "multi_agent_v1__spawn_agent".into(),
                arguments: json!({"message": "inspect"}),
            },
            &request.client_tools,
        )
        .unwrap();
        assert_eq!(function["type"], "function_call");
        assert_eq!(function["name"], "spawn_agent");
        assert_eq!(function["namespace"], "multi_agent_v1");

        let custom = client_tool_output_item(
            &ToolCall {
                id: "call_3".into(),
                name: "apply_patch".into(),
                arguments: json!({"input": "*** Begin Patch"}),
            },
            &request.client_tools,
        )
        .unwrap();
        assert_eq!(custom["type"], "custom_tool_call");
        assert_eq!(custom["input"], "*** Begin Patch");
    }

    #[test]
    fn rejects_non_text_input() {
        let body = json!({
            "model": "nexus/fusion",
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_image", "image_url": "https://example.com/a.png"}
            ]}]
        });
        assert!(parse_request(&body)
            .unwrap_err()
            .contains("text input only"));
    }

    #[test]
    fn builds_completed_text_and_tool_responses() {
        let request = parse_request(&codex_request_fixture()).unwrap();
        let body = completed_response(
            "nexus/fusion",
            Some("Checking."),
            &[ToolCall {
                id: "call_4".into(),
                name: "shell_command".into(),
                arguments: json!({"command": "cargo test"}),
            }],
            &request.client_tools,
            TokenUsage {
                input_tokens: 10,
                output_tokens: 4,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(body["object"], "response");
        assert_eq!(body["output"][0]["type"], "message");
        assert_eq!(body["output"][1]["type"], "function_call");
        assert_eq!(body["usage"]["total_tokens"], 14);
    }

    #[test]
    fn sse_stream_ends_after_completed_event_blank_line() {
        let body = completed_response(
            "nexus/fusion",
            Some("Done"),
            &[],
            &[],
            TokenUsage::default(),
        )
        .unwrap();
        let stream = sse_body(&body).unwrap();
        assert!(stream.contains("event: response.created\n"));
        assert!(stream.contains("event: response.output_item.done\n"));
        assert!(stream.contains("event: response.completed\n"));
        assert!(stream.ends_with("\n\n"));
    }
}
