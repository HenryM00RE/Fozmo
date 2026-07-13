use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DsdBufferHealth {
    pub ring_capacity_samples: u64,
    pub ring_fill_samples: u64,
    pub ring_low_watermark_samples: u64,
    pub ring_capacity_ms: f64,
    pub ring_fill_ms: f64,
    pub ring_low_watermark_ms: f64,
    pub callback_frames: u32,
    pub callback_ms: f64,
    #[serde(default)]
    pub requested_hardware_buffer_frames: u32,
    #[serde(default)]
    pub requested_hardware_buffer_ms: f64,
    #[serde(default)]
    pub hardware_buffer_min_frames: u32,
    #[serde(default)]
    pub hardware_buffer_max_frames: u32,
    pub hardware_buffer_frames: u32,
    pub hardware_buffer_ms: f64,
    pub lock_miss_events: u64,
    #[serde(default)]
    pub callback_deadline_miss_events: u64,
    #[serde(default)]
    pub soft_callback_gap_125_events: u64,
    #[serde(default)]
    pub soft_callback_gap_150_events: u64,
    #[serde(default)]
    pub soft_callback_gap_175_events: u64,
    #[serde(default)]
    pub last_soft_callback_gap_ms: f64,
    #[serde(default)]
    pub last_soft_callback_gap_at_ms: u64,
    #[serde(default)]
    pub ring_below_250ms_events: u64,
    #[serde(default)]
    pub ring_below_100ms_events: u64,
    #[serde(default)]
    pub ring_below_50ms_events: u64,
    #[serde(default)]
    pub ring_below_callback_events: u64,
    #[serde(default)]
    pub last_ring_pressure_at_ms: u64,
    #[serde(default)]
    pub marker_error_events: u64,
    #[serde(default)]
    pub program_idle_splice_events: u64,
    #[serde(default)]
    pub program_to_idle_events: u64,
    #[serde(default)]
    pub idle_to_program_events: u64,
    #[serde(default)]
    pub mixed_output_events: u64,
    #[serde(default)]
    pub last_output_transition_id: u32,
    #[serde(default)]
    pub last_output_transition_at_ms: u64,
    #[serde(default)]
    pub repeated_payload_events: u64,
    #[serde(default)]
    pub callback_index: u64,
    #[serde(default)]
    pub last_callback_at_ms: u64,
    #[serde(default)]
    pub last_callback_gap_ms: f64,
    #[serde(default)]
    pub last_callback_frames: u32,
    #[serde(default)]
    pub last_output_kind_id: u32,
    #[serde(default)]
    pub last_ring_fill_samples: u64,
    #[serde(default)]
    pub last_program_read_samples: u64,
    #[serde(default)]
    pub ring_read_cursor_samples: u64,
    #[serde(default)]
    pub last_payload_fingerprint: u64,
    #[serde(default)]
    pub last_payload_fingerprint_at_ms: u64,
    #[serde(default)]
    pub marker_scan_count: u64,
    #[serde(default)]
    pub every_callback_scan_enabled: bool,
    pub last_underrun_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SyncSignalPath {
    pub source_format: Option<String>,
    pub source_rate: u32,
    pub source_bit_depth: u32,
    pub dsp_filter: String,
    pub dsp_target_rate: u32,
    #[serde(default)]
    pub src_path_kind: Option<String>,
    #[serde(default)]
    pub src_capped_fallback: bool,
    #[serde(default = "default_true")]
    pub src_phase_profile_preserved: bool,
    #[serde(default)]
    pub src_ratio_num: u32,
    #[serde(default)]
    pub src_ratio_den: u32,
    pub output_device: Option<String>,
    pub output_rate: u32,
    pub output_bit_depth: u32,
    #[serde(default)]
    pub output_mode: Option<String>,
    #[serde(default)]
    pub active_output_mode: Option<String>,
    #[serde(default)]
    pub output_transport: Option<String>,
    #[serde(default)]
    pub dsd_stability_resets: u64,
    #[serde(default)]
    pub dsd_modulator: Option<String>,
    pub exclusive: bool,
    pub cpu_percent: f32,
    pub resample_time_ns: u64,
    #[serde(default)]
    pub dsd_upsample_time_ns: u64,
    #[serde(default)]
    pub dsd_modulate_time_ns: u64,
    #[serde(default)]
    pub dsd_output_pending_samples: u64,
    #[serde(default)]
    pub dsd_overbudget_blocks: u64,
    #[serde(default)]
    pub dsd_last_load: f32,
    #[serde(default)]
    pub dsd_recent_load_p95: f32,
    #[serde(default)]
    pub dsd_recent_load_p99: f32,
    #[serde(default)]
    pub dsd_buffer_health: Option<DsdBufferHealth>,
    #[serde(default)]
    pub dop_ring_capacity_ms: f64,
    #[serde(default)]
    pub dop_ring_fill_ms: f64,
    #[serde(default)]
    pub dop_ring_low_watermark_ms: f64,
    #[serde(default)]
    pub dop_callback_frames: u32,
    #[serde(default)]
    pub dop_callback_ms: f64,
    #[serde(default)]
    pub dop_requested_hardware_buffer_frames: u32,
    #[serde(default)]
    pub dop_requested_hardware_buffer_ms: f64,
    #[serde(default)]
    pub dop_hardware_buffer_min_frames: u32,
    #[serde(default)]
    pub dop_hardware_buffer_max_frames: u32,
    #[serde(default)]
    pub dop_hardware_buffer_frames: u32,
    #[serde(default)]
    pub dop_hardware_buffer_ms: f64,
    #[serde(default)]
    pub dop_lock_miss_events: u64,
    #[serde(default)]
    pub dop_callback_deadline_miss_events: u64,
    #[serde(default)]
    pub dop_soft_callback_gap_125_events: u64,
    #[serde(default)]
    pub dop_soft_callback_gap_150_events: u64,
    #[serde(default)]
    pub dop_soft_callback_gap_175_events: u64,
    #[serde(default)]
    pub dop_last_soft_callback_gap_ms: f64,
    #[serde(default)]
    pub dop_last_soft_callback_gap_at_ms: u64,
    #[serde(default)]
    pub dop_ring_below_250ms_events: u64,
    #[serde(default)]
    pub dop_ring_below_100ms_events: u64,
    #[serde(default)]
    pub dop_ring_below_50ms_events: u64,
    #[serde(default)]
    pub dop_ring_below_callback_events: u64,
    #[serde(default)]
    pub dop_last_ring_pressure_at_ms: u64,
    #[serde(default)]
    pub dop_marker_error_events: u64,
    #[serde(default)]
    pub dop_program_idle_splice_events: u64,
    #[serde(default)]
    pub dop_program_to_idle_events: u64,
    #[serde(default)]
    pub dop_idle_to_program_events: u64,
    #[serde(default)]
    pub dop_mixed_output_events: u64,
    #[serde(default)]
    pub dop_last_output_transition_id: u32,
    #[serde(default)]
    pub dop_last_output_transition_at_ms: u64,
    #[serde(default)]
    pub dop_repeated_payload_events: u64,
    #[serde(default)]
    pub dop_callback_index: u64,
    #[serde(default)]
    pub dop_last_callback_at_ms: u64,
    #[serde(default)]
    pub dop_last_callback_gap_ms: f64,
    #[serde(default)]
    pub dop_last_callback_frames: u32,
    #[serde(default)]
    pub dop_last_output_kind_id: u32,
    #[serde(default)]
    pub dop_last_ring_fill_samples: u64,
    #[serde(default)]
    pub dop_last_program_read_samples: u64,
    #[serde(default)]
    pub dop_ring_read_cursor_samples: u64,
    #[serde(default)]
    pub dop_last_payload_fingerprint: u64,
    #[serde(default)]
    pub dop_last_payload_fingerprint_at_ms: u64,
    #[serde(default)]
    pub dop_marker_scan_count: u64,
    #[serde(default)]
    pub dop_every_callback_scan_enabled: bool,
    #[serde(default)]
    pub dop_last_underrun_at_ms: u64,
    #[serde(default)]
    pub output_ring_fill_now_ms: f64,
    #[serde(default)]
    pub output_ring_fill_min_ms: f64,
    #[serde(default)]
    pub startup_ring_low_watermark_ms: f64,
    #[serde(default)]
    pub startup_ready_ms: u64,
    #[serde(default)]
    pub startup_first_render_block_ms: f64,
    #[serde(default)]
    pub startup_producer_over_budget_count: u64,
    #[serde(default)]
    pub startup_callback_gaps_ms: Vec<f64>,
    #[serde(default)]
    pub underrun_count: u64,
    #[serde(default)]
    pub producer_over_budget_count: u64,
    #[serde(default)]
    pub max_render_block_ms: f64,
    #[serde(default)]
    pub max_audio_callback_gap_ms: f64,
    #[serde(default)]
    pub dsp_graph_rebuild_count: u64,
    #[serde(default)]
    pub sample_rate_change_count: u64,
    #[serde(default)]
    pub dop_alignment_reset_count: u64,
    #[serde(default)]
    pub coreaudio_dop_open_count: u64,
    #[serde(default)]
    pub coreaudio_dop_start_count: u64,
    #[serde(default)]
    pub coreaudio_dop_stop_count: u64,
    #[serde(default)]
    pub coreaudio_dop_drop_count: u64,
    #[serde(default)]
    pub coreaudio_dop_quiesce_count: u64,
    #[serde(default)]
    pub coreaudio_dop_last_lifecycle_event_id: u32,
    #[serde(default)]
    pub coreaudio_dop_last_lifecycle_at_ms: u64,
    #[serde(default)]
    pub reopen_reason_count: u64,
    #[serde(default)]
    pub last_reopen_reason_id: u32,
    #[serde(default)]
    pub last_reopen_reason_at_ms: u64,
    #[serde(default)]
    pub flush_reason_count: u64,
    #[serde(default)]
    pub last_flush_reason_id: u32,
    #[serde(default)]
    pub last_flush_reason_at_ms: u64,
    #[serde(default)]
    pub modulator_reset_count: u64,
    #[serde(default)]
    pub decoder_starved_count: u64,
    #[serde(default)]
    pub source_read_time_ms: f64,
    #[serde(default)]
    pub max_source_read_ms: f64,
    #[serde(default)]
    pub source_read_stall_count: u64,
    #[serde(default)]
    pub source_read_stall_last_at_ms: u64,
    #[serde(default)]
    pub decoder_decode_time_ms: f64,
    #[serde(default)]
    pub max_decoder_decode_ms: f64,
    #[serde(default)]
    pub decoder_decode_stall_count: u64,
    #[serde(default)]
    pub decoder_decode_stall_last_at_ms: u64,
    #[serde(default)]
    pub lock_wait_max_ms: f64,
    pub block_duration_ns: u64,
    #[serde(default)]
    pub signal_peak: f32,
    #[serde(default)]
    pub signal_peak_max: f32,
    #[serde(default)]
    pub signal_clipping: bool,
    #[serde(default)]
    pub signal_clip_events: u64,
    #[serde(default)]
    pub signal_clip_samples: u64,
    #[serde(default)]
    pub dsd_limiter_peak_ratio: f32,
    #[serde(default)]
    pub dsd_limiter_peak_ratio_max: f32,
    #[serde(default)]
    pub dsd_limiter_active: bool,
    #[serde(default)]
    pub dsd_limiter_events: u64,
    #[serde(default)]
    pub dsd_limiter_samples: u64,
    pub underrun_events: u64,
    pub underrun_samples: u64,
}

fn default_true() -> bool {
    true
}

/// The server-side chain for a browser-zone stream, recorded when the stream
/// route serves (or starts transcoding) a local track for that zone. The
/// browser itself cannot see any of this — the source spec comes from the
/// library, and EQ/encode happen inside the derivative pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct BrowserStreamSignal {
    /// Local library track this signal describes.
    pub track_id: i64,
    /// "original", "flac" (lossless derivative), "flac_passthrough", "opus".
    pub variant: String,
    #[serde(default)]
    pub opus_kbps: Option<u32>,
    pub eq_active: bool,
    #[serde(default)]
    pub eq_active_bands: u32,
    /// Uppercase source container label, e.g. "FLAC".
    #[serde(default)]
    pub source_format: Option<String>,
    /// 0 when the library has no technical metadata for the track.
    #[serde(default)]
    pub source_rate: u32,
    #[serde(default)]
    pub source_bits: u32,
    /// Delivered stream rate/bits: 48000/0 for Opus, source rate/24 for the
    /// EQ'd FLAC derivative, source rate/bits for passthrough.
    #[serde(default)]
    pub output_rate: u32,
    #[serde(default)]
    pub output_bits: u32,
}
