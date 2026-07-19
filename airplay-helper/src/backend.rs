use crate::compat::{ArtworkData, AtomicPlayerState, AudioProducer, PAUSED, PLAYING};
use crate::{AirPlayTarget, ap2, raop};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use fozmo_airplay_protocol::Metadata;
use ringbuf::HeapRb;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

const RING_CAPACITY_SAMPLES: usize = 44_100 * 2 * 4;
const AIRPLAY2_TAIL_DRAIN: Duration = Duration::from_secs(2);

pub struct BackendSession {
    pub producer: Option<AudioProducer>,
    pub state: Arc<AtomicPlayerState>,
    pub alive: Arc<AtomicBool>,
    handle: BackendHandle,
}

enum BackendHandle {
    Raop(raop::AirPlayStream),
    AirPlay2(ap2::AirPlay2Stream),
}

impl BackendSession {
    pub fn open(
        target: AirPlayTarget,
        metadata: Metadata,
        initial_volume: Option<f32>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if let Some(reason) = target.unsupported_reason() {
            return Err(reason.into());
        }
        let ring = HeapRb::<f64>::new(RING_CAPACITY_SAMPLES);
        let (producer, consumer) = ring.split();
        let state = AtomicPlayerState::playing();
        let alive = Arc::new(AtomicBool::new(true));
        let airplay2 = target.prefers_airplay2_transport();
        let handle = if airplay2 {
            BackendHandle::AirPlay2(ap2::open(
                target,
                consumer,
                Arc::clone(&state),
                initial_volume,
            )?)
        } else {
            BackendHandle::Raop(raop::open(
                target,
                consumer,
                Arc::clone(&state),
                raop_metadata(metadata),
                Arc::new(AtomicU32::new(f32::NAN.to_bits())),
                initial_volume,
            )?)
        };
        Ok(Self {
            producer: Some(producer),
            state,
            alive,
            handle,
        })
    }

    pub fn pause(&self) {
        self.state.state.store(PAUSED, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.state.state.store(PLAYING, Ordering::Relaxed);
    }

    pub fn flush(&self) {
        self.state.flush_buffer.store(true, Ordering::Relaxed);
    }

    pub fn set_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        match &self.handle {
            BackendHandle::Raop(stream) => stream.set_volume(volume),
            BackendHandle::AirPlay2(stream) => stream.set_volume(volume),
        }
    }

    pub fn set_metadata(&self, metadata: Metadata) {
        if let BackendHandle::Raop(stream) = &self.handle {
            stream.set_metadata(raop_metadata(metadata));
        }
    }

    pub fn reset_requested(&self) -> bool {
        match &self.handle {
            BackendHandle::Raop(stream) => stream.reset_requested(),
            BackendHandle::AirPlay2(stream) => stream.reset_requested(),
        }
    }

    pub fn drain_buffered_tail(&self) {
        if matches!(self.handle, BackendHandle::AirPlay2(_)) {
            // The AirPlay 2 live sender owns a receiver jitter buffer beyond
            // this session's PCM ring. Keep standalone `play` alive long
            // enough for the final ALAC frames and render delay to drain.
            thread::sleep(AIRPLAY2_TAIL_DRAIN);
        }
    }
}

impl Drop for BackendSession {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

fn raop_metadata(metadata: Metadata) -> raop::AirPlayMetadata {
    raop::AirPlayMetadata {
        title: metadata.title,
        artist: metadata.artist,
        album: metadata.album,
        artwork: metadata.artwork.and_then(|artwork| {
            STANDARD
                .decode(artwork.data_base64)
                .ok()
                .map(|data| ArtworkData {
                    mime: artwork.mime,
                    data,
                })
        }),
    }
}
