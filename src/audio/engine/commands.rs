use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsp::eq::EqConfig;
use crate::audio::dsp::resampler::FilterType;
use crate::audio::engine::signal_path::OutputMode;
use crate::settings::DsdSourceRule;
use symphonia::core::io::MediaSource;

use super::metadata::{TrackCover, TrackTags};
use super::session::PlaybackSession;

#[derive(Clone)]
pub struct QueueItem {
    pub file_path: String,
    /// Cover supplied by the library — used only if the file itself has no embedded
    /// or sidecar artwork. Lets user-uploaded album covers show in the player bar.
    pub fallback_cover: Option<TrackCover>,
    /// Metadata supplied by the source when a cached stream has no embedded tags.
    pub fallback_tags: Option<TrackTags>,
}

pub struct StreamQueueItem {
    pub source: Box<dyn MediaSource>,
    pub ext_hint: Option<String>,
    pub display_name: String,
    pub fallback_cover: Option<TrackCover>,
    pub fallback_tags: Option<TrackTags>,
}

/// A fully probed, optionally pre-seeked stream session. Preparing this away
/// from the audio worker lets an explicit source handoff keep the current
/// output ring playing while network and decoder setup complete.
pub struct PreparedStream {
    pub(super) session: PlaybackSession,
    pub(super) tags: TrackTags,
    pub(super) cover: Option<TrackCover>,
    pub(super) start_position_secs: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeamlessHandoffBoundary {
    pub epoch: u64,
    /// Source position immediately after every old-source sample already
    /// accepted by the output and DSP pipelines.
    pub position_secs: f64,
    /// Program-audio duration currently available to cover destination setup.
    pub output_cushion_secs: f64,
}

#[derive(Clone)]
pub struct LivePlaybackConfig {
    pub filter_type: FilterType,
    pub target_rate: u32,
    pub upsampling_enabled: bool,
    pub exclusive: bool,
    pub dsp_buffer_ms: u32,
    pub output_mode: OutputMode,
    pub dsd_modulator: DsdModulator,
    pub dsd_isi_penalty: f32,
    pub dsd_rules: Vec<DsdSourceRule>,
    pub eq: Option<EqConfig>,
}

pub enum PlayerCommand {
    Play {
        epoch: u64,
        file_path: String,
        fallback_cover: Option<TrackCover>,
        fallback_tags: Option<TrackTags>,
        /// Replaces the queue used for auto-advance after this track ends.
        queue: Vec<QueueItem>,
    },
    /// Play a track from an already-open progressive stream (e.g. Qobuz).
    /// The queue contains already-open follow-up streams for EOF auto-advance.
    PlayStream {
        epoch: u64,
        source: Box<dyn MediaSource>,
        ext_hint: Option<String>,
        display_name: String,
        fallback_cover: Option<TrackCover>,
        fallback_tags: Option<TrackTags>,
        queue: Vec<StreamQueueItem>,
    },
    /// Install an already-probed stream. `preserve_output` requests a
    /// continuous handoff through the current output ring; incompatible output
    /// rates still fall back to the ordinary protected transition.
    PlayPreparedStream {
        epoch: u64,
        prepared: Box<PreparedStream>,
        display_name: String,
        fallback_cover: Option<TrackCover>,
        fallback_tags: Option<TrackTags>,
        queue: Vec<StreamQueueItem>,
        preserve_output: bool,
    },
    /// Freeze source rendering at an exact media position while allowing the
    /// output callback to drain the already-rendered ring. Used to line up an
    /// external live source before a prepared, output-preserving handoff.
    BeginSeamlessHandoff {
        expected_epoch: u64,
        destination_source_rate: Option<u32>,
        response: tokio::sync::oneshot::Sender<Result<SeamlessHandoffBoundary, String>>,
    },
    /// Resume source rendering after a prepared handoff was abandoned.
    CancelSeamlessHandoff {
        expected_epoch: u64,
    },
    Pause,
    Resume,
    Stop {
        epoch: u64,
    },
    Seek {
        seconds: f64,
    },
    /// Skip to the next queued track. Stops if the queue is empty.
    Next {
        epoch: u64,
    },
    /// Replace the auto-advance queue without touching the currently-playing
    /// track. Used by the Now Playing view's reorder / clear / shuffle so the
    /// in-flight playback isn't restarted.
    SetQueue {
        queue: Vec<QueueItem>,
        expected_epoch: Option<u64>,
    },
    SetStreamQueue {
        queue: Vec<StreamQueueItem>,
        expected_current: Option<String>,
        expected_epoch: Option<u64>,
    },
    SetRepeatOne {
        repeat_one: bool,
    },
    UpdateConfig {
        filter_type: FilterType,
        target_rate: u32,
        upsampling_enabled: bool,
        exclusive: bool,
        dsp_buffer_ms: u32,
    },
    ApplyPlaybackConfig {
        config: LivePlaybackConfig,
    },
    SelectDevice {
        name: Option<String>,
    },
    /// Drop and reopen the current output without changing the selected device.
    /// Used when external hardware has power/input state changes that can leave
    /// an existing OS stream alive but silent.
    ReopenOutput,
    UpdateEq(EqConfig),
    /// Switch the DSP/output path between PCM and DSD-over-PCM. Forces a stream
    /// rebuild because the wire format changes.
    SetOutputMode {
        mode: OutputMode,
    },
    SetDsdRules {
        rules: Vec<DsdSourceRule>,
    },
    SetDsdModulator {
        modulator: DsdModulator,
    },
    SetDsdIsiPenalty {
        penalty: f32,
    },
    SetAirPlayVolume {
        volume: f32,
    },
}
