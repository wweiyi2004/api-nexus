#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod fusion;
mod proxy;
mod security;
mod storage;

use commands::AppState;
use reqwest::Client;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use tokio::sync::RwLock;

fn main() {
    env_logger::init();

    let app_config = config::load_config();
    let auto_start = app_config.auto_start;
    let request_logs = Arc::new(
        storage::RequestLogStore::open(
            &config::database_path(),
            app_config.max_log_entries,
            app_config.log_retention_days,
        )
        .expect("failed to open request log database"),
    );
    let persisted_stats = request_logs
        .initial_token_stats()
        .expect("failed to load persisted token statistics");

    let state = Arc::new(AppState {
        config: Arc::new(RwLock::new(app_config)),
        client: Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap(),
        token_stats: Arc::new(RwLock::new(proxy::TokenStats {
            request_count: persisted_stats.request_count,
            input_tokens: persisted_stats.input_tokens,
            output_tokens: persisted_stats.output_tokens,
            cached_tokens: persisted_stats.cache_read_tokens + persisted_stats.cache_write_tokens,
            cache_read_tokens: persisted_stats.cache_read_tokens,
            cache_write_tokens: persisted_stats.cache_write_tokens,
        })),
        request_logs,
        shutdown_tx: RwLock::new(None),
        server_task: RwLock::new(None),
        running: Arc::new(RwLock::new(false)),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            commands::get_request_logs,
            commands::clear_request_logs,
            commands::export_request_logs_csv,
            commands::run_fusion,
            commands::get_fusion_runs,
            commands::get_fusion_run,
            commands::clear_fusion_runs,
            commands::start_proxy,
            commands::stop_proxy,
            commands::test_provider,
            commands::fetch_provider_models,
        ])
        .setup(move |app| {
            let show_item = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
            let toggle_item =
                MenuItem::with_id(app, "toggle_proxy", "切换代理", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &toggle_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("API Nexus")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    "toggle_proxy" => {
                        let state: tauri::State<'_, Arc<AppState>> = app.state();
                        let state_arc: Arc<AppState> = state.inner().clone();
                        tauri::async_runtime::spawn(async move {
                            let running = *state_arc.running.read().await;
                            if running {
                                let _ = commands::do_stop_proxy(&state_arc).await;
                            } else {
                                let _ = commands::do_start_proxy(&state_arc).await;
                            }
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::DoubleClick { .. } = event {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            if auto_start {
                let state_arc: Arc<AppState> = app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = commands::do_start_proxy(&state_arc).await {
                        log::error!("Auto-start proxy failed: {}", err);
                    }
                });
            }
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if let Some(window) = window.get_webview_window("main") {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
