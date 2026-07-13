use crate::agent;
use crate::app::bootstrap::build_app_state;
use crate::app::config::{AppConfig, AppMode};
use crate::app::error::AppError;
use crate::app::identity;
use crate::app::import::import_legacy_workspace;
use crate::app::paths::AppPaths;
use crate::app::server;
use crate::diagnostics::crash::install_crash_handlers;
use crate::diagnostics::logging::init_logging;
use crate::playback::monitor::spawn_listening_monitor;
use crate::playback::service::{
    apply_active_zone_playback_settings, spawn_playback_zone_cache_warmer,
};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run() -> Result<(), AppError> {
    install_crash_handlers();

    if std::env::args().any(|argument| matches!(argument.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(());
    }

    if let Some(source) = import_workspace_argument()? {
        let paths = AppPaths::from_env();
        let report = import_legacy_workspace(&source, &paths, |event| {
            if let Ok(json) = serde_json::to_string(&event) {
                println!("{json}");
            }
        })
        .map_err(|error| {
            println!(
                "{}",
                serde_json::json!({ "stage": "error", "message": &error })
            );
            AppError::persistence(error)
        })?;
        println!(
            "{}",
            serde_json::json!({ "stage": "result", "report": report })
        );
        return Ok(());
    }

    let config = AppConfig::from_env()?;
    init_logging(config.log_format);
    match config.mode {
        AppMode::Agent => run_agent_mode().await,
        AppMode::Core => run_core_mode(config).await,
    }
}

fn print_help() {
    println!(
        "Fozmo server\n\nUSAGE:\n    fozmo-server [OPTIONS]\n    fozmo-server --import-workspace <PATH>\n\nMAINTENANCE:\n    --import-workspace <PATH>  Import a stopped legacy workspace, emit JSON progress, and exit\n\nOPTIONS:\n    --lan                     Listen on LAN interfaces\n    --port <PORT>             HTTP port\n    --require-pairing         Require browser pairing\n    --help, -h                Show this help\n\nPACKAGED ROOTS (environment):\n    FOZMO_RESOURCE_DIR        Read-only bundled resources\n    FOZMO_DATA_DIR            Persistent settings, database, history and user assets\n    FOZMO_CACHE_DIR           Rebuildable caches\n    FOZMO_LOG_DIR             Logs\n    FOZMO_WORKSPACE_DIR       Explicit legacy single-root development layout"
    );
}

async fn run_agent_mode() -> Result<(), AppError> {
    agent::run_agent()
        .await
        .map_err(|source| AppError::agent(source.to_string()))
}

async fn run_core_mode(config: AppConfig) -> Result<(), AppError> {
    print_banner();

    let paths = AppPaths::from_env();
    if config.release_smoke {
        validate_release_smoke_environment(&paths)?;
    }
    paths
        .ensure_directories()
        .map_err(|source| AppError::io("create application directories", source))?;
    let _data_lock = paths
        .acquire_data_lock()
        .map_err(|source| AppError::io("lock application data directory", source))?;
    let state = build_app_state(
        &paths,
        config.public_base_url.clone(),
        config.port,
        config.pairing_required,
        config.pairing_token_ttl_secs,
        config.allow_query_token_auth,
        config.release_smoke,
    )?;

    // Replay the active zone's persisted settings into the freshly-started worker.
    apply_active_zone_playback_settings(&state);
    spawn_playback_zone_cache_warmer(state.clone());
    maybe_spawn_startup_scan(&state, config.startup_scan_enabled);
    spawn_listening_monitor(state.clone());
    Arc::clone(state.qobuz()).start_home_cache_warmer();

    server::serve(state, &paths, &config).await
}

fn validate_release_smoke_environment(paths: &AppPaths) -> Result<(), AppError> {
    let required_root_keys =
        ["RESOURCE_DIR", "DATA_DIR", "CACHE_DIR", "LOG_DIR"].map(identity::env_key);
    let missing = required_root_keys
        .iter()
        .filter(|key| std::env::var_os(key).is_none())
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(AppError::persistence(format!(
            "--release-smoke requires explicit packaged roots; missing {}",
            missing.join(", ")
        )));
    }

    let workspace_keys = [
        identity::env_key("WORKSPACE_DIR"),
        identity::legacy_env_key("WORKSPACE_DIR"),
    ];
    if let Some(key) = workspace_keys
        .iter()
        .find(|key| std::env::var_os(key).is_some())
    {
        return Err(AppError::persistence(format!(
            "--release-smoke forbids inherited workspace root {key}"
        )));
    }

    let current_executable = std::env::current_exe()
        .map_err(|source| AppError::io("resolve release-smoke executable", source))?;
    paths
        .validate_release_smoke_layout(&current_executable)
        .map_err(|message| {
            AppError::persistence(format!("invalid --release-smoke environment: {message}"))
        })
}

fn import_workspace_argument() -> Result<Option<PathBuf>, AppError> {
    let args = std::env::args().collect::<Vec<_>>();
    import_workspace_argument_from(&args)
}

fn import_workspace_argument_from(args: &[String]) -> Result<Option<PathBuf>, AppError> {
    let mut source = None;
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--import-workspace" {
            let value = args.get(index + 1).ok_or_else(|| {
                AppError::persistence(
                    "--import-workspace requires the path to a stopped legacy workspace"
                        .to_string(),
                )
            })?;
            if source.replace(PathBuf::from(value)).is_some() {
                return Err(AppError::persistence(
                    "--import-workspace may only be supplied once".to_string(),
                ));
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    if source.is_some() && args.iter().any(|argument| argument == "--release-smoke") {
        return Err(AppError::persistence(
            "--release-smoke cannot be combined with --import-workspace".to_string(),
        ));
    }
    Ok(source)
}

fn print_banner() {
    println!("--------------------------------------------------");
    println!(
        "                 {}                 ",
        identity::APP_DISPLAY_NAME
    );
    println!("            (HQPlayer-style DSP Engine)           ");
    println!("--------------------------------------------------");
}

fn maybe_spawn_startup_scan(state: &crate::app::state::AppState, startup_scan_enabled: bool) {
    if startup_scan_enabled {
        let startup_scan_library = Arc::clone(state.library());
        let _startup_scan =
            tokio::task::spawn_blocking(move || match startup_scan_library.scan() {
                Ok(result) => println!(
                    "library: initial scan complete: scanned {}, updated {}, removed {}",
                    result.scanned, result.updated, result.removed
                ),
                Err(e) => eprintln!("library: initial scan failed: {}", e),
            });
    } else {
        println!("library: startup scan disabled; use Settings to rescan manually");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_smoke_cannot_be_combined_with_workspace_import() {
        let args = [
            "fozmo-server".to_string(),
            "--release-smoke".to_string(),
            "--import-workspace".to_string(),
            "/tmp/legacy".to_string(),
        ];

        let error = import_workspace_argument_from(&args).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--release-smoke cannot be combined with --import-workspace")
        );
    }
}
