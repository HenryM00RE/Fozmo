use crate::app::capabilities::BuildCapabilities;
use crate::app::state::AppState;
use crate::audio::device_volume;
use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::player::Player;
use crate::diagnostics::logging::{error_kind, sanitize_error};
use crate::playback::service::hegel_settings_for_zone;
use crate::playback::sonos::sonos_target_for_zone;
use crate::playback::upnp::upnp_target_for_zone;
use crate::protocol::{
    AgentBufferState, BrowserStreamSignal, DsdBufferHealth, SinkProtocol, SourceRef, SyncSignalPath,
};
use crate::services::hegel;
use crate::settings::DsdSourceRule;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

const SONOS_PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(750);
const SONOS_VOLUME_POLL_INTERVAL: Duration = Duration::from_secs(5);
const UPNP_PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(900);
const UPNP_VOLUME_POLL_INTERVAL: Duration = Duration::from_secs(5);
const HEGEL_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Serialize, JsonSchema)]
pub struct StatusResponse {
    pub surface: String,
    pub capabilities: BuildCapabilities,
    /// Runtime state of the separately built direct-AirPlay process.
    pub airplay_helper_state: String,
    pub state: String,
    pub file_name: Option<String>,
    pub current_source: Option<SourceRef>,
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    pub cover_version: u64,
    pub source_rate: u32,
    pub target_rate: u32,
    pub source_bits: u32,
    pub target_bits: u32,
    pub configured_target_rate: u32,
    pub configured_target_bit_depth: u32,
    pub upsampling_enabled: bool,
    pub filter_type: String,
    pub active_filter_type: String,
    pub src_path_kind: Option<String>,
    pub src_capped_fallback: bool,
    pub src_phase_profile_preserved: bool,
    pub src_ratio_num: u32,
    pub src_ratio_den: u32,
    pub dither_mode: String,
    pub output_mode: String,
    pub active_output_mode: String,
    pub dsd_modulator: String,
    pub dsd_isi_penalty: f32,
    pub output_transport: String,
    pub output_notice_id: u64,
    pub output_notice: Option<String>,
    pub upnp_config_applied_to_current_playback: bool,
    pub upnp_restart_pending: bool,
    pub upnp_render_status: String,
    pub upnp_active_render_signature: Option<String>,
    pub upnp_configured_render_signature: Option<String>,
    pub upnp_last_render_ms: Option<u64>,
    pub upnp_last_prepare_ms: Option<u64>,
    pub upnp_last_cache_hit: Option<bool>,
    pub transport_pending: String,
    pub transport_pending_position_secs: Option<f64>,
    pub dsd_stability_resets: u64,
    pub dsd_rules_enabled: bool,
    pub dsd_rules: Vec<DsdSourceRule>,
    pub volume: f32,
    pub device_volume: Option<f32>,
    pub device_volume_supported: bool,
    pub device_volume_max: Option<f32>,
    pub device_volume_message: Option<String>,
    pub headroom_db: f32,
    pub dsp_buffer_ms: u32,
    pub exclusive: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub playback_speed: Option<String>,
    pub resample_time_ns: u64,
    pub dsd_upsample_time_ns: u64,
    pub dsd_modulate_time_ns: u64,
    pub dsd_output_pending_samples: u64,
    pub dsd_buffer_health: Option<DsdBufferHealth>,
    pub dsd_overbudget_blocks: u64,
    pub dsd_last_load: f32,
    pub dsd_recent_load_p95: f32,
    pub dsd_recent_load_p99: f32,
    pub dop_ring_capacity_ms: f64,
    pub dop_ring_fill_ms: f64,
    pub dop_ring_low_watermark_ms: f64,
    pub dop_callback_frames: u32,
    pub dop_callback_ms: f64,
    pub dop_requested_hardware_buffer_frames: u32,
    pub dop_requested_hardware_buffer_ms: f64,
    pub dop_hardware_buffer_min_frames: u32,
    pub dop_hardware_buffer_max_frames: u32,
    pub dop_hardware_buffer_frames: u32,
    pub dop_hardware_buffer_ms: f64,
    pub dop_lock_miss_events: u64,
    pub dop_callback_deadline_miss_events: u64,
    pub dop_soft_callback_gap_125_events: u64,
    pub dop_soft_callback_gap_150_events: u64,
    pub dop_soft_callback_gap_175_events: u64,
    pub dop_last_soft_callback_gap_ms: f64,
    pub dop_last_soft_callback_gap_at_ms: u64,
    pub dop_ring_below_250ms_events: u64,
    pub dop_ring_below_100ms_events: u64,
    pub dop_ring_below_50ms_events: u64,
    pub dop_ring_below_callback_events: u64,
    pub dop_last_ring_pressure_at_ms: u64,
    pub dop_marker_error_events: u64,
    pub dop_program_idle_splice_events: u64,
    pub dop_program_to_idle_events: u64,
    pub dop_idle_to_program_events: u64,
    pub dop_mixed_output_events: u64,
    pub dop_last_output_transition_id: u32,
    pub dop_last_output_transition_at_ms: u64,
    pub dop_repeated_payload_events: u64,
    pub dop_callback_index: u64,
    pub dop_last_callback_at_ms: u64,
    pub dop_last_callback_gap_ms: f64,
    pub dop_last_callback_frames: u32,
    pub dop_last_output_kind_id: u32,
    pub dop_last_ring_fill_samples: u64,
    pub dop_last_program_read_samples: u64,
    pub dop_ring_read_cursor_samples: u64,
    pub dop_last_payload_fingerprint: u64,
    pub dop_last_payload_fingerprint_at_ms: u64,
    pub dop_marker_scan_count: u64,
    pub dop_every_callback_scan_enabled: bool,
    pub dop_last_underrun_at_ms: u64,
    pub output_ring_fill_now_ms: f64,
    pub output_ring_fill_min_ms: f64,
    pub startup_ring_low_watermark_ms: f64,
    pub startup_ready_ms: u64,
    pub startup_first_render_block_ms: f64,
    pub startup_producer_over_budget_count: u64,
    pub startup_callback_gaps_ms: Vec<f64>,
    pub underrun_count: u64,
    pub producer_over_budget_count: u64,
    pub max_render_block_ms: f64,
    pub max_audio_callback_gap_ms: f64,
    pub dsp_graph_rebuild_count: u64,
    pub sample_rate_change_count: u64,
    pub dop_alignment_reset_count: u64,
    pub coreaudio_dop_open_count: u64,
    pub coreaudio_dop_start_count: u64,
    pub coreaudio_dop_stop_count: u64,
    pub coreaudio_dop_drop_count: u64,
    pub coreaudio_dop_quiesce_count: u64,
    pub coreaudio_dop_last_lifecycle_event_id: u32,
    pub coreaudio_dop_last_lifecycle_at_ms: u64,
    pub reopen_reason_count: u64,
    pub last_reopen_reason_id: u32,
    pub last_reopen_reason_at_ms: u64,
    pub flush_reason_count: u64,
    pub last_flush_reason_id: u32,
    pub last_flush_reason_at_ms: u64,
    pub modulator_reset_count: u64,
    pub decoder_starved_count: u64,
    pub source_read_time_ms: f64,
    pub max_source_read_ms: f64,
    pub source_read_stall_count: u64,
    pub source_read_stall_last_at_ms: u64,
    pub decoder_decode_time_ms: f64,
    pub max_decoder_decode_ms: f64,
    pub decoder_decode_stall_count: u64,
    pub decoder_decode_stall_last_at_ms: u64,
    pub lock_wait_max_ms: f64,
    pub block_duration_ns: u64,
    pub cpu_percent: f32,
    pub meter_l: f32,
    pub meter_r: f32,
    pub signal_peak: f32,
    pub signal_peak_max: f32,
    pub signal_clipping: bool,
    pub signal_clip_events: u64,
    pub signal_clip_samples: u64,
    pub dsd_limiter_peak_ratio: f32,
    pub dsd_limiter_peak_ratio_max: f32,
    pub dsd_limiter_active: bool,
    pub dsd_limiter_events: u64,
    pub dsd_limiter_samples: u64,
    pub underrun_events: u64,
    pub underrun_samples: u64,
    pub selected_device: Option<String>,
    pub active_zone_id: String,
    pub active_zone_name: String,
    pub zone_protocol: SinkProtocol,
    pub remote_connected: bool,
    pub remote_signal_path: Option<SyncSignalPath>,
    pub remote_buffer_state: Option<AgentBufferState>,
    /// Server-side chain for the active browser-zone stream, when known.
    #[serde(default)]
    pub browser_stream_signal: Option<BrowserStreamSignal>,
}

pub async fn refresh_active_output_status(state: &AppState) {
    let _ = state;
    #[cfg(feature = "sonos")]
    {
        refresh_sonos_playback(state).await;
        refresh_sonos_volume(state).await;
    }
    #[cfg(feature = "upnp")]
    {
        refresh_upnp_playback(state).await;
        refresh_upnp_volume(state).await;
    }
    #[cfg(feature = "hegel")]
    refresh_active_hegel_status(state).await;
}

#[derive(Clone, Copy, Default)]
struct DopDebugFields {
    ring_capacity_ms: f64,
    ring_fill_ms: f64,
    ring_low_watermark_ms: f64,
    callback_frames: u32,
    callback_ms: f64,
    requested_hardware_buffer_frames: u32,
    requested_hardware_buffer_ms: f64,
    hardware_buffer_min_frames: u32,
    hardware_buffer_max_frames: u32,
    hardware_buffer_frames: u32,
    hardware_buffer_ms: f64,
    lock_miss_events: u64,
    callback_deadline_miss_events: u64,
    soft_callback_gap_125_events: u64,
    soft_callback_gap_150_events: u64,
    soft_callback_gap_175_events: u64,
    last_soft_callback_gap_ms: f64,
    last_soft_callback_gap_at_ms: u64,
    ring_below_250ms_events: u64,
    ring_below_100ms_events: u64,
    ring_below_50ms_events: u64,
    ring_below_callback_events: u64,
    last_ring_pressure_at_ms: u64,
    marker_error_events: u64,
    program_idle_splice_events: u64,
    program_to_idle_events: u64,
    idle_to_program_events: u64,
    mixed_output_events: u64,
    last_output_transition_id: u32,
    last_output_transition_at_ms: u64,
    repeated_payload_events: u64,
    callback_index: u64,
    last_callback_at_ms: u64,
    last_callback_gap_ms: f64,
    last_callback_frames: u32,
    last_output_kind_id: u32,
    last_ring_fill_samples: u64,
    last_program_read_samples: u64,
    ring_read_cursor_samples: u64,
    last_payload_fingerprint: u64,
    last_payload_fingerprint_at_ms: u64,
    marker_scan_count: u64,
    every_callback_scan_enabled: bool,
    last_underrun_at_ms: u64,
}

impl DopDebugFields {
    fn from_health(health: Option<&DsdBufferHealth>) -> Self {
        health
            .map(|health| Self {
                ring_capacity_ms: health.ring_capacity_ms,
                ring_fill_ms: health.ring_fill_ms,
                ring_low_watermark_ms: health.ring_low_watermark_ms,
                callback_frames: health.callback_frames,
                callback_ms: health.callback_ms,
                requested_hardware_buffer_frames: health.requested_hardware_buffer_frames,
                requested_hardware_buffer_ms: health.requested_hardware_buffer_ms,
                hardware_buffer_min_frames: health.hardware_buffer_min_frames,
                hardware_buffer_max_frames: health.hardware_buffer_max_frames,
                hardware_buffer_frames: health.hardware_buffer_frames,
                hardware_buffer_ms: health.hardware_buffer_ms,
                lock_miss_events: health.lock_miss_events,
                callback_deadline_miss_events: health.callback_deadline_miss_events,
                soft_callback_gap_125_events: health.soft_callback_gap_125_events,
                soft_callback_gap_150_events: health.soft_callback_gap_150_events,
                soft_callback_gap_175_events: health.soft_callback_gap_175_events,
                last_soft_callback_gap_ms: health.last_soft_callback_gap_ms,
                last_soft_callback_gap_at_ms: health.last_soft_callback_gap_at_ms,
                ring_below_250ms_events: health.ring_below_250ms_events,
                ring_below_100ms_events: health.ring_below_100ms_events,
                ring_below_50ms_events: health.ring_below_50ms_events,
                ring_below_callback_events: health.ring_below_callback_events,
                last_ring_pressure_at_ms: health.last_ring_pressure_at_ms,
                marker_error_events: health.marker_error_events,
                program_idle_splice_events: health.program_idle_splice_events,
                program_to_idle_events: health.program_to_idle_events,
                idle_to_program_events: health.idle_to_program_events,
                mixed_output_events: health.mixed_output_events,
                last_output_transition_id: health.last_output_transition_id,
                last_output_transition_at_ms: health.last_output_transition_at_ms,
                repeated_payload_events: health.repeated_payload_events,
                callback_index: health.callback_index,
                last_callback_at_ms: health.last_callback_at_ms,
                last_callback_gap_ms: health.last_callback_gap_ms,
                last_callback_frames: health.last_callback_frames,
                last_output_kind_id: health.last_output_kind_id,
                last_ring_fill_samples: health.last_ring_fill_samples,
                last_program_read_samples: health.last_program_read_samples,
                ring_read_cursor_samples: health.ring_read_cursor_samples,
                last_payload_fingerprint: health.last_payload_fingerprint,
                last_payload_fingerprint_at_ms: health.last_payload_fingerprint_at_ms,
                marker_scan_count: health.marker_scan_count,
                every_callback_scan_enabled: health.every_callback_scan_enabled,
                last_underrun_at_ms: health.last_underrun_at_ms,
            })
            .unwrap_or_default()
    }

    fn from_signal(signal: &SyncSignalPath) -> Self {
        Self {
            ring_capacity_ms: signal.dop_ring_capacity_ms,
            ring_fill_ms: signal.dop_ring_fill_ms,
            ring_low_watermark_ms: signal.dop_ring_low_watermark_ms,
            callback_frames: signal.dop_callback_frames,
            callback_ms: signal.dop_callback_ms,
            requested_hardware_buffer_frames: signal.dop_requested_hardware_buffer_frames,
            requested_hardware_buffer_ms: signal.dop_requested_hardware_buffer_ms,
            hardware_buffer_min_frames: signal.dop_hardware_buffer_min_frames,
            hardware_buffer_max_frames: signal.dop_hardware_buffer_max_frames,
            hardware_buffer_frames: signal.dop_hardware_buffer_frames,
            hardware_buffer_ms: signal.dop_hardware_buffer_ms,
            lock_miss_events: signal.dop_lock_miss_events,
            callback_deadline_miss_events: signal.dop_callback_deadline_miss_events,
            soft_callback_gap_125_events: signal.dop_soft_callback_gap_125_events,
            soft_callback_gap_150_events: signal.dop_soft_callback_gap_150_events,
            soft_callback_gap_175_events: signal.dop_soft_callback_gap_175_events,
            last_soft_callback_gap_ms: signal.dop_last_soft_callback_gap_ms,
            last_soft_callback_gap_at_ms: signal.dop_last_soft_callback_gap_at_ms,
            ring_below_250ms_events: signal.dop_ring_below_250ms_events,
            ring_below_100ms_events: signal.dop_ring_below_100ms_events,
            ring_below_50ms_events: signal.dop_ring_below_50ms_events,
            ring_below_callback_events: signal.dop_ring_below_callback_events,
            last_ring_pressure_at_ms: signal.dop_last_ring_pressure_at_ms,
            marker_error_events: signal.dop_marker_error_events,
            program_idle_splice_events: signal.dop_program_idle_splice_events,
            program_to_idle_events: signal.dop_program_to_idle_events,
            idle_to_program_events: signal.dop_idle_to_program_events,
            mixed_output_events: signal.dop_mixed_output_events,
            last_output_transition_id: signal.dop_last_output_transition_id,
            last_output_transition_at_ms: signal.dop_last_output_transition_at_ms,
            repeated_payload_events: signal.dop_repeated_payload_events,
            callback_index: signal.dop_callback_index,
            last_callback_at_ms: signal.dop_last_callback_at_ms,
            last_callback_gap_ms: signal.dop_last_callback_gap_ms,
            last_callback_frames: signal.dop_last_callback_frames,
            last_output_kind_id: signal.dop_last_output_kind_id,
            last_ring_fill_samples: signal.dop_last_ring_fill_samples,
            last_program_read_samples: signal.dop_last_program_read_samples,
            ring_read_cursor_samples: signal.dop_ring_read_cursor_samples,
            last_payload_fingerprint: signal.dop_last_payload_fingerprint,
            last_payload_fingerprint_at_ms: signal.dop_last_payload_fingerprint_at_ms,
            marker_scan_count: signal.dop_marker_scan_count,
            every_callback_scan_enabled: signal.dop_every_callback_scan_enabled,
            last_underrun_at_ms: signal.dop_last_underrun_at_ms,
        }
    }
}

fn apply_dop_debug_fields(response: &mut StatusResponse, debug: DopDebugFields) {
    response.dop_ring_capacity_ms = debug.ring_capacity_ms;
    response.dop_ring_fill_ms = debug.ring_fill_ms;
    response.dop_ring_low_watermark_ms = debug.ring_low_watermark_ms;
    response.dop_callback_frames = debug.callback_frames;
    response.dop_callback_ms = debug.callback_ms;
    response.dop_requested_hardware_buffer_frames = debug.requested_hardware_buffer_frames;
    response.dop_requested_hardware_buffer_ms = debug.requested_hardware_buffer_ms;
    response.dop_hardware_buffer_min_frames = debug.hardware_buffer_min_frames;
    response.dop_hardware_buffer_max_frames = debug.hardware_buffer_max_frames;
    response.dop_hardware_buffer_frames = debug.hardware_buffer_frames;
    response.dop_hardware_buffer_ms = debug.hardware_buffer_ms;
    response.dop_lock_miss_events = debug.lock_miss_events;
    response.dop_callback_deadline_miss_events = debug.callback_deadline_miss_events;
    response.dop_soft_callback_gap_125_events = debug.soft_callback_gap_125_events;
    response.dop_soft_callback_gap_150_events = debug.soft_callback_gap_150_events;
    response.dop_soft_callback_gap_175_events = debug.soft_callback_gap_175_events;
    response.dop_last_soft_callback_gap_ms = debug.last_soft_callback_gap_ms;
    response.dop_last_soft_callback_gap_at_ms = debug.last_soft_callback_gap_at_ms;
    response.dop_ring_below_250ms_events = debug.ring_below_250ms_events;
    response.dop_ring_below_100ms_events = debug.ring_below_100ms_events;
    response.dop_ring_below_50ms_events = debug.ring_below_50ms_events;
    response.dop_ring_below_callback_events = debug.ring_below_callback_events;
    response.dop_last_ring_pressure_at_ms = debug.last_ring_pressure_at_ms;
    response.dop_marker_error_events = debug.marker_error_events;
    response.dop_program_idle_splice_events = debug.program_idle_splice_events;
    response.dop_program_to_idle_events = debug.program_to_idle_events;
    response.dop_idle_to_program_events = debug.idle_to_program_events;
    response.dop_mixed_output_events = debug.mixed_output_events;
    response.dop_last_output_transition_id = debug.last_output_transition_id;
    response.dop_last_output_transition_at_ms = debug.last_output_transition_at_ms;
    response.dop_repeated_payload_events = debug.repeated_payload_events;
    response.dop_callback_index = debug.callback_index;
    response.dop_last_callback_at_ms = debug.last_callback_at_ms;
    response.dop_last_callback_gap_ms = debug.last_callback_gap_ms;
    response.dop_last_callback_frames = debug.last_callback_frames;
    response.dop_last_output_kind_id = debug.last_output_kind_id;
    response.dop_last_ring_fill_samples = debug.last_ring_fill_samples;
    response.dop_last_program_read_samples = debug.last_program_read_samples;
    response.dop_ring_read_cursor_samples = debug.ring_read_cursor_samples;
    response.dop_last_payload_fingerprint = debug.last_payload_fingerprint;
    response.dop_last_payload_fingerprint_at_ms = debug.last_payload_fingerprint_at_ms;
    response.dop_marker_scan_count = debug.marker_scan_count;
    response.dop_every_callback_scan_enabled = debug.every_callback_scan_enabled;
    response.dop_last_underrun_at_ms = debug.last_underrun_at_ms;
}

pub async fn refresh_sonos_playback(state: &AppState) {
    for zone in state.zones().list_zones() {
        if !zone.enabled || zone.protocol != SinkProtocol::SonosUpnp {
            continue;
        }
        let Ok(target) = sonos_target_for_zone(state, &zone.id) else {
            continue;
        };
        if let Err(e) = state
            .sonos()
            .refresh_playback_if_stale(&zone.id, &target, SONOS_PLAYBACK_POLL_INTERVAL)
            .await
        {
            warn!(
                event = "external_service_failure",
                service = "sonos",
                operation = "playback_refresh",
                zone_id = %zone.id,
                error_kind = error_kind(&e),
                error = %sanitize_error(&e),
                "Sonos playback refresh failed"
            );
        }
    }
}

pub async fn refresh_sonos_volume(state: &AppState) {
    for zone in state.zones().list_zones() {
        if !zone.enabled || zone.protocol != SinkProtocol::SonosUpnp {
            continue;
        }
        let Ok(target) = sonos_target_for_zone(state, &zone.id) else {
            continue;
        };
        if let Err(e) = state
            .sonos()
            .refresh_volume_if_stale(&zone.id, &target, SONOS_VOLUME_POLL_INTERVAL)
            .await
        {
            warn!(
                event = "external_service_failure",
                service = "sonos",
                operation = "volume_refresh",
                zone_id = %zone.id,
                error_kind = error_kind(&e),
                error = %sanitize_error(&e),
                "Sonos volume refresh failed"
            );
        }
    }
}

pub async fn refresh_upnp_playback(state: &AppState) {
    for zone in state.zones().list_zones() {
        if !zone.enabled || zone.protocol != SinkProtocol::UpnpAvRenderer {
            continue;
        }
        let Ok(target) = upnp_target_for_zone(state, &zone.id) else {
            continue;
        };
        if let Err(e) = state
            .upnp()
            .refresh_playback_snapshot(&zone.id, &target, UPNP_PLAYBACK_POLL_INTERVAL)
            .await
        {
            warn!(
                event = "external_service_failure",
                service = "upnp",
                operation = "playback_refresh",
                zone_id = %zone.id,
                error_kind = error_kind(&e),
                error = %sanitize_error(&e),
                "UPnP playback refresh failed"
            );
        }
    }
}

pub async fn refresh_upnp_volume(state: &AppState) {
    for zone in state.zones().list_zones() {
        if !zone.enabled || zone.protocol != SinkProtocol::UpnpAvRenderer {
            continue;
        }
        let Ok(target) = upnp_target_for_zone(state, &zone.id) else {
            continue;
        };
        if let Err(e) = state
            .upnp()
            .refresh_volume(&zone.id, &target, UPNP_VOLUME_POLL_INTERVAL)
            .await
        {
            warn!(
                event = "external_service_failure",
                service = "upnp",
                operation = "volume_refresh",
                zone_id = %zone.id,
                error_kind = error_kind(&e),
                error = %sanitize_error(&e),
                "UPnP volume refresh failed"
            );
        }
    }
}

async fn refresh_active_hegel_status(state: &AppState) {
    let zone_id = state.zones().active_zone_id();
    let Some(settings) = hegel_settings_for_zone(state, &zone_id) else {
        return;
    };
    if !state
        .hegel_status()
        .mark_poll_due(HEGEL_STATUS_POLL_INTERVAL)
    {
        return;
    }
    let host = settings.host.as_deref().unwrap_or_default();
    match hegel::query_status(host, settings.port).await {
        Ok(status) => {
            state.hegel_status().remember(status);
        }
        Err(e) => warn!(
            event = "external_service_failure",
            service = "hegel",
            operation = "status_refresh",
            zone_id,
            error_kind = error_kind(&e),
            error = %sanitize_error(&e),
            "Hegel passive status query failed"
        ),
    }
}

pub fn build_status_response(state: &AppState) -> StatusResponse {
    let zone_id = state.zones().active_zone_id();
    build_status_response_for_zone(state, &zone_id).unwrap_or_else(|_| {
        build_status_response_for_player(state, state.zones().active_player(), zone_id)
    })
}

fn status_source_for_file_name(
    state: &AppState,
    zone_id: &str,
    file_name: &str,
) -> Option<SourceRef> {
    state
        .listening()
        .source_for_file_name(state.library(), zone_id, file_name)
        .or_else(|| {
            state
                .library()
                .track_id_for_file_name(file_name)
                .ok()
                .flatten()
                .and_then(|track_id| {
                    state
                        .library()
                        .source_ref_for_track_id(track_id)
                        .ok()
                        .flatten()
                })
        })
}

pub fn build_status_response_for_zone(
    state: &AppState,
    zone_id: &str,
) -> Result<StatusResponse, String> {
    if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::SonosUpnp) {
        let player = state
            .zones()
            .player_for_zone(zone_id)
            .unwrap_or_else(|| state.zones().active_player());
        Ok(build_status_response_for_sonos(
            state,
            player,
            zone_id.to_string(),
        ))
    } else if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::UpnpAvRenderer) {
        let player = state
            .zones()
            .player_for_zone(zone_id)
            .unwrap_or_else(|| state.zones().active_player());
        Ok(build_status_response_for_upnp(
            state,
            player,
            zone_id.to_string(),
        ))
    } else if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::RemoteAgent) {
        Ok(build_status_response_for_player(
            state,
            state.zones().active_player(),
            zone_id.to_string(),
        ))
    } else if let Some(player) = state.zones().player_for_zone(zone_id) {
        Ok(build_status_response_for_player(
            state,
            player,
            zone_id.to_string(),
        ))
    } else {
        Err(format!("Zone '{zone_id}' is not available"))
    }
}

fn build_status_response_for_sonos(
    state: &AppState,
    player: Arc<Player>,
    zone_id: String,
) -> StatusResponse {
    let mut response = build_status_response_for_player(state, player, zone_id.clone());
    response.zone_protocol = SinkProtocol::SonosUpnp;
    response.output_transport = "sonos_upnp".to_string();
    response.exclusive = false;
    response.device_volume_supported = true;
    response.device_volume_max = None;
    if let Some(snapshot) = state.sonos().snapshot(&zone_id) {
        response.state = snapshot.state;
        response.file_name = snapshot.file_name;
        response.current_source = response
            .file_name
            .as_deref()
            .and_then(|file_name| status_source_for_file_name(state, &zone_id, file_name));
        response.track_title = snapshot.track_title;
        response.track_artist = snapshot.track_artist;
        response.track_album = snapshot.track_album;
        response.source_rate = snapshot.source_rate;
        response.target_rate = snapshot.target_rate;
        response.source_bits = snapshot.source_bits;
        response.target_bits = snapshot.target_bits;
        response.position_secs = snapshot.position_secs;
        response.duration_secs = snapshot.duration_secs;
        response.device_volume = snapshot.volume;
        response.volume = snapshot.volume.unwrap_or(response.volume);
        response.output_notice = snapshot.notice;
    } else {
        clear_live_playback_identity(&mut response, false);
    }
    response
}

fn build_status_response_for_upnp(
    state: &AppState,
    player: Arc<Player>,
    zone_id: String,
) -> StatusResponse {
    let mut response = build_status_response_for_player(state, player, zone_id.clone());
    response.zone_protocol = SinkProtocol::UpnpAvRenderer;
    response.output_transport = "upnp_av_renderer".to_string();
    response.exclusive = false;
    response.device_volume_supported = true;
    response.device_volume_max = None;
    if let Some(snapshot) = state.upnp().snapshot(&zone_id) {
        let file_name = snapshot.file_name;
        response.state = snapshot.state;
        response.current_source = snapshot.current_source.or_else(|| {
            file_name
                .as_deref()
                .and_then(|file_name| status_source_for_file_name(state, &zone_id, file_name))
        });
        response.file_name = file_name;
        response.track_title = snapshot.track_title;
        response.track_artist = snapshot.track_artist;
        response.track_album = snapshot.track_album;
        response.source_rate = snapshot.source_rate;
        response.target_rate = snapshot.target_rate;
        response.source_bits = snapshot.source_bits;
        response.target_bits = snapshot.target_bits;
        if let Some(active_output_mode) = snapshot.active_output_mode {
            response.active_output_mode = active_output_mode;
        }
        response.position_secs = snapshot.position_secs;
        response.duration_secs = snapshot.duration_secs;
        response.device_volume = snapshot.volume;
        response.volume = snapshot.volume.unwrap_or(response.volume);
        response.playback_speed = snapshot.playback_speed;
        response.output_notice = snapshot.notice;
        response.upnp_config_applied_to_current_playback =
            snapshot.config_applied_to_current_playback;
        response.upnp_restart_pending = snapshot.restart_pending;
        response.upnp_render_status = snapshot.render_status;
        response.upnp_active_render_signature = snapshot.active_render_signature;
        response.upnp_configured_render_signature = snapshot.configured_render_signature;
        response.upnp_last_render_ms = snapshot.last_render_ms;
        response.upnp_last_prepare_ms = snapshot.last_prepare_ms;
        response.upnp_last_cache_hit = snapshot.last_cache_hit;
        response.transport_pending = snapshot.transport_pending;
        response.transport_pending_position_secs = snapshot.transport_pending_position_secs;
    } else {
        clear_live_playback_identity(&mut response, false);
    }
    response
}

fn clear_live_playback_identity(response: &mut StatusResponse, preserve_timeline: bool) {
    response.state = "Stopped".to_string();
    response.file_name = None;
    response.current_source = None;
    response.track_title = None;
    response.track_artist = None;
    response.track_album = None;
    response.cover_version = 0;
    response.source_rate = 0;
    response.target_rate = 0;
    response.source_bits = 0;
    response.target_bits = 0;
    response.transport_pending = "none".to_string();
    response.transport_pending_position_secs = None;
    response.src_path_kind = None;
    response.src_capped_fallback = false;
    response.src_phase_profile_preserved = true;
    response.src_ratio_num = 0;
    response.src_ratio_den = 0;
    if !preserve_timeline {
        response.position_secs = 0.0;
        response.duration_secs = 0.0;
    }
    response.dsd_buffer_health = None;
    apply_dop_debug_fields(response, DopDebugFields::default());
    response.output_ring_fill_now_ms = 0.0;
    response.output_ring_fill_min_ms = 0.0;
    response.startup_ring_low_watermark_ms = 0.0;
    response.startup_ready_ms = 0;
    response.startup_first_render_block_ms = 0.0;
    response.startup_producer_over_budget_count = 0;
    response.startup_callback_gaps_ms = Vec::new();
    response.underrun_count = 0;
    response.producer_over_budget_count = 0;
    response.max_render_block_ms = 0.0;
    response.max_audio_callback_gap_ms = 0.0;
    response.dsp_graph_rebuild_count = 0;
    response.sample_rate_change_count = 0;
    response.dop_alignment_reset_count = 0;
    response.coreaudio_dop_open_count = 0;
    response.coreaudio_dop_start_count = 0;
    response.coreaudio_dop_stop_count = 0;
    response.coreaudio_dop_drop_count = 0;
    response.coreaudio_dop_quiesce_count = 0;
    response.coreaudio_dop_last_lifecycle_event_id = 0;
    response.coreaudio_dop_last_lifecycle_at_ms = 0;
    response.reopen_reason_count = 0;
    response.last_reopen_reason_id = 0;
    response.last_reopen_reason_at_ms = 0;
    response.flush_reason_count = 0;
    response.last_flush_reason_id = 0;
    response.last_flush_reason_at_ms = 0;
    response.modulator_reset_count = 0;
    response.decoder_starved_count = 0;
    response.lock_wait_max_ms = 0.0;
    response.signal_peak = 0.0;
    response.signal_peak_max = 0.0;
    response.signal_clipping = false;
    response.signal_clip_events = 0;
    response.signal_clip_samples = 0;
    response.dsd_limiter_peak_ratio = 0.0;
    response.dsd_limiter_peak_ratio_max = 0.0;
    response.dsd_limiter_active = false;
    response.dsd_limiter_events = 0;
    response.dsd_limiter_samples = 0;
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::audio::player::PlaybackState;
    #[cfg(feature = "hegel")]
    use crate::library::{ZoneHegelSettings, ZoneSettings};
    use crate::playback::apply_settings::apply_playback_settings_for_zone;
    use crate::playback::test_support::{agent_capabilities, app_state, qobuz_source};
    use crate::protocol::AgentPlaybackState;
    #[cfg(feature = "hegel")]
    use crate::services::hegel::HegelStatus;

    #[test]
    fn stopped_local_status_does_not_report_default_signal_path() {
        let state = app_state("stopped-local-signal-path");

        let status = build_status_response(&state);

        assert_eq!(status.state, "Stopped");
        assert!(status.file_name.is_none());
        assert!(status.current_source.is_none());
        assert_eq!(status.source_rate, 0);
        assert_eq!(status.target_rate, 0);
        assert_eq!(status.source_bits, 0);
        assert_eq!(status.target_bits, 0);
        assert!(status.dsd_buffer_health.is_none());
    }

    #[cfg(feature = "hegel")]
    #[test]
    fn per_zone_hegel_assignment_reports_hegel_device_volume() {
        let state = app_state("status-zone-hegel-volume");
        let zone_id = state.zones().active_zone_id();
        let zone = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("active zone should exist");
        state
            .library()
            .upsert_zone_definition(
                &zone.id,
                &zone.name,
                "local_coreaudio",
                zone.device_name.as_deref(),
                zone.enabled,
            )
            .unwrap();
        state
            .library()
            .set_zone_settings(
                &zone_id,
                ZoneSettings {
                    device_type: Some("hegel".to_string()),
                    hegel: Some(ZoneHegelSettings {
                        host: Some("10.200.0.166".to_string()),
                        max_volume: 50,
                        ..ZoneHegelSettings::default()
                    }),
                    ..ZoneSettings::default()
                },
            )
            .unwrap();
        state.hegel_status().remember(HegelStatus {
            volume: Some(44),
            ..HegelStatus::default()
        });

        let status = build_status_response(&state);

        assert!(status.device_volume_supported);
        assert_eq!(status.device_volume, Some(0.44));
        assert_eq!(status.device_volume_max, Some(0.5));
        assert_eq!(
            status.device_volume_message.as_deref(),
            Some("Hegel volume; max 50%")
        );
    }

    #[test]
    fn coreaudio_dop_status_reports_live_buffer_health() {
        let state = app_state("coreaudio-dop-buffer-health");
        let player = state.zones().active_player();
        player.set_playback_state_for_test(PlaybackState::Playing);
        player.set_coreaudio_dop_buffer_health_for_test(
            11_289_600, 1_411_200, 705_600, 352_800, 8192,
        );

        let status = build_status_response(&state);
        let health = status
            .dsd_buffer_health
            .expect("CoreAudio DoP status should include buffer health");

        assert_eq!(health.ring_capacity_samples, 1_411_200);
        assert_eq!(health.ring_fill_samples, 705_600);
        assert_eq!(health.ring_low_watermark_samples, 352_800);
        assert!((health.ring_capacity_ms - 1000.0).abs() < 0.001);
        assert!((health.ring_fill_ms - 500.0).abs() < 0.001);
        assert!((health.ring_low_watermark_ms - 250.0).abs() < 0.001);
        assert!((status.output_ring_fill_now_ms - 500.0).abs() < 0.001);
        assert!((status.output_ring_fill_min_ms - 250.0).abs() < 0.001);
        assert_eq!(health.hardware_buffer_min_frames, 4096);
        assert_eq!(health.hardware_buffer_max_frames, 16_384);
        assert_eq!(health.hardware_buffer_frames, 8192);
        assert!((status.dop_ring_capacity_ms - 1000.0).abs() < 0.001);
        assert!((status.dop_ring_fill_ms - 500.0).abs() < 0.001);
        assert!((status.dop_ring_low_watermark_ms - 250.0).abs() < 0.001);
        assert_eq!(status.dop_callback_frames, 8192);
        assert!((status.dop_callback_ms - 11.609977).abs() < 0.001);
        assert_eq!(status.dop_hardware_buffer_min_frames, 4096);
        assert_eq!(status.dop_hardware_buffer_max_frames, 16_384);
        assert_eq!(status.dop_hardware_buffer_frames, 8192);
        assert!((status.dop_hardware_buffer_ms - 11.609977).abs() < 0.001);
    }

    #[test]
    fn coreaudio_dop_unmeasured_low_watermark_uses_current_fill() {
        let state = app_state("coreaudio-dop-buffer-health-unmeasured-low");
        let player = state.zones().active_player();
        player.set_playback_state_for_test(PlaybackState::Playing);
        player.set_coreaudio_dop_buffer_health_for_test(
            11_289_600,
            1_411_200,
            705_600,
            u64::MAX,
            8192,
        );

        let status = build_status_response(&state);
        let health = status
            .dsd_buffer_health
            .expect("CoreAudio DoP status should include buffer health");

        assert_eq!(health.ring_low_watermark_samples, 705_600);
        assert!((health.ring_low_watermark_ms - 500.0).abs() < 0.001);
        assert!((status.output_ring_fill_min_ms - 500.0).abs() < 0.001);
    }

    #[test]
    fn diagnostics_status_exposes_exact_field_names() {
        let state = app_state("diagnostics-status-fields");
        let player = state.zones().active_player();
        player.set_playback_state_for_test(PlaybackState::Playing);
        player.set_coreaudio_dop_buffer_health_for_test(
            11_289_600, 1_411_200, 705_600, 352_800, 8192,
        );
        player.set_playback_diagnostics_for_test();

        let status = build_status_response(&state);
        let json = serde_json::to_value(&status).expect("status serializes");

        assert_eq!(json["output_ring_fill_now_ms"], serde_json::json!(500.0));
        assert_eq!(json["output_ring_fill_min_ms"], serde_json::json!(250.0));
        assert_eq!(json["dop_ring_capacity_ms"], serde_json::json!(1000.0));
        assert_eq!(json["dop_ring_fill_ms"], serde_json::json!(500.0));
        assert_eq!(json["dop_ring_low_watermark_ms"], serde_json::json!(250.0));
        assert_eq!(json["dop_callback_frames"], serde_json::json!(8192));
        assert!(
            (json["dop_callback_ms"].as_f64().expect("dop_callback_ms") - 11.609977).abs() < 0.001
        );
        assert_eq!(json["dop_hardware_buffer_frames"], serde_json::json!(8192));
        assert_eq!(
            json["dop_hardware_buffer_min_frames"],
            serde_json::json!(4096)
        );
        assert_eq!(
            json["dop_hardware_buffer_max_frames"],
            serde_json::json!(16_384)
        );
        assert!(
            (json["dop_hardware_buffer_ms"]
                .as_f64()
                .expect("dop_hardware_buffer_ms")
                - 11.609977)
                .abs()
                < 0.001
        );
        assert_eq!(json["dop_lock_miss_events"], serde_json::json!(10));
        assert_eq!(
            json["dop_callback_deadline_miss_events"],
            serde_json::json!(11)
        );
        assert_eq!(
            json["dop_soft_callback_gap_125_events"],
            serde_json::json!(15)
        );
        assert_eq!(
            json["dop_soft_callback_gap_150_events"],
            serde_json::json!(16)
        );
        assert_eq!(
            json["dop_soft_callback_gap_175_events"],
            serde_json::json!(17)
        );
        assert_eq!(
            json["dop_last_soft_callback_gap_ms"],
            serde_json::json!(12.5)
        );
        assert_eq!(
            json["dop_last_soft_callback_gap_at_ms"],
            serde_json::json!(1_765_000_000_100_u64)
        );
        assert_eq!(json["dop_ring_below_250ms_events"], serde_json::json!(18));
        assert_eq!(json["dop_ring_below_100ms_events"], serde_json::json!(19));
        assert_eq!(json["dop_ring_below_50ms_events"], serde_json::json!(20));
        assert_eq!(
            json["dop_ring_below_callback_events"],
            serde_json::json!(21)
        );
        assert_eq!(
            json["dop_last_ring_pressure_at_ms"],
            serde_json::json!(1_765_000_000_200_u64)
        );
        assert_eq!(json["dop_marker_error_events"], serde_json::json!(12));
        assert_eq!(
            json["dop_program_idle_splice_events"],
            serde_json::json!(13)
        );
        assert_eq!(json["dop_program_to_idle_events"], serde_json::json!(22));
        assert_eq!(json["dop_idle_to_program_events"], serde_json::json!(23));
        assert_eq!(json["dop_mixed_output_events"], serde_json::json!(24));
        assert_eq!(json["dop_last_output_transition_id"], serde_json::json!(2));
        assert_eq!(
            json["dop_last_output_transition_at_ms"],
            serde_json::json!(1_765_000_000_300_u64)
        );
        assert_eq!(json["dop_repeated_payload_events"], serde_json::json!(14));
        assert_eq!(
            json["dop_last_underrun_at_ms"],
            serde_json::json!(1_765_000_000_000_u64)
        );
        assert_eq!(json["underrun_count"], serde_json::json!(3));
        assert_eq!(json["producer_over_budget_count"], serde_json::json!(4));
        assert_eq!(json["max_render_block_ms"], serde_json::json!(2.5));
        assert_eq!(json["max_audio_callback_gap_ms"], serde_json::json!(3.75));
        assert_eq!(json["dsp_graph_rebuild_count"], serde_json::json!(5));
        assert_eq!(json["sample_rate_change_count"], serde_json::json!(6));
        assert_eq!(json["dop_alignment_reset_count"], serde_json::json!(7));
        assert_eq!(json["coreaudio_dop_open_count"], serde_json::json!(1));
        assert_eq!(json["coreaudio_dop_start_count"], serde_json::json!(1));
        assert_eq!(json["reopen_reason_count"], serde_json::json!(1));
        assert_eq!(json["last_reopen_reason_id"], serde_json::json!(3));
        assert_eq!(json["flush_reason_count"], serde_json::json!(1));
        assert_eq!(json["last_flush_reason_id"], serde_json::json!(1));
        assert_eq!(json["modulator_reset_count"], serde_json::json!(8));
        assert_eq!(json["decoder_starved_count"], serde_json::json!(9));
        assert_eq!(json["source_read_time_ms"], serde_json::json!(12.0));
        assert_eq!(json["max_source_read_ms"], serde_json::json!(12.0));
        assert_eq!(json["source_read_stall_count"], serde_json::json!(1));
        assert!(
            json["source_read_stall_last_at_ms"]
                .as_u64()
                .expect("source_read_stall_last_at_ms")
                > 0
        );
        assert_eq!(json["decoder_decode_time_ms"], serde_json::json!(15.0));
        assert_eq!(json["max_decoder_decode_ms"], serde_json::json!(15.0));
        assert_eq!(json["decoder_decode_stall_count"], serde_json::json!(1));
        assert!(
            json["decoder_decode_stall_last_at_ms"]
                .as_u64()
                .expect("decoder_decode_stall_last_at_ms")
                > 0
        );
        assert_eq!(json["lock_wait_max_ms"], serde_json::json!(1.25));
    }

    #[test]
    fn signal_path_diagnostics_default_when_missing() {
        let signal: SyncSignalPath = serde_json::from_value(serde_json::json!({
            "source_rate": 0,
            "source_bit_depth": 0,
            "dsp_filter": "",
            "dsp_target_rate": 0,
            "output_rate": 0,
            "output_bit_depth": 0,
            "exclusive": false,
            "cpu_percent": 0.0,
            "resample_time_ns": 0,
            "block_duration_ns": 0,
            "underrun_events": 0,
            "underrun_samples": 0
        }))
        .expect("defaulted diagnostic fields");

        assert_eq!(signal.output_ring_fill_now_ms, 0.0);
        assert_eq!(signal.output_ring_fill_min_ms, 0.0);
        assert_eq!(signal.dop_ring_capacity_ms, 0.0);
        assert_eq!(signal.dop_ring_fill_ms, 0.0);
        assert_eq!(signal.dop_ring_low_watermark_ms, 0.0);
        assert_eq!(signal.dop_callback_frames, 0);
        assert_eq!(signal.dop_callback_ms, 0.0);
        assert_eq!(signal.dop_hardware_buffer_min_frames, 0);
        assert_eq!(signal.dop_hardware_buffer_max_frames, 0);
        assert_eq!(signal.dop_hardware_buffer_frames, 0);
        assert_eq!(signal.dop_hardware_buffer_ms, 0.0);
        assert_eq!(signal.dop_lock_miss_events, 0);
        assert_eq!(signal.dop_callback_deadline_miss_events, 0);
        assert_eq!(signal.dop_soft_callback_gap_125_events, 0);
        assert_eq!(signal.dop_soft_callback_gap_150_events, 0);
        assert_eq!(signal.dop_soft_callback_gap_175_events, 0);
        assert_eq!(signal.dop_last_soft_callback_gap_ms, 0.0);
        assert_eq!(signal.dop_last_soft_callback_gap_at_ms, 0);
        assert_eq!(signal.dop_ring_below_250ms_events, 0);
        assert_eq!(signal.dop_ring_below_100ms_events, 0);
        assert_eq!(signal.dop_ring_below_50ms_events, 0);
        assert_eq!(signal.dop_ring_below_callback_events, 0);
        assert_eq!(signal.dop_last_ring_pressure_at_ms, 0);
        assert_eq!(signal.dop_marker_error_events, 0);
        assert_eq!(signal.dop_program_idle_splice_events, 0);
        assert_eq!(signal.dop_program_to_idle_events, 0);
        assert_eq!(signal.dop_idle_to_program_events, 0);
        assert_eq!(signal.dop_mixed_output_events, 0);
        assert_eq!(signal.dop_last_output_transition_id, 0);
        assert_eq!(signal.dop_last_output_transition_at_ms, 0);
        assert_eq!(signal.dop_repeated_payload_events, 0);
        assert_eq!(signal.dop_last_underrun_at_ms, 0);
        assert_eq!(signal.underrun_count, 0);
        assert_eq!(signal.producer_over_budget_count, 0);
        assert_eq!(signal.max_render_block_ms, 0.0);
        assert_eq!(signal.max_audio_callback_gap_ms, 0.0);
        assert_eq!(signal.dsp_graph_rebuild_count, 0);
        assert_eq!(signal.sample_rate_change_count, 0);
        assert_eq!(signal.dop_alignment_reset_count, 0);
        assert_eq!(signal.coreaudio_dop_open_count, 0);
        assert_eq!(signal.coreaudio_dop_start_count, 0);
        assert_eq!(signal.coreaudio_dop_stop_count, 0);
        assert_eq!(signal.coreaudio_dop_drop_count, 0);
        assert_eq!(signal.coreaudio_dop_quiesce_count, 0);
        assert_eq!(signal.coreaudio_dop_last_lifecycle_event_id, 0);
        assert_eq!(signal.coreaudio_dop_last_lifecycle_at_ms, 0);
        assert_eq!(signal.reopen_reason_count, 0);
        assert_eq!(signal.last_reopen_reason_id, 0);
        assert_eq!(signal.last_reopen_reason_at_ms, 0);
        assert_eq!(signal.flush_reason_count, 0);
        assert_eq!(signal.last_flush_reason_id, 0);
        assert_eq!(signal.last_flush_reason_at_ms, 0);
        assert_eq!(signal.modulator_reset_count, 0);
        assert_eq!(signal.decoder_starved_count, 0);
        assert_eq!(signal.source_read_time_ms, 0.0);
        assert_eq!(signal.max_source_read_ms, 0.0);
        assert_eq!(signal.source_read_stall_count, 0);
        assert_eq!(signal.source_read_stall_last_at_ms, 0);
        assert_eq!(signal.decoder_decode_time_ms, 0.0);
        assert_eq!(signal.max_decoder_decode_ms, 0.0);
        assert_eq!(signal.decoder_decode_stall_count, 0);
        assert_eq!(signal.decoder_decode_stall_last_at_ms, 0);
        assert_eq!(signal.lock_wait_max_ms, 0.0);
    }

    #[test]
    fn remote_agent_status_does_not_reuse_local_cover_version() {
        let state = app_state("remote-agent-cover-version");
        let local_player = state.zones().active_player();
        local_player.set_cover_version_for_test(42);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        state.zones().select_zone(&zone_id).unwrap();
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Playing".to_string(),
                current_source: None,
                file_name: Some("remote-track".to_string()),
                track_title: Some("Remote Song".to_string()),
                track_artist: Some("Remote Artist".to_string()),
                track_album: Some("Remote Album".to_string()),
                source_rate: 44_100,
                target_rate: 44_100,
                source_bits: 16,
                target_bits: 24,
                duration_secs: 180.0,
                position_secs: 1.0,
                volume: 1.0,
            },
            "http://core.test",
        );

        let status = build_status_response(&state);

        assert_eq!(status.zone_protocol, SinkProtocol::RemoteAgent);
        assert_eq!(status.track_title.as_deref(), Some("Remote Song"));
        assert_eq!(status.cover_version, 0);
    }

    #[test]
    fn remote_agent_status_prefers_reported_source_identity() {
        let state = app_state("remote-agent-exact-source");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-exact".to_string(),
            "Exact Agent".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Exact Agent"))
            .unwrap()
            .id;
        state.zones().select_zone(&zone_id).unwrap();
        let exact = qobuz_source(202, false);
        state.zones().update_playback(
            "agent-exact",
            AgentPlaybackState {
                state: "Playing".to_string(),
                current_source: Some(exact.clone()),
                file_name: Some("duplicate display title".to_string()),
                track_title: Some("Track 202".to_string()),
                duration_secs: 180.0,
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );

        let status = build_status_response(&state);

        assert_eq!(
            status.current_source.map(|source| source.key()),
            Some(exact.key())
        );
    }

    #[test]
    fn remote_agent_without_playback_snapshot_does_not_reuse_local_status() {
        let state = app_state("remote-agent-no-playback");
        let local_player = state.zones().active_player();
        local_player.set_playback_state_for_test(PlaybackState::Playing);
        local_player.set_cover_version_for_test(42);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        state.zones().select_zone(&zone_id).unwrap();

        let status = build_status_response(&state);

        assert_eq!(status.zone_protocol, SinkProtocol::RemoteAgent);
        assert_eq!(status.state, "Stopped");
        assert!(status.file_name.is_none());
        assert!(status.current_source.is_none());
        assert_eq!(status.cover_version, 0);
    }

    #[test]
    fn remote_agent_status_reports_saved_dsd_modulator_without_playback() {
        let state = app_state("remote-agent-saved-dsd-modulator");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        state.zones().select_zone(&zone_id).unwrap();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.dsd_modulator = Some("EC depth 2".to_string());
            });

        let status = build_status_response(&state);

        assert_eq!(status.zone_protocol, SinkProtocol::RemoteAgent);
        assert_eq!(status.dsd_modulator, "EcDepth2");
    }

    #[test]
    fn disabled_local_dsp_status_reports_saved_requested_settings() {
        let state = app_state("local-disabled-dsp-saved-settings");
        let zone_id = state.zones().active_zone_id();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.filter_type = Some("Minimum16k".to_string());
                settings.target_rate = Some(384_000);
                settings.upsampling_enabled = Some(false);
                settings.exclusive = Some(true);
                settings.dither_mode = Some("TPDF".to_string());
                settings.output_mode = Some("Dsd256".to_string());
                settings.dsd_modulator = Some("EC depth 2".to_string());
                settings.dsd_isi_penalty = Some(0.012);
                settings.dsd_rules_enabled = true;
                settings.dsd_rules = vec![DsdSourceRule {
                    source_rate: 44_100,
                    filter_type: "Split16k".to_string(),
                    output_mode: "Dsd256".to_string(),
                }];
                settings.headroom_db = Some(-6.0);
                settings.dsp_buffer_ms = Some(250);
            });
        apply_playback_settings_for_zone(&state, &zone_id);

        let status = build_status_response(&state);

        assert!(!status.upsampling_enabled);
        assert_eq!(status.filter_type, "Minimum16k");
        assert_eq!(status.configured_target_rate, 384_000);
        assert_eq!(status.dither_mode, "Auto");
        assert_eq!(status.output_mode, "Dsd256");
        assert_eq!(status.active_output_mode, "Pcm");
        assert_eq!(status.dsd_modulator, "EcDepth2");
        assert_eq!(status.dsd_isi_penalty, 0.012);
        assert!(status.dsd_rules_enabled);
        assert_eq!(status.dsd_rules.len(), 1);
        assert_eq!(status.headroom_db, -6.0);
        assert_eq!(status.dsp_buffer_ms, 250);
        assert!(status.exclusive);
    }

    #[test]
    fn disabled_remote_dsp_status_keeps_saved_requested_settings() {
        let state = app_state("remote-disabled-dsp-saved-settings");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        state.zones().select_zone(&zone_id).unwrap();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.filter_type = Some("Minimum16k".to_string());
                settings.upsampling_enabled = Some(false);
                settings.output_mode = Some("Dsd256".to_string());
                settings.dsd_modulator = Some("EC depth 2".to_string());
            });
        state.zones().update_signal_path(
            "agent-1",
            SyncSignalPath {
                dsp_filter: "Split16k".to_string(),
                output_mode: Some("Pcm".to_string()),
                active_output_mode: Some("Pcm".to_string()),
                dsd_modulator: Some("EcDepth2".to_string()),
                ..SyncSignalPath::default()
            },
        );

        let status = build_status_response(&state);

        assert!(!status.upsampling_enabled);
        assert_eq!(status.filter_type, "Minimum16k");
        assert_eq!(status.output_mode, "Dsd256");
        assert_eq!(status.active_output_mode, "Pcm");
        assert_eq!(status.dsd_modulator, "EcDepth2");
    }

    #[test]
    fn remote_agent_status_reports_live_dsd_modulator_from_signal_path() {
        let state = app_state("remote-agent-live-dsd-modulator");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        state.zones().select_zone(&zone_id).unwrap();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
            });
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Playing".to_string(),
                file_name: Some("remote-track".to_string()),
                source_rate: 44_100,
                target_rate: 2_822_400,
                source_bits: 16,
                target_bits: 1,
                duration_secs: 180.0,
                position_secs: 1.0,
                volume: 1.0,
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );
        state.zones().update_signal_path(
            "agent-1",
            SyncSignalPath {
                dsd_modulator: Some("Standard".to_string()),
                ..SyncSignalPath::default()
            },
        );

        let status = build_status_response(&state);

        assert_eq!(status.zone_protocol, SinkProtocol::RemoteAgent);
        assert_eq!(status.dsd_modulator, "Standard");
    }

    #[test]
    fn remote_agent_status_reports_live_diagnostics_from_signal_path() {
        let state = app_state("remote-agent-live-diagnostics");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        state.zones().select_zone(&zone_id).unwrap();
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Playing".to_string(),
                file_name: Some("remote-track".to_string()),
                source_rate: 44_100,
                target_rate: 2_822_400,
                source_bits: 16,
                target_bits: 1,
                duration_secs: 180.0,
                position_secs: 1.0,
                volume: 1.0,
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );
        state.zones().update_signal_path(
            "agent-1",
            SyncSignalPath {
                dop_ring_capacity_ms: 1000.0,
                dop_ring_fill_ms: 10.0,
                dop_ring_low_watermark_ms: 5.0,
                dop_callback_frames: 4096,
                dop_callback_ms: 5.8,
                dop_hardware_buffer_min_frames: 512,
                dop_hardware_buffer_max_frames: 16_384,
                dop_hardware_buffer_frames: 4096,
                dop_hardware_buffer_ms: 5.8,
                dop_lock_miss_events: 10,
                dop_callback_deadline_miss_events: 11,
                dop_soft_callback_gap_125_events: 12,
                dop_soft_callback_gap_150_events: 13,
                dop_soft_callback_gap_175_events: 14,
                dop_last_soft_callback_gap_ms: 12.5,
                dop_last_soft_callback_gap_at_ms: 16,
                dop_ring_below_250ms_events: 17,
                dop_ring_below_100ms_events: 18,
                dop_ring_below_50ms_events: 19,
                dop_ring_below_callback_events: 20,
                dop_last_ring_pressure_at_ms: 21,
                dop_marker_error_events: 12,
                dop_program_idle_splice_events: 13,
                dop_repeated_payload_events: 14,
                dop_last_underrun_at_ms: 15,
                output_ring_fill_now_ms: 10.0,
                output_ring_fill_min_ms: 5.0,
                underrun_count: 1,
                producer_over_budget_count: 2,
                max_render_block_ms: 3.0,
                max_audio_callback_gap_ms: 4.0,
                dsp_graph_rebuild_count: 5,
                sample_rate_change_count: 6,
                dop_alignment_reset_count: 7,
                modulator_reset_count: 8,
                decoder_starved_count: 9,
                source_read_time_ms: 6.0,
                max_source_read_ms: 7.0,
                source_read_stall_count: 22,
                source_read_stall_last_at_ms: 23,
                decoder_decode_time_ms: 8.0,
                max_decoder_decode_ms: 9.0,
                decoder_decode_stall_count: 24,
                decoder_decode_stall_last_at_ms: 25,
                lock_wait_max_ms: 0.5,
                ..SyncSignalPath::default()
            },
        );

        let status = build_status_response(&state);

        assert_eq!(status.zone_protocol, SinkProtocol::RemoteAgent);
        assert_eq!(status.output_ring_fill_now_ms, 10.0);
        assert_eq!(status.output_ring_fill_min_ms, 5.0);
        assert_eq!(status.dop_ring_capacity_ms, 1000.0);
        assert_eq!(status.dop_ring_fill_ms, 10.0);
        assert_eq!(status.dop_ring_low_watermark_ms, 5.0);
        assert_eq!(status.dop_callback_frames, 4096);
        assert_eq!(status.dop_callback_ms, 5.8);
        assert_eq!(status.dop_hardware_buffer_min_frames, 512);
        assert_eq!(status.dop_hardware_buffer_max_frames, 16_384);
        assert_eq!(status.dop_hardware_buffer_frames, 4096);
        assert_eq!(status.dop_hardware_buffer_ms, 5.8);
        assert_eq!(status.dop_lock_miss_events, 10);
        assert_eq!(status.dop_callback_deadline_miss_events, 11);
        assert_eq!(status.dop_soft_callback_gap_125_events, 12);
        assert_eq!(status.dop_soft_callback_gap_150_events, 13);
        assert_eq!(status.dop_soft_callback_gap_175_events, 14);
        assert_eq!(status.dop_last_soft_callback_gap_ms, 12.5);
        assert_eq!(status.dop_last_soft_callback_gap_at_ms, 16);
        assert_eq!(status.dop_ring_below_250ms_events, 17);
        assert_eq!(status.dop_ring_below_100ms_events, 18);
        assert_eq!(status.dop_ring_below_50ms_events, 19);
        assert_eq!(status.dop_ring_below_callback_events, 20);
        assert_eq!(status.dop_last_ring_pressure_at_ms, 21);
        assert_eq!(status.dop_marker_error_events, 12);
        assert_eq!(status.dop_program_idle_splice_events, 13);
        assert_eq!(status.dop_repeated_payload_events, 14);
        assert_eq!(status.dop_last_underrun_at_ms, 15);
        assert_eq!(status.underrun_count, 1);
        assert_eq!(status.producer_over_budget_count, 2);
        assert_eq!(status.max_render_block_ms, 3.0);
        assert_eq!(status.max_audio_callback_gap_ms, 4.0);
        assert_eq!(status.dsp_graph_rebuild_count, 5);
        assert_eq!(status.sample_rate_change_count, 6);
        assert_eq!(status.dop_alignment_reset_count, 7);
        assert_eq!(status.modulator_reset_count, 8);
        assert_eq!(status.decoder_starved_count, 9);
        assert_eq!(status.source_read_time_ms, 6.0);
        assert_eq!(status.max_source_read_ms, 7.0);
        assert_eq!(status.source_read_stall_count, 22);
        assert_eq!(status.source_read_stall_last_at_ms, 23);
        assert_eq!(status.decoder_decode_time_ms, 8.0);
        assert_eq!(status.max_decoder_decode_ms, 9.0);
        assert_eq!(status.decoder_decode_stall_count, 24);
        assert_eq!(status.decoder_decode_stall_last_at_ms, 25);
        assert_eq!(status.lock_wait_max_ms, 0.5);
    }
}

fn build_status_response_for_player(
    state: &AppState,
    player: Arc<Player>,
    zone_id: String,
) -> StatusResponse {
    let snapshot = player.snapshot();
    let signal = &snapshot.signal_path;
    let config = &snapshot.config;
    let metrics = &snapshot.metrics;
    let diagnostics = &snapshot.diagnostics;
    let play_state = snapshot.state.as_name().to_string();

    let zone_playback_settings = state.settings().playback_for_zone(&zone_id);
    let filter_name = zone_playback_settings
        .filter_type
        .as_deref()
        .and_then(crate::audio::resampler::FilterType::from_name)
        .map(|filter| filter.as_name().to_string())
        .or_else(|| {
            config
                .filter_type
                .map(|filter| filter.as_name().to_string())
        })
        .unwrap_or_else(|| "Unknown".to_string());
    let live_filter_name = config
        .filter_type
        .map(|filter| filter.as_name().to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let active_filter_name = config
        .active_filter_type
        .map(|filter| filter.as_name().to_string())
        .unwrap_or_else(|| live_filter_name.clone());
    let src_path_kind = signal.src_path_kind.map(|kind| kind.as_name().to_string());
    let src_capped_fallback = signal.src_capped_fallback;
    let src_phase_profile_preserved = signal.src_phase_profile_preserved;
    let src_ratio_num = signal.src_ratio_num;
    let src_ratio_den = signal.src_ratio_den;
    let dither_name = "Auto".to_string();
    let output_mode_name = zone_playback_settings
        .output_mode
        .as_deref()
        .and_then(crate::audio::player::OutputMode::from_name)
        .map(|mode| mode.as_name().to_string())
        .unwrap_or_else(|| signal.output_mode.as_name().to_string());
    let active_output_mode_name = signal.active_output_mode.as_name().to_string();
    let dsd_modulator = zone_playback_settings
        .dsd_modulator
        .as_deref()
        .and_then(DsdModulator::from_name)
        .map(|modulator| modulator.as_name().to_string())
        .unwrap_or_else(|| config.dsd_modulator.as_name().to_string());
    let dsp_buffer_ms = zone_playback_settings
        .dsp_buffer_ms
        .unwrap_or(config.dsp_buffer_ms);
    let dsd_isi_penalty = zone_playback_settings
        .dsd_isi_penalty
        .unwrap_or(config.dsd_isi_penalty);
    let headroom_db = zone_playback_settings
        .headroom_db
        .unwrap_or(config.headroom_db);
    let exclusive = zone_playback_settings.exclusive.unwrap_or(config.exclusive);
    let output_transport = signal.output_transport.as_name().to_string();
    let output_notice_id = signal.output_notice_id;
    let output_notice = snapshot.output_notice.clone();
    let dsd_stability_resets = signal.dsd_stability_resets;
    let zone_protocol = state
        .zones()
        .zone_protocol(&zone_id)
        .unwrap_or(SinkProtocol::LocalCoreAudio);
    let dsd_rules_enabled = zone_playback_settings.dsd_rules_enabled;
    let dsd_rules = zone_playback_settings.dsd_rules;

    let src_rate = signal.source_rate;
    let tgt_rate = signal.target_rate;
    let src_bits = signal.source_bits;
    let tgt_bits = signal.target_bits;
    let configured_tgt_rate = zone_playback_settings
        .target_rate
        .unwrap_or(config.configured_target_rate);
    let configured_tgt_bits = match zone_playback_settings.target_bit_depth.unwrap_or(24) {
        bits @ (16 | 24 | 32) => bits,
        _ => 24,
    };
    let upsampling_enabled = zone_playback_settings
        .upsampling_enabled
        .unwrap_or(config.upsampling_enabled);

    let position_secs = if tgt_rate > 0 {
        metrics.position_samples as f64 / tgt_rate as f64
    } else {
        0.0
    };

    let duration_secs = if src_rate > 0 {
        metrics.duration_samples as f64 / src_rate as f64
    } else {
        0.0
    };

    let resample_time_ns = metrics.resample_time_ns;
    let dsd_upsample_time_ns = metrics.dsd_upsample_time_ns;
    let dsd_modulate_time_ns = metrics.dsd_modulate_time_ns;
    let dsd_output_pending_samples = metrics.dsd_output_pending_samples;
    let dsd_buffer_health = metrics.dsd_buffer_health.clone();
    let dop_debug = DopDebugFields::from_health(dsd_buffer_health.as_ref());
    let dsd_overbudget_blocks = metrics.dsd_overbudget_blocks;
    let dsd_last_load = metrics.dsd_last_load;
    let dsd_recent_load_p95 = metrics.dsd_recent_load_p95;
    let dsd_recent_load_p99 = metrics.dsd_recent_load_p99;
    let block_duration_ns = metrics.block_duration_ns;

    let cpu_percent = state.diagnostics().sample_cpu_percent();

    let meter_l = metrics.meter_l;
    let meter_r = metrics.meter_r;
    let signal_peak = metrics.signal_peak;
    let signal_peak_max = metrics.signal_peak_max;
    let signal_clipping = metrics.signal_clipping;
    let signal_clip_events = metrics.signal_clip_events;
    let signal_clip_samples = metrics.signal_clip_samples;
    let dsd_limiter_peak_ratio = metrics.dsd_limiter_peak_ratio;
    let dsd_limiter_peak_ratio_max = metrics.dsd_limiter_peak_ratio_max;
    let dsd_limiter_active = metrics.dsd_limiter_active;
    let dsd_limiter_events = metrics.dsd_limiter_events;
    let dsd_limiter_samples = metrics.dsd_limiter_samples;

    let file_name = snapshot.file_name.clone();
    let tags = snapshot.track_tags.clone();
    let cover_version = snapshot.cover_version;
    let selected_device = snapshot.device_name.clone();
    let active_zone_id = zone_id;
    let active_zone_name = state.zones().zone_name(&active_zone_id);

    // Prefer library DB tags over what Symphonia could read from the file.
    let current_source = file_name
        .as_deref()
        .and_then(|name| status_source_for_file_name(state, &active_zone_id, name));
    let (db_title, db_artist, db_album) = current_source
        .as_ref()
        .and_then(|source| match source {
            SourceRef::LocalTrack { track_id, .. } => {
                state.library().tags_for_track_id(*track_id).ok().flatten()
            }
            SourceRef::QobuzTrack { .. } => None,
        })
        .or_else(|| {
            file_name
                .as_deref()
                .map(|name| state.library().tags_for_file_name(name))
        })
        .unwrap_or((None, None, None));

    let remote = if zone_protocol == SinkProtocol::RemoteAgent {
        state.zones().remote_snapshot_for_zone(&active_zone_id)
    } else {
        None
    };
    let hegel_settings = if cfg!(feature = "hegel") {
        hegel_settings_for_zone(state, &active_zone_id)
    } else {
        None
    };
    let (device_volume_status, device_volume_max) = if let Some(settings) = hegel_settings.as_ref()
    {
        let cached = state.hegel_status().cached();
        (
            device_volume::DeviceVolumeStatus {
                supported: true,
                volume: cached
                    .as_ref()
                    .and_then(|status| status.volume)
                    .map(|volume| volume.min(settings.max_volume) as f32 / 100.0),
                message: Some(format!("Hegel volume; max {}%", settings.max_volume)),
            },
            Some(settings.max_volume as f32 / 100.0),
        )
    } else if zone_protocol == SinkProtocol::RemoteAgent {
        (
            device_volume::DeviceVolumeStatus::unsupported(
                "Device volume for remote agents is not available from this control",
            ),
            None,
        )
    } else if zone_protocol == SinkProtocol::UpnpAvRenderer {
        (
            device_volume::DeviceVolumeStatus {
                supported: true,
                volume: state
                    .upnp()
                    .snapshot(&active_zone_id)
                    .and_then(|s| s.volume),
                message: None,
            },
            None,
        )
    } else if matches!(
        zone_protocol,
        SinkProtocol::AirPlayRaop | SinkProtocol::AirPlay2
    ) {
        let airplay_volume = player.airplay_device_volume();
        (
            device_volume::DeviceVolumeStatus {
                supported: true,
                volume: airplay_volume,
                message: airplay_volume.is_none().then(|| {
                    "AirPlay receiver volume is unknown; current receiver volume will be preserved"
                        .to_string()
                }),
            },
            None,
        )
    } else {
        (
            device_volume::output_device_volume_status(selected_device.as_deref()),
            None,
        )
    };
    let remote_signal_path = remote.as_ref().and_then(|r| r.signal_path.clone());
    let remote_buffer_state = remote.as_ref().and_then(|r| r.buffer.clone());
    let browser_stream_signal = state.zones().browser_stream_signal(&active_zone_id);
    let mut response = StatusResponse {
        surface: "local".to_string(),
        capabilities: BuildCapabilities::current(),
        airplay_helper_state: state.airplay().helper_status().as_str().to_string(),
        state: play_state,
        file_name,
        current_source,
        track_title: db_title.or(tags.title),
        track_artist: db_artist.or(tags.artist),
        track_album: db_album.or(tags.album),
        cover_version,
        source_rate: src_rate,
        target_rate: tgt_rate,
        source_bits: src_bits,
        target_bits: tgt_bits,
        configured_target_rate: configured_tgt_rate,
        configured_target_bit_depth: configured_tgt_bits,
        upsampling_enabled,
        filter_type: filter_name,
        active_filter_type: active_filter_name,
        src_path_kind,
        src_capped_fallback,
        src_phase_profile_preserved,
        src_ratio_num,
        src_ratio_den,
        dither_mode: dither_name,
        output_mode: output_mode_name,
        active_output_mode: active_output_mode_name,
        dsd_modulator,
        dsd_isi_penalty,
        output_transport,
        output_notice_id,
        output_notice,
        upnp_config_applied_to_current_playback: true,
        upnp_restart_pending: false,
        upnp_render_status: "idle".to_string(),
        upnp_active_render_signature: None,
        upnp_configured_render_signature: None,
        upnp_last_render_ms: None,
        upnp_last_prepare_ms: None,
        upnp_last_cache_hit: None,
        transport_pending: "none".to_string(),
        transport_pending_position_secs: None,
        dsd_stability_resets,
        dsd_rules_enabled,
        dsd_rules,
        volume: config.volume,
        device_volume: device_volume_status.volume,
        device_volume_supported: device_volume_status.supported,
        device_volume_max,
        device_volume_message: device_volume_status.message,
        headroom_db,
        dsp_buffer_ms,
        exclusive,
        position_secs,
        duration_secs,
        playback_speed: None,
        resample_time_ns,
        dsd_upsample_time_ns,
        dsd_modulate_time_ns,
        dsd_output_pending_samples,
        dsd_buffer_health,
        dsd_overbudget_blocks,
        dsd_last_load,
        dsd_recent_load_p95,
        dsd_recent_load_p99,
        dop_ring_capacity_ms: dop_debug.ring_capacity_ms,
        dop_ring_fill_ms: dop_debug.ring_fill_ms,
        dop_ring_low_watermark_ms: dop_debug.ring_low_watermark_ms,
        dop_callback_frames: dop_debug.callback_frames,
        dop_callback_ms: dop_debug.callback_ms,
        dop_requested_hardware_buffer_frames: dop_debug.requested_hardware_buffer_frames,
        dop_requested_hardware_buffer_ms: dop_debug.requested_hardware_buffer_ms,
        dop_hardware_buffer_min_frames: dop_debug.hardware_buffer_min_frames,
        dop_hardware_buffer_max_frames: dop_debug.hardware_buffer_max_frames,
        dop_hardware_buffer_frames: dop_debug.hardware_buffer_frames,
        dop_hardware_buffer_ms: dop_debug.hardware_buffer_ms,
        dop_lock_miss_events: dop_debug.lock_miss_events,
        dop_callback_deadline_miss_events: dop_debug.callback_deadline_miss_events,
        dop_soft_callback_gap_125_events: dop_debug.soft_callback_gap_125_events,
        dop_soft_callback_gap_150_events: dop_debug.soft_callback_gap_150_events,
        dop_soft_callback_gap_175_events: dop_debug.soft_callback_gap_175_events,
        dop_last_soft_callback_gap_ms: dop_debug.last_soft_callback_gap_ms,
        dop_last_soft_callback_gap_at_ms: dop_debug.last_soft_callback_gap_at_ms,
        dop_ring_below_250ms_events: dop_debug.ring_below_250ms_events,
        dop_ring_below_100ms_events: dop_debug.ring_below_100ms_events,
        dop_ring_below_50ms_events: dop_debug.ring_below_50ms_events,
        dop_ring_below_callback_events: dop_debug.ring_below_callback_events,
        dop_last_ring_pressure_at_ms: dop_debug.last_ring_pressure_at_ms,
        dop_marker_error_events: dop_debug.marker_error_events,
        dop_program_idle_splice_events: dop_debug.program_idle_splice_events,
        dop_program_to_idle_events: dop_debug.program_to_idle_events,
        dop_idle_to_program_events: dop_debug.idle_to_program_events,
        dop_mixed_output_events: dop_debug.mixed_output_events,
        dop_last_output_transition_id: dop_debug.last_output_transition_id,
        dop_last_output_transition_at_ms: dop_debug.last_output_transition_at_ms,
        dop_repeated_payload_events: dop_debug.repeated_payload_events,
        dop_callback_index: dop_debug.callback_index,
        dop_last_callback_at_ms: dop_debug.last_callback_at_ms,
        dop_last_callback_gap_ms: dop_debug.last_callback_gap_ms,
        dop_last_callback_frames: dop_debug.last_callback_frames,
        dop_last_output_kind_id: dop_debug.last_output_kind_id,
        dop_last_ring_fill_samples: dop_debug.last_ring_fill_samples,
        dop_last_program_read_samples: dop_debug.last_program_read_samples,
        dop_ring_read_cursor_samples: dop_debug.ring_read_cursor_samples,
        dop_last_payload_fingerprint: dop_debug.last_payload_fingerprint,
        dop_last_payload_fingerprint_at_ms: dop_debug.last_payload_fingerprint_at_ms,
        dop_marker_scan_count: dop_debug.marker_scan_count,
        dop_every_callback_scan_enabled: dop_debug.every_callback_scan_enabled,
        dop_last_underrun_at_ms: dop_debug.last_underrun_at_ms,
        output_ring_fill_now_ms: diagnostics.output_ring_fill_now_ms,
        output_ring_fill_min_ms: diagnostics.output_ring_fill_min_ms,
        startup_ring_low_watermark_ms: diagnostics.startup_ring_low_watermark_ms,
        startup_ready_ms: diagnostics.startup_ready_ms,
        startup_first_render_block_ms: diagnostics.startup_first_render_block_ms,
        startup_producer_over_budget_count: diagnostics.startup_producer_over_budget_count,
        startup_callback_gaps_ms: diagnostics.startup_callback_gaps_ms.clone(),
        underrun_count: diagnostics.underrun_count,
        producer_over_budget_count: diagnostics.producer_over_budget_count,
        max_render_block_ms: diagnostics.max_render_block_ms,
        max_audio_callback_gap_ms: diagnostics.max_audio_callback_gap_ms,
        dsp_graph_rebuild_count: diagnostics.dsp_graph_rebuild_count,
        sample_rate_change_count: diagnostics.sample_rate_change_count,
        dop_alignment_reset_count: diagnostics.dop_alignment_reset_count,
        coreaudio_dop_open_count: diagnostics.coreaudio_dop_open_count,
        coreaudio_dop_start_count: diagnostics.coreaudio_dop_start_count,
        coreaudio_dop_stop_count: diagnostics.coreaudio_dop_stop_count,
        coreaudio_dop_drop_count: diagnostics.coreaudio_dop_drop_count,
        coreaudio_dop_quiesce_count: diagnostics.coreaudio_dop_quiesce_count,
        coreaudio_dop_last_lifecycle_event_id: diagnostics.coreaudio_dop_last_lifecycle_event_id,
        coreaudio_dop_last_lifecycle_at_ms: diagnostics.coreaudio_dop_last_lifecycle_at_ms,
        reopen_reason_count: diagnostics.reopen_reason_count,
        last_reopen_reason_id: diagnostics.last_reopen_reason_id,
        last_reopen_reason_at_ms: diagnostics.last_reopen_reason_at_ms,
        flush_reason_count: diagnostics.flush_reason_count,
        last_flush_reason_id: diagnostics.last_flush_reason_id,
        last_flush_reason_at_ms: diagnostics.last_flush_reason_at_ms,
        modulator_reset_count: diagnostics.modulator_reset_count,
        decoder_starved_count: diagnostics.decoder_starved_count,
        source_read_time_ms: diagnostics.source_read_time_ms,
        max_source_read_ms: diagnostics.max_source_read_ms,
        source_read_stall_count: diagnostics.source_read_stall_count,
        source_read_stall_last_at_ms: diagnostics.source_read_stall_last_at_ms,
        decoder_decode_time_ms: diagnostics.decoder_decode_time_ms,
        max_decoder_decode_ms: diagnostics.max_decoder_decode_ms,
        decoder_decode_stall_count: diagnostics.decoder_decode_stall_count,
        decoder_decode_stall_last_at_ms: diagnostics.decoder_decode_stall_last_at_ms,
        lock_wait_max_ms: diagnostics.lock_wait_max_ms,
        block_duration_ns,
        cpu_percent,
        meter_l,
        meter_r,
        signal_peak,
        signal_peak_max,
        signal_clipping,
        signal_clip_events,
        signal_clip_samples,
        dsd_limiter_peak_ratio,
        dsd_limiter_peak_ratio_max,
        dsd_limiter_active,
        dsd_limiter_events,
        dsd_limiter_samples,
        underrun_events: metrics.underrun_events,
        underrun_samples: metrics.underrun_samples,
        selected_device,
        active_zone_id: active_zone_id.clone(),
        active_zone_name,
        zone_protocol: zone_protocol.clone(),
        remote_connected: remote.is_some(),
        remote_signal_path,
        remote_buffer_state,
        browser_stream_signal,
    };

    if let Some(remote) = remote.and_then(|r| r.playback) {
        response.state = remote.state;
        response.file_name = remote.file_name;
        response.current_source = remote.current_source.or_else(|| {
            response
                .file_name
                .as_deref()
                .and_then(|file_name| {
                    status_source_for_file_name(state, &active_zone_id, file_name)
                })
                .or_else(|| state.listening().active_source(&active_zone_id))
        });
        response.track_title = remote.track_title;
        response.track_artist = remote.track_artist;
        response.track_album = remote.track_album;
        response.cover_version = 0;
        response.source_rate = remote.source_rate;
        response.target_rate = remote.target_rate;
        response.source_bits = remote.source_bits;
        response.target_bits = remote.target_bits;
        response.position_secs = remote.position_secs;
        response.duration_secs = remote.duration_secs;
        response.volume = remote.volume;
        if let Some(signal) = response.remote_signal_path.clone() {
            if response.upsampling_enabled {
                response.filter_type = signal.dsp_filter.clone();
            }
            response.active_filter_type = signal.dsp_filter.clone();
            response.src_path_kind = signal.src_path_kind.clone();
            response.src_capped_fallback = signal.src_capped_fallback;
            response.src_phase_profile_preserved = signal.src_phase_profile_preserved;
            response.src_ratio_num = signal.src_ratio_num;
            response.src_ratio_den = signal.src_ratio_den;
            if response.upsampling_enabled
                && let Some(output_mode) = &signal.output_mode
            {
                response.output_mode = output_mode.clone();
            }
            if let Some(active_output_mode) = &signal.active_output_mode {
                response.active_output_mode = active_output_mode.clone();
            }
            if let Some(output_transport) = &signal.output_transport {
                response.output_transport = output_transport.clone();
            }
            response.dsd_stability_resets = signal.dsd_stability_resets;
            if response.upsampling_enabled
                && let Some(dsd_modulator) = signal
                    .dsd_modulator
                    .as_deref()
                    .and_then(DsdModulator::from_name)
            {
                response.dsd_modulator = dsd_modulator.as_name().to_string();
            }
            response.resample_time_ns = signal.resample_time_ns;
            response.dsd_upsample_time_ns = signal.dsd_upsample_time_ns;
            response.dsd_modulate_time_ns = signal.dsd_modulate_time_ns;
            response.dsd_output_pending_samples = signal.dsd_output_pending_samples;
            response.dsd_buffer_health = signal.dsd_buffer_health.clone();
            apply_dop_debug_fields(&mut response, DopDebugFields::from_signal(&signal));
            response.dsd_overbudget_blocks = signal.dsd_overbudget_blocks;
            response.dsd_last_load = signal.dsd_last_load;
            response.dsd_recent_load_p95 = signal.dsd_recent_load_p95;
            response.dsd_recent_load_p99 = signal.dsd_recent_load_p99;
            response.output_ring_fill_now_ms = signal.output_ring_fill_now_ms;
            response.output_ring_fill_min_ms = signal.output_ring_fill_min_ms;
            response.startup_ring_low_watermark_ms = signal.startup_ring_low_watermark_ms;
            response.startup_ready_ms = signal.startup_ready_ms;
            response.startup_first_render_block_ms = signal.startup_first_render_block_ms;
            response.startup_producer_over_budget_count = signal.startup_producer_over_budget_count;
            response.startup_callback_gaps_ms = signal.startup_callback_gaps_ms.clone();
            response.underrun_count = signal.underrun_count;
            response.producer_over_budget_count = signal.producer_over_budget_count;
            response.max_render_block_ms = signal.max_render_block_ms;
            response.max_audio_callback_gap_ms = signal.max_audio_callback_gap_ms;
            response.dsp_graph_rebuild_count = signal.dsp_graph_rebuild_count;
            response.sample_rate_change_count = signal.sample_rate_change_count;
            response.dop_alignment_reset_count = signal.dop_alignment_reset_count;
            response.coreaudio_dop_open_count = signal.coreaudio_dop_open_count;
            response.coreaudio_dop_start_count = signal.coreaudio_dop_start_count;
            response.coreaudio_dop_stop_count = signal.coreaudio_dop_stop_count;
            response.coreaudio_dop_drop_count = signal.coreaudio_dop_drop_count;
            response.coreaudio_dop_quiesce_count = signal.coreaudio_dop_quiesce_count;
            response.coreaudio_dop_last_lifecycle_event_id =
                signal.coreaudio_dop_last_lifecycle_event_id;
            response.coreaudio_dop_last_lifecycle_at_ms = signal.coreaudio_dop_last_lifecycle_at_ms;
            response.reopen_reason_count = signal.reopen_reason_count;
            response.last_reopen_reason_id = signal.last_reopen_reason_id;
            response.last_reopen_reason_at_ms = signal.last_reopen_reason_at_ms;
            response.flush_reason_count = signal.flush_reason_count;
            response.last_flush_reason_id = signal.last_flush_reason_id;
            response.last_flush_reason_at_ms = signal.last_flush_reason_at_ms;
            response.modulator_reset_count = signal.modulator_reset_count;
            response.decoder_starved_count = signal.decoder_starved_count;
            response.source_read_time_ms = signal.source_read_time_ms;
            response.max_source_read_ms = signal.max_source_read_ms;
            response.source_read_stall_count = signal.source_read_stall_count;
            response.source_read_stall_last_at_ms = signal.source_read_stall_last_at_ms;
            response.decoder_decode_time_ms = signal.decoder_decode_time_ms;
            response.max_decoder_decode_ms = signal.max_decoder_decode_ms;
            response.decoder_decode_stall_count = signal.decoder_decode_stall_count;
            response.decoder_decode_stall_last_at_ms = signal.decoder_decode_stall_last_at_ms;
            response.lock_wait_max_ms = signal.lock_wait_max_ms;
            response.block_duration_ns = signal.block_duration_ns;
            response.cpu_percent = signal.cpu_percent;
            response.signal_peak = signal.signal_peak;
            response.signal_peak_max = signal.signal_peak_max;
            response.signal_clipping = signal.signal_clipping;
            response.signal_clip_events = signal.signal_clip_events;
            response.signal_clip_samples = signal.signal_clip_samples;
            response.dsd_limiter_peak_ratio = signal.dsd_limiter_peak_ratio;
            response.dsd_limiter_peak_ratio_max = signal.dsd_limiter_peak_ratio_max;
            response.dsd_limiter_active = signal.dsd_limiter_active;
            response.dsd_limiter_events = signal.dsd_limiter_events;
            response.dsd_limiter_samples = signal.dsd_limiter_samples;
            response.underrun_events = signal.underrun_events;
            response.underrun_samples = signal.underrun_samples;
            response.selected_device = signal.output_device.clone();
        }
    } else if zone_protocol == SinkProtocol::RemoteAgent {
        clear_live_playback_identity(&mut response, false);
    }
    if !matches!(response.state.as_str(), "Playing" | "Paused" | "Starting") {
        let preserve_timeline = response.zone_protocol != SinkProtocol::RemoteAgent;
        clear_live_playback_identity(&mut response, preserve_timeline);
    }
    response
}
