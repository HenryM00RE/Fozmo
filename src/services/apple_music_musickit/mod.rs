//! Native Apple Music provider adapter.

mod ipc;
#[path = "../apple_music/live_source.rs"]
mod live_source;
mod model;
mod music_app;
mod process_tap;
mod service;
mod source_format;

pub(crate) use model::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleMusicAlbumVersionRequest,
    AppleMusicAuthorizeRequest, AppleMusicCaptureConfirmationRequest, AppleMusicCatalogQuery,
    AppleMusicCatalogSearchQuery, AppleMusicComparisonSwitchRequest, AppleMusicDevPlaySongRequest,
    AppleMusicMvpError, AppleMusicMvpStatus, AppleMusicPlayRequest,
    AppleMusicProcessTapStartRequest, AppleMusicTransportRequest, ApplePlaybackSnapshot,
    HelperMessage,
};
pub(crate) use music_app::{
    MusicAppSnapshot, activate_catalog_track, pause as pause_music_app,
    pause_and_status as pause_music_app_and_status, play as play_music_app,
    play_current_once as play_music_app_current_once, prepare_bit_perfect as prepare_music_app,
    set_position as set_music_app_position,
    set_position_and_play as set_music_app_position_and_play, status as music_app_status,
};
pub(crate) use process_tap::ProcessTapRateTransition;
pub(crate) use service::{AppleMusicComparisonReferenceState, AppleMusicService};
