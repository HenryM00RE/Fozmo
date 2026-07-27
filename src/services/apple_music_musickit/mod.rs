//! Native Apple Music provider adapter.

mod ipc;
mod model;
mod music_app;
#[allow(dead_code)]
mod queue_generation;
mod queue_verification;
mod service;
mod source_format;

pub(crate) use model::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleMusicAlbumVersionRequest,
    AppleMusicAuthorizeRequest, AppleMusicCatalogQuery, AppleMusicCatalogSearchQuery,
    AppleMusicMvpError, AppleMusicMvpStatus, AppleMusicPlayRequest, AppleVerifiedFormat,
};
pub(crate) use music_app::{
    MusicAppSnapshot, pause as pause_music_app, play as play_music_app, play_queue_generation,
    prepare_bit_perfect as prepare_music_app, set_position as set_music_app_position,
    status as music_app_status, wait_for_player_notification as wait_for_music_app_notification,
};
pub(crate) use service::AppleMusicService;
