use crate::app::state::AppState;
use crate::diagnostics::logging::sanitize_error;
use crate::playback::artist_radio::local_artist_radio_next_source_from_source_for_zone;
use crate::playback::error::PlaybackError;
use crate::playback::intent::PlaybackOutcome;
use crate::playback::lastfm::{lastfm_radio_has_future_queue, lastfm_radio_next_source_for_zone};
use crate::playback::qobuz::qobuz_radio_next_request_for_zone;
use crate::playback::queue::append_source_to_now_playing_queue;
use crate::playback::request::{PlaybackGuard, PlaybackRequest};
use crate::playback::source::{qobuz_queue_source_refs, qobuz_source_ref_from_play_request};
use crate::protocol::SourceRef;
use tracing::warn;

pub(crate) enum QueueAdvance {
    Completed,
    Play {
        request: Box<PlaybackRequest>,
        outcome: PlaybackOutcome,
    },
    AdvanceSink {
        profile_id: String,
    },
}

pub(crate) struct QueueAdvancePolicy<'a> {
    state: &'a AppState,
}

impl<'a> QueueAdvancePolicy<'a> {
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(crate) async fn advance(&self, zone_id: &str) -> Result<QueueAdvance, PlaybackError> {
        let profile_id = self
            .state
            .listening()
            .profile_id(zone_id)
            .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
        let active_source = self.state.listening().active_source(zone_id);
        let active_source_key = active_source.as_ref().map(SourceRef::key);
        let queue_empty = self.queue_is_exhausted(zone_id, active_source.as_ref());

        if queue_empty && self.state.settings().lastfm_radio_enabled() {
            match lastfm_radio_next_source_for_zone((*self.state).clone(), zone_id).await {
                Ok(Some(source)) => {
                    let current_active_source = self.state.listening().active_source(zone_id);
                    if current_active_source.as_ref().map(SourceRef::key) != active_source_key {
                        return Ok(QueueAdvance::Completed);
                    }
                    if self.queue_is_exhausted(zone_id, current_active_source.as_ref()) {
                        self.append_radio_source(zone_id, &source, "lastfm");
                        return Ok(QueueAdvance::Play {
                            request: Box::new(radio_request(profile_id, source)),
                            outcome: PlaybackOutcome::Completed,
                        });
                    }
                }
                Ok(None) => {
                    let current_active_source = self.state.listening().active_source(zone_id);
                    if current_active_source.as_ref().map(SourceRef::key) != active_source_key {
                        return Ok(QueueAdvance::Completed);
                    }
                    if self.queue_is_exhausted(zone_id, current_active_source.as_ref()) {
                        warn!(
                            event = "external_service_failure",
                            service = "lastfm",
                            error_kind = "not_found",
                            zone_id,
                            "Last.fm returned no playable recommendation; falling back"
                        );
                    }
                }
                Err(error) => {
                    warn!(
                        event = "external_service_failure",
                        service = "lastfm",
                        error_kind = "error",
                        zone_id,
                        error = %sanitize_error(&error),
                        "Last.fm radio failed; falling back"
                    );
                }
            }
        }

        if queue_empty && let Some(active_source) = active_source.as_ref() {
            match local_artist_radio_next_source_from_source_for_zone(
                self.state,
                zone_id,
                active_source,
            ) {
                Ok(Some(source)) => {
                    let current_active_source = self.state.listening().active_source(zone_id);
                    if current_active_source.as_ref().map(SourceRef::key) != active_source_key {
                        return Ok(QueueAdvance::Completed);
                    }
                    self.append_radio_source(zone_id, &source, "artist_radio");
                    return Ok(QueueAdvance::Play {
                        request: Box::new(radio_request(profile_id, source)),
                        outcome: PlaybackOutcome::Completed,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        event = "external_service_failure",
                        service = "artist_radio",
                        error_kind = "error",
                        zone_id,
                        error = %sanitize_error(&error),
                        "Local artist radio fallback failed"
                    );
                }
            }
        }

        match qobuz_radio_next_request_for_zone((*self.state).clone(), zone_id).await {
            Ok(Some(qobuz_request)) => {
                let queue = qobuz_queue_source_refs(&qobuz_request);
                let source = qobuz_source_ref_from_play_request(&qobuz_request);
                let radio_auto = qobuz_request.radio_auto;
                Ok(QueueAdvance::Play {
                    request: Box::new(PlaybackRequest {
                        profile_id,
                        source,
                        queue,
                        radio_auto,
                        guard: PlaybackGuard::none(),
                        qobuz_request: Some(Box::new(qobuz_request)),
                    }),
                    outcome: PlaybackOutcome::QobuzRadioAdvanced,
                })
            }
            Ok(None) => Ok(QueueAdvance::AdvanceSink { profile_id }),
            Err(error) => Err(PlaybackError::integration(error)),
        }
    }

    fn queue_is_exhausted(&self, zone_id: &str, active_source: Option<&SourceRef>) -> bool {
        active_source
            .map(|source| !lastfm_radio_has_future_queue(self.state, zone_id, source))
            .unwrap_or_else(|| {
                self.state
                    .library()
                    .zone_queue(zone_id)
                    .map(|queue| queue.is_empty())
                    .unwrap_or(false)
            })
    }

    fn append_radio_source(&self, zone_id: &str, source: &SourceRef, service: &'static str) {
        if let Err(error) = append_source_to_now_playing_queue(self.state, zone_id, source) {
            match service {
                "lastfm" => warn!(
                    event = "playback_queue_persist_failed",
                    service,
                    zone_id,
                    error_kind = "library",
                    error = %sanitize_error(&error),
                    "Failed to append Last.fm radio source"
                ),
                _ => warn!(
                    event = "playback_queue_persist_failed",
                    service,
                    zone_id,
                    error_kind = "library",
                    error = %sanitize_error(&error),
                    "Failed to append local radio source"
                ),
            }
        }
    }
}

fn radio_request(profile_id: String, source: SourceRef) -> PlaybackRequest {
    let radio_auto = source.is_radio();
    PlaybackRequest {
        profile_id,
        source,
        queue: Vec::new(),
        radio_auto,
        guard: PlaybackGuard::none(),
        qobuz_request: None,
    }
}
