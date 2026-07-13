use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::audio::dsp::eq::{EqConfig, EqProcessor};
use crate::audio::dsp::resampler::FilterType;

use super::metadata::{TrackCover, TrackTags};
use super::signal_path::{OutputMode, OutputTransport};
use super::state::{AtomicPlayerState, PLAYBACK_STOPPED};

pub(super) fn set_cover(
    slot: &Mutex<Option<TrackCover>>,
    version: &AtomicU64,
    value: Option<TrackCover>,
) {
    *slot.lock().unwrap() = value;
    version.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn clear_now_playing(
    file_name: &Mutex<Option<String>>,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
) {
    *file_name.lock().unwrap() = None;
    *track_tags.lock().unwrap() = TrackTags::default();
    set_cover(track_cover, cover_version, None);
}

pub(super) fn clear_timeline(state: &AtomicPlayerState) {
    state.position_samples.store(0, Ordering::Relaxed);
    state.duration_samples.store(0, Ordering::Relaxed);
}

pub(super) fn stop_without_output_flush(state: &AtomicPlayerState) {
    state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
}

pub(super) fn stop_after_failed_start(state: &AtomicPlayerState) {
    state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
    clear_timeline(state);
    state.reset_boundary_diagnostic_maxima();
}

pub(super) fn stop_after_eof_without_next(
    file_name: &Mutex<Option<String>>,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
    state: &AtomicPlayerState,
) {
    state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
    clear_now_playing(file_name, track_tags, track_cover, cover_version);
}

pub(super) fn clear_stop_metrics(state: &AtomicPlayerState) {
    clear_timeline(state);
    state.resample_time_ns.store(0, Ordering::Relaxed);
    state.dsd_upsample_time_ns.store(0, Ordering::Relaxed);
    state.dsd_modulate_time_ns.store(0, Ordering::Relaxed);
    state.dsd_output_pending_samples.store(0, Ordering::Relaxed);
    clear_dsd_buffer_health(state);
    clear_pcm_buffer_health(state);
    state.dsd_overbudget_blocks.store(0, Ordering::Relaxed);
    state.reset_boundary_diagnostic_maxima();
    state.reset_decode_timing_metrics();
    state
        .dsd_last_load
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    state
        .dsd_recent_load_p95
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    state
        .dsd_recent_load_p99
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    state.block_duration_ns.store(0, Ordering::Relaxed);
    state.meter_l.store(0, Ordering::Relaxed);
    state.meter_r.store(0, Ordering::Relaxed);
    state.reset_signal_level_metrics();
    state.underrun_events.store(0, Ordering::Relaxed);
    state.underrun_samples.store(0, Ordering::Relaxed);
    state.source_bits.store(16, Ordering::Relaxed);
    state.target_bits.store(24, Ordering::Relaxed);
    state
        .output_transport
        .store(OutputTransport::None.as_id(), Ordering::Relaxed);
}

pub(super) fn clear_dsd_buffer_health(state: &AtomicPlayerState) {
    state.dsd_ring_capacity_samples.store(0, Ordering::Relaxed);
    state.dsd_ring_fill_samples.store(0, Ordering::Relaxed);
    state
        .dsd_ring_low_watermark_samples
        .store(u64::MAX, Ordering::Relaxed);
    state.dsd_callback_frames.store(0, Ordering::Relaxed);
    state
        .dsd_requested_hardware_buffer_frames
        .store(0, Ordering::Relaxed);
    state
        .dsd_hardware_buffer_min_frames
        .store(0, Ordering::Relaxed);
    state
        .dsd_hardware_buffer_max_frames
        .store(0, Ordering::Relaxed);
    state.dsd_hardware_buffer_frames.store(0, Ordering::Relaxed);
    state.dsd_lock_miss_events.store(0, Ordering::Relaxed);
    state
        .dsd_callback_deadline_miss_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_soft_callback_gap_125_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_soft_callback_gap_150_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_soft_callback_gap_175_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_last_soft_callback_gap_ns
        .store(0, Ordering::Relaxed);
    state
        .dsd_last_soft_callback_gap_at_ms
        .store(0, Ordering::Relaxed);
    state
        .dsd_ring_below_250ms_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_ring_below_100ms_events
        .store(0, Ordering::Relaxed);
    state.dsd_ring_below_50ms_events.store(0, Ordering::Relaxed);
    state
        .dsd_ring_below_callback_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_last_ring_pressure_at_ms
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_marker_error_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_program_idle_splice_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_repeated_payload_events
        .store(0, Ordering::Relaxed);
    state.dsd_dop_callback_index.store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_callback_at_ms
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_callback_gap_ns
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_callback_frames
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_output_kind_id
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_ring_fill_samples
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_program_read_samples
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_ring_read_cursor_samples
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_payload_fingerprint
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_last_payload_fingerprint_at_ms
        .store(0, Ordering::Relaxed);
    state.dsd_dop_marker_scan_count.store(0, Ordering::Relaxed);
    state
        .dsd_dop_every_callback_scan_enabled
        .store(false, Ordering::Relaxed);
    state.dsd_last_underrun_at_ms.store(0, Ordering::Relaxed);
}

pub(super) fn clear_pcm_buffer_health(state: &AtomicPlayerState) {
    state.pcm_ring_capacity_samples.store(0, Ordering::Relaxed);
    state.pcm_ring_fill_samples.store(0, Ordering::Relaxed);
    state
        .pcm_ring_low_watermark_samples
        .store(u64::MAX, Ordering::Relaxed);
    state.pcm_callback_frames.store(0, Ordering::Relaxed);
    state.pcm_lock_miss_events.store(0, Ordering::Relaxed);
}

pub(super) fn reset_dsd_buffer_watermark(state: &AtomicPlayerState) {
    let fill = state.dsd_ring_fill_samples.load(Ordering::Relaxed);
    let low = if fill == 0 { u64::MAX } else { fill };
    state
        .dsd_ring_low_watermark_samples
        .store(low, Ordering::Relaxed);
    state.dsd_lock_miss_events.store(0, Ordering::Relaxed);
    state
        .dsd_callback_deadline_miss_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_soft_callback_gap_125_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_soft_callback_gap_150_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_soft_callback_gap_175_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_last_soft_callback_gap_ns
        .store(0, Ordering::Relaxed);
    state
        .dsd_last_soft_callback_gap_at_ms
        .store(0, Ordering::Relaxed);
    state
        .dsd_ring_below_250ms_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_ring_below_100ms_events
        .store(0, Ordering::Relaxed);
    state.dsd_ring_below_50ms_events.store(0, Ordering::Relaxed);
    state
        .dsd_ring_below_callback_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_last_ring_pressure_at_ms
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_marker_error_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_program_idle_splice_events
        .store(0, Ordering::Relaxed);
    state
        .dsd_dop_repeated_payload_events
        .store(0, Ordering::Relaxed);
    state.dsd_last_underrun_at_ms.store(0, Ordering::Relaxed);
    state.reset_boundary_diagnostic_maxima();
}

pub(super) fn reset_pcm_buffer_watermark(state: &AtomicPlayerState) {
    let fill = state.pcm_ring_fill_samples.load(Ordering::Relaxed);
    let low = if fill == 0 { u64::MAX } else { fill };
    state
        .pcm_ring_low_watermark_samples
        .store(low, Ordering::Relaxed);
    state.pcm_lock_miss_events.store(0, Ordering::Relaxed);
    state.reset_boundary_diagnostic_maxima();
}

pub(super) fn stop_and_clear_now_playing(
    file_name: &Mutex<Option<String>>,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
    state: &AtomicPlayerState,
) {
    clear_now_playing(file_name, track_tags, track_cover, cover_version);
    clear_timeline(state);
    stop_without_output_flush(state);
}

pub(super) fn full_stop_and_clear_now_playing(
    file_name: &Mutex<Option<String>>,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
    state: &AtomicPlayerState,
) {
    clear_now_playing(file_name, track_tags, track_cover, cover_version);
    clear_stop_metrics(state);
    stop_without_output_flush(state);
}

pub(super) fn publish_start_failure(
    file_name: &Mutex<Option<String>>,
    track_tags: &Mutex<TrackTags>,
    track_cover: &Mutex<Option<TrackCover>>,
    cover_version: &AtomicU64,
    error: &dyn std::fmt::Display,
) {
    *file_name.lock().unwrap() = Some(format!("Error: {error}"));
    *track_tags.lock().unwrap() = TrackTags::default();
    set_cover(track_cover, cover_version, None);
}

pub(super) fn publish_output_notice(
    state: &AtomicPlayerState,
    notice: &Mutex<Option<String>>,
    message: String,
) {
    *notice.lock().unwrap() = Some(message);
    state.output_notice_id.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn publish_config_status(
    state: &AtomicPlayerState,
    filter_type: FilterType,
    configured_target_rate: u32,
    upsampling_enabled: bool,
    exclusive_mode: bool,
    dsp_buffer_ms: u32,
    output_mode: OutputMode,
) {
    state
        .filter_type
        .store(filter_type.as_id(), Ordering::Relaxed);
    if !output_mode.is_dsd() {
        state
            .active_output_mode
            .store(OutputMode::Pcm.as_id(), Ordering::Relaxed);
        state
            .active_filter_type
            .store(filter_type.as_id(), Ordering::Relaxed);
    }
    state
        .configured_target_rate
        .store(configured_target_rate, Ordering::Relaxed);
    state
        .upsampling_enabled
        .store(upsampling_enabled, Ordering::Relaxed);
    state.exclusive.store(exclusive_mode, Ordering::Relaxed);
    state.dsp_buffer_ms.store(dsp_buffer_ms, Ordering::Relaxed);
}

pub(super) struct OutputSignalStatus {
    pub(super) target_rate: u32,
    pub(super) eq_rate: u32,
    pub(super) target_bits: u32,
    pub(super) active_mode: OutputMode,
    pub(super) active_filter: FilterType,
    pub(super) transport: OutputTransport,
    pub(super) exclusive: Option<bool>,
}

pub(super) fn install_output_signal_status(
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
    eq_config: &EqConfig,
    status: OutputSignalStatus,
) -> u32 {
    eq_processor.update(status.eq_rate, eq_config);
    eq_processor.reset();
    state.store_target_rate_for_output(status.target_rate);
    state
        .target_bits
        .store(status.target_bits, Ordering::Relaxed);
    if let Some(exclusive) = status.exclusive {
        state.exclusive.store(exclusive, Ordering::Relaxed);
    }
    state
        .active_output_mode
        .store(status.active_mode.as_id(), Ordering::Relaxed);
    state
        .active_filter_type
        .store(status.active_filter.as_id(), Ordering::Relaxed);
    state
        .output_transport
        .store(status.transport.as_id(), Ordering::Relaxed);
    status.target_rate
}

pub(super) fn publish_pcm_fallback_status(
    state: &AtomicPlayerState,
    target_rate: u32,
    filter_type: FilterType,
) {
    state.target_rate.store(target_rate, Ordering::Relaxed);
    publish_active_pcm_status(state, filter_type);
}

pub(super) fn publish_active_pcm_status(state: &AtomicPlayerState, filter_type: FilterType) {
    state
        .active_output_mode
        .store(OutputMode::Pcm.as_id(), Ordering::Relaxed);
    state
        .active_filter_type
        .store(filter_type.as_id(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::super::state::PLAYBACK_PLAYING;
    use super::*;

    #[test]
    fn output_notice_replaces_message_and_bumps_id() {
        let state = AtomicPlayerState::new();
        let notice = Mutex::new(None);

        publish_output_notice(&state, &notice, "first".to_string());
        publish_output_notice(&state, &notice, "second".to_string());

        assert_eq!(notice.lock().unwrap().as_deref(), Some("second"));
        assert_eq!(state.output_notice_id.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn pcm_fallback_status_keeps_requested_rate_and_filter_visible() {
        let state = AtomicPlayerState::new();

        publish_pcm_fallback_status(&state, 176_400, FilterType::SincExtreme32k);

        assert_eq!(state.target_rate.load(Ordering::Relaxed), 176_400);
        assert_eq!(
            state.active_output_mode.load(Ordering::Relaxed),
            OutputMode::Pcm.as_id()
        );
        assert_eq!(
            state.active_filter_type.load(Ordering::Relaxed),
            FilterType::SincExtreme32k.as_id()
        );
    }

    #[test]
    fn active_pcm_status_sets_mode_and_filter_without_target_rate() {
        let state = AtomicPlayerState::new();
        state.target_rate.store(96_000, Ordering::Relaxed);

        publish_active_pcm_status(&state, FilterType::SincExtreme32k);

        assert_eq!(state.target_rate.load(Ordering::Relaxed), 96_000);
        assert_eq!(
            state.active_output_mode.load(Ordering::Relaxed),
            OutputMode::Pcm.as_id()
        );
        assert_eq!(
            state.active_filter_type.load(Ordering::Relaxed),
            FilterType::SincExtreme32k.as_id()
        );
    }

    #[test]
    fn config_status_publishes_pcm_filter_and_settings() {
        let state = AtomicPlayerState::new();

        publish_config_status(
            &state,
            FilterType::SincExtreme32k,
            176_400,
            false,
            false,
            200,
            OutputMode::Pcm,
        );

        assert_eq!(
            state.filter_type.load(Ordering::Relaxed),
            FilterType::SincExtreme32k.as_id()
        );
        assert_eq!(
            state.active_output_mode.load(Ordering::Relaxed),
            OutputMode::Pcm.as_id()
        );
        assert_eq!(
            state.active_filter_type.load(Ordering::Relaxed),
            FilterType::SincExtreme32k.as_id()
        );
        assert_eq!(
            state.configured_target_rate.load(Ordering::Relaxed),
            176_400
        );
        assert!(!state.upsampling_enabled.load(Ordering::Relaxed));
        assert!(!state.exclusive.load(Ordering::Relaxed));
        assert_eq!(state.dsp_buffer_ms.load(Ordering::Relaxed), 200);
    }

    #[test]
    fn config_status_keeps_active_filter_while_dsd_is_active() {
        let state = AtomicPlayerState::new();
        state
            .active_filter_type
            .store(FilterType::SincExtreme32k.as_id(), Ordering::Relaxed);

        publish_config_status(
            &state,
            FilterType::SincExtreme32k,
            352_800,
            true,
            true,
            0,
            OutputMode::Dsd128,
        );

        assert_eq!(
            state.filter_type.load(Ordering::Relaxed),
            FilterType::SincExtreme32k.as_id()
        );
        assert_eq!(
            state.active_filter_type.load(Ordering::Relaxed),
            FilterType::SincExtreme32k.as_id()
        );
        assert_eq!(
            state.configured_target_rate.load(Ordering::Relaxed),
            352_800
        );
        assert!(state.upsampling_enabled.load(Ordering::Relaxed));
        assert!(state.exclusive.load(Ordering::Relaxed));
    }

    #[test]
    fn clearing_buffer_health_keeps_low_watermarks_unmeasured() {
        let state = AtomicPlayerState::new();
        state
            .dsd_ring_low_watermark_samples
            .store(0, Ordering::Relaxed);
        state
            .pcm_ring_low_watermark_samples
            .store(0, Ordering::Relaxed);
        state
            .dsd_callback_deadline_miss_events
            .store(3, Ordering::Relaxed);
        state
            .dsd_dop_marker_error_events
            .store(4, Ordering::Relaxed);
        state
            .dsd_dop_program_idle_splice_events
            .store(5, Ordering::Relaxed);
        state
            .dsd_dop_repeated_payload_events
            .store(6, Ordering::Relaxed);

        clear_dsd_buffer_health(&state);
        clear_pcm_buffer_health(&state);

        assert_eq!(
            state.dsd_ring_low_watermark_samples.load(Ordering::Relaxed),
            u64::MAX
        );
        assert_eq!(
            state.pcm_ring_low_watermark_samples.load(Ordering::Relaxed),
            u64::MAX
        );
        assert_eq!(
            state
                .dsd_callback_deadline_miss_events
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(state.dsd_dop_marker_error_events.load(Ordering::Relaxed), 0);
        assert_eq!(
            state
                .dsd_dop_program_idle_splice_events
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            state
                .dsd_dop_repeated_payload_events
                .load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn full_stop_clears_now_playing_metrics_without_flushing_output() {
        let state = AtomicPlayerState::new();
        let file_name = Mutex::new(Some("track.flac".to_string()));
        let track_tags = Mutex::new(TrackTags {
            title: Some("Track".to_string()),
            ..TrackTags::default()
        });
        let track_cover = Mutex::new(Some(TrackCover {
            mime: "image/jpeg".to_string(),
            data: vec![1, 2, 3],
        }));
        let cover_version = AtomicU64::new(7);
        state.state.store(PLAYBACK_PLAYING, Ordering::Relaxed);
        state.position_samples.store(123, Ordering::Relaxed);
        state.duration_samples.store(456, Ordering::Relaxed);
        state.resample_time_ns.store(789, Ordering::Relaxed);
        state
            .output_transport
            .store(OutputTransport::PcmShared.as_id(), Ordering::Relaxed);

        full_stop_and_clear_now_playing(
            &file_name,
            &track_tags,
            &track_cover,
            &cover_version,
            &state,
        );

        assert!(file_name.lock().unwrap().is_none());
        let tags = track_tags.lock().unwrap();
        assert!(tags.title.is_none());
        assert!(tags.artist.is_none());
        assert!(tags.album.is_none());
        drop(tags);
        assert!(track_cover.lock().unwrap().is_none());
        assert_eq!(cover_version.load(Ordering::Relaxed), 8);
        assert_eq!(state.state.load(Ordering::Relaxed), PLAYBACK_STOPPED);
        assert!(!state.flush_buffer.load(Ordering::Relaxed));
        assert_eq!(state.position_samples.load(Ordering::Relaxed), 0);
        assert_eq!(state.duration_samples.load(Ordering::Relaxed), 0);
        assert_eq!(state.resample_time_ns.load(Ordering::Relaxed), 0);
        assert_eq!(
            state.output_transport.load(Ordering::Relaxed),
            OutputTransport::None.as_id()
        );
    }

    #[test]
    fn start_failure_publishes_error_metadata_and_clears_cover() {
        let file_name = Mutex::new(Some("old.flac".to_string()));
        let track_tags = Mutex::new(TrackTags {
            title: Some("Old".to_string()),
            artist: Some("Artist".to_string()),
            ..TrackTags::default()
        });
        let track_cover = Mutex::new(Some(TrackCover {
            mime: "image/png".to_string(),
            data: vec![9],
        }));
        let cover_version = AtomicU64::new(3);

        publish_start_failure(
            &file_name,
            &track_tags,
            &track_cover,
            &cover_version,
            &"decode failed",
        );

        assert_eq!(
            file_name.lock().unwrap().as_deref(),
            Some("Error: decode failed")
        );
        let tags = track_tags.lock().unwrap();
        assert!(tags.title.is_none());
        assert!(tags.artist.is_none());
        drop(tags);
        assert!(track_cover.lock().unwrap().is_none());
        assert_eq!(cover_version.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn failed_start_stop_resets_timeline_without_flushing() {
        let state = AtomicPlayerState::new();
        state.state.store(PLAYBACK_PLAYING, Ordering::Relaxed);
        state.position_samples.store(42, Ordering::Relaxed);
        state.duration_samples.store(99, Ordering::Relaxed);
        state.flush_buffer.store(false, Ordering::Relaxed);

        stop_after_failed_start(&state);

        assert_eq!(state.state.load(Ordering::Relaxed), PLAYBACK_STOPPED);
        assert_eq!(state.position_samples.load(Ordering::Relaxed), 0);
        assert_eq!(state.duration_samples.load(Ordering::Relaxed), 0);
        assert!(!state.flush_buffer.load(Ordering::Relaxed));
    }

    #[test]
    fn eof_without_next_stops_and_clears_now_playing_only() {
        let state = AtomicPlayerState::new();
        let file_name = Mutex::new(Some("track.flac".to_string()));
        let track_tags = Mutex::new(TrackTags {
            title: Some("Track".to_string()),
            ..TrackTags::default()
        });
        let track_cover = Mutex::new(Some(TrackCover {
            mime: "image/jpeg".to_string(),
            data: vec![1],
        }));
        let cover_version = AtomicU64::new(2);
        state.state.store(PLAYBACK_PLAYING, Ordering::Relaxed);
        state.position_samples.store(1200, Ordering::Relaxed);
        state.duration_samples.store(2400, Ordering::Relaxed);
        state.flush_buffer.store(false, Ordering::Relaxed);

        stop_after_eof_without_next(
            &file_name,
            &track_tags,
            &track_cover,
            &cover_version,
            &state,
        );

        assert_eq!(state.state.load(Ordering::Relaxed), PLAYBACK_STOPPED);
        assert!(file_name.lock().unwrap().is_none());
        assert!(track_tags.lock().unwrap().title.is_none());
        assert!(track_cover.lock().unwrap().is_none());
        assert_eq!(cover_version.load(Ordering::Relaxed), 3);
        assert_eq!(state.position_samples.load(Ordering::Relaxed), 1200);
        assert_eq!(state.duration_samples.load(Ordering::Relaxed), 2400);
        assert!(!state.flush_buffer.load(Ordering::Relaxed));
    }
}
