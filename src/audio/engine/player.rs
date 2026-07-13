use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsp::eq::EqConfig;
use crate::audio::dsp::resampler::{FilterType, ResamplerPathKind};
use crate::audio::sinks::airplay;
use crate::protocol::DsdBufferHealth;
use crate::settings::DsdSourceRule;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use symphonia::core::io::MediaSource;

pub(crate) use super::buffers::{AudioConsumer, MAX_DSP_BUFFER_MS};
pub use super::commands::{LivePlaybackConfig, PlayerCommand, QueueItem, StreamQueueItem};
pub use super::metadata::{TrackCover, TrackTags, read_track_metadata};
use super::render::clamp_headroom_db;
pub use super::state::AtomicPlayerState;
use super::state::{
    PLAYBACK_PAUSED, PLAYBACK_PLAYING, PLAYBACK_STARTING, PLAYBACK_STOPPED,
    STARTUP_CALLBACK_GAP_SLOTS, nanos_to_millis,
};
use super::worker_loop::spawn_audio_worker;
use super::worker_state::WorkerShared;
pub use crate::audio::engine::signal_path::{OutputMode, OutputTransport};

pub const DEFAULT_HEADROOM_DB: f32 = -4.0;

fn clamp_dsd_isi_penalty(penalty: f32) -> f32 {
    if penalty.is_finite() {
        penalty.clamp(0.0, 0.05)
    } else {
        0.0
    }
}

pub struct Player {
    state: Arc<AtomicPlayerState>,
    file_name: Arc<std::sync::Mutex<Option<String>>>,
    track_tags: Arc<std::sync::Mutex<TrackTags>>,
    track_cover: Arc<std::sync::Mutex<Option<TrackCover>>>,
    cover_version: Arc<std::sync::atomic::AtomicU64>,
    device_name: Arc<std::sync::Mutex<Option<String>>>,
    eq_config: Arc<std::sync::Mutex<EqConfig>>,
    output_notice: Arc<std::sync::Mutex<Option<String>>>,
    airplay_device_volume: Arc<AtomicU32>,
    playback_epoch: Arc<AtomicU64>,
    stream_queue_len: Arc<AtomicUsize>,
    stream_auto_advance_pending: Arc<AtomicBool>,
    worker_shutdown: Arc<AtomicBool>,
    command_tx: Option<tokio::sync::mpsc::UnboundedSender<PlayerCommand>>,
    worker_thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Starting,
    Playing,
    Paused,
}

impl PlaybackState {
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
        }
    }

    pub fn is_stopped(self) -> bool {
        self == Self::Stopped
    }

    fn from_id(id: u32) -> Self {
        match id {
            PLAYBACK_PLAYING => Self::Playing,
            PLAYBACK_PAUSED => Self::Paused,
            PLAYBACK_STARTING => Self::Starting,
            _ => Self::Stopped,
        }
    }
}

#[derive(Clone)]
pub struct PlayerSnapshot {
    pub state: PlaybackState,
    pub file_name: Option<String>,
    pub track_tags: TrackTags,
    pub track_cover: Option<TrackCover>,
    pub cover_version: u64,
    pub device_name: Option<String>,
    pub eq_config: EqConfig,
    pub output_notice: Option<String>,
    pub signal_path: SignalPathSnapshot,
    pub config: PlaybackConfigSnapshot,
    pub diagnostics: PlaybackDiagnosticsSnapshot,
    pub metrics: PlaybackMetricsSnapshot,
}

#[derive(Clone)]
pub struct SignalPathSnapshot {
    pub source_format: Option<String>,
    pub source_rate: u32,
    pub source_bits: u32,
    pub target_rate: u32,
    pub target_bits: u32,
    pub src_path_kind: Option<ResamplerPathKind>,
    pub src_capped_fallback: bool,
    pub src_phase_profile_preserved: bool,
    pub src_ratio_num: u32,
    pub src_ratio_den: u32,
    pub output_device: Option<String>,
    pub output_mode: OutputMode,
    pub active_output_mode: OutputMode,
    pub output_transport: OutputTransport,
    pub output_notice_id: u64,
    pub dsd_stability_resets: u64,
}

#[derive(Clone)]
pub struct PlaybackConfigSnapshot {
    pub configured_target_rate: u32,
    pub upsampling_enabled: bool,
    pub filter_type: Option<FilterType>,
    pub active_filter_type: Option<FilterType>,
    pub dither_mode_id: u32,
    pub output_mode: OutputMode,
    pub dsd_modulator: DsdModulator,
    pub dsd_isi_penalty: f32,
    pub volume: f32,
    pub headroom_db: f32,
    pub dsp_buffer_ms: u32,
    pub exclusive: bool,
}

#[derive(Clone)]
pub struct PlaybackMetricsSnapshot {
    pub position_samples: u64,
    pub duration_samples: u64,
    pub resample_time_ns: u64,
    pub dsd_upsample_time_ns: u64,
    pub dsd_modulate_time_ns: u64,
    pub dsd_output_pending_samples: u64,
    pub dsd_buffer_health: Option<DsdBufferHealth>,
    pub dsd_overbudget_blocks: u64,
    pub dsd_last_load: f32,
    pub dsd_recent_load_p95: f32,
    pub dsd_recent_load_p99: f32,
    pub block_duration_ns: u64,
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
}

#[derive(Clone, Default)]
pub struct PlaybackDiagnosticsSnapshot {
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
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        let state = Arc::new(AtomicPlayerState::new());
        let file_name = Arc::new(std::sync::Mutex::new(None));
        let track_tags = Arc::new(std::sync::Mutex::new(TrackTags::default()));
        let track_cover = Arc::new(std::sync::Mutex::new(None));
        let cover_version = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let device_name = Arc::new(std::sync::Mutex::new(None));
        let eq_config = Arc::new(std::sync::Mutex::new(EqConfig::default()));
        let output_notice = Arc::new(std::sync::Mutex::new(None));
        let airplay_device_volume = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let playback_epoch = Arc::new(AtomicU64::new(0));
        let stream_queue_len = Arc::new(AtomicUsize::new(0));
        let stream_auto_advance_pending = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::new(AtomicBool::new(false));

        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();

        let worker_shared = WorkerShared {
            shutdown: Arc::clone(&worker_shutdown),
            state: Arc::clone(&state),
            file_name: Arc::clone(&file_name),
            track_tags: Arc::clone(&track_tags),
            track_cover: Arc::clone(&track_cover),
            cover_version: Arc::clone(&cover_version),
            device_name: Arc::clone(&device_name),
            output_notice: Arc::clone(&output_notice),
            airplay_device_volume: Arc::clone(&airplay_device_volume),
            playback_epoch: Arc::clone(&playback_epoch),
            stream_queue_len: Arc::clone(&stream_queue_len),
            stream_auto_advance_pending: Arc::clone(&stream_auto_advance_pending),
        };

        let worker_thread = spawn_audio_worker(worker_shared, command_rx);

        Self {
            state,
            file_name,
            track_tags,
            track_cover,
            cover_version,
            device_name,
            eq_config,
            output_notice,
            airplay_device_volume,
            playback_epoch,
            stream_queue_len,
            stream_auto_advance_pending,
            worker_shutdown,
            command_tx: Some(command_tx),
            worker_thread: Some(worker_thread),
        }
    }

    fn send_command(&self, cmd: PlayerCommand) {
        if self
            .command_tx
            .as_ref()
            .is_none_or(|command_tx| command_tx.send(cmd).is_err())
        {
            eprintln!("AudioWorker: command channel closed");
        }
    }

    fn mark_playback_change(&self) -> u64 {
        self.playback_epoch.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot_impl(true)
    }

    /// Snapshot that leaves `track_cover` as `None`. Embedded cover art can run
    /// to megabytes, so status loops that poll many times per second must use
    /// this instead of `snapshot()` to avoid cloning the image every tick.
    pub fn snapshot_no_cover(&self) -> PlayerSnapshot {
        self.snapshot_impl(false)
    }

    fn snapshot_impl(&self, include_cover: bool) -> PlayerSnapshot {
        let state = PlaybackState::from_id(self.state.state.load(Ordering::Relaxed));
        let file_name = self.file_name.lock().unwrap().clone();
        let track_tags = self.track_tags.lock().unwrap().clone();
        let track_cover = if include_cover {
            self.track_cover.lock().unwrap().clone()
        } else {
            None
        };
        let cover_version = self.cover_version.load(Ordering::Relaxed);
        let device_name = self.device_name.lock().unwrap().clone();
        let eq_config = self.eq_config.lock().unwrap().clone();
        let output_notice = self.output_notice.lock().unwrap().clone();
        let source_rate = self.state.source_rate.load(Ordering::Relaxed);
        let source_bits = self.state.source_bits.load(Ordering::Relaxed);
        let target_rate = self.state.target_rate.load(Ordering::Relaxed);
        let target_bits = self.state.target_bits.load(Ordering::Relaxed);
        let output_mode = OutputMode::from_id(self.state.output_mode.load(Ordering::Relaxed));
        let active_output_mode =
            OutputMode::from_id(self.state.active_output_mode.load(Ordering::Relaxed));
        let output_transport =
            OutputTransport::from_id(self.state.output_transport.load(Ordering::Relaxed));
        let dsd_buffer_health = self.dsd_buffer_health_snapshot(output_transport);
        let diagnostics = self.diagnostics_snapshot(output_transport, dsd_buffer_health.as_ref());

        PlayerSnapshot {
            state,
            file_name: file_name.clone(),
            track_tags,
            track_cover,
            cover_version,
            device_name: device_name.clone(),
            eq_config,
            output_notice,
            signal_path: SignalPathSnapshot {
                source_format: file_name,
                source_rate,
                source_bits,
                target_rate,
                target_bits,
                src_path_kind: ResamplerPathKind::from_id(
                    self.state.src_path_kind.load(Ordering::Relaxed),
                ),
                src_capped_fallback: self.state.src_capped_fallback.load(Ordering::Relaxed),
                src_phase_profile_preserved: self
                    .state
                    .src_phase_profile_preserved
                    .load(Ordering::Relaxed),
                src_ratio_num: self.state.src_ratio_num.load(Ordering::Relaxed),
                src_ratio_den: self.state.src_ratio_den.load(Ordering::Relaxed),
                output_device: device_name,
                output_mode,
                active_output_mode,
                output_transport,
                output_notice_id: self.state.output_notice_id.load(Ordering::Relaxed),
                dsd_stability_resets: self.state.dsd_stability_resets.load(Ordering::Relaxed),
            },
            config: PlaybackConfigSnapshot {
                configured_target_rate: self.state.configured_target_rate.load(Ordering::Relaxed),
                upsampling_enabled: self.state.upsampling_enabled.load(Ordering::Relaxed),
                filter_type: FilterType::from_id(self.state.filter_type.load(Ordering::Relaxed)),
                active_filter_type: FilterType::from_id(
                    self.state.active_filter_type.load(Ordering::Relaxed),
                ),
                dither_mode_id: self.state.dither_mode.load(Ordering::Relaxed),
                output_mode,
                dsd_modulator: DsdModulator::from_id(
                    self.state.dsd_modulator.load(Ordering::Relaxed),
                ),
                dsd_isi_penalty: f32::from_bits(self.state.dsd_isi_penalty.load(Ordering::Relaxed)),
                volume: f32::from_bits(self.state.volume.load(Ordering::Relaxed)),
                headroom_db: f32::from_bits(self.state.headroom_db.load(Ordering::Relaxed)),
                dsp_buffer_ms: self.state.dsp_buffer_ms.load(Ordering::Relaxed),
                exclusive: self.state.exclusive.load(Ordering::Relaxed),
            },
            diagnostics,
            metrics: PlaybackMetricsSnapshot {
                position_samples: self.state.position_samples.load(Ordering::Relaxed),
                duration_samples: self.state.duration_samples.load(Ordering::Relaxed),
                resample_time_ns: self.state.resample_time_ns.load(Ordering::Relaxed),
                dsd_upsample_time_ns: self.state.dsd_upsample_time_ns.load(Ordering::Relaxed),
                dsd_modulate_time_ns: self.state.dsd_modulate_time_ns.load(Ordering::Relaxed),
                dsd_output_pending_samples: self
                    .state
                    .dsd_output_pending_samples
                    .load(Ordering::Relaxed),
                dsd_buffer_health,
                dsd_overbudget_blocks: self.state.dsd_overbudget_blocks.load(Ordering::Relaxed),
                dsd_last_load: f32::from_bits(self.state.dsd_last_load.load(Ordering::Relaxed)),
                dsd_recent_load_p95: f32::from_bits(
                    self.state.dsd_recent_load_p95.load(Ordering::Relaxed),
                ),
                dsd_recent_load_p99: f32::from_bits(
                    self.state.dsd_recent_load_p99.load(Ordering::Relaxed),
                ),
                block_duration_ns: self.state.block_duration_ns.load(Ordering::Relaxed),
                meter_l: f32::from_bits(self.state.meter_l.load(Ordering::Relaxed)),
                meter_r: f32::from_bits(self.state.meter_r.load(Ordering::Relaxed)),
                signal_peak: f32::from_bits(self.state.signal_peak.load(Ordering::Relaxed)),
                signal_peak_max: f32::from_bits(self.state.signal_peak_max.load(Ordering::Relaxed)),
                signal_clipping: self.state.signal_clipping.load(Ordering::Relaxed),
                signal_clip_events: self.state.signal_clip_events.load(Ordering::Relaxed),
                signal_clip_samples: self.state.signal_clip_samples.load(Ordering::Relaxed),
                dsd_limiter_peak_ratio: f32::from_bits(
                    self.state.dsd_limiter_peak_ratio.load(Ordering::Relaxed),
                ),
                dsd_limiter_peak_ratio_max: f32::from_bits(
                    self.state
                        .dsd_limiter_peak_ratio_max
                        .load(Ordering::Relaxed),
                ),
                dsd_limiter_active: self.state.dsd_limiter_active.load(Ordering::Relaxed),
                dsd_limiter_events: self.state.dsd_limiter_events.load(Ordering::Relaxed),
                dsd_limiter_samples: self.state.dsd_limiter_samples.load(Ordering::Relaxed),
                underrun_events: self.state.underrun_events.load(Ordering::Relaxed),
                underrun_samples: self.state.underrun_samples.load(Ordering::Relaxed),
            },
        }
    }

    fn diagnostics_snapshot(
        &self,
        output_transport: OutputTransport,
        dsd_buffer_health: Option<&DsdBufferHealth>,
    ) -> PlaybackDiagnosticsSnapshot {
        let (output_ring_fill_now_ms, output_ring_fill_min_ms) =
            if let Some(health) = dsd_buffer_health {
                (health.ring_fill_ms, health.ring_low_watermark_ms)
            } else {
                self.pcm_buffer_fill_ms_snapshot(output_transport)
                    .unwrap_or((0.0, 0.0))
            };

        PlaybackDiagnosticsSnapshot {
            output_ring_fill_now_ms,
            output_ring_fill_min_ms,
            startup_ring_low_watermark_ms: self.startup_ring_low_watermark_ms_snapshot(),
            startup_ready_ms: self.state.startup_ready_ms.load(Ordering::Relaxed),
            startup_first_render_block_ms: nanos_to_millis(
                self.state
                    .startup_first_render_block_ns
                    .load(Ordering::Relaxed),
            ),
            startup_producer_over_budget_count: self
                .state
                .startup_overbudget_blocks
                .load(Ordering::Relaxed),
            startup_callback_gaps_ms: self.startup_callback_gaps_ms_snapshot(),
            underrun_count: self.state.underrun_events.load(Ordering::Relaxed),
            producer_over_budget_count: self.state.dsd_overbudget_blocks.load(Ordering::Relaxed),
            max_render_block_ms: nanos_to_millis(
                self.state.max_render_block_ns.load(Ordering::Relaxed),
            ),
            max_audio_callback_gap_ms: nanos_to_millis(
                self.state.max_audio_callback_gap_ns.load(Ordering::Relaxed),
            ),
            dsp_graph_rebuild_count: self.state.dsp_graph_rebuild_count.load(Ordering::Relaxed),
            sample_rate_change_count: self.state.sample_rate_change_count.load(Ordering::Relaxed),
            dop_alignment_reset_count: self.state.dop_alignment_reset_count.load(Ordering::Relaxed),
            coreaudio_dop_open_count: self.state.coreaudio_dop_open_count.load(Ordering::Relaxed),
            coreaudio_dop_start_count: self.state.coreaudio_dop_start_count.load(Ordering::Relaxed),
            coreaudio_dop_stop_count: self.state.coreaudio_dop_stop_count.load(Ordering::Relaxed),
            coreaudio_dop_drop_count: self.state.coreaudio_dop_drop_count.load(Ordering::Relaxed),
            coreaudio_dop_quiesce_count: self
                .state
                .coreaudio_dop_quiesce_count
                .load(Ordering::Relaxed),
            coreaudio_dop_last_lifecycle_event_id: self
                .state
                .coreaudio_dop_last_lifecycle_event_id
                .load(Ordering::Relaxed),
            coreaudio_dop_last_lifecycle_at_ms: self
                .state
                .coreaudio_dop_last_lifecycle_at_ms
                .load(Ordering::Relaxed),
            reopen_reason_count: self.state.reopen_reason_count.load(Ordering::Relaxed),
            last_reopen_reason_id: self.state.last_reopen_reason_id.load(Ordering::Relaxed),
            last_reopen_reason_at_ms: self.state.last_reopen_reason_at_ms.load(Ordering::Relaxed),
            flush_reason_count: self.state.flush_reason_count.load(Ordering::Relaxed),
            last_flush_reason_id: self.state.last_flush_reason_id.load(Ordering::Relaxed),
            last_flush_reason_at_ms: self.state.last_flush_reason_at_ms.load(Ordering::Relaxed),
            modulator_reset_count: self.state.modulator_reset_count.load(Ordering::Relaxed),
            decoder_starved_count: self.state.decoder_starved_count.load(Ordering::Relaxed),
            source_read_time_ms: nanos_to_millis(
                self.state.source_read_time_ns.load(Ordering::Relaxed),
            ),
            max_source_read_ms: nanos_to_millis(
                self.state.max_source_read_ns.load(Ordering::Relaxed),
            ),
            source_read_stall_count: self.state.source_read_stall_count.load(Ordering::Relaxed),
            source_read_stall_last_at_ms: self
                .state
                .source_read_stall_last_at_ms
                .load(Ordering::Relaxed),
            decoder_decode_time_ms: nanos_to_millis(
                self.state.decoder_decode_time_ns.load(Ordering::Relaxed),
            ),
            max_decoder_decode_ms: nanos_to_millis(
                self.state.max_decoder_decode_ns.load(Ordering::Relaxed),
            ),
            decoder_decode_stall_count: self
                .state
                .decoder_decode_stall_count
                .load(Ordering::Relaxed),
            decoder_decode_stall_last_at_ms: self
                .state
                .decoder_decode_stall_last_at_ms
                .load(Ordering::Relaxed),
            lock_wait_max_ms: nanos_to_millis(self.state.lock_wait_max_ns.load(Ordering::Relaxed)),
        }
    }

    fn startup_ring_low_watermark_ms_snapshot(&self) -> f64 {
        let units = self
            .state
            .startup_ring_low_watermark_units
            .load(Ordering::Relaxed);
        let units_per_sec = self
            .state
            .startup_ring_units_per_sec
            .load(Ordering::Relaxed);
        if units == u64::MAX || units_per_sec == 0 {
            0.0
        } else {
            units as f64 * 1000.0 / units_per_sec as f64
        }
    }

    fn startup_callback_gaps_ms_snapshot(&self) -> Vec<f64> {
        let count = self
            .state
            .startup_callback_gap_count
            .load(Ordering::Relaxed)
            .min(STARTUP_CALLBACK_GAP_SLOTS as u64) as usize;
        self.state.startup_callback_gaps_ns[..count]
            .iter()
            .map(|gap| nanos_to_millis(gap.load(Ordering::Relaxed)))
            .collect()
    }

    fn pcm_buffer_fill_ms_snapshot(&self, output_transport: OutputTransport) -> Option<(f64, f64)> {
        if !matches!(
            output_transport,
            OutputTransport::PcmShared
                | OutputTransport::PcmWasapiExclusive
                | OutputTransport::PcmAsio
                | OutputTransport::PcmAirPlayRaop
                | OutputTransport::PcmAirPlay2
                | OutputTransport::PcmCoreAudio
        ) {
            return None;
        }

        let ring_capacity_samples = self.state.pcm_ring_capacity_samples.load(Ordering::Relaxed);
        let target_rate = self.state.target_rate.load(Ordering::Relaxed);
        if ring_capacity_samples == 0 || target_rate == 0 {
            return None;
        }

        let ring_fill_samples = self.state.pcm_ring_fill_samples.load(Ordering::Relaxed);
        let raw_low_watermark_samples = self
            .state
            .pcm_ring_low_watermark_samples
            .load(Ordering::Relaxed);
        let ring_low_watermark_samples = if raw_low_watermark_samples == u64::MAX
            || raw_low_watermark_samples > ring_capacity_samples
        {
            ring_fill_samples
        } else {
            raw_low_watermark_samples
        };
        let samples_per_ms = target_rate as f64 * 2.0 / 1000.0;

        Some((
            ring_fill_samples as f64 / samples_per_ms,
            ring_low_watermark_samples as f64 / samples_per_ms,
        ))
    }

    fn dsd_buffer_health_snapshot(
        &self,
        output_transport: OutputTransport,
    ) -> Option<DsdBufferHealth> {
        if output_transport != OutputTransport::DopCoreAudio {
            return None;
        }
        let ring_capacity_samples = self.state.dsd_ring_capacity_samples.load(Ordering::Relaxed);
        let dop_frame_rate = self.state.target_rate.load(Ordering::Relaxed) / 16;
        if ring_capacity_samples == 0 || dop_frame_rate == 0 {
            return None;
        }

        let ring_fill_samples = self.state.dsd_ring_fill_samples.load(Ordering::Relaxed);
        let raw_low_watermark_samples = self
            .state
            .dsd_ring_low_watermark_samples
            .load(Ordering::Relaxed);
        let ring_low_watermark_samples = if raw_low_watermark_samples == u64::MAX
            || raw_low_watermark_samples > ring_capacity_samples
        {
            ring_fill_samples
        } else {
            raw_low_watermark_samples
        };
        let callback_frames = self.state.dsd_callback_frames.load(Ordering::Relaxed);
        let requested_hardware_buffer_frames = self
            .state
            .dsd_requested_hardware_buffer_frames
            .load(Ordering::Relaxed);
        let hardware_buffer_min_frames = self
            .state
            .dsd_hardware_buffer_min_frames
            .load(Ordering::Relaxed);
        let hardware_buffer_max_frames = self
            .state
            .dsd_hardware_buffer_max_frames
            .load(Ordering::Relaxed);
        let hardware_buffer_frames = self
            .state
            .dsd_hardware_buffer_frames
            .load(Ordering::Relaxed);
        let samples_per_ms = dop_frame_rate as f64 * 2.0 / 1000.0;
        let frames_per_ms = dop_frame_rate as f64 / 1000.0;

        Some(DsdBufferHealth {
            ring_capacity_samples,
            ring_fill_samples,
            ring_low_watermark_samples,
            ring_capacity_ms: ring_capacity_samples as f64 / samples_per_ms,
            ring_fill_ms: ring_fill_samples as f64 / samples_per_ms,
            ring_low_watermark_ms: ring_low_watermark_samples as f64 / samples_per_ms,
            callback_frames,
            callback_ms: callback_frames as f64 / frames_per_ms,
            requested_hardware_buffer_frames,
            requested_hardware_buffer_ms: requested_hardware_buffer_frames as f64 / frames_per_ms,
            hardware_buffer_min_frames,
            hardware_buffer_max_frames,
            hardware_buffer_frames,
            hardware_buffer_ms: hardware_buffer_frames as f64 / frames_per_ms,
            lock_miss_events: self.state.dsd_lock_miss_events.load(Ordering::Relaxed),
            callback_deadline_miss_events: self
                .state
                .dsd_callback_deadline_miss_events
                .load(Ordering::Relaxed),
            soft_callback_gap_125_events: self
                .state
                .dsd_soft_callback_gap_125_events
                .load(Ordering::Relaxed),
            soft_callback_gap_150_events: self
                .state
                .dsd_soft_callback_gap_150_events
                .load(Ordering::Relaxed),
            soft_callback_gap_175_events: self
                .state
                .dsd_soft_callback_gap_175_events
                .load(Ordering::Relaxed),
            last_soft_callback_gap_ms: nanos_to_millis(
                self.state
                    .dsd_last_soft_callback_gap_ns
                    .load(Ordering::Relaxed),
            ),
            last_soft_callback_gap_at_ms: self
                .state
                .dsd_last_soft_callback_gap_at_ms
                .load(Ordering::Relaxed),
            ring_below_250ms_events: self
                .state
                .dsd_ring_below_250ms_events
                .load(Ordering::Relaxed),
            ring_below_100ms_events: self
                .state
                .dsd_ring_below_100ms_events
                .load(Ordering::Relaxed),
            ring_below_50ms_events: self
                .state
                .dsd_ring_below_50ms_events
                .load(Ordering::Relaxed),
            ring_below_callback_events: self
                .state
                .dsd_ring_below_callback_events
                .load(Ordering::Relaxed),
            last_ring_pressure_at_ms: self
                .state
                .dsd_last_ring_pressure_at_ms
                .load(Ordering::Relaxed),
            marker_error_events: self
                .state
                .dsd_dop_marker_error_events
                .load(Ordering::Relaxed),
            program_idle_splice_events: self
                .state
                .dsd_dop_program_idle_splice_events
                .load(Ordering::Relaxed),
            program_to_idle_events: self
                .state
                .dsd_dop_program_to_idle_events
                .load(Ordering::Relaxed),
            idle_to_program_events: self
                .state
                .dsd_dop_idle_to_program_events
                .load(Ordering::Relaxed),
            mixed_output_events: self
                .state
                .dsd_dop_mixed_output_events
                .load(Ordering::Relaxed),
            last_output_transition_id: self
                .state
                .dsd_dop_last_output_transition_id
                .load(Ordering::Relaxed),
            last_output_transition_at_ms: self
                .state
                .dsd_dop_last_output_transition_at_ms
                .load(Ordering::Relaxed),
            repeated_payload_events: self
                .state
                .dsd_dop_repeated_payload_events
                .load(Ordering::Relaxed),
            callback_index: self.state.dsd_dop_callback_index.load(Ordering::Relaxed),
            last_callback_at_ms: self
                .state
                .dsd_dop_last_callback_at_ms
                .load(Ordering::Relaxed),
            last_callback_gap_ms: nanos_to_millis(
                self.state
                    .dsd_dop_last_callback_gap_ns
                    .load(Ordering::Relaxed),
            ),
            last_callback_frames: self
                .state
                .dsd_dop_last_callback_frames
                .load(Ordering::Relaxed),
            last_output_kind_id: self
                .state
                .dsd_dop_last_output_kind_id
                .load(Ordering::Relaxed),
            last_ring_fill_samples: self
                .state
                .dsd_dop_last_ring_fill_samples
                .load(Ordering::Relaxed),
            last_program_read_samples: self
                .state
                .dsd_dop_last_program_read_samples
                .load(Ordering::Relaxed),
            ring_read_cursor_samples: self
                .state
                .dsd_dop_ring_read_cursor_samples
                .load(Ordering::Relaxed),
            last_payload_fingerprint: self
                .state
                .dsd_dop_last_payload_fingerprint
                .load(Ordering::Relaxed),
            last_payload_fingerprint_at_ms: self
                .state
                .dsd_dop_last_payload_fingerprint_at_ms
                .load(Ordering::Relaxed),
            marker_scan_count: self.state.dsd_dop_marker_scan_count.load(Ordering::Relaxed),
            every_callback_scan_enabled: self
                .state
                .dsd_dop_every_callback_scan_enabled
                .load(Ordering::Relaxed),
            last_underrun_at_ms: self.state.dsd_last_underrun_at_ms.load(Ordering::Relaxed),
        })
    }

    pub fn signal_path(&self) -> SignalPathSnapshot {
        self.snapshot().signal_path
    }

    pub fn current_file_name(&self) -> Option<String> {
        self.file_name.lock().unwrap().clone()
    }

    pub fn current_tags(&self) -> TrackTags {
        self.track_tags.lock().unwrap().clone()
    }

    pub fn current_cover(&self) -> Option<TrackCover> {
        self.track_cover.lock().unwrap().clone()
    }

    pub fn cover_version(&self) -> u64 {
        self.cover_version.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn set_cover_version_for_test(&self, version: u64) {
        self.cover_version.store(version, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn set_cover_for_test(&self, cover: Option<TrackCover>) {
        *self.track_cover.lock().unwrap() = cover;
        self.cover_version.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn set_playback_state_for_test(&self, state: PlaybackState) {
        let id = match state {
            PlaybackState::Stopped => super::state::PLAYBACK_STOPPED,
            PlaybackState::Starting => PLAYBACK_STARTING,
            PlaybackState::Playing => PLAYBACK_PLAYING,
            PlaybackState::Paused => PLAYBACK_PAUSED,
        };
        self.state.state.store(id, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn set_current_file_name_for_test(&self, file_name: Option<String>) {
        *self.file_name.lock().unwrap() = file_name;
    }

    #[cfg(test)]
    pub fn set_coreaudio_dop_buffer_health_for_test(
        &self,
        target_rate: u32,
        capacity_samples: u64,
        fill_samples: u64,
        low_watermark_samples: u64,
        hardware_buffer_frames: u32,
    ) {
        self.state.target_rate.store(target_rate, Ordering::Relaxed);
        self.state
            .output_transport
            .store(OutputTransport::DopCoreAudio.as_id(), Ordering::Relaxed);
        self.state
            .dsd_ring_capacity_samples
            .store(capacity_samples, Ordering::Relaxed);
        self.state
            .dsd_ring_fill_samples
            .store(fill_samples, Ordering::Relaxed);
        self.state
            .dsd_ring_low_watermark_samples
            .store(low_watermark_samples, Ordering::Relaxed);
        self.state
            .dsd_callback_frames
            .store(hardware_buffer_frames, Ordering::Relaxed);
        let min_frames = if hardware_buffer_frames == 0 {
            0
        } else {
            hardware_buffer_frames.saturating_div(2).max(1)
        };
        self.state
            .dsd_hardware_buffer_min_frames
            .store(min_frames, Ordering::Relaxed);
        self.state
            .dsd_hardware_buffer_max_frames
            .store(hardware_buffer_frames.saturating_mul(2), Ordering::Relaxed);
        self.state
            .dsd_hardware_buffer_frames
            .store(hardware_buffer_frames, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn set_pcm_buffer_health_for_test(
        &self,
        target_rate: u32,
        capacity_samples: u64,
        fill_samples: u64,
        low_watermark_samples: u64,
    ) {
        self.state.target_rate.store(target_rate, Ordering::Relaxed);
        self.state
            .output_transport
            .store(OutputTransport::PcmShared.as_id(), Ordering::Relaxed);
        self.state
            .pcm_ring_capacity_samples
            .store(capacity_samples, Ordering::Relaxed);
        self.state
            .pcm_ring_fill_samples
            .store(fill_samples, Ordering::Relaxed);
        self.state
            .pcm_ring_low_watermark_samples
            .store(low_watermark_samples, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn set_playback_diagnostics_for_test(&self) {
        self.state.underrun_events.store(3, Ordering::Relaxed);
        self.state.dsd_overbudget_blocks.store(4, Ordering::Relaxed);
        self.state.record_render_block_ns(2_500_000);
        self.state.record_audio_callback_gap_ns(3_750_000);
        self.state
            .dsp_graph_rebuild_count
            .store(5, Ordering::Relaxed);
        self.state
            .sample_rate_change_count
            .store(6, Ordering::Relaxed);
        self.state
            .dop_alignment_reset_count
            .store(7, Ordering::Relaxed);
        self.state.modulator_reset_count.store(8, Ordering::Relaxed);
        self.state.decoder_starved_count.store(9, Ordering::Relaxed);
        self.state.dsd_lock_miss_events.store(10, Ordering::Relaxed);
        self.state
            .dsd_callback_deadline_miss_events
            .store(11, Ordering::Relaxed);
        self.state
            .dsd_soft_callback_gap_125_events
            .store(15, Ordering::Relaxed);
        self.state
            .dsd_soft_callback_gap_150_events
            .store(16, Ordering::Relaxed);
        self.state
            .dsd_soft_callback_gap_175_events
            .store(17, Ordering::Relaxed);
        self.state
            .dsd_last_soft_callback_gap_ns
            .store(12_500_000, Ordering::Relaxed);
        self.state
            .dsd_last_soft_callback_gap_at_ms
            .store(1_765_000_000_100, Ordering::Relaxed);
        self.state
            .dsd_ring_below_250ms_events
            .store(18, Ordering::Relaxed);
        self.state
            .dsd_ring_below_100ms_events
            .store(19, Ordering::Relaxed);
        self.state
            .dsd_ring_below_50ms_events
            .store(20, Ordering::Relaxed);
        self.state
            .dsd_ring_below_callback_events
            .store(21, Ordering::Relaxed);
        self.state
            .dsd_last_ring_pressure_at_ms
            .store(1_765_000_000_200, Ordering::Relaxed);
        self.state
            .dsd_dop_marker_error_events
            .store(12, Ordering::Relaxed);
        self.state
            .dsd_dop_program_idle_splice_events
            .store(13, Ordering::Relaxed);
        self.state
            .dsd_dop_program_to_idle_events
            .store(22, Ordering::Relaxed);
        self.state
            .dsd_dop_idle_to_program_events
            .store(23, Ordering::Relaxed);
        self.state
            .dsd_dop_mixed_output_events
            .store(24, Ordering::Relaxed);
        self.state
            .dsd_dop_last_output_transition_id
            .store(2, Ordering::Relaxed);
        self.state
            .dsd_dop_last_output_transition_at_ms
            .store(1_765_000_000_300, Ordering::Relaxed);
        self.state
            .dsd_dop_repeated_payload_events
            .store(14, Ordering::Relaxed);
        self.state
            .dsd_last_underrun_at_ms
            .store(1_765_000_000_000, Ordering::Relaxed);
        self.state
            .record_coreaudio_dop_lifecycle(super::state::COREAUDIO_DOP_LIFECYCLE_OPEN_ATTEMPT);
        self.state
            .record_coreaudio_dop_lifecycle(super::state::COREAUDIO_DOP_LIFECYCLE_START);
        self.state
            .record_reopen_reason(super::state::REOPEN_REASON_SET_OUTPUT_MODE);
        self.state.request_flush(super::state::FLUSH_REASON_REOPEN);
        self.state.record_source_read_ns(4_500_000, 10_000_000);
        self.state.record_source_read_ns(12_000_000, 10_000_000);
        self.state.record_decoder_decode_ns(3_000_000, 10_000_000);
        self.state.record_decoder_decode_ns(15_000_000, 10_000_000);
        self.state.record_lock_wait_ns(1_250_000);
    }

    pub fn selected_device_name(&self) -> Option<String> {
        self.device_name.lock().unwrap().clone()
    }

    pub fn eq_config(&self) -> EqConfig {
        self.eq_config.lock().unwrap().clone()
    }

    pub fn playback_state(&self) -> PlaybackState {
        PlaybackState::from_id(self.state.state.load(Ordering::Relaxed))
    }

    pub fn output_mode(&self) -> OutputMode {
        OutputMode::from_id(self.state.output_mode.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.5);
        let previous = f32::from_bits(self.state.volume.swap(volume.to_bits(), Ordering::Relaxed));
        if (previous - volume).abs() > f32::EPSILON {
            self.state.reset_signal_level_metrics();
        }
    }

    pub fn playback_epoch(&self) -> u64 {
        self.playback_epoch.load(Ordering::Relaxed)
    }

    pub fn reserve_playback_change(&self) -> u64 {
        self.playback_epoch.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn has_stream_auto_advance_in_flight(&self) -> bool {
        self.stream_auto_advance_pending.load(Ordering::Relaxed)
    }

    pub fn stream_queue_len(&self) -> usize {
        self.stream_queue_len.load(Ordering::Relaxed)
    }

    pub fn play_if_epoch(
        &self,
        expected_epoch: u64,
        file_path: String,
        fallback_cover: Option<TrackCover>,
        fallback_tags: Option<TrackTags>,
        queue: Vec<QueueItem>,
    ) -> bool {
        if self
            .playback_epoch
            .compare_exchange(
                expected_epoch,
                expected_epoch + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return false;
        }
        self.send_command(PlayerCommand::Play {
            epoch: expected_epoch + 1,
            file_path,
            fallback_cover,
            fallback_tags,
            queue,
        });
        true
    }

    // Epoch-guarded playback needs the complete stream handoff payload at one synchronization point.
    #[allow(clippy::too_many_arguments)]
    pub fn play_stream_if_epoch(
        &self,
        expected_epoch: u64,
        source: Box<dyn MediaSource>,
        ext_hint: Option<String>,
        display_name: String,
        fallback_cover: Option<TrackCover>,
        fallback_tags: Option<TrackTags>,
        queue: Vec<StreamQueueItem>,
    ) -> bool {
        if self
            .playback_epoch
            .compare_exchange(
                expected_epoch,
                expected_epoch + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return false;
        }
        self.send_command(PlayerCommand::PlayStream {
            epoch: expected_epoch + 1,
            source,
            ext_hint,
            display_name,
            fallback_cover,
            fallback_tags,
            queue,
        });
        true
    }

    pub fn set_stream_queue_if_epoch(
        &self,
        queue: Vec<StreamQueueItem>,
        expected_current: Option<String>,
        expected_epoch: Option<u64>,
    ) {
        self.send_command(PlayerCommand::SetStreamQueue {
            queue,
            expected_current,
            expected_epoch,
        });
    }

    pub fn set_repeat_one(&self, repeat_one: bool) {
        self.send_command(PlayerCommand::SetRepeatOne { repeat_one });
    }

    pub fn next(&self) {
        let epoch = self.mark_playback_change();
        self.send_command(PlayerCommand::Next { epoch });
    }

    pub fn set_queue_if_epoch(&self, queue: Vec<QueueItem>, expected_epoch: Option<u64>) {
        self.send_command(PlayerCommand::SetQueue {
            queue,
            expected_epoch,
        });
    }

    pub fn pause(&self) {
        if self.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING {
            self.state.state.store(PLAYBACK_PAUSED, Ordering::Relaxed);
        }
        self.send_command(PlayerCommand::Pause);
    }

    pub fn resume(&self) {
        self.send_command(PlayerCommand::Resume);
    }

    pub fn stop(&self) {
        let epoch = self.mark_playback_change();
        self.state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
        self.send_command(PlayerCommand::Stop { epoch });
    }

    pub fn seek(&self, seconds: f64) {
        self.send_command(PlayerCommand::Seek { seconds });
    }

    pub fn update_config(
        &self,
        filter_type: FilterType,
        target_rate: u32,
        upsampling_enabled: bool,
        exclusive: bool,
        dsp_buffer_ms: u32,
    ) {
        let dsp_buffer_ms = dsp_buffer_ms.min(MAX_DSP_BUFFER_MS);
        self.state
            .filter_type
            .store(filter_type.as_id(), Ordering::Relaxed);
        if !OutputMode::from_id(self.state.output_mode.load(Ordering::Relaxed)).is_dsd() {
            self.state
                .active_filter_type
                .store(filter_type.as_id(), Ordering::Relaxed);
        }
        self.state
            .configured_target_rate
            .store(target_rate, Ordering::Relaxed);
        self.state
            .upsampling_enabled
            .store(upsampling_enabled, Ordering::Relaxed);
        self.state.exclusive.store(exclusive, Ordering::Relaxed);
        self.state
            .dsp_buffer_ms
            .store(dsp_buffer_ms, Ordering::Relaxed);
        self.send_command(PlayerCommand::UpdateConfig {
            filter_type,
            target_rate,
            upsampling_enabled,
            exclusive,
            dsp_buffer_ms,
        });
    }

    pub fn apply_playback_config(&self, mut config: LivePlaybackConfig) {
        config.dsp_buffer_ms = config.dsp_buffer_ms.min(MAX_DSP_BUFFER_MS);
        config.dsd_isi_penalty = clamp_dsd_isi_penalty(config.dsd_isi_penalty);

        self.state
            .filter_type
            .store(config.filter_type.as_id(), Ordering::Relaxed);
        if !config.output_mode.is_dsd() {
            self.state
                .active_filter_type
                .store(config.filter_type.as_id(), Ordering::Relaxed);
        }
        self.state
            .configured_target_rate
            .store(config.target_rate, Ordering::Relaxed);
        self.state
            .upsampling_enabled
            .store(config.upsampling_enabled, Ordering::Relaxed);
        self.state
            .exclusive
            .store(config.exclusive, Ordering::Relaxed);
        self.state
            .dsp_buffer_ms
            .store(config.dsp_buffer_ms, Ordering::Relaxed);
        self.state
            .output_mode
            .store(config.output_mode.as_id(), Ordering::Relaxed);
        if !config.output_mode.is_dsd() {
            self.state
                .active_output_mode
                .store(OutputMode::Pcm.as_id(), Ordering::Relaxed);
        }
        self.state
            .dsd_modulator
            .store(config.dsd_modulator.as_id(), Ordering::Relaxed);
        self.state
            .dsd_isi_penalty
            .store(config.dsd_isi_penalty.to_bits(), Ordering::Relaxed);
        if let Some(eq) = config.eq.clone() {
            *self.eq_config.lock().unwrap() = eq;
        }

        self.send_command(PlayerCommand::ApplyPlaybackConfig { config });
    }

    pub fn select_device(&self, name: Option<String>) {
        self.send_command(PlayerCommand::SelectDevice { name });
    }

    pub fn reopen_output(&self) {
        self.send_command(PlayerCommand::ReopenOutput);
    }

    pub fn set_dither_mode(&self, mode_id: u32) {
        self.state.dither_mode.store(mode_id, Ordering::Relaxed);
    }

    pub fn update_eq(&self, config: EqConfig) {
        *self.eq_config.lock().unwrap() = config.clone();
        self.send_command(PlayerCommand::UpdateEq(config));
    }

    pub fn set_output_mode(&self, mode: OutputMode) {
        self.state
            .output_mode
            .store(mode.as_id(), Ordering::Relaxed);
        self.send_command(PlayerCommand::SetOutputMode { mode });
    }

    pub fn set_dsd_rules(&self, rules: Vec<DsdSourceRule>) {
        self.send_command(PlayerCommand::SetDsdRules { rules });
    }

    pub fn set_dsd_modulator(&self, modulator: DsdModulator) {
        self.state
            .dsd_modulator
            .store(modulator.as_id(), Ordering::Relaxed);
        self.send_command(PlayerCommand::SetDsdModulator { modulator });
    }

    pub fn set_dsd_isi_penalty(&self, penalty: f32) {
        let penalty = clamp_dsd_isi_penalty(penalty);
        self.state
            .dsd_isi_penalty
            .store(penalty.to_bits(), Ordering::Relaxed);
        self.send_command(PlayerCommand::SetDsdIsiPenalty { penalty });
    }

    pub fn set_headroom_db(&self, db: f32) {
        let db = clamp_headroom_db(db);
        let previous = f32::from_bits(self.state.headroom_db.swap(db.to_bits(), Ordering::Relaxed));
        if (previous - db).abs() > f32::EPSILON {
            self.state.reset_signal_level_metrics();
        }
    }

    pub fn set_airplay_device_volume(&self, volume: f32) {
        let volume = airplay::normalize_device_volume(volume).unwrap_or(0.0);
        self.airplay_device_volume
            .store(volume.to_bits(), Ordering::Relaxed);
        self.send_command(PlayerCommand::SetAirPlayVolume { volume });
    }

    pub fn airplay_device_volume(&self) -> Option<f32> {
        airplay::normalize_device_volume(f32::from_bits(
            self.airplay_device_volume.load(Ordering::Relaxed),
        ))
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // `Player` is shared through `Arc`, so this only runs after the last
        // owner is gone. Signal long-running worker steps before closing the
        // command channel. Dropping the JoinHandle detaches the thread instead
        // of blocking the caller: cooperative DSP/backpressure/drain paths exit
        // promptly, while an in-flight source read or OS device open retains
        // only worker-owned state until that external operation returns.
        self.worker_shutdown.store(true, Ordering::Release);
        self.command_tx.take();
        drop(self.worker_thread.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_config_is_visible_in_immediate_snapshot() {
        let player = Player::new();

        player.update_config(FilterType::SincExtreme32k, 96_000, false, false, 250);

        let config = player.snapshot().config;
        assert_eq!(config.filter_type, Some(FilterType::SincExtreme32k));
        assert_eq!(config.configured_target_rate, 96_000);
        assert!(!config.upsampling_enabled);
        assert!(!config.exclusive);
        assert_eq!(config.dsp_buffer_ms, 250);
    }

    #[test]
    fn new_player_starts_with_dsp_disabled() {
        let player = Player::new();

        assert!(!player.snapshot().config.upsampling_enabled);
    }

    #[test]
    fn apply_playback_config_is_visible_in_immediate_snapshot() {
        let player = Player::new();

        player.apply_playback_config(LivePlaybackConfig {
            filter_type: FilterType::Minimum16k,
            target_rate: 176_400,
            upsampling_enabled: true,
            exclusive: false,
            dsp_buffer_ms: MAX_DSP_BUFFER_MS + 1,
            output_mode: OutputMode::Pcm,
            dsd_modulator: DsdModulator::default(),
            dsd_isi_penalty: 1.0,
            dsd_rules: Vec::new(),
            eq: Some(EqConfig::default()),
        });

        let config = player.snapshot().config;
        assert_eq!(config.filter_type, Some(FilterType::Minimum16k));
        assert_eq!(config.active_filter_type, Some(FilterType::Minimum16k));
        assert_eq!(config.configured_target_rate, 176_400);
        assert!(config.upsampling_enabled);
        assert!(!config.exclusive);
        assert_eq!(config.dsp_buffer_ms, MAX_DSP_BUFFER_MS);
        assert_eq!(config.output_mode, OutputMode::Pcm);
        assert_eq!(config.dsd_isi_penalty, 0.05);
    }

    #[test]
    fn pcm_buffer_health_feeds_generic_ring_diagnostics() {
        let player = Player::new();

        player.set_playback_state_for_test(PlaybackState::Playing);
        player.set_pcm_buffer_health_for_test(192_000, 384_000, 192_000, 96_000);

        let diagnostics = player.snapshot().diagnostics;

        assert!((diagnostics.output_ring_fill_now_ms - 500.0).abs() < 0.001);
        assert!((diagnostics.output_ring_fill_min_ms - 250.0).abs() < 0.001);
    }

    #[test]
    fn pcm_unmeasured_low_watermark_uses_current_fill() {
        let player = Player::new();

        player.set_playback_state_for_test(PlaybackState::Playing);
        player.set_pcm_buffer_health_for_test(192_000, 384_000, 192_000, u64::MAX);

        let diagnostics = player.snapshot().diagnostics;

        assert!((diagnostics.output_ring_fill_min_ms - 500.0).abs() < 0.001);
    }

    #[test]
    fn worker_lifetime_follows_last_player_owner() {
        let player = Arc::new(Player::new());
        let state = Arc::downgrade(&player.state);
        let shutdown = Arc::clone(&player.worker_shutdown);
        let remaining_owner = Arc::clone(&player);

        drop(player);
        remaining_owner.select_device(Some("still-owned".to_string()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while remaining_owner.snapshot().device_name.as_deref() != Some("still-owned")
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            remaining_owner.snapshot().device_name.as_deref(),
            Some("still-owned"),
            "dropping one Arc owner must not stop the worker"
        );

        drop(remaining_owner);
        assert!(shutdown.load(Ordering::Acquire));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while state.upgrade().is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            state.upgrade().is_none(),
            "the detached worker should cooperatively release its state"
        );
    }
}
