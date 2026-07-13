use super::{auth_token_from_headers, internal_error};
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::audio::player::TrackCover;
use crate::library::{
    AlbumDetail, AlbumEdit, AlbumPlayResolveRequest, AlbumSummary, AlbumVersionSummary,
    AutoMetaItemsQuery, AutoMetaRunRequest, CanonicalAlbum, CanonicalTrack, LibraryScanProgress,
    MAX_ARTWORK_BYTES, ManualQobuzVersionRequest, ManualSearchRequest, MatchCandidate,
    MatchRequest, MbidLookupRequest, MetaBrainzTestRequest, MetaBrainzTestResponse,
    QobuzLinkRequest, QobuzMatchAssessment, QobuzMatchResponse, QobuzMatchTestCandidate,
    QobuzMatchTestResponse, QobuzTrackLinkSummary, TrackSummary,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Extension, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Serialize;
use serde_json::json;
use std::net::SocketAddr;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/library/albums/:id",
            get(library_album_detail).put(library_update_album),
        )
        .route(
            "/api/library/albums/:id/versions/qobuz",
            post(library_add_qobuz_version),
        )
        .route(
            "/api/library/albums/:id/versions/:version_id/primary",
            post(library_set_primary_version),
        )
        .route(
            "/api/library/albums/:id/qobuz/match",
            post(library_match_qobuz),
        )
        .route(
            "/api/library/albums/:id/qobuz/link",
            post(library_link_qobuz),
        )
        .route(
            "/api/library/albums/:id/qobuz/unlink",
            post(library_unlink_qobuz),
        )
        .route(
            "/api/library/albums/:id/qobuz/credits/refresh",
            post(library_refresh_qobuz_credits),
        )
        .route(
            "/api/library/albums/:id/play-sources",
            post(library_album_play_sources),
        )
        .route(
            "/api/library/qobuz-albums/:id",
            get(library_album_by_qobuz_id),
        )
        .route("/api/library/albums/:id/match", post(library_match_album))
        .route(
            "/api/library/albums/:id/metabrainz/test",
            post(library_test_metabrainz),
        )
        .route(
            "/api/library/albums/:id/metabrainz/qobuz/test",
            post(library_test_metabrainz_qobuz),
        )
        .route("/api/library/autometa/run", post(library_run_autometa))
        .route(
            "/api/library/autometa/progress",
            get(library_autometa_progress),
        )
        .route(
            "/api/library/autometa/status",
            get(library_autometa_progress),
        )
        .route(
            "/api/library/autometa/jobs",
            post(library_create_autometa_job),
        )
        .route(
            "/api/library/autometa/jobs/:id/pause",
            post(library_pause_autometa_job),
        )
        .route(
            "/api/library/autometa/jobs/:id/resume",
            post(library_resume_autometa_job),
        )
        .route(
            "/api/library/autometa/jobs/:id/stop",
            post(library_stop_autometa_job),
        )
        .route(
            "/api/library/autometa/jobs/:id/items",
            get(library_autometa_job_items),
        )
        .route("/api/library/autometa/audit", get(library_autometa_audit))
        .route("/api/library/albums/:id/reset", post(library_reset_album))
        .route(
            "/api/library/albums/:id/mark-reviewed",
            post(library_mark_reviewed),
        )
        .route(
            "/api/library/albums/:id/match/search",
            post(library_match_search),
        )
        .route(
            "/api/library/albums/:id/match/mbid",
            post(library_match_mbid),
        )
        .route(
            "/api/library/albums/:id/candidates/:cand_id/preview",
            get(library_candidate_preview),
        )
        .route(
            "/api/library/albums/:id/cover",
            post(library_upload_album_cover)
                .layer(DefaultBodyLimit::max(MAX_ARTWORK_BYTES + 1024 * 1024)),
        )
        .route(
            "/api/library/albums/:id/art/refresh",
            post(library_refresh_album_art),
        )
        .route("/api/library/art/refresh", post(library_refresh_all_art))
        .route("/api/library/rescan/status", get(library_rescan_status))
        .route("/api/library/rescan", post(library_rescan))
}

/// Remote-safe album detail routes. These support opening and playing albums
/// from the authenticated remote surface without exposing local library
/// mutation, file upload, matching, scanning, or autometa endpoints.
pub fn remote_routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/albums/:id", get(remote_library_album_detail))
        .route(
            "/api/library/albums/:id/play-sources",
            post(library_album_play_sources),
        )
}

#[derive(Debug, Serialize)]
struct RemoteAlbumDetail {
    album: AlbumSummary,
    tracks: Vec<TrackSummary>,
    candidates: Vec<MatchCandidate>,
    versions: Vec<RemoteAlbumVersionSummary>,
    canonical_album: Option<CanonicalAlbum>,
    canonical_tracks: Vec<CanonicalTrack>,
    qobuz_track_links: Vec<QobuzTrackLinkSummary>,
}

#[derive(Debug, Serialize)]
struct RemoteAlbumVersionSummary {
    id: i64,
    album_id: i64,
    provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_id: Option<String>,
    title: String,
    artist: Option<String>,
    year: Option<i32>,
    track_count: i64,
    art_id: Option<i64>,
    format: Option<String>,
    sample_rate: Option<i64>,
    bit_depth: Option<i64>,
    source_label: Option<String>,
    status: String,
    is_primary: bool,
    musicbrainz_match_status: Option<String>,
    musicbrainz_release_id: Option<String>,
    musicbrainz_tagged_at: Option<i64>,
    qobuz_match_status: Option<String>,
    qobuz_tagged_at: Option<i64>,
    autometa_message: Option<String>,
}

impl RemoteAlbumDetail {
    fn from_local(detail: AlbumDetail) -> Self {
        Self {
            album: detail.album,
            tracks: detail.tracks,
            candidates: detail.candidates,
            versions: detail
                .versions
                .into_iter()
                .map(RemoteAlbumVersionSummary::from_local)
                .collect(),
            canonical_album: detail.canonical_album,
            canonical_tracks: detail.canonical_tracks,
            qobuz_track_links: detail.qobuz_track_links,
        }
    }
}

impl RemoteAlbumVersionSummary {
    fn from_local(version: AlbumVersionSummary) -> Self {
        let provider_id = (version.provider != "local").then_some(version.provider_id);
        Self {
            id: version.id,
            album_id: version.album_id,
            provider: version.provider,
            provider_id,
            title: version.title,
            artist: version.artist,
            year: version.year,
            track_count: version.track_count,
            art_id: version.art_id,
            format: version.format,
            sample_rate: version.sample_rate,
            bit_depth: version.bit_depth,
            source_label: version.source_label,
            status: version.status,
            is_primary: version.is_primary,
            musicbrainz_match_status: version.musicbrainz_match_status,
            musicbrainz_release_id: version.musicbrainz_release_id,
            musicbrainz_tagged_at: version.musicbrainz_tagged_at,
            qobuz_match_status: version.qobuz_match_status,
            qobuz_tagged_at: version.qobuz_tagged_at,
            autometa_message: version.autometa_message,
        }
    }
}

async fn library_refresh_album_art(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .improve_album_art(id)
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_refresh_all_art(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let result = state
        .library()
        .improve_all_album_art()
        .await
        .map_err(internal_error)?;
    Ok(Json(result).into_response())
}

async fn library_album_detail(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    match state
        .library()
        .run_blocking(move |library| library.album_detail_for_profile(&profile_id, id))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn remote_library_album_detail(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    match state
        .library()
        .run_blocking(move |library| library.album_detail_for_profile(&profile_id, id))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(RemoteAlbumDetail::from_local(detail)).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_update_album(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<AlbumEdit>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .run_blocking(move |library| library.update_album(id, edit))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_add_qobuz_version(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ManualQobuzVersionRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .run_blocking(move |library| library.add_manual_qobuz_version(id, req))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_set_primary_version(
    State(state): State<AppState>,
    Path((album_id, version_id)): Path<(i64, i64)>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .run_blocking(move |library| library.set_primary_version(album_id, version_id))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_match_qobuz(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(current) = state
        .library()
        .run_blocking(move |library| library.album_detail(id))
        .await
        .map_err(internal_error)?
    else {
        return Err((StatusCode::NOT_FOUND, "Album not found".to_string()));
    };
    let artist = current.album.album_artist.as_deref().unwrap_or("");
    let query = if artist.is_empty() {
        current.album.title.clone()
    } else {
        format!("{} {}", artist, current.album.title)
    };
    let results = state
        .qobuz()
        .search_albums(&query)
        .await
        .map_err(internal_error)?;
    let mut best: Option<(
        crate::services::qobuz::QobuzAlbumDetail,
        crate::library::QobuzMatchAssessment,
    )> = None;
    for album in results.albums.into_iter().take(5) {
        let Ok(candidate) = state.qobuz().album_detail_basic(&album.id).await else {
            continue;
        };
        let assessment_candidate = candidate.clone();
        let Some(assessment) = state
            .library()
            .run_blocking(move |library| library.qobuz_link_assessment(id, &assessment_candidate))
            .await
            .map_err(internal_error)?
        else {
            return Err((StatusCode::NOT_FOUND, "Album not found".to_string()));
        };
        // An auto-linkable candidate beats any score; among equals, score wins.
        if best.as_ref().is_none_or(|(_, current)| {
            (assessment.auto_link, assessment.score) > (current.auto_link, current.score)
        }) {
            let stop = assessment.auto_link;
            best = Some((candidate, assessment));
            if stop {
                break;
            }
        }
    }

    let Some((candidate, assessment)) = best else {
        return Ok(Json(QobuzMatchResponse {
            album: current.album,
            matched: false,
            qobuz_album_id: None,
            score: 0,
            status: "not_found".to_string(),
        }));
    };

    if assessment.auto_link {
        let next =
            link_qobuz_album_and_refresh_art(&state, id, &candidate, assessment.score, "matched")
                .await?;
        Ok(Json(QobuzMatchResponse {
            album: next.album,
            matched: true,
            qobuz_album_id: Some(candidate.album.id),
            score: assessment.score,
            status: "matched".to_string(),
        }))
    } else {
        let qobuz_album_id = candidate.album.id.clone();
        let score = assessment.score;
        let next = state
            .library()
            .run_blocking(move |library| {
                library.mark_qobuz_candidate_for_review(id, &candidate, score)
            })
            .await
            .map_err(internal_error)?
            .ok_or_else(|| (StatusCode::NOT_FOUND, "Album not found".to_string()))?;
        Ok(Json(QobuzMatchResponse {
            album: next.album,
            matched: false,
            qobuz_album_id: Some(qobuz_album_id),
            score,
            status: "needs_review".to_string(),
        }))
    }
}

async fn library_link_qobuz(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<QobuzLinkRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let qobuz_id = req.qobuz_album_id.trim();
    if qobuz_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "qobuz_album_id is required".to_string(),
        ));
    }
    let detail = state
        .qobuz()
        .album_detail_basic(qobuz_id)
        .await
        .map_err(internal_error)?;
    let score = state
        .library()
        .run_blocking({
            let detail = detail.clone();
            move |library| library.qobuz_match_score(id, &detail)
        })
        .await
        .map_err(internal_error)?;
    let detail = link_qobuz_album_and_refresh_art(&state, id, &detail, score, "matched").await?;
    Ok(Json(detail))
}

async fn library_unlink_qobuz(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let detail = state
        .library()
        .run_blocking(move |library| library.unlink_qobuz_album(id))
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Album not found".to_string()))?;
    Ok(Json(detail))
}

async fn library_refresh_qobuz_credits(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(current) = state
        .library()
        .run_blocking(move |library| library.album_detail(id))
        .await
        .map_err(internal_error)?
    else {
        return Err((StatusCode::NOT_FOUND, "Album not found".to_string()));
    };
    let Some(qobuz_album_id) = current.album.qobuz_album_id.clone().or_else(|| {
        current
            .canonical_album
            .as_ref()
            .map(|a| a.qobuz_album_id.clone())
    }) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Album is not linked to Qobuz".to_string(),
        ));
    };

    let detail = state
        .qobuz()
        .album_detail(&qobuz_album_id)
        .await
        .map_err(internal_error)?;
    let score = current.album.qobuz_match_confidence.unwrap_or(100);
    let status = current
        .album
        .qobuz_match_status
        .as_deref()
        .unwrap_or("matched");
    let detail = link_qobuz_album_and_refresh_art(&state, id, &detail, score, status).await?;
    Ok(Json(detail))
}

async fn library_album_by_qobuz_id(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    match state
        .library()
        .run_blocking(move |library| library.album_by_qobuz_id_for_profile(&profile_id, &id))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Ok(Json(json!({ "album": null })).into_response()),
    }
}

async fn library_album_play_sources(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<AlbumPlayResolveRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let start_index = req.start_index.unwrap_or(0);
    let shuffle = req.shuffle;
    let version_id = req.version_id;
    let plan = state
        .library()
        .run_blocking(move |library| {
            library.resolve_album_playback(id, start_index, shuffle, version_id)
        })
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Album not found".to_string()))?;
    Ok(Json(plan))
}

async fn fetch_qobuz_album_cover(
    state: &AppState,
    detail: &crate::services::qobuz::QobuzAlbumDetail,
) -> Option<TrackCover> {
    let url = detail.album.image_url.as_deref()?;
    let (mime, data) = state.qobuz().fetch_cover_public(url).await.ok()?;
    match (mime, data) {
        (Some(mime), Some(data)) => Some(TrackCover { mime, data }),
        _ => None,
    }
}

async fn link_qobuz_album_and_refresh_art(
    state: &AppState,
    album_id: i64,
    detail: &crate::services::qobuz::QobuzAlbumDetail,
    score: i64,
    status: &str,
) -> Result<AlbumDetail, (StatusCode, String)> {
    let detail_for_link = detail.clone();
    let status = status.to_string();
    let linked = state
        .library()
        .run_blocking(move |library| {
            library.link_qobuz_album(album_id, &detail_for_link, None, score, &status)
        })
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Album not found".to_string()))?;

    let background_state = state.clone();
    let background_detail = detail.clone();
    tokio::spawn(async move {
        if let Some(cover) = fetch_qobuz_album_cover(&background_state, &background_detail).await
            && let Err(error) = background_state
                .library()
                .run_blocking(move |library| library.set_qobuz_album_art(album_id, &cover))
                .await
        {
            eprintln!("qobuz: background cover update failed: {error}");
        }
        if let Err(error) = background_state.library().improve_album_art(album_id).await {
            eprintln!("itunes: background cover upgrade after qobuz link failed: {error}");
        }
    });
    Ok(linked)
}

async fn library_upload_album_cover(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut data: Option<Vec<u8>> = None;
    let mut mime: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Multipart error: {:?}", e)))?
    {
        if field.name() == Some("cover") || field.file_name().is_some() {
            if let Some(ct) = field.content_type() {
                mime = Some(ct.to_string());
            }
            let bytes = field.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("Read cover bytes: {:?}", e),
                )
            })?;
            data = Some(bytes.to_vec());
            break;
        }
    }
    let Some(data) = data else {
        return Err((StatusCode::BAD_REQUEST, "Missing cover file".to_string()));
    };
    match state
        .library()
        .run_blocking(move |library| {
            library.set_album_cover(id, data, mime.as_deref().unwrap_or(""))
        })
        .await
        .map_err(album_cover_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

fn album_cover_error(error: String) -> (StatusCode, String) {
    if error.starts_with("Album art") || error.starts_with("Cover image") {
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, error)
    } else {
        internal_error(error)
    }
}

async fn library_rescan(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<(StatusCode, Json<LibraryScanProgress>), (StatusCode, String)> {
    let token = auth_token_from_headers(&headers);
    if !state.pairing().verify_control_token(token.as_deref())
        && !crate::app::auth::local_filesystem_request_allowed(
            &headers,
            peer.as_ref().map(|ConnectInfo(addr)| *addr),
        )
    {
        return Err((
            StatusCode::FORBIDDEN,
            "Library rescans are only available from the local control UI".to_string(),
        ));
    }

    if state.library().try_begin_scan() {
        let scan_state = state.clone();
        tokio::spawn(async move {
            let scan_library = scan_state.library().clone();
            let scan_result =
                tokio::task::spawn_blocking(move || scan_library.run_active_scan_files()).await;
            let result = match scan_result {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    eprintln!("library: rescan failed: {error}");
                    return;
                }
                Err(error) => {
                    scan_state
                        .library()
                        .fail_active_scan(&format!("Scan worker failed: {error}"));
                    eprintln!("library: rescan worker failed: {error}");
                    return;
                }
            };

            let qobuz_status = scan_state.qobuz().status().await;
            if qobuz_status.initialized {
                scan_state
                    .library()
                    .set_scan_progress_phase("matching", "Checking Qobuz album links");
                if let Err(e) = auto_match_qobuz_albums(&scan_state).await {
                    eprintln!("qobuz: auto-match after scan failed: {e}");
                }
            }
            scan_state.library().finish_active_scan(result);
        });
    }

    Ok((StatusCode::ACCEPTED, Json(state.library().scan_progress())))
}

async fn library_rescan_status(
    State(state): State<AppState>,
) -> Result<Json<LibraryScanProgress>, (StatusCode, String)> {
    Ok(Json(state.library().scan_progress()))
}

async fn auto_match_qobuz_albums(state: &AppState) -> Result<(), String> {
    let albums = state
        .library()
        .run_blocking(|library| library.albums())
        .await?;
    for album in albums.into_iter().filter(|a| a.qobuz_album_id.is_none()) {
        auto_match_qobuz_album(state, &album).await?;
    }
    Ok(())
}

/// Search Qobuz for one local album and either auto-link the canonical
/// version (strict evidence gate — see `qobuz_link_assessment`) or park the
/// best candidate for review. Silently skips albums Qobuz can't find.
async fn auto_match_qobuz_album(
    state: &AppState,
    album: &crate::library::AlbumSummary,
) -> Result<(), String> {
    let query = album
        .album_artist
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|artist| format!("{} {}", artist, album.title))
        .unwrap_or_else(|| album.title.clone());
    let Ok(results) = state.qobuz().search_albums(&query).await else {
        return Ok(());
    };
    let mut best: Option<(
        crate::services::qobuz::QobuzAlbumDetail,
        crate::library::QobuzMatchAssessment,
    )> = None;
    for candidate in results.albums.into_iter().take(3) {
        let Ok(detail) = state.qobuz().album_detail_basic(&candidate.id).await else {
            continue;
        };
        let detail_for_assessment = detail.clone();
        let album_id = album.id;
        let Some(assessment) = state
            .library()
            .run_blocking(move |library| {
                library.qobuz_link_assessment(album_id, &detail_for_assessment)
            })
            .await?
        else {
            return Ok(());
        };
        if best.as_ref().is_none_or(|(_, current)| {
            (assessment.auto_link, assessment.score) > (current.auto_link, current.score)
        }) {
            let stop = assessment.auto_link;
            best = Some((detail, assessment));
            if stop {
                break;
            }
        }
    }
    let Some((detail, assessment)) = best else {
        return Ok(());
    };
    if assessment.auto_link {
        let detail_for_link = detail.clone();
        let album_id = album.id;
        let score = assessment.score;
        let _ = state
            .library()
            .run_blocking(move |library| {
                library.link_qobuz_album(album_id, &detail_for_link, None, score, "matched")
            })
            .await?;
        let background_state = state.clone();
        let background_detail = detail.clone();
        let background_album_id = album.id;
        tokio::spawn(async move {
            if let Some(cover) =
                fetch_qobuz_album_cover(&background_state, &background_detail).await
            {
                let _ = background_state
                    .library()
                    .run_blocking(move |library| {
                        library.set_qobuz_album_art(background_album_id, &cover)
                    })
                    .await;
            }
            let _ = background_state
                .library()
                .improve_album_art(background_album_id)
                .await;
        });
    } else if assessment.score >= 70 && assessment.barcode_match != Some(false) {
        let detail_for_review = detail.clone();
        let album_id = album.id;
        let score = assessment.score;
        let _ = state
            .library()
            .run_blocking(move |library| {
                library.mark_qobuz_candidate_for_review(album_id, &detail_for_review, score)
            })
            .await?;
    }
    Ok(())
}

async fn library_match_album(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<MatchRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .match_album(id, req)
        .await
        .map_err(internal_error)?
    {
        Some(mut response) => {
            // A MusicBrainz apply cleans up title/artist/year — exactly what
            // the Qobuz matcher keys on — so re-run Qobuz matching while the
            // album is fresh. Existing confirmed links are left alone; only
            // unlinked or needs-review albums are reassessed.
            let qobuz_rematch = response.applied
                && response
                    .album
                    .qobuz_match_status
                    .as_deref()
                    .is_none_or(|status| status == "needs_review");
            if qobuz_rematch && state.qobuz().status().await.initialized {
                if let Err(e) = auto_match_qobuz_album(&state, &response.album).await {
                    eprintln!("qobuz: auto-match after MusicBrainz apply failed: {e}");
                }
                // Refresh the album summary so the response reflects any link.
                if let Some(detail) = state
                    .library()
                    .run_blocking(move |library| library.album_detail(id))
                    .await
                    .map_err(internal_error)?
                {
                    response.album = detail.album;
                }
            }
            Ok(Json(response).into_response())
        }
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_reset_album(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .run_blocking(move |library| library.reset_album_to_file_tags(id))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_test_metabrainz(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<MetaBrainzTestRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .test_metabrainz_album(id, req)
        .await
        .map_err(internal_error)?
    {
        Some(response) => Ok(Json(response).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_test_metabrainz_qobuz(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<MetaBrainzTestRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(mut response) = state
        .library()
        .test_metabrainz_album(id, req)
        .await
        .map_err(internal_error)?
    else {
        return Err((StatusCode::NOT_FOUND, "Album not found".to_string()));
    };
    response.qobuz_match = Some(simulate_qobuz_after_metabrainz(&state, &response).await?);
    Ok(Json(response).into_response())
}

async fn library_run_autometa(
    State(state): State<AppState>,
    Json(req): Json<AutoMetaRunRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    library_create_autometa_job(State(state), Json(req)).await
}

async fn library_create_autometa_job(
    State(state): State<AppState>,
    Json(req): Json<AutoMetaRunRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mode = req.mode;
    let link_qobuz = req.link_qobuz;
    let progress = state
        .library()
        .run_blocking(move |library| library.create_autometa_job(&mode, link_qobuz))
        .await
        .map_err(|error| {
            if error.contains("already") && error.contains("job") {
                (StatusCode::CONFLICT, error)
            } else {
                internal_error(error)
            }
        })?;
    if progress.status == "running" {
        spawn_autometa_worker(state.clone(), progress.job_id.unwrap_or_default());
    }
    Ok(Json(progress).into_response())
}

async fn library_autometa_progress(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    Ok(Json(state.library().autometa_progress()).into_response())
}

async fn library_pause_autometa_job(
    State(state): State<AppState>,
    Path(job_id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let progress = state
        .library()
        .run_blocking(move |library| library.set_autometa_job_status(job_id, "paused"))
        .await
        .map_err(internal_error)?;
    Ok(Json(progress).into_response())
}

async fn library_resume_autometa_job(
    State(state): State<AppState>,
    Path(job_id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (progress, should_spawn) = state
        .library()
        .run_blocking(move |library| library.resume_autometa_job(job_id))
        .await
        .map_err(|error| {
            if error.contains("not found") {
                (StatusCode::NOT_FOUND, error)
            } else if error.contains("Cannot resume") {
                (StatusCode::CONFLICT, error)
            } else {
                internal_error(error)
            }
        })?;
    if should_spawn {
        spawn_autometa_worker(state.clone(), job_id);
    }
    Ok(Json(progress).into_response())
}

async fn library_stop_autometa_job(
    State(state): State<AppState>,
    Path(job_id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let progress = state
        .library()
        .run_blocking(move |library| library.set_autometa_job_status(job_id, "stopping"))
        .await
        .map_err(internal_error)?;
    Ok(Json(progress).into_response())
}

async fn library_autometa_job_items(
    State(state): State<AppState>,
    Path(job_id): Path<i64>,
    Query(query): Query<AutoMetaItemsQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let status = query.status;
    let items = state
        .library()
        .run_blocking(move |library| library.autometa_job_items(job_id, status.as_deref()))
        .await
        .map_err(internal_error)?;
    Ok(Json(items).into_response())
}

async fn library_autometa_audit(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let issues = state
        .library()
        .run_blocking(|library| library.autometa_audit_issues())
        .await
        .map_err(internal_error)?;
    Ok(Json(issues).into_response())
}

fn spawn_autometa_worker(state: AppState, job_id: i64) {
    tokio::spawn(async move {
        if let Err(error) = run_autometa_worker(state.clone(), job_id).await
            && let Err(persist_error) = state.library().fail_autometa_job(job_id, &error)
        {
            eprintln!("autometa: failed to persist worker failure: {persist_error}");
            state.library().fail_autometa_progress(error);
        }
    });
}

async fn run_autometa_worker(state: AppState, job_id: i64) -> Result<(), String> {
    loop {
        if !wait_autometa_job_runnable(&state, job_id).await? {
            return Ok(());
        }
        let Some(work) = state.library().claim_autometa_work_item(job_id)? else {
            state.library().complete_autometa_job_if_done(job_id)?;
            return Ok(());
        };
        let item_id = work.item_id;
        let version = work.version;
        state
            .library()
            .set_autometa_current(&version.album_title, &version.version_label);
        let progress = state.library().autometa_job_progress(job_id)?;
        let link_qobuz = progress.link_qobuz;
        let mb_done = autometa_musicbrainz_done(&version);
        let qobuz_done = !link_qobuz || autometa_qobuz_done(&version);
        if mb_done && qobuz_done {
            state.library().finish_autometa_item(
                job_id,
                item_id,
                "skipped",
                "done",
                &format!("Skipped {}", version.version_label),
                version.musicbrainz_release_id.as_deref(),
                autometa_existing_qobuz_match(&version),
            )?;
            continue;
        }

        let mb_result = if mb_done {
            Some(crate::library::AutoMetaMusicBrainzResult {
                release_id: version.musicbrainz_release_id.clone().unwrap_or_default(),
            })
        } else {
            state
                .library()
                .update_autometa_item_phase(job_id, item_id, "musicbrainz")?;
            match state
                .library()
                .autometa_match_musicbrainz_version(version.album_id, version.version_id)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    let message = format!("AutoMetadata MusicBrainz error: {error}");
                    if let Err(status_error) = state.library().set_version_musicbrainz_status(
                        version.version_id,
                        "error",
                        None,
                        Some(&message),
                    ) {
                        eprintln!(
                            "autometa: failed to record MusicBrainz error for version {}: {status_error}",
                            version.version_id
                        );
                    }
                    state.library().finish_autometa_item(
                        job_id,
                        item_id,
                        "error",
                        "musicbrainz",
                        &format!("MusicBrainz error for {}: {error}", version.version_label),
                        None,
                        None,
                    )?;
                    continue;
                }
            }
        };

        let Some(mb_result) = mb_result else {
            state.library().set_version_musicbrainz_status(
                version.version_id,
                "needs_review",
                None,
                Some("No safe MusicBrainz match"),
            )?;
            state.library().finish_autometa_item(
                job_id,
                item_id,
                "needs_review",
                "musicbrainz",
                &format!("No MusicBrainz match for {}", version.version_label),
                None,
                None,
            )?;
            continue;
        };

        state.library().set_version_musicbrainz_status(
            version.version_id,
            "matched",
            Some(&mb_result.release_id),
            Some("MusicBrainz matched"),
        )?;

        if !link_qobuz {
            state.library().finish_autometa_item(
                job_id,
                item_id,
                "matched",
                "done",
                &format!("Matched {}", version.version_label),
                Some(&mb_result.release_id),
                None,
            )?;
            continue;
        }

        if let Some(qobuz_id) = autometa_existing_qobuz_match(&version) {
            state.library().set_version_qobuz_status(
                version.version_id,
                "matched",
                Some(&format!("Qobuz already matched {qobuz_id}")),
            )?;
            state.library().finish_autometa_item(
                job_id,
                item_id,
                "matched",
                "done",
                format!(
                    "Matched {} + existing Qobuz {}",
                    version.version_label, qobuz_id
                )
                .as_str(),
                Some(&mb_result.release_id),
                Some(qobuz_id),
            )?;
            continue;
        }

        if !version.is_primary_version {
            state.library().set_version_qobuz_status(
                version.version_id,
                "needs_review",
                Some("AutoMetadata only auto-links Qobuz from the primary local version"),
            )?;
            state.library().finish_autometa_item(
                job_id,
                item_id,
                "needs_review",
                "qobuz",
                &format!(
                    "Qobuz needs review for non-primary version {}",
                    version.version_label
                ),
                Some(&mb_result.release_id),
                None,
            )?;
            continue;
        }

        state
            .library()
            .update_autometa_item_phase(job_id, item_id, "qobuz")?;
        match autometa_link_top_qobuz(&state, version.album_id, version.version_id).await {
            Ok(AutoMetaQobuzResult::Matched { qobuz_id, .. }) => {
                state.library().set_version_qobuz_status(
                    version.version_id,
                    "matched",
                    Some(&format!("Qobuz matched {qobuz_id}")),
                )?;
                state.library().finish_autometa_item(
                    job_id,
                    item_id,
                    "matched",
                    "done",
                    &format!("Matched {} + Qobuz {}", version.version_label, qobuz_id),
                    Some(&mb_result.release_id),
                    Some(&qobuz_id),
                )?;
            }
            Ok(AutoMetaQobuzResult::NeedsReview {
                qobuz_id,
                score,
                reason,
            }) => {
                state.library().set_version_qobuz_status(
                    version.version_id,
                    "needs_review",
                    Some(&format!("{reason} for {qobuz_id}")),
                )?;
                state.library().finish_autometa_item(
                    job_id,
                    item_id,
                    "needs_review",
                    "qobuz",
                    format!(
                        "Qobuz needs review for {} (score {score})",
                        version.version_label
                    )
                    .as_str(),
                    Some(&mb_result.release_id),
                    None,
                )?;
            }
            Ok(AutoMetaQobuzResult::NotFound) => {
                state.library().set_version_qobuz_status(
                    version.version_id,
                    "needs_review",
                    Some("No Qobuz album found"),
                )?;
                state.library().finish_autometa_item(
                    job_id,
                    item_id,
                    "needs_review",
                    "qobuz",
                    &format!("No Qobuz match for {}", version.version_label),
                    Some(&mb_result.release_id),
                    None,
                )?;
            }
            Err(error) => {
                let message = format!("AutoMetadata Qobuz error: {error}");
                if let Err(status_error) = state.library().set_version_qobuz_status(
                    version.version_id,
                    "error",
                    Some(&message),
                ) {
                    eprintln!(
                        "autometa: failed to record Qobuz error for version {}: {status_error}",
                        version.version_id
                    );
                }
                state.library().finish_autometa_item(
                    job_id,
                    item_id,
                    "error",
                    "qobuz",
                    &format!("Qobuz error for {}: {error}", version.version_label),
                    Some(&mb_result.release_id),
                    None,
                )?;
            }
        }
    }
}

async fn wait_autometa_job_runnable(state: &AppState, job_id: i64) -> Result<bool, String> {
    loop {
        match state.library().autometa_job_status(job_id)?.as_deref() {
            Some("running") => return Ok(true),
            Some("paused") => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            Some("stopping") => {
                state
                    .library()
                    .stop_autometa_job_after_current_item(job_id)?;
                return Ok(false);
            }
            _ => return Ok(false),
        }
    }
}

fn autometa_musicbrainz_done(version: &crate::library::AutoMetaLocalVersion) -> bool {
    version.musicbrainz_match_status.as_deref() == Some("matched")
        && version
            .musicbrainz_release_id
            .as_deref()
            .is_some_and(crate::library::is_valid_musicbrainz_release_id)
}

#[cfg(test)]
fn autometa_version_done(version: &crate::library::AutoMetaLocalVersion, link_qobuz: bool) -> bool {
    autometa_musicbrainz_done(version) && (!link_qobuz || autometa_qobuz_done(version))
}

fn autometa_qobuz_done(version: &crate::library::AutoMetaLocalVersion) -> bool {
    version.qobuz_match_status.as_deref() == Some("matched")
        || autometa_existing_qobuz_match(version).is_some()
}

fn autometa_existing_qobuz_match(version: &crate::library::AutoMetaLocalVersion) -> Option<&str> {
    if version.album_qobuz_match_status.as_deref() != Some("matched") {
        return None;
    }
    version
        .album_qobuz_album_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
enum AutoMetaQobuzResult {
    Matched {
        qobuz_id: String,
        score: i64,
    },
    NeedsReview {
        qobuz_id: String,
        score: i64,
        reason: String,
    },
    NotFound,
}

/// AutoMetadata may accept a fuzzy Qobuz album match at this score. A known
/// barcode conflict remains an absolute veto so similarly named editions do
/// not become linked when the providers disagree on release identity.
const AUTOMETA_QOBUZ_AUTO_LINK_MIN_SCORE: i64 = 50;

fn autometa_qobuz_auto_link_eligible(assessment: &QobuzMatchAssessment) -> bool {
    assessment.score >= AUTOMETA_QOBUZ_AUTO_LINK_MIN_SCORE
        && assessment.barcode_match != Some(false)
}

async fn autometa_link_top_qobuz(
    state: &AppState,
    album_id: i64,
    version_id: i64,
) -> Result<AutoMetaQobuzResult, String> {
    let Some(current) = state.library().album_detail(album_id)? else {
        return Ok(AutoMetaQobuzResult::NotFound);
    };
    let artist = current.album.album_artist.as_deref().unwrap_or("");
    let query = if artist.is_empty() {
        current.album.title.clone()
    } else {
        format!("{} {}", artist, current.album.title)
    };
    let results = match state.qobuz().search_albums(&query).await {
        Ok(results) => results,
        Err(error) if qobuz_lookup_not_found(&error) => return Ok(AutoMetaQobuzResult::NotFound),
        Err(error) => return Err(error),
    };
    let mut best: Option<(
        crate::services::qobuz::QobuzAlbumDetail,
        crate::library::QobuzMatchAssessment,
    )> = None;
    for album in results.albums.into_iter().take(5) {
        let detail = match state.qobuz().album_detail_basic(&album.id).await {
            Ok(detail) => detail,
            Err(error) if qobuz_lookup_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        let Some(assessment) = state
            .library()
            .qobuz_link_assessment_for_version(album_id, version_id, &detail)?
        else {
            return Ok(AutoMetaQobuzResult::NotFound);
        };
        if best.as_ref().is_none_or(|(_, current)| {
            (
                autometa_qobuz_auto_link_eligible(&assessment),
                assessment.auto_link,
                assessment.score,
            ) > (
                autometa_qobuz_auto_link_eligible(current),
                current.auto_link,
                current.score,
            )
        }) {
            // Preserve the existing preference for strict evidence. Once a
            // strict candidate also clears the score floor, no weaker fuzzy
            // candidate can outrank it.
            let stop = assessment.auto_link && autometa_qobuz_auto_link_eligible(&assessment);
            best = Some((detail, assessment));
            if stop {
                break;
            }
        }
    }
    let Some((detail, assessment)) = best else {
        return Ok(AutoMetaQobuzResult::NotFound);
    };
    let qobuz_id = detail.album.id.clone();
    if autometa_qobuz_auto_link_eligible(&assessment) {
        link_qobuz_album_and_refresh_art(state, album_id, &detail, assessment.score, "matched")
            .await
            .map_err(|(_, message)| message)?;
        return Ok(AutoMetaQobuzResult::Matched {
            qobuz_id,
            score: assessment.score,
        });
    }
    if assessment.score >= AUTOMETA_QOBUZ_AUTO_LINK_MIN_SCORE {
        state
            .library()
            .mark_qobuz_candidate_for_review(album_id, &detail, assessment.score)?;
    }
    Ok(autometa_qobuz_result_for_assessment(qobuz_id, &assessment))
}

fn autometa_qobuz_result_for_assessment(
    qobuz_id: String,
    assessment: &QobuzMatchAssessment,
) -> AutoMetaQobuzResult {
    if autometa_qobuz_auto_link_eligible(assessment) {
        return AutoMetaQobuzResult::Matched {
            qobuz_id,
            score: assessment.score,
        };
    }
    let reason = if assessment.barcode_match == Some(false) {
        format!("Qobuz barcode conflict at score {}", assessment.score)
    } else if assessment.score < AUTOMETA_QOBUZ_AUTO_LINK_MIN_SCORE {
        format!(
            "Qobuz score {} under {}",
            assessment.score, AUTOMETA_QOBUZ_AUTO_LINK_MIN_SCORE
        )
    } else {
        format!("Qobuz score {} needs review", assessment.score)
    };
    AutoMetaQobuzResult::NeedsReview {
        qobuz_id,
        score: assessment.score,
        reason,
    }
}

fn qobuz_lookup_not_found(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("no result matching given argument")
        || normalized.contains("no albums in qobuz search response")
}

async fn simulate_qobuz_after_metabrainz(
    state: &AppState,
    response: &MetaBrainzTestResponse,
) -> Result<QobuzMatchTestResponse, (StatusCode, String)> {
    let empty = |status: &str| QobuzMatchTestResponse {
        matched: false,
        qobuz_album_id: None,
        score: 0,
        status: status.to_string(),
        query: String::new(),
        album: None,
        assessment: None,
        candidates: Vec::new(),
    };

    let qobuz_status = state.qobuz().status().await;
    if !qobuz_status.initialized {
        return Ok(empty("qobuz_unavailable"));
    }
    if response.best_candidate.is_none() || response.preview.is_none() {
        return Ok(empty("metabrainz_not_found"));
    }

    let (album, tracks) = simulated_metabrainz_metadata(response);
    let query = album
        .album_artist
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|artist| format!("{} {}", artist, album.title))
        .unwrap_or_else(|| album.title.clone());
    let results = state
        .qobuz()
        .search_albums(&query)
        .await
        .map_err(internal_error)?;

    let Some(top_album) = results.albums.into_iter().next() else {
        return Ok(QobuzMatchTestResponse {
            matched: false,
            qobuz_album_id: None,
            score: 0,
            status: "not_found".to_string(),
            query,
            album: None,
            assessment: None,
            candidates: Vec::new(),
        });
    };

    let (album, assessment, track_count) =
        match state.qobuz().album_detail_basic(&top_album.id).await {
            Ok(detail) => {
                let assessment = state
                    .library()
                    .qobuz_link_assessment_for_metadata(&album, &tracks, &detail);
                (detail.album, assessment, detail.tracks.len())
            }
            Err(_) => {
                let track_count = top_album.tracks_count.unwrap_or(0) as usize;
                (
                    top_album,
                    QobuzMatchAssessment {
                        score: 0,
                        auto_link: false,
                        barcode_match: None,
                    },
                    track_count,
                )
            }
        };
    let candidates = vec![QobuzMatchTestCandidate {
        album: album.clone(),
        assessment: assessment.clone(),
        track_count,
    }];
    let qobuz_album_id = Some(album.id.clone());
    let score = assessment.score;
    let matched = assessment.auto_link;
    Ok(QobuzMatchTestResponse {
        matched,
        qobuz_album_id,
        score,
        status: if matched { "matched" } else { "needs_review" }.to_string(),
        query,
        album: Some(album),
        assessment: Some(assessment),
        candidates,
    })
}

fn simulated_metabrainz_metadata(
    response: &MetaBrainzTestResponse,
) -> (crate::library::AlbumSummary, Vec<TrackSummary>) {
    let mut album = response.album.clone();
    if let Some(preview) = response.preview.as_ref() {
        album.title = preview.album.title.to.clone();
        album.album_artist = preview.album.album_artist.to.clone();
        album.year = preview.album.year.to;
        album.match_status = "matched".to_string();
        album.mb_barcode = preview.album.mb_barcode.clone();
    }

    let mut tracks = response.tracks.clone();
    if let Some(preview) = response.preview.as_ref() {
        for proposed in &preview.tracks {
            if let Some(track) = tracks
                .iter_mut()
                .find(|track| track.id == proposed.file_track_id)
            {
                track.title = proposed.title.to.clone();
                track.artist = proposed.artist.to.clone();
                track.album = Some(album.title.clone());
                track.album_artist = album.album_artist.clone();
                track.year = album.year;
                track.track_number = proposed.track_number.to;
                track.disc_number = proposed.disc_number.to;
            }
        }
    }
    (album, tracks)
}

async fn library_mark_reviewed(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .run_blocking(move |library| library.mark_album_reviewed(id))
        .await
        .map_err(internal_error)?
    {
        Some(detail) => Ok(Json(detail).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_match_search(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ManualSearchRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .manual_search(id, req)
        .await
        .map_err(internal_error)?
    {
        Some(response) => Ok(Json(response).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_match_mbid(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<MbidLookupRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .lookup_mbid(id, req)
        .await
        .map_err(internal_error)?
    {
        Some(response) => Ok(Json(response).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Album not found".to_string())),
    }
}

async fn library_candidate_preview(
    State(state): State<AppState>,
    Path((album_id, candidate_id)): Path<(i64, i64)>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    match state
        .library()
        .preview_candidate(album_id, candidate_id)
        .await
        .map_err(internal_error)?
    {
        Some(preview) => Ok(Json(preview).into_response()),
        None => Err((StatusCode::NOT_FOUND, "Candidate not found".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::AutoMetaLocalVersion;

    fn autometa_version(
        musicbrainz_match_status: Option<&str>,
        version_qobuz_match_status: Option<&str>,
        album_qobuz_match_status: Option<&str>,
        album_qobuz_album_id: Option<&str>,
    ) -> AutoMetaLocalVersion {
        AutoMetaLocalVersion {
            album_id: 1,
            version_id: 2,
            album_title: "Album".to_string(),
            version_label: "Library".to_string(),
            is_primary_version: true,
            musicbrainz_match_status: musicbrainz_match_status.map(str::to_string),
            musicbrainz_release_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            qobuz_match_status: version_qobuz_match_status.map(str::to_string),
            album_qobuz_match_status: album_qobuz_match_status.map(str::to_string),
            album_qobuz_album_id: album_qobuz_album_id.map(str::to_string),
        }
    }

    #[test]
    fn autometa_done_reuses_album_level_qobuz_match() {
        let version = autometa_version(Some("matched"), None, Some("matched"), Some("qobuz-42"));

        assert!(autometa_version_done(&version, true));
        assert_eq!(autometa_existing_qobuz_match(&version), Some("qobuz-42"));
    }

    #[test]
    fn autometa_done_requires_qobuz_id_when_linking_qobuz() {
        let version = autometa_version(Some("matched"), None, Some("matched"), None);

        assert!(!autometa_version_done(&version, true));
        assert!(autometa_version_done(&version, false));
    }

    #[test]
    fn autometa_done_rejects_stale_musicbrainz_match_without_valid_release_id() {
        let mut version = autometa_version(
            Some("matched"),
            Some("matched"),
            Some("matched"),
            Some("qobuz-42"),
        );
        version.musicbrainz_release_id = Some("matched".to_string());

        assert!(!autometa_version_done(&version, true));
        assert!(!autometa_version_done(&version, false));
    }

    #[test]
    fn autometa_qobuz_done_rejects_blank_or_unmatched_album_qobuz_id() {
        let blank_id = autometa_version(Some("matched"), None, Some("matched"), Some("   "));
        let unmatched_album = autometa_version(
            Some("matched"),
            None,
            Some("needs_review"),
            Some("qobuz-42"),
        );

        assert_eq!(autometa_existing_qobuz_match(&blank_id), None);
        assert!(!autometa_qobuz_done(&blank_id));
        assert_eq!(autometa_existing_qobuz_match(&unmatched_album), None);
        assert!(!autometa_qobuz_done(&unmatched_album));
    }

    #[test]
    fn autometa_qobuz_not_found_errors_do_not_fail_batch() {
        assert!(qobuz_lookup_not_found(
            "Qobuz album search failed: No result matching given argument"
        ));
        assert!(qobuz_lookup_not_found(
            "No albums in Qobuz search response: {}"
        ));
        assert!(!qobuz_lookup_not_found("Qobuz album search returned 401"));
    }

    #[test]
    fn autometa_qobuz_result_accepts_score_at_threshold() {
        let assessment = QobuzMatchAssessment {
            score: 50,
            auto_link: false,
            barcode_match: None,
        };

        let result =
            autometa_qobuz_result_for_assessment("qobuz-threshold".to_string(), &assessment);

        assert_eq!(
            result,
            AutoMetaQobuzResult::Matched {
                qobuz_id: "qobuz-threshold".to_string(),
                score: 50,
            }
        );
    }

    #[test]
    fn autometa_qobuz_result_rejects_score_below_threshold() {
        let assessment = QobuzMatchAssessment {
            score: 49,
            auto_link: true,
            barcode_match: Some(true),
        };

        let result = autometa_qobuz_result_for_assessment("qobuz-below".to_string(), &assessment);

        assert_eq!(
            result,
            AutoMetaQobuzResult::NeedsReview {
                qobuz_id: "qobuz-below".to_string(),
                score: 49,
                reason: "Qobuz score 49 under 50".to_string(),
            }
        );
    }

    #[test]
    fn autometa_qobuz_result_keeps_barcode_conflict_veto() {
        let assessment = QobuzMatchAssessment {
            score: 100,
            auto_link: false,
            barcode_match: Some(false),
        };

        let result =
            autometa_qobuz_result_for_assessment("qobuz-conflict".to_string(), &assessment);

        assert_eq!(
            result,
            AutoMetaQobuzResult::NeedsReview {
                qobuz_id: "qobuz-conflict".to_string(),
                score: 100,
                reason: "Qobuz barcode conflict at score 100".to_string(),
            }
        );
    }
}
