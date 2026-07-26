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
    QUEUE_PLAYLIST_NAME,
};
pub(crate) use music_app::{
    MusicAppSnapshot, delete_queue_playlist, pause as pause_music_app, play as play_music_app,
    play_queue_playlist, prepare_bit_perfect as prepare_music_app, queue_playlist_track_count,
    queue_playlist_track_keys, set_position as set_music_app_position, status as music_app_status,
    wait_for_player_notification as wait_for_music_app_notification,
};
pub(crate) use service::AppleMusicService;
