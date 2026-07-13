//! Small compatibility surface used by the imported GPL transport sources.
//!
//! These are helper-local playback primitives, not Fozmo server types.

use ringbuf::{Consumer, Producer, SharedRb};
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};

pub type AudioConsumer = Consumer<f64, Arc<SharedRb<f64, Vec<MaybeUninit<f64>>>>>;
pub type AudioProducer = Producer<f64, Arc<SharedRb<f64, Vec<MaybeUninit<f64>>>>>;

pub const PLAYING: u32 = 1;
pub const PAUSED: u32 = 2;

pub struct AtomicPlayerState {
    pub exclusive: AtomicBool,
    pub target_rate: AtomicU32,
    pub target_bits: AtomicU32,
    pub flush_buffer: AtomicBool,
    pub state: AtomicU32,
    pub underrun_events: AtomicU64,
    pub underrun_samples: AtomicU64,
    pub volume: AtomicU32,
    pub dither_mode: AtomicU32,
    pub meter_l: AtomicU32,
    pub meter_r: AtomicU32,
    pub position_samples: AtomicU64,
}

impl AtomicPlayerState {
    pub fn new() -> Self {
        Self {
            exclusive: AtomicBool::new(false),
            target_rate: AtomicU32::new(44_100),
            target_bits: AtomicU32::new(16),
            flush_buffer: AtomicBool::new(false),
            state: AtomicU32::new(PLAYING),
            underrun_events: AtomicU64::new(0),
            underrun_samples: AtomicU64::new(0),
            // The MIT client applies Fozmo's software volume before the PCM
            // boundary. The helper must not apply it a second time.
            volume: AtomicU32::new(1.0f32.to_bits()),
            dither_mode: AtomicU32::new(DitherPreference::Off as u32),
            meter_l: AtomicU32::new(0.0f32.to_bits()),
            meter_r: AtomicU32::new(0.0f32.to_bits()),
            position_samples: AtomicU64::new(0),
        }
    }

    pub fn playing() -> Arc<Self> {
        Arc::new(Self::new())
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum DitherPreference {
    Auto = 0,
    Tpdf = 1,
    Off = 2,
}

impl DitherPreference {
    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            0 => Some(Self::Auto),
            1 => Some(Self::Tpdf),
            2 => Some(Self::Off),
            _ => None,
        }
    }
}

pub struct DitherState {
    state: u64,
}

impl DitherState {
    pub fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    pub fn tpdf(&mut self) -> f64 {
        fn next(state: &mut u64) -> f64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            (*state as f64) / (u64::MAX as f64)
        }
        next(&mut self.state) - next(&mut self.state)
    }
}

#[derive(Clone)]
pub struct ArtworkData {
    pub mime: String,
    pub data: Vec<u8>,
}
