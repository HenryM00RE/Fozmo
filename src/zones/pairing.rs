use crate::secrets::{SecretKey, SecretValue, SecretsStore};
use crate::settings::{
    AuthTokenBinding, AuthTokenKind, PairingTokenRecord, RemoteSessionClientMetadata, SettingsStore,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_PAIRING_TOKEN_TTL_SECS: u64 = 60 * 5;
pub const DEFAULT_CONTROL_SESSION_TTL_SECS: u64 = 60 * 60 * 24 * 7;
pub const DEFAULT_AGENT_TOKEN_TTL_SECS: u64 = 60 * 60 * 24 * 90;
pub const REMOTE_LINK_CODE_TTL_SECS: u64 = 60 * 5;
pub const DEFAULT_REMOTE_SESSION_TTL_SECS: u64 = 60 * 60 * 24 * 7;
/// Keep terminal token metadata long enough for troubleshooting, without
/// retaining expired token hashes forever.
const TOKEN_HISTORY_RETENTION_SECS: u64 = 60 * 60 * 24 * 30;
pub const CONTROL_SESSION_COOKIE: &str = "fozmo_control_session";
pub const REMOTE_SESSION_COOKIE: &str = "fozmo_remote_session";

pub const SCOPE_SESSION_CREATE: &str = "session:create";
pub const SCOPE_CONTROL: &str = "control";
pub const SCOPE_AGENT_CONNECT: &str = "agent:connect";
pub const SCOPE_STREAM_READ: &str = "stream:read";
pub const SCOPE_REMOTE: &str = "remote";
pub const SCOPE_REMOTE_SESSION_CREATE: &str = "remote:session:create";

#[derive(Clone)]
pub struct PairingManager {
    secrets: Arc<dyn SecretsStore>,
    records_key: SecretKey,
    /// Lazily loaded once, then shared by all cloned managers. Verification
    /// only takes a read lock and never touches the OS keychain.
    records: Arc<RwLock<Option<Vec<PairingTokenRecord>>>>,
    /// Runtime-only usage timestamps. They are folded into the next explicit
    /// durable mutation, but authentication traffic never persists them.
    last_used: Arc<Mutex<HashMap<String, u64>>>,
    /// Stream credentials delivered over currently connected native-agent
    /// sockets. They are deliberately memory-only and revoked on disconnect.
    agent_stream_sessions: Arc<RwLock<HashSet<String>>>,
    auth_required: bool,
    token_ttl_secs: u64,
    allow_query_token_auth: bool,
}

impl PairingManager {
    pub fn new(
        settings: Arc<SettingsStore>,
        secrets: Arc<dyn SecretsStore>,
        auth_required: bool,
        token_ttl_secs: u64,
        allow_query_token_auth: bool,
    ) -> Self {
        let records_key = settings.pairing_token_records_secret_key();
        Self {
            secrets,
            records_key,
            records: Arc::new(RwLock::new(None)),
            last_used: Arc::new(Mutex::new(HashMap::new())),
            agent_stream_sessions: Arc::new(RwLock::new(HashSet::new())),
            auth_required,
            token_ttl_secs: token_ttl_secs.max(1),
            allow_query_token_auth,
        }
    }

    pub fn auth_required(&self) -> bool {
        self.auth_required
    }

    pub fn query_token_auth_allowed(&self, local_request: bool) -> bool {
        self.allow_query_token_auth && local_request
    }

    pub fn create_token(&self) -> Result<IssuedPairingToken, String> {
        self.create_scoped_token(
            AuthTokenKind::PairingToken,
            &[SCOPE_SESSION_CREATE],
            "Browser pairing token",
            self.token_ttl_secs.min(DEFAULT_PAIRING_TOKEN_TTL_SECS),
            None,
            None,
            None,
        )
    }

    pub fn create_control_session(
        &self,
        subject: Option<String>,
    ) -> Result<IssuedPairingToken, String> {
        self.create_scoped_token(
            AuthTokenKind::ControlSession,
            &[SCOPE_CONTROL],
            "Browser control session",
            DEFAULT_CONTROL_SESSION_TTL_SECS,
            subject,
            None,
            None,
        )
    }

    pub fn create_agent_token(&self, label: Option<String>) -> Result<IssuedPairingToken, String> {
        self.create_scoped_token(
            AuthTokenKind::AgentToken,
            &[SCOPE_AGENT_CONNECT, SCOPE_STREAM_READ],
            label.clone().as_deref().unwrap_or("Agent token"),
            DEFAULT_AGENT_TOKEN_TTL_SECS,
            label,
            None,
            None,
        )
    }

    pub fn create_agent_stream_session(&self) -> String {
        let token = random_url_token(32);
        self.agent_stream_sessions
            .write()
            .unwrap()
            .insert(token_hash(&token));
        token
    }

    pub fn revoke_agent_stream_session(&self, token: &str) {
        if let Some(hash) = normalized_token_hash(Some(token)) {
            self.agent_stream_sessions.write().unwrap().remove(&hash);
        }
    }

    /// High-entropy single-use code exchanged on the remote listener for a
    /// remote session. 256-bit URL-safe token; a human-facing display may
    /// group/hyphenate it but must never reduce its entropy.
    pub fn create_remote_link_code(
        &self,
        subject: Option<String>,
    ) -> Result<IssuedPairingToken, String> {
        self.create_scoped_token(
            AuthTokenKind::RemoteLinkCode,
            &[SCOPE_REMOTE_SESSION_CREATE],
            "Remote link code",
            REMOTE_LINK_CODE_TTL_SECS,
            subject,
            None,
            None,
        )
    }

    /// Remote sessions are scoped to the remote listener. The LAN/local
    /// control surface must never accept them as control credentials.
    #[allow(dead_code)]
    pub fn create_remote_session(
        &self,
        subject: Option<String>,
    ) -> Result<IssuedPairingToken, String> {
        self.create_remote_session_with_metadata(subject, None)
    }

    pub fn create_remote_session_with_metadata(
        &self,
        subject: Option<String>,
        metadata: Option<RemoteSessionClientMetadata>,
    ) -> Result<IssuedPairingToken, String> {
        self.create_scoped_token(
            AuthTokenKind::RemoteSession,
            &[SCOPE_REMOTE],
            "Remote browser session",
            DEFAULT_REMOTE_SESSION_TTL_SECS,
            subject,
            None,
            metadata,
        )
    }

    pub fn verify_remote_token(&self, token: Option<&str>) -> bool {
        self.verify_token_scope_matching(token, SCOPE_REMOTE, |record| {
            matches!(record.kind, AuthTokenKind::RemoteSession)
        })
    }

    pub fn consume_remote_link_code(&self, token: Option<&str>) -> Result<bool, String> {
        self.consume_token_scope(token, SCOPE_REMOTE_SESSION_CREATE)
    }

    /// Number of unexpired, unrevoked remote sessions. Metadata only; never
    /// exposes token hashes.
    pub fn count_active_remote_sessions(&self) -> usize {
        let now = now_unix_secs();
        self.ensure_records_loaded()
            .map(|()| {
                let records = self.records.read().unwrap();
                records
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .filter(|record| {
                        token_record_active(record, now) && token_has_scope(record, SCOPE_REMOTE)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// Metadata-only remote sessions for local/LAN management UI. Token
    /// hashes and token values never leave this module.
    pub fn list_remote_sessions(&self) -> Result<Vec<RemoteSessionMetadata>, String> {
        self.ensure_records_loaded()?;
        let now = now_unix_secs();
        let records = self.records.read().unwrap();
        let records = records.as_deref().unwrap_or_default();
        let last_used = self.last_used.lock().unwrap();
        let sessions = records
            .iter()
            .filter_map(|record| {
                if !matches!(record.kind, AuthTokenKind::RemoteSession)
                    || !token_has_scope(record, SCOPE_REMOTE)
                    || record.revoked_at_unix_secs.is_some()
                {
                    return None;
                }
                Some(RemoteSessionMetadata {
                    id: record.id.clone(),
                    label: record
                        .subject
                        .clone()
                        .or_else(|| record.label.clone())
                        .unwrap_or_else(|| "Remote browser session".to_string()),
                    issued_at_unix_secs: record.issued_at_unix_secs,
                    expires_at_unix_secs: record.expires_at_unix_secs,
                    last_used_at_unix_secs: last_used
                        .get(&record.id)
                        .copied()
                        .max(record.last_used_at_unix_secs),
                    active: token_record_active(record, now),
                    client: record.remote_session_metadata.clone(),
                })
            })
            .collect::<Vec<_>>();
        Ok(sessions)
    }

    /// Revoke an active or expired remote session by opaque record ID. Other
    /// token kinds are intentionally ignored.
    pub fn revoke_remote_session_by_id(&self, id: &str) -> Result<bool, String> {
        let id = id.trim();
        if id.is_empty() {
            return Ok(false);
        }
        let now = now_unix_secs();
        let mut records_guard = self.records.write().unwrap();
        let records = self.records_mut(&mut records_guard)?;
        let mut candidate = records.clone();
        self.merge_last_used(&mut candidate);
        let mut revoked = false;
        for record in &mut candidate {
            if record.id == id
                && matches!(record.kind, AuthTokenKind::RemoteSession)
                && token_has_scope(record, SCOPE_REMOTE)
                && record.revoked_at_unix_secs.is_none()
            {
                record.revoked_at_unix_secs = Some(now);
                revoked = true;
            }
        }
        if revoked {
            prune_token_history(&mut candidate, now);
            self.save_records(&candidate)?;
            *records = candidate;
        }
        Ok(revoked)
    }

    // Stream-scoped tokens are reserved for direct media-proxy auth handoff.
    #[allow(dead_code)]
    pub fn create_stream_token(
        &self,
        ttl_secs: u64,
        binding: AuthTokenBinding,
    ) -> Result<IssuedPairingToken, String> {
        self.create_scoped_token(
            AuthTokenKind::StreamToken,
            &[SCOPE_STREAM_READ],
            "Stream token",
            ttl_secs,
            None,
            Some(binding),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_scoped_token(
        &self,
        kind: AuthTokenKind,
        scopes: &[&str],
        label: &str,
        ttl_secs: u64,
        subject: Option<String>,
        binding: Option<AuthTokenBinding>,
        remote_session_metadata: Option<RemoteSessionClientMetadata>,
    ) -> Result<IssuedPairingToken, String> {
        let token = random_url_token(32);
        let now = now_unix_secs();
        let record = PairingTokenRecord {
            id: random_url_token(16),
            kind,
            token_hash: token_hash(&token),
            scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
            subject,
            label: Some(label.to_string()),
            issued_at_unix_secs: now,
            expires_at_unix_secs: now.saturating_add(ttl_secs.max(1)),
            last_used_at_unix_secs: None,
            rotated_at_unix_secs: None,
            revoked_at_unix_secs: None,
            binding,
            remote_session_metadata,
        };
        let expires_at_unix_secs = record.expires_at_unix_secs;
        let mut records_guard = self.records.write().unwrap();
        let records = self.records_mut(&mut records_guard)?;
        let mut candidate = records.clone();
        self.merge_last_used(&mut candidate);
        prune_token_history(&mut candidate, now);
        candidate.push(record);
        self.save_records(&candidate)?;
        *records = candidate;
        Ok(IssuedPairingToken {
            token,
            expires_at_unix_secs,
        })
    }

    #[allow(dead_code)]
    pub fn verify(&self, token: Option<&str>) -> bool {
        if !self.auth_required {
            return true;
        }
        self.verify_control_token(token)
    }

    #[allow(dead_code)]
    pub fn verify_issued_token(&self, token: Option<&str>) -> bool {
        self.verify_control_token(token)
    }

    pub fn verify_control_token(&self, token: Option<&str>) -> bool {
        self.verify_token_scope_matching(token, SCOPE_CONTROL, |record| {
            matches!(
                record.kind,
                AuthTokenKind::ControlSession | AuthTokenKind::LegacyToken
            )
        })
    }

    pub fn verify_agent_token(&self, token: Option<&str>) -> bool {
        self.verify_token_scope(token, SCOPE_AGENT_CONNECT)
    }

    pub fn verify_stream_token(&self, token: Option<&str>) -> bool {
        if let Some(hash) = normalized_token_hash(token)
            && self
                .agent_stream_sessions
                .read()
                .unwrap()
                .iter()
                .any(|known| constant_time_eq(known.as_bytes(), hash.as_bytes()))
        {
            return true;
        }
        self.verify_token_scope(token, SCOPE_STREAM_READ)
    }

    fn verify_token_scope(&self, token: Option<&str>, scope: &str) -> bool {
        self.verify_token_scope_matching(token, scope, |_| true)
    }

    fn verify_token_scope_matching(
        &self,
        token: Option<&str>,
        scope: &str,
        kind_allowed: impl Fn(&PairingTokenRecord) -> bool,
    ) -> bool {
        let Some(hash) = normalized_token_hash(token) else {
            return false;
        };
        let now = now_unix_secs();
        match self.ensure_records_loaded() {
            Ok(()) => {
                let records = self.records.read().unwrap();
                let mut matched_id = None;
                for known in records.as_deref().unwrap_or_default() {
                    if token_record_active(known, now)
                        && token_has_scope(known, scope)
                        && kind_allowed(known)
                        && constant_time_eq(known.token_hash.as_bytes(), hash.as_bytes())
                    {
                        matched_id = Some(known.id.clone());
                    }
                }
                drop(records);
                if let Some(id) = matched_id {
                    self.last_used.lock().unwrap().insert(id, now);
                    true
                } else {
                    false
                }
            }
            Err(e) => {
                eprintln!("pairing: failed to read pairing records: {e}");
                false
            }
        }
    }

    pub fn consume_pairing_token(&self, token: Option<&str>) -> Result<bool, String> {
        self.consume_token_scope(token, SCOPE_SESSION_CREATE)
    }

    pub fn consume_token_scope(&self, token: Option<&str>, scope: &str) -> Result<bool, String> {
        let Some(hash) = normalized_token_hash(token) else {
            return Ok(false);
        };
        let now = now_unix_secs();
        let mut records_guard = self.records.write().unwrap();
        let records = self.records_mut(&mut records_guard)?;
        let mut candidate = records.clone();
        self.merge_last_used(&mut candidate);
        let mut consumed = false;
        for record in &mut candidate {
            if token_record_active(record, now)
                && token_has_scope(record, scope)
                && constant_time_eq(record.token_hash.as_bytes(), hash.as_bytes())
            {
                record.last_used_at_unix_secs = Some(now);
                record.revoked_at_unix_secs = Some(now);
                consumed = true;
            }
        }
        if consumed {
            prune_token_history(&mut candidate, now);
            self.save_records(&candidate)?;
            *records = candidate;
        }
        Ok(consumed)
    }

    pub fn revoke_token(&self, token: Option<&str>) -> Result<bool, String> {
        let Some(hash) = normalized_token_hash(token) else {
            return Ok(false);
        };
        let now = now_unix_secs();
        let mut records_guard = self.records.write().unwrap();
        let records = self.records_mut(&mut records_guard)?;
        let mut candidate = records.clone();
        self.merge_last_used(&mut candidate);
        let mut revoked = false;
        for record in &mut candidate {
            if token_record_active(record, now)
                && constant_time_eq(record.token_hash.as_bytes(), hash.as_bytes())
            {
                record.revoked_at_unix_secs = Some(now);
                revoked = true;
            }
        }
        if revoked {
            prune_token_history(&mut candidate, now);
            self.save_records(&candidate)?;
            *records = candidate;
        }
        Ok(revoked)
    }

    pub fn revoke_all_active(&self) -> Result<usize, String> {
        let now = now_unix_secs();
        let mut records_guard = self.records.write().unwrap();
        let records = self.records_mut(&mut records_guard)?;
        let mut candidate = records.clone();
        self.merge_last_used(&mut candidate);
        let mut revoked = 0usize;
        for record in &mut candidate {
            if token_record_active(record, now) {
                record.revoked_at_unix_secs = Some(now);
                revoked += 1;
            }
        }
        if revoked > 0 {
            prune_token_history(&mut candidate, now);
            self.save_records(&candidate)?;
            *records = candidate;
        }
        Ok(revoked)
    }

    fn load_records(&self) -> Result<Vec<PairingTokenRecord>, String> {
        self.secrets
            .get(self.records_key.clone())
            .map_err(|e| e.to_string())?
            .map(|value| {
                serde_json::from_str::<Vec<PairingTokenRecord>>(value.expose_secret())
                    .map_err(|e| e.to_string())
            })
            .transpose()
            .map(|records| {
                let mut records = records.unwrap_or_default();
                records.iter_mut().for_each(normalize_record);
                records
            })
    }

    fn ensure_records_loaded(&self) -> Result<(), String> {
        if self.records.read().unwrap().is_some() {
            return Ok(());
        }
        let mut records = self.records.write().unwrap();
        if records.is_none() {
            *records = Some(self.load_records()?);
        }
        Ok(())
    }

    fn records_mut<'a>(
        &self,
        records: &'a mut Option<Vec<PairingTokenRecord>>,
    ) -> Result<&'a mut Vec<PairingTokenRecord>, String> {
        if records.is_none() {
            *records = Some(self.load_records()?);
        }
        Ok(records.as_mut().expect("pairing records just loaded"))
    }

    fn merge_last_used(&self, records: &mut [PairingTokenRecord]) {
        let last_used = self.last_used.lock().unwrap();
        for record in records {
            if let Some(timestamp) = last_used.get(&record.id) {
                record.last_used_at_unix_secs = Some(
                    record
                        .last_used_at_unix_secs
                        .unwrap_or_default()
                        .max(*timestamp),
                );
            }
        }
    }

    fn save_records(&self, records: &[PairingTokenRecord]) -> Result<(), String> {
        let json = serde_json::to_string_pretty(records).map_err(|e| e.to_string())?;
        self.secrets
            .put(self.records_key.clone(), SecretValue::new(json))
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct IssuedPairingToken {
    pub token: String,
    pub expires_at_unix_secs: u64,
}

#[derive(Debug, Clone)]
pub struct RemoteSessionMetadata {
    pub id: String,
    pub label: String,
    pub issued_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
    pub last_used_at_unix_secs: Option<u64>,
    pub active: bool,
    pub client: Option<RemoteSessionClientMetadata>,
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for i in 0..max_len {
        let l = left.get(i).copied().unwrap_or(0);
        let r = right.get(i).copied().unwrap_or(0);
        diff |= (l ^ r) as usize;
    }
    diff == 0
}

pub(crate) fn constant_time_token_matches(known_tokens: &[String], token: &str) -> bool {
    known_tokens
        .iter()
        .any(|known| constant_time_eq(known.as_bytes(), token.as_bytes()))
}

fn normalized_token_hash(token: Option<&str>) -> Option<String> {
    token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(token_hash)
}

fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

fn token_record_active(record: &PairingTokenRecord, now: u64) -> bool {
    record.revoked_at_unix_secs.is_none() && record.expires_at_unix_secs > now
}

fn prune_token_history(records: &mut Vec<PairingTokenRecord>, now: u64) {
    records.retain(|record| {
        if token_record_active(record, now) {
            return true;
        }
        let terminal_at = record
            .revoked_at_unix_secs
            .unwrap_or(record.expires_at_unix_secs);
        terminal_at.saturating_add(TOKEN_HISTORY_RETENTION_SECS) > now
    });
}

fn token_has_scope(record: &PairingTokenRecord, scope: &str) -> bool {
    record.scopes.iter().any(|known| known == scope)
}

fn normalize_record(record: &mut PairingTokenRecord) {
    if record.scopes.is_empty() {
        record.scopes = match record.kind {
            AuthTokenKind::PairingToken => vec![SCOPE_SESSION_CREATE.to_string()],
            AuthTokenKind::ControlSession => vec![SCOPE_CONTROL.to_string()],
            AuthTokenKind::AgentToken => {
                vec![
                    SCOPE_AGENT_CONNECT.to_string(),
                    SCOPE_STREAM_READ.to_string(),
                ]
            }
            AuthTokenKind::StreamToken => vec![SCOPE_STREAM_READ.to_string()],
            AuthTokenKind::RemoteLinkCode => vec![SCOPE_REMOTE_SESSION_CREATE.to_string()],
            AuthTokenKind::RemoteSession => {
                vec![SCOPE_CONTROL.to_string(), SCOPE_REMOTE.to_string()]
            }
            AuthTokenKind::LegacyToken => vec![
                SCOPE_CONTROL.to_string(),
                SCOPE_AGENT_CONNECT.to_string(),
                SCOPE_STREAM_READ.to_string(),
            ],
        };
    }
    if record.label.is_none() {
        record.label = Some(
            match record.kind {
                AuthTokenKind::PairingToken => "Browser pairing token",
                AuthTokenKind::ControlSession => "Browser control session",
                AuthTokenKind::AgentToken => "Agent token",
                AuthTokenKind::StreamToken => "Stream token",
                AuthTokenKind::RemoteLinkCode => "Remote link code",
                AuthTokenKind::RemoteSession => "Remote browser session",
                AuthTokenKind::LegacyToken => "Legacy pairing token",
            }
            .to_string(),
        );
    }
}

fn random_url_token(bytes_len: usize) -> String {
    let mut bytes = vec![0_u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{MemorySecretsStore, SecretsStore};
    use std::path::{Path, PathBuf};

    fn temp_settings_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fozmo-pairing-{name}-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn pairing(
        path: &Path,
        auth_required: bool,
    ) -> (Arc<SettingsStore>, Arc<dyn SecretsStore>, PairingManager) {
        let settings = Arc::new(SettingsStore::new(path.to_path_buf()));
        let secrets: Arc<dyn SecretsStore> = Arc::new(MemorySecretsStore::new());
        let pairing = PairingManager::new(
            Arc::clone(&settings),
            Arc::clone(&secrets),
            auth_required,
            DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
        );
        (settings, secrets, pairing)
    }

    fn stored_records(
        settings: &SettingsStore,
        secrets: &dyn SecretsStore,
    ) -> Vec<PairingTokenRecord> {
        secrets
            .get(settings.pairing_token_records_secret_key())
            .unwrap()
            .map(|value| serde_json::from_str(value.expose_secret()).unwrap())
            .unwrap_or_default()
    }

    #[test]
    fn pairing_token_round_trips_through_hashed_settings() {
        let path = temp_settings_path("round-trip");
        let (settings, secrets, pairing) = pairing(&path, true);

        let issued = pairing.create_token().unwrap();

        assert!(pairing.consume_pairing_token(Some(&issued.token)).unwrap());
        assert!(
            !pairing
                .consume_pairing_token(Some("not-the-token"))
                .unwrap()
        );
        let snapshot = settings.snapshot();
        assert!(snapshot.pairing_tokens.is_none());
        assert!(snapshot.pairing_token_records.is_empty());
        let records = stored_records(settings.as_ref(), secrets.as_ref());
        assert_eq!(records.len(), 1);
        assert_ne!(records[0].token_hash, issued.token);
        assert_eq!(records[0].kind, AuthTokenKind::PairingToken);
        assert_eq!(records[0].scopes, vec![SCOPE_SESSION_CREATE.to_string()]);
        assert!(records[0].expires_at_unix_secs > now_unix_secs());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pairing_tokens_are_scoped_to_the_workspace_settings_path() {
        let path_a = temp_settings_path("scope-a");
        let path_b = temp_settings_path("scope-b");
        let settings_a = Arc::new(SettingsStore::new(path_a.clone()));
        let settings_b = Arc::new(SettingsStore::new(path_b.clone()));
        let secrets: Arc<dyn SecretsStore> = Arc::new(MemorySecretsStore::new());
        let pairing_a = PairingManager::new(
            Arc::clone(&settings_a),
            Arc::clone(&secrets),
            true,
            DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
        );
        let pairing_b = PairingManager::new(
            Arc::clone(&settings_b),
            Arc::clone(&secrets),
            true,
            DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
        );

        let token_a = pairing_a.create_control_session(None).unwrap().token;

        assert!(pairing_a.verify_issued_token(Some(&token_a)));
        assert!(!pairing_b.verify_issued_token(Some(&token_a)));
        assert_ne!(
            settings_a.pairing_token_records_secret_key().account(),
            settings_b.pairing_token_records_secret_key().account()
        );
        let _ = std::fs::remove_file(path_a);
        let _ = std::fs::remove_file(path_b);
    }

    #[test]
    fn legacy_global_pairing_tokens_are_not_accepted() {
        let path = temp_settings_path("legacy-global");
        let (settings, secrets, pairing) = pairing(&path, true);
        let token = pairing.create_control_session(None).unwrap().token;
        let scoped_key = settings.pairing_token_records_secret_key();
        let records = secrets.get(scoped_key.clone()).unwrap().unwrap();

        secrets
            .put(SecretKey::LegacyGlobalPairingTokenRecords, records)
            .unwrap();
        secrets.delete(scoped_key).unwrap();

        // A fresh workspace manager must not fall back to the legacy global
        // key. The existing manager intentionally keeps its in-memory index.
        let restarted = PairingManager::new(
            Arc::clone(&settings),
            Arc::clone(&secrets),
            true,
            DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
        );
        assert!(!restarted.verify_issued_token(Some(&token)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn generated_pairing_tokens_are_url_safe_and_distinct() {
        let path = temp_settings_path("random");
        let (_settings, _secrets, pairing) = pairing(&path, true);

        let first = pairing.create_token().unwrap().token;
        let second = pairing.create_token().unwrap().token;

        assert_ne!(first, second);
        assert_eq!(first.len(), 43);
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_pairing_tokens_are_migrated_to_records() {
        let path = temp_settings_path("legacy");
        std::fs::write(&path, r#"{ "pairing_tokens": ["legacy-token"] }"#).unwrap();
        let settings = Arc::new(SettingsStore::new(path.clone()));
        let secrets: Arc<dyn SecretsStore> = Arc::new(MemorySecretsStore::new());
        settings.migrate_legacy_secrets(secrets.as_ref(), DEFAULT_PAIRING_TOKEN_TTL_SECS);
        let pairing = PairingManager::new(
            Arc::clone(&settings),
            Arc::clone(&secrets),
            true,
            DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
        );

        assert!(pairing.verify_issued_token(Some("legacy-token")));
        let snapshot = settings.snapshot();
        assert!(snapshot.pairing_tokens.is_none());
        assert!(snapshot.pairing_token_records.is_empty());
        let records = stored_records(settings.as_ref(), secrets.as_ref());
        assert_eq!(records.len(), 1);
        assert_ne!(records[0].token_hash, "legacy-token");
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("legacy-token"));
        assert!(!persisted.contains("pairing_tokens"));
        assert!(!persisted.contains("pairing_token_records"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn expired_and_revoked_tokens_fail_verification() {
        let path = temp_settings_path("expired-revoked");
        let settings = Arc::new(SettingsStore::new(path.clone()));
        let secrets: Arc<dyn SecretsStore> = Arc::new(MemorySecretsStore::new());
        let pairing =
            PairingManager::new(Arc::clone(&settings), Arc::clone(&secrets), true, 1, false);
        let expired = "expired-token";
        let revoked = "revoked-token";
        let now = now_unix_secs();
        let records = vec![
            PairingTokenRecord {
                id: "expired".to_string(),
                kind: AuthTokenKind::ControlSession,
                token_hash: token_hash(expired),
                scopes: vec![SCOPE_CONTROL.to_string()],
                subject: None,
                label: None,
                issued_at_unix_secs: now.saturating_sub(10),
                expires_at_unix_secs: now,
                last_used_at_unix_secs: None,
                rotated_at_unix_secs: None,
                revoked_at_unix_secs: None,
                binding: None,
                remote_session_metadata: None,
            },
            PairingTokenRecord {
                id: "revoked".to_string(),
                kind: AuthTokenKind::ControlSession,
                token_hash: token_hash(revoked),
                scopes: vec![SCOPE_CONTROL.to_string()],
                subject: None,
                label: None,
                issued_at_unix_secs: now,
                expires_at_unix_secs: now.saturating_add(100),
                last_used_at_unix_secs: None,
                rotated_at_unix_secs: None,
                revoked_at_unix_secs: Some(now),
                binding: None,
                remote_session_metadata: None,
            },
        ];
        secrets
            .put(
                settings.pairing_token_records_secret_key(),
                SecretValue::new(serde_json::to_string_pretty(&records).unwrap()),
            )
            .unwrap();

        assert!(!pairing.verify_issued_token(Some(expired)));
        assert!(!pairing.verify_issued_token(Some(revoked)));
        assert!(!pairing.verify_issued_token(None));
        assert!(!pairing.verify_issued_token(Some("")));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn revocation_invalidates_tokens() {
        let path = temp_settings_path("revoke");
        let (_settings, _secrets, pairing) = pairing(&path, true);
        let first = pairing.create_control_session(None).unwrap().token;
        let second = pairing.create_control_session(None).unwrap().token;

        assert!(pairing.revoke_token(Some(&first)).unwrap());
        assert!(!pairing.verify_issued_token(Some(&first)));
        assert!(pairing.verify_issued_token(Some(&second)));
        assert_eq!(pairing.revoke_all_active().unwrap(), 1);
        assert!(!pairing.verify_issued_token(Some(&second)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn verification_tracks_last_used_without_persisting_it() {
        let path = temp_settings_path("scopes");
        let (settings, secrets, pairing) = pairing(&path, true);
        let agent = pairing
            .create_agent_token(Some("Test agent".to_string()))
            .unwrap()
            .token;

        assert!(pairing.verify_agent_token(Some(&agent)));
        assert!(pairing.verify_stream_token(Some(&agent)));
        assert!(!pairing.verify_control_token(Some(&agent)));

        let persisted_records = stored_records(settings.as_ref(), secrets.as_ref());
        let record = persisted_records
            .iter()
            .find(|record| record.kind == AuthTokenKind::AgentToken)
            .expect("agent token should be stored");
        assert_eq!(
            record.scopes,
            vec![
                SCOPE_AGENT_CONNECT.to_string(),
                SCOPE_STREAM_READ.to_string()
            ]
        );
        assert_eq!(record.last_used_at_unix_secs, None);

        // Usage remains available to runtime metadata consumers without a
        // SecretsStore::put (and therefore without a keychain write).
        let runtime_last_used = pairing.last_used.lock().unwrap();
        assert!(runtime_last_used.get(&record.id).is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_stream_sessions_are_memory_only_and_revocable() {
        let path = temp_settings_path("agent-stream-session");
        let (settings, secrets, pairing) = pairing(&path, true);
        let before = stored_records(settings.as_ref(), secrets.as_ref());

        let token = pairing.create_agent_stream_session();
        assert!(pairing.verify_stream_token(Some(&token)));
        assert!(!pairing.verify_agent_token(Some(&token)));
        assert_eq!(
            serde_json::to_value(stored_records(settings.as_ref(), secrets.as_ref())).unwrap(),
            serde_json::to_value(before).unwrap()
        );

        pairing.revoke_agent_stream_session(&token);
        assert!(!pairing.verify_stream_token(Some(&token)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn token_history_prunes_expired_unrevoked_records_after_retention_window() {
        let path = temp_settings_path("history-retention");
        let settings = Arc::new(SettingsStore::new(path.clone()));
        let secrets: Arc<dyn SecretsStore> = Arc::new(MemorySecretsStore::new());
        let now = now_unix_secs();
        let records = vec![PairingTokenRecord {
            id: "long-expired".to_string(),
            kind: AuthTokenKind::ControlSession,
            token_hash: token_hash("long-expired"),
            scopes: vec![SCOPE_CONTROL.to_string()],
            subject: None,
            label: None,
            issued_at_unix_secs: now
                .saturating_sub(TOKEN_HISTORY_RETENTION_SECS)
                .saturating_sub(100),
            expires_at_unix_secs: now
                .saturating_sub(TOKEN_HISTORY_RETENTION_SECS)
                .saturating_sub(1),
            last_used_at_unix_secs: None,
            rotated_at_unix_secs: None,
            revoked_at_unix_secs: None,
            binding: None,
            remote_session_metadata: None,
        }];
        secrets
            .put(
                settings.pairing_token_records_secret_key(),
                SecretValue::new(serde_json::to_string_pretty(&records).unwrap()),
            )
            .unwrap();
        let pairing = PairingManager::new(
            Arc::clone(&settings),
            Arc::clone(&secrets),
            true,
            DEFAULT_PAIRING_TOKEN_TTL_SECS,
            false,
        );

        pairing.create_control_session(None).unwrap();

        let persisted = stored_records(settings.as_ref(), secrets.as_ref());
        assert_eq!(persisted.len(), 1);
        assert_ne!(persisted[0].id, "long-expired");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pairing_verify_allows_missing_token_when_auth_is_disabled() {
        let path = temp_settings_path("disabled");
        let (_settings, _secrets, pairing) = pairing(&path, false);

        assert!(pairing.verify(None));
        assert!(pairing.verify(Some("")));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn strict_pairing_token_verification_ignores_auth_required_flag() {
        let path = temp_settings_path("strict-disabled");
        let (_settings, _secrets, pairing) = pairing(&path, false);

        let token = pairing.create_control_session(None).unwrap().token;

        assert!(pairing.verify_issued_token(Some(&token)));
        assert!(!pairing.verify_issued_token(None));
        assert!(!pairing.verify_issued_token(Some("not-the-token")));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remote_session_tokens_verify_only_for_remote_scope() {
        let path = temp_settings_path("remote-session-scopes");
        let (_settings, _secrets, pairing) = pairing(&path, false);

        let session = pairing.create_remote_session(None).unwrap().token;

        assert!(pairing.verify_remote_token(Some(&session)));
        assert!(!pairing.verify_control_token(Some(&session)));
        assert_eq!(pairing.count_active_remote_sessions(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migrated_remote_sessions_with_old_control_scope_do_not_verify_as_lan_control() {
        let path = temp_settings_path("remote-session-old-control-scope");
        let (settings, secrets, pairing) = pairing(&path, true);
        let token = "old-remote-session-token";
        let now = now_unix_secs();
        let records = vec![PairingTokenRecord {
            id: "old-remote".to_string(),
            kind: AuthTokenKind::RemoteSession,
            token_hash: token_hash(token),
            scopes: vec![SCOPE_CONTROL.to_string(), SCOPE_REMOTE.to_string()],
            subject: None,
            label: None,
            issued_at_unix_secs: now,
            expires_at_unix_secs: now.saturating_add(100),
            last_used_at_unix_secs: None,
            rotated_at_unix_secs: None,
            revoked_at_unix_secs: None,
            binding: None,
            remote_session_metadata: None,
        }];
        secrets
            .put(
                settings.pairing_token_records_secret_key(),
                SecretValue::new(serde_json::to_string_pretty(&records).unwrap()),
            )
            .unwrap();

        assert!(pairing.verify_remote_token(Some(token)));
        assert!(!pairing.verify_control_token(Some(token)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn lan_control_sessions_do_not_verify_for_remote_scope() {
        let path = temp_settings_path("lan-not-remote");
        let (_settings, _secrets, pairing) = pairing(&path, true);

        let control = pairing.create_control_session(None).unwrap().token;

        assert!(pairing.verify_control_token(Some(&control)));
        assert!(!pairing.verify_remote_token(Some(&control)));
        assert_eq!(pairing.count_active_remote_sessions(), 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remote_link_code_is_single_use_and_high_entropy() {
        let path = temp_settings_path("remote-link-code");
        let (_settings, _secrets, pairing) = pairing(&path, false);

        let code = pairing.create_remote_link_code(None).unwrap().token;

        // 32 random bytes => 43 URL-safe base64 chars (256-bit entropy).
        assert_eq!(code.len(), 43);
        assert!(pairing.consume_remote_link_code(Some(&code)).unwrap());
        assert!(!pairing.consume_remote_link_code(Some(&code)).unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remote_link_code_cannot_be_used_as_a_session_token() {
        let path = temp_settings_path("remote-link-not-session");
        let (_settings, _secrets, pairing) = pairing(&path, false);

        let code = pairing.create_remote_link_code(None).unwrap().token;

        assert!(!pairing.verify_remote_token(Some(&code)));
        assert!(!pairing.verify_control_token(Some(&code)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn expired_remote_link_codes_are_rejected() {
        let path = temp_settings_path("remote-link-expired");
        let (settings, secrets, pairing) = pairing(&path, false);
        let code = "expired-link-code";
        let now = now_unix_secs();
        let records = vec![PairingTokenRecord {
            id: "expired-link".to_string(),
            kind: AuthTokenKind::RemoteLinkCode,
            token_hash: token_hash(code),
            scopes: vec![SCOPE_REMOTE_SESSION_CREATE.to_string()],
            subject: None,
            label: None,
            issued_at_unix_secs: now.saturating_sub(600),
            expires_at_unix_secs: now.saturating_sub(1),
            last_used_at_unix_secs: None,
            rotated_at_unix_secs: None,
            revoked_at_unix_secs: None,
            binding: None,
            remote_session_metadata: None,
        }];
        secrets
            .put(
                settings.pairing_token_records_secret_key(),
                SecretValue::new(serde_json::to_string_pretty(&records).unwrap()),
            )
            .unwrap();

        assert!(!pairing.consume_remote_link_code(Some(code)).unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remote_session_metadata_never_exposes_token_hashes() {
        let path = temp_settings_path("remote-session-metadata");
        let (settings, secrets, pairing) = pairing(&path, false);

        let session = pairing.create_remote_session(None).unwrap().token;

        let records = stored_records(settings.as_ref(), secrets.as_ref());
        let record = records
            .iter()
            .find(|record| record.kind == AuthTokenKind::RemoteSession)
            .expect("remote session should be stored");
        assert_ne!(record.token_hash, session);
        // Counting is metadata-only and does not leak the token.
        assert_eq!(pairing.count_active_remote_sessions(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remote_session_listing_and_revoke_are_scoped_to_remote_sessions() {
        let path = temp_settings_path("remote-session-list-revoke");
        let (_settings, _secrets, pairing) = pairing(&path, true);

        let remote = pairing
            .create_remote_session(Some("Phone".to_string()))
            .unwrap()
            .token;
        let control = pairing.create_control_session(None).unwrap().token;
        assert!(pairing.verify_remote_token(Some(&remote)));
        assert!(pairing.verify_control_token(Some(&control)));

        let sessions = pairing.list_remote_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].label, "Phone");
        assert!(sessions[0].active);
        assert!(sessions[0].last_used_at_unix_secs.is_some());

        assert!(
            pairing
                .revoke_remote_session_by_id(&sessions[0].id)
                .unwrap()
        );
        assert!(!pairing.verify_remote_token(Some(&remote)));
        assert!(pairing.verify_control_token(Some(&control)));
        assert!(pairing.list_remote_sessions().unwrap().is_empty());
        assert!(
            !pairing
                .revoke_remote_session_by_id(&sessions[0].id)
                .unwrap()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn constant_time_comparison_checks_content_and_length() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"same", b"same-but-longer"));
        assert!(!constant_time_eq(b"same-but-longer", b"same"));
    }
}
