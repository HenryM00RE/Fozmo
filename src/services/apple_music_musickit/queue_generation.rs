//! Immutable, generation-scoped Apple Music queue playlists (protocol v4).
//!
//! The v3 design kept one playlist called `Fozmo` and rebuilt it in place. Two
//! things went wrong with that, both of them unfixable without changing the
//! model:
//!
//! * **Ambiguous creates.** When a create request's response was lost — a
//!   helper restart, a timeout — a retry had no way to tell "Apple never got
//!   it" from "Apple got it and the reply vanished". Retrying created a second
//!   playlist; not retrying hung. Because every playlist had the same name,
//!   neither the retry nor a later cleanup could tell them apart.
//! * **Delete-to-edit.** A queue change meant deleting the playlist and making
//!   a new one, so the identity Fozmo was playing from could be destroyed by a
//!   background queue edit in another zone.
//!
//! v4 fixes both by making a playlist generation *immutable and named after
//! itself*. Two slots, `Fozmo A` and `Fozmo B`, alternate so a new generation
//! never touches the one currently playing. Each generation carries an opaque
//! token and a fingerprint of its contents in the playlist description, which
//! gives a retry something exact to search for. A retry is therefore a
//! read-only adoption, never a second create — and if an ambiguous create never
//! becomes discoverable, the generation is quarantined rather than duplicated.
//!
//! Nothing here appends. The whole safe island goes into the create request, so
//! there is no second POST whose lost response would raise the same ambiguity a
//! layer down.

use super::model::AppleQueueEntry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Protocol tag embedded in every generation description.
pub(crate) const QUEUE_PROTOCOL_TAG: &str = "fozmo.queue.v4";

/// Transport timeout for a stage-or-adopt round trip.
///
/// Deliberately longer than the generic 20-second helper timeout and than the
/// 60-second head-readiness deadline, because a stage/adopt that times out on
/// the transport is exactly the ambiguous case: Apple may still be creating the
/// playlist. Giving the helper room to answer converts most would-be
/// ambiguities into plain results.
pub(crate) const STAGE_OR_ADOPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(65);

/// Which of the two alternating playlist slots a generation occupies.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppleQueueSlot {
    A,
    B,
}

impl AppleQueueSlot {
    pub(crate) const ALL: [AppleQueueSlot; 2] = [AppleQueueSlot::A, AppleQueueSlot::B];

    /// The Music.app and library playlist name for this slot.
    pub(crate) fn playlist_name(self) -> &'static str {
        match self {
            Self::A => "Fozmo A",
            Self::B => "Fozmo B",
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }

    pub(crate) fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// How far a generation has got, and whether a retry may create.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppleQueueLifecycle {
    /// Persisted locally; the helper has not been contacted.
    Planned,
    /// The helper was allowed to issue the POST. Whether Apple received it is
    /// unknown from here, so no later attempt may create.
    CreateSent,
    /// A playlist with this exact generation description exists and its
    /// contents were verified.
    Materialized,
    /// An ambiguous create never became discoverable, or two playlists shared
    /// one generation description. Held for guarded cleanup, never played.
    Quarantined,
    /// Superseded and safe to delete once no reference remains.
    Retired,
}

/// What is holding a generation open.
///
/// Deletion is refused while any of these exist. "Not currently playing" is not
/// one of the states, because a paused or parked generation is exactly the one
/// a resume needs to find intact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppleQueueOwnerKind {
    Materializing,
    Prepared,
    Active,
    Paused,
    Parked,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(crate) struct AppleQueueOwnerRef {
    pub zone_id: String,
    pub kind: AppleQueueOwnerKind,
}

impl AppleQueueOwnerRef {
    pub(crate) fn new(zone_id: impl Into<String>, kind: AppleQueueOwnerKind) -> Self {
        Self {
            zone_id: zone_id.into(),
            kind,
        }
    }
}

/// Everything known about one generation in one slot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct AppleQueueSlotRecord {
    pub slot: AppleQueueSlot,
    /// Stable across every attempt for this generation, so the helper can
    /// coalesce concurrent work by generation rather than by a transient
    /// command ID that changes on each retry.
    pub operation_id: String,
    pub generation: String,
    pub fingerprint: String,
    pub requested_song_ids: Vec<String>,
    #[serde(default)]
    pub accepted_entries: Vec<AppleQueueEntry>,
    #[serde(default)]
    pub rejected_song_ids: Vec<String>,
    /// Apple Music API library-playlist ID. Used for Web API inspection and
    /// ownership verification.
    #[serde(default)]
    pub web_playlist_id: Option<String>,
    /// Music.app playlist persistent ID. Used for playback, observation, and
    /// guarded deletion. The two identities are not interchangeable and both
    /// must match before anything is deleted.
    #[serde(default)]
    pub music_app_persistent_id: Option<String>,
    pub lifecycle: AppleQueueLifecycle,
    #[serde(default)]
    pub active_references: Vec<AppleQueueOwnerRef>,
    #[serde(default)]
    pub prepared_references: Vec<AppleQueueOwnerRef>,
    /// Positional Music.app database IDs, for transport matching.
    #[serde(default)]
    pub music_app_database_ids: Vec<String>,
}

impl AppleQueueSlotRecord {
    /// Plan a generation for `requested_song_ids` without contacting anyone.
    pub(crate) fn plan(
        slot: AppleQueueSlot,
        requested_song_ids: Vec<String>,
        format_context_fingerprint: &str,
    ) -> Self {
        let generation = new_generation_token();
        let fingerprint = content_fingerprint(&requested_song_ids, format_context_fingerprint);
        Self {
            slot,
            operation_id: format!("stage-{generation}"),
            generation,
            fingerprint,
            requested_song_ids,
            accepted_entries: Vec::new(),
            rejected_song_ids: Vec::new(),
            web_playlist_id: None,
            music_app_persistent_id: None,
            lifecycle: AppleQueueLifecycle::Planned,
            active_references: Vec::new(),
            prepared_references: Vec::new(),
            music_app_database_ids: Vec::new(),
        }
    }

    /// The exact playlist description that identifies this generation.
    pub(crate) fn description(&self) -> String {
        generation_description(self.slot, &self.generation, &self.fingerprint)
    }

    pub(crate) fn is_referenced(&self) -> bool {
        !self.active_references.is_empty() || !self.prepared_references.is_empty()
    }
}

/// The exact description string a generation is identified by.
///
/// The retry path searches for this verbatim, so it must be built in one place
/// and never reformatted.
pub(crate) fn generation_description(
    slot: AppleQueueSlot,
    generation: &str,
    fingerprint: &str,
) -> String {
    format!(
        "{QUEUE_PROTOCOL_TAG};slot={};generation={generation};fingerprint={fingerprint}",
        slot.as_str()
    )
}

/// Parse a description back into its parts, or `None` when it is not a v4
/// generation description at all.
pub(crate) fn parse_generation_description(
    description: &str,
) -> Option<(AppleQueueSlot, String, String)> {
    let mut parts = description.trim().split(';');
    if parts.next()? != QUEUE_PROTOCOL_TAG {
        return None;
    }
    let (mut slot, mut generation, mut fingerprint) = (None, None, None);
    for part in parts {
        let (key, value) = part.split_once('=')?;
        match key {
            "slot" => {
                slot = match value {
                    "A" => Some(AppleQueueSlot::A),
                    "B" => Some(AppleQueueSlot::B),
                    _ => return None,
                }
            }
            "generation" => generation = Some(value.to_string()),
            "fingerprint" => fingerprint = Some(value.to_string()),
            _ => return None,
        }
    }
    Some((slot?, generation?, fingerprint?))
}

/// An opaque, unguessable generation token.
///
/// Formatted like a UUID purely so it is recognizable in Apple's UI and in
/// logs; nothing parses the shape.
fn new_generation_token() -> String {
    let bytes = rand::random::<u128>().to_be_bytes();
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Hash of exactly what this generation is supposed to contain.
///
/// Includes the format context so a generation staged under one macOS or
/// Music.app build is never adopted after that context changed underneath it.
pub(crate) fn content_fingerprint(song_ids: &[String], format_context_fingerprint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(QUEUE_PROTOCOL_TAG.as_bytes());
    hasher.update(b"\0");
    hasher.update(format_context_fingerprint.as_bytes());
    for song_id in song_ids {
        hasher.update(b"\0");
        hasher.update(song_id.as_bytes());
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One stage-or-adopt attempt, as handed to the helper.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct StageOrAdoptAttempt {
    pub operation_id: String,
    pub generation: String,
    pub slot: AppleQueueSlot,
    pub fingerprint: String,
    pub requested_song_ids: Vec<String>,
    /// True only for the very first attempt at this generation. Every later
    /// attempt is a read-only adoption, which is what makes a lost create
    /// response survivable without risking a duplicate.
    pub allow_create: bool,
}

/// What the helper answered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct StageOrAdoptResult {
    pub slot_name: String,
    pub generation: String,
    pub operation_id: String,
    pub fingerprint: String,
    pub requested_count: usize,
    pub accepted_entries: Vec<AppleQueueEntry>,
    pub rejected_song_ids: Vec<String>,
    pub web_playlist_id: Option<String>,
    /// Ordered catalog IDs read back from the Web API playlist relationship.
    ///
    /// This is intentionally independent of `accepted_entries`: comparing the
    /// two is what proves Apple stored the complete requested order.
    #[serde(default)]
    pub server_catalog_ids: Vec<Option<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StageRefusal {
    /// The helper answered about a different generation or fingerprint.
    Mismatched { detail: String },
    /// The generation was quarantined and must not be used.
    Quarantined,
}

/// Refusal reasons for a guarded delete, each naming the specific mismatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DeletionRefusal {
    SlotMismatch,
    GenerationMismatch,
    FingerprintMismatch,
    WebPlaylistIdMismatch,
    MusicAppPersistentIdMismatch,
    MissingIdentity,
    Referenced(AppleQueueOwnerKind),
    TransitionReferenced,
    TransportCleanupIncomplete,
}

/// Every identity a caller must present to delete a generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeletionRequest {
    pub slot: AppleQueueSlot,
    pub generation: String,
    pub description: String,
    pub fingerprint: String,
    pub web_playlist_id: String,
    pub music_app_persistent_id: String,
    /// Whether any coordinator transition still names this generation.
    pub referenced_by_transition: bool,
    /// Whether Apple transport cleanup has finished for this generation.
    pub transport_cleanup_complete: bool,
}

impl AppleQueueSlotRecord {
    /// Build the next attempt at this generation.
    ///
    /// The single most important line in this module is `allow_create`: it is
    /// true exactly once, when the lifecycle is still `Planned`. Calling this
    /// transitions the record to `CreateSent` *before* the helper is contacted,
    /// so a crash between here and the POST still leaves a record that forbids
    /// creating.
    pub(crate) fn next_attempt(&mut self) -> Result<StageOrAdoptAttempt, StageRefusal> {
        if self.lifecycle == AppleQueueLifecycle::Quarantined {
            return Err(StageRefusal::Quarantined);
        }
        let allow_create = self.lifecycle == AppleQueueLifecycle::Planned;
        if allow_create {
            self.lifecycle = AppleQueueLifecycle::CreateSent;
        }
        Ok(StageOrAdoptAttempt {
            operation_id: self.operation_id.clone(),
            generation: self.generation.clone(),
            slot: self.slot,
            fingerprint: self.fingerprint.clone(),
            requested_song_ids: self.requested_song_ids.clone(),
            allow_create,
        })
    }

    /// Fold a helper result into the record.
    pub(crate) fn adopt(&mut self, result: &StageOrAdoptResult) -> Result<(), StageRefusal> {
        if result.operation_id != self.operation_id {
            return Err(StageRefusal::Mismatched {
                detail: format!(
                    "the helper answered for operation {} rather than {}",
                    result.operation_id, self.operation_id
                ),
            });
        }
        if result.generation != self.generation {
            return Err(StageRefusal::Mismatched {
                detail: format!(
                    "the helper answered for generation {} rather than {}",
                    result.generation, self.generation
                ),
            });
        }
        if result.fingerprint != self.fingerprint {
            return Err(StageRefusal::Mismatched {
                detail: "the helper answered with a different content fingerprint".to_string(),
            });
        }
        if result.slot_name != self.slot.playlist_name() {
            return Err(StageRefusal::Mismatched {
                detail: format!(
                    "the helper answered for playlist {} rather than {}",
                    result.slot_name,
                    self.slot.playlist_name()
                ),
            });
        }
        if result.requested_count != self.requested_song_ids.len() {
            return Err(StageRefusal::Mismatched {
                detail: format!(
                    "the helper answered for {} requested tracks rather than {}",
                    result.requested_count,
                    self.requested_song_ids.len()
                ),
            });
        }
        if result
            .web_playlist_id
            .as_deref()
            .is_none_or(|playlist_id| playlist_id.trim().is_empty())
        {
            return Err(StageRefusal::Mismatched {
                detail: "the helper returned no Apple Music Web API playlist identity".to_string(),
            });
        }
        if result.accepted_entries.is_empty() {
            return Err(StageRefusal::Mismatched {
                detail: "the helper returned an empty accepted queue".to_string(),
            });
        }
        self.accepted_entries = result.accepted_entries.clone();
        self.rejected_song_ids = result.rejected_song_ids.clone();
        self.web_playlist_id = result.web_playlist_id.clone();
        self.lifecycle = AppleQueueLifecycle::Materialized;
        Ok(())
    }

    /// Give up on an ambiguous create without risking a duplicate.
    pub(crate) fn quarantine(&mut self) {
        self.lifecycle = AppleQueueLifecycle::Quarantined;
    }

    /// Whether this generation may be deleted, and if not, why.
    ///
    /// Every identity must match. Checking only the name, or only "is it
    /// playing", is how the previous design managed to delete a playlist out
    /// from under a paused zone.
    pub(crate) fn deletion_guard(&self, request: &DeletionRequest) -> Result<(), DeletionRefusal> {
        if request.slot != self.slot {
            return Err(DeletionRefusal::SlotMismatch);
        }
        if request.generation != self.generation || request.description != self.description() {
            return Err(DeletionRefusal::GenerationMismatch);
        }
        if request.fingerprint != self.fingerprint {
            return Err(DeletionRefusal::FingerprintMismatch);
        }
        let (Some(web_playlist_id), Some(persistent_id)) = (
            self.web_playlist_id.as_deref(),
            self.music_app_persistent_id.as_deref(),
        ) else {
            return Err(DeletionRefusal::MissingIdentity);
        };
        if request.web_playlist_id != web_playlist_id {
            return Err(DeletionRefusal::WebPlaylistIdMismatch);
        }
        if request.music_app_persistent_id != persistent_id {
            return Err(DeletionRefusal::MusicAppPersistentIdMismatch);
        }
        if self.lifecycle == AppleQueueLifecycle::CreateSent {
            // Still materializing: Apple may yet publish the playlist this
            // record describes, and deleting the record's claim on it would
            // orphan the result.
            return Err(DeletionRefusal::Referenced(
                AppleQueueOwnerKind::Materializing,
            ));
        }
        if let Some(reference) = self
            .active_references
            .iter()
            .chain(self.prepared_references.iter())
            .next()
        {
            return Err(DeletionRefusal::Referenced(reference.kind));
        }
        if request.referenced_by_transition {
            return Err(DeletionRefusal::TransitionReferenced);
        }
        if !request.transport_cleanup_complete {
            return Err(DeletionRefusal::TransportCleanupIncomplete);
        }
        Ok(())
    }
}

/// The durable half of the protocol.
///
/// Persisted before the helper is contacted so that a crash mid-create still
/// leaves behind a record that forbids a second create. Holds no credentials —
/// only generation identities, accepted entries, references, and the format
/// context — so it lives in the cache directory rather than anywhere secret.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AppleQueueGenerationCache {
    #[serde(default)]
    pub file_version: u32,
    #[serde(default)]
    pub slot_a: Option<AppleQueueSlotRecord>,
    #[serde(default)]
    pub slot_b: Option<AppleQueueSlotRecord>,
    /// Fingerprint of the macOS/Music.app/helper/driver context these records
    /// were staged under.
    #[serde(default)]
    pub format_context_fingerprint: String,
}

const CACHE_FILE_VERSION: u32 = 4;

impl AppleQueueGenerationCache {
    pub(crate) fn slot(&self, slot: AppleQueueSlot) -> Option<&AppleQueueSlotRecord> {
        match slot {
            AppleQueueSlot::A => self.slot_a.as_ref(),
            AppleQueueSlot::B => self.slot_b.as_ref(),
        }
    }

    pub(crate) fn slot_mut(&mut self, slot: AppleQueueSlot) -> &mut Option<AppleQueueSlotRecord> {
        match slot {
            AppleQueueSlot::A => &mut self.slot_a,
            AppleQueueSlot::B => &mut self.slot_b,
        }
    }

    pub(crate) fn put(&mut self, record: AppleQueueSlotRecord) {
        let slot = record.slot;
        *self.slot_mut(slot) = Some(record);
    }

    /// The slot a new generation may use.
    ///
    /// Never the slot holding a referenced generation, which is what keeps a
    /// background queue edit in one zone from disturbing playback in another.
    pub(crate) fn reserve_inactive_slot(&self) -> Option<AppleQueueSlot> {
        AppleQueueSlot::ALL.into_iter().find(|slot| {
            self.slot(*slot)
                .is_none_or(|record| !record.is_referenced())
        })
    }
}

/// File-backed store for the generation cache.
pub(crate) struct AppleQueueGenerationStore {
    path: PathBuf,
}

impl AppleQueueGenerationStore {
    /// `<cache>/apple-music/queue-generations-v4.json`.
    pub(crate) fn new(cache_dir: &Path) -> Self {
        Self {
            path: cache_dir
                .join("apple-music")
                .join("queue-generations-v4.json"),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Read the cache, treating an unreadable or unparsable file as empty.
    ///
    /// A corrupt cache must not stop playback: the worst case is that a
    /// generation is re-staged, and the exact-description search will adopt any
    /// playlist the lost record described rather than duplicating it.
    pub(crate) fn load(&self) -> AppleQueueGenerationCache {
        let Ok(bytes) = fs::read(&self.path) else {
            return AppleQueueGenerationCache::default();
        };
        serde_json::from_slice::<AppleQueueGenerationCache>(&bytes)
            .ok()
            .filter(|cache| cache.file_version == CACHE_FILE_VERSION)
            .unwrap_or_default()
    }

    /// Write through a temporary file, flush, then rename.
    ///
    /// The rename is what makes the record durable at exactly one instant: a
    /// crash either leaves the previous cache or the new one, never a
    /// half-written record that would make a create look un-sent.
    pub(crate) fn save(&self, cache: &AppleQueueGenerationCache) -> Result<(), String> {
        let mut cache = cache.clone();
        cache.file_version = CACHE_FILE_VERSION;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "the Apple queue cache path has no directory".to_string())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create the Apple queue cache directory: {error}"))?;
        let bytes = serde_json::to_vec_pretty(&cache)
            .map_err(|error| format!("serialize the Apple queue cache: {error}"))?;
        let temporary = self.path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temporary)
                .map_err(|error| format!("write the Apple queue cache: {error}"))?;
            file.write_all(&bytes)
                .map_err(|error| format!("write the Apple queue cache: {error}"))?;
            file.flush()
                .map_err(|error| format!("flush the Apple queue cache: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("sync the Apple queue cache: {error}"))?;
        }
        fs::rename(&temporary, &self.path)
            .map_err(|error| format!("replace the Apple queue cache: {error}"))
    }
}

/// What normalization did to a queue after the helper answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RejectedTrackNormalization {
    /// Rejected tail song IDs, to be removed from the effective queue.
    pub removed_song_ids: Vec<String>,
    pub requested_count: usize,
    pub accepted_count: usize,
    pub rejected_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NormalizationFailure {
    /// Apple would not accept the track the user asked to play. There is
    /// nothing to fall back to, and the playlist must not be activated.
    HeadRejected { song_id: String },
    /// The helper answered about tracks nobody requested.
    UnrequestedEntries { song_ids: Vec<String> },
}

/// Validate a helper result and work out what the queue must lose.
///
/// Runs before any transition token is created, so a rejected tail track never
/// becomes some token's `next_item_id` and then fails to exist.
pub(crate) fn normalize_helper_result(
    requested_song_ids: &[String],
    result: &StageOrAdoptResult,
) -> Result<RejectedTrackNormalization, NormalizationFailure> {
    let requested = requested_song_ids.iter().collect::<BTreeSet<_>>();
    let unrequested = result
        .accepted_entries
        .iter()
        .map(|entry| &entry.song_id)
        .filter(|song_id| !requested.contains(*song_id))
        .cloned()
        .collect::<Vec<_>>();
    if !unrequested.is_empty() {
        return Err(NormalizationFailure::UnrequestedEntries {
            song_ids: unrequested,
        });
    }
    let accepted = result
        .accepted_entries
        .iter()
        .map(|entry| entry.song_id.as_str())
        .collect::<BTreeSet<_>>();
    let Some(head) = requested_song_ids.first() else {
        return Ok(RejectedTrackNormalization {
            removed_song_ids: Vec::new(),
            requested_count: 0,
            accepted_count: 0,
            rejected_count: 0,
        });
    };
    if !accepted.contains(head.as_str()) {
        return Err(NormalizationFailure::HeadRejected {
            song_id: head.clone(),
        });
    }
    let removed_song_ids = requested_song_ids
        .iter()
        .filter(|song_id| !accepted.contains(song_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    Ok(RejectedTrackNormalization {
        requested_count: requested_song_ids.len(),
        accepted_count: result.accepted_entries.len(),
        rejected_count: removed_song_ids.len(),
        removed_song_ids,
    })
}

/// A library playlist as the Web API describes it, for adoption search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LibraryPlaylistSummary {
    pub web_playlist_id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AdoptionOutcome {
    /// Exactly one playlist carries this generation description.
    Adopted(LibraryPlaylistSummary),
    /// No playlist carries it. The caller keeps waiting; only the first attempt
    /// was ever allowed to create.
    NotFound,
    /// More than one playlist carries the exact same generation description.
    /// Fail closed: playing either could be the wrong one, and deleting either
    /// could destroy the right one.
    AmbiguousDuplicate(Vec<LibraryPlaylistSummary>),
}

/// Find the playlist for one generation among every library playlist.
///
/// Matches the slot name *and* the exact description. Two playlists sharing a
/// slot name but carrying different generation descriptions is the ordinary
/// case during a handoff and resolves cleanly; two sharing the exact same
/// description is not something Fozmo can create on purpose, so it quarantines.
pub(crate) fn resolve_generation(
    playlists: &[LibraryPlaylistSummary],
    slot: AppleQueueSlot,
    description: &str,
) -> AdoptionOutcome {
    let matches = playlists
        .iter()
        .filter(|playlist| {
            playlist.name == slot.playlist_name()
                && playlist.description.as_deref() == Some(description)
        })
        .cloned()
        .collect::<Vec<_>>();
    match matches.len() {
        0 => AdoptionOutcome::NotFound,
        1 => AdoptionOutcome::Adopted(matches.into_iter().next().expect("one match")),
        _ => AdoptionOutcome::AmbiguousDuplicate(matches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(song_id: &str) -> AppleQueueEntry {
        AppleQueueEntry {
            song_id: song_id.to_string(),
            title: song_id.to_string(),
            artist: "Artist".to_string(),
            duration_secs: Some(200.0),
            catalog_song_id: Some(song_id.to_string()),
            album_title: Some("Album".to_string()),
            disc_number: Some(1),
            track_number: Some(1),
            storefront: Some("nz".to_string()),
        }
    }

    fn record(song_ids: &[&str]) -> AppleQueueSlotRecord {
        AppleQueueSlotRecord::plan(
            AppleQueueSlot::A,
            song_ids.iter().map(|id| id.to_string()).collect(),
            "context-1",
        )
    }

    fn result_for(record: &AppleQueueSlotRecord, accepted: &[&str]) -> StageOrAdoptResult {
        StageOrAdoptResult {
            slot_name: record.slot.playlist_name().to_string(),
            generation: record.generation.clone(),
            operation_id: record.operation_id.clone(),
            fingerprint: record.fingerprint.clone(),
            requested_count: record.requested_song_ids.len(),
            accepted_entries: accepted.iter().map(|id| entry(id)).collect(),
            rejected_song_ids: record
                .requested_song_ids
                .iter()
                .filter(|id| !accepted.contains(&id.as_str()))
                .cloned()
                .collect(),
            web_playlist_id: Some("p.web-1".to_string()),
            server_catalog_ids: accepted.iter().map(|id| Some((*id).to_string())).collect(),
        }
    }

    /// The lost-response case. The original helper task did create the
    /// playlist; the retry must find it rather than make a second one.
    #[test]
    fn a_lost_create_response_is_adopted_rather_than_recreated() {
        let mut record = record(&["1", "2"]);
        let first = record.next_attempt().expect("a first attempt");
        assert!(first.allow_create);

        // The response never arrives, but Apple did create the playlist.
        let created = LibraryPlaylistSummary {
            web_playlist_id: "p.web-1".to_string(),
            name: record.slot.playlist_name().to_string(),
            description: Some(record.description()),
        };

        let retry = record.next_attempt().expect("a retry");
        assert!(!retry.allow_create);
        assert_eq!(retry.generation, first.generation);
        assert_eq!(retry.operation_id, first.operation_id);
        assert_eq!(
            resolve_generation(
                std::slice::from_ref(&created),
                record.slot,
                &record.description()
            ),
            AdoptionOutcome::Adopted(created)
        );
    }

    /// A helper restart loses in-memory state but not the persisted record, and
    /// the record is what forbids creating.
    #[test]
    fn a_helper_restart_between_create_and_response_keeps_retries_adoption_only() {
        let mut record = record(&["1"]);
        record.next_attempt().expect("a first attempt");

        for _ in 0..5 {
            assert!(!record.next_attempt().expect("a retry").allow_create);
        }
    }

    /// Concurrent commands carry different transient IDs; the generation token
    /// and operation ID are what the helper coalesces on.
    #[test]
    fn concurrent_attempts_share_one_generation_and_operation_id() {
        let mut record = record(&["1", "2"]);
        let first = record.next_attempt().expect("a first attempt");
        let second = record.next_attempt().expect("a second attempt");

        assert_eq!(first.generation, second.generation);
        assert_eq!(first.operation_id, second.operation_id);
    }

    #[test]
    fn a_quarantined_generation_refuses_every_further_attempt() {
        let mut record = record(&["1"]);
        record.next_attempt().expect("a first attempt");
        record.quarantine();

        assert_eq!(record.next_attempt(), Err(StageRefusal::Quarantined));
    }

    /// The ordinary handoff: slot A holds the outgoing generation and a new one
    /// arrives. Same name, different description, and both resolve.
    #[test]
    fn playlists_sharing_a_slot_name_resolve_by_generation_description() {
        let old = record(&["1"]);
        let mut new = record(&["2"]);
        new.generation = new_generation_token();
        new.fingerprint = content_fingerprint(&new.requested_song_ids, "context-1");
        let playlists = vec![
            LibraryPlaylistSummary {
                web_playlist_id: "p.old".to_string(),
                name: AppleQueueSlot::A.playlist_name().to_string(),
                description: Some(old.description()),
            },
            LibraryPlaylistSummary {
                web_playlist_id: "p.new".to_string(),
                name: AppleQueueSlot::A.playlist_name().to_string(),
                description: Some(new.description()),
            },
        ];

        assert!(matches!(
            resolve_generation(&playlists, AppleQueueSlot::A, &old.description()),
            AdoptionOutcome::Adopted(found) if found.web_playlist_id == "p.old"
        ));
        assert!(matches!(
            resolve_generation(&playlists, AppleQueueSlot::A, &new.description()),
            AdoptionOutcome::Adopted(found) if found.web_playlist_id == "p.new"
        ));
    }

    /// Two playlists with the identical generation description means Fozmo
    /// cannot tell which one it built. Playing either risks the wrong content;
    /// deleting either risks the right one.
    #[test]
    fn duplicate_exact_generation_descriptions_fail_closed() {
        let record = record(&["1"]);
        let duplicate = |id: &str| LibraryPlaylistSummary {
            web_playlist_id: id.to_string(),
            name: AppleQueueSlot::A.playlist_name().to_string(),
            description: Some(record.description()),
        };

        let outcome = resolve_generation(
            &[duplicate("p.one"), duplicate("p.two")],
            AppleQueueSlot::A,
            &record.description(),
        );

        assert!(matches!(outcome, AdoptionOutcome::AmbiguousDuplicate(found) if found.len() == 2));
    }

    #[test]
    fn a_generation_description_round_trips() {
        let record = record(&["1", "2"]);
        let description = record.description();

        assert_eq!(
            parse_generation_description(&description),
            Some((
                AppleQueueSlot::A,
                record.generation.clone(),
                record.fingerprint.clone()
            ))
        );
        assert_eq!(parse_generation_description("Fozmo"), None);
    }

    #[test]
    fn the_fingerprint_covers_content_and_format_context() {
        let ids = vec!["1".to_string(), "2".to_string()];
        let reordered = vec!["2".to_string(), "1".to_string()];

        assert_ne!(
            content_fingerprint(&ids, "context-1"),
            content_fingerprint(&reordered, "context-1")
        );
        assert_ne!(
            content_fingerprint(&ids, "context-1"),
            content_fingerprint(&ids, "context-2")
        );
    }

    #[test]
    fn adoption_requires_the_stable_operation_and_requested_count() {
        let mut record = record(&["1", "2"]);
        record.next_attempt().expect("an attempt");
        let mut wrong_operation = result_for(&record, &["1", "2"]);
        wrong_operation.operation_id = "stage-other".to_string();
        assert!(matches!(
            record.adopt(&wrong_operation),
            Err(StageRefusal::Mismatched { .. })
        ));

        let mut wrong_count = result_for(&record, &["1", "2"]);
        wrong_count.requested_count = 1;
        assert!(matches!(
            record.adopt(&wrong_count),
            Err(StageRefusal::Mismatched { .. })
        ));
    }

    fn deletable() -> (AppleQueueSlotRecord, DeletionRequest) {
        let mut record = record(&["1"]);
        record.next_attempt().expect("an attempt");
        let result = result_for(&record, &["1"]);
        record.adopt(&result).expect("adoption");
        record.music_app_persistent_id = Some("PID-1".to_string());
        let request = DeletionRequest {
            slot: record.slot,
            generation: record.generation.clone(),
            description: record.description(),
            fingerprint: record.fingerprint.clone(),
            web_playlist_id: "p.web-1".to_string(),
            music_app_persistent_id: "PID-1".to_string(),
            referenced_by_transition: false,
            transport_cleanup_complete: true,
        };
        (record, request)
    }

    #[test]
    fn a_fully_matching_unreferenced_generation_may_be_deleted() {
        let (record, request) = deletable();

        assert_eq!(record.deletion_guard(&request), Ok(()));
    }

    /// "Not currently playing" is not enough: a paused or parked generation is
    /// exactly the one a resume needs to still exist.
    #[test]
    fn deletion_is_refused_while_any_reference_remains() {
        for kind in [
            AppleQueueOwnerKind::Prepared,
            AppleQueueOwnerKind::Active,
            AppleQueueOwnerKind::Paused,
            AppleQueueOwnerKind::Parked,
        ] {
            let (mut record, request) = deletable();
            record
                .active_references
                .push(AppleQueueOwnerRef::new("zone-1", kind));

            assert_eq!(
                record.deletion_guard(&request),
                Err(DeletionRefusal::Referenced(kind))
            );
        }
    }

    #[test]
    fn deletion_is_refused_while_the_generation_is_still_materializing() {
        let (mut record, request) = deletable();
        record.lifecycle = AppleQueueLifecycle::CreateSent;

        assert_eq!(
            record.deletion_guard(&request),
            Err(DeletionRefusal::Referenced(
                AppleQueueOwnerKind::Materializing
            ))
        );
    }

    #[test]
    fn deletion_is_refused_while_a_transition_or_transport_still_names_it() {
        let (record, mut request) = deletable();
        request.referenced_by_transition = true;
        assert_eq!(
            record.deletion_guard(&request),
            Err(DeletionRefusal::TransitionReferenced)
        );

        let (record, mut request) = deletable();
        request.transport_cleanup_complete = false;
        assert_eq!(
            record.deletion_guard(&request),
            Err(DeletionRefusal::TransportCleanupIncomplete)
        );
    }

    /// The two Apple identities serve different purposes and both are required:
    /// the Web ID proves ownership, the persistent ID names what Music.app will
    /// actually delete.
    #[test]
    fn any_identity_mismatch_blocks_deletion() {
        type DeletionCase = (fn(&mut DeletionRequest), DeletionRefusal);
        let cases: [DeletionCase; 5] = [
            (
                |request| request.slot = AppleQueueSlot::B,
                DeletionRefusal::SlotMismatch,
            ),
            (
                |request| request.generation = "other".to_string(),
                DeletionRefusal::GenerationMismatch,
            ),
            (
                |request| request.fingerprint = "deadbeef".to_string(),
                DeletionRefusal::FingerprintMismatch,
            ),
            (
                |request| request.web_playlist_id = "p.other".to_string(),
                DeletionRefusal::WebPlaylistIdMismatch,
            ),
            (
                |request| request.music_app_persistent_id = "PID-2".to_string(),
                DeletionRefusal::MusicAppPersistentIdMismatch,
            ),
        ];

        for (mutate, expected) in cases {
            let (record, mut request) = deletable();
            mutate(&mut request);

            assert_eq!(record.deletion_guard(&request), Err(expected));
        }
    }

    #[test]
    fn deletion_is_refused_before_both_apple_identities_are_known() {
        let (mut record, request) = deletable();
        record.music_app_persistent_id = None;

        assert_eq!(
            record.deletion_guard(&request),
            Err(DeletionRefusal::MissingIdentity)
        );
    }

    /// Apple refusing the track the user actually asked for is not something a
    /// queue edit can paper over.
    #[test]
    fn a_rejected_head_fails_before_the_playlist_is_activated() {
        let record = record(&["1", "2", "3"]);
        let result = result_for(&record, &["2", "3"]);

        assert_eq!(
            normalize_helper_result(&record.requested_song_ids, &result),
            Err(NormalizationFailure::HeadRejected {
                song_id: "1".to_string()
            })
        );
    }

    #[test]
    fn a_rejected_tail_is_normalized_out_of_the_effective_queue() {
        let record = record(&["1", "2", "3"]);
        let result = result_for(&record, &["1", "3"]);

        assert_eq!(
            normalize_helper_result(&record.requested_song_ids, &result),
            Ok(RejectedTrackNormalization {
                removed_song_ids: vec!["2".to_string()],
                requested_count: 3,
                accepted_count: 2,
                rejected_count: 1,
            })
        );
    }

    #[test]
    fn entries_nobody_requested_are_refused() {
        let record = record(&["1"]);
        let mut result = result_for(&record, &["1"]);
        result.accepted_entries.push(entry("99"));

        assert_eq!(
            normalize_helper_result(&record.requested_song_ids, &result),
            Err(NormalizationFailure::UnrequestedEntries {
                song_ids: vec!["99".to_string()]
            })
        );
    }

    /// A new generation must never land in the slot a referenced one occupies —
    /// that is the whole point of having two.
    #[test]
    fn a_referenced_slot_is_never_reserved_for_a_new_generation() {
        let mut cache = AppleQueueGenerationCache::default();
        let mut held = record(&["1"]);
        held.active_references.push(AppleQueueOwnerRef::new(
            "zone-1",
            AppleQueueOwnerKind::Active,
        ));
        cache.put(held);

        assert_eq!(cache.reserve_inactive_slot(), Some(AppleQueueSlot::B));
    }

    #[test]
    fn both_slots_referenced_leaves_nothing_to_reserve() {
        let mut cache = AppleQueueGenerationCache::default();
        for slot in AppleQueueSlot::ALL {
            let mut held = record(&["1"]);
            held.slot = slot;
            held.prepared_references.push(AppleQueueOwnerRef::new(
                "zone",
                AppleQueueOwnerKind::Prepared,
            ));
            cache.put(held);
        }

        assert_eq!(cache.reserve_inactive_slot(), None);
    }

    #[test]
    fn the_cache_round_trips_through_an_atomic_write() {
        let directory =
            std::env::temp_dir().join(format!("fozmo-queue-generations-{}", rand::random::<u64>()));
        let store = AppleQueueGenerationStore::new(&directory);
        let mut cache = AppleQueueGenerationCache {
            format_context_fingerprint: "context-1".to_string(),
            ..AppleQueueGenerationCache::default()
        };
        cache.put(record(&["1", "2"]));

        store.save(&cache).expect("the cache writes");
        let loaded = store.load();

        assert_eq!(loaded.slot_a, cache.slot_a);
        assert_eq!(loaded.format_context_fingerprint, "context-1");
        assert!(
            store
                .path()
                .ends_with("apple-music/queue-generations-v4.json")
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_unreadable_cache_reads_as_empty_rather_than_failing() {
        let directory = std::env::temp_dir().join(format!(
            "fozmo-queue-generations-bad-{}",
            rand::random::<u64>()
        ));
        let store = AppleQueueGenerationStore::new(&directory);
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), b"{ not json").unwrap();

        assert_eq!(store.load(), AppleQueueGenerationCache::default());
        let _ = fs::remove_dir_all(&directory);
    }
}
