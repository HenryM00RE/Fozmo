//! Developer bit-perfect loopback verifier for the Fozmo Capture HAL driver.
//!
//! Plays a deterministic F32 signal (marker burst + PRBS) into the Fozmo
//! Capture output and reads it back from the capture input, asserting
//! sample-exact equality and flat underrun/overrun/snap telemetry once the
//! streams are locked. Requires the driver to be installed and published by
//! coreaudiod, so it only runs on macOS with the driver present.
//!
//! Usage:
//!   cargo run --release --bin fozmo_capture_loopback -- [--rates 44100,96000,192000] [--secs 30]

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("fozmo_capture_loopback only runs on macOS with the Fozmo Capture driver installed.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    std::process::exit(macos::run());
}

#[cfg(target_os = "macos")]
mod macos {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleRate, StreamConfig};
    use ringbuf::HeapRb;
    use std::time::{Duration, Instant};

    const DEVICE_NAME: &str = "Fozmo Capture";
    const DEVICE_UID: &str = "com.fozmo.audio.capture";
    const CHANNELS: u16 = 2;
    const MARKER_FRAMES: usize = 64;
    const PRBS_SEED: u64 = 0x5eed_f02f_ab1e_0001;
    const ALIGN_TIMEOUT: Duration = Duration::from_secs(10);

    // Telemetry selectors exposed by drivers/fozmo-capture (see the driver
    // README); duplicated here because this dev binary is self-contained.
    const PROP_UNDERRUNS: u32 = 0x7472_756e; // trun
    const PROP_OVERRUNS: u32 = 0x7472_6f76; // trov
    const PROP_SNAPS: u32 = 0x7472_736e; // trsn

    pub(super) fn run() -> i32 {
        let (rates, secs) = parse_args();
        let Some(device_id) = coreaudio::device_id_for_uid(DEVICE_UID) else {
            eprintln!("{DEVICE_NAME} is not visible to CoreAudio. Install the driver first.");
            return 1;
        };
        let mut failed = false;
        for rate in rates {
            println!("=== {rate} Hz, {secs} s ===");
            match run_rate(device_id, rate, secs) {
                Ok(report) => println!("{report}"),
                Err(err) => {
                    eprintln!("FAIL at {rate} Hz: {err}");
                    failed = true;
                }
            }
        }
        if failed { 1 } else { 0 }
    }

    fn parse_args() -> (Vec<u32>, u64) {
        let mut rates = vec![44_100, 96_000, 192_000];
        let mut secs = 30_u64;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--rates" => {
                    if let Some(value) = args.next() {
                        rates = value
                            .split(',')
                            .filter_map(|rate| rate.trim().parse().ok())
                            .collect();
                    }
                }
                "--secs" => {
                    if let Some(value) = args.next()
                        && let Ok(parsed) = value.parse()
                    {
                        secs = parsed;
                    }
                }
                other => eprintln!("Ignoring unknown argument: {other}"),
            }
        }
        (rates, secs)
    }

    /// Deterministic test signal: a fixed marker burst (left/right mirrored to
    /// catch channel swaps) followed by xorshift PRBS noise at ±0.5.
    fn signal_sample(index: u64) -> f32 {
        let frame = index / u64::from(CHANNELS);
        let channel = index % u64::from(CHANNELS);
        if (frame as usize) < MARKER_FRAMES {
            let base = 1.0 - (frame as f32) / MARKER_FRAMES as f32;
            return if channel == 0 { base } else { -base };
        }
        let mut state = PRBS_SEED ^ (index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 40) as f32 / (1u64 << 24) as f32) - 0.5
    }

    fn run_rate(
        device_id: coreaudio_sys::AudioDeviceID,
        rate: u32,
        secs: u64,
    ) -> Result<String, String> {
        coreaudio::set_nominal_rate(device_id, rate)?;

        let host = cpal::default_host();
        let input_device = host
            .input_devices()
            .map_err(|err| format!("input enumeration failed: {err}"))?
            .find(|device| device.name().is_ok_and(|name| name == DEVICE_NAME))
            .ok_or_else(|| format!("{DEVICE_NAME} input not found"))?;
        let output_device = host
            .output_devices()
            .map_err(|err| format!("output enumeration failed: {err}"))?
            .find(|device| device.name().is_ok_and(|name| name == DEVICE_NAME))
            .ok_or_else(|| format!("{DEVICE_NAME} output not found"))?;
        let config = StreamConfig {
            channels: CHANNELS,
            sample_rate: SampleRate(rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Input first: the driver ring resets on the 0 -> 1 IO transition and
        // snaps forward on backlog, so a late input client would lose the
        // marker burst.
        let ring = HeapRb::<f32>::new(rate as usize * usize::from(CHANNELS) * 4);
        let (mut producer, mut consumer) = ring.split();
        let input_stream = input_device
            .build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let _ = producer.push_slice(data);
                },
                |err| eprintln!("input stream error: {err}"),
                None,
            )
            .map_err(|err| format!("could not open input stream: {err}"))?;
        input_stream
            .play()
            .map_err(|err| format!("could not start input stream: {err}"))?;

        let mut generator_index = 0_u64;
        let output_stream = output_device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    for sample in data.iter_mut() {
                        *sample = signal_sample(generator_index);
                        generator_index += 1;
                    }
                },
                |err| eprintln!("output stream error: {err}"),
                None,
            )
            .map_err(|err| format!("could not open output stream: {err}"))?;
        output_stream
            .play()
            .map_err(|err| format!("could not start output stream: {err}"))?;

        // Align: scan the incoming zero/underrun preamble for the marker burst.
        let mut staging = vec![0.0_f32; 8192];
        let align_deadline = Instant::now() + ALIGN_TIMEOUT;
        let mut aligned = false;
        let mut expected_index = 0_u64;
        let mut carry: Vec<f32> = Vec::new();
        while !aligned {
            if Instant::now() > align_deadline {
                return Err("did not find the marker burst within 10 s".to_string());
            }
            let popped = consumer.pop_slice(&mut staging);
            if popped == 0 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            carry.extend_from_slice(&staging[..popped]);
            // Marker starts on a frame boundary with L=+1.0.
            for start in 0..carry.len().saturating_sub(MARKER_FRAMES * 2) {
                if carry[start] == 1.0
                    && (0..MARKER_FRAMES * 2)
                        .all(|offset| carry[start + offset] == signal_sample(offset as u64))
                {
                    carry.drain(..start);
                    aligned = true;
                    break;
                }
            }
            if !aligned && carry.len() > staging.len() * 4 {
                let keep = MARKER_FRAMES * 2;
                carry.drain(..carry.len() - keep);
            }
        }

        // Locked: baseline the driver counters, then require them flat.
        let baseline = read_counters(device_id);
        let total_samples = rate as u64 * u64::from(CHANNELS) * secs;
        let mut mismatches = 0_u64;
        let mut first_mismatch = None;
        for sample in carry.drain(..) {
            check_sample(
                sample,
                &mut expected_index,
                &mut mismatches,
                &mut first_mismatch,
            );
        }
        let run_deadline = Instant::now() + Duration::from_secs(secs + 15);
        while expected_index < total_samples {
            if Instant::now() > run_deadline {
                return Err(format!(
                    "verification stalled at sample {expected_index}/{total_samples}"
                ));
            }
            let popped = consumer.pop_slice(&mut staging);
            if popped == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            for sample in &staging[..popped] {
                if expected_index >= total_samples {
                    break;
                }
                check_sample(
                    *sample,
                    &mut expected_index,
                    &mut mismatches,
                    &mut first_mismatch,
                );
            }
        }
        drop(output_stream);
        drop(input_stream);
        let end = read_counters(device_id);

        let mut problems = Vec::new();
        if mismatches > 0 {
            problems.push(format!(
                "{mismatches} mismatched samples (first at index {})",
                first_mismatch.unwrap_or_default()
            ));
        }
        for (name, before, after) in [
            ("underruns", baseline.0, end.0),
            ("overruns", baseline.1, end.1),
            ("snaps", baseline.2, end.2),
        ] {
            if after > before {
                problems.push(format!("{name} moved {before} -> {after} during the run"));
            }
        }
        if problems.is_empty() {
            Ok(format!(
                "PASS: {total_samples} samples bit-exact; underruns/overruns/snaps flat ({}/{}/{})",
                end.0, end.1, end.2
            ))
        } else {
            Err(problems.join("; "))
        }
    }

    fn check_sample(
        sample: f32,
        expected_index: &mut u64,
        mismatches: &mut u64,
        first_mismatch: &mut Option<u64>,
    ) {
        if sample.to_bits() != signal_sample(*expected_index).to_bits() {
            *mismatches += 1;
            first_mismatch.get_or_insert(*expected_index);
        }
        *expected_index += 1;
    }

    fn read_counters(device_id: coreaudio_sys::AudioDeviceID) -> (u64, u64, u64) {
        (
            coreaudio::read_u64(device_id, PROP_UNDERRUNS).unwrap_or(0),
            coreaudio::read_u64(device_id, PROP_OVERRUNS).unwrap_or(0),
            coreaudio::read_u64(device_id, PROP_SNAPS).unwrap_or(0),
        )
    }

    mod coreaudio {
        use core_foundation::base::TCFType;
        use core_foundation::string::{CFString, CFStringRef};
        use coreaudio_sys::{
            AudioDeviceID, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
            AudioObjectPropertyAddress, AudioObjectSetPropertyData, kAudioDevicePropertyDeviceUID,
            kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDevices,
            kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeGlobal,
            kAudioObjectSystemObject,
        };
        use std::time::{Duration, Instant};
        use std::{mem, ptr};

        fn address(selector: u32) -> AudioObjectPropertyAddress {
            AudioObjectPropertyAddress {
                mSelector: selector,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMaster,
            }
        }

        pub(super) fn device_id_for_uid(uid: &str) -> Option<AudioDeviceID> {
            let devices_address = address(kAudioHardwarePropertyDevices);
            let mut size = 0_u32;
            let status = unsafe {
                AudioObjectGetPropertyDataSize(
                    kAudioObjectSystemObject,
                    &devices_address,
                    0,
                    ptr::null(),
                    &mut size,
                )
            };
            if status != 0 || size == 0 {
                return None;
            }
            let count = size as usize / mem::size_of::<AudioDeviceID>();
            let mut devices = vec![0 as AudioDeviceID; count];
            let status = unsafe {
                AudioObjectGetPropertyData(
                    kAudioObjectSystemObject,
                    &devices_address,
                    0,
                    ptr::null(),
                    &mut size,
                    devices.as_mut_ptr() as *mut libc::c_void,
                )
            };
            if status != 0 {
                return None;
            }
            let uid_address = address(kAudioDevicePropertyDeviceUID);
            devices.into_iter().find(|device_id| {
                let mut cf_uid: CFStringRef = ptr::null();
                let mut uid_size = mem::size_of::<CFStringRef>() as u32;
                let status = unsafe {
                    AudioObjectGetPropertyData(
                        *device_id,
                        &uid_address,
                        0,
                        ptr::null(),
                        &mut uid_size,
                        &mut cf_uid as *mut _ as *mut libc::c_void,
                    )
                };
                status == 0
                    && !cf_uid.is_null()
                    && unsafe { CFString::wrap_under_create_rule(cf_uid) } == CFString::new(uid)
            })
        }

        pub(super) fn read_u64(device_id: AudioDeviceID, selector: u32) -> Option<u64> {
            let property = address(selector);
            let mut value = 0_u64;
            let mut size = mem::size_of::<u64>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    device_id,
                    &property,
                    0,
                    ptr::null(),
                    &mut size,
                    &mut value as *mut _ as *mut libc::c_void,
                )
            };
            (status == 0).then_some(value)
        }

        fn read_f64(device_id: AudioDeviceID, selector: u32) -> Option<f64> {
            let property = address(selector);
            let mut value = 0_f64;
            let mut size = mem::size_of::<f64>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    device_id,
                    &property,
                    0,
                    ptr::null(),
                    &mut size,
                    &mut value as *mut _ as *mut libc::c_void,
                )
            };
            (status == 0).then_some(value)
        }

        /// Write the nominal rate and poll for the async config-change apply.
        pub(super) fn set_nominal_rate(device_id: AudioDeviceID, rate: u32) -> Result<(), String> {
            let property = address(kAudioDevicePropertyNominalSampleRate);
            let value = f64::from(rate);
            let status = unsafe {
                AudioObjectSetPropertyData(
                    device_id,
                    &property,
                    0,
                    ptr::null(),
                    mem::size_of::<f64>() as u32,
                    &value as *const _ as *const libc::c_void,
                )
            };
            if status != 0 {
                return Err(format!("nominal rate write failed (status {status})"));
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let applied = read_f64(device_id, kAudioDevicePropertyNominalSampleRate);
                if applied.is_some_and(|applied| (applied - value).abs() < 0.5) {
                    return Ok(());
                }
                if Instant::now() > deadline {
                    return Err(format!(
                        "driver did not confirm {rate} Hz within 3 s (current {applied:?})"
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}
