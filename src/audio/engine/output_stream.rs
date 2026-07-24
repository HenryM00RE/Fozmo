use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::buffers::DsdWorkerState;
use super::dsd_path::DsdFallbackKey;
use crate::audio::engine::state::{
    AtomicPlayerState, COREAUDIO_DOP_LIFECYCLE_DROP, COREAUDIO_DOP_LIFECYCLE_QUIESCE,
    COREAUDIO_DOP_LIFECYCLE_STOP, FLUSH_REASON_REOPEN,
};
#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::output::asio_output;
#[cfg(target_os = "macos")]
use crate::audio::output::coreaudio_hog::set_hog_mode;
#[cfg(target_os = "windows")]
use crate::audio::output::wasapi_exclusive;
use crate::audio::sinks::airplay;

use cpal::traits::StreamTrait;

#[cfg(all(target_os = "windows", feature = "asio"))]
const ASIO_REOPEN_SETTLE_DELAY: Duration = Duration::from_millis(750);

/// Output stream handle. The variant determines which OS audio backend is driving playback.
/// The inner values are kept alive solely so their `Drop` impls run when the output is replaced.
// Some platform-specific variants are only constructed on matching feature/OS combinations.
#[allow(dead_code)]
pub(super) enum ActiveOutput {
    Cpal(CpalOutput),
    #[cfg(target_os = "windows")]
    WasapiExclusive(wasapi_exclusive::ExclusiveStream),
    #[cfg(target_os = "windows")]
    WasapiExclusiveDop(wasapi_exclusive::ExclusiveStream),
    #[cfg(target_os = "macos")]
    CoreAudioPcm(CoreAudioPcmOutput),
    #[cfg(target_os = "macos")]
    CoreAudioDop(CoreAudioDopOutput),
    #[cfg(all(target_os = "windows", feature = "asio"))]
    AsioPcm(asio_output::AsioStream),
    #[cfg(all(target_os = "windows", feature = "asio"))]
    AsioNativeDsd(asio_output::AsioStream),
    AirPlayRaop(airplay::sender::AirPlayStream),
    AirPlay2(airplay::sender::AirPlayStream),
}

impl ActiveOutput {
    pub(super) fn debug_name(&self) -> &'static str {
        match self {
            Self::Cpal(_) => "cpal",
            #[cfg(target_os = "windows")]
            Self::WasapiExclusive(_) => "wasapi-exclusive-pcm",
            #[cfg(target_os = "windows")]
            Self::WasapiExclusiveDop(_) => "wasapi-exclusive-dop",
            #[cfg(target_os = "macos")]
            Self::CoreAudioPcm(_) => "coreaudio-pcm",
            #[cfg(target_os = "macos")]
            Self::CoreAudioDop(_) => "coreaudio-dop",
            #[cfg(all(target_os = "windows", feature = "asio"))]
            Self::AsioPcm(_) => "asio-pcm",
            #[cfg(all(target_os = "windows", feature = "asio"))]
            Self::AsioNativeDsd(_) => "asio-native-dsd",
            Self::AirPlayRaop(_) => "airplay-raop",
            Self::AirPlay2(_) => "airplay2",
        }
    }

    pub(super) fn quiesce_before_reopen(&self, state: &AtomicPlayerState) {
        match self {
            Self::Cpal(output) => {
                state.request_flush(FLUSH_REASON_REOPEN);
                thread::sleep(Duration::from_millis(50));
                if let Err(err) = output.stream.pause() {
                    eprintln!("AudioWorker: Failed to pause CPAL stream before reopen: {err:?}");
                }
            }
            #[cfg(target_os = "macos")]
            Self::CoreAudioPcm(_) => {
                state.request_flush(FLUSH_REASON_REOPEN);
                thread::sleep(Duration::from_millis(50));
            }
            #[cfg(target_os = "macos")]
            Self::CoreAudioDop(_) => {
                state.request_flush(FLUSH_REASON_REOPEN);
                state.record_coreaudio_dop_lifecycle(COREAUDIO_DOP_LIFECYCLE_QUIESCE);
                thread::sleep(Duration::from_millis(50));
            }
            _ => {}
        }
    }

    pub(super) fn reset_notice(&self) -> Option<&'static str> {
        match self {
            #[cfg(all(target_os = "windows", feature = "asio"))]
            Self::AsioPcm(stream) | Self::AsioNativeDsd(stream) => stream
                .reset_requested()
                .then_some("ASIO driver requested a stream reset; reopening output."),
            Self::AirPlayRaop(stream) => stream
                .reset_requested()
                .then_some("AirPlay connection ended; reopening output."),
            Self::AirPlay2(stream) => stream
                .reset_requested()
                .then_some("AirPlay 2 connection ended; reopening output."),
            _ => None,
        }
    }

    pub(super) fn should_reopen_on_interrupted_track_change(&self) -> bool {
        match self {
            // Keep CoreAudio DoP alive across seek/skip when the carrier stays
            // compatible; stopping and reopening the AudioUnit can pop some DACs.
            Self::AirPlayRaop(_) => true,
            Self::AirPlay2(_) => true,
            _ => false,
        }
    }

    pub(super) fn supports_continuous_dsd_renderer_swap(&self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Self::CoreAudioDop(_) => true,
            #[cfg(target_os = "windows")]
            Self::WasapiExclusiveDop(_) => true,
            _ => false,
        }
    }

    pub(super) fn needs_startup_warmup(&self, target_rate: u32) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Self::CoreAudioPcm(_) => target_rate >= 176_400,
            #[cfg(target_os = "macos")]
            Self::CoreAudioDop(_) => true,
            _ => false,
        }
    }

    fn settle_after_drop_delay(&self) -> Duration {
        match self {
            #[cfg(all(target_os = "windows", feature = "asio"))]
            Self::AsioPcm(_) | Self::AsioNativeDsd(_) => ASIO_REOPEN_SETTLE_DELAY,
            _ => Duration::from_millis(0),
        }
    }
}

pub(super) fn drop_active_stream_for_reopen(
    active_stream: &mut Option<ActiveOutput>,
    state: &AtomicPlayerState,
) {
    let Some(stream) = active_stream.take() else {
        return;
    };
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: dropping active output stream for reopen: {}",
            stream.debug_name()
        );
    }
    stream.quiesce_before_reopen(state);
    let settle_delay = stream.settle_after_drop_delay();
    drop(stream);
    if settle_delay > Duration::from_millis(0) {
        thread::sleep(settle_delay);
    }
}

pub(super) fn reset_output_pipeline_for_reopen(
    active_stream: &mut Option<ActiveOutput>,
    dsd_state: &mut Option<DsdWorkerState>,
    dsd_fallback_key: &mut Option<DsdFallbackKey>,
    state: &AtomicPlayerState,
    flush_output: bool,
) {
    drop_active_stream_for_reopen(active_stream, state);
    *dsd_state = None;
    *dsd_fallback_key = None;
    if flush_output {
        state.request_flush(FLUSH_REASON_REOPEN);
    }
}

pub(super) struct CpalOutput {
    pub(super) stream: cpal::Stream,
    #[cfg(target_os = "macos")]
    pub(super) hogged_device: Option<coreaudio_sys::AudioDeviceID>,
}

#[cfg(target_os = "macos")]
pub(super) struct CoreAudioPcmOutput {
    pub(super) audio_unit: coreaudio::audio_unit::AudioUnit,
    pub(super) hogged_device: Option<coreaudio_sys::AudioDeviceID>,
}

#[cfg(target_os = "macos")]
pub(super) struct CoreAudioDopOutput {
    pub(super) audio_unit: coreaudio::audio_unit::AudioUnit,
    pub(super) hogged_device: Option<coreaudio_sys::AudioDeviceID>,
    pub(super) state: Arc<AtomicPlayerState>,
}

#[cfg(target_os = "macos")]
impl Drop for CpalOutput {
    fn drop(&mut self) {
        if let Some(dev_id) = self.hogged_device.take() {
            unsafe {
                let _ = set_hog_mode(dev_id, false);
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for CoreAudioPcmOutput {
    fn drop(&mut self) {
        let _ = self.audio_unit.stop();
        if let Some(dev_id) = self.hogged_device.take() {
            unsafe {
                let _ = set_hog_mode(dev_id, false);
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for CoreAudioDopOutput {
    fn drop(&mut self) {
        self.state
            .record_coreaudio_dop_lifecycle(COREAUDIO_DOP_LIFECYCLE_STOP);
        let _ = self.audio_unit.stop();
        self.state
            .record_coreaudio_dop_lifecycle(COREAUDIO_DOP_LIFECYCLE_DROP);
        if let Some(dev_id) = self.hogged_device.take() {
            unsafe {
                let _ = set_hog_mode(dev_id, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::audio::engine::signal_path::OutputMode;

    #[test]
    fn reset_pipeline_without_stream_clears_fallback_state_and_flushes() {
        let state = AtomicPlayerState::new();
        let mut active_stream = None;
        let mut dsd_state = None;
        let mut fallback_key = Some(DsdFallbackKey::new(
            Some("DAC".to_string()),
            OutputMode::Dsd128,
            44_100,
        ));

        reset_output_pipeline_for_reopen(
            &mut active_stream,
            &mut dsd_state,
            &mut fallback_key,
            &state,
            true,
        );

        assert!(active_stream.is_none());
        assert!(dsd_state.is_none());
        assert!(fallback_key.is_none());
        assert!(state.flush_buffer.load(Ordering::Relaxed));
    }

    #[test]
    fn reset_pipeline_can_skip_output_flush() {
        let state = AtomicPlayerState::new();
        let mut active_stream = None;
        let mut dsd_state = None;
        let mut fallback_key = Some(DsdFallbackKey::new(None, OutputMode::Dsd256, 48_000));

        reset_output_pipeline_for_reopen(
            &mut active_stream,
            &mut dsd_state,
            &mut fallback_key,
            &state,
            false,
        );

        assert!(fallback_key.is_none());
        assert!(!state.flush_buffer.load(Ordering::Relaxed));
    }
}
