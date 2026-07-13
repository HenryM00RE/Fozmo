//! Secret storage behind a single OS keychain item.
//!
//! All secrets live in one JSON bundle under a single keychain entry so the
//! platform (macOS in particular) raises at most one access prompt per app
//! identity, instead of one per secret. [`SecretsStore::warm_up`] loads the
//! bundle at a predictable time during boot and folds any per-secret items
//! from older versions into the bundle the first time it is created.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

const KEYRING_SERVICE: &str = "com.fozmo.secrets";
/// Single keychain item holding every secret as a JSON object keyed by
/// [`SecretKey::account`] strings.
const BUNDLE_ACCOUNT: &str = "secrets-bundle-v1";

pub trait SecretsStore: Send + Sync {
    fn get(&self, key: SecretKey) -> Result<Option<SecretValue>, SecretError>;
    fn put(&self, key: SecretKey, value: SecretValue) -> Result<(), SecretError>;
    fn delete(&self, key: SecretKey) -> Result<(), SecretError>;

    /// Eagerly loads the backing store so any interactive unlock (the macOS
    /// keychain prompt) happens now, at a predictable time, instead of when a
    /// feature first needs a secret. `legacy_keys` names the per-secret
    /// keychain items of older app versions to fold into consolidated storage
    /// the first time it is created. A dismissed prompt surfaces as
    /// [`SecretError::UserCancelled`] and the next access retries.
    fn warm_up(&self, _legacy_keys: &[SecretKey]) -> Result<(), SecretError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretKey {
    LastFmApiKey,
    QobuzSession {
        account: String,
    },
    PairingTokenRecords {
        namespace: String,
    },
    /// Pre-workspace-scoping account. Read only during keychain migration and
    /// never used for authentication.
    LegacyGlobalPairingTokenRecords,
    RemoteTlsKey {
        installation_id: String,
    },
    /// Pre-installation-scoping TLS account. Read only during migration and
    /// never used to start a new remote listener.
    LegacyRemoteTlsKey,
}

impl SecretKey {
    pub fn pairing_token_records(namespace: impl Into<String>) -> Self {
        Self::PairingTokenRecords {
            namespace: namespace.into(),
        }
    }

    pub fn account(&self) -> String {
        match self {
            Self::LastFmApiKey => "lastfm-api-key".to_string(),
            Self::QobuzSession { account } => format!("qobuz-session-{account}"),
            Self::PairingTokenRecords { namespace } => {
                format!("pairing-token-records-{namespace}")
            }
            Self::LegacyGlobalPairingTokenRecords => "pairing-token-records".to_string(),
            Self::RemoteTlsKey { installation_id } => {
                format!("remote-tls-key-{installation_id}")
            }
            Self::LegacyRemoteTlsKey => "remote-tls-key".to_string(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue([redacted])")
    }
}

#[derive(Debug, Clone)]
pub enum SecretError {
    Keyring(String),
    /// The user dismissed the OS keychain prompt. Deliberately distinct from
    /// [`Self::Keyring`]: a dismissal is not cached, so the next access can
    /// retry once someone is at the machine.
    UserCancelled(String),
    Serialization(String),
    #[cfg(test)]
    Unavailable(String),
    #[cfg(feature = "dev-secrets-file")]
    Io(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keyring(message) => write!(f, "keyring error: {message}"),
            Self::UserCancelled(message) => write!(f, "keychain access cancelled: {message}"),
            Self::Serialization(message) => write!(f, "secret serialization error: {message}"),
            #[cfg(test)]
            Self::Unavailable(message) => write!(f, "secret storage unavailable: {message}"),
            #[cfg(feature = "dev-secrets-file")]
            Self::Io(message) => write!(f, "secret storage I/O error: {message}"),
        }
    }
}

impl std::error::Error for SecretError {}

impl From<serde_json::Error> for SecretError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}

pub struct KeyringSecretsStore {
    backend: Arc<dyn KeyringBackend>,
    state: Mutex<BundleState>,
}

struct BundleState {
    load: BundleLoad,
    /// Legacy per-secret keychain items to fold into the bundle when it is
    /// first created; remembered from `warm_up` so a cancelled load can retry
    /// the migration on the next access.
    migration_keys: Vec<SecretKey>,
}

enum BundleLoad {
    Unloaded,
    Loaded(HashMap<String, String>),
    Failed(SecretError),
}

/// Raw keychain access by account name; the store layers bundling, caching
/// and legacy migration on top.
trait KeyringBackend: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn put(&self, account: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

struct AppleKeyringBackend;

/// macOS reports a dismissed keychain prompt as errSecUserCanceled; the
/// keyring crate only surfaces the human-readable message, so match on it.
fn map_keyring_error(error: keyring::Error) -> SecretError {
    let message = error.to_string();
    if message.contains("User canceled") || message.contains("errSecUserCanceled") {
        SecretError::UserCancelled(message)
    } else {
        SecretError::Keyring(message)
    }
}

impl AppleKeyringBackend {
    fn entry(account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(KEYRING_SERVICE, account).map_err(map_keyring_error)
    }
}

impl KeyringBackend for AppleKeyringBackend {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        match Self::entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring_error(e)),
        }
    }

    fn put(&self, account: &str, value: &str) -> Result<(), SecretError> {
        Self::entry(account)?
            .set_password(value)
            .map_err(map_keyring_error)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_keyring_error(e)),
        }
    }
}

impl KeyringSecretsStore {
    pub fn new() -> Self {
        Self::with_backend(Arc::new(AppleKeyringBackend))
    }

    fn with_backend<B>(backend: Arc<B>) -> Self
    where
        B: KeyringBackend + 'static,
    {
        Self {
            backend,
            state: Mutex::new(BundleState {
                load: BundleLoad::Unloaded,
                migration_keys: Vec::new(),
            }),
        }
    }

    /// Loads the bundle if needed. Holding the state lock across the backend
    /// call is deliberate: concurrent first accesses must not each raise a
    /// keychain prompt.
    fn ensure_loaded<'a>(
        &self,
        state: &'a mut BundleState,
    ) -> Result<&'a mut HashMap<String, String>, SecretError> {
        if let BundleLoad::Unloaded = state.load {
            match self.load_bundle(&state.migration_keys) {
                Ok(bundle) => state.load = BundleLoad::Loaded(bundle),
                Err(error) => {
                    // A dismissed prompt stays Unloaded so the next access
                    // retries once someone is at the machine; other failures
                    // are cached to avoid prompt loops.
                    if !matches!(error, SecretError::UserCancelled(_)) {
                        state.load = BundleLoad::Failed(error.clone());
                    }
                    return Err(error);
                }
            }
        }
        match &mut state.load {
            BundleLoad::Loaded(bundle) => Ok(bundle),
            BundleLoad::Failed(error) => Err(error.clone()),
            BundleLoad::Unloaded => unreachable!("bundle load handled above"),
        }
    }

    fn load_bundle(
        &self,
        migration_keys: &[SecretKey],
    ) -> Result<HashMap<String, String>, SecretError> {
        match self.backend.get(BUNDLE_ACCOUNT)? {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => self.migrate_legacy_items(migration_keys),
        }
    }

    /// Folds pre-bundle per-secret keychain items into a fresh bundle. Legacy
    /// items are only deleted after the bundle write succeeds; any read or
    /// write failure aborts with the legacy items untouched.
    fn migrate_legacy_items(
        &self,
        keys: &[SecretKey],
    ) -> Result<HashMap<String, String>, SecretError> {
        let mut bundle = HashMap::new();
        for key in keys {
            let account = key.account();
            if let Some(value) = self.backend.get(&account)? {
                bundle.insert(account, value);
            }
        }
        self.write_bundle(&bundle)?;
        for account in bundle.keys() {
            if let Err(error) = self.backend.delete(account) {
                tracing::warn!(
                    event = "secrets_migration",
                    status = "warning",
                    account,
                    "failed to remove migrated keychain item: {error}"
                );
            }
        }
        Ok(bundle)
    }

    fn write_bundle(&self, bundle: &HashMap<String, String>) -> Result<(), SecretError> {
        let json = serde_json::to_string(bundle)?;
        self.backend.put(BUNDLE_ACCOUNT, &json)
    }
}

impl Default for KeyringSecretsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretsStore for KeyringSecretsStore {
    fn get(&self, key: SecretKey) -> Result<Option<SecretValue>, SecretError> {
        let mut state = self.state.lock().unwrap();
        let bundle = self.ensure_loaded(&mut state)?;
        Ok(bundle
            .get(&key.account())
            .map(|value| SecretValue::new(value.clone())))
    }

    fn put(&self, key: SecretKey, value: SecretValue) -> Result<(), SecretError> {
        let mut state = self.state.lock().unwrap();
        let bundle = self.ensure_loaded(&mut state)?;
        let account = key.account();
        let previous = bundle.insert(account.clone(), value.expose_secret().to_string());
        if let Err(error) = self.write_bundle(bundle) {
            // Keep the in-memory bundle consistent with the keychain.
            match previous {
                Some(previous) => bundle.insert(account, previous),
                None => bundle.remove(&account),
            };
            return Err(error);
        }
        Ok(())
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretError> {
        let mut state = self.state.lock().unwrap();
        let bundle = self.ensure_loaded(&mut state)?;
        let account = key.account();
        let Some(previous) = bundle.remove(&account) else {
            return Ok(());
        };
        if let Err(error) = self.write_bundle(bundle) {
            bundle.insert(account, previous);
            return Err(error);
        }
        Ok(())
    }

    fn warm_up(&self, legacy_keys: &[SecretKey]) -> Result<(), SecretError> {
        let mut state = self.state.lock().unwrap();
        state.migration_keys = legacy_keys.to_vec();
        self.ensure_loaded(&mut state).map(|_| ())
    }
}

/// Process-local secret storage for the packaged release startup smoke.
///
/// This is intentionally crate-private and selected only after runtime proves
/// that `--release-smoke` is using a packaged helper and disposable roots. It
/// prevents release verification from reading or modifying the user's Keychain.
#[derive(Default)]
pub(crate) struct EphemeralSecretsStore {
    values: Mutex<HashMap<String, SecretValue>>,
}

impl EphemeralSecretsStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl SecretsStore for EphemeralSecretsStore {
    fn get(&self, key: SecretKey) -> Result<Option<SecretValue>, SecretError> {
        Ok(self.values.lock().unwrap().get(&key.account()).cloned())
    }

    fn put(&self, key: SecretKey, value: SecretValue) -> Result<(), SecretError> {
        self.values.lock().unwrap().insert(key.account(), value);
        Ok(())
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretError> {
        self.values.lock().unwrap().remove(&key.account());
        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemorySecretsStore {
    values: Mutex<HashMap<String, SecretValue>>,
}

#[cfg(test)]
impl MemorySecretsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl SecretsStore for MemorySecretsStore {
    fn get(&self, key: SecretKey) -> Result<Option<SecretValue>, SecretError> {
        Ok(self.values.lock().unwrap().get(&key.account()).cloned())
    }

    fn put(&self, key: SecretKey, value: SecretValue) -> Result<(), SecretError> {
        self.values.lock().unwrap().insert(key.account(), value);
        Ok(())
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretError> {
        self.values.lock().unwrap().remove(&key.account());
        Ok(())
    }
}

#[cfg(feature = "dev-secrets-file")]
pub struct DevFileSecretsStore {
    path: std::path::PathBuf,
    values: Mutex<HashMap<String, SecretValue>>,
}

#[cfg(feature = "dev-secrets-file")]
impl DevFileSecretsStore {
    pub fn new(path: std::path::PathBuf) -> Self {
        let values = std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str::<HashMap<String, String>>(&body).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| (key, SecretValue::new(value)))
            .collect();
        Self {
            path,
            values: Mutex::new(values),
        }
    }

    fn save(&self, values: &HashMap<String, SecretValue>) -> Result<(), SecretError> {
        let plain = values
            .iter()
            .map(|(key, value)| (key.clone(), value.expose_secret().to_string()))
            .collect::<HashMap<_, _>>();
        let json = serde_json::to_string_pretty(&plain)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SecretError::Io(e.to_string()))?;
        }
        std::fs::write(&self.path, json).map_err(|e| SecretError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[cfg(feature = "dev-secrets-file")]
impl SecretsStore for DevFileSecretsStore {
    fn get(&self, key: SecretKey) -> Result<Option<SecretValue>, SecretError> {
        Ok(self.values.lock().unwrap().get(&key.account()).cloned())
    }

    fn put(&self, key: SecretKey, value: SecretValue) -> Result<(), SecretError> {
        let mut values = self.values.lock().unwrap();
        values.insert(key.account(), value);
        self.save(&values)
    }

    fn delete(&self, key: SecretKey) -> Result<(), SecretError> {
        let mut values = self.values.lock().unwrap();
        values.remove(&key.account());
        self.save(&values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct TestKeyringBackend {
        values: Mutex<HashMap<String, String>>,
        get_calls: AtomicUsize,
        put_calls: AtomicUsize,
        delete_calls: AtomicUsize,
        get_errors: Mutex<HashMap<String, SecretError>>,
        put_error: Mutex<Option<SecretError>>,
    }

    impl TestKeyringBackend {
        fn seed(&self, account: impl Into<String>, value: impl Into<String>) {
            self.values
                .lock()
                .unwrap()
                .insert(account.into(), value.into());
        }

        fn fail_get(&self, account: impl Into<String>, error: SecretError) {
            self.get_errors
                .lock()
                .unwrap()
                .insert(account.into(), error);
        }

        fn clear_get_failures(&self) {
            self.get_errors.lock().unwrap().clear();
        }

        fn fail_puts(&self, error: SecretError) {
            *self.put_error.lock().unwrap() = Some(error);
        }

        fn clear_put_failures(&self) {
            *self.put_error.lock().unwrap() = None;
        }

        fn raw_value(&self, account: &str) -> Option<String> {
            self.values.lock().unwrap().get(account).cloned()
        }
    }

    impl KeyringBackend for TestKeyringBackend {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            self.get_calls.fetch_add(1, Ordering::Relaxed);
            if let Some(error) = self.get_errors.lock().unwrap().get(account).cloned() {
                return Err(error);
            }
            Ok(self.values.lock().unwrap().get(account).cloned())
        }

        fn put(&self, account: &str, value: &str) -> Result<(), SecretError> {
            self.put_calls.fetch_add(1, Ordering::Relaxed);
            if let Some(error) = self.put_error.lock().unwrap().clone() {
                return Err(error);
            }
            self.values
                .lock()
                .unwrap()
                .insert(account.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.delete_calls.fetch_add(1, Ordering::Relaxed);
            self.values.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[test]
    fn memory_store_round_trips_and_deletes_values() {
        let store = MemorySecretsStore::new();
        let key = SecretKey::LastFmApiKey;

        assert!(store.get(key.clone()).unwrap().is_none());
        store
            .put(key.clone(), SecretValue::new("secret-value"))
            .unwrap();

        assert_eq!(
            store.get(key.clone()).unwrap().unwrap().expose_secret(),
            "secret-value"
        );

        store.delete(key.clone()).unwrap();
        assert!(store.get(key).unwrap().is_none());
    }

    #[test]
    fn bundle_round_trips_put_get_delete_in_single_item() {
        let backend = Arc::new(TestKeyringBackend::default());
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        store
            .put(SecretKey::LastFmApiKey, SecretValue::new("secret-value"))
            .unwrap();
        assert_eq!(
            store
                .get(SecretKey::LastFmApiKey)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "secret-value"
        );
        // Everything lives in the single bundle item; no per-secret items.
        assert!(
            backend
                .raw_value(BUNDLE_ACCOUNT)
                .unwrap()
                .contains("secret-value")
        );
        assert!(backend.raw_value("lastfm-api-key").is_none());

        store.delete(SecretKey::LastFmApiKey).unwrap();
        assert!(store.get(SecretKey::LastFmApiKey).unwrap().is_none());
        assert_eq!(backend.raw_value(BUNDLE_ACCOUNT).unwrap(), "{}");
    }

    #[test]
    fn all_secrets_share_one_keychain_read() {
        let backend = Arc::new(TestKeyringBackend::default());
        backend.seed(
            BUNDLE_ACCOUNT,
            r#"{"lastfm-api-key":"lastfm-secret","remote-tls-key-install-123":"tls-secret"}"#,
        );
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        for _ in 0..2 {
            assert_eq!(
                store
                    .get(SecretKey::LastFmApiKey)
                    .unwrap()
                    .unwrap()
                    .expose_secret(),
                "lastfm-secret"
            );
            assert_eq!(
                store
                    .get(SecretKey::RemoteTlsKey {
                        installation_id: "install-123".to_string(),
                    })
                    .unwrap()
                    .unwrap()
                    .expose_secret(),
                "tls-secret"
            );
            assert!(
                store
                    .get(SecretKey::pairing_token_records("workspace"))
                    .unwrap()
                    .is_none()
            );
        }

        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn warm_up_migrates_legacy_items_and_removes_them() {
        let backend = Arc::new(TestKeyringBackend::default());
        backend.seed("lastfm-api-key", "legacy-lastfm");
        backend.seed("remote-tls-key", "legacy-tls");
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        store
            .warm_up(&[
                SecretKey::LastFmApiKey,
                SecretKey::LegacyGlobalPairingTokenRecords,
                SecretKey::LegacyRemoteTlsKey,
                SecretKey::QobuzSession {
                    account: "abc123".to_string(),
                },
            ])
            .unwrap();

        assert_eq!(
            store
                .get(SecretKey::LastFmApiKey)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "legacy-lastfm"
        );
        assert_eq!(
            store
                .get(SecretKey::LegacyRemoteTlsKey)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "legacy-tls"
        );
        assert!(backend.raw_value("lastfm-api-key").is_none());
        assert!(backend.raw_value("remote-tls-key").is_none());
        let bundle: HashMap<String, String> =
            serde_json::from_str(&backend.raw_value(BUNDLE_ACCOUNT).unwrap()).unwrap();
        assert_eq!(bundle.len(), 2);
    }

    #[test]
    fn warm_up_on_fresh_install_creates_empty_bundle() {
        let backend = Arc::new(TestKeyringBackend::default());
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        store
            .warm_up(&[SecretKey::LastFmApiKey, SecretKey::LegacyRemoteTlsKey])
            .unwrap();

        assert_eq!(backend.raw_value(BUNDLE_ACCOUNT).unwrap(), "{}");
        assert_eq!(backend.put_calls.load(Ordering::Relaxed), 1);
        let reads_after_warm_up = backend.get_calls.load(Ordering::Relaxed);
        assert!(store.get(SecretKey::LastFmApiKey).unwrap().is_none());
        assert_eq!(
            backend.get_calls.load(Ordering::Relaxed),
            reads_after_warm_up
        );
    }

    #[test]
    fn cancelled_bundle_read_is_not_cached_and_retries_migration() {
        let backend = Arc::new(TestKeyringBackend::default());
        backend.seed("lastfm-api-key", "legacy-lastfm");
        backend.fail_get(
            BUNDLE_ACCOUNT,
            SecretError::UserCancelled("User canceled the operation.".to_string()),
        );
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        let error = store.warm_up(&[SecretKey::LastFmApiKey]).unwrap_err();
        assert!(matches!(error, SecretError::UserCancelled(_)));
        // Nothing migrated or deleted while the prompt was dismissed.
        assert!(backend.raw_value("lastfm-api-key").is_some());

        // The next access retries the full load, including the migration.
        backend.clear_get_failures();
        assert_eq!(
            store
                .get(SecretKey::LastFmApiKey)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "legacy-lastfm"
        );
        assert!(backend.raw_value("lastfm-api-key").is_none());
    }

    #[test]
    fn cancelled_legacy_read_aborts_migration_with_items_intact() {
        let backend = Arc::new(TestKeyringBackend::default());
        backend.seed("remote-tls-key", "legacy-tls");
        backend.fail_get(
            "remote-tls-key",
            SecretError::UserCancelled("User canceled the operation.".to_string()),
        );
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        let error = store.warm_up(&[SecretKey::LegacyRemoteTlsKey]).unwrap_err();
        assert!(matches!(error, SecretError::UserCancelled(_)));
        assert_eq!(backend.put_calls.load(Ordering::Relaxed), 0);
        assert_eq!(backend.delete_calls.load(Ordering::Relaxed), 0);

        backend.clear_get_failures();
        store.warm_up(&[SecretKey::LegacyRemoteTlsKey]).unwrap();
        assert_eq!(
            store
                .get(SecretKey::LegacyRemoteTlsKey)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "legacy-tls"
        );
        assert!(backend.raw_value("remote-tls-key").is_none());
    }

    #[test]
    fn non_cancelled_read_errors_are_cached_to_avoid_prompt_loops() {
        let backend = Arc::new(TestKeyringBackend::default());
        backend.fail_get(BUNDLE_ACCOUNT, SecretError::Keyring("locked".to_string()));
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        assert!(store.get(SecretKey::LastFmApiKey).is_err());
        assert!(store.get(SecretKey::LastFmApiKey).is_err());

        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_bundle_write_rolls_back_in_memory_state() {
        let backend = Arc::new(TestKeyringBackend::default());
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));
        store.warm_up(&[]).unwrap();

        backend.fail_puts(SecretError::Keyring("write failed".to_string()));
        assert!(
            store
                .put(SecretKey::LastFmApiKey, SecretValue::new("new-secret"))
                .is_err()
        );
        backend.clear_put_failures();
        // The failed put must not linger in memory as if it were stored.
        assert!(store.get(SecretKey::LastFmApiKey).unwrap().is_none());

        store
            .put(SecretKey::LastFmApiKey, SecretValue::new("stored-secret"))
            .unwrap();
        backend.fail_puts(SecretError::Keyring("write failed".to_string()));
        assert!(store.delete(SecretKey::LastFmApiKey).is_err());
        backend.clear_put_failures();
        // The failed delete must keep the value visible.
        assert_eq!(
            store
                .get(SecretKey::LastFmApiKey)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "stored-secret"
        );
    }

    #[test]
    fn put_does_not_clobber_bundle_when_load_fails() {
        let backend = Arc::new(TestKeyringBackend::default());
        backend.fail_get(BUNDLE_ACCOUNT, SecretError::Keyring("locked".to_string()));
        let store = KeyringSecretsStore::with_backend(Arc::clone(&backend));

        assert!(
            store
                .put(SecretKey::LastFmApiKey, SecretValue::new("new-secret"))
                .is_err()
        );
        assert_eq!(backend.put_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn secret_key_accounts_are_stable() {
        assert_eq!(SecretKey::LastFmApiKey.account(), "lastfm-api-key");
        assert_eq!(
            SecretKey::QobuzSession {
                account: "abc123".to_string()
            }
            .account(),
            "qobuz-session-abc123"
        );
        assert_eq!(
            SecretKey::pairing_token_records("workspace").account(),
            "pairing-token-records-workspace"
        );
        assert_eq!(
            SecretKey::LegacyGlobalPairingTokenRecords.account(),
            "pairing-token-records"
        );
        assert_eq!(
            SecretKey::RemoteTlsKey {
                installation_id: "install-123".to_string()
            }
            .account(),
            "remote-tls-key-install-123"
        );
        assert_eq!(SecretKey::LegacyRemoteTlsKey.account(), "remote-tls-key");
    }

    #[test]
    fn secret_value_debug_is_redacted() {
        assert_eq!(
            format!("{:?}", SecretValue::new("secret")),
            "SecretValue([redacted])"
        );
    }
}
