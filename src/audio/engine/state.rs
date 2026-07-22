use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsp::resampler::{DEFAULT_FILTER_TYPE, ResamplerRuntimeInfo};
use crate::audio::engine::player::DEFAULT_HEADROOM_DB;
use crate::audio::engine::signal_path::OutputTransport;

pub(super) const PLAYBACK_STOPPED: u32 = 0;
pub(super) const PLAYBACK_PLAYING: u32 = 1;
pub(super) const PLAYBACK_PAUSED: u32 = 2;
pub(super) const PLAYBACK_STARTING: u32 = 3;
pub(super) const STARTUP_DIAGNOSTIC_WINDOW_MS: u64 = 5_000;
pub(super) const STARTUP_CALLBACK_GAP_SLOTS: usize = 20;

pub(super) const COREAUDIO_DOP_LIFECYCLE_OPEN_ATTEMPT: u32 = 1;
pub(super) const COREAUDIO_DOP_LIFECYCLE_START: u32 = 2;
pub(super) const COREAUDIO_DOP_LIFECYCLE_QUIESCE: u32 = 3;
pub(super) const COREAUDIO_DOP_LIFECYCLE_STOP: u32 = 4;
pub(super) const COREAUDIO_DOP_LIFECYCLE_DROP: u32 = 5;

pub(super) const REOPEN_REASON_UPDATE_CONFIG: u32 = 1;
pub(super) const REOPEN_REASON_SELECT_DEVICE: u32 = 2;
pub(super) const REOPEN_REASON_SET_OUTPUT_MODE: u32 = 3;
pub(super) const REOPEN_REASON_DSD_RULES: u32 = 4;
pub(super) const REOPEN_REASON_DSD_MODULATOR: u32 = 5;
pub(super) const REOPEN_REASON_DSD_ISI: u32 = 6;
pub(super) const REOPEN_REASON_PENDING_START: u32 = 7;
pub(super) const REOPEN_REASON_SEEK: u32 = 8;
pub(super) const REOPEN_REASON_EXTERNAL_DEVICE_READY: u32 = 9;
pub(super) const REOPEN_REASON_EOF_DRAIN_TIMEOUT: u32 = 10;

pub(super) const FLUSH_REASON_REOPEN: u32 = 1;
pub(super) const FLUSH_REASON_PENDING_START: u32 = 2;
pub(super) const FLUSH_REASON_RESTART_SESSION: u32 = 3;
pub(super) const FLUSH_REASON_RECONFIGURE_SESSION: u32 = 4;
pub(super) const FLUSH_REASON_SEEK: u32 = 5;
pub(super) const FLUSH_REASON_CALLBACK_CONSUMED: u32 = 6;

pub(super) const DOP_OUTPUT_TRANSITION_PROGRAM_TO_IDLE: u32 = 1;
pub(super) const DOP_OUTPUT_TRANSITION_IDLE_TO_PROGRAM: u32 = 2;
pub(super) const DOP_OUTPUT_TRANSITION_PROGRAM_TO_MIXED: u32 = 3;
pub(super) const DOP_OUTPUT_TRANSITION_MIXED_TO_PROGRAM: u32 = 4;
pub(super) const DOP_OUTPUT_TRANSITION_MIXED_TO_IDLE: u32 = 5;
pub(super) const DOP_OUTPUT_TRANSITION_IDLE_TO_MIXED: u32 = 6;

pub struct AtomicPlayerState {
    pub state: AtomicU32,
    pub source_rate: AtomicU32,
    pub target_rate: AtomicU32,
    pub source_bits: AtomicU32,
    pub target_bits: AtomicU32,
    pub configured_target_rate: AtomicU32, // 0 = Auto Best
    pub upsampling_enabled: AtomicBool,
    pub filter_type: AtomicU32,        // See FilterType::as_id.
    pub active_filter_type: AtomicU32, // Effective renderer filter, including DSD rules.
    pub dither_mode: AtomicU32,        // See DitherPreference::as_id.
    /// Active SRC path kind. 0 means bypass/unknown; see ResamplerPathKind::as_id.
    pub src_path_kind: AtomicU32,
    pub src_capped_fallback: AtomicBool,
    pub src_phase_profile_preserved: AtomicBool,
    pub src_ratio_num: AtomicU32,
    pub src_ratio_den: AtomicU32,
    pub volume: AtomicU32, // f32 bits
    /// Configured attenuation before final transport packaging, stored as f32 dB bits.
    pub headroom_db: AtomicU32,
    /// DSP output preroll buffer in milliseconds. 0 means automatic/default.
    pub dsp_buffer_ms: AtomicU32,
    pub exclusive: AtomicBool,
    pub position_samples: AtomicU64,
    pub duration_samples: AtomicU64,
    pub resample_time_ns: AtomicU64,
    pub dsd_upsample_time_ns: AtomicU64,
    pub dsd_modulate_time_ns: AtomicU64,
    pub dsd_output_pending_samples: AtomicU64,
    pub dsd_ring_capacity_samples: AtomicU64,
    pub dsd_ring_fill_samples: AtomicU64,
    pub dsd_ring_low_watermark_samples: AtomicU64,
    pub dsd_callback_frames: AtomicU32,
    pub dsd_requested_hardware_buffer_frames: AtomicU32,
    pub dsd_hardware_buffer_min_frames: AtomicU32,
    pub dsd_hardware_buffer_max_frames: AtomicU32,
    pub dsd_hardware_buffer_frames: AtomicU32,
    pub dsd_lock_miss_events: AtomicU64,
    pub dsd_callback_deadline_miss_events: AtomicU64,
    pub dsd_soft_callback_gap_125_events: AtomicU64,
    pub dsd_soft_callback_gap_150_events: AtomicU64,
    pub dsd_soft_callback_gap_175_events: AtomicU64,
    pub dsd_last_soft_callback_gap_ns: AtomicU64,
    pub dsd_last_soft_callback_gap_at_ms: AtomicU64,
    pub dsd_ring_below_250ms_events: AtomicU64,
    pub dsd_ring_below_100ms_events: AtomicU64,
    pub dsd_ring_below_50ms_events: AtomicU64,
    pub dsd_ring_below_callback_events: AtomicU64,
    pub dsd_last_ring_pressure_at_ms: AtomicU64,
    pub dsd_dop_marker_error_events: AtomicU64,
    pub dsd_dop_program_idle_splice_events: AtomicU64,
    pub dsd_dop_program_to_idle_events: AtomicU64,
    pub dsd_dop_idle_to_program_events: AtomicU64,
    pub dsd_dop_mixed_output_events: AtomicU64,
    pub dsd_dop_last_output_transition_id: AtomicU32,
    pub dsd_dop_last_output_transition_at_ms: AtomicU64,
    pub dsd_dop_repeated_payload_events: AtomicU64,
    pub dsd_dop_callback_index: AtomicU64,
    pub dsd_dop_last_callback_at_ms: AtomicU64,
    pub dsd_dop_last_callback_gap_ns: AtomicU64,
    pub dsd_dop_last_callback_frames: AtomicU32,
    pub dsd_dop_last_output_kind_id: AtomicU32,
    pub dsd_dop_last_ring_fill_samples: AtomicU64,
    pub dsd_dop_last_program_read_samples: AtomicU64,
    pub dsd_dop_ring_read_cursor_samples: AtomicU64,
    pub dsd_dop_last_payload_fingerprint: AtomicU64,
    pub dsd_dop_last_payload_fingerprint_at_ms: AtomicU64,
    pub dsd_dop_marker_scan_count: AtomicU64,
    pub dsd_dop_every_callback_scan_enabled: AtomicBool,
    pub dsd_last_underrun_at_ms: AtomicU64,
    pub pcm_ring_capacity_samples: AtomicU64,
    pub pcm_ring_fill_samples: AtomicU64,
    pub pcm_ring_low_watermark_samples: AtomicU64,
    pub pcm_callback_frames: AtomicU32,
    pub pcm_lock_miss_events: AtomicU64,
    /// Count of DSD render blocks that exceeded their realtime block duration.
    pub dsd_overbudget_blocks: AtomicU64,
    /// Most recent DSD render load ratio, stored as f32 bits.
    pub dsd_last_load: AtomicU32,
    /// Recent-window p95 DSD render load ratio, stored as f32 bits.
    pub dsd_recent_load_p95: AtomicU32,
    /// Recent-window p99 DSD render load ratio, stored as f32 bits.
    pub dsd_recent_load_p99: AtomicU32,
    pub block_duration_ns: AtomicU64,
    pub meter_l: AtomicU32, // f32 bits
    pub meter_r: AtomicU32, // f32 bits
    /// Most recent rendered full-scale peak after source volume/headroom, stored as f32 bits.
    pub signal_peak: AtomicU32,
    /// Highest rendered full-scale peak since the last signal reset, stored as f32 bits.
    pub signal_peak_max: AtomicU32,
    /// Whether the most recent rendered block hit or exceeded full scale.
    pub signal_clipping: AtomicBool,
    /// Count of rendered blocks that hit or exceeded full scale since reset.
    pub signal_clip_events: AtomicU64,
    /// Count of samples that hit or exceeded full scale since reset.
    pub signal_clip_samples: AtomicU64,
    /// Most recent DSD pre-modulator peak as a ratio of the modulator input limit.
    pub dsd_limiter_peak_ratio: AtomicU32,
    /// Highest DSD pre-modulator peak ratio since reset.
    pub dsd_limiter_peak_ratio_max: AtomicU32,
    /// Whether the most recent DSD block touched the soft input-limiter knee.
    pub dsd_limiter_active: AtomicBool,
    /// Count of DSD blocks that touched the soft input-limiter knee since reset.
    pub dsd_limiter_events: AtomicU64,
    /// Count of DSD-rate channel samples above the soft input-limiter knee since reset.
    pub dsd_limiter_samples: AtomicU64,
    pub max_render_block_ns: AtomicU64,
    pub max_audio_callback_gap_ns: AtomicU64,
    pub dsp_graph_rebuild_count: AtomicU64,
    pub sample_rate_change_count: AtomicU64,
    pub dop_alignment_reset_count: AtomicU64,
    pub coreaudio_dop_open_count: AtomicU64,
    pub coreaudio_dop_start_count: AtomicU64,
    pub coreaudio_dop_stop_count: AtomicU64,
    pub coreaudio_dop_drop_count: AtomicU64,
    pub coreaudio_dop_quiesce_count: AtomicU64,
    pub coreaudio_dop_last_lifecycle_event_id: AtomicU32,
    pub coreaudio_dop_last_lifecycle_at_ms: AtomicU64,
    pub reopen_reason_count: AtomicU64,
    pub last_reopen_reason_id: AtomicU32,
    pub last_reopen_reason_at_ms: AtomicU64,
    pub flush_reason_count: AtomicU64,
    pub last_flush_reason_id: AtomicU32,
    pub last_flush_reason_at_ms: AtomicU64,
    pub modulator_reset_count: AtomicU64,
    pub decoder_starved_count: AtomicU64,
    pub source_read_time_ns: AtomicU64,
    pub max_source_read_ns: AtomicU64,
    pub source_read_stall_count: AtomicU64,
    pub source_read_stall_last_at_ms: AtomicU64,
    pub decoder_decode_time_ns: AtomicU64,
    pub max_decoder_decode_ns: AtomicU64,
    pub decoder_decode_stall_count: AtomicU64,
    pub decoder_decode_stall_last_at_ms: AtomicU64,
    pub lock_wait_max_ns: AtomicU64,
    /// Wall-clock start of the current startup diagnostic window in Unix ms.
    pub startup_started_at_ms: AtomicU64,
    /// Elapsed milliseconds from startup boundary to STARTING -> PLAYING.
    pub startup_ready_ms: AtomicU64,
    /// Lowest output ring fill observed during the startup diagnostic window.
    pub startup_ring_low_watermark_units: AtomicU64,
    /// Units per second for `startup_ring_low_watermark_units`.
    pub startup_ring_units_per_sec: AtomicU64,
    /// First render block duration in the startup diagnostic window.
    pub startup_first_render_block_ns: AtomicU64,
    /// Render blocks over realtime budget during the startup diagnostic window.
    pub startup_overbudget_blocks: AtomicU64,
    /// Number of startup callback gaps recorded, capped by `STARTUP_CALLBACK_GAP_SLOTS`.
    pub startup_callback_gap_count: AtomicU64,
    pub startup_callback_gaps_ns: [AtomicU64; STARTUP_CALLBACK_GAP_SLOTS],
    pub flush_buffer: AtomicBool,
    /// Allows the output callback to consume a final short block instead of
    /// waiting for the normal underrun-recovery refill threshold at EOF.
    pub eof_drain_requested: AtomicBool,
    pub underrun_events: AtomicU64,
    pub underrun_samples: AtomicU64,
    /// Requested output mode: 0 = PCM, 1 = DSD128, 2 = DSD256, 3 = DSD64.
    pub output_mode: AtomicU32,
    /// Successfully opened output mode; may differ from requested `output_mode` after fallback.
    pub active_output_mode: AtomicU32,
    /// See [`OutputTransport::as_id`].
    pub output_transport: AtomicU32,
    /// Bumped whenever a new output fallback/reset notice should be displayed.
    pub output_notice_id: AtomicU64,
    /// Cumulative count of modulator stability resets across both channels.
    /// Non-zero indicates the DSD path has gone unstable at least once.
    pub dsd_stability_resets: AtomicU64,
    /// Requested DSD modulator family/depth. See [`DsdModulator::as_id`].
    pub dsd_modulator: AtomicU32,
    /// DSD EC transition-loss compensation, stored as f32 bits. 0.0 = ideal DAC.
    pub dsd_isi_penalty: AtomicU32,
}

impl Default for AtomicPlayerState {
    fn default() -> Self {
        Self::new()
    }
}

impl AtomicPlayerState {
    pub fn new() -> Self {
        Self {
            state: AtomicU32::new(PLAYBACK_STOPPED),
            source_rate: AtomicU32::new(44100),
            target_rate: AtomicU32::new(192000),
            source_bits: AtomicU32::new(16),
            target_bits: AtomicU32::new(24),
            configured_target_rate: AtomicU32::new(0),
            upsampling_enabled: AtomicBool::new(false),
            filter_type: AtomicU32::new(DEFAULT_FILTER_TYPE.as_id()),
            active_filter_type: AtomicU32::new(DEFAULT_FILTER_TYPE.as_id()),
            dither_mode: AtomicU32::new(0), // Auto default
            src_path_kind: AtomicU32::new(0),
            src_capped_fallback: AtomicBool::new(false),
            src_phase_profile_preserved: AtomicBool::new(true),
            src_ratio_num: AtomicU32::new(1),
            src_ratio_den: AtomicU32::new(1),
            volume: AtomicU32::new(1.0f32.to_bits()),
            headroom_db: AtomicU32::new(DEFAULT_HEADROOM_DB.to_bits()),
            dsp_buffer_ms: AtomicU32::new(0),
            exclusive: AtomicBool::new(true),
            position_samples: AtomicU64::new(0),
            duration_samples: AtomicU64::new(0),
            resample_time_ns: AtomicU64::new(0),
            dsd_upsample_time_ns: AtomicU64::new(0),
            dsd_modulate_time_ns: AtomicU64::new(0),
            dsd_output_pending_samples: AtomicU64::new(0),
            dsd_ring_capacity_samples: AtomicU64::new(0),
            dsd_ring_fill_samples: AtomicU64::new(0),
            dsd_ring_low_watermark_samples: AtomicU64::new(0),
            dsd_callback_frames: AtomicU32::new(0),
            dsd_requested_hardware_buffer_frames: AtomicU32::new(0),
            dsd_hardware_buffer_min_frames: AtomicU32::new(0),
            dsd_hardware_buffer_max_frames: AtomicU32::new(0),
            dsd_hardware_buffer_frames: AtomicU32::new(0),
            dsd_lock_miss_events: AtomicU64::new(0),
            dsd_callback_deadline_miss_events: AtomicU64::new(0),
            dsd_soft_callback_gap_125_events: AtomicU64::new(0),
            dsd_soft_callback_gap_150_events: AtomicU64::new(0),
            dsd_soft_callback_gap_175_events: AtomicU64::new(0),
            dsd_last_soft_callback_gap_ns: AtomicU64::new(0),
            dsd_last_soft_callback_gap_at_ms: AtomicU64::new(0),
            dsd_ring_below_250ms_events: AtomicU64::new(0),
            dsd_ring_below_100ms_events: AtomicU64::new(0),
            dsd_ring_below_50ms_events: AtomicU64::new(0),
            dsd_ring_below_callback_events: AtomicU64::new(0),
            dsd_last_ring_pressure_at_ms: AtomicU64::new(0),
            dsd_dop_marker_error_events: AtomicU64::new(0),
            dsd_dop_program_idle_splice_events: AtomicU64::new(0),
            dsd_dop_program_to_idle_events: AtomicU64::new(0),
            dsd_dop_idle_to_program_events: AtomicU64::new(0),
            dsd_dop_mixed_output_events: AtomicU64::new(0),
            dsd_dop_last_output_transition_id: AtomicU32::new(0),
            dsd_dop_last_output_transition_at_ms: AtomicU64::new(0),
            dsd_dop_repeated_payload_events: AtomicU64::new(0),
            dsd_dop_callback_index: AtomicU64::new(0),
            dsd_dop_last_callback_at_ms: AtomicU64::new(0),
            dsd_dop_last_callback_gap_ns: AtomicU64::new(0),
            dsd_dop_last_callback_frames: AtomicU32::new(0),
            dsd_dop_last_output_kind_id: AtomicU32::new(0),
            dsd_dop_last_ring_fill_samples: AtomicU64::new(0),
            dsd_dop_last_program_read_samples: AtomicU64::new(0),
            dsd_dop_ring_read_cursor_samples: AtomicU64::new(0),
            dsd_dop_last_payload_fingerprint: AtomicU64::new(0),
            dsd_dop_last_payload_fingerprint_at_ms: AtomicU64::new(0),
            dsd_dop_marker_scan_count: AtomicU64::new(0),
            dsd_dop_every_callback_scan_enabled: AtomicBool::new(false),
            dsd_last_underrun_at_ms: AtomicU64::new(0),
            pcm_ring_capacity_samples: AtomicU64::new(0),
            pcm_ring_fill_samples: AtomicU64::new(0),
            pcm_ring_low_watermark_samples: AtomicU64::new(0),
            pcm_callback_frames: AtomicU32::new(0),
            pcm_lock_miss_events: AtomicU64::new(0),
            dsd_overbudget_blocks: AtomicU64::new(0),
            dsd_last_load: AtomicU32::new(0.0f32.to_bits()),
            dsd_recent_load_p95: AtomicU32::new(0.0f32.to_bits()),
            dsd_recent_load_p99: AtomicU32::new(0.0f32.to_bits()),
            block_duration_ns: AtomicU64::new(0),
            meter_l: AtomicU32::new(0),
            meter_r: AtomicU32::new(0),
            signal_peak: AtomicU32::new(0),
            signal_peak_max: AtomicU32::new(0),
            signal_clipping: AtomicBool::new(false),
            signal_clip_events: AtomicU64::new(0),
            signal_clip_samples: AtomicU64::new(0),
            dsd_limiter_peak_ratio: AtomicU32::new(0),
            dsd_limiter_peak_ratio_max: AtomicU32::new(0),
            dsd_limiter_active: AtomicBool::new(false),
            dsd_limiter_events: AtomicU64::new(0),
            dsd_limiter_samples: AtomicU64::new(0),
            max_render_block_ns: AtomicU64::new(0),
            max_audio_callback_gap_ns: AtomicU64::new(0),
            dsp_graph_rebuild_count: AtomicU64::new(0),
            sample_rate_change_count: AtomicU64::new(0),
            dop_alignment_reset_count: AtomicU64::new(0),
            coreaudio_dop_open_count: AtomicU64::new(0),
            coreaudio_dop_start_count: AtomicU64::new(0),
            coreaudio_dop_stop_count: AtomicU64::new(0),
            coreaudio_dop_drop_count: AtomicU64::new(0),
            coreaudio_dop_quiesce_count: AtomicU64::new(0),
            coreaudio_dop_last_lifecycle_event_id: AtomicU32::new(0),
            coreaudio_dop_last_lifecycle_at_ms: AtomicU64::new(0),
            reopen_reason_count: AtomicU64::new(0),
            last_reopen_reason_id: AtomicU32::new(0),
            last_reopen_reason_at_ms: AtomicU64::new(0),
            flush_reason_count: AtomicU64::new(0),
            last_flush_reason_id: AtomicU32::new(0),
            last_flush_reason_at_ms: AtomicU64::new(0),
            modulator_reset_count: AtomicU64::new(0),
            decoder_starved_count: AtomicU64::new(0),
            source_read_time_ns: AtomicU64::new(0),
            max_source_read_ns: AtomicU64::new(0),
            source_read_stall_count: AtomicU64::new(0),
            source_read_stall_last_at_ms: AtomicU64::new(0),
            decoder_decode_time_ns: AtomicU64::new(0),
            max_decoder_decode_ns: AtomicU64::new(0),
            decoder_decode_stall_count: AtomicU64::new(0),
            decoder_decode_stall_last_at_ms: AtomicU64::new(0),
            lock_wait_max_ns: AtomicU64::new(0),
            startup_started_at_ms: AtomicU64::new(0),
            startup_ready_ms: AtomicU64::new(0),
            startup_ring_low_watermark_units: AtomicU64::new(u64::MAX),
            startup_ring_units_per_sec: AtomicU64::new(0),
            startup_first_render_block_ns: AtomicU64::new(0),
            startup_overbudget_blocks: AtomicU64::new(0),
            startup_callback_gap_count: AtomicU64::new(0),
            startup_callback_gaps_ns: std::array::from_fn(|_| AtomicU64::new(0)),
            flush_buffer: AtomicBool::new(false),
            eof_drain_requested: AtomicBool::new(false),
            underrun_events: AtomicU64::new(0),
            underrun_samples: AtomicU64::new(0),
            output_mode: AtomicU32::new(0),
            active_output_mode: AtomicU32::new(0),
            output_transport: AtomicU32::new(OutputTransport::None.as_id()),
            output_notice_id: AtomicU64::new(0),
            dsd_stability_resets: AtomicU64::new(0),
            dsd_modulator: AtomicU32::new(DsdModulator::default().as_id()),
            dsd_isi_penalty: AtomicU32::new(0.0f32.to_bits()),
        }
    }

    pub fn record_coreaudio_dop_lifecycle(&self, event_id: u32) {
        match event_id {
            COREAUDIO_DOP_LIFECYCLE_OPEN_ATTEMPT => {
                self.coreaudio_dop_open_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            COREAUDIO_DOP_LIFECYCLE_START => {
                self.coreaudio_dop_start_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            COREAUDIO_DOP_LIFECYCLE_QUIESCE => {
                self.coreaudio_dop_quiesce_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            COREAUDIO_DOP_LIFECYCLE_STOP => {
                self.coreaudio_dop_stop_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            COREAUDIO_DOP_LIFECYCLE_DROP => {
                self.coreaudio_dop_drop_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        self.coreaudio_dop_last_lifecycle_event_id
            .store(event_id, Ordering::Relaxed);
        self.coreaudio_dop_last_lifecycle_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
    }

    pub fn record_reopen_reason(&self, reason_id: u32) {
        self.reopen_reason_count.fetch_add(1, Ordering::Relaxed);
        self.last_reopen_reason_id
            .store(reason_id, Ordering::Relaxed);
        self.last_reopen_reason_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
    }

    pub fn request_flush(&self, reason_id: u32) {
        self.flush_buffer.store(true, Ordering::Relaxed);
        self.flush_reason_count.fetch_add(1, Ordering::Relaxed);
        self.last_flush_reason_id
            .store(reason_id, Ordering::Relaxed);
        self.last_flush_reason_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
    }

    pub fn record_flush_consumed(&self) {
        self.last_flush_reason_id
            .store(FLUSH_REASON_CALLBACK_CONSUMED, Ordering::Relaxed);
        self.last_flush_reason_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
    }

    pub fn record_dop_output_transition(&self, transition_id: u32) {
        match transition_id {
            DOP_OUTPUT_TRANSITION_PROGRAM_TO_IDLE => {
                self.dsd_dop_program_to_idle_events
                    .fetch_add(1, Ordering::Relaxed);
            }
            DOP_OUTPUT_TRANSITION_IDLE_TO_PROGRAM => {
                self.dsd_dop_idle_to_program_events
                    .fetch_add(1, Ordering::Relaxed);
            }
            DOP_OUTPUT_TRANSITION_PROGRAM_TO_MIXED
            | DOP_OUTPUT_TRANSITION_MIXED_TO_PROGRAM
            | DOP_OUTPUT_TRANSITION_MIXED_TO_IDLE
            | DOP_OUTPUT_TRANSITION_IDLE_TO_MIXED => {
                self.dsd_dop_mixed_output_events
                    .fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        self.dsd_dop_last_output_transition_id
            .store(transition_id, Ordering::Relaxed);
        self.dsd_dop_last_output_transition_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
    }

    pub fn reset_signal_level_metrics(&self) {
        self.signal_peak.store(0, Ordering::Relaxed);
        self.signal_peak_max.store(0, Ordering::Relaxed);
        self.signal_clipping.store(false, Ordering::Relaxed);
        self.signal_clip_events.store(0, Ordering::Relaxed);
        self.signal_clip_samples.store(0, Ordering::Relaxed);
        self.dsd_limiter_peak_ratio.store(0, Ordering::Relaxed);
        self.dsd_limiter_peak_ratio_max.store(0, Ordering::Relaxed);
        self.dsd_limiter_active.store(false, Ordering::Relaxed);
        self.dsd_limiter_events.store(0, Ordering::Relaxed);
        self.dsd_limiter_samples.store(0, Ordering::Relaxed);
    }

    pub fn record_render_block_ns(&self, ns: u64) {
        record_atomic_max(&self.max_render_block_ns, ns);
    }

    pub fn record_audio_callback_gap_ns(&self, ns: u64) {
        record_atomic_max(&self.max_audio_callback_gap_ns, ns);
    }

    pub fn record_source_read_ns(&self, ns: u64, stall_threshold_ns: u64) {
        self.source_read_time_ns.store(ns, Ordering::Relaxed);
        record_atomic_max(&self.max_source_read_ns, ns);
        if stall_threshold_ns > 0 && ns > stall_threshold_ns {
            self.source_read_stall_count.fetch_add(1, Ordering::Relaxed);
            self.source_read_stall_last_at_ms
                .store(unix_epoch_millis(), Ordering::Relaxed);
        }
    }

    pub fn record_decoder_decode_ns(&self, ns: u64, stall_threshold_ns: u64) {
        self.decoder_decode_time_ns.store(ns, Ordering::Relaxed);
        record_atomic_max(&self.max_decoder_decode_ns, ns);
        if stall_threshold_ns > 0 && ns > stall_threshold_ns {
            self.decoder_decode_stall_count
                .fetch_add(1, Ordering::Relaxed);
            self.decoder_decode_stall_last_at_ms
                .store(unix_epoch_millis(), Ordering::Relaxed);
        }
    }

    pub fn record_lock_wait_ns(&self, ns: u64) {
        record_atomic_max(&self.lock_wait_max_ns, ns);
    }

    pub fn store_target_rate_for_output(&self, target_rate: u32) {
        let had_active_output =
            self.output_transport.load(Ordering::Relaxed) != OutputTransport::None.as_id();
        let previous = self.target_rate.swap(target_rate, Ordering::Relaxed);
        if had_active_output && previous != 0 && previous != target_rate {
            self.sample_rate_change_count
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn store_src_runtime_info(&self, info: Option<ResamplerRuntimeInfo>) {
        if let Some(info) = info {
            self.src_path_kind
                .store(info.path_kind.as_id(), Ordering::Relaxed);
            self.src_capped_fallback
                .store(info.uses_capped_fallback, Ordering::Relaxed);
            self.src_phase_profile_preserved
                .store(info.phase_profile_preserved, Ordering::Relaxed);
            self.src_ratio_num.store(info.ratio_num, Ordering::Relaxed);
            self.src_ratio_den.store(info.ratio_den, Ordering::Relaxed);
        } else {
            self.src_path_kind.store(0, Ordering::Relaxed);
            self.src_capped_fallback.store(false, Ordering::Relaxed);
            self.src_phase_profile_preserved
                .store(true, Ordering::Relaxed);
            self.src_ratio_num.store(1, Ordering::Relaxed);
            self.src_ratio_den.store(1, Ordering::Relaxed);
        }
    }

    pub fn reset_boundary_diagnostic_maxima(&self) {
        self.max_render_block_ns.store(0, Ordering::Relaxed);
        self.max_audio_callback_gap_ns.store(0, Ordering::Relaxed);
        self.max_source_read_ns.store(0, Ordering::Relaxed);
        self.max_decoder_decode_ns.store(0, Ordering::Relaxed);
        self.lock_wait_max_ns.store(0, Ordering::Relaxed);
    }

    pub fn reset_decode_timing_metrics(&self) {
        self.source_read_time_ns.store(0, Ordering::Relaxed);
        self.max_source_read_ns.store(0, Ordering::Relaxed);
        self.source_read_stall_count.store(0, Ordering::Relaxed);
        self.source_read_stall_last_at_ms
            .store(0, Ordering::Relaxed);
        self.decoder_decode_time_ns.store(0, Ordering::Relaxed);
        self.max_decoder_decode_ns.store(0, Ordering::Relaxed);
        self.decoder_decode_stall_count.store(0, Ordering::Relaxed);
        self.decoder_decode_stall_last_at_ms
            .store(0, Ordering::Relaxed);
    }

    pub fn begin_startup_diagnostics(&self) {
        self.startup_started_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
        self.startup_ready_ms.store(0, Ordering::Relaxed);
        self.startup_ring_low_watermark_units
            .store(u64::MAX, Ordering::Relaxed);
        self.startup_ring_units_per_sec.store(0, Ordering::Relaxed);
        self.startup_first_render_block_ns
            .store(0, Ordering::Relaxed);
        self.startup_overbudget_blocks.store(0, Ordering::Relaxed);
        self.startup_callback_gap_count.store(0, Ordering::Relaxed);
        for gap in &self.startup_callback_gaps_ns {
            gap.store(0, Ordering::Relaxed);
        }
    }

    pub fn startup_window_elapsed_ms(&self) -> Option<u64> {
        let started = self.startup_started_at_ms.load(Ordering::Relaxed);
        if started == 0 {
            return None;
        }
        let elapsed = unix_epoch_millis().saturating_sub(started);
        (elapsed <= STARTUP_DIAGNOSTIC_WINDOW_MS).then_some(elapsed)
    }

    pub fn record_startup_ready(&self) {
        let started = self.startup_started_at_ms.load(Ordering::Relaxed);
        if started == 0 {
            return;
        }
        let elapsed = unix_epoch_millis().saturating_sub(started);
        let _ = self.startup_ready_ms.compare_exchange(
            0,
            elapsed,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    pub fn record_startup_ring_fill(&self, units: u64, units_per_sec: u64) {
        if self.startup_window_elapsed_ms().is_none() {
            return;
        }
        if units_per_sec > 0 {
            self.startup_ring_units_per_sec
                .store(units_per_sec, Ordering::Relaxed);
        }
        let mut low = self
            .startup_ring_low_watermark_units
            .load(Ordering::Relaxed);
        while units < low {
            match self.startup_ring_low_watermark_units.compare_exchange_weak(
                low,
                units,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => low = next,
            }
        }
    }

    pub fn record_startup_render_block_ns(&self, ns: u64, over_budget: bool) {
        if self.startup_window_elapsed_ms().is_none() {
            return;
        }
        let _ = self.startup_first_render_block_ns.compare_exchange(
            0,
            ns,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        if over_budget {
            self.startup_overbudget_blocks
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_startup_callback_gap_ns(&self, ns: u64) {
        if self.startup_window_elapsed_ms().is_none() {
            return;
        }
        let index = self
            .startup_callback_gap_count
            .fetch_add(1, Ordering::Relaxed);
        if (index as usize) < STARTUP_CALLBACK_GAP_SLOTS {
            self.startup_callback_gaps_ns[index as usize].store(ns, Ordering::Relaxed);
        }
    }

    pub fn diagnostics_debug_summary(&self) -> String {
        format!(
            "ring_fill={} ring_low={} pcm_ring_fill={} pcm_ring_low={} underruns={} dsd_lock_misses={} dop_deadline_misses={} dop_soft_gaps_125={} dop_ring_below_callback={} dop_marker_errors={} dop_splices={} dop_repeated_payloads={} pcm_lock_misses={} overbudget={} max_render_ms={:.3} max_callback_gap_ms={:.3} max_source_read_ms={:.3} max_decode_ms={:.3} source_read_stalls={} decode_stalls={} dsp_rebuilds={} sample_rate_changes={} dop_alignment_resets={} modulator_resets={} decoder_starved={} lock_wait_max_ms={:.3}",
            self.dsd_ring_fill_samples.load(Ordering::Relaxed),
            self.dsd_ring_low_watermark_samples.load(Ordering::Relaxed),
            self.pcm_ring_fill_samples.load(Ordering::Relaxed),
            self.pcm_ring_low_watermark_samples.load(Ordering::Relaxed),
            self.underrun_events.load(Ordering::Relaxed),
            self.dsd_lock_miss_events.load(Ordering::Relaxed),
            self.dsd_callback_deadline_miss_events
                .load(Ordering::Relaxed),
            self.dsd_soft_callback_gap_125_events
                .load(Ordering::Relaxed),
            self.dsd_ring_below_callback_events.load(Ordering::Relaxed),
            self.dsd_dop_marker_error_events.load(Ordering::Relaxed),
            self.dsd_dop_program_idle_splice_events
                .load(Ordering::Relaxed),
            self.dsd_dop_repeated_payload_events.load(Ordering::Relaxed),
            self.pcm_lock_miss_events.load(Ordering::Relaxed),
            self.dsd_overbudget_blocks.load(Ordering::Relaxed),
            nanos_to_millis(self.max_render_block_ns.load(Ordering::Relaxed)),
            nanos_to_millis(self.max_audio_callback_gap_ns.load(Ordering::Relaxed)),
            nanos_to_millis(self.max_source_read_ns.load(Ordering::Relaxed)),
            nanos_to_millis(self.max_decoder_decode_ns.load(Ordering::Relaxed)),
            self.source_read_stall_count.load(Ordering::Relaxed),
            self.decoder_decode_stall_count.load(Ordering::Relaxed),
            self.dsp_graph_rebuild_count.load(Ordering::Relaxed),
            self.sample_rate_change_count.load(Ordering::Relaxed),
            self.dop_alignment_reset_count.load(Ordering::Relaxed),
            self.modulator_reset_count.load(Ordering::Relaxed),
            self.decoder_starved_count.load(Ordering::Relaxed),
            nanos_to_millis(self.lock_wait_max_ns.load(Ordering::Relaxed)),
        )
    }
}

pub(crate) fn nanos_to_millis(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

pub(crate) fn unix_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn record_atomic_max(slot: &AtomicU64, value: u64) {
    let mut current = slot.load(Ordering::Relaxed);
    while value > current {
        match slot.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{AtomicPlayerState, nanos_to_millis};

    #[test]
    fn diagnostics_atomic_max_helpers_keep_largest_value() {
        let state = AtomicPlayerState::new();

        state.record_render_block_ns(10);
        state.record_render_block_ns(7);
        state.record_audio_callback_gap_ns(30);
        state.record_audio_callback_gap_ns(40);
        state.record_lock_wait_ns(5);
        state.record_lock_wait_ns(4);

        assert_eq!(state.max_render_block_ns.load(Ordering::Relaxed), 10);
        assert_eq!(state.max_audio_callback_gap_ns.load(Ordering::Relaxed), 40);
        assert_eq!(state.lock_wait_max_ns.load(Ordering::Relaxed), 5);
        assert_eq!(nanos_to_millis(1_500_000), 1.5);
    }

    #[test]
    fn diagnostics_sample_rate_changes_count_distinct_output_rates() {
        let state = AtomicPlayerState::new();

        state.store_target_rate_for_output(192_000);
        state.store_target_rate_for_output(192_000);
        assert_eq!(
            state.sample_rate_change_count.load(Ordering::Relaxed),
            0,
            "first output install should not count as a rate change"
        );
        state.output_transport.store(
            crate::audio::engine::signal_path::OutputTransport::PcmCoreAudio.as_id(),
            Ordering::Relaxed,
        );
        state.store_target_rate_for_output(176_400);

        assert_eq!(state.sample_rate_change_count.load(Ordering::Relaxed), 1);
        assert_eq!(state.target_rate.load(Ordering::Relaxed), 176_400);
    }

    #[test]
    fn diagnostics_boundary_reset_clears_maxima_only() {
        let state = AtomicPlayerState::new();
        state.record_render_block_ns(10);
        state.record_audio_callback_gap_ns(20);
        state.record_lock_wait_ns(30);
        state.dsp_graph_rebuild_count.store(2, Ordering::Relaxed);

        state.reset_boundary_diagnostic_maxima();

        assert_eq!(state.max_render_block_ns.load(Ordering::Relaxed), 0);
        assert_eq!(state.max_audio_callback_gap_ns.load(Ordering::Relaxed), 0);
        assert_eq!(state.lock_wait_max_ns.load(Ordering::Relaxed), 0);
        assert_eq!(state.dsp_graph_rebuild_count.load(Ordering::Relaxed), 2);
    }
}
