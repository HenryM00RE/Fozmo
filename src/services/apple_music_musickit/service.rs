use super::ipc::{read_json_frame, write_json_frame};
use super::model::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleLibraryStatus,
    AppleMusicMvpError, AppleMusicMvpState, AppleMusicMvpStatus, AppleQueueSync,
    EXPECTED_HELPER_BUNDLE_ID, HelperMessage, PROTOCOL_VERSION,
};
use super::music_app::pid as music_app_pid;
use super::source_format::{
    AppleMusicDecoderDetection, AppleMusicSourceFormat, SourceFormatProbeState,
    query_recent_music_app_source_format,
};
use async_trait::async_trait;
use rand::{RngCore, rngs::OsRng};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::time::{Instant, timeout, timeout_at};

const HELPER_EXECUTABLE: &str = "FozmoAppleMusicHelper";
const HELPER_APP: &str = "FozmoAppleMusicHelper.app";
const HELPER_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const HELPER_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// Apple builds the queue playlist in one request, so the whole upcoming run
/// travels in a single call. Cap it so a long queue cannot stall a start.
const MAX_QUEUE_PLAYLIST_LENGTH: usize = 100;
// `log show` routinely needs around a second even for an empty, tightly
// filtered query. Keep enough total budget for a retry, but never launch a
// query with a leftover sliver that cannot reasonably finish.
const SOURCE_FORMAT_PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const SOURCE_FORMAT_QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const SOURCE_FORMAT_MIN_QUERY_TIMEOUT: Duration = Duration::from_millis(1_500);
const SOURCE_FORMAT_PROBE_RETRY_INTERVAL: Duration = Duration::from_millis(75);

fn source_format_query_budget(remaining: Duration) -> Option<Duration> {
    (remaining >= SOURCE_FORMAT_MIN_QUERY_TIMEOUT)
        .then_some(remaining.min(SOURCE_FORMAT_QUERY_TIMEOUT))
}

pub(crate) struct AppleMusicService {
    helper_path: PathBuf,
    runtime_root: PathBuf,
    status: Arc<Mutex<AppleMusicMvpStatus>>,
    source_format_probe: Mutex<SourceFormatProbeState>,
    playback_switch: AsyncMutex<()>,
    connection: AsyncMutex<Option<HelperConnection>>,
    next_command_id: AtomicU64,
}

#[allow(dead_code)]
#[async_trait]
pub(crate) trait AppleMusicHelperClient: Send + Sync {
    async fn lookup_song(
        &self,
        song_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogSong, AppleMusicMvpError>;
    async fn lookup_album(
        &self,
        album_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogAlbum, AppleMusicMvpError>;
    async fn search_songs(
        &self,
        term: String,
        storefront: Option<String>,
        limit: u32,
    ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError>;
    fn snapshot(&self) -> AppleMusicMvpStatus;
}

struct HelperConnection {
    child: Child,
    writer: OwnedWriteHalf,
    events: broadcast::Sender<HelperMessage>,
    alive: Arc<AtomicBool>,
    session_id: String,
    socket_path: PathBuf,
}

impl AppleMusicService {
    pub(crate) fn new(resource_dir: &Path, cache_dir: &Path) -> Self {
        let helper_path = helper_executable_path(resource_dir);
        let helper_present = helper_path.is_file();
        Self {
            helper_path,
            runtime_root: cache_dir.join("apple-music"),
            status: Arc::new(Mutex::new(AppleMusicMvpStatus::new(helper_present))),
            source_format_probe: Mutex::new(SourceFormatProbeState::default()),
            playback_switch: AsyncMutex::new(()),
            connection: AsyncMutex::new(None),
            next_command_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn status(&self) -> AppleMusicMvpStatus {
        let helper_present = self.helper_path.is_file();
        let mut status = self.status.lock().unwrap();
        status.helper_present = helper_present;
        if status.helper_pid.is_none() {
            if helper_present && status.state == AppleMusicMvpState::HelperMissing {
                status.state = AppleMusicMvpState::Stopped;
            } else if !helper_present {
                status.state = AppleMusicMvpState::HelperMissing;
            }
        }
        status.clone()
    }

    /// Probe the decoder owned by the native Music.app process. The query uses
    /// a strict per-track wall-clock boundary and accepts only a fresh ALAC
    /// decoder event for Music.app's exact PID.
    pub(crate) async fn probe_music_app_source_format(
        &self,
        boundary: SystemTime,
    ) -> Result<Option<AppleMusicSourceFormat>, AppleMusicMvpError> {
        let deadline = Instant::now() + SOURCE_FORMAT_PROBE_TIMEOUT;
        let pid = loop {
            if let Some(pid) = music_app_pid() {
                break pid;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(error(
                    "music_app_not_running",
                    "Music.app did not become available for lossless-format verification.",
                    true,
                    "source_format",
                    true,
                ));
            }
            tokio::time::sleep(remaining.min(SOURCE_FORMAT_PROBE_RETRY_INTERVAL)).await;
        };
        self.probe_source_format_for_pid_until(pid, boundary, "Music.app", deadline)
            .await
    }

    async fn probe_source_format_for_pid_until(
        &self,
        music_app_pid: u32,
        boundary: SystemTime,
        process_name: &str,
        deadline: Instant,
    ) -> Result<Option<AppleMusicSourceFormat>, AppleMusicMvpError> {
        let mut successful_query = false;
        let mut last_error = None;
        let mut latest_lossy = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(query_timeout) = source_format_query_budget(remaining) else {
                break;
            };
            let query = tokio::task::spawn_blocking(move || {
                query_recent_music_app_source_format(music_app_pid, boundary, query_timeout)
            })
            .await;
            match query {
                Ok(Ok(detection)) => {
                    successful_query = true;
                    if let Some(detection) = self.source_format_probe.lock().unwrap().accept_new(
                        music_app_pid,
                        boundary,
                        detection,
                    ) {
                        match detection {
                            AppleMusicDecoderDetection::Lossless(source_format) => {
                                tracing::info!(
                                    event = "apple_music_lossless_format_verified",
                                    process = process_name,
                                    source_rate_hz = source_format.sample_rate_hz,
                                    source_bits =
                                        source_format.source_bit_depth_bits.unwrap_or_default(),
                                    "Verified a fresh Apple Lossless decoder"
                                );
                                return Ok(Some(source_format));
                            }
                            AppleMusicDecoderDetection::Lossy {
                                codec,
                                sample_rate_hz,
                                ..
                            } => {
                                // Music commonly opens a short-lived AAC
                                // decoder while changing catalog tracks, then
                                // opens the authoritative ALAC decoder roughly
                                // a second later. Keep the local Player muted
                                // and poll for ALAC through the full deadline.
                                // If none arrives, the remembered AAC event
                                // still makes this a strict lossy rejection.
                                tracing::info!(
                                    event = "apple_music_transitional_lossy_decoder",
                                    process = process_name,
                                    codec,
                                    sample_rate_hz,
                                    "Holding output while waiting for Apple Lossless"
                                );
                                latest_lossy = Some((codec, sample_rate_hz));
                            }
                        }
                    }
                }
                Ok(Err(message)) => last_error = Some(message),
                Err(join_error) => {
                    last_error = Some(format!(
                        "The Music.app source-format probe stopped unexpectedly: {join_error}"
                    ));
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(remaining.min(SOURCE_FORMAT_PROBE_RETRY_INTERVAL)).await;
        }
        if let Some((codec, sample_rate_hz)) = latest_lossy {
            return Err(error(
                "lossy_source_format_selected",
                format!(
                    "{process_name} selected {codec} at {sample_rate_hz} Hz and did not expose an Apple Lossless decoder before the verification deadline. Fozmo did not connect this stream to the DSP."
                ),
                false,
                "source_format",
                false,
            ));
        }
        if successful_query {
            Ok(None)
        } else {
            Err(error(
                "music_app_source_format_probe_failed",
                last_error.unwrap_or_else(|| {
                    format!(
                        "The {process_name} source-format probe timed out before reading Unified Log."
                    )
                }),
                true,
                "source_format",
                true,
            ))
        }
    }

    /// Serialize Music.app selection and Fozmo Capture handoffs so two product
    /// playback requests cannot interleave their provider-boundary work.
    pub(crate) async fn lock_playback_switch(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.playback_switch.lock().await
    }

    pub(crate) async fn launch(&self) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        self.ensure_launched().await?;
        Ok(self.status())
    }

    pub(crate) async fn refresh_status(&self) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        if self.connection.lock().await.is_none() {
            return Ok(self.status());
        }
        self.send_simple_command("get_status", &["ready"]).await?;
        Ok(self.status())
    }

    pub(crate) async fn authorize(
        &self,
        present_ui: bool,
    ) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        self.ensure_launched().await?;
        self.set_state(AppleMusicMvpState::CheckingAuthorization);
        let mut command = self.next_command("authorize").await?;
        command.present_ui = Some(present_ui);
        self.send_and_wait(command, &["authorization_changed"])
            .await?;
        Ok(self.status())
    }

    pub(crate) async fn lookup_song(
        &self,
        song_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogSong, AppleMusicMvpError> {
        let song_id = validate_catalog_id(song_id, "song_not_found")?;
        let storefront = validate_storefront(storefront)?;
        self.ensure_ready().await?;
        let mut command = self.next_command("lookup_song").await?;
        command.song_id = Some(song_id);
        command.storefront = storefront;
        let event = self.send_and_wait(command, &["catalog_song"]).await?;
        event.catalog_song.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty song response.",
                false,
                "catalog_lookup",
                true,
            )
        })
    }

    /// Rebuild the Fozmo queue playlist so it holds exactly `song_ids`, in order.
    ///
    /// The first entry is the track to play; the rest are the queue behind it.
    /// Fozmo starts the playlist from the top rather than at an index, because
    /// `play track N of playlist` plays one track and stops instead of
    /// advancing, which would give up the gapless boundary this exists for.
    pub(crate) async fn sync_queue_playlist(
        &self,
        song_ids: Vec<String>,
    ) -> Result<AppleQueueSync, AppleMusicMvpError> {
        if song_ids.is_empty() || song_ids.len() > MAX_QUEUE_PLAYLIST_LENGTH {
            return Err(error(
                "queue_sync_invalid",
                "Fozmo needs between 1 and 100 Apple Music tracks to build its queue playlist.",
                false,
                "validating_request",
                true,
            ));
        }
        let song_ids = song_ids
            .into_iter()
            .map(|song_id| validate_catalog_id(song_id, "song_not_found"))
            .collect::<Result<Vec<_>, _>>()?;
        self.ensure_ready().await?;
        let mut command = self.next_command("sync_queue").await?;
        command.song_ids = song_ids;
        let event = self.send_and_wait(command, &["queue_synced"]).await?;
        event.queue_sync.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty queue-sync response.",
                false,
                "queue_sync",
                true,
            )
        })
    }

    /// Whether Apple will accept the library writes the queue playlist needs.
    ///
    /// Adding subscription tracks to a library requires Sync Library, and
    /// neither MusicKit nor AppleScript exposes that setting, so the only
    /// honest check is asking Apple to list the user's playlists.
    pub(crate) async fn library_status(&self) -> Result<AppleLibraryStatus, AppleMusicMvpError> {
        self.ensure_ready().await?;
        let command = self.next_command("library_status").await?;
        let event = self.send_and_wait(command, &["library_status"]).await?;
        event.library_status.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty library-status response.",
                false,
                "library_status",
                true,
            )
        })
    }

    pub(crate) async fn lookup_album(
        &self,
        album_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogAlbum, AppleMusicMvpError> {
        let album_id = validate_catalog_id(album_id, "album_not_found")?;
        let storefront = validate_storefront(storefront)?;
        self.ensure_ready().await?;
        let mut command = self.next_command("lookup_album").await?;
        command.album_id = Some(album_id);
        command.storefront = storefront;
        let event = self.send_and_wait(command, &["catalog_album"]).await?;
        event.catalog_album.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty album response.",
                false,
                "catalog_lookup",
                true,
            )
        })
    }

    pub(crate) async fn search_songs(
        &self,
        term: String,
        storefront: Option<String>,
        limit: u32,
    ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError> {
        let term = validate_catalog_search_term(term)?;
        let storefront = validate_storefront(storefront)?;
        if !(1..=25).contains(&limit) {
            return Err(error(
                "catalog_search_limit_invalid",
                "Choose between 1 and 25 Apple Music search results.",
                false,
                "validating_request",
                true,
            ));
        }
        self.ensure_ready().await?;
        let mut command = self.next_command("search_songs").await?;
        command.term = Some(term);
        command.storefront = storefront;
        command.limit = Some(limit);
        let event = self.send_and_wait(command, &["catalog_search"]).await?;
        event.catalog_search.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty search response.",
                false,
                "catalog_search",
                true,
            )
        })
    }

    async fn ensure_launched(&self) -> Result<(), AppleMusicMvpError> {
        {
            let mut guard = self.connection.lock().await;
            if let Some(connection) = guard.as_mut() {
                if !connection.alive.load(Ordering::Acquire) {
                    cleanup_socket(&connection.socket_path);
                    *guard = None;
                } else {
                    match connection.child.try_wait() {
                        Ok(None) => return Ok(()),
                        Ok(Some(_)) | Err(_) => {
                            cleanup_socket(&connection.socket_path);
                            *guard = None;
                        }
                    }
                }
            }
        }

        if !self.helper_path.is_file() {
            self.set_state(AppleMusicMvpState::HelperMissing);
            return Err(error(
                "helper_missing",
                "The Fozmo Apple Music helper has not been built or bundled.",
                false,
                "launching_helper",
                true,
            ));
        }

        self.set_state(AppleMusicMvpState::LaunchingHelper);
        self.clear_error();
        let (listener, socket_path) = self.bind_private_socket()?;
        let session_id = format!("am-{}", random_hex(10));
        let token = random_hex(32);
        let canonical_helper = std::fs::canonicalize(&self.helper_path).map_err(|_| {
            error(
                "helper_missing",
                "The Fozmo Apple Music helper cannot be resolved.",
                false,
                "launching_helper",
                true,
            )
        })?;
        let helper_app = helper_app_bundle_path(&canonical_helper).ok_or_else(|| {
            error(
                "helper_missing",
                "The Fozmo Apple Music helper is not inside a valid app bundle.",
                false,
                "launching_helper",
                true,
            )
        })?;
        let bootstrap_path =
            write_helper_bootstrap(&self.runtime_root, &socket_path, &token, &session_id).map_err(
                |_| {
                    cleanup_socket(&socket_path);
                    error(
                        "helper_launch_failed",
                        "Fozmo could not create the private Apple Music launch record.",
                        true,
                        "launching_helper",
                        true,
                    )
                },
            )?;

        // Launch through LaunchServices so macOS associates the process with
        // the signed bundle's Info.plist and MusicKit App ID. Executing the
        // Mach-O inside Contents/MacOS directly makes TCC treat it as an
        // unbundled process and abort authorization even though the bundle
        // contains NSAppleMusicUsageDescription. Pass only a protected
        // bootstrap-file path on `open`'s command line; putting the random
        // launch token in `--env` would expose it through the process list.
        let mut child = Command::new("/usr/bin/open")
            .arg("-W")
            .arg("-n")
            .arg("-g")
            .arg("--env")
            .arg(format!(
                "FOZMO_APPLE_MUSIC_BOOTSTRAP={}",
                bootstrap_path.to_string_lossy()
            ))
            .arg(helper_app)
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                cleanup_socket(&socket_path);
                cleanup_bootstrap(&bootstrap_path);
                error(
                    "helper_launch_failed",
                    "Fozmo could not launch the Apple Music helper.",
                    true,
                    "launching_helper",
                    true,
                )
            })?;
        if child.id().is_none() {
            let _ = child.kill().await;
            cleanup_socket(&socket_path);
            cleanup_bootstrap(&bootstrap_path);
            let failure = error(
                "helper_launch_failed",
                "The Apple Music helper launched without a process identifier.",
                true,
                "launching_helper",
                true,
            );
            self.record_error(failure.clone());
            return Err(failure);
        }

        let (mut stream, _) = match timeout(HELPER_CONNECT_TIMEOUT, listener.accept()).await {
            Ok(Ok(connection)) => connection,
            _ => {
                cleanup_bootstrap(&bootstrap_path);
                let child_state = match child.try_wait() {
                    Ok(Some(status)) => format!("exited ({status})"),
                    Ok(None) => "still running".to_string(),
                    Err(_) => "unknown".to_string(),
                };
                tracing::warn!(
                    event = "apple_music_helper_connect_timeout",
                    child_state,
                    "Apple Music helper did not connect to its private IPC socket"
                );
                let _ = child.kill().await;
                cleanup_socket(&socket_path);
                let failure = error(
                    "helper_launch_failed",
                    "The Apple Music helper did not connect to Fozmo.",
                    true,
                    "launching_helper",
                    true,
                );
                self.record_error(failure.clone());
                return Err(failure);
            }
        };
        cleanup_bootstrap(&bootstrap_path);
        let hello: HelperMessage =
            match timeout(HELPER_CONNECT_TIMEOUT, read_json_frame(&mut stream)).await {
                Ok(Ok(hello)) => hello,
                _ => {
                    let _ = child.kill().await;
                    cleanup_socket(&socket_path);
                    let failure = error(
                        "helper_protocol_mismatch",
                        "The Apple Music helper sent an invalid handshake.",
                        false,
                        "launching_helper",
                        true,
                    );
                    self.record_error(failure.clone());
                    return Err(failure);
                }
            };
        let launched_pid = hello.pid.filter(|pid| *pid > 0);
        if hello.v != PROTOCOL_VERSION
            || hello.message_type != "hello"
            || hello.session_id.as_deref() != Some(session_id.as_str())
            || hello.token.as_deref() != Some(token.as_str())
            || launched_pid.is_none()
            || hello.bundle_id.as_deref() != Some(EXPECTED_HELPER_BUNDLE_ID)
        {
            let _ = child.kill().await;
            cleanup_socket(&socket_path);
            let failure = error(
                "helper_protocol_mismatch",
                "The Apple Music helper identity or protocol did not match.",
                false,
                "launching_helper",
                true,
            );
            self.record_error(failure.clone());
            return Err(failure);
        }
        let launched_pid = launched_pid.expect("validated helper PID");

        let mut accept =
            HelperMessage::command("cmd-accept".to_string(), "accept", session_id.clone());
        accept.protocol_version = Some(PROTOCOL_VERSION);
        if write_json_frame(&mut stream, &accept).await.is_err() {
            let _ = child.kill().await;
            cleanup_socket(&socket_path);
            let failure = error(
                "helper_protocol_mismatch",
                "Fozmo could not accept the Apple Music helper connection.",
                true,
                "launching_helper",
                true,
            );
            self.record_error(failure.clone());
            return Err(failure);
        }

        let (mut reader, writer) = stream.into_split();
        let (events, _) = broadcast::channel(64);
        {
            let mut status = self.status.lock().unwrap();
            status.helper_pid = Some(launched_pid);
            status.helper_version = hello.helper_version.clone();
            status.helper_musickit_entitled = hello.musickit_entitled.unwrap_or(false);
            status.helper_capabilities = hello.capabilities.clone();
            status.session_id = Some(session_id.clone());
            status.state = AppleMusicMvpState::CheckingAuthorization;
            status.last_error = None;
        }
        let event_sender = events.clone();
        let shared_status = Arc::clone(&self.status);
        let connection_alive = Arc::new(AtomicBool::new(true));
        let reader_alive = Arc::clone(&connection_alive);
        let reader_session_id = session_id.clone();
        tokio::spawn(async move {
            loop {
                match read_json_frame::<_, HelperMessage>(&mut reader).await {
                    Ok(message) if message.v == PROTOCOL_VERSION => {
                        apply_helper_event(&shared_status, &message);
                        let _ = event_sender.send(message);
                    }
                    Ok(_) => {
                        let failure = helper_connection_failure_event(
                            &reader_session_id,
                            "helper_protocol_mismatch",
                            "The Apple Music helper changed protocol versions.",
                            false,
                        );
                        record_shared_error(
                            &shared_status,
                            error(
                                "helper_protocol_mismatch",
                                "The Apple Music helper changed protocol versions.",
                                false,
                                "helper_connection",
                                false,
                            ),
                        );
                        let _ = event_sender.send(failure);
                        break;
                    }
                    Err(_) => {
                        let stopping = matches!(
                            shared_status.lock().unwrap().state,
                            AppleMusicMvpState::Stopping | AppleMusicMvpState::Stopped
                        );
                        if !stopping {
                            let failure = helper_connection_failure_event(
                                &reader_session_id,
                                "helper_exited",
                                "The Apple Music helper connection closed.",
                                true,
                            );
                            record_shared_error(
                                &shared_status,
                                error(
                                    "helper_exited",
                                    "The Apple Music helper connection closed.",
                                    true,
                                    "helper_connection",
                                    true,
                                ),
                            );
                            let _ = event_sender.send(failure);
                        }
                        break;
                    }
                }
            }
            reader_alive.store(false, Ordering::Release);
        });

        *self.connection.lock().await = Some(HelperConnection {
            child,
            writer,
            events,
            alive: connection_alive,
            session_id,
            socket_path,
        });

        // The helper sends an unsolicited ready snapshot immediately after
        // accept. Waiting for an explicit status response keeps launch
        // deterministic even if that event raced with connection storage.
        self.send_simple_command("get_status", &["ready"]).await?;
        Ok(())
    }

    async fn ensure_ready(&self) -> Result<(), AppleMusicMvpError> {
        self.ensure_launched().await?;
        let status = self.status();
        if !status.helper_musickit_entitled {
            return Err(error(
                "musickit_capability_unavailable",
                "This helper is not signed with a development profile for the MusicKit-enabled App ID.",
                false,
                "checking_capability",
                true,
            ));
        }
        if status.authorization != "authorized" {
            return Err(error(
                "music_authorization_not_determined",
                "Authorize Apple Music before using the catalog.",
                false,
                "checking_authorization",
                true,
            ));
        }
        if status.can_play_catalog_content == Some(false) {
            return Err(error(
                "subscription_required",
                "This Apple Music account cannot play catalog content.",
                false,
                "checking_subscription",
                true,
            ));
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
        if self.connection.lock().await.is_none() {
            return Ok(self.status());
        }
        self.set_state(AppleMusicMvpState::Stopping);
        let command_result = self.send_simple_command("shutdown", &["will_exit"]).await;
        let mut connection = self.connection.lock().await.take();
        if let Some(mut connection) = connection.take() {
            if timeout(Duration::from_secs(3), connection.child.wait())
                .await
                .is_err()
            {
                let _ = connection.child.kill().await;
                let _ = connection.child.wait().await;
            }
            cleanup_socket(&connection.socket_path);
        }
        {
            let mut status = self.status.lock().unwrap();
            status.helper_pid = None;
            status.session_id = None;
            status.state = if self.helper_path.is_file() {
                AppleMusicMvpState::Stopped
            } else {
                AppleMusicMvpState::HelperMissing
            };
        }
        command_result?;
        Ok(self.status())
    }

    async fn send_simple_command(
        &self,
        message_type: &str,
        expected_events: &[&str],
    ) -> Result<HelperMessage, AppleMusicMvpError> {
        let command = self.next_command(message_type).await?;
        self.send_and_wait(command, expected_events).await
    }

    async fn next_command(&self, message_type: &str) -> Result<HelperMessage, AppleMusicMvpError> {
        let session_id = self.session_id().await?;
        Ok(HelperMessage::command(
            self.command_id(),
            message_type,
            session_id,
        ))
    }

    async fn session_id(&self) -> Result<String, AppleMusicMvpError> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|connection| connection.session_id.clone())
            .ok_or_else(|| {
                error(
                    "helper_exited",
                    "The Apple Music helper is not connected.",
                    true,
                    "helper_connection",
                    true,
                )
            })
    }

    fn command_id(&self) -> String {
        format!(
            "cmd-{}",
            self.next_command_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn send_and_wait(
        &self,
        command: HelperMessage,
        expected_events: &[&str],
    ) -> Result<HelperMessage, AppleMusicMvpError> {
        let command_id = command.id.clone().unwrap_or_default();
        self.send_serialized_and_wait(command_id, &command, expected_events)
            .await
    }

    async fn send_serialized_and_wait<T: serde::Serialize>(
        &self,
        command_id: String,
        command: &T,
        expected_events: &[&str],
    ) -> Result<HelperMessage, AppleMusicMvpError> {
        let mut receiver = {
            let mut guard = self.connection.lock().await;
            let connection = guard.as_mut().ok_or_else(|| {
                error(
                    "helper_exited",
                    "The Apple Music helper is not connected.",
                    true,
                    "helper_connection",
                    true,
                )
            })?;
            if connection.child.try_wait().ok().flatten().is_some() {
                return Err(error(
                    "helper_exited",
                    "The Apple Music helper exited.",
                    true,
                    "helper_connection",
                    true,
                ));
            }
            let receiver = connection.events.subscribe();
            if write_json_frame(&mut connection.writer, command)
                .await
                .is_err()
            {
                connection.alive.store(false, Ordering::Release);
                return Err(error(
                    "helper_exited",
                    "Fozmo could not send a command to the Apple Music helper.",
                    true,
                    "helper_connection",
                    false,
                ));
            }
            receiver
        };

        let deadline = Instant::now() + HELPER_COMMAND_TIMEOUT;
        loop {
            let event = match timeout_at(deadline, receiver.recv()).await.map_err(|_| {
                error(
                    "helper_exited",
                    "The Apple Music helper did not confirm the command in time.",
                    true,
                    "helper_command",
                    false,
                )
            })? {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(error(
                        "helper_exited",
                        "The Apple Music helper event stream closed.",
                        true,
                        "helper_connection",
                        false,
                    ));
                }
            };
            if event.command_id.as_deref() != Some(command_id.as_str()) {
                continue;
            }
            if event.message_type == "helper_error" {
                let failure = error(
                    event.code.as_deref().unwrap_or("apple_music_unavailable"),
                    event
                        .message
                        .as_deref()
                        .unwrap_or("The Apple Music helper reported an error."),
                    event.retryable.unwrap_or(false),
                    "helper_command",
                    true,
                );
                self.record_error(failure.clone());
                return Err(failure);
            }
            if expected_events.contains(&event.message_type.as_str()) {
                return Ok(event);
            }
        }
    }

    fn bind_private_socket(&self) -> Result<(UnixListener, PathBuf), AppleMusicMvpError> {
        std::fs::create_dir_all(&self.runtime_root).map_err(|_| {
            error(
                "helper_launch_failed",
                "Fozmo could not create the private Apple Music runtime directory.",
                true,
                "launching_helper",
                true,
            )
        })?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.runtime_root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| {
                error(
                    "helper_launch_failed",
                    "Fozmo could not protect the Apple Music runtime directory.",
                    false,
                    "launching_helper",
                    true,
                )
            })?;
        let socket_path = self.runtime_root.join(format!("am-{}.sock", random_hex(6)));
        if socket_path.as_os_str().as_encoded_bytes().len() >= 100 {
            return Err(error(
                "helper_launch_failed",
                "The Apple Music runtime path is too long for a private socket.",
                false,
                "launching_helper",
                true,
            ));
        }
        let listener = UnixListener::bind(&socket_path).map_err(|_| {
            error(
                "helper_launch_failed",
                "Fozmo could not open the private Apple Music IPC socket.",
                true,
                "launching_helper",
                true,
            )
        })?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |_| {
                cleanup_socket(&socket_path);
                error(
                    "helper_launch_failed",
                    "Fozmo could not protect the Apple Music IPC socket.",
                    false,
                    "launching_helper",
                    true,
                )
            },
        )?;
        Ok((listener, socket_path))
    }

    fn set_state(&self, state: AppleMusicMvpState) {
        self.status.lock().unwrap().state = state;
    }

    fn clear_error(&self) {
        self.status.lock().unwrap().last_error = None;
    }

    fn record_error(&self, failure: AppleMusicMvpError) {
        record_shared_error(&self.status, failure);
    }
}

#[async_trait]
impl AppleMusicHelperClient for AppleMusicService {
    async fn lookup_song(
        &self,
        song_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogSong, AppleMusicMvpError> {
        AppleMusicService::lookup_song(self, song_id, storefront).await
    }

    async fn lookup_album(
        &self,
        album_id: String,
        storefront: Option<String>,
    ) -> Result<AppleCatalogAlbum, AppleMusicMvpError> {
        AppleMusicService::lookup_album(self, album_id, storefront).await
    }

    async fn search_songs(
        &self,
        term: String,
        storefront: Option<String>,
        limit: u32,
    ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError> {
        AppleMusicService::search_songs(self, term, storefront, limit).await
    }

    fn snapshot(&self) -> AppleMusicMvpStatus {
        self.status()
    }
}

fn apply_helper_event(status: &Arc<Mutex<AppleMusicMvpStatus>>, event: &HelperMessage) {
    let mut status = status.lock().unwrap();
    if let Some(authorization) = &event.authorization {
        status.authorization = authorization.clone();
    }
    if let Some(can_play) = event.can_play_catalog_content {
        status.can_play_catalog_content = Some(can_play);
    }
    match event.message_type.as_str() {
        "ready" | "authorization_changed" => {
            status.state = if status.authorization == "authorized" {
                AppleMusicMvpState::Ready
            } else {
                AppleMusicMvpState::AwaitingAuthorization
            };
        }
        "catalog_song" | "catalog_album" | "catalog_search" => {
            status.state = AppleMusicMvpState::Ready
        }
        "helper_error" => {
            let failure = error(
                event.code.as_deref().unwrap_or("apple_music_unavailable"),
                event
                    .message
                    .as_deref()
                    .unwrap_or("The Apple Music helper reported an error."),
                event.retryable.unwrap_or(false),
                "helper_event",
                true,
            );
            status.state = AppleMusicMvpState::Failed;
            status.last_error = Some(failure);
        }
        "will_exit" => status.state = AppleMusicMvpState::Stopping,
        _ => {}
    }
}

fn record_shared_error(status: &Arc<Mutex<AppleMusicMvpStatus>>, failure: AppleMusicMvpError) {
    let mut status = status.lock().unwrap();
    status.state = AppleMusicMvpState::Failed;
    status.last_error = Some(failure);
}

fn helper_executable_path(resource_dir: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("FOZMO_APPLE_MUSIC_HELPER") {
        return PathBuf::from(path);
    }
    if resource_dir.file_name().and_then(|name| name.to_str()) == Some("Resources") {
        return resource_dir
            .parent()
            .unwrap_or(resource_dir)
            .join("Helpers")
            .join(HELPER_APP)
            .join("Contents")
            .join("MacOS")
            .join(HELPER_EXECUTABLE);
    }
    resource_dir
        .join("target")
        .join("apple-music-helper")
        .join(HELPER_APP)
        .join("Contents")
        .join("MacOS")
        .join(HELPER_EXECUTABLE)
}

fn helper_app_bundle_path(helper_executable: &Path) -> Option<&Path> {
    let app = helper_executable.parent()?.parent()?.parent()?;
    (app.extension().and_then(|extension| extension.to_str()) == Some("app")
        && app.join("Contents/Info.plist").is_file())
    .then_some(app)
}

fn write_helper_bootstrap(
    runtime_root: &Path,
    socket_path: &Path,
    token: &str,
    session_id: &str,
) -> Result<PathBuf, std::io::Error> {
    let path = runtime_root.join(format!("am-{}.bootstrap.json", random_hex(6)));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "socketPath": socket_path,
            "token": token,
            "sessionID": session_id,
        }),
    )
    .map_err(std::io::Error::other)?;
    file.flush()?;
    Ok(path)
}

fn cleanup_bootstrap(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn cleanup_socket(socket_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
}

fn helper_connection_failure_event(
    session_id: &str,
    code: &str,
    message: &str,
    retryable: bool,
) -> HelperMessage {
    let mut event =
        HelperMessage::command("event".to_string(), "helper_error", session_id.to_string());
    event.id = None;
    event.command_id = None;
    event.code = Some(code.to_string());
    event.message = Some(message.to_string());
    event.retryable = Some(retryable);
    event
}

fn random_hex(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut value);
    let mut output = String::with_capacity(bytes * 2);
    for byte in value {
        use std::fmt::Write;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn validate_catalog_id(
    value: String,
    error_code: &'static str,
) -> Result<String, AppleMusicMvpError> {
    let value = value.trim().to_string();
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(error(
            error_code,
            "Enter a valid Apple Music catalog ID.",
            false,
            "validating_request",
            true,
        ));
    }
    Ok(value)
}

fn validate_catalog_search_term(value: String) -> Result<String, AppleMusicMvpError> {
    let value = value.trim().to_string();
    if value.is_empty() || value.chars().count() > 200 || value.chars().any(char::is_control) {
        return Err(error(
            "catalog_search_term_invalid",
            "Enter an Apple Music search term between 1 and 200 characters.",
            false,
            "validating_request",
            true,
        ));
    }
    Ok(value)
}

fn validate_storefront(value: Option<String>) -> Result<Option<String>, AppleMusicMvpError> {
    let value = normalize_optional(value).map(|value| value.to_ascii_lowercase());
    if value.as_deref().is_some_and(|value| {
        value.len() > 8 || !value.bytes().all(|byte| byte.is_ascii_alphabetic())
    }) {
        return Err(error(
            "apple_music_storefront_invalid",
            "Enter a valid Apple Music storefront code.",
            false,
            "validating_request",
            true,
        ));
    }
    Ok(value)
}

fn error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
    stage: impl Into<String>,
    cleanup_complete: bool,
) -> AppleMusicMvpError {
    AppleMusicMvpError {
        code: code.into(),
        message: message.into(),
        retryable,
        stage: stage.into(),
        cleanup_complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_format_query_budget_rejects_tiny_deadline_remainders() {
        assert_eq!(
            source_format_query_budget(Duration::from_millis(1_499)),
            None
        );
        assert_eq!(
            source_format_query_budget(Duration::from_millis(1_500)),
            Some(Duration::from_millis(1_500))
        );
        assert_eq!(
            source_format_query_budget(Duration::from_secs(2)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            source_format_query_budget(Duration::from_secs(4)),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn source_checkout_helper_path_targets_a_real_app_bundle() {
        let path = helper_executable_path(Path::new("/repo/Fozmo"));
        assert_eq!(
            path,
            PathBuf::from(
                "/repo/Fozmo/target/apple-music-helper/FozmoAppleMusicHelper.app/Contents/MacOS/FozmoAppleMusicHelper"
            )
        );
    }

    #[test]
    fn packaged_helper_path_is_sibling_of_resources() {
        let path = helper_executable_path(Path::new("/Applications/Fozmo.app/Contents/Resources"));
        assert_eq!(
            path,
            PathBuf::from(
                "/Applications/Fozmo.app/Contents/Helpers/FozmoAppleMusicHelper.app/Contents/MacOS/FozmoAppleMusicHelper"
            )
        );
    }

    #[derive(Default)]
    struct FakeHelper {
        status: Mutex<Option<AppleMusicMvpStatus>>,
    }

    #[async_trait]
    impl AppleMusicHelperClient for FakeHelper {
        async fn lookup_song(
            &self,
            song_id: String,
            storefront: Option<String>,
        ) -> Result<AppleCatalogSong, AppleMusicMvpError> {
            Ok(AppleCatalogSong {
                song_id,
                storefront: storefront.unwrap_or_else(|| "nz".to_string()),
                title: "Fake song".to_string(),
                artist: "Fake artist".to_string(),
                ..AppleCatalogSong::default()
            })
        }

        async fn lookup_album(
            &self,
            album_id: String,
            storefront: Option<String>,
        ) -> Result<AppleCatalogAlbum, AppleMusicMvpError> {
            Ok(AppleCatalogAlbum {
                album_id,
                storefront: storefront.unwrap_or_else(|| "nz".to_string()),
                title: "Fake album".to_string(),
                artist: "Fake artist".to_string(),
                ..AppleCatalogAlbum::default()
            })
        }

        async fn search_songs(
            &self,
            term: String,
            storefront: Option<String>,
            limit: u32,
        ) -> Result<AppleCatalogSearchResult, AppleMusicMvpError> {
            let storefront = storefront.unwrap_or_else(|| "nz".to_string());
            Ok(AppleCatalogSearchResult {
                term,
                storefront: storefront.clone(),
                songs: (0..limit)
                    .map(|index| AppleCatalogSong {
                        song_id: format!("song-{index}"),
                        storefront: storefront.clone(),
                        title: format!("Fake song {index}"),
                        artist: "Fake artist".to_string(),
                        ..AppleCatalogSong::default()
                    })
                    .collect(),
                albums: vec![AppleCatalogAlbum {
                    album_id: "album-0".to_string(),
                    storefront,
                    title: "Fake album".to_string(),
                    artist: "Fake artist".to_string(),
                    ..AppleCatalogAlbum::default()
                }],
            })
        }

        fn snapshot(&self) -> AppleMusicMvpStatus {
            self.status
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| AppleMusicMvpStatus::new(false))
        }
    }

    #[tokio::test]
    async fn fake_helper_exercises_catalog_without_entitlement() {
        let fake = FakeHelper::default();
        let search = fake
            .search_songs("fake".to_string(), Some("nz".to_string()), 3)
            .await
            .unwrap();
        assert_eq!(search.term, "fake");
        assert_eq!(search.songs.len(), 3);
        assert_eq!(search.albums.len(), 1);
        let song = fake
            .lookup_song("2037093408".to_string(), Some("nz".to_string()))
            .await
            .unwrap();
        assert_eq!(song.song_id, "2037093408");
    }

    #[test]
    fn catalog_search_validation_accepts_unicode_and_rejects_unsafe_input() {
        assert_eq!(
            validate_catalog_search_term("  Björk – Jóga  ".to_string()).unwrap(),
            "Björk – Jóga"
        );
        assert!(validate_catalog_search_term(" \n ".to_string()).is_err());
        assert!(validate_catalog_search_term("hello\u{0000}world".to_string()).is_err());
        assert!(validate_catalog_search_term("x".repeat(201)).is_err());
    }
}
