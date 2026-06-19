use crate::config::{self, AppConfig, Provider};
use crate::proxy;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub client: Client,
    pub token_stats: proxy::TokenStatsState,
    pub request_logs: proxy::RequestLogState,
    pub shutdown_tx: RwLock<Option<broadcast::Sender<()>>>,
    pub server_task: RwLock<Option<JoinHandle<()>>>,
    pub running: Arc<RwLock<bool>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerStatus {
    pub running: bool,
    pub port: u16,
    pub host: String,
    pub url: String,
}

#[tauri::command]
pub async fn get_config(state: tauri::State<'_, Arc<AppState>>) -> Result<AppConfig, String> {
    let config = state.config.read().await;
    Ok(config.clone())
}

#[tauri::command]
pub async fn save_config_cmd(
    state: tauri::State<'_, Arc<AppState>>,
    mut config: AppConfig,
) -> Result<AppConfig, String> {
    let state_arc: &Arc<AppState> = state.inner();
    config = config::normalize_config(config);

    let restart_required = {
        let current = state.config.read().await;
        let running = *state.running.read().await;
        running
            && (current.proxy_host != config.proxy_host || current.proxy_port != config.proxy_port)
    };

    config::save_config(&config)?;
    let mut current = state.config.write().await;
    *current = config.clone();
    drop(current);

    if restart_required {
        do_stop_proxy(state_arc).await?;
        do_start_proxy(state_arc).await?;
    }

    Ok(config)
}

#[tauri::command]
pub async fn add_provider(
    state: tauri::State<'_, Arc<AppState>>,
    provider: Provider,
) -> Result<AppConfig, String> {
    let mut config = state.config.write().await;
    config.providers.push(provider);
    config::save_config(&config)?;
    Ok(config.clone())
}

#[tauri::command]
pub async fn update_provider(
    state: tauri::State<'_, Arc<AppState>>,
    provider: Provider,
) -> Result<AppConfig, String> {
    let mut config = state.config.write().await;
    let found = config.providers.iter_mut().find(|p| p.id == provider.id);
    if found.is_none() {
        return Err(format!("Provider not found: {}", provider.id));
    }
    if let Some(p) = found {
        *p = provider;
    }
    config::save_config(&config)?;
    Ok(config.clone())
}

#[tauri::command]
pub async fn remove_provider(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<AppConfig, String> {
    let mut config = state.config.write().await;
    config.providers.retain(|p| p.id != id);
    config::save_config(&config)?;
    Ok(config.clone())
}

#[tauri::command]
pub async fn get_server_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<ServerStatus, String> {
    let running = *state.running.read().await;
    let config = state.config.read().await;
    Ok(ServerStatus {
        running,
        port: config.proxy_port,
        host: config.proxy_host.clone(),
        url: format!("http://{}:{}", config.proxy_host, config.proxy_port),
    })
}

#[tauri::command]
pub async fn get_token_stats(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<proxy::TokenStats, String> {
    Ok(state.token_stats.read().await.clone())
}

#[tauri::command]
pub async fn reset_token_stats(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    *state.token_stats.write().await = proxy::TokenStats::default();
    Ok(())
}

#[tauri::command]
pub async fn get_request_logs(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<proxy::RequestLogEntry>, String> {
    let logs = state.request_logs.read().await;
    Ok(logs.iter().rev().cloned().collect())
}

#[tauri::command]
pub async fn clear_request_logs(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    state.request_logs.write().await.clear();
    Ok(())
}

pub async fn do_start_proxy(state: &Arc<AppState>) -> Result<ServerStatus, String> {
    let running = *state.running.read().await;
    if running {
        return Err("Proxy server is already running".to_string());
    }

    let (addr, config_arc) = {
        let config = state.config.read().await;
        (
            format!("{}:{}", config.proxy_host, config.proxy_port),
            state.config.clone(),
        )
    };

    let (shutdown_tx, mut shutdown_rx) = broadcast::channel::<()>(1);

    let router = proxy::create_proxy_router_with_stats(
        config_arc,
        state.token_stats.clone(),
        state.request_logs.clone(),
    );

    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Failed to bind to {}: {}", addr, e))?;

    *state.shutdown_tx.write().await = Some(shutdown_tx);
    *state.running.write().await = true;

    let running_flag = Arc::clone(&state.running);
    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.recv().await;
            })
            .await
            .ok();
        *running_flag.write().await = false;
    });
    *state.server_task.write().await = Some(handle);

    let config = state.config.read().await;
    Ok(ServerStatus {
        running: true,
        port: config.proxy_port,
        host: config.proxy_host.clone(),
        url: format!("http://{}:{}", config.proxy_host, config.proxy_port),
    })
}

#[tauri::command]
pub async fn start_proxy(state: tauri::State<'_, Arc<AppState>>) -> Result<ServerStatus, String> {
    let state_arc: &Arc<AppState> = state.inner();
    do_start_proxy(state_arc).await
}

pub async fn do_stop_proxy(state: &Arc<AppState>) -> Result<(), String> {
    let tx = { state.shutdown_tx.write().await.take() };
    if let Some(tx) = tx {
        let _ = tx.send(());
    }

    let task = { state.server_task.write().await.take() };
    if let Some(task) = task {
        let mut task = task;
        if timeout(Duration::from_secs(2), &mut task).await.is_err() {
            task.abort();
        }
    }

    *state.running.write().await = false;
    Ok(())
}

#[tauri::command]
pub async fn stop_proxy(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let state_arc: &Arc<AppState> = state.inner();
    do_stop_proxy(state_arc).await
}

#[tauri::command]
pub async fn test_provider(
    state: tauri::State<'_, Arc<AppState>>,
    provider: Provider,
    model: Option<String>,
) -> Result<serde_json::Value, String> {
    let client = &state.client;
    let protocol = provider.protocol.to_ascii_lowercase();
    let is_anthropic = protocol == "anthropic";

    // Connection tests (no model) hit the cheap model-list endpoint instead of
    // running a real inference round trip. Anthropic-compatible vendor endpoints
    // don't always implement /v1/models, so there we fall back to a minimal
    // 1-token inference when a model is configured.
    let quick_anthropic_model = if model.is_none() && is_anthropic {
        provider.models.first().cloned()
    } else {
        None
    };

    let (test_model, max_tokens, timeout_secs) = match (&model, &quick_anthropic_model) {
        (Some(model), _) => (Some(model.clone()), 8, 20),
        (None, Some(model)) => (Some(model.clone()), 1, 20),
        (None, None) => (None, 0, 10),
    };

    let (method, url, body) = if let Some(model) = test_model.clone() {
        let path = if is_anthropic {
            "/v1/messages"
        } else {
            "/v1/chat/completions"
        };
        let url = if is_anthropic {
            proxy::anthropic_upstream_url(&provider.base_url, path)
        } else {
            proxy::openai_upstream_url(&provider.base_url, path)
        };
        (
            "POST",
            url,
            Some(json!({
                "model": model,
                "max_tokens": max_tokens,
                "messages": [{"role": "user", "content": "Reply exactly: ok"}]
            })),
        )
    } else if is_anthropic {
        (
            "GET",
            proxy::anthropic_upstream_url(&provider.base_url, "/v1/models"),
            None,
        )
    } else {
        (
            "GET",
            proxy::openai_upstream_url(&provider.base_url, "/v1/models"),
            None,
        )
    };

    let mut req = if method == "POST" {
        let request = client.post(&url);
        if let Some(body) = body {
            request.json(&body)
        } else {
            request
        }
    } else {
        client.get(&url)
    };
    req = req.timeout(std::time::Duration::from_secs(timeout_secs));

    if protocol == "anthropic" {
        req = req
            .header("x-api-key", provider.api_key)
            .header("anthropic-version", "2023-06-01");
    } else {
        req = req.header("Authorization", format!("Bearer {}", provider.api_key));
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    let status = resp.status().as_u16();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or(json!({"error": "Failed to parse response"}));

    Ok(json!({
        "status": status,
        "url": url,
        "model": test_model,
        "body": body,
        "success": (200..300).contains(&status)
    }))
}

fn model_id_from_value(value: &Value) -> Option<String> {
    if let Some(model) = value.as_str() {
        return Some(model.to_string());
    }

    ["id", "name", "model"]
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .map(str::to_string)
}

fn extract_model_ids(body: &Value) -> Vec<String> {
    let mut models = Vec::new();

    for key in ["data", "models", "items"] {
        if let Some(items) = body.get(key).and_then(Value::as_array) {
            models.extend(items.iter().filter_map(model_id_from_value));
        }
    }

    if let Some(items) = body.as_array() {
        models.extend(items.iter().filter_map(model_id_from_value));
    }

    models.sort();
    models.dedup();
    models
}

#[tauri::command]
pub async fn fetch_provider_models(
    state: tauri::State<'_, Arc<AppState>>,
    provider: Provider,
) -> Result<Vec<String>, String> {
    fetch_provider_models_with_client(&state.client, provider).await
}

async fn fetch_provider_models_with_client(
    client: &Client,
    provider: Provider,
) -> Result<Vec<String>, String> {
    if provider.base_url.trim().is_empty() {
        return Err("Base URL is required".to_string());
    }

    let protocol = provider.protocol.to_ascii_lowercase();
    let is_anthropic = protocol == "anthropic";
    let url = if is_anthropic {
        proxy::anthropic_upstream_url(&provider.base_url, "/v1/models")
    } else {
        proxy::openai_upstream_url(&provider.base_url, "/v1/models")
    };

    let mut req = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(20));

    if !provider.api_key.trim().is_empty() {
        if is_anthropic {
            req = req
                .header("x-api-key", provider.api_key)
                .header("anthropic-version", "2023-06-01");
        } else {
            req = req.header("Authorization", format!("Bearer {}", provider.api_key));
        }
    } else if is_anthropic {
        req = req.header("anthropic-version", "2023-06-01");
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Failed to fetch models: {}", e))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Failed to fetch models: HTTP {} - {}",
            status.as_u16(),
            text
        ));
    }

    let body: Value = serde_json::from_str(&text)
        .map_err(|e| format!("Failed to parse model list response: {}", e))?;
    let models = extract_model_ids(&body);
    if models.is_empty() {
        return Err("No models found in response".to_string());
    }

    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{header, HeaderMap},
        response::IntoResponse,
        routing::get,
        Json, Router,
    };
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

    #[test]
    fn model_ids_are_extracted_from_common_response_shapes() {
        let openai_body = json!({
            "data": [
                {"id": "gpt-4o"},
                {"id": "gpt-4o-mini"}
            ]
        });
        assert_eq!(
            extract_model_ids(&openai_body),
            vec!["gpt-4o", "gpt-4o-mini"]
        );

        let alternate_body = json!({
            "models": [
                "deepseek-chat",
                {"name": "deepseek-reasoner"}
            ]
        });
        assert_eq!(
            extract_model_ids(&alternate_body),
            vec!["deepseek-chat", "deepseek-reasoner"]
        );
    }

    #[tokio::test]
    async fn provider_models_are_fetched_from_openai_compatible_endpoint() {
        let upstream = Router::new().route(
            "/v1/models",
            get(|headers: HeaderMap| async move {
                assert_eq!(
                    headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok()),
                    Some("Bearer upstream-key")
                );

                Json(json!({
                    "data": [
                        {"id": "model-b"},
                        {"id": "model-a"}
                    ]
                }))
                .into_response()
            }),
        );
        let (addr, task) = spawn_router(upstream).await;
        let provider = Provider {
            protocol: "openai".to_string(),
            base_url: format!("http://{}", addr),
            api_key: "upstream-key".to_string(),
            ..Default::default()
        };

        let models = fetch_provider_models_with_client(&Client::new(), provider)
            .await
            .unwrap();

        assert_eq!(models, vec!["model-a", "model-b"]);
        task.abort();
    }

    #[tokio::test]
    async fn provider_models_are_fetched_from_anthropic_compatible_endpoint() {
        let upstream = Router::new().route(
            "/v1/models",
            get(|headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("x-api-key").and_then(|value| value.to_str().ok()),
                    Some("anthropic-key")
                );
                assert_eq!(
                    headers
                        .get("anthropic-version")
                        .and_then(|value| value.to_str().ok()),
                    Some("2023-06-01")
                );

                Json(json!({
                    "data": [
                        {"id": "claude-sonnet-4"},
                        {"id": "claude-opus-4"}
                    ]
                }))
                .into_response()
            }),
        );
        let (addr, task) = spawn_router(upstream).await;
        let provider = Provider {
            protocol: "anthropic".to_string(),
            base_url: format!("http://{}", addr),
            api_key: "anthropic-key".to_string(),
            ..Default::default()
        };

        let models = fetch_provider_models_with_client(&Client::new(), provider)
            .await
            .unwrap();

        assert_eq!(models, vec!["claude-opus-4", "claude-sonnet-4"]);
        task.abort();
    }
}
