use crate::app::state::AppState;
use crate::audio::player::TrackTags;
use crate::audio::upnp::{self, UpnpAsset, UpnpRendererTarget, UpnpSource};
use crate::playback::commands::is_current_playback_sequence;
use crate::playback::config::playback_config_for_zone;
use crate::playback::error::PlaybackError;
use crate::playback::sequencer::PlaybackRequestSequence;
use crate::playback::upnp_dsp::{
    UpnpDspDecision, UpnpDspStreamingPolicy, passthrough_render_signature,
    rendered_upnp_source_if_needed,
};
use crate::protocol::{CapabilityDetectionSource, PlaybackConfig, SourceRef};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

const UPNP_RECONFIGURE_DEBOUNCE: Duration = Duration::from_millis(400);

#[allow(clippy::too_many_arguments)]
pub(crate) async fn play_upnp_source_for_zone(
    state: AppState,
    zone_id: &str,
    profile_id: String,
    expected_playback_sequence: Option<PlaybackRequestSequence>,
    source_ref: SourceRef,
    queue_sources: Vec<SourceRef>,
    radio_auto: bool,
    playback_config: PlaybackConfig,
) -> Result<(), PlaybackError> {
    let target = upnp_target_for_zone(&state, zone_id)?;
    if expected_playback_sequence
        .as_ref()
        .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
    {
        state
            .upnp()
            .mark_stale_command_discard(zone_id, "before_upnp_prepare");
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let command_generation = state.upnp().begin_prepare_command(zone_id);
    let asset = prepare_upnp_asset(&state, zone_id, &source_ref, &target, &playback_config).await?;
    if !state
        .upnp()
        .command_generation_matches(zone_id, command_generation)
    {
        state
            .upnp()
            .mark_stale_command_discard(zone_id, "after_upnp_prepare_generation");
        return Err(PlaybackError::conflict("Playback changed"));
    }
    if expected_playback_sequence
        .as_ref()
        .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
    {
        state
            .upnp()
            .mark_stale_command_discard(zone_id, "after_upnp_prepare");
        return Err(PlaybackError::conflict("Playback changed"));
    }
    state
        .upnp()
        .play_with_expected_generation(zone_id, &target, asset, Some(command_generation))
        .await
        .map_err(PlaybackError::integration)?;
    if expected_playback_sequence
        .as_ref()
        .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
    {
        state
            .upnp()
            .mark_stale_command_discard(zone_id, "after_upnp_play");
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let _ = state.library().set_zone_queue(zone_id, &queue_sources);
    state.listening().start_with_radio(
        state.library(),
        zone_id.to_string(),
        state.zones().zone_name(zone_id),
        profile_id,
        source_ref,
        queue_sources,
        radio_auto,
    );
    Ok(())
}

pub(crate) async fn prewarm_upnp_source_for_zone(
    state: AppState,
    zone_id: &str,
    expected_playback_sequence: Option<PlaybackRequestSequence>,
    expected_active_source: Option<SourceRef>,
    source_ref: SourceRef,
    require_queued_next: bool,
) -> Result<(), PlaybackError> {
    let target = upnp_target_for_zone(&state, zone_id)?;
    let queued_next_matches = upnp_queued_next_matches(&state, zone_id, &source_ref)?;
    if require_queued_next && !queued_next_matches {
        return Err(PlaybackError::conflict("Track is no longer next in queue"));
    }
    if let Some(expected) = expected_active_source.as_ref()
        && state
            .listening()
            .active_source(zone_id)
            .as_ref()
            .map(SourceRef::key)
            != Some(expected.key())
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .unwrap_or_else(|| state.zones().active_player());
    let playback_config = playback_config_for_zone(&state, zone_id, &player);
    let asset = prepare_upnp_asset(&state, zone_id, &source_ref, &target, &playback_config).await?;
    let queued_next_matches = upnp_queued_next_matches(&state, zone_id, &source_ref)?;
    if require_queued_next && !queued_next_matches {
        return Err(PlaybackError::conflict("Track is no longer next in queue"));
    }
    if expected_playback_sequence
        .as_ref()
        .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    if let Some(expected) = expected_active_source.as_ref()
        && state
            .listening()
            .active_source(zone_id)
            .as_ref()
            .map(SourceRef::key)
            != Some(expected.key())
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    debug!(
        event = "stream_resolution",
        service = "upnp",
        zone_id,
        asset_id = %asset.id,
        mime_type = %asset.mime_type,
        "UPnP source prewarmed"
    );
    if !queued_next_matches {
        debug!(
            event = "upnp_next_asset_not_armed",
            zone_id,
            asset_id = %asset.id,
            source_key = %source_ref.key(),
            "UPnP source prewarmed but not armed because it is not the first queued item"
        );
        return Ok(());
    }
    let expected_current_source_key = expected_active_source.as_ref().map(SourceRef::key);
    match state
        .upnp()
        .arm_next_transport_uri(
            zone_id,
            &target,
            &asset,
            expected_current_source_key.as_deref(),
        )
        .await
    {
        Ok(()) => {
            debug!(
                event = "upnp_next_asset_armed",
                zone_id,
                asset_id = %asset.id,
                next_source_key = %source_ref.key(),
                "UPnP renderer accepted next-track handoff"
            );
        }
        Err(error) if error == "Playback changed" => {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        Err(error) => {
            warn!(
                event = "upnp_next_asset_arm_failed",
                zone_id,
                asset_id = %asset.id,
                next_source_key = %source_ref.key(),
                error = %error,
                "UPnP renderer did not accept next-track handoff; falling back to stopped-state auto-advance"
            );
            state.upnp().mark_notice(
                zone_id,
                format!(
                    "UPnP renderer did not accept next-track handoff; fallback auto-advance remains active: {error}"
                ),
            );
        }
    }
    Ok(())
}

fn upnp_queued_next_matches(
    state: &AppState,
    zone_id: &str,
    source_ref: &SourceRef,
) -> Result<bool, PlaybackError> {
    let queued = state
        .library()
        .zone_queue(zone_id)
        .map_err(PlaybackError::library)?;
    Ok(queued
        .first()
        .is_some_and(|entry| entry.source.key() == source_ref.key()))
}

pub(crate) fn enqueue_upnp_config_reapply_for_zone(state: AppState, zone_id: &str) {
    if state.zones().zone_protocol(zone_id) != Some(crate::protocol::SinkProtocol::UpnpAvRenderer) {
        return;
    }
    let generation = state.upnp().begin_reconfigure(zone_id);
    let zone_id = zone_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(UPNP_RECONFIGURE_DEBOUNCE).await;
        if !state
            .upnp()
            .reconfigure_generation_matches(&zone_id, generation)
        {
            return;
        }
        if let Err(error) =
            apply_pending_upnp_config_reapply(state.clone(), &zone_id, generation).await
        {
            state
                .upnp()
                .finish_reconfigure(&zone_id, generation, "failed", None, None, None, None);
            state
                .upnp()
                .mark_notice(&zone_id, format!("UPnP settings apply failed: {error:?}"));
        }
    });
}

pub(crate) async fn seek_upnp_with_dsp_fallback(
    state: &AppState,
    zone_id: &str,
    target: &UpnpRendererTarget,
    seconds: f64,
) -> Result<(), PlaybackError> {
    let outcome = state
        .upnp()
        .seek(zone_id, target, seconds)
        .await
        .map_err(PlaybackError::integration)?;
    if !outcome.needs_completed_render_fallback {
        return Ok(());
    }
    let Some(snapshot) = state.upnp().snapshot(zone_id) else {
        return Ok(());
    };
    if snapshot.current_render_or_stream_plan.as_deref() != Some("progressive_wav_stream") {
        return Ok(());
    }
    let Some(source_ref) = snapshot.current_source else {
        return Ok(());
    };
    warn!(
        event = "upnp_dsp_seek_completed_render_fallback",
        zone_id,
        seconds,
        verification = ?outcome.verification,
        "UPnP progressive DSP seek was not confirmed; switching to completed render"
    );
    state.upnp().mark_notice(
        zone_id,
        "UPnP renderer did not confirm progressive DSP seek; switching to completed render"
            .to_string(),
    );
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .unwrap_or_else(|| state.zones().active_player());
    let playback_config = playback_config_for_zone(state, zone_id, &player);
    let asset = prepare_upnp_asset_with_streaming_policy(
        state,
        zone_id,
        &source_ref,
        target,
        &playback_config,
        UpnpDspStreamingPolicy::ForceCompletedRender,
    )
    .await?;
    state
        .upnp()
        .play(zone_id, target, asset)
        .await
        .map_err(PlaybackError::integration)?;
    let retry = state
        .upnp()
        .seek(zone_id, target, seconds)
        .await
        .map_err(PlaybackError::integration)?;
    if !retry.confirmed {
        warn!(
            event = "upnp_dsp_seek_completed_render_unconfirmed",
            zone_id,
            seconds,
            verification = ?retry.verification,
            "UPnP completed-render seek was accepted but not confirmed"
        );
    }
    Ok(())
}

async fn apply_pending_upnp_config_reapply(
    state: AppState,
    zone_id: &str,
    generation: u64,
) -> Result<(), PlaybackError> {
    let Some(snapshot) = state.upnp().snapshot(zone_id) else {
        state
            .upnp()
            .finish_reconfigure(zone_id, generation, "saved", None, None, None, None);
        return Ok(());
    };
    let Some(source_ref) = snapshot.current_source.clone() else {
        state
            .upnp()
            .finish_reconfigure(zone_id, generation, "saved", None, None, None, None);
        return Ok(());
    };
    if snapshot.state == "Stopped" {
        state.upnp().finish_reconfigure(
            zone_id,
            generation,
            "saved",
            snapshot.configured_render_signature,
            snapshot.last_render_ms,
            snapshot.last_prepare_ms,
            snapshot.last_cache_hit,
        );
        return Ok(());
    }

    let target = upnp_target_for_zone(&state, zone_id)?;
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .unwrap_or_else(|| state.zones().active_player());
    let playback_config = playback_config_for_zone(&state, zone_id, &player);
    let command_generation = state.upnp().current_command_generation(zone_id);
    state
        .upnp()
        .mark_reconfigure_status(zone_id, generation, "rendering");
    let asset = prepare_upnp_asset(&state, zone_id, &source_ref, &target, &playback_config).await?;
    let configured_signature = asset.configured_render_signature.clone();
    if !state
        .upnp()
        .reconfigure_generation_matches(zone_id, generation)
    {
        return Ok(());
    }
    let Some(current) = state.upnp().snapshot(zone_id) else {
        state.upnp().finish_reconfigure(
            zone_id,
            generation,
            "saved",
            configured_signature,
            asset.render_ms,
            asset.prepare_ms,
            asset.cache_hit,
        );
        return Ok(());
    };
    if !state
        .upnp()
        .command_generation_matches(zone_id, command_generation)
    {
        state.upnp().finish_reconfigure(
            zone_id,
            generation,
            "stale",
            configured_signature,
            asset.render_ms,
            asset.prepare_ms,
            asset.cache_hit,
        );
        return Ok(());
    }
    if current.current_source.as_ref().map(SourceRef::key) != Some(source_ref.key()) {
        state.upnp().finish_reconfigure(
            zone_id,
            generation,
            "stale",
            configured_signature,
            asset.render_ms,
            asset.prepare_ms,
            asset.cache_hit,
        );
        return Ok(());
    }
    if current.active_render_signature == asset.render_signature {
        state.upnp().finish_reconfigure(
            zone_id,
            generation,
            "applied",
            configured_signature,
            asset.render_ms,
            asset.prepare_ms,
            asset.cache_hit,
        );
        return Ok(());
    }

    let resume_position = current.position_secs;
    state
        .upnp()
        .mark_reconfigure_status(zone_id, generation, "switching");
    if let Err(error) = state
        .upnp()
        .play_with_expected_generation(zone_id, &target, asset, Some(command_generation))
        .await
    {
        if error == "Playback changed" {
            state
                .upnp()
                .finish_reconfigure(zone_id, generation, "stale", None, None, None, None);
            return Ok(());
        }
        return Err(PlaybackError::integration(error));
    }
    if resume_position.is_finite() && resume_position > 1.0 {
        let _ = state.upnp().seek(zone_id, &target, resume_position).await;
    }
    state
        .upnp()
        .finish_reconfigure(zone_id, generation, "applied", None, None, None, None);
    Ok(())
}

pub(crate) fn upnp_target_for_zone(
    state: &AppState,
    zone_id: &str,
) -> Result<UpnpRendererTarget, PlaybackError> {
    let stored_target = state
        .zones()
        .zone_bound_device_name(zone_id)
        .as_deref()
        .and_then(upnp::parse_target_device_name)
        .ok_or_else(|| PlaybackError::not_found("UPnP renderer zone not available"))?;
    let target = fresh_upnp_target_for_playback(state, zone_id, stored_target);
    Ok(apply_saved_upnp_capabilities(state, zone_id, target))
}

fn fresh_upnp_target_for_playback(
    state: &AppState,
    zone_id: &str,
    stored_target: UpnpRendererTarget,
) -> UpnpRendererTarget {
    for renderer in state.upnp().renderers() {
        if !renderer.online || renderer.target.id != stored_target.id {
            continue;
        }
        match upnp::classify_upnp_target_refresh(&stored_target, &renderer.target) {
            upnp::UpnpTargetRefreshKind::SameOrigin => {
                return merge_upnp_playback_target(stored_target, renderer.target, false);
            }
            upnp::UpnpTargetRefreshKind::VerifiedEndpointMove => {
                return merge_upnp_playback_target(stored_target, renderer.target, true);
            }
            upnp::UpnpTargetRefreshKind::UnverifiedEndpointMove => {
                warn!(
                    event = "upnp_renderer_endpoint_move_unverified",
                    zone_id,
                    renderer_id = %stored_target.id,
                    stored_origin = %upnp::upnp_target_origin_label(&stored_target),
                    discovered_origin = %upnp::upnp_target_origin_label(&renderer.target),
                    "Ignoring UPnP renderer endpoint move that could not be verified"
                );
                state.upnp().mark_notice(
                    zone_id,
                    format!(
                        "UPnP renderer endpoint moved from {} to {} but could not be verified",
                        upnp::upnp_target_origin_label(&stored_target),
                        upnp::upnp_target_origin_label(&renderer.target)
                    ),
                );
            }
            upnp::UpnpTargetRefreshKind::IdentityCollision => {
                warn!(
                    event = "upnp_renderer_identity_collision",
                    zone_id,
                    renderer_id = %stored_target.id,
                    stored_origin = %upnp::upnp_target_origin_label(&stored_target),
                    discovered_origin = %upnp::upnp_target_origin_label(&renderer.target),
                    "Ignoring UPnP renderer discovery with matching id but different origin"
                );
                state.upnp().mark_notice(
                    zone_id,
                    "UPnP renderer identity changed; re-pair this zone before playback".to_string(),
                );
            }
        }
    }
    stored_target
}

fn merge_upnp_playback_target(
    mut stored_target: UpnpRendererTarget,
    live_target: UpnpRendererTarget,
    use_live_endpoint: bool,
) -> UpnpRendererTarget {
    let keep_stored_capabilities =
        should_keep_stored_upnp_capabilities(&stored_target, &live_target);
    if use_live_endpoint {
        stored_target.host = live_target.host.clone();
        stored_target.port = live_target.port;
        stored_target.av_transport_control_url = live_target.av_transport_control_url.clone();
        stored_target.rendering_control_url = live_target.rendering_control_url.clone();
        stored_target.connection_manager_url = live_target.connection_manager_url.clone();
    }
    stored_target.name = live_target.name;
    stored_target.model = live_target.model;
    stored_target.manufacturer = live_target.manufacturer;
    if !keep_stored_capabilities {
        stored_target.max_sample_rate = live_target.max_sample_rate;
        stored_target.max_bit_depth = live_target.max_bit_depth;
        stored_target.max_dsd_rate = live_target.max_dsd_rate;
        stored_target.capability_detection_source = live_target.capability_detection_source;
        stored_target.capability_detection_status = live_target.capability_detection_status;
        stored_target.capability_detection_message = live_target.capability_detection_message;
        stored_target.pcm_containers = live_target.pcm_containers;
    }
    if !live_target.protocol_info.is_empty() {
        stored_target.protocol_info = live_target.protocol_info;
    }
    stored_target
}

fn apply_saved_upnp_capabilities(
    state: &AppState,
    zone_id: &str,
    mut target: UpnpRendererTarget,
) -> UpnpRendererTarget {
    let Ok(settings) = state.library().zone_settings(zone_id) else {
        return target;
    };
    let Some(capabilities) = settings.upnp_capabilities else {
        return target;
    };
    target.max_sample_rate = capabilities.max_sample_rate;
    target.max_bit_depth = capabilities.max_bit_depth;
    target.max_dsd_rate = capabilities.max_dsd_rate;
    target.capability_detection_source = CapabilityDetectionSource::Probed;
    target.capability_detection_status = crate::protocol::CapabilityDetectionStatus::Complete;
    target.capability_detection_message = Some("Saved UPnP capability override".to_string());
    target.pcm_containers = capabilities.pcm_containers;
    target
}

fn should_keep_stored_upnp_capabilities(
    stored_target: &UpnpRendererTarget,
    live_target: &UpnpRendererTarget,
) -> bool {
    upnp_capability_confidence(stored_target.capability_detection_source)
        > upnp_capability_confidence(live_target.capability_detection_source)
        && upnp_capability_extent(stored_target) >= upnp_capability_extent(live_target)
}

fn upnp_capability_confidence(source: CapabilityDetectionSource) -> u8 {
    match source {
        CapabilityDetectionSource::Probed => 3,
        CapabilityDetectionSource::Advertised => 2,
        CapabilityDetectionSource::Probing => 1,
        CapabilityDetectionSource::Fallback => 0,
    }
}

fn upnp_capability_extent(target: &UpnpRendererTarget) -> u64 {
    u64::from(target.max_sample_rate) * u64::from(target.max_bit_depth.max(1))
        + u64::from(target.max_dsd_rate.unwrap_or(0)) * 1_000_000
}

async fn prepare_upnp_asset(
    state: &AppState,
    zone_id: &str,
    source_ref: &SourceRef,
    target: &UpnpRendererTarget,
    playback_config: &PlaybackConfig,
) -> Result<UpnpAsset, PlaybackError> {
    let streaming_policy = upnp_dsp_streaming_policy_for_target(target);
    prepare_upnp_asset_with_streaming_policy(
        state,
        zone_id,
        source_ref,
        target,
        playback_config,
        streaming_policy,
    )
    .await
}

fn upnp_dsp_streaming_policy_for_target(target: &UpnpRendererTarget) -> UpnpDspStreamingPolicy {
    let kef = target
        .manufacturer
        .as_deref()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("KEF"))
        || target.name.trim().to_ascii_lowercase().starts_with("kef ");
    if kef {
        // KEF renderers reject the generated progressive WAV path. Render to
        // their compatible file-backed PCM container before handing off the
        // URI so the renderer receives a normal seekable asset.
        UpnpDspStreamingPolicy::ForceCompletedRender
    } else {
        UpnpDspStreamingPolicy::Auto
    }
}

async fn prepare_upnp_asset_with_streaming_policy(
    state: &AppState,
    zone_id: &str,
    source_ref: &SourceRef,
    target: &UpnpRendererTarget,
    playback_config: &PlaybackConfig,
    streaming_policy: UpnpDspStreamingPolicy,
) -> Result<UpnpAsset, PlaybackError> {
    let prepare_started = Instant::now();
    debug!(
        event = "upnp_prepare_start",
        zone_id = %zone_id,
        renderer = %target.name,
        "Preparing UPnP asset"
    );
    let mut decision = rendered_upnp_source_if_needed(
        state,
        zone_id,
        source_ref,
        target,
        playback_config,
        streaming_policy,
    )
    .await
    .map_err(PlaybackError::integration)?;
    debug!(
        event = "upnp_prepare_dsp_decision",
        zone_id = %zone_id,
        render_ms = ?decision.render_ms,
        cache_hit = ?decision.cache_hit,
        source_rate = decision.source_rate,
        source_bits = decision.source_bits,
        output_rate = decision.output_rate,
        output_bits = decision.output_bits,
        active_output_mode = %decision.active_output_mode,
        "Resolved UPnP DSP decision"
    );
    let source = if let Some(rendered) = decision.rendered.take() {
        rendered
    } else {
        upnp_source_from_ref(state, zone_id, source_ref, target).await?
    };
    let render_signature = if decision.render_ms.is_some() {
        decision.render_signature.clone()
    } else {
        source_render_signature(&source, target, playback_config)
    };
    let mut asset = state.upnp().prepare_source(source, target);
    apply_rendered_signal_path_to_asset(&mut asset, &decision);
    asset.active_output_mode = Some(decision.active_output_mode.clone());
    state.upnp().update_cached_asset_signal_path(&asset);
    asset.render_signature = Some(render_signature.clone());
    asset.configured_render_signature = Some(render_signature);
    asset.render_ms = decision.render_ms;
    asset.prepare_ms = Some(elapsed_ms(prepare_started));
    asset.cache_hit = decision.cache_hit;
    asset.render_or_stream_plan = decision.render_or_stream_plan.clone();
    asset.cache_lookup_ms = decision.cache_lookup_ms;
    asset.cache_wait_ms = decision.cache_wait_ms;
    debug!(
        event = "upnp_prepare_finish",
        zone_id = %zone_id,
        asset = %asset.id,
        prepare_ms = ?asset.prepare_ms,
        render_ms = ?asset.render_ms,
        cache_hit = ?asset.cache_hit,
        render_or_stream_plan = ?asset.render_or_stream_plan,
        cache_lookup_ms = ?asset.cache_lookup_ms,
        cache_wait_ms = ?asset.cache_wait_ms,
        "Prepared UPnP asset"
    );
    Ok(asset)
}

fn apply_rendered_signal_path_to_asset(asset: &mut UpnpAsset, decision: &UpnpDspDecision) {
    if decision.render_ms.is_none() {
        return;
    }
    asset.source_rate = decision.source_rate;
    asset.source_bits = decision.source_bits;
    asset.target_rate = decision.output_rate;
    asset.target_bits = decision.output_bits;
    asset.active_output_mode = Some(decision.active_output_mode.clone());
}

fn source_render_signature(
    source: &UpnpSource,
    target: &UpnpRendererTarget,
    playback_config: &PlaybackConfig,
) -> String {
    match source {
        UpnpSource::LocalFile {
            source_ref,
            source_rate,
            source_bits,
            ..
        }
        | UpnpSource::RemoteStream {
            source_ref,
            source_rate,
            source_bits,
            ..
        }
        | UpnpSource::GeneratedDspStream {
            source_ref,
            source_rate,
            source_bits,
            ..
        } => passthrough_render_signature(
            source_ref,
            *source_rate,
            *source_bits,
            target,
            playback_config,
        ),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

async fn upnp_source_from_ref(
    state: &AppState,
    zone_id: &str,
    source_ref: &SourceRef,
    target: &UpnpRendererTarget,
) -> Result<UpnpSource, PlaybackError> {
    match source_ref {
        SourceRef::LocalTrack {
            track_id,
            title,
            artist,
            album,
            duration_secs,
            ..
        } => {
            let path = state
                .library()
                .track_path(*track_id)
                .map_err(PlaybackError::library)?
                .ok_or_else(|| PlaybackError::not_found("Track not found"))?;
            let track = state
                .library()
                .track_by_id(*track_id)
                .map_err(PlaybackError::library)?;
            let byte_len = tokio::fs::metadata(&path).await.ok().map(|meta| meta.len());
            Ok(UpnpSource::LocalFile {
                source_ref: source_ref.clone(),
                path,
                tags: TrackTags {
                    title: title.clone(),
                    artist: artist.clone(),
                    album: album.clone(),
                    duration_secs: *duration_secs,
                    ..TrackTags::default()
                },
                cover: None,
                byte_len,
                source_rate: track
                    .as_ref()
                    .and_then(|track| positive_i64_as_u32(track.sample_rate))
                    .unwrap_or(0),
                source_bits: track
                    .as_ref()
                    .and_then(|track| positive_i64_as_u32(track.bit_depth))
                    .unwrap_or(0),
            })
        }
        SourceRef::QobuzTrack {
            track_id,
            title,
            artist,
            album,
            image_url: _,
            duration_secs,
            ..
        } => {
            let resolve_started = Instant::now();
            let qobuz_hires_enabled = state
                .library()
                .zone_settings(zone_id)
                .map(|settings| settings.qobuz_hires_enabled)
                .unwrap_or(false);
            let qobuz_format_id = qobuz_format_id_for_upnp_target(target, qobuz_hires_enabled);
            debug!(
                event = "qobuz_upnp_quality_select",
                zone_id,
                renderer = %target.name,
                qobuz_hires_enabled,
                requested_format_id = qobuz_format_id,
                max_sample_rate = target.max_sample_rate,
                max_bit_depth = target.max_bit_depth,
                capability_detection_source = ?target.capability_detection_source,
                "Selected Qobuz quality for UPnP renderer"
            );
            let stream = state
                .qobuz()
                .resolved_stream_for_format(*track_id, Some(qobuz_format_id))
                .await
                .map_err(PlaybackError::integration)?;
            let qobuz_resolve_ms = resolve_started
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64;
            let registration_started = Instant::now();
            let asset_id = format!("qobuz-{track_id}-{}", stream.format_id);
            let token = state.upnp().register_remote_stream(
                &asset_id,
                None,
                stream.mime_type.clone(),
                stream.byte_len,
                Some(stream.format_id),
            );
            let asset_registration_ms = registration_started
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64;
            let stream_url =
                upnp_qobuz_stream_url(state.public_base_url(), &asset_id, &token, *track_id);
            Ok(UpnpSource::RemoteStream {
                id: asset_id,
                source_ref: source_ref.clone(),
                stream_url,
                mime_type: stream.mime_type,
                byte_len: stream.byte_len,
                art_url: None,
                tags: TrackTags {
                    title: title.clone(),
                    artist: artist.clone(),
                    album: album.clone(),
                    album_artist: artist.clone(),
                    duration_secs: *duration_secs,
                    ..TrackTags::default()
                },
                source_rate: stream.sample_rate_hz,
                source_bits: stream.bit_depth as u32,
                qobuz_resolve_ms: Some(qobuz_resolve_ms),
                asset_registration_ms: Some(asset_registration_ms),
            })
        }
    }
}

pub(super) fn qobuz_format_id_for_upnp_target(
    target: &UpnpRendererTarget,
    hires_enabled: bool,
) -> u32 {
    if !hires_enabled || is_sonos_upnp_target(target) {
        return 6;
    }
    if target.max_bit_depth >= 24 && target.max_sample_rate >= 192_000 {
        27
    } else if target.max_bit_depth >= 24 && target.max_sample_rate >= 96_000 {
        7
    } else {
        6
    }
}

fn positive_i64_as_u32(value: Option<i64>) -> Option<u32> {
    let value = value?;
    (value > 0).then_some(value.min(i64::from(u32::MAX)) as u32)
}

fn is_sonos_upnp_target(target: &UpnpRendererTarget) -> bool {
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

fn upnp_qobuz_stream_url(
    public_base_url: &str,
    asset_id: &str,
    token: &str,
    track_id: u64,
) -> String {
    format!(
        "{}/upnp/qobuz/{}/{}/{}",
        public_base_url.trim_end_matches('/'),
        urlencoding::encode(asset_id),
        urlencoding::encode(token),
        track_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::ZoneUpnpCapabilities;
    use crate::playback::test_support::app_state;

    #[test]
    fn qobuz_upnp_stream_url_uses_path_tokens() {
        let url = upnp_qobuz_stream_url(
            "http://192.168.1.30:3001/",
            "qobuz-423765381-6",
            "abcDEF123",
            423765381,
        );

        assert_eq!(
            url,
            "http://192.168.1.30:3001/upnp/qobuz/qobuz-423765381-6/abcDEF123/423765381"
        );
        assert!(!url.contains('?'));
        assert!(!url.contains('&'));
    }

    #[test]
    fn kef_uses_completed_render_instead_of_progressive_wav() {
        let mut target = test_upnp_target("kef-lsx", "192.168.1.91", "/AVTransport");
        target.name = "KEF LSX".to_string();
        target.manufacturer = Some("KEF".to_string());

        assert_eq!(
            upnp_dsp_streaming_policy_for_target(&target),
            UpnpDspStreamingPolicy::ForceCompletedRender
        );
    }

    #[test]
    fn non_kef_retains_progressive_dsp_streaming() {
        let target = test_upnp_target("renderer", "192.168.1.50", "/AVTransport");

        assert_eq!(
            upnp_dsp_streaming_policy_for_target(&target),
            UpnpDspStreamingPolicy::Auto
        );
    }

    #[test]
    fn saved_upnp_capabilities_override_live_renderer_caps() {
        let state = app_state("saved-upnp-capabilities");
        let zone_id = "upnp-renderer-1";
        let target = UpnpRendererTarget {
            id: "renderer-1".to_string(),
            name: "Renderer".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: None,
            manufacturer: None,
            av_transport_control_url: "/AVTransport".to_string(),
            rendering_control_url: None,
            connection_manager_url: None,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
            max_dsd_rate: Some(64),
            capability_detection_source: crate::protocol::CapabilityDetectionSource::Advertised,
            capability_detection_status: crate::protocol::CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        };
        state
            .library()
            .upsert_zone_definition(
                zone_id,
                "Renderer",
                "upnp_av_renderer",
                Some(&upnp::target_device_name(&target)),
                true,
            )
            .unwrap();
        let mut settings = state.library().zone_settings(zone_id).unwrap();
        settings.upnp_capabilities = Some(ZoneUpnpCapabilities {
            max_sample_rate: 96_000,
            max_bit_depth: 24,
            max_dsd_rate: None,
            pcm_containers: Vec::new(),
        });
        state
            .library()
            .set_zone_settings(zone_id, settings)
            .unwrap();

        let overridden = apply_saved_upnp_capabilities(&state, zone_id, target);

        assert_eq!(overridden.max_sample_rate, 96_000);
        assert_eq!(overridden.max_bit_depth, 24);
        assert_eq!(overridden.max_dsd_rate, None);
    }

    #[test]
    fn rendered_upnp_asset_keeps_source_and_output_signal_path() {
        let source_ref = test_source_ref();
        let mut asset = UpnpAsset {
            id: "asset".to_string(),
            source_ref: source_ref.clone(),
            stream_url: "http://127.0.0.1/upnp/stream/asset".to_string(),
            mime_type: "audio/flac".to_string(),
            byte_len: None,
            art_url: None,
            title: Some("Track".to_string()),
            artist: None,
            album: None,
            duration_secs: None,
            source_rate: 192_000,
            target_rate: 192_000,
            source_bits: 24,
            target_bits: 24,
            active_output_mode: None,
            qobuz_resolve_ms: None,
            asset_registration_ms: None,
            render_signature: None,
            configured_render_signature: None,
            render_ms: None,
            prepare_ms: None,
            cache_hit: None,
            render_or_stream_plan: None,
            cache_lookup_ms: None,
            cache_wait_ms: None,
        };
        let decision = UpnpDspDecision {
            rendered: None,
            render_signature: "sig".to_string(),
            render_ms: Some(12),
            cache_hit: Some(false),
            render_or_stream_plan: Some("eager_render".to_string()),
            cache_lookup_ms: Some(1),
            cache_wait_ms: Some(2),
            active_output_mode: "Pcm".to_string(),
            source_rate: 44_100,
            source_bits: 16,
            output_rate: 192_000,
            output_bits: 24,
        };

        apply_rendered_signal_path_to_asset(&mut asset, &decision);

        assert_eq!(asset.source_ref.key(), source_ref.key());
        assert_eq!(asset.source_rate, 44_100);
        assert_eq!(asset.source_bits, 16);
        assert_eq!(asset.target_rate, 192_000);
        assert_eq!(asset.target_bits, 24);
    }

    #[test]
    fn qobuz_upnp_format_respects_toggle_and_renderer_caps() {
        let mut target = UpnpRendererTarget {
            id: "renderer-1".to_string(),
            name: "Renderer".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: None,
            manufacturer: None,
            av_transport_control_url: "/AVTransport".to_string(),
            rendering_control_url: None,
            connection_manager_url: None,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
            max_dsd_rate: None,
            capability_detection_source: crate::protocol::CapabilityDetectionSource::Advertised,
            capability_detection_status: crate::protocol::CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        };

        assert_eq!(qobuz_format_id_for_upnp_target(&target, false), 6);
        assert_eq!(qobuz_format_id_for_upnp_target(&target, true), 27);

        target.max_sample_rate = 96_000;
        assert_eq!(qobuz_format_id_for_upnp_target(&target, true), 7);

        target.max_sample_rate = 48_000;
        assert_eq!(qobuz_format_id_for_upnp_target(&target, true), 6);

        target.max_sample_rate = 192_000;
        target.max_bit_depth = 16;
        assert_eq!(qobuz_format_id_for_upnp_target(&target, true), 6);

        target.max_sample_rate = 48_000;
        target.max_bit_depth = 16;
        target.capability_detection_source = crate::protocol::CapabilityDetectionSource::Fallback;
        assert_eq!(qobuz_format_id_for_upnp_target(&target, true), 6);

        target.capability_detection_source = crate::protocol::CapabilityDetectionSource::Advertised;
        assert_eq!(qobuz_format_id_for_upnp_target(&target, true), 6);

        target.capability_detection_source = crate::protocol::CapabilityDetectionSource::Fallback;
        target.manufacturer = Some("Sonos".to_string());
        assert_eq!(qobuz_format_id_for_upnp_target(&target, true), 6);
    }

    fn test_source_ref() -> SourceRef {
        SourceRef::LocalTrack {
            track_id: 1,
            file_name: None,
            title: Some("Track".to_string()),
            artist: None,
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn test_upnp_target(id: &str, host: &str, control_url: &str) -> UpnpRendererTarget {
        UpnpRendererTarget {
            id: id.to_string(),
            name: "Renderer".to_string(),
            host: host.to_string(),
            port: 1400,
            model: None,
            manufacturer: None,
            av_transport_control_url: control_url.to_string(),
            rendering_control_url: None,
            connection_manager_url: None,
            max_sample_rate: 48_000,
            max_bit_depth: 16,
            max_dsd_rate: None,
            capability_detection_source: crate::protocol::CapabilityDetectionSource::Fallback,
            capability_detection_status: crate::protocol::CapabilityDetectionStatus::Unknown,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        }
    }

    #[test]
    fn live_upnp_target_refresh_improves_caps_without_fallback_downgrade() {
        let stored = UpnpRendererTarget {
            id: "renderer-1".to_string(),
            name: "Renderer".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: None,
            manufacturer: None,
            av_transport_control_url: "/old/AVTransport".to_string(),
            rendering_control_url: None,
            connection_manager_url: None,
            max_sample_rate: 48_000,
            max_bit_depth: 16,
            max_dsd_rate: None,
            capability_detection_source: crate::protocol::CapabilityDetectionSource::Fallback,
            capability_detection_status: crate::protocol::CapabilityDetectionStatus::Unknown,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        };
        let mut live = stored.clone();
        live.av_transport_control_url = "/new/AVTransport".to_string();
        live.max_sample_rate = 192_000;
        live.max_bit_depth = 24;
        live.capability_detection_source = crate::protocol::CapabilityDetectionSource::Probed;

        let refreshed = merge_upnp_playback_target(stored.clone(), live, false);
        assert_eq!(refreshed.av_transport_control_url, "/old/AVTransport");
        assert_eq!(refreshed.host, "127.0.0.1");
        assert_eq!(refreshed.port, 1400);
        assert_eq!(refreshed.max_sample_rate, 192_000);
        assert_eq!(refreshed.max_bit_depth, 24);
        assert_eq!(
            refreshed.capability_detection_source,
            crate::protocol::CapabilityDetectionSource::Probed
        );

        let mut stored_probed = refreshed.clone();
        stored_probed.av_transport_control_url = "/saved/AVTransport".to_string();
        let mut live_fallback = stored.clone();
        live_fallback.av_transport_control_url = "/fresh/AVTransport".to_string();
        let refreshed = merge_upnp_playback_target(stored_probed, live_fallback, false);
        assert_eq!(refreshed.av_transport_control_url, "/saved/AVTransport");
        assert_eq!(refreshed.max_sample_rate, 192_000);
        assert_eq!(refreshed.max_bit_depth, 24);
        assert_eq!(
            refreshed.capability_detection_source,
            crate::protocol::CapabilityDetectionSource::Probed
        );
    }

    #[test]
    fn live_upnp_target_refresh_rejects_same_id_different_origin() {
        let state = app_state("upnp-spoofed-refresh");
        let zone_id = "upnp-victim-udn";
        let stored = test_upnp_target(
            "victim-udn",
            "192.168.1.10",
            "http://192.168.1.10:1400/AVTransport",
        );
        state.zones().sync_upnp_renderers(vec![upnp::UpnpRenderer {
            target: stored.clone(),
            online: true,
        }]);
        let mut spoof = stored.clone();
        spoof.host = "192.168.1.66".to_string();
        spoof.av_transport_control_url = "http://192.168.1.66:1400/AVTransport".to_string();
        spoof.max_sample_rate = 192_000;
        spoof.max_bit_depth = 24;
        spoof.capability_detection_source = crate::protocol::CapabilityDetectionSource::Probed;
        state.upnp().insert_test_renderer(spoof, true);

        let target = upnp_target_for_zone(&state, zone_id).unwrap();

        assert_eq!(target.host, "192.168.1.10");
        assert_eq!(
            target.av_transport_control_url,
            "http://192.168.1.10:1400/AVTransport"
        );
        assert_eq!(target.max_sample_rate, 48_000);
        assert_eq!(
            state
                .upnp()
                .snapshot(zone_id)
                .and_then(|snapshot| snapshot.notice),
            Some("UPnP renderer identity changed; re-pair this zone before playback".to_string())
        );
    }

    #[test]
    fn live_upnp_target_refresh_allows_verified_same_host_port_move() {
        let state = app_state("upnp-hegel-port-refresh");
        let zone_id = "upnp-hegel-udn";
        let mut stored = test_upnp_target(
            "hegel-udn",
            "192.168.1.50",
            "http://192.168.1.50:38400/AVTransport",
        );
        stored.name = "Hegel H390".to_string();
        stored.port = 38400;
        stored.model = Some("H390".to_string());
        stored.manufacturer = Some("Hegel".to_string());
        stored.max_sample_rate = 192_000;
        stored.max_bit_depth = 24;
        stored.capability_detection_source = crate::protocol::CapabilityDetectionSource::Probed;
        state.zones().sync_upnp_renderers(vec![upnp::UpnpRenderer {
            target: stored.clone(),
            online: true,
        }]);
        let mut live = stored.clone();
        live.port = 38401;
        live.av_transport_control_url = "http://192.168.1.50:38401/AVTransport".to_string();
        live.rendering_control_url = Some("http://192.168.1.50:38401/RenderingControl".to_string());
        live.max_sample_rate = 48_000;
        live.max_bit_depth = 16;
        live.capability_detection_source = crate::protocol::CapabilityDetectionSource::Fallback;
        state.upnp().insert_test_renderer(live, true);

        let target = upnp_target_for_zone(&state, zone_id).unwrap();

        assert_eq!(target.host, "192.168.1.50");
        assert_eq!(target.port, 38401);
        assert_eq!(
            target.av_transport_control_url,
            "http://192.168.1.50:38401/AVTransport"
        );
        assert_eq!(
            target.rendering_control_url.as_deref(),
            Some("http://192.168.1.50:38401/RenderingControl")
        );
        assert_eq!(target.max_sample_rate, 192_000);
        assert_eq!(target.max_bit_depth, 24);
        assert_eq!(
            target.capability_detection_source,
            crate::protocol::CapabilityDetectionSource::Probed
        );
    }

    #[test]
    fn live_upnp_target_refresh_rejects_unverified_same_host_port_move() {
        let state = app_state("upnp-hegel-port-unverified");
        let zone_id = "upnp-hegel-udn";
        let mut stored = test_upnp_target(
            "hegel-udn",
            "192.168.1.50",
            "http://192.168.1.50:38400/AVTransport",
        );
        stored.name = "Hegel H390".to_string();
        stored.port = 38400;
        state.zones().sync_upnp_renderers(vec![upnp::UpnpRenderer {
            target: stored.clone(),
            online: true,
        }]);
        let mut live = stored.clone();
        live.name = "Different Renderer".to_string();
        live.port = 38401;
        live.av_transport_control_url = "http://192.168.1.50:38401/AVTransport".to_string();
        live.max_sample_rate = 192_000;
        live.max_bit_depth = 24;
        live.capability_detection_source = crate::protocol::CapabilityDetectionSource::Probed;
        state.upnp().insert_test_renderer(live, true);

        let target = upnp_target_for_zone(&state, zone_id).unwrap();

        assert_eq!(target.port, 38400);
        assert_eq!(
            target.av_transport_control_url,
            "http://192.168.1.50:38400/AVTransport"
        );
        assert_eq!(
            state
                .upnp()
                .snapshot(zone_id)
                .and_then(|snapshot| snapshot.notice),
            Some(
                "UPnP renderer endpoint moved from 192.168.1.50:38400 to 192.168.1.50:38401 but could not be verified"
                    .to_string()
            )
        );
    }

    #[test]
    fn live_upnp_target_refresh_allows_same_origin_capability_merge() {
        let state = app_state("upnp-same-origin-refresh");
        let zone_id = "upnp-victim-udn";
        let stored = test_upnp_target(
            "victim-udn",
            "192.168.1.10",
            "http://192.168.1.10:1400/AVTransport",
        );
        state.zones().sync_upnp_renderers(vec![upnp::UpnpRenderer {
            target: stored.clone(),
            online: true,
        }]);
        let mut live = stored.clone();
        live.av_transport_control_url = "http://192.168.1.10:1400/fresh/AVTransport".to_string();
        live.max_sample_rate = 192_000;
        live.max_bit_depth = 24;
        live.capability_detection_source = crate::protocol::CapabilityDetectionSource::Probed;
        state.upnp().insert_test_renderer(live, true);

        let target = upnp_target_for_zone(&state, zone_id).unwrap();

        assert_eq!(
            target.av_transport_control_url,
            "http://192.168.1.10:1400/AVTransport"
        );
        assert_eq!(target.max_sample_rate, 192_000);
        assert_eq!(target.max_bit_depth, 24);
    }
}
