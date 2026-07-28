use super::ipc::{read_json_frame, write_json_frame};
use super::model::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleLibraryPlaylistIdentity,
    AppleLibraryStatus, AppleMusicMvpError, AppleMusicMvpState, AppleMusicMvpStatus,
    AppleQueuePlaylistInventory, AppleQueuePlaylistItem, EXPECTED_HELPER_BUNDLE_ID, HelperMessage,
    PROTOCOL_VERSION,
};
use super::music_app::{
    QueueGenerationDeletion, QueueGenerationObservation, delete_queue_generation,
    pid as music_app_pid, queue_generation_observation,
};
use super::queue_generation::{
    AppleQueueGenerationCache, AppleQueueGenerationStore, AppleQueueLifecycle, AppleQueueOwnerKind,
    AppleQueueOwnerRef, AppleQueueSlot, AppleQueueSlotRecord, DeletionRequest,
    NormalizationFailure, STAGE_OR_ADOPT_TIMEOUT, StageOrAdoptAttempt, StageOrAdoptResult,
    StageRefusal, content_fingerprint, normalize_helper_result, parse_generation_description,
};
use super::queue_verification::{StartupReadinessPolicy, verify_server_order, visible_prefix};
use super::source_format::{
    AppleMusicDecoderDetection, AppleMusicSourceFormat, DecoderLogObserver, SourceFormatProbeState,
};
use async_trait::async_trait;
use rand::{RngCore, rngs::OsRng};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc};
use tokio::time::{Instant, timeout, timeout_at};

const HELPER_EXECUTABLE: &str = "FozmoAppleMusicHelper";
const HELPER_APP: &str = "FozmoAppleMusicHelper.app";
const HELPER_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const HELPER_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const QUEUE_GENERATION_VISIBLE_TIMEOUT: Duration = Duration::from_secs(60);
const QUEUE_GENERATION_VISIBLE_POLL: Duration = Duration::from_millis(250);
/// Apple builds the queue playlist in one request, so the whole upcoming run
/// travels in a single call. Cap it so a long queue cannot stall a start.
const MAX_QUEUE_PLAYLIST_LENGTH: usize = 100;
// `log show` routinely needs around a second even for an empty, tightly
// filtered query. Keep enough total budget for a retry, but never launch a
// query with a leftover sliver that cannot reasonably finish.
const SOURCE_FORMAT_PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const SOURCE_FORMAT_PROBE_RETRY_INTERVAL: Duration = Duration::from_millis(75);

pub(crate) struct AppleMusicService {
    helper_path: PathBuf,
    runtime_root: PathBuf,
    status: Arc<Mutex<AppleMusicMvpStatus>>,
    source_format_probe: Mutex<SourceFormatProbeState>,
    decoder_log_observer: Mutex<DecoderLogObserver>,
    playback_switch: AsyncMutex<()>,
    installation_id: String,
    queue_generation_store: AppleQueueGenerationStore,
    queue_generations: AsyncMutex<AppleQueueGenerationCache>,
    current_queue_target: Mutex<Option<AppleQueuePlaybackTarget>>,
    connection: AsyncMutex<Option<HelperConnection>>,
    next_command_id: AtomicU64,
    startup_timing: Mutex<Option<StartupTimingState>>,
}

struct StartupTimingState {
    timing: super::model::AppleMusicStartupTiming,
    started: Instant,
    phase_started: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueCleanupResult {
    Deleted,
    NotFound,
    IdentityMismatch,
    Failed,
}

struct QueueCleanupAttempt {
    result: QueueCleanupResult,
    web_absent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppleQueuePlaybackTarget {
    pub playlist_name: String,
    pub persistent_id: String,
    pub database_ids: Vec<String>,
    pub accepted_count: usize,
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
    pub(crate) fn new(
        resource_dir: &Path,
        data_dir: &Path,
        cache_dir: &Path,
        installation_id: impl Into<String>,
    ) -> Self {
        let helper_path = helper_executable_path(resource_dir);
        let helper_present = helper_path.is_file();
        let queue_generation_store = AppleQueueGenerationStore::new(data_dir, cache_dir);
        let mut queue_generations = queue_generation_store.load();
        let startup_idle_since = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut released_stale_reference = false;
        for slot in AppleQueueSlot::ALL {
            if let Some(record) = queue_generations.slot_mut(slot).as_mut()
                && record.is_referenced()
            {
                record.active_references.clear();
                record.prepared_references.clear();
                record.idle_since = Some(startup_idle_since);
                released_stale_reference = true;
            }
        }
        for record in &mut queue_generations.retired {
            if record.is_referenced() {
                record.active_references.clear();
                record.prepared_references.clear();
                record.idle_since = Some(startup_idle_since);
                released_stale_reference = true;
            }
        }
        if released_stale_reference {
            let _ = queue_generation_store.save(&queue_generations);
        }
        let mut initial_status = AppleMusicMvpStatus::new(helper_present);
        initial_status.managed_playlists = super::model::AppleMusicManagedPlaylistStatus {
            active_slots: AppleQueueSlot::ALL
                .into_iter()
                .filter_map(|slot| queue_generations.slot(slot))
                .filter(|record| !record.installation_owner.is_empty())
                .count(),
            cleanup_pending: AppleQueueSlot::ALL
                .into_iter()
                .filter_map(|slot| queue_generations.slot(slot))
                .chain(queue_generations.retired.iter())
                .filter(|record| record.cleanup_pending)
                .count(),
            legacy_candidates: AppleQueueSlot::ALL
                .into_iter()
                .filter_map(|slot| queue_generations.slot(slot))
                .chain(queue_generations.retired.iter())
                .filter(|record| record.installation_owner.is_empty())
                .count(),
            ledger_path: queue_generation_store.path().to_string_lossy().to_string(),
            nested_folder_qualified: queue_generations.nested_folder_qualified,
            single_pass_cached_format_qualified: queue_generations
                .single_pass_cached_format_qualified,
        };
        Self {
            helper_path,
            runtime_root: cache_dir.join("apple-music"),
            status: Arc::new(Mutex::new(initial_status)),
            source_format_probe: Mutex::new(SourceFormatProbeState::default()),
            decoder_log_observer: Mutex::new(DecoderLogObserver::default()),
            playback_switch: AsyncMutex::new(()),
            installation_id: installation_id.into(),
            queue_generation_store,
            queue_generations: AsyncMutex::new(queue_generations),
            current_queue_target: Mutex::new(None),
            connection: AsyncMutex::new(None),
            next_command_id: AtomicU64::new(1),
            startup_timing: Mutex::new(None),
        }
    }

    pub(crate) fn begin_startup(&self) -> String {
        let startup_id = format!("startup-{}", random_hex(10));
        let now = Instant::now();
        let timing = super::model::AppleMusicStartupTiming {
            startup_id: startup_id.clone(),
            started_unix_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            phase_ms: Default::default(),
            total_ttfs_ms: None,
            outcome: "starting".to_string(),
        };
        *self.startup_timing.lock().unwrap() = Some(StartupTimingState {
            timing: timing.clone(),
            started: now,
            phase_started: now,
        });
        self.status.lock().unwrap().latest_startup = Some(timing);
        startup_id
    }

    pub(crate) fn mark_startup_phase(&self, startup_id: &str, phase: &'static str) {
        let mut active = self.startup_timing.lock().unwrap();
        let Some(active) = active
            .as_mut()
            .filter(|active| active.timing.startup_id == startup_id)
        else {
            return;
        };
        if active.timing.total_ttfs_ms.is_some() {
            return;
        }
        let now = Instant::now();
        active.timing.phase_ms.insert(
            phase.to_string(),
            now.duration_since(active.phase_started).as_millis() as u64,
        );
        active.phase_started = now;
        self.status.lock().unwrap().latest_startup = Some(active.timing.clone());
    }

    pub(crate) fn complete_startup(&self, startup_id: &str, succeeded: bool) {
        let mut active = self.startup_timing.lock().unwrap();
        let Some(active) = active
            .as_mut()
            .filter(|active| active.timing.startup_id == startup_id)
        else {
            return;
        };
        if active.timing.total_ttfs_ms.is_some() {
            return;
        }
        active.timing.total_ttfs_ms = Some(
            active
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        );
        active.timing.outcome = if succeeded {
            "output_started"
        } else {
            "failed"
        }
        .to_string();
        self.status.lock().unwrap().latest_startup = Some(active.timing.clone());
    }

    fn record_helper_startup_phases(
        &self,
        startup_id: &str,
        phases: &std::collections::BTreeMap<String, u64>,
    ) {
        let mut current = self.startup_timing.lock().unwrap();
        let Some(active) = current
            .as_mut()
            .filter(|active| active.timing.startup_id == startup_id)
        else {
            return;
        };
        for (phase, milliseconds) in phases {
            if matches!(
                phase.as_str(),
                "catalog_resolution"
                    | "playlist_create"
                    | "playlist_adoption"
                    | "server_verification"
            ) {
                active
                    .timing
                    .phase_ms
                    .insert(format!("helper_{phase}"), *milliseconds);
            }
        }
        self.status.lock().unwrap().latest_startup = Some(active.timing.clone());
    }

    pub(crate) fn current_queue_target(&self) -> Option<AppleQueuePlaybackTarget> {
        self.current_queue_target.lock().unwrap().clone()
    }

    pub(crate) fn format_context_fingerprint(&self) -> String {
        let helper_version = self.status().helper_version.or_else(|| {
            helper_info_plist_path(&self.helper_path)
                .map(|path| plist_value(&path, "CFBundleShortVersionString"))
        });
        queue_format_context(helper_version.as_deref())
    }

    pub(crate) async fn mark_current_queue_active(
        &self,
        zone_id: &str,
    ) -> Result<(), AppleMusicMvpError> {
        self.set_current_queue_owner(zone_id, AppleQueueOwnerKind::Active)
            .await
    }

    pub(crate) async fn mark_current_queue_paused(
        &self,
        zone_id: &str,
    ) -> Result<(), AppleMusicMvpError> {
        self.set_current_queue_owner(zone_id, AppleQueueOwnerKind::Paused)
            .await
    }

    async fn set_current_queue_owner(
        &self,
        zone_id: &str,
        kind: AppleQueueOwnerKind,
    ) -> Result<(), AppleMusicMvpError> {
        let Some(target) = self.current_queue_target() else {
            return Ok(());
        };
        let mut cache = self.queue_generations.lock().await;
        for slot in AppleQueueSlot::ALL {
            if let Some(record) = cache.slot_mut(slot).as_mut() {
                record
                    .active_references
                    .retain(|reference| reference.zone_id != zone_id);
                record
                    .prepared_references
                    .retain(|reference| reference.zone_id != zone_id);
                if record.music_app_persistent_id.as_deref() == Some(&target.persistent_id) {
                    record.idle_since = None;
                    let owner = AppleQueueOwnerRef::new(zone_id, kind);
                    if kind == AppleQueueOwnerKind::Prepared {
                        record.prepared_references.push(owner);
                    } else {
                        record.active_references.push(owner);
                    }
                }
            }
        }
        self.save_queue_cache(&cache)
    }

    pub(crate) async fn release_queue_owner(
        &self,
        zone_id: &str,
    ) -> Result<(), AppleMusicMvpError> {
        let mut cache = self.queue_generations.lock().await;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        for slot in AppleQueueSlot::ALL {
            if let Some(record) = cache.slot_mut(slot).as_mut() {
                let before = record.active_references.len() + record.prepared_references.len();
                record
                    .active_references
                    .retain(|reference| reference.zone_id != zone_id);
                record
                    .prepared_references
                    .retain(|reference| reference.zone_id != zone_id);
                let after = record.active_references.len() + record.prepared_references.len();
                if before != after && after == 0 {
                    record.idle_since = Some(now);
                }
            }
        }
        self.save_queue_cache(&cache)
    }

    /// Stage one immutable queue generation and wait until Music.app exposes
    /// enough of its verified prefix for safe startup.
    pub(crate) async fn stage_queue_generation(
        &self,
        song_ids: Vec<String>,
        startup_id: Option<&str>,
        zone_id: &str,
    ) -> Result<AppleQueuePlaybackTarget, AppleMusicMvpError> {
        if song_ids.is_empty() || song_ids.len() > MAX_QUEUE_PLAYLIST_LENGTH {
            return Err(error(
                "queue_stage_invalid",
                "Fozmo needs between 1 and 100 Apple Music tracks to stage a queue.",
                false,
                "queue_stage",
                true,
            ));
        }
        let song_ids = song_ids
            .into_iter()
            .map(|song_id| validate_catalog_id(song_id, "queue_song_id_invalid"))
            .collect::<Result<Vec<_>, _>>()?;
        let context = self.format_context_fingerprint();
        let fingerprint = content_fingerprint(&song_ids, &context);
        let mut cache = self.queue_generations.lock().await;
        if cache.retain_records_for_new_context(context) {
            self.save_queue_cache(&cache)?;
        }

        let reusable_slot = AppleQueueSlot::ALL.into_iter().find(|slot| {
            cache.slot(*slot).is_some_and(|record| {
                record.fingerprint == fingerprint
                    && record.requested_song_ids == song_ids
                    && record.installation_owner == self.installation_id
                    && !matches!(
                        record.lifecycle,
                        AppleQueueLifecycle::Quarantined | AppleQueueLifecycle::Retired
                    )
            })
        });
        if let Some(slot) = reusable_slot
            && let Some(record) = cache.slot(slot)
            && let Some(persistent_id) = record.music_app_persistent_id.clone()
        {
            let required = if record.accepted_entries.len() > 1 {
                2
            } else {
                1
            };
            let name = slot.playlist_name().to_string();
            let description = record.description();
            let known_id = persistent_id.clone();
            let observation = tokio::task::spawn_blocking(move || {
                queue_generation_observation(&name, &description, Some(&known_id), Some(required))
            })
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
            if let Some(observation) = observation {
                let prefix = visible_prefix(&record.accepted_entries, &observation.tracks);
                if prefix.length >= required {
                    let target = AppleQueuePlaybackTarget {
                        playlist_name: slot.playlist_name().to_string(),
                        persistent_id,
                        database_ids: prefix.database_ids,
                        accepted_count: record.accepted_entries.len(),
                    };
                    *self.current_queue_target.lock().unwrap() = Some(target.clone());
                    tracing::info!(
                        event = "apple_music_queue_generation_exact_reuse",
                        slot = slot.as_str(),
                        generation = record.generation,
                        "Reused a verified queue generation without launching the helper"
                    );
                    if let Some(startup_id) = startup_id {
                        self.mark_startup_phase(startup_id, "exact_generation_reuse");
                    }
                    mark_cache_owner(&mut cache, &target, zone_id, AppleQueueOwnerKind::Prepared);
                    self.save_queue_cache(&cache)?;
                    return Ok(target);
                }
            }
        }
        self.ensure_ready().await?;
        if let Some(startup_id) = startup_id {
            self.mark_startup_phase(startup_id, "helper_readiness");
        }
        let slot = if let Some(slot) = reusable_slot {
            slot
        } else {
            let current_slot =
                self.current_queue_target
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|target| {
                        AppleQueueSlot::ALL
                            .into_iter()
                            .find(|slot| slot.playlist_name() == target.playlist_name)
                    });
            let preferred = current_slot.map(AppleQueueSlot::other);
            preferred
                .filter(|slot| queue_slot_replaceable(cache.slot(*slot)))
                .or_else(|| {
                    AppleQueueSlot::ALL
                        .into_iter()
                        .find(|slot| Some(*slot) != current_slot && queue_slot_replaceable(cache.slot(*slot)))
                })
                .ok_or_else(|| {
                    error(
                        "queue_generation_slots_busy",
                        "Both immutable Apple Music queue slots are still owned by active or materializing generations.",
                        true,
                        "queue_stage",
                        true,
                    )
                })?
        };

        if reusable_slot.is_none() {
            if let Some(stale) = cache.slot(slot).cloned() {
                let (Some(web_playlist_id), Some(music_app_persistent_id)) = (
                    stale.web_playlist_id.clone(),
                    stale.music_app_persistent_id.clone(),
                ) else {
                    return Err(error(
                        "queue_generation_slot_not_deletable",
                        format!(
                            "The superseded {} queue generation is missing an Apple identity.",
                            slot.playlist_name()
                        ),
                        true,
                        "queue_cleanup",
                        true,
                    ));
                };
                let request = DeletionRequest {
                    slot,
                    generation: stale.generation.clone(),
                    description: stale.description(),
                    fingerprint: stale.fingerprint.clone(),
                    web_playlist_id,
                    music_app_persistent_id: music_app_persistent_id.clone(),
                    installation_owner: self.installation_id.clone(),
                    parent_folder_id: stale.parent_folder_id.clone(),
                    referenced_by_transition: false,
                    transport_cleanup_complete: true,
                };
                stale.deletion_guard(&request).map_err(|refusal| {
                    error(
                        "queue_generation_delete_refused",
                        format!(
                            "Fozmo refused to delete a superseded Apple queue generation: {refusal:?}"
                        ),
                        false,
                        "queue_cleanup",
                        true,
                    )
                })?;
                let attempt = self.cleanup_owned_record(&stale).await;
                if attempt.web_absent {
                    remove_record_by_web_id(&mut cache, stale.web_playlist_id.as_deref());
                    self.save_queue_cache(&cache)?;
                } else {
                    mark_record_cleanup_pending(&mut cache, stale.web_playlist_id.as_deref());
                    self.save_queue_cache(&cache)?;
                    return Err(error(
                        "queue_generation_delete_unconfirmed",
                        format!(
                            "Fozmo could not confirm removal of superseded {} ({:?}). The generation remains in the durable cleanup ledger.",
                            slot.playlist_name(),
                            attempt.result
                        ),
                        true,
                        "queue_cleanup",
                        true,
                    ));
                }
            }
            let format_context_fingerprint = cache.format_context_fingerprint.clone();
            let parent_folder_id = cache
                .nested_folder_qualified
                .then(|| cache.folder_web_id.clone())
                .flatten();
            cache.put(AppleQueueSlotRecord::plan(
                slot,
                song_ids.clone(),
                &format_context_fingerprint,
                &self.installation_id,
                parent_folder_id,
            ));
            self.save_queue_cache(&cache)?;
        }
        let record = cache
            .slot_mut(slot)
            .as_mut()
            .expect("the selected queue slot has a record");
        let attempt = record.next_attempt().map_err(stage_refusal_error)?;
        // Persist CreateSent before the helper can issue the POST.
        self.save_queue_cache(&cache)?;

        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
        let helper_stage = self.stage_or_adopt_queue(attempt, startup_id, progress_tx);
        tokio::pin!(helper_stage);
        let mut progress_open = true;
        let mut visibility_task: Option<
            tokio::task::JoinHandle<Result<MusicAppGenerationVisibility, String>>,
        > = None;
        let mut visibility_accepted = None;
        let result = loop {
            tokio::select! {
                helper_result = &mut helper_stage => {
                    break match helper_result {
                        Ok(result) => result,
                        Err(failure) => {
                            if let Some(task) = visibility_task.take() {
                                task.abort();
                            }
                            if matches!(
                                failure.code.as_str(),
                                "queue_generation_ambiguous" | "queue_generation_not_visible"
                            ) && let Some(record) = cache.slot_mut(slot).as_mut()
                            {
                                record.quarantine();
                                record.cleanup_pending = true;
                                record.idle_since = Some(unix_timestamp_secs());
                                let _ = self.queue_generation_store.save(&cache);
                            }
                            return Err(failure);
                        }
                    };
                }
                progress = progress_rx.recv(), if progress_open && visibility_task.is_none() => {
                    let Some(progress) = progress else {
                        progress_open = false;
                        continue;
                    };
                    let Some(created) = progress.stage_or_adopt else {
                        continue;
                    };
                    let Some(record) = cache.slot_mut(slot).as_mut() else {
                        continue;
                    };
                    let web_playlist_id = created
                        .web_playlist_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|id| !id.is_empty());
                    let progress_matches = created.operation_id == record.operation_id
                        && created.generation == record.generation
                        && created.fingerprint == record.fingerprint
                        && created.slot_name == record.slot.playlist_name()
                        && created.requested_count == record.requested_song_ids.len()
                        && !created.accepted_entries.is_empty()
                        && web_playlist_id.is_some();
                    if !progress_matches {
                        continue;
                    }
                    record.web_playlist_id = web_playlist_id.map(str::to_string);
                    record.accepted_entries = created.accepted_entries.clone();
                    record.rejected_song_ids = created.rejected_song_ids.clone();
                    let description = record.description();
                    let known_persistent_id = record.music_app_persistent_id.clone();
                    self.save_queue_cache(&cache)?;
                    visibility_accepted = Some(created.accepted_entries.clone());
                    visibility_task = Some(tokio::spawn(wait_for_music_app_generation_visibility(
                        slot,
                        created.generation,
                        description,
                        known_persistent_id,
                        created.accepted_entries,
                    )));
                }
            }
        };
        if let Some(startup_id) = startup_id {
            self.record_helper_startup_phases(startup_id, &result.phase_timings_ms);
        }
        if let Some(startup_id) = startup_id {
            self.mark_startup_phase(startup_id, "catalog_playlist_server");
        }
        if let Err(failure) = normalize_helper_result(&song_ids, &result) {
            quarantine_record(&mut cache, slot);
            let _ = self.queue_generation_store.save(&cache);
            return Err(normalization_error(failure));
        }
        if !result.rejected_song_ids.is_empty() {
            quarantine_record(&mut cache, slot);
            let _ = self.queue_generation_store.save(&cache);
            return Err(error(
                "queue_tracks_rejected",
                format!(
                    "Apple Music rejected {} track(s) from Fozmo's immutable queue.",
                    result.rejected_song_ids.len()
                ),
                false,
                "queue_stage",
                true,
            ));
        }
        if let Err(failure) =
            verify_server_order(&result.accepted_entries, &result.server_catalog_ids)
        {
            quarantine_record(&mut cache, slot);
            let _ = self.queue_generation_store.save(&cache);
            return Err(error(
                "queue_server_order_mismatch",
                format!("Apple Music stored a different immutable queue order: {failure:?}"),
                false,
                "queue_stage",
                true,
            ));
        }
        tracing::info!(
            event = "apple_music_queue_generation_server_verified",
            slot = slot.as_str(),
            generation = result.generation,
            requested = result.requested_count,
            accepted = result.accepted_entries.len(),
            rejected = result.rejected_song_ids.len(),
            web_playlist_id = result.web_playlist_id.as_deref().unwrap_or_default(),
            "Verified the immutable Apple Music queue generation on Apple's server"
        );
        let (accepted, description, persistent_id) = {
            let record = cache
                .slot_mut(slot)
                .as_mut()
                .expect("the staged queue slot still exists");
            if let Err(refusal) = record.adopt(&result) {
                record.quarantine();
                let failure = stage_refusal_error(refusal);
                let _ = self.queue_generation_store.save(&cache);
                return Err(failure);
            }
            (
                record.accepted_entries.clone(),
                record.description(),
                record.music_app_persistent_id.clone(),
            )
        };
        self.save_queue_cache(&cache)?;

        if visibility_accepted.as_ref() != Some(&accepted) {
            if let Some(task) = visibility_task.take() {
                task.abort();
            }
        }
        let visibility_task = visibility_task.unwrap_or_else(|| {
            tokio::spawn(wait_for_music_app_generation_visibility(
                slot,
                result.generation.clone(),
                description,
                persistent_id,
                accepted.clone(),
            ))
        });
        let visibility = visibility_task
            .await
            .map_err(|join_error| {
                error(
                    "queue_music_app_poll_stopped",
                    format!("The Music.app queue poll stopped: {join_error}"),
                    true,
                    "queue_visibility",
                    true,
                )
            })?
            .map_err(|message| {
                error(
                    "queue_generation_not_visible_in_music_app",
                    message,
                    true,
                    "queue_visibility",
                    true,
                )
            })?;
        let target = AppleQueuePlaybackTarget {
            playlist_name: slot.playlist_name().to_string(),
            persistent_id: visibility.persistent_id,
            database_ids: visibility.database_ids.clone(),
            accepted_count: accepted.len(),
        };
        let record = cache
            .slot_mut(slot)
            .as_mut()
            .expect("the visible queue slot still exists");
        record.music_app_persistent_id = Some(target.persistent_id.clone());
        record.music_app_database_ids = visibility.database_ids;
        self.save_queue_cache(&cache)?;
        *self.current_queue_target.lock().unwrap() = Some(target.clone());
        mark_cache_owner(&mut cache, &target, zone_id, AppleQueueOwnerKind::Prepared);
        self.save_queue_cache(&cache)?;
        if let Some(startup_id) = startup_id {
            self.mark_startup_phase(startup_id, "music_app_visibility");
        }
        Ok(target)
    }

    fn save_queue_cache(
        &self,
        cache: &AppleQueueGenerationCache,
    ) -> Result<(), AppleMusicMvpError> {
        self.queue_generation_store.save(cache).map_err(|message| {
            error(
                "queue_generation_cache_failed",
                message,
                true,
                "queue_stage",
                true,
            )
        })?;
        let records = AppleQueueSlot::ALL
            .into_iter()
            .filter_map(|slot| cache.slot(slot))
            .collect::<Vec<_>>();
        self.status.lock().unwrap().managed_playlists =
            super::model::AppleMusicManagedPlaylistStatus {
                active_slots: records
                    .iter()
                    .filter(|record| record.installation_owner == self.installation_id)
                    .count(),
                cleanup_pending: records
                    .iter()
                    .copied()
                    .chain(cache.retired.iter())
                    .filter(|record| {
                        record.installation_owner == self.installation_id && record.cleanup_pending
                    })
                    .count(),
                legacy_candidates: records
                    .iter()
                    .copied()
                    .chain(cache.retired.iter())
                    .filter(|record| record.installation_owner.is_empty())
                    .count(),
                ledger_path: self
                    .queue_generation_store
                    .path()
                    .to_string_lossy()
                    .to_string(),
                nested_folder_qualified: cache.nested_folder_qualified,
                single_pass_cached_format_qualified: cache.single_pass_cached_format_qualified,
            };
        Ok(())
    }

    pub(crate) async fn single_pass_cached_format_qualified(&self) -> bool {
        self.queue_generations
            .lock()
            .await
            .single_pass_cached_format_qualified
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

    pub(crate) async fn prepare_music_app_decoder_observer(
        &self,
    ) -> Result<(), AppleMusicMvpError> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let pid = loop {
            if let Some(pid) = music_app_pid() {
                break pid;
            }
            if Instant::now() >= deadline {
                return Err(error(
                    "music_app_not_running",
                    "Music.app did not become available for decoder observation.",
                    true,
                    "source_format",
                    true,
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        self.decoder_log_observer
            .lock()
            .unwrap()
            .ensure(pid)
            .map_err(|message| {
                error(
                    "music_app_source_format_observer_failed",
                    message,
                    true,
                    "source_format",
                    true,
                )
            })
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
            if remaining.is_zero() {
                break;
            }
            let query = self
                .decoder_log_observer
                .lock()
                .unwrap()
                .latest_after(music_app_pid, boundary);
            match query {
                Ok(detection) => {
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
                Err(message) => last_error = Some(message),
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
        if self.status().authorization == "authorized" {
            let _ = self.ensure_queue_folder().await;
            let _ = timeout(
                Duration::from_secs(5),
                self.cleanup_queue_playlists(u64::MAX, &[]),
            )
            .await;
        }
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
        if self.status().authorization == "authorized" {
            let _ = self.ensure_queue_folder().await;
            let _ = timeout(
                Duration::from_secs(5),
                self.cleanup_queue_playlists(u64::MAX, &[]),
            )
            .await;
        }
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

    async fn stage_or_adopt_queue(
        &self,
        attempt: StageOrAdoptAttempt,
        startup_id: Option<&str>,
        progress: mpsc::UnboundedSender<HelperMessage>,
    ) -> Result<StageOrAdoptResult, AppleMusicMvpError> {
        let session_id = self.session_id().await?;
        let mut command = HelperMessage::command(
            attempt.operation_id.clone(),
            "stage_or_adopt_queue",
            session_id,
        );
        command.operation_id = Some(attempt.operation_id.clone());
        command.generation = Some(attempt.generation);
        command.slot = Some(attempt.slot);
        command.fingerprint = Some(attempt.fingerprint);
        command.song_ids = attempt.requested_song_ids;
        command.allow_create = Some(attempt.allow_create);
        command.installation_owner = Some(attempt.installation_owner);
        command.parent_folder_id = attempt.parent_folder_id;
        command.known_web_playlist_id = attempt.known_web_playlist_id;
        command.startup_id = startup_id.map(str::to_string);
        let event = self
            .send_serialized_and_wait(
                attempt.operation_id,
                &command,
                &["queue_generation_staged"],
                STAGE_OR_ADOPT_TIMEOUT,
                Some(&progress),
            )
            .await?;
        event.stage_or_adopt.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned an empty queue-generation response.",
                false,
                "queue_stage",
                true,
            )
        })
    }

    async fn get_library_playlist(
        &self,
        web_playlist_id: &str,
    ) -> Result<Option<AppleLibraryPlaylistIdentity>, AppleMusicMvpError> {
        self.ensure_ready().await?;
        let mut command = self.next_command("get_library_playlist").await?;
        command.web_playlist_id = Some(web_playlist_id.to_string());
        let event = self.send_and_wait(command, &["library_playlist"]).await?;
        Ok(event.library_playlist)
    }

    async fn ensure_queue_folder(&self) -> Result<(), AppleMusicMvpError> {
        if self
            .queue_generations
            .lock()
            .await
            .folder_web_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
        {
            return Ok(());
        }
        self.ensure_ready().await?;
        let mut command = self.next_command("ensure_queue_folder").await?;
        command.installation_owner = Some(self.installation_id.clone());
        let event = self.send_and_wait(command, &["queue_folder"]).await?;
        let folder = event.library_playlist.ok_or_else(|| {
            error(
                "helper_protocol_mismatch",
                "The Apple Music helper returned no queue-folder identity.",
                false,
                "queue_folder",
                true,
            )
        })?;
        let mut cache = self.queue_generations.lock().await;
        cache.folder_web_id = Some(folder.id);
        // Qualification is deliberately independent. Merely creating the
        // folder does not prove Music.app can play/delete nested children.
        self.save_queue_cache(&cache)
    }

    async fn server_queue_playlist_inventory(
        &self,
    ) -> Result<Vec<AppleLibraryPlaylistIdentity>, AppleMusicMvpError> {
        self.ensure_ready().await?;
        let command = self.next_command("queue_playlist_inventory").await?;
        let event = self
            .send_and_wait(command, &["queue_playlist_inventory"])
            .await?;
        Ok(event.playlist_inventory)
    }

    pub(crate) async fn queue_playlist_inventory(
        &self,
    ) -> Result<AppleQueuePlaylistInventory, AppleMusicMvpError> {
        let server = self.server_queue_playlist_inventory().await?;
        let cache = self.queue_generations.lock().await;
        let records = AppleQueueSlot::ALL
            .into_iter()
            .filter_map(|slot| cache.slot(slot))
            .chain(cache.retired.iter())
            .cloned()
            .collect::<Vec<_>>();
        drop(cache);

        let mut inventory = AppleQueuePlaylistInventory::default();
        let mut seen = std::collections::HashSet::new();
        for playlist in server {
            seen.insert(playlist.id.clone());
            let record = records
                .iter()
                .find(|record| record.web_playlist_id.as_deref() == Some(&playlist.id));
            let parsed_owner = playlist
                .description
                .as_deref()
                .and_then(parse_generation_description)
                .map(|(owner, _, _, _)| owner);
            let item = AppleQueuePlaylistItem {
                web_playlist_id: playlist.id,
                name: playlist.name,
                description: playlist.description,
                music_app_persistent_id: record
                    .and_then(|record| record.music_app_persistent_id.clone()),
                installation_owner: parsed_owner.clone(),
                lifecycle: record
                    .map(|record| format!("{:?}", record.lifecycle).to_ascii_lowercase())
                    .unwrap_or_else(|| "untracked".to_string()),
                referenced: record.is_some_and(AppleQueueSlotRecord::is_referenced),
                cleanup_pending: record.is_some_and(|record| record.cleanup_pending),
            };
            match parsed_owner {
                Some(owner) if owner == self.installation_id => inventory.owned.push(item),
                Some(_) => inventory.foreign_installations.push(item),
                None => inventory.legacy_candidates.push(item),
            }
        }
        // Keep local tombstones visible even while Apple is still converging
        // or a helper lookup could not discover the resource.
        for record in records {
            let Some(web_playlist_id) = record.web_playlist_id.clone() else {
                continue;
            };
            if seen.contains(&web_playlist_id) {
                continue;
            }
            let item = AppleQueuePlaylistItem {
                web_playlist_id,
                name: record.slot.playlist_name().to_string(),
                description: Some(record.description()),
                music_app_persistent_id: record.music_app_persistent_id.clone(),
                installation_owner: (!record.installation_owner.is_empty())
                    .then(|| record.installation_owner.clone()),
                lifecycle: format!("{:?}", record.lifecycle).to_ascii_lowercase(),
                referenced: record.is_referenced(),
                cleanup_pending: record.cleanup_pending,
            };
            if record.installation_owner == self.installation_id {
                inventory.owned.push(item);
            } else if record.installation_owner.is_empty() {
                inventory.legacy_candidates.push(item);
            } else {
                inventory.foreign_installations.push(item);
            }
        }
        Ok(inventory)
    }

    pub(crate) async fn cleanup_queue_playlists(
        &self,
        minimum_idle_secs: u64,
        selected_legacy_web_ids: &[String],
    ) -> Result<AppleQueuePlaylistInventory, AppleMusicMvpError> {
        self.reconcile_untracked_owned().await?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let candidates = {
            let cache = self.queue_generations.lock().await;
            AppleQueueSlot::ALL
                .into_iter()
                .filter_map(|slot| cache.slot(slot))
                .chain(cache.retired.iter())
                .filter(|record| {
                    record.installation_owner == self.installation_id
                        && !record.is_referenced()
                        && (record.cleanup_pending
                            || record.idle_since.is_some_and(|idle_since| {
                                now.saturating_sub(idle_since) >= minimum_idle_secs
                            }))
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        for record in candidates {
            let attempt = self.cleanup_owned_record(&record).await;
            tracing::info!(
                event = "apple_music_queue_cleanup_attempt",
                generation = record.generation,
                slot = record.slot.as_str(),
                result = ?attempt.result,
                web_absent = attempt.web_absent,
                "Attempted confirmed Apple Music queue cleanup"
            );
            let mut cache = self.queue_generations.lock().await;
            if attempt.web_absent {
                remove_record_by_web_id(&mut cache, record.web_playlist_id.as_deref());
            } else {
                mark_record_cleanup_pending(&mut cache, record.web_playlist_id.as_deref());
            }
            self.save_queue_cache(&cache)?;
        }

        if !selected_legacy_web_ids.is_empty() {
            self.cleanup_selected_legacy(selected_legacy_web_ids)
                .await?;
        }
        self.queue_playlist_inventory().await
    }

    async fn reconcile_untracked_owned(&self) -> Result<(), AppleMusicMvpError> {
        let server = self.server_queue_playlist_inventory().await?;
        let mut cache = self.queue_generations.lock().await;
        let known = AppleQueueSlot::ALL
            .into_iter()
            .filter_map(|slot| cache.slot(slot))
            .chain(cache.retired.iter())
            .filter_map(|record| record.web_playlist_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut added = false;
        for playlist in server {
            if known.contains(&playlist.id) {
                continue;
            }
            let Some(description) = playlist.description.as_deref() else {
                continue;
            };
            let Some((owner, slot, generation, fingerprint)) =
                parse_generation_description(description)
            else {
                continue;
            };
            if owner != self.installation_id {
                continue;
            }
            let mut matched_existing = false;
            for candidate_slot in AppleQueueSlot::ALL {
                if let Some(record) = cache.slot_mut(candidate_slot).as_mut()
                    && record.description() == description
                {
                    record.web_playlist_id = Some(playlist.id.clone());
                    record.lifecycle = AppleQueueLifecycle::Retired;
                    record.cleanup_pending = true;
                    matched_existing = true;
                    added = true;
                    break;
                }
            }
            if !matched_existing
                && let Some(record) = cache
                    .retired
                    .iter_mut()
                    .find(|record| record.description() == description)
            {
                record.web_playlist_id = Some(playlist.id.clone());
                record.lifecycle = AppleQueueLifecycle::Retired;
                record.cleanup_pending = true;
                matched_existing = true;
                added = true;
            }
            if matched_existing {
                continue;
            }
            cache
                .retired
                .push(AppleQueueSlotRecord::untracked_cleanup_tombstone(
                    owner,
                    slot,
                    generation,
                    fingerprint,
                    playlist.id,
                ));
            added = true;
        }
        if added {
            self.save_queue_cache(&cache)?;
        }
        Ok(())
    }

    async fn cleanup_owned_record(&self, record: &AppleQueueSlotRecord) -> QueueCleanupAttempt {
        let Some(web_playlist_id) = record.web_playlist_id.as_deref() else {
            return QueueCleanupAttempt {
                result: QueueCleanupResult::Failed,
                web_absent: false,
            };
        };
        let expected_description = record.description();
        match self.get_library_playlist(web_playlist_id).await {
            Ok(None) => {
                return QueueCleanupAttempt {
                    result: QueueCleanupResult::NotFound,
                    web_absent: true,
                };
            }
            Ok(Some(playlist))
                if playlist.name != record.slot.playlist_name()
                    || playlist.description.as_deref() != Some(&expected_description)
                    || !playlist.can_edit =>
            {
                return QueueCleanupAttempt {
                    result: QueueCleanupResult::IdentityMismatch,
                    web_absent: false,
                };
            }
            Err(_) => {
                return QueueCleanupAttempt {
                    result: QueueCleanupResult::Failed,
                    web_absent: false,
                };
            }
            Ok(Some(_)) => {}
        }
        let persistent_id = if let Some(persistent_id) = record.music_app_persistent_id.clone() {
            persistent_id
        } else {
            let name = record.slot.playlist_name().to_string();
            let description = expected_description.clone();
            let observation = tokio::task::spawn_blocking(move || {
                queue_generation_observation(&name, &description, None, Some(1))
            })
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
            let Some(observation) = observation else {
                return QueueCleanupAttempt {
                    result: QueueCleanupResult::NotFound,
                    web_absent: false,
                };
            };
            observation.persistent_id
        };
        let name = record.slot.playlist_name().to_string();
        let description = expected_description;
        let deletion = tokio::task::spawn_blocking(move || {
            delete_queue_generation(&name, &description, &persistent_id)
        })
        .await;
        match deletion {
            Ok(Ok(QueueGenerationDeletion::Deleted)) => {
                let deadline = Instant::now() + Duration::from_secs(20);
                loop {
                    match self.get_library_playlist(web_playlist_id).await {
                        Ok(None) => {
                            return QueueCleanupAttempt {
                                result: QueueCleanupResult::Deleted,
                                web_absent: true,
                            };
                        }
                        Ok(Some(_)) if Instant::now() < deadline => {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                        _ => {
                            return QueueCleanupAttempt {
                                result: QueueCleanupResult::Deleted,
                                web_absent: false,
                            };
                        }
                    }
                }
            }
            Ok(Ok(QueueGenerationDeletion::NotFound)) => QueueCleanupAttempt {
                // The Web identity was independently confirmed present above.
                result: QueueCleanupResult::NotFound,
                web_absent: false,
            },
            _ => QueueCleanupAttempt {
                result: QueueCleanupResult::Failed,
                web_absent: false,
            },
        }
    }

    async fn cleanup_selected_legacy(
        &self,
        selected_web_ids: &[String],
    ) -> Result<(), AppleMusicMvpError> {
        let server = self.server_queue_playlist_inventory().await?;
        for selected in selected_web_ids {
            let Some(playlist) = server.iter().find(|playlist| &playlist.id == selected) else {
                continue;
            };
            // Owner-scoped v5 resources, including foreign installations, are
            // never accepted through the legacy escape hatch.
            if playlist
                .description
                .as_deref()
                .and_then(parse_generation_description)
                .is_some()
                || !playlist.can_edit
            {
                continue;
            }
            if server
                .iter()
                .filter(|candidate| {
                    candidate.name == playlist.name && candidate.description == playlist.description
                })
                .count()
                != 1
            {
                continue;
            }
            let description = playlist.description.clone().unwrap_or_default();
            let name = playlist.name.clone();
            let observation = {
                let name = name.clone();
                let description = description.clone();
                tokio::task::spawn_blocking(move || {
                    queue_generation_observation(&name, &description, None, Some(1))
                })
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
            };
            let Some(observation) = observation else {
                continue;
            };
            let persistent_id = observation.persistent_id;
            let _ = tokio::task::spawn_blocking(move || {
                delete_queue_generation(&name, &description, &persistent_id)
            })
            .await;
        }
        Ok(())
    }

    /// Whether Apple will accept the library writes the queue playlist needs.
    ///
    /// Adding subscription tracks to a library requires Sync Library, and
    /// neither MusicKit nor AppleScript exposes that setting, so the only
    /// honest check is asking Apple to list the user's playlists.
    #[allow(dead_code)]
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
                "The Apple Music helper does not match this Fozmo build. Rebuild or reinstall the helper, then restart Fozmo.",
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
        // Only unreferenced records are eligible. The short deadline keeps
        // shutdown bounded; unfinished attempts remain durable tombstones.
        let _ = timeout(Duration::from_secs(3), self.cleanup_queue_playlists(0, &[])).await;
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
        self.send_serialized_and_wait(
            command_id,
            &command,
            expected_events,
            HELPER_COMMAND_TIMEOUT,
            None,
        )
        .await
    }

    async fn send_serialized_and_wait<T: serde::Serialize>(
        &self,
        command_id: String,
        command: &T,
        expected_events: &[&str],
        command_timeout: Duration,
        progress: Option<&mpsc::UnboundedSender<HelperMessage>>,
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

        let deadline = Instant::now() + command_timeout;
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
            if event.message_type == "queue_generation_created" {
                if let Some(progress) = progress {
                    let _ = progress.send(event);
                }
                continue;
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

struct MusicAppGenerationVisibility {
    persistent_id: String,
    database_ids: Vec<String>,
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn wait_for_music_app_generation_visibility(
    slot: AppleQueueSlot,
    generation: String,
    description: String,
    mut persistent_id: Option<String>,
    accepted: Vec<super::model::AppleQueueEntry>,
) -> Result<MusicAppGenerationVisibility, String> {
    let readiness = StartupReadinessPolicy {
        has_native_successor: accepted.len() > 1,
        later_visible_adoption_qualified: false,
    };
    let required = readiness.required_entries(accepted.len());
    let deadline = Instant::now() + QUEUE_GENERATION_VISIBLE_TIMEOUT;
    let mut last_prefix = 0;
    let mut logged_prefix = usize::MAX;
    loop {
        let name = slot.playlist_name().to_string();
        let generation_description = description.clone();
        let known_id = persistent_id.clone();
        let observation = tokio::task::spawn_blocking(move || {
            queue_generation_observation(
                &name,
                &generation_description,
                known_id.as_deref(),
                Some(required),
            )
        })
        .await
        .map_err(|join_error| format!("Music.app queue observation stopped: {join_error}"))??;
        if let Some(QueueGenerationObservation {
            persistent_id: observed_id,
            tracks,
        }) = observation
        {
            persistent_id = Some(observed_id.clone());
            let prefix = visible_prefix(&accepted, &tracks);
            last_prefix = prefix.length;
            if prefix.length != logged_prefix {
                tracing::info!(
                    event = "apple_music_queue_generation_visible_prefix",
                    slot = slot.as_str(),
                    generation,
                    visible = prefix.length,
                    required,
                    accepted = accepted.len(),
                    stopped_by = ?prefix.stopped_by,
                    "Music.app's verified immutable queue prefix changed"
                );
                logged_prefix = prefix.length;
            }
            if prefix.length >= required {
                return Ok(MusicAppGenerationVisibility {
                    persistent_id: observed_id,
                    database_ids: prefix.database_ids,
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Music.app exposed {last_prefix} of the {required} verified queue entries required for startup within 60 seconds."
            ));
        }
        tokio::time::sleep(QUEUE_GENERATION_VISIBLE_POLL).await;
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

fn helper_info_plist_path(helper_executable: &Path) -> Option<PathBuf> {
    helper_app_bundle_path(helper_executable).map(|bundle| bundle.join("Contents/Info.plist"))
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

fn queue_slot_replaceable(record: Option<&AppleQueueSlotRecord>) -> bool {
    record.is_none_or(|record| {
        !record.is_referenced()
            && !matches!(
                record.lifecycle,
                AppleQueueLifecycle::CreateSent | AppleQueueLifecycle::Planned
            )
    })
}

fn mark_cache_owner(
    cache: &mut AppleQueueGenerationCache,
    target: &AppleQueuePlaybackTarget,
    zone_id: &str,
    kind: AppleQueueOwnerKind,
) {
    for slot in AppleQueueSlot::ALL {
        if let Some(record) = cache.slot_mut(slot).as_mut() {
            record
                .active_references
                .retain(|reference| reference.zone_id != zone_id);
            record
                .prepared_references
                .retain(|reference| reference.zone_id != zone_id);
            if record.music_app_persistent_id.as_deref() == Some(&target.persistent_id) {
                record.idle_since = None;
                let owner = AppleQueueOwnerRef::new(zone_id, kind);
                if kind == AppleQueueOwnerKind::Prepared {
                    record.prepared_references.push(owner);
                } else {
                    record.active_references.push(owner);
                }
            }
        }
    }
}

fn remove_record_by_web_id(cache: &mut AppleQueueGenerationCache, web_playlist_id: Option<&str>) {
    let Some(web_playlist_id) = web_playlist_id else {
        return;
    };
    for slot in AppleQueueSlot::ALL {
        if cache
            .slot(slot)
            .and_then(|record| record.web_playlist_id.as_deref())
            == Some(web_playlist_id)
        {
            *cache.slot_mut(slot) = None;
        }
    }
    cache
        .retired
        .retain(|record| record.web_playlist_id.as_deref() != Some(web_playlist_id));
}

fn mark_record_cleanup_pending(
    cache: &mut AppleQueueGenerationCache,
    web_playlist_id: Option<&str>,
) {
    let Some(web_playlist_id) = web_playlist_id else {
        return;
    };
    for slot in AppleQueueSlot::ALL {
        if let Some(record) = cache.slot_mut(slot).as_mut()
            && record.web_playlist_id.as_deref() == Some(web_playlist_id)
        {
            record.cleanup_pending = true;
        }
    }
    for record in &mut cache.retired {
        if record.web_playlist_id.as_deref() == Some(web_playlist_id) {
            record.cleanup_pending = true;
        }
    }
}

fn quarantine_record(cache: &mut AppleQueueGenerationCache, slot: AppleQueueSlot) {
    if let Some(record) = cache.slot_mut(slot).as_mut() {
        record.quarantine();
    }
}

fn queue_format_context(helper_version: Option<&str>) -> String {
    let macos_build = command_value("sw_vers", &["-buildVersion"]);
    let music_version = plist_value(
        Path::new("/System/Applications/Music.app/Contents/Info.plist"),
        "CFBundleShortVersionString",
    );
    let music_build = plist_value(
        Path::new("/System/Applications/Music.app/Contents/Info.plist"),
        "CFBundleVersion",
    );
    let driver_build = plist_value(
        Path::new("/Library/Audio/Plug-Ins/HAL/FozmoCapture.driver/Contents/Info.plist"),
        "CFBundleVersion",
    );
    format!(
        "macos={macos_build};music={music_version};music_build={music_build};helper_protocol={PROTOCOL_VERSION};helper={};driver={driver_build}",
        helper_version.unwrap_or("unknown")
    )
}

fn plist_value(path: &Path, key: &str) -> String {
    command_value(
        "plutil",
        &[
            "-extract",
            key,
            "raw",
            "-o",
            "-",
            path.to_str().unwrap_or_default(),
        ],
    )
}

fn command_value(program: &str, arguments: &[&str]) -> String {
    StdCommand::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn stage_refusal_error(refusal: StageRefusal) -> AppleMusicMvpError {
    let message = match refusal {
        StageRefusal::Mismatched { detail } => {
            format!("The Apple Music helper returned a mismatched queue generation: {detail}.")
        }
        StageRefusal::Quarantined => {
            "This Apple Music queue generation was quarantined after an ambiguous create."
                .to_string()
        }
    };
    error(
        "queue_generation_refused",
        message,
        false,
        "queue_stage",
        true,
    )
}

fn normalization_error(failure: NormalizationFailure) -> AppleMusicMvpError {
    match failure {
        NormalizationFailure::HeadRejected { song_id } => error(
            "queue_head_rejected",
            format!("Apple Music rejected the requested first queue track {song_id}."),
            false,
            "queue_stage",
            true,
        ),
        NormalizationFailure::UnrequestedEntries { song_ids } => error(
            "queue_unrequested_entries",
            format!(
                "Apple Music returned {} queue entries Fozmo did not request.",
                song_ids.len()
            ),
            false,
            "queue_stage",
            true,
        ),
    }
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

    #[test]
    fn music_app_not_found_keeps_the_generation_cleanup_pending() {
        let mut cache = AppleQueueGenerationCache::default();
        let mut record = AppleQueueSlotRecord::plan(
            AppleQueueSlot::A,
            vec!["1".to_string()],
            "context",
            "00000000-0000-4000-8000-000000000001",
            None,
        );
        record.web_playlist_id = Some("p.web".to_string());
        record.music_app_persistent_id = Some("PID".to_string());
        cache.put(record);

        mark_record_cleanup_pending(&mut cache, Some("p.web"));

        let retained = cache.slot_a.expect("the identity must remain durable");
        assert!(retained.cleanup_pending);
        assert_eq!(retained.web_playlist_id.as_deref(), Some("p.web"));
    }
}
