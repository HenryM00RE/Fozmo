use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::audio::dsp::resampler::FilterType;
use crate::settings::DsdSourceRule;

use super::buffers::{output_pending_len, output_start_preroll_ready};
use super::dsd_path::DsdFallbackKey;
use super::output_stream::{ActiveOutput, reset_output_pipeline_for_reopen};
use super::signal_path::{OutputMode, dsd_policy_for_source};
use super::state::{PLAYBACK_PAUSED, PLAYBACK_PLAYING, PLAYBACK_STARTING, PLAYBACK_STOPPED};
use super::worker_state::WorkerRuntime;
use super::worker_status::publish_output_notice;

const COREAUDIO_HIGH_RATE_STARTUP_WARMUP: Duration = Duration::from_millis(250);

pub(super) fn handle_output_reset_notice(runtime: &mut WorkerRuntime) {
    let shared = &runtime.shared;
    let output = &mut runtime.output;
    let buffers = &mut runtime.buffers;
    let active_stream = &mut output.active_stream;
    let Some(reset_notice) = active_stream.as_ref().and_then(ActiveOutput::reset_notice) else {
        return;
    };

    publish_output_notice(
        &shared.state,
        &shared.output_notice,
        reset_notice.to_string(),
    );
    reset_output_pipeline_for_reopen(
        active_stream,
        &mut buffers.dsd_state,
        &mut output.dsd_fallback_key,
        &shared.state,
        true,
    );
    output.next_stream_retry = Instant::now();
}

pub(super) fn should_attempt_dsd_output(runtime: &WorkerRuntime) -> bool {
    let shared = &runtime.shared;
    let playback = &runtime.playback;
    let output = &runtime.output;
    let config = &runtime.config;
    let playback_state = shared.state.state.load(Ordering::Relaxed);
    dsd_output_open_ready(
        output.active_stream.is_none(),
        playback.session.is_some(),
        playback_state,
        output.next_stream_retry,
        Instant::now(),
    ) && config.output_mode.is_dsd()
}

pub(super) fn should_attempt_pcm_output(runtime: &WorkerRuntime) -> bool {
    let shared = &runtime.shared;
    let playback = &runtime.playback;
    let output = &runtime.output;
    let config = &runtime.config;
    output_open_ready(
        output.active_stream.is_none(),
        playback.session.is_some(),
        shared.state.state.load(Ordering::Relaxed),
        output.next_stream_retry,
        Instant::now(),
    ) && pcm_output_allowed_for_mode(
        config.output_mode,
        playback
            .session
            .as_ref()
            .map(|session| {
                dsd_fallback_matches_pcm_retry(
                    output.dsd_fallback_key.as_ref(),
                    output.active_device_name.clone(),
                    config.output_mode,
                    config.filter_type,
                    session.dsp_path.source_rate(),
                    &config.dsd_rules,
                )
            })
            .unwrap_or(false),
    )
}

pub(super) fn promote_starting_to_playing_if_output_ready(runtime: &mut WorkerRuntime) {
    let Some(active_stream_name) = runtime
        .output
        .active_stream
        .as_ref()
        .map(ActiveOutput::debug_name)
    else {
        runtime.playback.use_transition_preroll = false;
        return;
    };
    let state = &runtime.shared.state.state;
    if state.load(Ordering::Relaxed) != PLAYBACK_STARTING {
        runtime.playback.use_transition_preroll = false;
        return;
    }
    let protective_preroll = startup_protective_preroll(runtime);
    record_startup_ring_fill(runtime);
    if !startup_output_warmup_ready(runtime) {
        return;
    }
    if !output_start_preroll_ready(
        runtime.buffers.dsd_state.as_ref(),
        &runtime.buffers.prod,
        runtime.config.target_rate,
        runtime.config.dsp_buffer_ms,
        runtime.shared.state.flush_buffer.load(Ordering::Relaxed),
        runtime.playback.use_transition_preroll,
        protective_preroll,
    ) {
        return;
    }
    if state
        .compare_exchange(
            PLAYBACK_STARTING,
            PLAYBACK_PLAYING,
            Ordering::Relaxed,
            Ordering::Relaxed,
        )
        .is_ok()
    {
        runtime.shared.state.record_startup_ready();
        runtime.playback.use_transition_preroll = false;
        if crate::audio::debug::audio_debug_enabled() {
            eprintln!(
                "AudioWorker DEBUG: playback state STARTING -> PLAYING via {}",
                active_stream_name
            );
        }
    }
}

fn startup_output_warmup_ready(runtime: &WorkerRuntime) -> bool {
    let Some(active_stream) = runtime.output.active_stream.as_ref() else {
        return false;
    };
    if !active_stream.needs_startup_warmup(runtime.config.target_rate) {
        return true;
    }
    startup_warmup_elapsed_ready(runtime.output.active_stream_opened_at, Instant::now())
}

fn startup_warmup_elapsed_ready(opened_at: Option<Instant>, now: Instant) -> bool {
    opened_at
        .map(|opened_at| now.duration_since(opened_at) >= COREAUDIO_HIGH_RATE_STARTUP_WARMUP)
        .unwrap_or(false)
}

pub(super) fn startup_protective_preroll(runtime: &WorkerRuntime) -> bool {
    if runtime.playback.use_transition_preroll {
        return false;
    }
    if runtime.config.output_mode.is_dsd() {
        let p99 = f32::from_bits(
            runtime
                .shared
                .state
                .dsd_recent_load_p99
                .load(Ordering::Relaxed),
        );
        return p99 >= 0.85
            || runtime
                .shared
                .state
                .startup_overbudget_blocks
                .load(Ordering::Relaxed)
                > 0;
    }
    false
}

pub(super) fn record_startup_ring_fill(runtime: &WorkerRuntime) {
    let pending = output_pending_len(runtime.buffers.dsd_state.as_ref(), &runtime.buffers.prod);
    let units_per_sec = if let Some(ds) = runtime.buffers.dsd_state.as_ref() {
        #[cfg(all(target_os = "windows", feature = "asio"))]
        if ds.native.is_some() {
            (ds.wire_rate.max(1) as u64).div_ceil(8)
        } else {
            ds.dop_frame_rate.max(1) as u64 * 2
        }
        #[cfg(not(all(target_os = "windows", feature = "asio")))]
        {
            ds.dop_frame_rate.max(1) as u64 * 2
        }
    } else {
        runtime.config.target_rate.max(1) as u64 * 2
    };
    runtime
        .shared
        .state
        .record_startup_ring_fill(pending as u64, units_per_sec);
}

fn output_open_ready(
    no_active_stream: bool,
    has_session: bool,
    playback_state: u32,
    next_stream_retry: Instant,
    now: Instant,
) -> bool {
    no_active_stream
        && has_session
        && playback_state != PLAYBACK_STOPPED
        && now >= next_stream_retry
}

fn dsd_output_open_ready(
    no_active_stream: bool,
    has_session: bool,
    playback_state: u32,
    next_stream_retry: Instant,
    now: Instant,
) -> bool {
    playback_state != PLAYBACK_PAUSED
        && output_open_ready(
            no_active_stream,
            has_session,
            playback_state,
            next_stream_retry,
            now,
        )
}

fn dsd_fallback_matches_pcm_retry(
    fallback_key: Option<&DsdFallbackKey>,
    active_device_name: Option<String>,
    output_mode: OutputMode,
    filter_type: FilterType,
    source_rate: u32,
    dsd_rules: &[DsdSourceRule],
) -> bool {
    let policy = dsd_policy_for_source(output_mode, filter_type, source_rate, dsd_rules);
    fallback_key
        == Some(&DsdFallbackKey::new(
            active_device_name,
            policy.mode,
            source_rate,
        ))
}

fn pcm_output_allowed_for_mode(output_mode: OutputMode, dsd_fallback_active: bool) -> bool {
    !output_mode.is_dsd() || dsd_fallback_active
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::audio::dsp::resampler::FilterType;
    use crate::audio::engine::dsd_path::DsdFallbackKey;
    use crate::audio::engine::signal_path::OutputMode;
    use crate::audio::engine::state::{
        PLAYBACK_PAUSED, PLAYBACK_PLAYING, PLAYBACK_STARTING, PLAYBACK_STOPPED,
    };
    use crate::settings::DsdSourceRule;

    use super::{
        dsd_fallback_matches_pcm_retry, dsd_output_open_ready, output_open_ready,
        pcm_output_allowed_for_mode, startup_warmup_elapsed_ready,
    };

    #[test]
    fn output_open_requires_active_session_running_state_and_elapsed_retry() {
        let now = Instant::now();

        assert!(output_open_ready(true, true, PLAYBACK_PLAYING, now, now));
        assert!(output_open_ready(
            true,
            true,
            PLAYBACK_PAUSED,
            now - Duration::from_millis(1),
            now
        ));
        assert!(output_open_ready(
            true,
            true,
            PLAYBACK_STARTING,
            now - Duration::from_millis(1),
            now
        ));
        assert!(!output_open_ready(false, true, PLAYBACK_PLAYING, now, now));
        assert!(!output_open_ready(true, false, PLAYBACK_PLAYING, now, now));
        assert!(!output_open_ready(true, true, PLAYBACK_STOPPED, now, now));
        assert!(!output_open_ready(
            true,
            true,
            PLAYBACK_PLAYING,
            now + Duration::from_millis(1),
            now
        ));
    }

    #[test]
    fn dsd_output_does_not_open_while_paused() {
        let now = Instant::now();

        assert!(!dsd_output_open_ready(
            true,
            true,
            PLAYBACK_PAUSED,
            now,
            now
        ));
        assert!(dsd_output_open_ready(
            true,
            true,
            PLAYBACK_STARTING,
            now,
            now
        ));
    }

    #[test]
    fn startup_warmup_waits_until_elapsed() {
        let now = Instant::now();

        assert!(!startup_warmup_elapsed_ready(None, now));
        assert!(!startup_warmup_elapsed_ready(
            Some(now - Duration::from_millis(249)),
            now
        ));
        assert!(startup_warmup_elapsed_ready(
            Some(now - Duration::from_millis(250)),
            now
        ));
    }

    #[test]
    fn dsd_fallback_key_must_match_device_mode_and_source_rate() {
        let key = DsdFallbackKey::new(Some("DAC".to_string()), OutputMode::Dsd128, 44_100);

        assert!(dsd_fallback_matches_pcm_retry(
            Some(&key),
            Some("DAC".to_string()),
            OutputMode::Dsd128,
            FilterType::Split128k,
            44_100,
            &[]
        ));
        assert!(!dsd_fallback_matches_pcm_retry(
            Some(&key),
            Some("Other DAC".to_string()),
            OutputMode::Dsd128,
            FilterType::Split128k,
            44_100,
            &[]
        ));
        assert!(!dsd_fallback_matches_pcm_retry(
            Some(&key),
            Some("DAC".to_string()),
            OutputMode::Dsd256,
            FilterType::Split128k,
            44_100,
            &[]
        ));
        assert!(!dsd_fallback_matches_pcm_retry(
            Some(&key),
            Some("DAC".to_string()),
            OutputMode::Dsd128,
            FilterType::Split128k,
            48_000,
            &[]
        ));
    }

    #[test]
    fn dsd_fallback_key_uses_source_rule_effective_mode() {
        let key = DsdFallbackKey::new(Some("DAC".to_string()), OutputMode::Dsd128, 96_000);
        let rules = vec![DsdSourceRule {
            source_rate: 96_000,
            output_mode: "Dsd128".to_string(),
            filter_type: "Linear".to_string(),
        }];

        assert!(dsd_fallback_matches_pcm_retry(
            Some(&key),
            Some("DAC".to_string()),
            OutputMode::Dsd256,
            FilterType::Split128k,
            96_000,
            &rules
        ));
    }

    #[test]
    fn pcm_output_requires_dsd_fallback_when_dsd_mode_is_requested() {
        assert!(pcm_output_allowed_for_mode(OutputMode::Pcm, false));
        assert!(!pcm_output_allowed_for_mode(OutputMode::Dsd128, false));
        assert!(!pcm_output_allowed_for_mode(OutputMode::Dsd256, false));
        assert!(pcm_output_allowed_for_mode(OutputMode::Dsd128, true));
        assert!(pcm_output_allowed_for_mode(OutputMode::Dsd256, true));
    }
}
