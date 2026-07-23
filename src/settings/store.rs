#[cfg(feature = "apple_music_capture")]
use super::AppleMusicCaptureSettings;
use super::{AppearanceSettings, ListeningProfile, PersistedSettings, ZonePlaybackSettings};
use crate::app::paths::atomic_write;
use crate::secrets::{SecretKey, SecretValue, SecretsStore};
use crate::settings::validation::parse_settings;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SETTINGS_DEBOUNCE: Duration = Duration::from_millis(25);

impl PersistedSettings {
    #[allow(dead_code)]
    pub fn load(path: &Path) -> Self {
        Self::try_load(path).unwrap_or_else(|error| panic!("{error}"))
    }

    pub fn try_load(path: &Path) -> Result<Self, String> {
        let backup_path = settings_backup_path(path);
        match fs::read_to_string(path) {
            Ok(body) => match parse_settings(path, &body) {
                Ok(settings) => Ok(settings),
                Err(primary_error) => {
                    let quarantine = quarantine_invalid_settings(path)?;
                    match recover_settings_backup(path, &backup_path) {
                        Ok(settings) => {
                            let _ = fs::remove_file(settings_recovery_marker_path(path));
                            Ok(settings)
                        }
                        Err(backup_error) => {
                            let message = format!(
                                "{primary_error}; invalid primary was moved to {:?}; {backup_error}",
                                quarantine
                            );
                            let _ = atomic_write(
                                &settings_recovery_marker_path(path),
                                message.as_bytes(),
                            );
                            Err(message)
                        }
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let marker = settings_recovery_marker_path(path);
                if marker.exists() {
                    let detail = fs::read_to_string(&marker)
                        .unwrap_or_else(|_| "settings recovery is required".to_string());
                    Err(detail)
                } else if backup_path.exists() {
                    recover_settings_backup(path, &backup_path)
                } else {
                    Ok(Self::default())
                }
            }
            Err(error) => Err(format!("read settings {:?}: {error}", path)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize settings: {error}"))?;
        // A backup is only replaced with JSON that was successfully parsed,
        // so a corrupt primary can never destroy the last known-good copy.
        let backup_path = settings_backup_path(path);
        if let Ok(existing) = fs::read_to_string(path)
            && parse_settings(path, &existing).is_ok()
        {
            atomic_write(&backup_path, existing.as_bytes())?;
        }
        atomic_write(path, &json)?;
        if !backup_path.exists() {
            atomic_write(&backup_path, &json)?;
        }
        let _ = fs::remove_file(settings_recovery_marker_path(path));
        Ok(())
    }
}

/// Thread-safe wrapper that pairs the in-memory settings with the on-disk path. Mutating a
/// field via `update` automatically rewrites the file.
pub struct SettingsStore {
    pub(super) inner: Arc<Mutex<PersistedSettings>>,
    pub(super) path: PathBuf,
    pending: Arc<Mutex<PersistedSettings>>,
    mutation_lock: Arc<Mutex<()>>,
    writer: SettingsWriter,
    secret_namespace: String,
}

struct SettingsWriteJob {
    settings: PersistedSettings,
    debounce: bool,
    reply: mpsc::SyncSender<Result<(), String>>,
}

struct SettingsWriter {
    sender: mpsc::Sender<SettingsWriteJob>,
    #[cfg(test)]
    write_count: Arc<AtomicUsize>,
}

impl SettingsWriter {
    fn new(
        path: PathBuf,
        inner: Arc<Mutex<PersistedSettings>>,
        pending: Arc<Mutex<PersistedSettings>>,
        mutation_lock: Arc<Mutex<()>>,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel::<SettingsWriteJob>();
        #[cfg(test)]
        let write_count = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        let worker_write_count = Arc::clone(&write_count);
        std::thread::Builder::new()
            .name("fozmo-settings-writer".to_string())
            .spawn(move || {
                while let Ok(first) = receiver.recv() {
                    let mut jobs = vec![first];
                    if jobs[0].debounce {
                        let mut deadline = Instant::now() + SETTINGS_DEBOUNCE;
                        while let Some(remaining) = deadline.checked_duration_since(Instant::now())
                        {
                            match receiver.recv_timeout(remaining) {
                                Ok(job) => {
                                    jobs.push(job);
                                    deadline = Instant::now() + SETTINGS_DEBOUNCE;
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => break,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    }

                    let settings = jobs.last().unwrap().settings.clone();
                    #[cfg(test)]
                    worker_write_count.fetch_add(1, Ordering::Relaxed);
                    let result = settings.save(&path);
                    let _mutation = mutation_lock.lock().unwrap();
                    if result.is_ok() {
                        *inner.lock().unwrap() = settings;
                    } else {
                        while let Ok(job) = receiver.try_recv() {
                            jobs.push(job);
                        }
                        *pending.lock().unwrap() = inner.lock().unwrap().clone();
                    }
                    for job in jobs {
                        let _ = job.reply.send(result.clone());
                    }
                }
            })
            .map_err(|error| format!("start settings persistence worker: {error}"))?;
        Ok(Self {
            sender,
            #[cfg(test)]
            write_count,
        })
    }

    fn enqueue(
        &self,
        settings: PersistedSettings,
        debounce: bool,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .send(SettingsWriteJob {
                settings,
                debounce,
                reply,
            })
            .map_err(|_| "settings persistence worker stopped".to_string())?;
        Ok(response)
    }

    fn await_result(response: mpsc::Receiver<Result<(), String>>) -> Result<(), String> {
        response
            .recv()
            .map_err(|_| "settings persistence worker dropped its reply".to_string())?
    }
}

impl SettingsStore {
    #[allow(dead_code)]
    pub fn new(path: PathBuf) -> Self {
        Self::try_new(path).unwrap_or_else(|error| panic!("{error}"))
    }

    #[allow(dead_code)]
    pub fn try_new(path: PathBuf) -> Result<Self, String> {
        let namespace = path_secret_namespace(&path);
        Self::try_new_with_namespace(path, namespace)
    }

    pub fn try_new_with_namespace(
        path: PathBuf,
        secret_namespace: impl Into<String>,
    ) -> Result<Self, String> {
        let mut initial = PersistedSettings::try_load(&path)?;
        if super::profiles::migrate_profile_images(&path, &mut initial)? {
            initial.save(&path)?;
        }
        let inner = Arc::new(Mutex::new(initial.clone()));
        let pending = Arc::new(Mutex::new(initial));
        let mutation_lock = Arc::new(Mutex::new(()));
        let secret_namespace = secret_namespace.into();
        if secret_namespace.trim().is_empty() {
            return Err("settings secret namespace cannot be empty".to_string());
        }
        let writer = SettingsWriter::new(
            path.clone(),
            Arc::clone(&inner),
            Arc::clone(&pending),
            Arc::clone(&mutation_lock),
        )?;
        Ok(Self {
            inner,
            path,
            pending,
            mutation_lock,
            writer,
            secret_namespace,
        })
    }

    pub fn snapshot(&self) -> PersistedSettings {
        self.inner.lock().unwrap().clone()
    }

    /// Workspace-scoped key for all pairing, control, agent, stream, and
    /// remote-session records. The namespace is opaque so local paths are not
    /// exposed in the OS keychain account name.
    pub fn pairing_token_records_secret_key(&self) -> SecretKey {
        SecretKey::pairing_token_records(self.secret_namespace.clone())
    }

    pub fn legacy_pairing_token_records_secret_key(&self) -> SecretKey {
        SecretKey::pairing_token_records(path_secret_namespace(&self.path))
    }

    pub fn profiles(&self) -> Vec<ListeningProfile> {
        self.inner.lock().unwrap().normalized_profiles()
    }

    pub fn active_profile_id(&self) -> String {
        self.inner.lock().unwrap().active_profile_id()
    }

    pub fn playback_for_zone(&self, zone_id: &str) -> ZonePlaybackSettings {
        self.inner.lock().unwrap().playback_for_zone(zone_id)
    }

    pub fn hegel_settings(&self) -> super::HegelSettings {
        self.inner.lock().unwrap().hegel.clone()
    }

    #[cfg(feature = "apple_music_capture")]
    pub fn apple_music_capture_settings(&self) -> AppleMusicCaptureSettings {
        self.inner.lock().unwrap().apple_music_capture.clone()
    }

    pub fn appearance_settings(&self) -> AppearanceSettings {
        self.inner.lock().unwrap().appearance.clone()
    }

    pub fn remote_access_settings(&self) -> super::RemoteAccessSettings {
        self.inner.lock().unwrap().remote_access.clone()
    }

    pub fn qobuz_radio_enabled(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .qobuz_radio_enabled
            .unwrap_or(true)
    }

    pub fn lastfm_radio_enabled(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .lastfm_radio_enabled
            .unwrap_or(false)
    }

    /// Apply a mutation to the persisted settings and write to disk.
    pub fn try_update<F: FnOnce(&mut PersistedSettings)>(&self, mutator: F) -> Result<(), String> {
        self.commit_mutation(|next| {
            mutator(next);
            Ok(())
        })
    }

    pub fn update<F: FnOnce(&mut PersistedSettings)>(&self, mutator: F) -> Result<(), String> {
        self.try_update(mutator)
    }

    pub fn update_playback_for_zone<F: FnOnce(&mut ZonePlaybackSettings)>(
        &self,
        zone_id: &str,
        mutator: F,
    ) -> Result<(), String> {
        self.try_update_playback_for_zone(zone_id, mutator)
    }

    pub fn try_update_playback_for_zone<F: FnOnce(&mut ZonePlaybackSettings)>(
        &self,
        zone_id: &str,
        mutator: F,
    ) -> Result<(), String> {
        self.commit_mutation(|next| {
            let mut playback = next.playback_for_zone(zone_id);
            mutator(&mut playback);
            playback.normalize_names();
            next.zone_settings
                .insert(zone_id.to_string(), playback.clone());
            next.mirror_legacy_playback_fields(&playback);
            Ok(())
        })
    }

    /// Persist an absolute, idempotent playback control after a short quiet
    /// period. Concurrent callers receive the result of the same durable
    /// snapshot, while non-debounced mutations remain immediate.
    pub fn try_update_playback_for_zone_debounced<F: FnOnce(&mut ZonePlaybackSettings)>(
        &self,
        zone_id: &str,
        mutator: F,
    ) -> Result<(), String> {
        self.commit_mutation_with_debounce(true, |next| {
            let mut playback = next.playback_for_zone(zone_id);
            mutator(&mut playback);
            playback.normalize_names();
            next.zone_settings
                .insert(zone_id.to_string(), playback.clone());
            next.mirror_legacy_playback_fields(&playback);
            Ok(())
        })
    }

    pub(super) fn commit_mutation<T>(
        &self,
        mutator: impl FnOnce(&mut PersistedSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        self.commit_mutation_with_debounce(false, mutator)
    }

    fn commit_mutation_with_debounce<T>(
        &self,
        debounce: bool,
        mutator: impl FnOnce(&mut PersistedSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        // Serialize snapshot construction, but release this lock before the
        // worker serializes, fsyncs, and renames. Later mutations build on the
        // pending snapshot and can therefore be safely coalesced.
        let _mutation = self.mutation_lock.lock().unwrap();
        let mut next = self.pending.lock().unwrap().clone();
        let result = mutator(&mut next)?;
        next.normalize_profiles();
        *self.pending.lock().unwrap() = next.clone();
        let response = match self.writer.enqueue(next, debounce) {
            Ok(response) => response,
            Err(error) => {
                *self.pending.lock().unwrap() = self.inner.lock().unwrap().clone();
                return Err(error);
            }
        };
        drop(_mutation);
        SettingsWriter::await_result(response)?;
        Ok(result)
    }

    #[cfg(test)]
    pub(super) fn persisted_write_count(&self) -> usize {
        self.writer.write_count.load(Ordering::Relaxed)
    }

    pub fn migrate_legacy_secrets(&self, secrets: &dyn SecretsStore, pairing_token_ttl_secs: u64) {
        let _mutation = self.mutation_lock.lock().unwrap();
        let mut next = self.pending.lock().unwrap().clone();
        let mut changed = false;

        if let Some(api_key) = normalize_secret(next.lastfm_api_key.as_deref()) {
            match secrets.get(SecretKey::LastFmApiKey) {
                Ok(Some(_)) => {
                    next.lastfm_api_key = None;
                    changed = true;
                }
                Ok(None) => match secrets.put(SecretKey::LastFmApiKey, SecretValue::new(api_key)) {
                    Ok(()) => {
                        next.lastfm_api_key = None;
                        changed = true;
                    }
                    Err(e) => eprintln!("settings: failed to migrate Last.fm API key: {e}"),
                },
                Err(e) => eprintln!("settings: failed to inspect migrated Last.fm API key: {e}"),
            }
        } else if next.lastfm_api_key.is_some() {
            next.lastfm_api_key = None;
            changed = true;
        }

        let legacy_raw_tokens = next.pairing_tokens.clone().unwrap_or_default();
        let legacy_records = next.pairing_token_records.clone();
        if !legacy_raw_tokens.is_empty() || !legacy_records.is_empty() {
            match secrets.get(self.pairing_token_records_secret_key()) {
                Ok(Some(_)) => {
                    next.pairing_tokens = None;
                    next.pairing_token_records.clear();
                    changed = true;
                }
                Ok(None) => {
                    let records = migrated_pairing_records(
                        legacy_records,
                        legacy_raw_tokens,
                        pairing_token_ttl_secs,
                    );
                    match serde_json::to_string_pretty(&records)
                        .map_err(|e| e.to_string())
                        .and_then(|json| {
                            secrets
                                .put(
                                    self.pairing_token_records_secret_key(),
                                    SecretValue::new(json),
                                )
                                .map_err(|e| e.to_string())
                        }) {
                        Ok(()) => {
                            next.pairing_tokens = None;
                            next.pairing_token_records.clear();
                            changed = true;
                        }
                        Err(e) => eprintln!("settings: failed to migrate pairing records: {e}"),
                    }
                }
                Err(e) => eprintln!("settings: failed to inspect migrated pairing records: {e}"),
            }
        } else if next.pairing_tokens.is_some() {
            next.pairing_tokens = None;
            changed = true;
        }

        if changed {
            next.normalize_profiles();
            *self.pending.lock().unwrap() = next.clone();
            match self.writer.enqueue(next, false) {
                Ok(response) => {
                    drop(_mutation);
                    if let Err(error) = SettingsWriter::await_result(response) {
                        eprintln!("settings: secret migration was not persisted: {error}");
                    }
                }
                Err(error) => {
                    *self.pending.lock().unwrap() = self.inner.lock().unwrap().clone();
                    eprintln!("settings: secret migration was not persisted: {error}");
                }
            }
        }
    }
}

pub(crate) fn path_secret_namespace(path: &Path) -> String {
    let stable_path = path.canonicalize().unwrap_or_else(|_| {
        let file_name = path.file_name().map(PathBuf::from);
        if let (Some(parent), Some(file_name)) = (path.parent(), file_name)
            && let Ok(parent) = parent.canonicalize()
        {
            return parent.join(file_name);
        }
        if path.is_relative()
            && let Ok(current_dir) = std::env::current_dir()
        {
            return current_dir.join(path);
        }
        path.to_path_buf()
    });
    URL_SAFE_NO_PAD.encode(Sha256::digest(stable_path.to_string_lossy().as_bytes()))
}

pub(crate) fn settings_backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    path.with_file_name(format!("{file_name}.bak"))
}

fn settings_recovery_marker_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    path.with_file_name(format!("{file_name}.recovery-required"))
}

fn recover_settings_backup(path: &Path, backup_path: &Path) -> Result<PersistedSettings, String> {
    let body = fs::read_to_string(backup_path)
        .map_err(|error| format!("read settings backup {:?}: {error}", backup_path))?;
    let settings = parse_settings(backup_path, &body)
        .map_err(|error| format!("settings backup is invalid: {error}"))?;
    atomic_write(path, body.as_bytes())?;
    Ok(settings)
}

fn quarantine_invalid_settings(path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let quarantine = path.with_file_name(format!("{file_name}.corrupt-{suffix}"));
    fs::rename(path, &quarantine)
        .map_err(|error| format!("quarantine invalid settings {:?}: {error}", path))?;
    Ok(quarantine)
}

fn normalize_secret(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn migrated_pairing_records(
    mut records: Vec<super::PairingTokenRecord>,
    raw_tokens: Vec<String>,
    token_ttl_secs: u64,
) -> Vec<super::PairingTokenRecord> {
    let now = now_unix_secs();
    let expires_at = now.saturating_add(token_ttl_secs.max(1));
    let existing_hashes = records
        .iter()
        .map(|record| record.token_hash.clone())
        .collect::<Vec<_>>();
    for token in raw_tokens
        .iter()
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
    {
        let hash = token_hash(token);
        if existing_hashes
            .iter()
            .any(|known| constant_time_eq(known.as_bytes(), hash.as_bytes()))
            || records
                .iter()
                .any(|known| constant_time_eq(known.token_hash.as_bytes(), hash.as_bytes()))
        {
            continue;
        }
        records.push(super::PairingTokenRecord {
            id: random_url_token(16),
            kind: super::AuthTokenKind::LegacyToken,
            token_hash: hash,
            scopes: vec![
                "control".to_string(),
                "agent:connect".to_string(),
                "stream:read".to_string(),
            ],
            subject: None,
            label: Some("Migrated pairing token".to_string()),
            issued_at_unix_secs: now,
            expires_at_unix_secs: expires_at,
            last_used_at_unix_secs: None,
            rotated_at_unix_secs: None,
            revoked_at_unix_secs: None,
            binding: None,
            remote_session_metadata: None,
        });
    }
    records
}

fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

fn random_url_token(bytes_len: usize) -> String {
    let mut bytes = vec![0_u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for i in 0..max_len {
        let l = left.get(i).copied().unwrap_or(0);
        let r = right.get(i).copied().unwrap_or(0);
        diff |= (l ^ r) as usize;
    }
    diff == 0
}
