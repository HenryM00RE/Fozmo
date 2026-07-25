use super::ipc::{read_json_frame, write_json_frame};
use super::model::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleMusicComparisonReference,
    AppleMusicComparisonStatus, AppleMusicComparisonTrack, AppleMusicMvpError, AppleMusicMvpState,
    AppleMusicMvpStatus, ApplePlaybackSnapshot, EXPECTED_HELPER_BUNDLE_ID, HelperMessage,
    HelperQueueItem, PROTOCOL_VERSION, SetQueueCommand,
};
use super::process_tap::{
    ProcessTapController, active_musickit_renderer_pids as discover_active_musickit_renderers,
};
use crate::audio::player::Player;
use crate::protocol::SourceRef;
use async_trait::async_trait;
use rand::{RngCore, rngs::OsRng};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::time::{Instant, timeout, timeout_at};

const HELPER_EXECUTABLE: &str = "FozmoAppleMusicHelper";
const HELPER_APP: &str = "FozmoAppleMusicHelper.app";
const HELPER_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const HELPER_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct AppleMusicService {
    helper_path: PathBuf,
    runtime_root: PathBuf,
    status: Arc<Mutex<AppleMusicMvpStatus>>,
    process_tap: Mutex<ProcessTapController>,
    comparison: Mutex<ComparisonSession>,
    playback: Arc<Mutex<Option<ApplePlaybackSnapshot>>>,
    comparison_switch: AsyncMutex<()>,
    connection: AsyncMutex<Option<HelperConnection>>,
    next_command_id: AtomicU64,
    next_queue_revision: AtomicU64,
    system_audio_capture_confirmed: AtomicBool,
}

#[allow(dead_code)]
#[async_trait]
pub(crate) trait AppleMusicHelperClient: Send + Sync {
    async fn lookup_song(
        &self,
        song_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogSong, AppleMusicMvpError>;
    async fn lookup_album(
        &self,
        album_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogAlbum, AppleMusicMvpError>;
    async fn search_songs(
        &self,
        term: String,
        storefront: Option<String>,
        limit: u32,
    ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError>;
    async fn set_queue(
        &self,
        sources: Vec<SourceRef>,
        start_index: usize,
    ) -> Result<u64, AppleMusicMvpError>;
    async fn play(&self) -> Result<(), AppleMusicMvpError>;
    async fn pause(&self) -> Result<(), AppleMusicMvpError>;
    async fn resume(&self) -> Result<(), AppleMusicMvpError>;
    async fn seek(&self, seconds: f64) -> Result<(), AppleMusicMvpError>;
    async fn skip_next(&self) -> Result<(), AppleMusicMvpError>;
    async fn stop(&self) -> Result<(), AppleMusicMvpError>;
    fn snapshot(&self) -> AppleMusicMvpStatus;
}

#[derive(Debug, Clone)]
pub(crate) struct AppleMusicComparisonReferenceState {
    pub(crate) zone_id: String,
    pub(crate) zone_name: String,
    pub(crate) profile_id: String,
    pub(crate) source: SourceRef,
    pub(crate) queue: Vec<SourceRef>,
    pub(crate) position_secs: f64,
}

#[derive(Default)]
struct ComparisonSession {
    reference: Option<AppleMusicComparisonReferenceState>,
    status: AppleMusicComparisonStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplePlaybackEventEffect {
    pub(crate) stale: bool,
    pub(crate) advance_by: usize,
    pub(crate) finished_reason: Option<String>,
}

impl ApplePlaybackEventEffect {
    fn stale() -> Self {
        Self {
            stale: true,
            advance_by: 0,
            finished_reason: None,
        }
    }
}

struct HelperConnection {
    child: Child,
    writer: OwnedWriteHalf,
    events: broadcast::Sender<HelperMessage>,
    session_id: String,
    socket_path: PathBuf,
}

impl AppleMusicService {
    pub(crate) fn new(resource_dir: &Path, cache_dir: &Path) -> Self {
        let helper_path = helper_executable_path(resource_dir);
        let helper_present = helper_path.is_file();
        Self {
            helper_path,
            runtime_root: cache_dir.join("apple-music"),
            status: Arc::new(Mutex::new(AppleMusicMvpStatus::new(helper_present))),
            process_tap: Mutex::new(ProcessTapController::default()),
            comparison: Mutex::new(ComparisonSession::default()),
            playback: Arc::new(Mutex::new(None)),
            comparison_switch: AsyncMutex::new(()),
            connection: AsyncMutex::new(None),
            next_command_id: AtomicU64::new(1),
            next_queue_revision: AtomicU64::new(1),
            system_audio_capture_confirmed: AtomicBool::new(false),
        }
    }

    pub(crate) fn status(&self) -> AppleMusicMvpStatus {
        let helper_present = self.helper_path.is_file();
        let process_tap = self.process_tap.lock().unwrap().status();
        let mut comparison = self.comparison.lock().unwrap().status.clone();
        if process_tap.state == "running" {
            comparison.active_side = "apple_music".to_string();
        } else if comparison.active_side == "apple_music" {
            comparison.active_side = "idle".to_string();
        }
        let mut status = self.status.lock().unwrap();
        status.helper_present = helper_present;
        status.process_tap = process_tap;
        status.playback_session = self.playback.lock().unwrap().clone();
        status.comparison = comparison;
        if status.helper_pid.is_none() {
            if helper_present && status.state == AppleMusicMvpState::HelperMissing {
                status.state = AppleMusicMvpState::Stopped;
            } else if !helper_present {
                status.state = AppleMusicMvpState::HelperMissing;
            }
        }
        status.clone()
    }

    pub(crate) fn start_process_tap(
        &self,
        player: Arc<Player>,
        confirm_system_audio_capture: bool,
        mute_original_audio: bool,
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        self.process_tap.lock().unwrap().start(
            player,
            confirm_system_audio_capture,
            mute_original_audio,
        )?;
        self.comparison.lock().unwrap().status.active_side = "apple_music".to_string();
        Ok(self.status())
    }

    pub(crate) fn prepare_process_tap(
        &self,
        player: Arc<Player>,
        confirm_system_audio_capture: bool,
        mute_original_audio: bool,
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        self.process_tap.lock().unwrap().prepare(
            player,
            confirm_system_audio_capture,
            mute_original_audio,
        )?;
        Ok(self.status())
    }

    pub(crate) fn confirm_system_audio_capture(&self, confirmed: bool) {
        if confirmed {
            self.system_audio_capture_confirmed
                .store(true, Ordering::Release);
        }
    }

    pub(crate) fn prepare_musickit_process_tap(
        &self,
        player: Arc<Player>,
        preexisting_renderer_pids: &[u32],
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        if !self.system_audio_capture_confirmed.load(Ordering::Acquire) {
            return Err(error(
                "process_tap_confirmation_required",
                "Confirm macOS system-audio capture in Apple Music settings before playback.",
                false,
                "permission",
                true,
            ));
        }
        self.status().helper_pid.ok_or_else(|| {
            error(
                "helper_exited",
                "The Apple Music helper is not connected.",
                true,
                "helper_connection",
                true,
            )
        })?;
        self.process_tap.lock().unwrap().prepare_musickit_renderer(
            player,
            true,
            preexisting_renderer_pids,
        )?;
        Ok(self.status())
    }

    pub(crate) fn active_musickit_renderer_pids(&self) -> Result<Vec<u32>, AppleMusicMvpError> {
        discover_active_musickit_renderers()
    }

    pub(crate) fn discard_process_tap_buffer(&self) -> Result<u64, AppleMusicMvpError> {
        self.process_tap.lock().unwrap().discard_buffered_audio()
    }

    pub(crate) fn process_tap_buffered_audio_secs(&self) -> Result<f64, AppleMusicMvpError> {
        self.process_tap.lock().unwrap().buffered_audio_secs()
    }

    pub(crate) fn prepare_process_tap_stream(&self) -> Result<(), AppleMusicMvpError> {
        self.process_tap.lock().unwrap().prepare_player_stream()
    }

    pub(crate) fn commit_process_tap(
        &self,
        preserve_output: bool,
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        self.process_tap.lock().unwrap().commit(preserve_output)?;
        self.comparison.lock().unwrap().status.active_side = "apple_music".to_string();
        Ok(self.status())
    }

    pub(crate) fn stop_process_tap(&self) -> AppleMusicMvpStatus {
        self.process_tap.lock().unwrap().stop();
        let mut comparison = self.comparison.lock().unwrap();
        if comparison.status.active_side == "apple_music" {
            comparison.status.active_side = "idle".to_string();
        }
        drop(comparison);
        self.status()
    }

    pub(crate) fn process_tap_playback_epoch(&self) -> Option<u64> {
        self.process_tap.lock().unwrap().playback_epoch()
    }

    pub(crate) fn activate_playback(
        &self,
        zone_id: String,
        player_epoch: u64,
        helper_session_id: String,
        queue_revision: u64,
        segment: Vec<SourceRef>,
    ) {
        *self.playback.lock().unwrap() = Some(ApplePlaybackSnapshot {
            zone_id,
            player_epoch,
            helper_session_id,
            queue_revision,
            segment,
            current_segment_index: 0,
            playback_state: "playing".to_string(),
            position_secs: 0.0,
            last_error: None,
        });
    }

    pub(crate) fn playback_snapshot_for_zone(
        &self,
        zone_id: &str,
    ) -> Option<ApplePlaybackSnapshot> {
        self.playback
            .lock()
            .unwrap()
            .as_ref()
            .filter(|snapshot| snapshot.zone_id == zone_id)
            .cloned()
    }

    pub(crate) fn clear_playback(&self, zone_id: &str, player_epoch: Option<u64>) -> bool {
        let mut playback = self.playback.lock().unwrap();
        let owns = playback.as_ref().is_some_and(|snapshot| {
            snapshot.zone_id == zone_id
                && player_epoch.is_none_or(|epoch| snapshot.player_epoch == epoch)
        });
        if owns {
            playback.take();
        }
        owns
    }

    pub(crate) fn update_playback_state(&self, zone_id: &str, state: &str) {
        if let Some(snapshot) = self
            .playback
            .lock()
            .unwrap()
            .as_mut()
            .filter(|snapshot| snapshot.zone_id == zone_id)
        {
            snapshot.playback_state = state.to_string();
        }
    }

    pub(crate) fn mark_playback_failed(
        &self,
        zone_id: &str,
        player_epoch: u64,
        failure: AppleMusicMvpError,
    ) {
        if let Some(snapshot) =
            self.playback.lock().unwrap().as_mut().filter(|snapshot| {
                snapshot.zone_id == zone_id && snapshot.player_epoch == player_epoch
            })
        {
            snapshot.playback_state = "failed".to_string();
            snapshot.last_error = Some(failure.clone());
        }
        self.record_error(failure);
    }

    pub(crate) fn apply_playback_event(&self, event: &HelperMessage) -> ApplePlaybackEventEffect {
        let mut playback = self.playback.lock().unwrap();
        let Some(snapshot) = playback.as_mut() else {
            return ApplePlaybackEventEffect::stale();
        };
        if event.session_id.as_deref() != Some(snapshot.helper_session_id.as_str())
            || event
                .queue_revision
                .is_some_and(|revision| revision != snapshot.queue_revision)
        {
            return ApplePlaybackEventEffect::stale();
        }
        if let Some(state) = event.playback_state.as_deref() {
            snapshot.playback_state = state.to_string();
        }
        if let Some(position) = event
            .playback_position
            .or(event.playback_time_secs)
            .filter(|position| position.is_finite() && *position >= 0.0)
        {
            snapshot.position_secs = position;
        }
        if event.message_type == "helper_error" || event.message_type == "interrupted" {
            let failure = error(
                event.code.as_deref().unwrap_or("apple_music_interrupted"),
                event
                    .message
                    .as_deref()
                    .unwrap_or("Apple Music playback was interrupted."),
                event.retryable.unwrap_or(true),
                "helper_event",
                false,
            );
            snapshot.last_error = Some(failure);
            snapshot.playback_state = "failed".to_string();
            return ApplePlaybackEventEffect {
                stale: false,
                advance_by: 0,
                finished_reason: Some("failed".to_string()),
            };
        }
        if event.message_type == "queue_finished" {
            snapshot.playback_state = "stopped".to_string();
            return ApplePlaybackEventEffect {
                stale: false,
                advance_by: 0,
                finished_reason: Some(
                    event
                        .finish_reason
                        .clone()
                        .unwrap_or_else(|| "completed".to_string()),
                ),
            };
        }
        if event.message_type != "entry_changed" && event.message_type != "queue_prepared" {
            return ApplePlaybackEventEffect {
                stale: false,
                advance_by: 0,
                finished_reason: None,
            };
        }
        let Some(index) = event.segment_index else {
            return ApplePlaybackEventEffect::stale();
        };
        if index >= snapshot.segment.len() || index < snapshot.current_segment_index {
            return ApplePlaybackEventEffect::stale();
        }
        let advance_by = index.saturating_sub(snapshot.current_segment_index);
        snapshot.current_segment_index = index;
        ApplePlaybackEventEffect {
            stale: false,
            advance_by,
            finished_reason: None,
        }
    }

    pub(crate) async fn lock_comparison_switch(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.comparison_switch.lock().await
    }

    pub(crate) fn comparison_reference(&self) -> Option<AppleMusicComparisonReferenceState> {
        self.comparison.lock().unwrap().reference.clone()
    }

    pub(crate) fn comparison_switched_to_apple(
        &self,
        reference: AppleMusicComparisonReferenceState,
        apple_music_track: AppleMusicComparisonTrack,
        match_position: bool,
    ) {
        let mut comparison = self.comparison.lock().unwrap();
        comparison.status.active_side = "apple_music".to_string();
        comparison.status.can_switch_to_fozmo = true;
        comparison.status.match_position = match_position;
        comparison.status.reference = Some(reference_status(&reference, reference.position_secs));
        comparison.status.apple_music_track = Some(apple_music_track);
        comparison.status.last_switch_message =
            Some("Apple Music is feeding the same Fozmo DSP/output path.".to_string());
        comparison.reference = Some(reference);
    }

    pub(crate) fn comparison_switched_to_fozmo(
        &self,
        apple_music_track: Option<AppleMusicComparisonTrack>,
        match_position: bool,
        position_secs: f64,
    ) {
        let mut comparison = self.comparison.lock().unwrap();
        comparison.status.active_side = "fozmo".to_string();
        comparison.status.can_switch_to_fozmo = comparison.reference.is_some();
        comparison.status.match_position = match_position;
        if let Some(reference) = comparison.reference.as_mut() {
            reference.position_secs = position_secs;
        }
        comparison.status.reference = comparison
            .reference
            .as_ref()
            .map(|reference| reference_status(reference, position_secs));
        if apple_music_track.is_some() {
            comparison.status.apple_music_track = apple_music_track;
        }
        comparison.status.last_switch_message =
            Some("The remembered Fozmo source is feeding the same DSP/output path.".to_string());
    }

    pub(crate) async fn launch(&self) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        self.ensure_launched().await?;
        Ok(self.status())
    }

    pub(crate) async fn refresh_status(&self) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        if self.connection.lock().await.is_none() {
            return Ok(self.status());
        }
        self.send_simple_command("get_status", &["ready"]).await?;
        Ok(self.status())
    }

    pub(crate) async fn authorize(
        &self,
        present_ui: bool,
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        self.ensure_launched().await?;
        self.set_state(AppleMusicMvpState::CheckingAuthorization);
        let mut command = self.next_command("authorize").await?;
        command.present_ui = Some(present_ui);
        self.send_and_wait(command, &["authorization_changed"])
            .await?;
        Ok(self.status())
    }

    pub(crate) async fn play_song(
        &self,
        song_id: String,
        storefront: Option<String>,
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        let song_id = song_id.trim().to_string();
        if song_id.is_empty() || song_id.len() > 256 {
            return Err(error(
                "song_not_found",
                "Enter a valid Apple Music song ID.",
                false,
                "validating_request",
                true,
            ));
        }
        let storefront = normalize_optional(storefront);
        self.prepare_queue(
            &[SourceRef::AppleMusicTrack {
                song_id,
                storefront,
                title: None,
                artist: None,
                album: None,
                album_artist: None,
                album_id: None,
                artwork_url: None,
                duration_secs: None,
                track_number: None,
                disc_number: None,
                isrc: None,
                radio: false,
                radio_context: None,
                playlist_context: None,
            }],
            0,
        )
        .await?;
        self.play_prepared().await?;
        Ok(self.status())
    }

    pub(crate) async fn lookup_song(
        &self,
        song_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogSong, AppleMusicMvpError> {
        let song_id = validate_catalog_id(song_id, "song_not_found")?;
        let storefront = validate_storefront(storefront)?;
        self.ensure_ready().await?;
        let mut command = self.next_command("lookup_song").await?;
        command.song_id = Some(song_id);
        command.storefront = storefront;
        let event = self.send_and_wait(command, &["catalog_song"]).await?;
        event.catalog_song.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty song response.",
                false,
                "catalog_lookup",
                true,
            )
        })
    }

    pub(crate) async fn lookup_album(
        &self,
        album_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogAlbum, AppleMusicMvpError> {
        let album_id = validate_catalog_id(album_id, "album_not_found")?;
        let storefront = validate_storefront(storefront)?;
        self.ensure_ready().await?;
        let mut command = self.next_command("lookup_album").await?;
        command.album_id = Some(album_id);
        command.storefront = storefront;
        let event = self.send_and_wait(command, &["catalog_album"]).await?;
        event.catalog_album.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty album response.",
                false,
                "catalog_lookup",
                true,
            )
        })
    }

    pub(crate) async fn search_songs(
        &self,
        term: String,
        storefront: Option<String>,
        limit: u32,
    ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError> {
        let term = validate_catalog_search_term(term)?;
        let storefront = validate_storefront(storefront)?;
        if !(1..=25).contains(&limit) {
            return Err(error(
                "catalog_search_limit_invalid",
                "Choose between 1 and 25 Apple Music search results.",
                false,
                "validating_request",
                true,
            ));
        }
        self.ensure_ready().await?;
        let mut command = self.next_command("search_songs").await?;
        command.term = Some(term);
        command.storefront = storefront;
        command.limit = Some(limit);
        let event = self.send_and_wait(command, &["catalog_search"]).await?;
        event.catalog_search.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty search response.",
                false,
                "catalog_search",
                true,
            )
        })
    }

    pub(crate) async fn prepare_queue(
        &self,
        sources: &[SourceRef],
        start_index: usize,
    ) -> Result<u64, AppleMusicMvpError> {
        if sources.is_empty() || sources.len() > 100 || start_index >= sources.len() {
            return Err(error(
                "queue_prepare_failed",
                "The Apple Music queue request is empty or invalid.",
                false,
                "validating_request",
                true,
            ));
        }
        let mut items = Vec::with_capacity(sources.len());
        for (segment_index, source) in sources.iter().enumerate() {
            let SourceRef::AppleMusicTrack {
                song_id,
                storefront,
                ..
            } = source
            else {
                return Err(error(
                    "queue_prepare_failed",
                    "A MusicKit queue segment can contain only Apple Music tracks.",
                    false,
                    "validating_request",
                    true,
                ));
            };
            items.push(HelperQueueItem {
                song_id: validate_catalog_id(song_id.clone(), "song_not_found")?,
                storefront: validate_storefront(storefront.clone())?,
                segment_index,
            });
        }
        self.ensure_ready().await?;
        self.set_state(AppleMusicMvpState::PreparingQueue);
        let command_id = self.command_id();
        let session_id = self.session_id().await?;
        let queue_revision = self.next_queue_revision.fetch_add(1, Ordering::Relaxed);
        let command = SetQueueCommand {
            v: PROTOCOL_VERSION,
            id: command_id.clone(),
            message_type: "set_queue",
            session_id,
            queue_revision,
            items,
            start_index,
        };
        self.send_serialized_and_wait(command_id, &command, &["queue_prepared"])
            .await?;
        Ok(queue_revision)
    }

    pub(crate) async fn play_prepared(&self) -> Result<(), AppleMusicMvpError> {
        self.send_simple_command("play", &["playback_state_changed"])
            .await?;
        Ok(())
    }

    pub(crate) async fn seek_helper(&self, seconds: f64) -> Result<(), AppleMusicMvpError> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(error(
                "apple_music_seek_invalid",
                "Apple Music seek position must be a non-negative number.",
                false,
                "validating_request",
                true,
            ));
        }
        let mut command = self.next_command("seek").await?;
        command.position_secs = Some(seconds);
        self.send_and_wait(command, &["playback_time", "playback_state_changed"])
            .await?;
        Ok(())
    }

    pub(crate) async fn skip_next_helper(&self) -> Result<(), AppleMusicMvpError> {
        self.send_simple_command("skip_next", &["entry_changed", "queue_finished"])
            .await?;
        Ok(())
    }

    pub(crate) async fn subscribe_events(
        &self,
    ) -> Result<broadcast::Receiver<HelperMessage>, AppleMusicMvpError> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|connection| connection.events.subscribe())
            .ok_or_else(|| {
                error(
                    "helper_exited",
                    "The Apple Music helper is not connected.",
                    true,
                    "helper_connection",
                    true,
                )
            })
    }

    pub(crate) async fn helper_session_id(&self) -> Result<String, AppleMusicMvpError> {
        self.session_id().await
    }

    pub(crate) async fn transport(
        &self,
        command: &str,
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        match command {
            "pause" | "resume" | "stop" => {
                self.send_simple_command(command, &["playback_state_changed"])
                    .await?;
                Ok(self.status())
            }
            "skip_next" => {
                self.skip_next_helper().await?;
                Ok(self.status())
            }
            "shutdown" => self.shutdown().await,
            _ => Err(error(
                "apple_music_unavailable",
                "That transport command is not available in the helper proof.",
                false,
                "validating_request",
                true,
            )),
        }
    }

    async fn ensure_launched(&self) -> Result<(), AppleMusicMvpError> {
        {
            let mut guard = self.connection.lock().await;
            if let Some(connection) = guard.as_mut() {
                match connection.child.try_wait() {
                    Ok(None) => return Ok(()),
                    Ok(Some(_)) | Err(_) => {
                        cleanup_socket(&connection.socket_path);
                        *guard = None;
                    }
                }
            }
        }

        if !self.helper_path.is_file() {
            self.set_state(AppleMusicMvpState::HelperMissing);
            return Err(error(
                "helper_missing",
                "The Fozmo Apple Music helper has not been built or bundled.",
                false,
                "launching_helper",
                true,
            ));
        }

        self.set_state(AppleMusicMvpState::LaunchingHelper);
        self.clear_error();
        let (listener, socket_path) = self.bind_private_socket()?;
        let session_id = format!("am-{}", random_hex(10));
        let token = random_hex(32);
        let canonical_helper = std::fs::canonicalize(&self.helper_path).map_err(|_| {
            error(
                "helper_missing",
                "The Fozmo Apple Music helper cannot be resolved.",
                false,
                "launching_helper",
                true,
            )
        })?;

        let mut child = Command::new(canonical_helper)
            .env("FOZMO_APPLE_MUSIC_SOCKET", &socket_path)
            .env("FOZMO_APPLE_MUSIC_TOKEN", &token)
            .env("FOZMO_APPLE_MUSIC_SESSION_ID", &session_id)
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                cleanup_socket(&socket_path);
                error(
                    "helper_launch_failed",
                    "Fozmo could not launch the Apple Music helper.",
                    true,
                    "launching_helper",
                    true,
                )
            })?;
        let launched_pid = match child.id() {
            Some(pid) => pid,
            None => {
                let _ = child.kill().await;
                cleanup_socket(&socket_path);
                let failure = error(
                    "helper_launch_failed",
                    "The Apple Music helper launched without a process identifier.",
                    true,
                    "launching_helper",
                    true,
                );
                self.record_error(failure.clone());
                return Err(failure);
            }
        };

        let (mut stream, _) = match timeout(HELPER_CONNECT_TIMEOUT, listener.accept()).await {
            Ok(Ok(connection)) => connection,
            _ => {
                let child_state = match child.try_wait() {
                    Ok(Some(status)) => format!("exited ({status})"),
                    Ok(None) => "still running".to_string(),
                    Err(_) => "unknown".to_string(),
                };
                tracing::warn!(
                    event = "apple_music_helper_connect_timeout",
                    child_state,
                    "Apple Music helper did not connect to its private IPC socket"
                );
                let _ = child.kill().await;
                cleanup_socket(&socket_path);
                let failure = error(
                    "helper_launch_failed",
                    "The Apple Music helper did not connect to Fozmo.",
                    true,
                    "launching_helper",
                    true,
                );
                self.record_error(failure.clone());
                return Err(failure);
            }
        };
        let hello: HelperMessage =
            match timeout(HELPER_CONNECT_TIMEOUT, read_json_frame(&mut stream)).await {
                Ok(Ok(hello)) => hello,
                _ => {
                    let _ = child.kill().await;
                    cleanup_socket(&socket_path);
                    let failure = error(
                        "helper_protocol_mismatch",
                        "The Apple Music helper sent an invalid handshake.",
                        false,
                        "launching_helper",
                        true,
                    );
                    self.record_error(failure.clone());
                    return Err(failure);
                }
            };
        if hello.v != PROTOCOL_VERSION
            || hello.message_type != "hello"
            || hello.session_id.as_deref() != Some(session_id.as_str())
            || hello.token.as_deref() != Some(token.as_str())
            || hello.pid != Some(launched_pid)
            || hello.bundle_id.as_deref() != Some(EXPECTED_HELPER_BUNDLE_ID)
        {
            let _ = child.kill().await;
            cleanup_socket(&socket_path);
            let failure = error(
                "helper_protocol_mismatch",
                "The Apple Music helper identity or protocol did not match.",
                false,
                "launching_helper",
                true,
            );
            self.record_error(failure.clone());
            return Err(failure);
        }

        let mut accept =
            HelperMessage::command("cmd-accept".to_string(), "accept", session_id.clone());
        accept.protocol_version = Some(PROTOCOL_VERSION);
        if write_json_frame(&mut stream, &accept).await.is_err() {
            let _ = child.kill().await;
            cleanup_socket(&socket_path);
            let failure = error(
                "helper_protocol_mismatch",
                "Fozmo could not accept the Apple Music helper connection.",
                true,
                "launching_helper",
                true,
            );
            self.record_error(failure.clone());
            return Err(failure);
        }

        let (mut reader, writer) = stream.into_split();
        let (events, _) = broadcast::channel(64);
        {
            let mut status = self.status.lock().unwrap();
            status.helper_pid = Some(launched_pid);
            status.helper_version = hello.helper_version.clone();
            status.helper_musickit_entitled = hello.musickit_entitled.unwrap_or(false);
            status.helper_capabilities = hello.capabilities.clone();
            status.session_id = Some(session_id.clone());
            status.state = AppleMusicMvpState::CheckingAuthorization;
            status.last_error = None;
        }
        let event_sender = events.clone();
        let shared_status = Arc::clone(&self.status);
        let shared_playback = Arc::clone(&self.playback);
        let reader_session_id = session_id.clone();
        tokio::spawn(async move {
            loop {
                match read_json_frame::<_, HelperMessage>(&mut reader).await {
                    Ok(message) if message.v == PROTOCOL_VERSION => {
                        apply_helper_event(&shared_status, &message);
                        let _ = event_sender.send(message);
                    }
                    Ok(_) => {
                        let failure = helper_connection_failure_event(
                            &reader_session_id,
                            &shared_playback,
                            "helper_protocol_mismatch",
                            "The Apple Music helper changed protocol versions.",
                            false,
                        );
                        record_shared_error(
                            &shared_status,
                            error(
                                "helper_protocol_mismatch",
                                "The Apple Music helper changed protocol versions.",
                                false,
                                "helper_connection",
                                false,
                            ),
                        );
                        let _ = event_sender.send(failure);
                        break;
                    }
                    Err(_) => {
                        let stopping = matches!(
                            shared_status.lock().unwrap().state,
                            AppleMusicMvpState::Stopping | AppleMusicMvpState::Stopped
                        );
                        if !stopping {
                            let failure = helper_connection_failure_event(
                                &reader_session_id,
                                &shared_playback,
                                "helper_exited",
                                "The Apple Music helper connection closed.",
                                true,
                            );
                            record_shared_error(
                                &shared_status,
                                error(
                                    "helper_exited",
                                    "The Apple Music helper connection closed.",
                                    true,
                                    "helper_connection",
                                    true,
                                ),
                            );
                            let _ = event_sender.send(failure);
                        }
                        break;
                    }
                }
            }
        });

        *self.connection.lock().await = Some(HelperConnection {
            child,
            writer,
            events,
            session_id,
            socket_path,
        });

        // The helper sends an unsolicited ready snapshot immediately after
        // accept. Waiting for an explicit status response keeps launch
        // deterministic even if that event raced with connection storage.
        self.send_simple_command("get_status", &["ready"]).await?;
        Ok(())
    }

    async fn ensure_ready(&self) -> Result<(), AppleMusicMvpError> {
        self.ensure_launched().await?;
        let status = self.status();
        if !status.helper_musickit_entitled {
            return Err(error(
                "musickit_capability_unavailable",
                "This helper is not signed with a development profile for the MusicKit-enabled App ID.",
                false,
                "checking_capability",
                true,
            ));
        }
        if status.authorization != "authorized" {
            return Err(error(
                "music_authorization_not_determined",
                "Authorize Apple Music before using the catalog.",
                false,
                "checking_authorization",
                true,
            ));
        }
        if status.can_play_catalog_content == Some(false) {
            return Err(error(
                "subscription_required",
                "This Apple Music account cannot play catalog content.",
                false,
                "checking_subscription",
                true,
            ));
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        if self.connection.lock().await.is_none() {
            return Ok(self.status());
        }
        self.set_state(AppleMusicMvpState::Stopping);
        let command_result = self.send_simple_command("shutdown", &["will_exit"]).await;
        let mut connection = self.connection.lock().await.take();
        if let Some(mut connection) = connection.take() {
            if timeout(Duration::from_secs(3), connection.child.wait())
                .await
                .is_err()
            {
                let _ = connection.child.kill().await;
                let _ = connection.child.wait().await;
            }
            cleanup_socket(&connection.socket_path);
        }
        {
            let mut status = self.status.lock().unwrap();
            status.helper_pid = None;
            status.session_id = None;
            status.playback_state = "stopped".to_string();
            status.playback_time_secs = None;
            status.now_playing = None;
            status.state = if self.helper_path.is_file() {
                AppleMusicMvpState::Stopped
            } else {
                AppleMusicMvpState::HelperMissing
            };
        }
        command_result?;
        Ok(self.status())
    }

    async fn send_simple_command(
        &self,
        message_type: &str,
        expected_events: &[&str],
    ) -> Result<HelperMessage, AppleMusicMvpError> {
        let command = self.next_command(message_type).await?;
        self.send_and_wait(command, expected_events).await
    }

    async fn next_command(&self, message_type: &str) -> Result<HelperMessage, AppleMusicMvpError> {
        let session_id = self.session_id().await?;
        Ok(HelperMessage::command(
            self.command_id(),
            message_type,
            session_id,
        ))
    }

    async fn session_id(&self) -> Result<String, AppleMusicMvpError> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|connection| connection.session_id.clone())
            .ok_or_else(|| {
                error(
                    "helper_exited",
                    "The Apple Music helper is not connected.",
                    true,
                    "helper_connection",
                    true,
                )
            })
    }

    fn command_id(&self) -> String {
        format!(
            "cmd-{}",
            self.next_command_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn send_and_wait(
        &self,
        command: HelperMessage,
        expected_events: &[&str],
    ) -> Result<HelperMessage, AppleMusicMvpError> {
        let command_id = command.id.clone().unwrap_or_default();
        self.send_serialized_and_wait(command_id, &command, expected_events)
            .await
    }

    async fn send_serialized_and_wait<T: serde::Serialize>(
        &self,
        command_id: String,
        command: &T,
        expected_events: &[&str],
    ) -> Result<HelperMessage, AppleMusicMvpError> {
        let mut receiver = {
            let mut guard = self.connection.lock().await;
            let connection = guard.as_mut().ok_or_else(|| {
                error(
                    "helper_exited",
                    "The Apple Music helper is not connected.",
                    true,
                    "helper_connection",
                    true,
                )
            })?;
            if connection.child.try_wait().ok().flatten().is_some() {
                return Err(error(
                    "helper_exited",
                    "The Apple Music helper exited.",
                    true,
                    "helper_connection",
                    true,
                ));
            }
            let receiver = connection.events.subscribe();
            write_json_frame(&mut connection.writer, command)
                .await
                .map_err(|_| {
                    error(
                        "helper_exited",
                        "Fozmo could not send a command to the Apple Music helper.",
                        true,
                        "helper_connection",
                        false,
                    )
                })?;
            receiver
        };

        let deadline = Instant::now() + HELPER_COMMAND_TIMEOUT;
        loop {
            let event = match timeout_at(deadline, receiver.recv()).await.map_err(|_| {
                error(
                    "helper_exited",
                    "The Apple Music helper did not confirm the command in time.",
                    true,
                    "helper_command",
                    false,
                )
            })? {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(error(
                        "helper_exited",
                        "The Apple Music helper event stream closed.",
                        true,
                        "helper_connection",
                        false,
                    ));
                }
            };
            if event.command_id.as_deref() != Some(command_id.as_str()) {
                continue;
            }
            if event.message_type == "helper_error" {
                let failure = error(
                    event.code.as_deref().unwrap_or("apple_music_unavailable"),
                    event
                        .message
                        .as_deref()
                        .unwrap_or("The Apple Music helper reported an error."),
                    event.retryable.unwrap_or(false),
                    "helper_command",
                    true,
                );
                self.record_error(failure.clone());
                return Err(failure);
            }
            if expected_events.contains(&event.message_type.as_str()) {
                return Ok(event);
            }
        }
    }

    fn bind_private_socket(&self) -> Result<(UnixListener, PathBuf), AppleMusicMvpError> {
        std::fs::create_dir_all(&self.runtime_root).map_err(|_| {
            error(
                "helper_launch_failed",
                "Fozmo could not create the private Apple Music runtime directory.",
                true,
                "launching_helper",
                true,
            )
        })?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.runtime_root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| {
                error(
                    "helper_launch_failed",
                    "Fozmo could not protect the Apple Music runtime directory.",
                    false,
                    "launching_helper",
                    true,
                )
            })?;
        let socket_path = self.runtime_root.join(format!("am-{}.sock", random_hex(6)));
        if socket_path.as_os_str().as_encoded_bytes().len() >= 100 {
            return Err(error(
                "helper_launch_failed",
                "The Apple Music runtime path is too long for a private socket.",
                false,
                "launching_helper",
                true,
            ));
        }
        let listener = UnixListener::bind(&socket_path).map_err(|_| {
            error(
                "helper_launch_failed",
                "Fozmo could not open the private Apple Music IPC socket.",
                true,
                "launching_helper",
                true,
            )
        })?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |_| {
                cleanup_socket(&socket_path);
                error(
                    "helper_launch_failed",
                    "Fozmo could not protect the Apple Music IPC socket.",
                    false,
                    "launching_helper",
                    true,
                )
            },
        )?;
        Ok((listener, socket_path))
    }

    fn set_state(&self, state: AppleMusicMvpState) {
        self.status.lock().unwrap().state = state;
    }

    fn clear_error(&self) {
        self.status.lock().unwrap().last_error = None;
    }

    fn record_error(&self, failure: AppleMusicMvpError) {
        record_shared_error(&self.status, failure);
    }
}

#[async_trait]
impl AppleMusicHelperClient for AppleMusicService {
    async fn lookup_song(
        &self,
        song_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogSong, AppleMusicMvpError> {
        AppleMusicService::lookup_song(self, song_id, storefront).await
    }

    async fn lookup_album(
        &self,
        album_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogAlbum, AppleMusicMvpError> {
        AppleMusicService::lookup_album(self, album_id, storefront).await
    }

    async fn search_songs(
        &self,
        term: String,
        storefront: Option<String>,
        limit: u32,
    ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError> {
        AppleMusicService::search_songs(self, term, storefront, limit).await
    }

    async fn set_queue(
        &self,
        sources: Vec<SourceRef>,
        start_index: usize,
    ) -> Result<u64, AppleMusicMvpError> {
        self.prepare_queue(&sources, start_index).await
    }

    async fn play(&self) -> Result<(), AppleMusicMvpError> {
        self.play_prepared().await
    }

    async fn pause(&self) -> Result<(), AppleMusicMvpError> {
        self.transport("pause").await.map(|_| ())
    }

    async fn resume(&self) -> Result<(), AppleMusicMvpError> {
        self.transport("resume").await.map(|_| ())
    }

    async fn seek(&self, seconds: f64) -> Result<(), AppleMusicMvpError> {
        self.seek_helper(seconds).await
    }

    async fn skip_next(&self) -> Result<(), AppleMusicMvpError> {
        self.skip_next_helper().await
    }

    async fn stop(&self) -> Result<(), AppleMusicMvpError> {
        self.transport("stop").await.map(|_| ())
    }

    fn snapshot(&self) -> AppleMusicMvpStatus {
        self.status()
    }
}

fn apply_helper_event(status: &Arc<Mutex<AppleMusicMvpStatus>>, event: &HelperMessage) {
    let mut status = status.lock().unwrap();
    status
        .recent_events
        .push(super::model::AppleMusicHelperEventSummary {
            event_type: event.message_type.clone(),
            queue_revision: event.queue_revision,
            segment_index: event.segment_index,
            song_id: event
                .song_id
                .clone()
                .or_else(|| event.now_playing.as_ref().map(|now| now.song_id.clone())),
            playback_position: event.playback_position.or(event.playback_time_secs),
            finish_reason: event.finish_reason.clone(),
            code: event.code.clone(),
        });
    if status.recent_events.len() > 50 {
        let drain = status.recent_events.len() - 50;
        status.recent_events.drain(..drain);
    }
    if let Some(authorization) = &event.authorization {
        status.authorization = authorization.clone();
    }
    if let Some(can_play) = event.can_play_catalog_content {
        status.can_play_catalog_content = Some(can_play);
    }
    if let Some(playback_state) = &event.playback_state {
        status.playback_state = playback_state.clone();
    }
    if let Some(playback_time) = event.playback_time_secs {
        status.playback_time_secs = Some(playback_time);
    }
    if let Some(queue_revision) = event.queue_revision {
        status.queue_revision = queue_revision;
    }
    if matches!(
        event.message_type.as_str(),
        "now_playing_changed" | "entry_changed" | "queue_prepared"
    ) {
        status.now_playing = event.now_playing.clone();
    }
    match event.message_type.as_str() {
        "ready" | "authorization_changed" => {
            status.state = if status.authorization == "authorized" {
                AppleMusicMvpState::Ready
            } else {
                AppleMusicMvpState::AwaitingAuthorization
            };
        }
        "queue_prepared" | "catalog_song" | "catalog_album" | "catalog_search" => {
            status.state = AppleMusicMvpState::Ready
        }
        "playback_state_changed" => {
            status.state = match status.playback_state.as_str() {
                "playing" => AppleMusicMvpState::Playing,
                "paused" => AppleMusicMvpState::Paused,
                _ => AppleMusicMvpState::Ready,
            };
            if status.playback_state == "stopped" {
                status.now_playing = None;
                status.playback_time_secs = None;
            }
        }
        "helper_error" => {
            let failure = error(
                event.code.as_deref().unwrap_or("apple_music_unavailable"),
                event
                    .message
                    .as_deref()
                    .unwrap_or("The Apple Music helper reported an error."),
                event.retryable.unwrap_or(false),
                "helper_event",
                true,
            );
            status.state = AppleMusicMvpState::Failed;
            status.last_error = Some(failure);
        }
        "will_exit" => status.state = AppleMusicMvpState::Stopping,
        _ => {}
    }
}

fn record_shared_error(status: &Arc<Mutex<AppleMusicMvpStatus>>, failure: AppleMusicMvpError) {
    let mut status = status.lock().unwrap();
    status.state = AppleMusicMvpState::Failed;
    status.last_error = Some(failure);
}

fn helper_executable_path(resource_dir: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("FOZMO_APPLE_MUSIC_HELPER") {
        return PathBuf::from(path);
    }
    if resource_dir.file_name().and_then(|name| name.to_str()) == Some("Resources") {
        return resource_dir
            .parent()
            .unwrap_or(resource_dir)
            .join("Helpers")
            .join(HELPER_APP)
            .join("Contents")
            .join("MacOS")
            .join(HELPER_EXECUTABLE);
    }
    resource_dir
        .join("target")
        .join("apple-music-helper")
        .join(HELPER_APP)
        .join("Contents")
        .join("MacOS")
        .join(HELPER_EXECUTABLE)
}

fn reference_status(
    reference: &AppleMusicComparisonReferenceState,
    position_secs: f64,
) -> AppleMusicComparisonReference {
    let (provider, title, artist, album) = match &reference.source {
        SourceRef::LocalTrack {
            title,
            artist,
            album,
            ..
        } => (
            "local".to_string(),
            title.clone(),
            artist.clone(),
            album.clone(),
        ),
        SourceRef::QobuzTrack {
            title,
            artist,
            album,
            ..
        } => (
            "qobuz".to_string(),
            title.clone(),
            artist.clone(),
            album.clone(),
        ),
        SourceRef::AppleMusicTrack {
            title,
            artist,
            album,
            ..
        } => (
            "apple_music".to_string(),
            title.clone(),
            artist.clone(),
            album.clone(),
        ),
    };
    AppleMusicComparisonReference {
        zone_id: reference.zone_id.clone(),
        zone_name: reference.zone_name.clone(),
        provider,
        title,
        artist,
        album,
        position_secs,
    }
}

fn cleanup_socket(socket_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
}

fn helper_connection_failure_event(
    session_id: &str,
    playback: &Mutex<Option<ApplePlaybackSnapshot>>,
    code: &str,
    message: &str,
    retryable: bool,
) -> HelperMessage {
    let mut event =
        HelperMessage::command("event".to_string(), "helper_error", session_id.to_string());
    event.id = None;
    event.command_id = None;
    event.code = Some(code.to_string());
    event.message = Some(message.to_string());
    event.retryable = Some(retryable);
    if let Some(snapshot) = playback
        .lock()
        .unwrap()
        .as_ref()
        .filter(|snapshot| snapshot.helper_session_id == session_id)
    {
        event.queue_revision = Some(snapshot.queue_revision);
        event.segment_index = Some(snapshot.current_segment_index);
        event.song_id = snapshot
            .current_source()
            .and_then(SourceRef::apple_music_song_id)
            .map(str::to_string);
    }
    event
}

fn random_hex(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut value);
    let mut output = String::with_capacity(bytes * 2);
    for byte in value {
        use std::fmt::Write;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn validate_catalog_id(
    value: String,
    error_code: &'static str,
) -> Result<String, AppleMusicMvpError> {
    let value = value.trim().to_string();
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(error(
            error_code,
            "Enter a valid Apple Music catalog ID.",
            false,
            "validating_request",
            true,
        ));
    }
    Ok(value)
}

fn validate_catalog_search_term(value: String) -> Result<String, AppleMusicMvpError> {
    let value = value.trim().to_string();
    if value.is_empty() || value.chars().count() > 200 || value.chars().any(char::is_control) {
        return Err(error(
            "catalog_search_term_invalid",
            "Enter an Apple Music search term between 1 and 200 characters.",
            false,
            "validating_request",
            true,
        ));
    }
    Ok(value)
}

fn validate_storefront(value: Option<String>) -> Result<Option<String>, AppleMusicMvpError> {
    let value = normalize_optional(value).map(|value| value.to_ascii_lowercase());
    if value.as_deref().is_some_and(|value| {
        value.len() > 8 || !value.bytes().all(|byte| byte.is_ascii_alphabetic())
    }) {
        return Err(error(
            "apple_music_storefront_invalid",
            "Enter a valid Apple Music storefront code.",
            false,
            "validating_request",
            true,
        ));
    }
    Ok(value)
}

fn error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
    stage: impl Into<String>,
    cleanup_complete: bool,
) -> AppleMusicMvpError {
    AppleMusicMvpError {
        code: code.into(),
        message: message.into(),
        retryable,
        stage: stage.into(),
        cleanup_complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SourceRef;

    #[test]
    fn source_checkout_helper_path_targets_a_real_app_bundle() {
        let path = helper_executable_path(Path::new("/repo/Fozmo"));
        assert_eq!(
            path,
            PathBuf::from(
                "/repo/Fozmo/target/apple-music-helper/FozmoAppleMusicHelper.app/Contents/MacOS/FozmoAppleMusicHelper"
            )
        );
    }

    #[test]
    fn packaged_helper_path_is_sibling_of_resources() {
        let path = helper_executable_path(Path::new("/Applications/Fozmo.app/Contents/Resources"));
        assert_eq!(
            path,
            PathBuf::from(
                "/Applications/Fozmo.app/Contents/Helpers/FozmoAppleMusicHelper.app/Contents/MacOS/FozmoAppleMusicHelper"
            )
        );
    }

    #[test]
    fn helper_event_rejects_stale_display_state_on_stop() {
        let status = Arc::new(Mutex::new(AppleMusicMvpStatus::new(true)));
        {
            let mut value = status.lock().unwrap();
            value.now_playing = Some(Default::default());
            value.playback_time_secs = Some(12.0);
        }
        let mut event = HelperMessage::command(
            "event".to_string(),
            "playback_state_changed",
            "session".to_string(),
        );
        event.playback_state = Some("stopped".to_string());
        apply_helper_event(&status, &event);
        let value = status.lock().unwrap();
        assert!(value.now_playing.is_none());
        assert!(value.playback_time_secs.is_none());
        assert_eq!(value.state, AppleMusicMvpState::Ready);
    }

    fn apple_source(song_id: &str) -> SourceRef {
        SourceRef::AppleMusicTrack {
            song_id: song_id.to_string(),
            storefront: Some("nz".to_string()),
            title: Some(format!("Song {song_id}")),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            album_id: Some("album-1".to_string()),
            artwork_url: None,
            duration_secs: Some(180.0),
            track_number: None,
            disc_number: None,
            isrc: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn event(message_type: &str, revision: u64, index: usize) -> HelperMessage {
        let mut event = HelperMessage::command(
            "event".to_string(),
            message_type,
            "helper-session".to_string(),
        );
        event.id = None;
        event.command_id = None;
        event.queue_revision = Some(revision);
        event.segment_index = Some(index);
        event.song_id = Some("same".to_string());
        event
    }

    #[test]
    fn stale_and_duplicate_events_cannot_advance_a_replaced_queue() {
        let service = AppleMusicService::new(Path::new("/missing"), Path::new("/tmp"));
        service.activate_playback(
            "local-core".to_string(),
            7,
            "helper-session".to_string(),
            11,
            vec![
                apple_source("same"),
                apple_source("same"),
                apple_source("tail"),
            ],
        );

        let stale = service.apply_playback_event(&event("entry_changed", 10, 1));
        assert!(stale.stale);

        let advanced = service.apply_playback_event(&event("entry_changed", 11, 1));
        assert!(!advanced.stale);
        assert_eq!(advanced.advance_by, 1);

        let duplicate = service.apply_playback_event(&event("entry_changed", 11, 1));
        assert!(!duplicate.stale);
        assert_eq!(duplicate.advance_by, 0);

        let jumped = service.apply_playback_event(&event("entry_changed", 11, 2));
        assert_eq!(jumped.advance_by, 1);
        assert_eq!(
            service
                .playback_snapshot_for_zone("local-core")
                .unwrap()
                .current_segment_index,
            2
        );
    }

    #[derive(Default)]
    struct FakeHelper {
        revision: AtomicU64,
        status: Mutex<Option<AppleMusicMvpStatus>>,
        queue: Mutex<Vec<SourceRef>>,
    }

    #[async_trait]
    impl AppleMusicHelperClient for FakeHelper {
        async fn lookup_song(
            &self,
            song_id: String,
            storefront: Option<String>,
        ) -> Result<AppleCatalogSong, AppleMusicMvpError> {
            Ok(AppleCatalogSong {
                song_id,
                storefront: storefront.unwrap_or_else(|| "nz".to_string()),
                title: "Fake song".to_string(),
                artist: "Fake artist".to_string(),
                ..AppleCatalogSong::default()
            })
        }

        async fn lookup_album(
            &self,
            album_id: String,
            storefront: Option<String>,
        ) -> Result<AppleCatalogAlbum, AppleMusicMvpError> {
            Ok(AppleCatalogAlbum {
                album_id,
                storefront: storefront.unwrap_or_else(|| "nz".to_string()),
                title: "Fake album".to_string(),
                artist: "Fake artist".to_string(),
                ..AppleCatalogAlbum::default()
            })
        }

        async fn search_songs(
            &self,
            term: String,
            storefront: Option<String>,
            limit: u32,
        ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError> {
            let storefront = storefront.unwrap_or_else(|| "nz".to_string());
            Ok(AppleCatalogSearchResult {
                term,
                storefront: storefront.clone(),
                songs: (0..limit)
                    .map(|index| AppleCatalogSong {
                        song_id: format!("song-{index}"),
                        storefront: storefront.clone(),
                        title: format!("Fake song {index}"),
                        artist: "Fake artist".to_string(),
                        ..AppleCatalogSong::default()
                    })
                    .collect(),
            })
        }

        async fn set_queue(
            &self,
            sources: Vec<SourceRef>,
            _start_index: usize,
        ) -> Result<u64, AppleMusicMvpError> {
            *self.queue.lock().unwrap() = sources;
            Ok(self.revision.fetch_add(1, Ordering::Relaxed) + 1)
        }

        async fn play(&self) -> Result<(), AppleMusicMvpError> {
            Ok(())
        }

        async fn pause(&self) -> Result<(), AppleMusicMvpError> {
            Ok(())
        }

        async fn resume(&self) -> Result<(), AppleMusicMvpError> {
            Ok(())
        }

        async fn seek(&self, _seconds: f64) -> Result<(), AppleMusicMvpError> {
            Ok(())
        }

        async fn skip_next(&self) -> Result<(), AppleMusicMvpError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), AppleMusicMvpError> {
            Ok(())
        }

        fn snapshot(&self) -> AppleMusicMvpStatus {
            self.status
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| AppleMusicMvpStatus::new(false))
        }
    }

    #[tokio::test]
    async fn fake_helper_exercises_catalog_and_queue_without_entitlement() {
        let fake = FakeHelper::default();
        let search = fake
            .search_songs("fake".to_string(), Some("nz".to_string()), 3)
            .await
            .unwrap();
        assert_eq!(search.term, "fake");
        assert_eq!(search.songs.len(), 3);
        let song = fake
            .lookup_song("2037093408".to_string(), Some("nz".to_string()))
            .await
            .unwrap();
        let revision = fake
            .set_queue(vec![song.source_ref(), song.source_ref()], 0)
            .await
            .unwrap();

        assert_eq!(revision, 1);
        assert_eq!(fake.queue.lock().unwrap().len(), 2);
        fake.play().await.unwrap();
        fake.pause().await.unwrap();
        fake.seek(30.0).await.unwrap();
        fake.resume().await.unwrap();
        fake.skip_next().await.unwrap();
        fake.stop().await.unwrap();
    }

    #[test]
    fn catalog_search_validation_accepts_unicode_and_rejects_unsafe_input() {
        assert_eq!(
            validate_catalog_search_term("  Björk – Jóga  ".to_string()).unwrap(),
            "Björk – Jóga"
        );
        assert!(validate_catalog_search_term(" \n ".to_string()).is_err());
        assert!(validate_catalog_search_term("hello\u{0000}world".to_string()).is_err());
        assert!(validate_catalog_search_term("x".repeat(201)).is_err());
    }
}
