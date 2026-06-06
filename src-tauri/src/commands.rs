use crate::config::{self, AppConfig, Provider};
use crate::proxy;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub client: Client,
    pub token_stats: proxy::TokenStatsState,
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
    config: AppConfig,
) -> Result<(), String> {
    let state_arc: &Arc<AppState> = state.inner();
    let restart_required = {
        let current = state.config.read().await;
        let running = *state.running.read().await;
        running
            && (current.proxy_host != config.proxy_host || current.proxy_port != config.proxy_port)
    };

    config::save_config(&config)?;
    let mut current = state.config.write().await;
    *current = config;
    drop(current);

    if restart_required {
        do_stop_proxy(state_arc).await?;
        do_start_proxy(state_arc).await?;
    }

    Ok(())
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
    if let Some(p) = config.providers.iter_mut().find(|p| p.id == provider.id) {
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

    let router = proxy::create_proxy_router_with_stats(config_arc, state.token_stats.clone());

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
) -> Result<serde_json::Value, String> {
    let client = &state.client;
    let protocol = provider.protocol.to_ascii_lowercase();
    let test_model = provider.models.first().cloned();

    let (method, url, body) = if let Some(model) = test_model.clone() {
        if protocol == "anthropic" {
            (
                "POST",
                proxy::anthropic_upstream_url(&provider.base_url, "/v1/messages"),
                Some(json!({
                    "model": model,
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "Reply exactly: ok"}]
                })),
            )
        } else {
            (
                "POST",
                proxy::openai_upstream_url(&provider.base_url, "/v1/chat/completions"),
                Some(json!({
                    "model": model,
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "Reply exactly: ok"}]
                })),
            )
        }
    } else if protocol == "anthropic" {
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
