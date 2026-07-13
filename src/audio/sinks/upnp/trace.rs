use super::*;
use super::{probe::*, session::*, soap::*};

impl UpnpRendererService {
    pub fn mark_renderer_http_request(
        &self,
        asset_id: &str,
        token: &str,
        kind: &str,
        range: Option<&str>,
    ) {
        let context = self.trace_context_for_request(asset_id, token);
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = trace_for_request_mut(&mut traces, asset_id, context.as_ref()) else {
            return;
        };
        let elapsed_since_play_ms = trace_elapsed_ms(trace).or(trace.total_elapsed_ms);
        let request = UpnpHttpTrace {
            play_id: trace.play_id,
            asset_id: asset_id.to_string(),
            kind: kind.to_string(),
            range: range.map(str::to_string),
            since_play_ms: elapsed_since_play_ms,
            request_elapsed_ms: None,
            elapsed_since_play_ms,
        };
        eprintln!(
            "upnp: play trace event=renderer_http_request zone={} play_id={} asset={} kind={} range={} since_play_ms={}",
            trace.zone_id,
            trace.play_id,
            asset_id,
            kind,
            range.unwrap_or(""),
            elapsed_since_play_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
        if trace.first_renderer_request.is_none() {
            trace.first_renderer_request = Some(request.clone());
        }
        let mut next_first_byte_after_ms = None;
        let request_relative_to_eof_ms = elapsed_since_play_ms.and_then(|request| {
            trace
                .first_playing_observed_ms
                .zip(trace.current_duration_ms)
                .map(|(playing, duration)| request as i64 - (playing + duration) as i64)
        });
        if let Some(next) = trace.next_handoff.as_mut()
            && next.asset_id == asset_id
        {
            if next.renderer_requested_at_ms.is_none() {
                next.renderer_requested_at_ms = elapsed_since_play_ms;
                next.renderer_request_relative_to_eof_ms = request_relative_to_eof_ms;
            }
            if next.first_byte_after_next_ms.is_none() {
                next.first_byte_after_next_ms = elapsed_since_play_ms;
                next_first_byte_after_ms = elapsed_since_play_ms;
            }
            eprintln!(
                "upnp: play trace event=next_renderer_http_request zone={} play_id={} current_asset={} next_asset={} kind={} range={} since_play_ms={}",
                trace.zone_id,
                trace.play_id,
                trace.asset_id,
                asset_id,
                kind,
                range.unwrap_or(""),
                elapsed_since_play_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            );
        }
        if next_first_byte_after_ms.is_some() {
            trace.first_byte_after_seek_or_next_ms = next_first_byte_after_ms;
        }
        trace.renderer_requests.push(request);
    }

    pub fn generated_startup_pacing_allowed(&self, asset_id: &str, token: &str) -> bool {
        let context = self.trace_context_for_request(asset_id, token);
        let traces = self.traces.lock().unwrap();
        let Some(trace) = trace_for_request(&traces, asset_id, context.as_ref()) else {
            return true;
        };
        trace.renderer_requests.len() <= 1
    }

    pub fn mark_qobuz_proxy_first_byte(
        &self,
        asset_id: &str,
        token: &str,
        track_id: u64,
        range: Option<&str>,
        status: u16,
        request_elapsed_ms: Option<u64>,
    ) {
        let context = self.trace_context_for_request(asset_id, token);
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = trace_for_request_mut(&mut traces, asset_id, context.as_ref()) else {
            return;
        };
        let elapsed_since_play_ms = trace_elapsed_ms(trace).or(trace.total_elapsed_ms);
        let proxy = UpnpProxyTrace {
            play_id: trace.play_id,
            asset_id: asset_id.to_string(),
            track_id,
            range: range.map(str::to_string),
            status,
            since_play_ms: elapsed_since_play_ms,
            request_elapsed_ms,
            elapsed_since_play_ms,
        };
        eprintln!(
            "upnp: play trace event=qobuz_proxy_first_byte zone={} play_id={} asset={} track_id={} range={} status={} since_play_ms={} request_elapsed_ms={}",
            trace.zone_id,
            trace.play_id,
            asset_id,
            track_id,
            range.unwrap_or(""),
            status,
            elapsed_since_play_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            request_elapsed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
        if trace.first_qobuz_proxy_byte.is_none() {
            trace.first_qobuz_proxy_byte = Some(proxy.clone());
        }
        trace.qobuz_proxy_bytes.push(proxy);
        let zone_id = trace.zone_id.clone();
        let play_id = trace.play_id;
        drop(traces);
        self.mark_startup_accepted(&zone_id, play_id, "qobuz_proxy_first_byte");
    }

    pub fn mark_local_media_first_byte(
        &self,
        asset_id: &str,
        token: &str,
        range: Option<&str>,
        status: u16,
        request_elapsed_ms: Option<u64>,
    ) {
        self.mark_local_media_audio_ready(
            asset_id,
            token,
            range,
            status,
            request_elapsed_ms,
            "local_media_first_byte",
            "local_media_first_byte",
            false,
        );
    }

    pub fn mark_local_media_first_body_byte(
        &self,
        asset_id: &str,
        token: &str,
        range: Option<&str>,
        status: u16,
        request_elapsed_ms: Option<u64>,
    ) {
        let context = self.trace_context_for_request(asset_id, token);
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = trace_for_request_mut(&mut traces, asset_id, context.as_ref()) else {
            return;
        };
        let elapsed_since_play_ms = trace_elapsed_ms(trace).or(trace.total_elapsed_ms);
        if trace.first_local_body_byte_ms.is_none() {
            trace.first_local_body_byte_ms = elapsed_since_play_ms;
        }
        eprintln!(
            "upnp: play trace event=local_media_first_body_byte zone={} play_id={} asset={} range={} status={} since_play_ms={} request_elapsed_ms={}",
            trace.zone_id,
            trace.play_id,
            asset_id,
            range.unwrap_or(""),
            status,
            elapsed_since_play_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            request_elapsed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }

    pub fn mark_local_media_dop_frame(
        &self,
        asset_id: &str,
        token: &str,
        range: Option<&str>,
        status: u16,
        request_elapsed_ms: Option<u64>,
    ) {
        self.mark_local_media_audio_ready(
            asset_id,
            token,
            range,
            status,
            request_elapsed_ms,
            "local_media_dop_frame",
            "local_media_dop_frame",
            true,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn mark_local_media_audio_ready(
        &self,
        asset_id: &str,
        token: &str,
        range: Option<&str>,
        status: u16,
        request_elapsed_ms: Option<u64>,
        event: &str,
        startup_reason: &str,
        dop_frame: bool,
    ) {
        let context = self.trace_context_for_request(asset_id, token);
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = trace_for_request_mut(&mut traces, asset_id, context.as_ref()) else {
            return;
        };
        let elapsed_since_play_ms = trace_elapsed_ms(trace).or(trace.total_elapsed_ms);
        let zone_id = trace.zone_id.clone();
        let play_id = trace.play_id;
        if trace.first_local_audio_payload_ms.is_none() {
            trace.first_local_audio_payload_ms = elapsed_since_play_ms;
        }
        if dop_frame && trace.first_local_dop_frame_ms.is_none() {
            trace.first_local_dop_frame_ms = elapsed_since_play_ms;
        }
        eprintln!(
            "upnp: play trace event={} zone={} play_id={} asset={} range={} status={} since_play_ms={} request_elapsed_ms={}",
            event,
            zone_id,
            play_id,
            asset_id,
            range.unwrap_or(""),
            status,
            elapsed_since_play_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            request_elapsed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
        drop(traces);
        self.mark_startup_accepted(&zone_id, play_id, startup_reason);
        let seek_media_evidence =
            (range.is_some() && status == 206) || (range.is_none() && status == 200);
        if seek_media_evidence && self.trace_has_active_range_request_since_seek(&zone_id, play_id)
        {
            if range.is_none() && status == 200 {
                eprintln!(
                    "upnp: play trace event=seek_confirmed_by_full_reopen zone={} play_id={} asset={} since_play_ms={}",
                    zone_id,
                    play_id,
                    asset_id,
                    elapsed_since_play_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
            }
            self.mark_seek_media_range_ready(&zone_id, play_id, asset_id);
            let mut traces = self.traces.lock().unwrap();
            if let Some(trace) = traces.get_mut(&zone_id)
                && trace.play_id == play_id
                && trace.first_byte_after_seek_or_next_ms.is_none()
            {
                trace.first_byte_after_seek_or_next_ms = elapsed_since_play_ms;
            }
        }
    }

    pub fn mark_stale_command_discard(&self, zone_id: &str, reason: &str) {
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id) {
            trace.stale_command_discards = trace.stale_command_discards.saturating_add(1);
            eprintln!(
                "upnp: play trace event=stale_discard zone={} play_id={} asset={} reason={}",
                zone_id, trace.play_id, trace.asset_id, reason
            );
        }
    }

    pub fn diagnostics_for_zone(
        &self,
        zone_id: &str,
        public_base_url: String,
        target: UpnpRendererTarget,
    ) -> UpnpDiagnostics {
        let mut last_play_trace = self.traces.lock().unwrap().get(zone_id).cloned();
        if let Some(trace) = last_play_trace.as_mut()
            && let Some(session) = self.sessions.lock().unwrap().get(zone_id)
            && session.play_id == trace.play_id
        {
            trace.startup_phase = startup_phase(session);
            trace.startup_elapsed_ms = session
                .startup
                .as_ref()
                .map(|startup| elapsed_ms(startup.started_at));
            trace.startup_confirmation = session
                .startup
                .as_ref()
                .and_then(|startup| startup.accepted_reason.clone());
        }
        UpnpDiagnostics {
            zone_id: zone_id.to_string(),
            warnings: upnp_diagnostic_warnings(&public_base_url, &target),
            public_base_url,
            capability_probe: self
                .capability_probe_diagnostics
                .lock()
                .unwrap()
                .get(&target.id)
                .cloned(),
            renderer: target,
            last_play_trace,
        }
    }

    pub(super) fn trace_has_renderer_get(&self, zone_id: &str, play_id: u64) -> bool {
        self.traces
            .lock()
            .unwrap()
            .get(zone_id)
            .filter(|trace| trace.play_id == play_id)
            .is_some_and(|trace| {
                trace
                    .renderer_requests
                    .iter()
                    .any(|request| request.kind == "local_get")
            })
    }

    pub(super) fn trace_has_qobuz_proxy_byte(&self, zone_id: &str, play_id: u64) -> bool {
        self.traces
            .lock()
            .unwrap()
            .get(zone_id)
            .filter(|trace| trace.play_id == play_id)
            .is_some_and(|trace| !trace.qobuz_proxy_bytes.is_empty())
    }

    pub(super) fn trace_has_startup_fetch_evidence(&self, zone_id: &str, play_id: u64) -> bool {
        self.trace_has_qobuz_proxy_byte(zone_id, play_id)
            || self.trace_has_renderer_get(zone_id, play_id)
    }

    pub(super) fn trace_has_renderer_head(&self, zone_id: &str, play_id: u64) -> bool {
        self.traces
            .lock()
            .unwrap()
            .get(zone_id)
            .filter(|trace| trace.play_id == play_id)
            .is_some_and(|trace| {
                trace
                    .renderer_requests
                    .iter()
                    .any(|request| request.kind == "local_head")
            })
    }

    pub(super) fn begin_play_trace(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
        asset: &UpnpAsset,
    ) {
        self.register_stream_trace_context(zone_id, play_id, asset);
        let previous_handoff = self
            .traces
            .lock()
            .unwrap()
            .get(zone_id)
            .and_then(|trace| trace.next_handoff.as_ref())
            .filter(|handoff| handoff.fresh_play_after_completion)
            .cloned();
        let fallback_notice = previous_handoff
            .as_ref()
            .and_then(|handoff| handoff.fallback_reason.as_deref())
            .map(|reason| {
                format!(
                    "UPnP gapless handoff unavailable; this track was started with fallback auto-advance: {reason}"
                )
            });
        let trace = UpnpPlayTrace {
            play_id,
            zone_id: zone_id.to_string(),
            renderer_name: target.name.clone(),
            renderer_model: target.model.clone(),
            asset_id: asset.id.clone(),
            title: asset.title.clone(),
            mime_type: asset.mime_type.clone(),
            byte_len: asset.byte_len,
            stream_host: Url::parse(&asset.stream_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string)),
            started_at_ms: unix_epoch_ms(),
            current_duration_ms: asset.duration_secs.and_then(duration_millis),
            total_elapsed_ms: None,
            qobuz_resolve_ms: asset.qobuz_resolve_ms,
            asset_registration_ms: asset.asset_registration_ms,
            render_ms: asset.render_ms,
            prepare_ms: asset.prepare_ms,
            cache_hit: asset.cache_hit,
            render_or_stream_plan: asset.render_or_stream_plan.clone(),
            cache_lookup_ms: asset.cache_lookup_ms,
            cache_wait_ms: asset.cache_wait_ms,
            render_signature: asset.render_signature.clone(),
            configured_render_signature: asset.configured_render_signature.clone(),
            source_rate: asset.source_rate,
            target_rate: asset.target_rate,
            source_bits: asset.source_bits,
            target_bits: asset.target_bits,
            active_output_mode: asset.active_output_mode.clone(),
            render_container: if asset_is_dop_wav(asset) {
                Some("dop_wav".to_string())
            } else {
                upnp_pcm_container_from_mime(&asset.mime_type)
                    .map(|container| container.as_str().to_string())
            },
            first_renderer_request: None,
            renderer_requests: Vec::new(),
            first_local_body_byte_ms: None,
            first_local_audio_payload_ms: None,
            first_local_dop_frame_ms: None,
            first_qobuz_proxy_byte: None,
            qobuz_proxy_bytes: Vec::new(),
            first_playing_observed_ms: None,
            startup_phase: "Starting".to_string(),
            startup_confirmation: None,
            startup_elapsed_ms: Some(0),
            startup_accept_deadline_ms: UPNP_STARTUP_ACCEPT_TIMEOUT.as_millis() as u64,
            startup_playing_deadline_ms: UPNP_STARTUP_PLAYING_TIMEOUT.as_millis() as u64,
            last_transport_state: None,
            last_refresh_error: None,
            stale_command_discards: 0,
            soap: Vec::new(),
            seeks: Vec::new(),
            active_seek_started_ms: None,
            active_seek_renderer_request_count: 0,
            next_handoff: None,
            previous_handoff,
            dop_control_policy: None,
            skipped_initial_stop: false,
            hegel_mute_guard: None,
            used_renderer_next: false,
            handoff_promoted_without_play: false,
            dop_seek_strategy: None,
            first_byte_after_seek_or_next_ms: None,
            notice: fallback_notice.clone(),
        };
        eprintln!(
            "upnp: play trace event=start zone={} play_id={} renderer={} asset={} title={} mime={} source_rate={} target_rate={} source_bits={} target_bits={} active_output_mode={} render_container={} render_ms={} prepare_ms={} cache_hit={} render_or_stream_plan={} cache_lookup_ms={} cache_wait_ms={} qobuz_resolve_ms={} asset_registration_ms={}",
            zone_id,
            play_id,
            target.name,
            asset.id,
            asset.title.as_deref().unwrap_or(""),
            asset.mime_type,
            asset.source_rate,
            asset.target_rate,
            asset.source_bits,
            asset.target_bits,
            asset.active_output_mode.as_deref().unwrap_or("none"),
            upnp_pcm_container_from_mime(&asset.mime_type)
                .map(|container| container.as_str().to_string())
                .unwrap_or_else(|| "none".to_string()),
            asset
                .render_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            asset
                .prepare_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            asset
                .cache_hit
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            asset.render_or_stream_plan.as_deref().unwrap_or("none"),
            asset
                .cache_lookup_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            asset
                .cache_wait_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            asset
                .qobuz_resolve_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            asset
                .asset_registration_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
        self.traces
            .lock()
            .unwrap()
            .insert(zone_id.to_string(), trace);
        if let Some(notice) = fallback_notice {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(session) = sessions.get_mut(zone_id)
                && session.play_id == play_id
            {
                session.notice = Some(notice);
            }
        }
    }

    pub(super) fn register_stream_trace_context(
        &self,
        zone_id: &str,
        play_id: u64,
        asset: &UpnpAsset,
    ) {
        let Some(token) = stream_token_from_url(&asset.stream_url) else {
            return;
        };
        self.evict_expired_stream_trace_contexts();
        self.stream_trace_contexts.lock().unwrap().insert(
            token,
            UpnpTraceContext {
                zone_id: zone_id.to_string(),
                play_id,
                asset_id: asset.id.clone(),
                expires_at: Instant::now() + UPNP_ASSET_TTL,
            },
        );
    }

    pub(super) fn trace_context_for_request(
        &self,
        asset_id: &str,
        token: &str,
    ) -> Option<UpnpTraceContext> {
        if token.trim().is_empty() {
            return None;
        }
        self.evict_expired_stream_trace_contexts();
        self.stream_trace_contexts
            .lock()
            .unwrap()
            .get(token)
            .filter(|context| context.asset_id == asset_id)
            .cloned()
    }

    pub(super) fn finish_play_trace(&self, zone_id: &str, play_id: u64, notice: Option<String>) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        if trace.play_id != play_id {
            return;
        }
        trace.total_elapsed_ms = trace_elapsed_ms(trace);
        if notice.is_some() {
            trace.notice = notice;
        }
        eprintln!(
            "upnp: play trace event=finish zone={} play_id={} asset={} elapsed_ms={} notice={}",
            zone_id,
            trace.play_id,
            trace.asset_id,
            trace
                .total_elapsed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            trace.notice.as_deref().unwrap_or("")
        );
    }

    pub(super) fn record_soap_trace(&self, zone_id: &str, soap: UpnpSoapTrace) {
        eprintln!(
            "upnp: play trace event=soap zone={} action={} attempt={} ok={} elapsed_ms={} timeout_ms={} error={}",
            zone_id,
            soap.action,
            soap.attempt,
            soap.ok,
            soap.elapsed_ms,
            soap.timeout_ms,
            soap.error.as_deref().unwrap_or("")
        );
        if let Some(trace) = self.traces.lock().unwrap().get_mut(zone_id) {
            trace.soap.push(soap);
        }
    }

    pub(super) fn record_dop_control_policy(
        &self,
        zone_id: &str,
        play_id: u64,
        policy: UpnpDopControlPolicy,
        skipped_initial_stop: bool,
    ) {
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id)
            && trace.play_id == play_id
        {
            trace.dop_control_policy = Some(policy.as_str().to_string());
            trace.skipped_initial_stop = skipped_initial_stop;
            eprintln!(
                "upnp: play trace event=dop_control_policy zone={} play_id={} asset={} policy={} skipped_initial_stop={}",
                zone_id,
                play_id,
                trace.asset_id,
                policy.as_str(),
                skipped_initial_stop
            );
        }
    }

    pub fn mark_hegel_mute_guard(&self, zone_id: &str, state: &str) {
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id) {
            trace.hegel_mute_guard = Some(state.to_string());
            eprintln!(
                "upnp: play trace event=hegel_mute_guard zone={} play_id={} asset={} state={}",
                zone_id, trace.play_id, trace.asset_id, state
            );
        }
    }

    pub fn mark_dop_seek_strategy(&self, zone_id: &str, strategy: &str) {
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id) {
            trace.dop_seek_strategy = Some(strategy.to_string());
            eprintln!(
                "upnp: play trace event=dop_seek_strategy zone={} play_id={} asset={} strategy={}",
                zone_id, trace.play_id, trace.asset_id, strategy
            );
        }
    }

    pub(super) fn record_seek_trace(
        &self,
        zone_id: &str,
        target_secs: f64,
        seek_advertised: Option<bool>,
        verification: Option<String>,
        result: &Result<String, String>,
    ) {
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id) {
            let seek = UpnpSeekTrace {
                target_secs,
                seek_advertised,
                verification,
                elapsed_since_play_ms: trace_elapsed_ms(trace).or(trace.total_elapsed_ms),
                ok: result.is_ok(),
                error: result.as_ref().err().cloned(),
            };
            eprintln!(
                "upnp: play trace event=seek zone={} play_id={} asset={} target_secs={} ok={} seek_advertised={} verification={} since_play_ms={} error={}",
                zone_id,
                trace.play_id,
                trace.asset_id,
                target_secs,
                seek.ok,
                seek.seek_advertised
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                seek.verification.as_deref().unwrap_or(""),
                seek.elapsed_since_play_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                seek.error.as_deref().unwrap_or("")
            );
            trace.seeks.push(seek);
        }
    }

    pub(super) fn mark_seek_attempt_started(&self, zone_id: &str, play_id: u64) -> Option<u64> {
        let mut traces = self.traces.lock().unwrap();
        let trace = traces.get_mut(zone_id)?;
        if trace.play_id != play_id {
            return None;
        }
        let elapsed_ms = trace_elapsed_ms(trace)
            .or(trace.total_elapsed_ms)
            .unwrap_or(0);
        trace.active_seek_started_ms = Some(elapsed_ms);
        trace.active_seek_renderer_request_count = trace.renderer_requests.len();
        Some(elapsed_ms)
    }

    pub(super) fn record_next_handoff_prepared(
        &self,
        zone_id: &str,
        play_id: u64,
        asset: &UpnpAsset,
    ) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        if trace.play_id != play_id {
            return;
        }
        let prepared_at_ms = trace_elapsed_ms(trace)
            .or(trace.total_elapsed_ms)
            .unwrap_or(0);
        trace.next_handoff = Some(UpnpNextHandoffTrace {
            asset_id: asset.id.clone(),
            source_key: asset.source_ref.key(),
            title: asset.title.clone(),
            mime_type: asset.mime_type.clone(),
            stream_host: Url::parse(&asset.stream_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string)),
            prepared_at_ms,
            armed_at_ms: None,
            renderer_requested_at_ms: None,
            renderer_request_relative_to_eof_ms: None,
            promoted_at_ms: None,
            promoted_without_play: false,
            transition_path: None,
            fallback_reason: None,
            fresh_play_after_completion: false,
            first_byte_after_next_ms: None,
            ok: false,
            error: None,
        });
        eprintln!(
            "upnp: play trace event=next_asset_prepared zone={} play_id={} current_asset={} next_asset={} next_source_key={} mime={} elapsed_ms={}",
            zone_id,
            trace.play_id,
            trace.asset_id,
            asset.id,
            asset.source_ref.key(),
            asset.mime_type,
            prepared_at_ms
        );
    }

    pub(super) fn record_next_handoff_armed(
        &self,
        zone_id: &str,
        play_id: u64,
        asset: &UpnpAsset,
        result: &Result<(), String>,
    ) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        if trace.play_id != play_id {
            return;
        }
        let elapsed_ms = trace_elapsed_ms(trace).or(trace.total_elapsed_ms);
        if let Some(next) = trace.next_handoff.as_mut()
            && next.asset_id == asset.id
        {
            next.armed_at_ms = elapsed_ms;
            next.ok = result.is_ok();
            next.error = result.as_ref().err().cloned();
        }
        eprintln!(
            "upnp: play trace event=next_asset_armed zone={} play_id={} current_asset={} next_asset={} ok={} elapsed_ms={} error={}",
            zone_id,
            trace.play_id,
            trace.asset_id,
            asset.id,
            result.is_ok(),
            elapsed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            result.as_ref().err().map(String::as_str).unwrap_or("")
        );
    }

    pub fn mark_next_handoff_promoted(&self, zone_id: &str, source_key: &str) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        let elapsed_ms = trace_elapsed_ms(trace).or(trace.total_elapsed_ms);
        if let Some(next) = trace.next_handoff.as_mut()
            && next.source_key == source_key
        {
            next.promoted_at_ms = elapsed_ms;
        }
        eprintln!(
            "upnp: play trace event=next_asset_promoted zone={} play_id={} current_asset={} next_source_key={} elapsed_ms={}",
            zone_id,
            trace.play_id,
            trace.asset_id,
            source_key,
            elapsed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }

    pub fn mark_next_handoff_fallback(&self, zone_id: &str, source_key: &str, reason: &str) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        if let Some(next) = trace.next_handoff.as_mut()
            && next.source_key == source_key
        {
            next.transition_path = Some("fallback_auto_advance".to_string());
            next.fallback_reason = Some(reason.to_string());
            next.fresh_play_after_completion = true;
        }
        trace.notice = Some(format!(
            "UPnP gapless handoff unavailable; using fallback auto-advance: {reason}"
        ));
        eprintln!(
            "upnp: play trace event=next_handoff_fallback zone={} play_id={} current_asset={} next_source_key={} reason={} fresh_play_after_completion=true",
            zone_id, trace.play_id, trace.asset_id, source_key, reason
        );
    }

    pub(super) fn mark_next_handoff_transition_path(
        &self,
        zone_id: &str,
        source_key: &str,
        path: &str,
    ) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        if let Some(next) = trace.next_handoff.as_mut()
            && next.source_key == source_key
        {
            next.transition_path = Some(path.to_string());
        }
    }

    pub(super) fn mark_renderer_next_used(&self, zone_id: &str, source_key: &str) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        trace.used_renderer_next = true;
        if let Some(next) = trace.next_handoff.as_mut()
            && next.source_key == source_key
        {
            next.promoted_without_play = true;
        }
        eprintln!(
            "upnp: play trace event=renderer_next_used zone={} play_id={} asset={} next_source_key={}",
            zone_id, trace.play_id, trace.asset_id, source_key
        );
    }

    pub(super) fn mark_handoff_promoted_without_play(&self, zone_id: &str, source_key: &str) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        trace.handoff_promoted_without_play = true;
        let elapsed_ms = trace_elapsed_ms(trace).or(trace.total_elapsed_ms);
        if let Some(next) = trace.next_handoff.as_mut()
            && next.source_key == source_key
        {
            next.promoted_at_ms = elapsed_ms;
            next.promoted_without_play = true;
        }
        eprintln!(
            "upnp: play trace event=handoff_promoted_without_play zone={} play_id={} asset={} next_source_key={} elapsed_ms={}",
            zone_id,
            trace.play_id,
            trace.asset_id,
            source_key,
            elapsed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }

    pub(super) fn trace_has_active_range_request_since_seek(
        &self,
        zone_id: &str,
        play_id: u64,
    ) -> bool {
        self.traces
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|trace| {
                let Some(seek_started_ms) = trace.active_seek_started_ms else {
                    return false;
                };
                trace.play_id == play_id
                    && trace
                        .renderer_requests
                        .iter()
                        .skip(trace.active_seek_renderer_request_count)
                        .any(|request| {
                            request.kind == "local_get"
                                && request
                                    .since_play_ms
                                    .is_some_and(|ms| ms >= seek_started_ms)
                        })
            })
    }

    pub fn trace_has_seek_media_evidence(&self, zone_id: &str, play_id: u64) -> bool {
        self.traces
            .lock()
            .unwrap()
            .get(zone_id)
            .is_some_and(|trace| {
                trace.play_id == play_id && trace.first_byte_after_seek_or_next_ms.is_some()
            })
    }

    pub(super) fn mark_startup_accepted(&self, zone_id: &str, play_id: u64, reason: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(zone_id) else {
            return;
        };
        if session.play_id != play_id {
            drop(sessions);
            self.mark_stale_command_discard(zone_id, "startup_acceptance_for_stale_play");
            return;
        }
        let now = Instant::now();
        let current_asset_id = session.current.as_ref().map(|asset| asset.id.clone());
        let Some(startup) = session.startup.as_mut() else {
            return;
        };
        if startup.play_id != play_id
            || Some(startup.asset_id.as_str()) != current_asset_id.as_deref()
        {
            drop(sessions);
            self.mark_stale_command_discard(zone_id, "startup_acceptance_for_mismatched_asset");
            return;
        }
        if startup.accepted_at.is_none() {
            startup.accepted_at = Some(now);
            startup.accepted_reason = Some(reason.to_string());
            drop(sessions);
            let mut traces = self.traces.lock().unwrap();
            if let Some(trace) = traces.get_mut(zone_id)
                && trace.play_id == play_id
            {
                trace.startup_phase = "Accepted".to_string();
                trace.startup_confirmation = Some(reason.to_string());
                trace.startup_elapsed_ms = trace_elapsed_ms(trace);
                eprintln!(
                    "upnp: play trace event=startup_confirmed zone={} play_id={} asset={} reason={} elapsed_ms={}",
                    zone_id,
                    play_id,
                    trace.asset_id,
                    reason,
                    trace
                        .startup_elapsed_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
            }
        }
    }

    pub(super) fn mark_startup_timeout(&self, zone_id: &str, play_id: u64, notice: &str) {
        {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(session) = sessions.get_mut(zone_id)
                && session.play_id == play_id
            {
                session.notice = Some(notice.to_string());
                if let Some(startup) = session.startup.as_mut() {
                    startup.timed_out = true;
                }
            }
        }
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id)
            && trace.play_id == play_id
        {
            trace.startup_phase = "TimedOut".to_string();
            trace.startup_elapsed_ms = trace_elapsed_ms(trace);
            trace.notice = Some(notice.to_string());
            eprintln!(
                "upnp: play trace event=startup_timeout zone={} play_id={} asset={} elapsed_ms={} notice={}",
                zone_id,
                play_id,
                trace.asset_id,
                trace
                    .startup_elapsed_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                notice
            );
        }
    }

    pub(super) fn record_startup_failure(&self, zone_id: &str, play_id: u64, notice: &str) {
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id)
            && trace.play_id == play_id
        {
            trace.startup_phase = "Failed".to_string();
            trace.startup_elapsed_ms = trace_elapsed_ms(trace);
            trace.notice = Some(notice.to_string());
            eprintln!(
                "upnp: play trace event=startup_failed zone={} play_id={} asset={} elapsed_ms={} notice={}",
                zone_id,
                play_id,
                trace.asset_id,
                trace
                    .startup_elapsed_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                notice
            );
        }
    }

    pub(super) fn record_transport_state(&self, zone_id: &str, state: Option<&str>) {
        let Some(state) = state else {
            return;
        };
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id) {
            trace.last_transport_state = Some(upnp_state_label(state));
        }
    }

    pub(super) fn record_refresh_error(
        &self,
        zone_id: &str,
        play_id: u64,
        error: &str,
        inconclusive: bool,
    ) {
        let mut traces = self.traces.lock().unwrap();
        if let Some(trace) = traces.get_mut(zone_id)
            && trace.play_id == play_id
        {
            trace.last_refresh_error = Some(error.to_string());
            eprintln!(
                "upnp: play trace event=refresh_timeout zone={} play_id={} asset={} inconclusive={} error={}",
                zone_id, play_id, trace.asset_id, inconclusive, error
            );
        }
    }

    pub(super) fn mark_first_playing_observed(&self, zone_id: &str) {
        let mut traces = self.traces.lock().unwrap();
        let Some(trace) = traces.get_mut(zone_id) else {
            return;
        };
        if trace.first_playing_observed_ms.is_some() {
            return;
        }
        trace.first_playing_observed_ms = trace_elapsed_ms(trace);
        trace.startup_phase = "Playing".to_string();
        eprintln!(
            "upnp: play trace event=first_playing zone={} play_id={} asset={} elapsed_ms={}",
            zone_id,
            trace.play_id,
            trace.asset_id,
            trace
                .first_playing_observed_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }
}

pub(super) fn trace_for_request<'a>(
    traces: &'a HashMap<String, UpnpPlayTrace>,
    asset_id: &str,
    context: Option<&UpnpTraceContext>,
) -> Option<&'a UpnpPlayTrace> {
    if let Some(zone_id) = context.and_then(|context| {
        traces
            .get(&context.zone_id)
            .filter(|trace| {
                trace.play_id == context.play_id
                    && (trace.asset_id == asset_id
                        || trace
                            .next_handoff
                            .as_ref()
                            .is_some_and(|next| next.asset_id == asset_id))
            })
            .map(|_| context.zone_id.clone())
    }) {
        return traces.get(&zone_id);
    }
    traces.values().find(|trace| {
        trace.asset_id == asset_id
            || trace
                .next_handoff
                .as_ref()
                .is_some_and(|next| next.asset_id == asset_id)
    })
}

pub(super) fn trace_for_request_mut<'a>(
    traces: &'a mut HashMap<String, UpnpPlayTrace>,
    asset_id: &str,
    context: Option<&UpnpTraceContext>,
) -> Option<&'a mut UpnpPlayTrace> {
    if let Some(zone_id) = context.and_then(|context| {
        traces
            .get(&context.zone_id)
            .filter(|trace| {
                trace.play_id == context.play_id
                    && (trace.asset_id == asset_id
                        || trace
                            .next_handoff
                            .as_ref()
                            .is_some_and(|next| next.asset_id == asset_id))
            })
            .map(|_| context.zone_id.clone())
    }) {
        return traces.get_mut(&zone_id);
    }
    traces.values_mut().find(|trace| {
        trace.asset_id == asset_id
            || trace
                .next_handoff
                .as_ref()
                .is_some_and(|next| next.asset_id == asset_id)
    })
}

pub(super) fn stream_token_from_url(stream_url: &str) -> Option<String> {
    let url = Url::parse(stream_url).ok()?;
    if let Some((_, token)) = url.query_pairs().find(|(key, _)| key == "token") {
        return Some(token.into_owned());
    }
    let segments: Vec<_> = url.path_segments()?.collect();
    if segments.len() >= 5 && segments[0] == "upnp" && segments[1] == "qobuz" {
        return Some(segments[3].to_string());
    }
    None
}

pub(super) fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

pub(super) fn trace_elapsed_ms(trace: &UpnpPlayTrace) -> Option<u64> {
    let now = unix_epoch_ms();
    now.checked_sub(trace.started_at_ms)
}

fn duration_millis(duration_secs: f64) -> Option<u64> {
    if !duration_secs.is_finite() || duration_secs <= 0.0 {
        return None;
    }
    Some((duration_secs * 1000.0).round().clamp(0.0, u64::MAX as f64) as u64)
}

pub(super) fn unix_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
