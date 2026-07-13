use base64::Engine;
use md5::{Digest, Md5};
use rand::{RngCore, rngs::OsRng};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::secrets::{SecretKey, SecretValue, SecretsStore};

use super::{QobuzService, QobuzStatus, QobuzUser, qobuz_reqwest_error};

const BASE_URL: &str = "https://www.qobuz.com/api.json/0.2";
const LOGIN_PAGE_URL: &str = "https://play.qobuz.com/login";
const BUNDLE_BASE_URL: &str = "https://play.qobuz.com";
const SESSION_FILE_NAME: &str = "session.json";
const OAUTH_STATE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_OAUTH_STATES: usize = 32;

#[derive(Clone)]
pub(super) struct BundleTokens {
    pub(super) app_id: String,
    pub(super) secrets: Vec<String>,
    pub(super) private_key: Option<String>,
}

impl fmt::Debug for BundleTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BundleTokens")
            .field("app_id", &self.app_id)
            .field("secrets", &"[redacted]")
            .field(
                "private_key",
                &self.private_key.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct UserSession {
    pub(super) user_auth_token: String,
    pub(super) user: QobuzUser,
}

impl fmt::Debug for UserSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserSession")
            .field("user_auth_token", &"[redacted]")
            .field("user", &"[redacted]")
            .finish()
    }
}

pub(super) struct PendingOAuthState {
    value: String,
    issued_at: Instant,
    peer_ip: Option<IpAddr>,
}

pub(super) fn load_session(
    cache_dir: &Path,
    secrets: &dyn SecretsStore,
    account: &str,
) -> Option<UserSession> {
    load_session_with_store(cache_dir, secrets, account)
}

impl QobuzService {
    fn save_session(&self, session: Option<&UserSession>) -> Result<(), String> {
        save_session_with_store(
            &self.cache_dir,
            self.secrets.as_ref(),
            &self.session_account,
            session,
        )
    }

    pub async fn init(&self) -> Result<(), String> {
        self.ensure_tokens().await.map(|_| ())
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<QobuzStatus, String> {
        let tokens = self.ensure_tokens().await?;
        let url = build_url("/user/login");
        let response: Value = self
            .http
            .get(url)
            .headers(app_headers(&tokens.app_id)?)
            .query(&[
                ("email", email.to_string()),
                ("password", password.to_string()),
            ])
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz login request failed", e))?
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz login response was not JSON", e))?;

        if response.get("status").and_then(Value::as_str) == Some("error") {
            let message = response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Qobuz rejected the login");
            warn!(
                event = "qobuz_auth_failure",
                flow = "password",
                error = %crate::diagnostics::logging::sanitize_error(message),
                "Qobuz rejected authentication"
            );
            return Err("Qobuz authentication failed".to_string());
        }

        let session = parse_login_response(&response)?;
        self.save_session(Some(&session))?;
        *self.session.write().await = Some(session.clone());
        // New account = possibly different streaming tier - drop any cached
        // preferred format so the next play re-probes.
        *self.preferred_format_id.write().await = None;
        self.clear_home_cache().await;
        self.clear_album_detail_cache().await;
        Ok(self.status().await)
    }

    pub async fn logout(&self) -> QobuzStatus {
        *self.session.write().await = None;
        if let Err(e) = self.save_session(None) {
            eprintln!("qobuz: failed to clear secure session: {e}");
        }
        *self.preferred_format_id.write().await = None;
        self.clear_home_cache().await;
        self.clear_album_detail_cache().await;
        self.status().await
    }

    pub async fn oauth_url(
        &self,
        redirect_url: &str,
        peer_ip: Option<IpAddr>,
    ) -> Result<(String, String), String> {
        let tokens = self.ensure_tokens().await?;
        let state = generate_oauth_state();
        let url = format!(
            "https://www.qobuz.com/signin/oauth?ext_app_id={}&redirect_url={}&state={}",
            tokens.app_id,
            urlencoding::encode(redirect_url),
            urlencoding::encode(&state),
        );
        let mut pending = self.pending_oauth_states.lock().await;
        pending.retain(|candidate| candidate.issued_at.elapsed() <= OAUTH_STATE_TTL);
        let remove_count = pending
            .len()
            .saturating_sub(MAX_PENDING_OAUTH_STATES.saturating_sub(1));
        pending.drain(..remove_count);
        pending.push(PendingOAuthState {
            value: state.clone(),
            issued_at: Instant::now(),
            peer_ip,
        });
        Ok((url, state))
    }

    pub async fn oauth_state_matches_peer(&self, presented: &str, peer_ip: Option<IpAddr>) -> bool {
        let Some(peer_ip) = peer_ip else {
            return false;
        };
        let mut pending = self.pending_oauth_states.lock().await;
        pending.retain(|candidate| candidate.issued_at.elapsed() <= OAUTH_STATE_TTL);
        pending.iter().any(|candidate| {
            candidate
                .peer_ip
                .is_some_and(|issued_ip| oauth_peer_ips_match(issued_ip, peer_ip))
                && crate::zones::constant_time_token_matches(
                    std::slice::from_ref(&candidate.value),
                    presented,
                )
        })
    }

    pub async fn pending_oauth_state_for_peer(&self, peer_ip: Option<IpAddr>) -> Option<String> {
        let peer_ip = peer_ip?;
        let mut pending = self.pending_oauth_states.lock().await;
        pending.retain(|candidate| candidate.issued_at.elapsed() <= OAUTH_STATE_TTL);
        let mut matches = pending.iter().filter(|candidate| {
            candidate
                .peer_ip
                .is_some_and(|issued_ip| oauth_peer_ips_match(issued_ip, peer_ip))
        });
        let state = matches.next()?.value.clone();
        matches.next().is_none().then_some(state)
    }

    pub async fn login_with_oauth_code(
        &self,
        code: &str,
        oauth_state: &str,
    ) -> Result<QobuzStatus, String> {
        self.consume_oauth_state(oauth_state).await?;
        let tokens = self.ensure_tokens().await?;
        let private_key = tokens
            .private_key
            .clone()
            .ok_or_else(|| "Qobuz OAuth key was not found in the web bundle".to_string())?;

        let callback_response: Value = self
            .http
            .get(build_url("/oauth/callback"))
            .headers(app_headers(&tokens.app_id)?)
            .query(&[
                ("code", code.to_string()),
                ("private_key", private_key),
                ("app_id", tokens.app_id.clone()),
            ])
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz OAuth callback failed", e))?
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz OAuth callback response was not JSON", e))?;

        let token = callback_response
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| "Qobuz OAuth callback did not return a token".to_string())?
            .to_string();

        let login_response: Value = self
            .http
            .post(build_url("/user/login"))
            .headers(auth_headers(&tokens.app_id, &token)?)
            .header("Content-Type", "text/plain;charset=UTF-8")
            .body("extra=partner")
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz OAuth session request failed", e))?
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz OAuth session response was not JSON", e))?;

        if login_response.get("status").and_then(Value::as_str) == Some("error") {
            let message = login_response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Qobuz rejected the OAuth token");
            warn!(
                event = "qobuz_auth_failure",
                flow = "oauth",
                error = %crate::diagnostics::logging::sanitize_error(message),
                "Qobuz rejected OAuth authentication"
            );
            return Err("Qobuz authentication failed".to_string());
        }

        let session = parse_login_response(&login_response)?;
        self.save_session(Some(&session))?;
        *self.session.write().await = Some(session.clone());
        *self.preferred_format_id.write().await = None;
        self.clear_home_cache().await;
        self.clear_album_detail_cache().await;
        Ok(self.status().await)
    }

    async fn consume_oauth_state(&self, presented: &str) -> Result<(), String> {
        let mut pending = self.pending_oauth_states.lock().await;
        pending.retain(|candidate| candidate.issued_at.elapsed() <= OAUTH_STATE_TTL);
        let Some(index) = pending.iter().position(|candidate| {
            crate::zones::constant_time_token_matches(
                std::slice::from_ref(&candidate.value),
                presented,
            )
        }) else {
            return Err("Invalid or expired Qobuz OAuth state".to_string());
        };
        pending.swap_remove(index);
        Ok(())
    }

    pub(super) async fn ensure_tokens(&self) -> Result<BundleTokens, String> {
        if let Some(tokens) = self.tokens.read().await.clone() {
            return Ok(tokens);
        }

        // Do the network extraction outside the write lock. If the Qobuz web
        // bundle is slow or unreachable, status checks should not queue behind
        // a held write lock and make the whole integration look frozen.
        let tokens = extract_bundle_tokens(&self.http).await?;
        let mut guard = self.tokens.write().await;
        if let Some(existing) = guard.clone() {
            return Ok(existing);
        }
        *guard = Some(tokens.clone());
        Ok(tokens)
    }

    pub(super) async fn ensure_secret(&self) -> Result<String, String> {
        if let Some(secret) = self.validated_secret.read().await.clone() {
            return Ok(secret);
        }
        let tokens = self.ensure_tokens().await?;
        for secret in &tokens.secrets {
            if self.test_secret(&tokens.app_id, secret).await? {
                *self.validated_secret.write().await = Some(secret.clone());
                return Ok(secret.clone());
            }
        }
        Err("Qobuz app secrets were extracted, but none validated".to_string())
    }

    async fn test_secret(&self, app_id: &str, secret: &str) -> Result<bool, String> {
        let track_id = 5_966_783_u64;
        let format_id = 5_u32;
        let timestamp = timestamp();
        let signature = sign_get_file_url(track_id, format_id, timestamp, secret);
        let response = self
            .http
            .get(build_url("/track/getFileUrl"))
            .headers(app_headers(app_id)?)
            .query(&[
                ("track_id", track_id.to_string()),
                ("format_id", format_id.to_string()),
                ("intent", "stream".to_string()),
                ("request_ts", timestamp.to_string()),
                ("request_sig", signature),
            ])
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("validate Qobuz app secret", e))?;
        Ok(response.status() != StatusCode::BAD_REQUEST)
    }
}

fn load_session_with_store(
    cache_dir: &Path,
    secrets: &dyn SecretsStore,
    account: &str,
) -> Option<UserSession> {
    let key = SecretKey::QobuzSession {
        account: account.to_string(),
    };
    let legacy = load_json_session(cache_dir);

    match secrets.get(key.clone()) {
        Ok(Some(value)) => match serde_json::from_str::<UserSession>(value.expose_secret()) {
            Ok(session) => {
                remove_legacy_session_file(cache_dir);
                Some(session)
            }
            Err(e) => {
                eprintln!("qobuz: secure session was invalid; ignoring session: {e}");
                None
            }
        },
        Ok(None) => {
            if let Some(session) = legacy {
                match serde_json::to_string_pretty(&session)
                    .map_err(|e| e.to_string())
                    .and_then(|json| {
                        secrets
                            .put(key, SecretValue::new(json))
                            .map_err(|e| e.to_string())
                    }) {
                    Ok(()) => {
                        remove_legacy_session_file(cache_dir);
                        Some(session)
                    }
                    Err(e) => {
                        warn_json_session_ignored(cache_dir, &e);
                        None
                    }
                }
            } else {
                None
            }
        }
        Err(e) => {
            eprintln!("qobuz: secure session storage unavailable: {e}");
            None
        }
    }
}

fn save_session_with_store(
    cache_dir: &Path,
    secrets: &dyn SecretsStore,
    account: &str,
    session: Option<&UserSession>,
) -> Result<(), String> {
    let key = SecretKey::QobuzSession {
        account: account.to_string(),
    };
    if let Some(session) = session {
        let json = match serde_json::to_string_pretty(session) {
            Ok(json) => json,
            Err(e) => {
                return Err(format!("qobuz: failed to serialize session: {e}"));
            }
        };
        secrets
            .put(key, SecretValue::new(json))
            .map_err(|e| format!("save Qobuz session in secure store: {e}"))?;
        remove_legacy_session_file(cache_dir);
    } else {
        secrets
            .delete(key)
            .map_err(|e| format!("delete Qobuz session from secure store: {e}"))?;
        remove_legacy_session_file(cache_dir);
    }
    Ok(())
}

fn load_json_session(cache_dir: &Path) -> Option<UserSession> {
    let session_path = session_file_path(cache_dir);
    if !session_path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(session_path).ok()?;
    serde_json::from_str::<UserSession>(&content).ok()
}

#[cfg(test)]
fn save_json_session(cache_dir: &Path, json: &str) -> Result<(), String> {
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("create cache directory: {e}"))?;
    let path = session_file_path(cache_dir);
    std::fs::write(&path, json).map_err(|e| format!("write session JSON: {e}"))?;
    restrict_session_file_permissions(&path);
    Ok(())
}

fn remove_legacy_session_file(cache_dir: &Path) {
    let path = session_file_path(cache_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "qobuz: failed to remove legacy session JSON {:?}: {e}",
            path
        ),
    }
}

fn session_file_path(cache_dir: &Path) -> std::path::PathBuf {
    cache_dir.join(SESSION_FILE_NAME)
}

pub(super) fn session_account(cache_dir: &Path) -> String {
    let stable_path = if cache_dir.is_absolute() {
        cache_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(cache_dir))
            .unwrap_or_else(|_| cache_dir.to_path_buf())
    };
    let digest = sha2::Sha256::digest(stable_path.to_string_lossy().as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn warn_json_session_ignored(cache_dir: &Path, reason: &str) {
    eprintln!(
        "qobuz: WARNING: secure session storage unavailable ({reason}); \
         ignoring legacy local JSON auth material at {:?}",
        session_file_path(cache_dir)
    );
}

#[cfg(all(test, unix))]
fn restrict_session_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!("qobuz: failed to restrict session JSON permissions: {e}");
    }
}

#[cfg(all(test, not(unix)))]
fn restrict_session_file_permissions(_path: &Path) {}

async fn extract_bundle_tokens(client: &Client) -> Result<BundleTokens, String> {
    let login_page = client
        .get(LOGIN_PAGE_URL)
        .send()
        .await
        .map_err(|e| qobuz_reqwest_error("fetch Qobuz login page", e))?
        .text()
        .await
        .map_err(|e| qobuz_reqwest_error("read Qobuz login page", e))?;

    let bundle_url = extract_bundle_url(&login_page)?;
    let bundle_content = client
        .get(format!("{}{}", BUNDLE_BASE_URL, bundle_url))
        .send()
        .await
        .map_err(|e| qobuz_reqwest_error("fetch Qobuz web bundle", e))?
        .text()
        .await
        .map_err(|e| qobuz_reqwest_error("read Qobuz web bundle", e))?;

    let app_id = extract_app_id(&bundle_content)?;
    let secrets = extract_secrets(&bundle_content)?;
    let private_key = extract_private_key(&bundle_content);
    if secrets.is_empty() {
        return Err("No Qobuz app secrets found in web bundle".to_string());
    }

    Ok(BundleTokens {
        app_id,
        secrets,
        private_key,
    })
}

fn extract_bundle_url(html: &str) -> Result<String, String> {
    let re =
        Regex::new(r#"<script src="(/resources/\d+\.\d+\.\d+-[a-z]\d{3}/bundle\.js)"></script>"#)
            .map_err(|e| e.to_string())?;
    re.captures(html)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| "Qobuz bundle URL not found".to_string())
}

fn generate_oauth_state() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn oauth_peer_ips_match(issued: IpAddr, presented: IpAddr) -> bool {
    issued == presented || (issued.is_loopback() && presented.is_loopback())
}

fn extract_app_id(bundle: &str) -> Result<String, String> {
    let re =
        Regex::new(r#"production:\{api:\{appId:"(?P<app_id>\d{9})""#).map_err(|e| e.to_string())?;
    re.captures(bundle)
        .and_then(|caps| caps.name("app_id"))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| "Qobuz app ID not found".to_string())
}

fn extract_secrets(bundle: &str) -> Result<Vec<String>, String> {
    let seed_re = Regex::new(
        r#"[a-z]\.initialSeed\("(?P<seed>[\w=]+)",window\.utimezone\.(?P<timezone>[a-z]+)\)"#,
    )
    .map_err(|e| e.to_string())?;

    let mut seeds = std::collections::HashMap::<String, String>::new();
    let mut timezones = Vec::new();
    for caps in seed_re.captures_iter(bundle) {
        if let (Some(seed), Some(tz)) = (caps.name("seed"), caps.name("timezone")) {
            let tz = tz.as_str().to_string();
            seeds.insert(tz.clone(), seed.as_str().to_string());
            timezones.push(tz);
        }
    }

    if seeds.is_empty() {
        return Err("No Qobuz secret seeds found".to_string());
    }

    let tz_pattern = timezones
        .iter()
        .map(|tz| {
            let mut chars = tz.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("|");

    let info_re = Regex::new(&format!(
        r#"name:"\w+/(?P<timezone>{})",info:"(?P<info>[\w=]+)",extras:"(?P<extras>[\w=]+)""#,
        tz_pattern
    ))
    .map_err(|e| e.to_string())?;

    let mut secrets = Vec::new();
    for caps in info_re.captures_iter(bundle) {
        let Some(tz) = caps.name("timezone") else {
            continue;
        };
        let Some(info) = caps.name("info") else {
            continue;
        };
        let Some(extras) = caps.name("extras") else {
            continue;
        };
        let Some(seed) = seeds.get(&tz.as_str().to_lowercase()) else {
            continue;
        };
        let combined = format!("{}{}{}", seed, info.as_str(), extras.as_str());
        if combined.len() <= 44 {
            continue;
        }
        let trimmed = &combined[..combined.len() - 44];
        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(trimmed)
            && let Ok(secret) = String::from_utf8(decoded)
        {
            secrets.push(secret);
        }
    }

    if secrets.is_empty() {
        let simple_re = Regex::new(r#"appSecret:"([a-f0-9]{32})""#).map_err(|e| e.to_string())?;
        for caps in simple_re.captures_iter(bundle) {
            if let Some(secret) = caps.get(1) {
                secrets.push(secret.as_str().to_string());
            }
        }
    }

    Ok(secrets)
}

fn extract_private_key(bundle: &str) -> Option<String> {
    let re = Regex::new(r#"privateKey:\s*"(?P<key>[A-Za-z0-9]{6,30})""#).ok()?;
    re.captures(bundle)
        .and_then(|caps| caps.name("key"))
        .map(|m| m.as_str().to_string())
}

fn parse_login_response(response: &Value) -> Result<UserSession, String> {
    let user = response
        .get("user")
        .ok_or_else(|| "No user object in Qobuz login response".to_string())?;
    let user_auth_token = response
        .get("user_auth_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "No auth token in Qobuz login response".to_string())?
        .to_string();

    let credential = user.get("credential");
    let subscription_label = credential
        .and_then(|c| c.get("parameters"))
        .and_then(|p| p.get("short_label"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();

    Ok(UserSession {
        user_auth_token,
        user: QobuzUser {
            email: user
                .get("email")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            display_name: user
                .get("display_name")
                .and_then(Value::as_str)
                .or_else(|| user.get("login").and_then(Value::as_str))
                .unwrap_or("")
                .to_string(),
            subscription_label,
        },
    })
}

pub(super) fn build_url(path: &str) -> String {
    format!("{}{}", BASE_URL, path)
}

pub(super) fn app_headers(app_id: &str) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-App-Id",
        HeaderValue::from_str(app_id).map_err(|e| format!("invalid Qobuz app id: {e}"))?,
    );
    Ok(headers)
}

pub(super) fn auth_headers(app_id: &str, auth_token: &str) -> Result<HeaderMap, String> {
    let mut headers = app_headers(app_id)?;
    headers.insert(
        "X-User-Auth-Token",
        HeaderValue::from_str(auth_token).map_err(|e| format!("invalid Qobuz auth token: {e}"))?,
    );
    Ok(headers)
}

pub(super) fn headers_for_optional_session(
    app_id: &str,
    auth_token: Option<&str>,
) -> Result<HeaderMap, String> {
    match auth_token {
        Some(token) => auth_headers(app_id, token),
        None => app_headers(app_id),
    }
}

fn generate_signature(method: &str, params: &str, ts: u64, secret: &str) -> String {
    let sig_string = format!("{}{}{}{}", method, params, ts, secret);
    let mut hasher = Md5::new();
    hasher.update(sig_string.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn sign_search(
    method: &str,
    query: &str,
    limit: u32,
    offset: u32,
    search_type: Option<&str>,
    ts: u64,
    secret: &str,
) -> String {
    let mut params = format!("limit{}offset{}query{}", limit, offset, query);
    if let Some(search_type) = search_type {
        params.push_str(&format!("type{}", search_type));
    }
    generate_signature(method, &params, ts, secret)
}

pub(super) fn sign_request(
    method: &str,
    kv_pairs: &[(&str, &str)],
    ts: u64,
    secret: &str,
) -> String {
    let mut sorted = kv_pairs.to_vec();
    sorted.sort_by_key(|(key, _)| *key);
    let params = sorted
        .iter()
        .map(|(key, value)| format!("{key}{value}"))
        .collect::<String>();
    generate_signature(method, &params, ts, secret)
}

pub(super) fn sign_get_file_url(track_id: u64, format_id: u32, ts: u64, secret: &str) -> String {
    let params = format!("format_id{}intentstreamtrack_id{}", format_id, track_id);
    generate_signature("trackgetFileUrl", &params, ts, secret)
}

pub(super) fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{MemorySecretsStore, SecretError};

    struct FailingSecretsStore;

    impl SecretsStore for FailingSecretsStore {
        fn get(&self, _key: SecretKey) -> Result<Option<SecretValue>, SecretError> {
            Ok(None)
        }

        fn put(&self, _key: SecretKey, _value: SecretValue) -> Result<(), SecretError> {
            Err(SecretError::Unavailable("locked".to_string()))
        }

        fn delete(&self, _key: SecretKey) -> Result<(), SecretError> {
            Err(SecretError::Unavailable("locked".to_string()))
        }
    }

    fn qobuz_session_key(cache_dir: &Path) -> SecretKey {
        SecretKey::QobuzSession {
            account: session_account(cache_dir),
        }
    }

    fn store_session(store: &MemorySecretsStore, cache_dir: &Path, session: &UserSession) {
        store
            .put(
                qobuz_session_key(cache_dir),
                SecretValue::new(serde_json::to_string_pretty(session).unwrap()),
            )
            .unwrap();
    }

    fn stored_session(store: &MemorySecretsStore, cache_dir: &Path) -> Option<UserSession> {
        store
            .get(qobuz_session_key(cache_dir))
            .unwrap()
            .and_then(|value| serde_json::from_str(value.expose_secret()).ok())
    }

    fn session(token: &str) -> UserSession {
        UserSession {
            user_auth_token: token.to_string(),
            user: QobuzUser {
                email: format!("{token}@example.test"),
                display_name: format!("User {token}"),
                subscription_label: "Studio".to_string(),
            },
        }
    }

    fn temp_cache_dir(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fozmo-qobuz-auth-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn qobuz_service(name: &str) -> QobuzService {
        QobuzService::new(
            temp_cache_dir(name),
            std::sync::Arc::new(MemorySecretsStore::new()),
        )
        .unwrap()
    }

    #[test]
    fn oauth_state_nonces_are_random_and_url_safe() {
        let first = generate_oauth_state();
        let second = generate_oauth_state();

        assert_ne!(first, second);
        assert!(first.len() >= 40);
        assert!(
            first
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        );
    }

    #[tokio::test]
    async fn oauth_state_rejects_mismatch_and_consumes_a_valid_nonce_once() {
        let service = qobuz_service("oauth-state-once");
        service
            .pending_oauth_states
            .lock()
            .await
            .push(PendingOAuthState {
                value: "valid-oauth-state".to_string(),
                issued_at: Instant::now(),
                peer_ip: None,
            });

        assert!(
            service
                .consume_oauth_state("wrong-oauth-state")
                .await
                .is_err()
        );
        assert_eq!(service.pending_oauth_states.lock().await.len(), 1);
        assert!(
            service
                .consume_oauth_state("valid-oauth-state")
                .await
                .is_ok()
        );
        assert!(service.pending_oauth_states.lock().await.is_empty());
        assert!(
            service
                .consume_oauth_state("valid-oauth-state")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn oauth_state_rejects_and_removes_expired_nonce() {
        let service = qobuz_service("oauth-state-expired");
        service
            .pending_oauth_states
            .lock()
            .await
            .push(PendingOAuthState {
                value: "expired-oauth-state".to_string(),
                issued_at: Instant::now() - OAUTH_STATE_TTL - Duration::from_secs(1),
                peer_ip: None,
            });

        let error = service
            .consume_oauth_state("expired-oauth-state")
            .await
            .unwrap_err();

        assert_eq!(error, "Invalid or expired Qobuz OAuth state");
        assert!(service.pending_oauth_states.lock().await.is_empty());
    }

    #[tokio::test]
    async fn oauth_state_supports_overlapping_browser_flows() {
        let service = qobuz_service("oauth-state-overlapping-browsers");
        service.pending_oauth_states.lock().await.extend([
            PendingOAuthState {
                value: "lan-browser-state".to_string(),
                issued_at: Instant::now(),
                peer_ip: Some("192.168.1.20".parse().unwrap()),
            },
            PendingOAuthState {
                value: "server-browser-state".to_string(),
                issued_at: Instant::now(),
                peer_ip: Some("127.0.0.1".parse().unwrap()),
            },
        ]);

        service
            .consume_oauth_state("lan-browser-state")
            .await
            .unwrap();
        service
            .consume_oauth_state("server-browser-state")
            .await
            .unwrap();
        assert!(service.pending_oauth_states.lock().await.is_empty());
    }

    #[tokio::test]
    async fn oauth_state_can_fall_back_to_the_initiating_lan_peer() {
        let service = qobuz_service("oauth-state-lan-peer");
        service
            .pending_oauth_states
            .lock()
            .await
            .push(PendingOAuthState {
                value: "lan-peer-state".to_string(),
                issued_at: Instant::now(),
                peer_ip: Some("192.168.1.20".parse().unwrap()),
            });

        assert!(
            service
                .oauth_state_matches_peer("lan-peer-state", Some("192.168.1.20".parse().unwrap()))
                .await
        );
        assert!(
            !service
                .oauth_state_matches_peer("lan-peer-state", Some("192.168.1.21".parse().unwrap()))
                .await
        );
        assert!(
            !service
                .oauth_state_matches_peer("wrong-state", Some("192.168.1.20".parse().unwrap()))
                .await
        );
    }

    #[tokio::test]
    async fn oauth_callback_can_recover_one_pending_state_for_its_peer() {
        let service = qobuz_service("oauth-state-recover-for-peer");
        service
            .pending_oauth_states
            .lock()
            .await
            .push(PendingOAuthState {
                value: "state-not-echoed-by-provider".to_string(),
                issued_at: Instant::now(),
                peer_ip: Some("192.168.1.20".parse().unwrap()),
            });

        assert_eq!(
            service
                .pending_oauth_state_for_peer(Some("192.168.1.20".parse().unwrap()))
                .await
                .as_deref(),
            Some("state-not-echoed-by-provider")
        );
        assert!(
            service
                .pending_oauth_state_for_peer(Some("192.168.1.21".parse().unwrap()))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn oauth_callback_does_not_guess_between_two_flows_from_one_peer() {
        let service = qobuz_service("oauth-state-ambiguous-peer");
        let peer_ip = "192.168.1.20".parse().unwrap();
        service.pending_oauth_states.lock().await.extend([
            PendingOAuthState {
                value: "first-state".to_string(),
                issued_at: Instant::now(),
                peer_ip: Some(peer_ip),
            },
            PendingOAuthState {
                value: "second-state".to_string(),
                issued_at: Instant::now(),
                peer_ip: Some(peer_ip),
            },
        ]);

        assert!(
            service
                .pending_oauth_state_for_peer(Some(peer_ip))
                .await
                .is_none()
        );
    }

    #[test]
    fn oauth_state_treats_ipv4_and_ipv6_loopback_as_the_same_peer() {
        assert!(oauth_peer_ips_match(
            "127.0.0.1".parse().unwrap(),
            "::1".parse().unwrap()
        ));
    }

    #[test]
    fn credential_container_debug_output_is_redacted() {
        let bundle = BundleTokens {
            app_id: "123456789".to_string(),
            secrets: vec!["bundle-secret-value".to_string()],
            private_key: Some("oauth-private-key".to_string()),
        };
        let session = session("user-auth-token-value");

        let bundle_debug = format!("{bundle:?}");
        let session_debug = format!("{session:?}");
        assert!(!bundle_debug.contains("bundle-secret-value"));
        assert!(!bundle_debug.contains("oauth-private-key"));
        assert!(!session_debug.contains("user-auth-token-value"));
        assert!(!session_debug.contains("@example.test"));
    }

    #[test]
    fn secret_store_session_load_wins_over_legacy_json_and_removes_it() {
        let cache_dir = temp_cache_dir("secret-store-wins");
        let secure_session = session("secure");
        let legacy_session = session("legacy");
        save_json_session(
            &cache_dir,
            &serde_json::to_string_pretty(&legacy_session).unwrap(),
        )
        .unwrap();
        let store = MemorySecretsStore::new();
        store_session(&store, &cache_dir, &secure_session);

        let loaded =
            load_session_with_store(&cache_dir, &store, &session_account(&cache_dir)).unwrap();

        assert_eq!(loaded.user_auth_token, "secure");
        assert!(!session_file_path(&cache_dir).exists());
    }

    #[test]
    fn legacy_json_migrates_to_secret_store_when_available() {
        let cache_dir = temp_cache_dir("legacy-migrates");
        let legacy_session = session("legacy");
        save_json_session(
            &cache_dir,
            &serde_json::to_string_pretty(&legacy_session).unwrap(),
        )
        .unwrap();
        let store = MemorySecretsStore::new();

        let loaded =
            load_session_with_store(&cache_dir, &store, &session_account(&cache_dir)).unwrap();

        assert_eq!(loaded.user_auth_token, "legacy");
        assert_eq!(
            stored_session(&store, &cache_dir).unwrap().user_auth_token,
            legacy_session.user_auth_token
        );
        assert!(!session_file_path(&cache_dir).exists());
    }

    #[test]
    fn invalid_secret_store_session_does_not_fall_back_to_legacy_json() {
        let cache_dir = temp_cache_dir("invalid-secret-no-fallback");
        let legacy_session = session("legacy");
        save_json_session(
            &cache_dir,
            &serde_json::to_string_pretty(&legacy_session).unwrap(),
        )
        .unwrap();
        let store = MemorySecretsStore::new();
        store
            .put(qobuz_session_key(&cache_dir), SecretValue::new("{ nope"))
            .unwrap();

        let loaded = load_session_with_store(&cache_dir, &store, &session_account(&cache_dir));

        assert!(loaded.is_none());
        assert!(session_file_path(&cache_dir).exists());
    }

    #[test]
    fn secure_store_save_failure_does_not_fall_back_to_local_json() {
        let cache_dir = temp_cache_dir("no-fallback-json");
        let store = FailingSecretsStore;

        let result = save_session_with_store(
            &cache_dir,
            &store,
            &session_account(&cache_dir),
            Some(&session("fallback")),
        );

        assert!(result.is_err());
        assert!(!session_file_path(&cache_dir).exists());
    }

    #[test]
    fn logout_deletes_secret_store_and_legacy_json_session_state() {
        let cache_dir = temp_cache_dir("logout-clears");
        let legacy_session = session("legacy");
        save_json_session(
            &cache_dir,
            &serde_json::to_string_pretty(&legacy_session).unwrap(),
        )
        .unwrap();
        let store = MemorySecretsStore::new();
        store_session(&store, &cache_dir, &session("secure"));

        save_session_with_store(&cache_dir, &store, &session_account(&cache_dir), None).unwrap();

        assert!(stored_session(&store, &cache_dir).is_none());
        assert!(!session_file_path(&cache_dir).exists());
    }
}
