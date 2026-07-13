//! MIT client for the standalone GPL AirPlay helper.
//!
//! This module performs no AirPlay networking. It converts Fozmo's internal
//! floating-point stream to the documented IPC PCM format and sends only
//! versioned control messages plus PCM over Unix-domain sockets.

use super::AirPlayTarget;
#[cfg(feature = "airplay_helper")]
use super::helper_client;
#[cfg(all(unix, feature = "airplay_helper"))]
use super::{AIRPLAY_BIT_DEPTH, AIRPLAY_SAMPLE_RATE, pcm};
#[cfg(all(unix, feature = "airplay_helper"))]
use crate::audio::dsp::dither::{DitherPreference, DitherState};
use crate::audio::engine::player::{AtomicPlayerState, AudioConsumer, TrackCover, TrackTags};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use fozmo_airplay_protocol::{Artwork, Metadata, PCM_CHANNELS};
#[cfg(feature = "airplay_helper")]
use fozmo_airplay_protocol::{Command, PCM_SAMPLE_RATE, ResponsePayload};
#[cfg(all(unix, feature = "airplay_helper"))]
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
#[cfg(all(unix, feature = "airplay_helper"))]
use std::thread;
#[cfg(all(unix, feature = "airplay_helper"))]
use std::time::{Duration, Instant};

const FRAME_SIZE: usize = 352;
const CHANNELS: usize = PCM_CHANNELS as usize;

#[derive(Clone, Default)]
pub struct AirPlayMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub artwork: Option<TrackCover>,
}

impl AirPlayMetadata {
    pub fn from_player(
        file_name: Option<String>,
        tags: TrackTags,
        cover: Option<TrackCover>,
    ) -> Self {
        Self {
            title: tags.title.or(file_name),
            artist: tags.artist,
            album: tags.album,
            artwork: cover,
        }
    }

    fn protocol_metadata(&self) -> Metadata {
        Metadata {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            artwork: self.artwork.as_ref().map(|artwork| Artwork {
                mime: artwork.mime.clone(),
                data_base64: STANDARD.encode(&artwork.data),
            }),
        }
    }
}

enum StreamCommand {
    SetVolume(f32),
    SetMetadata(AirPlayMetadata),
    Stop,
}

pub struct AirPlayStream {
    tx: mpsc::Sender<StreamCommand>,
    done: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
    volume_state: Arc<AtomicU32>,
}

impl AirPlayStream {
    pub fn set_volume(&self, volume: f32) {
        let volume = super::normalize_device_volume(volume).unwrap_or(0.0);
        self.volume_state.store(volume.to_bits(), Ordering::Relaxed);
        let _ = self.tx.send(StreamCommand::SetVolume(volume));
    }

    pub fn set_metadata(&self, metadata: AirPlayMetadata) {
        let _ = self.tx.send(StreamCommand::SetMetadata(metadata));
    }

    pub fn reset_requested(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

impl Drop for AirPlayStream {
    fn drop(&mut self) {
        let _ = self.tx.send(StreamCommand::Stop);
        self.done.store(true, Ordering::Relaxed);
    }
}

#[cfg(all(unix, feature = "airplay_helper"))]
pub fn open(
    target: AirPlayTarget,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
    metadata: AirPlayMetadata,
    volume_state: Arc<AtomicU32>,
    initial_volume: Option<f32>,
) -> Result<AirPlayStream, Box<dyn std::error::Error>> {
    if let Some(reason) = target.unsupported_reason() {
        return Err(reason.into());
    }
    let response = helper_client::request(Command::Open {
        receiver_id: target.id.clone(),
        metadata: metadata.protocol_metadata(),
        initial_volume,
    })?;
    let ResponsePayload::Opened {
        stream_id,
        pcm_socket,
    } = response
    else {
        return Err("AirPlay helper returned an unexpected open response".into());
    };
    let pcm_stream = match helper_client::connect_pcm(&pcm_socket, &stream_id) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = helper_client::request(Command::Close {
                stream_id: stream_id.clone(),
            });
            return Err(error.into());
        }
    };

    let (tx, rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let ended = Arc::new(AtomicBool::new(false));
    let thread_ended = Arc::clone(&ended);
    let target_id = target.id.clone();
    thread::Builder::new()
        .name(format!("AirPlayHelperStream-{}", target.name))
        .spawn(move || {
            if let Err(error) = run_stream(
                pcm_stream,
                &stream_id,
                &target_id,
                cons,
                state,
                rx,
                thread_done,
            ) {
                eprintln!("airplay helper stream ended with error: {error}");
            }
            let _ = helper_client::request(Command::Close { stream_id });
            thread_ended.store(true, Ordering::Relaxed);
        })?;

    Ok(AirPlayStream {
        tx,
        done,
        ended,
        volume_state,
    })
}

#[cfg(not(all(unix, feature = "airplay_helper")))]
pub fn open(
    _target: AirPlayTarget,
    _cons: AudioConsumer,
    _state: Arc<AtomicPlayerState>,
    _metadata: AirPlayMetadata,
    _volume_state: Arc<AtomicU32>,
    _initial_volume: Option<f32>,
) -> Result<AirPlayStream, Box<dyn std::error::Error>> {
    Err(super::AIRPLAY2_FEATURE_DISABLED_MESSAGE.into())
}

#[cfg(all(unix, feature = "airplay_helper"))]
fn run_stream(
    mut pcm_stream: std::os::unix::net::UnixStream,
    stream_id: &str,
    target_id: &str,
    mut cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
    rx: mpsc::Receiver<StreamCommand>,
    done: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut samples = vec![0.0f64; FRAME_SIZE * CHANNELS];
    let mut pcm_i16 = Vec::with_capacity(samples.len());
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    let mut dither_state = DitherState::new(target_dither_seed(target_id));
    let packet_duration = Duration::from_secs_f64(FRAME_SIZE as f64 / PCM_SAMPLE_RATE as f64);
    let mut next_packet_at = Instant::now();
    let mut was_playing = false;

    state.exclusive.store(false, Ordering::Relaxed);
    state
        .target_rate
        .store(AIRPLAY_SAMPLE_RATE, Ordering::Relaxed);
    state
        .target_bits
        .store(AIRPLAY_BIT_DEPTH as u32, Ordering::Relaxed);

    while !done.load(Ordering::Relaxed) {
        while let Ok(command) = rx.try_recv() {
            match command {
                StreamCommand::SetVolume(volume) => {
                    expect_ack(helper_client::request(Command::SetVolume {
                        stream_id: stream_id.to_string(),
                        volume,
                    })?)?;
                }
                StreamCommand::SetMetadata(metadata) => {
                    expect_ack(helper_client::request(Command::SetMetadata {
                        stream_id: stream_id.to_string(),
                        metadata: metadata.protocol_metadata(),
                    })?)?;
                }
                StreamCommand::Stop => return Ok(()),
            }
        }

        if state.flush_buffer.swap(false, Ordering::Relaxed) {
            cons.clear();
            expect_ack(helper_client::request(Command::Flush {
                stream_id: stream_id.to_string(),
            })?)?;
        }

        let playing = state.state.load(Ordering::Relaxed) == 1;
        if !playing {
            if was_playing {
                expect_ack(helper_client::request(Command::Pause {
                    stream_id: stream_id.to_string(),
                })?)?;
                was_playing = false;
            }
            next_packet_at = Instant::now();
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if !was_playing {
            expect_ack(helper_client::request(Command::Resume {
                stream_id: stream_id.to_string(),
            })?)?;
            was_playing = true;
            next_packet_at = Instant::now();
        }

        let read = cons.pop_slice(&mut samples);
        if read < samples.len() {
            let missing = (samples.len() - read) as u64;
            let previous = state.underrun_events.fetch_add(1, Ordering::Relaxed);
            state.underrun_samples.fetch_add(missing, Ordering::Relaxed);
            for sample in &mut samples[read..] {
                *sample = 0.0;
            }
            if previous == 0 || (previous + 1).is_power_of_two() {
                eprintln!(
                    "airplay helper: underrun #{}, missing {} samples",
                    previous + 1,
                    missing
                );
            }
        }

        let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
        let dither = DitherPreference::from_id(state.dither_mode.load(Ordering::Relaxed))
            .unwrap_or(DitherPreference::Auto);
        let (max_l, max_r) = pcm::quantize_interleaved_i16(
            &samples,
            volume,
            dither,
            &mut dither_state,
            &mut pcm_i16,
        );
        state
            .meter_l
            .store((max_l as f32).to_bits(), Ordering::Relaxed);
        state
            .meter_r
            .store((max_r as f32).to_bits(), Ordering::Relaxed);

        bytes.clear();
        for sample in &pcm_i16 {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        pcm_stream.write_all(&bytes)?;
        state
            .position_samples
            .fetch_add(FRAME_SIZE as u64, Ordering::Relaxed);

        next_packet_at += packet_duration;
        let now = Instant::now();
        if next_packet_at > now {
            thread::sleep(next_packet_at - now);
        } else if now.duration_since(next_packet_at) > Duration::from_millis(100) {
            next_packet_at = now;
        }
    }
    Ok(())
}

#[cfg(feature = "airplay_helper")]
fn expect_ack(response: ResponsePayload) -> Result<(), Box<dyn std::error::Error>> {
    if matches!(
        response,
        ResponsePayload::Ack | ResponsePayload::Volume { .. }
    ) {
        Ok(())
    } else {
        Err("AirPlay helper returned an unexpected command response".into())
    }
}

fn target_dither_seed(target_id: &str) -> u64 {
    let mut seed = 0xcbf2_9ce4_8422_2325u64;
    for byte in target_id.bytes() {
        seed ^= byte as u64;
        seed = seed.wrapping_mul(0x100_0000_01b3);
    }
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_crosses_boundary_as_standalone_protocol_data() {
        let metadata = AirPlayMetadata {
            title: Some("Title".into()),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            artwork: Some(TrackCover {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }),
        }
        .protocol_metadata();
        assert_eq!(metadata.title.as_deref(), Some("Title"));
        assert_eq!(
            metadata
                .artwork
                .as_ref()
                .map(|art| art.data_base64.as_str()),
            Some("AQID")
        );
    }

    #[test]
    fn dither_seed_depends_only_on_opaque_receiver_id() {
        assert_eq!(
            target_dither_seed("receiver-a"),
            target_dither_seed("receiver-a")
        );
        assert_ne!(
            target_dither_seed("receiver-a"),
            target_dither_seed("receiver-b")
        );
    }
}
