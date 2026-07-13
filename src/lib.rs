#[cfg(all(debug_assertions, target_os = "macos"))]
mod rt_allocator;

#[cfg(all(debug_assertions, target_os = "macos"))]
#[global_allocator]
static DEBUG_RT_ALLOCATOR: rt_allocator::DetectingAllocator = rt_allocator::DetectingAllocator;

pub mod audio {
    pub(crate) mod debug;
    pub mod dsd;
    pub mod dsp;
    pub mod engine;
    pub mod output;
    pub mod sinks;
    pub mod transcode;
    // Compatibility facade for older audio call sites; remove entries after imports migrate.
    #[allow(unused_imports)]
    pub use dsd::{delta_sigma, dop, dsd_coeffs, dsd_render, native_dsd};
    pub use dsp::{dither, eq, resampler};
    pub use engine::player;
    #[cfg(all(target_os = "windows", feature = "asio"))]
    pub use output::asio_output;
    #[cfg(target_os = "macos")]
    // Compatibility facade for direct CoreAudio hog-mode experiments.
    #[allow(unused_imports)]
    pub use output::coreaudio_hog;
    #[cfg(target_os = "windows")]
    pub use output::wasapi_exclusive;
    pub use output::{device_caps, device_volume};
    pub use sinks::{airplay, sonos, upnp};
}

pub mod api;
pub mod app;
mod cpu;
mod diagnostics;
pub mod error;
mod web {
    pub mod ws;
}

mod agent;
mod library;
mod listening;
mod playback;
mod protocol;
mod secrets;
mod services;
mod settings;
mod zones;
