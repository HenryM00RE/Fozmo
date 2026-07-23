use crate::app::state::AppState;
use crate::audio::device_volume;
use crate::audio::player::{TrackCover, TrackTags};
use crate::diagnostics::logging::sanitize_error;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackOutcome};
use crate::playback::now_playing::current_playback_matches_expected;
use crate::playback::request::{PlaybackGuard, PlaybackRequest};
use crate::playback::resolver::{
    local_player_queue_items_from_sources, lookup_fallback_cover, resolve_track_path,
};
use crate::playback::service::{
    airplay_volume_with_max, apply_playback_settings_for_zone, hegel_settings_for_zone,
    prepare_airplay_volume_for_zone, prepare_hegel_for_zone,
};
use crate::playback::source::qobuz_play_request_from_source_ref;
use crate::protocol::{SinkProtocol, SourceRef};
use crate::services::hegel;
use crate::services::qobuz::QobuzPlayRequest;
use std::future::Future;
use symphonia::core::io::MediaSource;
use tracing::warn;

struct SelectedQobuzStream {
    source: Box<dyn MediaSource>,
    ext: String,
    display_name: String,
}

pub(crate) struct LocalPlaybackSink<'a> {
    state: &'a AppState,
}

impl<'a> LocalPlaybackSink<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(super) async fn play(
        &self,
        zone_id: &str,
        request: PlaybackRequest,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        match &request.source {
            SourceRef::LocalTrack { .. } => self.play_local_track(zone_id, request).await,
            SourceRef::QobuzTrack { .. } => self.play_qobuz_stream(zone_id, request).await,
        }
    }

    pub(super) async fn next(
        &self,
        zone_id: &str,
        profile_id: String,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let mut queued_sources = self.state.listening().queued_sources(zone_id);
        if matches!(queued_sources.first(), Some(SourceRef::QobuzTrack { .. })) {
            let source = queued_sources.remove(0);
            return self
                .play(
                    zone_id,
                    PlaybackRequest {
                        profile_id,
                        source,
                        queue: queued_sources,
                        radio_auto: false,
                        guard: PlaybackGuard::none(),
                        qobuz_request: None,
                    },
                )
                .await;
        }

        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        player.next();
        self.state.listening().next(self.state.library(), zone_id);
        Ok(PlaybackOutcome::Completed)
    }

    pub(super) fn pause(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        player.pause();
        Ok(())
    }

    pub(super) async fn resume(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        prepare_hegel_for_zone(self.state, zone_id).await?;
        player.resume();
        Ok(())
    }

    pub(super) fn stop(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        player.stop();
        Ok(())
    }

    pub(super) fn seek(&self, zone_id: &str, seconds: f64) -> Result<(), PlaybackError> {
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        player.seek(seconds);
        Ok(())
    }

    pub(super) fn set_loop_mode(
        &self,
        zone_id: &str,
        mode: &LoopMode,
    ) -> Result<(), PlaybackError> {
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        player.set_repeat_one(mode.repeat_one());
        Ok(())
    }

    pub(super) fn set_volume(&self, _zone_id: &str, _volume: f32) -> Result<(), PlaybackError> {
        Ok(())
    }

    pub(super) async fn set_device_volume(
        &self,
        zone_id: &str,
        volume: f32,
    ) -> Result<(), PlaybackError> {
        if let Some(hegel_settings) = hegel_settings_for_zone(self.state, zone_id) {
            let hegel_volume = normalized_hegel_volume(&hegel_settings, volume);
            let status = hegel::set_volume(
                hegel_settings.host.as_deref().unwrap_or_default(),
                hegel_settings.port,
                hegel_volume,
            )
            .await
            .map_err(PlaybackError::integration)?;
            self.state.hegel_status().remember(status);
        } else if matches!(
            self.state.zones().zone_protocol(zone_id),
            Some(SinkProtocol::AirPlayRaop | SinkProtocol::AirPlay2)
        ) {
            let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                return Err(PlaybackError::ZoneNotAvailable);
            };
            let volume = self
                .state
                .library()
                .zone_settings(zone_id)
                .map(|settings| airplay_volume_with_max(&settings, volume))
                .unwrap_or_else(|_| volume.clamp(0.0, 1.0));
            player.set_airplay_device_volume(volume);
            let _ = self
                .state
                .library()
                .remember_zone_airplay_volume(zone_id, volume);
        } else {
            let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                return Err(PlaybackError::ZoneNotAvailable);
            };
            let device_name = player.selected_device_name();
            device_volume::set_output_device_volume(device_name.as_deref(), volume)
                .map_err(PlaybackError::bad_request)?;
        }
        Ok(())
    }

    async fn play_local_track(
        &self,
        zone_id: &str,
        request: PlaybackRequest,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let PlaybackRequest {
            profile_id,
            source,
            queue,
            radio_auto,
            guard,
            qobuz_request: _,
        } = request;
        let radio_auto = radio_auto || source.is_radio();
        let SourceRef::LocalTrack {
            track_id,
            file_name,
            ..
        } = &source
        else {
            return Err(PlaybackError::bad_request("Expected local source"));
        };
        let full_path = resolve_track_path(
            self.state,
            (*track_id > 0).then_some(*track_id),
            file_name.as_deref(),
        )?
        .ok_or_else(|| PlaybackError::bad_request("Missing file_name or track_id"))?;
        let path_str = full_path.to_string_lossy().to_string();
        let fallback_cover = lookup_fallback_cover(self.state, &path_str);
        let queue_items = local_player_queue_items_from_sources(self.state, &queue);
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        apply_playback_settings_for_zone(self.state, zone_id);
        prepare_airplay_volume_for_zone(self.state, zone_id, &player);
        prepare_hegel_for_zone(self.state, zone_id).await?;
        if !guard.is_current(self.state) {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        let play_epoch = player.reserve_playback_change();
        if !player.play_if_epoch(play_epoch, path_str, fallback_cover, None, queue_items) {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        self.persist_queue(zone_id, &queue);
        if radio_auto {
            self.state.listening().start_with_radio(
                self.state.library(),
                zone_id.to_string(),
                self.state.zones().zone_name(zone_id),
                profile_id,
                source,
                queue,
                true,
            );
        } else {
            self.state.listening().start(
                self.state.library(),
                zone_id.to_string(),
                self.state.zones().zone_name(zone_id),
                profile_id,
                source,
                queue,
            );
        }
        Ok(PlaybackOutcome::Completed)
    }

    async fn play_qobuz_stream(
        &self,
        zone_id: &str,
        mut request: PlaybackRequest,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let req = request
            .qobuz_request
            .take()
            .map(|request| *request)
            .or_else(|| {
                qobuz_play_request_from_source_ref(
                    &request.source,
                    &request.queue,
                    request.radio_auto,
                )
            })
            .ok_or_else(|| PlaybackError::bad_request("Qobuz source was not playable"))?;
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        apply_playback_settings_for_zone(self.state, zone_id);
        prepare_airplay_volume_for_zone(self.state, zone_id, &player);
        prepare_hegel_for_zone(self.state, zone_id).await?;
        if !current_playback_matches_expected(self.state, zone_id, &req.expected_current) {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        if !request.guard.is_current(self.state) {
            return Err(PlaybackError::conflict("Playback changed"));
        }

        let cover_fut = async {
            let url = req.image_url.as_deref()?;
            let (mime, data) = self.state.qobuz().fetch_cover_public(url).await.ok()?;
            match (mime, data) {
                (Some(mime), Some(data)) => Some(TrackCover { mime, data }),
                _ => None,
            }
        };
        let stream_fut = async {
            self.state
                .qobuz()
                .open_stream(&req)
                .await
                .map(|handle| SelectedQobuzStream {
                    source: Box::new(handle.source),
                    ext: handle.ext,
                    display_name: handle.display_name,
                })
        };

        self.complete_selected_qobuz_start(zone_id, request, &req, &player, stream_fut, cover_fut)
            .await
    }

    async fn complete_selected_qobuz_start<StreamFuture, CoverFuture>(
        &self,
        zone_id: &str,
        request: PlaybackRequest,
        req: &QobuzPlayRequest,
        player: &std::sync::Arc<crate::audio::player::Player>,
        stream_fut: StreamFuture,
        cover_fut: CoverFuture,
    ) -> Result<PlaybackOutcome, PlaybackError>
    where
        StreamFuture: Future<Output = Result<SelectedQobuzStream, String>>,
        CoverFuture: Future<Output = Option<TrackCover>>,
    {
        // Only assets for the selected track belong on the initial-playback
        // critical path. The monitor fills the next-track stream queue after
        // this source has been committed.
        let (stream_result, fallback_cover) = tokio::join!(stream_fut, cover_fut);
        let handle = stream_result.map_err(PlaybackError::retryable_network)?;
        if !current_playback_matches_expected(self.state, zone_id, &req.expected_current) {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        if !request.guard.is_current(self.state) {
            return Err(PlaybackError::conflict("Playback changed"));
        }

        let fallback_tags = TrackTags {
            title: req.title.clone(),
            artist: req.artist.clone(),
            album: req.album.clone(),
            album_artist: req.artist.clone(),
            duration_secs: req.duration_secs,
            ..TrackTags::default()
        };
        let play_epoch = player.reserve_playback_change();
        let started = player.play_stream_if_epoch(
            play_epoch,
            handle.source,
            Some(handle.ext),
            handle.display_name,
            fallback_cover,
            Some(fallback_tags),
            Vec::new(),
        );
        if !started {
            return Err(PlaybackError::conflict("Playback changed"));
        }

        let PlaybackRequest {
            profile_id,
            source,
            queue,
            radio_auto,
            ..
        } = request;
        self.persist_queue(zone_id, &queue);
        self.state.listening().start_with_radio(
            self.state.library(),
            zone_id.to_string(),
            self.state.zones().zone_name(zone_id),
            profile_id,
            source,
            queue,
            radio_auto,
        );
        Ok(PlaybackOutcome::Completed)
    }

    fn persist_queue(&self, zone_id: &str, queue: &[SourceRef]) {
        if let Err(error) = self.state.library().set_zone_queue(zone_id, queue) {
            warn!(
                event = "playback_queue_persist_failed",
                zone_id,
                error_kind = "library",
                error = %sanitize_error(&error),
                "Failed to persist zone queue"
            );
        }
    }
}

fn normalized_hegel_volume(settings: &crate::settings::HegelSettings, volume: f32) -> u8 {
    let requested = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    requested.min(settings.max_volume)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::qobuz::prefetch_qobuz_queue_track_into_player;
    use crate::playback::test_support::{app_state, qobuz_source};
    use crate::services::qobuz::QobuzQueueTrack;
    use std::io::{self, Cursor, Read, Seek, SeekFrom};
    use std::time::Duration;

    struct TestMediaSource {
        cursor: Cursor<Vec<u8>>,
    }

    impl TestMediaSource {
        fn empty() -> Self {
            Self {
                cursor: Cursor::new(Vec::new()),
            }
        }
    }

    impl Read for TestMediaSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.cursor.read(buf)
        }
    }

    impl Seek for TestMediaSource {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(pos)
        }
    }

    impl MediaSource for TestMediaSource {
        fn is_seekable(&self) -> bool {
            true
        }

        fn byte_len(&self) -> Option<u64> {
            Some(self.cursor.get_ref().len() as u64)
        }
    }

    fn qobuz_request(track_id: u64) -> QobuzPlayRequest {
        QobuzPlayRequest {
            track_id,
            title: Some(format!("Track {track_id}")),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_id: Some("album".to_string()),
            image_url: None,
            duration_secs: Some(180.0),
            format_id: None,
            expected_current: None,
            radio_auto: false,
            replace_current: true,
            playlist_context: None,
            queue: Vec::new(),
        }
    }

    fn qobuz_queue_track(track_id: u64) -> QobuzQueueTrack {
        QobuzQueueTrack {
            track_id,
            title: Some(format!("Track {track_id}")),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_id: Some("album".to_string()),
            image_url: None,
            duration_secs: Some(180.0),
            format_id: None,
            radio: false,
            playlist_context: None,
        }
    }

    #[tokio::test]
    async fn qobuz_selected_track_starts_while_next_prefetch_is_blocked_and_failing() {
        let state = app_state("qobuz-selected-before-next-prefetch");
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        let source = qobuz_source(41, false);
        let next = qobuz_source(42, false);
        let queue = vec![next.clone()];
        let req = qobuz_request(41);
        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("active test zone should have a player");
        apply_playback_settings_for_zone(&state, &zone_id);
        let config_before = player.snapshot().config;
        let epoch_before = player.playback_epoch();

        let (next_started_tx, next_started_rx) = tokio::sync::oneshot::channel();
        let (release_next_tx, release_next_rx) = tokio::sync::oneshot::channel();
        let (next_handle_tx, next_handle_rx) = tokio::sync::oneshot::channel();
        let selected_stream_fut = async move {
            let next_handle = tokio::spawn(async move {
                let _ = next_started_tx.send(());
                let _ = release_next_rx.await;
                Err::<(), &'static str>("simulated next-track failure")
            });
            let _ = next_handle_tx.send(next_handle);
            Ok(SelectedQobuzStream {
                source: Box::new(TestMediaSource::empty()),
                ext: "flac".to_string(),
                display_name: "Artist - Track 41".to_string(),
            })
        };

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            LocalPlaybackSink::new(&state).complete_selected_qobuz_start(
                &zone_id,
                PlaybackRequest {
                    profile_id: state.settings().active_profile_id(),
                    source: source.clone(),
                    queue: queue.clone(),
                    radio_auto: false,
                    guard: PlaybackGuard::none(),
                    qobuz_request: None,
                },
                &req,
                &player,
                selected_stream_fut,
                std::future::ready(None),
            ),
        )
        .await
        .expect("selected-track handoff must not wait for next-track prefetch")
        .expect("selected track should start");

        assert!(matches!(outcome, PlaybackOutcome::Completed));
        assert!(player.playback_epoch() > epoch_before);
        assert_eq!(player.stream_queue_len(), 0);
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|active| active.key()),
            Some(source.key())
        );
        assert_eq!(
            state
                .listening()
                .queued_sources(&zone_id)
                .into_iter()
                .map(|queued| queued.key())
                .collect::<Vec<_>>(),
            vec![next.key()]
        );
        assert_eq!(
            state
                .library()
                .zone_queue(&zone_id)
                .unwrap()
                .into_iter()
                .map(|entry| entry.source.key())
                .collect::<Vec<_>>(),
            vec!["qobuz:42".to_string()]
        );
        let config_after = player.snapshot().config;
        assert_eq!(
            config_after.upsampling_enabled,
            config_before.upsampling_enabled
        );
        assert_eq!(
            config_after.configured_target_rate,
            config_before.configured_target_rate
        );

        let next_handle = next_handle_rx.await.unwrap();
        next_started_rx.await.unwrap();
        assert!(!next_handle.is_finished());
        release_next_tx.send(()).unwrap();
        assert_eq!(
            next_handle.await.unwrap(),
            Err("simulated next-track failure")
        );
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|active| active.key()),
            Some(source.key())
        );
    }

    #[tokio::test]
    async fn stale_qobuz_next_prefetch_epoch_cannot_replace_selected_track() {
        let state = app_state("qobuz-stale-next-prefetch-epoch");
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        let source = qobuz_source(51, false);
        let next = qobuz_source(52, false);
        state.listening().start(
            state.library(),
            zone_id.clone(),
            state.zones().zone_name(&zone_id),
            state.settings().active_profile_id(),
            source.clone(),
            vec![next.clone()],
        );
        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&next))
            .unwrap();
        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("active test zone should have a player");
        let stale_epoch = player.playback_epoch();
        player.reserve_playback_change();

        let result = prefetch_qobuz_queue_track_into_player(
            state.clone(),
            zone_id.clone(),
            qobuz_queue_track(52),
            source.key(),
            stale_epoch,
            false,
        )
        .await;

        assert_eq!(result.unwrap_err(), "Playback changed");
        assert_eq!(player.stream_queue_len(), 0);
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|active| active.key()),
            Some(source.key())
        );
        assert_eq!(
            state
                .library()
                .zone_queue(&zone_id)
                .unwrap()
                .into_iter()
                .map(|entry| entry.source.key())
                .collect::<Vec<_>>(),
            vec![next.key()]
        );
    }
}
