//! WASAPI exclusive-mode output backend (Windows only).
//!
//! Mirrors the role of `coreaudio_hog` on macOS: provides bit-perfect, rate-exact output
//! by taking exclusive control of the audio endpoint via IAudioClient + event-driven render.
//!
//! CPAL's WASAPI host runs in shared mode only. When the user toggles the exclusive switch
//! on Windows, the player routes through this backend instead of CPAL.

use crate::audio::dsd::dop::{DopIdlePattern, DopMarkerStamper};
use crate::audio::dsp::dither::{DitherPreference, DitherState};
use crate::audio::engine::output_ramp::PcmTransitionRamp;
use crate::audio::engine::player::AtomicPlayerState;
use crate::audio::output::sample_format::{
    OutputSampleFormat, OutputSampleType, encode_interleaved_f64,
    encode_interleaved_i32_passthrough,
};
use ringbuf::{Consumer, SharedRb};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wasapi::{
    AudioClient, Direction, SampleType, StreamMode, WaveFormat, calculate_period_100ns,
    initialize_mta,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentThread,
    SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows::core::w;

type AudioConsumer = Consumer<f64, Arc<SharedRb<f64, Vec<MaybeUninit<f64>>>>>;
pub type DopConsumer = Consumer<i32, Arc<SharedRb<i32, Vec<MaybeUninit<i32>>>>>;

pub struct ExclusiveStream {
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for ExclusiveStream {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub struct MmcssGuard {
    handle: HANDLE,
}

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.handle);
        }
    }
}

pub fn boost_current_thread_for_audio(label: &str) -> Option<MmcssGuard> {
    let mut task_index = 0;
    match unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) } {
        Ok(handle) => {
            unsafe {
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
            }
            println!("{label}: MMCSS Pro Audio priority enabled.");
            Some(MmcssGuard { handle })
        }
        Err(e) => {
            eprintln!("{label}: Failed to enable MMCSS Pro Audio priority: {e:?}");
            None
        }
    }
}

/// Open a WASAPI exclusive stream at `target_rate`.
///
/// On failure the consumer is returned so the caller can fall back to a different backend.
pub fn open(
    device_name: Option<&str>,
    target_rate: u32,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
) -> Result<(ExclusiveStream, u32), (Box<dyn std::error::Error>, AudioConsumer)> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = Arc::clone(&shutdown);

    let cons_slot: Arc<Mutex<Option<AudioConsumer>>> = Arc::new(Mutex::new(Some(cons)));
    let cons_slot_thread = Arc::clone(&cons_slot);

    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let device_name_owned = device_name.map(|s| s.to_string());

    let handle = thread::Builder::new()
        .name("WasapiExclusive".to_string())
        .spawn(move || {
            render_thread(
                device_name_owned,
                target_rate,
                state,
                shutdown_thread,
                cons_slot_thread,
                tx,
            );
        })
        .map_err(|e| -> (Box<dyn std::error::Error>, AudioConsumer) {
            let cons = cons_slot.lock().unwrap().take().expect("consumer present");
            (Box::new(e), cons)
        })?;

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(actual_rate)) => Ok((
            ExclusiveStream {
                shutdown,
                handle: Some(handle),
            },
            actual_rate,
        )),
        Ok(Err(msg)) => {
            let _ = handle.join();
            let cons = cons_slot
                .lock()
                .unwrap()
                .take()
                .expect("consumer must remain on init failure");
            Err((msg.into(), cons))
        }
        Err(e) => {
            shutdown.store(true, Ordering::Relaxed);
            let _ = handle.join();
            let cons = cons_slot
                .lock()
                .unwrap()
                .take()
                .expect("consumer must remain on init failure");
            Err((format!("WASAPI init timed out: {e}").into(), cons))
        }
    }
}

fn render_thread(
    device_name: Option<String>,
    target_rate: u32,
    state: Arc<AtomicPlayerState>,
    shutdown: Arc<AtomicBool>,
    cons_slot: Arc<Mutex<Option<AudioConsumer>>>,
    init_tx: std::sync::mpsc::Sender<Result<u32, String>>,
) {
    let _mmcss_guard = boost_current_thread_for_audio("WasapiExclusive");

    // initialize_mta returns HRESULT; non-zero severity bit means failure.
    let hr = initialize_mta();
    if hr.is_err() {
        let _ = init_tx.send(Err(format!("CoInitializeEx(MTA) failed: HRESULT {hr:?}")));
        return;
    }

    let device = match resolve_device(device_name.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    let mut audio_client = match device.get_iaudioclient() {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_iaudioclient failed: {e:?}")));
            return;
        }
    };

    // Probe a supported exclusive-mode format at the target rate.
    // Preference: Float32 → Int32 (24-in-32) → Int24 packed → Int16.
    let format = match probe_format(&audio_client, target_rate) {
        Some(f) => f,
        None => {
            let _ = init_tx.send(Err(format!(
                "No supported exclusive-mode format at {target_rate}Hz"
            )));
            return;
        }
    };

    let blockalign = format.get_blockalign() as usize;
    let valid_bits = format.get_validbitspersample() as usize;
    let sample_type = format.get_subformat().unwrap_or(SampleType::Float);
    let actual_rate = format.get_samplespersec();

    state
        .target_bits
        .store(valid_bits as u32, Ordering::Relaxed);

    let (_def_period, min_period) = match audio_client.get_device_period() {
        Ok(p) => p,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_device_period failed: {e:?}")));
            return;
        }
    };

    let requested_period_hns = 100_000i64.max(min_period * 4);
    let desired_period = match audio_client.calculate_aligned_period_near(
        requested_period_hns,
        Some(512),
        &format,
    ) {
        Ok(p) => p,
        Err(e) => {
            let _ = init_tx.send(Err(format!("calculate_aligned_period_near failed: {e:?}")));
            return;
        }
    };

    let mode = StreamMode::EventsExclusive {
        period_hns: desired_period,
    };

    // Initialize. Handle the buffer-alignment recovery dance from the WASAPI docs.
    if let Err(e) = audio_client.initialize_client(&format, &Direction::Render, &mode) {
        if let wasapi::WasapiError::Windows(werr) = &e {
            if werr.code() == windows::Win32::Media::Audio::AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
                let new_size = match audio_client.get_buffer_size() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = init_tx.send(Err(format!("get_buffer_size failed: {e:?}")));
                        return;
                    }
                };
                let aligned_period =
                    calculate_period_100ns(new_size as i64, format.get_samplespersec() as i64);
                audio_client = match device.get_iaudioclient() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(format!("get_iaudioclient retry failed: {e:?}")));
                        return;
                    }
                };
                let retry_mode = StreamMode::EventsExclusive {
                    period_hns: aligned_period,
                };
                if let Err(e2) =
                    audio_client.initialize_client(&format, &Direction::Render, &retry_mode)
                {
                    let _ = init_tx.send(Err(format!("Retry initialize_client failed: {e2:?}")));
                    return;
                }
            } else {
                let _ = init_tx.send(Err(format!("initialize_client failed: {e:?}")));
                return;
            }
        } else {
            let _ = init_tx.send(Err(format!("initialize_client failed: {e:?}")));
            return;
        }
    }

    let h_event = match audio_client.set_get_eventhandle() {
        Ok(h) => h,
        Err(e) => {
            let _ = init_tx.send(Err(format!("set_get_eventhandle failed: {e:?}")));
            return;
        }
    };

    let render_client = match audio_client.get_audiorenderclient() {
        Ok(rc) => rc,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_audiorenderclient failed: {e:?}")));
            return;
        }
    };

    // Cache the negotiated exclusive-mode buffer size so we can sanity-check the per-cycle
    // available-frame count. Some USB drivers under stress have been observed to return
    // padding values that underflow get_available_space_in_frames.
    let buffer_size_frames = match audio_client.get_buffer_size() {
        Ok(n) => n,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_buffer_size failed: {e:?}")));
            return;
        }
    };

    if let Err(e) = audio_client.start_stream() {
        let _ = init_tx.send(Err(format!("start_stream failed: {e:?}")));
        return;
    }

    // Take ownership of the consumer now that init succeeded.
    let mut cons = match cons_slot.lock().unwrap().take() {
        Some(c) => c,
        None => {
            let _ = init_tx.send(Err("consumer was already taken".into()));
            return;
        }
    };

    // Signal success to caller.
    if init_tx.send(Ok(actual_rate)).is_err() {
        // Caller went away; release.
        let _ = audio_client.stop_stream();
        return;
    }

    println!(
        "WasapiExclusive: render loop start — rate={}Hz buffer={} frames period={}hns blockalign={} valid_bits={} sample_type={:?}",
        actual_rate, buffer_size_frames, desired_period, blockalign, valid_bits, sample_type,
    );

    // Render loop.
    let mut scratch_f64: Vec<f64> = Vec::new();
    let mut scratch_bytes: Vec<u8> = Vec::new();
    let channels = format.get_nchannels() as usize;
    let device_format = OutputSampleFormat {
        sample_type: output_sample_type(sample_type),
        valid_bits,
        bytes_per_sample: blockalign / channels,
        channels,
    };
    let mut dither_state = DitherState::new(make_dither_seed(actual_rate, buffer_size_frames));
    let mut event_failures = 0u32;
    let mut ramp = PcmTransitionRamp::new(channels);

    while !shutdown.load(Ordering::Relaxed) {
        let raw_available = match audio_client.get_available_space_in_frames() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("WasapiExclusive: get_available_space_in_frames error: {e:?}");
                break;
            }
        };

        // Defensive clamp: in pathological driver states this can return a value larger
        // than the actual buffer (the underlying GetCurrentPadding can briefly desync).
        // Trusting that blindly would make us tell WASAPI we have N frames to render and
        // then write into a buffer pointer that's smaller than expected.
        if raw_available > buffer_size_frames {
            eprintln!(
                "WasapiExclusive: clamping available {} > buffer_size {}",
                raw_available, buffer_size_frames,
            );
        }
        let available = raw_available.min(buffer_size_frames) as usize;

        if available > 0 {
            let needed_samples = available * channels;
            scratch_f64.clear();
            scratch_f64.resize(needed_samples, 0.0);

            // Pull from the f64 ring buffer; underrun -> silence (already zeroed).
            let is_playing = state.state.load(Ordering::Relaxed) == 1;
            if state.flush_buffer.swap(false, Ordering::Relaxed) {
                cons.clear();
            }

            // Pop whole frames only: an odd-count pop would orphan one channel's
            // sample in the ring and permanently swap L/R from then on.
            let read = if is_playing {
                let aligned = (cons.len().min(needed_samples) / channels) * channels;
                cons.pop_slice(&mut scratch_f64[..aligned])
            } else {
                0
            };
            if is_playing && read < needed_samples {
                let missing = (needed_samples - read) as u64;
                let previous = state.underrun_events.fetch_add(1, Ordering::Relaxed);
                state.underrun_samples.fetch_add(missing, Ordering::Relaxed);
                if previous == 0 || (previous + 1).is_power_of_two() {
                    eprintln!(
                        "WasapiExclusive: underrun #{}, missing {} samples",
                        previous + 1,
                        missing,
                    );
                }
            }
            ramp.process(&mut scratch_f64, needed_samples, channels, is_playing);

            // Apply volume, accumulate meters.
            let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
            let mut max_l = 0.0f64;
            let mut max_r = 0.0f64;
            for i in 0..scratch_f64.len() {
                let v = scratch_f64[i] * volume;
                scratch_f64[i] = v;
                if channels >= 2 {
                    if i % 2 == 0 {
                        max_l = max_l.max(v.abs());
                    } else {
                        max_r = max_r.max(v.abs());
                    }
                } else {
                    max_l = max_l.max(v.abs());
                    max_r = max_l;
                }
            }
            state
                .meter_l
                .store((max_l as f32).to_bits(), Ordering::Relaxed);
            state
                .meter_r
                .store((max_r as f32).to_bits(), Ordering::Relaxed);

            if is_playing {
                let played_frames = (read / channels) as u64;
                state
                    .position_samples
                    .fetch_add(played_frames, Ordering::Relaxed);
            }

            // Encode into device format.
            scratch_bytes.clear();
            scratch_bytes.resize(available * blockalign, 0);
            encode_interleaved_f64(
                &scratch_f64,
                &mut scratch_bytes,
                device_format,
                DitherPreference::from_id(state.dither_mode.load(Ordering::Relaxed))
                    .unwrap_or(DitherPreference::Auto),
                &mut dither_state,
            );

            if let Err(e) = render_client.write_to_device(available, &scratch_bytes, None) {
                eprintln!("WasapiExclusive: write_to_device error: {e:?}");
                break;
            }
        }

        // Wait for the device event (timeout in ms). Spurious timeouts are fine — just loop.
        if h_event.wait_for_event(200).is_err() {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            // A persistently broken event handle would otherwise busy-spin this loop.
            event_failures += 1;
            if event_failures >= 50 {
                eprintln!(
                    "WasapiExclusive: {event_failures} consecutive event waits failed; stopping render loop"
                );
                break;
            }
            continue;
        }
        event_failures = 0;
    }

    let _ = audio_client.stop_stream();
}

fn make_dither_seed(actual_rate: u32, buffer_size_frames: u32) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x517c_c1b7_2722_0a95);
    now ^ ((actual_rate as u64) << 32) ^ buffer_size_frames as u64
}

fn output_sample_type(sample_type: SampleType) -> OutputSampleType {
    match sample_type {
        SampleType::Float => OutputSampleType::Float,
        SampleType::Int => OutputSampleType::Int,
    }
}

fn resolve_device(name: Option<&str>) -> Result<wasapi::Device, String> {
    let enumerator = wasapi::DeviceEnumerator::new()
        .map_err(|e| format!("DeviceEnumerator::new failed: {e:?}"))?;
    match name {
        None => enumerator
            .get_default_device(&Direction::Render)
            .map_err(|e| format!("get_default_device failed: {e:?}")),
        Some(target) => {
            let collection = enumerator
                .get_device_collection(&Direction::Render)
                .map_err(|e| format!("get_device_collection failed: {e:?}"))?;
            let count = collection
                .get_nbr_devices()
                .map_err(|e| format!("get_nbr_devices failed: {e:?}"))?;
            for i in 0..count {
                if let Ok(dev) = collection.get_device_at_index(i) {
                    if let Ok(n) = dev.get_friendlyname() {
                        if n.trim() == target.trim() {
                            return Ok(dev);
                        }
                    }
                }
            }
            Err(format!("Device '{target}' not found"))
        }
    }
}

fn probe_format(client: &AudioClient, rate: u32) -> Option<WaveFormat> {
    let channels = 2;
    let candidates = [
        (32, 32, SampleType::Float),
        (32, 32, SampleType::Int),
        (32, 24, SampleType::Int),
        (24, 24, SampleType::Int),
        (16, 16, SampleType::Int),
    ];
    for (storage_bits, valid_bits, ty) in candidates {
        let fmt = WaveFormat::new(storage_bits, valid_bits, &ty, rate as usize, channels, None);
        if let Ok(adjusted) = client.is_supported_exclusive_with_quirks(&fmt) {
            return Some(adjusted);
        }
    }
    None
}

/// Open a WASAPI exclusive stream that carries DSD-over-PCM (DoP) at `dop_frame_rate`.
///
/// DSD128 → 352.8 kHz, DSD256 → 705.6 kHz. The endpoint must accept Int24 in either a
/// 32-bit container or packed-24 layout at that rate — if it doesn't, this returns an
/// error rather than falling back to a lossy format (DoP markers are not survivable
/// through dither/quantize, so a format mismatch silently produces garbage).
///
/// The consumer carries pre-packed DoP samples (24-bit values in `i32` containers, marker
/// in the top 8 bits). Bytes are copied to the device buffer with no further processing.
pub fn open_dop(
    device_name: Option<&str>,
    dop_frame_rate: u32,
    cons: DopConsumer,
    state: Arc<AtomicPlayerState>,
) -> Result<(ExclusiveStream, u32), (Box<dyn std::error::Error>, DopConsumer)> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = Arc::clone(&shutdown);

    let cons_slot: Arc<Mutex<Option<DopConsumer>>> = Arc::new(Mutex::new(Some(cons)));
    let cons_slot_thread = Arc::clone(&cons_slot);

    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let device_name_owned = device_name.map(|s| s.to_string());

    let handle = thread::Builder::new()
        .name("WasapiExclusiveDop".to_string())
        .spawn(move || {
            render_thread_dop(
                device_name_owned,
                dop_frame_rate,
                state,
                shutdown_thread,
                cons_slot_thread,
                tx,
            );
        })
        .map_err(|e| -> (Box<dyn std::error::Error>, DopConsumer) {
            let cons = cons_slot.lock().unwrap().take().expect("consumer present");
            (Box::new(e), cons)
        })?;

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(actual_rate)) => Ok((
            ExclusiveStream {
                shutdown,
                handle: Some(handle),
            },
            actual_rate,
        )),
        Ok(Err(msg)) => {
            let _ = handle.join();
            let cons = cons_slot
                .lock()
                .unwrap()
                .take()
                .expect("consumer must remain on init failure");
            Err((msg.into(), cons))
        }
        Err(e) => {
            shutdown.store(true, Ordering::Relaxed);
            let _ = handle.join();
            let cons = cons_slot
                .lock()
                .unwrap()
                .take()
                .expect("consumer must remain on init failure");
            Err((format!("WASAPI DoP init timed out: {e}").into(), cons))
        }
    }
}

/// DoP-only format probe. Restricted to Int24 (either 24-in-32 left-justified or
/// packed-24) since DoP markers require exact 24-bit values.
fn probe_format_dop(client: &AudioClient, rate: u32) -> Option<WaveFormat> {
    let channels = 2;
    let candidates = [(32, 24, SampleType::Int), (24, 24, SampleType::Int)];
    for (storage_bits, valid_bits, ty) in candidates {
        let fmt = WaveFormat::new(storage_bits, valid_bits, &ty, rate as usize, channels, None);
        if let Ok(adjusted) = client.is_supported_exclusive_with_quirks(&fmt) {
            return Some(adjusted);
        }
    }
    None
}

// The WASAPI DoP render thread owns device, state, buffers, and startup synchronization together.
#[allow(clippy::too_many_arguments)]
fn render_thread_dop(
    device_name: Option<String>,
    dop_frame_rate: u32,
    state: Arc<AtomicPlayerState>,
    shutdown: Arc<AtomicBool>,
    cons_slot: Arc<Mutex<Option<DopConsumer>>>,
    init_tx: std::sync::mpsc::Sender<Result<u32, String>>,
) {
    let _mmcss_guard = boost_current_thread_for_audio("WasapiExclusiveDop");

    let hr = initialize_mta();
    if hr.is_err() {
        let _ = init_tx.send(Err(format!("CoInitializeEx(MTA) failed: HRESULT {hr:?}")));
        return;
    }

    let device = match resolve_device(device_name.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    let mut audio_client = match device.get_iaudioclient() {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_iaudioclient failed: {e:?}")));
            return;
        }
    };

    let format = match probe_format_dop(&audio_client, dop_frame_rate) {
        Some(f) => f,
        None => {
            let _ = init_tx.send(Err(format!(
                "DAC does not accept Int24 exclusive at {dop_frame_rate}Hz — DoP unavailable"
            )));
            return;
        }
    };

    let blockalign = format.get_blockalign() as usize;
    let valid_bits = format.get_validbitspersample() as usize;
    let sample_type = format.get_subformat().unwrap_or(SampleType::Int);
    let bytes_per_sample = blockalign / format.get_nchannels() as usize;
    let actual_rate = format.get_samplespersec();
    let device_format = OutputSampleFormat {
        sample_type: output_sample_type(sample_type),
        valid_bits,
        bytes_per_sample,
        channels: format.get_nchannels() as usize,
    };

    if valid_bits != 24 || !matches!(sample_type, SampleType::Int) {
        let _ = init_tx.send(Err(format!(
            "Probed format is not Int24 ({}-bit {:?}) — DoP unavailable",
            valid_bits, sample_type
        )));
        return;
    }

    // The quirks probe may return an adjusted format. DoP frames are only valid at
    // exactly the requested rate — anything else puts garbage on the wire, so refuse.
    if actual_rate != dop_frame_rate {
        let _ = init_tx.send(Err(format!(
            "Probed format rate {actual_rate}Hz != requested DoP rate {dop_frame_rate}Hz — DoP unavailable"
        )));
        return;
    }

    state
        .target_bits
        .store(valid_bits as u32, Ordering::Relaxed);

    let (_def_period, min_period) = match audio_client.get_device_period() {
        Ok(p) => p,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_device_period failed: {e:?}")));
            return;
        }
    };

    let requested_period_hns = 100_000i64.max(min_period * 4);
    let desired_period = match audio_client.calculate_aligned_period_near(
        requested_period_hns,
        Some(512),
        &format,
    ) {
        Ok(p) => p,
        Err(e) => {
            let _ = init_tx.send(Err(format!("calculate_aligned_period_near failed: {e:?}")));
            return;
        }
    };

    let mode = StreamMode::EventsExclusive {
        period_hns: desired_period,
    };

    if let Err(e) = audio_client.initialize_client(&format, &Direction::Render, &mode) {
        if let wasapi::WasapiError::Windows(werr) = &e {
            if werr.code() == windows::Win32::Media::Audio::AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
                let new_size = match audio_client.get_buffer_size() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = init_tx.send(Err(format!("get_buffer_size failed: {e:?}")));
                        return;
                    }
                };
                let aligned_period =
                    calculate_period_100ns(new_size as i64, format.get_samplespersec() as i64);
                audio_client = match device.get_iaudioclient() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(format!("get_iaudioclient retry failed: {e:?}")));
                        return;
                    }
                };
                let retry_mode = StreamMode::EventsExclusive {
                    period_hns: aligned_period,
                };
                if let Err(e2) =
                    audio_client.initialize_client(&format, &Direction::Render, &retry_mode)
                {
                    let _ =
                        init_tx.send(Err(format!("DoP retry initialize_client failed: {e2:?}")));
                    return;
                }
            } else {
                let _ = init_tx.send(Err(format!("DoP initialize_client failed: {e:?}")));
                return;
            }
        } else {
            let _ = init_tx.send(Err(format!("DoP initialize_client failed: {e:?}")));
            return;
        }
    }

    let h_event = match audio_client.set_get_eventhandle() {
        Ok(h) => h,
        Err(e) => {
            let _ = init_tx.send(Err(format!("set_get_eventhandle failed: {e:?}")));
            return;
        }
    };

    let render_client = match audio_client.get_audiorenderclient() {
        Ok(rc) => rc,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_audiorenderclient failed: {e:?}")));
            return;
        }
    };

    let buffer_size_frames = match audio_client.get_buffer_size() {
        Ok(n) => n,
        Err(e) => {
            let _ = init_tx.send(Err(format!("get_buffer_size failed: {e:?}")));
            return;
        }
    };

    if let Err(e) = audio_client.start_stream() {
        let _ = init_tx.send(Err(format!("start_stream failed: {e:?}")));
        return;
    }

    let mut cons = match cons_slot.lock().unwrap().take() {
        Some(c) => c,
        None => {
            let _ = init_tx.send(Err("DoP consumer was already taken".into()));
            return;
        }
    };

    if init_tx.send(Ok(actual_rate)).is_err() {
        let _ = audio_client.stop_stream();
        return;
    }

    println!(
        "WasapiExclusiveDop: render loop start — rate={}Hz buffer={} frames period={}hns blockalign={} bytes_per_sample={}",
        actual_rate, buffer_size_frames, desired_period, blockalign, bytes_per_sample,
    );

    let channels = format.get_nchannels() as usize;
    let mut scratch_i32: Vec<i32> = Vec::new();
    let mut scratch_bytes: Vec<u8> = Vec::new();
    let mut dop_idle = DopIdlePattern::new();
    // Single marker-phase authority for the stream: every outgoing frame gets its
    // marker byte re-stamped here, so the 0x05/0xFA cadence stays continuous across
    // program/idle splices (underrun, pause/resume, flush) no matter what phase the
    // packer or idle generator were sitting at.
    let mut marker_stamper = DopMarkerStamper::new();
    let mut event_failures = 0u32;

    while !shutdown.load(Ordering::Relaxed) {
        let raw_available = match audio_client.get_available_space_in_frames() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("WasapiExclusiveDop: get_available_space_in_frames error: {e:?}");
                break;
            }
        };
        if raw_available > buffer_size_frames {
            eprintln!(
                "WasapiExclusiveDop: clamping available {} > buffer_size {}",
                raw_available, buffer_size_frames,
            );
        }
        let available = raw_available.min(buffer_size_frames) as usize;

        if available > 0 {
            let needed_samples = available * channels;
            scratch_i32.clear();
            scratch_i32.resize(needed_samples, 0);

            let is_playing = state.state.load(Ordering::Relaxed) == 1;
            if state.flush_buffer.swap(false, Ordering::Relaxed) {
                cons.clear();
            }

            // Pop whole frames only. The ring is a flat sample queue: taking an odd
            // count would orphan one channel's sample and permanently slip L/R, which
            // in DoP mode mismatches markers within a frame and drops DSD lock.
            let read = if is_playing {
                let aligned = (cons.len().min(needed_samples) / channels) * channels;
                cons.pop_slice(&mut scratch_i32[..aligned])
            } else {
                0
            };
            if is_playing && read < needed_samples {
                let missing = (needed_samples - read) as u64;
                let previous = state.underrun_events.fetch_add(1, Ordering::Relaxed);
                state.underrun_samples.fetch_add(missing, Ordering::Relaxed);
                if previous == 0 || (previous + 1).is_power_of_two() {
                    eprintln!(
                        "WasapiExclusiveDop: underrun #{}, missing {} samples",
                        previous + 1,
                        missing,
                    );
                }
                dop_idle.fill_interleaved_i32(&mut scratch_i32[read..], channels);
            } else if !is_playing {
                dop_idle.fill_interleaved_i32(&mut scratch_i32, channels);
            }

            // DSD has no meaningful per-sample amplitude — meters stay at zero in DoP
            // mode. Volume control is not applied at the bit level (it would scramble
            // markers); use analog volume or the modulator's input scaling instead.
            state.meter_l.store(0, Ordering::Relaxed);
            state.meter_r.store(0, Ordering::Relaxed);

            if is_playing {
                let played_frames = (read / channels) as u64;
                state
                    .position_samples
                    .fetch_add(played_frames * 16, Ordering::Relaxed);
            }

            // Re-stamp every outgoing marker (program and idle alike) from the single
            // stream-lifetime alternation source.
            marker_stamper.restamp_interleaved_i32(&mut scratch_i32, channels);

            // Pack i32 DoP values into the device's wire layout.
            scratch_bytes.clear();
            scratch_bytes.resize(available * blockalign, 0);
            encode_interleaved_i32_passthrough(&scratch_i32, &mut scratch_bytes, device_format);

            if let Err(e) = render_client.write_to_device(available, &scratch_bytes, None) {
                eprintln!("WasapiExclusiveDop: write_to_device error: {e:?}");
                break;
            }
        }

        if h_event.wait_for_event(200).is_err() {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            // A persistently broken event handle would otherwise busy-spin this loop.
            event_failures += 1;
            if event_failures >= 50 {
                eprintln!(
                    "WasapiExclusiveDop: {event_failures} consecutive event waits failed; stopping render loop"
                );
                break;
            }
            continue;
        }
        event_failures = 0;
    }

    let _ = audio_client.stop_stream();
}

/// List active WASAPI render endpoints by friendly name. Currently unused — the device list
/// endpoint relies on CPAL's WASAPI enumeration, which surfaces the same friendly names — but
/// kept for future direct WASAPI listing if CPAL's enumeration becomes insufficient.
#[allow(dead_code)]
pub fn list_devices() -> Vec<String> {
    if initialize_mta().is_err() {
        return Vec::new();
    }
    let enumerator = match wasapi::DeviceEnumerator::new() {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let collection = match enumerator.get_device_collection(&Direction::Render) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let count = collection.get_nbr_devices().unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..count {
        if let Ok(dev) = collection.get_device_at_index(i) {
            if let Ok(name) = dev.get_friendlyname() {
                out.push(name);
            }
        }
    }
    out
}
