//! Persisted user preferences. Loaded on startup, auto-saved on every change.
//!
//! Stored as JSON at `<workspace>/settings.json`. Fields are all optional so a partial or
//! missing file degrades gracefully to the program defaults.

mod dsd;
mod model;
mod playback;
mod profiles;
mod store;
mod validation;

pub use dsd::DsdSourceRule;
#[cfg(feature = "apple_music_capture")]
pub use model::AppleMusicCaptureSettings;
pub use model::{
    AppearanceSettings, AuthTokenBinding, AuthTokenKind, DEFAULT_PROFILE_ID, HegelSettings,
    ListeningProfile, PairingTokenRecord, PersistedSettings, RemoteAccessSettings,
    RemoteSessionClientMetadata,
};
pub use playback::ZonePlaybackSettings;
pub use store::SettingsStore;
pub(crate) use store::path_secret_namespace;
pub(crate) use validation::parse_settings as parse_settings_read_only;
pub use validation::validate_remote_access;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{MemorySecretsStore, SecretKey, SecretsStore};
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_settings_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fozmo-settings-{name}-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn remove_settings_files(path: &std::path::Path) {
        let parent = path.parent().unwrap();
        let prefix = path.file_name().unwrap().to_string_lossy().to_string();
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    #[test]
    fn zone_playback_settings_fall_back_to_legacy_fields() {
        let settings = PersistedSettings {
            filter_type: Some("Minimum16k".to_string()),
            target_rate: Some(192_000),
            upsampling_enabled: Some(false),
            exclusive: Some(false),
            device_name: Some("Legacy Device".to_string()),
            ..PersistedSettings::default()
        };

        let playback = settings.playback_for_zone("local-core");

        assert_eq!(playback.filter_type.as_deref(), Some("Minimum16k"));
        assert_eq!(playback.target_rate, Some(192_000));
        assert_eq!(playback.upsampling_enabled, Some(false));
        assert_eq!(playback.exclusive, Some(false));
        assert_eq!(playback.device_name.as_deref(), Some("Legacy Device"));
    }

    #[test]
    fn new_zone_playback_defaults_to_dsp_disabled() {
        let settings = PersistedSettings::default();

        let playback = settings.playback_for_zone("new-output");

        assert_eq!(playback.upsampling_enabled, Some(false));
    }

    #[test]
    fn qobuz_radio_is_enabled_by_default() {
        let path = temp_settings_path("qobuz-radio-default");
        let store = SettingsStore::new(path);

        assert!(store.qobuz_radio_enabled());
    }

    #[test]
    fn lastfm_radio_is_disabled_by_default() {
        let path = temp_settings_path("lastfm-radio-default");
        let store = SettingsStore::new(path);

        assert!(!store.lastfm_radio_enabled());
    }

    #[test]
    fn secret_fields_are_not_serialized_to_settings_json() {
        let path = temp_settings_path("secret-serialization");
        let settings = PersistedSettings {
            pairing_tokens: Some(vec!["raw-token".to_string()]),
            pairing_token_records: vec![PairingTokenRecord {
                id: "record".to_string(),
                kind: AuthTokenKind::LegacyToken,
                token_hash: "hash".to_string(),
                scopes: vec!["control".to_string()],
                subject: None,
                label: None,
                issued_at_unix_secs: 1,
                expires_at_unix_secs: 2,
                last_used_at_unix_secs: None,
                rotated_at_unix_secs: None,
                revoked_at_unix_secs: None,
                binding: None,
                remote_session_metadata: None,
            }],
            lastfm_api_key: Some("test-key".to_string()),
            ..PersistedSettings::default()
        };
        settings.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();

        assert!(!saved.contains("pairing_tokens"));
        assert!(!saved.contains("pairing_token_records"));
        assert!(!saved.contains("lastfm_api_key"));
        assert!(!saved.contains("raw-token"));
        assert!(!saved.contains("test-key"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_secret_fields_migrate_to_secret_store_and_rewrite_settings() {
        let path = temp_settings_path("legacy-secret-migration");
        std::fs::write(
            &path,
            r#"{
                "lastfm_api_key": "  test-key  ",
                "pairing_tokens": ["legacy-token"],
                "pairing_token_records": [
                    {
                        "id": "record",
                        "token_hash": "existing-hash",
                        "issued_at_unix_secs": 1,
                        "expires_at_unix_secs": 9999999999
                    }
                ]
            }"#,
        )
        .unwrap();
        let store = SettingsStore::new(path.clone());
        let secrets = MemorySecretsStore::new();

        store.migrate_legacy_secrets(&secrets, crate::zones::DEFAULT_PAIRING_TOKEN_TTL_SECS);

        assert_eq!(
            secrets
                .get(SecretKey::LastFmApiKey)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "test-key"
        );
        let records_json = secrets
            .get(store.pairing_token_records_secret_key())
            .unwrap()
            .unwrap()
            .expose_secret()
            .to_string();
        let records: Vec<PairingTokenRecord> = serde_json::from_str(&records_json).unwrap();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .any(|record| record.token_hash == "existing-hash")
        );
        assert!(
            records
                .iter()
                .all(|record| record.token_hash != "legacy-token")
        );

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("lastfm_api_key"));
        assert!(!saved.contains("pairing_tokens"));
        assert!(!saved.contains("pairing_token_records"));
        assert!(!saved.contains("legacy-token"));
        assert!(!saved.contains("test-key"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn updating_one_zone_preserves_other_zone_playback_settings() {
        let path = temp_settings_path("zones");
        let store = SettingsStore::new(path.clone());

        let _ = store.update_playback_for_zone("macbook", |settings| {
            settings.filter_type = Some("SincExperimental1m".to_string());
            settings.target_rate = Some(96_000);
        });
        let _ = store.update_playback_for_zone("pc", |settings| {
            settings.filter_type = Some("Minimum16k".to_string());
            settings.target_rate = Some(384_000);
            settings.exclusive = Some(true);
        });

        let snapshot = store.snapshot();
        let macbook = snapshot.playback_for_zone("macbook");
        let pc = snapshot.playback_for_zone("pc");

        assert_eq!(macbook.filter_type.as_deref(), Some("SplitPhase128kE3"));
        assert_eq!(macbook.target_rate, Some(96_000));
        assert_eq!(pc.filter_type.as_deref(), Some("Minimum16k"));
        assert_eq!(pc.target_rate, Some(384_000));
        assert_eq!(pc.exclusive, Some(true));
        assert_eq!(snapshot.filter_type.as_deref(), Some("Minimum16k"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn store_update_normalizes_profiles_before_persisting() {
        let path = temp_settings_path("normalize-profiles");
        let initial = PersistedSettings {
            profiles: Some(vec![
                ListeningProfile {
                    id: String::new(),
                    name: "Missing id".to_string(),
                    color: "#4f84a5".to_string(),
                    image: None,
                    recent_searches: Vec::new(),
                },
                ListeningProfile {
                    id: "night".to_string(),
                    name: "Night".to_string(),
                    color: "#59806c".to_string(),
                    image: None,
                    recent_searches: Vec::new(),
                },
            ]),
            active_profile_id: Some("missing".to_string()),
            ..PersistedSettings::default()
        };
        initial.save(&path).unwrap();
        let store = SettingsStore::new(path.clone());

        let _ = store.update(|settings| {
            settings.qobuz_radio_enabled = Some(false);
        });

        let saved = PersistedSettings::load(&path);
        let profiles = saved.profiles.as_ref().unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "night");
        assert_eq!(profiles[0].name, "Night");
        assert_eq!(profiles[0].color, "#59806c");
        assert_eq!(saved.active_profile_id.as_deref(), Some("night"));
        assert_eq!(saved.qobuz_radio_enabled, Some(false));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_settings_write_returns_error_without_publishing_snapshot() {
        let path = temp_settings_path("failed-write-snapshot");
        let store = SettingsStore::new(path.clone());
        std::fs::create_dir(&path).unwrap();

        let result = store.try_update(|settings| {
            settings.qobuz_radio_enabled = Some(false);
        });

        assert!(result.is_err());
        assert!(store.qobuz_radio_enabled());

        std::fs::remove_dir(&path).unwrap();
        remove_settings_files(&path);
    }

    #[test]
    fn concurrent_debounced_playback_updates_share_one_durable_write() {
        const UPDATES: usize = 8;
        let path = temp_settings_path("debounced-playback-writes");
        let store = Arc::new(SettingsStore::new(path.clone()));
        let barrier = Arc::new(Barrier::new(UPDATES));
        let threads = (0..UPDATES)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .try_update_playback_for_zone_debounced(
                            &format!("zone-{index}"),
                            |settings| settings.volume = Some(index as f32 / 10.0),
                        )
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap();
        }

        assert_eq!(store.persisted_write_count(), 1);
        let saved = PersistedSettings::load(&path);
        for index in 0..UPDATES {
            assert_eq!(
                saved.playback_for_zone(&format!("zone-{index}")).volume,
                Some(index as f32 / 10.0)
            );
        }

        remove_settings_files(&path);
    }

    #[test]
    fn failed_debounced_batch_is_discarded_before_later_success() {
        const UPDATES: usize = 4;
        let path = temp_settings_path("failed-debounced-batch");
        let store = Arc::new(SettingsStore::new(path.clone()));
        std::fs::create_dir(&path).unwrap();
        let barrier = Arc::new(Barrier::new(UPDATES));
        let threads = (0..UPDATES)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.try_update_playback_for_zone_debounced(
                        &format!("failed-zone-{index}"),
                        |settings| settings.volume = Some(0.5),
                    )
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            assert!(thread.join().unwrap().is_err());
        }
        assert!(store.snapshot().zone_settings.is_empty());

        std::fs::remove_dir(&path).unwrap();
        store
            .try_update(|settings| settings.qobuz_radio_enabled = Some(false))
            .unwrap();

        let saved = PersistedSettings::load(&path);
        assert_eq!(saved.qobuz_radio_enabled, Some(false));
        assert!(
            saved
                .zone_settings
                .keys()
                .all(|zone_id| !zone_id.starts_with("failed-zone-"))
        );
        assert_eq!(store.persisted_write_count(), 2);

        remove_settings_files(&path);
    }

    #[test]
    fn update_profile_persists_profile_image_as_external_asset() {
        let path = temp_settings_path("profile-image");
        let store = SettingsStore::new(path.clone());
        let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

        store
            .update_profile("default", "Default", "#7c8f6a", Some(image))
            .unwrap();

        let saved = PersistedSettings::load(&path);
        let profile = saved
            .normalized_profiles()
            .into_iter()
            .find(|profile| profile.id == "default")
            .unwrap();
        let image_url = profile.image.expect("profile image URL");
        assert!(image_url.starts_with("/profile-images/"));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("data:image/")
        );
        store
            .update_profile("default", "Renamed", "#7c8f6a", Some(&image_url))
            .unwrap();
        assert_eq!(
            store.profiles()[0].image.as_deref(),
            Some(image_url.as_str())
        );
        let image_path = path
            .parent()
            .unwrap()
            .join(image_url.trim_start_matches('/'));
        assert!(image_path.is_file());

        let _ = std::fs::remove_file(image_path);
        let _ = std::fs::remove_dir(path.parent().unwrap().join("profile-images"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_inline_profile_image_is_migrated_on_store_open() {
        let path = temp_settings_path("profile-image-migration");
        let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let mut settings = PersistedSettings::default();
        let profile = ListeningProfile {
            id: "legacy-profile".to_string(),
            name: "Legacy".to_string(),
            image: Some(image.to_string()),
            ..ListeningProfile::default()
        };
        settings.profiles = Some(vec![profile]);
        settings.save(&path).unwrap();

        let store = SettingsStore::new(path.clone());
        let profile = store.profiles().into_iter().next().unwrap();
        let image_url = profile.image.expect("migrated profile image URL");
        assert!(image_url.starts_with("/profile-images/"));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("data:image/")
        );
        let image_path = path
            .parent()
            .unwrap()
            .join(image_url.trim_start_matches('/'));
        assert!(image_path.is_file());

        let _ = std::fs::remove_file(image_path);
        remove_settings_files(&path);
    }

    #[test]
    fn corrupt_primary_is_quarantined_and_valid_backup_is_restored() {
        let path = temp_settings_path("recover-backup");
        PersistedSettings {
            qobuz_radio_enabled: Some(false),
            ..PersistedSettings::default()
        }
        .save(&path)
        .unwrap();
        PersistedSettings {
            qobuz_radio_enabled: Some(true),
            ..PersistedSettings::default()
        }
        .save(&path)
        .unwrap();
        std::fs::write(&path, "{ broken").unwrap();

        let recovered = PersistedSettings::try_load(&path).unwrap();
        assert_eq!(recovered.qobuz_radio_enabled, Some(false));
        assert!(
            serde_json::from_str::<PersistedSettings>(&std::fs::read_to_string(&path).unwrap())
                .is_ok()
        );
        let prefix = format!("{}.corrupt-", path.file_name().unwrap().to_string_lossy());
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        );
        remove_settings_files(&path);
    }

    #[test]
    fn corrupt_primary_without_backup_fails_closed_across_restarts() {
        let path = temp_settings_path("fail-closed");
        std::fs::write(&path, "{ broken").unwrap();

        assert!(PersistedSettings::try_load(&path).is_err());
        assert!(PersistedSettings::try_load(&path).is_err());
        assert!(!path.exists());
        assert!(
            path.with_file_name(format!(
                "{}.recovery-required",
                path.file_name().unwrap().to_string_lossy()
            ))
            .exists()
        );
        remove_settings_files(&path);
    }

    #[test]
    fn installation_namespace_is_stable_across_data_path_changes() {
        let path_a = temp_settings_path("stable-a");
        let path_b = temp_settings_path("stable-b");
        let store_a = SettingsStore::try_new_with_namespace(path_a.clone(), "install-123").unwrap();
        let store_b = SettingsStore::try_new_with_namespace(path_b.clone(), "install-123").unwrap();
        assert_eq!(
            store_a.pairing_token_records_secret_key().account(),
            store_b.pairing_token_records_secret_key().account()
        );
        remove_settings_files(&path_a);
        remove_settings_files(&path_b);
    }
}
