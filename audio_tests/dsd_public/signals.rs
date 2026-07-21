//! Deterministic source fixtures for the public PCM-to-DSD measurement bench.
//!
//! Samples in this module are always the PCM values presented to the production
//! renderer. `headroom_db` is the attenuation that the caller will apply while
//! rendering. Fixtures that specify an effective level compensate their source
//! amplitudes for that attenuation; the rated stress fixture deliberately does
//! not, because its contract is a 0.999 peak *before* headroom.

use std::f64::consts::PI;
use std::fmt;
use std::ops::Range;

pub const SOURCE_RATE_44K1_HZ: u32 = 44_100;
pub const SOURCE_RATE_176K4_HZ: u32 = 176_400;
pub const LEVEL_SWEEP_FIXTURE_ID: &str = "coherent_level_sweep";
pub const IDLE_TINY_SIGNAL_FIXTURE_ID: &str = "idle_tiny_signal";
pub const STRESS_RATED_FIXTURE_ID: &str = "high_frequency_stress";
pub const STRESS_LEVEL_MATCHED_FIXTURE_ID: &str = "high_frequency_stress_level_matched";
pub const HIRES_MULTITONE_FIXTURE_ID: &str = "hires_multitone";
/// Smallest source-frame guard accepted by the public measurement fixtures.
///
/// The caller may select a larger value from the production reconstruction
/// filter's support, but may not shorten the isolation below this floor.
pub const MIN_FILTER_GUARD_FRAMES: usize = 16_384;

pub const LEVEL_SWEEP_ANALYZE_FRAMES: usize = 16_384;
pub const LEVEL_SWEEP_EFFECTIVE_DBFS: [f64; 4] = [-6.0, -20.0, -60.0, -100.0];
pub const LEVEL_SWEEP_TARGET_HZ: f64 = 1_000.0;
pub const LEVEL_SWEEP_PHASE_RAD: f64 = 0.0;

pub const IDLE_ANALYZE_FRAMES: usize = 16_384;
pub const IDLE_DC_EFFECTIVE_LEFT: f64 = 1.0e-6;
pub const IDLE_DC_EFFECTIVE_RIGHT: f64 = -1.0e-6;
pub const IDLE_TONE_TARGET_HZ: f64 = 100.0;
pub const IDLE_TONE_EFFECTIVE_DBFS: f64 = -120.0;
pub const IDLE_TONE_PHASE_RAD: f64 = 0.37;

pub const STRESS_STEADY_ANALYZE_FRAMES: usize = 16_384;
pub const STRESS_CLEAN_MUTE_FRAMES: usize = 2_048;
/// Backward-compatible name for the fixed clean center of the guarded mute.
pub const STRESS_MUTE_FRAMES: usize = STRESS_CLEAN_MUTE_FRAMES;
pub const STRESS_TARGET_HZ: [f64; 2] = [18_000.0, 19_000.0];
pub const STRESS_PHASES_RAD: [f64; 2] = [0.31, 1.17];
pub const STRESS_PHASE_REVERSAL_RAD: f64 = PI;
pub const STRESS_SOURCE_PEAK: f64 = 0.999;
/// Common post-headroom peak for matched stress comparisons.
///
/// This is the rated fixture's 0.999 source peak after the most conservative
/// production headroom (-4 dB), so matching does not overdrive another path.
pub const STRESS_LEVEL_MATCHED_EFFECTIVE_PEAK: f64 = 0.630_326_387_135_713_1;

pub const HIRES_ANALYZE_FRAMES: usize = 32_768;
pub const HIRES_TARGET_HZ: [f64; 4] = [1_000.0, 18_000.0, 40_000.0, 70_000.0];
pub const HIRES_PHASES_RAD: [f64; 4] = [0.13, 0.71, 1.43, 2.11];
pub const HIRES_EFFECTIVE_PEAK_DBFS: f64 = -6.0;

pub const STRESS_POST_ROLL_RANGE: &str = "stress_post_roll";
pub const HIRES_POST_ROLL_RANGE: &str = "hires_post_roll";

pub const STRESS_SETTLE_RANGE: &str = "stress_settle";
pub const STRESS_STEADY_ANALYSIS_RANGE: &str = "stress_steady_analysis";
pub const STRESS_PRE_MUTE_GUARD_RANGE: &str = "stress_pre_mute_guard";
pub const STRESS_MUTE_ENTRY_TRANSITION_RANGE: &str = "stress_mute_entry_transition";
pub const STRESS_CLEAN_MUTE_RANGE: &str = "stress_clean_mute";
pub const STRESS_PRE_RECOVERY_TRANSITION_RANGE: &str = "stress_pre_recovery_transition";
/// Compatibility marker used by analysis code to locate the start of mute.
pub const STRESS_MUTE_RANGE: &str = STRESS_MUTE_ENTRY_TRANSITION_RANGE;
pub const STRESS_RECOVERY_RANGE: &str = "stress_recovery";
pub const STRESS_LEVEL_MATCHED_POST_ROLL_RANGE: &str = "matched_stress_post_roll";
pub const STRESS_LEVEL_MATCHED_SETTLE_RANGE: &str = "matched_stress_settle";
pub const STRESS_LEVEL_MATCHED_STEADY_ANALYSIS_RANGE: &str = "matched_stress_steady_analysis";
pub const STRESS_LEVEL_MATCHED_PRE_MUTE_GUARD_RANGE: &str = "matched_stress_pre_mute_guard";
pub const STRESS_LEVEL_MATCHED_MUTE_ENTRY_TRANSITION_RANGE: &str =
    "matched_stress_mute_entry_transition";
pub const STRESS_LEVEL_MATCHED_CLEAN_MUTE_RANGE: &str = "matched_stress_clean_mute";
pub const STRESS_LEVEL_MATCHED_PRE_RECOVERY_TRANSITION_RANGE: &str =
    "matched_stress_pre_recovery_transition";
pub const STRESS_LEVEL_MATCHED_MUTE_RANGE: &str = STRESS_LEVEL_MATCHED_MUTE_ENTRY_TRANSITION_RANGE;
pub const STRESS_LEVEL_MATCHED_RECOVERY_RANGE: &str = "matched_stress_recovery";
pub const HIRES_SETTLE_RANGE: &str = "hires_settle";
pub const HIRES_ANALYSIS_RANGE: &str = "hires_analysis";

const LEVEL_PRE_GUARD_RANGE_NAMES: [&str; 4] = [
    "level_-6_dbfs_pre_guard",
    "level_-20_dbfs_pre_guard",
    "level_-60_dbfs_pre_guard",
    "level_-100_dbfs_pre_guard",
];
const LEVEL_ANALYSIS_RANGE_NAMES: [&str; 4] = [
    "level_-6_dbfs_analysis",
    "level_-20_dbfs_analysis",
    "level_-60_dbfs_analysis",
    "level_-100_dbfs_analysis",
];
const LEVEL_CARRIER_NAMES: [&str; 4] = [
    "level_-6_dbfs",
    "level_-20_dbfs",
    "level_-60_dbfs",
    "level_-100_dbfs",
];
const LEVEL_POST_GUARD_RANGE_NAMES: [&str; 4] = [
    "level_-6_dbfs_post_guard",
    "level_-20_dbfs_post_guard",
    "level_-60_dbfs_post_guard",
    "level_-100_dbfs_post_guard",
];

pub const IDLE_SILENCE_PRE_GUARD_RANGE: &str = "silence_pre_guard";
pub const IDLE_SILENCE_ANALYSIS_RANGE: &str = "silence_analysis";
pub const IDLE_SILENCE_POST_GUARD_RANGE: &str = "silence_post_guard";
pub const IDLE_DC_PRE_GUARD_RANGE: &str = "dc_pre_guard";
pub const IDLE_DC_ANALYSIS_RANGE: &str = "dc_analysis";
pub const IDLE_DC_POST_GUARD_RANGE: &str = "dc_post_guard";
pub const IDLE_TONE_PRE_GUARD_RANGE: &str = "tone_100hz_pre_guard";
pub const IDLE_TONE_ANALYSIS_RANGE: &str = "tone_100hz_analysis";
pub const IDLE_TONE_POST_GUARD_RANGE: &str = "tone_100hz_post_guard";

/// The role of a named source-frame range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangePurpose {
    Settle,
    Analyze,
    Guard,
    Mute,
    Recovery,
    PostRoll,
}

/// A half-open interval in source PCM frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRange {
    pub name: &'static str,
    pub purpose: RangePurpose,
    pub start: usize,
    pub end: usize,
}

impl SourceRange {
    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn frames(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// An expected sinusoidal component and the exact coherent carrier generated.
#[derive(Clone, Debug, PartialEq)]
pub struct Carrier {
    pub name: &'static str,
    pub target_hz: f64,
    pub actual_hz: f64,
    pub fft_bin: usize,
    pub coherent_frames: usize,
    pub source_amplitude: f64,
    pub effective_amplitude: f64,
    pub phase_rad: f64,
    pub phase_reversal_rad: Option<f64>,
    pub analysis_ranges: Vec<&'static str>,
}

impl Carrier {
    pub fn source_dbfs(&self) -> f64 {
        amplitude_to_dbfs(self.source_amplitude)
    }

    pub fn effective_dbfs(&self) -> f64 {
        amplitude_to_dbfs(self.effective_amplitude)
    }
}

/// Expected stereo DC values in one analyzed source interval.
#[derive(Clone, Debug, PartialEq)]
pub struct DcOffset {
    pub analysis_range: &'static str,
    pub source_left: f64,
    pub source_right: f64,
    pub effective_left: f64,
    pub effective_right: f64,
}

/// A complete in-memory deterministic stereo PCM fixture.
#[derive(Clone, Debug, PartialEq)]
pub struct StereoSignal {
    pub id: &'static str,
    pub sample_rate_hz: u32,
    pub headroom_db: f64,
    pub headroom_gain: f64,
    pub filter_guard_frames: usize,
    pub left: Vec<f64>,
    pub right: Vec<f64>,
    pub ranges: Vec<SourceRange>,
    pub carriers: Vec<Carrier>,
    pub dc_offsets: Vec<DcOffset>,
}

impl StereoSignal {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn range(&self, name: &str) -> Option<&SourceRange> {
        self.ranges.iter().find(|range| range.name == name)
    }

    pub fn source_peak(&self) -> f64 {
        self.left
            .iter()
            .chain(&self.right)
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()))
    }

    pub fn effective_peak(&self) -> f64 {
        self.source_peak() * self.headroom_gain
    }

    /// Check the invariants relied on by the renderer and analysis code.
    pub fn validate(&self) -> Result<(), SignalError> {
        if self.sample_rate_hz == 0 {
            return Err(SignalError::InvalidMetadata("source sample rate is zero"));
        }
        if !self.headroom_db.is_finite()
            || self.headroom_db > 0.0
            || !self.headroom_gain.is_finite()
            || self.headroom_gain <= 0.0
            || self.headroom_gain > 1.0
        {
            return Err(SignalError::InvalidHeadroomDb(self.headroom_db));
        }
        if !metadata_values_match(self.headroom_gain, dbfs_to_amplitude(self.headroom_db)) {
            return Err(SignalError::InvalidMetadata(
                "headroom gain does not match headroom dB",
            ));
        }
        if self.filter_guard_frames < MIN_FILTER_GUARD_FRAMES {
            return Err(SignalError::InvalidFilterGuardFrames(
                self.filter_guard_frames,
            ));
        }
        if self.left.len() != self.right.len() {
            return Err(SignalError::ChannelLengthMismatch {
                left: self.left.len(),
                right: self.right.len(),
            });
        }

        validate_channel("left", &self.left)?;
        validate_channel("right", &self.right)?;

        let mut cursor = 0;
        for (index, range) in self.ranges.iter().enumerate() {
            if range.is_empty() || range.start != cursor || range.end > self.frames() {
                return Err(SignalError::InvalidRange {
                    name: range.name,
                    start: range.start,
                    end: range.end,
                    frames: self.frames(),
                });
            }
            if self.ranges[..index]
                .iter()
                .any(|previous| previous.name == range.name)
            {
                return Err(SignalError::InvalidMetadata("duplicate source range name"));
            }
            cursor = range.end;
        }
        if cursor != self.frames() {
            return Err(SignalError::InvalidMetadata(
                "source ranges do not cover the complete fixture",
            ));
        }

        for carrier in &self.carriers {
            if !carrier.target_hz.is_finite()
                || !carrier.actual_hz.is_finite()
                || !carrier.source_amplitude.is_finite()
                || !carrier.effective_amplitude.is_finite()
                || !carrier.phase_rad.is_finite()
                || carrier.target_hz <= 0.0
                || carrier.actual_hz <= 0.0
                || carrier.actual_hz >= self.sample_rate_hz as f64 / 2.0
                || carrier.fft_bin == 0
                || carrier.coherent_frames == 0
                || carrier.source_amplitude <= 0.0
                || carrier.effective_amplitude <= 0.0
                || carrier
                    .phase_reversal_rad
                    .is_some_and(|phase| !phase.is_finite())
            {
                return Err(SignalError::InvalidMetadata("invalid carrier metadata"));
            }
            if !metadata_values_match(
                carrier.effective_amplitude,
                carrier.source_amplitude * self.headroom_gain,
            ) {
                return Err(SignalError::InvalidMetadata(
                    "carrier effective amplitude does not match source amplitude and headroom",
                ));
            }
            if carrier.analysis_ranges.is_empty()
                || carrier
                    .analysis_ranges
                    .iter()
                    .any(|name| self.range(name).is_none())
            {
                return Err(SignalError::InvalidMetadata(
                    "carrier references an unknown analysis range",
                ));
            }
        }

        for dc in &self.dc_offsets {
            if self.range(dc.analysis_range).is_none()
                || ![
                    dc.source_left,
                    dc.source_right,
                    dc.effective_left,
                    dc.effective_right,
                ]
                .into_iter()
                .all(f64::is_finite)
            {
                return Err(SignalError::InvalidMetadata("invalid DC metadata"));
            }
            if !metadata_values_match(dc.effective_left, dc.source_left * self.headroom_gain)
                || !metadata_values_match(dc.effective_right, dc.source_right * self.headroom_gain)
            {
                return Err(SignalError::InvalidMetadata(
                    "DC effective values do not match source values and headroom",
                ));
            }
        }

        if self.id == IDLE_TINY_SIGNAL_FIXTURE_ID {
            let [dc] = self.dc_offsets.as_slice() else {
                return Err(SignalError::InvalidMetadata(
                    "idle fixture must declare exactly one tiny-DC section",
                ));
            };
            if dc.analysis_range != IDLE_DC_ANALYSIS_RANGE
                || dc.source_left <= 0.0
                || dc.source_right >= 0.0
                || !metadata_values_match(dc.effective_left, IDLE_DC_EFFECTIVE_LEFT)
                || !metadata_values_match(dc.effective_right, IDLE_DC_EFFECTIVE_RIGHT)
            {
                return Err(SignalError::InvalidMetadata(
                    "idle tiny-DC metadata must declare opposing expected polarities",
                ));
            }
            let range = self
                .range(IDLE_DC_ANALYSIS_RANGE)
                .ok_or(SignalError::InvalidMetadata(
                    "idle fixture is missing its tiny-DC analysis range",
                ))?
                .frames();
            if !self.left[range.clone()]
                .iter()
                .all(|sample| metadata_values_match(*sample, dc.source_left))
                || !self.right[range]
                    .iter()
                    .all(|sample| metadata_values_match(*sample, dc.source_right))
            {
                return Err(SignalError::InvalidMetadata(
                    "idle tiny-DC samples do not match declared metadata",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SignalError {
    InvalidHeadroomDb(f64),
    InvalidFilterGuardFrames(usize),
    ChannelLengthMismatch {
        left: usize,
        right: usize,
    },
    NonFiniteSample {
        channel: &'static str,
        frame: usize,
    },
    SourceSampleOutOfRange {
        channel: &'static str,
        frame: usize,
        value: f64,
    },
    InvalidRange {
        name: &'static str,
        start: usize,
        end: usize,
        frames: usize,
    },
    InvalidMetadata(&'static str),
}

impl fmt::Display for SignalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeadroomDb(db) => write!(
                formatter,
                "headroom must be a finite, non-positive dB value with nonzero gain (got {db})"
            ),
            Self::InvalidFilterGuardFrames(frames) => write!(
                formatter,
                "filter guard must be at least {MIN_FILTER_GUARD_FRAMES} source frames (got {frames})"
            ),
            Self::ChannelLengthMismatch { left, right } => {
                write!(
                    formatter,
                    "stereo channel length mismatch: {left} vs {right}"
                )
            }
            Self::NonFiniteSample { channel, frame } => {
                write!(formatter, "{channel} sample at frame {frame} is nonfinite")
            }
            Self::SourceSampleOutOfRange {
                channel,
                frame,
                value,
            } => write!(
                formatter,
                "{channel} source sample at frame {frame} exceeds full scale: {value}"
            ),
            Self::InvalidRange {
                name,
                start,
                end,
                frames,
            } => write!(
                formatter,
                "invalid source range {name}: {start}..{end} for {frames} frames"
            ),
            Self::InvalidMetadata(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SignalError {}

/// Generate the four-section coherent 1 kHz effective-level sweep at 44.1 kHz.
pub fn coherent_level_sweep(
    headroom_db: f64,
    guard_frames: usize,
) -> Result<StereoSignal, SignalError> {
    coherent_level_sections(headroom_db, guard_frames, &[0, 1, 2, 3])
}

/// Generate one section of the coherent level sweep for fast table tuning.
pub fn coherent_level_probe(
    headroom_db: f64,
    guard_frames: usize,
    effective_dbfs: f64,
) -> Result<StereoSignal, SignalError> {
    let index = LEVEL_SWEEP_EFFECTIVE_DBFS
        .iter()
        .position(|candidate| (candidate - effective_dbfs).abs() < 1.0e-12)
        .ok_or(SignalError::InvalidMetadata(
            "level probe must select -6, -20, -60, or -100 dBFS",
        ))?;
    coherent_level_sections(headroom_db, guard_frames, &[index])
}

fn coherent_level_sections(
    headroom_db: f64,
    guard_frames: usize,
    level_indices: &[usize],
) -> Result<StereoSignal, SignalError> {
    let headroom_gain = checked_headroom_gain(headroom_db)?;
    checked_filter_guard_frames(guard_frames)?;
    let (fft_bin, actual_hz) = coherent_carrier(
        LEVEL_SWEEP_TARGET_HZ,
        SOURCE_RATE_44K1_HZ,
        LEVEL_SWEEP_ANALYZE_FRAMES,
    );
    let section_frames = guard_frames * 2 + LEVEL_SWEEP_ANALYZE_FRAMES;
    let total_frames = level_indices.len() * section_frames;
    let mut left = Vec::with_capacity(total_frames);
    let mut ranges = Vec::with_capacity(level_indices.len() * 3);
    let mut carriers = Vec::with_capacity(level_indices.len());

    for (section, &index) in level_indices.iter().enumerate() {
        let effective_dbfs = LEVEL_SWEEP_EFFECTIVE_DBFS[index];
        let effective_amplitude = dbfs_to_amplitude(effective_dbfs);
        let source_amplitude = effective_amplitude / headroom_gain;
        let section_start = section * section_frames;
        let analysis_start = section_start + guard_frames;
        let analysis_end = analysis_start + LEVEL_SWEEP_ANALYZE_FRAMES;
        let section_end = analysis_end + guard_frames;

        ranges.push(source_range(
            LEVEL_PRE_GUARD_RANGE_NAMES[index],
            RangePurpose::Guard,
            section_start,
            analysis_start,
        ));
        ranges.push(source_range(
            LEVEL_ANALYSIS_RANGE_NAMES[index],
            RangePurpose::Analyze,
            analysis_start,
            analysis_end,
        ));
        ranges.push(source_range(
            LEVEL_POST_GUARD_RANGE_NAMES[index],
            RangePurpose::Guard,
            analysis_end,
            section_end,
        ));
        carriers.push(Carrier {
            name: LEVEL_CARRIER_NAMES[index],
            target_hz: LEVEL_SWEEP_TARGET_HZ,
            actual_hz,
            fft_bin,
            coherent_frames: LEVEL_SWEEP_ANALYZE_FRAMES,
            source_amplitude,
            effective_amplitude,
            phase_rad: LEVEL_SWEEP_PHASE_RAD,
            phase_reversal_rad: None,
            analysis_ranges: vec![LEVEL_ANALYSIS_RANGE_NAMES[index]],
        });

        left.extend((section_start..section_end).map(|frame| {
            source_amplitude * sine_at(frame, SOURCE_RATE_44K1_HZ, actual_hz, LEVEL_SWEEP_PHASE_RAD)
        }));
    }

    finish_signal(StereoSignal {
        id: LEVEL_SWEEP_FIXTURE_ID,
        sample_rate_hz: SOURCE_RATE_44K1_HZ,
        headroom_db,
        headroom_gain,
        filter_guard_frames: guard_frames,
        right: left.clone(),
        left,
        ranges,
        carriers,
        dc_offsets: Vec::new(),
    })
}

/// Generate silence, opposing tiny DC, and a coherent ~100 Hz -120 dBFS tone.
pub fn idle_tiny_signal(
    headroom_db: f64,
    guard_frames: usize,
) -> Result<StereoSignal, SignalError> {
    let headroom_gain = checked_headroom_gain(headroom_db)?;
    checked_filter_guard_frames(guard_frames)?;
    let section_frames = guard_frames * 2 + IDLE_ANALYZE_FRAMES;
    let total_frames = section_frames * 3;
    let mut left = Vec::with_capacity(total_frames);
    let mut right = Vec::with_capacity(total_frames);
    let mut ranges = Vec::with_capacity(9);

    push_guarded_analysis_ranges(
        &mut ranges,
        0,
        guard_frames,
        IDLE_ANALYZE_FRAMES,
        IDLE_SILENCE_PRE_GUARD_RANGE,
        IDLE_SILENCE_ANALYSIS_RANGE,
        IDLE_SILENCE_POST_GUARD_RANGE,
    );
    left.resize(section_frames, 0.0);
    right.resize(section_frames, 0.0);

    let source_dc_left = IDLE_DC_EFFECTIVE_LEFT / headroom_gain;
    let source_dc_right = IDLE_DC_EFFECTIVE_RIGHT / headroom_gain;
    let dc_start = section_frames;
    push_guarded_analysis_ranges(
        &mut ranges,
        dc_start,
        guard_frames,
        IDLE_ANALYZE_FRAMES,
        IDLE_DC_PRE_GUARD_RANGE,
        IDLE_DC_ANALYSIS_RANGE,
        IDLE_DC_POST_GUARD_RANGE,
    );
    left.extend(std::iter::repeat_n(source_dc_left, section_frames));
    right.extend(std::iter::repeat_n(source_dc_right, section_frames));

    let (fft_bin, actual_hz) = coherent_carrier(
        IDLE_TONE_TARGET_HZ,
        SOURCE_RATE_44K1_HZ,
        IDLE_ANALYZE_FRAMES,
    );
    let effective_amplitude = dbfs_to_amplitude(IDLE_TONE_EFFECTIVE_DBFS);
    let source_amplitude = effective_amplitude / headroom_gain;
    let tone_start = section_frames * 2;
    push_guarded_analysis_ranges(
        &mut ranges,
        tone_start,
        guard_frames,
        IDLE_ANALYZE_FRAMES,
        IDLE_TONE_PRE_GUARD_RANGE,
        IDLE_TONE_ANALYSIS_RANGE,
        IDLE_TONE_POST_GUARD_RANGE,
    );
    let tone = (0..section_frames).map(|frame| {
        source_amplitude * sine_at(frame, SOURCE_RATE_44K1_HZ, actual_hz, IDLE_TONE_PHASE_RAD)
    });
    left.extend(tone.clone());
    right.extend(tone);

    finish_signal(StereoSignal {
        id: IDLE_TINY_SIGNAL_FIXTURE_ID,
        sample_rate_hz: SOURCE_RATE_44K1_HZ,
        headroom_db,
        headroom_gain,
        filter_guard_frames: guard_frames,
        left,
        right,
        ranges,
        carriers: vec![Carrier {
            name: "tone_100hz_-120_dbfs",
            target_hz: IDLE_TONE_TARGET_HZ,
            actual_hz,
            fft_bin,
            coherent_frames: IDLE_ANALYZE_FRAMES,
            source_amplitude,
            effective_amplitude,
            phase_rad: IDLE_TONE_PHASE_RAD,
            phase_reversal_rad: None,
            analysis_ranges: vec![IDLE_TONE_ANALYSIS_RANGE],
        }],
        dc_offsets: vec![DcOffset {
            analysis_range: IDLE_DC_ANALYSIS_RANGE,
            source_left: source_dc_left,
            source_right: source_dc_right,
            effective_left: IDLE_DC_EFFECTIVE_LEFT,
            effective_right: IDLE_DC_EFFECTIVE_RIGHT,
        }],
    })
}

/// Generate the rated DSD128 two-tone, mute, and phase-reversed recovery stress.
///
/// Unlike the other rated fixtures, this signal is normalized to 0.999 at the
/// source. The caller's production headroom is applied afterwards.
pub fn high_frequency_stress(
    headroom_db: f64,
    guard_frames: usize,
) -> Result<StereoSignal, SignalError> {
    build_high_frequency_stress(headroom_db, guard_frames, StressLevelContract::RatedSource)
}

/// Generate a directly comparable stress fixture at one effective peak.
///
/// Source amplitude compensates the selected production headroom so every
/// modulator sees [`STRESS_LEVEL_MATCHED_EFFECTIVE_PEAK`] after headroom. The
/// common target is the rated fixture's 0.999 peak after -4 dB, keeping both
/// current production headroom policies at or below the rated source peak.
pub fn high_frequency_matched_stress(
    headroom_db: f64,
    guard_frames: usize,
) -> Result<StereoSignal, SignalError> {
    build_high_frequency_stress(
        headroom_db,
        guard_frames,
        StressLevelContract::MatchedEffective,
    )
}

/// Descriptive alias for [`high_frequency_matched_stress`].
pub fn high_frequency_stress_level_matched(
    headroom_db: f64,
    guard_frames: usize,
) -> Result<StereoSignal, SignalError> {
    high_frequency_matched_stress(headroom_db, guard_frames)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StressLevelContract {
    RatedSource,
    MatchedEffective,
}

fn build_high_frequency_stress(
    headroom_db: f64,
    guard_frames: usize,
    contract: StressLevelContract,
) -> Result<StereoSignal, SignalError> {
    let headroom_gain = checked_headroom_gain(headroom_db)?;
    checked_filter_guard_frames(guard_frames)?;
    let (fixture_id, range_names, carrier_names, requested_source_peak) = match contract {
        StressLevelContract::RatedSource => (
            STRESS_RATED_FIXTURE_ID,
            [
                STRESS_SETTLE_RANGE,
                STRESS_STEADY_ANALYSIS_RANGE,
                STRESS_PRE_MUTE_GUARD_RANGE,
                STRESS_MUTE_ENTRY_TRANSITION_RANGE,
                STRESS_CLEAN_MUTE_RANGE,
                STRESS_PRE_RECOVERY_TRANSITION_RANGE,
                STRESS_RECOVERY_RANGE,
                STRESS_POST_ROLL_RANGE,
            ],
            ["stress_18khz", "stress_19khz"],
            STRESS_SOURCE_PEAK,
        ),
        StressLevelContract::MatchedEffective => (
            STRESS_LEVEL_MATCHED_FIXTURE_ID,
            [
                STRESS_LEVEL_MATCHED_SETTLE_RANGE,
                STRESS_LEVEL_MATCHED_STEADY_ANALYSIS_RANGE,
                STRESS_LEVEL_MATCHED_PRE_MUTE_GUARD_RANGE,
                STRESS_LEVEL_MATCHED_MUTE_ENTRY_TRANSITION_RANGE,
                STRESS_LEVEL_MATCHED_CLEAN_MUTE_RANGE,
                STRESS_LEVEL_MATCHED_PRE_RECOVERY_TRANSITION_RANGE,
                STRESS_LEVEL_MATCHED_RECOVERY_RANGE,
                STRESS_LEVEL_MATCHED_POST_ROLL_RANGE,
            ],
            ["matched_stress_18khz", "matched_stress_19khz"],
            STRESS_LEVEL_MATCHED_EFFECTIVE_PEAK / headroom_gain,
        ),
    };
    if !requested_source_peak.is_finite()
        || requested_source_peak <= 0.0
        || requested_source_peak > STRESS_SOURCE_PEAK * (1.0 + 1.0e-12)
    {
        return Err(SignalError::InvalidMetadata(
            "stress level contract exceeds the rated source peak",
        ));
    }
    let carriers = STRESS_TARGET_HZ.map(|target_hz| {
        coherent_carrier(target_hz, SOURCE_RATE_44K1_HZ, STRESS_STEADY_ANALYZE_FRAMES)
    });
    let actual_hz = [carriers[0].1, carriers[1].1];
    let steady_end = guard_frames + STRESS_STEADY_ANALYZE_FRAMES;
    let guard_end = steady_end + guard_frames;
    let mute_entry_end = guard_end + guard_frames;
    let clean_mute_end = mute_entry_end + STRESS_CLEAN_MUTE_FRAMES;
    let mute_end = clean_mute_end + guard_frames;
    let recovery_frames = guard_frames + STRESS_STEADY_ANALYZE_FRAMES;
    let recovery_end = mute_end + recovery_frames;
    let total_frames = recovery_end + guard_frames;
    let mut raw = Vec::with_capacity(total_frames);

    raw.extend((0..guard_end).map(|frame| stress_sample(frame, actual_hz, 0.0)));
    raw.resize(mute_end, 0.0);
    raw.extend(
        (0..recovery_frames)
            .map(|frame| stress_sample(frame, actual_hz, STRESS_PHASE_REVERSAL_RAD)),
    );
    raw.extend(
        (recovery_frames..recovery_frames + guard_frames)
            .map(|frame| stress_sample(frame, actual_hz, STRESS_PHASE_REVERSAL_RAD)),
    );
    let raw_peak = peak(&raw);
    if raw_peak <= 0.0 || !raw_peak.is_finite() {
        return Err(SignalError::InvalidMetadata(
            "stress normalization peak is invalid",
        ));
    }
    let source_amplitude = requested_source_peak / raw_peak;
    let left: Vec<_> = raw
        .into_iter()
        .map(|sample| sample * source_amplitude)
        .collect();

    let ranges = vec![
        source_range(range_names[0], RangePurpose::Settle, 0, guard_frames),
        source_range(
            range_names[1],
            RangePurpose::Analyze,
            guard_frames,
            steady_end,
        ),
        source_range(range_names[2], RangePurpose::Guard, steady_end, guard_end),
        source_range(
            range_names[3],
            RangePurpose::Guard,
            guard_end,
            mute_entry_end,
        ),
        source_range(
            range_names[4],
            RangePurpose::Mute,
            mute_entry_end,
            clean_mute_end,
        ),
        source_range(
            range_names[5],
            RangePurpose::Guard,
            clean_mute_end,
            mute_end,
        ),
        source_range(
            range_names[6],
            RangePurpose::Recovery,
            mute_end,
            recovery_end,
        ),
        source_range(
            range_names[7],
            RangePurpose::PostRoll,
            recovery_end,
            total_frames,
        ),
    ];
    let carrier_metadata = (0..2)
        .map(|index| Carrier {
            name: carrier_names[index],
            target_hz: STRESS_TARGET_HZ[index],
            actual_hz: actual_hz[index],
            fft_bin: carriers[index].0,
            coherent_frames: STRESS_STEADY_ANALYZE_FRAMES,
            source_amplitude,
            effective_amplitude: source_amplitude * headroom_gain,
            phase_rad: STRESS_PHASES_RAD[index],
            phase_reversal_rad: Some(STRESS_PHASE_REVERSAL_RAD),
            analysis_ranges: vec![range_names[1], range_names[6]],
        })
        .collect();

    finish_signal(StereoSignal {
        id: fixture_id,
        sample_rate_hz: SOURCE_RATE_44K1_HZ,
        headroom_db,
        headroom_gain,
        filter_guard_frames: guard_frames,
        right: left.clone(),
        left,
        ranges,
        carriers: carrier_metadata,
        dc_offsets: Vec::new(),
    })
}

/// Generate the coherent 176.4 kHz four-carrier hi-res reconstruction fixture.
pub fn hires_multitone(headroom_db: f64, guard_frames: usize) -> Result<StereoSignal, SignalError> {
    let headroom_gain = checked_headroom_gain(headroom_db)?;
    checked_filter_guard_frames(guard_frames)?;
    let effective_peak = dbfs_to_amplitude(HIRES_EFFECTIVE_PEAK_DBFS);
    let requested_source_peak = effective_peak / headroom_gain;
    if !requested_source_peak.is_finite() || requested_source_peak > 1.0 {
        return Err(SignalError::SourceSampleOutOfRange {
            channel: "left",
            frame: 0,
            value: requested_source_peak,
        });
    }

    let coherent = HIRES_TARGET_HZ
        .map(|target_hz| coherent_carrier(target_hz, SOURCE_RATE_176K4_HZ, HIRES_ANALYZE_FRAMES));
    let actual_hz = coherent.map(|(_, actual_hz)| actual_hz);
    let analysis_end = guard_frames + HIRES_ANALYZE_FRAMES;
    let total_frames = analysis_end + guard_frames;
    let raw: Vec<_> = (0..total_frames)
        .map(|frame| {
            (0..actual_hz.len())
                .map(|index| {
                    sine_at(
                        frame,
                        SOURCE_RATE_176K4_HZ,
                        actual_hz[index],
                        HIRES_PHASES_RAD[index],
                    )
                })
                .sum::<f64>()
        })
        .collect();
    let raw_peak = peak(&raw);
    if raw_peak <= 0.0 || !raw_peak.is_finite() {
        return Err(SignalError::InvalidMetadata(
            "hi-res normalization peak is invalid",
        ));
    }
    let source_amplitude = requested_source_peak / raw_peak;
    let left: Vec<_> = raw
        .into_iter()
        .map(|sample| sample * source_amplitude)
        .collect();
    let ranges = vec![
        source_range(HIRES_SETTLE_RANGE, RangePurpose::Settle, 0, guard_frames),
        source_range(
            HIRES_ANALYSIS_RANGE,
            RangePurpose::Analyze,
            guard_frames,
            analysis_end,
        ),
        source_range(
            HIRES_POST_ROLL_RANGE,
            RangePurpose::PostRoll,
            analysis_end,
            total_frames,
        ),
    ];
    let carriers = (0..HIRES_TARGET_HZ.len())
        .map(|index| Carrier {
            name: match index {
                0 => "hires_1khz",
                1 => "hires_18khz",
                2 => "hires_40khz",
                _ => "hires_70khz",
            },
            target_hz: HIRES_TARGET_HZ[index],
            actual_hz: actual_hz[index],
            fft_bin: coherent[index].0,
            coherent_frames: HIRES_ANALYZE_FRAMES,
            source_amplitude,
            effective_amplitude: source_amplitude * headroom_gain,
            phase_rad: HIRES_PHASES_RAD[index],
            phase_reversal_rad: None,
            analysis_ranges: vec![HIRES_ANALYSIS_RANGE],
        })
        .collect();

    finish_signal(StereoSignal {
        id: HIRES_MULTITONE_FIXTURE_ID,
        sample_rate_hz: SOURCE_RATE_176K4_HZ,
        headroom_db,
        headroom_gain,
        filter_guard_frames: guard_frames,
        right: left.clone(),
        left,
        ranges,
        carriers,
        dc_offsets: Vec::new(),
    })
}

pub fn dbfs_to_amplitude(dbfs: f64) -> f64 {
    10.0_f64.powf(dbfs / 20.0)
}

pub fn amplitude_to_dbfs(amplitude: f64) -> f64 {
    20.0 * amplitude.abs().log10()
}

fn metadata_values_match(actual: f64, expected: f64) -> bool {
    if actual == expected {
        return true;
    }
    let scale = actual.abs().max(expected.abs()).max(f64::MIN_POSITIVE);
    (actual - expected).abs() <= 64.0 * f64::EPSILON * scale
}

fn checked_headroom_gain(headroom_db: f64) -> Result<f64, SignalError> {
    if !headroom_db.is_finite() || headroom_db > 0.0 {
        return Err(SignalError::InvalidHeadroomDb(headroom_db));
    }
    let gain = dbfs_to_amplitude(headroom_db);
    if !gain.is_finite() || gain <= 0.0 || gain > 1.0 {
        return Err(SignalError::InvalidHeadroomDb(headroom_db));
    }
    Ok(gain)
}

fn checked_filter_guard_frames(guard_frames: usize) -> Result<(), SignalError> {
    if guard_frames < MIN_FILTER_GUARD_FRAMES {
        return Err(SignalError::InvalidFilterGuardFrames(guard_frames));
    }
    Ok(())
}

fn coherent_carrier(target_hz: f64, sample_rate_hz: u32, frames: usize) -> (usize, f64) {
    let bin_hz = sample_rate_hz as f64 / frames as f64;
    let fft_bin = (target_hz / bin_hz).round().max(1.0) as usize;
    (fft_bin, fft_bin as f64 * bin_hz)
}

fn sine_at(frame: usize, sample_rate_hz: u32, frequency_hz: f64, phase_rad: f64) -> f64 {
    (2.0 * PI * frequency_hz * frame as f64 / sample_rate_hz as f64 + phase_rad).sin()
}

fn stress_sample(frame: usize, actual_hz: [f64; 2], phase_offset: f64) -> f64 {
    (0..actual_hz.len())
        .map(|index| {
            sine_at(
                frame,
                SOURCE_RATE_44K1_HZ,
                actual_hz[index],
                STRESS_PHASES_RAD[index] + phase_offset,
            )
        })
        .sum()
}

fn source_range(
    name: &'static str,
    purpose: RangePurpose,
    start: usize,
    end: usize,
) -> SourceRange {
    SourceRange {
        name,
        purpose,
        start,
        end,
    }
}

fn push_guarded_analysis_ranges(
    ranges: &mut Vec<SourceRange>,
    start: usize,
    guard_frames: usize,
    analyze_frames: usize,
    pre_guard_name: &'static str,
    analysis_name: &'static str,
    post_guard_name: &'static str,
) {
    let analysis_start = start + guard_frames;
    let analysis_end = analysis_start + analyze_frames;
    let end = analysis_end + guard_frames;
    ranges.push(source_range(
        pre_guard_name,
        RangePurpose::Guard,
        start,
        analysis_start,
    ));
    ranges.push(source_range(
        analysis_name,
        RangePurpose::Analyze,
        analysis_start,
        analysis_end,
    ));
    ranges.push(source_range(
        post_guard_name,
        RangePurpose::Guard,
        analysis_end,
        end,
    ));
}

fn peak(samples: &[f64]) -> f64 {
    samples
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(sample.abs()))
}

fn validate_channel(channel: &'static str, samples: &[f64]) -> Result<(), SignalError> {
    for (frame, sample) in samples.iter().copied().enumerate() {
        if !sample.is_finite() {
            return Err(SignalError::NonFiniteSample { channel, frame });
        }
        if sample.abs() > 1.0 {
            return Err(SignalError::SourceSampleOutOfRange {
                channel,
                frame,
                value: sample,
            });
        }
    }
    Ok(())
}

fn finish_signal(signal: StereoSignal) -> Result<StereoSignal, SignalError> {
    signal.validate()?;
    Ok(signal)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1.0e-12;
    const TEST_GUARD_FRAMES: usize = MIN_FILTER_GUARD_FRAMES + 1_024;

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected:.16e}, got {actual:.16e} (tolerance {tolerance:.3e})"
        );
    }

    fn fit_amplitude(samples: &[f64], sample_rate_hz: u32, frequency_hz: f64) -> f64 {
        let len = samples.len() as f64;
        let (sin_sum, cos_sum) =
            samples
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |(sin_sum, cos_sum), (frame, sample)| {
                    let phase = 2.0 * PI * frequency_hz * frame as f64 / sample_rate_hz as f64;
                    (
                        sin_sum + sample * phase.sin(),
                        cos_sum + sample * phase.cos(),
                    )
                });
        2.0 * sin_sum.hypot(cos_sum) / len
    }

    fn assert_partition(signal: &StereoSignal, expected: &[(&str, RangePurpose, usize, usize)]) {
        assert_eq!(signal.ranges.len(), expected.len());
        for (actual, &(name, purpose, start, end)) in signal.ranges.iter().zip(expected) {
            assert_eq!(actual.name, name);
            assert_eq!(actual.purpose, purpose);
            assert_eq!(actual.frames(), start..end);
        }
        signal.validate().unwrap();
        assert!(
            signal
                .left
                .iter()
                .chain(&signal.right)
                .all(|x| x.is_finite())
        );
    }

    #[test]
    fn level_sweep_has_exact_ranges_bins_and_effective_levels() {
        let signal = coherent_level_sweep(-4.0, TEST_GUARD_FRAMES).unwrap();
        let section = 2 * TEST_GUARD_FRAMES + LEVEL_SWEEP_ANALYZE_FRAMES;
        assert_eq!(signal.sample_rate_hz, SOURCE_RATE_44K1_HZ);
        assert_eq!(signal.filter_guard_frames, TEST_GUARD_FRAMES);
        assert_eq!(signal.frames(), 4 * section);
        assert_eq!(signal.left, signal.right);
        let mut expected = Vec::new();
        for index in 0..LEVEL_SWEEP_EFFECTIVE_DBFS.len() {
            let start = index * section;
            let analysis_start = start + TEST_GUARD_FRAMES;
            let analysis_end = analysis_start + LEVEL_SWEEP_ANALYZE_FRAMES;
            expected.push((
                LEVEL_PRE_GUARD_RANGE_NAMES[index],
                RangePurpose::Guard,
                start,
                analysis_start,
            ));
            expected.push((
                LEVEL_ANALYSIS_RANGE_NAMES[index],
                RangePurpose::Analyze,
                analysis_start,
                analysis_end,
            ));
            expected.push((
                LEVEL_POST_GUARD_RANGE_NAMES[index],
                RangePurpose::Guard,
                analysis_end,
                start + section,
            ));
        }
        assert_partition(&signal, &expected);

        let expected_hz = 372.0 * SOURCE_RATE_44K1_HZ as f64 / LEVEL_SWEEP_ANALYZE_FRAMES as f64;
        for (index, carrier) in signal.carriers.iter().enumerate() {
            assert_eq!(carrier.fft_bin, 372);
            assert_close(carrier.actual_hz, expected_hz, EPS);
            assert_close(
                carrier.effective_dbfs(),
                LEVEL_SWEEP_EFFECTIVE_DBFS[index],
                2.0e-12,
            );
            assert_close(
                carrier.source_amplitude * signal.headroom_gain,
                carrier.effective_amplitude,
                EPS,
            );
            let range = signal.range(carrier.analysis_ranges[0]).unwrap().frames();
            let fitted_source = fit_amplitude(
                &signal.left[range],
                signal.sample_rate_hz,
                carrier.actual_hz,
            );
            assert_close(fitted_source, carrier.source_amplitude, 2.0e-12);
        }
    }

    #[test]
    fn level_probe_keeps_one_declared_section() {
        let signal = coherent_level_probe(-4.0, TEST_GUARD_FRAMES, -60.0).unwrap();
        let section = 2 * TEST_GUARD_FRAMES + LEVEL_SWEEP_ANALYZE_FRAMES;
        assert_eq!(signal.frames(), section);
        assert_eq!(signal.carriers.len(), 1);
        assert_eq!(signal.carriers[0].name, "level_-60_dbfs");
        assert_close(signal.carriers[0].effective_dbfs(), -60.0, 2.0e-12);
        assert_partition(
            &signal,
            &[
                (
                    LEVEL_PRE_GUARD_RANGE_NAMES[2],
                    RangePurpose::Guard,
                    0,
                    TEST_GUARD_FRAMES,
                ),
                (
                    LEVEL_ANALYSIS_RANGE_NAMES[2],
                    RangePurpose::Analyze,
                    TEST_GUARD_FRAMES,
                    TEST_GUARD_FRAMES + LEVEL_SWEEP_ANALYZE_FRAMES,
                ),
                (
                    LEVEL_POST_GUARD_RANGE_NAMES[2],
                    RangePurpose::Guard,
                    TEST_GUARD_FRAMES + LEVEL_SWEEP_ANALYZE_FRAMES,
                    section,
                ),
            ],
        );
        assert!(coherent_level_probe(-4.0, TEST_GUARD_FRAMES, -30.0).is_err());
    }

    #[test]
    fn level_sweep_intentionally_uses_one_fixed_descending_shared_state_order() {
        // The canonical matrix renders this one continuous fixture so levels
        // share renderer state. Independent and reverse-order variants would
        // multiply cells and are intentionally not part of the canonical v2 matrix.
        let signal = coherent_level_sweep(-4.0, TEST_GUARD_FRAMES).unwrap();
        assert_eq!(signal.id, LEVEL_SWEEP_FIXTURE_ID);
        assert_eq!(
            signal
                .ranges
                .iter()
                .filter(|range| range.purpose == RangePurpose::Analyze)
                .map(|range| range.name)
                .collect::<Vec<_>>(),
            LEVEL_ANALYSIS_RANGE_NAMES
        );
        assert!(
            signal
                .carriers
                .windows(2)
                .all(|pair| pair[0].effective_amplitude > pair[1].effective_amplitude)
        );
        assert!(
            signal
                .ranges
                .windows(2)
                .all(|pair| pair[0].end == pair[1].start)
        );
    }

    #[test]
    fn idle_sequence_has_silence_opposing_dc_and_coherent_tiny_tone() {
        let signal = idle_tiny_signal(-2.0, TEST_GUARD_FRAMES).unwrap();
        let section = 2 * TEST_GUARD_FRAMES + IDLE_ANALYZE_FRAMES;
        assert_eq!(signal.frames(), 3 * section);
        assert_partition(
            &signal,
            &[
                (
                    IDLE_SILENCE_PRE_GUARD_RANGE,
                    RangePurpose::Guard,
                    0,
                    TEST_GUARD_FRAMES,
                ),
                (
                    IDLE_SILENCE_ANALYSIS_RANGE,
                    RangePurpose::Analyze,
                    TEST_GUARD_FRAMES,
                    TEST_GUARD_FRAMES + IDLE_ANALYZE_FRAMES,
                ),
                (
                    IDLE_SILENCE_POST_GUARD_RANGE,
                    RangePurpose::Guard,
                    TEST_GUARD_FRAMES + IDLE_ANALYZE_FRAMES,
                    section,
                ),
                (
                    IDLE_DC_PRE_GUARD_RANGE,
                    RangePurpose::Guard,
                    section,
                    section + TEST_GUARD_FRAMES,
                ),
                (
                    IDLE_DC_ANALYSIS_RANGE,
                    RangePurpose::Analyze,
                    section + TEST_GUARD_FRAMES,
                    section + TEST_GUARD_FRAMES + IDLE_ANALYZE_FRAMES,
                ),
                (
                    IDLE_DC_POST_GUARD_RANGE,
                    RangePurpose::Guard,
                    section + TEST_GUARD_FRAMES + IDLE_ANALYZE_FRAMES,
                    2 * section,
                ),
                (
                    IDLE_TONE_PRE_GUARD_RANGE,
                    RangePurpose::Guard,
                    2 * section,
                    2 * section + TEST_GUARD_FRAMES,
                ),
                (
                    IDLE_TONE_ANALYSIS_RANGE,
                    RangePurpose::Analyze,
                    2 * section + TEST_GUARD_FRAMES,
                    2 * section + TEST_GUARD_FRAMES + IDLE_ANALYZE_FRAMES,
                ),
                (
                    IDLE_TONE_POST_GUARD_RANGE,
                    RangePurpose::Guard,
                    2 * section + TEST_GUARD_FRAMES + IDLE_ANALYZE_FRAMES,
                    3 * section,
                ),
            ],
        );

        assert!(signal.left[..section].iter().all(|&sample| sample == 0.0));
        assert!(signal.right[..section].iter().all(|&sample| sample == 0.0));
        let dc = &signal.dc_offsets[0];
        assert!(
            signal.left[section..2 * section]
                .iter()
                .all(|&sample| sample == dc.source_left)
        );
        assert!(
            signal.right[section..2 * section]
                .iter()
                .all(|&sample| sample == dc.source_right)
        );
        let dc_range = signal.range(dc.analysis_range).unwrap().frames();
        assert!(
            signal.left[dc_range.clone()]
                .iter()
                .all(|&sample| sample == dc.source_left)
        );
        assert!(
            signal.right[dc_range]
                .iter()
                .all(|&sample| sample == dc.source_right)
        );
        assert_close(dc.source_left * signal.headroom_gain, 1.0e-6, EPS);
        assert_close(dc.source_right * signal.headroom_gain, -1.0e-6, EPS);

        let carrier = &signal.carriers[0];
        assert_eq!(carrier.fft_bin, 37);
        assert_close(
            carrier.actual_hz,
            37.0 * SOURCE_RATE_44K1_HZ as f64 / IDLE_ANALYZE_FRAMES as f64,
            EPS,
        );
        assert_close(carrier.effective_dbfs(), -120.0, 2.0e-12);
        let range = signal.range(IDLE_TONE_ANALYSIS_RANGE).unwrap().frames();
        let fitted = fit_amplitude(
            &signal.left[range],
            signal.sample_rate_hz,
            carrier.actual_hz,
        );
        assert_close(fitted * signal.headroom_gain, 1.0e-6, 1.0e-17);
    }

    #[test]
    fn stress_sequence_has_exact_peak_mute_and_phase_reversed_recovery() {
        let signal = high_frequency_stress(-4.0, TEST_GUARD_FRAMES).unwrap();
        let steady_end = TEST_GUARD_FRAMES + STRESS_STEADY_ANALYZE_FRAMES;
        let guard_end = steady_end + TEST_GUARD_FRAMES;
        let mute_entry_end = guard_end + TEST_GUARD_FRAMES;
        let clean_mute_end = mute_entry_end + STRESS_CLEAN_MUTE_FRAMES;
        let mute_end = clean_mute_end + TEST_GUARD_FRAMES;
        let recovery_frames = TEST_GUARD_FRAMES + STRESS_STEADY_ANALYZE_FRAMES;
        let recovery_end = mute_end + recovery_frames;
        let total_frames = recovery_end + TEST_GUARD_FRAMES;
        assert_eq!(signal.id, STRESS_RATED_FIXTURE_ID);
        assert_eq!(signal.frames(), total_frames);
        assert_eq!(signal.left, signal.right);
        assert_partition(
            &signal,
            &[
                (
                    STRESS_SETTLE_RANGE,
                    RangePurpose::Settle,
                    0,
                    TEST_GUARD_FRAMES,
                ),
                (
                    STRESS_STEADY_ANALYSIS_RANGE,
                    RangePurpose::Analyze,
                    TEST_GUARD_FRAMES,
                    steady_end,
                ),
                (
                    STRESS_PRE_MUTE_GUARD_RANGE,
                    RangePurpose::Guard,
                    steady_end,
                    guard_end,
                ),
                (
                    STRESS_MUTE_ENTRY_TRANSITION_RANGE,
                    RangePurpose::Guard,
                    guard_end,
                    mute_entry_end,
                ),
                (
                    STRESS_CLEAN_MUTE_RANGE,
                    RangePurpose::Mute,
                    mute_entry_end,
                    clean_mute_end,
                ),
                (
                    STRESS_PRE_RECOVERY_TRANSITION_RANGE,
                    RangePurpose::Guard,
                    clean_mute_end,
                    mute_end,
                ),
                (
                    STRESS_RECOVERY_RANGE,
                    RangePurpose::Recovery,
                    mute_end,
                    recovery_end,
                ),
                (
                    STRESS_POST_ROLL_RANGE,
                    RangePurpose::PostRoll,
                    recovery_end,
                    total_frames,
                ),
            ],
        );
        assert_close(signal.source_peak(), STRESS_SOURCE_PEAK, 4.0 * f64::EPSILON);
        assert_close(
            signal.effective_peak(),
            STRESS_SOURCE_PEAK * signal.headroom_gain,
            4.0 * f64::EPSILON,
        );
        assert!(
            signal.left[guard_end..mute_end]
                .iter()
                .all(|&sample| sample == 0.0)
        );
        assert_close(signal.left[mute_end], -signal.left[0], 4.0e-15);
        assert_eq!(signal.carriers[0].fft_bin, 6_687);
        assert_eq!(signal.carriers[1].fft_bin, 7_059);
        assert!(
            signal
                .carriers
                .iter()
                .all(|carrier| carrier.phase_reversal_rad == Some(PI))
        );
    }

    #[test]
    fn matched_stress_has_identical_effective_peak_and_carriers_across_headrooms() {
        let standard = high_frequency_stress_level_matched(-4.0, TEST_GUARD_FRAMES).unwrap();
        let search = high_frequency_stress_level_matched(-2.0, TEST_GUARD_FRAMES).unwrap();

        for signal in [&standard, &search] {
            assert_eq!(signal.id, STRESS_LEVEL_MATCHED_FIXTURE_ID);
            assert_close(
                signal.effective_peak(),
                STRESS_LEVEL_MATCHED_EFFECTIVE_PEAK,
                8.0 * f64::EPSILON,
            );
            assert_eq!(
                signal
                    .range(STRESS_LEVEL_MATCHED_CLEAN_MUTE_RANGE)
                    .unwrap()
                    .len(),
                STRESS_CLEAN_MUTE_FRAMES
            );
            assert_eq!(
                signal
                    .carriers
                    .iter()
                    .map(|carrier| carrier.fft_bin)
                    .collect::<Vec<_>>(),
                vec![6_687, 7_059]
            );
            assert!(
                signal
                    .range(STRESS_LEVEL_MATCHED_MUTE_ENTRY_TRANSITION_RANGE)
                    .is_some()
            );
            assert!(
                signal
                    .range(STRESS_LEVEL_MATCHED_PRE_RECOVERY_TRANSITION_RANGE)
                    .is_some()
            );
        }
        assert_close(
            standard.source_peak(),
            STRESS_SOURCE_PEAK,
            8.0 * f64::EPSILON,
        );
        assert!(search.source_peak() < standard.source_peak());
        for (standard, search) in standard.carriers.iter().zip(&search.carriers) {
            assert_close(
                standard.effective_amplitude,
                search.effective_amplitude,
                8.0 * f64::EPSILON,
            );
            assert_close(standard.actual_hz, search.actual_hz, EPS);
            assert!(standard.name.starts_with("matched_stress_"));
            assert!(search.name.starts_with("matched_stress_"));
        }
    }

    #[test]
    fn rated_stress_keeps_source_peak_fixed_and_applies_production_headroom() {
        let standard = high_frequency_stress(-4.0, TEST_GUARD_FRAMES).unwrap();
        let search = high_frequency_stress(-2.0, TEST_GUARD_FRAMES).unwrap();
        for signal in [&standard, &search] {
            assert_eq!(signal.id, STRESS_RATED_FIXTURE_ID);
            assert_close(signal.source_peak(), STRESS_SOURCE_PEAK, 4.0 * f64::EPSILON);
            assert_close(
                signal.effective_peak(),
                STRESS_SOURCE_PEAK * signal.headroom_gain,
                4.0 * f64::EPSILON,
            );
            assert_eq!(
                signal
                    .range(STRESS_MUTE_ENTRY_TRANSITION_RANGE)
                    .unwrap()
                    .len(),
                TEST_GUARD_FRAMES
            );
            assert_eq!(
                signal.range(STRESS_CLEAN_MUTE_RANGE).unwrap().len(),
                STRESS_CLEAN_MUTE_FRAMES
            );
            assert_eq!(
                signal
                    .range(STRESS_PRE_RECOVERY_TRANSITION_RANGE)
                    .unwrap()
                    .len(),
                TEST_GUARD_FRAMES
            );
        }
        assert!(search.effective_peak() > standard.effective_peak());
        for (standard, search) in standard.carriers.iter().zip(&search.carriers) {
            assert_close(standard.source_amplitude, search.source_amplitude, EPS);
            assert!(search.effective_amplitude > standard.effective_amplitude);
            assert_eq!(standard.fft_bin, search.fft_bin);
        }
    }

    #[test]
    fn hires_multitone_is_coherent_and_normalized_after_headroom() {
        let signal = hires_multitone(-4.0, TEST_GUARD_FRAMES).unwrap();
        let analysis_end = TEST_GUARD_FRAMES + HIRES_ANALYZE_FRAMES;
        let total_frames = analysis_end + TEST_GUARD_FRAMES;
        assert_eq!(signal.sample_rate_hz, SOURCE_RATE_176K4_HZ);
        assert_eq!(signal.frames(), total_frames);
        assert_partition(
            &signal,
            &[
                (
                    HIRES_SETTLE_RANGE,
                    RangePurpose::Settle,
                    0,
                    TEST_GUARD_FRAMES,
                ),
                (
                    HIRES_ANALYSIS_RANGE,
                    RangePurpose::Analyze,
                    TEST_GUARD_FRAMES,
                    analysis_end,
                ),
                (
                    HIRES_POST_ROLL_RANGE,
                    RangePurpose::PostRoll,
                    analysis_end,
                    total_frames,
                ),
            ],
        );
        assert_close(
            signal.effective_peak(),
            dbfs_to_amplitude(HIRES_EFFECTIVE_PEAK_DBFS),
            4.0 * f64::EPSILON,
        );
        assert_eq!(
            signal
                .carriers
                .iter()
                .map(|carrier| carrier.fft_bin)
                .collect::<Vec<_>>(),
            vec![186, 3_344, 7_430, 13_003]
        );
        for carrier in &signal.carriers {
            assert_close(
                carrier.actual_hz,
                carrier.fft_bin as f64 * SOURCE_RATE_176K4_HZ as f64 / HIRES_ANALYZE_FRAMES as f64,
                EPS,
            );
            assert_close(
                carrier.source_amplitude * signal.headroom_gain,
                carrier.effective_amplitude,
                EPS,
            );
        }
    }

    #[test]
    fn fixtures_are_deterministic_finite_and_below_full_scale_at_production_headrooms() {
        for headroom_db in [-4.0, -2.0] {
            let fixtures = [
                coherent_level_sweep(headroom_db, TEST_GUARD_FRAMES).unwrap(),
                idle_tiny_signal(headroom_db, TEST_GUARD_FRAMES).unwrap(),
                high_frequency_stress(headroom_db, TEST_GUARD_FRAMES).unwrap(),
                high_frequency_stress_level_matched(headroom_db, TEST_GUARD_FRAMES).unwrap(),
                hires_multitone(headroom_db, TEST_GUARD_FRAMES).unwrap(),
            ];
            for fixture in fixtures {
                fixture.validate().unwrap();
                assert!(fixture.source_peak() <= 1.0);
            }
        }

        assert_eq!(
            coherent_level_sweep(-4.0, TEST_GUARD_FRAMES),
            coherent_level_sweep(-4.0, TEST_GUARD_FRAMES)
        );
        assert_eq!(
            idle_tiny_signal(-4.0, TEST_GUARD_FRAMES),
            idle_tiny_signal(-4.0, TEST_GUARD_FRAMES)
        );
        assert_eq!(
            high_frequency_stress(-4.0, TEST_GUARD_FRAMES),
            high_frequency_stress(-4.0, TEST_GUARD_FRAMES)
        );
        assert_eq!(
            high_frequency_stress_level_matched(-4.0, TEST_GUARD_FRAMES),
            high_frequency_stress_level_matched(-4.0, TEST_GUARD_FRAMES)
        );
        assert_eq!(
            hires_multitone(-4.0, TEST_GUARD_FRAMES),
            hires_multitone(-4.0, TEST_GUARD_FRAMES)
        );
    }

    #[test]
    fn invalid_headroom_or_filter_guard_is_rejected() {
        assert!(matches!(
            coherent_level_sweep(f64::NAN, TEST_GUARD_FRAMES),
            Err(SignalError::InvalidHeadroomDb(_))
        ));
        assert!(matches!(
            idle_tiny_signal(1.0, TEST_GUARD_FRAMES),
            Err(SignalError::InvalidHeadroomDb(_))
        ));
        assert!(matches!(
            coherent_level_sweep(-12.0, TEST_GUARD_FRAMES),
            Err(SignalError::SourceSampleOutOfRange { .. })
        ));
        assert!(matches!(
            hires_multitone(-12.0, TEST_GUARD_FRAMES),
            Err(SignalError::SourceSampleOutOfRange { .. })
        ));
        assert!(matches!(
            high_frequency_stress_level_matched(-12.0, TEST_GUARD_FRAMES),
            Err(SignalError::InvalidMetadata(
                "stress level contract exceeds the rated source peak"
            ))
        ));
        let short_guard = MIN_FILTER_GUARD_FRAMES - 1;
        for fixture in [
            coherent_level_sweep(-4.0, short_guard),
            idle_tiny_signal(-4.0, short_guard),
            high_frequency_stress(-4.0, short_guard),
            high_frequency_stress_level_matched(-4.0, short_guard),
            hires_multitone(-4.0, short_guard),
        ] {
            assert!(matches!(
                fixture,
                Err(SignalError::InvalidFilterGuardFrames(frames)) if frames == short_guard
            ));
        }

        let mut fixture = coherent_level_sweep(-4.0, MIN_FILTER_GUARD_FRAMES).unwrap();
        fixture.filter_guard_frames = short_guard;
        assert!(matches!(
            fixture.validate(),
            Err(SignalError::InvalidFilterGuardFrames(frames)) if frames == short_guard
        ));
    }

    #[test]
    fn validation_rejects_inconsistent_level_and_tiny_dc_metadata() {
        let mut fixture = coherent_level_sweep(-4.0, TEST_GUARD_FRAMES).unwrap();
        fixture.headroom_gain *= 0.99;
        assert!(matches!(
            fixture.validate(),
            Err(SignalError::InvalidMetadata(
                "headroom gain does not match headroom dB"
            ))
        ));

        let mut fixture = coherent_level_sweep(-4.0, TEST_GUARD_FRAMES).unwrap();
        fixture.carriers[0].effective_amplitude *= 0.99;
        assert!(matches!(
            fixture.validate(),
            Err(SignalError::InvalidMetadata(
                "carrier effective amplitude does not match source amplitude and headroom"
            ))
        ));

        let mut fixture = idle_tiny_signal(-4.0, TEST_GUARD_FRAMES).unwrap();
        fixture.dc_offsets[0].effective_left *= 0.99;
        assert!(matches!(
            fixture.validate(),
            Err(SignalError::InvalidMetadata(
                "DC effective values do not match source values and headroom"
            ))
        ));

        let mut fixture = idle_tiny_signal(-4.0, TEST_GUARD_FRAMES).unwrap();
        let dc_range = fixture.range(IDLE_DC_ANALYSIS_RANGE).unwrap().frames();
        fixture.dc_offsets[0].source_right = fixture.dc_offsets[0].source_right.abs();
        fixture.dc_offsets[0].effective_right = fixture.dc_offsets[0].effective_right.abs();
        for sample in &mut fixture.right[dc_range] {
            *sample = sample.abs();
        }
        assert!(matches!(
            fixture.validate(),
            Err(SignalError::InvalidMetadata(
                "idle tiny-DC metadata must declare opposing expected polarities"
            ))
        ));
    }
}
