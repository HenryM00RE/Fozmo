use super::*;
use super::{session::*, soap::*};

/// Clears this seek's reservation when its future is dropped, including task
/// cancellation while suspended at an await point. Generation matching keeps
/// an older cancelled seek from clearing a newer reservation for the zone.
struct SeekReservationGuard<'a> {
    service: &'a UpnpRendererService,
    zone_id: &'a str,
    generation: u64,
}

impl<'a> SeekReservationGuard<'a> {
    fn new(service: &'a UpnpRendererService, zone_id: &'a str) -> Self {
        Self {
            service,
            zone_id,
            generation: service.begin_seek_reservation(zone_id),
        }
    }

    fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for SeekReservationGuard<'_> {
    fn drop(&mut self) {
        self.service
            .clear_seek_reservation(self.zone_id, Some(self.generation));
    }
}

impl UpnpRendererService {
    pub async fn play(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        asset: UpnpAsset,
    ) -> Result<(), String> {
        self.play_with_expected_generation(zone_id, target, asset, None)
            .await
    }

    pub async fn play_with_expected_generation(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        asset: UpnpAsset,
        expected_generation: Option<u64>,
    ) -> Result<(), String> {
        let command_lock = self.command_lock_for_zone(zone_id);
        let _guard = command_lock.lock().await;
        if let Some(generation) = expected_generation {
            if !self.command_generation_matches(zone_id, generation) {
                return Err("Playback changed".to_string());
            }
        } else {
            self.bump_command_generation(zone_id);
        }
        self.play_locked(zone_id, target, asset, UPNP_STARTUP_ACCEPT_TIMEOUT)
            .await
    }

    pub async fn arm_next_transport_uri(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        asset: &UpnpAsset,
        expected_current_source_key: Option<&str>,
    ) -> Result<(), String> {
        let command_lock = self.command_lock_for_zone(zone_id);
        let _guard = command_lock.lock().await;
        if self.next_handoff_blocked_by_seek(zone_id) {
            // Treat a prewarm that raced a seek as stale so its caller retries
            // after the settle window instead of remembering it as completed.
            return Err("Playback changed".to_string());
        }
        let (play_id, current_source_key, already_armed) = {
            let sessions = self.sessions.lock().unwrap();
            let Some(session) = sessions.get(zone_id) else {
                return Err("Playback changed".to_string());
            };
            let Some(current) = session.current.as_ref() else {
                return Err("Playback changed".to_string());
            };
            (
                session.play_id,
                current.source_ref.key(),
                session
                    .armed_next
                    .as_ref()
                    .is_some_and(|armed| armed.source_ref.key() == asset.source_ref.key()),
            )
        };
        if expected_current_source_key.is_some_and(|expected| expected != current_source_key) {
            return Err("Playback changed".to_string());
        }
        // SetNextAVTransportURI is not a harmless refresh on every renderer.
        // KEF can reopen the queued stream each time it is sent, so keep a
        // successfully armed source stable until playback, seeking, or a queue
        // change explicitly clears it.
        if already_armed {
            return Ok(());
        }
        self.record_next_handoff_prepared(zone_id, play_id, asset);
        self.register_stream_trace_context(zone_id, play_id, asset);
        if self.next_uri_unsupported(target) {
            let result = Err("UPnP renderer does not support SetNextAVTransportURI".to_string());
            self.record_next_handoff_armed(zone_id, play_id, asset, &result);
            return result;
        }
        let mut result = Err("UPnP next-track handoff was not attempted".to_string());
        for attempt in 1..=UPNP_NEXT_URI_ATTEMPTS {
            result = self
                .set_next_av_transport_uri(zone_id, target, asset, attempt)
                .await;
            if result.is_ok()
                || result
                    .as_ref()
                    .err()
                    .is_some_and(|error| set_next_uri_unsupported_error(error))
                || attempt == UPNP_NEXT_URI_ATTEMPTS
            {
                break;
            }
            if self.seek_reservation_active(zone_id) {
                result = Err("Playback changed".to_string());
                break;
            }
            tokio::time::sleep(UPNP_NEXT_URI_RETRY_SETTLE * u32::from(attempt)).await;
        }
        if result.is_ok() {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(session) = sessions.get_mut(zone_id)
                && session.play_id == play_id
            {
                session.armed_next = Some(asset.clone());
            }
        }
        if self.seek_reservation_active(zone_id) {
            // A Seek can reserve the zone without waiting for this command
            // lock. If it arrived while SetNextAVTransportURI was on the wire,
            // keep the local armed state so the waiting Seek clears the
            // renderer's NextURI before sending REL_TIME.
            let result = Err("Playback changed".to_string());
            self.record_next_handoff_armed(zone_id, play_id, asset, &result);
            return result;
        }
        if result
            .as_ref()
            .err()
            .is_some_and(|error| set_next_uri_unsupported_error(error))
        {
            self.mark_next_uri_unsupported(target);
        }
        self.record_next_handoff_armed(zone_id, play_id, asset, &result);
        result
    }

    pub async fn renderer_next_if_armed_for_source(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        source: &SourceRef,
    ) -> Result<bool, String> {
        let command_lock = self.command_lock_for_zone(zone_id);
        let _guard = command_lock.lock().await;
        if !self.armed_next_matches_source(zone_id, source) {
            return Ok(false);
        }
        self.bump_command_generation(zone_id);
        self.traced_soap_action(
            zone_id,
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Next",
            "<InstanceID>0</InstanceID>",
            UPNP_SOAP_ACTION_TIMEOUT,
            1,
        )
        .await
        .map(|_| ())?;
        self.promote_armed_next_for_source(zone_id, source, true);
        self.mark_next_handoff_transition_path(zone_id, &source.key(), "explicit_renderer_next");
        Ok(true)
    }

    pub fn promote_armed_next_if_matches(&self, zone_id: &str, source: &SourceRef) -> bool {
        self.promote_armed_next_for_source(zone_id, source, true)
    }

    pub fn has_armed_next_for_source(&self, zone_id: &str, source: &SourceRef) -> bool {
        self.armed_next_matches_source(zone_id, source)
    }

    pub fn current_playback_uses_hegel_dop_wav(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
    ) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(zone_id)
            .and_then(|session| session.current.as_ref())
            .is_some_and(|asset| {
                dop_control_policy_for(target, asset) == UpnpDopControlPolicy::HegelH390DopWav
            })
    }

    pub(super) fn next_uri_unsupported(&self, target: &UpnpRendererTarget) -> bool {
        self.next_uri_unsupported_renderers
            .lock()
            .unwrap()
            .contains(&target.id)
    }

    pub(super) fn mark_next_uri_unsupported(&self, target: &UpnpRendererTarget) {
        self.next_uri_unsupported_renderers
            .lock()
            .unwrap()
            .insert(target.id.clone());
    }

    fn armed_next_matches_source(&self, zone_id: &str, source: &SourceRef) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(zone_id)
            .and_then(|session| session.armed_next.as_ref())
            .is_some_and(|asset| asset.source_ref.key() == source.key())
    }

    fn promote_armed_next_for_source(
        &self,
        zone_id: &str,
        source: &SourceRef,
        mark_renderer_next: bool,
    ) -> bool {
        let source_key = source.key();
        let promoted = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(session) = sessions.get_mut(zone_id) else {
                return false;
            };
            let Some(next) = session.armed_next.take() else {
                return false;
            };
            if next.source_ref.key() != source_key {
                session.armed_next = Some(next);
                return false;
            }
            session.current = Some(next);
            session.state = "Transitioning".to_string();
            session.started_at = None;
            session.paused_position = 0.0;
            session.playback_polled_at = None;
            session.transport_pending = Some("loading".to_string());
            session.transport_pending_position_secs = None;
            session.startup = None;
            true
        };
        if promoted {
            if mark_renderer_next {
                self.mark_renderer_next_used(zone_id, &source_key);
            }
            self.mark_handoff_promoted_without_play(zone_id, &source_key);
        }
        promoted
    }

    pub fn begin_prepare_command(&self, zone_id: &str) -> u64 {
        self.bump_command_generation(zone_id)
    }

    pub fn command_generation_matches(&self, zone_id: &str, generation: u64) -> bool {
        self.command_generations
            .lock()
            .unwrap()
            .get(zone_id)
            .copied()
            .unwrap_or_default()
            == generation
    }

    pub fn current_command_generation(&self, zone_id: &str) -> u64 {
        self.command_generations
            .lock()
            .unwrap()
            .get(zone_id)
            .copied()
            .unwrap_or_default()
    }

    pub(super) fn bump_command_generation(&self, zone_id: &str) -> u64 {
        let mut generations = self.command_generations.lock().unwrap();
        let generation = generations
            .get(zone_id)
            .copied()
            .unwrap_or_default()
            .saturating_add(1);
        generations.insert(zone_id.to_string(), generation);
        generation
    }

    pub(super) async fn play_locked(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        asset: UpnpAsset,
        accept_timeout: Duration,
    ) -> Result<(), String> {
        let previous_session = self.sessions.lock().unwrap().get(zone_id).cloned();
        let play_id = self.prime_session_for_play(zone_id, asset.clone());
        self.begin_play_trace(zone_id, play_id, target, &asset);
        if let Err(error) = self
            .replace_transport_uri_and_play(zone_id, play_id, target, &asset)
            .await
        {
            self.finish_play_trace(zone_id, play_id, Some(error.clone()));
            self.restore_session_after_failed_play(
                zone_id,
                play_id,
                previous_session.clone(),
                &error,
            );
            return Err(error);
        }
        if let Err(error) = self
            .wait_for_startup_acceptance(zone_id, play_id, accept_timeout)
            .await
        {
            self.mark_startup_timeout(zone_id, play_id, &error);
            self.finish_play_trace(zone_id, play_id, Some(error.clone()));
            self.restore_session_after_failed_play(
                zone_id,
                play_id,
                previous_session.clone(),
                &error,
            );
            return Err(error);
        }
        if target_requires_verified_playing(target)
            && let Err(error) = self
                .wait_for_startup_playing_confirmation(
                    zone_id,
                    play_id,
                    target,
                    UPNP_KEF_STARTUP_PLAYING_TIMEOUT,
                )
                .await
        {
            if self.session_startup_failure(zone_id, play_id).is_none() {
                self.mark_startup_timeout(zone_id, play_id, &error);
            }
            self.finish_play_trace(zone_id, play_id, Some(error.clone()));
            self.restore_session_after_failed_play(zone_id, play_id, previous_session, &error);
            return Err(error);
        }
        self.finish_play_trace(zone_id, play_id, None);
        self.promote_capabilities_from_observed_playback(target, &asset);
        self.spawn_startup_playing_watchdog(zone_id.to_string(), target.clone(), play_id);
        Ok(())
    }

    pub async fn pause(&self, zone_id: &str, target: &UpnpRendererTarget) -> Result<(), String> {
        let command_lock = self.command_lock_for_zone(zone_id);
        let _guard = command_lock.lock().await;
        self.bump_command_generation(zone_id);
        self.soap_action(
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Pause",
            "<InstanceID>0</InstanceID>",
        )
        .await?;
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.paused_position = session_position(session);
        session.started_at = None;
        session.state = "Paused".to_string();
        session.playback_polled_at = None;
        session.startup = None;
        session.transport_pending = None;
        session.transport_pending_position_secs = None;
        Ok(())
    }

    pub async fn resume(&self, zone_id: &str, target: &UpnpRendererTarget) -> Result<(), String> {
        let command_lock = self.command_lock_for_zone(zone_id);
        let _guard = command_lock.lock().await;
        self.bump_command_generation(zone_id);
        self.play_transport(zone_id, target, 1).await?;
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.started_at = None;
        session.state = "Transitioning".to_string();
        session.playback_polled_at = None;
        session.startup = None;
        session.transport_pending = Some("loading".to_string());
        session.transport_pending_position_secs = None;
        Ok(())
    }

    pub async fn stop(&self, zone_id: &str, target: &UpnpRendererTarget) -> Result<(), String> {
        self.clear_seek_reservation(zone_id, None);
        self.clear_seek_settling(zone_id);
        let command_lock = self.command_lock_for_zone(zone_id);
        let _guard = command_lock.lock().await;
        self.bump_command_generation(zone_id);
        self.soap_action(
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Stop",
            "<InstanceID>0</InstanceID>",
        )
        .await?;
        let mut sessions = self.sessions.lock().unwrap();
        let previous_play_id = sessions
            .get(zone_id)
            .map(|session| session.play_id)
            .unwrap_or(0);
        let session = UpnpSession {
            play_id: previous_play_id,
            ..UpnpSession::default()
        };
        sessions.insert(zone_id.to_string(), session);
        Ok(())
    }

    pub async fn next(&self, target: &UpnpRendererTarget) -> Result<(), String> {
        self.soap_action(
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Next",
            "<InstanceID>0</InstanceID>",
        )
        .await?;
        Ok(())
    }

    pub async fn seek(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        seconds: f64,
    ) -> Result<UpnpSeekOutcome, String> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err("Seek position must be a finite non-negative value".to_string());
        }
        // Reserve seeking before waiting for the transport lock. This prevents
        // an in-flight next-track prewarm from arming itself in the gap between
        // the UI request arriving and the Seek SOAP action being serialized.
        let reservation_guard = SeekReservationGuard::new(self, zone_id);
        let command_generation = reservation_guard.generation();
        let (command_generation, seek_advertised, was_playing, play_id, progressive_dop, result) = {
            let command_lock = self.command_lock_for_zone(zone_id);
            let _guard = command_lock.lock().await;
            if !self.seek_reservation_matches(zone_id, command_generation) {
                return Ok(superseded_seek_outcome());
            }
            if target_requires_verified_playing(target) && self.has_armed_next_for_zone(zone_id) {
                // KEF decoders can fail a seek while NextURI is armed. Clear
                // it first; the monitor will re-arm the queued item after the
                // normal post-seek settle window.
                self.clear_armed_next_before_seek(zone_id, target).await?;
                if !self.seek_reservation_matches(zone_id, command_generation) {
                    return Ok(superseded_seek_outcome());
                }
            }
            let seek_advertised = self.seek_action_advertised(target).await;
            if !self.seek_reservation_matches(zone_id, command_generation) {
                return Ok(superseded_seek_outcome());
            }
            if target_requires_verified_playing(target)
                && let Some(remaining) = self.seek_settle_remaining(zone_id)
            {
                // KEF decoders fail the transport when a second Seek lands
                // during the reopen/range-scan window. Let newer requests
                // replace this reservation while we wait, then send only the
                // latest target once the renderer has settled.
                tokio::time::sleep(remaining).await;
                if !self.seek_reservation_matches(zone_id, command_generation) {
                    return Ok(superseded_seek_outcome());
                }
            }
            let (was_playing, play_id) = {
                let sessions = self.sessions.lock().unwrap();
                let session = sessions.get(zone_id);
                (
                    session.is_some_and(session_is_effectively_playing),
                    session.map(|session| session.play_id).unwrap_or_default(),
                )
            };
            {
                let mut sessions = self.sessions.lock().unwrap();
                let session = sessions.entry(zone_id.to_string()).or_default();
                session.paused_position = seconds.max(0.0);
                session.started_at = None;
                session.state = if was_playing {
                    "Transitioning".to_string()
                } else {
                    "Paused".to_string()
                };
                session.playback_polled_at = None;
                if was_playing {
                    session.transport_pending = Some("seeking".to_string());
                    session.transport_pending_position_secs = Some(seconds.max(0.0));
                } else {
                    session.transport_pending = None;
                    session.transport_pending_position_secs = None;
                }
            }
            if was_playing {
                self.mark_seek_attempt_started(zone_id, play_id);
            }
            let progressive_dop =
                self.current_playback_uses_progressive_dop_stream(zone_id, play_id);
            let body = format!(
                "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>{}</Target>",
                format_hhmmss(seconds)
            );
            let result = self
                .traced_soap_action(
                    zone_id,
                    target,
                    target.av_transport_control_url.as_str(),
                    "urn:schemas-upnp-org:service:AVTransport:1",
                    "Seek",
                    &body,
                    UPNP_SOAP_ACTION_TIMEOUT,
                    1,
                )
                .await;
            (
                command_generation,
                seek_advertised,
                was_playing,
                play_id,
                progressive_dop,
                result,
            )
        };
        if let Err(error) = &result {
            self.clear_seek_reservation(zone_id, Some(command_generation));
            self.clear_seek_settling(zone_id);
            self.clear_seek_pending(zone_id, play_id, seconds, was_playing);
            self.record_seek_trace(zone_id, seconds, seek_advertised, None, &result);
            return Err(error.clone());
        }
        if was_playing
            && self.command_generation_matches(zone_id, command_generation)
            && self
                .seek_post_play_required(zone_id, target, progressive_dop)
                .await
            && let Err(error) = self.play_transport(zone_id, target, 1).await
        {
            self.clear_seek_reservation(zone_id, Some(command_generation));
            self.clear_seek_pending(zone_id, play_id, seconds, true);
            return Err(error);
        }
        let verification = if was_playing {
            self.verify_seek_started(zone_id, target, play_id, command_generation, seconds)
                .await
        } else {
            None
        };
        let confirmed = !was_playing
            || matches!(
                verification.as_deref(),
                Some(
                    "already_satisfied"
                        | "renderer_range_request"
                        | "position_converged"
                        | "superseded"
                )
            );
        if was_playing && confirmed && self.command_generation_matches(zone_id, command_generation)
        {
            // KEF renderers can fail the active transport if SetNextAVTransportURI
            // is issued while their decoder is still reopening/range-scanning
            // after Seek. Keep next-track arming out of that short settle window.
            self.finish_seek_reservation(
                zone_id,
                command_generation,
                Some(UPNP_SEEK_NEXT_HANDOFF_SETTLE),
            );
        } else if self.seek_reservation_matches(zone_id, command_generation) {
            // An unconfirmed seek is an error-recovery path. Release the active
            // reservation so it cannot strand prewarming forever, but retain
            // the same cooldown before another SetNextAVTransportURI attempt.
            self.clear_seek_reservation(zone_id, Some(command_generation));
            self.defer_next_handoff_after_seek(zone_id, UPNP_SEEK_NEXT_HANDOFF_SETTLE);
        }
        let needs_completed_render_fallback = was_playing
            && !confirmed
            && self.current_playback_uses_progressive_dsp_stream(zone_id, play_id)
            && !progressive_dop;
        self.record_seek_trace(
            zone_id,
            seconds,
            seek_advertised,
            verification.clone(),
            &result,
        );
        if was_playing && self.command_generation_matches(zone_id, command_generation) {
            self.spawn_seek_playing_watchdog(
                zone_id.to_string(),
                target.clone(),
                play_id,
                command_generation,
            );
        }
        Ok(UpnpSeekOutcome {
            confirmed,
            verification,
            needs_completed_render_fallback,
        })
    }

    pub(super) async fn seek_action_advertised(&self, target: &UpnpRendererTarget) -> Option<bool> {
        self.current_transport_actions(target)
            .await
            .ok()
            .map(|actions| {
                actions
                    .iter()
                    .any(|action| action.eq_ignore_ascii_case("Seek"))
            })
    }

    pub(super) async fn current_transport_actions(
        &self,
        target: &UpnpRendererTarget,
    ) -> Result<Vec<String>, String> {
        let body = self
            .soap_action(
                target,
                target.av_transport_control_url.as_str(),
                "urn:schemas-upnp-org:service:AVTransport:1",
                "GetCurrentTransportActions",
                "<InstanceID>0</InstanceID>",
            )
            .await?;
        Ok(tag_text(&body, "Actions")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect())
    }

    async fn seek_post_play_required(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        progressive_dop: bool,
    ) -> bool {
        match tokio::time::timeout(
            UPNP_PLAYBACK_REFRESH_TIMEOUT,
            self.transport_snapshot(target),
        )
        .await
        {
            Ok(Ok(snapshot)) => {
                let state = snapshot
                    .state
                    .as_deref()
                    .map(upnp_state_label)
                    .unwrap_or_default();
                self.record_transport_state(zone_id, snapshot.state.as_deref());
                !matches!(state.as_str(), "Playing" | "Transitioning")
            }
            _ => !progressive_dop,
        }
    }

    pub(super) async fn verify_seek_started(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        play_id: u64,
        command_generation: u64,
        target_secs: f64,
    ) -> Option<String> {
        let started = Instant::now();
        loop {
            tokio::time::sleep(UPNP_SEEK_PLAYING_POLL_INTERVAL).await;
            if !self.command_generation_matches(zone_id, command_generation) {
                return Some("superseded".to_string());
            }
            if !self.session_seek_pending(zone_id, play_id) {
                return Some("already_satisfied".to_string());
            }
            if self.trace_has_active_range_request_since_seek(zone_id, play_id) {
                self.clear_seek_pending(zone_id, play_id, target_secs, true);
                return Some("renderer_range_request".to_string());
            }
            if let Ok(Ok(snapshot)) = tokio::time::timeout(
                UPNP_PLAYBACK_REFRESH_TIMEOUT,
                self.transport_snapshot(target),
            )
            .await
                && snapshot.position_secs.is_some_and(|position| {
                    (position - target_secs).abs() <= UPNP_SEEK_PENDING_POSITION_TOLERANCE_SECS
                })
            {
                let mut sessions = self.sessions.lock().unwrap();
                if let Some(session) = sessions.get_mut(zone_id)
                    && session.play_id == play_id
                    && session.transport_pending.as_deref() == Some("seeking")
                {
                    session.transport_pending = None;
                    session.transport_pending_position_secs = None;
                    session.paused_position = target_secs;
                    session.state = "Playing".to_string();
                    session.started_at = Some(Instant::now());
                    session.playback_polled_at = Some(Instant::now());
                }
                drop(sessions);
                self.finish_seek_reservation(
                    zone_id,
                    command_generation,
                    Some(UPNP_SEEK_NEXT_HANDOFF_SETTLE),
                );
                return Some("position_converged".to_string());
            }
            let progressive_dop =
                self.current_playback_uses_progressive_dop_stream(zone_id, play_id);
            let timeout = if progressive_dop {
                UPNP_SEEK_PLAYING_TIMEOUT
            } else {
                UPNP_SEEK_PLAYING_TIMEOUT.min(Duration::from_millis(750))
            };
            if started.elapsed() >= timeout {
                self.mark_notice(
                    zone_id,
                    "Renderer accepted Seek but has not reported the target position or requested a byte range yet".to_string(),
                );
                if progressive_dop {
                    return Some("pending_dop_waiting_for_range".to_string());
                }
                self.clear_seek_pending(zone_id, play_id, target_secs, true);
                let _ = self.play_transport(zone_id, target, 2).await;
                return Some("pending_no_confirmation".to_string());
            }
        }
    }

    pub async fn set_volume(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        volume: f32,
    ) -> Result<(), String> {
        let Some(url) = target.rendering_control_url.as_deref() else {
            return Err("UPnP renderer does not expose RenderingControl volume".to_string());
        };
        let level = (volume.clamp(0.0, 1.0) * 100.0).round() as u32;
        let body = format!(
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>{level}</DesiredVolume>"
        );
        self.soap_action(
            target,
            url,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "SetVolume",
            &body,
        )
        .await?;
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.volume = Some(level as f32 / 100.0);
        session.volume_polled_at = Some(Instant::now());
        Ok(())
    }

    pub(super) async fn set_av_transport_uri(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        asset: &UpnpAsset,
        attempt: u8,
    ) -> Result<(), String> {
        let metadata = didl_metadata(asset, target);
        let body = format!(
            "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>",
            xml_escape(&asset.stream_url),
            xml_escape(&metadata)
        );
        self.traced_soap_action(
            zone_id,
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "SetAVTransportURI",
            &body,
            UPNP_SOAP_SET_URI_TIMEOUT,
            attempt,
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn set_next_av_transport_uri(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        asset: &UpnpAsset,
        attempt: u8,
    ) -> Result<(), String> {
        let metadata = didl_metadata(asset, target);
        let body = format!(
            "<InstanceID>0</InstanceID><NextURI>{}</NextURI><NextURIMetaData>{}</NextURIMetaData>",
            xml_escape(&asset.stream_url),
            xml_escape(&metadata)
        );
        self.traced_soap_action(
            zone_id,
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "SetNextAVTransportURI",
            &body,
            UPNP_SOAP_SET_NEXT_URI_TIMEOUT,
            attempt,
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn clear_next_av_transport_uri(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        attempt: u8,
    ) -> Result<(), String> {
        self.traced_soap_action(
            zone_id,
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "SetNextAVTransportURI",
            "<InstanceID>0</InstanceID><NextURI></NextURI><NextURIMetaData></NextURIMetaData>",
            UPNP_SOAP_SET_NEXT_URI_TIMEOUT,
            attempt,
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn clear_armed_next_before_seek(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
    ) -> Result<(), String> {
        let mut last_error = None;
        for attempt in 1..=UPNP_NEXT_URI_ATTEMPTS {
            match self
                .clear_next_av_transport_uri(zone_id, target, attempt)
                .await
            {
                Ok(()) => {
                    if let Some(session) = self.sessions.lock().unwrap().get_mut(zone_id) {
                        session.armed_next = None;
                    }
                    return Ok(());
                }
                Err(error) => last_error = Some(error),
            }
            if attempt < UPNP_NEXT_URI_ATTEMPTS {
                tokio::time::sleep(UPNP_NEXT_URI_RETRY_SETTLE * u32::from(attempt)).await;
            }
        }
        let error = format!(
            "Could not clear the armed UPnP next track before seeking: {}",
            last_error.unwrap_or_else(|| "unknown renderer error".to_string())
        );
        self.mark_notice(zone_id, error.clone());
        Err(error)
    }

    fn has_armed_next_for_zone(&self, zone_id: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|session| session.armed_next.is_some())
    }

    pub(super) async fn replace_transport_uri_and_play(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
        asset: &UpnpAsset,
    ) -> Result<(), String> {
        let mut last_error = None;
        let policy = dop_control_policy_for(target, asset);
        let skip_initial_stop = policy.skips_initial_stop();
        self.record_dop_control_policy(zone_id, play_id, policy, skip_initial_stop);
        if !skip_initial_stop {
            let _ = self.stop_transport(zone_id, target).await;
        }
        let mut recovery_stop_sent = false;
        for attempt in 1..=2 {
            let set_result = self
                .set_av_transport_uri(zone_id, target, asset, attempt)
                .await;
            match set_result {
                Ok(()) => match self.play_transport(zone_id, target, attempt).await {
                    Ok(()) => {
                        if let Some(timeout) = policy.startup_evidence_timeout() {
                            if self
                                .wait_for_startup_evidence(zone_id, play_id, target, timeout)
                                .await
                                .is_ok()
                            {
                                return Ok(());
                            }
                            if self.trace_has_startup_fetch_evidence(zone_id, play_id) {
                                self.mark_notice(
                                    zone_id,
                                    "UPnP renderer fetched DoP audio; waiting for playback confirmation"
                                        .to_string(),
                                );
                                return Ok(());
                            }
                            last_error = Some(
                                "UPnP Hegel DoP startup did not produce audio evidence".to_string(),
                            );
                        } else {
                            return Ok(());
                        }
                    }
                    Err(error) => {
                        if self
                            .play_error_accepted_after_renderer_fetch(
                                zone_id, play_id, target, &error,
                            )
                            .await
                        {
                            return Ok(());
                        }
                        last_error = Some(error);
                    }
                },
                Err(error) => {
                    last_error = Some(error);
                }
            }
            if attempt == 1 {
                if skip_initial_stop && !recovery_stop_sent {
                    let _ = self.stop_transport(zone_id, target).await;
                    recovery_stop_sent = true;
                }
                tokio::time::sleep(UPNP_HEGEL_DOP_RETRY_SETTLE).await;
            }
            if !self.session_play_id_matches(zone_id, play_id) {
                return Err("Playback changed".to_string());
            }
        }
        let error = last_error.unwrap_or_else(|| "UPnP play failed".to_string());
        if self.trace_has_startup_fetch_evidence(zone_id, play_id)
            && !self.session_playing_confirmed(zone_id, play_id)
        {
            let notice = format!("{error}; renderer fetched audio but did not report PLAYING");
            self.mark_startup_timeout(zone_id, play_id, &notice);
            return Err(notice);
        }
        Err(error)
    }

    pub(super) async fn play_error_accepted_after_renderer_fetch(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
        error: &str,
    ) -> bool {
        self.play_error_accepted_after_renderer_fetch_with_grace(
            zone_id,
            play_id,
            target,
            error,
            UPNP_PLAY_ERROR_ACCEPT_GRACE,
        )
        .await
    }

    pub(super) async fn play_error_accepted_after_renderer_fetch_with_grace(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
        error: &str,
        grace: Duration,
    ) -> bool {
        if !self.trace_has_startup_fetch_evidence(zone_id, play_id) {
            return false;
        }
        if self
            .wait_for_startup_playing_confirmation(zone_id, play_id, target, grace)
            .await
            .is_ok()
        {
            let notice =
                format!("UPnP Play returned an error after renderer reported PLAYING: {error}");
            {
                let mut sessions = self.sessions.lock().unwrap();
                if let Some(session) = sessions.get_mut(zone_id)
                    && session.play_id == play_id
                {
                    session.notice = Some(notice.clone());
                }
            }
            let mut traces = self.traces.lock().unwrap();
            if let Some(trace) = traces.get_mut(zone_id)
                && trace.play_id == play_id
            {
                trace.notice = Some(notice);
            }
            true
        } else {
            false
        }
    }

    pub(super) async fn wait_for_startup_playing_confirmation(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
        timeout: Duration,
    ) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if self.session_playing_confirmed(zone_id, play_id) {
                return Ok(());
            }
            if !self.session_play_id_matches(zone_id, play_id) {
                return Err("Playback changed".to_string());
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err("UPnP startup timed out before renderer reported PLAYING".to_string());
            }
            let poll_timeout = remaining.min(UPNP_PLAYBACK_REFRESH_TIMEOUT);
            match tokio::time::timeout(poll_timeout, self.transport_snapshot(target)).await {
                Ok(Ok(snapshot)) => {
                    let transport_state = snapshot.state.clone();
                    self.record_transport_state(zone_id, transport_state.as_deref());
                    if self.apply_startup_transport_snapshot(zone_id, play_id, snapshot)? {
                        return Ok(());
                    }
                }
                Ok(Err(error)) => self.record_refresh_error(zone_id, play_id, &error, true),
                Err(_) => self.record_refresh_error(
                    zone_id,
                    play_id,
                    "UPnP playback refresh timed out",
                    true,
                ),
            }
            tokio::time::sleep(Duration::from_millis(100).min(remaining)).await;
        }
    }

    async fn wait_for_startup_evidence(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
        timeout: Duration,
    ) -> Result<(), String> {
        let started = Instant::now();
        loop {
            {
                let sessions = self.sessions.lock().unwrap();
                let Some(session) = sessions.get(zone_id) else {
                    return Err("UPnP startup session disappeared".to_string());
                };
                if session.play_id != play_id {
                    return Err("Playback changed".to_string());
                }
                if session.state == "Playing"
                    || session
                        .startup
                        .as_ref()
                        .is_some_and(|startup| startup.accepted_at.is_some())
                {
                    return Ok(());
                }
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err("UPnP startup timed out before audio evidence".to_string());
            }
            let poll_timeout = remaining.min(UPNP_PLAYBACK_REFRESH_TIMEOUT);
            if let Ok(Ok(snapshot)) =
                tokio::time::timeout(poll_timeout, self.transport_snapshot(target)).await
            {
                let transport_state = snapshot.state.clone();
                self.record_transport_state(zone_id, transport_state.as_deref());
                if self.apply_startup_transport_snapshot(zone_id, play_id, snapshot)? {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100).min(remaining)).await;
        }
    }

    pub(super) fn apply_startup_transport_snapshot(
        &self,
        zone_id: &str,
        play_id: u64,
        snapshot: UpnpTransportSnapshot,
    ) -> Result<bool, String> {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(zone_id)
            .ok_or_else(|| "UPnP startup session disappeared".to_string())?;
        if session.play_id != play_id {
            return Err("Playback changed".to_string());
        }
        session.playback_polled_at = Some(now);
        if let Some(error) = upnp_transport_snapshot_error(&snapshot) {
            apply_upnp_transport_error(session, &snapshot, &error);
            drop(sessions);
            self.record_startup_failure(zone_id, play_id, &error);
            return Err(error);
        }
        if startup_transport_snapshot_is_inconclusive(session, &snapshot, now) {
            session.state = "Transitioning".to_string();
            if let (Some(current), Some(duration)) =
                (session.current.as_mut(), snapshot.duration_secs)
                && duration > 0.0
            {
                current.duration_secs = Some(duration);
            }
            return Ok(false);
        }
        reconcile_session_with_transport(session, snapshot, now);
        if session.state == "Playing" {
            mark_session_startup_playing(session, now);
            drop(sessions);
            self.mark_first_playing_observed(zone_id);
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) async fn stop_transport(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
    ) -> Result<(), String> {
        self.traced_soap_action(
            zone_id,
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Stop",
            "<InstanceID>0</InstanceID>",
            UPNP_SOAP_STOP_TIMEOUT,
            1,
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn play_transport(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        attempt: u8,
    ) -> Result<(), String> {
        self.traced_soap_action(
            zone_id,
            target,
            target.av_transport_control_url.as_str(),
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Play",
            "<InstanceID>0</InstanceID><Speed>1</Speed>",
            UPNP_SOAP_PLAY_TIMEOUT,
            attempt,
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn transport_snapshot(
        &self,
        target: &UpnpRendererTarget,
    ) -> Result<UpnpTransportSnapshot, String> {
        let transport_body = self
            .soap_action(
                target,
                target.av_transport_control_url.as_str(),
                "urn:schemas-upnp-org:service:AVTransport:1",
                "GetTransportInfo",
                "<InstanceID>0</InstanceID>",
            )
            .await?;
        let position_body = self
            .soap_action(
                target,
                target.av_transport_control_url.as_str(),
                "urn:schemas-upnp-org:service:AVTransport:1",
                "GetPositionInfo",
                "<InstanceID>0</InstanceID>",
            )
            .await
            .unwrap_or_default();
        Ok(UpnpTransportSnapshot {
            state: tag_text(&transport_body, "CurrentTransportState"),
            status: tag_text(&transport_body, "CurrentTransportStatus"),
            playback_speed: tag_text(&transport_body, "CurrentSpeed"),
            position_secs: tag_text(&position_body, "RelTime")
                .and_then(|time| parse_upnp_time(&time)),
            duration_secs: tag_text(&position_body, "TrackDuration")
                .and_then(|time| parse_upnp_time(&time)),
            current_uri: tag_text(&position_body, "TrackURI"),
        })
    }

    pub(super) async fn get_volume(&self, target: &UpnpRendererTarget) -> Result<f32, String> {
        let Some(url) = target.rendering_control_url.as_deref() else {
            return Err("UPnP renderer does not expose RenderingControl volume".to_string());
        };
        let body = self
            .soap_action(
                target,
                url,
                "urn:schemas-upnp-org:service:RenderingControl:1",
                "GetVolume",
                "<InstanceID>0</InstanceID><Channel>Master</Channel>",
            )
            .await?;
        tag_text(&body, "CurrentVolume")
            .and_then(|value| value.trim().parse::<f32>().ok())
            .map(|volume| (volume / 100.0).clamp(0.0, 1.0))
            .ok_or_else(|| "UPnP GetVolume response did not include CurrentVolume".to_string())
    }
}

fn target_requires_verified_playing(target: &UpnpRendererTarget) -> bool {
    target
        .manufacturer
        .as_deref()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("KEF"))
        || target.name.trim().to_ascii_lowercase().starts_with("kef ")
}

fn superseded_seek_outcome() -> UpnpSeekOutcome {
    UpnpSeekOutcome {
        confirmed: true,
        verification: Some("superseded".to_string()),
        needs_completed_render_fallback: false,
    }
}

fn set_next_uri_unsupported_error(error: &str) -> bool {
    error.contains("UPnP SOAP error 401") || error.contains("Invalid Action")
}
