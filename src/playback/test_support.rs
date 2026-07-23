use crate::app::state::{
    AppCoreServices, AppMediaServices, AppPlaybackServices, AppRuntimeServices, AppState,
    AppStatePaths,
};
use crate::audio::airplay;
use crate::audio::player::Player;
use crate::audio::sonos;
use crate::audio::upnp;
use crate::diagnostics::status::DiagnosticsService;
use crate::library::Library;
use crate::listening::ListeningTracker;
use crate::playback::config_applicator::PlaybackConfigApplicator;
use crate::playback::sequencer::PlaybackCommandSequencer;
use crate::protocol::{AgentCapabilities, OutputDeviceCapabilities, SourceRef};
use crate::secrets::{MemorySecretsStore, SecretsStore};
#[cfg(feature = "apple_music_capture")]
use crate::services::apple_music::AppleMusicCaptureService;
#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
use crate::services::apple_music_musickit::AppleMusicService;
use crate::services::hegel::HegelStatusCache;
use crate::services::lastfm::LastFmService;
use crate::services::qobuz::QobuzService;
use crate::settings::SettingsStore;
use crate::zones::{PairingManager, ZoneManager};
use std::sync::Arc;

pub(crate) fn app_state(name: &str) -> AppState {
    app_state_with_pairing(name, false, false)
}

pub(crate) fn app_state_with_pairing(
    name: &str,
    pairing_required: bool,
    allow_query_token_auth: bool,
) -> AppState {
    let root = std::env::temp_dir().join(format!(
        "fozmo-playback-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let settings = Arc::new(SettingsStore::new(root.join("settings.json")));
    let secrets: Arc<dyn SecretsStore> = Arc::new(MemorySecretsStore::new());
    let player = Arc::new(Player::new());
    let zones = ZoneManager::new(Arc::clone(&player), None);
    let library = Arc::new(
        Library::new(
            root.join("library.db"),
            vec![root.join("music")],
            root.join("art"),
        )
        .unwrap(),
    );
    AppState::new(
        AppCoreServices {
            settings: Arc::clone(&settings),
            secrets: Arc::clone(&secrets),
            library,
            listening: Arc::new(ListeningTracker::default()),
        },
        AppMediaServices {
            qobuz: Arc::new(
                QobuzService::new(root.join("qobuz-cache"), Arc::clone(&secrets)).unwrap(),
            ),
            lastfm: Arc::new(LastFmService::new().unwrap()),
            #[cfg(feature = "apple_music_capture")]
            apple_music_capture: Arc::new(AppleMusicCaptureService::new(Arc::clone(&player))),
            #[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
            apple_music: Arc::new(AppleMusicService::new(&root, &root.join("cache"))),
            airplay: Arc::new(airplay::AirPlayRegistry::new()),
            sonos: Arc::new(
                sonos::SonosService::new(root.join("sonos-cache"), "http://core.test".to_string())
                    .unwrap(),
            ),
            upnp: Arc::new(upnp::UpnpRendererService::new(
                "http://core.test".to_string(),
            )),
            local_transcode: Arc::new(crate::audio::transcode::LocalTranscodeService::new(
                root.join("transcode-cache"),
            )),
        },
        AppPlaybackServices {
            playback_sequencer: PlaybackCommandSequencer::default(),
            playback_config_applicator: PlaybackConfigApplicator::default(),
            zones,
        },
        AppRuntimeServices {
            pairing: PairingManager::new(
                settings,
                Arc::clone(&secrets),
                pairing_required,
                crate::zones::DEFAULT_PAIRING_TOKEN_TTL_SECS,
                allow_query_token_auth,
            ),
            diagnostics: DiagnosticsService::new(),
            hegel_status: HegelStatusCache::default(),
            remote_access: crate::app::server_remote::RemoteAccessController::new(
                &crate::app::paths::AppPaths::from_workspace_dir(&root),
                3000,
                "test-installation",
            ),
        },
        AppStatePaths {
            public_base_url: "http://core.test".to_string(),
            music_dir: root.join("music"),
            presets_dir: root.join("presets"),
            built_in_presets_dir: root.join("presets"),
            appearance_assets_dir: root.join("static").join("user-fonts"),
            settings_path: root.join("settings.json"),
            backups_dir: root.join("backups"),
        },
    )
}

pub(crate) fn agent_capabilities(device_name: &str) -> AgentCapabilities {
    AgentCapabilities {
        output_devices: vec![device_name.to_string()],
        output_device_capabilities: vec![OutputDeviceCapabilities {
            name: device_name.to_string(),
            backend: Some("coreaudio".to_string()),
            max_sample_rate: 192_000,
            max_bit_depth: 32,
            supports_dsd128: false,
            supports_dsd256: false,
        }],
        max_sample_rate: 192_000,
        max_bit_depth: 32,
        exclusive_supported: false,
        supports_dsd128: false,
        supports_dsd256: false,
        browser: false,
    }
}

pub(crate) fn qobuz_source(track_id: u64, radio: bool) -> SourceRef {
    SourceRef::QobuzTrack {
        track_id,
        title: Some(format!("Track {track_id}")),
        artist: Some("Artist".to_string()),
        album: Some("Album".to_string()),
        album_id: Some("album".to_string()),
        image_url: None,
        duration_secs: Some(180.0),
        radio,
        radio_context: None,
        playlist_context: None,
    }
}
