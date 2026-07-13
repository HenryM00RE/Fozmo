use crate::app::identity;
use crate::error::DomainError;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const INSTALL_FILE_VERSION: u32 = 1;
const DEVELOPMENT_DATA_DIR_NAME: &str = "Fozmo-dev";
pub(crate) const RELEASE_SMOKE_MARKER_NAME: &str = ".fozmo-release-smoke";
pub(crate) const RELEASE_SMOKE_MARKER_CONTENT: &str = "fozmo-release-smoke-v1\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    /// Compatibility root for development tools that still reason in terms of
    /// one workspace. In packaged mode this is the writable data root.
    pub workspace_dir: PathBuf,
    pub resource_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub music_dir: PathBuf,
    pub static_dir: PathBuf,
    pub built_in_presets_dir: PathBuf,
    pub presets_dir: PathBuf,
    pub appearance_assets_dir: PathBuf,
    pub library_dir: PathBuf,
    pub art_dir: PathBuf,
    pub thumbnail_cache_dir: PathBuf,
    pub qobuz_cache_dir: PathBuf,
    pub sonos_cache_dir: PathBuf,
    pub transcode_cache_dir: PathBuf,
    pub tls_dir: PathBuf,
    pub backups_dir: PathBuf,
    pub settings_path: PathBuf,
    pub install_path: PathBuf,
    pub data_lock_path: PathBuf,
    pub dev_secrets_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct InstallMetadata {
    pub file_version: u32,
    pub installation_id: String,
    pub created_at_unix_secs: u64,
    #[serde(default)]
    pub last_successful_app_version: Option<String>,
    #[serde(default)]
    pub data_schema_version: u32,
}

/// Held for the lifetime of the core server. File locks are released by the
/// kernel on process exit, so a crash cannot leave an unrecoverable stale lock.
#[derive(Debug)]
pub struct DataRootLock {
    file: File,
    path: PathBuf,
}

impl Drop for DataRootLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            eprintln!("persistence: failed to unlock {:?}: {error}", self.path);
        }
    }
}

impl AppPaths {
    pub fn from_env() -> Self {
        let resource = std::env::var(identity::env_key("RESOURCE_DIR"))
            .ok()
            .map(PathBuf::from);
        let data = std::env::var(identity::env_key("DATA_DIR"))
            .ok()
            .map(PathBuf::from);
        let cache = std::env::var(identity::env_key("CACHE_DIR"))
            .ok()
            .map(PathBuf::from);
        let logs = std::env::var(identity::env_key("LOG_DIR"))
            .ok()
            .map(PathBuf::from);

        // Split roots take precedence when a packaged launcher supplies any
        // of them. FOZMO_WORKSPACE_DIR remains an explicit compatibility mode
        // for smoke tests and legacy single-root development workflows.
        if resource.is_some() || data.is_some() || cache.is_some() || logs.is_some() {
            let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            return Self::from_roots(
                resource.unwrap_or(current),
                data.unwrap_or_else(default_data_dir),
                cache.unwrap_or_else(default_cache_dir),
                logs.unwrap_or_else(default_log_dir),
            );
        }

        if let Ok(workspace_dir) = std::env::var(identity::env_key("WORKSPACE_DIR")) {
            return Self::from_workspace_dir(PathBuf::from(workspace_dir));
        }

        // A source checkout is read-only by default. Keep development state
        // in OS-appropriate Fozmo-dev roots so a normal `cargo run` cannot
        // contaminate the repository with settings, databases, logs, or keys.
        Self::from_development_roots(
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            default_development_data_dir(),
            default_development_cache_dir(),
            default_development_log_dir(),
        )
    }

    fn from_development_roots(
        resource_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
        log_dir: impl Into<PathBuf>,
    ) -> Self {
        let resource_dir = resource_dir.into();
        let mut paths = Self::from_roots(&resource_dir, data_dir, cache_dir, log_dir);
        // Source checkouts retain the historical `presets/` resource name;
        // packaged app resources are assembled under `default-presets/`.
        paths.built_in_presets_dir = resource_dir.join("presets");
        paths
    }

    pub fn from_workspace_dir(workspace_dir: impl Into<PathBuf>) -> Self {
        let workspace_dir = workspace_dir.into();
        let library_dir = workspace_dir.join("library");
        Self {
            resource_dir: workspace_dir.clone(),
            data_dir: workspace_dir.clone(),
            cache_dir: library_dir.clone(),
            log_dir: workspace_dir.join("logs"),
            music_dir: workspace_dir.join("music"),
            static_dir: workspace_dir.join("static"),
            built_in_presets_dir: workspace_dir.join("presets"),
            presets_dir: workspace_dir.join("presets"),
            appearance_assets_dir: workspace_dir.join("static").join("user-fonts"),
            art_dir: library_dir.join("art"),
            thumbnail_cache_dir: library_dir.join("art").join("thumbnails"),
            qobuz_cache_dir: library_dir.join("qobuz-cache"),
            sonos_cache_dir: library_dir.join("sonos-cache"),
            transcode_cache_dir: library_dir.join("transcode-cache"),
            tls_dir: library_dir.join("tls"),
            backups_dir: workspace_dir.join("backups"),
            settings_path: workspace_dir.join("settings.json"),
            install_path: workspace_dir.join("install.json"),
            data_lock_path: workspace_dir.join(".fozmo.lock"),
            dev_secrets_path: workspace_dir.join("secrets.dev.json"),
            library_dir,
            workspace_dir,
        }
    }

    pub fn from_roots(
        resource_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
        log_dir: impl Into<PathBuf>,
    ) -> Self {
        let resource_dir = resource_dir.into();
        let data_dir = data_dir.into();
        let cache_dir = cache_dir.into();
        let log_dir = log_dir.into();
        let library_dir = data_dir.join("library");
        Self {
            workspace_dir: data_dir.clone(),
            static_dir: resource_dir.join("static"),
            built_in_presets_dir: resource_dir.join("default-presets"),
            music_dir: data_dir.join("music"),
            presets_dir: data_dir.join("presets"),
            appearance_assets_dir: data_dir.join("appearance"),
            art_dir: library_dir.join("art"),
            thumbnail_cache_dir: cache_dir.join("thumbnails"),
            qobuz_cache_dir: cache_dir.join("qobuz"),
            sonos_cache_dir: cache_dir.join("sonos"),
            transcode_cache_dir: cache_dir.join("transcode"),
            tls_dir: data_dir.join("tls"),
            backups_dir: data_dir.join("backups"),
            settings_path: data_dir.join("settings.json"),
            install_path: data_dir.join("install.json"),
            data_lock_path: data_dir.join(".fozmo.lock"),
            dev_secrets_path: data_dir.join("secrets.dev.json"),
            library_dir,
            resource_dir,
            data_dir,
            cache_dir,
            log_dir,
        }
    }

    /// Prove that the private release-smoke switch is running inside the
    /// packaged helper with disposable, tightly-scoped writable roots.
    /// Validation happens before any settings, database, or secrets migration.
    pub(crate) fn validate_release_smoke_layout(
        &self,
        current_executable: &Path,
    ) -> Result<(), String> {
        let roots = [
            ("data", &self.data_dir),
            ("cache", &self.cache_dir),
            ("logs", &self.log_dir),
        ];
        for (expected_name, path) in roots {
            if !path.is_absolute() {
                return Err(format!("{expected_name} root must be absolute: {path:?}"));
            }
            if path.file_name().and_then(|name| name.to_str()) != Some(expected_name) {
                return Err(format!(
                    "release-smoke {expected_name} root must be named '{expected_name}': {path:?}"
                ));
            }
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|error| format!("inspect release-smoke root {path:?}: {error}"))?;
            if !metadata.file_type().is_dir() {
                return Err(format!(
                    "release-smoke {expected_name} root must be a real directory, not a symlink: {path:?}"
                ));
            }
            let mut entries = std::fs::read_dir(path)
                .map_err(|error| format!("read release-smoke root {path:?}: {error}"))?;
            if entries.next().is_some() {
                return Err(format!(
                    "release-smoke {expected_name} root must be empty before startup: {path:?}"
                ));
            }
        }

        let root = self
            .data_dir
            .parent()
            .ok_or_else(|| "release-smoke data root has no parent".to_string())?;
        if self.cache_dir.parent() != Some(root) || self.log_dir.parent() != Some(root) {
            return Err(
                "release-smoke data, cache, and logs must share one private parent".to_string(),
            );
        }
        let root_metadata = std::fs::symlink_metadata(root)
            .map_err(|error| format!("inspect release-smoke parent {root:?}: {error}"))?;
        if !root_metadata.file_type().is_dir() {
            return Err(format!(
                "release-smoke parent must be a real directory, not a symlink: {root:?}"
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if root_metadata.permissions().mode() & 0o077 != 0 {
                return Err(format!(
                    "release-smoke parent must not be accessible by group or other users: {root:?}"
                ));
            }
        }

        let marker = root.join(RELEASE_SMOKE_MARKER_NAME);
        let marker_metadata = std::fs::symlink_metadata(&marker)
            .map_err(|error| format!("inspect release-smoke marker {marker:?}: {error}"))?;
        if !marker_metadata.file_type().is_file() {
            return Err(format!(
                "release-smoke marker must be a regular file, not a symlink: {marker:?}"
            ));
        }
        let marker_body = std::fs::read_to_string(&marker)
            .map_err(|error| format!("read release-smoke marker {marker:?}: {error}"))?;
        if marker_body != RELEASE_SMOKE_MARKER_CONTENT {
            return Err(format!(
                "release-smoke marker has invalid contents: {marker:?}"
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if marker_metadata.permissions().mode() & 0o077 != 0 {
                return Err(format!(
                    "release-smoke marker must not be accessible by group or other users: {marker:?}"
                ));
            }
        }

        let canonical_root = std::fs::canonicalize(root)
            .map_err(|error| format!("resolve release-smoke parent {root:?}: {error}"))?;
        for (expected_name, path) in roots {
            let canonical = std::fs::canonicalize(path)
                .map_err(|error| format!("resolve release-smoke root {path:?}: {error}"))?;
            if canonical.parent() != Some(canonical_root.as_path())
                || canonical.file_name().and_then(|name| name.to_str()) != Some(expected_name)
            {
                return Err(format!(
                    "release-smoke {expected_name} root escapes its private parent: {path:?}"
                ));
            }
        }

        let resource_metadata = std::fs::symlink_metadata(&self.resource_dir).map_err(|error| {
            format!(
                "inspect packaged release-smoke resources {:?}: {error}",
                self.resource_dir
            )
        })?;
        if !resource_metadata.file_type().is_dir() {
            return Err(format!(
                "release-smoke resources must be a real packaged directory: {:?}",
                self.resource_dir
            ));
        }
        let canonical_resources = std::fs::canonicalize(&self.resource_dir).map_err(|error| {
            format!(
                "resolve packaged release-smoke resources {:?}: {error}",
                self.resource_dir
            )
        })?;
        if !canonical_resources.ends_with(Path::new("Contents/Resources")) {
            return Err(format!(
                "release-smoke resources must belong to a macOS app bundle: {:?}",
                self.resource_dir
            ));
        }
        if canonical_resources.starts_with(&canonical_root) {
            return Err("release-smoke resources must be outside the disposable roots".to_string());
        }
        let frontend = canonical_resources.join("static/react-app/index.html");
        if !std::fs::symlink_metadata(&frontend)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false)
        {
            return Err(format!(
                "release-smoke packaged frontend is missing or not a regular file: {frontend:?}"
            ));
        }

        let contents = canonical_resources.parent().ok_or_else(|| {
            "release-smoke Resources directory has no Contents parent".to_string()
        })?;
        let expected_executable = contents.join("Helpers/fozmo-server");
        let expected_executable = std::fs::canonicalize(&expected_executable).map_err(|error| {
            format!("resolve packaged release-smoke helper {expected_executable:?}: {error}")
        })?;
        let current_executable = std::fs::canonicalize(current_executable).map_err(|error| {
            format!("resolve current release-smoke executable {current_executable:?}: {error}")
        })?;
        if current_executable != expected_executable {
            return Err(format!(
                "release-smoke may run only from the packaged helper: {current_executable:?}"
            ));
        }

        Ok(())
    }

    pub fn ensure_directories(&self) -> std::io::Result<()> {
        // Resource paths are deliberately not created here: an installed app
        // bundle is read-only and missing resources are a packaging error.
        for path in [
            &self.data_dir,
            &self.cache_dir,
            &self.log_dir,
            &self.music_dir,
            &self.presets_dir,
            &self.appearance_assets_dir,
            &self.library_dir,
            &self.art_dir,
            &self.thumbnail_cache_dir,
            &self.qobuz_cache_dir,
            &self.sonos_cache_dir,
            &self.transcode_cache_dir,
            &self.tls_dir,
            &self.backups_dir,
        ] {
            std::fs::create_dir_all(path)?;
        }
        Ok(())
    }

    pub fn acquire_data_lock(&self) -> std::io::Result<DataRootLock> {
        std::fs::create_dir_all(&self.data_dir)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // Do not truncate until after the non-blocking lock succeeds;
            // otherwise a second process could erase the owning PID.
            .truncate(false)
            .open(&self.data_lock_path)?;
        file.try_lock().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "another Fozmo server is using data root {:?}: {error}",
                    self.data_dir
                ),
            )
        })?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "pid={}", std::process::id())?;
        writeln!(file, "started_at={}", now_unix_secs())?;
        file.sync_all()?;
        Ok(DataRootLock {
            file,
            path: self.data_lock_path.clone(),
        })
    }

    pub fn load_or_create_install_metadata(&self) -> Result<InstallMetadata, DomainError> {
        match self.load_install_metadata() {
            Ok(Some(metadata)) => Ok(metadata),
            Ok(None) => {
                let metadata = InstallMetadata {
                    file_version: INSTALL_FILE_VERSION,
                    installation_id: random_installation_id(),
                    created_at_unix_secs: now_unix_secs(),
                    last_successful_app_version: None,
                    data_schema_version: 0,
                };
                write_install_metadata(&self.install_path, &metadata)
                    .map_err(DomainError::persistence)?;
                Ok(metadata)
            }
            Err(error) => Err(error),
        }
    }

    /// Read and validate installation metadata without creating or modifying
    /// anything. The legacy importer uses this to preserve an existing stable
    /// UUID during data-root relocation.
    pub fn load_install_metadata(&self) -> Result<Option<InstallMetadata>, DomainError> {
        read_install_metadata(&self.install_path)
    }

    pub fn record_successful_start(
        &self,
        metadata: &mut InstallMetadata,
        data_schema_version: u32,
    ) -> Result<(), DomainError> {
        metadata.last_successful_app_version = Some(env!("CARGO_PKG_VERSION").to_string());
        metadata.data_schema_version = data_schema_version;
        write_install_metadata(&self.install_path, metadata).map_err(DomainError::persistence)
    }

    /// Record a completed server startup after the listener has bound.
    ///
    /// Loading the file again here keeps the success marker out of state
    /// construction, where a later bind failure would otherwise make a
    /// failed launch look successful.
    pub fn record_current_start_success(
        &self,
        data_schema_version: u32,
    ) -> Result<(), DomainError> {
        let mut metadata = self.load_or_create_install_metadata()?;
        self.record_successful_start(&mut metadata, data_schema_version)
    }

    pub fn print_summary(&self) {
        println!("Static assets path: {:?}", self.static_dir);
        println!("Built-in EQ presets path: {:?}", self.built_in_presets_dir);
        println!("User EQ presets path: {:?}", self.presets_dir);
        println!(
            "Library database path: {:?}",
            self.library_dir.join("library.db")
        );
        println!("Settings file: {:?}", self.settings_path);
        println!("Cache path: {:?}", self.cache_dir);
        println!("Logs path: {:?}", self.log_dir);
        #[cfg(feature = "dev-secrets-file")]
        println!("Dev secrets file: {:?}", self.dev_secrets_path);
    }
}

fn read_install_metadata(path: &Path) -> Result<Option<InstallMetadata>, DomainError> {
    let mut body = String::new();
    match File::open(path) {
        Ok(mut file) => file.read_to_string(&mut body).map_err(|error| {
            DomainError::persistence(format!("read install metadata {:?}: {error}", path))
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(DomainError::persistence(format!(
                "open install metadata {:?}: {error}",
                path
            )));
        }
    };
    let metadata: InstallMetadata = serde_json::from_str(&body).map_err(|error| {
        DomainError::persistence(format!("parse install metadata {:?}: {error}", path))
    })?;
    if metadata.file_version != INSTALL_FILE_VERSION {
        return Err(DomainError::persistence(format!(
            "install metadata {:?} has unsupported version {}",
            path, metadata.file_version
        )));
    }
    if !valid_installation_id(&metadata.installation_id) {
        return Err(DomainError::persistence(format!(
            "install metadata {:?} has an invalid installation id",
            path
        )));
    }
    Ok(Some(metadata))
}

pub(crate) fn write_install_metadata(
    path: &Path,
    metadata: &InstallMetadata,
) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(metadata)
        .map_err(|error| format!("serialize install metadata: {error}"))?;
    atomic_write(path, &json)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("path {:?} has no parent directory", path))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create parent directory {:?}: {error}", parent))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data");
    let temp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        now_unix_nanos()
    ));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| format!("create temporary file {:?}: {error}", temp))?;
        file.write_all(bytes)
            .map_err(|error| format!("write temporary file {:?}: {error}", temp))?;
        file.sync_all()
            .map_err(|error| format!("sync temporary file {:?}: {error}", temp))?;
        std::fs::rename(&temp, path)
            .map_err(|error| format!("replace {:?} atomically: {error}", path))?;
        sync_directory(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

fn random_installation_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    // UUID v4 markers make the identifier recognizable without adding a UUID
    // dependency to the server build.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn valid_installation_id(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(index, ch)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                ch == '-'
            } else {
                ch.is_ascii_hexdigit()
            }
        })
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(target_os = "macos")]
fn default_development_data_dir() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Application Support")
        .join(DEVELOPMENT_DATA_DIR_NAME)
}

#[cfg(target_os = "windows")]
fn default_development_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(home_dir)
        .join(DEVELOPMENT_DATA_DIR_NAME)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn default_development_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/share"))
        .join(DEVELOPMENT_DATA_DIR_NAME)
}

#[cfg(target_os = "macos")]
fn default_development_cache_dir() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Caches")
        .join(DEVELOPMENT_DATA_DIR_NAME)
}

#[cfg(target_os = "windows")]
fn default_development_cache_dir() -> PathBuf {
    default_development_data_dir().join("cache")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn default_development_cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cache"))
        .join(DEVELOPMENT_DATA_DIR_NAME)
}

#[cfg(target_os = "macos")]
fn default_development_log_dir() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Logs")
        .join(DEVELOPMENT_DATA_DIR_NAME)
}

#[cfg(not(target_os = "macos"))]
fn default_development_log_dir() -> PathBuf {
    default_development_data_dir().join("logs")
}

#[cfg(target_os = "macos")]
fn default_data_dir() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Application Support")
        .join(identity::DATA_DIR_NAME)
}

#[cfg(not(target_os = "macos"))]
fn default_data_dir() -> PathBuf {
    home_dir()
        .join(".local/share")
        .join(identity::DATA_DIR_NAME)
}

#[cfg(target_os = "macos")]
fn default_cache_dir() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Caches")
        .join(identity::DATA_DIR_NAME)
}

#[cfg(not(target_os = "macos"))]
fn default_cache_dir() -> PathBuf {
    home_dir().join(".cache").join(identity::DATA_DIR_NAME)
}

#[cfg(target_os = "macos")]
fn default_log_dir() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Logs")
        .join(identity::DATA_DIR_NAME)
}

#[cfg(not(target_os = "macos"))]
fn default_log_dir() -> PathBuf {
    default_data_dir().join("logs")
}

pub fn clean_windows_verbatim(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{stripped}"));
    }
    if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path
}

pub fn dedupe_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut deduped: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !deduped.iter().any(|existing| same_path(existing, &path)) {
            deduped.push(path);
        }
    }
    deduped
}

fn same_path(left: &Path, right: &Path) -> bool {
    same_path_text(left.to_string_lossy(), right.to_string_lossy())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn same_path_text(left: Cow<'_, str>, right: Cow<'_, str>) -> bool {
    left.eq_ignore_ascii_case(&right)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn same_path_text(left: Cow<'_, str>, right: Cow<'_, str>) -> bool {
    left == right
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fozmo-paths-{name}-{}", now_unix_nanos()))
    }

    fn release_smoke_fixture(name: &str) -> (PathBuf, AppPaths, PathBuf) {
        let fixture = temp_root(name);
        let resources = fixture.join("Fozmo.app/Contents/Resources");
        let executable = fixture.join("Fozmo.app/Contents/Helpers/fozmo-server");
        let runtime = fixture.join("runtime");
        std::fs::create_dir_all(resources.join("static/react-app")).unwrap();
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::create_dir_all(runtime.join("data")).unwrap();
        std::fs::create_dir_all(runtime.join("cache")).unwrap();
        std::fs::create_dir_all(runtime.join("logs")).unwrap();
        std::fs::write(
            resources.join("static/react-app/index.html"),
            "<title>Fozmo</title>",
        )
        .unwrap();
        std::fs::write(&executable, "packaged helper").unwrap();
        std::fs::write(
            runtime.join(RELEASE_SMOKE_MARKER_NAME),
            RELEASE_SMOKE_MARKER_CONTENT,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(
                runtime.join(RELEASE_SMOKE_MARKER_NAME),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let paths = AppPaths::from_roots(
            resources,
            runtime.join("data"),
            runtime.join("cache"),
            runtime.join("logs"),
        );
        (fixture, paths, executable)
    }

    #[test]
    fn workspace_layout_remains_backward_compatible() {
        let paths = AppPaths::from_workspace_dir("/workspace/app");
        assert_eq!(paths.music_dir, PathBuf::from("/workspace/app/music"));
        assert_eq!(paths.static_dir, PathBuf::from("/workspace/app/static"));
        assert_eq!(paths.presets_dir, PathBuf::from("/workspace/app/presets"));
        assert_eq!(paths.library_dir, PathBuf::from("/workspace/app/library"));
        assert_eq!(paths.tls_dir, PathBuf::from("/workspace/app/library/tls"));
        assert_eq!(
            paths.settings_path,
            PathBuf::from("/workspace/app/settings.json")
        );
    }

    #[test]
    fn packaged_layout_separates_resources_data_caches_and_logs() {
        let paths = AppPaths::from_roots("/app/resources", "/data", "/cache", "/logs");
        assert_eq!(paths.static_dir, PathBuf::from("/app/resources/static"));
        assert_eq!(
            paths.built_in_presets_dir,
            PathBuf::from("/app/resources/default-presets")
        );
        assert_eq!(paths.library_dir, PathBuf::from("/data/library"));
        assert_eq!(
            paths.appearance_assets_dir,
            PathBuf::from("/data/appearance")
        );
        assert_eq!(paths.qobuz_cache_dir, PathBuf::from("/cache/qobuz"));
        assert_eq!(
            paths.thumbnail_cache_dir,
            PathBuf::from("/cache/thumbnails")
        );
        assert_eq!(paths.log_dir, PathBuf::from("/logs"));
    }

    #[test]
    fn development_layout_keeps_runtime_state_outside_the_source_tree() {
        let paths = AppPaths::from_development_roots(
            "/repo",
            "/data/Fozmo-dev",
            "/cache/Fozmo-dev",
            "/logs/Fozmo-dev",
        );
        assert_eq!(paths.static_dir, PathBuf::from("/repo/static"));
        assert_eq!(paths.built_in_presets_dir, PathBuf::from("/repo/presets"));
        assert_eq!(
            paths.settings_path,
            PathBuf::from("/data/Fozmo-dev/settings.json")
        );
        assert_eq!(
            paths.dev_secrets_path,
            PathBuf::from("/data/Fozmo-dev/secrets.dev.json")
        );
        assert_eq!(paths.cache_dir, PathBuf::from("/cache/Fozmo-dev"));
        assert_eq!(paths.log_dir, PathBuf::from("/logs/Fozmo-dev"));
    }

    #[test]
    fn release_smoke_layout_accepts_packaged_helper_and_private_roots() {
        let (fixture, paths, executable) = release_smoke_fixture("release-smoke-valid");
        paths.validate_release_smoke_layout(&executable).unwrap();

        let wrong_executable = fixture.join("fozmo-server");
        std::fs::write(&wrong_executable, "unpackaged helper").unwrap();
        assert!(
            paths
                .validate_release_smoke_layout(&wrong_executable)
                .unwrap_err()
                .contains("packaged helper")
        );
        let _ = std::fs::remove_dir_all(fixture);
    }

    #[test]
    fn release_smoke_layout_rejects_missing_marker_and_split_parents() {
        let (fixture, paths, executable) = release_smoke_fixture("release-smoke-invalid");
        std::fs::remove_file(
            paths
                .data_dir
                .parent()
                .unwrap()
                .join(RELEASE_SMOKE_MARKER_NAME),
        )
        .unwrap();
        assert!(
            paths
                .validate_release_smoke_layout(&executable)
                .unwrap_err()
                .contains("marker")
        );

        std::fs::write(
            paths
                .data_dir
                .parent()
                .unwrap()
                .join(RELEASE_SMOKE_MARKER_NAME),
            RELEASE_SMOKE_MARKER_CONTENT,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                paths
                    .data_dir
                    .parent()
                    .unwrap()
                    .join(RELEASE_SMOKE_MARKER_NAME),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let other_cache = fixture.join("other/cache");
        std::fs::create_dir_all(&other_cache).unwrap();
        let split = AppPaths::from_roots(
            &paths.resource_dir,
            &paths.data_dir,
            other_cache,
            &paths.log_dir,
        );
        assert!(
            split
                .validate_release_smoke_layout(&executable)
                .unwrap_err()
                .contains("share one private parent")
        );
        let _ = std::fs::remove_dir_all(fixture);
    }

    #[test]
    fn release_smoke_layout_rejects_preexisting_runtime_data() {
        let (fixture, paths, executable) = release_smoke_fixture("release-smoke-nonempty");
        std::fs::write(paths.data_dir.join("settings.json"), "{}").unwrap();
        assert!(
            paths
                .validate_release_smoke_layout(&executable)
                .unwrap_err()
                .contains("must be empty before startup")
        );
        let _ = std::fs::remove_dir_all(fixture);
    }

    #[cfg(unix)]
    #[test]
    fn release_smoke_layout_rejects_symlinked_writable_roots() {
        use std::os::unix::fs::symlink;

        let (fixture, paths, executable) = release_smoke_fixture("release-smoke-symlink");
        let cache_target = paths.data_dir.parent().unwrap().join("real-cache");
        std::fs::remove_dir(&paths.cache_dir).unwrap();
        std::fs::create_dir(&cache_target).unwrap();
        symlink(&cache_target, &paths.cache_dir).unwrap();
        assert!(
            paths
                .validate_release_smoke_layout(&executable)
                .unwrap_err()
                .contains("not a symlink")
        );
        let _ = std::fs::remove_dir_all(fixture);
    }

    #[test]
    fn data_root_lock_is_exclusive_and_released_on_drop() {
        let root = temp_root("lock");
        let paths = AppPaths::from_workspace_dir(&root);
        let first = paths.acquire_data_lock().unwrap();
        assert_eq!(
            paths.acquire_data_lock().unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        drop(first);
        paths.acquire_data_lock().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn install_metadata_keeps_a_stable_uuid() {
        let root = temp_root("install");
        let paths = AppPaths::from_workspace_dir(&root);
        paths.ensure_directories().unwrap();
        let first = paths.load_or_create_install_metadata().unwrap();
        let second = paths.load_or_create_install_metadata().unwrap();
        assert_eq!(first.installation_id, second.installation_id);
        assert!(valid_installation_id(&first.installation_id));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn successful_start_marker_is_written_only_when_explicitly_recorded() {
        let root = temp_root("successful-start");
        let paths = AppPaths::from_workspace_dir(&root);
        paths.ensure_directories().unwrap();
        let initial = paths.load_or_create_install_metadata().unwrap();
        assert_eq!(initial.last_successful_app_version, None);
        assert_eq!(initial.data_schema_version, 0);

        paths.record_current_start_success(7).unwrap();
        let recorded = paths.load_or_create_install_metadata().unwrap();
        assert_eq!(
            recorded.last_successful_app_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(recorded.data_schema_version, 7);
        assert_eq!(recorded.installation_id, initial.installation_id);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dedupe_paths_preserves_first_seen_order() {
        let paths = dedupe_paths([
            PathBuf::from("/music/a"),
            PathBuf::from("/music/b"),
            PathBuf::from("/music/a"),
        ]);
        assert_eq!(
            paths,
            vec![PathBuf::from("/music/a"), PathBuf::from("/music/b")]
        );
    }

    #[test]
    fn clean_windows_verbatim_removes_prefixes() {
        assert_eq!(
            clean_windows_verbatim(PathBuf::from(r"\\?\C:\Music")),
            PathBuf::from(r"C:\Music")
        );
        assert_eq!(
            clean_windows_verbatim(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
    }
}
