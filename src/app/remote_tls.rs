//! TLS identity for the remote access listener.
//!
//! Either loads a user-supplied certificate/key pair or generates a
//! self-signed ECDSA P-256 certificate whose private key is persisted in the
//! secrets store. If the secrets store cannot persist the key, remote start
//! fails closed instead of generating an in-memory key that would silently
//! rotate on every boot.

use crate::secrets::{SecretKey, SecretValue, SecretsStore};
use crate::settings::RemoteAccessSettings;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::Path;

pub const REMOTE_CERT_FILENAME: &str = "remote-cert.pem";
const GENERATED_SAN: &str = "fozmo.remote";
const GENERATED_VALIDITY_DAYS: i64 = 3650;

#[derive(Clone)]
pub struct RemoteTlsIdentity {
    pub cert_pem: String,
    pub key_pem: String,
    pub fingerprint_sha256: String,
    /// True when the identity came from user-supplied cert/key paths.
    pub custom: bool,
}

impl fmt::Debug for RemoteTlsIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteTlsIdentity")
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("custom", &self.custom)
            .field("key_pem", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTlsError {
    SecretStore(String),
    Io(String),
    InvalidCertificate(String),
    InvalidKey(String),
    Generation(String),
}

impl fmt::Display for RemoteTlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SecretStore(message) => {
                write!(f, "remote TLS key storage unavailable: {message}")
            }
            Self::Io(message) => write!(f, "remote TLS certificate I/O error: {message}"),
            Self::InvalidCertificate(message) => {
                write!(f, "invalid remote TLS certificate: {message}")
            }
            Self::InvalidKey(message) => write!(f, "invalid remote TLS private key: {message}"),
            Self::Generation(message) => {
                write!(f, "remote TLS certificate generation failed: {message}")
            }
        }
    }
}

impl std::error::Error for RemoteTlsError {}

pub fn load_or_generate(
    tls_dir: &Path,
    secrets: &dyn SecretsStore,
    settings: &RemoteAccessSettings,
    installation_id: &str,
) -> Result<RemoteTlsIdentity, RemoteTlsError> {
    match (
        settings.custom_cert_path.as_deref(),
        settings.custom_key_path.as_deref(),
    ) {
        (Some(cert_path), Some(key_path)) => return load_custom(cert_path, key_path),
        (Some(_), None) | (None, Some(_)) => {
            return Err(RemoteTlsError::InvalidKey(
                "custom_cert_path and custom_key_path must be configured together".to_string(),
            ));
        }
        (None, None) => {}
    }
    load_or_generate_self_signed(tls_dir, secrets, installation_id)
}

fn load_custom(cert_path: &str, key_path: &str) -> Result<RemoteTlsIdentity, RemoteTlsError> {
    let cert_pem = std::fs::read_to_string(cert_path)
        .map_err(|e| RemoteTlsError::Io(format!("failed to read {cert_path}: {e}")))?;
    let key_pem = std::fs::read_to_string(key_path)
        .map_err(|e| RemoteTlsError::Io(format!("failed to read {key_path}: {e}")))?;
    rustls_pki_types::PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|e| RemoteTlsError::InvalidKey(format!("{key_path}: {e:?}")))?;
    let fingerprint_sha256 = sha256_fingerprint(&cert_pem)?;
    Ok(RemoteTlsIdentity {
        cert_pem,
        key_pem,
        fingerprint_sha256,
        custom: true,
    })
}

fn load_or_generate_self_signed(
    tls_dir: &Path,
    secrets: &dyn SecretsStore,
    installation_id: &str,
) -> Result<RemoteTlsIdentity, RemoteTlsError> {
    let cert_path = tls_dir.join(REMOTE_CERT_FILENAME);
    let secret_key = SecretKey::RemoteTlsKey {
        installation_id: installation_id.to_string(),
    };
    let stored_key = secrets
        .get(secret_key.clone())
        .map_err(|e| RemoteTlsError::SecretStore(e.to_string()))?;

    if let Some(stored_key) = stored_key {
        let key_pem = stored_key.expose_secret().to_string();
        let key_pair = rcgen::KeyPair::from_pem(&key_pem)
            .map_err(|e| RemoteTlsError::InvalidKey(format!("stored remote TLS key: {e}")))?;
        let cert_pem = match std::fs::read_to_string(&cert_path) {
            Ok(cert_pem)
                if sha256_fingerprint(&cert_pem).is_ok()
                    && certificate_matches_key(&cert_pem, &key_pem) =>
            {
                cert_pem
            }
            // The public cert is derivable from the persisted key, so a
            // missing, corrupt, or mismatched cert file is recreated without
            // rotating the key.
            _ => write_generated_cert(&cert_path, &key_pair)?,
        };
        let fingerprint_sha256 = sha256_fingerprint(&cert_pem)?;
        return Ok(RemoteTlsIdentity {
            cert_pem,
            key_pem,
            fingerprint_sha256,
            custom: false,
        });
    }

    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| RemoteTlsError::Generation(format!("key generation: {e}")))?;
    let key_pem = key_pair.serialize_pem();
    // Persist the key before exposing the identity: if this fails the remote
    // listener must stay stopped rather than serve a key that rotates on boot.
    secrets
        .put(secret_key, SecretValue::new(key_pem.clone()))
        .map_err(|e| RemoteTlsError::SecretStore(e.to_string()))?;
    let cert_pem = write_generated_cert(&cert_path, &key_pair)?;
    let fingerprint_sha256 = sha256_fingerprint(&cert_pem)?;
    Ok(RemoteTlsIdentity {
        cert_pem,
        key_pem,
        fingerprint_sha256,
        custom: false,
    })
}

fn certificate_matches_key(cert_pem: &str, key_pem: &str) -> bool {
    let Ok(certs) =
        CertificateDer::pem_slice_iter(cert_pem.as_bytes()).collect::<Result<Vec<_>, _>>()
    else {
        return false;
    };
    let Ok(key) = rustls_pki_types::PrivateKeyDer::from_pem_slice(key_pem.as_bytes()) else {
        return false;
    };
    if certs.is_empty() {
        return false;
    }

    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_no_client_auth().with_single_cert(certs, key))
        .is_ok()
}

fn write_generated_cert(
    cert_path: &Path,
    key_pair: &rcgen::KeyPair,
) -> Result<String, RemoteTlsError> {
    let mut params = rcgen::CertificateParams::new(vec![GENERATED_SAN.to_string()])
        .map_err(|e| RemoteTlsError::Generation(format!("certificate params: {e}")))?;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(GENERATED_VALIDITY_DAYS);
    let cert = params
        .self_signed(key_pair)
        .map_err(|e| RemoteTlsError::Generation(format!("self-signed certificate: {e}")))?;
    let cert_pem = cert.pem();
    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| RemoteTlsError::Io(format!("failed to create {parent:?}: {e}")))?;
    }
    std::fs::write(cert_path, &cert_pem)
        .map_err(|e| RemoteTlsError::Io(format!("failed to write {cert_path:?}: {e}")))?;
    Ok(cert_pem)
}

/// Uppercase colon-separated SHA-256 fingerprint of the first certificate in
/// the PEM body; stable and copyable for out-of-band verification.
pub fn sha256_fingerprint(cert_pem: &str) -> Result<String, RemoteTlsError> {
    let cert = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .map_err(|e| RemoteTlsError::InvalidCertificate(format!("{e:?}")))?;
    let digest = Sha256::digest(cert.as_ref());
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{MemorySecretsStore, SecretError};
    use std::path::PathBuf;
    use std::sync::Arc;

    const TEST_INSTALLATION_ID: &str = "00000000-0000-4000-8000-000000000001";

    fn load_test(
        tls_dir: &Path,
        secrets: &dyn SecretsStore,
        settings: &RemoteAccessSettings,
    ) -> Result<RemoteTlsIdentity, RemoteTlsError> {
        load_or_generate(tls_dir, secrets, settings, TEST_INSTALLATION_ID)
    }

    fn temp_tls_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fozmo-remote-tls-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    struct FailingSecretsStore;

    impl SecretsStore for FailingSecretsStore {
        fn get(&self, _key: SecretKey) -> Result<Option<SecretValue>, SecretError> {
            Err(SecretError::Keyring("locked".to_string()))
        }

        fn put(&self, _key: SecretKey, _value: SecretValue) -> Result<(), SecretError> {
            Err(SecretError::Keyring("locked".to_string()))
        }

        fn delete(&self, _key: SecretKey) -> Result<(), SecretError> {
            Err(SecretError::Keyring("locked".to_string()))
        }
    }

    #[test]
    fn generated_identity_is_reused_across_restarts() {
        let tls_dir = temp_tls_dir("reuse");
        let secrets = Arc::new(MemorySecretsStore::new());
        let settings = RemoteAccessSettings::default();

        let first = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap();
        let second = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap();

        assert!(!first.custom);
        assert_eq!(first.fingerprint_sha256, second.fingerprint_sha256);
        assert_eq!(first.key_pem, second.key_pem);
        assert_eq!(first.cert_pem, second.cert_pem);
        assert!(tls_dir.join(REMOTE_CERT_FILENAME).exists());
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    #[test]
    fn generated_keys_are_isolated_by_installation_id() {
        let root = temp_tls_dir("scoped");
        let secrets = Arc::new(MemorySecretsStore::new());
        let settings = RemoteAccessSettings::default();
        let first_id = "00000000-0000-4000-8000-000000000001";
        let second_id = "00000000-0000-4000-8000-000000000002";

        let first =
            load_or_generate(&root.join("first"), secrets.as_ref(), &settings, first_id).unwrap();
        let second =
            load_or_generate(&root.join("second"), secrets.as_ref(), &settings, second_id).unwrap();

        assert_ne!(first.key_pem, second.key_pem);
        assert!(
            secrets
                .get(SecretKey::RemoteTlsKey {
                    installation_id: first_id.to_string(),
                })
                .unwrap()
                .is_some()
        );
        assert!(
            secrets
                .get(SecretKey::RemoteTlsKey {
                    installation_id: second_id.to_string(),
                })
                .unwrap()
                .is_some()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fingerprint_is_stable_and_copyable_for_the_same_cert() {
        let tls_dir = temp_tls_dir("fingerprint");
        let secrets = Arc::new(MemorySecretsStore::new());
        let settings = RemoteAccessSettings::default();

        let identity = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap();
        let recomputed = sha256_fingerprint(&identity.cert_pem).unwrap();

        assert_eq!(identity.fingerprint_sha256, recomputed);
        assert_eq!(identity.fingerprint_sha256.len(), 32 * 3 - 1);
        assert!(
            identity
                .fingerprint_sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase() || c == ':')
        );
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    #[test]
    fn missing_cert_file_is_recreated_without_rotating_the_key() {
        let tls_dir = temp_tls_dir("cert-recreate");
        let secrets = Arc::new(MemorySecretsStore::new());
        let settings = RemoteAccessSettings::default();

        let first = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap();
        std::fs::remove_file(tls_dir.join(REMOTE_CERT_FILENAME)).unwrap();
        let second = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap();

        assert_eq!(first.key_pem, second.key_pem);
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    #[test]
    fn mismatched_cert_file_is_recreated_for_the_stored_key() {
        let tls_dir = temp_tls_dir("cert-mismatch");
        let secrets = Arc::new(MemorySecretsStore::new());
        let settings = RemoteAccessSettings::default();

        let first = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap();
        let unrelated_key = rcgen::KeyPair::generate().unwrap();
        write_generated_cert(&tls_dir.join(REMOTE_CERT_FILENAME), &unrelated_key).unwrap();

        let repaired = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap();

        assert_eq!(first.key_pem, repaired.key_pem);
        assert!(certificate_matches_key(
            &repaired.cert_pem,
            &repaired.key_pem
        ));
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    #[test]
    fn custom_paths_override_generated_identity() {
        let tls_dir = temp_tls_dir("custom");
        let secrets = Arc::new(MemorySecretsStore::new());

        // Build a distinct custom identity fixture with rcgen.
        std::fs::create_dir_all(&tls_dir).unwrap();
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let custom_cert = write_generated_cert(&tls_dir.join("custom-cert.pem"), &key_pair)
            .expect("fixture cert should generate");
        let custom_cert_path = tls_dir.join("custom-cert.pem");
        let custom_key_path = tls_dir.join("custom-key.pem");
        std::fs::write(&custom_key_path, key_pair.serialize_pem()).unwrap();

        let custom_settings = RemoteAccessSettings {
            custom_cert_path: Some(custom_cert_path.to_string_lossy().to_string()),
            custom_key_path: Some(custom_key_path.to_string_lossy().to_string()),
            ..RemoteAccessSettings::default()
        };
        let custom = load_test(&tls_dir, secrets.as_ref(), &custom_settings).unwrap();
        assert!(custom.custom);
        assert_eq!(
            custom.fingerprint_sha256,
            sha256_fingerprint(&custom_cert).unwrap()
        );

        let _ = std::fs::remove_dir_all(tls_dir);
    }

    #[test]
    fn one_sided_custom_identity_fails_closed() {
        let tls_dir = temp_tls_dir("custom-one-sided");
        let secrets = Arc::new(MemorySecretsStore::new());
        let settings = RemoteAccessSettings {
            custom_cert_path: Some(
                tls_dir
                    .join("custom-cert.pem")
                    .to_string_lossy()
                    .to_string(),
            ),
            custom_key_path: None,
            ..RemoteAccessSettings::default()
        };

        let error = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap_err();

        assert!(matches!(error, RemoteTlsError::InvalidKey(_)));
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    #[test]
    fn missing_custom_files_fail_before_listener_start() {
        let tls_dir = temp_tls_dir("custom-missing");
        let secrets = Arc::new(MemorySecretsStore::new());
        let settings = RemoteAccessSettings {
            custom_cert_path: Some("/nonexistent/cert.pem".to_string()),
            custom_key_path: Some("/nonexistent/key.pem".to_string()),
            ..RemoteAccessSettings::default()
        };

        let error = load_test(&tls_dir, secrets.as_ref(), &settings).unwrap_err();

        assert!(matches!(error, RemoteTlsError::Io(_)));
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    #[test]
    fn unavailable_secret_store_fails_closed() {
        let tls_dir = temp_tls_dir("fail-closed");
        let settings = RemoteAccessSettings::default();

        let error = load_test(&tls_dir, &FailingSecretsStore, &settings).unwrap_err();

        assert!(matches!(error, RemoteTlsError::SecretStore(_)));
        assert!(!tls_dir.join(REMOTE_CERT_FILENAME).exists());
        let _ = std::fs::remove_dir_all(tls_dir);
    }
}
