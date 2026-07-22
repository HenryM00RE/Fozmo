#[cfg(target_os = "macos")]
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(target_os = "macos")]
use std::thread;
use std::time::Instant;
#[cfg(target_os = "macos")]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::output::asio_output;
#[cfg(target_os = "macos")]
use crate::audio::output::coreaudio_hog::{
    find_device_id_by_name, get_default_output_device, set_hog_mode,
};
#[cfg(target_os = "windows")]
use crate::audio::output::wasapi_exclusive;
use crate::audio::sinks::airplay::{self, sender::AirPlayMetadata};

use super::buffers::AudioConsumer;
#[cfg(target_os = "macos")]
use super::buffers::DopConsumer;
use super::output_ramp::PcmTransitionRamp;
use super::output_stream::{ActiveOutput, CpalOutput};
#[cfg(target_os = "macos")]
use super::output_stream::{CoreAudioDopOutput, CoreAudioPcmOutput};
use super::signal_path::OutputTransport;
#[cfg(target_os = "macos")]
use super::state::PLAYBACK_STARTING;
use super::state::{AtomicPlayerState, PLAYBACK_PLAYING};
#[cfg(target_os = "macos")]
use super::state::{
    COREAUDIO_DOP_LIFECYCLE_OPEN_ATTEMPT, COREAUDIO_DOP_LIFECYCLE_START,
    DOP_OUTPUT_TRANSITION_IDLE_TO_MIXED, DOP_OUTPUT_TRANSITION_IDLE_TO_PROGRAM,
    DOP_OUTPUT_TRANSITION_MIXED_TO_IDLE, DOP_OUTPUT_TRANSITION_MIXED_TO_PROGRAM,
    DOP_OUTPUT_TRANSITION_PROGRAM_TO_IDLE, DOP_OUTPUT_TRANSITION_PROGRAM_TO_MIXED,
};

#[cfg(target_os = "macos")]
const COREAUDIO_DOP_HIGH_RATE_THRESHOLD_HZ: u32 = 700_000;
#[cfg(target_os = "macos")]
const COREAUDIO_DOP_DEFAULT_BUFFER_FRAMES: u32 = 4096;
#[cfg(target_os = "macos")]
const COREAUDIO_DOP_HIGH_RATE_BUFFER_FRAMES: u32 = 16_384;
#[cfg(target_os = "macos")]
const COREAUDIO_DOP_DIAGNOSTIC_SCAN_INTERVAL_CALLBACKS: u32 = 64;
#[cfg(target_os = "macos")]
const COREAUDIO_MAX_CALLBACK_SAMPLES: usize = 32_768;
#[cfg(target_os = "macos")]
const COREAUDIO_OPEN_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(0),
    Duration::from_millis(75),
    Duration::from_millis(150),
    Duration::from_millis(300),
];

#[cfg(target_os = "macos")]
#[derive(Default)]
struct CoreAudioDopRingPressureState {
    below_250ms: bool,
    below_100ms: bool,
    below_50ms: bool,
    below_callback: bool,
}

/// Callback-owned storage that is accessed by the control thread only before
/// the AudioUnit starts. Once started, CoreAudio is the sole accessor. This
/// removes the mutex and lock-miss underrun path from the real-time callback.
#[cfg(target_os = "macos")]
struct CoreAudioCallbackOwned<T> {
    value: UnsafeCell<Option<T>>,
}

#[cfg(target_os = "macos")]
unsafe impl<T: Send> Sync for CoreAudioCallbackOwned<T> {}

#[cfg(target_os = "macos")]
impl<T> CoreAudioCallbackOwned<T> {
    fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(Some(value)),
        }
    }

    /// Safety: called only by the registered callback after registration; the
    /// control thread never accesses the value after the AudioUnit starts.
    unsafe fn callback_ptr(&self) -> *mut T {
        unsafe {
            match (&mut *self.value.get()).as_mut() {
                Some(value) => value as *mut T,
                None => std::ptr::null_mut(),
            }
        }
    }

    /// Called only on registration/start failure, before callbacks can run.
    fn take_before_start(&self) -> Option<T> {
        unsafe { (&mut *self.value.get()).take() }
    }
}

// Output opening bridges device selection, stream consumers, and AirPlay state in one boundary call.
#[allow(clippy::too_many_arguments)]
pub(super) fn open_audio_stream(
    device_name_opt: &Option<String>,
    target_rate: u32,
    exclusive: bool,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
    airplay_metadata: AirPlayMetadata,
    airplay_device_volume: Arc<AtomicU32>,
    airplay_volume: Option<f32>,
) -> Result<(ActiveOutput, u32), Box<dyn std::error::Error>> {
    if let Some(device_name) = device_name_opt
        .as_deref()
        .filter(|name| airplay::is_airplay_device_name(name))
    {
        let target = airplay::parse_trusted_target_device_name(device_name)
            .ok_or("AirPlay receiver is not trusted or no longer available")?;
        let airplay2 = target.prefers_airplay2_transport();
        let stream = airplay::sender::open(
            target,
            cons,
            Arc::clone(&state),
            airplay_metadata,
            airplay_device_volume,
            airplay_volume,
        )?;
        state.exclusive.store(false, Ordering::Relaxed);
        state.store_target_rate_for_output(airplay::AIRPLAY_SAMPLE_RATE);
        state
            .target_bits
            .store(airplay::AIRPLAY_BIT_DEPTH as u32, Ordering::Relaxed);
        let transport = if airplay2 {
            OutputTransport::PcmAirPlay2
        } else {
            OutputTransport::PcmAirPlayRaop
        };
        state
            .output_transport
            .store(transport.as_id(), Ordering::Relaxed);
        return Ok((
            if airplay2 {
                ActiveOutput::AirPlay2(stream)
            } else {
                ActiveOutput::AirPlayRaop(stream)
            },
            airplay::AIRPLAY_SAMPLE_RATE,
        ));
    }

    #[cfg(all(target_os = "windows", feature = "asio"))]
    if let Some(driver_name) = device_name_opt
        .as_deref()
        .and_then(|name| name.strip_prefix("ASIO: "))
    {
        return match asio_output::open_pcm(driver_name, target_rate, cons, Arc::clone(&state)) {
            Ok((stream, actual_rate)) => {
                state.exclusive.store(true, Ordering::Relaxed);
                state.store_target_rate_for_output(actual_rate);
                state
                    .output_transport
                    .store(OutputTransport::PcmAsio.as_id(), Ordering::Relaxed);
                Ok((ActiveOutput::AsioPcm(stream), actual_rate))
            }
            Err((e, _returned_cons)) => Err(e),
        };
    }

    #[cfg(all(target_os = "windows", not(feature = "asio")))]
    if device_name_opt
        .as_deref()
        .map(|name| name.starts_with("ASIO: "))
        .unwrap_or(false)
    {
        return Err("ASIO device selected but binary was built without the `asio` feature".into());
    }

    #[cfg(target_os = "macos")]
    if device_name_opt
        .as_deref()
        .and_then(find_device_id_by_name)
        .is_some()
    {
        return match open_coreaudio_pcm_stream(
            device_name_opt,
            target_rate,
            exclusive,
            cons,
            Arc::clone(&state),
        ) {
            Ok((stream, actual_rate)) => Ok((ActiveOutput::CoreAudioPcm(stream), actual_rate)),
            Err((e, _returned_cons)) => Err(e),
        };
    }

    #[cfg(target_os = "windows")]
    let cons = {
        let is_asio_target = device_name_opt
            .as_deref()
            .map(|n| n.starts_with("ASIO: "))
            .unwrap_or(false);

        if exclusive && !is_asio_target {
            match wasapi_exclusive::open(
                device_name_opt.as_deref(),
                target_rate,
                cons,
                Arc::clone(&state),
            ) {
                Ok((stream, actual_rate)) => {
                    state.exclusive.store(true, Ordering::Relaxed);
                    state.store_target_rate_for_output(actual_rate);
                    state.output_transport.store(
                        OutputTransport::PcmWasapiExclusive.as_id(),
                        Ordering::Relaxed,
                    );
                    println!(
                        "AudioWorker: WASAPI exclusive stream opened at {}Hz",
                        actual_rate
                    );
                    return Ok((ActiveOutput::WasapiExclusive(stream), actual_rate));
                }
                Err((e, returned_cons)) => {
                    eprintln!(
                        "AudioWorker: WASAPI exclusive open failed ({:?}); falling back to shared mode",
                        e
                    );
                    state.exclusive.store(false, Ordering::Relaxed);
                    returned_cons
                }
            }
        } else {
            cons
        }
    };

    let host = cpal::default_host();
    let lookup_name = device_name_opt.as_deref();
    let device = match lookup_name {
        Some(name) => {
            let mut found_device = None;
            let requested_name = name.trim();
            for d in host.output_devices()? {
                if let Ok(n) = d.name()
                    && n.trim() == requested_name
                {
                    found_device = Some(d);
                    break;
                }
            }
            match found_device {
                Some(device) => device,
                None => {
                    eprintln!(
                        "AudioWorker: Selected audio device '{}' not found; falling back to system default output.",
                        name
                    );
                    host.default_output_device().ok_or(format!(
                        "Selected audio device '{}' not found, and no default output audio device found",
                        name
                    ))?
                }
            }
        }
        None => host
            .default_output_device()
            .ok_or("No default output audio device found")?,
    };

    let resolved_name = device
        .name()
        .unwrap_or_else(|_| "Unknown Device".to_string());
    println!("AudioWorker: Opening device: {}", resolved_name);

    #[cfg(target_os = "macos")]
    let hogged_dev_id: Option<coreaudio_sys::AudioDeviceID> = if exclusive {
        let dev_id = find_device_id_by_name(&resolved_name).or_else(|| {
            if device_name_opt.is_none() {
                unsafe { get_default_output_device() }
            } else {
                None
            }
        });

        if let Some(dev_id) = dev_id {
            unsafe {
                match set_hog_mode(dev_id, true) {
                    Ok(_) => {
                        state.exclusive.store(true, Ordering::Relaxed);
                        println!("AudioWorker: CoreAudio exclusive Hog Mode enabled.");
                        Some(dev_id)
                    }
                    Err(status) => {
                        eprintln!(
                            "AudioWorker: Failed to enable CoreAudio Hog Mode. OSStatus: {}",
                            status
                        );
                        state.exclusive.store(false, Ordering::Relaxed);
                        None
                    }
                }
            }
        } else {
            eprintln!("AudioWorker: Could not map device name to CoreAudio AudioDeviceID.");
            state.exclusive.store(false, Ordering::Relaxed);
            None
        }
    } else {
        state.exclusive.store(false, Ordering::Relaxed);
        None
    };
    #[cfg(not(target_os = "macos"))]
    {
        let _ = exclusive;
        state.exclusive.store(false, Ordering::Relaxed);
    }

    let requested_default_buffer = cpal::StreamConfig {
        channels: 2,
        sample_rate: cpal::SampleRate(target_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let requested_fixed_buffer = cpal::StreamConfig {
        channels: 2,
        sample_rate: cpal::SampleRate(target_rate),
        buffer_size: cpal::BufferSize::Fixed(512),
    };

    let default_config = device.default_output_config().ok().map(|cfg| {
        let mut config = cfg.config();
        config.channels = 2;
        config.buffer_size = cpal::BufferSize::Default;
        config
    });

    let mut attempts = Vec::new();

    if resolved_name == "Unknown Device" {
        if let Some(config) = default_config {
            attempts.push(("device default rate / default buffer", config));
        }
        attempts.push(("requested rate / default buffer", requested_default_buffer));
        attempts.push(("requested rate / 512 frame buffer", requested_fixed_buffer));
    } else {
        attempts.push(("requested rate / default buffer", requested_default_buffer));
        attempts.push(("requested rate / 512 frame buffer", requested_fixed_buffer));
        if let Some(config) = default_config {
            attempts.push(("device default rate / default buffer", config));
        }
    }

    let mut last_error: Option<Box<dyn std::error::Error>> = None;
    let mut cons_opt = Some(cons);

    for (label, config) in attempts {
        let cons_to_try = cons_opt.take().expect("audio consumer should be available");
        match build_stream_helper(&device, &config, cons_to_try, Arc::clone(&state)) {
            Ok(stream) => {
                stream.play()?;
                state.store_target_rate_for_output(config.sample_rate.0);
                state.target_bits.store(32, Ordering::Relaxed);
                state
                    .output_transport
                    .store(OutputTransport::PcmShared.as_id(), Ordering::Relaxed);
                println!(
                    "AudioWorker: Opened output stream at {}Hz ({})",
                    config.sample_rate.0, label
                );
                let cpal_out = CpalOutput {
                    stream,
                    #[cfg(target_os = "macos")]
                    hogged_device: hogged_dev_id,
                };
                return Ok((ActiveOutput::Cpal(cpal_out), config.sample_rate.0));
            }
            Err((e, cons_returned)) => {
                eprintln!(
                    "AudioWorker: Output config failed: {} at {}Hz ({:?})",
                    label, config.sample_rate.0, e
                );
                last_error = Some(e);
                cons_opt = Some(cons_returned);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "No output stream configurations were attempted".into()))
}

#[cfg(target_os = "macos")]
fn open_coreaudio_audio_unit_with_retry(
    device_id: coreaudio_sys::AudioDeviceID,
    label: &str,
) -> Result<coreaudio::audio_unit::AudioUnit, coreaudio::Error> {
    use coreaudio::audio_unit::macos_helpers::audio_unit_from_device_id;

    let mut last_error = None;
    for (attempt, delay) in COREAUDIO_OPEN_RETRY_DELAYS.iter().copied().enumerate() {
        if delay > Duration::from_millis(0) {
            thread::sleep(delay);
        }
        match audio_unit_from_device_id(device_id, false) {
            Ok(audio_unit) => {
                if attempt > 0 {
                    println!(
                        "AudioWorker: CoreAudio {label} audio unit opened after {} retries",
                        attempt
                    );
                }
                return Ok(audio_unit);
            }
            Err(err) => {
                eprintln!(
                    "AudioWorker: CoreAudio {label} audio unit open attempt {} failed: {:?}",
                    attempt + 1,
                    err
                );
                last_error = Some(err);
            }
        }
    }

    Err(last_error.expect("CoreAudio retry loop should attempt at least once"))
}

#[cfg(target_os = "macos")]
fn start_coreaudio_audio_unit_with_retry(
    audio_unit: &mut coreaudio::audio_unit::AudioUnit,
    label: &str,
) -> Result<(), coreaudio::Error> {
    let mut last_error = None;
    for (attempt, delay) in COREAUDIO_OPEN_RETRY_DELAYS.iter().copied().enumerate() {
        if delay > Duration::from_millis(0) {
            thread::sleep(delay);
        }
        match audio_unit.start() {
            Ok(()) => {
                if attempt > 0 {
                    println!(
                        "AudioWorker: CoreAudio {label} stream started after {} retries",
                        attempt
                    );
                }
                return Ok(());
            }
            Err(err) => {
                eprintln!(
                    "AudioWorker: CoreAudio {label} stream start attempt {} failed: {:?}",
                    attempt + 1,
                    err
                );
                last_error = Some(err);
            }
        }
    }

    Err(last_error.expect("CoreAudio retry loop should attempt at least once"))
}

#[cfg(target_os = "macos")]
fn coreaudio_open_step_with_retry<F>(
    label: &str,
    phase: &str,
    mut operation: F,
) -> Result<(), coreaudio::Error>
where
    F: FnMut() -> Result<(), coreaudio::Error>,
{
    let mut last_error = None;
    for (attempt, delay) in COREAUDIO_OPEN_RETRY_DELAYS.iter().copied().enumerate() {
        if delay > Duration::from_millis(0) {
            thread::sleep(delay);
        }
        match operation() {
            Ok(()) => {
                if attempt > 0 {
                    println!(
                        "AudioWorker: CoreAudio {label} {phase} succeeded after {} retries",
                        attempt
                    );
                }
                return Ok(());
            }
            Err(err) => {
                eprintln!(
                    "AudioWorker: CoreAudio {label} {phase} attempt {} failed: {:?}",
                    attempt + 1,
                    err
                );
                last_error = Some(err);
            }
        }
    }

    Err(last_error.expect("CoreAudio retry loop should attempt at least once"))
}

#[cfg(target_os = "macos")]
fn open_coreaudio_pcm_stream(
    device_name_opt: &Option<String>,
    target_rate: u32,
    exclusive: bool,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
) -> Result<(CoreAudioPcmOutput, u32), (Box<dyn std::error::Error>, AudioConsumer)> {
    use coreaudio::audio_unit::audio_format::LinearPcmFlags;
    use coreaudio::audio_unit::macos_helpers::set_device_sample_rate;
    use coreaudio::audio_unit::render_callback::{self, data};
    use coreaudio::audio_unit::{Element, SampleFormat, Scope, StreamFormat};
    use coreaudio::sys::kAudioUnitProperty_StreamFormat;

    let device_id = match device_name_opt.as_deref() {
        Some(name) => match find_device_id_by_name(name) {
            Some(device_id) => device_id,
            None => {
                return Err((
                    format!("Selected CoreAudio device '{name}' not found").into(),
                    cons,
                ));
            }
        },
        None => match unsafe { get_default_output_device() } {
            Some(device_id) => device_id,
            None => return Err(("No default CoreAudio output device found".into(), cons)),
        },
    };
    let resolved_name = unsafe { crate::audio::output::coreaudio_hog::get_device_name(device_id) }
        .unwrap_or_else(|| "Unknown Device".to_string());
    println!(
        "AudioWorker: Opening CoreAudio PCM device: {} at {}Hz",
        resolved_name, target_rate
    );

    let hogged_dev_id: Option<coreaudio_sys::AudioDeviceID> = if exclusive {
        unsafe {
            match set_hog_mode(device_id, true) {
                Ok(_) => {
                    state.exclusive.store(true, Ordering::Relaxed);
                    println!("AudioWorker: CoreAudio PCM Hog Mode enabled.");
                    Some(device_id)
                }
                Err(status) => {
                    eprintln!(
                        "AudioWorker: Failed to enable CoreAudio PCM Hog Mode. OSStatus: {}",
                        status
                    );
                    state.exclusive.store(false, Ordering::Relaxed);
                    None
                }
            }
        }
    } else {
        state.exclusive.store(false, Ordering::Relaxed);
        None
    };

    let release_hog = |hogged_dev_id: Option<coreaudio_sys::AudioDeviceID>| {
        if let Some(dev_id) = hogged_dev_id {
            unsafe {
                let _ = set_hog_mode(dev_id, false);
            }
            state.exclusive.store(false, Ordering::Relaxed);
        }
    };

    if let Err(e) = set_device_sample_rate(device_id, target_rate as f64) {
        release_hog(hogged_dev_id);
        return Err((
            format!("CoreAudio device does not accept {target_rate}Hz PCM rate: {e}").into(),
            cons,
        ));
    }

    let mut audio_unit = match open_coreaudio_audio_unit_with_retry(device_id, "PCM") {
        Ok(audio_unit) => audio_unit,
        Err(e) => {
            release_hog(hogged_dev_id);
            return Err((Box::new(e), cons));
        }
    };

    let stream_format = StreamFormat {
        sample_rate: target_rate as f64,
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
        channels: 2,
    };
    let asbd = stream_format.to_asbd();
    if let Err(e) = coreaudio_open_step_with_retry("PCM", "stream-format set", || {
        audio_unit.set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Input,
            Element::Output,
            Some(&asbd),
        )
    }) {
        release_hog(hogged_dev_id);
        return Err((Box::new(e), cons));
    }

    let ring_capacity_samples = cons.len() + cons.free_len();
    publish_pcm_buffer_health(&state, ring_capacity_samples, cons.len(), 0, false);
    state
        .pcm_ring_low_watermark_samples
        .store(u64::MAX, Ordering::Relaxed);
    let cons_cell = Arc::new(CoreAudioCallbackOwned::new(cons));
    let cons_cell_clone = Arc::clone(&cons_cell);
    let callback_state = Arc::clone(&state);
    let mut scratch = vec![0.0_f64; COREAUDIO_MAX_CALLBACK_SAMPLES];
    let mut ramp = PcmTransitionRamp::new(2);
    let mut last_callback_at = None::<Instant>;
    type PcmArgs = render_callback::Args<data::Interleaved<f32>>;

    if let Err(e) = audio_unit.set_render_callback(move |args: PcmArgs| {
        #[cfg(all(debug_assertions, target_os = "macos"))]
        let _allocation_guard = crate::rt_allocator::RealtimeCallbackGuard::enter();
        let out = args.data.buffer;
        let channels = args.data.channels.max(1);
        (|| {
            let callback_now = Instant::now();
            if let Some(previous) = last_callback_at.replace(callback_now) {
                let gap_ns = callback_now.duration_since(previous).as_nanos() as u64;
                callback_state.record_audio_callback_gap_ns(gap_ns);
                callback_state.record_startup_callback_gap_ns(gap_ns);
            }
            let volume = f32::from_bits(callback_state.volume.load(Ordering::Relaxed)) as f64;
            let is_playing = callback_state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING;
            if let Some(c) = unsafe { cons_cell_clone.callback_ptr().as_mut() } {
                if callback_state.flush_buffer.swap(false, Ordering::Relaxed) {
                    c.clear();
                }

                let mut max_l = 0.0f64;
                let mut max_r = 0.0f64;
                if out.len() > scratch.len() {
                    record_pcm_underrun_rt(&callback_state, out.len() as u64);
                    out.fill(0.0);
                    return Ok(());
                }
                scratch[..out.len()].fill(0.0);
                let scratch = &mut scratch[..out.len()];
                publish_pcm_buffer_health(
                    &callback_state,
                    ring_capacity_samples,
                    c.len(),
                    out.len() / channels,
                    is_playing,
                );
                if is_playing {
                    // Pop whole frames only: an odd-count pop would orphan one
                    // channel's sample and permanently swap L/R from then on.
                    let aligned = (c.len().min(scratch.len()) / channels) * channels;
                    let read = c.pop_slice(&mut scratch[..aligned]);
                    if read < out.len() {
                        record_pcm_underrun_rt(&callback_state, (out.len() - read) as u64);
                    }

                    ramp.process(scratch, out.len(), channels, true);
                    for (i, sample) in out.iter_mut().enumerate() {
                        let val = scratch[i] * volume;
                        *sample = val as f32;
                        if i % channels == 0 {
                            max_l = max_l.max(val.abs());
                        } else if i % channels == 1 {
                            max_r = max_r.max(val.abs());
                        }
                    }

                    let played_frames = (read / channels) as u64;
                    callback_state
                        .position_samples
                        .fetch_add(played_frames, Ordering::Relaxed);
                } else {
                    ramp.process(scratch, 0, channels, false);
                    for (i, sample) in out.iter_mut().enumerate() {
                        let val = scratch[i] * volume;
                        *sample = val as f32;
                    }
                }

                callback_state
                    .meter_l
                    .store((max_l as f32).to_bits(), Ordering::Relaxed);
                callback_state
                    .meter_r
                    .store((max_r as f32).to_bits(), Ordering::Relaxed);
            } else {
                if is_playing {
                    record_pcm_underrun_rt(&callback_state, out.len() as u64);
                }
                for sample in out.iter_mut() {
                    *sample = 0.0;
                }
            }
            Ok(())
        })()
    }) {
        let cons_returned = cons_cell.take_before_start().unwrap();
        release_hog(hogged_dev_id);
        return Err((Box::new(e), cons_returned));
    }

    if let Err(e) = start_coreaudio_audio_unit_with_retry(&mut audio_unit, "PCM") {
        let cons_returned = cons_cell.take_before_start().unwrap();
        release_hog(hogged_dev_id);
        return Err((Box::new(e), cons_returned));
    }

    state.store_target_rate_for_output(target_rate);
    state.target_bits.store(32, Ordering::Relaxed);
    state
        .output_transport
        .store(OutputTransport::PcmCoreAudio.as_id(), Ordering::Relaxed);
    println!(
        "AudioWorker: Opened CoreAudio PCM stream at {}Hz (float32)",
        target_rate
    );

    Ok((
        CoreAudioPcmOutput {
            audio_unit,
            hogged_device: hogged_dev_id,
        },
        target_rate,
    ))
}

#[cfg(target_os = "macos")]
pub(super) fn open_coreaudio_dop_stream(
    device_name_opt: &Option<String>,
    dop_frame_rate: u32,
    ring_capacity_samples: usize,
    exclusive: bool,
    cons: DopConsumer,
    state: Arc<AtomicPlayerState>,
) -> Result<CoreAudioDopOutput, (Box<dyn std::error::Error>, DopConsumer)> {
    use coreaudio::audio_unit::audio_format::LinearPcmFlags;
    use coreaudio::audio_unit::macos_helpers::{
        find_matching_physical_format, set_device_physical_stream_format, set_device_sample_rate,
    };
    use coreaudio::audio_unit::render_callback::{self, data};
    use coreaudio::audio_unit::{Element, SampleFormat, Scope, StreamFormat};
    use coreaudio::sys::kAudioUnitProperty_StreamFormat;

    state.record_coreaudio_dop_lifecycle(COREAUDIO_DOP_LIFECYCLE_OPEN_ATTEMPT);

    let device_id = match device_name_opt.as_deref() {
        Some(name) => match find_device_id_by_name(name) {
            Some(device_id) => device_id,
            None => {
                return Err((
                    format!("Selected CoreAudio device '{name}' not found").into(),
                    cons,
                ));
            }
        },
        None => match unsafe { get_default_output_device() } {
            Some(device_id) => device_id,
            None => return Err(("No default CoreAudio output device found".into(), cons)),
        },
    };
    let resolved_name = unsafe { crate::audio::output::coreaudio_hog::get_device_name(device_id) }
        .unwrap_or_else(|| "Unknown Device".to_string());
    println!(
        "AudioWorker: Opening CoreAudio DoP device: {} at {}Hz",
        resolved_name, dop_frame_rate
    );

    let hogged_dev_id: Option<coreaudio_sys::AudioDeviceID> = if exclusive {
        unsafe {
            match set_hog_mode(device_id, true) {
                Ok(_) => {
                    state.exclusive.store(true, Ordering::Relaxed);
                    println!("AudioWorker: CoreAudio DoP Hog Mode enabled.");
                    Some(device_id)
                }
                Err(status) => {
                    eprintln!(
                        "AudioWorker: Failed to enable CoreAudio DoP Hog Mode. OSStatus: {}",
                        status
                    );
                    state.exclusive.store(false, Ordering::Relaxed);
                    None
                }
            }
        }
    } else {
        state.exclusive.store(false, Ordering::Relaxed);
        None
    };

    let release_hog = |hogged_dev_id: Option<coreaudio_sys::AudioDeviceID>| {
        if let Some(dev_id) = hogged_dev_id {
            unsafe {
                let _ = set_hog_mode(dev_id, false);
            }
            state.exclusive.store(false, Ordering::Relaxed);
        }
    };

    if let Err(e) = set_device_sample_rate(device_id, dop_frame_rate as f64) {
        release_hog(hogged_dev_id);
        return Err((
            format!("CoreAudio device does not accept {dop_frame_rate}Hz DoP rate: {e}").into(),
            cons,
        ));
    }
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: CoreAudio DoP sample rate accepted: device={} rate={}Hz",
            resolved_name, dop_frame_rate
        );
    }

    let physical_format = find_matching_physical_format(
        device_id,
        StreamFormat {
            sample_rate: dop_frame_rate as f64,
            sample_format: SampleFormat::I32,
            flags: LinearPcmFlags::empty(),
            channels: 2,
        },
    );
    let Some(physical_asbd) = physical_format else {
        release_hog(hogged_dev_id);
        return Err((
            format!(
                "CoreAudio device has no 32-bit integer physical format at {dop_frame_rate}Hz for DoP"
            )
            .into(),
            cons,
        ));
    };
    if let Err(e) = set_device_physical_stream_format(device_id, physical_asbd) {
        release_hog(hogged_dev_id);
        return Err((
            format!("CoreAudio failed to select integer DoP format: {e}").into(),
            cons,
        ));
    }
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: CoreAudio DoP selected physical ASBD sample_rate={} bits={} channels={} bytes_per_frame={}",
            physical_asbd.mSampleRate as u32,
            physical_asbd.mBitsPerChannel,
            physical_asbd.mChannelsPerFrame,
            physical_asbd.mBytesPerFrame,
        );
    }
    println!(
        "AudioWorker: CoreAudio DoP physical format set: {}Hz, {} bits, {} channels",
        physical_asbd.mSampleRate as u32,
        physical_asbd.mBitsPerChannel,
        physical_asbd.mChannelsPerFrame
    );

    let requested_buffer_frames = recommended_coreaudio_dop_buffer_frames(dop_frame_rate);
    state
        .dsd_requested_hardware_buffer_frames
        .store(requested_buffer_frames, Ordering::Relaxed);
    state
        .dsd_hardware_buffer_min_frames
        .store(0, Ordering::Relaxed);
    state
        .dsd_hardware_buffer_max_frames
        .store(0, Ordering::Relaxed);
    record_coreaudio_device_buffer_frame_range(state.as_ref(), device_id);
    let mut direct_device_buffer_frames = match configure_coreaudio_device_output_buffer_frames(
        device_id,
        requested_buffer_frames,
    ) {
        Ok(frames) => {
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: CoreAudio DoP direct device buffer frames before AudioUnit requested={} actual={}",
                    requested_buffer_frames, frames
                );
            }
            Some(frames)
        }
        Err(e) => {
            let actual = current_coreaudio_device_output_buffer_frames(device_id).ok();
            eprintln!(
                "AudioWorker: CoreAudio DoP direct device buffer request to {} frames failed ({e}); current device buffer is {}",
                requested_buffer_frames,
                actual
                    .map(|frames| frames.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            );
            None
        }
    };

    let mut audio_unit = match open_coreaudio_audio_unit_with_retry(device_id, "DoP") {
        Ok(audio_unit) => audio_unit,
        Err(e) => {
            release_hog(hogged_dev_id);
            return Err((Box::new(e), cons));
        }
    };

    let stream_format = StreamFormat {
        sample_rate: dop_frame_rate as f64,
        sample_format: SampleFormat::I32,
        flags: LinearPcmFlags::IS_SIGNED_INTEGER | LinearPcmFlags::IS_PACKED,
        channels: 2,
    };
    let asbd = stream_format.to_asbd();
    if let Err(e) = coreaudio_open_step_with_retry("DoP", "stream-format set", || {
        audio_unit.set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Input,
            Element::Output,
            Some(&asbd),
        )
    }) {
        release_hog(hogged_dev_id);
        return Err((Box::new(e), cons));
    }

    record_coreaudio_device_buffer_frame_range(state.as_ref(), device_id);
    match configure_coreaudio_device_output_buffer_frames(device_id, requested_buffer_frames) {
        Ok(frames) => {
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: CoreAudio DoP direct device buffer frames after AudioUnit format requested={} actual={}",
                    requested_buffer_frames, frames
                );
            }
            direct_device_buffer_frames = Some(frames);
        }
        Err(e) => {
            if direct_device_buffer_frames.is_none() {
                let actual = current_coreaudio_device_output_buffer_frames(device_id).ok();
                eprintln!(
                    "AudioWorker: CoreAudio DoP direct device buffer request after AudioUnit format to {} frames failed ({e}); current device buffer is {}",
                    requested_buffer_frames,
                    actual
                        .map(|frames| frames.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
            } else if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: CoreAudio DoP direct device buffer refresh after AudioUnit format failed ({e}); keeping earlier device request"
                );
            }
        }
    }

    let hardware_buffer_frames = match direct_device_buffer_frames {
        Some(_) => current_coreaudio_device_output_buffer_frames(device_id)
            .ok()
            .or_else(|| current_coreaudio_output_buffer_frames(&mut audio_unit).ok())
            .unwrap_or(requested_buffer_frames),
        None => {
            match configure_coreaudio_output_buffer_frames(&mut audio_unit, requested_buffer_frames)
            {
                Ok(frames) => {
                    if crate::audio::debug::audio_debug_enabled() {
                        eprintln!(
                            "AudioWorker DEBUG: CoreAudio DoP AudioUnit buffer frames requested={} actual={}",
                            requested_buffer_frames, frames
                        );
                    }
                    frames
                }
                Err(e) => {
                    let actual = current_coreaudio_device_output_buffer_frames(device_id)
                        .ok()
                        .or_else(|| current_coreaudio_output_buffer_frames(&mut audio_unit).ok());
                    eprintln!(
                        "AudioWorker: CoreAudio DoP could not set AudioUnit buffer to {} frames ({e}); continuing with {} frames",
                        requested_buffer_frames,
                        actual
                            .map(|frames| frames.to_string())
                            .unwrap_or_else(|| "device default".to_string())
                    );
                    actual.unwrap_or(requested_buffer_frames)
                }
            }
        }
    };
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: CoreAudio DoP final buffer frames requested={} actual={}",
            requested_buffer_frames, hardware_buffer_frames
        );
    }
    let initial_ring_fill_samples = cons.len();
    state
        .dsd_ring_capacity_samples
        .store(ring_capacity_samples as u64, Ordering::Relaxed);
    state
        .dsd_ring_fill_samples
        .store(initial_ring_fill_samples as u64, Ordering::Relaxed);
    state
        .dsd_ring_low_watermark_samples
        .store(u64::MAX, Ordering::Relaxed);
    state
        .dsd_callback_frames
        .store(hardware_buffer_frames, Ordering::Relaxed);
    state
        .dsd_hardware_buffer_frames
        .store(hardware_buffer_frames, Ordering::Relaxed);
    state.record_startup_ring_fill(
        initial_ring_fill_samples as u64,
        dop_frame_rate.max(1) as u64 * 2,
    );

    let cons_cell = Arc::new(CoreAudioCallbackOwned::new(cons));
    let cons_cell_clone = Arc::clone(&cons_cell);
    let callback_state = Arc::clone(&state);
    let mut scratch =
        vec![0_i32; (hardware_buffer_frames as usize * 2).max(COREAUDIO_MAX_CALLBACK_SAMPLES)];
    let mut dop_idle = crate::audio::dsd::dop::DopIdlePattern::new();
    let mut idle_callbacks_remaining = 0_u8;
    let mut dop_recovery_active = false;
    let mut last_output_kind = CoreAudioDopOutputKind::Unknown;
    let mut last_program_payload_fingerprint = None::<u64>;
    let diagnostic_scan_interval = coreaudio_dop_diagnostic_scan_interval(
        dop_frame_rate,
        crate::audio::debug::audio_debug_enabled(),
    );
    let scan_every_callback = diagnostic_scan_interval == 1;
    let mut diagnostic_callbacks_until_scan = 0_u32;
    // Single marker-phase authority for the stream: every outgoing frame gets its
    // marker byte re-stamped just before the device sees it, so the 0x05/0xFA cadence
    // stays continuous across program/idle splices (underrun, pause/resume, flush).
    // The DSD payload bits stay in the original frame order; only the marker byte is
    // normalized.
    let mut marker_stamper = crate::audio::dsd::dop::DopMarkerStamper::new();
    let mut last_callback_at = None::<Instant>;
    let mut callback_index = 0_u64;
    let mut ring_read_cursor_samples = 0_u64;
    let mut ring_pressure_state = CoreAudioDopRingPressureState::default();
    type DopArgs = render_callback::Args<data::Interleaved<i32>>;

    if let Err(e) = audio_unit.set_render_callback(move |args: DopArgs| {
        #[cfg(all(debug_assertions, target_os = "macos"))]
        let _allocation_guard = crate::rt_allocator::RealtimeCallbackGuard::enter();
        let out = args.data.buffer;
        let channels = args.data.channels.max(1);
        (|| {
            let mut output_kind = CoreAudioDopOutputKind::Idle;
            let mut callback_ring_fill_samples = 0_usize;
            let mut callback_program_read_samples = 0_usize;
            callback_index = callback_index.saturating_add(1);
            let callback_now = Instant::now();
            let callback_at_ms = coreaudio_unix_epoch_millis();
            let callback_gap_ns = last_callback_at
                .replace(callback_now)
                .map(|previous| callback_now.duration_since(previous).as_nanos() as u64);
            if let Some(gap_ns) = callback_gap_ns {
                callback_state.record_audio_callback_gap_ns(gap_ns);
                callback_state.record_startup_callback_gap_ns(gap_ns);
            }
            let playback_state = callback_state.state.load(Ordering::Relaxed);
            let is_playing = playback_state == PLAYBACK_PLAYING;
            let is_starting = playback_state == PLAYBACK_STARTING;
            if is_playing && let Some(gap_ns) = callback_gap_ns {
                record_coreaudio_dop_soft_callback_gap(
                    &callback_state,
                    gap_ns,
                    out.len() / channels,
                    dop_frame_rate,
                );
                record_coreaudio_dop_callback_deadline_miss(
                    &callback_state,
                    gap_ns,
                    out.len() / channels,
                    channels,
                    dop_frame_rate,
                );
            }
            if let Some(c) = unsafe { cons_cell_clone.callback_ptr().as_mut() } {
                if callback_state.flush_buffer.swap(false, Ordering::Relaxed) {
                    callback_state.record_flush_consumed();
                    c.clear();
                    record_coreaudio_dop_alignment_reset(&callback_state);
                    idle_callbacks_remaining = 0;
                    dop_recovery_active = false;
                }

                if is_playing {
                    if out.len() > scratch.len() {
                        record_coreaudio_dop_underrun(&callback_state, out.len() as u64, false);
                        dop_idle.fill_interleaved_i32(out, channels);
                        return Ok(());
                    }
                    let ring_fill = c.len();
                    callback_ring_fill_samples = ring_fill;
                    publish_coreaudio_dop_buffer_health(
                        &callback_state,
                        ring_capacity_samples,
                        ring_fill,
                        out.len() / channels,
                        hardware_buffer_frames,
                        dop_frame_rate,
                        true,
                    );
                    record_coreaudio_dop_ring_pressure(
                        &callback_state,
                        ring_fill,
                        out.len(),
                        channels,
                        dop_frame_rate,
                        &mut ring_pressure_state,
                    );

                    match coreaudio_dop_callback_plan_with_eof(
                        ring_fill,
                        out.len(),
                        channels,
                        idle_callbacks_remaining,
                        dop_recovery_active,
                        ring_capacity_samples,
                        callback_state.eof_drain_requested.load(Ordering::Relaxed),
                    ) {
                        CoreAudioDopCallbackPlan::PlayProgram { samples } => {
                            dop_recovery_active = false;
                            let read = c.pop_slice(&mut scratch[..samples]);
                            callback_program_read_samples = read;
                            if ring_capacity_samples > 0 {
                                ring_read_cursor_samples = ring_read_cursor_samples
                                    .wrapping_add(read as u64)
                                    % ring_capacity_samples as u64;
                            }
                            debug_assert_eq!(read, samples);
                            let played_frames = copy_coreaudio_dop_program_or_idle(
                                out,
                                &scratch[..read],
                                channels,
                                &mut dop_idle,
                            );
                            output_kind = if read >= samples {
                                CoreAudioDopOutputKind::Program
                            } else {
                                CoreAudioDopOutputKind::Mixed
                            };
                            if read < samples {
                                record_coreaudio_dop_underrun(
                                    &callback_state,
                                    (samples - played_frames * channels) as u64,
                                    false,
                                );
                            }

                            callback_state
                                .position_samples
                                .fetch_add(played_frames as u64 * 16, Ordering::Relaxed);
                        }
                        CoreAudioDopCallbackPlan::RecoveryIdle {
                            missing_samples,
                            entering_recovery,
                        } => {
                            dop_recovery_active = true;
                            if entering_recovery {
                                record_coreaudio_dop_underrun(
                                    &callback_state,
                                    missing_samples as u64,
                                    false,
                                );
                            }
                            dop_idle.fill_interleaved_i32(out, channels);
                        }
                        CoreAudioDopCallbackPlan::IdleHold => {
                            idle_callbacks_remaining -= 1;
                            dop_recovery_active = false;
                            dop_idle.fill_interleaved_i32(out, channels);
                        }
                    }
                } else {
                    dop_recovery_active = false;
                    callback_ring_fill_samples = c.len();
                    publish_coreaudio_dop_buffer_health(
                        &callback_state,
                        ring_capacity_samples,
                        c.len(),
                        out.len() / channels,
                        hardware_buffer_frames,
                        dop_frame_rate,
                        false,
                    );
                    dop_idle.fill_interleaved_i32(out, channels);
                    if is_starting {
                        let discarded = discard_coreaudio_dop_misaligned_samples(c, channels);
                        record_coreaudio_dop_alignment_discard(&callback_state, discarded);
                    }
                }

                callback_state.meter_l.store(0, Ordering::Relaxed);
                callback_state.meter_r.store(0, Ordering::Relaxed);
            } else {
                if is_playing {
                    record_coreaudio_dop_underrun(&callback_state, out.len() as u64, true);
                    idle_callbacks_remaining = 1;
                    dop_recovery_active = false;
                }
                dop_idle.fill_interleaved_i32(out, channels);
            }
            // Re-stamp every outgoing marker (program and idle alike) from the
            // single stream-lifetime alternation source.
            marker_stamper.restamp_interleaved_i32(out, channels);
            let scan_payload_and_markers = coreaudio_dop_should_scan_diagnostics(
                &mut diagnostic_callbacks_until_scan,
                diagnostic_scan_interval,
            );
            let payload_fingerprint = record_coreaudio_dop_output_diagnostics(
                &callback_state,
                out,
                channels,
                output_kind,
                &mut last_output_kind,
                &mut last_program_payload_fingerprint,
                scan_payload_and_markers,
            );
            record_coreaudio_dop_callback_trace(
                &callback_state,
                CoreAudioDopCallbackTrace {
                    callback_index,
                    callback_at_ms,
                    callback_gap_ns: callback_gap_ns.unwrap_or(0),
                    callback_frames: out.len() / channels,
                    output_kind,
                    ring_fill_samples: callback_ring_fill_samples,
                    program_read_samples: callback_program_read_samples,
                    ring_read_cursor_samples,
                    payload_fingerprint,
                    scan_every_callback,
                },
            );
            Ok(())
        })()
    }) {
        let cons_returned = cons_cell.take_before_start().unwrap();
        release_hog(hogged_dev_id);
        return Err((Box::new(e), cons_returned));
    }

    if let Err(e) = start_coreaudio_audio_unit_with_retry(&mut audio_unit, "DoP") {
        let cons_returned = cons_cell.take_before_start().unwrap();
        release_hog(hogged_dev_id);
        return Err((Box::new(e), cons_returned));
    }
    state.record_coreaudio_dop_lifecycle(COREAUDIO_DOP_LIFECYCLE_START);

    state.target_bits.store(24, Ordering::Relaxed);
    state
        .output_transport
        .store(OutputTransport::DopCoreAudio.as_id(), Ordering::Relaxed);
    println!(
        "AudioWorker: Opened CoreAudio DoP stream at {}Hz (signed i32)",
        dop_frame_rate
    );

    Ok(CoreAudioDopOutput {
        audio_unit,
        hogged_device: hogged_dev_id,
        state,
    })
}

#[cfg(all(test, target_os = "macos"))]
const COREAUDIO_DOP_IDLE_PAYLOAD: u16 = 0x6969;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoreAudioDopOutputKind {
    Unknown,
    Program,
    Idle,
    Mixed,
}

#[cfg(target_os = "macos")]
impl CoreAudioDopOutputKind {
    fn as_id(self) -> u32 {
        match self {
            Self::Unknown => 0,
            Self::Program => 1,
            Self::Idle => 2,
            Self::Mixed => 3,
        }
    }
}

#[cfg(target_os = "macos")]
struct CoreAudioDopCallbackTrace {
    callback_index: u64,
    callback_at_ms: u64,
    callback_gap_ns: u64,
    callback_frames: usize,
    output_kind: CoreAudioDopOutputKind,
    ring_fill_samples: usize,
    program_read_samples: usize,
    ring_read_cursor_samples: u64,
    payload_fingerprint: Option<u64>,
    scan_every_callback: bool,
}

#[cfg(target_os = "macos")]
fn copy_coreaudio_dop_program_or_idle(
    out: &mut [i32],
    program: &[i32],
    channels: usize,
    dop_idle: &mut crate::audio::dsd::dop::DopIdlePattern,
) -> usize {
    let channels = channels.max(1);
    debug_assert_eq!(out.len() % channels, 0);

    let program_samples = ((program.len().min(out.len())) / channels) * channels;
    out[..program_samples].copy_from_slice(&program[..program_samples]);
    if program_samples < out.len() {
        dop_idle.fill_interleaved_i32(&mut out[program_samples..], channels);
    }
    program_samples / channels
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_payload(sample: i32) -> u16 {
    (((sample as u32) >> 8) & 0xffff) as u16
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_marker(sample: i32) -> u8 {
    (((sample as u32) >> 24) & 0xff) as u8
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_diagnostic_scan_interval(dop_frame_rate: u32, debug_enabled: bool) -> u32 {
    if debug_enabled || dop_frame_rate >= COREAUDIO_DOP_HIGH_RATE_THRESHOLD_HZ {
        1
    } else {
        COREAUDIO_DOP_DIAGNOSTIC_SCAN_INTERVAL_CALLBACKS
    }
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_should_scan_diagnostics(callbacks_until_scan: &mut u32, interval: u32) -> bool {
    let interval = interval.max(1);
    if *callbacks_until_scan == 0 {
        *callbacks_until_scan = interval - 1;
        true
    } else {
        *callbacks_until_scan -= 1;
        false
    }
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_output_diagnostics(
    state: &AtomicPlayerState,
    out: &[i32],
    channels: usize,
    output_kind: CoreAudioDopOutputKind,
    last_output_kind: &mut CoreAudioDopOutputKind,
    last_program_payload_fingerprint: &mut Option<u64>,
    scan_payload_and_markers: bool,
) -> Option<u64> {
    let payload_fingerprint = if scan_payload_and_markers {
        state
            .dsd_dop_marker_scan_count
            .fetch_add(1, Ordering::Relaxed);
        if coreaudio_dop_has_marker_error(out, channels) {
            state
                .dsd_dop_marker_error_events
                .fetch_add(1, Ordering::Relaxed);
        }
        Some(coreaudio_dop_payload_fingerprint(out))
    } else {
        None
    };

    if let Some(fingerprint) = payload_fingerprint {
        state
            .dsd_dop_last_payload_fingerprint
            .store(fingerprint, Ordering::Relaxed);
        state
            .dsd_dop_last_payload_fingerprint_at_ms
            .store(coreaudio_unix_epoch_millis(), Ordering::Relaxed);
    }

    if coreaudio_dop_is_payload_source_splice(*last_output_kind, output_kind) {
        state
            .dsd_dop_program_idle_splice_events
            .fetch_add(1, Ordering::Relaxed);
        if let Some(transition_id) =
            coreaudio_dop_output_transition_id(*last_output_kind, output_kind)
        {
            state.record_dop_output_transition(transition_id);
        }
    }

    if output_kind == CoreAudioDopOutputKind::Program {
        let Some(fingerprint) = payload_fingerprint else {
            *last_program_payload_fingerprint = None;
            *last_output_kind = output_kind;
            return None;
        };
        if last_program_payload_fingerprint.is_some_and(|previous| previous == fingerprint) {
            state
                .dsd_dop_repeated_payload_events
                .fetch_add(1, Ordering::Relaxed);
        }
        *last_program_payload_fingerprint = Some(fingerprint);
    } else {
        *last_program_payload_fingerprint = None;
    }

    *last_output_kind = output_kind;
    payload_fingerprint
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_is_payload_source_splice(
    previous: CoreAudioDopOutputKind,
    current: CoreAudioDopOutputKind,
) -> bool {
    previous != CoreAudioDopOutputKind::Unknown && previous != current
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_callback_trace(
    state: &AtomicPlayerState,
    trace: CoreAudioDopCallbackTrace,
) {
    state
        .dsd_dop_callback_index
        .store(trace.callback_index, Ordering::Relaxed);
    state
        .dsd_dop_last_callback_at_ms
        .store(trace.callback_at_ms, Ordering::Relaxed);
    state
        .dsd_dop_last_callback_gap_ns
        .store(trace.callback_gap_ns, Ordering::Relaxed);
    state
        .dsd_dop_last_callback_frames
        .store(trace.callback_frames as u32, Ordering::Relaxed);
    state
        .dsd_dop_last_output_kind_id
        .store(trace.output_kind.as_id(), Ordering::Relaxed);
    state
        .dsd_dop_last_ring_fill_samples
        .store(trace.ring_fill_samples as u64, Ordering::Relaxed);
    state
        .dsd_dop_last_program_read_samples
        .store(trace.program_read_samples as u64, Ordering::Relaxed);
    state
        .dsd_dop_ring_read_cursor_samples
        .store(trace.ring_read_cursor_samples, Ordering::Relaxed);
    if let Some(fingerprint) = trace.payload_fingerprint {
        state
            .dsd_dop_last_payload_fingerprint
            .store(fingerprint, Ordering::Relaxed);
        state
            .dsd_dop_last_payload_fingerprint_at_ms
            .store(trace.callback_at_ms, Ordering::Relaxed);
    }
    state
        .dsd_dop_every_callback_scan_enabled
        .store(trace.scan_every_callback, Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_output_transition_id(
    previous: CoreAudioDopOutputKind,
    current: CoreAudioDopOutputKind,
) -> Option<u32> {
    match (previous, current) {
        (CoreAudioDopOutputKind::Program, CoreAudioDopOutputKind::Idle) => {
            Some(DOP_OUTPUT_TRANSITION_PROGRAM_TO_IDLE)
        }
        (CoreAudioDopOutputKind::Idle, CoreAudioDopOutputKind::Program) => {
            Some(DOP_OUTPUT_TRANSITION_IDLE_TO_PROGRAM)
        }
        (CoreAudioDopOutputKind::Program, CoreAudioDopOutputKind::Mixed) => {
            Some(DOP_OUTPUT_TRANSITION_PROGRAM_TO_MIXED)
        }
        (CoreAudioDopOutputKind::Mixed, CoreAudioDopOutputKind::Program) => {
            Some(DOP_OUTPUT_TRANSITION_MIXED_TO_PROGRAM)
        }
        (CoreAudioDopOutputKind::Mixed, CoreAudioDopOutputKind::Idle) => {
            Some(DOP_OUTPUT_TRANSITION_MIXED_TO_IDLE)
        }
        (CoreAudioDopOutputKind::Idle, CoreAudioDopOutputKind::Mixed) => {
            Some(DOP_OUTPUT_TRANSITION_IDLE_TO_MIXED)
        }
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_has_marker_error(out: &[i32], channels: usize) -> bool {
    let channels = channels.max(1);
    let mut previous_marker = None::<u8>;
    for frame in out.chunks_exact(channels) {
        let marker = coreaudio_dop_marker(frame[0]);
        if !matches!(marker, 0x05 | 0xfa) {
            return true;
        }
        if frame
            .iter()
            .any(|sample| coreaudio_dop_marker(*sample) != marker)
        {
            return true;
        }
        if previous_marker.is_some_and(|previous| previous == marker) {
            return true;
        }
        previous_marker = Some(marker);
    }
    false
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_payload_fingerprint(out: &[i32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for sample in out {
        hash ^= coreaudio_dop_payload(*sample) as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(target_os = "macos")]
fn coreaudio_unix_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_misaligned_samples(samples: usize, channels: usize) -> usize {
    let channels = channels.max(1);
    samples % channels
}

#[cfg(target_os = "macos")]
fn discard_coreaudio_dop_misaligned_samples(c: &mut DopConsumer, channels: usize) -> usize {
    let stray = coreaudio_dop_misaligned_samples(c.len(), channels);
    let mut discarded = 0;
    for _ in 0..stray {
        if c.pop().is_some() {
            discarded += 1;
        }
    }
    discarded
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_alignment_reset(state: &AtomicPlayerState) {
    state
        .dop_alignment_reset_count
        .fetch_add(1, Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_alignment_discard(state: &AtomicPlayerState, discarded_samples: usize) {
    if discarded_samples > 0 {
        record_coreaudio_dop_alignment_reset(state);
    }
}

#[cfg(test)]
mod pcm_diagnostic_tests {
    use std::sync::atomic::Ordering;

    use super::*;

    #[test]
    fn pcm_buffer_health_tracks_low_watermark_only_when_requested() {
        let state = AtomicPlayerState::new();
        state
            .pcm_ring_low_watermark_samples
            .store(u64::MAX, Ordering::Relaxed);

        publish_pcm_buffer_health(&state, 4096, 2048, 256, true);
        publish_pcm_buffer_health(&state, 4096, 3072, 256, true);
        publish_pcm_buffer_health(&state, 4096, 1024, 256, true);
        publish_pcm_buffer_health(&state, 4096, 3584, 256, false);

        assert_eq!(
            state.pcm_ring_capacity_samples.load(Ordering::Relaxed),
            4096
        );
        assert_eq!(state.pcm_ring_fill_samples.load(Ordering::Relaxed), 3584);
        assert_eq!(
            state.pcm_ring_low_watermark_samples.load(Ordering::Relaxed),
            1024
        );
        assert_eq!(state.pcm_callback_frames.load(Ordering::Relaxed), 256);
    }

    #[test]
    fn pcm_lock_miss_counts_as_underrun() {
        let state = AtomicPlayerState::new();

        record_pcm_underrun(&state, 512, true);

        assert_eq!(state.underrun_events.load(Ordering::Relaxed), 1);
        assert_eq!(state.underrun_samples.load(Ordering::Relaxed), 512);
        assert_eq!(state.pcm_lock_miss_events.load(Ordering::Relaxed), 1);
    }
}

#[cfg(target_os = "macos")]
fn recommended_coreaudio_dop_buffer_frames(dop_frame_rate: u32) -> u32 {
    if let Some(frames) = coreaudio_dop_buffer_frames_override() {
        println!("AudioWorker: DoP hardware buffer override active: {frames} frames");
        return frames;
    }
    if dop_frame_rate >= COREAUDIO_DOP_HIGH_RATE_THRESHOLD_HZ {
        COREAUDIO_DOP_HIGH_RATE_BUFFER_FRAMES
    } else {
        COREAUDIO_DOP_DEFAULT_BUFFER_FRAMES
    }
}

/// Diagnostic knob: FOZMO_DOP_BUFFER_FRAMES forces the requested hardware buffer
/// size for DoP streams (the device may still clamp it to its own min/max range).
#[cfg(target_os = "macos")]
fn coreaudio_dop_buffer_frames_override() -> Option<u32> {
    let raw = std::env::var(crate::app::identity::env_key("DOP_BUFFER_FRAMES")).ok()?;
    parse_coreaudio_dop_buffer_frames_override(&raw)
}

#[cfg(target_os = "macos")]
fn parse_coreaudio_dop_buffer_frames_override(raw: &str) -> Option<u32> {
    let frames = raw.trim().parse::<u32>().ok()?;
    (64..=65_536).contains(&frames).then_some(frames)
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreAudioDopCallbackPlan {
    PlayProgram {
        samples: usize,
    },
    RecoveryIdle {
        missing_samples: usize,
        entering_recovery: bool,
    },
    IdleHold,
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_recovery_resume_samples(
    callback_samples: usize,
    ring_capacity_samples: usize,
) -> usize {
    let half_capacity = (ring_capacity_samples / 2).max(callback_samples);
    let stable_refill = (ring_capacity_samples / 8).max(callback_samples.saturating_mul(2));
    stable_refill.min(half_capacity)
}

#[cfg(all(test, target_os = "macos"))]
fn coreaudio_dop_callback_plan(
    ring_fill_samples: usize,
    callback_samples: usize,
    channels: usize,
    idle_callbacks_remaining: u8,
    recovery_active: bool,
    ring_capacity_samples: usize,
) -> CoreAudioDopCallbackPlan {
    coreaudio_dop_callback_plan_with_eof(
        ring_fill_samples,
        callback_samples,
        channels,
        idle_callbacks_remaining,
        recovery_active,
        ring_capacity_samples,
        false,
    )
}

#[cfg(target_os = "macos")]
fn coreaudio_dop_callback_plan_with_eof(
    ring_fill_samples: usize,
    callback_samples: usize,
    channels: usize,
    idle_callbacks_remaining: u8,
    recovery_active: bool,
    ring_capacity_samples: usize,
    eof_drain_requested: bool,
) -> CoreAudioDopCallbackPlan {
    if idle_callbacks_remaining > 0 {
        return CoreAudioDopCallbackPlan::IdleHold;
    }
    let channels = channels.max(1);
    let available_aligned = (ring_fill_samples / channels) * channels;
    if eof_drain_requested && available_aligned > 0 && available_aligned < callback_samples {
        return CoreAudioDopCallbackPlan::PlayProgram {
            samples: available_aligned,
        };
    }
    let recovery_resume_samples =
        coreaudio_dop_recovery_resume_samples(callback_samples, ring_capacity_samples);
    if recovery_active && available_aligned < recovery_resume_samples {
        CoreAudioDopCallbackPlan::RecoveryIdle {
            missing_samples: callback_samples.saturating_sub(available_aligned),
            entering_recovery: false,
        }
    } else if available_aligned >= callback_samples {
        CoreAudioDopCallbackPlan::PlayProgram {
            samples: callback_samples,
        }
    } else {
        CoreAudioDopCallbackPlan::RecoveryIdle {
            missing_samples: callback_samples - available_aligned,
            entering_recovery: !recovery_active,
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod coreaudio_dop_tests {
    use super::*;
    use crate::audio::dsd::dop::{DopIdlePattern, DopMarkerStamper};
    use crate::audio::engine::state::{
        AtomicPlayerState, PLAYBACK_PAUSED, PLAYBACK_STARTING, PLAYBACK_STOPPED,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn coreaudio_dop_buffer_recommendations_prioritize_callback_slack() {
        assert_eq!(recommended_coreaudio_dop_buffer_frames(352_800), 4096);
        assert_eq!(recommended_coreaudio_dop_buffer_frames(705_600), 16_384);
    }

    #[test]
    fn coreaudio_dop_callback_plan_enters_recovery_when_short() {
        assert_eq!(
            coreaudio_dop_callback_plan(8192, 8192, 2, 0, false, 65_536),
            CoreAudioDopCallbackPlan::PlayProgram { samples: 8192 }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(8191, 8192, 2, 0, false, 65_536),
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples: 2,
                entering_recovery: true
            }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(0, 8192, 2, 0, false, 65_536),
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples: 8192,
                entering_recovery: true
            }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(8192, 8192, 2, 1, false, 65_536),
            CoreAudioDopCallbackPlan::IdleHold
        );
    }

    #[test]
    fn coreaudio_dop_eof_drain_consumes_short_final_block() {
        assert_eq!(
            coreaudio_dop_callback_plan_with_eof(3744, 8192, 2, 0, true, 65_536, true),
            CoreAudioDopCallbackPlan::PlayProgram { samples: 3744 }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(3744, 8192, 2, 0, true, 65_536),
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples: 4448,
                entering_recovery: false,
            }
        );
    }

    #[test]
    fn coreaudio_dop_callback_plan_holds_recovery_until_refill_threshold() {
        assert_eq!(coreaudio_dop_recovery_resume_samples(8192, 65_536), 16_384);
        assert_eq!(
            coreaudio_dop_callback_plan(8192, 8192, 2, 0, true, 65_536),
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples: 0,
                entering_recovery: false
            }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(16_382, 8192, 2, 0, true, 65_536),
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples: 0,
                entering_recovery: false
            }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(16_384, 8192, 2, 0, true, 65_536),
            CoreAudioDopCallbackPlan::PlayProgram { samples: 8192 }
        );
    }

    #[test]
    fn coreaudio_dop_recovery_resume_threshold_caps_to_half_ring() {
        assert_eq!(coreaudio_dop_recovery_resume_samples(8192, 24_576), 12_288);
        assert_eq!(
            coreaudio_dop_callback_plan(12_286, 8192, 2, 0, true, 24_576),
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples: 0,
                entering_recovery: false
            }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(12_288, 8192, 2, 0, true, 24_576),
            CoreAudioDopCallbackPlan::PlayProgram { samples: 8192 }
        );
    }

    #[test]
    fn coreaudio_dop_recovery_waits_for_stable_refill_on_large_rings() {
        let callback_samples = 8192;
        let ring_capacity_samples = 2_822_400;
        let stable_refill = 352_800;
        assert_eq!(
            coreaudio_dop_recovery_resume_samples(callback_samples, ring_capacity_samples),
            stable_refill
        );
        assert_eq!(
            coreaudio_dop_callback_plan(
                stable_refill - 2,
                callback_samples,
                2,
                0,
                true,
                ring_capacity_samples
            ),
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples: 0,
                entering_recovery: false
            }
        );
        assert_eq!(
            coreaudio_dop_callback_plan(
                stable_refill,
                callback_samples,
                2,
                0,
                true,
                ring_capacity_samples
            ),
            CoreAudioDopCallbackPlan::PlayProgram {
                samples: callback_samples
            }
        );
    }

    #[test]
    fn coreaudio_dop_buffer_frames_override_parses_and_bounds() {
        assert_eq!(
            parse_coreaudio_dop_buffer_frames_override("2048"),
            Some(2048)
        );
        assert_eq!(
            parse_coreaudio_dop_buffer_frames_override(" 4096 "),
            Some(4096)
        );
        assert_eq!(parse_coreaudio_dop_buffer_frames_override("64"), Some(64));
        assert_eq!(
            parse_coreaudio_dop_buffer_frames_override("65536"),
            Some(65_536)
        );
        assert_eq!(parse_coreaudio_dop_buffer_frames_override("63"), None);
        assert_eq!(parse_coreaudio_dop_buffer_frames_override("65537"), None);
        assert_eq!(parse_coreaudio_dop_buffer_frames_override("0"), None);
        assert_eq!(parse_coreaudio_dop_buffer_frames_override("abc"), None);
        assert_eq!(parse_coreaudio_dop_buffer_frames_override(""), None);
    }

    #[test]
    fn coreaudio_dop_callback_deadline_miss_counts_as_underrun() {
        let state = AtomicPlayerState::new();

        record_coreaudio_dop_callback_deadline_miss(&state, 10_000_000, 4096, 2, 705_600);

        assert_eq!(
            state
                .dsd_callback_deadline_miss_events
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(state.underrun_events.load(Ordering::Relaxed), 0);

        record_coreaudio_dop_callback_deadline_miss(&state, 17_500_000, 4096, 2, 705_600);

        assert_eq!(
            state
                .dsd_callback_deadline_miss_events
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(state.underrun_events.load(Ordering::Relaxed), 1);
        assert!(
            state.underrun_samples.load(Ordering::Relaxed) >= 8192,
            "deadline miss should estimate at least one callback of missing DoP samples"
        );
        assert!(state.dsd_last_underrun_at_ms.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn coreaudio_dop_short_recovery_emits_idle_without_consuming_program() {
        let channels = 2;
        let callback_samples = 8192;
        let (mut prod, cons) = crate::audio::engine::buffers::new_dop_ring(176_400, 0);
        let program: Vec<i32> = (0..(callback_samples - channels))
            .map(|i| i as i32)
            .collect();
        assert_eq!(prod.push_slice(&program), program.len());

        let state = AtomicPlayerState::new();
        state.position_samples.store(1234, Ordering::Relaxed);
        let mut out = vec![0_i32; callback_samples];
        let mut idle = DopIdlePattern::new();

        match coreaudio_dop_callback_plan(cons.len(), callback_samples, channels, 0, false, 65_536)
        {
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples,
                entering_recovery,
            } => {
                assert_eq!(missing_samples, channels);
                assert!(entering_recovery);
                idle.fill_interleaved_i32(&mut out, channels);
            }
            other => panic!("expected recovery idle, got {other:?}"),
        }

        assert_eq!(cons.len(), program.len());
        assert_eq!(state.position_samples.load(Ordering::Relaxed), 1234);
        for sample in &out {
            assert_eq!(((*sample as u32) >> 8) & 0xffff, 0x6969);
        }
    }

    #[test]
    fn coreaudio_dop_program_copy_drops_misaligned_sample() {
        let channels = 2;
        let program = vec![0x1111_i32, 0x2222_i32, 0x3333_i32];
        let mut out = vec![0x7fff_7fff_i32; 4];
        let mut idle = crate::audio::dsd::dop::DopIdlePattern::new();

        let played_frames =
            copy_coreaudio_dop_program_or_idle(&mut out, &program, channels, &mut idle);

        assert_eq!(played_frames, 1);
        assert_eq!(&out[..2], &program[..2]);
        for sample in &out[2..] {
            assert_eq!(((*sample as u32) >> 8) & 0xffff, 0x6969);
        }
    }

    #[test]
    fn coreaudio_dop_output_diagnostics_count_splices_and_marker_errors() {
        fn sample(marker: u32, payload: u32) -> i32 {
            ((marker << 24) | ((payload & 0xffff) << 8)) as i32
        }

        let channels = 2;
        let state = AtomicPlayerState::new();
        let mut last_kind = CoreAudioDopOutputKind::Unknown;
        let mut last_fingerprint = None;
        let program = [
            sample(0x05, 0x1111),
            sample(0x05, 0x2222),
            sample(0xfa, 0x3333),
            sample(0xfa, 0x4444),
        ];
        let idle = [
            sample(0x05, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
            sample(0x05, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
            sample(0xfa, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
            sample(0xfa, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
        ];

        record_coreaudio_dop_output_diagnostics(
            &state,
            &program,
            channels,
            CoreAudioDopOutputKind::Program,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );
        record_coreaudio_dop_output_diagnostics(
            &state,
            &idle,
            channels,
            CoreAudioDopOutputKind::Idle,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );
        record_coreaudio_dop_output_diagnostics(
            &state,
            &program,
            channels,
            CoreAudioDopOutputKind::Program,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );

        assert_eq!(
            state
                .dsd_dop_program_idle_splice_events
                .load(Ordering::Relaxed),
            2
        );
        assert_eq!(state.dsd_dop_marker_error_events.load(Ordering::Relaxed), 0);

        let bad_marker = [sample(0x05, 0x1111), sample(0xfa, 0x2222)];
        record_coreaudio_dop_output_diagnostics(
            &state,
            &bad_marker,
            channels,
            CoreAudioDopOutputKind::Program,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );
        assert_eq!(state.dsd_dop_marker_error_events.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn coreaudio_dop_output_diagnostics_count_repeated_program_payloads() {
        fn sample(marker: u32, payload: u32) -> i32 {
            ((marker << 24) | ((payload & 0xffff) << 8)) as i32
        }

        let channels = 2;
        let state = AtomicPlayerState::new();
        let mut last_kind = CoreAudioDopOutputKind::Unknown;
        let mut last_fingerprint = None;
        let program = [
            sample(0x05, 0x1111),
            sample(0x05, 0x2222),
            sample(0xfa, 0x3333),
            sample(0xfa, 0x4444),
        ];
        let idle = [
            sample(0x05, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
            sample(0x05, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
        ];

        record_coreaudio_dop_output_diagnostics(
            &state,
            &program,
            channels,
            CoreAudioDopOutputKind::Program,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );
        record_coreaudio_dop_output_diagnostics(
            &state,
            &program,
            channels,
            CoreAudioDopOutputKind::Program,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );
        assert_eq!(
            state
                .dsd_dop_repeated_payload_events
                .load(Ordering::Relaxed),
            1
        );

        record_coreaudio_dop_output_diagnostics(
            &state,
            &idle,
            channels,
            CoreAudioDopOutputKind::Idle,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );
        record_coreaudio_dop_output_diagnostics(
            &state,
            &program,
            channels,
            CoreAudioDopOutputKind::Program,
            &mut last_kind,
            &mut last_fingerprint,
            true,
        );
        assert_eq!(
            state
                .dsd_dop_repeated_payload_events
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn coreaudio_dop_output_diagnostics_can_skip_expensive_scans() {
        fn sample(marker: u32, payload: u32) -> i32 {
            ((marker << 24) | ((payload & 0xffff) << 8)) as i32
        }

        let channels = 2;
        let state = AtomicPlayerState::new();
        let mut last_kind = CoreAudioDopOutputKind::Unknown;
        let mut last_fingerprint = None;
        let program = [sample(0x05, 0x1111), sample(0xfa, 0x2222)];
        let idle = [
            sample(0x05, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
            sample(0x05, COREAUDIO_DOP_IDLE_PAYLOAD as u32),
        ];

        record_coreaudio_dop_output_diagnostics(
            &state,
            &program,
            channels,
            CoreAudioDopOutputKind::Program,
            &mut last_kind,
            &mut last_fingerprint,
            false,
        );
        record_coreaudio_dop_output_diagnostics(
            &state,
            &idle,
            channels,
            CoreAudioDopOutputKind::Idle,
            &mut last_kind,
            &mut last_fingerprint,
            false,
        );

        assert_eq!(
            state
                .dsd_dop_program_idle_splice_events
                .load(Ordering::Relaxed),
            1,
            "cheap splice diagnostics still run on skipped scan callbacks"
        );
        assert_eq!(
            state.dsd_dop_marker_error_events.load(Ordering::Relaxed),
            0,
            "marker validation is part of the sampled scan"
        );
        assert_eq!(
            state
                .dsd_dop_repeated_payload_events
                .load(Ordering::Relaxed),
            0,
            "payload fingerprinting is part of the sampled scan"
        );
    }

    #[test]
    fn coreaudio_dop_diagnostics_scan_cadence_is_sampled_unless_debug_or_high_rate() {
        assert_eq!(coreaudio_dop_diagnostic_scan_interval(352_800, true), 1);
        assert_eq!(
            coreaudio_dop_diagnostic_scan_interval(352_800, false),
            COREAUDIO_DOP_DIAGNOSTIC_SCAN_INTERVAL_CALLBACKS
        );
        assert_eq!(coreaudio_dop_diagnostic_scan_interval(705_600, false), 1);

        let mut callbacks_until_scan = 0;
        assert!(coreaudio_dop_should_scan_diagnostics(
            &mut callbacks_until_scan,
            3
        ));
        assert!(!coreaudio_dop_should_scan_diagnostics(
            &mut callbacks_until_scan,
            3
        ));
        assert!(!coreaudio_dop_should_scan_diagnostics(
            &mut callbacks_until_scan,
            3
        ));
        assert!(coreaudio_dop_should_scan_diagnostics(
            &mut callbacks_until_scan,
            3
        ));

        let mut debug_callbacks_until_scan = 0;
        for _ in 0..4 {
            assert!(coreaudio_dop_should_scan_diagnostics(
                &mut debug_callbacks_until_scan,
                1
            ));
        }
    }

    #[test]
    fn coreaudio_dop_idle_to_program_restamp_keeps_marker_phase() {
        let channels = 2;
        let mut out = vec![0_i32; 8];
        let mut idle = DopIdlePattern::new();
        let mut stamper = DopMarkerStamper::new();

        idle.fill_interleaved_i32(&mut out[..4], channels);
        out[4] = 0x05aa_aa00_u32 as i32;
        out[5] = 0x05bb_bb00_u32 as i32;
        out[6] = 0x05cc_cc00_u32 as i32;
        out[7] = 0x05dd_dd00_u32 as i32;

        stamper.restamp_interleaved_i32(&mut out, channels);

        let markers: Vec<u32> = out
            .chunks(channels)
            .map(|frame| ((*frame.first().unwrap() as u32) >> 24) & 0xff)
            .collect();
        assert_eq!(markers, vec![0x05, 0xfa, 0x05, 0xfa]);
    }

    #[test]
    fn coreaudio_dop_recovery_idle_to_program_restamp_keeps_marker_phase() {
        let channels = 2;
        let mut out = vec![0_i32; 12];
        let mut idle = DopIdlePattern::new();
        let mut stamper = DopMarkerStamper::new();

        idle.fill_interleaved_i32(&mut out[..8], channels);
        out[8] = 0x05aa_aa00_u32 as i32;
        out[9] = 0x05bb_bb00_u32 as i32;
        out[10] = 0x05cc_cc00_u32 as i32;
        out[11] = 0x05dd_dd00_u32 as i32;

        stamper.restamp_interleaved_i32(&mut out, channels);

        let markers: Vec<u32> = out
            .chunks(channels)
            .map(|frame| ((*frame.first().unwrap() as u32) >> 24) & 0xff)
            .collect();
        assert_eq!(markers, vec![0x05, 0xfa, 0x05, 0xfa, 0x05, 0xfa]);
    }

    #[test]
    fn coreaudio_dop_program_recovery_program_splice_preserves_carrier_and_order() {
        fn sample(marker: u32, payload: u32) -> i32 {
            ((marker << 24) | ((payload & 0xffff) << 8)) as i32
        }

        fn payload(sample: i32) -> u32 {
            ((sample as u32) >> 8) & 0xffff
        }

        fn assert_alternating_markers(out: &[i32], channels: usize) {
            for (frame_idx, frame) in out.chunks(channels).enumerate() {
                let expected = if frame_idx % 2 == 0 { 0x05 } else { 0xfa };
                for sample in frame {
                    assert_eq!(((*sample as u32) >> 24) & 0xff, expected);
                }
            }
        }

        let channels = 2;
        let callback_samples = 6;
        let ring_capacity_samples = 64;
        let (mut prod, mut cons) = crate::audio::engine::buffers::new_dop_ring(176_400, 0);
        let mut scratch = vec![0_i32; callback_samples];
        let mut out = vec![0_i32; callback_samples];
        let mut wire = Vec::new();
        let mut idle = DopIdlePattern::new();
        let mut stamper = DopMarkerStamper::new();

        let program_a: Vec<i32> = (0..callback_samples)
            .map(|i| sample(0x05, 0x100 + i as u32))
            .collect();
        assert_eq!(prod.push_slice(&program_a), program_a.len());
        match coreaudio_dop_callback_plan(
            cons.len(),
            callback_samples,
            channels,
            0,
            false,
            ring_capacity_samples,
        ) {
            CoreAudioDopCallbackPlan::PlayProgram { samples } => {
                let read = cons.pop_slice(&mut scratch[..samples]);
                assert_eq!(read, callback_samples);
                copy_coreaudio_dop_program_or_idle(&mut out, &scratch[..read], channels, &mut idle);
            }
            other => panic!("expected program callback, got {other:?}"),
        }
        stamper.restamp_interleaved_i32(&mut out, channels);
        wire.extend_from_slice(&out);
        assert_eq!(cons.len(), 0);

        let short_program_b: Vec<i32> = (0..(callback_samples - channels))
            .map(|i| sample(0xfa, 0x200 + i as u32))
            .collect();
        assert_eq!(prod.push_slice(&short_program_b), short_program_b.len());
        match coreaudio_dop_callback_plan(
            cons.len(),
            callback_samples,
            channels,
            0,
            false,
            ring_capacity_samples,
        ) {
            CoreAudioDopCallbackPlan::RecoveryIdle {
                missing_samples,
                entering_recovery,
            } => {
                assert_eq!(missing_samples, channels);
                assert!(entering_recovery);
                idle.fill_interleaved_i32(&mut out, channels);
            }
            other => panic!("expected recovery idle, got {other:?}"),
        }
        stamper.restamp_interleaved_i32(&mut out, channels);
        wire.extend_from_slice(&out);
        assert_eq!(
            cons.len(),
            short_program_b.len(),
            "recovery idle must not consume partial program frames"
        );

        let rest_program_b: Vec<i32> = (0..(callback_samples + channels))
            .map(|i| sample(0x05, 0x300 + i as u32))
            .collect();
        assert_eq!(prod.push_slice(&rest_program_b), rest_program_b.len());
        match coreaudio_dop_callback_plan(
            cons.len(),
            callback_samples,
            channels,
            0,
            true,
            ring_capacity_samples,
        ) {
            CoreAudioDopCallbackPlan::PlayProgram { samples } => {
                let read = cons.pop_slice(&mut scratch[..samples]);
                assert_eq!(read, callback_samples);
                copy_coreaudio_dop_program_or_idle(&mut out, &scratch[..read], channels, &mut idle);
            }
            other => panic!("expected recovery to resume program, got {other:?}"),
        }
        stamper.restamp_interleaved_i32(&mut out, channels);
        wire.extend_from_slice(&out);

        assert_alternating_markers(&wire, channels);

        for (actual, expected) in wire[..callback_samples].iter().zip(&program_a) {
            assert_eq!(payload(*actual), payload(*expected));
        }
        for sample in &wire[callback_samples..callback_samples * 2] {
            assert_eq!(payload(*sample), 0x6969);
        }
        let expected_resume: Vec<i32> = short_program_b
            .iter()
            .chain(rest_program_b.iter())
            .take(callback_samples)
            .copied()
            .collect();
        for (actual, expected) in wire[callback_samples * 2..callback_samples * 3]
            .iter()
            .zip(expected_resume)
        {
            assert_eq!(payload(*actual), payload(expected));
        }
    }

    #[test]
    fn coreaudio_dop_non_playing_states_emit_idle_only() {
        let channels = 2;
        for state in [PLAYBACK_PAUSED, PLAYBACK_STARTING, PLAYBACK_STOPPED] {
            let mut out = vec![0x1111_i32; 8];
            let mut idle = DopIdlePattern::new();

            idle.fill_interleaved_i32(&mut out, channels);

            for sample in &out {
                assert_eq!(
                    ((*sample as u32) >> 8) & 0xffff,
                    0x6969,
                    "state {state} should output DoP idle payload only"
                );
            }
        }
    }

    #[test]
    fn coreaudio_dop_misaligned_samples_tracks_frame_remainder() {
        assert_eq!(coreaudio_dop_misaligned_samples(0, 2), 0);
        assert_eq!(coreaudio_dop_misaligned_samples(1, 2), 1);
        assert_eq!(coreaudio_dop_misaligned_samples(2, 2), 0);
        assert_eq!(coreaudio_dop_misaligned_samples(3, 2), 1);
        assert_eq!(coreaudio_dop_misaligned_samples(5, 1), 0);
    }

    #[test]
    fn coreaudio_dop_discard_misaligned_samples_drops_lone_remainder() {
        let (mut prod, mut cons) = crate::audio::engine::buffers::new_dop_ring(176_400, 0);
        assert_eq!(prod.push_slice(&[0x1234]), 1);

        assert_eq!(discard_coreaudio_dop_misaligned_samples(&mut cons, 2), 1);

        assert_eq!(cons.len(), 0);

        assert_eq!(prod.push_slice(&[0x1111, 0x2222]), 2);
        assert_eq!(discard_coreaudio_dop_misaligned_samples(&mut cons, 2), 0);
        assert_eq!(cons.len(), 2);
    }

    #[test]
    fn coreaudio_dop_alignment_diagnostics_count_flush_and_discard() {
        let state = AtomicPlayerState::new();

        record_coreaudio_dop_alignment_reset(&state);
        record_coreaudio_dop_alignment_discard(&state, 0);
        record_coreaudio_dop_alignment_discard(&state, 1);

        assert_eq!(state.dop_alignment_reset_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn coreaudio_dop_idle_health_does_not_lower_playback_watermark() {
        let state = AtomicPlayerState::new();
        state
            .dsd_ring_low_watermark_samples
            .store(u64::MAX, Ordering::Relaxed);

        publish_coreaudio_dop_buffer_health(&state, 1_411_200, 0, 8192, 8192, 705_600, false);

        assert_eq!(
            state.dsd_ring_fill_samples.load(Ordering::Relaxed),
            0,
            "idle health still reports current fill"
        );
        assert_eq!(
            state.dsd_ring_low_watermark_samples.load(Ordering::Relaxed),
            u64::MAX,
            "startup/paused idle callbacks must not become playback low-watermark"
        );
    }

    #[test]
    fn coreaudio_dop_playing_health_tracks_playback_watermark() {
        let state = AtomicPlayerState::new();
        state
            .dsd_ring_low_watermark_samples
            .store(u64::MAX, Ordering::Relaxed);

        publish_coreaudio_dop_buffer_health(&state, 1_411_200, 705_600, 8192, 8192, 705_600, true);
        publish_coreaudio_dop_buffer_health(&state, 1_411_200, 352_800, 8192, 8192, 705_600, true);
        publish_coreaudio_dop_buffer_health(&state, 1_411_200, 529_200, 8192, 8192, 705_600, true);

        assert_eq!(
            state.dsd_ring_low_watermark_samples.load(Ordering::Relaxed),
            352_800
        );
    }
}

#[cfg(target_os = "macos")]
fn record_coreaudio_device_buffer_frame_range(
    state: &AtomicPlayerState,
    device_id: coreaudio_sys::AudioDeviceID,
) -> Option<(u32, u32)> {
    match current_coreaudio_device_buffer_frame_range(device_id) {
        Ok(range) => {
            let (min, max) = coreaudio_buffer_frame_range_bounds(range);
            state
                .dsd_hardware_buffer_min_frames
                .store(min, Ordering::Relaxed);
            state
                .dsd_hardware_buffer_max_frames
                .store(max, Ordering::Relaxed);
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: CoreAudio DoP device buffer frame range min={} max={}",
                    min, max
                );
            }
            Some((min, max))
        }
        Err(e) => {
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: CoreAudio DoP device buffer frame range read failed: {e}"
                );
            }
            None
        }
    }
}

#[cfg(target_os = "macos")]
fn configure_coreaudio_device_output_buffer_frames(
    device_id: coreaudio_sys::AudioDeviceID,
    requested_frames: u32,
) -> Result<u32, String> {
    use coreaudio_sys::{
        AudioObjectHasProperty, AudioObjectIsPropertySettable, AudioObjectSetPropertyData, Boolean,
        kAudioDevicePropertyBufferFrameSize,
    };
    use std::{mem, ptr};

    let address = coreaudio_device_buffer_address(kAudioDevicePropertyBufferFrameSize);
    if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
        return Err("device does not expose kAudioDevicePropertyBufferFrameSize".to_string());
    }

    let mut frames = requested_frames.max(1);
    match current_coreaudio_device_buffer_frame_range(device_id) {
        Ok(range) => {
            let (min, max) = coreaudio_buffer_frame_range_bounds(range);
            frames = requested_frames.clamp(min, max);
        }
        Err(e) => {
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!("AudioWorker DEBUG: CoreAudio DoP direct buffer range read failed: {e}");
            }
        }
    }

    let mut settable: Boolean = 0;
    let status = unsafe { AudioObjectIsPropertySettable(device_id, &address, &mut settable) };
    if status != 0 {
        return Err(format!(
            "settable check failed for kAudioDevicePropertyBufferFrameSize: OSStatus {status}"
        ));
    }
    if settable == 0 {
        return Err("kAudioDevicePropertyBufferFrameSize is not settable".to_string());
    }

    let status = unsafe {
        AudioObjectSetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            mem::size_of::<u32>() as u32,
            &frames as *const _ as *const libc::c_void,
        )
    };
    if status != 0 {
        return Err(format!(
            "set failed for kAudioDevicePropertyBufferFrameSize={frames}: OSStatus {status}"
        ));
    }

    current_coreaudio_device_output_buffer_frames(device_id).or(Ok(frames))
}

#[cfg(target_os = "macos")]
fn coreaudio_buffer_frame_range_bounds(range: coreaudio_sys::AudioValueRange) -> (u32, u32) {
    let min = range.mMinimum.ceil().max(1.0) as u32;
    let max = range.mMaximum.floor().max(min as f64) as u32;
    (min, max)
}

#[cfg(target_os = "macos")]
fn current_coreaudio_device_buffer_frame_range(
    device_id: coreaudio_sys::AudioDeviceID,
) -> Result<coreaudio_sys::AudioValueRange, String> {
    use coreaudio_sys::{
        AudioObjectGetPropertyData, AudioObjectHasProperty, AudioObjectPropertyAddress,
        AudioValueRange, kAudioDevicePropertyBufferFrameSizeRange,
        kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeGlobal,
    };
    use std::{mem, ptr};

    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyBufferFrameSizeRange,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };
    if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
        return Err("device does not expose kAudioDevicePropertyBufferFrameSizeRange".to_string());
    }

    let mut range = AudioValueRange {
        mMinimum: 0.0,
        mMaximum: 0.0,
    };
    let mut size = mem::size_of::<AudioValueRange>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut range as *mut _ as *mut libc::c_void,
        )
    };
    if status == 0 {
        Ok(range)
    } else {
        Err(format!(
            "read failed for kAudioDevicePropertyBufferFrameSizeRange: OSStatus {status}"
        ))
    }
}

#[cfg(target_os = "macos")]
fn current_coreaudio_device_output_buffer_frames(
    device_id: coreaudio_sys::AudioDeviceID,
) -> Result<u32, String> {
    use coreaudio_sys::{
        AudioObjectGetPropertyData, AudioObjectHasProperty, kAudioDevicePropertyBufferFrameSize,
    };
    use std::{mem, ptr};

    let address = coreaudio_device_buffer_address(kAudioDevicePropertyBufferFrameSize);
    if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
        return Err("device does not expose kAudioDevicePropertyBufferFrameSize".to_string());
    }

    let mut frames = 0_u32;
    let mut size = mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut frames as *mut _ as *mut libc::c_void,
        )
    };
    if status == 0 {
        Ok(frames)
    } else {
        Err(format!(
            "read failed for kAudioDevicePropertyBufferFrameSize: OSStatus {status}"
        ))
    }
}

#[cfg(target_os = "macos")]
fn coreaudio_device_buffer_address(
    selector: coreaudio_sys::AudioObjectPropertySelector,
) -> coreaudio_sys::AudioObjectPropertyAddress {
    coreaudio_sys::AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: coreaudio_sys::kAudioObjectPropertyScopeGlobal,
        mElement: coreaudio_sys::kAudioObjectPropertyElementMaster,
    }
}

#[cfg(target_os = "macos")]
fn configure_coreaudio_output_buffer_frames(
    audio_unit: &mut coreaudio::audio_unit::AudioUnit,
    requested_frames: u32,
) -> Result<u32, coreaudio::Error> {
    use coreaudio::audio_unit::{Element, Scope};
    use coreaudio_sys::{
        AudioValueRange, kAudioDevicePropertyBufferFrameSize,
        kAudioDevicePropertyBufferFrameSizeRange,
    };

    let range: AudioValueRange = audio_unit.get_property(
        kAudioDevicePropertyBufferFrameSizeRange,
        Scope::Global,
        Element::Output,
    )?;
    let min = range.mMinimum.ceil().max(1.0) as u32;
    let max = range.mMaximum.floor().max(min as f64) as u32;
    let frames = requested_frames.clamp(min, max);
    audio_unit.set_property(
        kAudioDevicePropertyBufferFrameSize,
        Scope::Global,
        Element::Output,
        Some(&frames),
    )?;
    audio_unit
        .get_property(
            kAudioDevicePropertyBufferFrameSize,
            Scope::Global,
            Element::Output,
        )
        .or(Ok(frames))
}

#[cfg(target_os = "macos")]
fn current_coreaudio_output_buffer_frames(
    audio_unit: &mut coreaudio::audio_unit::AudioUnit,
) -> Result<u32, coreaudio::Error> {
    use coreaudio::audio_unit::{Element, Scope};
    use coreaudio_sys::kAudioDevicePropertyBufferFrameSize;

    audio_unit.get_property(
        kAudioDevicePropertyBufferFrameSize,
        Scope::Global,
        Element::Output,
    )
}

#[cfg(target_os = "macos")]
fn publish_coreaudio_dop_buffer_health(
    state: &AtomicPlayerState,
    ring_capacity_samples: usize,
    ring_fill_samples: usize,
    callback_frames: usize,
    hardware_buffer_frames: u32,
    dop_frame_rate: u32,
    track_low_watermark: bool,
) {
    let fill = ring_fill_samples as u64;
    state
        .dsd_ring_capacity_samples
        .store(ring_capacity_samples as u64, Ordering::Relaxed);
    state.dsd_ring_fill_samples.store(fill, Ordering::Relaxed);
    state
        .dsd_callback_frames
        .store(callback_frames as u32, Ordering::Relaxed);
    state
        .dsd_hardware_buffer_frames
        .store(hardware_buffer_frames, Ordering::Relaxed);
    state.record_startup_ring_fill(fill, dop_frame_rate.max(1) as u64 * 2);

    if track_low_watermark {
        let mut low = state.dsd_ring_low_watermark_samples.load(Ordering::Relaxed);
        while fill < low {
            match state.dsd_ring_low_watermark_samples.compare_exchange_weak(
                low,
                fill,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => low = next,
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_underrun(state: &AtomicPlayerState, missing_samples: u64, lock_miss: bool) {
    state.underrun_events.fetch_add(1, Ordering::Relaxed);
    state
        .underrun_samples
        .fetch_add(missing_samples, Ordering::Relaxed);
    if lock_miss {
        state.dsd_lock_miss_events.fetch_add(1, Ordering::Relaxed);
    }
    state
        .dsd_last_underrun_at_ms
        .store(unix_epoch_millis(), Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_soft_callback_gap(
    state: &AtomicPlayerState,
    gap_ns: u64,
    callback_frames: usize,
    dop_frame_rate: u32,
) {
    if callback_frames == 0 || dop_frame_rate == 0 {
        return;
    }
    let expected_ns =
        (callback_frames as u128 * 1_000_000_000_u128 / dop_frame_rate as u128) as u64;
    let threshold_125_ns = expected_ns.saturating_mul(125) / 100;
    if gap_ns <= threshold_125_ns {
        return;
    }

    let now_ms = unix_epoch_millis();
    state
        .dsd_soft_callback_gap_125_events
        .fetch_add(1, Ordering::Relaxed);
    if gap_ns > expected_ns.saturating_mul(150) / 100 {
        state
            .dsd_soft_callback_gap_150_events
            .fetch_add(1, Ordering::Relaxed);
    }
    if gap_ns > expected_ns.saturating_mul(175) / 100 {
        state
            .dsd_soft_callback_gap_175_events
            .fetch_add(1, Ordering::Relaxed);
    }
    state
        .dsd_last_soft_callback_gap_ns
        .store(gap_ns, Ordering::Relaxed);
    state
        .dsd_last_soft_callback_gap_at_ms
        .store(now_ms, Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_ring_pressure(
    state: &AtomicPlayerState,
    ring_fill_samples: usize,
    callback_samples: usize,
    channels: usize,
    dop_frame_rate: u32,
    pressure: &mut CoreAudioDopRingPressureState,
) {
    if channels == 0 || dop_frame_rate == 0 {
        return;
    }
    let samples_per_second = dop_frame_rate as u64 * channels.max(1) as u64;
    let fill = ring_fill_samples as u64;
    let callback_threshold = callback_samples as u64;
    let thresholds = [
        (
            samples_per_second.saturating_mul(250) / 1000,
            &mut pressure.below_250ms,
            &state.dsd_ring_below_250ms_events,
        ),
        (
            samples_per_second.saturating_mul(100) / 1000,
            &mut pressure.below_100ms,
            &state.dsd_ring_below_100ms_events,
        ),
        (
            samples_per_second.saturating_mul(50) / 1000,
            &mut pressure.below_50ms,
            &state.dsd_ring_below_50ms_events,
        ),
        (
            callback_threshold,
            &mut pressure.below_callback,
            &state.dsd_ring_below_callback_events,
        ),
    ];

    let mut pressure_seen = false;
    for (threshold, active, counter) in thresholds {
        let below = threshold > 0 && fill <= threshold;
        if below && !*active {
            counter.fetch_add(1, Ordering::Relaxed);
            pressure_seen = true;
        }
        *active = below;
    }
    if pressure_seen {
        state
            .dsd_last_ring_pressure_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
    }
}

#[cfg(target_os = "macos")]
fn record_coreaudio_dop_callback_deadline_miss(
    state: &AtomicPlayerState,
    gap_ns: u64,
    callback_frames: usize,
    channels: usize,
    dop_frame_rate: u32,
) {
    if callback_frames == 0 || dop_frame_rate == 0 {
        return;
    }
    let expected_ns =
        (callback_frames as u128 * 1_000_000_000_u128 / dop_frame_rate as u128) as u64;
    let threshold_ns = expected_ns.saturating_mul(2);
    if gap_ns <= threshold_ns {
        return;
    }

    let excess_ns = gap_ns.saturating_sub(expected_ns);
    let estimated_missing_frames =
        (excess_ns as u128 * dop_frame_rate as u128 / 1_000_000_000_u128) as u64;
    let missing_samples = estimated_missing_frames
        .saturating_mul(channels.max(1) as u64)
        .max((callback_frames * channels.max(1)) as u64);

    state
        .dsd_callback_deadline_miss_events
        .fetch_add(1, Ordering::Relaxed);
    state.underrun_events.fetch_add(1, Ordering::Relaxed);
    state
        .underrun_samples
        .fetch_add(missing_samples, Ordering::Relaxed);
    state
        .dsd_last_underrun_at_ms
        .store(unix_epoch_millis(), Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn unix_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn publish_pcm_buffer_health(
    state: &AtomicPlayerState,
    ring_capacity_samples: usize,
    ring_fill_samples: usize,
    callback_frames: usize,
    track_low_watermark: bool,
) {
    let fill = ring_fill_samples as u64;
    state
        .pcm_ring_capacity_samples
        .store(ring_capacity_samples as u64, Ordering::Relaxed);
    state.pcm_ring_fill_samples.store(fill, Ordering::Relaxed);
    state
        .pcm_callback_frames
        .store(callback_frames as u32, Ordering::Relaxed);
    state.record_startup_ring_fill(
        fill,
        state.target_rate.load(Ordering::Relaxed).max(1) as u64 * 2,
    );

    if track_low_watermark {
        let mut low = state.pcm_ring_low_watermark_samples.load(Ordering::Relaxed);
        while fill < low {
            match state.pcm_ring_low_watermark_samples.compare_exchange_weak(
                low,
                fill,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => low = next,
            }
        }
    }
}

fn record_pcm_underrun(state: &AtomicPlayerState, missing_samples: u64, lock_miss: bool) {
    let previous = state.underrun_events.fetch_add(1, Ordering::Relaxed);
    state
        .underrun_samples
        .fetch_add(missing_samples, Ordering::Relaxed);
    if lock_miss {
        state.pcm_lock_miss_events.fetch_add(1, Ordering::Relaxed);
    }
    if previous == 0 || (previous + 1).is_power_of_two() {
        eprintln!(
            "AudioWorker: PCM underrun #{}{}, missing {} samples",
            previous + 1,
            if lock_miss {
                " (callback lock miss)"
            } else {
                ""
            },
            missing_samples,
        );
    }
}

#[cfg(target_os = "macos")]
fn record_pcm_underrun_rt(state: &AtomicPlayerState, missing_samples: u64) {
    state.underrun_events.fetch_add(1, Ordering::Relaxed);
    state
        .underrun_samples
        .fetch_add(missing_samples, Ordering::Relaxed);
}

fn build_stream_helper(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
) -> Result<cpal::Stream, (Box<dyn std::error::Error>, AudioConsumer)> {
    let ring_capacity_samples = cons.len() + cons.free_len();
    publish_pcm_buffer_health(&state, ring_capacity_samples, cons.len(), 0, false);
    state
        .pcm_ring_low_watermark_samples
        .store(u64::MAX, Ordering::Relaxed);
    let cons_cell = Arc::new(std::sync::Mutex::new(Some(cons)));
    let cons_cell_clone = Arc::clone(&cons_cell);
    let callback_channels = usize::from(config.channels.max(1));
    // CPAL's default buffer size may vary between callbacks. Size scratch from
    // the much larger output ring before the callback starts so the real-time
    // path never has to grow it.
    let mut scratch = vec![0.0_f64; ring_capacity_samples];
    let mut ramp = PcmTransitionRamp::new(callback_channels);

    let stream_res = device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
            let is_playing = state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING;

            let lock_start = Instant::now();
            let lock_result = cons_cell_clone.try_lock();
            state.record_lock_wait_ns(lock_start.elapsed().as_nanos() as u64);
            if let Ok(mut guard) = lock_result {
                if let Some(ref mut c) = *guard {
                    if state.flush_buffer.swap(false, Ordering::Relaxed) {
                        c.clear();
                    }

                    let mut max_l = 0.0f64;
                    let mut max_r = 0.0f64;
                    if data.len() > scratch.len() {
                        if is_playing {
                            record_pcm_underrun(&state, data.len() as u64, false);
                        }
                        data.fill(0.0);
                        state.meter_l.store(0.0_f32.to_bits(), Ordering::Relaxed);
                        state.meter_r.store(0.0_f32.to_bits(), Ordering::Relaxed);
                        return;
                    }
                    let scratch = &mut scratch[..data.len()];
                    scratch.fill(0.0);
                    publish_pcm_buffer_health(
                        &state,
                        ring_capacity_samples,
                        c.len(),
                        data.len() / 2,
                        is_playing,
                    );

                    if is_playing {
                        // Pop whole stereo frames only: an odd-count pop would orphan
                        // one channel's sample and permanently swap L/R from then on.
                        let aligned = (c.len().min(scratch.len()) / 2) * 2;
                        let read = c.pop_slice(&mut scratch[..aligned]);
                        if read < data.len() {
                            record_pcm_underrun(&state, (data.len() - read) as u64, false);
                        }

                        ramp.process(scratch, data.len(), callback_channels, true);
                        for i in 0..data.len() {
                            let val = scratch[i] * volume;
                            data[i] = val as f32;

                            if i % 2 == 0 {
                                max_l = max_l.max(val.abs());
                            } else {
                                max_r = max_r.max(val.abs());
                            }
                        }

                        let played_frames = (read / 2) as u64;
                        state
                            .position_samples
                            .fetch_add(played_frames, Ordering::Relaxed);
                    } else {
                        ramp.process(scratch, 0, callback_channels, false);
                        for i in 0..data.len() {
                            data[i] = (scratch[i] * volume) as f32;
                        }
                    }

                    state
                        .meter_l
                        .store((max_l as f32).to_bits(), Ordering::Relaxed);
                    state
                        .meter_r
                        .store((max_r as f32).to_bits(), Ordering::Relaxed);
                }
            } else {
                if is_playing {
                    record_pcm_underrun(&state, data.len() as u64, true);
                }
                for x in data.iter_mut() {
                    *x = 0.0;
                }
            }
        },
        move |err| {
            eprintln!("AudioWorker: Output stream error: {:?}", err);
        },
        None,
    );

    match stream_res {
        Ok(stream) => Ok(stream),
        Err(err) => {
            let cons_returned = cons_cell.lock().unwrap().take().unwrap();
            Err((err.into(), cons_returned))
        }
    }
}
