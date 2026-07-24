//! Native Apple Music helper proof.
//!
//! This module deliberately stops at the first implementation gate: launching
//! a real MusicKit application bundle, authenticating a private local IPC
//! channel, and coordinating a single helper-owned playback session. PCM
//! capture and playback-router integration are separate follow-on gates.

mod ipc;
#[path = "../apple_music/live_source.rs"]
mod live_source;
mod model;
mod music_app;
mod process_tap;
mod service;

pub(crate) use model::{
    AppleMusicAuthorizeRequest, AppleMusicComparisonSwitchRequest, AppleMusicDevPlaySongRequest,
    AppleMusicMvpError, AppleMusicMvpStatus, AppleMusicProcessTapStartRequest,
    AppleMusicTransportRequest,
};
pub(crate) use music_app::{
    MusicAppSnapshot, pause as pause_music_app, pause_and_status as pause_music_app_and_status,
    play as play_music_app, set_position as set_music_app_position,
    set_position_and_play as set_music_app_position_and_play, status as music_app_status,
};
pub(crate) use service::{AppleMusicComparisonReferenceState, AppleMusicService};
