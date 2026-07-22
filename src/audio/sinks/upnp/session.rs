use super::*;

impl UpnpRendererService {
    pub async fn refresh_playback_snapshot(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        max_age: Duration,
    ) -> Result<(), String> {
        let command_lock = self.command_lock_for_zone(zone_id);
        let Ok(_guard) = command_lock.try_lock() else {
            return Ok(());
        };
        let should_poll = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .get(zone_id)
                .and_then(|session| session.playback_polled_at)
                .is_none_or(|polled_at| polled_at.elapsed() >= max_age)
        };
        if !should_poll {
            return Ok(());
        }
        let result = match tokio::time::timeout(
            UPNP_PLAYBACK_REFRESH_TIMEOUT,
            self.transport_snapshot(target),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err("UPnP playback refresh timed out".to_string()),
        };
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        let now = Instant::now();
        session.playback_polled_at = Some(now);
        match result {
            Ok(snapshot) => {
                let transport_state = snapshot.state.clone();
                let transport_error = upnp_transport_snapshot_error(&snapshot);
                drop(sessions);
                self.record_transport_state(zone_id, transport_state.as_deref());
                let mut sessions = self.sessions.lock().unwrap();
                let session = sessions.entry(zone_id.to_string()).or_default();
                if let Some(error) = transport_error {
                    let play_id = session.play_id;
                    let is_new_failure = session
                        .startup
                        .as_ref()
                        .is_some_and(|startup| !startup.failed);
                    apply_upnp_transport_error(session, &snapshot, &error);
                    drop(sessions);
                    if is_new_failure {
                        self.record_startup_failure(zone_id, play_id, &error);
                    }
                    return Ok(());
                }
                if startup_transport_snapshot_is_inconclusive(session, &snapshot, now) {
                    session.state = "Transitioning".to_string();
                    if let (Some(current), Some(duration)) =
                        (session.current.as_mut(), snapshot.duration_secs)
                        && duration > 0.0
                    {
                        current.duration_secs = Some(duration);
                    }
                    return Ok(());
                }
                let promoted_source_key = reconcile_session_with_transport(session, snapshot, now);
                let playing = session.state == "Playing";
                if playing {
                    mark_session_startup_playing(session, now);
                }
                drop(sessions);
                if let Some(source_key) = promoted_source_key.as_deref() {
                    self.mark_renderer_next_used(zone_id, source_key);
                    self.mark_handoff_promoted_without_play(zone_id, source_key);
                    self.mark_next_handoff_transition_path(
                        zone_id,
                        source_key,
                        "observed_renderer_transition",
                    );
                }
                if playing {
                    self.mark_first_playing_observed(zone_id);
                }
                Ok(())
            }
            Err(e) => {
                if startup_refresh_error_is_inconclusive(session, now) {
                    let play_id = session.play_id;
                    drop(sessions);
                    self.record_refresh_error(zone_id, play_id, &e, true);
                    return Ok(());
                }
                session.notice = Some(e.clone());
                let play_id = session.play_id;
                drop(sessions);
                self.record_refresh_error(zone_id, play_id, &e, false);
                Err(e)
            }
        }
    }

    pub async fn refresh_volume(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        max_age: Duration,
    ) -> Result<(), String> {
        let command_lock = self.command_lock_for_zone(zone_id);
        let Ok(_guard) = command_lock.try_lock() else {
            return Ok(());
        };
        let Some(_) = target.rendering_control_url else {
            return Ok(());
        };
        let should_poll = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .get(zone_id)
                .and_then(|session| session.volume_polled_at)
                .is_none_or(|polled_at| polled_at.elapsed() >= max_age)
        };
        if !should_poll {
            return Ok(());
        }
        let result = match tokio::time::timeout(
            UPNP_VOLUME_REFRESH_TIMEOUT,
            self.get_volume(target),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err("UPnP GetVolume timed out".to_string()),
        };
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.volume_polled_at = Some(Instant::now());
        match result {
            Ok(volume) => {
                session.volume = Some(volume);
                Ok(())
            }
            Err(e) => {
                session.notice = Some(e.clone());
                Err(e)
            }
        }
    }

    pub fn snapshot(&self, zone_id: &str) -> Option<UpnpPlaybackSnapshot> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions.get(zone_id)?;
        let current = session.current.clone();
        let active_render_signature = current
            .as_ref()
            .and_then(|asset| asset.render_signature.clone());
        let configured_render_signature = session
            .reconfigure
            .configured_render_signature
            .clone()
            .or_else(|| {
                current
                    .as_ref()
                    .and_then(|asset| asset.configured_render_signature.clone())
            });
        let config_applied_to_current_playback = !session.reconfigure.restart_pending
            && active_render_signature == configured_render_signature;
        Some(UpnpPlaybackSnapshot {
            state: session.state.clone(),
            file_name: current.as_ref().and_then(|asset| {
                asset
                    .title
                    .clone()
                    .or_else(|| Some(format!("upnp:{}", asset.id)))
            }),
            current_source: current.as_ref().map(|asset| asset.source_ref.clone()),
            track_title: current.as_ref().and_then(|asset| asset.title.clone()),
            track_artist: current.as_ref().and_then(|asset| asset.artist.clone()),
            track_album: current.as_ref().and_then(|asset| asset.album.clone()),
            position_secs: session_position(session),
            duration_secs: current
                .as_ref()
                .and_then(|asset| asset.duration_secs)
                .unwrap_or(0.0),
            source_rate: current.as_ref().map(|asset| asset.source_rate).unwrap_or(0),
            target_rate: current.as_ref().map(|asset| asset.target_rate).unwrap_or(0),
            source_bits: current.as_ref().map(|asset| asset.source_bits).unwrap_or(0),
            target_bits: current.as_ref().map(|asset| asset.target_bits).unwrap_or(0),
            active_output_mode: current
                .as_ref()
                .and_then(|asset| asset.active_output_mode.clone()),
            volume: session.volume,
            playback_speed: session.playback_speed.clone(),
            notice: session.notice.clone(),
            config_applied_to_current_playback,
            restart_pending: session.reconfigure.restart_pending,
            render_status: session.reconfigure.render_status.clone(),
            active_render_signature,
            configured_render_signature,
            current_render_or_stream_plan: current
                .as_ref()
                .and_then(|asset| asset.render_or_stream_plan.clone()),
            last_render_ms: session
                .reconfigure
                .last_render_ms
                .or_else(|| current.as_ref().and_then(|asset| asset.render_ms)),
            last_prepare_ms: session
                .reconfigure
                .last_prepare_ms
                .or_else(|| current.as_ref().and_then(|asset| asset.prepare_ms)),
            last_cache_hit: session
                .reconfigure
                .last_cache_hit
                .or_else(|| current.as_ref().and_then(|asset| asset.cache_hit)),
            transport_pending: session_transport_pending(session),
            transport_pending_position_secs: session.transport_pending_position_secs,
        })
    }

    pub fn begin_reconfigure(&self, zone_id: &str) -> u64 {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.reconfigure.generation = session.reconfigure.generation.saturating_add(1);
        session.reconfigure.restart_pending = true;
        session.reconfigure.render_status = "pending".to_string();
        session.reconfigure.generation
    }

    pub fn reconfigure_generation_matches(&self, zone_id: &str, generation: u64) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|session| session.reconfigure.generation == generation)
    }

    pub fn mark_reconfigure_status(&self, zone_id: &str, generation: u64, status: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        if session.reconfigure.generation == generation {
            session.reconfigure.render_status = status.to_string();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_reconfigure(
        &self,
        zone_id: &str,
        generation: u64,
        status: &str,
        configured_render_signature: Option<String>,
        render_ms: Option<u64>,
        prepare_ms: Option<u64>,
        cache_hit: Option<bool>,
    ) {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        if session.reconfigure.generation != generation {
            return;
        }
        session.reconfigure.restart_pending = false;
        session.reconfigure.render_status = status.to_string();
        if configured_render_signature.is_some() {
            session.reconfigure.configured_render_signature = configured_render_signature;
        }
        if render_ms.is_some() {
            session.reconfigure.last_render_ms = render_ms;
        }
        if prepare_ms.is_some() {
            session.reconfigure.last_prepare_ms = prepare_ms;
        }
        if cache_hit.is_some() {
            session.reconfigure.last_cache_hit = cache_hit;
        }
    }

    pub fn mark_notice(&self, zone_id: &str, notice: String) {
        self.sessions
            .lock()
            .unwrap()
            .entry(zone_id.to_string())
            .or_default()
            .notice = Some(notice);
    }

    pub fn current_play_id(&self, zone_id: &str) -> u64 {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .map(|session| session.play_id)
            .unwrap_or_default()
    }

    pub async fn wait_for_seek_media_evidence(
        &self,
        zone_id: &str,
        play_id: u64,
        timeout: Duration,
    ) -> bool {
        let started = Instant::now();
        loop {
            if self.trace_has_seek_media_evidence(zone_id, play_id) {
                return true;
            }
            if started.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(UPNP_SEEK_PLAYING_POLL_INTERVAL).await;
        }
    }

    #[cfg(test)]
    pub(crate) fn seed_playback_for_test(&self, zone_id: &str, asset: UpnpAsset, state: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.current = Some(asset);
        session.armed_next = None;
        session.state = state.to_string();
        session.started_at = (state == "Playing").then(Instant::now);
        session.paused_position = 0.0;
        session.notice = None;
        session.reconfigure.restart_pending = false;
        session.reconfigure.render_status = "applied".to_string();
        session.reconfigure.configured_render_signature = session
            .current
            .as_ref()
            .and_then(|asset| asset.configured_render_signature.clone());
    }

    pub(super) async fn wait_for_startup_acceptance(
        &self,
        zone_id: &str,
        play_id: u64,
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
            if started.elapsed() >= timeout {
                return Err("UPnP startup timed out before renderer accepted audio".to_string());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub(super) fn spawn_startup_playing_watchdog(
        &self,
        zone_id: String,
        target: UpnpRendererTarget,
        play_id: u64,
    ) {
        let service = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            loop {
                tokio::time::sleep(UPNP_STARTUP_POLL_INTERVAL).await;
                if !service.session_play_id_matches(&zone_id, play_id) {
                    return;
                }
                if service.session_playing_confirmed(&zone_id, play_id) {
                    return;
                }
                let _ = service
                    .refresh_playback_snapshot(&zone_id, &target, Duration::ZERO)
                    .await;
                if service.session_playing_confirmed(&zone_id, play_id) {
                    return;
                }
                if service.session_startup_failure(&zone_id, play_id).is_some() {
                    return;
                }
                if started.elapsed() >= UPNP_STARTUP_PLAYING_TIMEOUT {
                    let notice =
                        "UPnP startup timed out before renderer reported PLAYING".to_string();
                    let mut sessions = service.sessions.lock().unwrap();
                    if let Some(session) = sessions.get_mut(&zone_id)
                        && session.play_id == play_id
                        && session
                            .startup
                            .as_ref()
                            .is_some_and(|startup| startup.confirmed_playing_at.is_none())
                    {
                        session.notice = Some(notice.clone());
                        if let Some(startup) = session.startup.as_mut() {
                            startup.timed_out = true;
                        }
                        drop(sessions);
                        service.mark_startup_timeout(&zone_id, play_id, &notice);
                    }
                    return;
                }
            }
        });
    }

    pub(super) fn spawn_seek_playing_watchdog(
        &self,
        zone_id: String,
        target: UpnpRendererTarget,
        play_id: u64,
        command_generation: u64,
    ) {
        let service = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            loop {
                tokio::time::sleep(UPNP_SEEK_PLAYING_POLL_INTERVAL).await;
                if !service.command_generation_matches(&zone_id, command_generation) {
                    return;
                }
                if !service.session_seek_pending(&zone_id, play_id) {
                    return;
                }
                let _ = service
                    .refresh_playback_snapshot(&zone_id, &target, Duration::ZERO)
                    .await;
                if !service.session_seek_pending(&zone_id, play_id) {
                    return;
                }
                if started.elapsed() < UPNP_SEEK_PLAYING_TIMEOUT {
                    continue;
                }
                let command_lock = service.command_lock_for_zone(&zone_id);
                let _guard = command_lock.lock().await;
                if service.command_generation_matches(&zone_id, command_generation)
                    && service.session_seek_pending(&zone_id, play_id)
                {
                    service.mark_notice(
                        &zone_id,
                        "Renderer accepted Seek but did not confirm the target position or request a byte range".to_string(),
                    );
                    if service.current_playback_uses_progressive_dop_stream(&zone_id, play_id) {
                        tracing::warn!(
                            event = "seek_watchdog_dop_no_play",
                            zone_id = %zone_id,
                            play_id,
                            "Progressive DoP seek watchdog skipped blind Play"
                        );
                        return;
                    }
                    let _ = service.play_transport(&zone_id, &target, 2).await;
                }
                return;
            }
        });
    }

    pub(super) fn prime_session_for_play(&self, zone_id: &str, asset: UpnpAsset) -> u64 {
        self.clear_seek_reservation(zone_id, None);
        self.clear_seek_settling(zone_id);
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        let play_id = session.play_id.saturating_add(1);
        session.play_id = play_id;
        session.current = Some(asset);
        session.armed_next = None;
        session.state = "Transitioning".to_string();
        session.started_at = None;
        session.paused_position = 0.0;
        session.playback_polled_at = None;
        session.notice = None;
        session.transport_pending = Some("loading".to_string());
        session.transport_pending_position_secs = None;
        session.reconfigure.restart_pending = false;
        session.reconfigure.render_status = "applied".to_string();
        session.reconfigure.configured_render_signature = session
            .current
            .as_ref()
            .and_then(|asset| asset.configured_render_signature.clone());
        session.reconfigure.last_render_ms =
            session.current.as_ref().and_then(|asset| asset.render_ms);
        session.reconfigure.last_prepare_ms =
            session.current.as_ref().and_then(|asset| asset.prepare_ms);
        session.reconfigure.last_cache_hit =
            session.current.as_ref().and_then(|asset| asset.cache_hit);
        session.startup = Some(UpnpStartup {
            play_id,
            asset_id: session
                .current
                .as_ref()
                .map(|asset| asset.id.clone())
                .unwrap_or_default(),
            started_at: Instant::now(),
            accepted_at: None,
            accepted_reason: None,
            confirmed_playing_at: None,
            failed: false,
            timed_out: false,
        });
        play_id
    }

    pub(super) fn restore_session_after_failed_play(
        &self,
        zone_id: &str,
        failed_play_id: u64,
        previous_session: Option<UpnpSession>,
        error: &str,
    ) {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions
            .get(zone_id)
            .is_none_or(|session| session.play_id != failed_play_id)
        {
            return;
        }
        let mut restored = previous_session.unwrap_or_default();
        // Invalidate late HTTP evidence from the failed asset before another
        // attempt reuses this zone. The previous source remains available for
        // completion detection, but Stop/SetURI may already have interrupted
        // it, so never claim that it is still playing.
        restored.play_id = failed_play_id.saturating_add(1);
        restored.armed_next = None;
        restored.state = "Stopped".to_string();
        restored.started_at = None;
        restored.playback_polled_at = None;
        restored.notice = Some(error.to_string());
        restored.startup = None;
        restored.transport_pending = None;
        restored.transport_pending_position_secs = None;
        sessions.insert(zone_id.to_string(), restored);
    }

    pub(super) fn command_lock_for_zone(&self, zone_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.command_locks.lock().unwrap();
        locks
            .entry(zone_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub fn next_handoff_blocked_by_seek(&self, zone_id: &str) -> bool {
        if self.seek_reservation_active(zone_id) {
            return true;
        }
        let mut settling = self.seek_settling_until.lock().unwrap();
        let blocked = settling
            .get(zone_id)
            .is_some_and(|deadline| Instant::now() < *deadline);
        if !blocked {
            settling.remove(zone_id);
        }
        blocked
    }

    pub(super) fn defer_next_handoff_after_seek(&self, zone_id: &str, duration: Duration) {
        self.seek_settling_until
            .lock()
            .unwrap()
            .insert(zone_id.to_string(), Instant::now() + duration);
    }

    pub(super) fn clear_seek_settling(&self, zone_id: &str) {
        self.seek_settling_until.lock().unwrap().remove(zone_id);
    }

    pub(super) fn seek_settle_remaining(&self, zone_id: &str) -> Option<Duration> {
        let mut settling = self.seek_settling_until.lock().unwrap();
        let now = Instant::now();
        match settling.get(zone_id).copied() {
            Some(deadline) if deadline > now => Some(deadline.duration_since(now)),
            Some(_) => {
                settling.remove(zone_id);
                None
            }
            None => None,
        }
    }

    pub(super) fn begin_seek_reservation(&self, zone_id: &str) -> u64 {
        let generation = self.bump_command_generation(zone_id);
        self.seek_reservations
            .lock()
            .unwrap()
            .insert(zone_id.to_string(), generation);
        generation
    }

    pub(super) fn seek_reservation_active(&self, zone_id: &str) -> bool {
        self.seek_reservations.lock().unwrap().contains_key(zone_id)
    }

    pub(super) fn seek_reservation_matches(&self, zone_id: &str, generation: u64) -> bool {
        self.seek_reservations
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|reserved| *reserved == generation)
    }

    pub(super) fn clear_seek_reservation(&self, zone_id: &str, generation: Option<u64>) {
        let mut reservations = self.seek_reservations.lock().unwrap();
        if generation.is_none_or(|expected| {
            reservations
                .get(zone_id)
                .is_some_and(|reserved| *reserved == expected)
        }) {
            reservations.remove(zone_id);
        }
    }

    pub(super) fn finish_seek_reservation(
        &self,
        zone_id: &str,
        generation: u64,
        settle_for: Option<Duration>,
    ) {
        if !self.seek_reservation_matches(zone_id, generation) {
            return;
        }
        self.clear_seek_reservation(zone_id, Some(generation));
        if let Some(duration) = settle_for {
            self.defer_next_handoff_after_seek(zone_id, duration);
        }
    }

    pub(super) fn finish_active_seek_reservation(&self, zone_id: &str, settle_for: Duration) {
        let generation = self.seek_reservations.lock().unwrap().get(zone_id).copied();
        if let Some(generation) = generation {
            self.finish_seek_reservation(zone_id, generation, Some(settle_for));
        }
    }

    pub(super) fn session_play_id_matches(&self, zone_id: &str, play_id: u64) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|session| session.play_id == play_id)
    }

    pub(super) fn session_playing_confirmed(&self, zone_id: &str, play_id: u64) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|session| {
                session.play_id == play_id
                    && (session.state == "Playing"
                        || session
                            .startup
                            .as_ref()
                            .is_some_and(|startup| startup.confirmed_playing_at.is_some()))
            })
    }

    pub(super) fn session_startup_failure(&self, zone_id: &str, play_id: u64) -> Option<String> {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .filter(|session| session.play_id == play_id)
            .and_then(|session| {
                session
                    .startup
                    .as_ref()
                    .filter(|startup| startup.failed)
                    .and_then(|_| session.notice.clone())
            })
    }

    pub(super) fn session_seek_pending(&self, zone_id: &str, play_id: u64) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|session| {
                session.play_id == play_id
                    && session.transport_pending.as_deref() == Some("seeking")
            })
    }

    pub(super) fn current_playback_uses_progressive_dsp_stream(
        &self,
        zone_id: &str,
        play_id: u64,
    ) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|session| {
                session.play_id == play_id
                    && session.current.as_ref().is_some_and(|asset| {
                        asset.render_or_stream_plan.as_deref() == Some("progressive_wav_stream")
                    })
            })
    }

    pub(super) fn current_playback_uses_progressive_dop_stream(
        &self,
        zone_id: &str,
        play_id: u64,
    ) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|session| {
                session.play_id == play_id
                    && session.current.as_ref().is_some_and(|asset| {
                        asset.render_or_stream_plan.as_deref() == Some("progressive_wav_stream")
                            && asset_is_dop_wav(asset)
                    })
            })
    }

    pub(super) fn clear_seek_pending(
        &self,
        zone_id: &str,
        play_id: u64,
        target_secs: f64,
        mark_playing: bool,
    ) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(zone_id) else {
            return;
        };
        if session.play_id != play_id || session.transport_pending.as_deref() != Some("seeking") {
            return;
        }
        session.transport_pending = None;
        session.transport_pending_position_secs = None;
        session.paused_position = target_secs.max(0.0);
        if mark_playing {
            session.state = "Playing".to_string();
            session.started_at = Some(Instant::now());
            session.playback_polled_at = Some(Instant::now());
        }
    }

    pub(super) fn mark_seek_media_range_ready(&self, zone_id: &str, play_id: u64, asset_id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(zone_id) else {
            return;
        };
        if session.play_id != play_id
            || session.transport_pending.as_deref() != Some("seeking")
            || session
                .current
                .as_ref()
                .is_none_or(|asset| asset.id != asset_id)
        {
            return;
        }
        session.transport_pending = None;
        session.transport_pending_position_secs = None;
        if session.state == "Transitioning" {
            session.state = "Playing".to_string();
            session.started_at = Some(Instant::now());
        }
        drop(sessions);
        self.finish_active_seek_reservation(zone_id, UPNP_SEEK_NEXT_HANDOFF_SETTLE);
    }
}

pub(super) fn session_position(session: &UpnpSession) -> f64 {
    session_position_at(session, Instant::now())
}

pub(super) fn session_position_at(session: &UpnpSession, now: Instant) -> f64 {
    if session.state != "Playing" {
        return session.paused_position;
    }
    session
        .started_at
        .and_then(|started| now.checked_duration_since(started))
        .map(|elapsed| session.paused_position + elapsed.as_secs_f64())
        .unwrap_or(session.paused_position)
}

pub(super) fn session_transport_pending(session: &UpnpSession) -> String {
    if let Some(kind) = session.transport_pending.as_deref() {
        return kind.to_string();
    }
    if session.reconfigure.restart_pending {
        return "loading".to_string();
    }
    if session.state == "Transitioning" || startup_is_active(session, Instant::now()) {
        return "loading".to_string();
    }
    "none".to_string()
}

pub(super) fn session_is_effectively_playing(session: &UpnpSession) -> bool {
    session.state == "Playing"
        || (session.state == "Transitioning"
            && (session.started_at.is_some()
                || session
                    .startup
                    .as_ref()
                    .is_some_and(|startup| startup.confirmed_playing_at.is_some())))
}

pub(super) fn startup_phase(session: &UpnpSession) -> String {
    let Some(startup) = session.startup.as_ref() else {
        return if session.state == "Playing" {
            "Playing".to_string()
        } else {
            "Idle".to_string()
        };
    };
    if startup.confirmed_playing_at.is_some() || session.state == "Playing" {
        "Playing".to_string()
    } else if startup.failed {
        "Failed".to_string()
    } else if startup.timed_out {
        "TimedOut".to_string()
    } else if startup.accepted_at.is_some() {
        "Accepted".to_string()
    } else {
        "Starting".to_string()
    }
}

pub(super) fn startup_is_active(session: &UpnpSession, now: Instant) -> bool {
    session.startup.as_ref().is_some_and(|startup| {
        startup.confirmed_playing_at.is_none()
            && !startup.failed
            && !startup.timed_out
            && now
                .checked_duration_since(startup.started_at)
                .is_some_and(|elapsed| elapsed < UPNP_STARTUP_PLAYING_TIMEOUT)
    })
}

pub(super) fn startup_transport_snapshot_is_inconclusive(
    session: &UpnpSession,
    transport: &UpnpTransportSnapshot,
    now: Instant,
) -> bool {
    if !startup_is_active(session, now) {
        return false;
    }
    let label = transport
        .state
        .as_deref()
        .map(upnp_state_label)
        .unwrap_or_default();
    if matches!(
        label.as_str(),
        "" | "Paused" | "Stopped" | "Transitioning" | "NO_MEDIA_PRESENT"
    ) {
        return true;
    }
    label == "Playing" && startup_transport_uri_is_stale(session, transport.current_uri.as_deref())
}

pub(super) fn startup_transport_uri_is_stale(
    session: &UpnpSession,
    current_uri: Option<&str>,
) -> bool {
    let Some(uri) = current_uri.map(str::trim) else {
        return false;
    };
    if uri.is_empty() || uri == "NOT_IMPLEMENTED" {
        return false;
    }
    session
        .current
        .as_ref()
        .is_some_and(|asset| asset.stream_url != uri)
}

pub(super) fn startup_refresh_error_is_inconclusive(session: &UpnpSession, now: Instant) -> bool {
    startup_is_active(session, now)
}

pub(super) fn mark_session_startup_playing(session: &mut UpnpSession, now: Instant) {
    if let Some(startup) = session.startup.as_mut()
        && startup.confirmed_playing_at.is_none()
    {
        startup.confirmed_playing_at = Some(now);
    }
}

pub(super) fn upnp_transport_snapshot_error(snapshot: &UpnpTransportSnapshot) -> Option<String> {
    let status = snapshot.status.as_deref()?.trim();
    status.eq_ignore_ascii_case("ERROR_OCCURRED").then(|| {
        let state = snapshot.state.as_deref().unwrap_or("unknown").trim();
        format!("UPnP renderer reported ERROR_OCCURRED while transport state was {state}")
    })
}

pub(super) fn apply_upnp_transport_error(
    session: &mut UpnpSession,
    snapshot: &UpnpTransportSnapshot,
    error: &str,
) {
    let projected_position = session_position(session);
    let known_duration = snapshot
        .duration_secs
        .filter(|duration| *duration > 0.0)
        .or_else(|| {
            session
                .current
                .as_ref()
                .and_then(|current| current.duration_secs)
        });
    let completed_before_error = known_duration.is_some_and(|duration| {
        upnp_position_is_complete(projected_position, duration)
            || upnp_position_is_complete(session.paused_position, duration)
    });
    session.state = "Stopped".to_string();
    session.started_at = None;
    session.playback_speed = snapshot.playback_speed.clone();
    if completed_before_error
        && snapshot
            .position_secs
            .is_none_or(|position| position <= UPNP_ENDED_RESET_POSITION_SECS)
    {
        session.paused_position = known_duration.unwrap_or(projected_position);
    } else if let Some(position) = snapshot.position_secs {
        session.paused_position = position.max(0.0);
    }
    if let (Some(current), Some(duration)) = (session.current.as_mut(), snapshot.duration_secs)
        && duration > 0.0
    {
        current.duration_secs = Some(duration);
    }
    session.transport_pending = None;
    session.transport_pending_position_secs = None;
    session.notice = Some(error.to_string());
    if let Some(startup) = session.startup.as_mut() {
        startup.failed = true;
    }
}

pub(super) fn reconcile_session_with_transport(
    session: &mut UpnpSession,
    transport: UpnpTransportSnapshot,
    now: Instant,
) -> Option<String> {
    let projected_position = session_position_at(session, now);
    let previous_duration = session
        .current
        .as_ref()
        .and_then(|current| current.duration_secs);
    let mut promoted_source_key = None;

    if let Some(state) = transport.state {
        session.state = upnp_state_label(&state);
    }
    session.playback_speed = transport.playback_speed;

    let mut deferred_armed_next_uri = false;
    if let Some(current_uri) = transport.current_uri.as_deref() {
        let current_uri = current_uri.trim();
        if !current_uri.is_empty()
            && current_uri != "NOT_IMPLEMENTED"
            && session.state != "Transitioning"
            && session
                .current
                .as_ref()
                .is_some_and(|asset| asset.stream_url != current_uri)
        {
            let matches_armed_next = session
                .armed_next
                .as_ref()
                .is_some_and(|asset| asset.stream_url == current_uri);
            if matches_armed_next && session.state == "Playing" {
                if let Some(next) = session.armed_next.take() {
                    promoted_source_key = Some(next.source_ref.key());
                    session.current = Some(next);
                    session.paused_position = 0.0;
                    session.transport_pending = None;
                    session.transport_pending_position_secs = None;
                }
            } else if matches_armed_next {
                // Some KEF firmware loads NextURI as CurrentURI at the end of
                // a track but remains STOPPED. Loading is not playback: keep
                // the old current source and armed next item so the monitor can
                // issue a fresh Play fallback without consuming the queue.
                deferred_armed_next_uri = true;
            } else {
                session.current = None;
            }
        }
    }

    // Some renderers (notably KEF) start an armed SetNextAVTransportURI item
    // but continue returning the previous TrackURI. The reset from the
    // completed track's tail to the start of a playing track is authoritative
    // handoff evidence in that case. Promote before applying TrackDuration so
    // a next-track duration is not written onto the previous asset.
    if promoted_source_key.is_none()
        && upnp_timeline_indicates_armed_next(
            session,
            transport.position_secs,
            projected_position,
            previous_duration,
        )
        && let Some(next) = session.armed_next.take()
    {
        promoted_source_key = Some(next.source_ref.key());
        session.current = Some(next);
        session.paused_position = 0.0;
        session.transport_pending = None;
        session.transport_pending_position_secs = None;
    }

    if !deferred_armed_next_uri
        && let (Some(current), Some(duration)) = (session.current.as_mut(), transport.duration_secs)
        && duration > 0.0
    {
        current.duration_secs = Some(duration);
    }

    let mut observed_position = transport.position_secs.map(|position| position.max(0.0));
    if let Some(position) = observed_position
        && startup_position_is_implausibly_ahead(session, position, now)
    {
        observed_position = None;
    }
    if let Some(position) = observed_position {
        if session.state != "Playing"
            && upnp_reset_position_after_completion(session, position, projected_position)
        {
            session.state = "Stopped".to_string();
            session.paused_position = session
                .current
                .as_ref()
                .and_then(|current| current.duration_secs)
                .unwrap_or(projected_position);
        } else if session.state == "Transitioning" && position < projected_position {
            session.paused_position = projected_position;
        } else {
            session.paused_position = if session.state == "Playing"
                && (position - projected_position).abs() < UPNP_POSITION_RESYNC_THRESHOLD_SECS
            {
                projected_position
            } else {
                position
            };
        }
    }

    if session.state == "Playing" {
        if observed_position.is_some() || session.started_at.is_none() {
            session.started_at = Some(now);
        }
        if transport_pending_is_satisfied(session, observed_position) {
            session.transport_pending = None;
            session.transport_pending_position_secs = None;
        }
    } else {
        session.started_at = None;
        if !matches!(session.state.as_str(), "Transitioning") {
            session.transport_pending = None;
            session.transport_pending_position_secs = None;
        }
    }
    promoted_source_key
}

fn upnp_timeline_indicates_armed_next(
    session: &UpnpSession,
    observed_position: Option<f64>,
    projected_position: f64,
    previous_duration: Option<f64>,
) -> bool {
    session.current.is_some()
        && session.armed_next.is_some()
        && session.state == "Playing"
        && session.transport_pending.as_deref() != Some("seeking")
        && observed_position.is_some_and(|position| position <= UPNP_ENDED_RESET_POSITION_SECS)
        && previous_duration.is_some_and(|duration| {
            upnp_position_is_complete(projected_position, duration)
                || upnp_position_is_complete(session.paused_position, duration)
        })
}

pub(super) fn transport_pending_is_satisfied(
    session: &UpnpSession,
    observed_position: Option<f64>,
) -> bool {
    match session.transport_pending.as_deref() {
        Some("seeking") => {
            let Some(target) = session.transport_pending_position_secs else {
                return observed_position.is_some();
            };
            observed_position.is_some_and(|position| {
                (position - target).abs() <= UPNP_SEEK_PENDING_POSITION_TOLERANCE_SECS
            })
        }
        Some("loading") | Some("buffering") => true,
        Some(_) => observed_position.is_some(),
        None => false,
    }
}

pub(super) fn startup_position_is_implausibly_ahead(
    session: &UpnpSession,
    observed_position: f64,
    now: Instant,
) -> bool {
    if session.state != "Playing" || session.paused_position > UPNP_POSITION_RESYNC_THRESHOLD_SECS {
        return false;
    }
    let Some(startup) = session.startup.as_ref() else {
        return false;
    };
    let position_anchor = startup.accepted_at.unwrap_or(startup.started_at);
    let Some(elapsed) = now.checked_duration_since(position_anchor) else {
        return false;
    };
    if elapsed >= UPNP_STARTUP_PLAYING_TIMEOUT {
        return false;
    }
    observed_position > elapsed.as_secs_f64() + UPNP_STARTUP_POSITION_AHEAD_GRACE_SECS
}

pub(super) fn upnp_reset_position_after_completion(
    session: &UpnpSession,
    observed_position: f64,
    projected_position: f64,
) -> bool {
    if observed_position > UPNP_ENDED_RESET_POSITION_SECS {
        return false;
    }
    let Some(duration) = session
        .current
        .as_ref()
        .and_then(|current| current.duration_secs)
    else {
        return false;
    };
    upnp_position_is_complete(projected_position, duration)
        || upnp_position_is_complete(session.paused_position, duration)
}

pub(super) fn upnp_position_is_complete(position: f64, duration: f64) -> bool {
    duration.is_finite()
        && position.is_finite()
        && duration > 0.0
        && (position >= duration * UPNP_COMPLETION_RATIO
            || duration - position <= UPNP_COMPLETION_TAIL_SECONDS)
}

pub(super) fn upnp_state_label(value: &str) -> String {
    match value.trim() {
        "PLAYING" => "Playing",
        "TRANSITIONING" => "Transitioning",
        "PAUSED_PLAYBACK" | "PAUSED_RECORDING" => "Paused",
        "STOPPED" | "NO_MEDIA_PRESENT" => "Stopped",
        other if other.eq_ignore_ascii_case("playing") => "Playing",
        other if other.eq_ignore_ascii_case("transitioning") => "Transitioning",
        other if other.eq_ignore_ascii_case("paused") => "Paused",
        other if other.eq_ignore_ascii_case("stopped") => "Stopped",
        other => other,
    }
    .to_string()
}
