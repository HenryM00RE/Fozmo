use crate::app::rate_limit::AuthRateLimiter;
use crate::app::server_remote::RemoteAccessController;
use crate::audio::airplay;
use crate::audio::sonos;
use crate::audio::transcode::LocalTranscodeService;
use crate::audio::upnp;
use crate::diagnostics::status::DiagnosticsService;
use crate::library::Library;
use crate::listening::ListeningTracker;
use crate::playback::config_applicator::PlaybackConfigApplicator;
use crate::playback::sequencer::PlaybackCommandSequencer;
use crate::secrets::{SecretKey, SecretsStore};
#[cfg(feature = "apple_music_capture")]
use crate::services::apple_music::AppleMusicCaptureService;
use crate::services::hegel;
use crate::services::lastfm::LastFmService;
use crate::services::qobuz::QobuzService;
use crate::settings::SettingsStore;
use crate::zones::{PairingManager, ZoneManager};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    settings: Arc<SettingsStore>,
    secrets: Arc<dyn SecretsStore>,
    library: Arc<Library>,
    listening: Arc<ListeningTracker>,
    qobuz: Arc<QobuzService>,
    lastfm: Arc<LastFmService>,
    #[cfg(feature = "apple_music_capture")]
    apple_music_capture: Arc<AppleMusicCaptureService>,
    airplay: Arc<airplay::AirPlayRegistry>,
    sonos: Arc<sonos::SonosService>,
    upnp: Arc<upnp::UpnpRendererService>,
    local_transcode: Arc<LocalTranscodeService>,
    playback_sequencer: PlaybackCommandSequencer,
    playback_config_applicator: PlaybackConfigApplicator,
    zones: ZoneManager,
    pairing: PairingManager,
    diagnostics: DiagnosticsService,
    hegel_status: hegel::HegelStatusCache,
    remote_access: RemoteAccessController,
    remote_auth_limiter: Arc<AuthRateLimiter>,
    public_base_url: String,
    music_dir: PathBuf,
    presets_dir: PathBuf,
    built_in_presets_dir: PathBuf,
    appearance_assets_dir: PathBuf,
    settings_path: PathBuf,
    backups_dir: PathBuf,
}

pub(crate) struct AppCoreServices {
    pub(crate) settings: Arc<SettingsStore>,
    pub(crate) secrets: Arc<dyn SecretsStore>,
    pub(crate) library: Arc<Library>,
    pub(crate) listening: Arc<ListeningTracker>,
}

pub(crate) struct AppMediaServices {
    pub(crate) qobuz: Arc<QobuzService>,
    pub(crate) lastfm: Arc<LastFmService>,
    #[cfg(feature = "apple_music_capture")]
    pub(crate) apple_music_capture: Arc<AppleMusicCaptureService>,
    pub(crate) airplay: Arc<airplay::AirPlayRegistry>,
    pub(crate) sonos: Arc<sonos::SonosService>,
    pub(crate) upnp: Arc<upnp::UpnpRendererService>,
    pub(crate) local_transcode: Arc<LocalTranscodeService>,
}

pub(crate) struct AppPlaybackServices {
    pub(crate) playback_sequencer: PlaybackCommandSequencer,
    pub(crate) playback_config_applicator: PlaybackConfigApplicator,
    pub(crate) zones: ZoneManager,
}

pub(crate) struct AppRuntimeServices {
    pub(crate) pairing: PairingManager,
    pub(crate) diagnostics: DiagnosticsService,
    pub(crate) hegel_status: hegel::HegelStatusCache,
    pub(crate) remote_access: RemoteAccessController,
}

pub(crate) struct AppStatePaths {
    pub(crate) public_base_url: String,
    pub(crate) music_dir: PathBuf,
    pub(crate) presets_dir: PathBuf,
    pub(crate) built_in_presets_dir: PathBuf,
    pub(crate) appearance_assets_dir: PathBuf,
    pub(crate) settings_path: PathBuf,
    pub(crate) backups_dir: PathBuf,
}

impl AppState {
    pub(crate) fn new(
        core: AppCoreServices,
        media: AppMediaServices,
        playback: AppPlaybackServices,
        runtime: AppRuntimeServices,
        paths: AppStatePaths,
    ) -> Self {
        Self {
            settings: core.settings,
            secrets: core.secrets,
            library: core.library,
            listening: core.listening,
            qobuz: media.qobuz,
            lastfm: media.lastfm,
            #[cfg(feature = "apple_music_capture")]
            apple_music_capture: media.apple_music_capture,
            airplay: media.airplay,
            sonos: media.sonos,
            upnp: media.upnp,
            local_transcode: media.local_transcode,
            playback_sequencer: playback.playback_sequencer,
            playback_config_applicator: playback.playback_config_applicator,
            zones: playback.zones,
            pairing: runtime.pairing,
            diagnostics: runtime.diagnostics,
            hegel_status: runtime.hegel_status,
            remote_access: runtime.remote_access,
            remote_auth_limiter: Arc::new(AuthRateLimiter::new()),
            public_base_url: paths.public_base_url,
            music_dir: paths.music_dir,
            presets_dir: paths.presets_dir,
            built_in_presets_dir: paths.built_in_presets_dir,
            appearance_assets_dir: paths.appearance_assets_dir,
            settings_path: paths.settings_path,
            backups_dir: paths.backups_dir,
        }
    }

    pub(crate) fn settings(&self) -> &Arc<SettingsStore> {
        &self.settings
    }

    pub(crate) fn secrets(&self) -> &Arc<dyn SecretsStore> {
        &self.secrets
    }

    pub(crate) fn lastfm_api_key(&self) -> Option<String> {
        self.secrets
            .get(SecretKey::LastFmApiKey)
            .ok()
            .flatten()
            .and_then(|value| normalize_secret(Some(value.expose_secret())))
            .or_else(lastfm_api_key_from_env)
    }

    pub(crate) fn lastfm_api_key_source(&self) -> Option<&'static str> {
        if self
            .secrets
            .get(SecretKey::LastFmApiKey)
            .ok()
            .flatten()
            .and_then(|value| normalize_secret(Some(value.expose_secret())))
            .is_some()
        {
            Some("secret_store")
        } else if lastfm_api_key_from_env().is_some() {
            Some("env")
        } else {
            None
        }
    }

    pub(crate) fn lastfm_radio_active(&self) -> bool {
        self.settings.lastfm_radio_enabled() && self.lastfm_api_key().is_some()
    }

    pub(crate) fn library(&self) -> &Arc<Library> {
        &self.library
    }

    pub(crate) fn listening(&self) -> &Arc<ListeningTracker> {
        &self.listening
    }

    pub(crate) fn qobuz(&self) -> &Arc<QobuzService> {
        &self.qobuz
    }

    pub(crate) fn lastfm(&self) -> &Arc<LastFmService> {
        &self.lastfm
    }

    #[cfg(feature = "apple_music_capture")]
    pub(crate) fn apple_music_capture(&self) -> &Arc<AppleMusicCaptureService> {
        &self.apple_music_capture
    }

    pub(crate) fn airplay(&self) -> &Arc<airplay::AirPlayRegistry> {
        &self.airplay
    }

    pub(crate) fn sonos(&self) -> &Arc<sonos::SonosService> {
        &self.sonos
    }

    pub(crate) fn upnp(&self) -> &Arc<upnp::UpnpRendererService> {
        &self.upnp
    }

    pub(crate) fn local_transcode(&self) -> &Arc<LocalTranscodeService> {
        &self.local_transcode
    }

    pub(crate) fn zones(&self) -> &ZoneManager {
        &self.zones
    }

    pub(crate) fn pairing(&self) -> &PairingManager {
        &self.pairing
    }

    pub(crate) fn playback_sequencer(&self) -> &PlaybackCommandSequencer {
        &self.playback_sequencer
    }

    pub(crate) fn playback_config_applicator(&self) -> &PlaybackConfigApplicator {
        &self.playback_config_applicator
    }

    pub(crate) fn diagnostics(&self) -> &DiagnosticsService {
        &self.diagnostics
    }

    pub(crate) fn hegel_status(&self) -> &hegel::HegelStatusCache {
        &self.hegel_status
    }

    pub(crate) fn remote_access(&self) -> &RemoteAccessController {
        &self.remote_access
    }

    pub(crate) fn remote_auth_limiter(&self) -> &Arc<AuthRateLimiter> {
        &self.remote_auth_limiter
    }

    pub(crate) fn public_base_url(&self) -> &String {
        &self.public_base_url
    }

    pub(crate) fn music_dir(&self) -> &Path {
        &self.music_dir
    }

    pub(crate) fn presets_dir(&self) -> &Path {
        &self.presets_dir
    }

    pub(crate) fn built_in_presets_dir(&self) -> &Path {
        &self.built_in_presets_dir
    }

    pub(crate) fn appearance_assets_dir(&self) -> &Path {
        &self.appearance_assets_dir
    }

    /// Flush the SQLite WAL without stopping playback or the HTTP server.
    /// The graceful-shutdown owner should call this after it stops accepting
    /// state-changing work.
    pub fn checkpoint_persistence(&self) -> Result<(), String> {
        self.library.checkpoint()
    }

    /// Create a validated SQLite/settings snapshot. Used by the updater before
    /// it replaces the app and available to maintenance/control surfaces.
    pub fn create_persistence_backup(&self, reason: &str) -> Result<PathBuf, String> {
        self.library
            .create_backup(&self.settings_path, &self.backups_dir, reason)
    }
}

fn lastfm_api_key_from_env() -> Option<String> {
    std::env::var(crate::app::identity::env_key("LASTFM_API_KEY"))
        .ok()
        .and_then(|value| normalize_secret(Some(&value)))
}

fn normalize_secret(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
