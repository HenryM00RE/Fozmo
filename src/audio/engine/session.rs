use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::{Hint, ProbedMetadata};
use symphonia::core::units::Time;

use crate::audio::dsp::eq::{EqConfig, EqProcessor};
use crate::audio::dsp::resampler::{FilterType, ResamplerRuntimeInfo, SincResampler};
use crate::audio::engine::signal_path::resolve_pcm_dsp_target;

use super::buffers::DsdWorkerState;
use super::commands::StreamQueueItem;
use super::metadata::{TrackCover, TrackTags, apply_fallback_tags, collect_reader_metadata};
use super::queue_state::PendingStart;
use super::signal_path::OutputMode;
use super::state::{
    AtomicPlayerState, FLUSH_REASON_RECONFIGURE_SESSION, FLUSH_REASON_RESTART_SESSION,
    FLUSH_REASON_SEEK, PLAYBACK_PLAYING, PLAYBACK_STARTING, PLAYBACK_STOPPED,
};
use super::worker_status::{reset_dsd_buffer_watermark, set_cover};

const FLAC_STREAM_MARKER: &[u8; 4] = b"fLaC";
const FLAC_DIRECT_SCAN_LIMIT: u64 = 16 * 1024 * 1024;
type OpenFormatResult =
    Result<(Box<dyn FormatReader>, Option<ProbedMetadata>), Box<dyn std::error::Error>>;

pub(super) struct PlaybackSession {
    pub(super) format: Box<dyn FormatReader>,
    pub(super) decoder: Box<dyn Decoder>,
    pub(super) track_id: u32,
    pub(super) dsp_path: DspPath,
    pub(super) seek_request: Option<f64>,
    pub(super) output_buffer: Vec<f64>,
    pub(super) sample_buffer: AudioBuffer<f64>,
}

pub(super) struct PendingSessionInit {
    pub(super) result:
        Result<(PlaybackSession, TrackTags, Option<TrackCover>), Box<dyn std::error::Error>>,
    pub(super) fallback_cover: Option<TrackCover>,
    pub(super) fallback_tags: Option<TrackTags>,
    pub(super) display_name: String,
    pub(super) current_file_path: Option<String>,
    pub(super) current_fallback_tags: Option<TrackTags>,
}

pub(super) struct StartedSessionRates {
    pub(super) source_rate: u32,
    pub(super) target_rate: u32,
}

pub(super) struct OffsetMediaSource {
    inner: Box<dyn MediaSource>,
    base_offset: u64,
}

impl OffsetMediaSource {
    fn new(mut inner: Box<dyn MediaSource>, base_offset: u64) -> std::io::Result<Self> {
        inner.seek(SeekFrom::Start(base_offset))?;
        Ok(Self { inner, base_offset })
    }

    fn logical_len(&self) -> Option<u64> {
        self.inner
            .byte_len()
            .map(|len| len.saturating_sub(self.base_offset))
    }
}

impl Read for OffsetMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for OffsetMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(pos) => pos,
            SeekFrom::Current(delta) => {
                let physical = self.inner.stream_position()?;
                let logical = physical.saturating_sub(self.base_offset) as i128 + delta as i128;
                if logical < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek before start of logical media source",
                    ));
                }
                logical as u64
            }
            SeekFrom::End(delta) => {
                let Some(len) = self.logical_len() else {
                    let physical = self.inner.seek(SeekFrom::End(delta))?;
                    return Ok(physical.saturating_sub(self.base_offset));
                };
                let logical = len as i128 + delta as i128;
                if logical < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek before start of logical media source",
                    ));
                }
                logical as u64
            }
        };
        let physical = self.base_offset.checked_add(target).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek position overflow in logical media source",
            )
        })?;
        self.inner.seek(SeekFrom::Start(physical))?;
        Ok(target)
    }
}

impl MediaSource for OffsetMediaSource {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        self.logical_len()
    }
}

fn open_format_from_source(
    mut source: Box<dyn MediaSource>,
    ext_hint: Option<&str>,
) -> OpenFormatResult {
    if is_flac_hint(ext_hint)
        && source.is_seekable()
        && let Some(offset) = find_flac_marker_offset(source.as_mut(), FLAC_DIRECT_SCAN_LIMIT)?
    {
        if offset > 0 {
            println!("AudioWorker: FLAC stream marker found at byte {offset}; opening from marker");
        }
        let source = Box::new(OffsetMediaSource::new(source, offset)?);
        let mss = MediaSourceStream::new(source, Default::default());
        let format =
            symphonia::default::formats::FlacReader::try_new(mss, &FormatOptions::default())?;
        return Ok((Box::new(format), None));
    }

    let mss = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    Ok((probed.format, Some(probed.metadata)))
}

pub(super) fn is_flac_hint(ext_hint: Option<&str>) -> bool {
    ext_hint
        .map(|ext| ext.trim_start_matches('.').eq_ignore_ascii_case("flac"))
        .unwrap_or(false)
}

pub(super) fn find_flac_marker_offset<R: Read + Seek + ?Sized>(
    source: &mut R,
    max_scan_bytes: u64,
) -> std::io::Result<Option<u64>> {
    let original_pos = source.stream_position()?;
    source.seek(SeekFrom::Start(0))?;

    let mut buf = [0_u8; 64 * 1024];
    let mut absolute = 0_u64;
    let mut rolling = [0_u8; 4];
    let mut rolling_len = 0_usize;
    let mut found = None;

    while absolute < max_scan_bytes {
        let to_read = (max_scan_bytes - absolute).min(buf.len() as u64) as usize;
        let read = match source.read(&mut buf[..to_read]) {
            Ok(read) => read,
            Err(err) => {
                let _ = source.seek(SeekFrom::Start(original_pos));
                return Err(err);
            }
        };
        if read == 0 {
            break;
        }

        for (idx, byte) in buf[..read].iter().copied().enumerate() {
            if rolling_len < rolling.len() {
                rolling[rolling_len] = byte;
                rolling_len += 1;
            } else {
                rolling.copy_within(1.., 0);
                rolling[rolling.len() - 1] = byte;
            }

            if rolling_len == rolling.len() && &rolling == FLAC_STREAM_MARKER {
                found = Some(absolute + idx as u64 + 1 - FLAC_STREAM_MARKER.len() as u64);
                break;
            }
        }

        if found.is_some() {
            break;
        }
        absolute += read as u64;
    }

    source.seek(SeekFrom::Start(original_pos))?;
    Ok(found)
}

// This enum owns the active real-time DSP path; keep it allocation-free after construction.
#[allow(clippy::large_enum_variant)]
pub(super) enum DspPath {
    Bypass { source_rate: u32 },
    Resample(SincResampler),
}

impl DspPath {
    fn new(
        filter_type: FilterType,
        source_rate: u32,
        target_rate: u32,
        upsampling_enabled: bool,
        device_name: Option<&str>,
    ) -> Self {
        let (target_rate, should_resample) =
            resolve_pcm_dsp_target(source_rate, target_rate, upsampling_enabled, device_name);
        if should_resample {
            DspPath::Resample(SincResampler::new(filter_type, source_rate, target_rate))
        } else {
            DspPath::Bypass { source_rate }
        }
    }

    pub(super) fn source_rate(&self) -> u32 {
        match self {
            DspPath::Bypass { source_rate } => *source_rate,
            DspPath::Resample(resampler) => resampler.source_rate(),
        }
    }

    pub(super) fn target_rate(&self) -> u32 {
        match self {
            DspPath::Bypass { source_rate } => *source_rate,
            DspPath::Resample(resampler) => resampler.target_rate(),
        }
    }

    pub(super) fn runtime_info(&self) -> Option<ResamplerRuntimeInfo> {
        match self {
            DspPath::Bypass { .. } => None,
            DspPath::Resample(resampler) => Some(resampler.runtime_info()),
        }
    }

    pub(super) fn reset(&mut self) {
        if let DspPath::Resample(resampler) = self {
            resampler.reset();
        }
    }

    pub(super) fn is_gapless_compatible_with(&self, next: &Self) -> bool {
        match (self, next) {
            (
                DspPath::Bypass { source_rate },
                DspPath::Bypass {
                    source_rate: next_source_rate,
                },
            ) => source_rate == next_source_rate,
            (DspPath::Resample(resampler), DspPath::Resample(next_resampler)) => {
                resampler.filter_type() == next_resampler.filter_type()
                    && resampler.source_rate() == next_resampler.source_rate()
                    && resampler.target_rate() == next_resampler.target_rate()
                    && resampler.runtime_info().path_kind == next_resampler.runtime_info().path_kind
            }
            _ => false,
        }
    }

    pub(super) fn render(
        &mut self,
        samples_l: &[f64],
        samples_r: &[f64],
        output: &mut Vec<f64>,
    ) -> usize {
        match self {
            DspPath::Bypass { .. } => {
                output.clear();
                let frames = samples_l.len().min(samples_r.len());
                output.reserve(frames * 2);
                for i in 0..frames {
                    output.push(samples_l[i]);
                    output.push(samples_r[i]);
                }
                frames
            }
            DspPath::Resample(resampler) => {
                resampler.input(samples_l, samples_r);
                output.clear();
                resampler.process(output)
            }
        }
    }

    pub(super) fn drain_eof(&mut self, output: &mut Vec<f64>) -> usize {
        output.clear();
        match self {
            DspPath::Bypass { .. } => 0,
            DspPath::Resample(resampler) => resampler.drain_eof(output),
        }
    }
}

impl PlaybackSession {
    pub(super) fn convert_decoded_buffer(
        sample_buffer: &mut AudioBuffer<f64>,
        decoded: &AudioBufferRef<'_>,
    ) {
        let spec = *decoded.spec();
        let capacity = decoded.capacity();
        let reuse_buffer = sample_buffer.spec() == &spec && sample_buffer.capacity() >= capacity;
        if !reuse_buffer {
            *sample_buffer = AudioBuffer::<f64>::new(capacity as u64, spec);
        } else {
            sample_buffer.clear();
        }
        decoded.convert(sample_buffer);
    }
}

pub(super) fn init_file_session(
    file_path: &str,
    filter_type: FilterType,
    target_rate: u32,
    upsampling_enabled: bool,
    device_name: Option<&str>,
) -> Result<(PlaybackSession, TrackTags, Option<TrackCover>), Box<dyn std::error::Error>> {
    let file = File::open(file_path)?;
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_string());
    let folder = Path::new(file_path).parent().map(|p| p.to_path_buf());
    init_session_from_source(
        Box::new(file),
        ext.as_deref(),
        folder.as_deref(),
        filter_type,
        target_rate,
        upsampling_enabled,
        device_name,
    )
}

pub(super) fn init_pending_start_session(
    start: PendingStart,
    filter_type: FilterType,
    target_rate: u32,
    upsampling_enabled: bool,
    device_name: Option<&str>,
) -> PendingSessionInit {
    match start {
        PendingStart::File { item, .. } => {
            println!("AudioWorker: Starting track {:?}", item.file_path);
            let display_name = Path::new(&item.file_path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| item.file_path.clone());
            let current_file_path = Some(item.file_path.clone());
            let current_fallback_tags = item.fallback_tags.clone();
            let result = init_file_session(
                &item.file_path,
                filter_type,
                target_rate,
                upsampling_enabled,
                device_name,
            );
            PendingSessionInit {
                result,
                fallback_cover: item.fallback_cover,
                fallback_tags: item.fallback_tags,
                display_name,
                current_file_path,
                current_fallback_tags,
            }
        }
        PendingStart::Stream { item, .. } => {
            let StreamQueueItem {
                source,
                ext_hint,
                display_name,
                fallback_cover,
                fallback_tags,
            } = item;
            println!("AudioWorker: Starting stream {:?}", display_name);
            let current_fallback_tags = fallback_tags.clone();
            let result = init_session_from_source(
                source,
                ext_hint.as_deref(),
                None,
                filter_type,
                target_rate,
                upsampling_enabled,
                device_name,
            );
            PendingSessionInit {
                result,
                fallback_cover,
                fallback_tags,
                display_name,
                current_file_path: None,
                current_fallback_tags,
            }
        }
    }
}

// Session install coordinates metadata, cover state, DSP state, and EQ in one restart boundary.
#[allow(clippy::too_many_arguments)]
pub(super) fn install_restarted_file_session(
    new_session: PlaybackSession,
    mut tags: TrackTags,
    cover: Option<TrackCover>,
    fallback_tags: Option<TrackTags>,
    state: &AtomicPlayerState,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
    eq_processor: &mut EqProcessor,
    eq_config: &EqConfig,
) -> (PlaybackSession, u32) {
    apply_fallback_tags(&mut tags, fallback_tags);
    let source_bits = tags.bits_per_sample.unwrap_or(16);
    *track_tags.lock().unwrap() = tags;
    if cover.is_some() {
        set_cover(track_cover, cover_version, cover);
    }

    let source_rate = new_session.dsp_path.source_rate();
    let target_rate = new_session.dsp_path.target_rate();
    eq_processor.update(target_rate, eq_config);
    eq_processor.reset();
    state
        .dsp_graph_rebuild_count
        .fetch_add(1, Ordering::Relaxed);
    state.source_rate.store(source_rate, Ordering::Relaxed);
    state.target_rate.store(target_rate, Ordering::Relaxed);
    state.store_src_runtime_info(new_session.dsp_path.runtime_info());
    state.source_bits.store(source_bits, Ordering::Relaxed);

    (new_session, target_rate)
}

// File restart carries the full playback-session context needed to rebuild state atomically.
#[allow(clippy::too_many_arguments)]
pub(super) fn restart_file_session(
    file_path: &str,
    filter_type: FilterType,
    target_rate: u32,
    upsampling_enabled: bool,
    device_name: Option<&str>,
    fallback_tags: Option<TrackTags>,
    state: &AtomicPlayerState,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
    eq_processor: &mut EqProcessor,
    eq_config: &EqConfig,
) -> Result<(PlaybackSession, u32), Box<dyn std::error::Error>> {
    let (new_session, tags, cover) = init_file_session(
        file_path,
        filter_type,
        target_rate,
        upsampling_enabled,
        device_name,
    )?;
    Ok(install_restarted_file_session(
        new_session,
        tags,
        cover,
        fallback_tags,
        state,
        track_tags,
        track_cover,
        cover_version,
        eq_processor,
        eq_config,
    ))
}

// Current-session restart preserves playback, metadata, cover, and resume state together.
#[allow(clippy::too_many_arguments)]
pub(super) fn restart_current_file_session(
    current_file_path: Option<&str>,
    filter_type: FilterType,
    target_rate: u32,
    upsampling_enabled: bool,
    device_name: Option<&str>,
    fallback_tags: Option<TrackTags>,
    state: &AtomicPlayerState,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
    eq_processor: &mut EqProcessor,
    eq_config: &EqConfig,
    stop_while_restarting: bool,
    resume_seconds: Option<f64>,
) -> Result<Option<(PlaybackSession, u32)>, Box<dyn std::error::Error>> {
    let Some(path) = current_file_path else {
        return Ok(None);
    };

    let active_dsd_position_rate =
        if OutputMode::from_id(state.active_output_mode.load(Ordering::Relaxed)).is_dsd() {
            Some(state.target_rate.load(Ordering::Relaxed))
        } else {
            None
        };

    if stop_while_restarting {
        state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
    }
    state.request_flush(FLUSH_REASON_RESTART_SESSION);

    let (mut restarted_session, restarted_target_rate) = restart_file_session(
        path,
        filter_type,
        target_rate,
        upsampling_enabled,
        device_name,
        fallback_tags,
        state,
        track_tags,
        track_cover,
        cover_version,
        eq_processor,
        eq_config,
    )?;

    if let Some(seconds) = resume_seconds.filter(|seconds| seconds.is_finite() && *seconds > 0.0) {
        let resume_position_rate = active_dsd_position_rate.unwrap_or(restarted_target_rate);
        if !apply_seek_to_session(
            &mut restarted_session,
            seconds,
            state,
            None,
            eq_processor,
            resume_position_rate,
        ) {
            state.position_samples.store(0, Ordering::Relaxed);
        }
    }

    if stop_while_restarting {
        state.state.store(PLAYBACK_STARTING, Ordering::Relaxed);
    }
    Ok(Some((restarted_session, restarted_target_rate)))
}

// Reconfiguration mirrors the live DSP/session fields that must change together.
#[allow(clippy::too_many_arguments)]
pub(super) fn reconfigure_current_session(
    session: &mut PlaybackSession,
    filter_type: FilterType,
    target_rate: u32,
    upsampling_enabled: bool,
    device_name: Option<&str>,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
    eq_config: &EqConfig,
) -> u32 {
    let source_rate = session.dsp_path.source_rate();
    session.dsp_path = DspPath::new(
        filter_type,
        source_rate,
        target_rate,
        upsampling_enabled,
        device_name,
    );
    session.output_buffer.clear();
    state.request_flush(FLUSH_REASON_RECONFIGURE_SESSION);

    let new_target_rate = session.dsp_path.target_rate();
    eq_processor.update(new_target_rate, eq_config);
    eq_processor.reset();
    state
        .dsp_graph_rebuild_count
        .fetch_add(1, Ordering::Relaxed);
    state.source_rate.store(source_rate, Ordering::Relaxed);
    state.target_rate.store(new_target_rate, Ordering::Relaxed);
    state.store_src_runtime_info(session.dsp_path.runtime_info());
    new_target_rate
}

// Publishing session metadata updates tags, cover, atomics, and version counters as one event.
#[allow(clippy::too_many_arguments)]
pub(super) fn publish_started_session_metadata(
    session: &PlaybackSession,
    mut tags: TrackTags,
    cover: Option<TrackCover>,
    fallback_cover: Option<TrackCover>,
    fallback_tags: Option<TrackTags>,
    state: &AtomicPlayerState,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
) -> StartedSessionRates {
    apply_fallback_tags(&mut tags, fallback_tags);
    let source_bits = tags.bits_per_sample.unwrap_or(16);
    *track_tags.lock().unwrap() = tags;

    // Embedded or sidecar art wins; fall back to the cover the library handed
    // us. Always replace, including with None, so missing art doesn't keep the
    // previous track's cover.
    set_cover(track_cover, cover_version, cover.or(fallback_cover));

    let source_rate = session.dsp_path.source_rate();
    let target_rate = session.dsp_path.target_rate();
    state.source_rate.store(source_rate, Ordering::Relaxed);
    state.store_src_runtime_info(session.dsp_path.runtime_info());
    state.source_bits.store(source_bits, Ordering::Relaxed);
    state.duration_samples.store(
        session.format.tracks()[0]
            .codec_params
            .n_frames
            .unwrap_or(0),
        Ordering::Relaxed,
    );
    state.position_samples.store(0, Ordering::Relaxed);

    StartedSessionRates {
        source_rate,
        target_rate,
    }
}

pub(super) fn init_session_from_source(
    source: Box<dyn MediaSource>,
    ext_hint: Option<&str>,
    folder_for_cover: Option<&Path>,
    filter_type: FilterType,
    target_rate: u32,
    upsampling_enabled: bool,
    device_name: Option<&str>,
) -> Result<(PlaybackSession, TrackTags, Option<TrackCover>), Box<dyn std::error::Error>> {
    let (mut format, mut probed_metadata) = open_format_from_source(source, ext_hint)?;

    let (mut tags, cover) =
        collect_reader_metadata(&mut format, &mut probed_metadata, folder_for_cover);

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("No playable audio track found")?;

    let track_id = track.id;
    let decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let source_rate = track.codec_params.sample_rate.unwrap_or(44100);
    tags.bits_per_sample = track.codec_params.bits_per_sample;

    let dsp_path = DspPath::new(
        filter_type,
        source_rate,
        target_rate,
        upsampling_enabled,
        device_name,
    );

    let session = PlaybackSession {
        format,
        decoder,
        track_id,
        dsp_path,
        seek_request: None,
        output_buffer: Vec::with_capacity(16384),
        sample_buffer: AudioBuffer::<f64>::unused(),
    };
    Ok((session, tags, cover))
}

pub(super) fn apply_seek_to_session(
    sess: &mut PlaybackSession,
    seconds: f64,
    state: &AtomicPlayerState,
    dsd_state: Option<&mut DsdWorkerState>,
    eq_processor: &mut EqProcessor,
    target_rate: u32,
) -> bool {
    let secs = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: seek boundary before: seconds={:.3} {}",
            secs,
            state.diagnostics_debug_summary()
        );
    }
    let target_time = Time::new(secs.floor() as u64, secs.fract());
    let seek_to = SeekTo::Time {
        time: target_time,
        track_id: Some(sess.track_id),
    };
    if sess.format.seek(SeekMode::Accurate, seek_to).is_err() {
        eprintln!("AudioWorker: Seek failed");
        return false;
    }

    sess.decoder.reset();
    sess.dsp_path.reset();
    sess.output_buffer.clear();
    if let Some(ds) = dsd_state {
        ds.reset_for_playback_boundary_with_diagnostics(state);
    }
    eq_processor.reset();
    if state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING {
        state.state.store(PLAYBACK_STARTING, Ordering::Relaxed);
    }
    state.request_flush(FLUSH_REASON_SEEK);
    state.underrun_events.store(0, Ordering::Relaxed);
    state.underrun_samples.store(0, Ordering::Relaxed);
    reset_dsd_buffer_watermark(state);
    state.dsd_overbudget_blocks.store(0, Ordering::Relaxed);
    state
        .dsd_last_load
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    state
        .dsd_recent_load_p95
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    state
        .dsd_recent_load_p99
        .store(0.0f32.to_bits(), Ordering::Relaxed);

    let tgt_rate = if target_rate > 0 {
        target_rate
    } else {
        sess.dsp_path.target_rate()
    };
    let target_samples_upsampled = (secs * tgt_rate as f64) as u64;
    state
        .position_samples
        .store(target_samples_upsampled, Ordering::Relaxed);
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: seek boundary after: seconds={:.3} {}",
            secs,
            state.diagnostics_debug_summary()
        );
    }
    println!("AudioWorker: Successfully seeked to {}s", secs);
    true
}

#[cfg(test)]
mod tests {
    use super::{
        DspPath, OffsetMediaSource, TrackTags, find_flac_marker_offset,
        restart_current_file_session,
    };
    use crate::audio::dsp::eq::{EqConfig, EqProcessor};
    use crate::audio::dsp::resampler::FilterType;
    use crate::audio::engine::signal_path::OutputMode;
    use crate::audio::engine::state::{AtomicPlayerState, PLAYBACK_STARTING};
    use std::fs::{File, remove_file};
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use symphonia::core::io::MediaSource;

    #[test]
    fn flac_marker_scan_finds_marker_past_probe_limit_and_restores_position() {
        let mut bytes = vec![0_u8; 1_100_000];
        bytes.extend_from_slice(b"fLaC");
        bytes.extend_from_slice(&[0_u8; 16]);
        let mut source = Cursor::new(bytes);
        source.seek(SeekFrom::Start(123)).unwrap();

        let offset = find_flac_marker_offset(&mut source, 2 * 1024 * 1024).unwrap();

        assert_eq!(offset, Some(1_100_000));
        assert_eq!(source.stream_position().unwrap(), 123);
    }

    #[test]
    fn offset_media_source_maps_logical_start_to_marker() {
        let inner: Box<dyn MediaSource> = Box::new(Cursor::new(b"junkfLaCdata".to_vec()));
        let mut source = OffsetMediaSource::new(inner, 4).unwrap();
        let mut marker = [0_u8; 4];

        source.read_exact(&mut marker).unwrap();
        assert_eq!(&marker, b"fLaC");
        assert_eq!(source.seek(SeekFrom::Start(0)).unwrap(), 0);
        source.read_exact(&mut marker).unwrap();
        assert_eq!(&marker, b"fLaC");
        assert_eq!(source.byte_len(), Some(8));
    }

    #[test]
    fn restart_current_file_session_resumes_rebuilt_decoder_at_previous_position() {
        let sample_rate = 44_100;
        let dsp_target_rate = 88_200;
        let resume_seconds = 1.0;
        let wav_path = unique_test_wav_path("restart-resume");
        write_silence_wav(&wav_path, sample_rate, sample_rate * 3);

        let state = AtomicPlayerState::new();
        state
            .position_samples
            .store(sample_rate as u64, Ordering::Relaxed);
        state.target_rate.store(sample_rate, Ordering::Relaxed);
        let track_tags = Mutex::new(TrackTags::default());
        let track_cover = Mutex::new(None);
        let cover_version = AtomicU64::new(0);
        let eq_config = EqConfig::default();
        let mut eq_processor = EqProcessor::new(sample_rate, &eq_config);
        let path = wav_path.to_string_lossy();

        let (mut restarted_session, restarted_target_rate) = restart_current_file_session(
            Some(path.as_ref()),
            FilterType::LinearPhase128k,
            dsp_target_rate,
            true,
            None,
            None,
            &state,
            &track_tags,
            &track_cover,
            &cover_version,
            &mut eq_processor,
            &eq_config,
            true,
            Some(resume_seconds),
        )
        .unwrap()
        .unwrap();

        let first_packet = loop {
            let packet = restarted_session.format.next_packet().unwrap();
            if packet.track_id() == restarted_session.track_id {
                break packet;
            }
        };

        assert_eq!(restarted_target_rate, dsp_target_rate);
        assert_eq!(
            state.position_samples.load(Ordering::Relaxed),
            dsp_target_rate as u64
        );
        assert_eq!(state.state.load(Ordering::Relaxed), PLAYBACK_STARTING);
        assert!(
            first_packet.ts() >= sample_rate as u64 / 2,
            "restarted packet timestamp should be near the resume point, got {}",
            first_packet.ts()
        );

        remove_file(wav_path).unwrap();
    }

    #[test]
    fn restart_current_file_session_preserves_dsd_position_units() {
        let sample_rate = 44_100;
        let dsp_target_rate = 176_400;
        let wire_rate = OutputMode::Dsd128.dsd_wire_rate(sample_rate).unwrap();
        let wav_path = unique_test_wav_path("restart-dsd-resume");
        write_silence_wav(&wav_path, sample_rate, sample_rate * 3);

        let state = AtomicPlayerState::new();
        state
            .active_output_mode
            .store(OutputMode::Dsd128.as_id(), Ordering::Relaxed);
        state.target_rate.store(wire_rate, Ordering::Relaxed);
        state
            .position_samples
            .store(wire_rate as u64, Ordering::Relaxed);
        let track_tags = Mutex::new(TrackTags::default());
        let track_cover = Mutex::new(None);
        let cover_version = AtomicU64::new(0);
        let eq_config = EqConfig::default();
        let mut eq_processor = EqProcessor::new(dsp_target_rate, &eq_config);
        let path = wav_path.to_string_lossy();

        let (_, restarted_target_rate) = restart_current_file_session(
            Some(path.as_ref()),
            FilterType::LinearPhase128k,
            dsp_target_rate,
            true,
            None,
            None,
            &state,
            &track_tags,
            &track_cover,
            &cover_version,
            &mut eq_processor,
            &eq_config,
            true,
            Some(1.0),
        )
        .unwrap()
        .unwrap();

        assert_eq!(restarted_target_rate, dsp_target_rate);
        assert_eq!(
            state.position_samples.load(Ordering::Relaxed),
            wire_rate as u64
        );

        remove_file(wav_path).unwrap();
    }

    #[test]
    fn dsp_path_uses_long_filter_resampler_for_downsampling() {
        let path = DspPath::new(FilterType::SplitPhase128kE3, 96_000, 44_100, true, None);
        let DspPath::Resample(resampler) = path else {
            panic!("expected resampling path");
        };

        assert_eq!(resampler.source_rate(), 96_000);
        assert_eq!(resampler.target_rate(), 44_100);
        assert_eq!(resampler.filter_type(), FilterType::SplitPhase128kE3);
        assert!(resampler.is_high_latency());
    }

    #[test]
    fn dsp_path_gapless_compatibility_requires_matching_processing_path() {
        let current = DspPath::new(FilterType::SplitPhase128kE3, 44_100, 176_400, true, None);
        let same = DspPath::new(FilterType::SplitPhase128kE3, 44_100, 176_400, true, None);
        let different_filter = DspPath::new(FilterType::Minimum16k, 44_100, 176_400, true, None);
        let different_source =
            DspPath::new(FilterType::SplitPhase128kE3, 48_000, 192_000, true, None);
        let bypass = DspPath::new(FilterType::SplitPhase128kE3, 44_100, 176_400, false, None);

        assert!(current.is_gapless_compatible_with(&same));
        assert!(!current.is_gapless_compatible_with(&different_filter));
        assert!(!current.is_gapless_compatible_with(&different_source));
        assert!(!current.is_gapless_compatible_with(&bypass));
    }

    #[test]
    fn bypass_gapless_compatibility_requires_matching_source_rate() {
        let current = DspPath::new(FilterType::SplitPhase128kE3, 44_100, 176_400, false, None);
        let same = DspPath::new(FilterType::Minimum16k, 44_100, 192_000, false, None);
        let different_source =
            DspPath::new(FilterType::SplitPhase128kE3, 48_000, 176_400, false, None);

        assert!(current.is_gapless_compatible_with(&same));
        assert!(!current.is_gapless_compatible_with(&different_source));
    }

    fn unique_test_wav_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("fozmo-{label}-{}-{nanos}.wav", std::process::id()))
    }

    fn write_silence_wav(path: &Path, sample_rate: u32, frames: u32) {
        let channels = 2_u16;
        let bits_per_sample = 16_u16;
        let bytes_per_sample = bits_per_sample / 8;
        let block_align = channels * bytes_per_sample;
        let byte_rate = sample_rate * block_align as u32;
        let data_bytes = frames * block_align as u32;
        let mut file = File::create(path).unwrap();

        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + data_bytes).to_le_bytes()).unwrap();
        file.write_all(b"WAVE").unwrap();
        file.write_all(b"fmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&channels.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&byte_rate.to_le_bytes()).unwrap();
        file.write_all(&block_align.to_le_bytes()).unwrap();
        file.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&data_bytes.to_le_bytes()).unwrap();
        file.write_all(&vec![0_u8; data_bytes as usize]).unwrap();
    }
}
