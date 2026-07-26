//! Native Apple Music provider adapter.

mod ipc;
mod model;
mod music_app;
mod service;
mod source_format;

pub(crate) use model::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleMusicAlbumVersionRequest,
    AppleMusicAuthorizeRequest, AppleMusicCatalogQuery, AppleMusicCatalogSearchQuery,
    AppleMusicMvpError, AppleMusicMvpStatus, AppleMusicPlayRequest, AppleVerifiedFormat,
};
pub(crate) use music_app::{
    MusicAppSnapshot, activate_catalog_track, pause as pause_music_app, play as play_music_app,
    play_current_in_context as play_music_app_current_in_context,
    play_current_once as play_music_app_current_once, prepare_bit_perfect as prepare_music_app,
    set_position as set_music_app_position, status as music_app_status,
    wait_for_player_notification as wait_for_music_app_notification,
};
pub(crate) use service::AppleMusicService;
