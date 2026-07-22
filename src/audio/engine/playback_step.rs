use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::buffers::{
    OutputDrainResult, flush_and_wait_for_output_at_eof, output_start_preroll_ready,
};
use super::decode::{
    DecodePumpResult, flush_dsd_staged_pcm_at_eof, flush_dsd_upsampler_tail_at_eof,
    flush_pcm_resampler_tail_at_eof, pump_active_session,
};
use super::output_stream::{ActiveOutput, reset_output_pipeline_for_reopen};
use super::session::apply_seek_to_session;
use super::signal_path::{OutputMode, OutputTransport};
use super::state::{
    PLAYBACK_PLAYING, PLAYBACK_STARTING, PLAYBACK_STOPPED, REOPEN_REASON_EOF_DRAIN_TIMEOUT,
};
use super::worker_state::WorkerRuntime;
use super::worker_status::{publish_output_notice, stop_after_eof_without_next};

pub(super) fn run_playback_step(runtime: &mut WorkerRuntime) {
    if runtime.shared.shutdown_requested() {
        return;
    }
    let protective_preroll = runtime.shared.state.state.load(Ordering::Relaxed)
        == PLAYBACK_STARTING
        && super::output_stage::startup_protective_preroll(runtime);
    let shared = &runtime.shared;
    let playback = &mut runtime.playback;
    let output = &mut runtime.output;
    let buffers = &mut runtime.buffers;
    let config = &mut runtime.config;
    let session = &mut playback.session;
    let dsd_state = &mut buffers.dsd_state;
    let prod = &mut buffers.prod;
    let eq_processor = &mut config.eq_processor;
    let target_rate = config.target_rate;
    let active_stream = &mut output.active_stream;
    let dsd_fallback_key = &mut output.dsd_fallback_key;
    let next_stream_retry = &mut output.next_stream_retry;
    let session_epoch = playback.session_epoch;
    let queues = &mut playback.queues;
    let repeat_one = playback.repeat_one;
    let current_file_path = &mut playback.current_file_path;
    let current_fallback_tags = &mut playback.current_fallback_tags;
    let pending_start = &mut playback.pending_start;

    let playback_state = shared.state.state.load(Ordering::Relaxed);
    let is_output_active =
        playback_state == PLAYBACK_PLAYING || playback_state == PLAYBACK_STARTING;
    if !is_output_active {
        thread::sleep(Duration::from_millis(20));
        return;
    }
    let can_prime_before_stream_open = can_prime_without_output_consumer(
        config.output_mode,
        playback_state,
        active_stream.is_none(),
    );
    if active_stream.is_none() && !can_prime_before_stream_open {
        thread::sleep(Duration::from_millis(5));
        return;
    }
    if shared.state.flush_buffer.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(1));
        return;
    }
    let startup_preroll_ready = playback_state == PLAYBACK_STARTING
        && active_stream.is_some()
        && output_start_preroll_ready(
            dsd_state.as_ref(),
            prod,
            target_rate,
            config.dsp_buffer_ms,
            false,
            playback.use_transition_preroll,
            protective_preroll,
        );
    if should_pause_render_while_output_warms(
        playback_state,
        active_stream.is_some(),
        startup_preroll_ready,
    ) {
        thread::sleep(Duration::from_millis(1));
        return;
    }

    let Some(sess) = session.as_mut() else {
        shared
            .state
            .state
            .store(PLAYBACK_STOPPED, Ordering::Relaxed);
        return;
    };

    if let Some(secs) = sess.seek_request.take() {
        apply_seek_to_session(
            sess,
            secs,
            &shared.state,
            dsd_state.as_mut(),
            eq_processor,
            target_rate,
        );
    }

    let should_continue = || {
        let playback_state = shared.state.state.load(Ordering::Relaxed);
        !shared.shutdown_requested()
            && (playback_state == PLAYBACK_PLAYING || playback_state == PLAYBACK_STARTING)
            && active_stream
                .as_ref()
                .and_then(ActiveOutput::reset_notice)
                .is_none()
    };

    match pump_active_session(
        sess,
        dsd_state.as_mut(),
        prod,
        &shared.state,
        eq_processor,
        target_rate,
        should_continue,
    ) {
        DecodePumpResult::Progress => {}
        DecodePumpResult::Backpressured => {
            thread::sleep(Duration::from_millis(5));
        }
        DecodePumpResult::EndOfStream => {
            println!("AudioWorker: EOF reached.");
            let can_advance = session_epoch == shared.playback_epoch.load(Ordering::Relaxed);
            let next_start = queues.eof_next_start(
                can_advance,
                repeat_one,
                current_file_path.clone(),
                shared.track_cover.lock().unwrap().clone(),
                current_fallback_tags.clone(),
                session_epoch,
            );

            if let Some(next_start) = next_start {
                playback.pending_start_gapless =
                    can_advance && active_stream.is_some() && !repeat_one;
                playback.gapless_dsp_path = if playback.pending_start_gapless {
                    session
                        .take()
                        .map(|previous_session| previous_session.dsp_path)
                } else {
                    None
                };
                if !playback.pending_start_gapless {
                    if let Some(ds) = dsd_state.as_mut() {
                        flush_dsd_staged_pcm_at_eof(ds, &shared.state, eq_processor, || {
                            !shared.shutdown_requested()
                                && shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING
                                && shared.playback_epoch.load(Ordering::Relaxed) == session_epoch
                                && eof_output_can_drain(active_stream.as_ref())
                        });
                        flush_dsd_upsampler_tail_at_eof(ds, &shared.state, || {
                            !shared.shutdown_requested()
                                && shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING
                                && shared.playback_epoch.load(Ordering::Relaxed) == session_epoch
                                && eof_output_can_drain(active_stream.as_ref())
                        });
                    } else if let Some(sess) = session.as_mut() {
                        flush_pcm_resampler_tail_at_eof(
                            sess,
                            prod,
                            &shared.state,
                            eq_processor,
                            target_rate,
                            || {
                                !shared.shutdown_requested()
                                    && shared.state.state.load(Ordering::Relaxed)
                                        == PLAYBACK_PLAYING
                                    && shared.playback_epoch.load(Ordering::Relaxed)
                                        == session_epoch
                                    && eof_output_can_drain(active_stream.as_ref())
                            },
                        );
                    }
                    let drain_result =
                        flush_and_wait_for_output_at_eof(dsd_state, prod, &shared.state, || {
                            !shared.shutdown_requested()
                                && shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING
                                && shared.playback_epoch.load(Ordering::Relaxed) == session_epoch
                                && eof_output_can_drain(active_stream.as_ref())
                        });
                    recover_from_eof_drain_timeout(
                        drain_result,
                        active_stream,
                        dsd_state,
                        dsd_fallback_key,
                        next_stream_retry,
                        &shared.state,
                        &shared.output_notice,
                    );
                    *session = None;
                }
                *pending_start = Some(next_start);
            } else {
                if let Some(ds) = dsd_state.as_mut() {
                    flush_dsd_staged_pcm_at_eof(ds, &shared.state, eq_processor, || {
                        !shared.shutdown_requested()
                            && shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING
                            && shared.playback_epoch.load(Ordering::Relaxed) == session_epoch
                            && eof_output_can_drain(active_stream.as_ref())
                    });
                    flush_dsd_upsampler_tail_at_eof(ds, &shared.state, || {
                        !shared.shutdown_requested()
                            && shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING
                            && shared.playback_epoch.load(Ordering::Relaxed) == session_epoch
                            && eof_output_can_drain(active_stream.as_ref())
                    });
                } else if let Some(sess) = session.as_mut() {
                    flush_pcm_resampler_tail_at_eof(
                        sess,
                        prod,
                        &shared.state,
                        eq_processor,
                        target_rate,
                        || {
                            !shared.shutdown_requested()
                                && shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING
                                && shared.playback_epoch.load(Ordering::Relaxed) == session_epoch
                                && eof_output_can_drain(active_stream.as_ref())
                        },
                    );
                }
                let drain_result =
                    flush_and_wait_for_output_at_eof(dsd_state, prod, &shared.state, || {
                        !shared.shutdown_requested()
                            && shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING
                            && shared.playback_epoch.load(Ordering::Relaxed) == session_epoch
                            && eof_output_can_drain(active_stream.as_ref())
                    });
                recover_from_eof_drain_timeout(
                    drain_result,
                    active_stream,
                    dsd_state,
                    dsd_fallback_key,
                    next_stream_retry,
                    &shared.state,
                    &shared.output_notice,
                );
                *session = None;
                playback.pending_start_gapless = false;
                playback.gapless_dsp_path = None;
                queues.clear_stream_auto_advance_pending();
                *current_file_path = None;
                *current_fallback_tags = None;
                stop_after_eof_without_next(
                    &shared.file_name,
                    &shared.track_tags,
                    &shared.track_cover,
                    &shared.cover_version,
                    &shared.state,
                );
            }
        }
        DecodePumpResult::FatalError => {
            shared
                .state
                .state
                .store(PLAYBACK_STOPPED, Ordering::Relaxed);
        }
    }
}

fn can_prime_without_output_consumer(
    output_mode: OutputMode,
    playback_state: u32,
    no_active_stream: bool,
) -> bool {
    output_mode.is_dsd() && playback_state == PLAYBACK_STARTING && no_active_stream
}

fn should_pause_render_while_output_warms(
    playback_state: u32,
    has_active_stream: bool,
    startup_preroll_ready: bool,
) -> bool {
    playback_state == PLAYBACK_STARTING && has_active_stream && startup_preroll_ready
}

fn eof_output_can_drain(active_stream: Option<&ActiveOutput>) -> bool {
    active_stream.is_some_and(|stream| stream.reset_notice().is_none())
}

#[allow(clippy::too_many_arguments)]
fn recover_from_eof_drain_timeout(
    result: OutputDrainResult,
    active_stream: &mut Option<ActiveOutput>,
    dsd_state: &mut Option<super::buffers::DsdWorkerState>,
    dsd_fallback_key: &mut Option<super::dsd_path::DsdFallbackKey>,
    next_stream_retry: &mut Instant,
    state: &super::state::AtomicPlayerState,
    output_notice: &std::sync::Mutex<Option<String>>,
) {
    if result != OutputDrainResult::TimedOut {
        return;
    }

    eprintln!("AudioWorker: Releasing stalled output after EOF drain timeout.");
    state.record_reopen_reason(REOPEN_REASON_EOF_DRAIN_TIMEOUT);
    reset_output_pipeline_for_reopen(active_stream, dsd_state, dsd_fallback_key, state, true);
    state
        .output_transport
        .store(OutputTransport::None.as_id(), Ordering::Relaxed);
    *next_stream_retry = Instant::now();
    publish_output_notice(
        state,
        output_notice,
        "Audio output stopped consuming at the track boundary; the device was released for automatic recovery."
            .to_string(),
    );
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        can_prime_without_output_consumer, eof_output_can_drain, recover_from_eof_drain_timeout,
        should_pause_render_while_output_warms,
    };
    use crate::audio::engine::buffers::{
        OutputDrainResult, new_audio_ring, output_start_preroll_ready,
    };
    use crate::audio::engine::signal_path::OutputMode;
    use crate::audio::engine::state::{PLAYBACK_PLAYING, PLAYBACK_STARTING};

    #[test]
    fn pcm_never_primes_without_an_output_consumer() {
        assert!(!can_prime_without_output_consumer(
            OutputMode::Pcm,
            PLAYBACK_STARTING,
            true,
        ));
    }

    #[test]
    fn eof_drain_stops_when_startup_has_no_output_consumer() {
        assert!(!eof_output_can_drain(None));
    }

    #[test]
    fn completed_eof_drain_does_not_mutate_output_recovery_state() {
        let state = crate::audio::engine::state::AtomicPlayerState::new();
        let output_notice = std::sync::Mutex::new(None);
        let mut active_stream = None;
        let mut dsd_state = None;
        let mut fallback = None;
        let original_retry = Instant::now() + Duration::from_secs(30);
        let mut next_retry = original_retry;

        recover_from_eof_drain_timeout(
            OutputDrainResult::Drained,
            &mut active_stream,
            &mut dsd_state,
            &mut fallback,
            &mut next_retry,
            &state,
            &output_notice,
        );

        assert_eq!(next_retry, original_retry);
        assert!(output_notice.lock().unwrap().is_none());
    }

    #[test]
    fn timed_out_eof_drain_marks_output_for_clean_recovery() {
        let state = crate::audio::engine::state::AtomicPlayerState::new();
        state.output_transport.store(
            crate::audio::engine::signal_path::OutputTransport::DopCoreAudio.as_id(),
            std::sync::atomic::Ordering::Relaxed,
        );
        let output_notice = std::sync::Mutex::new(None);
        let mut active_stream = None;
        let mut dsd_state = None;
        let mut fallback = Some(crate::audio::engine::dsd_path::DsdFallbackKey::new(
            Some("Hegel H390 USB".to_string()),
            OutputMode::Dsd128,
            44_100,
        ));
        let mut next_retry = Instant::now() + Duration::from_secs(30);

        recover_from_eof_drain_timeout(
            OutputDrainResult::TimedOut,
            &mut active_stream,
            &mut dsd_state,
            &mut fallback,
            &mut next_retry,
            &state,
            &output_notice,
        );

        assert!(fallback.is_none());
        assert_eq!(
            state
                .output_transport
                .load(std::sync::atomic::Ordering::Relaxed),
            crate::audio::engine::signal_path::OutputTransport::None.as_id()
        );
        assert_eq!(
            state
                .last_reopen_reason_id
                .load(std::sync::atomic::Ordering::Relaxed),
            crate::audio::engine::state::REOPEN_REASON_EOF_DRAIN_TIMEOUT
        );
        assert!(output_notice.lock().unwrap().is_some());
    }

    #[test]
    fn dsd_keeps_preopen_priming_while_starting() {
        for mode in [OutputMode::Dsd64, OutputMode::Dsd128, OutputMode::Dsd256] {
            assert!(can_prime_without_output_consumer(
                mode,
                PLAYBACK_STARTING,
                true,
            ));
            assert!(!can_prime_without_output_consumer(
                mode,
                PLAYBACK_PLAYING,
                true,
            ));
            assert!(!can_prime_without_output_consumer(
                mode,
                PLAYBACK_STARTING,
                false,
            ));
        }
    }

    #[test]
    fn startup_stops_rendering_once_preroll_is_ready_for_an_open_stream() {
        assert!(should_pause_render_while_output_warms(
            PLAYBACK_STARTING,
            true,
            true,
        ));
        assert!(!should_pause_render_while_output_warms(
            PLAYBACK_STARTING,
            true,
            false,
        ));
        assert!(!should_pause_render_while_output_warms(
            PLAYBACK_STARTING,
            false,
            true,
        ));
        assert!(!should_pause_render_while_output_warms(
            PLAYBACK_PLAYING,
            true,
            true,
        ));
    }

    #[test]
    fn high_rate_pcm_transition_waits_with_room_left_in_the_ring() {
        let target_rate = 352_800;
        let (mut prod, _cons) = new_audio_ring(target_rate, 1_000);
        let capacity = prod.len() + prod.free_len();
        let transition_samples = target_rate as usize * 2 * 10 / 1_000;

        assert_eq!(
            prod.push_slice(&vec![0.0; transition_samples]),
            transition_samples
        );
        let preroll_ready =
            output_start_preroll_ready(None, &prod, target_rate, 1_000, false, true, false);

        assert!(should_pause_render_while_output_warms(
            PLAYBACK_STARTING,
            true,
            preroll_ready,
        ));
        assert!(prod.free_len() > capacity / 2);
    }
}
