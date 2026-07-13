use super::*;
use super::{session::*, soap::*, trace::*};

impl UpnpRendererService {
    pub async fn calibrate_renderer_capabilities(
        &self,
        zone_id: &str,
    ) -> Result<UpnpCapabilityCalibration, String> {
        let target_id = zone_id
            .strip_prefix("upnp-")
            .ok_or_else(|| format!("Zone '{zone_id}' is not a UPnP renderer"))?;
        let (target, online) = {
            let renderers = self.renderers.lock().unwrap();
            let renderer = renderers
                .get(target_id)
                .ok_or_else(|| format!("UPnP renderer for zone '{zone_id}' is not available"))?;
            (renderer.target.clone(), renderer.online)
        };
        if is_sonos_renderer_target(&target) {
            return Ok(UpnpCapabilityCalibration {
                message: target
                    .capability_detection_message
                    .clone()
                    .unwrap_or_else(|| "Sonos UPnP transport limit".to_string()),
                target,
            });
        }
        if !online {
            let message =
                "UPnP renderer is offline; refresh outputs and try calibration again".to_string();
            self.update_renderer_detection_state(
                &target.id,
                target.capability_detection_source,
                CapabilityDetectionStatus::Failed,
                Some(message.clone()),
            );
            let target = self
                .renderer_target(&target.id)
                .unwrap_or_else(|| target.clone());
            return Ok(UpnpCapabilityCalibration { target, message });
        }
        let cache_key = capability_probe_cache_key(&target);
        if self
            .capability_probe_tasks
            .lock()
            .unwrap()
            .contains(&cache_key)
        {
            let message = "UPnP capability calibration is already running".to_string();
            self.update_renderer_detection_state(
                &target.id,
                CapabilityDetectionSource::Probing,
                CapabilityDetectionStatus::Probing,
                Some(message.clone()),
            );
            let target = self
                .renderer_target(&target.id)
                .unwrap_or_else(|| target.clone());
            return Ok(UpnpCapabilityCalibration { target, message });
        }
        if !self.session_idle_for_probe(zone_id) {
            let message =
                "UPnP capability calibration deferred while renderer is active".to_string();
            self.update_renderer_detection_state(
                &target.id,
                target.capability_detection_source,
                CapabilityDetectionStatus::Deferred,
                Some(message.clone()),
            );
            let target = self
                .renderer_target(&target.id)
                .unwrap_or_else(|| target.clone());
            return Ok(UpnpCapabilityCalibration { target, message });
        }
        {
            let mut tasks = self.capability_probe_tasks.lock().unwrap();
            if !tasks.insert(cache_key.clone()) {
                let message = "UPnP capability calibration is already running".to_string();
                self.update_renderer_detection_state(
                    &target.id,
                    CapabilityDetectionSource::Probing,
                    CapabilityDetectionStatus::Probing,
                    Some(message.clone()),
                );
                let target = self
                    .renderer_target(&target.id)
                    .unwrap_or_else(|| target.clone());
                return Ok(UpnpCapabilityCalibration { target, message });
            }
        }
        self.update_renderer_detection_state(
            &target.id,
            CapabilityDetectionSource::Probing,
            CapabilityDetectionStatus::Probing,
            Some("Calibrating UPnP renderer capabilities".to_string()),
        );

        let result = match tokio::time::timeout(
            UPNP_PROBE_TOTAL_TIMEOUT,
            self.probe_renderer_capabilities(zone_id, &target),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let message = format!(
                    "UPnP capability calibration timed out after {}s",
                    UPNP_PROBE_TOTAL_TIMEOUT.as_secs()
                );
                self.cleanup_probe_after_abort(zone_id, &target).await;
                self.finish_capability_probe_diagnostics(
                    &target.id,
                    target.capability_detection_source,
                    CapabilityDetectionStatus::Failed,
                    Some(message.clone()),
                    target.max_sample_rate,
                    target.max_bit_depth,
                    target.max_dsd_rate,
                    &target.pcm_containers,
                    Some("timeout".to_string()),
                );
                UpnpCapabilityProbeResult {
                    max_sample_rate: target.max_sample_rate,
                    max_bit_depth: target.max_bit_depth,
                    max_dsd_rate: target.max_dsd_rate,
                    detection_source: target.capability_detection_source,
                    detection_status: CapabilityDetectionStatus::Failed,
                    detection_message: Some(message),
                    basis: Some("timeout".to_string()),
                    pcm_containers: target.pcm_containers.clone(),
                }
            }
        };
        if matches!(
            result.detection_status,
            CapabilityDetectionStatus::Complete | CapabilityDetectionStatus::Unknown
        ) && result.detection_source == CapabilityDetectionSource::Probed
        {
            self.cache_capability_result(&target, result.clone());
        }
        let message = result
            .detection_message
            .clone()
            .unwrap_or_else(|| "UPnP capability calibration finished".to_string());
        let mut target_after = target.clone();
        {
            let mut renderers = self.renderers.lock().unwrap();
            if let Some(renderer) = renderers.get_mut(&target.id) {
                apply_probe_result_to_target(&mut renderer.target, result);
                target_after = renderer.target.clone();
            }
        }
        self.capability_probe_tasks
            .lock()
            .unwrap()
            .remove(&cache_key);
        Ok(UpnpCapabilityCalibration {
            target: target_after,
            message,
        })
    }

    pub(super) fn update_renderer_detection_state(
        &self,
        renderer_id: &str,
        source: CapabilityDetectionSource,
        status: CapabilityDetectionStatus,
        message: Option<String>,
    ) {
        let mut renderers = self.renderers.lock().unwrap();
        if let Some(renderer) = renderers.get_mut(renderer_id) {
            renderer.target.capability_detection_source = source;
            renderer.target.capability_detection_status = status;
            renderer.target.capability_detection_message = message;
        }
    }

    pub(super) fn renderer_target(&self, renderer_id: &str) -> Option<UpnpRendererTarget> {
        self.renderers
            .lock()
            .unwrap()
            .get(renderer_id)
            .map(|renderer| renderer.target.clone())
    }

    pub(super) fn promote_capabilities_from_observed_playback(
        &self,
        target: &UpnpRendererTarget,
        asset: &UpnpAsset,
    ) {
        if is_probe_asset(asset) || is_sonos_renderer_target(target) {
            return;
        }
        let promoted = {
            let mut renderers = self.renderers.lock().unwrap();
            let Some(renderer) = renderers.get_mut(&target.id) else {
                return;
            };
            let observed_rate = if asset.render_ms.is_some() {
                asset.target_rate
            } else {
                asset.source_rate
            };
            let observed_bits = if asset.render_ms.is_some() {
                asset.target_bits
            } else {
                asset.source_bits
            };
            if observed_bits == 1 {
                let Some(dsd_rate) = dsd_rate_for_sample_rate(observed_rate) else {
                    return;
                };
                if renderer
                    .target
                    .max_dsd_rate
                    .is_some_and(|current| current >= dsd_rate)
                {
                    return;
                }
                renderer.target.max_dsd_rate = Some(dsd_rate);
                renderer.target.capability_detection_source = CapabilityDetectionSource::Probed;
                renderer.target.capability_detection_status = CapabilityDetectionStatus::Complete;
                renderer.target.capability_detection_message =
                    Some(format!("Observed successful DSD{dsd_rate} UPnP playback"));
            } else {
                if observed_rate == 0 || observed_bits == 0 {
                    return;
                }
                let observed_bits_u8 = observed_bits.min(u32::from(u8::MAX)) as u8;
                let improves_rate = observed_rate > renderer.target.max_sample_rate;
                let improves_bits = observed_bits_u8 > renderer.target.max_bit_depth
                    && observed_rate >= renderer.target.max_sample_rate;
                if !improves_rate && !improves_bits {
                    return;
                }
                renderer.target.max_sample_rate =
                    renderer.target.max_sample_rate.max(observed_rate);
                renderer.target.max_bit_depth = renderer.target.max_bit_depth.max(observed_bits_u8);
                if let Some(container) = upnp_pcm_container_from_mime(&asset.mime_type) {
                    upsert_pcm_container_capability(
                        &mut renderer.target.pcm_containers,
                        container,
                        observed_rate,
                        observed_bits_u8,
                    );
                }
                renderer.target.capability_detection_source = CapabilityDetectionSource::Probed;
                renderer.target.capability_detection_status = CapabilityDetectionStatus::Unknown;
                renderer.target.capability_detection_message = Some(format!(
                    "Observed successful UPnP playback at {}/{}; DSD still unknown",
                    observed_bits_u8, observed_rate
                ));
            }
            UpnpCapabilityProbeResult {
                max_sample_rate: renderer.target.max_sample_rate,
                max_bit_depth: renderer.target.max_bit_depth,
                max_dsd_rate: renderer.target.max_dsd_rate,
                detection_source: renderer.target.capability_detection_source,
                detection_status: renderer.target.capability_detection_status,
                detection_message: renderer.target.capability_detection_message.clone(),
                basis: Some("observed_playback".to_string()),
                pcm_containers: renderer.target.pcm_containers.clone(),
            }
        };
        self.cache_capability_result(target, promoted.clone());
        self.record_observed_capability_promotion(target, &promoted);
    }

    pub(super) fn cache_capability_result(
        &self,
        target: &UpnpRendererTarget,
        result: UpnpCapabilityProbeResult,
    ) {
        let cache_key = capability_probe_cache_key(target);
        let mut cache = self.capability_probe_cache.lock().unwrap();
        let merged = cache
            .get(&cache_key)
            .cloned()
            .map(|existing| merge_capability_results(existing, result.clone()))
            .unwrap_or(result);
        cache.insert(cache_key, merged);
    }

    pub(super) fn start_capability_probe_diagnostics(&self, target: &UpnpRendererTarget) {
        self.capability_probe_diagnostics.lock().unwrap().insert(
            target.id.clone(),
            UpnpCapabilityProbeDiagnostics {
                renderer_id: target.id.clone(),
                source: CapabilityDetectionSource::Probing,
                status: CapabilityDetectionStatus::Probing,
                message: Some("Detecting UPnP renderer capabilities".to_string()),
                started_at_ms: unix_epoch_ms(),
                finished_at_ms: None,
                final_max_sample_rate: target.max_sample_rate,
                final_max_bit_depth: target.max_bit_depth,
                final_max_dsd_rate: target.max_dsd_rate,
                final_pcm_containers: target.pcm_containers.clone(),
                basis: Some("active_probe".to_string()),
                attempts: Vec::new(),
            },
        );
    }

    pub(super) fn begin_capability_probe_attempt(
        &self,
        target: &UpnpRendererTarget,
        kind: &str,
        candidate: &str,
        asset: &UpnpAsset,
        probe_target: &UpnpRendererTarget,
    ) -> usize {
        let protocol_info = protocol_info_for_asset(asset, probe_target);
        let mut diagnostics = self.capability_probe_diagnostics.lock().unwrap();
        let entry = diagnostics.entry(target.id.clone()).or_insert_with(|| {
            UpnpCapabilityProbeDiagnostics {
                renderer_id: target.id.clone(),
                source: CapabilityDetectionSource::Probing,
                status: CapabilityDetectionStatus::Probing,
                message: Some("Detecting UPnP renderer capabilities".to_string()),
                started_at_ms: unix_epoch_ms(),
                finished_at_ms: None,
                final_max_sample_rate: target.max_sample_rate,
                final_max_bit_depth: target.max_bit_depth,
                final_max_dsd_rate: target.max_dsd_rate,
                final_pcm_containers: target.pcm_containers.clone(),
                basis: Some("active_probe".to_string()),
                attempts: Vec::new(),
            }
        });
        let index = entry.attempts.len();
        entry.attempts.push(UpnpCapabilityProbeAttempt {
            kind: kind.to_string(),
            candidate: candidate.to_string(),
            mime_type: asset.mime_type.clone(),
            protocol_info,
            started_at_ms: unix_epoch_ms(),
            finished_at_ms: None,
            accepted: false,
            renderer_get: false,
            renderer_head: false,
            playing_observed: false,
            terminal_state: None,
            evidence: None,
            error: None,
        });
        index
    }

    pub(super) fn finish_capability_probe_attempt(
        &self,
        renderer_id: &str,
        attempt_index: usize,
        acceptance: &UpnpProbeAcceptance,
    ) {
        let mut diagnostics = self.capability_probe_diagnostics.lock().unwrap();
        let Some(entry) = diagnostics.get_mut(renderer_id) else {
            return;
        };
        let Some(attempt) = entry.attempts.get_mut(attempt_index) else {
            return;
        };
        attempt.finished_at_ms = Some(unix_epoch_ms());
        attempt.accepted = acceptance.accepted;
        attempt.renderer_get = acceptance.renderer_get;
        attempt.renderer_head = acceptance.renderer_head;
        attempt.playing_observed = acceptance.playing_observed;
        attempt.terminal_state = acceptance.terminal_state.clone();
        attempt.evidence = acceptance.evidence.clone();
        attempt.error = acceptance.error.clone();
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_capability_probe_diagnostics(
        &self,
        renderer_id: &str,
        source: CapabilityDetectionSource,
        status: CapabilityDetectionStatus,
        message: Option<String>,
        max_sample_rate: u32,
        max_bit_depth: u8,
        max_dsd_rate: Option<u16>,
        pcm_containers: &[UpnpPcmContainerCapability],
        basis: Option<String>,
    ) {
        let mut diagnostics = self.capability_probe_diagnostics.lock().unwrap();
        let entry = diagnostics
            .entry(renderer_id.to_string())
            .or_insert_with(|| UpnpCapabilityProbeDiagnostics {
                renderer_id: renderer_id.to_string(),
                source,
                status,
                message: message.clone(),
                started_at_ms: unix_epoch_ms(),
                finished_at_ms: None,
                final_max_sample_rate: max_sample_rate,
                final_max_bit_depth: max_bit_depth,
                final_max_dsd_rate: max_dsd_rate,
                final_pcm_containers: pcm_containers.to_vec(),
                basis: basis.clone(),
                attempts: Vec::new(),
            });
        entry.source = source;
        entry.status = status;
        entry.message = message;
        entry.finished_at_ms = Some(unix_epoch_ms());
        entry.final_max_sample_rate = max_sample_rate;
        entry.final_max_bit_depth = max_bit_depth;
        entry.final_max_dsd_rate = max_dsd_rate;
        entry.final_pcm_containers = pcm_containers.to_vec();
        entry.basis = basis;
    }

    pub(super) fn record_observed_capability_promotion(
        &self,
        target: &UpnpRendererTarget,
        result: &UpnpCapabilityProbeResult,
    ) {
        self.finish_capability_probe_diagnostics(
            &target.id,
            result.detection_source,
            result.detection_status,
            result.detection_message.clone(),
            result.max_sample_rate,
            result.max_bit_depth,
            result.max_dsd_rate,
            &result.pcm_containers,
            result.basis.clone(),
        );
    }

    pub(super) fn session_idle_for_probe(&self, zone_id: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(zone_id).is_none_or(|session| {
            !matches!(
                upnp_state_label(&session.state).as_str(),
                "Playing" | "Transitioning"
            )
        })
    }

    pub(super) async fn renderer_transport_idle_for_probe(
        &self,
        target: &UpnpRendererTarget,
    ) -> bool {
        match tokio::time::timeout(
            UPNP_PLAYBACK_REFRESH_TIMEOUT,
            self.transport_snapshot(target),
        )
        .await
        {
            Ok(Ok(snapshot)) => snapshot.state.as_deref().is_none_or(|state| {
                !matches!(
                    upnp_state_label(state).as_str(),
                    "Playing" | "Transitioning"
                )
            }),
            _ => false,
        }
    }

    pub(super) async fn probe_renderer_capabilities(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
    ) -> UpnpCapabilityProbeResult {
        self.start_capability_probe_diagnostics(target);
        let mut result = UpnpCapabilityProbeResult {
            max_sample_rate: target.max_sample_rate,
            max_bit_depth: target.max_bit_depth,
            max_dsd_rate: target.max_dsd_rate,
            detection_source: target.capability_detection_source,
            detection_status: CapabilityDetectionStatus::Failed,
            detection_message: Some("Capability probe did not complete".to_string()),
            basis: Some("active_probe".to_string()),
            pcm_containers: target.pcm_containers.clone(),
        };
        let command_lock = self.command_lock_for_zone(zone_id);
        let _guard = command_lock.lock().await;
        if !self.session_idle_for_probe(zone_id)
            || !self.renderer_transport_idle_for_probe(target).await
        {
            result.detection_status = CapabilityDetectionStatus::Deferred;
            result.detection_message =
                Some("Capability probe deferred while renderer is active".to_string());
            result.basis = Some("idle_check".to_string());
            self.finish_capability_probe_diagnostics(
                &target.id,
                result.detection_source,
                result.detection_status,
                result.detection_message.clone(),
                result.max_sample_rate,
                result.max_bit_depth,
                result.max_dsd_rate,
                &result.pcm_containers,
                result.basis.clone(),
            );
            return result;
        }

        for candidate in pcm_probe_candidates(target) {
            let formats = pcm_probe_formats_for_candidate(target, candidate);
            if let Ok(format) = self
                .probe_pcm_candidate_locked(zone_id, target, candidate, &formats)
                .await
            {
                result.max_sample_rate = candidate.sample_rate;
                result.max_bit_depth = target.max_bit_depth.max(candidate.bit_depth);
                upsert_pcm_container_capability(
                    &mut result.pcm_containers,
                    format.container(),
                    candidate.sample_rate,
                    candidate.bit_depth,
                );
                result.detection_source = CapabilityDetectionSource::Probed;
                result.detection_status = CapabilityDetectionStatus::Complete;
                result.detection_message = Some(format!(
                    "PCM capability probe accepted {}kHz/{} as {}",
                    candidate.sample_rate / 1000,
                    candidate.bit_depth,
                    format.container().as_str()
                ));
                result.basis = Some("active_probe_pcm".to_string());
                break;
            }
        }
        if result.max_dsd_rate.is_none() && should_probe_dsd(target, &result) {
            let continue_after_dsd64 = renderer_supports_dsf(target);
            for rate in UPNP_DSD_PROBE_RATES {
                if self
                    .probe_dsf_rate_locked(zone_id, target, rate)
                    .await
                    .is_ok()
                {
                    result.max_dsd_rate = Some(rate);
                    result.detection_source = CapabilityDetectionSource::Probed;
                    result.detection_status = CapabilityDetectionStatus::Complete;
                    result.detection_message =
                        Some(format!("PCM/DSD capability probes accepted DSD{rate}"));
                    result.basis = Some("active_probe_dsd".to_string());
                    if !continue_after_dsd64 {
                        break;
                    }
                }
            }
        }
        if result.detection_source == CapabilityDetectionSource::Fallback {
            result.detection_status = CapabilityDetectionStatus::Failed;
            result.detection_message =
                Some("Capability probe failed; using safe UPnP defaults".to_string());
            result.basis = Some("active_probe_failed".to_string());
        }
        self.finish_capability_probe_diagnostics(
            &target.id,
            result.detection_source,
            result.detection_status,
            result.detection_message.clone(),
            result.max_sample_rate,
            result.max_bit_depth,
            result.max_dsd_rate,
            &result.pcm_containers,
            result.basis.clone(),
        );
        result
    }

    pub(super) async fn probe_pcm_candidate_locked(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        candidate: PcmProbeCandidate,
        formats: &[PcmProbeFormat],
    ) -> Result<PcmProbeFormat, String> {
        let mut last_error = None;
        for format in formats {
            match self
                .probe_pcm_rate_locked(zone_id, target, candidate, *format)
                .await
            {
                Ok(()) => return Ok(*format),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            format!(
                "UPnP probe {}Hz/{} was not accepted by renderer",
                candidate.sample_rate, candidate.bit_depth
            )
        }))
    }

    pub(super) async fn probe_pcm_rate_locked(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        candidate: PcmProbeCandidate,
        format: PcmProbeFormat,
    ) -> Result<(), String> {
        let sample_rate = candidate.sample_rate;
        let bit_depth = candidate.bit_depth;
        let path = write_probe_pcm_file(sample_rate, bit_depth, format)?;
        let byte_len = std::fs::metadata(&path).ok().map(|meta| meta.len());
        let mut probe_target = target.clone();
        probe_target.max_sample_rate = sample_rate;
        probe_target.max_bit_depth = probe_target.max_bit_depth.max(bit_depth);
        probe_target.capability_detection_source = CapabilityDetectionSource::Probing;
        probe_target.capability_detection_status = CapabilityDetectionStatus::Probing;
        probe_target.capability_detection_message = Some(format!(
            "Probing PCM {}kHz/{}",
            sample_rate / 1000,
            bit_depth
        ));
        let asset = self.prepare_source(
            UpnpSource::LocalFile {
                source_ref: SourceRef::LocalTrack {
                    track_id: -1,
                    file_name: Some(path.to_string_lossy().to_string()),
                    title: Some(format!(
                        "Fozmo probe {}kHz/{}",
                        sample_rate / 1000,
                        bit_depth
                    )),
                    artist: Some("Fozmo".to_string()),
                    album: Some("Output probe".to_string()),
                    album_artist: None,
                    album_id: None,
                    art_id: None,
                    duration_secs: Some(f64::from(UPNP_PROBE_DURATION_MS) / 1000.0),
                    ext_hint: None,
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                path,
                tags: TrackTags {
                    title: Some(format!(
                        "Fozmo probe {}kHz/{}",
                        sample_rate / 1000,
                        bit_depth
                    )),
                    artist: Some("Fozmo".to_string()),
                    album: Some("Output probe".to_string()),
                    duration_secs: Some(f64::from(UPNP_PROBE_DURATION_MS) / 1000.0),
                    ..TrackTags::default()
                },
                cover: None,
                byte_len,
                source_rate: sample_rate,
                source_bits: u32::from(bit_depth),
            },
            &probe_target,
        );
        let attempt_index = self.begin_capability_probe_attempt(
            target,
            "pcm",
            &format!("{sample_rate}Hz/{bit_depth}/{format:?}"),
            &asset,
            &probe_target,
        );
        let play_result = self
            .play_locked(zone_id, &probe_target, asset, UPNP_PROBE_ACCEPT_TIMEOUT)
            .await;
        let play_id = self
            .sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .map(|session| session.play_id)
            .unwrap_or(0);
        let acceptance = self
            .probe_acceptance_from_play_result(zone_id, play_id, target, play_result)
            .await;
        let _ = self.stop_transport(zone_id, target).await;
        self.clear_probe_session(zone_id, play_id);
        self.finish_capability_probe_attempt(&target.id, attempt_index, &acceptance);
        if acceptance.accepted {
            Ok(())
        } else {
            Err(acceptance.error.unwrap_or_else(|| {
                format!("UPnP probe {}Hz was not accepted by renderer", sample_rate)
            }))
        }
    }

    pub(super) async fn probe_dsf_rate_locked(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        dsd_rate: u16,
    ) -> Result<(), String> {
        let path = write_probe_dsf_file(dsd_rate)?;
        let byte_len = std::fs::metadata(&path).ok().map(|meta| meta.len());
        let sample_rate = dsd_sample_rate(dsd_rate)
            .ok_or_else(|| format!("unsupported DSD probe rate DSD{dsd_rate}"))?;
        let mut probe_target = target.clone();
        probe_target.max_dsd_rate = Some(dsd_rate);
        probe_target.capability_detection_source = CapabilityDetectionSource::Probing;
        probe_target.capability_detection_status = CapabilityDetectionStatus::Probing;
        probe_target.capability_detection_message = Some(format!("Probing DSD{dsd_rate}"));
        let asset = self.prepare_source(
            UpnpSource::LocalFile {
                source_ref: SourceRef::LocalTrack {
                    track_id: -1,
                    file_name: Some(path.to_string_lossy().to_string()),
                    title: Some(format!("Fozmo probe DSD{dsd_rate}")),
                    artist: Some("Fozmo".to_string()),
                    album: Some("Output probe".to_string()),
                    album_artist: None,
                    album_id: None,
                    art_id: None,
                    duration_secs: Some(f64::from(UPNP_PROBE_DURATION_MS) / 1000.0),
                    ext_hint: Some("dsf".to_string()),
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                path,
                tags: TrackTags {
                    title: Some(format!("Fozmo probe DSD{dsd_rate}")),
                    artist: Some("Fozmo".to_string()),
                    album: Some("Output probe".to_string()),
                    duration_secs: Some(f64::from(UPNP_PROBE_DURATION_MS) / 1000.0),
                    ..TrackTags::default()
                },
                cover: None,
                byte_len,
                source_rate: sample_rate,
                source_bits: 1,
            },
            &probe_target,
        );
        let attempt_index = self.begin_capability_probe_attempt(
            target,
            "dsd",
            &format!("DSD{dsd_rate}"),
            &asset,
            &probe_target,
        );
        let play_result = self
            .play_locked(zone_id, &probe_target, asset, UPNP_PROBE_ACCEPT_TIMEOUT)
            .await;
        let play_id = self
            .sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .map(|session| session.play_id)
            .unwrap_or(0);
        let acceptance = self
            .probe_acceptance_from_play_result(zone_id, play_id, target, play_result)
            .await;
        let _ = self.stop_transport(zone_id, target).await;
        self.clear_probe_session(zone_id, play_id);
        self.finish_capability_probe_attempt(&target.id, attempt_index, &acceptance);
        if dsd_probe_accepted(&acceptance) {
            Ok(())
        } else {
            Err(acceptance.error.unwrap_or_else(|| {
                format!(
                    "UPnP probe DSD{} did not reach PLAYING on renderer",
                    dsd_rate
                )
            }))
        }
    }

    pub(super) async fn probe_acceptance_observation(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
    ) -> UpnpProbeAcceptance {
        let started = Instant::now();
        let mut renderer_get = self.trace_has_renderer_get(zone_id, play_id);
        let mut renderer_head = self.trace_has_renderer_head(zone_id, play_id);
        let mut playing_observed = false;
        let mut terminal_state = None;
        if renderer_get {
            return UpnpProbeAcceptance {
                accepted: true,
                renderer_get,
                renderer_head,
                playing_observed,
                terminal_state,
                evidence: Some("renderer_http_get".to_string()),
                error: None,
            };
        }
        while started.elapsed() < UPNP_PROBE_VERIFY_TIMEOUT {
            renderer_get |= self.trace_has_renderer_get(zone_id, play_id);
            renderer_head |= self.trace_has_renderer_head(zone_id, play_id);
            if renderer_get {
                break;
            }
            if let Ok(Ok(snapshot)) = tokio::time::timeout(
                UPNP_PLAYBACK_REFRESH_TIMEOUT,
                self.transport_snapshot(target),
            )
            .await
                && let Some(state) = snapshot.state.as_deref()
            {
                let label = upnp_state_label(state);
                terminal_state = Some(label.clone());
                match label.as_str() {
                    "Playing" => {
                        playing_observed = true;
                        break;
                    }
                    "Stopped" | "NO_MEDIA_PRESENT" | "No Media" => {
                        return UpnpProbeAcceptance {
                            accepted: false,
                            renderer_get,
                            renderer_head,
                            playing_observed,
                            terminal_state,
                            evidence: None,
                            error: Some(format!(
                                "UPnP probe stopped immediately after startup ({label})"
                            )),
                        };
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(UPNP_PROBE_VERIFY_POLL_INTERVAL).await;
        }
        let accepted = renderer_get || playing_observed;
        UpnpProbeAcceptance {
            accepted,
            renderer_get,
            renderer_head,
            playing_observed,
            terminal_state,
            evidence: if renderer_get {
                Some("renderer_http_get".to_string())
            } else if playing_observed {
                Some("transport_playing".to_string())
            } else if renderer_head {
                Some("renderer_http_head_only".to_string())
            } else {
                None
            },
            error: (!accepted).then(|| {
                "UPnP probe was not fetched by renderer and PLAYING was not observed".to_string()
            }),
        }
    }

    pub(super) async fn probe_acceptance_from_play_result(
        &self,
        zone_id: &str,
        play_id: u64,
        target: &UpnpRendererTarget,
        play_result: Result<(), String>,
    ) -> UpnpProbeAcceptance {
        match play_result {
            Ok(()) => {
                self.probe_acceptance_observation(zone_id, play_id, target)
                    .await
            }
            Err(play_error) => {
                let mut acceptance = self
                    .probe_acceptance_observation(zone_id, play_id, target)
                    .await;
                if acceptance.accepted {
                    acceptance.evidence = acceptance
                        .evidence
                        .map(|evidence| format!("{evidence}_after_play_error"))
                        .or_else(|| Some("accepted_after_play_error".to_string()));
                    acceptance.error = None;
                    acceptance
                } else {
                    acceptance.error = Some(match acceptance.error {
                        Some(observation_error) => {
                            format!("{play_error}; {observation_error}")
                        }
                        None => play_error,
                    });
                    acceptance
                }
            }
        }
    }

    pub(super) fn clear_probe_session(&self, zone_id: &str, play_id: u64) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(zone_id) else {
            return;
        };
        if session.play_id != play_id {
            return;
        }
        let is_probe = session
            .current
            .as_ref()
            .and_then(|asset| asset.source_ref.local_track_id())
            == Some(-1);
        if is_probe {
            *session = UpnpSession::default();
        }
    }

    pub(super) async fn cleanup_probe_after_abort(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
    ) {
        let _ = self.stop_transport(zone_id, target).await;
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(zone_id) else {
            return;
        };
        let is_probe = session
            .current
            .as_ref()
            .and_then(|asset| asset.source_ref.local_track_id())
            == Some(-1);
        if is_probe {
            *session = UpnpSession::default();
        }
    }
}

pub(super) fn dsd_probe_accepted(acceptance: &UpnpProbeAcceptance) -> bool {
    acceptance.playing_observed
}

pub(super) fn infer_capabilities(protocol_info: &[String]) -> UpnpCapabilityInference {
    let mut max_sample_rate = 0;
    let mut max_bit_depth = 0;
    let mut max_dsd_rate = None;
    let mut pcm_containers = Vec::new();
    let mut saw_pcm_format = false;
    let mut saw_exact_pcm_rate = false;
    let mut saw_open_ended_pcm_format = false;
    let mut saw_dsd_format = false;
    let mut saw_exact_dsd_rate = false;

    for raw in protocol_info {
        let Some(parsed) = parse_protocol_info(raw) else {
            continue;
        };
        let content = parsed.content_format.to_ascii_lowercase();
        let params = parsed.additional_info.to_ascii_lowercase();
        let combined = format!("{content};{params}");

        if content_is_pcm_like(&content) {
            saw_pcm_format = true;
            let mut entry_has_exact_rate = false;
            let entry_bit_depth = pcm_bit_depth_from_text(&combined);
            let entry_container = upnp_pcm_container_from_mime(&content);
            for token in capability_tokens(&combined) {
                if let Some(rate) = pcm_rate_from_token(&token) {
                    max_sample_rate = max_sample_rate.max(rate);
                    saw_exact_pcm_rate = true;
                    entry_has_exact_rate = true;
                    if let Some(container) = entry_container
                        && entry_bit_depth > 0
                    {
                        upsert_pcm_container_capability(
                            &mut pcm_containers,
                            container,
                            rate,
                            entry_bit_depth,
                        );
                    }
                }
            }
            if !entry_has_exact_rate && content_is_probeable_pcm_like(&content) {
                saw_open_ended_pcm_format = true;
            }
            max_bit_depth = max_bit_depth.max(entry_bit_depth);
        }

        if content_is_dsd_like(&content) || combined.contains("dsd") {
            saw_dsd_format = true;
            for token in capability_tokens(&combined) {
                if let Some(rate) = dsd_rate_from_token(&token) {
                    max_dsd_rate = Some(max_dsd_rate.unwrap_or(0).max(rate));
                    saw_exact_dsd_rate = true;
                }
            }
        }
    }

    let needs_probe = (!saw_exact_pcm_rate && (saw_pcm_format || protocol_info.is_empty()))
        || saw_open_ended_pcm_format
        || (saw_dsd_format && !saw_exact_dsd_rate);
    let detection_source = if needs_probe {
        CapabilityDetectionSource::Fallback
    } else if saw_exact_pcm_rate || max_dsd_rate.is_some() {
        CapabilityDetectionSource::Advertised
    } else {
        CapabilityDetectionSource::Fallback
    };
    let detection_status = if needs_probe {
        CapabilityDetectionStatus::Unknown
    } else if saw_exact_pcm_rate || max_dsd_rate.is_some() {
        CapabilityDetectionStatus::Complete
    } else {
        CapabilityDetectionStatus::Unknown
    };
    let detection_message = if saw_open_ended_pcm_format {
        Some("UPnP advertised open-ended lossless formats; exact max requires probing".to_string())
    } else if protocol_info.is_empty() {
        Some(
            "Renderer did not return UPnP Sink protocolInfo; using safe defaults until probed"
                .to_string(),
        )
    } else if needs_probe {
        Some("UPnP protocolInfo is incomplete; using safe defaults until probed".to_string())
    } else {
        None
    };

    UpnpCapabilityInference {
        max_sample_rate: if max_sample_rate > 0 {
            max_sample_rate
        } else {
            UPNP_FALLBACK_SAMPLE_RATE
        },
        max_bit_depth: if max_bit_depth > 0 {
            max_bit_depth
        } else {
            UPNP_FALLBACK_BIT_DEPTH
        },
        max_dsd_rate,
        detection_source,
        detection_status,
        detection_message,
        needs_probe,
        pcm_containers,
    }
}

pub(super) struct ParsedProtocolInfo<'a> {
    content_format: &'a str,
    additional_info: &'a str,
}

pub(super) fn parse_protocol_info(value: &str) -> Option<ParsedProtocolInfo<'_>> {
    let mut parts = value.splitn(4, ':');
    let _protocol = parts.next()?.trim();
    let _network = parts.next()?.trim();
    let content_format = parts.next()?.trim();
    let additional_info = parts.next().unwrap_or("").trim();
    (!content_format.is_empty()).then_some(ParsedProtocolInfo {
        content_format,
        additional_info,
    })
}

pub(super) fn content_is_pcm_like(content: &str) -> bool {
    matches!(
        content,
        "audio/flac"
            | "audio/x-flac"
            | "audio/wav"
            | "audio/wave"
            | "audio/x-wav"
            | "audio/l16"
            | "audio/l24"
            | "audio/lpcm"
            | "audio/x-lpcm"
            | "audio/aiff"
            | "audio/x-aiff"
    ) || content.contains("flac")
        || content.contains("lpcm")
        || content.contains("wav")
        || content.contains("l16")
        || content.contains("l24")
}

pub(super) fn content_is_probeable_pcm_like(content: &str) -> bool {
    content.contains("flac")
        || content.contains("wav")
        || content.contains("aiff")
        || content.contains("lpcm")
}

pub(super) fn upnp_pcm_container_from_mime(mime: &str) -> Option<UpnpPcmContainer> {
    let mime = mime.to_ascii_lowercase();
    if mime.contains("flac") {
        Some(UpnpPcmContainer::Flac)
    } else if mime.contains("wav") || mime.contains("wave") {
        Some(UpnpPcmContainer::Wav)
    } else {
        None
    }
}

pub(super) fn upsert_pcm_container_capability(
    capabilities: &mut Vec<UpnpPcmContainerCapability>,
    container: UpnpPcmContainer,
    sample_rate: u32,
    bit_depth: u8,
) {
    if sample_rate == 0 || bit_depth == 0 {
        return;
    }
    if let Some(existing) = capabilities
        .iter_mut()
        .find(|capability| capability.container == container)
    {
        existing.max_sample_rate = existing.max_sample_rate.max(sample_rate);
        existing.max_bit_depth = existing.max_bit_depth.max(bit_depth);
    } else {
        capabilities.push(UpnpPcmContainerCapability {
            container,
            max_sample_rate: sample_rate,
            max_bit_depth: bit_depth,
        });
    }
    capabilities.sort_by_key(|capability| match capability.container {
        UpnpPcmContainer::Flac => 0,
        UpnpPcmContainer::Wav => 1,
    });
}

pub(super) fn content_is_dsd_like(content: &str) -> bool {
    content.contains("dsd")
        || content.contains("dsf")
        || content.contains("dff")
        || content.contains("x-dsd")
}

pub(super) fn capability_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '=')))
        .flat_map(|part| part.split(['_', '-', '=']))
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

pub(super) fn pcm_rate_from_token(token: &str) -> Option<u32> {
    let compact = token.trim();
    if let Some(khz) = compact.strip_suffix("khz") {
        return parse_khz_rate(khz);
    }
    if let Some(rate) = compact.strip_suffix("hz").and_then(parse_hz_rate) {
        return Some(rate);
    }
    if let Some(rate) = parse_hz_rate(compact) {
        return Some(rate);
    }
    parse_khz_rate(compact)
}

pub(super) fn parse_hz_rate(value: &str) -> Option<u32> {
    let parsed = value.parse::<u32>().ok()?;
    match parsed {
        44_100 | 48_000 | 88_200 | 96_000 | 176_400 | 192_000 | 352_800 | 384_000 | 705_600
        | 768_000 => Some(parsed),
        _ => None,
    }
}

pub(super) fn parse_khz_rate(value: &str) -> Option<u32> {
    match value {
        "44" | "44.1" => Some(44_100),
        "48" => Some(48_000),
        "88" | "88.2" => Some(88_200),
        "96" => Some(96_000),
        "176" | "176.4" => Some(176_400),
        "192" => Some(192_000),
        "352" | "352.8" => Some(352_800),
        "384" => Some(384_000),
        "705" | "705.6" => Some(705_600),
        "768" => Some(768_000),
        _ => None,
    }
}

pub(super) fn pcm_bit_depth_from_text(value: &str) -> u8 {
    let lower = value.to_ascii_lowercase();
    if lower.contains("bitspersample=32")
        || lower.contains("bitdepth=32")
        || lower.contains("32bit")
        || lower.contains("s32")
    {
        32
    } else if lower.contains("bitspersample=24")
        || lower.contains("bitdepth=24")
        || lower.contains("24bit")
        || lower.contains("s24")
        || lower.contains("l24")
        || lower.contains("_24")
    {
        24
    } else if lower.contains("bitspersample=16")
        || lower.contains("bitdepth=16")
        || lower.contains("16bit")
        || lower.contains("s16")
        || lower.contains("l16")
    {
        16
    } else {
        0
    }
}

pub(super) fn dsd_rate_from_token(token: &str) -> Option<u16> {
    match token {
        "dsd64" | "dsf64" | "dff64" | "64" | "2822400" | "3072000" => Some(64),
        "dsd128" | "dsf128" | "dff128" | "128" | "5644800" | "6144000" => Some(128),
        "dsd256" | "dsf256" | "dff256" | "256" | "11289600" | "12288000" => Some(256),
        _ => None,
    }
}

pub(super) fn renderer_supports_flac(target: &UpnpRendererTarget) -> bool {
    target
        .protocol_info
        .iter()
        .any(|value| protocol_info_mime(value).is_some_and(|mime| mime.contains("flac")))
}

pub(super) fn pcm_probe_formats_for_candidate(
    target: &UpnpRendererTarget,
    candidate: PcmProbeCandidate,
) -> Vec<PcmProbeFormat> {
    let mut formats = Vec::new();
    if candidate.bit_depth <= 24 && renderer_supports_flac(target) {
        formats.push(PcmProbeFormat::Flac);
    }
    formats.push(PcmProbeFormat::Wav);
    formats
}

pub(super) fn pcm_probe_candidates(target: &UpnpRendererTarget) -> Vec<PcmProbeCandidate> {
    let mut candidates = Vec::new();
    let advertised_max = advertised_max_pcm_rate(target);
    if let Some(max_rate) = advertised_max.filter(|rate| *rate > 192_000) {
        candidates.extend(
            UPNP_PCM_EXTENDED_PROBE_RATES
                .into_iter()
                .filter(|rate| *rate <= max_rate)
                .map(|sample_rate| PcmProbeCandidate {
                    sample_rate,
                    bit_depth: 32,
                }),
        );
        candidates.extend(
            UPNP_PCM_EXTENDED_PROBE_RATES
                .into_iter()
                .filter(|rate| *rate <= max_rate)
                .map(|sample_rate| PcmProbeCandidate {
                    sample_rate,
                    bit_depth: 24,
                }),
        );
    }
    candidates.extend(UPNP_PCM_PROBE_LADDER);
    candidates.sort_by(|a, b| {
        b.sample_rate
            .cmp(&a.sample_rate)
            .then_with(|| b.bit_depth.cmp(&a.bit_depth))
    });
    candidates.dedup();
    if advertised_max.is_none_or(|rate| rate <= 192_000) {
        candidates.retain(|candidate| candidate.sample_rate <= 192_000);
    }
    candidates
}

pub(super) fn advertised_max_pcm_rate(target: &UpnpRendererTarget) -> Option<u32> {
    target
        .protocol_info
        .iter()
        .filter_map(|raw| {
            let parsed = parse_protocol_info(raw)?;
            let combined = format!(
                "{};{}",
                parsed.content_format.to_ascii_lowercase(),
                parsed.additional_info.to_ascii_lowercase()
            );
            capability_tokens(&combined)
                .into_iter()
                .filter_map(|token| pcm_rate_from_token(&token))
                .max()
        })
        .max()
}

pub(super) fn renderer_supports_dsf(target: &UpnpRendererTarget) -> bool {
    target.protocol_info.iter().any(|value| {
        protocol_info_mime(value).is_some_and(|mime| {
            let mime = mime.to_ascii_lowercase();
            mime.contains("dsf") || mime.contains("dsd")
        })
    })
}

pub(super) fn should_probe_dsd(
    target: &UpnpRendererTarget,
    result: &UpnpCapabilityProbeResult,
) -> bool {
    renderer_supports_dsf(target)
        || (result.detection_source == CapabilityDetectionSource::Probed
            && result.max_sample_rate >= 88_200
            && target.protocol_info.iter().any(|value| {
                protocol_info_mime(value)
                    .is_some_and(|mime| content_is_probeable_pcm_like(&mime.to_ascii_lowercase()))
            }))
}

pub(super) fn is_sonos_renderer_target(target: &UpnpRendererTarget) -> bool {
    target
        .manufacturer
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("sonos"))
        || target.name.to_ascii_lowercase().contains("sonos")
        || target
            .model
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("sonos"))
}

pub(super) fn capability_probe_cache_key(target: &UpnpRendererTarget) -> String {
    let mut hasher = Sha256::new();
    for value in &target.protocol_info {
        hasher.update(value.trim().as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    format!("{}:{}", target.id, URL_SAFE_NO_PAD.encode(&digest[..12]))
}

pub(super) fn merge_capability_results(
    mut existing: UpnpCapabilityProbeResult,
    incoming: UpnpCapabilityProbeResult,
) -> UpnpCapabilityProbeResult {
    existing.max_sample_rate = existing.max_sample_rate.max(incoming.max_sample_rate);
    existing.max_bit_depth = existing.max_bit_depth.max(incoming.max_bit_depth);
    existing.max_dsd_rate = match (existing.max_dsd_rate, incoming.max_dsd_rate) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    for capability in incoming.pcm_containers {
        upsert_pcm_container_capability(
            &mut existing.pcm_containers,
            capability.container,
            capability.max_sample_rate,
            capability.max_bit_depth,
        );
    }
    if capability_source_confidence(incoming.detection_source)
        >= capability_source_confidence(existing.detection_source)
    {
        existing.detection_source = incoming.detection_source;
    }
    if capability_status_confidence(incoming.detection_status)
        >= capability_status_confidence(existing.detection_status)
    {
        existing.detection_status = incoming.detection_status;
        existing.detection_message = incoming.detection_message;
        existing.basis = incoming.basis;
    }
    existing
}

pub(super) fn capability_source_confidence(source: CapabilityDetectionSource) -> u8 {
    match source {
        CapabilityDetectionSource::Probed => 3,
        CapabilityDetectionSource::Advertised => 2,
        CapabilityDetectionSource::Probing => 1,
        CapabilityDetectionSource::Fallback => 0,
    }
}

pub(super) fn capability_status_confidence(status: CapabilityDetectionStatus) -> u8 {
    match status {
        CapabilityDetectionStatus::Complete => 4,
        CapabilityDetectionStatus::Unknown => 3,
        CapabilityDetectionStatus::Failed => 2,
        CapabilityDetectionStatus::Deferred => 1,
        CapabilityDetectionStatus::Probing => 0,
    }
}

pub(super) fn apply_probe_result_to_target(
    target: &mut UpnpRendererTarget,
    result: UpnpCapabilityProbeResult,
) {
    target.max_sample_rate = result.max_sample_rate;
    target.max_bit_depth = result.max_bit_depth;
    target.max_dsd_rate = result.max_dsd_rate;
    target.capability_detection_source = result.detection_source;
    target.capability_detection_status = result.detection_status;
    target.capability_detection_message = result.detection_message;
    target.pcm_containers = result.pcm_containers;
}

pub(super) fn is_probe_asset(asset: &UpnpAsset) -> bool {
    asset.source_ref.local_track_id() == Some(-1)
}

pub(super) fn dsd_rate_for_sample_rate(sample_rate: u32) -> Option<u16> {
    match sample_rate {
        2_822_400 | 3_072_000 => Some(64),
        5_644_800 | 6_144_000 => Some(128),
        11_289_600 | 12_288_000 => Some(256),
        _ => None,
    }
}

pub(super) fn write_probe_pcm_file(
    sample_rate: u32,
    bit_depth: u8,
    format: PcmProbeFormat,
) -> Result<PathBuf, String> {
    let dir = probe_cache_dir()?;
    match format {
        PcmProbeFormat::Flac => {
            let path = dir.join(format!("silence-{sample_rate}-{bit_depth}.flac"));
            write_probe_flac_file(&path, sample_rate, bit_depth)?;
            Ok(path)
        }
        PcmProbeFormat::Wav => {
            let path = dir.join(format!("silence-{sample_rate}-{bit_depth}.wav"));
            write_probe_wav_file(&path, sample_rate, bit_depth)?;
            Ok(path)
        }
    }
}

pub(super) fn write_probe_dsf_file(dsd_rate: u16) -> Result<PathBuf, String> {
    let dir = probe_cache_dir()?;
    let path = dir.join(format!("silence-dsd{dsd_rate}.dsf"));
    write_probe_dsf_file_at(&path, dsd_rate)?;
    Ok(path)
}

pub(super) fn write_probe_dsf_file_at(path: &Path, dsd_rate: u16) -> Result<(), String> {
    write_probe_file_once(path, dsf_probe_bytes(dsd_rate)?)
}

pub(super) fn dsf_probe_bytes(dsd_rate: u16) -> Result<Vec<u8>, String> {
    let sample_rate = dsd_sample_rate(dsd_rate)
        .ok_or_else(|| format!("unsupported DSD probe rate DSD{dsd_rate}"))?;
    let channels = 2_u32;
    let sample_count = ((u64::from(sample_rate) * u64::from(UPNP_PROBE_DURATION_MS)) / 1000).max(1);
    let bytes_per_channel = sample_count.div_ceil(8) as usize;
    let blocks = bytes_per_channel
        .div_ceil(DSF_BLOCK_SIZE_PER_CHANNEL)
        .max(1);
    let payload_len = blocks * DSF_BLOCK_SIZE_PER_CHANNEL * channels as usize;
    let data_chunk_size = 12_u64 + payload_len as u64;
    let file_size = 28_u64 + 52 + data_chunk_size;
    let mut bytes = Vec::with_capacity(file_size as usize);

    bytes.extend_from_slice(b"DSD ");
    bytes.extend_from_slice(&28_u64.to_le_bytes());
    bytes.extend_from_slice(&file_size.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&52_u64.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&sample_count.to_le_bytes());
    bytes.extend_from_slice(&(DSF_BLOCK_SIZE_PER_CHANNEL as u32).to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_chunk_size.to_le_bytes());
    bytes.resize(file_size as usize, 0x69);
    Ok(bytes)
}

pub(super) fn dsd_sample_rate(dsd_rate: u16) -> Option<u32> {
    match dsd_rate {
        64 => Some(2_822_400),
        128 => Some(5_644_800),
        256 => Some(11_289_600),
        _ => None,
    }
}

pub(super) fn write_probe_flac_file(
    path: &Path,
    sample_rate: u32,
    bit_depth: u8,
) -> Result<(), String> {
    if bit_depth > 24 {
        return Err(format!(
            "FLAC UPnP probe does not support {bit_depth}-bit samples"
        ));
    }
    let frames = probe_frame_count(sample_rate);
    let samples = vec![0_i32; frames * 2];
    let source = MemSource::from_samples(&samples, 2, bit_depth as usize, sample_rate as usize);
    let config = config::Encoder::default()
        .into_verified()
        .map_err(|e| format!("verify UPnP probe FLAC config: {e:?}"))?;
    let stream = flacenc::encode_with_fixed_block_size(&config, source, 4096)
        .map_err(|e| format!("encode UPnP probe FLAC: {e}"))?;
    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("write UPnP probe FLAC bitstream: {e}"))?;
    write_probe_file_once(path, sink.into_inner())
}

pub(super) fn write_probe_wav_file(
    path: &Path,
    sample_rate: u32,
    bit_depth: u8,
) -> Result<(), String> {
    let channels = 2_u16;
    let bits_per_sample = u16::from(bit_depth);
    if !matches!(bits_per_sample, 16 | 24 | 32) {
        return Err(format!("unsupported WAV probe bit depth {bit_depth}"));
    }
    let frames = probe_frame_count(sample_rate);
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = frames as u32 * u32::from(block_align);
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);
    write_probe_file_once(path, bytes)
}

pub(super) fn probe_frame_count(sample_rate: u32) -> usize {
    ((u64::from(sample_rate) * u64::from(UPNP_PROBE_DURATION_MS)) / 1000)
        .max(1)
        .min(usize::MAX as u64) as usize
}

pub(super) fn write_probe_file_once(path: &Path, bytes: Vec<u8>) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("UPnP probe path is missing a parent directory".to_string());
    };
    let parent_meta =
        std::fs::symlink_metadata(parent).map_err(|e| format!("inspect UPnP probe dir: {e}"))?;
    if !parent_meta.is_dir() {
        return Err("UPnP probe parent is not a directory".to_string());
    }
    match probe_existing_file_matches(path, &bytes) {
        Ok(()) => return Ok(()),
        Err(ProbeFileState::Missing) => {}
        Err(ProbeFileState::Unsafe(error)) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            use std::io::Write;
            if let Err(e) = file.write_all(&bytes) {
                let _ = std::fs::remove_file(path);
                return Err(format!("write UPnP probe file: {e}"));
            }
            file.sync_all()
                .map_err(|e| format!("sync UPnP probe file: {e}"))
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            match probe_existing_file_matches(path, &bytes) {
                Ok(()) => Ok(()),
                Err(ProbeFileState::Missing) => {
                    Err("UPnP probe file disappeared during creation".to_string())
                }
                Err(ProbeFileState::Unsafe(error)) => Err(error),
            }
        }
        Err(e) => Err(format!("create UPnP probe file: {e}")),
    }
}

pub fn probe_path_is_streamable(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    meta.is_file() && probe_cache_dir().is_ok_and(|dir| path.parent() == Some(dir.as_path()))
}

fn probe_cache_dir() -> Result<PathBuf, String> {
    static PROBE_CACHE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    if let Some(dir) = PROBE_CACHE_DIR.get() {
        return Ok(dir.clone());
    }
    let dir = create_private_probe_cache_dir()?;
    let _ = PROBE_CACHE_DIR.set(dir.clone());
    Ok(PROBE_CACHE_DIR.get().cloned().unwrap_or(dir))
}

fn create_private_probe_cache_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    for _ in 0..16 {
        let mut token = [0_u8; 12];
        OsRng.fill_bytes(&mut token);
        let dir = base.join(format!(
            "fozmo-upnp-probes-{}",
            URL_SAFE_NO_PAD.encode(token)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                        .map_err(|e| format!("secure UPnP probe dir: {e}"))?;
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create UPnP probe dir: {e}")),
        }
    }
    Err("create UPnP probe dir: exhausted unique directory names".to_string())
}

enum ProbeFileState {
    Missing,
    Unsafe(String),
}

fn probe_existing_file_matches(path: &Path, expected: &[u8]) -> Result<(), ProbeFileState> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(ProbeFileState::Missing),
        Err(e) => {
            return Err(ProbeFileState::Unsafe(format!(
                "inspect UPnP probe file: {e}"
            )));
        }
    };
    if !meta.is_file() {
        return Err(ProbeFileState::Unsafe(
            "existing UPnP probe path is not a regular file".to_string(),
        ));
    }
    if meta.len() != expected.len() as u64 {
        return Err(ProbeFileState::Unsafe(
            "existing UPnP probe file did not match expected content".to_string(),
        ));
    }
    let existing = std::fs::read(path).map_err(|e| {
        ProbeFileState::Unsafe(format!("read existing UPnP probe file for validation: {e}"))
    })?;
    if existing == expected {
        Ok(())
    } else {
        Err(ProbeFileState::Unsafe(
            "existing UPnP probe file did not match expected content".to_string(),
        ))
    }
}
