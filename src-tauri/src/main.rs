#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod proxy;

use commands::AppState;
use reqwest::Client;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

fn main() {
    env_logger::init();

    let app_config = config::load_config();
    let auto_start = app_config.auto_start;

    let state = Arc::new(AppState {
        config: Arc::new(RwLock::new(app_config)),
        client: Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap(),
        token_stats: Arc::new(RwLock::new(proxy::TokenStats::default())),
        shutdown_tx: RwLock::new(None),
        server_task: RwLock::new(None),
        running: Arc::new(RwLock::new(false)),
    });

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config_cmd,
            commands::add_provider,
            commands::update_provider,
            commands::remove_provider,
            commands::get_server_status,
            commands::get_token_stats,
            commands::reset_token_stats,
            commands::start_proxy,
            commands::stop_proxy,
            commands::test_provider,
        ])
        .setup(move |app| {
            if auto_start {
                let state_arc: Arc<AppState> = app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = commands::do_start_proxy(&state_arc).await;
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
