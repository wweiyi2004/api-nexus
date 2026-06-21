//! Fusion web tools backed by a trusted local open-webSearch daemon.

#![allow(dead_code)]

use serde_json::{json, Value};

use crate::agentic::{ToolCall, ToolExecutor, ToolSpec};

pub struct WebTools {
    client: reqwest::Client,
    daemon_url: String,
    search_limit: u32,
    fetch_max_chars: u32,
}

impl WebTools {
    pub fn new(
        client: reqwest::Client,
        daemon_url: &str,
        search_limit: u32,
        fetch_max_chars: u32,
    ) -> Result<Self, String> {
        let daemon_url = validate_loopback_daemon_url(daemon_url)?;
        Ok(Self {
            client,
            daemon_url,
            search_limit,
            fetch_max_chars,
        })
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        vec![web_search_spec(), web_fetch_spec()]
    }

    async fn call_daemon(&self, path: &str, body: Value) -> Result<Value, String> {
        let response = self
            .client
            .post(format!("{}{}", self.daemon_url, path))
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("open-webSearch request failed: {error}"))?;
        let status = response.status();
        let response_text = response.text().await.unwrap_or_default();
        let envelope: Value = serde_json::from_str(&response_text)
            .map_err(|error| format!("Invalid open-webSearch response: {error}"))?;

        if envelope.get("status").and_then(Value::as_str) == Some("error") {
            return Err(envelope
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("open-webSearch returned an error")
                .to_string());
        }
        if !status.is_success() {
            return Err(format!(
                "open-webSearch HTTP {} - {}",
                status.as_u16(),
                response_text
            ));
        }
        if envelope.get("status").and_then(Value::as_str) != Some("ok") {
            return Err("Invalid open-webSearch response status".to_string());
        }
        envelope
            .get("data")
            .cloned()
            .filter(|data| !data.is_null())
            .ok_or_else(|| "open-webSearch response is missing data".to_string())
    }
}

#[async_trait::async_trait]
impl ToolExecutor for WebTools {
    async fn execute(&self, call: &ToolCall) -> Result<String, String> {
        match call.name.as_str() {
            "web_fetch" => {
                let url = required_string_argument(call, "url")?;
                let data = self
                    .call_daemon(
                        "/fetch-web",
                        json!({"url": url, "maxChars": self.fetch_max_chars}),
                    )
                    .await?;
                format_fetch_result(&data)
            }
            "web_search" => Err(format!(
                "web_search is not available yet (configured limit: {})",
                self.search_limit
            )),
            name => Err(format!("Unknown web tool: {name}")),
        }
    }
}

pub fn web_fetch_spec() -> ToolSpec {
    ToolSpec {
        name: "web_fetch".into(),
        description: "Fetch readable text from a public HTTP(S) URL.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Public HTTP(S) URL to fetch"}
            },
            "required": ["url"],
            "additionalProperties": false
        }),
    }
}

pub fn web_search_spec() -> ToolSpec {
    ToolSpec {
        name: "web_search".into(),
        description: "Search the public web and return relevant results.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"}
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

fn validate_loopback_daemon_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value.trim())
        .map_err(|error| format!("Invalid open-webSearch daemon URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("open-webSearch daemon URL must use HTTP or HTTPS".to_string());
    }
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if !loopback {
        return Err("open-webSearch daemon URL must use a loopback host".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("open-webSearch daemon URL cannot contain a query or fragment".to_string());
    }
    Ok(value.trim().trim_end_matches('/').to_string())
}

fn required_string_argument<'a>(call: &'a ToolCall, name: &str) -> Result<&'a str, String> {
    call.arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{} requires a non-empty {name}", call.name))
}

fn format_fetch_result(data: &Value) -> Result<String, String> {
    let content = data
        .get("content")
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| "open-webSearch fetch result has no content".to_string())?;
    let mut header = Vec::new();
    if let Some(title) = data.get("title").and_then(Value::as_str) {
        if !title.trim().is_empty() {
            header.push(format!("Title: {}", title.trim()));
        }
    }
    if let Some(url) = data.get("url").and_then(Value::as_str) {
        if !url.trim().is_empty() {
            header.push(format!("URL: {}", url.trim()));
        }
    }
    if header.is_empty() {
        Ok(content.to_string())
    } else {
        Ok(format!("{}\n\n{}", header.join("\n"), content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct DaemonState {
        requests: Arc<Mutex<Vec<(String, Value)>>>,
    }

    async fn fetch_web(State(state): State<DaemonState>, Json(body): Json<Value>) -> Json<Value> {
        state
            .requests
            .lock()
            .unwrap()
            .push(("/fetch-web".into(), body));
        Json(json!({
            "status": "ok",
            "data": {
                "url": "https://example.com",
                "title": "Example",
                "content": "Example page body"
            },
            "error": null
        }))
    }

    async fn daemon_error() -> (StatusCode, Json<Value>) {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "error",
                "data": null,
                "error": {"code": "validation_failed", "message": "bad URL"}
            })),
        )
    }

    async fn spawn_daemon() -> (String, DaemonState) {
        let state = DaemonState::default();
        let app = Router::new()
            .route("/fetch-web", post(fetch_web))
            .route("/error", post(daemon_error))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state)
    }

    #[tokio::test]
    async fn call_daemon_unwraps_success_and_error_envelopes() {
        let (url, _) = spawn_daemon().await;
        let tools = WebTools::new(reqwest::Client::new(), &url, 5, 12_000).unwrap();
        let data = tools
            .call_daemon("/fetch-web", json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert_eq!(data["content"], "Example page body");
        assert_eq!(
            tools.call_daemon("/error", json!({})).await.unwrap_err(),
            "bad URL"
        );
    }

    #[tokio::test]
    async fn web_fetch_posts_expected_body_and_formats_content() {
        let (url, state) = spawn_daemon().await;
        let tools = WebTools::new(reqwest::Client::new(), &url, 5, 12_345).unwrap();
        let result = tools
            .execute(&ToolCall {
                id: "call_1".into(),
                name: "web_fetch".into(),
                arguments: json!({"url": "https://example.com"}),
            })
            .await
            .unwrap();
        assert_eq!(
            result,
            "Title: Example\nURL: https://example.com\n\nExample page body"
        );
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests[0].0, "/fetch-web");
        assert_eq!(
            requests[0].1,
            json!({"url": "https://example.com", "maxChars": 12345})
        );
    }

    #[test]
    fn daemon_url_must_be_loopback() {
        assert!(WebTools::new(reqwest::Client::new(), "http://127.0.0.1:3210", 5, 30_000).is_ok());
        assert!(WebTools::new(reqwest::Client::new(), "http://localhost:3210", 5, 30_000).is_ok());
        assert!(
            WebTools::new(reqwest::Client::new(), "https://example.com", 5, 30_000)
                .err()
                .unwrap()
                .contains("loopback")
        );
    }
}
