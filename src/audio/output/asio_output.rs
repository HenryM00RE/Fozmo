//! Native ASIO output backend for Windows.
//!
//! ASIO is intentionally owned outside CPAL so this backend can negotiate the
//! ASIO DSD extension and keep all driver setup/teardown on one STA thread.

use crate::audio::dsd::native_dsd::NativeDsdOrder;
use crate::audio::engine::buffers::native_dsd_ring_capacity_bytes;
use crate::audio::engine::output_ramp::PcmTransitionRamp;
use crate::audio::engine::player::{AtomicPlayerState, AudioConsumer};
use ndsd_asio_sys::bindings::asio_import as ai;
use ndsd_asio_sys::bindings::errors::AsioErrorWrapper;
use ndsd_asio_sys::{Asio, AsioMessageSelectors, AsioSampleType, Driver};
use ringbuf::{HeapRb, Producer, SharedRb};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

pub type NativeProducer = Producer<u8, Arc<SharedRb<u8, Vec<MaybeUninit<u8>>>>>;

const NATIVE_DSD_INIT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct AsioStream {
    shutdown: Arc<AtomicBool>,
    reset_requested: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl AsioStream {
    pub fn reset_requested(&self) -> bool {
        self.reset_requested.swap(false, Ordering::AcqRel)
    }
}

impl Drop for AsioStream {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub struct NativeDsdOpen {
    pub stream: AsioStream,
    pub producer_l: NativeProducer,
    pub producer_r: NativeProducer,
    pub order: NativeDsdOrder,
    pub callback_bytes: usize,
}

#[derive(Debug)]
pub struct NativeDsdOpenError {
    message: String,
    timed_out: bool,
}

impl NativeDsdOpenError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timed_out: false,
        }
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timed_out: true,
        }
    }

    pub fn timed_out(&self) -> bool {
        self.timed_out
    }
}

impl std::fmt::Display for NativeDsdOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NativeDsdOpenError {}

struct ComGuard;

impl ComGuard {
    fn init_sta() -> Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| format!("CoInitializeEx(STA) failed: {e:?}"))?;
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub fn list_devices() -> Vec<String> {
    thread::Builder::new()
        .name("AsioEnumerate".to_string())
        .spawn(|| {
            let _com = ComGuard::init_sta().ok();
            Asio::new().driver_names()
        })
        .ok()
        .and_then(|h| h.join().ok())
        .unwrap_or_default()
}

fn success(code: i32) -> bool {
    code == AsioErrorWrapper::ASE_OK as i32 || code == AsioErrorWrapper::ASE_SUCCESS as i32
}

fn set_io_format(format: i32, required: bool) -> Result<(), String> {
    let mut io_format = ai::ASIOIoFormat {
        FormatType: format,
        future: [0; 508],
    };
    let code = unsafe {
        ai::ASIOFuture(
            ai::kAsioSetIoFormat as i32,
            (&mut io_format as *mut _) as *mut std::ffi::c_void,
        )
    };
    if success(code) || !required {
        Ok(())
    } else {
        Err(format!("ASIO driver rejected IO format request ({code})"))
    }
}

fn configure_rate(driver: &Driver, rate: u32) -> Result<(), String> {
    let supported = driver
        .can_sample_rate(rate as f64)
        .map_err(|e| format!("ASIOCanSampleRate({rate}) failed: {e}"))?;
    if !supported {
        return Err(format!("ASIO driver does not accept {rate} Hz"));
    }
    driver
        .set_sample_rate(rate as f64)
        .map_err(|e| format!("ASIOSetSampleRate({rate}) failed: {e}"))
}

fn configure_pcm_rate(driver: &Driver, requested_rate: u32) -> Result<u32, String> {
    let mut last_error = None;
    for rate in pcm_rate_candidates(requested_rate) {
        match configure_rate(driver, rate) {
            Ok(()) => return Ok(rate),
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        format!("ASIO driver does not accept requested rate {requested_rate} Hz")
    }))
}

fn pcm_rate_candidates(requested_rate: u32) -> Vec<u32> {
    const RATES_44_FAMILY: [u32; 4] = [44_100, 88_200, 176_400, 352_800];
    const RATES_48_FAMILY: [u32; 4] = [48_000, 96_000, 192_000, 384_000];
    const STANDARD_RATES: [u32; 8] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
    ];

    let mut candidates = vec![requested_rate];
    let family = if RATES_44_FAMILY.contains(&requested_rate) {
        &RATES_44_FAMILY[..]
    } else if RATES_48_FAMILY.contains(&requested_rate) {
        &RATES_48_FAMILY[..]
    } else {
        &STANDARD_RATES[..]
    };

    candidates.extend(
        family
            .iter()
            .rev()
            .copied()
            .filter(|rate| *rate < requested_rate),
    );
    candidates.dedup();
    candidates
}

fn register_reset_callback(
    driver: &Driver,
    reset: Arc<AtomicBool>,
) -> ndsd_asio_sys::bindings::MessageCallbackId {
    driver.add_message_callback(move |message| {
        if matches!(
            message,
            AsioMessageSelectors::kAsioResetRequest | AsioMessageSelectors::kAsioResyncRequest
        ) {
            reset.store(true, Ordering::Release);
        }
    })
}

pub fn open_pcm(
    driver_name: &str,
    target_rate: u32,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
) -> Result<(AsioStream, u32), (Box<dyn std::error::Error>, AudioConsumer)> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let reset_requested = Arc::new(AtomicBool::new(false));
    let cons_slot = Arc::new(Mutex::new(Some(cons)));
    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let driver_name = driver_name.to_string();
    let shutdown_thread = Arc::clone(&shutdown);
    let reset_thread = Arc::clone(&reset_requested);
    let cons_thread = Arc::clone(&cons_slot);

    let handle = thread::Builder::new()
        .name("AsioPcmControl".to_string())
        .spawn(move || {
            if let Err(e) = run_pcm_thread(
                &driver_name,
                target_rate,
                cons_thread,
                state,
                shutdown_thread,
                reset_thread,
                tx.clone(),
            ) {
                let _ = tx.send(Err(e));
            }
        })
        .map_err(|e| -> (Box<dyn std::error::Error>, AudioConsumer) {
            let cons = cons_slot
                .lock()
                .unwrap()
                .take()
                .expect("ASIO PCM consumer present");
            (Box::new(e), cons)
        })?;

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(rate)) => Ok((
            AsioStream {
                shutdown,
                reset_requested,
                handle: Some(handle),
            },
            rate,
        )),
        Ok(Err(message)) => {
            shutdown.store(true, Ordering::Release);
            let _ = handle.join();
            let cons = cons_slot
                .lock()
                .unwrap()
                .take()
                .expect("ASIO PCM consumer returned");
            Err((message.into(), cons))
        }
        Err(e) => {
            shutdown.store(true, Ordering::Release);
            let _ = handle.join();
            let cons = cons_slot
                .lock()
                .unwrap()
                .take()
                .expect("ASIO PCM consumer returned");
            Err((format!("ASIO PCM init timed out: {e}").into(), cons))
        }
    }
}

fn run_pcm_thread(
    driver_name: &str,
    target_rate: u32,
    cons_slot: Arc<Mutex<Option<AudioConsumer>>>,
    state: Arc<AtomicPlayerState>,
    shutdown: Arc<AtomicBool>,
    reset_requested: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::Sender<Result<u32, String>>,
) -> Result<(), String> {
    let _com = ComGuard::init_sta()?;
    let asio = Asio::new();
    let driver = asio
        .load_driver(driver_name)
        .map_err(|e| format!("Failed to load ASIO driver '{driver_name}': {e}"))?;
    set_io_format(ai::ASIOIoFormatType_e_kASIOPCMFormat, false)?;
    let actual_rate = configure_pcm_rate(&driver, target_rate)?;
    let sample_type = driver
        .output_data_type()
        .map_err(|e| format!("Failed to query ASIO PCM output type: {e}"))?;
    match &sample_type {
        AsioSampleType::ASIOSTFloat32LSB
        | AsioSampleType::ASIOSTInt32LSB
        | AsioSampleType::ASIOSTInt24LSB
        | AsioSampleType::ASIOSTInt16LSB => {}
        _ => {
            return Err(format!(
                "Unsupported ASIO PCM output format: {sample_type:?}"
            ));
        }
    }
    let streams = driver
        .prepare_output_stream(None, 2, None)
        .map_err(|e| format!("Failed to create ASIO PCM buffers: {e}"))?;
    let output = streams.output.ok_or("ASIO returned no PCM output stream")?;
    let frames = output.buffer_size as usize;
    let pointers = plane_pointers(&output)?;
    let callback_cons = Arc::clone(&cons_slot);
    let mut scratch = vec![0.0f64; frames * 2];
    let callback_state = Arc::clone(&state);
    let mut ramp = PcmTransitionRamp::new(2);
    let callback_id = driver.add_callback(move |info| {
        scratch.fill(0.0);
        let playing = callback_state.state.load(Ordering::Relaxed) == 1;
        let read = if let Ok(mut slot) = callback_cons.try_lock() {
            let cons = match slot.as_mut() {
                Some(cons) => cons,
                None => return,
            };
            if callback_state.flush_buffer.swap(false, Ordering::Relaxed) {
                cons.clear();
            }
            if playing {
                cons.pop_slice(&mut scratch)
            } else {
                0
            }
        } else {
            0
        };
        if playing && read < scratch.len() {
            callback_state
                .underrun_events
                .fetch_add(1, Ordering::Relaxed);
            callback_state
                .underrun_samples
                .fetch_add((scratch.len() - read) as u64, Ordering::Relaxed);
        }
        let scratch_len = scratch.len();
        ramp.process(&mut scratch, scratch_len, 2, playing);
        let volume = f32::from_bits(callback_state.volume.load(Ordering::Relaxed)) as f64;
        unsafe {
            write_pcm_planes(
                pointers,
                info.buffer_index as usize,
                &sample_type,
                &scratch,
                volume,
            );
        }
        if playing {
            callback_state
                .position_samples
                .fetch_add((read / 2) as u64, Ordering::Relaxed);
        }
    });
    let message_id = register_reset_callback(&driver, Arc::clone(&reset_requested));
    driver
        .start()
        .map_err(|e| format!("Failed to start ASIO PCM stream: {e}"))?;
    state.exclusive.store(true, Ordering::Relaxed);
    state.target_bits.store(32, Ordering::Relaxed);
    init_tx
        .send(Ok(actual_rate))
        .map_err(|_| "ASIO PCM caller disappeared")?;

    while !shutdown.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(10));
    }
    let _ = driver.stop();
    driver.remove_callback(callback_id);
    driver.remove_message_callback(message_id);
    let _ = driver.dispose_buffers();
    Ok(())
}

pub fn open_native_dsd(
    driver_name: &str,
    wire_rate: u32,
    dsp_buffer_ms: u32,
    state: Arc<AtomicPlayerState>,
) -> Result<NativeDsdOpen, NativeDsdOpenError> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let reset_requested = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<
        Result<(NativeProducer, NativeProducer, NativeDsdOrder, usize), String>,
    >();
    let driver_name = driver_name.to_string();
    let shutdown_thread = Arc::clone(&shutdown);
    let reset_thread = Arc::clone(&reset_requested);
    let handle = thread::Builder::new()
        .name("AsioNativeDsdControl".to_string())
        .spawn(move || {
            if let Err(e) = run_native_thread(
                &driver_name,
                wire_rate,
                dsp_buffer_ms,
                state,
                shutdown_thread,
                reset_thread,
                tx.clone(),
            ) {
                let _ = tx.send(Err(e));
            }
        })
        .map_err(|e| NativeDsdOpenError::new(e.to_string()))?;

    match rx.recv_timeout(NATIVE_DSD_INIT_TIMEOUT) {
        Ok(Ok((producer_l, producer_r, order, callback_bytes))) => Ok(NativeDsdOpen {
            stream: AsioStream {
                shutdown,
                reset_requested,
                handle: Some(handle),
            },
            producer_l,
            producer_r,
            order,
            callback_bytes,
        }),
        Ok(Err(message)) => {
            shutdown.store(true, Ordering::Release);
            let _ = handle.join();
            Err(NativeDsdOpenError::new(message))
        }
        Err(e) => {
            shutdown.store(true, Ordering::Release);
            let _ = handle.join();
            Err(NativeDsdOpenError::timeout(format!(
                "ASIO native DSD init timed out after {}s: {e}",
                NATIVE_DSD_INIT_TIMEOUT.as_secs()
            )))
        }
    }
}

fn run_native_thread(
    driver_name: &str,
    wire_rate: u32,
    dsp_buffer_ms: u32,
    state: Arc<AtomicPlayerState>,
    shutdown: Arc<AtomicBool>,
    reset_requested: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::Sender<
        Result<(NativeProducer, NativeProducer, NativeDsdOrder, usize), String>,
    >,
) -> Result<(), String> {
    let _com = ComGuard::init_sta()?;
    let asio = Asio::new();
    let driver = asio
        .load_driver(driver_name)
        .map_err(|e| format!("Failed to load ASIO driver '{driver_name}': {e}"))?;
    set_io_format(ai::ASIOIoFormatType_e_kASIODSDFormat, true)?;
    configure_rate(&driver, wire_rate)?;
    let streams = driver
        .prepare_output_stream(None, 2, None)
        .map_err(|e| format!("Failed to create native DSD buffers: {e}"))?;
    let output = streams
        .output
        .ok_or("ASIO returned no native DSD output stream")?;
    let order = match driver
        .output_data_type()
        .map_err(|e| format!("Failed to query native DSD sample type: {e}"))?
    {
        AsioSampleType::ASIOSTDSDInt8MSB1 => NativeDsdOrder::MsbFirst,
        AsioSampleType::ASIOSTDSDInt8LSB1 => NativeDsdOrder::LsbFirst,
        _ => return Err("ASIO native DSD format is not packed 1-bit-per-byte output".into()),
    };
    let callback_bytes = output.buffer_size as usize / 8;
    if callback_bytes == 0 {
        return Err("ASIO native DSD buffer size is smaller than one packed byte".into());
    }
    let pointers = plane_pointers(&output)?;
    let capacity = native_dsd_ring_capacity_bytes(wire_rate, callback_bytes, dsp_buffer_ms);
    let (producer_l, mut consumer_l) = HeapRb::<u8>::new(capacity).split();
    let (producer_r, mut consumer_r) = HeapRb::<u8>::new(capacity).split();
    let idle = order.idle_byte();
    let callback_state = Arc::clone(&state);
    let callback_id = driver.add_callback(move |info| unsafe {
        let index = info.buffer_index as usize;
        let left = std::slice::from_raw_parts_mut(pointers[0][index] as *mut u8, callback_bytes);
        let right = std::slice::from_raw_parts_mut(pointers[1][index] as *mut u8, callback_bytes);
        left.fill(idle);
        right.fill(idle);
        let playing = callback_state.state.load(Ordering::Relaxed) == 1;
        if callback_state.flush_buffer.swap(false, Ordering::Relaxed) {
            consumer_l.clear();
            consumer_r.clear();
        }
        let read = if playing {
            let available = consumer_l.len().min(consumer_r.len()).min(callback_bytes);
            let left_read = consumer_l.pop_slice(&mut left[..available]);
            let right_read = consumer_r.pop_slice(&mut right[..available]);
            debug_assert_eq!(left_read, right_read);
            left_read.min(right_read)
        } else {
            0
        };
        if playing && read < callback_bytes {
            callback_state
                .underrun_events
                .fetch_add(1, Ordering::Relaxed);
            callback_state
                .underrun_samples
                .fetch_add(((callback_bytes - read) * 8 * 2) as u64, Ordering::Relaxed);
        }
        callback_state.meter_l.store(0, Ordering::Relaxed);
        callback_state.meter_r.store(0, Ordering::Relaxed);
        if playing {
            callback_state
                .position_samples
                .fetch_add((read * 8) as u64, Ordering::Relaxed);
        }
    });
    let message_id = register_reset_callback(&driver, Arc::clone(&reset_requested));
    driver
        .start()
        .map_err(|e| format!("Failed to start native DSD stream: {e}"))?;
    state.exclusive.store(true, Ordering::Relaxed);
    state.target_bits.store(1, Ordering::Relaxed);
    init_tx
        .send(Ok((producer_l, producer_r, order, callback_bytes)))
        .map_err(|_| "ASIO native DSD caller disappeared")?;

    while !shutdown.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(10));
    }
    let _ = driver.stop();
    driver.remove_callback(callback_id);
    driver.remove_message_callback(message_id);
    let _ = driver.dispose_buffers();
    Ok(())
}

fn plane_pointers(output: &ndsd_asio_sys::AsioStream) -> Result<[[usize; 2]; 2], String> {
    if output.buffer_infos.len() < 2 {
        return Err("ASIO device exposes fewer than two output channels".into());
    }
    Ok([
        [
            output.buffer_infos[0].buffers[0] as usize,
            output.buffer_infos[0].buffers[1] as usize,
        ],
        [
            output.buffer_infos[1].buffers[0] as usize,
            output.buffer_infos[1].buffers[1] as usize,
        ],
    ])
}

unsafe fn write_pcm_planes(
    pointers: [[usize; 2]; 2],
    buffer_index: usize,
    sample_type: &AsioSampleType,
    samples: &[f64],
    volume: f64,
) {
    let frames = samples.len() / 2;
    match sample_type {
        AsioSampleType::ASIOSTFloat32LSB => {
            for channel in 0..2 {
                let output = unsafe {
                    std::slice::from_raw_parts_mut(
                        pointers[channel][buffer_index] as *mut f32,
                        frames,
                    )
                };
                for (frame, sample) in output.iter_mut().enumerate() {
                    *sample = (samples[frame * 2 + channel] * volume) as f32;
                }
            }
        }
        AsioSampleType::ASIOSTInt32LSB => {
            for channel in 0..2 {
                let output = unsafe {
                    std::slice::from_raw_parts_mut(
                        pointers[channel][buffer_index] as *mut i32,
                        frames,
                    )
                };
                for (frame, sample) in output.iter_mut().enumerate() {
                    let value = (samples[frame * 2 + channel] * volume).clamp(-1.0, 1.0);
                    *sample = (value * i32::MAX as f64) as i32;
                }
            }
        }
        AsioSampleType::ASIOSTInt16LSB => {
            for channel in 0..2 {
                let output = unsafe {
                    std::slice::from_raw_parts_mut(
                        pointers[channel][buffer_index] as *mut i16,
                        frames,
                    )
                };
                for (frame, sample) in output.iter_mut().enumerate() {
                    let value = (samples[frame * 2 + channel] * volume).clamp(-1.0, 1.0);
                    *sample = (value * i16::MAX as f64) as i16;
                }
            }
        }
        AsioSampleType::ASIOSTInt24LSB => {
            for channel in 0..2 {
                let output = unsafe {
                    std::slice::from_raw_parts_mut(
                        pointers[channel][buffer_index] as *mut u8,
                        frames * 3,
                    )
                };
                for frame in 0..frames {
                    let value = (samples[frame * 2 + channel] * volume).clamp(-1.0, 1.0);
                    let quantized = (value * 8_388_607.0) as i32;
                    let bytes = quantized.to_le_bytes();
                    output[frame * 3..frame * 3 + 3].copy_from_slice(&bytes[..3]);
                }
            }
        }
        _ => {
            unreachable!("unsupported ASIO PCM sample type was rejected during setup");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pcm_rate_candidates;

    #[test]
    fn pcm_rate_candidates_keep_requested_family_first() {
        assert_eq!(
            pcm_rate_candidates(352_800),
            vec![352_800, 176_400, 88_200, 44_100]
        );
        assert_eq!(
            pcm_rate_candidates(384_000),
            vec![384_000, 192_000, 96_000, 48_000]
        );
    }
}
