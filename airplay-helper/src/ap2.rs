use super::{
    AIRPLAY_BIT_DEPTH, AIRPLAY_SAMPLE_RATE, AIRPLAY2_ACCESS_UNSUPPORTED_MESSAGE, AirPlayTarget, pcm,
};
use crate::compat::{AtomicPlayerState, AudioConsumer, DitherPreference, DitherState};
use ap2rs_audio::{AlacEncoder, LiveAudioDecoder, LiveFrameSender, LivePcmFrame};
use ap2rs_client::Connection;
use ap2rs_core::Device as Ap2Device;
use ap2rs_core::codec::{AudioCodec, AudioFormat, SampleRate};
use ap2rs_core::device::{DeviceId, Version};
use ap2rs_core::features::Features;
use ap2rs_core::stream::{PtpMode, StreamConfig, StreamType, TimingProtocol};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const CHANNELS: usize = 2;
const FRAME_SIZE: usize = 352;
const INITIAL_BUFFER_FRAMES: usize = AIRPLAY_SAMPLE_RATE as usize * 2;
const INITIAL_BUFFER_MAX_WAIT: Duration = Duration::from_secs(5);
const LIVE_QUEUE_CAPACITY: usize = 384;
const FEEDBACK_INTERVAL: Duration = Duration::from_secs(2);
const HOMEPOD_FOZMO_PIN: &str = "3939";

#[derive(Clone, Copy)]
enum EnqueueMode {
    Block,
    DropIfFull,
}

enum StreamCommand {
    SetVolume(f32),
    Stop,
}

pub struct AirPlay2Stream {
    tx: mpsc::Sender<StreamCommand>,
    done: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
}

impl AirPlay2Stream {
    pub fn set_volume(&self, volume: f32) {
        let volume = super::device_volume_to_transport_volume(volume);
        let _ = self.tx.send(StreamCommand::SetVolume(volume));
    }

    pub fn reset_requested(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

impl Drop for AirPlay2Stream {
    fn drop(&mut self) {
        let _ = self.tx.send(StreamCommand::Stop);
        self.done.store(true, Ordering::Relaxed);
    }
}

pub fn open(
    target: AirPlayTarget,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
    initial_volume: Option<f32>,
) -> Result<AirPlay2Stream, Box<dyn std::error::Error>> {
    if let Some(reason) = target.unsupported_reason() {
        return Err(reason.into());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("AirPlay2Runtime")
        .build()?;
    let device = build_device(&target).map_err(map_open_error)?;
    let config = streaming_config().map_err(map_open_error)?;
    let mut connection = runtime
        .block_on(async {
            let mut connection =
                Connection::connect_with_pin(device, config, HOMEPOD_FOZMO_PIN).await?;
            connection.set_render_delay_ms(500);
            connection.setup().await?;
            if let Some(initial_volume) = initial_volume {
                let _ = connection
                    .set_volume(super::device_volume_to_transport_volume(initial_volume))
                    .await;
            }
            Ok::<_, ap2rs_core::error::Error>(connection)
        })
        .map_err(map_open_error)?;

    let (sender, decoder) =
        LiveAudioDecoder::create_pair(AIRPLAY_SAMPLE_RATE, CHANNELS as u8, LIVE_QUEUE_CAPACITY);
    let (tx, rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let ended = Arc::new(AtomicBool::new(false));
    let thread_ended = Arc::clone(&ended);
    let target_id = target.id.clone();
    let target_name = target.name.clone();

    thread::Builder::new()
        .name(format!("AirPlay2Stream-{target_name}"))
        .spawn(move || {
            if let Err(e) = run_stream(
                runtime,
                &mut connection,
                sender,
                decoder,
                cons,
                state,
                rx,
                thread_done,
                &target_id,
            ) {
                eprintln!("airplay2: stream ended with error: {e}");
            }
            thread_ended.store(true, Ordering::Relaxed);
        })?;

    Ok(AirPlay2Stream { tx, done, ended })
}

// AirPlay 2 streaming owns the runtime, connection, buffers, and completion state in one worker.
#[allow(clippy::too_many_arguments)]
fn run_stream(
    runtime: tokio::runtime::Runtime,
    connection: &mut Connection,
    sender: LiveFrameSender,
    decoder: LiveAudioDecoder,
    mut cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
    rx: mpsc::Receiver<StreamCommand>,
    done: Arc<AtomicBool>,
    target_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut samples = vec![0.0f64; FRAME_SIZE * CHANNELS];
    let mut pcm_i16 = Vec::with_capacity(FRAME_SIZE * CHANNELS);
    let mut dither_state = DitherState::new(target_dither_seed(target_id));

    state.exclusive.store(false, Ordering::Relaxed);
    state
        .target_rate
        .store(AIRPLAY_SAMPLE_RATE, Ordering::Relaxed);
    state
        .target_bits
        .store(AIRPLAY_BIT_DEPTH as u32, Ordering::Relaxed);

    prefill_initial_buffer(
        &runtime,
        connection,
        &sender,
        &mut cons,
        &state,
        &rx,
        &done,
        &mut samples,
        &mut pcm_i16,
        &mut dither_state,
    )?;
    if done.load(Ordering::Relaxed) {
        let _ = runtime.block_on(connection.disconnect());
        return Ok(());
    }

    runtime
        .block_on(connection.start_streaming_live(decoder))
        .map_err(map_open_error)?;

    let mut next_feedback = Instant::now() + FEEDBACK_INTERVAL;
    // The AP2 library clocks RTP output internally; feed its live decoder at
    // audio rate so we do not drain the engine ring or drop queued frames.
    let packet_duration = Duration::from_secs_f64(FRAME_SIZE as f64 / AIRPLAY_SAMPLE_RATE as f64);
    let mut next_packet_at = Instant::now();
    let mut was_playing = state.state.load(Ordering::Relaxed) == 1;

    while !done.load(Ordering::Relaxed) {
        if handle_commands(&runtime, connection, &rx)? {
            return Ok(());
        }

        let now = Instant::now();
        if now >= next_feedback {
            let _ = runtime.block_on(connection.send_feedback());
            next_feedback = now + FEEDBACK_INTERVAL;
        }

        if state.flush_buffer.swap(false, Ordering::Relaxed) {
            cons.clear();
            let _ = runtime.block_on(connection.send_flush(0, 0));
        }

        let playing = state.state.load(Ordering::Relaxed) == 1;
        if !playing {
            if was_playing {
                let _ = runtime.block_on(connection.pause());
                was_playing = false;
            }
            next_packet_at = Instant::now();
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if !was_playing {
            let _ = runtime.block_on(connection.resume());
            was_playing = true;
            next_packet_at = Instant::now();
        }

        let read = cons.pop_slice(&mut samples);
        if read < samples.len() {
            let missing = (samples.len() - read) as u64;
            let previous = state.underrun_events.fetch_add(1, Ordering::Relaxed);
            state.underrun_samples.fetch_add(missing, Ordering::Relaxed);
            if previous == 0 || (previous + 1).is_power_of_two() {
                eprintln!(
                    "airplay2: underrun #{}, missing {} samples",
                    previous + 1,
                    missing
                );
            }
            for sample in &mut samples[read..] {
                *sample = 0.0;
            }
        }

        if !push_samples(
            &sender,
            &samples,
            &state,
            &mut dither_state,
            &mut pcm_i16,
            EnqueueMode::Block,
        ) {
            return Err("AirPlay 2 live sender disconnected".into());
        }

        next_packet_at += packet_duration;
        let now = Instant::now();
        if next_packet_at > now {
            thread::sleep(next_packet_at - now);
        } else if now.duration_since(next_packet_at) > Duration::from_millis(100) {
            next_packet_at = now;
        }
    }

    let _ = runtime.block_on(connection.disconnect());
    Ok(())
}

// Prefill shares the live stream worker state so the initial buffer matches steady-state playback.
#[allow(clippy::too_many_arguments)]
fn prefill_initial_buffer(
    runtime: &tokio::runtime::Runtime,
    connection: &mut Connection,
    sender: &LiveFrameSender,
    cons: &mut AudioConsumer,
    state: &Arc<AtomicPlayerState>,
    rx: &mpsc::Receiver<StreamCommand>,
    done: &Arc<AtomicBool>,
    samples: &mut [f64],
    pcm_i16: &mut Vec<i16>,
    dither_state: &mut DitherState,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut filled_frames = 0usize;

    while filled_frames < INITIAL_BUFFER_FRAMES && !done.load(Ordering::Relaxed) {
        if handle_commands(runtime, connection, rx)? {
            return Ok(());
        }
        if state.flush_buffer.swap(false, Ordering::Relaxed) {
            cons.clear();
            let _ = runtime.block_on(connection.send_flush(0, 0));
        }
        if started.elapsed() >= INITIAL_BUFFER_MAX_WAIT {
            break;
        }
        if state.state.load(Ordering::Relaxed) != 1 {
            thread::sleep(Duration::from_millis(20));
            continue;
        }

        let read = cons.pop_slice(samples);
        let read = read & !1;
        if read == 0 {
            thread::sleep(Duration::from_millis(2));
            continue;
        }

        if !push_samples(
            sender,
            &samples[..read],
            state,
            dither_state,
            pcm_i16,
            EnqueueMode::DropIfFull,
        ) {
            break;
        }
        filled_frames += read / CHANNELS;
    }

    Ok(())
}

fn handle_commands(
    runtime: &tokio::runtime::Runtime,
    connection: &mut Connection,
    rx: &mpsc::Receiver<StreamCommand>,
) -> Result<bool, Box<dyn std::error::Error>> {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            StreamCommand::SetVolume(volume) => {
                runtime
                    .block_on(connection.set_volume(volume.clamp(0.0, 1.0)))
                    .map_err(map_open_error)?;
            }
            StreamCommand::Stop => {
                let _ = runtime.block_on(connection.disconnect());
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn push_samples(
    sender: &LiveFrameSender,
    samples: &[f64],
    state: &AtomicPlayerState,
    dither_state: &mut DitherState,
    pcm_i16: &mut Vec<i16>,
    mode: EnqueueMode,
) -> bool {
    let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
    let dither = DitherPreference::from_id(state.dither_mode.load(Ordering::Relaxed))
        .unwrap_or(DitherPreference::Auto);
    let (max_l, max_r) =
        pcm::quantize_interleaved_i16(samples, volume, dither, dither_state, pcm_i16);
    state
        .meter_l
        .store((max_l as f32).to_bits(), Ordering::Relaxed);
    state
        .meter_r
        .store((max_r as f32).to_bits(), Ordering::Relaxed);

    let frames = pcm_i16.len() / CHANNELS;
    if frames == 0 {
        return true;
    }
    let frame = LivePcmFrame {
        samples: pcm_i16.clone(),
        channels: CHANNELS as u8,
        sample_rate: AIRPLAY_SAMPLE_RATE,
    };
    let sent = match mode {
        EnqueueMode::Block => sender.send(frame),
        EnqueueMode::DropIfFull => sender.try_send(frame),
    };
    if !sent {
        return false;
    }
    state
        .position_samples
        .fetch_add(frames as u64, Ordering::Relaxed);
    true
}

fn streaming_config() -> Result<StreamConfig, String> {
    let audio_format = AudioFormat {
        codec: AudioCodec::Alac,
        sample_rate: SampleRate::Hz44100,
        bit_depth: AIRPLAY_BIT_DEPTH,
        channels: CHANNELS as u8,
        frames_per_packet: FRAME_SIZE as u32,
    };
    let asc = AlacEncoder::new(audio_format)
        .map_err(|e| e.to_string())?
        .magic_cookie();
    Ok(StreamConfig {
        stream_type: StreamType::Realtime,
        audio_format,
        timing_protocol: TimingProtocol::Ntp,
        ptp_mode: PtpMode::Master,
        latency_min: 22_050,
        latency_max: 132_300,
        supports_dynamic_stream_id: true,
        asc: Some(asc),
    })
}

fn build_device(target: &AirPlayTarget) -> Result<Ap2Device, String> {
    let ip = target
        .host
        .trim_matches(|ch| ch == '[' || ch == ']')
        .parse::<IpAddr>()
        .map_err(|e| format!("invalid AirPlay 2 address '{}': {e}", target.host))?;
    let device_id = target
        .device_id
        .as_deref()
        .unwrap_or(target.id.as_str())
        .to_string();
    let mac = if DeviceId::from_mac_string(&device_id).is_ok() {
        device_id
    } else {
        synthetic_mac(ip)
    };
    let id = DeviceId::from_mac_string(&mac).map_err(|e| e.to_string())?;
    let features = Features::from_txt_value(
        target
            .features
            .as_deref()
            .unwrap_or("0x4A7FCA00,0x3C354BD0"),
    )
    .map_err(|e| e.to_string())?;
    let source_version = target
        .source_version
        .as_deref()
        .map(Version::parse)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    Ok(Ap2Device {
        id,
        name: target.name.clone(),
        model: target
            .model
            .clone()
            .unwrap_or_else(|| "AudioAccessory5,1".to_string()),
        manufacturer: None,
        serial_number: None,
        addresses: vec![ip],
        port: target.port,
        features,
        required_sender_features: None,
        public_key: None,
        source_version,
        firmware_version: None,
        os_version: None,
        protocol_version: None,
        requires_password: target.password_protected,
        status_flags: 0,
        access_control: None,
        pairing_identity: None,
        system_pairing_identity: None,
        bluetooth_address: None,
        homekit_home_id: None,
        group_id: None,
        is_group_leader: false,
        group_public_name: None,
        group_contains_discoverable_leader: false,
        home_group_id: None,
        household_id: None,
        parent_group_id: None,
        parent_group_contains_discoverable_leader: false,
        tight_sync_id: None,
        raop_port: None,
        raop_encryption_types: None,
        raop_codecs: None,
        raop_transport: None,
        raop_metadata_types: None,
        raop_digest_auth: false,
        vodka_version: None,
    })
}

fn synthetic_mac(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            format!(
                "02:00:{:02X}:{:02X}:{:02X}:{:02X}",
                octets[0], octets[1], octets[2], octets[3]
            )
        }
        IpAddr::V6(_) => "02:00:00:00:00:01".to_string(),
    }
}

pub fn map_open_error_message(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("unexpected status code: 401")
        || lower.contains("unexpected status code: 403")
        || lower.contains("failed with 401")
        || lower.contains("failed with 403")
        || lower.contains("invalid pin")
        || lower.contains("pairing rejected")
        || lower.contains("pairing error: rejected")
    {
        AIRPLAY2_ACCESS_UNSUPPORTED_MESSAGE.to_string()
    } else {
        message.to_string()
    }
}

fn map_open_error(error: impl ToString) -> String {
    map_open_error_message(&error.to_string())
}

fn target_dither_seed(target_id: &str) -> u64 {
    let mut seed = 0xcbf2_9ce4_8422_2325u64;
    for byte in target_id.as_bytes() {
        seed ^= *byte as u64;
        seed = seed.wrapping_mul(0x100_0000_01b3);
    }
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_auth_failures_to_home_access_guidance() {
        for message in [
            "RTSP error: Unexpected status code: 401",
            "RTSP error: Unexpected status code: 403",
            "Pairing error: Invalid PIN",
            "Pairing rejected by device",
        ] {
            assert_eq!(
                map_open_error_message(message),
                AIRPLAY2_ACCESS_UNSUPPORTED_MESSAGE
            );
        }
    }

    #[test]
    fn leaves_non_auth_errors_intact() {
        assert_eq!(
            map_open_error_message("connection refused"),
            "connection refused"
        );
    }
}
