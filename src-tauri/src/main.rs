#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod proxy;

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

    let state = Arc::new(AppState {
        config: Arc::new(RwLock::new(app_config)),
        client: Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap(),
        token_stats: Arc::new(RwLock::new(proxy::TokenStats::default())),
        request_logs: Arc::new(RwLock::new(std::collections::VecDeque::new())),
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
            commands::get_request_logs,
            commands::clear_request_logs,
            commands::start_proxy,
            commands::stop_proxy,
            commands::test_provider,
        ])
        .setup(move |app| {
            let show_item = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
            let toggle_item = MenuItem::with_id(app, "toggle_proxy", "切换代理", true, None::<&str>)?;
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
