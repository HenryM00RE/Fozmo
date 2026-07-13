use crate::api::routes::auth_token_from_headers;
use crate::app::config::AppConfig;
use crate::app::state::AppState;
use crate::services::discovery;
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use reqwest::Url;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Which listener a request arrived on. Attached to the remote router as an
/// `Extension` so shared handlers can branch safely; requests without the
/// extension are local/LAN.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RequestSurface {
    Local,
    Remote,
}

/// Marker inserted into request extensions after successful remote auth.
/// Downstream handlers must use this marker — never `RequestSurface::Remote`
/// alone — as proof that remote auth ran.
#[derive(Debug, Clone, Copy)]
pub struct RemoteAuthenticated;

/// Validated request-scoped listening identity. This is deliberately separate
/// from the persisted default profile, which is only a UI preference.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProfileContext {
    pub id: String,
}

pub async fn resolve_profile_context(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let profile_id = match req.headers().get(crate::app::identity::PROFILE_HEADER) {
        Some(value) => match value.to_str() {
            Ok(value) if !value.trim().is_empty() && value.trim() == value => value.to_string(),
            _ => {
                return (StatusCode::BAD_REQUEST, "Invalid listening profile header")
                    .into_response();
            }
        },
        None => state.settings().active_profile_id(),
    };
    if !state
        .settings()
        .profiles()
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        return (StatusCode::NOT_FOUND, "Listening profile not found").into_response();
    }
    req.extensions_mut()
        .insert(ProfileContext { id: profile_id });
    next.run(req).await
}

/// Browser origins trusted by both CORS and WebSocket upgrade handlers on the
/// local/LAN listener. Keeping one set prevents the two browser boundaries
/// from drifting apart as public or discovery addresses change.
#[derive(Clone)]
pub(crate) struct TrustedWebOrigins(Arc<HashSet<String>>);

impl TrustedWebOrigins {
    pub(crate) fn allows(&self, origin: &str) -> bool {
        canonical_http_origin(origin).is_some_and(|origin| self.0.contains(&origin))
    }
}

pub(crate) fn trusted_web_origins(config: &AppConfig) -> TrustedWebOrigins {
    let mut origins = HashSet::from([
        format!("http://127.0.0.1:{}", config.port),
        format!("http://localhost:{}", config.port),
        format!("http://[::1]:{}", config.port),
        discovery::local_browser_base_url(config.port),
    ]);
    if let Some(origin) = origin_from_url(&config.public_base_url) {
        origins.insert(origin);
    }
    for hostname in discovery::trusted_browser_hostnames() {
        origins.insert(format!("http://{hostname}:{}", config.port));
    }
    for address in discovery::active_interface_ips() {
        let host = match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        };
        origins.insert(format!("http://{host}:{}", config.port));
    }
    TrustedWebOrigins(Arc::new(origins))
}

/// Browsers always send exactly one HTTP(S) Origin on a WebSocket handshake.
/// Originless clients are intentionally retained for authenticated native
/// agents and non-browser test clients; their existing authentication still
/// runs independently after this check.
pub(crate) fn websocket_origin_allowed(
    headers: &HeaderMap,
    trusted: Option<&TrustedWebOrigins>,
) -> bool {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let Some(value) = values.next() else {
        return true;
    };
    if values.next().is_some() {
        return false;
    }
    let Ok(raw_origin) = value.to_str() else {
        return false;
    };
    let Some(origin) = canonical_http_origin(raw_origin) else {
        return false;
    };
    trusted
        .map(|trusted| trusted.0.contains(&origin))
        .unwrap_or_else(|| origin_matches_host(&origin, headers))
}

fn canonical_http_origin(value: &str) -> Option<String> {
    if value.trim() != value || value.ends_with('/') {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return None;
    }
    origin_from_parsed_url(&url)
}

fn origin_from_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    origin_from_parsed_url(&url)
}

fn origin_from_parsed_url(url: &Url) -> Option<String> {
    let scheme = url.scheme();
    let host = url.host_str()?;
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_ascii_lowercase()
    };
    Some(format!("{scheme}://{host}{port}"))
}

pub async fn require_pairing(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path().to_string();
    if !state_changing_browser_request_allowed(req.method(), req.headers()) {
        warn!(
            event = "csrf_guard",
            status = "forbidden",
            error_kind = "cross_site",
            method = %req.method(),
            path,
            "Cross-site browser mutation rejected"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    if !state.pairing().auth_required() {
        return Ok(next.run(req).await);
    }
    if pairing_exempt(&path) {
        return Ok(next.run(req).await);
    }
    let cookie_token = control_session_token_from_headers(req.headers());
    let header_token = auth_token_from_headers(req.headers());
    let local_request = local_filesystem_request_allowed(
        req.headers(),
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    );
    let query_token = state
        .pairing()
        .query_token_auth_allowed(local_request)
        .then(|| query_param(req.uri().query(), "token"))
        .flatten();
    let auth_source = if cookie_token.is_some() {
        "cookie"
    } else if header_token.is_some() {
        "header"
    } else if query_token.is_some() {
        "query"
    } else {
        "missing"
    };
    let authorized = cookie_token
        .as_deref()
        .is_some_and(|token| state.pairing().verify_control_token(Some(token)))
        || header_token
            .as_deref()
            .is_some_and(|token| header_token_authorized_for_path(&state, &path, token))
        || query_token
            .as_deref()
            .is_some_and(|token| state.pairing().verify_control_token(Some(token)));
    if authorized {
        debug!(
            event = "pairing_auth",
            status = "ok",
            auth_source,
            peer_loopback = local_request,
            path,
            "Pairing auth accepted"
        );
        Ok(next.run(req).await)
    } else {
        warn!(
            event = "pairing_auth",
            status = "error",
            error_kind = "auth",
            auth_source,
            peer_loopback = local_request,
            path,
            "Pairing auth failed"
        );
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Always-on authorization boundary for controls that remain local in
/// trusted-LAN mode: music-folder management, Remote Access configuration,
/// link-code issuance, and remote-session administration.
///
/// Everything else on the LAN listener deliberately follows the trusted-home-
/// network posture. The separate TLS remote listener still uses mandatory
/// remote-session authentication and its explicit route allowlist.
pub async fn require_lan_admin(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let method = req.method();
    let path = req.uri().path();
    if !sensitive_lan_route(method, path) {
        return Ok(next.run(req).await);
    }

    let local_request = local_filesystem_request_allowed(
        req.headers(),
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    );
    let cookie_token = control_session_token_from_headers(req.headers());
    let header_token = auth_token_from_headers(req.headers());
    let authorized = local_request
        || cookie_token
            .as_deref()
            .is_some_and(|token| state.pairing().verify_control_token(Some(token)))
        || header_token
            .as_deref()
            .is_some_and(|token| state.pairing().verify_control_token(Some(token)));

    if authorized {
        debug!(
            event = "lan_admin_auth",
            status = "ok",
            peer_loopback = local_request,
            method = %method,
            path,
            "LAN admin authorization accepted"
        );
        Ok(next.run(req).await)
    } else {
        warn!(
            event = "lan_admin_auth",
            status = "forbidden",
            error_kind = "auth",
            peer_loopback = local_request,
            method = %method,
            path,
            "LAN admin authorization failed"
        );
        Err(StatusCode::FORBIDDEN)
    }
}

fn sensitive_lan_route(method: &Method, path: &str) -> bool {
    path.starts_with("/api/library/folders")
        || (method != Method::GET
            && method != Method::HEAD
            && method != Method::OPTIONS
            && path == "/api/remote/settings")
        || (*method == Method::POST && path == "/api/remote/link-code")
        || (matches!(*method, Method::GET | Method::HEAD) && path == "/api/remote/sessions")
        || (method != Method::GET
            && method != Method::HEAD
            && method != Method::OPTIONS
            && remote_session_revoke_path(path))
}

fn remote_session_revoke_path(path: &str) -> bool {
    path.strip_prefix("/api/remote/sessions/")
        .and_then(|rest| rest.strip_suffix("/revoke"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// Browsers automatically attach ambient network authority, so unsafe
/// requests with a browser origin context must prove they came from the served
/// origin. Originless native and command-line requests remain compatible,
/// including in pairing-disabled LAN mode.
fn state_changing_browser_request_allowed(method: &Method, headers: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }

    let mut fetch_sites = headers.get_all("sec-fetch-site").iter();
    if let Some(value) = fetch_sites.next() {
        if fetch_sites.next().is_some() {
            return false;
        }
        let Ok(value) = value.to_str() else {
            return false;
        };
        if !matches_ignore_ascii_case(value, &["same-origin", "none"]) {
            return false;
        }
    }

    let mut origins = headers.get_all(header::ORIGIN).iter();
    if let Some(value) = origins.next() {
        if origins.next().is_some() {
            return false;
        }
        let Ok(origin) = value.to_str() else {
            return false;
        };
        if canonical_http_origin(origin).is_none() || !origin_matches_host(origin, headers) {
            return false;
        }
    }

    let mut referers = headers.get_all(header::REFERER).iter();
    if let Some(value) = referers.next() {
        if referers.next().is_some() {
            return false;
        }
        let Ok(referer) = value.to_str() else {
            return false;
        };
        if !http_url_matches_host(referer, headers) {
            return false;
        }
    }

    // Originless native clients have no ambient browser authority and remain
    // compatible. Browser requests reach here only after every supplied
    // origin signal passed.
    true
}

fn matches_ignore_ascii_case(value: &str, expected: &[&str]) -> bool {
    expected
        .iter()
        .any(|expected| value.eq_ignore_ascii_case(expected))
}

/// Mandatory auth for the remote listener.
///
/// Accepts only the remote session cookie verified for the `remote` scope.
/// Unlike the LAN middleware it never consults `auth_required()`, never reads
/// header/bearer/query tokens, never accepts the LAN control-session cookie,
/// and has no loopback or local-filesystem bypass. Failures are rate-limited
/// per peer IP; token values are never logged.
pub async fn require_remote_auth(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    mut req: Request,
    next: Next,
) -> Response {
    let peer_ip = peer.as_ref().map(|ConnectInfo(addr)| addr.ip());
    let path = req.uri().path().to_string();
    if let Some(ip) = peer_ip
        && let Some(remaining) = state.remote_auth_limiter().lockout_remaining(ip)
    {
        warn!(
            event = "remote_auth_failure",
            status = "rate_limited",
            error_kind = "rate_limit",
            peer_ip = %ip,
            path,
            "Remote request rejected during lockout"
        );
        return rate_limited_response(remaining);
    }

    let token = remote_session_token_from_headers(req.headers());
    let token_presented = token.is_some();
    let authorized = token
        .as_deref()
        .is_some_and(|token| state.pairing().verify_remote_token(Some(token)));
    if authorized {
        if let Some(ip) = peer_ip {
            state.remote_auth_limiter().record_success(ip);
        }
        req.extensions_mut().insert(RemoteAuthenticated);
        return next.run(req).await;
    }

    // Only presented-but-invalid tokens count towards lockout so a fresh
    // client loading the app shell cannot lock itself out before pairing.
    if token_presented && let Some(ip) = peer_ip {
        state.remote_auth_limiter().record_failure(ip);
    }
    warn!(
        event = "remote_auth_failure",
        status = "error",
        error_kind = "auth",
        auth_source = if token_presented { "cookie" } else { "missing" },
        peer_ip = ?peer_ip,
        path,
        "Remote auth failed"
    );
    StatusCode::UNAUTHORIZED.into_response()
}

/// Guards browser-private zones on every `/api/zones/:zone_id/...` route.
///
/// A browser zone may only be seen or controlled by the browser session that
/// registered it, identified by the zone's agent id in the
/// `x-fozmo-browser-zone` request header. Non-owners get `404` so the zone's
/// existence is not revealed. Routes without a zone id segment (including
/// `/api/zones` itself, which filters in the handler) pass through.
pub async fn enforce_browser_zone_ownership(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(zone_id) = zone_id_path_segment(req.uri().path())
        && let Some(agent_id) = state.zones().browser_zone_agent_id(&zone_id)
        && browser_zone_header(req.headers()).as_deref() != Some(agent_id.as_str())
    {
        warn!(
            event = "browser_zone_ownership",
            status = "error",
            error_kind = "auth",
            path = %req.uri().path(),
            "Browser zone request rejected for non-owner"
        );
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(req).await
}

pub(crate) fn browser_zone_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(crate::app::identity::BROWSER_ZONE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn zone_id_path_segment(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/api/zones/")?;
    let segment = rest.split('/').next().unwrap_or("");
    if segment.is_empty() {
        return None;
    }
    match urlencoding::decode(segment) {
        Ok(decoded) => Some(decoded.into_owned()),
        Err(_) => Some(segment.to_string()),
    }
}

pub(crate) fn rate_limited_response(remaining: Duration) -> Response {
    let retry_after = remaining.as_secs().max(1).to_string();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after)],
    )
        .into_response()
}

pub(crate) fn remote_session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie| cookie_value(cookie, crate::zones::REMOTE_SESSION_COOKIE))
}

/// Security headers for every remote response. HSTS is added only for
/// user-supplied certificates; with a self-signed cert HSTS can trap browsers
/// behind an unproceedable interstitial.
///
/// `img-src` is broader than `'self'` because the SPA renders Qobuz artwork
/// straight from `static.qobuz.com` URLs returned by the API; all active
/// script/connect sources stay same-origin.
pub async fn remote_security_headers(req: Request, next: Next, hsts: bool) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; \
             style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             img-src 'self' https: data: blob:; font-src 'self' data: https://fonts.gstatic.com; connect-src 'self'; \
             media-src 'self' blob:; object-src 'none'; frame-ancestors 'none'; \
             base-uri 'none'; form-action 'self'",
        ),
    );
    if hsts {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=63072000"),
        );
    }
    response
}

fn pairing_exempt(path: &str) -> bool {
    matches!(
        path,
        "/api/pairing/start"
            | "/api/sessions/browser"
            | "/api/agents/token"
            | "/api/pairing/revoke-all"
            | "/api/qobuz/oauth/start"
            | "/api/qobuz/oauth/callback"
            | "/api/ws"
            | "/api/agent/ws"
    )
}

fn header_token_authorized_for_path(state: &AppState, path: &str, token: &str) -> bool {
    if state.pairing().verify_control_token(Some(token)) {
        return true;
    }
    path.starts_with("/api/stream/") && state.pairing().verify_stream_token(Some(token))
}

pub(crate) fn control_session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie| cookie_value(cookie, crate::zones::CONTROL_SESSION_COOKIE))
}

fn cookie_value(cookie: &str, key: &str) -> Option<String> {
    cookie.split(';').find_map(|part| {
        let (raw_key, raw_value) = part.trim().split_once('=')?;
        (raw_key == key && !raw_value.trim().is_empty()).then(|| raw_value.trim().to_string())
    })
}

pub(crate) fn local_filesystem_request_allowed(
    headers: &HeaderMap,
    peer_addr: Option<SocketAddr>,
) -> bool {
    if browser_origin_is_cross_site(headers) {
        return false;
    }
    if let Some(peer_addr) = peer_addr {
        return peer_addr.ip().is_loopback();
    }
    headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .is_some_and(host_is_loopback)
}

fn browser_origin_is_cross_site(headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return true;
    }
    if headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| !origin_matches_host(origin, headers))
    {
        return true;
    }
    headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|referer| !origin_matches_host(referer, headers))
}

fn origin_matches_host(origin_or_referer: &str, headers: &HeaderMap) -> bool {
    let Some(origin_host) = origin_host(origin_or_referer) else {
        return false;
    };
    headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .map(normalized_host_port)
        .is_some_and(|host| host.eq_ignore_ascii_case(&origin_host))
}

fn http_url_matches_host(value: &str, headers: &HeaderMap) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    origin_matches_host(value, headers)
}

fn origin_host(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("://")?;
    let host = rest.split('/').next()?.trim();
    (!host.is_empty()).then(|| normalized_host_port(host))
}

fn normalized_host_port(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn host_is_loopback(host: &str) -> bool {
    let host = host_without_port(host);
    host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("[::1]")
        || host == "::1"
        || host.parse::<IpAddr>().is_ok_and(|addr| addr.is_loopback())
}

pub(crate) fn same_origin_browser_request_allowed(headers: &HeaderMap) -> bool {
    if browser_origin_is_cross_site(headers) {
        return false;
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("same-origin"))
    {
        return true;
    }
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin_matches_host(origin, headers))
        || headers
            .get(header::REFERER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|referer| origin_matches_host(referer, headers))
}

fn host_without_port(host: &str) -> &str {
    let host = host.trim();
    if host.starts_with('[') {
        return host
            .split_once(']')
            .map(|(addr, _)| addr)
            .unwrap_or(host)
            .trim_start_matches('[');
    }
    if host.matches(':').count() == 1 {
        return host.rsplit_once(':').map(|(addr, _)| addr).unwrap_or(host);
    }
    host
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|part| {
        let (raw_key, raw_value) = part.split_once('=')?;
        (decode_query_component(raw_key).as_deref() == Some(key))
            .then(|| decode_query_component(raw_value))
            .flatten()
    })
}

fn decode_query_component(value: &str) -> Option<String> {
    let plus_normalized = value.replace('+', " ");
    match urlencoding::decode(&plus_normalized) {
        Ok(decoded) => Some(decoded.into_owned()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::{app_state, app_state_with_pairing};
    use crate::protocol::AgentCapabilities;
    use axum::http::HeaderValue;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::Extension,
        http::Request,
        middleware,
        routing::{get, post},
    };
    use proptest::prelude::*;
    use tower::ServiceExt;

    proptest! {
        #[test]
        fn property_cookie_parser_only_matches_the_exact_cookie_name(
            prefix in "[A-Za-z0-9_]{0,16}",
            token in "[A-Za-z0-9_-]{1,64}"
        ) {
            let key = crate::zones::CONTROL_SESSION_COOKIE;
            let cookie = format!("{prefix}{key}_shadow={token}; {key}={token}; other=value");
            prop_assert_eq!(cookie_value(&cookie, key), Some(token));
            prop_assert_eq!(cookie_value(&cookie, "missing"), None);
        }

        #[test]
        fn property_trusted_origin_does_not_accept_host_suffixes(label in "[a-z]{1,24}") {
            let trusted = websocket_test_origins();
            let hostile = format!("http://localhost.{label}:3000");
            prop_assert!(!websocket_origin_allowed(
                &websocket_headers("localhost:3000", Some(&hostile)),
                Some(&trusted),
            ));
        }

        #[test]
        fn property_decoded_zone_segment_never_consumes_a_following_path_segment(
            zone in "[A-Za-z0-9%_-]{1,64}",
            tail in "[A-Za-z0-9_-]{1,32}"
        ) {
            let parsed = zone_id_path_segment(&format!("/api/zones/{zone}/{tail}"));
            let expected = urlencoding::decode(&zone)
                .map(|value| value.into_owned())
                .unwrap_or(zone);
            prop_assert_eq!(parsed, Some(expected));
        }
    }

    fn websocket_headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        }
        headers
    }

    fn websocket_test_origins() -> TrustedWebOrigins {
        let mut origins = HashSet::from([
            "http://localhost:3000".to_string(),
            "http://192.168.1.42:3000".to_string(),
            "http://studio.local:3000".to_string(),
        ]);
        for hostname in discovery::trusted_browser_hostnames() {
            origins.insert(format!("http://{hostname}:3000"));
        }
        TrustedWebOrigins(Arc::new(origins))
    }

    #[test]
    fn websocket_origins_allow_trusted_loopback_lan_and_mdns_forms() {
        let trusted = websocket_test_origins();
        for (host, origin) in [
            ("localhost:3000", "http://localhost:3000"),
            ("192.168.1.42:3000", "http://192.168.1.42:3000"),
            ("studio.local:3000", "http://studio.local:3000"),
        ] {
            assert!(
                websocket_origin_allowed(&websocket_headers(host, Some(origin)), Some(&trusted)),
                "{origin} should be trusted"
            );
        }
        for hostname in discovery::trusted_browser_hostnames() {
            let host = format!("{hostname}:3000");
            let origin = format!("http://{host}");
            assert!(
                websocket_origin_allowed(&websocket_headers(&host, Some(&origin)), Some(&trusted)),
                "{origin} should be trusted"
            );
        }
    }

    #[test]
    fn websocket_origins_reject_hostile_malformed_and_duplicated_values() {
        let trusted = websocket_test_origins();
        assert!(!websocket_origin_allowed(
            &websocket_headers("localhost:3000", Some("https://evil.test")),
            Some(&trusted),
        ));
        assert!(!websocket_origin_allowed(
            &websocket_headers("localhost:3000", Some("http://localhost:3000/path")),
            Some(&trusted),
        ));
        if let Some(hostname) = discovery::system_hostname() {
            for origin in [
                format!("http://{hostname}.attacker.test:3000"),
                format!("http://{hostname}:3001"),
                format!("https://{hostname}:3000"),
            ] {
                assert!(!websocket_origin_allowed(
                    &websocket_headers(&format!("{hostname}:3000"), Some(&origin)),
                    Some(&trusted),
                ));
            }
        }

        let mut malformed = websocket_headers("localhost:3000", None);
        malformed.insert(
            header::ORIGIN,
            HeaderValue::from_bytes(&[0xff]).expect("opaque header value"),
        );
        assert!(!websocket_origin_allowed(&malformed, Some(&trusted)));

        let mut duplicated = websocket_headers("localhost:3000", None);
        duplicated.append(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:3000"),
        );
        duplicated.append(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:3000"),
        );
        assert!(!websocket_origin_allowed(&duplicated, Some(&trusted)));
    }

    #[test]
    fn websocket_origin_may_be_missing_for_non_browser_clients() {
        assert!(websocket_origin_allowed(
            &websocket_headers("localhost:3000", None),
            Some(&websocket_test_origins()),
        ));
    }

    #[test]
    fn zone_id_segments_are_extracted_and_decoded() {
        assert_eq!(
            zone_id_path_segment("/api/zones/browser-abc/status").as_deref(),
            Some("browser-abc")
        );
        assert_eq!(
            zone_id_path_segment("/api/zones/a%20b/play").as_deref(),
            Some("a b")
        );
        assert_eq!(zone_id_path_segment("/api/zones"), None);
        assert_eq!(zone_id_path_segment("/api/status"), None);
    }

    #[test]
    fn browser_agent_ws_requires_pairing_on_lan_surface() {
        assert!(!pairing_exempt("/api/agent/browser/ws"));
    }

    #[tokio::test]
    async fn profile_context_is_validated_and_isolated_per_request() {
        let state = app_state("profile-context-isolation");
        let alice = state.settings().create_profile("Alice").unwrap();
        let bob = state.settings().create_profile("Bob").unwrap();
        let app = Router::new()
            .route(
                "/profile",
                get(|Extension(profile): Extension<ProfileContext>| async move { profile.id }),
            )
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                resolve_profile_context,
            ))
            .with_state(state);

        let request = |profile_id: &str| {
            Request::builder()
                .uri("/profile")
                .header(crate::app::identity::PROFILE_HEADER, profile_id)
                .body(Body::empty())
                .unwrap()
        };
        let (alice_response, bob_response) = tokio::join!(
            app.clone().oneshot(request(&alice.id)),
            app.clone().oneshot(request(&bob.id)),
        );
        let alice_body = to_bytes(alice_response.unwrap().into_body(), 128)
            .await
            .unwrap();
        let bob_body = to_bytes(bob_response.unwrap().into_body(), 128)
            .await
            .unwrap();
        assert_eq!(alice_body.as_ref(), alice.id.as_bytes());
        assert_eq!(bob_body.as_ref(), bob.id.as_bytes());

        let missing = app
            .oneshot(request("profile-that-does-not-exist"))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn trusted_lan_admin_policy_restricts_folder_and_remote_credential_controls() {
        assert!(!sensitive_lan_route(&Method::GET, "/api/library/search"));
        assert!(!sensitive_lan_route(&Method::POST, "/api/pause"));
        assert!(!sensitive_lan_route(&Method::POST, "/api/queue"));
        assert!(!sensitive_lan_route(&Method::POST, "/api/upload"));
        assert!(!sensitive_lan_route(&Method::POST, "/api/qobuz/login"));
        assert!(!sensitive_lan_route(
            &Method::POST,
            "/api/zones/living-room/rename"
        ));
        assert!(!sensitive_lan_route(
            &Method::POST,
            "/api/zones/living-room/settings"
        ));
        assert!(!sensitive_lan_route(
            &Method::GET,
            "/api/diagnostics/export"
        ));
        assert!(!sensitive_lan_route(
            &Method::POST,
            "/api/future-admin-write"
        ));
        assert!(!sensitive_lan_route(&Method::GET, "/api/remote/settings"));

        assert!(sensitive_lan_route(&Method::GET, "/api/library/folders"));
        assert!(sensitive_lan_route(&Method::POST, "/api/library/folders"));
        assert!(sensitive_lan_route(
            &Method::POST,
            "/api/library/folders/pick"
        ));
        assert!(sensitive_lan_route(&Method::POST, "/api/remote/settings"));
        assert!(sensitive_lan_route(&Method::POST, "/api/remote/link-code"));
        assert!(sensitive_lan_route(&Method::GET, "/api/remote/sessions"));
        assert!(sensitive_lan_route(
            &Method::POST,
            "/api/remote/sessions/session-id/revoke"
        ));
        assert!(!sensitive_lan_route(
            &Method::GET,
            "/api/remote/sessions/session-id/revoke"
        ));
        assert!(!sensitive_lan_route(
            &Method::POST,
            "/api/remote/sessions/nested/id/revoke"
        ));
    }

    #[tokio::test]
    async fn pairing_disabled_csrf_guard_rejects_cross_site_browser_mutations_only() {
        let state = app_state_with_pairing("pairing-disabled-csrf", false, false);
        let app = Router::new()
            .route("/api/mutate", post(|| async { StatusCode::NO_CONTENT }))
            .route("/api/read", get(|| async { StatusCode::OK }))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_pairing,
            ))
            .with_state(state);

        let request = |method: Method| {
            Request::builder()
                .method(method)
                .uri("/api/mutate")
                .header(header::HOST, "player.local:3000")
                .body(Body::empty())
                .unwrap()
        };

        let native = app.clone().oneshot(request(Method::POST)).await.unwrap();
        assert_eq!(native.status(), StatusCode::NO_CONTENT);

        let same_origin = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/mutate")
                    .header(header::HOST, "player.local:3000")
                    .header(header::ORIGIN, "http://player.local:3000")
                    .header("sec-fetch-site", "same-origin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(same_origin.status(), StatusCode::NO_CONTENT);

        for hostile in [
            Request::builder()
                .method(Method::POST)
                .uri("/api/mutate")
                .header(header::HOST, "player.local:3000")
                .header(header::ORIGIN, "https://evil.test")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/mutate")
                .header(header::HOST, "player.local:3000")
                .header("sec-fetch-site", "same-site")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/mutate")
                .header(header::HOST, "player.local:3000")
                .header(header::ORIGIN, "null")
                .body(Body::empty())
                .unwrap(),
        ] {
            let response = app.clone().oneshot(hostile).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let mut duplicated_origin = request(Method::POST);
        duplicated_origin.headers_mut().append(
            header::ORIGIN,
            HeaderValue::from_static("http://player.local:3000"),
        );
        duplicated_origin.headers_mut().append(
            header::ORIGIN,
            HeaderValue::from_static("http://player.local:3000"),
        );
        assert_eq!(
            app.clone()
                .oneshot(duplicated_origin)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        let cross_site_read = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/read")
                    .header(header::HOST, "player.local:3000")
                    .header(header::ORIGIN, "https://evil.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cross_site_read.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn csrf_guard_precedes_pairing_exemptions_when_pairing_is_enabled() {
        let state = app_state_with_pairing("pairing-exempt-csrf", true, false);
        let app = Router::new()
            .route("/api/agents/token", post(|| async { StatusCode::CREATED }))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_pairing,
            ))
            .with_state(state);

        let cross_site = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/agents/token")
                    .header(header::HOST, "player.local:3000")
                    .header(header::ORIGIN, "https://evil.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cross_site.status(), StatusCode::FORBIDDEN);

        let native = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/agents/token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(native.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn trusted_lan_admin_policy_requires_loopback_or_control_session() {
        let state = app_state_with_pairing("trusted-lan-admin-boundary", false, false);
        assert!(!state.pairing().auth_required());
        let app = Router::new()
            .route(
                "/api/library/folders",
                post(|| async { StatusCode::CREATED }),
            )
            .route("/api/pause", post(|| async { StatusCode::OK }))
            .route("/api/upload", post(|| async { StatusCode::CREATED }))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_lan_admin,
            ))
            .with_state(state.clone());

        let lan_peer = SocketAddr::from(([192, 168, 1, 50], 4444));
        let mut folders = Request::builder()
            .method(Method::POST)
            .uri("/api/library/folders")
            .body(Body::empty())
            .unwrap();
        folders.extensions_mut().insert(ConnectInfo(lan_peer));
        assert_eq!(
            app.clone().oneshot(folders).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let mut upload = Request::builder()
            .method(Method::POST)
            .uri("/api/upload")
            .body(Body::empty())
            .unwrap();
        upload.extensions_mut().insert(ConnectInfo(lan_peer));
        assert_eq!(
            app.clone().oneshot(upload).await.unwrap().status(),
            StatusCode::CREATED
        );

        let mut playback = Request::builder()
            .method(Method::POST)
            .uri("/api/pause")
            .body(Body::empty())
            .unwrap();
        playback.extensions_mut().insert(ConnectInfo(lan_peer));
        assert_eq!(
            app.clone().oneshot(playback).await.unwrap().status(),
            StatusCode::OK
        );

        let control = state.pairing().create_control_session(None).unwrap();
        let mut authenticated_folders = Request::builder()
            .method(Method::POST)
            .uri("/api/library/folders")
            .header(
                header::COOKIE,
                format!("{}={}", crate::zones::CONTROL_SESSION_COOKIE, control.token),
            )
            .body(Body::empty())
            .unwrap();
        authenticated_folders
            .extensions_mut()
            .insert(ConnectInfo(lan_peer));
        assert_eq!(
            app.clone()
                .oneshot(authenticated_folders)
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        let loopback_peer = SocketAddr::from(([127, 0, 0, 1], 4444));
        let mut local_folders = Request::builder()
            .method(Method::POST)
            .uri("/api/library/folders")
            .body(Body::empty())
            .unwrap();
        local_folders
            .extensions_mut()
            .insert(ConnectInfo(loopback_peer));
        assert_eq!(
            app.oneshot(local_folders).await.unwrap().status(),
            StatusCode::CREATED
        );
    }

    #[tokio::test]
    async fn browser_zone_routes_are_hidden_from_non_owners() {
        let state = app_state("browser-zone-ownership");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "browser-3f9c2ab1d0e4".to_string(),
            "Safari on iPhone".to_string(),
            AgentCapabilities {
                output_devices: Vec::new(),
                output_device_capabilities: Vec::new(),
                max_sample_rate: 48_000,
                max_bit_depth: 24,
                exclusive_supported: false,
                supports_dsd128: false,
                supports_dsd256: false,
                browser: true,
            },
            tx,
        );
        let app = Router::new()
            .route("/api/zones/:zone_id/status", get(|| async { "ok" }))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                enforce_browser_zone_ownership,
            ))
            .with_state(state);

        let anonymous = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/zones/browser-3f9c2ab1d0e4/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::NOT_FOUND);

        let wrong_owner = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/zones/browser-3f9c2ab1d0e4/status")
                    .header(
                        crate::app::identity::BROWSER_ZONE_HEADER,
                        "browser-other-agent",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_owner.status(), StatusCode::NOT_FOUND);

        let owner = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/zones/browser-3f9c2ab1d0e4/status")
                    .header(
                        crate::app::identity::BROWSER_ZONE_HEADER,
                        "browser-3f9c2ab1d0e4",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(owner.status(), StatusCode::OK);

        // Non-browser zones stay reachable without the header.
        let local = app
            .oneshot(
                Request::builder()
                    .uri("/api/zones/local-core/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(local.status(), StatusCode::OK);
    }

    fn register_browser_zone(state: &AppState, agent_id: &str) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            agent_id.to_string(),
            "Safari on iPhone".to_string(),
            AgentCapabilities {
                output_devices: Vec::new(),
                output_device_capabilities: Vec::new(),
                max_sample_rate: 48_000,
                max_bit_depth: 24,
                exclusive_supported: false,
                supports_dsd128: false,
                supports_dsd256: false,
                browser: true,
            },
            tx,
        );
    }

    #[tokio::test]
    async fn browser_zone_owner_header_does_not_bypass_pairing() {
        let state = app_state_with_pairing("browser-zone-pairing-required", true, false);
        let agent_id = "browser-3f9c2ab1d0e4";
        register_browser_zone(&state, agent_id);
        let app = Router::new()
            .route("/api/zones", get(|| async { "zones" }))
            .route("/api/zones/:zone_id/status", get(|| async { "ok" }))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_pairing,
            ))
            .with_state(state.clone());

        let owner_status = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/zones/{agent_id}/status"))
                    .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(owner_status.status(), StatusCode::UNAUTHORIZED);

        let owner_list = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/zones")
                    .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(owner_list.status(), StatusCode::UNAUTHORIZED);

        let control = state.pairing().create_control_session(None).unwrap();
        let authenticated_owner = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/zones/{agent_id}/status"))
                    .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id)
                    .header(
                        header::COOKIE,
                        format!("{}={}", crate::zones::CONTROL_SESSION_COOKIE, control.token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authenticated_owner.status(), StatusCode::OK);
    }

    #[test]
    fn query_token_is_url_decoded() {
        assert_eq!(
            query_param(Some("other=1&token=a%2Bb+c"), "token").as_deref(),
            Some("a+b c")
        );
    }

    #[test]
    fn local_filesystem_requests_reject_cross_site_browser_contexts() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:3000"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.test"),
        );

        assert!(!local_filesystem_request_allowed(
            &headers,
            Some(SocketAddr::from(([127, 0, 0, 1], 4444))),
        ));
    }

    #[test]
    fn local_filesystem_requests_allow_same_origin_loopback() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:3000"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:3000"),
        );

        assert!(local_filesystem_request_allowed(
            &headers,
            Some(SocketAddr::from(([127, 0, 0, 1], 4444))),
        ));
    }

    #[test]
    fn local_filesystem_requests_reject_lan_peers() {
        let headers = HeaderMap::new();

        assert!(!local_filesystem_request_allowed(
            &headers,
            Some(SocketAddr::from(([192, 168, 1, 50], 4444))),
        ));
    }
}
