use crate::app::error::AppError;
use crate::app::paths::{AppPaths, clean_windows_verbatim, dedupe_paths};
use crate::app::state::{
    AppCoreServices, AppMediaServices, AppPlaybackServices, AppRuntimeServices, AppState,
    AppStatePaths,
};
use crate::audio::airplay::AirPlayRegistry;
use crate::audio::player::Player;
use crate::audio::sonos::SonosService;
use crate::audio::upnp::UpnpRendererService;
use crate::diagnostics::status::DiagnosticsService;
use crate::library::Library;
use crate::listening::ListeningTracker;
use crate::playback::config_applicator::PlaybackConfigApplicator;
use crate::playback::sequencer::PlaybackCommandSequencer;
#[cfg(not(test))]
use crate::secrets::KeyringSecretsStore;
use crate::secrets::{SecretKey, SecretValue, SecretsStore};
#[cfg(feature = "apple_music_capture")]
use crate::services::apple_music::AppleMusicCaptureService;
use crate::services::hegel::HegelStatusCache;
use crate::services::lastfm::LastFmService;
use crate::services::qobuz::QobuzService;
use crate::settings::SettingsStore;
use crate::zones::{PairingManager, ZoneManager};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn build_app_state(
    paths: &AppPaths,
    public_base_url: String,
    app_port: u16,
    pairing_required: bool,
    pairing_token_ttl_secs: u64,
    allow_query_token_auth: bool,
    release_smoke: bool,
) -> Result<AppState, AppError> {
    paths
        .ensure_directories()
        .map_err(|source| AppError::io("create application directories", source))?;
    paths.print_summary();

    let install = paths
        .load_or_create_install_metadata()
        .map_err(AppError::Persistence)?;
    let settings = Arc::new(
        SettingsStore::try_new_with_namespace(
            paths.settings_path.clone(),
            install.installation_id.clone(),
        )
        .map_err(AppError::persistence)?,
    );
    let secrets = build_secrets_store(paths, release_smoke);
    let qobuz_cache_dir = paths.qobuz_cache_dir.clone();
    let remote_tls_key = SecretKey::RemoteTlsKey {
        installation_id: install.installation_id.clone(),
    };
    // One eager load so the single keychain prompt (if any) appears at
    // launch, while someone is at the machine, rather than whenever the
    // remote listener or a streaming feature first touches a secret.
    let mut legacy_secret_keys = vec![
        SecretKey::LastFmApiKey,
        SecretKey::LegacyGlobalPairingTokenRecords,
        remote_tls_key,
        SecretKey::LegacyRemoteTlsKey,
        SecretKey::QobuzSession {
            account: crate::services::qobuz::session_account(&qobuz_cache_dir),
        },
        settings.legacy_pairing_token_records_secret_key(),
    ];
    if let Some(legacy_workspace) = imported_legacy_workspace(paths) {
        legacy_secret_keys.push(SecretKey::QobuzSession {
            account: crate::services::qobuz::session_account(
                &legacy_workspace.join("library").join("qobuz-cache"),
            ),
        });
        legacy_secret_keys.push(SecretKey::pairing_token_records(legacy_settings_namespace(
            &legacy_workspace.join("settings.json"),
        )));
    }
    if let Err(error) = secrets.warm_up(&legacy_secret_keys) {
        tracing::warn!(
            event = "secrets_warm_up",
            status = "error",
            error_kind = "secret_store",
            "secret storage unavailable at startup; the next feature that needs a secret will retry: {error}"
        );
    }
    migrate_stable_secret_namespaces(secrets.as_ref(), &settings, paths, &install.installation_id);
    // Global pairing records cannot be migrated safely: copying them into a
    // workspace would preserve cross-workspace trust, while copying them into
    // every workspace would preserve the vulnerability outright. Invalidate
    // them once discovered; each workspace must pair again under its own key.
    if matches!(
        secrets.get(SecretKey::LegacyGlobalPairingTokenRecords),
        Ok(Some(_))
    ) {
        if let Err(error) = secrets.delete(SecretKey::LegacyGlobalPairingTokenRecords) {
            tracing::warn!(
                event = "pairing_global_records_invalidate",
                status = "error",
                error_kind = "secret_store",
                "failed to remove legacy global pairing records: {error}"
            );
        } else {
            tracing::warn!(
                event = "pairing_global_records_invalidate",
                status = "ok",
                "legacy global pairing sessions were invalidated; workspace pairing is required"
            );
        }
    }
    settings.migrate_legacy_secrets(secrets.as_ref(), pairing_token_ttl_secs);
    let music_dirs = configured_music_dirs(&settings, paths);
    println!("Music library paths:");
    for dir in &music_dirs {
        println!("  {:?}", dir);
    }

    let player = Arc::new(Player::new());
    let zones = ZoneManager::new(Arc::clone(&player), settings.snapshot().active_zone_id);
    let library = Arc::new(
        Library::new_managed(
            paths.library_dir.join("library.db"),
            music_dirs,
            paths.art_dir.clone(),
            paths.thumbnail_cache_dir.clone(),
            &paths.settings_path,
            &paths.backups_dir,
        )
        .map_err(AppError::library)?,
    );
    let listening = Arc::new(ListeningTracker::default());
    let qobuz = Arc::new(
        QobuzService::new_with_session_account(
            qobuz_cache_dir,
            Arc::clone(&secrets),
            install.installation_id.clone(),
        )
        .map_err(AppError::qobuz)?,
    );
    let lastfm = Arc::new(LastFmService::new().map_err(AppError::lastfm)?);
    #[cfg(feature = "apple_music_capture")]
    let apple_music_capture = Arc::new(AppleMusicCaptureService::new(Arc::clone(&player)));
    let airplay = Arc::new(AirPlayRegistry::new());
    let sonos = Arc::new(
        SonosService::new(paths.sonos_cache_dir.clone(), public_base_url.clone())
            .map_err(AppError::Sonos)?,
    );
    let upnp = Arc::new(UpnpRendererService::new(public_base_url.clone()));

    let pairing = PairingManager::new(
        Arc::clone(&settings),
        Arc::clone(&secrets),
        pairing_required,
        pairing_token_ttl_secs,
        allow_query_token_auth,
    );

    let state = AppState::new(
        AppCoreServices {
            settings,
            secrets: Arc::clone(&secrets),
            library,
            listening,
        },
        AppMediaServices {
            qobuz,
            lastfm,
            #[cfg(feature = "apple_music_capture")]
            apple_music_capture,
            airplay,
            sonos,
            upnp,
            local_transcode: Arc::new(crate::audio::transcode::LocalTranscodeService::new(
                paths.transcode_cache_dir.clone(),
            )),
        },
        AppPlaybackServices {
            playback_sequencer: PlaybackCommandSequencer::default(),
            playback_config_applicator: PlaybackConfigApplicator::default(),
            zones,
        },
        AppRuntimeServices {
            pairing,
            diagnostics: DiagnosticsService::new(),
            hegel_status: HegelStatusCache::default(),
            remote_access: crate::app::server_remote::RemoteAccessController::new(
                paths,
                app_port,
                install.installation_id.clone(),
            ),
        },
        AppStatePaths {
            public_base_url,
            music_dir: paths.music_dir.clone(),
            presets_dir: paths.presets_dir.clone(),
            built_in_presets_dir: paths.built_in_presets_dir.clone(),
            appearance_assets_dir: paths.appearance_assets_dir.clone(),
            settings_path: paths.settings_path.clone(),
            backups_dir: paths.backups_dir.clone(),
        },
    );
    Ok(state)
}

fn build_secrets_store(paths: &AppPaths, release_smoke: bool) -> Arc<dyn SecretsStore> {
    if release_smoke {
        return Arc::new(crate::secrets::EphemeralSecretsStore::new());
    }

    #[cfg(test)]
    {
        let _ = paths;
        Arc::new(crate::secrets::MemorySecretsStore::new())
    }

    #[cfg(not(test))]
    {
        #[cfg(feature = "dev-secrets-file")]
        {
            if std::env::var(crate::app::identity::env_key("DEV_SECRETS_FILE"))
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false)
            {
                return Arc::new(crate::secrets::DevFileSecretsStore::new(
                    paths.dev_secrets_path.clone(),
                ));
            }
        }

        let _ = paths;
        Arc::new(KeyringSecretsStore::new())
    }
}

fn configured_music_dirs(settings: &SettingsStore, paths: &AppPaths) -> Vec<PathBuf> {
    let music_dirs = match settings.snapshot().music_dirs {
        Some(folders) => folders
            .into_iter()
            .map(|path| clean_windows_verbatim(PathBuf::from(path)))
            .collect(),
        None => vec![paths.music_dir.clone()],
    };
    dedupe_paths(music_dirs)
}

fn imported_legacy_workspace(paths: &AppPaths) -> Option<PathBuf> {
    let body = std::fs::read_to_string(paths.data_dir.join("import.json")).ok()?;
    let report: crate::app::import::LegacyImportReport = serde_json::from_str(&body).ok()?;
    Some(PathBuf::from(report.source_workspace))
}

fn legacy_settings_namespace(path: &std::path::Path) -> String {
    crate::settings::path_secret_namespace(path)
}

fn migrate_stable_secret_namespaces(
    secrets: &dyn SecretsStore,
    settings: &SettingsStore,
    paths: &AppPaths,
    installation_id: &str,
) {
    let qobuz_target = SecretKey::QobuzSession {
        account: installation_id.to_string(),
    };
    let pairing_target = settings.pairing_token_records_secret_key();
    let remote_tls_target = SecretKey::RemoteTlsKey {
        installation_id: installation_id.to_string(),
    };
    let mut qobuz_legacy = vec![SecretKey::QobuzSession {
        account: crate::services::qobuz::session_account(&paths.qobuz_cache_dir),
    }];
    let mut pairing_legacy = vec![settings.legacy_pairing_token_records_secret_key()];
    if let Some(workspace) = imported_legacy_workspace(paths) {
        qobuz_legacy.push(SecretKey::QobuzSession {
            account: crate::services::qobuz::session_account(
                &workspace.join("library").join("qobuz-cache"),
            ),
        });
        pairing_legacy.push(SecretKey::pairing_token_records(legacy_settings_namespace(
            &workspace.join("settings.json"),
        )));
    }
    for legacy in qobuz_legacy {
        migrate_secret_alias(secrets, legacy, qobuz_target.clone());
    }
    for legacy in pairing_legacy {
        migrate_secret_alias(secrets, legacy, pairing_target.clone());
    }
    migrate_secret_alias(secrets, SecretKey::LegacyRemoteTlsKey, remote_tls_target);
}

fn migrate_secret_alias(secrets: &dyn SecretsStore, legacy: SecretKey, target: SecretKey) {
    if legacy == target {
        return;
    }
    match secrets.get(target.clone()) {
        Ok(Some(target_value)) => {
            if let Ok(Some(legacy_value)) = secrets.get(legacy.clone())
                && legacy_value.expose_secret() == target_value.expose_secret()
                && let Err(error) = secrets.delete(legacy)
            {
                eprintln!("secrets: stable account exists but legacy cleanup failed: {error}");
            }
            return;
        }
        Err(error) => {
            eprintln!("secrets: failed to inspect stable account: {error}");
            return;
        }
        Ok(None) => {}
    }
    let value = match secrets.get(legacy.clone()) {
        Ok(Some(value)) => value,
        Ok(None) => return,
        Err(error) => {
            eprintln!("secrets: failed to inspect legacy account: {error}");
            return;
        }
    };
    let expected = value.expose_secret().to_string();
    if let Err(error) = secrets.put(target.clone(), SecretValue::new(expected.clone())) {
        eprintln!("secrets: failed to migrate stable account: {error}");
        return;
    }
    match secrets.get(target) {
        Ok(Some(saved)) if saved.expose_secret() == expected => {}
        Ok(_) => {
            eprintln!("secrets: stable account migration could not be verified");
            return;
        }
        Err(error) => {
            eprintln!("secrets: failed to verify stable account migration: {error}");
            return;
        }
    }
    if let Err(error) = secrets.delete(legacy) {
        eprintln!("secrets: stable account was written but legacy cleanup failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SourceRef;
    use crate::secrets::MemorySecretsStore;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn configured_music_folders_distinguish_new_and_explicitly_empty_settings() {
        let root = std::env::temp_dir().join(format!(
            "fozmo-bootstrap-music-folders-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = AppPaths::from_workspace_dir(&root);
        let settings = SettingsStore::new(root.join("settings.json"));

        assert_eq!(
            configured_music_dirs(&settings, &paths),
            vec![paths.music_dir.clone()]
        );

        let _ = settings.update(|current| current.music_dirs = Some(Vec::new()));
        assert!(configured_music_dirs(&settings, &paths).is_empty());
    }

    #[test]
    fn legacy_remote_tls_key_is_verified_then_moved_to_installation_scope() {
        let secrets = MemorySecretsStore::new();
        secrets
            .put(
                SecretKey::LegacyRemoteTlsKey,
                SecretValue::new("private-key-pem"),
            )
            .unwrap();
        let target = SecretKey::RemoteTlsKey {
            installation_id: "00000000-0000-4000-8000-000000000001".to_string(),
        };

        migrate_secret_alias(&secrets, SecretKey::LegacyRemoteTlsKey, target.clone());

        assert_eq!(
            secrets.get(target).unwrap().unwrap().expose_secret(),
            "private-key-pem"
        );
        assert!(
            secrets
                .get(SecretKey::LegacyRemoteTlsKey)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn build_app_state_preserves_persisted_queue_state() {
        let root = std::env::temp_dir().join(format!(
            "fozmo-bootstrap-queue-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = AppPaths::from_workspace_dir(&root);
        let state = build_app_state(
            &paths,
            "http://core.test".to_string(),
            3000,
            false,
            crate::zones::DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
            false,
        )
        .unwrap();
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        let queued = SourceRef::LocalTrack {
            track_id: 42,
            file_name: Some("02 Saved.wav".to_string()),
            title: Some("Saved".to_string()),
            artist: None,
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        };
        state.library().set_zone_queue(&zone_id, &[queued]).unwrap();
        state
            .library()
            .set_now_playing_queue(
                &zone_id,
                &json!({
                    "kind": "local",
                    "cursor": 0,
                    "items": [
                        { "title": "Current", "filename": "01 Current.wav" },
                        { "title": "Saved", "filename": "02 Saved.wav" }
                    ],
                    "loopMode": "off"
                }),
            )
            .unwrap();
        drop(state);

        let restored = build_app_state(
            &paths,
            "http://core.test".to_string(),
            3000,
            false,
            crate::zones::DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
            false,
        )
        .unwrap();
        let saved_queue = restored.library().zone_queue(&zone_id).unwrap();
        let saved_now_playing = restored
            .library()
            .now_playing_queue(&zone_id)
            .unwrap()
            .unwrap();

        assert_eq!(saved_queue.len(), 1);
        assert_eq!(
            saved_now_playing.state["items"].as_array().unwrap().len(),
            2
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
