use crate::diagnostics::logging::{error_kind, sanitize_error};
use futures_util::{StreamExt, stream};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use serde_json::Value;
use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use stream_download::http::HttpStream;
use stream_download::source::SourceStream;
use stream_download::storage::temp::TempStorageProvider;
use stream_download::{Settings, StreamDownload};
use symphonia::core::io::MediaSource;
use tracing::{debug, info, warn};

use super::auth::{auth_headers, build_url, sign_get_file_url, timestamp};
use super::{
    QobuzPlayRequest, QobuzProxyResponse, QobuzResolvedStream, QobuzService, QobuzTrack,
    qobuz_reqwest_error,
};

const QOBUZ_STARTUP_PROBE_BYTES: usize = 64 * 1024;

/// How long a resolved stream URL may be reused by the byte-range proxy
/// before we re-resolve it. Signed Qobuz URLs stay valid well beyond this;
/// an expired entry is also healed by the 401/403 retry path.
const PROXY_STREAM_URL_TTL: Duration = Duration::from_secs(5 * 60);

/// Validate the signed playback URL returned by Qobuz before it can cross a
/// server-side network boundary. Keep this allowlist deliberately narrower
/// than the domains used by Qobuz's API and artwork services.
pub(super) fn validate_qobuz_stream_url(raw_url: &str) -> Result<reqwest::Url, String> {
    let parsed =
        reqwest::Url::parse(raw_url).map_err(|_| "Qobuz stream URL is invalid".to_string())?;
    if parsed.scheme() != "https" {
        return Err("Qobuz stream URL must use https".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Qobuz stream URL must not include credentials".to_string());
    }
    if parsed.port().is_some_and(|port| port != 443) {
        return Err("Qobuz stream URL must use the default https port".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "Qobuz stream URL is missing a host".to_string())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !trusted_qobuz_stream_host(&host) {
        warn!(
            event = "stream_url_host_rejected",
            service = "qobuz",
            qobuz_stream_host = %host,
            "Qobuz stream URL host is not trusted"
        );
        return Err("Qobuz stream URL host is not trusted".to_string());
    }
    Ok(parsed)
}

fn trusted_qobuz_stream_host(host: &str) -> bool {
    matches!(
        host,
        "streaming.qobuz.com"
            | "streaming2.qobuz.com"
            | "streaming-qobuz-sec.akamaized.net"
            | "streaming-qobuz-std.akamaized.net"
    )
}

/// Progressive-stream reader handed to the audio worker. Wraps `stream-download`'s
/// `StreamDownload<TempStorageProvider>` so Symphonia can begin decoding as soon as
/// the prefetch threshold is reached, while the rest of the FLAC continues
/// downloading in the background.
pub struct QobuzStreamSource {
    inner: StreamDownload<TempStorageProvider>,
    byte_len: Option<u64>,
}

impl Read for QobuzStreamSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for QobuzStreamSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for QobuzStreamSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

pub struct QobuzStreamHandle {
    pub source: QobuzStreamSource,
    pub ext: String,
    pub display_name: String,
}

#[derive(Clone)]
pub(super) struct StreamUrl {
    pub(super) url: String,
    pub(super) requested_format_id: u32,
    pub(super) format_id: u32,
    pub(super) mime_type: String,
    /// Sample rate Qobuz reports for the served file (kHz, e.g. 44.1 / 96 / 192).
    /// Used purely for diagnostic logging; the decoder always reads the real
    /// rate out of the FLAC header.
    pub(super) sampling_rate: Option<f64>,
    pub(super) bit_depth: Option<u32>,
}

impl fmt::Debug for StreamUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamUrl")
            .field("url", &"[redacted]")
            .field("requested_format_id", &self.requested_format_id)
            .field("format_id", &self.format_id)
            .field("mime_type", &self.mime_type)
            .field("sampling_rate", &self.sampling_rate)
            .field("bit_depth", &self.bit_depth)
            .finish()
    }
}

impl QobuzService {
    pub async fn open_stream(&self, req: &QobuzPlayRequest) -> Result<QobuzStreamHandle, String> {
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(|| "Log in to Qobuz before playback".to_string())?;
        let tokens = self.ensure_tokens().await?;
        let secret = self.ensure_secret().await?;
        let cache_key = (req.track_id, req.format_id);
        let (mut stream, from_cache) = self
            .cached_or_resolved_stream_url(
                cache_key,
                &tokens.app_id,
                &session.user_auth_token,
                &secret,
            )
            .await?;

        tokio::fs::create_dir_all(&self.cache_dir)
            .await
            .map_err(|e| format!("create Qobuz cache: {e}"))?;

        // TempStorageProvider deletes its file on Drop, but a crash or kill -9 can
        // leave a stale file behind. Sweep anything older than 30 minutes so disk
        // usage stays bounded.
        self.evict_stale_temp_files();

        let display_name = qobuz_stream_display_name(req);
        let mut open_result = self
            .open_resolved_stream(req.track_id, stream.clone(), display_name.clone())
            .await;

        if let (true, Err(primary_error)) = (from_cache, &open_result) {
            warn!(
                event = "stream_resolution_failure",
                service = "qobuz",
                qobuz_track_id = req.track_id,
                requested_format_id = req.format_id.unwrap_or_default(),
                format_id = stream.format_id,
                cache_hit = true,
                error_kind = error_kind(primary_error),
                error = %sanitize_error(primary_error),
                "Cached Qobuz stream URL failed; refreshing"
            );
            self.proxy_stream_url_cache.write().await.remove(&cache_key);
            stream = self
                .resolve_and_remember_stream_url(
                    cache_key,
                    &tokens.app_id,
                    &session.user_auth_token,
                    &secret,
                )
                .await?;
            open_result = self
                .open_resolved_stream(req.track_id, stream.clone(), display_name.clone())
                .await;
        }

        match open_result {
            Ok((handle, stream)) => {
                self.remember_successful_stream(req.format_id, &stream, false)
                    .await;
                info!(
                    event = "stream_open",
                    service = "qobuz",
                    status = "ok",
                    qobuz_track_id = req.track_id,
                    requested_format_id = req.format_id.unwrap_or_default(),
                    format_id = stream.format_id,
                    cache_hit = from_cache,
                    "Qobuz stream opened"
                );
                Ok(handle)
            }
            Err(primary_error) if should_retry_cd_after_download_error(req, &stream) => {
                warn!(
                    event = "stream_resolution_failure",
                    service = "qobuz",
                    qobuz_track_id = req.track_id,
                    requested_format_id = req.format_id.unwrap_or_default(),
                    format_id = stream.format_id,
                    fallback_format_id = 6_u32,
                    error_kind = error_kind(&primary_error),
                    error = %sanitize_error(&primary_error),
                    "Qobuz stream failed; retrying CD quality"
                );
                let cd_stream = self
                    .stream_url_for_quality(
                        req.track_id,
                        6,
                        &tokens.app_id,
                        &session.user_auth_token,
                        &secret,
                    )
                    .await?;
                if !stream_matches_requested_format(6, &cd_stream) {
                    return Err(format!(
                        "Qobuz stream failed ({primary_error}); CD fallback did not return a playable stream"
                    ));
                }
                match self
                    .open_resolved_stream(req.track_id, cd_stream, display_name)
                    .await
                {
                    Ok((handle, stream)) => {
                        self.remember_successful_stream(req.format_id, &stream, true)
                            .await;
                        info!(
                            event = "stream_open",
                            service = "qobuz",
                            status = "ok",
                            qobuz_track_id = req.track_id,
                            requested_format_id = req.format_id.unwrap_or_default(),
                            format_id = stream.format_id,
                            fallback_format_id = 6_u32,
                            "Qobuz CD fallback stream opened"
                        );
                        Ok(handle)
                    }
                    Err(cd_error) => Err(format!(
                        "Qobuz stream failed ({primary_error}); CD fallback also failed ({cd_error})"
                    )),
                }
            }
            Err(primary_error) => Err(primary_error),
        }
    }

    pub async fn warm_album_stream_link_cache(
        &self,
        album_id: &str,
        source_track_id: Option<u64>,
        max_tracks: usize,
    ) -> Result<(), String> {
        self.warm_album_detail_cache(album_id).await?;
        let detail = self.album_detail(album_id).await?;
        let mut tracks = Vec::new();
        if let Some(track_id) = source_track_id {
            tracks.push((track_id, None));
            tracks.push((track_id, Some(6)));
        }
        tracks.extend(
            detail
                .tracks
                .iter()
                .filter(|track| track.streamable)
                .take(max_tracks)
                .flat_map(|track| {
                    [
                        (track.id, cached_qobuz_track_format_id(track)),
                        (track.id, Some(6)),
                    ]
                }),
        );
        tracks.sort_unstable();
        tracks.dedup();
        for (track_id, requested_format_id) in tracks {
            if let Err(err) = self
                .warm_track_stream_link_cache(track_id, requested_format_id)
                .await
            {
                warn!(
                    event = "stream_resolution_failure",
                    service = "qobuz",
                    qobuz_track_id = track_id,
                    requested_format_id = requested_format_id.unwrap_or_default(),
                    error_kind = error_kind(&err),
                    error = %sanitize_error(&err),
                    "Qobuz stream link cache warm failed"
                );
            }
        }
        Ok(())
    }

    pub async fn warm_track_stream_link_cache(
        &self,
        track_id: u64,
        requested_format_id: Option<u32>,
    ) -> Result<(), String> {
        let cache_key = (track_id, requested_format_id);
        if self.cached_proxy_stream_url(&cache_key).await.is_some() {
            return Ok(());
        }
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(|| "Log in to Qobuz before streaming".to_string())?;
        let tokens = self.ensure_tokens().await?;
        let secret = self.ensure_secret().await?;
        self.resolve_and_remember_stream_url(
            cache_key,
            &tokens.app_id,
            &session.user_auth_token,
            &secret,
        )
        .await
        .map(|_| ())
    }

    async fn open_resolved_stream(
        &self,
        track_id: u64,
        stream: StreamUrl,
        display_name: String,
    ) -> Result<(QobuzStreamHandle, StreamUrl), String> {
        let url = validate_qobuz_stream_url(&stream.url)?;

        let prefetch = prefetch_for_format(stream.format_id);
        debug!(
            event = "stream_resolved",
            service = "qobuz",
            qobuz_track_id = track_id,
            requested_format_id = stream.requested_format_id,
            format_id = stream.format_id,
            sampling_rate = stream.sampling_rate.unwrap_or_default(),
            bit_depth = stream.bit_depth.unwrap_or_default(),
            prefetch_kb = prefetch / 1024,
            "Qobuz stream URL resolved"
        );

        let probe_len = self.verify_resolved_stream_download(&stream).await?;

        // stream-download bundles its own reqwest (different major version from ours),
        // so the Client type used here is its re-export, not our `reqwest::Client`.
        let progressive_url = stream_download::http::reqwest::Url::parse(url.as_str())
            .map_err(|_| "Qobuz progressive stream URL is invalid".to_string())?;
        let http_stream = HttpStream::<stream_download::http::reqwest::Client>::new(
            self.progressive_stream_http.clone(),
            progressive_url,
        )
        .await
        .map_err(|e| {
            format!(
                "open Qobuz HTTP stream: {}",
                crate::diagnostics::logging::sanitize_error(&e.to_string())
            )
        })?;
        let byte_len = http_stream.content_length();
        debug!(
            event = "stream_probe",
            service = "qobuz",
            qobuz_track_id = track_id,
            format_id = stream.format_id,
            byte_len = byte_len.or(probe_len).unwrap_or_default(),
            "Qobuz stream probe succeeded"
        );

        let storage = TempStorageProvider::new_in(&self.cache_dir);
        let settings = Settings::default().prefetch_bytes(prefetch as u64);

        let reader = StreamDownload::from_stream(http_stream, storage, settings)
            .await
            .map_err(|e| {
                format!(
                    "init Qobuz progressive stream: {}",
                    crate::diagnostics::logging::sanitize_error(&e.to_string())
                )
            })?;

        let ext = extension_for_stream(&stream.mime_type, stream.format_id).to_string();
        let source = QobuzStreamSource {
            inner: reader,
            byte_len,
        };

        Ok((
            QobuzStreamHandle {
                source,
                ext,
                display_name,
            },
            stream,
        ))
    }

    async fn verify_resolved_stream_download(
        &self,
        stream: &StreamUrl,
    ) -> Result<Option<u64>, String> {
        let end = QOBUZ_STARTUP_PROBE_BYTES.saturating_sub(1);
        let url = validate_qobuz_stream_url(&stream.url)?;
        let response = self
            .stream_http
            .get(url)
            .header(RANGE, format!("bytes=0-{end}"))
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz stream startup probe failed", e))?;
        let status = response.status();
        if !(status.is_success() || status == StatusCode::PARTIAL_CONTENT) {
            return Err(format!("Qobuz stream startup probe returned HTTP {status}"));
        }
        let byte_len = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(content_range_total)
            .or_else(|| response.content_length());
        let bytes = response
            .bytes()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz stream startup probe read failed", e))?;
        if bytes.is_empty() {
            return Err("Qobuz stream startup probe returned no audio bytes".to_string());
        }
        Ok(byte_len)
    }

    async fn remember_successful_stream(
        &self,
        requested_format_id: Option<u32>,
        stream: &StreamUrl,
        used_download_fallback: bool,
    ) {
        if requested_format_id.is_some() || used_download_fallback {
            return;
        }
        if stream.format_id == stream.requested_format_id {
            *self.preferred_format_id.write().await = Some(stream.format_id);
        } else {
            debug!(
                event = "stream_quality_mismatch",
                service = "qobuz",
                requested_format_id = stream.requested_format_id,
                format_id = stream.format_id,
                "Qobuz served a different format; preferred quality not cached"
            );
        }
    }

    #[allow(dead_code)]
    pub async fn proxy_bytes(
        &self,
        track_id: u64,
        range_header: Option<&str>,
    ) -> Result<QobuzProxyResponse, String> {
        self.proxy_bytes_with_format(track_id, range_header, None)
            .await
    }

    pub async fn sonos_proxy_bytes(
        &self,
        track_id: u64,
        range_header: Option<&str>,
    ) -> Result<QobuzProxyResponse, String> {
        self.proxy_bytes_with_format(track_id, range_header, Some(6))
            .await
    }

    pub async fn sonos_cd_quality_stream(
        &self,
        track_id: u64,
    ) -> Result<QobuzResolvedStream, String> {
        self.resolved_stream_for_format(track_id, Some(6))
            .await
            .map_err(|err| {
                if err == "No playable Qobuz stream URL was returned" {
                    "Qobuz did not return a Sonos-compatible CD-quality stream".to_string()
                } else {
                    err
                }
            })
    }

    pub async fn resolved_stream_for_format(
        &self,
        track_id: u64,
        requested_format_id: Option<u32>,
    ) -> Result<QobuzResolvedStream, String> {
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(|| "Log in to Qobuz before streaming".to_string())?;
        let tokens = self.ensure_tokens().await?;
        let secret = self.ensure_secret().await?;
        let cache_key = (track_id, requested_format_id);
        let mut last_error = None;
        for attempt in 0..2 {
            let stream = if attempt == 0 {
                match self.cached_proxy_stream_url(&cache_key).await {
                    Some(stream) => stream,
                    None => {
                        self.stream_url(
                            track_id,
                            &tokens.app_id,
                            &session.user_auth_token,
                            &secret,
                            requested_format_id,
                        )
                        .await?
                    }
                }
            } else {
                self.stream_url(
                    track_id,
                    &tokens.app_id,
                    &session.user_auth_token,
                    &secret,
                    requested_format_id,
                )
                .await?
            };
            if requested_format_id
                .is_some_and(|format_id| !stream_matches_requested_format(format_id, &stream))
            {
                return Err("No playable Qobuz stream URL was returned".to_string());
            }
            match self.verify_resolved_stream_download(&stream).await {
                Ok(byte_len) => {
                    self.remember_proxy_stream_url(cache_key, stream.clone())
                        .await?;
                    if Some(stream.format_id) != requested_format_id {
                        self.remember_proxy_stream_url(
                            (track_id, Some(stream.format_id)),
                            stream.clone(),
                        )
                        .await?;
                    }
                    return Ok(QobuzResolvedStream {
                        mime_type: if stream.mime_type.is_empty() {
                            "audio/flac".to_string()
                        } else {
                            stream.mime_type
                        },
                        sample_rate_hz: qobuz_sampling_rate_hz(stream.sampling_rate),
                        bit_depth: stream.bit_depth.unwrap_or(16),
                        format_id: stream.format_id,
                        byte_len,
                    });
                }
                Err(err) => {
                    warn!(
                        event = "stream_resolution_failure",
                        service = "qobuz",
                        qobuz_track_id = track_id,
                        requested_format_id = requested_format_id.unwrap_or_default(),
                        format_id = stream.format_id,
                        error_kind = error_kind(&err),
                        error = %sanitize_error(&err),
                        "Qobuz resolved stream validation failed"
                    );
                    self.proxy_stream_url_cache.write().await.remove(&cache_key);
                    last_error = Some(err);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| "Qobuz stream validation failed".to_string()))
    }

    pub async fn proxy_bytes_with_format(
        &self,
        track_id: u64,
        range_header: Option<&str>,
        requested_format_id: Option<u32>,
    ) -> Result<QobuzProxyResponse, String> {
        let cache_key = (track_id, requested_format_id);
        let mut last_error: Option<String> = None;
        'attempts: for attempt in 0..2 {
            let cached = if attempt == 0 {
                self.cached_proxy_stream_url(&cache_key).await
            } else {
                None
            };
            let stream = match cached {
                Some(stream) => {
                    debug!(
                        event = "stream_cache",
                        service = "qobuz",
                        qobuz_track_id = track_id,
                        requested_format_id = requested_format_id.unwrap_or_default(),
                        cache_hit = true,
                        "Qobuz proxy stream URL cache hit"
                    );
                    stream
                }
                None => {
                    let session = self
                        .session
                        .read()
                        .await
                        .clone()
                        .ok_or_else(|| "Log in to Qobuz before streaming".to_string())?;
                    let tokens = self.ensure_tokens().await?;
                    let secret = self.ensure_secret().await?;
                    let stream = self
                        .stream_url(
                            track_id,
                            &tokens.app_id,
                            &session.user_auth_token,
                            &secret,
                            requested_format_id,
                        )
                        .await?;
                    debug!(
                        event = "stream_cache",
                        service = "qobuz",
                        qobuz_track_id = track_id,
                        requested_format_id = requested_format_id.unwrap_or_default(),
                        cache_hit = false,
                        "Qobuz proxy stream URL cache miss"
                    );
                    self.remember_proxy_stream_url(cache_key, stream.clone())
                        .await?;
                    stream
                }
            };

            let downstream_range = range_header;
            let url = match validate_qobuz_stream_url(&stream.url) {
                Ok(url) => url,
                Err(error) => {
                    self.proxy_stream_url_cache.write().await.remove(&cache_key);
                    return Err(error);
                }
            };
            let mut req = self.stream_http.get(url);
            req = req.header(RANGE, downstream_range.unwrap_or("bytes=0-"));
            let response = req
                .send()
                .await
                .map_err(|e| qobuz_reqwest_error("Qobuz proxy request failed", e))?;

            if !response.status().is_success() && attempt == 0 {
                self.proxy_stream_url_cache.write().await.remove(&cache_key);
                last_error = Some(format!("Qobuz stream URL expired: {}", response.status()));
                warn!(
                    event = "stream_resolution_failure",
                    service = "qobuz",
                    qobuz_track_id = track_id,
                    requested_format_id = requested_format_id.unwrap_or_default(),
                    status = %response.status(),
                    error_kind = "forbidden",
                    "Qobuz proxy stream URL expired"
                );
                continue;
            }

            let status = response.status();
            if !status.is_success() {
                last_error = Some(format!("Qobuz proxy returned HTTP {status}"));
                warn!(
                    event = "stream_resolution_failure",
                    service = "qobuz",
                    qobuz_track_id = track_id,
                    requested_format_id = requested_format_id.unwrap_or_default(),
                    status = %status,
                    error_kind = "http",
                    "Qobuz proxy returned non-success status"
                );
                continue;
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let content_length = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let content_range = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if downstream_range.is_some()
                && (status != StatusCode::PARTIAL_CONTENT || content_range.is_none())
            {
                let error = format!(
                    "Qobuz proxy range request returned {status} without usable Content-Range"
                );
                warn!(
                    event = "qobuz_proxy_range_mismatch",
                    service = "qobuz",
                    qobuz_track_id = track_id,
                    requested_format_id = requested_format_id.unwrap_or_default(),
                    format_id = stream.format_id,
                    downstream_range = downstream_range.unwrap_or(""),
                    status = %status,
                    has_content_range = content_range.is_some(),
                    "Qobuz upstream did not honor downstream range request"
                );
                if attempt == 0 {
                    self.proxy_stream_url_cache.write().await.remove(&cache_key);
                    last_error = Some(error);
                    continue 'attempts;
                }
                return Err(error);
            }
            let total_len = content_range.as_deref().and_then(content_range_total);
            let mut body_stream = response
                .bytes_stream()
                .map(|result| result.map_err(reqwest::Error::without_url))
                .boxed();
            let first_chunk = loop {
                match body_stream.next().await {
                    Some(Ok(bytes)) if bytes.is_empty() => continue,
                    Some(Ok(bytes)) => break bytes,
                    Some(Err(err)) if attempt == 0 => {
                        self.proxy_stream_url_cache.write().await.remove(&cache_key);
                        last_error = Some(format!("read first Qobuz proxy body chunk: {err}"));
                        continue 'attempts;
                    }
                    Some(Err(err)) => {
                        return Err(format!("read first Qobuz proxy body chunk: {err}"));
                    }
                    None if attempt == 0 => {
                        self.proxy_stream_url_cache.write().await.remove(&cache_key);
                        last_error = Some("Qobuz proxy returned no audio bytes".to_string());
                        continue 'attempts;
                    }
                    None => return Err("Qobuz proxy returned no audio bytes".to_string()),
                }
            };
            let first_chunk_len = first_chunk.len();
            let body = stream::once(async move { Ok::<_, reqwest::Error>(first_chunk) })
                .chain(body_stream)
                .boxed();
            info!(
                event = "qobuz_proxy_first_byte",
                service = "qobuz",
                qobuz_track_id = track_id,
                requested_format_id = requested_format_id.unwrap_or_default(),
                format_id = stream.format_id,
                range_requested = downstream_range.is_some(),
                status = %status,
                first_chunk_bytes = first_chunk_len,
                "Qobuz proxy produced first audio bytes"
            );

            let (status, content_length, content_range) =
                if downstream_range.is_none() && status == StatusCode::PARTIAL_CONTENT {
                    (
                        StatusCode::OK,
                        total_len.map(|len| len.to_string()).or(content_length),
                        None,
                    )
                } else {
                    (status, content_length, content_range)
                };

            return Ok(QobuzProxyResponse {
                status,
                content_type,
                content_length,
                content_range,
                sampling_rate_hz: stream
                    .sampling_rate
                    .map(|khz| (khz * 1000.0).round() as u32),
                bit_depth: stream.bit_depth,
                body,
            });
        }
        Err(last_error.unwrap_or_else(|| "Qobuz proxy failed".to_string()))
    }

    async fn cached_proxy_stream_url(&self, key: &(u64, Option<u32>)) -> Option<StreamUrl> {
        let cached = self.proxy_stream_url_cache.read().await.get(key).cloned()?;
        let (resolved_at, stream) = cached;
        if resolved_at.elapsed() < PROXY_STREAM_URL_TTL
            && validate_qobuz_stream_url(&stream.url).is_ok()
        {
            return Some(stream);
        }
        self.proxy_stream_url_cache.write().await.remove(key);
        None
    }

    async fn remember_proxy_stream_url(
        &self,
        key: (u64, Option<u32>),
        stream: StreamUrl,
    ) -> Result<(), String> {
        validate_qobuz_stream_url(&stream.url)?;
        let mut cache = self.proxy_stream_url_cache.write().await;
        cache.retain(|_, (resolved_at, _)| resolved_at.elapsed() < PROXY_STREAM_URL_TTL);
        cache.insert(key, (Instant::now(), stream));
        Ok(())
    }

    async fn cached_or_resolved_stream_url(
        &self,
        key: (u64, Option<u32>),
        app_id: &str,
        auth_token: &str,
        secret: &str,
    ) -> Result<(StreamUrl, bool), String> {
        if let Some(stream) = self.cached_proxy_stream_url(&key).await {
            return Ok((stream, true));
        }
        self.resolve_and_remember_stream_url(key, app_id, auth_token, secret)
            .await
            .map(|stream| (stream, false))
    }

    async fn resolve_and_remember_stream_url(
        &self,
        key: (u64, Option<u32>),
        app_id: &str,
        auth_token: &str,
        secret: &str,
    ) -> Result<StreamUrl, String> {
        let stream = self
            .stream_url(key.0, app_id, auth_token, secret, key.1)
            .await?;
        self.remember_proxy_stream_url(key, stream.clone()).await?;
        Ok(stream)
    }

    fn evict_stale_temp_files(&self) {
        let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(30 * 60)) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return;
        };
        for entry in entries.flatten() {
            if is_persistent_qobuz_cache_file(&entry.file_name()) {
                continue;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
                && modified < cutoff
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    async fn stream_url(
        &self,
        track_id: u64,
        app_id: &str,
        auth_token: &str,
        secret: &str,
        requested_format_id: Option<u32>,
    ) -> Result<StreamUrl, String> {
        // Build the try-order: an explicit version quality first, otherwise
        // cached preferred quality first (if we have one), followed by the
        // full fallback list. We dedupe so the preferred isn't tried twice.
        const DEFAULT_QUALITIES: [u32; 4] = [27, 7, 6, 5];
        let preferred = *self.preferred_format_id.read().await;
        let mut try_order: Vec<u32> = Vec::with_capacity(5);
        if let Some(requested) = requested_format_id {
            let fallback: &[u32] = match requested {
                27 => &[27, 7, 6, 5],
                7 => &[7, 6, 5],
                6 => &[6],
                5 => &[5],
                _ => &[requested, 27, 7, 6, 5],
            };
            for q in fallback {
                if !try_order.contains(q) {
                    try_order.push(*q);
                }
            }
        } else {
            if let Some(p) = preferred {
                try_order.push(p);
            }
            for q in DEFAULT_QUALITIES {
                if Some(q) != preferred {
                    try_order.push(q);
                }
            }
        }

        let last_idx = try_order.len() - 1;
        for (idx, format_id) in try_order.into_iter().enumerate() {
            match self
                .stream_url_for_quality(track_id, format_id, app_id, auth_token, secret)
                .await
            {
                Ok(stream) => {
                    if let Some(requested) = requested_format_id
                        && !stream_matches_requested_format(requested, &stream)
                    {
                        debug!(
                            event = "stream_quality_mismatch",
                            service = "qobuz",
                            qobuz_track_id = track_id,
                            requested_format_id = requested,
                            format_id = stream.format_id,
                            sampling_rate = stream.sampling_rate.unwrap_or_default(),
                            bit_depth = stream.bit_depth.unwrap_or_default(),
                            "Qobuz served a different format; trying next quality"
                        );
                        continue;
                    }
                    return Ok(stream);
                }
                Err(_) if idx != last_idx => continue,
                Err(e) => return Err(e),
            }
        }
        Err("No playable Qobuz stream URL was returned".to_string())
    }

    async fn stream_url_for_quality(
        &self,
        track_id: u64,
        format_id: u32,
        app_id: &str,
        auth_token: &str,
        secret: &str,
    ) -> Result<StreamUrl, String> {
        let timestamp = timestamp();
        let signature = sign_get_file_url(track_id, format_id, timestamp, secret);
        let response: Value = self
            .http
            .get(build_url("/track/getFileUrl"))
            .headers(auth_headers(app_id, auth_token)?)
            .query(&[
                ("track_id", track_id.to_string()),
                ("format_id", format_id.to_string()),
                ("intent", "stream".to_string()),
                ("request_ts", timestamp.to_string()),
                ("request_sig", signature),
            ])
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz stream URL request failed", e))?
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz stream URL response was not JSON", e))?;

        let url = response
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| "Qobuz stream URL response did not include a URL".to_string())?;
        let validated_url = validate_qobuz_stream_url(url)?;

        Ok(StreamUrl {
            url: validated_url.to_string(),
            requested_format_id: format_id,
            format_id: response
                .get("format_id")
                .and_then(Value::as_u64)
                .unwrap_or(format_id as u64) as u32,
            mime_type: response
                .get("mime_type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            sampling_rate: response.get("sampling_rate").and_then(Value::as_f64),
            bit_depth: response
                .get("bit_depth")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
        })
    }
}

fn content_range_total(value: &str) -> Option<u64> {
    let (_, total) = value.trim().split_once('/')?;
    if total == "*" {
        return None;
    }
    total.parse().ok()
}

fn extension_for_stream(mime_type: &str, format_id: u32) -> &'static str {
    if mime_type.contains("mpeg") || format_id == 5 {
        "mp3"
    } else if mime_type.contains("mp4") || mime_type.contains("aac") {
        "m4a"
    } else {
        "flac"
    }
}

fn cached_qobuz_track_format_id(track: &QobuzTrack) -> Option<u32> {
    let rate = track.maximum_sampling_rate.unwrap_or(44.1);
    let depth = track.maximum_bit_depth.unwrap_or(16);
    if depth <= 16 && rate <= 44.1 {
        Some(6)
    } else if rate > 96.0 {
        Some(27)
    } else {
        Some(7)
    }
}

fn is_persistent_qobuz_cache_file(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("home.json" | "album-details.json" | "artist-top-tracks.json")
    )
}

fn qobuz_sampling_rate_hz(sampling_rate: Option<f64>) -> u32 {
    sampling_rate
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .map(|rate| (rate * 1000.0).round() as u32)
        .unwrap_or(44_100)
}

/// Pre-buffer size for the progressive stream, scaled by Qobuz format tier so
/// that every quality has roughly the same time cushion (~5-7 seconds of audio)
/// before playback starts. Hi-res streams have much higher bitrate, so they
/// need correspondingly more bytes buffered to survive network jitter.
fn prefetch_for_format(format_id: u32) -> usize {
    match format_id {
        5 => 256 * 1024,    // MP3 320 kbps  ~= 40 KB/s -> ~6 s
        6 => 512 * 1024,    // CD 16/44.1   ~= 70 KB/s -> ~7 s
        7 => 1_536 * 1024,  // HiRes 24/96  ~= 220 KB/s -> ~7 s
        27 => 3_072 * 1024, // HiRes 24/192 ~= 460 KB/s -> ~7 s
        _ => 512 * 1024,    // Unknown tier: conservative default
    }
}

pub(super) fn stream_matches_requested_format(
    requested_format_id: u32,
    stream: &StreamUrl,
) -> bool {
    match requested_format_id {
        6 => stream.format_id == 6,
        7 => stream.format_id == 7 || stream.format_id == 6,
        27 => true,
        requested => stream.format_id == requested,
    }
}

pub(super) fn qobuz_stream_display_name(req: &QobuzPlayRequest) -> String {
    match (req.artist.as_deref(), req.title.as_deref()) {
        (Some(artist), Some(title)) => format!("{artist} - {title}"),
        (_, Some(title)) => title.to_string(),
        _ => format!("qobuz:{}", req.track_id),
    }
}

pub(super) fn should_retry_cd_after_download_error(
    req: &QobuzPlayRequest,
    stream: &StreamUrl,
) -> bool {
    !matches!(req.format_id, Some(5 | 6)) && !matches!(stream.format_id, 5 | 6)
}

// Kept as a small guard helper for future cache-path validation around proxy cleanup.
#[allow(dead_code)]
fn cache_path_is_inside(cache_dir: &Path, path: &Path) -> bool {
    path.starts_with(cache_dir)
}
