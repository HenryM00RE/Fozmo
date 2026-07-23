use super::auth_token_from_headers;
use crate::api::error::ApiError;
use crate::app::auth::{RemoteAuthenticated, RequestSurface, control_session_token_from_headers};
use crate::app::state::AppState;
use crate::audio::eq::EqConfig;
use crate::audio::player::TrackCover;
use crate::audio::transcode::{DerivativeFormat, DerivativeStream, TranscodeRequestError, opus};
use crate::audio::upnp::{UpnpCachedAsset, UpnpGeneratedDspStream};
use axum::{
    Extension, Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use std::collections::HashMap;
use std::io::Error as IoError;
use std::net::SocketAddr;
use std::path::{Path as StdPath, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

const FILE_STREAM_CHUNK_SIZE: u64 = 64 * 1024;
const WAV_HEADER_BYTES: u64 = 44;
const DOP_WAV_FRAME_BYTES: u64 = 6;
const HEGEL_DOP_STARTUP_PACE_LEAD: Duration = Duration::from_secs(1);
const HEGEL_DOP_STARTUP_PACE_WINDOW: Duration = Duration::from_secs(8);
const DLNA_CONTENT_FEATURES: &str =
    "DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000";

#[derive(Clone, Copy, Debug)]
enum ParsedByteRange {
    Satisfiable { start: u64, end: u64 },
    Unsatisfiable,
}

#[derive(Clone, Copy)]
struct RangeAlignment {
    data_start: u64,
    frame_bytes: u64,
}

#[derive(Clone, Copy)]
struct RangeTraceContext<'a> {
    asset_id: &'a str,
    active_output_mode: Option<&'a str>,
    target_rate: Option<u32>,
    target_bits: u32,
}

struct FileStreamResult {
    response: Response,
    media_ready: bool,
    status: StatusCode,
    request_elapsed_ms: Option<u64>,
}

#[derive(Clone, Copy)]
struct GeneratedStartupPacing {
    bytes_per_second: u64,
    lead_bytes: u64,
    pace_until_data_bytes: u64,
}

struct GeneratedStartupPacer {
    pacing: GeneratedStartupPacing,
    next_offset: u64,
    first_audio_at: Option<Instant>,
    data_bytes_sent: u64,
    announced_delay: bool,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/stream/local/:track_id", get(stream_local_track))
        .route("/api/stream/qobuz/:track_id", get(stream_qobuz_track))
}

enum StreamAuthDecision {
    Allowed,
    Denied(StatusCode, &'static str),
}

/// Remote stream requests are authorized only by the [`RemoteAuthenticated`]
/// marker that `require_remote_auth` inserts after verifying the remote
/// session cookie. `RequestSurface::Remote` alone is never proof of auth, and
/// header/query/stream tokens, LAN control cookies, and loopback posture are
/// all rejected on the remote listener.
fn remote_stream_auth(remote_authenticated: bool) -> StreamAuthDecision {
    if remote_authenticated {
        StreamAuthDecision::Allowed
    } else {
        StreamAuthDecision::Denied(
            StatusCode::UNAUTHORIZED,
            "Streaming requires an authenticated remote session",
        )
    }
}

/// LAN/local browser and integration auth for stream endpoints: header
/// stream/control tokens keep working for existing integrations, and the
/// `fozmo_control_session` cookie lets a paired browser `<audio>` element
/// stream without attaching tokens to URLs.
fn lan_stream_token_or_cookie_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let token = auth_token_from_headers(headers);
    state.pairing().verify_stream_token(token.as_deref())
        || state.pairing().verify_control_token(token.as_deref())
        || control_session_token_from_headers(headers)
            .is_some_and(|cookie| state.pairing().verify_control_token(Some(&cookie)))
}

fn local_stream_auth(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    surface: Option<RequestSurface>,
    remote_authenticated: bool,
) -> StreamAuthDecision {
    if matches!(surface, Some(RequestSurface::Remote)) {
        return remote_stream_auth(remote_authenticated);
    }
    if lan_stream_token_or_cookie_authorized(state, headers)
        || crate::app::auth::same_origin_browser_request_allowed(headers)
        || crate::app::auth::local_filesystem_request_allowed(headers, peer)
    {
        StreamAuthDecision::Allowed
    } else {
        StreamAuthDecision::Denied(
            StatusCode::FORBIDDEN,
            "Local track streaming is only available locally or with a paired session",
        )
    }
}

/// Qobuz streams proxy a user's logged-in Qobuz session, so they require the
/// same explicit LAN/local authorization as local browser streams even when
/// global pairing enforcement is disabled.
fn qobuz_stream_auth(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    surface: Option<RequestSurface>,
    remote_authenticated: bool,
) -> StreamAuthDecision {
    if matches!(surface, Some(RequestSurface::Remote)) {
        return remote_stream_auth(remote_authenticated);
    }
    if lan_stream_token_or_cookie_authorized(state, headers)
        || crate::app::auth::same_origin_browser_request_allowed(headers)
        || crate::app::auth::local_filesystem_request_allowed(headers, peer)
    {
        StreamAuthDecision::Allowed
    } else {
        StreamAuthDecision::Denied(
            StatusCode::FORBIDDEN,
            "Qobuz streaming is only available locally or with a paired session",
        )
    }
}

async fn stream_local_track(
    State(state): State<AppState>,
    Path(track_id): Path<i64>,
    Query(query): Query<HashMap<String, String>>,
    surface: Option<Extension<RequestSurface>>,
    remote_auth: Option<Extension<RemoteAuthenticated>>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    let decision = local_stream_auth(
        &state,
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
        surface.map(|Extension(surface)| surface),
        remote_auth.is_some(),
    );
    if let StreamAuthDecision::Denied(status, message) = decision {
        return (status, message).into_response();
    }
    let path = match state
        .library()
        .run_blocking(move |library| library.track_path(track_id))
        .await
    {
        Ok(Some(path)) => path,
        Ok(None) => return (StatusCode::NOT_FOUND, "Track not found").into_response(),
        Err(e) => return ApiError::internal(e).into_response(),
    };
    match query.get("variant").map(String::as_str) {
        None | Some("original") => {
            note_browser_stream_signal(&state, &query, track_id, &path, "original", None, None)
                .await;
            stream_file_path(
                path,
                headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
            )
            .await
            .response
        }
        Some("opus") => {
            let bitrate_kbps = match requested_opus_kbps(&query) {
                Ok(kbps) => kbps,
                Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
            };
            let eq = crate::audio::transcode::active_eq(zone_eq_for_stream(&state, &query));
            note_browser_stream_signal(
                &state,
                &query,
                track_id,
                &path,
                "opus",
                Some(bitrate_kbps),
                eq.as_ref(),
            )
            .await;
            stream_local_track_derivative(
                &state,
                track_id,
                path,
                DerivativeFormat::OggOpus { bitrate_kbps },
                eq,
                &headers,
            )
            .await
        }
        Some("flac") => {
            // "flac" means "lossless, with the zone's EQ baked in": without
            // active EQ the original bytes are already exactly that, so serve
            // the file directly and skip the transcode cache.
            match crate::audio::transcode::active_eq(zone_eq_for_stream(&state, &query)) {
                None => {
                    note_browser_stream_signal(
                        &state,
                        &query,
                        track_id,
                        &path,
                        "flac_passthrough",
                        None,
                        None,
                    )
                    .await;
                    stream_file_path(
                        path,
                        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
                    )
                    .await
                    .response
                }
                Some(eq) => {
                    note_browser_stream_signal(
                        &state,
                        &query,
                        track_id,
                        &path,
                        "flac",
                        None,
                        Some(&eq),
                    )
                    .await;
                    stream_local_track_derivative(
                        &state,
                        track_id,
                        path,
                        DerivativeFormat::Flac,
                        Some(eq),
                        &headers,
                    )
                    .await
                }
            }
        }
        Some(_) => (
            StatusCode::BAD_REQUEST,
            "Unknown stream variant; expected \"original\", \"flac\", or \"opus\"",
        )
            .into_response(),
    }
}

/// Record the server-side chain for the zone's signal-path UI. Only requests
/// that name a zone (i.e. browser-zone playback) are recorded, and prefetch
/// requests are skipped so warming up the next track does not clobber the
/// currently playing chain.
async fn note_browser_stream_signal(
    state: &AppState,
    query: &HashMap<String, String>,
    track_id: i64,
    path: &StdPath,
    variant: &str,
    opus_kbps: Option<u32>,
    eq: Option<&EqConfig>,
) {
    let Some(zone_id) = query
        .get("zone")
        .map(String::as_str)
        .filter(|zone| !zone.is_empty())
    else {
        return;
    };
    if query.get("prefetch").map(String::as_str) == Some("1") {
        return;
    }
    let track = state
        .library()
        .run_blocking(move |library| library.track_by_id(track_id))
        .await
        .ok()
        .flatten();
    let source_rate = track
        .as_ref()
        .and_then(|track| track.sample_rate)
        .and_then(|rate| u32::try_from(rate).ok())
        .unwrap_or(0);
    let source_bits = track
        .as_ref()
        .and_then(|track| track.bit_depth)
        .and_then(|bits| u32::try_from(bits).ok())
        .unwrap_or(0);
    let source_format = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_uppercase());
    let (output_rate, output_bits) = match variant {
        "opus" => (crate::audio::transcode::opus::OPUS_SAMPLE_RATE, 0),
        "flac" => (source_rate, 24),
        _ => (source_rate, source_bits),
    };
    state.zones().note_browser_stream_signal(
        zone_id,
        crate::protocol::BrowserStreamSignal {
            track_id,
            variant: variant.to_string(),
            opus_kbps,
            eq_active: eq.is_some(),
            eq_active_bands: eq
                .map(|config| config.bands.iter().filter(|band| band.enabled).count() as u32)
                .unwrap_or(0),
            source_format,
            source_rate,
            source_bits,
            output_rate,
            output_bits,
        },
    );
}

/// Browser-selectable Opus bitrate: absent falls back to the server default,
/// anything outside the allowed set is rejected to keep the cache bounded.
fn requested_opus_kbps(query: &HashMap<String, String>) -> Result<u32, &'static str> {
    match query.get("kbps").map(String::as_str) {
        None | Some("") => Ok(opus::configured_bitrate_kbps()),
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|kbps| opus::ALLOWED_BITRATE_KBPS.contains(kbps))
            .ok_or("Unsupported Opus bitrate; expected 128, 256, or 320"),
    }
}

/// The persisted EQ for the zone named in the request, when given. Streams
/// without a `zone` are served EQ-free; unknown zones fall back to the
/// settings store's legacy defaults, matching `/api/zones/:id/eq`.
fn zone_eq_for_stream(state: &AppState, query: &HashMap<String, String>) -> Option<EqConfig> {
    let zone_id = query
        .get("zone")
        .map(String::as_str)
        .filter(|zone| !zone.is_empty())?;
    state.settings().playback_for_zone(zone_id).eq
}

/// Playback derivative of a local track (issue #37): lossy Opus for mobile
/// data / Safari, or FLAC when EQ must be baked into a lossless stream. Auth
/// is identical to the original-quality route; the derivative comes from the
/// bounded transcode cache. Completed derivatives serve with full `Range`
/// support. While a derivative is still encoding, a rangeless request gets
/// `200 OK` without `Accept-Ranges` and the body is streamed as it is
/// produced, so playback starts without waiting for the whole track (or
/// album) to transcode; a request that carries a `Range` header instead
/// waits for the encode to finish and is answered from the completed file,
/// because iOS Safari's media stack sends range requests and refuses
/// rangeless streams outright (and a growing file has no total length to
/// answer a range against).
async fn stream_local_track_derivative(
    state: &AppState,
    track_id: i64,
    path: PathBuf,
    format: DerivativeFormat,
    eq: Option<EqConfig>,
    headers: &HeaderMap,
) -> Response {
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    match state
        .local_transcode()
        .stream_derivative(track_id, &path, format, eq)
    {
        Ok(DerivativeStream::Ready(cached)) => stream_file_path(cached, range).await.response,
        Ok(DerivativeStream::Generating { path, progress }) => {
            if range.is_some() {
                return match wait_for_derivative(progress).await {
                    Ok(()) => stream_file_path(path, range).await.response,
                    Err(error) => {
                        tracing::warn!(
                            event = "stream_transcode_failure",
                            track_id,
                            error = %crate::diagnostics::logging::sanitize_error(&error),
                            "Derivative encode failed while a ranged request waited"
                        );
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Local derivative stream request failed",
                        )
                            .into_response()
                    }
                };
            }
            let mut out = HeaderMap::new();
            out.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(format.content_type()),
            );
            let body = crate::audio::transcode::progressive_derivative_stream(path, progress);
            (StatusCode::OK, out, Body::from_stream(body)).into_response()
        }
        Err(TranscodeRequestError::SourceMissing) => {
            (StatusCode::NOT_FOUND, "Track file not found").into_response()
        }
        Err(TranscodeRequestError::Failed(e)) => {
            tracing::warn!(
                event = "stream_transcode_failure",
                track_id,
                error = %crate::diagnostics::logging::sanitize_error(&e),
                "Local derivative stream request failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Local derivative stream request failed",
            )
                .into_response()
        }
    }
}

/// Wait until a generating derivative reaches a terminal state. Encodes are
/// bounded by track length, so no artificial timeout: a slow encode is still
/// the fastest correct answer for a ranged request.
async fn wait_for_derivative(
    mut progress: tokio::sync::watch::Receiver<crate::audio::transcode::TranscodeProgress>,
) -> Result<(), String> {
    loop {
        let snapshot = progress.borrow().clone();
        if let Some(error) = snapshot.error {
            return Err(error);
        }
        if snapshot.done {
            return Ok(());
        }
        if progress.changed().await.is_err() {
            return Err("transcode job ended unexpectedly".to_string());
        }
    }
}

async fn stream_qobuz_track(
    State(state): State<AppState>,
    Path(track_id): Path<u64>,
    Query(query): Query<HashMap<String, String>>,
    surface: Option<Extension<RequestSurface>>,
    remote_auth: Option<Extension<RemoteAuthenticated>>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    let decision = qobuz_stream_auth(
        &state,
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
        surface.map(|Extension(surface)| surface),
        remote_auth.is_some(),
    );
    if let StreamAuthDecision::Denied(status, message) = decision {
        return (status, message).into_response();
    }
    let requested_format_id = match qobuz_browser_requested_format(&query) {
        Ok(format_id) => format_id,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let derivative_format = match qobuz_browser_derivative_format(&query) {
        Ok(format) => format,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let eq = crate::audio::transcode::active_eq(zone_eq_for_stream(&state, &query));
    if let (Some(format), Some(eq)) = (derivative_format, eq) {
        return stream_qobuz_track_derivative(
            &state,
            track_id,
            requested_format_id,
            format,
            eq,
            &headers,
            &query,
        )
        .await;
    }
    match state
        .qobuz()
        .proxy_bytes_with_format(
            track_id,
            headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
            requested_format_id,
        )
        .await
    {
        Ok(proxy) => {
            note_qobuz_browser_stream_signal(
                &state,
                &query,
                track_id,
                requested_format_id,
                qobuz_proxy_variant(requested_format_id),
                None,
                None,
                proxy.sampling_rate_hz.unwrap_or(0),
                proxy.bit_depth.unwrap_or(0),
            );
            let mut out = HeaderMap::new();
            if let Some(content_type) = proxy.content_type {
                if let Ok(value) = HeaderValue::from_str(&content_type) {
                    out.insert(header::CONTENT_TYPE, value);
                }
            } else {
                out.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(qobuz_browser_fallback_content_type(
                        requested_format_id,
                    )),
                );
            }
            if let Some(content_length) = proxy.content_length
                && let Ok(value) = HeaderValue::from_str(&content_length)
            {
                out.insert(header::CONTENT_LENGTH, value);
            }
            if let Some(content_range) = proxy.content_range
                && let Ok(value) = HeaderValue::from_str(&content_range)
            {
                out.insert(header::CONTENT_RANGE, value);
            }
            out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            (proxy.status, out, Body::from_stream(proxy.body)).into_response()
        }
        // Proxy errors can embed upstream detail (e.g. signed CDN URLs inside
        // reqwest errors); log a sanitized form and keep the body generic.
        Err(e) => {
            tracing::warn!(
                event = "stream_proxy_failure",
                service = "qobuz",
                qobuz_track_id = track_id,
                error = %crate::diagnostics::logging::sanitize_error(&e),
                "Qobuz browser stream proxy failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Qobuz stream request failed",
            )
                .into_response()
        }
    }
}

fn qobuz_browser_requested_format(
    query: &HashMap<String, String>,
) -> Result<Option<u32>, &'static str> {
    let quality_format = match query.get("quality").map(String::as_str) {
        None | Some("") | Some("original") | Some("source") => None,
        Some("lossy") | Some("mobile") | Some("data-saver") => Some(5),
        Some(_) => {
            return Err("Unknown Qobuz stream quality; expected \"original\" or \"lossy\"");
        }
    };
    let explicit_format = match query.get("format").map(String::as_str) {
        None | Some("") => None,
        Some("5") => Some(5),
        Some(_) => return Err("Unsupported Qobuz browser stream format; expected 5"),
    };
    if quality_format.is_some() && explicit_format.is_some() && quality_format != explicit_format {
        return Err("Conflicting Qobuz stream quality and format");
    }
    Ok(explicit_format.or(quality_format))
}

fn qobuz_browser_derivative_format(
    query: &HashMap<String, String>,
) -> Result<Option<DerivativeFormat>, &'static str> {
    match query.get("variant").map(String::as_str) {
        None | Some("") | Some("original") => Ok(None),
        Some("flac") => Ok(Some(DerivativeFormat::Flac)),
        Some("opus") => Ok(Some(DerivativeFormat::OggOpus {
            bitrate_kbps: requested_opus_kbps(query)?,
        })),
        Some(_) => {
            Err("Unknown Qobuz stream variant; expected \"original\", \"flac\", or \"opus\"")
        }
    }
}

fn qobuz_browser_fallback_content_type(requested_format_id: Option<u32>) -> &'static str {
    match requested_format_id {
        Some(5) => "audio/mpeg",
        _ => "audio/flac",
    }
}

fn qobuz_proxy_variant(requested_format_id: Option<u32>) -> &'static str {
    if requested_format_id == Some(5) {
        "qobuz_lossy"
    } else {
        "qobuz_flac"
    }
}

fn qobuz_delivery_variant(format: DerivativeFormat) -> &'static str {
    match format {
        DerivativeFormat::OggOpus { .. } => "qobuz_opus",
        DerivativeFormat::Flac => "qobuz_flac_eq",
    }
}

fn qobuz_delivery_opus_kbps(format: DerivativeFormat) -> Option<u32> {
    match format {
        DerivativeFormat::OggOpus { bitrate_kbps } => Some(bitrate_kbps),
        DerivativeFormat::Flac => None,
    }
}

/// Qobuz counterpart of [`note_browser_stream_signal`]. `source_rate` /
/// `source_bits` describe the delivered file (from `getFileUrl` for the
/// pass-through proxy, or the downloaded source's STREAMINFO for EQ
/// derivatives); 0 means unknown.
#[allow(clippy::too_many_arguments)]
fn note_qobuz_browser_stream_signal(
    state: &AppState,
    query: &HashMap<String, String>,
    track_id: u64,
    requested_format_id: Option<u32>,
    variant: &str,
    opus_kbps: Option<u32>,
    eq: Option<&EqConfig>,
    source_rate: u32,
    source_bits: u32,
) {
    let Some(zone_id) = query
        .get("zone")
        .map(String::as_str)
        .filter(|zone| !zone.is_empty())
    else {
        return;
    };
    if query.get("prefetch").map(String::as_str) == Some("1") {
        return;
    }
    let lossy = requested_format_id == Some(5);
    let output_rate = if variant == "qobuz_opus" {
        crate::audio::transcode::opus::OPUS_SAMPLE_RATE
    } else {
        0
    };
    let output_bits = if variant == "qobuz_flac_eq" { 24 } else { 0 };
    state.zones().note_browser_stream_signal(
        zone_id,
        crate::protocol::BrowserStreamSignal {
            track_id: track_id as i64,
            variant: variant.to_string(),
            opus_kbps,
            eq_active: eq.is_some(),
            eq_active_bands: eq
                .map(|config| config.bands.iter().filter(|band| band.enabled).count() as u32)
                .unwrap_or(0),
            source_format: Some(if lossy { "MP3" } else { "FLAC" }.to_string()),
            source_rate,
            source_bits,
            output_rate,
            output_bits,
        },
    );
}

/// Sample rate and bit depth from a FLAC file's STREAMINFO block, or `(0, 0)`
/// when the file is not FLAC (e.g. an MP3 source) or the header is malformed.
fn flac_streaminfo_quality(path: &StdPath) -> (u32, u32) {
    let mut header = [0_u8; 26];
    let read_ok = std::fs::File::open(path)
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut header))
        .is_ok();
    // "fLaC" magic + STREAMINFO (type 0) as the first metadata block; the
    // rate/bits fields start 10 bytes into STREAMINFO (offset 18 in the file).
    if !read_ok || &header[..4] != b"fLaC" || header[4] & 0x7f != 0 {
        return (0, 0);
    }
    let rate =
        (u32::from(header[18]) << 12) | (u32::from(header[19]) << 4) | (u32::from(header[20]) >> 4);
    let bits = ((u32::from(header[20]) & 0x01) << 4 | u32::from(header[21]) >> 4) + 1;
    (rate, bits)
}

async fn stream_qobuz_track_derivative(
    state: &AppState,
    track_id: u64,
    requested_format_id: Option<u32>,
    format: DerivativeFormat,
    eq: EqConfig,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Response {
    let path = match ensure_qobuz_browser_source_file(state, track_id, requested_format_id).await {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!(
                event = "qobuz_browser_eq_source_failed",
                qobuz_track_id = track_id,
                requested_format_id = requested_format_id.unwrap_or_default(),
                error = %crate::diagnostics::logging::sanitize_error(&e),
                "Qobuz browser EQ source download failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Qobuz EQ stream request failed",
            )
                .into_response();
        }
    };
    let (source_rate, source_bits) = flac_streaminfo_quality(&path);
    note_qobuz_browser_stream_signal(
        state,
        query,
        track_id,
        requested_format_id,
        qobuz_delivery_variant(format),
        qobuz_delivery_opus_kbps(format),
        Some(&eq),
        source_rate,
        source_bits,
    );
    stream_local_track_derivative(
        state,
        i64::try_from(track_id).unwrap_or(i64::MAX),
        path,
        format,
        Some(eq),
        headers,
    )
    .await
}

async fn ensure_qobuz_browser_source_file(
    state: &AppState,
    track_id: u64,
    requested_format_id: Option<u32>,
) -> Result<PathBuf, String> {
    let path = qobuz_browser_source_path(track_id, requested_format_id);
    if std::fs::metadata(&path)
        .ok()
        .is_some_and(|metadata| metadata.len() > 0)
    {
        return Ok(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| "Qobuz EQ source path has no parent".to_string())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("create Qobuz EQ source cache: {e}"))?;

    let ext = qobuz_browser_source_extension(requested_format_id);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let part_path = path.with_extension(format!("{ext}.{}.{}.part", std::process::id(), stamp));
    let mut file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|e| format!("create Qobuz EQ source temp file: {e}"))?;
    let proxy = state
        .qobuz()
        .proxy_bytes_with_format(track_id, None, requested_format_id)
        .await?;
    let mut body = proxy.body;
    let mut written = 0_u64;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| format!("read Qobuz EQ source bytes: {e}"))?;
        if chunk.is_empty() {
            continue;
        }
        written += chunk.len() as u64;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("write Qobuz EQ source bytes: {e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("flush Qobuz EQ source file: {e}"))?;
    drop(file);
    if written == 0 {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err("Qobuz EQ source download returned no audio bytes".to_string());
    }
    tokio::fs::rename(&part_path, &path)
        .await
        .map_err(|e| format!("commit Qobuz EQ source file: {e}"))?;
    Ok(path)
}

fn qobuz_browser_source_path(track_id: u64, requested_format_id: Option<u32>) -> PathBuf {
    std::env::temp_dir()
        .join("fozmo-qobuz-browser-eq")
        .join(format!(
            "track-{track_id}-format-{}.{}",
            requested_format_id.unwrap_or(0),
            qobuz_browser_source_extension(requested_format_id)
        ))
}

fn qobuz_browser_source_extension(requested_format_id: Option<u32>) -> &'static str {
    if requested_format_id == Some(5) {
        "mp3"
    } else {
        "flac"
    }
}

pub async fn sonos_stream(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let token = query.get("token").map(String::as_str).unwrap_or("");
    let Some(path) = state.sonos().asset_path_for_request(&asset_id, token) else {
        return (StatusCode::UNAUTHORIZED, "Invalid Sonos stream token").into_response();
    };
    stream_file_path(
        path,
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
    )
    .await
    .response
}

pub async fn sonos_qobuz_stream(
    State(state): State<AppState>,
    Path(track_id): Path<u64>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let asset_id = query.get("asset").map(String::as_str).unwrap_or("");
    let token = query.get("token").map(String::as_str).unwrap_or("");
    sonos_qobuz_stream_response(state, track_id, asset_id, token, headers).await
}

async fn sonos_qobuz_stream_response(
    state: AppState,
    track_id: u64,
    asset_id: &str,
    token: &str,
    headers: HeaderMap,
) -> Response {
    if !state
        .sonos()
        .qobuz_remote_stream_token_valid(asset_id, token, track_id, 6)
    {
        return (StatusCode::UNAUTHORIZED, "Invalid Sonos Qobuz stream token").into_response();
    }

    match state
        .qobuz()
        .sonos_proxy_bytes(
            track_id,
            headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
        )
        .await
    {
        Ok(proxy) => {
            let mut out = HeaderMap::new();
            if let Some(content_type) = proxy.content_type {
                if let Ok(value) = HeaderValue::from_str(&content_type) {
                    out.insert(header::CONTENT_TYPE, value);
                }
            } else {
                out.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/flac"));
            }
            if let Some(content_length) = proxy.content_length
                && let Ok(value) = HeaderValue::from_str(&content_length)
            {
                out.insert(header::CONTENT_LENGTH, value);
            }
            if let Some(content_range) = proxy.content_range
                && let Ok(value) = HeaderValue::from_str(&content_range)
            {
                out.insert(header::CONTENT_RANGE, value);
            }
            out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            (proxy.status, out, Body::from_stream(proxy.body)).into_response()
        }
        Err(e) => ApiError::internal(e).into_response(),
    }
}

pub async fn sonos_art(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = query.get("token").map(String::as_str).unwrap_or("");
    let Some(cover) = state.sonos().art_for_request(&asset_id, token) else {
        return (StatusCode::NOT_FOUND, "Sonos art not found").into_response();
    };
    artwork_response(cover)
}

pub async fn upnp_stream(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let token = query.get("token").map(String::as_str).unwrap_or("");
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    if let Some(asset) = state.upnp().asset_for_request(&asset_id, token) {
        if asset.is_probe && !crate::audio::upnp::probe_path_is_streamable(&asset.path) {
            return (StatusCode::NOT_FOUND, "UPnP probe asset not found").into_response();
        }
        state
            .upnp()
            .mark_renderer_http_request(&asset_id, token, "local_get", range);
        let alignment = upnp_asset_range_alignment(&asset);
        let result = stream_file_path_with_alignment(
            asset.path,
            range,
            alignment,
            Some(RangeTraceContext {
                asset_id: &asset_id,
                active_output_mode: asset.active_output_mode.as_deref(),
                target_rate: None,
                target_bits: asset.target_bits,
            }),
        )
        .await;
        if result.media_ready {
            state.upnp().mark_local_media_first_byte(
                &asset_id,
                token,
                range,
                result.status.as_u16(),
                result.request_elapsed_ms,
            );
        }
        return result.response;
    }
    if let Some(stream) = state
        .upnp()
        .generated_dsp_stream_for_request(&asset_id, token)
    {
        let Some(byte_len) = stream.byte_len else {
            return generated_stream_range_not_satisfiable(None).into_response();
        };
        if byte_len == 0 {
            return generated_stream_range_not_satisfiable(Some(0)).into_response();
        }
        let parsed_range = range.and_then(|header| parse_byte_range(header, byte_len));
        if matches!(parsed_range, Some(ParsedByteRange::Unsatisfiable)) {
            return generated_stream_range_not_satisfiable(Some(byte_len)).into_response();
        }
        let requested_range = parsed_range;
        let alignment = generated_stream_range_alignment(&stream);
        log_unaligned_dop_range_request(
            &asset_id,
            requested_range,
            alignment,
            stream.active_output_mode.as_deref(),
            Some(stream.target_rate),
            stream.target_bits,
        );
        let parsed_range = parsed_range.map(|range| align_byte_range(range, alignment, byte_len));
        let (start, end, status) = match parsed_range {
            Some(ParsedByteRange::Satisfiable { start, end }) => {
                (start, end, StatusCode::PARTIAL_CONTENT)
            }
            Some(ParsedByteRange::Unsatisfiable) => unreachable!(),
            None => (0, byte_len.saturating_sub(1), StatusCode::OK),
        };
        tracing::debug!(
            event = "upnp_generated_stream_range",
            asset_id = %asset_id,
            requested_range = ?requested_range,
            aligned_start = start,
            aligned_end = end,
            byte_len,
            status = status.as_u16(),
            mime_type = %stream.mime_type,
            target_rate = stream.target_rate,
            target_bits = stream.target_bits,
            active_output_mode = ?stream.active_output_mode,
            "Serving generated UPnP DSP stream range"
        );
        state
            .upnp()
            .mark_renderer_http_request(&asset_id, token, "local_get", range);
        let request_started = Instant::now();
        return match crate::playback::upnp_dsp::generated_upnp_dsp_wav_stream(
            state.clone(),
            stream.clone(),
            start,
            end,
        )
        .await
        {
            Ok(body) => {
                let mut out = HeaderMap::new();
                add_upnp_stream_headers(&mut out);
                if let Ok(value) = HeaderValue::from_str(&stream.mime_type) {
                    out.insert(header::CONTENT_TYPE, value);
                }
                if status == StatusCode::PARTIAL_CONTENT
                    && let Ok(value) =
                        HeaderValue::from_str(&format!("bytes {start}-{end}/{byte_len}"))
                {
                    out.insert(header::CONTENT_RANGE, value);
                }
                let content_len = end.saturating_sub(start).saturating_add(1);
                if let Ok(value) = HeaderValue::from_str(&content_len.to_string()) {
                    out.insert(header::CONTENT_LENGTH, value);
                }
                let trace_state = state.clone();
                let trace_asset_id = asset_id.clone();
                let trace_token = token.to_string();
                let trace_range = range.map(str::to_string);
                let is_dop_wav = generated_stream_range_alignment(&stream).is_some();
                let startup_pacing = state
                    .upnp()
                    .generated_startup_pacing_allowed(&asset_id, token)
                    .then(|| generated_stream_startup_pacing(&stream, start, status))
                    .flatten();
                let mut next_body_offset = start;
                let mut first_body_seen = false;
                let mut first_audio_seen = false;
                let traced_body = body.map(move |item| {
                    if let Ok(bytes) = item.as_ref()
                        && !bytes.is_empty()
                    {
                        let chunk_start = next_body_offset;
                        next_body_offset = next_body_offset.saturating_add(bytes.len() as u64);
                        let chunk_elapsed_ms = elapsed_ms(request_started);
                        if !first_body_seen {
                            first_body_seen = true;
                            trace_state.upnp().mark_local_media_first_body_byte(
                                &trace_asset_id,
                                &trace_token,
                                trace_range.as_deref(),
                                status.as_u16(),
                                Some(chunk_elapsed_ms),
                            );
                        }
                        if !first_audio_seen
                            && generated_chunk_contains_audio_payload(bytes, chunk_start)
                        {
                            if is_dop_wav {
                                if generated_chunk_contains_marker_valid_dop_frame(
                                    bytes,
                                    chunk_start,
                                ) {
                                    first_audio_seen = true;
                                    trace_state.upnp().mark_local_media_dop_frame(
                                        &trace_asset_id,
                                        &trace_token,
                                        trace_range.as_deref(),
                                        status.as_u16(),
                                        Some(chunk_elapsed_ms),
                                    );
                                }
                            } else {
                                first_audio_seen = true;
                                trace_state.upnp().mark_local_media_first_byte(
                                    &trace_asset_id,
                                    &trace_token,
                                    trace_range.as_deref(),
                                    status.as_u16(),
                                    Some(chunk_elapsed_ms),
                                );
                            }
                        }
                    }
                    item
                });
                let response_body: BoxStream<'static, Result<Bytes, IoError>> =
                    if let Some(pacing) = startup_pacing {
                        pace_generated_startup_stream(
                            traced_body.boxed(),
                            pacing,
                            start,
                            stream.zone_id.clone(),
                            asset_id.clone(),
                        )
                    } else {
                        traced_body.boxed()
                    };
                (status, out, Body::from_stream(response_body)).into_response()
            }
            Err(error) => {
                let safe_notice = crate::diagnostics::logging::sanitize_error(&error);
                if generated_stream_range_alignment(&stream).is_some() {
                    state
                        .upnp()
                        .mark_notice(stream.zone_id.as_str(), safe_notice);
                }
                ApiError::internal(error).into_response()
            }
        };
    }
    (StatusCode::UNAUTHORIZED, "Invalid UPnP stream token").into_response()
}

pub async fn upnp_stream_head(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = query.get("token").map(String::as_str).unwrap_or("");
    if let Some(asset) = state.upnp().asset_for_request(&asset_id, token) {
        if asset.is_probe && !crate::audio::upnp::probe_path_is_streamable(&asset.path) {
            return (StatusCode::NOT_FOUND, "UPnP probe asset not found").into_response();
        }
        state
            .upnp()
            .mark_renderer_http_request(&asset_id, token, "local_head", None);
        return stream_file_head(asset.path).await;
    }
    if let Some(stream) = state
        .upnp()
        .generated_dsp_stream_for_request(&asset_id, token)
    {
        state
            .upnp()
            .mark_renderer_http_request(&asset_id, token, "local_head", None);
        let mut headers = HeaderMap::new();
        add_upnp_stream_headers(&mut headers);
        if let Ok(value) = HeaderValue::from_str(&stream.mime_type) {
            headers.insert(header::CONTENT_TYPE, value);
        }
        if let Some(len) = stream.byte_len
            && let Ok(value) = HeaderValue::from_str(&len.to_string())
        {
            headers.insert(header::CONTENT_LENGTH, value);
        }
        return (StatusCode::OK, headers, Body::empty()).into_response();
    }
    (StatusCode::UNAUTHORIZED, "Invalid UPnP stream token").into_response()
}

pub async fn upnp_qobuz_stream(
    State(state): State<AppState>,
    Path(track_id): Path<u64>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let asset_id = query.get("asset").map(String::as_str).unwrap_or("");
    let token = query.get("token").map(String::as_str).unwrap_or("");
    upnp_qobuz_stream_response(state, track_id, asset_id, token, headers).await
}

pub async fn upnp_qobuz_stream_path(
    State(state): State<AppState>,
    Path((asset_id, token, track_id)): Path<(String, String, u64)>,
    headers: HeaderMap,
) -> Response {
    upnp_qobuz_stream_response(state, track_id, &asset_id, &token, headers).await
}

async fn upnp_qobuz_stream_response(
    state: AppState,
    track_id: u64,
    asset_id: &str,
    token: &str,
    headers: HeaderMap,
) -> Response {
    if !state.upnp().remote_stream_token_valid(asset_id, token) {
        return (StatusCode::UNAUTHORIZED, "Invalid UPnP Qobuz stream token").into_response();
    }
    let qobuz_format_id = state
        .upnp()
        .remote_stream_metadata_for_request(asset_id, token)
        .and_then(|metadata| metadata.qobuz_format_id)
        .or_else(|| qobuz_format_id_from_asset_id(asset_id))
        .or(Some(6));

    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let request_started = Instant::now();
    state
        .upnp()
        .mark_renderer_http_request(asset_id, token, "qobuz_get", range);
    match state
        .qobuz()
        .proxy_bytes_with_format(track_id, range, qobuz_format_id)
        .await
    {
        Ok(proxy) => {
            state.upnp().mark_qobuz_proxy_first_byte(
                asset_id,
                token,
                track_id,
                range,
                proxy.status.as_u16(),
                Some(elapsed_ms(request_started)),
            );
            let mut out = HeaderMap::new();
            if let Some(content_type) = proxy.content_type {
                if let Ok(value) = HeaderValue::from_str(&content_type) {
                    out.insert(header::CONTENT_TYPE, value);
                }
            } else {
                out.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/flac"));
            }
            if let Some(content_length) = proxy.content_length
                && let Ok(value) = HeaderValue::from_str(&content_length)
            {
                out.insert(header::CONTENT_LENGTH, value);
            }
            if let Some(content_range) = proxy.content_range
                && let Ok(value) = HeaderValue::from_str(&content_range)
            {
                out.insert(header::CONTENT_RANGE, value);
            }
            out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            (proxy.status, out, Body::from_stream(proxy.body)).into_response()
        }
        Err(e) => ApiError::internal(e).into_response(),
    }
}

pub async fn upnp_qobuz_stream_head(
    State(state): State<AppState>,
    Path(_track_id): Path<u64>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let asset_id = query.get("asset").map(String::as_str).unwrap_or("");
    let token = query.get("token").map(String::as_str).unwrap_or("");
    upnp_qobuz_stream_head_response(state, asset_id, token)
}

pub async fn upnp_qobuz_stream_path_head(
    State(state): State<AppState>,
    Path((asset_id, token, _track_id)): Path<(String, String, u64)>,
) -> Response {
    upnp_qobuz_stream_head_response(state, &asset_id, &token)
}

fn upnp_qobuz_stream_head_response(state: AppState, asset_id: &str, token: &str) -> Response {
    if !state.upnp().remote_stream_token_valid(asset_id, token) {
        return (StatusCode::UNAUTHORIZED, "Invalid UPnP Qobuz stream token").into_response();
    }
    state
        .upnp()
        .mark_renderer_http_request(asset_id, token, "qobuz_head", None);
    let Some(metadata) = state
        .upnp()
        .remote_stream_metadata_for_request(asset_id, token)
    else {
        return (StatusCode::NOT_FOUND, "UPnP Qobuz stream not found").into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(value) = HeaderValue::from_str(&metadata.mime_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Some(len) = metadata.byte_len
        && let Ok(value) = HeaderValue::from_str(&len.to_string())
    {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    (StatusCode::OK, headers, Body::empty()).into_response()
}

fn qobuz_format_id_from_asset_id(asset_id: &str) -> Option<u32> {
    let (_, format_id) = asset_id.rsplit_once('-')?;
    format_id.parse().ok()
}

pub async fn upnp_art(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = query.get("token").map(String::as_str).unwrap_or("");
    let Some(cover) = state.upnp().art_for_request(&asset_id, token) else {
        return (StatusCode::NOT_FOUND, "UPnP art not found").into_response();
    };
    artwork_response(cover)
}

pub async fn upnp_art_head(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = query.get("token").map(String::as_str).unwrap_or("");
    let Some(cover) = state.upnp().art_for_request(&asset_id, token) else {
        return (StatusCode::NOT_FOUND, "UPnP art not found").into_response();
    };
    let Some(headers) = artwork_headers(&cover.mime, &cover.data, Some(cover.data.len())) else {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported artwork").into_response();
    };
    (StatusCode::OK, headers, Body::empty()).into_response()
}

fn artwork_response(cover: TrackCover) -> Response {
    let Some(headers) = artwork_headers(&cover.mime, &cover.data, None) else {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported artwork").into_response();
    };
    (StatusCode::OK, headers, cover.data).into_response()
}

fn artwork_headers(mime: &str, data: &[u8], content_length: Option<usize>) -> Option<HeaderMap> {
    let mut headers = HeaderMap::new();
    let safe_mime = crate::library::safe_raster_artwork_mime(data, mime)?;
    let mime = HeaderValue::from_str(safe_mime)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_TYPE, mime);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(len) = content_length
        && let Ok(value) = HeaderValue::from_str(&len.to_string())
    {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    Some(headers)
}

#[cfg(test)]
mod derivative_stream_tests {
    use super::*;
    use crate::playback::test_support::app_state;
    use axum::body::to_bytes;

    /// iOS Safari sends `Range` requests and refuses rangeless media streams,
    /// so a ranged request that arrives while the derivative is still
    /// encoding must wait for completion and answer with a real 206.
    #[tokio::test]
    async fn ranged_request_on_generating_derivative_waits_and_serves_206() {
        let state = app_state("derivative-ranged-generating");
        let dir = std::env::temp_dir().join("fozmo-derivative-ranged-generating");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("tone.wav");
        crate::audio::transcode::test_support::write_wav(&source, 44_100, 12_000);
        let track_id = state.library().insert_track_for_test(&source);

        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1"));
        let response = stream_local_track_derivative(
            &state,
            track_id,
            source.clone(),
            DerivativeFormat::Flac,
            None,
            &headers,
        )
        .await;

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let content_range = response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .expect("ranged derivative response carries Content-Range")
            .to_string();
        assert!(
            content_range.starts_with("bytes 0-1/"),
            "Content-Range must report the completed derivative length: {content_range}"
        );
        assert!(!content_range.ends_with("/*"));
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body.len(), 2);
        assert_eq!(&body[..], b"fL", "derivative must start with FLAC magic");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod artwork_header_tests {
    use super::*;

    fn tiny_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    #[test]
    fn artwork_headers_harden_stream_art_responses() {
        let headers = artwork_headers("image/png", &tiny_png(), Some(123)).unwrap();

        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
        assert_eq!(
            headers
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            headers
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok()),
            Some("123")
        );

        assert!(artwork_headers("image/svg+xml", b"<svg></svg>", None).is_none());
    }
}

async fn stream_file_head(path: PathBuf) -> axum::response::Response {
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(_) => return (StatusCode::NOT_FOUND, "Track file not found").into_response(),
    };
    let mut headers = HeaderMap::new();
    add_upnp_stream_headers(&mut headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(audio_content_type_for_path(&path)),
    );
    if let Ok(value) = HeaderValue::from_str(&metadata.len().to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    (StatusCode::OK, headers, Body::empty()).into_response()
}

async fn stream_file_path(path: PathBuf, range_header: Option<&str>) -> FileStreamResult {
    stream_file_path_with_alignment(path, range_header, None, None).await
}

async fn stream_file_path_with_alignment(
    path: PathBuf,
    range_header: Option<&str>,
    alignment: Option<RangeAlignment>,
    trace_context: Option<RangeTraceContext<'_>>,
) -> FileStreamResult {
    let request_started = Instant::now();
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(_) => {
            return FileStreamResult {
                response: (StatusCode::NOT_FOUND, "Track file not found").into_response(),
                media_ready: false,
                status: StatusCode::NOT_FOUND,
                request_elapsed_ms: Some(elapsed_ms(request_started)),
            };
        }
    };
    let len = metadata.len();
    let mut headers = HeaderMap::new();
    add_upnp_stream_headers(&mut headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(audio_content_type_for_path(&path)),
    );
    if len == 0 {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        return FileStreamResult {
            response: (StatusCode::OK, headers, Body::empty()).into_response(),
            media_ready: false,
            status: StatusCode::OK,
            request_elapsed_ms: Some(elapsed_ms(request_started)),
        };
    }
    let range = range_header.and_then(|h| parse_byte_range(h, len));
    if let Some(context) = trace_context {
        log_unaligned_dop_range_request(
            context.asset_id,
            range,
            alignment,
            context.active_output_mode,
            context.target_rate,
            context.target_bits,
        );
    }
    let range = range.map(|range| align_byte_range(range, alignment, len));
    if matches!(range, Some(ParsedByteRange::Unsatisfiable)) {
        if let Ok(value) = HeaderValue::from_str(&format!("bytes */{len}")) {
            headers.insert(header::CONTENT_RANGE, value);
        }
        return FileStreamResult {
            response: (StatusCode::RANGE_NOT_SATISFIABLE, headers, Body::empty()).into_response(),
            media_ready: false,
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            request_elapsed_ms: Some(elapsed_ms(request_started)),
        };
    }
    let (start, end, status) = match range {
        Some(ParsedByteRange::Satisfiable { start, end }) => {
            (start, end, StatusCode::PARTIAL_CONTENT)
        }
        Some(ParsedByteRange::Unsatisfiable) => unreachable!(),
        None => (0, len.saturating_sub(1), StatusCode::OK),
    };
    let read_len = end.saturating_sub(start).saturating_add(1);
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(e) => {
            return FileStreamResult {
                response: ApiError::internal(format!("open stream file: {e}")).into_response(),
                media_ready: false,
                status: StatusCode::INTERNAL_SERVER_ERROR,
                request_elapsed_ms: Some(elapsed_ms(request_started)),
            };
        }
    };
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
        return FileStreamResult {
            response: ApiError::internal(format!("seek stream file: {e}")).into_response(),
            media_ready: false,
            status: StatusCode::INTERNAL_SERVER_ERROR,
            request_elapsed_ms: Some(elapsed_ms(request_started)),
        };
    }
    if status == StatusCode::PARTIAL_CONTENT
        && let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}"))
    {
        headers.insert(header::CONTENT_RANGE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&read_len.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    let stream =
        futures_util::stream::unfold((file, read_len), |(mut file, remaining)| async move {
            if remaining == 0 {
                return None;
            }
            let chunk_len = remaining.min(FILE_STREAM_CHUNK_SIZE) as usize;
            let mut chunk = vec![0; chunk_len];
            match file.read(&mut chunk).await {
                Ok(0) => None,
                Ok(n) => {
                    chunk.truncate(n);
                    Some((
                        Ok::<Bytes, std::io::Error>(Bytes::from(chunk)),
                        (file, remaining.saturating_sub(n as u64)),
                    ))
                }
                Err(e) => Some((Err(e), (file, 0))),
            }
        });
    FileStreamResult {
        response: (status, headers, Body::from_stream(stream)).into_response(),
        media_ready: true,
        status,
        request_elapsed_ms: Some(elapsed_ms(request_started)),
    }
}

fn upnp_asset_range_alignment(asset: &UpnpCachedAsset) -> Option<RangeAlignment> {
    dop_wav_range_alignment(
        asset.active_output_mode.as_deref(),
        &asset.mime_type,
        asset.target_bits,
    )
}

fn generated_stream_range_alignment(stream: &UpnpGeneratedDspStream) -> Option<RangeAlignment> {
    dop_wav_range_alignment(
        stream.active_output_mode.as_deref(),
        &stream.mime_type,
        stream.target_bits,
    )
}

fn dop_wav_range_alignment(
    active_output_mode: Option<&str>,
    mime_type: &str,
    target_bits: u32,
) -> Option<RangeAlignment> {
    let active_output_mode = active_output_mode?;
    let is_dsd = matches!(active_output_mode, "Dsd64" | "Dsd128" | "Dsd256");
    let is_wav = matches!(mime_type, "audio/wav" | "audio/wave" | "audio/x-wav");
    (is_dsd && is_wav && target_bits == 24).then_some(RangeAlignment {
        data_start: WAV_HEADER_BYTES,
        frame_bytes: DOP_WAV_FRAME_BYTES,
    })
}

fn generated_chunk_contains_audio_payload(bytes: &[u8], absolute_start: u64) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let Some(chunk_end) = absolute_start.checked_add(bytes.len() as u64 - 1) else {
        return false;
    };
    chunk_end >= WAV_HEADER_BYTES
}

fn generated_chunk_contains_marker_valid_dop_frame(bytes: &[u8], absolute_start: u64) -> bool {
    if bytes.len() < 6 {
        return false;
    }
    let Some(chunk_end) = absolute_start.checked_add(bytes.len() as u64 - 1) else {
        return false;
    };
    if chunk_end < WAV_HEADER_BYTES {
        return false;
    }
    let data_start = absolute_start.max(WAV_HEADER_BYTES);
    let first_frame_data_offset = data_start - WAV_HEADER_BYTES;
    let frame_aligned_data_offset = first_frame_data_offset
        + (DOP_WAV_FRAME_BYTES - first_frame_data_offset % DOP_WAV_FRAME_BYTES)
            % DOP_WAV_FRAME_BYTES;
    let mut frame_abs = WAV_HEADER_BYTES + frame_aligned_data_offset;
    while frame_abs.saturating_add(5) <= chunk_end {
        let idx = (frame_abs - absolute_start) as usize;
        let frame = &bytes[idx..idx + 6];
        let expected_marker =
            if ((frame_abs - WAV_HEADER_BYTES) / DOP_WAV_FRAME_BYTES).is_multiple_of(2) {
                0x05
            } else {
                0xFA
            };
        if frame[2] == expected_marker && frame[5] == expected_marker {
            return true;
        }
        frame_abs = frame_abs.saturating_add(DOP_WAV_FRAME_BYTES);
    }
    false
}

fn generated_stream_startup_pacing(
    stream: &UpnpGeneratedDspStream,
    start: u64,
    status: StatusCode,
) -> Option<GeneratedStartupPacing> {
    if !matches!(status, StatusCode::OK | StatusCode::PARTIAL_CONTENT)
        || start != 0
        || generated_stream_range_alignment(stream).is_none()
        || !generated_stream_target_is_hegel_h390(stream)
    {
        return None;
    }
    let bytes_per_sample = u64::from(stream.target_bits / 8);
    let bytes_per_second = u64::from(stream.target_rate)
        .saturating_mul(2)
        .saturating_mul(bytes_per_sample);
    if bytes_per_second == 0 {
        return None;
    }
    Some(GeneratedStartupPacing {
        bytes_per_second,
        lead_bytes: duration_to_bytes(HEGEL_DOP_STARTUP_PACE_LEAD, bytes_per_second),
        pace_until_data_bytes: duration_to_bytes(HEGEL_DOP_STARTUP_PACE_WINDOW, bytes_per_second),
    })
}

fn generated_stream_target_is_hegel_h390(stream: &UpnpGeneratedDspStream) -> bool {
    let combined = [
        stream.target.name.as_str(),
        stream.target.model.as_deref().unwrap_or_default(),
        stream.target.manufacturer.as_deref().unwrap_or_default(),
    ]
    .join(" ")
    .to_ascii_lowercase();
    combined.contains("hegel") && combined.contains("h390")
}

fn pace_generated_startup_stream(
    body: BoxStream<'static, Result<Bytes, IoError>>,
    pacing: GeneratedStartupPacing,
    start: u64,
    zone_id: String,
    asset_id: String,
) -> BoxStream<'static, Result<Bytes, IoError>> {
    let pacer = GeneratedStartupPacer {
        pacing,
        next_offset: start,
        first_audio_at: None,
        data_bytes_sent: 0,
        announced_delay: false,
    };
    stream::unfold(
        (body, pacer, zone_id, asset_id),
        |(mut body, mut pacer, zone_id, asset_id)| async move {
            let item = body.next().await?;
            if let Ok(bytes) = item.as_ref() {
                let delay = pacer.delay_for_chunk(bytes);
                if !delay.is_zero() {
                    if !pacer.announced_delay {
                        pacer.announced_delay = true;
                        eprintln!(
                            "upnp: play trace event=generated_startup_pacing zone={} asset={} delay_ms={} bytes_per_second={} lead_bytes={} pace_until_data_bytes={}",
                            zone_id,
                            asset_id,
                            delay.as_millis(),
                            pacer.pacing.bytes_per_second,
                            pacer.pacing.lead_bytes,
                            pacer.pacing.pace_until_data_bytes
                        );
                    }
                    tokio::time::sleep(delay).await;
                }
            }
            Some((item, (body, pacer, zone_id, asset_id)))
        },
    )
    .boxed()
}

impl GeneratedStartupPacer {
    fn delay_for_chunk(&mut self, bytes: &[u8]) -> Duration {
        if bytes.is_empty() {
            return Duration::ZERO;
        }
        let chunk_start = self.next_offset;
        self.next_offset = self.next_offset.saturating_add(bytes.len() as u64);
        let audio_bytes = generated_chunk_audio_byte_count(bytes, chunk_start);
        if audio_bytes == 0 || self.data_bytes_sent >= self.pacing.pace_until_data_bytes {
            return Duration::ZERO;
        }
        let first_audio_at = *self.first_audio_at.get_or_insert_with(Instant::now);
        self.data_bytes_sent = self.data_bytes_sent.saturating_add(audio_bytes);
        generated_startup_pacing_delay(
            self.data_bytes_sent.min(self.pacing.pace_until_data_bytes),
            first_audio_at.elapsed(),
            self.pacing.bytes_per_second,
            self.pacing.lead_bytes,
        )
    }
}

fn generated_chunk_audio_byte_count(bytes: &[u8], absolute_start: u64) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let Some(chunk_end_exclusive) = absolute_start.checked_add(bytes.len() as u64) else {
        return 0;
    };
    chunk_end_exclusive.saturating_sub(WAV_HEADER_BYTES)
        - absolute_start.saturating_sub(WAV_HEADER_BYTES)
}

fn generated_startup_pacing_delay(
    data_bytes_sent: u64,
    elapsed: Duration,
    bytes_per_second: u64,
    lead_bytes: u64,
) -> Duration {
    if bytes_per_second == 0 {
        return Duration::ZERO;
    }
    let allowed_bytes = duration_to_bytes(elapsed, bytes_per_second).saturating_add(lead_bytes);
    if data_bytes_sent <= allowed_bytes {
        return Duration::ZERO;
    }
    bytes_to_duration(data_bytes_sent - allowed_bytes, bytes_per_second)
}

fn duration_to_bytes(duration: Duration, bytes_per_second: u64) -> u64 {
    (duration.as_secs_f64() * bytes_per_second as f64).round() as u64
}

fn bytes_to_duration(bytes: u64, bytes_per_second: u64) -> Duration {
    Duration::from_secs_f64(bytes as f64 / bytes_per_second as f64)
}

fn align_byte_range(
    range: ParsedByteRange,
    alignment: Option<RangeAlignment>,
    len: u64,
) -> ParsedByteRange {
    let Some(alignment) = alignment.filter(|alignment| alignment.frame_bytes > 0) else {
        return range;
    };
    let ParsedByteRange::Satisfiable { start, end } = range else {
        return range;
    };
    let end = align_range_end(end, alignment, len);
    ParsedByteRange::Satisfiable {
        start,
        end: end.max(start),
    }
}

fn align_range_end(end: u64, alignment: RangeAlignment, len: u64) -> u64 {
    if end < alignment.data_start || end >= len.saturating_sub(1) {
        return end;
    }
    let offset = end - alignment.data_start + 1;
    let aligned_len = offset.div_ceil(alignment.frame_bytes) * alignment.frame_bytes;
    alignment
        .data_start
        .saturating_add(aligned_len)
        .saturating_sub(1)
        .min(len.saturating_sub(1))
}

fn log_unaligned_dop_range_request(
    asset_id: &str,
    range: Option<ParsedByteRange>,
    alignment: Option<RangeAlignment>,
    active_output_mode: Option<&str>,
    target_rate: Option<u32>,
    target_bits: u32,
) {
    let Some(alignment) = alignment.filter(|alignment| alignment.frame_bytes > 0) else {
        return;
    };
    let Some(ParsedByteRange::Satisfiable { start, .. }) = range else {
        return;
    };
    if start <= alignment.data_start {
        return;
    }
    let data_offset = start - alignment.data_start;
    let remainder = data_offset % alignment.frame_bytes;
    if remainder == 0 {
        return;
    }
    tracing::warn!(
        event = "upnp_dop_unaligned_range_request",
        asset_id,
        requested_start = start,
        served_start = start,
        frame_bytes = alignment.frame_bytes,
        data_start = alignment.data_start,
        data_offset,
        remainder,
        active_output_mode,
        target_rate,
        target_bits,
        "UPnP renderer requested an unaligned DoP WAV byte range; serving exact start"
    );
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn add_upnp_stream_headers(headers: &mut HeaderMap) {
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        HeaderName::from_static("transfermode.dlna.org"),
        HeaderValue::from_static("Streaming"),
    );
    headers.insert(
        HeaderName::from_static("contentfeatures.dlna.org"),
        HeaderValue::from_static(DLNA_CONTENT_FEATURES),
    );
}

fn generated_stream_range_not_satisfiable(byte_len: Option<u64>) -> Response {
    let mut headers = HeaderMap::new();
    add_upnp_stream_headers(&mut headers);
    if let Some(len) = byte_len
        && let Ok(value) = HeaderValue::from_str(&format!("bytes */{len}"))
    {
        headers.insert(header::CONTENT_RANGE, value);
    }
    (
        StatusCode::RANGE_NOT_SATISFIABLE,
        headers,
        Body::from("Generated UPnP DSP stream byte range is not satisfiable"),
    )
        .into_response()
}

fn audio_content_type_for_path(path: &StdPath) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("flac") => "audio/flac",
        Some("mp3") => "audio/mpeg",
        Some("wav") | Some("wave") => "audio/wav",
        Some("m4a") | Some("mp4") => "audio/mp4",
        Some("ogg") | Some("oga") => "audio/ogg",
        Some("opus") => "audio/opus",
        Some("aif") | Some("aiff") => "audio/aiff",
        _ => "application/octet-stream",
    }
}

fn parse_byte_range(header: &str, len: u64) -> Option<ParsedByteRange> {
    let spec = header.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 || len == 0 {
            return None;
        }
        let start = len.saturating_sub(suffix);
        return Some(ParsedByteRange::Satisfiable {
            start,
            end: len - 1,
        });
    }
    let start = start.parse::<u64>().ok()?;
    if start >= len {
        return Some(ParsedByteRange::Unsatisfiable);
    }
    let end = if end.is_empty() {
        len - 1
    } else {
        end.parse::<u64>().ok()?.min(len - 1)
    };
    if end < start {
        None
    } else {
        Some(ParsedByteRange::Satisfiable { start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::upnp::{UpnpRendererTarget, UpnpSource};
    use crate::playback::test_support::{app_state, app_state_with_pairing};
    use crate::protocol::{
        CapabilityDetectionSource, CapabilityDetectionStatus, PlaybackConfig, SourceRef,
    };
    use axum::body::to_bytes;
    use axum::http::{Method, Request};
    use axum::response::IntoResponse;
    use proptest::prelude::*;
    use rand::{RngCore, rngs::OsRng};
    use tower::ServiceExt;

    proptest! {
        #[test]
        fn property_parsed_byte_ranges_are_bounded_and_ordered(
            len in 1_u64..1_000_000,
            start in any::<u64>(),
            end in any::<u64>()
        ) {
            let header = format!("bytes={start}-{end}");
            if let Some(ParsedByteRange::Satisfiable { start, end }) = parse_byte_range(&header, len) {
                prop_assert!(start <= end);
                prop_assert!(end < len);
            }
        }

        #[test]
        fn property_suffix_ranges_never_underflow(len in 1_u64..1_000_000, suffix in 1_u64..2_000_000) {
            let parsed = parse_byte_range(&format!("bytes=-{suffix}"), len).unwrap();
            let ParsedByteRange::Satisfiable { start, end } = parsed else {
                prop_assert!(false, "suffix range should be satisfiable");
                return Ok(());
            };
            prop_assert!(start <= end);
            prop_assert_eq!(end, len - 1);
        }
    }

    const LAN_PEER: SocketAddr = SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 50)),
        4444,
    );
    const LOOPBACK_PEER: SocketAddr = SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
        4444,
    );

    fn stream_router(state: &AppState) -> Router {
        routes().with_state(state.clone())
    }

    fn remote_surface_router(state: &AppState) -> Router {
        // Deliberately no `require_remote_auth` middleware: the handlers must
        // deny remote-surface requests that lack the auth marker.
        routes()
            .layer(Extension(RequestSurface::Remote))
            .with_state(state.clone())
    }

    fn range_tuple(range: ParsedByteRange) -> Option<(u64, u64)> {
        match range {
            ParsedByteRange::Satisfiable { start, end } => Some((start, end)),
            ParsedByteRange::Unsatisfiable => None,
        }
    }

    fn stream_request(
        path: &str,
        cookie: Option<&str>,
        auth_header: Option<&str>,
        range: Option<&str>,
    ) -> Request<Body> {
        stream_request_from_peer(path, cookie, auth_header, range, LAN_PEER)
    }

    fn stream_request_from_peer(
        path: &str,
        cookie: Option<&str>,
        auth_header: Option<&str>,
        range: Option<&str>,
        peer: SocketAddr,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(header::HOST, "core.test:3000");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        if let Some(token) = auth_header {
            builder = builder.header(crate::app::identity::AUTH_HEADER, token);
        }
        if let Some(range) = range {
            builder = builder.header(header::RANGE, range);
        }
        let mut request = builder.body(Body::empty()).expect("request should build");
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    fn same_origin_stream_request(path: &str) -> Request<Body> {
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(header::HOST, "core.test:3000")
            .header(header::REFERER, "http://core.test:3000/")
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .expect("request should build");
        request.extensions_mut().insert(ConnectInfo(LAN_PEER));
        request
    }

    fn cross_site_stream_request(path: &str) -> Request<Body> {
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(header::HOST, "core.test:3000")
            .header(header::REFERER, "http://evil.test/")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .expect("request should build");
        request.extensions_mut().insert(ConnectInfo(LAN_PEER));
        request
    }

    async fn stream_status(app: &Router, request: Request<Body>) -> StatusCode {
        app.clone()
            .oneshot(request)
            .await
            .expect("router should respond")
            .status()
    }

    fn control_cookie(state: &AppState) -> String {
        let token = state.pairing().create_control_session(None).unwrap().token;
        format!("{}={}", crate::zones::CONTROL_SESSION_COOKIE, token)
    }

    fn seeded_track(state: &AppState, name: &str) -> (i64, PathBuf) {
        let path = temp_file(name);
        std::fs::write(&path, b"0123456789").unwrap();
        (state.library().insert_track_for_test(&path), path)
    }

    fn seeded_wav_track(state: &AppState, name: &str) -> (i64, PathBuf) {
        let mut token = [0_u8; 8];
        OsRng.fill_bytes(&mut token);
        let path = std::env::temp_dir().join(format!("{name}-{:x}.wav", u64::from_le_bytes(token)));
        crate::audio::transcode::test_support::write_wav(&path, 48_000, 9_600);
        (state.library().insert_track_for_test(&path), path)
    }

    fn upnp_test_target() -> UpnpRendererTarget {
        UpnpRendererTarget {
            id: "renderer".to_string(),
            name: "Renderer".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: None,
            manufacturer: None,
            av_transport_control_url: "/AVTransport".to_string(),
            rendering_control_url: None,
            connection_manager_url: None,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
            max_dsd_rate: None,
            capability_detection_source: CapabilityDetectionSource::Probed,
            capability_detection_status: CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        }
    }

    fn upnp_test_playback_config() -> PlaybackConfig {
        PlaybackConfig {
            filter_type: "Minimum16k".to_string(),
            target_rate: 96_000,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: false,
            dither_mode: "Auto".to_string(),
            output_mode: "Pcm".to_string(),
            dsd_modulator: "Standard".to_string(),
            dsd_isi_penalty: 0.0,
            dsd_rules: Vec::new(),
            headroom_db: 0.0,
            dsp_buffer_ms: 0,
            volume: 1.0,
            eq: Default::default(),
            output_device: None,
        }
    }

    fn generated_stream_source_ref(track_id: i64) -> SourceRef {
        SourceRef::LocalTrack {
            track_id,
            file_name: None,
            title: Some("Generated".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: Some(0.2),
            ext_hint: Some("wav".to_string()),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    #[tokio::test]
    async fn local_stream_rejects_unauthenticated_lan_peer_when_pairing_required() {
        let state = app_state_with_pairing("stream-local-unauth", true, false);
        let app = stream_router(&state);

        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/local/1", None, None, None)
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn local_stream_accepts_control_session_cookie() {
        let state = app_state_with_pairing("stream-local-cookie", true, false);
        let (track_id, path) = seeded_track(&state, "stream-local-cookie");
        let cookie = control_cookie(&state);
        let app = stream_router(&state);
        let uri = format!("/api/stream/local/{track_id}");

        assert_eq!(
            stream_status(&app, stream_request(&uri, Some(&cookie), None, None)).await,
            StatusCode::OK
        );

        let response = app
            .clone()
            .oneshot(stream_request(&uri, Some(&cookie), None, Some("bytes=5-")))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 5-9/10")
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn local_stream_accepts_header_tokens() {
        let state = app_state_with_pairing("stream-local-tokens", true, false);
        let (track_id, path) = seeded_track(&state, "stream-local-tokens");
        let app = stream_router(&state);
        let uri = format!("/api/stream/local/{track_id}");

        let control = state.pairing().create_control_session(None).unwrap().token;
        assert_eq!(
            stream_status(&app, stream_request(&uri, None, Some(&control), None)).await,
            StatusCode::OK
        );

        let stream_token = state
            .pairing()
            .create_stream_token(60, crate::settings::AuthTokenBinding::default())
            .unwrap()
            .token;
        assert_eq!(
            stream_status(&app, stream_request(&uri, None, Some(&stream_token), None)).await,
            StatusCode::OK
        );

        let agent_token = state.pairing().create_agent_token(None).unwrap().token;
        assert_eq!(
            stream_status(&app, stream_request(&uri, None, Some(&agent_token), None)).await,
            StatusCode::OK
        );

        let agent_stream_session = state.pairing().create_agent_stream_session();
        assert_eq!(
            stream_status(
                &app,
                stream_request(&uri, None, Some(&agent_stream_session), None)
            )
            .await,
            StatusCode::OK
        );
        state
            .pairing()
            .revoke_agent_stream_session(&agent_stream_session);
        assert_eq!(
            stream_status(
                &app,
                stream_request(&uri, None, Some(&agent_stream_session), None)
            )
            .await,
            StatusCode::FORBIDDEN
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn local_stream_accepts_same_origin_lan_browser_request() {
        let state = app_state_with_pairing("stream-local-same-origin", true, false);
        let (track_id, path) = seeded_track(&state, "stream-local-same-origin");
        let app = stream_router(&state);

        assert_eq!(
            stream_status(
                &app,
                same_origin_stream_request(&format!("/api/stream/local/{track_id}")),
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            stream_status(
                &app,
                cross_site_stream_request(&format!("/api/stream/local/{track_id}")),
            )
            .await,
            StatusCode::FORBIDDEN
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn remote_surface_streams_require_the_auth_marker() {
        // Even a valid remote session cookie must be rejected when the remote
        // auth middleware did not run: RequestSurface::Remote alone (or raw
        // cookies) is never proof of authentication in the handlers.
        let state = app_state("stream-remote-marker");
        let (track_id, path) = seeded_track(&state, "stream-remote-marker");
        let remote = state.pairing().create_remote_session(None).unwrap().token;
        let remote_cookie = format!("{}={}", crate::zones::REMOTE_SESSION_COOKIE, remote);
        let app = remote_surface_router(&state);

        assert_eq!(
            stream_status(
                &app,
                stream_request(
                    &format!("/api/stream/local/{track_id}"),
                    Some(&remote_cookie),
                    None,
                    None,
                ),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/qobuz/1", Some(&remote_cookie), None, None),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            stream_status(
                &app,
                stream_request(
                    "/api/stream/qobuz/1?quality=lossy",
                    Some(&remote_cookie),
                    None,
                    None,
                ),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );

        // Header/query-style credentials must not work remotely either.
        let control = state.pairing().create_control_session(None).unwrap().token;
        assert_eq!(
            stream_status(
                &app,
                stream_request(
                    &format!("/api/stream/local/{track_id}"),
                    None,
                    Some(&control),
                    None,
                ),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );

        // The Opus derivative variant shares the same remote auth posture.
        assert_eq!(
            stream_status(
                &app,
                stream_request(
                    &format!("/api/stream/local/{track_id}?variant=opus"),
                    Some(&remote_cookie),
                    None,
                    None,
                ),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn qobuz_browser_stream_query_selects_only_the_lossy_format() {
        let empty = HashMap::new();
        assert_eq!(qobuz_browser_requested_format(&empty).unwrap(), None);

        let mut quality = HashMap::new();
        quality.insert("quality".to_string(), "lossy".to_string());
        assert_eq!(qobuz_browser_requested_format(&quality).unwrap(), Some(5));

        let mut format = HashMap::new();
        format.insert("format".to_string(), "5".to_string());
        assert_eq!(qobuz_browser_requested_format(&format).unwrap(), Some(5));

        let mut original = HashMap::new();
        original.insert("quality".to_string(), "original".to_string());
        assert_eq!(qobuz_browser_requested_format(&original).unwrap(), None);

        let mut unsupported = HashMap::new();
        unsupported.insert("format".to_string(), "6".to_string());
        assert!(qobuz_browser_requested_format(&unsupported).is_err());
    }

    #[tokio::test]
    async fn local_stream_rejects_unknown_variants() {
        let state = app_state_with_pairing("stream-variant-unknown", true, false);
        let (track_id, path) = seeded_track(&state, "stream-variant-unknown");
        let cookie = control_cookie(&state);
        let app = stream_router(&state);

        assert_eq!(
            stream_status(
                &app,
                stream_request(
                    &format!("/api/stream/local/{track_id}?variant=mp3"),
                    Some(&cookie),
                    None,
                    None,
                ),
            )
            .await,
            StatusCode::BAD_REQUEST
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn local_opus_variant_streams_ogg_and_reuses_the_derivative_cache() {
        let state = app_state_with_pairing("stream-opus-variant", true, false);
        let (track_id, path) = seeded_wav_track(&state, "stream-opus-variant");
        let cookie = control_cookie(&state);
        let app = stream_router(&state);
        let uri = format!("/api/stream/local/{track_id}?variant=opus");

        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(stream_request(&uri, Some(&cookie), None, None))
                .await
                .expect("router should respond");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("audio/ogg")
            );
            let body = to_bytes(response.into_body(), 16 * 1024 * 1024)
                .await
                .unwrap();
            assert_eq!(&body[..4], b"OggS");
            assert!(body.windows(8).any(|window| window == b"OpusHead"));
        }
        assert_eq!(state.local_transcode().encodes_started(), 1);

        // Original-quality requests are untouched by the new variant.
        let original = app
            .clone()
            .oneshot(stream_request(
                &format!("/api/stream/local/{track_id}"),
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(original.status(), StatusCode::OK);
        assert_eq!(
            original
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("audio/wav")
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn qobuz_stream_requires_pairing_when_auth_required() {
        let state = app_state_with_pairing("stream-qobuz-auth", true, false);
        let app = stream_router(&state);

        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/qobuz/1", None, None, None)
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/qobuz/1?quality=lossy", None, None, None)
            )
            .await,
            StatusCode::FORBIDDEN
        );

        // With a valid control cookie the auth gate passes; the request then
        // fails server-side because the test state is not logged in to Qobuz,
        // and the error body stays generic (no upstream URL/credential leak).
        let cookie = control_cookie(&state);
        let response = app
            .clone()
            .oneshot(stream_request(
                "/api/stream/qobuz/1",
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"Qobuz stream request failed");
    }

    #[tokio::test]
    async fn qobuz_stream_accepts_same_origin_lan_browser_request() {
        let state = app_state_with_pairing("stream-qobuz-same-origin", true, false);
        let app = stream_router(&state);

        assert_eq!(
            stream_status(&app, same_origin_stream_request("/api/stream/qobuz/1")).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            stream_status(&app, cross_site_stream_request("/api/stream/qobuz/1")).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn qobuz_stream_rejects_unsupported_browser_stream_queries() {
        let state = app_state_with_pairing("stream-qobuz-query", true, false);
        let cookie = control_cookie(&state);
        let app = stream_router(&state);

        assert_eq!(
            stream_status(
                &app,
                stream_request(
                    "/api/stream/qobuz/1?quality=lossless",
                    Some(&cookie),
                    None,
                    None,
                ),
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/qobuz/1?format=6", Some(&cookie), None, None),
            )
            .await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn qobuz_stream_rejects_unauthenticated_lan_peer_when_pairing_disabled() {
        let state = app_state("stream-qobuz-lan-denied");
        assert!(!state.pairing().auth_required());
        let app = stream_router(&state);

        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/qobuz/1", None, None, None)
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn qobuz_stream_accepts_lan_stream_token_and_control_cookie_when_pairing_disabled() {
        let state = app_state("stream-qobuz-lan-authorized");
        assert!(!state.pairing().auth_required());
        let app = stream_router(&state);

        let stream_token = state
            .pairing()
            .create_stream_token(60, crate::settings::AuthTokenBinding::default())
            .unwrap()
            .token;
        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/qobuz/1", None, Some(&stream_token), None),
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR
        );

        let cookie = control_cookie(&state);
        assert_eq!(
            stream_status(
                &app,
                stream_request("/api/stream/qobuz/1", Some(&cookie), None, None),
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn qobuz_stream_accepts_loopback_browser_request_when_pairing_disabled() {
        let state = app_state("stream-qobuz-loopback");
        assert!(!state.pairing().auth_required());
        let app = stream_router(&state);

        assert_eq!(
            stream_status(
                &app,
                stream_request_from_peer("/api/stream/qobuz/1", None, None, None, LOOPBACK_PEER,),
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn sonos_qobuz_stream_rejects_token_for_different_track() {
        let state = app_state("sonos-qobuz-track-scope");
        let (asset_id, token) = state.sonos().register_qobuz_remote_stream(111, 6, None);
        let headers = HeaderMap::new();

        let matching =
            sonos_qobuz_stream_response(state.clone(), 111, &asset_id, &token, headers.clone())
                .await;
        assert_eq!(matching.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(matching.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"Internal server error");

        let mismatched = sonos_qobuz_stream_response(state, 222, &asset_id, &token, headers).await;
        assert_eq!(mismatched.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn local_file_range_returns_exact_partial_content() {
        let path = temp_file("range-partial");
        tokio::fs::write(&path, b"0123456789").await.unwrap();

        let result = stream_file_path(path.clone(), Some("bytes=5-")).await;
        assert!(result.media_ready);
        let response = result.response;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 5-9/10")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("5")
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"56789");

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn dop_wav_range_preserves_requested_start() {
        let path = temp_file("range-dop-align");
        let bytes: Vec<u8> = (0_u8..100).collect();
        tokio::fs::write(&path, &bytes).await.unwrap();

        let result = stream_file_path_with_alignment(
            path.clone(),
            Some("bytes=58-"),
            Some(RangeAlignment {
                data_start: WAV_HEADER_BYTES,
                frame_bytes: DOP_WAV_FRAME_BYTES,
            }),
            Some(RangeTraceContext {
                asset_id: "range-dop-align",
                active_output_mode: Some("Dsd64"),
                target_rate: Some(192_000),
                target_bits: 24,
            }),
        )
        .await;
        assert!(result.media_ready);
        let response = result.response;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 58-99/100")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("42")
        );
        let body = to_bytes(response.into_body(), 128).await.unwrap();
        assert_eq!(&body[..], &bytes[58..]);

        let _ = tokio::fs::remove_file(path).await;
    }

    #[test]
    fn dop_wav_alignment_preserves_starts_and_expands_bounded_ends() {
        let alignment = Some(RangeAlignment {
            data_start: WAV_HEADER_BYTES,
            frame_bytes: DOP_WAV_FRAME_BYTES,
        });

        assert_eq!(
            range_tuple(align_byte_range(
                ParsedByteRange::Satisfiable { start: 0, end: 43 },
                alignment,
                100
            )),
            Some((0, 43))
        );
        assert_eq!(
            range_tuple(align_byte_range(
                ParsedByteRange::Satisfiable { start: 58, end: 60 },
                alignment,
                100
            )),
            Some((58, 61))
        );
    }

    #[test]
    fn generated_dop_marker_detection_uses_six_byte_frame_phase() {
        let bytes = [
            0, 0, 0x05, 0, 0, 0x05, 0, 0, 0xFA, 0, 0, 0xFA, 0, 0, 0x05, 0, 0, 0x05,
        ];

        assert!(generated_chunk_contains_marker_valid_dop_frame(
            &bytes[0..6],
            44
        ));
        assert!(generated_chunk_contains_marker_valid_dop_frame(
            &bytes[6..12],
            50
        ));
        assert!(generated_chunk_contains_marker_valid_dop_frame(
            &bytes[12..18],
            56
        ));
    }

    #[test]
    fn generated_hegel_dop_pacing_applies_to_zero_start_partial_content() {
        let state = app_state("generated-hegel-dop-pacing");
        let mut target = upnp_test_target();
        target.name = "Hegel H390".to_string();
        target.model = Some("H390".to_string());
        let asset = state.upnp().prepare_source(
            UpnpSource::GeneratedDspStream {
                id: "hegel-dop-pacing".to_string(),
                zone_id: "zone".to_string(),
                source_ref: generated_stream_source_ref(1),
                mime_type: "audio/wav".to_string(),
                tags: Default::default(),
                source_rate: 48_000,
                source_bits: 16,
                target_rate: 192_000,
                target_bits: 24,
                active_output_mode: Some("Dsd64".to_string()),
                byte_len: Some(1024),
                dop_lead_in_data_len: 172_800,
                target: target.clone(),
                playback_config: upnp_test_playback_config(),
            },
            &target,
        );
        let token = asset.stream_url.split("token=").nth(1).unwrap();
        let stream = state
            .upnp()
            .generated_dsp_stream_for_request(&asset.id, token)
            .expect("generated stream");

        assert!(generated_stream_startup_pacing(&stream, 0, StatusCode::OK).is_some());
        assert!(generated_stream_startup_pacing(&stream, 0, StatusCode::PARTIAL_CONTENT).is_some());
        assert!(
            generated_stream_startup_pacing(&stream, WAV_HEADER_BYTES, StatusCode::PARTIAL_CONTENT)
                .is_none()
        );
    }

    #[test]
    fn generated_startup_pacing_counts_only_wav_audio_bytes() {
        assert_eq!(generated_chunk_audio_byte_count(&[0; 44], 0), 0);
        assert_eq!(generated_chunk_audio_byte_count(&[0; 12], 40), 8);
        assert_eq!(generated_chunk_audio_byte_count(&[0; 12], 44), 12);
    }

    #[test]
    fn generated_startup_pacing_delay_respects_lead_window() {
        let bytes_per_second = 1_000;

        assert_eq!(
            generated_startup_pacing_delay(200, Duration::ZERO, bytes_per_second, 250),
            Duration::ZERO
        );
        assert_eq!(
            generated_startup_pacing_delay(1_000, Duration::ZERO, bytes_per_second, 250),
            Duration::from_millis(750)
        );
        assert_eq!(
            generated_startup_pacing_delay(
                1_000,
                Duration::from_millis(750),
                bytes_per_second,
                250
            ),
            Duration::ZERO
        );
    }

    #[tokio::test]
    async fn local_file_suffix_range_returns_tail() {
        let path = temp_file("range-suffix");
        tokio::fs::write(&path, b"0123456789").await.unwrap();

        let result = stream_file_path(path.clone(), Some("bytes=-3")).await;
        assert!(result.media_ready);
        let response = result.response;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 7-9/10")
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"789");

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn local_file_unsatisfiable_range_returns_416() {
        let path = temp_file("range-unsat");
        tokio::fs::write(&path, b"0123456789").await.unwrap();

        let result = stream_file_path(path.clone(), Some("bytes=99-")).await;
        assert!(!result.media_ready);
        let response = result.response;
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes */10")
        );

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn local_file_head_returns_metadata_without_body() {
        let path = temp_file("head-local");
        tokio::fs::write(&path, b"0123456789").await.unwrap();

        let response = stream_file_head(path.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok()),
            Some("bytes")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("10")
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert!(body.is_empty());

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn upnp_probe_asset_outside_probe_cache_is_not_streamed() {
        let state = app_state("upnp-probe-outside-cache");
        let path = temp_file("upnp-probe-outside-cache");
        tokio::fs::write(&path, b"not probe bytes").await.unwrap();
        let target = upnp_test_target();
        let asset = state.upnp().prepare_source(
            UpnpSource::LocalFile {
                source_ref: SourceRef::LocalTrack {
                    track_id: -1,
                    file_name: Some(path.to_string_lossy().to_string()),
                    title: Some("Probe".to_string()),
                    artist: Some("Fozmo".to_string()),
                    album: Some("Output probe".to_string()),
                    album_artist: None,
                    album_id: None,
                    art_id: None,
                    duration_secs: Some(1.0),
                    ext_hint: None,
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                path: path.clone(),
                tags: Default::default(),
                cover: None,
                byte_len: Some(15),
                source_rate: 44_100,
                source_bits: 16,
            },
            &target,
        );
        let token = asset
            .stream_url
            .split("token=")
            .nth(1)
            .expect("token")
            .to_string();
        let mut query = HashMap::new();
        query.insert("token".to_string(), token);

        let response = upnp_stream(State(state), Path(asset.id), Query(query), HeaderMap::new())
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn generated_upnp_dsp_wav_range_returns_partial_content_and_exact_lengths() {
        let state = app_state("generated-upnp-range");
        let (track_id, path) = seeded_wav_track(&state, "generated-upnp-range");
        let source_ref = generated_stream_source_ref(track_id);
        let target = upnp_test_target();
        let byte_len = 44 + (96_000_f64 * 0.2).round() as u64 * 6;
        let asset = state.upnp().prepare_source(
            UpnpSource::GeneratedDspStream {
                id: "generated-range".to_string(),
                zone_id: "zone".to_string(),
                source_ref,
                mime_type: "audio/wav".to_string(),
                tags: crate::audio::player::TrackTags {
                    title: Some("Generated".to_string()),
                    artist: Some("Artist".to_string()),
                    album: Some("Album".to_string()),
                    duration_secs: Some(0.2),
                    ..Default::default()
                },
                source_rate: 48_000,
                source_bits: 16,
                target_rate: 96_000,
                target_bits: 24,
                active_output_mode: Some("Pcm".to_string()),
                byte_len: Some(byte_len),
                dop_lead_in_data_len: 0,
                target: target.clone(),
                playback_config: upnp_test_playback_config(),
            },
            &target,
        );
        let token = asset
            .stream_url
            .split("token=")
            .nth(1)
            .expect("token")
            .to_string();

        for (range, expected_len) in [("bytes=44-163", 120_usize), ("bytes=164-283", 120_usize)] {
            let mut query = HashMap::new();
            query.insert("token".to_string(), token.clone());
            let mut headers = HeaderMap::new();
            headers.insert(header::RANGE, HeaderValue::from_static(range));

            let response = upnp_stream(
                State(state.clone()),
                Path(asset.id.clone()),
                Query(query),
                headers,
            )
            .await
            .into_response();

            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
                Some(expected_len.to_string())
            );
            assert!(response.headers().get(header::CONTENT_RANGE).is_some());
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            assert_eq!(body.len(), expected_len);
        }

        let _ = tokio::fs::remove_file(path).await;
    }

    #[test]
    fn generated_stream_unsatisfiable_range_reports_total_length() {
        let response = generated_stream_range_not_satisfiable(Some(1234));
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes */1234")
        );
    }

    #[test]
    fn upnp_qobuz_path_head_validates_token_and_returns_metadata() {
        let state = app_state("upnp-qobuz-path-head");
        let token = state.upnp().register_remote_stream(
            "qobuz-423765381-6",
            None,
            "audio/flac".to_string(),
            Some(12_297_568),
            Some(6),
        );

        let response = upnp_qobuz_stream_head_response(state, "qobuz-423765381-6", &token);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("audio/flac")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("12297568")
        );
    }

    fn temp_file(prefix: &str) -> PathBuf {
        let mut token = [0_u8; 8];
        OsRng.fill_bytes(&mut token);
        std::env::temp_dir().join(format!("{prefix}-{:x}.bin", u64::from_le_bytes(token)))
    }
}
