//! Playback occurrence identities, revisions, and boundary tokens.
//!
//! Everything Fozmo used to identify by provider source key alone — "the Apple
//! track that is playing", "the next queue entry" — is ambiguous the moment a
//! queue holds the same track twice, which a shuffled album or a repeated radio
//! seed produces routinely. A boundary that commits against a source key can
//! therefore pop the wrong occurrence, and a late event from a cancelled
//! transition can commit against a queue that has already moved.
//!
//! This module supplies the identities that make those races decidable:
//!
//! * [`PlaybackItemId`] distinguishes two occurrences of the same track.
//! * [`ZoneRevision`] and [`OutputConfigRevision`] fence control operations.
//! * [`RenderConfigFingerprint`] fences prepared audio against a render path
//!   that changed after preparation.
//! * [`BoundaryToken`] binds all of them to one transition so a `Started` event
//!   is only ever committed by the transition that armed it.
//!
//! Nothing here reaches into the engine. The types are deliberately plain so
//! that the coordinator, the Player, and the Apple path can all hold them
//! without a dependency cycle.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-local identity for one occurrence of a track in a queue.
///
/// Two entries for the same song get different IDs, so "advance past the item
/// that just ended" cannot be satisfied by a later duplicate. IDs are never
/// persisted: a restored queue is assigned fresh ones, which is why no database
/// migration is required.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PlaybackItemId(u64);

impl PlaybackItemId {
    /// The next unused identity.
    ///
    /// Monotonic for the life of the process, so an ID that outlives its queue
    /// can never collide with a later one and silently match.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Construct a specific identity. Tests use this to pin ordering; the
    /// running system always allocates through [`PlaybackItemId::next`].
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PlaybackItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "item:{}", self.0)
    }
}

/// A queue occurrence: one identity plus the source it plays.
///
/// The source key stays a plain `String` because every provider already speaks
/// that form; introducing a parallel key type would only add conversions at
/// each boundary without making anything more decidable.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackItem {
    pub id: PlaybackItemId,
    pub source: crate::protocol::SourceRef,
}

impl PlaybackItem {
    /// Assign a fresh identity to `source`.
    pub fn new(source: crate::protocol::SourceRef) -> Self {
        Self {
            id: PlaybackItemId::next(),
            source,
        }
    }

    pub fn source_key(&self) -> String {
        self.source.key()
    }

    /// Assign fresh identities to a whole queue.
    ///
    /// Called when a queue is accepted or restored, which is the only moment at
    /// which occurrence identity is allowed to be minted.
    pub fn assign_all(
        sources: impl IntoIterator<Item = crate::protocol::SourceRef>,
    ) -> Vec<PlaybackItem> {
        sources.into_iter().map(PlaybackItem::new).collect()
    }
}

/// Monotonic per-zone counter invalidating everything prepared before it.
///
/// Incremented by play, every queue mutation, natural boundary commit, next,
/// stop, seek, loop-mode change, pause/resume scheduling, transfer, zone
/// disable, and output or DSP configuration change. Selecting a different zone
/// in the UI is not a playback event and does not increment it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ZoneRevision(pub u64);

impl ZoneRevision {
    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for ZoneRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "zone_rev:{}", self.0)
    }
}

/// Monotonic per-Player counter for output-device and render configuration.
///
/// Separate from [`ZoneRevision`] because a prepared item can survive a queue
/// edit that does not touch it, but can never survive the render path changing
/// underneath it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OutputConfigRevision(pub u64);

impl OutputConfigRevision {
    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Which physical output opening a frame cursor belongs to.
///
/// Frame counters restart whenever CoreAudio hands back a new stream, so a raw
/// frame number only means something paired with the generation that produced
/// it. Incremented every time a physical output is opened.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OutputGeneration(pub u64);

impl OutputGeneration {
    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Everything about the render path that prepared audio depends on.
///
/// A prepared decoder and renderer are only valid while every one of these
/// holds. Comparing the whole fingerprint rather than, say, just the device and
/// rate is what stops a prepared DSD item from being installed after the user
/// switched the modulator or resized the DSP buffer.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Serialize)]
pub struct RenderConfigFingerprint {
    /// Stable identity of the physical device, not its display name.
    pub output_device_uid: String,
    pub target_rate_hz: u32,
    pub exclusive_mode: bool,
    /// `"pcm"` or `"dsd"`; the transport below distinguishes DoP from native.
    pub pcm_dsd_mode: String,
    pub upsampling: String,
    pub filter: String,
    pub dsp_buffer_frames: u32,
    /// Hash rather than the settings themselves: an EQ curve is large and only
    /// its identity matters here.
    pub eq_config_hash: String,
    pub dsd_modulator: String,
    pub dsd_penalty: String,
    pub dsd_source_rules: String,
    /// `"dop"` or `"native"` when `pcm_dsd_mode` is DSD; empty for PCM.
    pub dsd_transport: String,
}

impl RenderConfigFingerprint {
    /// Whether prepared audio built against `self` may still be installed while
    /// `current` is in force.
    pub fn matches(&self, current: &RenderConfigFingerprint) -> bool {
        self == current
    }
}

/// Why the Player asked for, or reached, a boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryCause {
    /// The source ran out on its own.
    NaturalEof,
    /// The user asked for the next track.
    UserNext,
    /// Music.app advanced inside its own playlist; no new Player session.
    NativeAppleAdvance,
}

/// What actually happened to the output when a transition started.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "class")]
pub enum BoundaryClass {
    /// The physical carrier was retained across the boundary.
    ///
    /// `renderer_reconfigured` is true for the one legitimate case: a retained
    /// DoP carrier whose PCM source rate changed, which needs the renderer and
    /// modulator rebuilt without reopening the device.
    SeamlessSameCarrier { renderer_reconfigured: bool },
    /// Audio had to be restarted under protection. Named for the restart rather
    /// than a reopen because an Apple capture restart can keep the same
    /// physical output open.
    ProtectedRestart {
        reason: RestartReason,
        output_reopened: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartReason {
    /// The carrier itself changed, so the device must be reopened.
    CarrierChanged,
    /// A cached Apple format prediction disagreed with the live decoder.
    AppleFormatMismatch,
    /// The Apple format was never known well enough to prepare against.
    AppleFormatUnknown,
    /// Prepared audio was not installable and the fresh route ran instead.
    PreparedInstallFailed,
    /// The Apple capture session had to be rebuilt at a new rate.
    AppleCaptureRestart,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum BoundaryFailure {
    /// The token no longer describes the live zone, Player, or output.
    Stale,
    /// Preparation never completed, or completed and then expired.
    PreparationFailed { detail: String },
    /// The destination source could not be opened at all.
    SourceUnavailable { detail: String },
    /// A control operation cancelled the transition.
    Cancelled,
}

/// Identity of one attempted transition between two occurrences.
///
/// Every field is a fence. A `Started` event whose token disagrees with the
/// live state in any of them describes a transition that no longer exists, and
/// committing it would advance the wrong queue.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BoundaryToken {
    pub zone_id: String,
    pub zone_revision: u64,
    pub current_item_id: PlaybackItemId,
    pub next_item_id: PlaybackItemId,
    pub current_source_key: String,
    pub next_source_key: String,
    pub player_epoch: u64,
    pub output_config_revision: u64,
    pub transition_id: u64,
}

impl BoundaryToken {
    /// Allocate the next transition ID.
    ///
    /// Monotonic across zones so a transition ID is unambiguous in a trace that
    /// interleaves several of them.
    pub fn next_transition_id() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    pub fn new(
        zone_id: impl Into<String>,
        zone_revision: ZoneRevision,
        current: &PlaybackItem,
        next: &PlaybackItem,
        player_epoch: u64,
        output_config_revision: OutputConfigRevision,
    ) -> Self {
        Self {
            zone_id: zone_id.into(),
            zone_revision: zone_revision.get(),
            current_item_id: current.id,
            next_item_id: next.id,
            current_source_key: current.source_key(),
            next_source_key: next.source_key(),
            player_epoch,
            output_config_revision: output_config_revision.get(),
            transition_id: Self::next_transition_id(),
        }
    }

    /// Whether this token still describes the live zone and output state.
    ///
    /// Deliberately not `PartialEq` on the whole token: the caller holds the
    /// live revisions, not another token.
    pub fn is_current(
        &self,
        zone_id: &str,
        zone_revision: ZoneRevision,
        player_epoch: u64,
        output_config_revision: OutputConfigRevision,
    ) -> bool {
        self.zone_id == zone_id
            && self.zone_revision == zone_revision.get()
            && self.player_epoch == player_epoch
            && self.output_config_revision == output_config_revision.get()
    }
}

/// Player-side mirror of the live zone revision.
///
/// The coordinator increments its own revision and stores the new value here
/// *synchronously*, before enqueueing the command that clears prepared state.
/// Without that ordering an EOF racing a queue replacement can install the
/// prepared item while its own clear command is still sitting in the command
/// queue. The atomic is the thing that makes the invalidation win.
#[derive(Debug, Default)]
pub struct BoundaryRevisionGate {
    revision: AtomicU64,
}

impl BoundaryRevisionGate {
    pub fn new(revision: ZoneRevision) -> Self {
        Self {
            revision: AtomicU64::new(revision.get()),
        }
    }

    /// Publish a new revision. Must happen before the clear command is queued.
    pub fn store(&self, revision: ZoneRevision) {
        self.revision.store(revision.get(), Ordering::SeqCst);
    }

    pub fn load(&self) -> ZoneRevision {
        ZoneRevision(self.revision.load(Ordering::SeqCst))
    }

    /// Whether `token` may still be acted on.
    pub fn admits(&self, token: &BoundaryToken) -> bool {
        token.zone_revision == self.load().get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SourceRef;

    fn local(track_id: i64) -> SourceRef {
        SourceRef::LocalTrack {
            track_id,
            file_name: None,
            title: None,
            artist: None,
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    /// The entire reason occurrence IDs exist: a queue holding the same track
    /// twice must still be able to say which of the two just ended.
    #[test]
    fn duplicate_sources_receive_distinct_occurrence_ids() {
        let items = PlaybackItem::assign_all([local(7), local(7)]);

        assert_eq!(items[0].source_key(), items[1].source_key());
        assert_ne!(items[0].id, items[1].id);
    }

    #[test]
    fn occurrence_ids_are_monotonic() {
        let first = PlaybackItemId::next();
        let second = PlaybackItemId::next();

        assert!(second > first);
    }

    #[test]
    fn a_token_is_stale_when_any_fence_moved() {
        let items = PlaybackItem::assign_all([local(1), local(2)]);
        let token = BoundaryToken::new(
            "zone",
            ZoneRevision(4),
            &items[0],
            &items[1],
            9,
            OutputConfigRevision(2),
        );

        assert!(token.is_current("zone", ZoneRevision(4), 9, OutputConfigRevision(2)));
        assert!(!token.is_current("other", ZoneRevision(4), 9, OutputConfigRevision(2)));
        assert!(!token.is_current("zone", ZoneRevision(5), 9, OutputConfigRevision(2)));
        assert!(!token.is_current("zone", ZoneRevision(4), 10, OutputConfigRevision(2)));
        assert!(!token.is_current("zone", ZoneRevision(4), 9, OutputConfigRevision(3)));
    }

    /// The atomic must reject a token the moment the revision is published,
    /// which is strictly earlier than the queued clear command can run.
    #[test]
    fn the_revision_gate_rejects_a_token_before_prepared_state_is_cleared() {
        let items = PlaybackItem::assign_all([local(1), local(2)]);
        let gate = BoundaryRevisionGate::new(ZoneRevision(1));
        let token = BoundaryToken::new(
            "zone",
            ZoneRevision(1),
            &items[0],
            &items[1],
            0,
            OutputConfigRevision(0),
        );
        assert!(gate.admits(&token));

        gate.store(ZoneRevision(2));

        assert!(!gate.admits(&token));
    }

    #[test]
    fn a_render_fingerprint_change_invalidates_prepared_audio() {
        let base = RenderConfigFingerprint {
            output_device_uid: "hegel-h390".to_string(),
            target_rate_hz: 352_800,
            pcm_dsd_mode: "dsd".to_string(),
            dsd_transport: "dop".to_string(),
            ..RenderConfigFingerprint::default()
        };
        let mut changed = base.clone();
        changed.dsd_modulator = "seventh-order".to_string();

        assert!(base.matches(&base.clone()));
        assert!(!base.matches(&changed));
    }
}
