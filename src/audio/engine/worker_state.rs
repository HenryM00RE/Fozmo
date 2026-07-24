use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsp::eq::{EqConfig, EqProcessor};
use crate::audio::dsp::resampler::{DEFAULT_FILTER_TYPE, FilterType};
use crate::settings::DsdSourceRule;

use super::buffers::{
    AudioConsumer, AudioProducer, DsdWorkerState, ensure_audio_ring_capacity, new_audio_ring,
    ring_buffer_capacity_samples,
};
use super::dsd_path::DsdFallbackKey;
#[cfg(all(target_os = "windows", feature = "asio"))]
use super::dsd_path::NativeDsdFailureCache;
use super::metadata::{TrackCover, TrackTags};
use super::output_stream::ActiveOutput;
use super::queue_state::{PendingStart, WorkerQueues};
use super::session::{DspPath, PlaybackSession};
use super::signal_path::OutputMode;
use super::state::AtomicPlayerState;

#[derive(Clone)]
pub(super) struct WorkerShared {
    pub(super) shutdown: Arc<AtomicBool>,
    pub(super) state: Arc<AtomicPlayerState>,
    pub(super) file_name: Arc<Mutex<Option<String>>>,
    pub(super) track_tags: Arc<Mutex<TrackTags>>,
    pub(super) track_cover: Arc<Mutex<Option<TrackCover>>>,
    pub(super) cover_version: Arc<AtomicU64>,
    pub(super) device_name: Arc<Mutex<Option<String>>>,
    pub(super) output_notice: Arc<Mutex<Option<String>>>,
    pub(super) airplay_device_volume: Arc<AtomicU32>,
    pub(super) playback_epoch: Arc<AtomicU64>,
    pub(super) stream_queue_len: Arc<AtomicUsize>,
    pub(super) stream_auto_advance_pending: Arc<AtomicBool>,
}

impl WorkerShared {
    pub(super) fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

pub(super) struct WorkerPlaybackState {
    pub(super) session: Option<PlaybackSession>,
    pub(super) current_file_path: Option<String>,
    pub(super) current_fallback_tags: Option<TrackTags>,
    pub(super) queues: WorkerQueues,
    pub(super) repeat_one: bool,
    pub(super) pending_start: Option<PendingStart>,
    pub(super) session_epoch: u64,
    pub(super) reopen_output_for_pending_start: bool,
    pub(super) use_transition_preroll: bool,
    pub(super) pending_start_gapless: bool,
    pub(super) gapless_dsp_path: Option<DspPath>,
    pub(super) seamless_handoff_hold: bool,
}

impl WorkerPlaybackState {
    pub(super) fn new(shared: &WorkerShared) -> Self {
        Self {
            session: None,
            current_file_path: None,
            current_fallback_tags: None,
            queues: WorkerQueues::new(
                Arc::clone(&shared.stream_queue_len),
                Arc::clone(&shared.stream_auto_advance_pending),
            ),
            repeat_one: false,
            pending_start: None,
            session_epoch: shared.playback_epoch.load(Ordering::Relaxed),
            reopen_output_for_pending_start: false,
            use_transition_preroll: false,
            pending_start_gapless: false,
            gapless_dsp_path: None,
            seamless_handoff_hold: false,
        }
    }
}

pub(super) struct WorkerOutputState {
    pub(super) active_device_name: Option<String>,
    pub(super) active_stream: Option<ActiveOutput>,
    pub(super) active_stream_opened_at: Option<Instant>,
    pub(super) next_stream_retry: Instant,
    pub(super) dsd_fallback_key: Option<DsdFallbackKey>,
    #[cfg(all(target_os = "windows", feature = "asio"))]
    pub(super) native_dsd_failed_attempts: NativeDsdFailureCache,
}

impl WorkerOutputState {
    pub(super) fn new() -> Self {
        Self {
            active_device_name: None,
            active_stream: None,
            active_stream_opened_at: None,
            next_stream_retry: Instant::now(),
            dsd_fallback_key: None,
            #[cfg(all(target_os = "windows", feature = "asio"))]
            native_dsd_failed_attempts: NativeDsdFailureCache::default(),
        }
    }
}

pub(super) struct WorkerConfigState {
    pub(super) filter_type: FilterType,
    pub(super) configured_target_rate: u32,
    pub(super) upsampling_enabled: bool,
    pub(super) target_rate: u32,
    pub(super) exclusive_mode: bool,
    pub(super) dsp_buffer_ms: u32,
    pub(super) output_mode: OutputMode,
    pub(super) dsd_modulator: DsdModulator,
    pub(super) dsd_isi_penalty: f32,
    pub(super) current_eq_config: EqConfig,
    pub(super) eq_processor: EqProcessor,
    pub(super) dsd_rules: Vec<DsdSourceRule>,
}

impl WorkerConfigState {
    pub(super) fn new(shared: &WorkerShared) -> Self {
        let target_rate = 192000;
        let current_eq_config = EqConfig::default();
        Self {
            filter_type: DEFAULT_FILTER_TYPE,
            configured_target_rate: 0,
            upsampling_enabled: shared.state.upsampling_enabled.load(Ordering::Relaxed),
            target_rate,
            exclusive_mode: true,
            dsp_buffer_ms: shared.state.dsp_buffer_ms.load(Ordering::Relaxed),
            output_mode: OutputMode::from_id(shared.state.output_mode.load(Ordering::Relaxed)),
            dsd_modulator: DsdModulator::from_id(
                shared.state.dsd_modulator.load(Ordering::Relaxed),
            ),
            dsd_isi_penalty: f32::from_bits(shared.state.dsd_isi_penalty.load(Ordering::Relaxed)),
            eq_processor: EqProcessor::new(target_rate, &current_eq_config),
            current_eq_config,
            dsd_rules: Vec::new(),
        }
    }
}

pub(super) struct WorkerBufferState {
    pub(super) ring_capacity: usize,
    pub(super) prod: AudioProducer,
    pub(super) cons_opt: Option<AudioConsumer>,
    pub(super) dsd_state: Option<DsdWorkerState>,
}

impl WorkerBufferState {
    pub(super) fn new(target_rate: u32, dsp_buffer_ms: u32) -> Self {
        let ring_capacity = ring_buffer_capacity_samples(target_rate, dsp_buffer_ms);
        let (prod, cons) = new_audio_ring(target_rate, dsp_buffer_ms);
        Self {
            ring_capacity,
            prod,
            cons_opt: Some(cons),
            dsd_state: None,
        }
    }

    pub(super) fn clear_dsd_state(&mut self) {
        self.dsd_state = None;
    }

    pub(super) fn ensure_pcm_ring_capacity(
        &mut self,
        target_rate: u32,
        dsp_buffer_ms: u32,
    ) -> bool {
        ensure_audio_ring_capacity(
            target_rate,
            dsp_buffer_ms,
            &mut self.prod,
            &mut self.cons_opt,
            &mut self.ring_capacity,
        )
    }

    pub(super) fn reset_pcm_ring(&mut self, target_rate: u32, dsp_buffer_ms: u32) {
        self.ring_capacity = ring_buffer_capacity_samples(target_rate, dsp_buffer_ms);
        let (prod, cons) = new_audio_ring(target_rate, dsp_buffer_ms);
        self.prod = prod;
        self.cons_opt = Some(cons);
    }

    pub(super) fn take_pcm_consumer_after_ring_setup(&mut self) -> AudioConsumer {
        self.cons_opt
            .take()
            .expect("PCM consumer available after ring setup")
    }
}

pub(super) struct WorkerRuntime {
    pub(super) shared: WorkerShared,
    pub(super) playback: WorkerPlaybackState,
    pub(super) output: WorkerOutputState,
    pub(super) config: WorkerConfigState,
    pub(super) buffers: WorkerBufferState,
}

impl WorkerRuntime {
    pub(super) fn new(shared: WorkerShared) -> Self {
        let playback = WorkerPlaybackState::new(&shared);
        let output = WorkerOutputState::new();
        let config = WorkerConfigState::new(&shared);
        let buffers = WorkerBufferState::new(config.target_rate, config.dsp_buffer_ms);
        Self {
            shared,
            playback,
            output,
            config,
            buffers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerBufferState;
    use crate::audio::engine::buffers::{output_pending_len, ring_buffer_capacity_samples};

    #[test]
    fn worker_buffer_state_owns_pcm_ring_capacity_updates() {
        let mut buffers = WorkerBufferState::new(48_000, 0);
        buffers.cons_opt = None;

        assert!(buffers.ensure_pcm_ring_capacity(384_000, 0));

        assert_eq!(
            buffers.ring_capacity,
            ring_buffer_capacity_samples(384_000, 0)
        );
        assert!(buffers.cons_opt.is_some());
        assert_eq!(output_pending_len(None, &buffers.prod), 0);
    }

    #[test]
    fn worker_buffer_state_takes_pcm_consumer_after_ring_setup() {
        let mut buffers = WorkerBufferState::new(48_000, 0);

        let _consumer = buffers.take_pcm_consumer_after_ring_setup();

        assert!(buffers.cons_opt.is_none());
        assert!(buffers.ensure_pcm_ring_capacity(48_000, 0));
        assert!(buffers.cons_opt.is_some());
    }
}
