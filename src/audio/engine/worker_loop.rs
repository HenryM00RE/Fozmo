use std::thread;

use super::command_dispatch::handle_worker_command;
use super::commands::PlayerCommand;
use super::dsd_output::{DsdOutputOpenResult, ensure_dsd_output_stream};
use super::output_stage::{
    handle_output_reset_notice, promote_starting_to_playing_if_output_ready,
    should_attempt_dsd_output, should_attempt_pcm_output,
};
use super::pcm_output::{PcmOutputOpenResult, ensure_pcm_output_stream};
use super::playback_step::run_playback_step;
use super::session_start::{PendingStartResult, install_pending_start};
use super::worker_state::{WorkerRuntime, WorkerShared};

#[cfg(target_os = "windows")]
use crate::audio::output::wasapi_exclusive;

pub(super) fn spawn_audio_worker(
    shared: WorkerShared,
    command_rx: tokio::sync::mpsc::UnboundedReceiver<PlayerCommand>,
) -> thread::JoinHandle<()> {
    // Spawn a native OS thread because DSP and device FFI require low jitter.
    thread::Builder::new()
        .name("AudioWorker".to_string())
        .spawn(move || {
            run_worker(shared, command_rx);
        })
        .expect("Failed to spawn audio worker thread")
}

fn run_worker(
    shared: WorkerShared,
    mut command_rx: tokio::sync::mpsc::UnboundedReceiver<PlayerCommand>,
) {
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: worker thread online os={} arch={}",
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
    }
    #[cfg(target_os = "macos")]
    promote_current_thread_to_audio_qos();
    #[cfg(target_os = "windows")]
    let _mmcss_guard = wasapi_exclusive::boost_current_thread_for_audio("AudioWorker");

    let mut runtime = WorkerRuntime::new(shared);

    loop {
        if runtime.shared.shutdown_requested() {
            return;
        }
        handle_output_reset_notice(&mut runtime);

        loop {
            if runtime.shared.shutdown_requested() {
                return;
            }
            match command_rx.try_recv() {
                Ok(cmd) => handle_worker_command(cmd, &mut runtime),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
            }
        }

        if runtime.shared.shutdown_requested() {
            return;
        }
        if let PendingStartResult::StaleEpoch = install_pending_start(&mut runtime) {
            continue;
        }

        if runtime.shared.shutdown_requested() {
            return;
        }
        if should_attempt_dsd_output(&runtime)
            && let DsdOutputOpenResult::RetryLater = ensure_dsd_output_stream(&mut runtime)
        {
            continue;
        }

        if runtime.shared.shutdown_requested() {
            return;
        }
        if should_attempt_pcm_output(&runtime)
            && let PcmOutputOpenResult::RetryLater = ensure_pcm_output_stream(&mut runtime)
        {
            continue;
        }

        if runtime.shared.shutdown_requested() {
            return;
        }
        promote_starting_to_playing_if_output_ready(&mut runtime);

        run_playback_step(&mut runtime);
    }
}

#[cfg(target_os = "macos")]
fn promote_current_thread_to_audio_qos() {
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}
