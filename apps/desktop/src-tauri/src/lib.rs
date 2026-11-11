//! Switchyard desktop shell.
//!
//! The desktop process is a *client* of Switchyard Core. It owns no
//! orchestration state: its Rust side keeps one IPC connection to Core
//! ([`bridge`]), starts Core when it is not running ([`launcher`]), and
//! exposes a narrow command surface to the webview ([`commands`]). Quitting
//! the desktop app leaves Core and anything it manages running.

mod bridge;
mod commands;
mod launcher;

use std::process::ExitCode;
use std::sync::Arc;

use switchyard_protocol::paths::CorePaths;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

pub fn run() -> ExitCode {
    let filter = EnvFilter::try_from_env("SWITCHYARD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).try_init();

    let paths = match CorePaths::from_env() {
        Ok(paths) => paths,
        Err(err) => {
            tracing::error!(%err, "cannot resolve Switchyard paths");
            eprintln!("Switchyard: {err}");
            return ExitCode::from(2);
        }
    };
    let launcher = launcher::Launcher::detect(&paths);
    let bridge = Arc::new(bridge::Bridge::new(paths, launcher));

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::clone(&bridge))
        .invoke_handler(tauri::generate_handler![
            commands::core_connection,
            commands::core_environment,
            commands::core_request,
            commands::core_reconnect,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let bridge = Arc::clone(app.state::<Arc<bridge::Bridge>>().inner());
            tauri::async_runtime::spawn(bridge::supervise(handle, bridge));
            Ok(())
        })
        .run(tauri::generate_context!());

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "Switchyard desktop failed");
            ExitCode::FAILURE
        }
    }
}
