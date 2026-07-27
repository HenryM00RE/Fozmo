//! The one authority for local physical advancement.
//!
//! Fozmo used to advance a queue from three places at once: the Player's own
//! end-of-file handling, the listening observer's queue search, and a 500 ms
//! stopped-state poll. Each was individually reasonable and together they could
//! pop twice, pop the wrong duplicate, or pop while a prepared item was still
//! installing.
//!
//! This coordinator replaces all of that with a single rule: **the queue moves
//! only when destination audio has reached the output**, evidenced by a
//! `Started` event on the transition's own durable ticket. Everything else —
//! status polls, observers, diagnostics — is read-only.
//!
//! The coordinator is deliberately synchronous and free of engine types. It
//! takes the identity fences from [`super::identity`], the events from
//! [`super::boundary`], and two narrow traits for the side effects it must
//! order precisely (listening finalization and queue persistence). That is what
//! lets the whole commit path be tested against duplicate occurrences,
//! cancellation races, and persistence failures without any audio hardware.

use super::identity::{
    BoundaryCause, BoundaryClass, BoundaryFailure, BoundaryToken, OutputConfigRevision,
    PlaybackItem, PlaybackItemId, ZoneRevision,
};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Backoff for a queue snapshot that could not be written.
const PERSIST_RETRY_BACKOFF: [Duration; 4] = [
    Duration::from_millis(200),
    Duration::from_millis(800),
    Duration::from_secs(3),
    Duration::from_secs(10),
];

/// What the coordinator did with a `Started` event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitResult {
    /// This exact transition and attempt already committed. Safe to ignore:
    /// the queue has already moved and must not move again.
    AlreadyCommitted,
    /// The queue advanced by exactly one occurrence.
    Committed,
    /// The event describes a transition that no longer exists.
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelReason {
    QueueChanged,
    Seek,
    Stop,
    UserNext,
    OutputChanged,
    Transferred,
    PreparationExpired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionFailure {
    /// The prepared install failed and so did the single fresh-route fallback.
    PreparedAndFallbackFailed { detail: String },
    /// Nothing was prepared and the fresh route failed.
    FallbackFailed { detail: String },
}

/// A destination that is ready to install but has not started yet.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedBoundary {
    pub token: BoundaryToken,
    pub class: BoundaryClass,
    pub prepared_at: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransitionState {
    Idle,
    Resolving,
    Ready(PreparedBoundary),
    Armed,
    AwaitingStart,
    /// The single fresh-route retry. `attempt` is always 1: there is exactly
    /// one fallback per transition, and a second would be a way to skip a
    /// track without anyone asking.
    FallingBack {
        attempt: u8,
    },
    Completed,
    Cancelled(CancelReason),
    Failed(TransitionFailure),
}

/// Finalizing the previous listen and starting the next one.
///
/// Split out so ordering is enforced here rather than in the listening tracker:
/// the prior listen must be finalized before the queue pops, and the
/// destination listen must not start until it has.
pub trait ListenRecorder {
    fn finalize(&mut self, item: &PlaybackItem, cause: BoundaryCause);
    fn start(&mut self, item: &PlaybackItem);
}

/// Writing the remaining queue.
pub trait QueuePersistence {
    fn persist(&mut self, zone_id: &str, queue: &[PlaybackItem]) -> Result<(), String>;
}

/// A queue snapshot that still needs to reach storage.
///
/// Idempotent by construction: it is the whole remaining queue, not a delta, so
/// retrying it any number of times has the same effect as writing it once. That
/// is what makes "persistence failed" survivable without a second in-memory pop.
#[derive(Clone, Debug)]
struct PendingSnapshot {
    queue: Vec<PlaybackItem>,
    attempts: usize,
    next_attempt_at: Instant,
    last_error: String,
}

/// Per-zone playback authority.
///
/// Callers hold this behind the zone lock. Every method that mutates listening
/// state or the queue lives here, so "who advanced the queue?" has exactly one
/// answer.
pub struct TransitionCoordinator {
    zone_id: String,
    zone_revision: ZoneRevision,
    player_epoch: u64,
    output_config_revision: OutputConfigRevision,
    current: Option<PlaybackItem>,
    queue: VecDeque<PlaybackItem>,
    transition: TransitionState,
    active_transition_id: Option<u64>,
    /// Transition IDs already committed, so a duplicate `Started` is answered
    /// `AlreadyCommitted` rather than popping again.
    committed_transitions: Vec<u64>,
    /// Whether the prior listen for the active transition has been finalized.
    /// A failed transition still has to finalize it, exactly once.
    prior_listen_finalized: bool,
    pending_persist: Option<PendingSnapshot>,
}

impl TransitionCoordinator {
    pub fn new(zone_id: impl Into<String>) -> Self {
        Self {
            zone_id: zone_id.into(),
            zone_revision: ZoneRevision(1),
            player_epoch: 0,
            output_config_revision: OutputConfigRevision(0),
            current: None,
            queue: VecDeque::new(),
            transition: TransitionState::Idle,
            active_transition_id: None,
            committed_transitions: Vec::new(),
            prior_listen_finalized: false,
            pending_persist: None,
        }
    }

    pub fn zone_id(&self) -> &str {
        &self.zone_id
    }

    pub fn zone_revision(&self) -> ZoneRevision {
        self.zone_revision
    }

    pub fn player_epoch(&self) -> u64 {
        self.player_epoch
    }

    pub fn output_config_revision(&self) -> OutputConfigRevision {
        self.output_config_revision
    }

    pub fn current(&self) -> Option<&PlaybackItem> {
        self.current.as_ref()
    }

    /// The occurrence a natural boundary will move to.
    pub fn queue_head(&self) -> Option<&PlaybackItem> {
        self.queue.front()
    }

    pub fn queue(&self) -> impl Iterator<Item = &PlaybackItem> {
        self.queue.iter()
    }

    pub fn transition(&self) -> &TransitionState {
        &self.transition
    }

    /// Install a queue and its current occurrence.
    ///
    /// Bumps the zone revision, which is what invalidates any token already
    /// issued. Callers must publish the new revision into the Player's atomic
    /// gate *before* enqueueing the command that clears prepared state.
    pub fn accept_queue(&mut self, current: Option<PlaybackItem>, queue: Vec<PlaybackItem>) {
        self.current = current;
        self.queue = queue.into();
        self.bump_zone_revision();
        self.cancel_transition(CancelReason::QueueChanged);
    }

    /// Record a control operation that invalidates prepared state.
    pub fn invalidate(&mut self, reason: CancelReason) -> ZoneRevision {
        self.bump_zone_revision();
        self.cancel_transition(reason);
        self.zone_revision
    }

    /// Record that the Player restarted, invalidating tokens bound to the old
    /// session.
    pub fn advance_player_epoch(&mut self) -> u64 {
        self.player_epoch += 1;
        self.player_epoch
    }

    /// Record an output-device or render configuration change.
    pub fn advance_output_config(&mut self) -> OutputConfigRevision {
        self.output_config_revision = self.output_config_revision.next();
        self.bump_zone_revision();
        self.cancel_transition(CancelReason::OutputChanged);
        self.output_config_revision
    }

    fn bump_zone_revision(&mut self) {
        self.zone_revision = self.zone_revision.next();
    }

    fn cancel_transition(&mut self, reason: CancelReason) {
        if matches!(self.transition, TransitionState::Idle) {
            return;
        }
        self.transition = TransitionState::Cancelled(reason);
        self.active_transition_id = None;
        self.prior_listen_finalized = false;
    }

    /// Mint a token for the boundary from the current occurrence to the queue
    /// head, and enter `Resolving`.
    ///
    /// Returns `None` when there is nothing to transition to, which is a
    /// finished queue rather than an error.
    pub fn begin_transition(&mut self) -> Option<BoundaryToken> {
        let current = self.current.as_ref()?;
        let next = self.queue.front()?;
        let token = BoundaryToken::new(
            &self.zone_id,
            self.zone_revision,
            current,
            next,
            self.player_epoch,
            self.output_config_revision,
        );
        self.active_transition_id = Some(token.transition_id);
        self.prior_listen_finalized = false;
        self.transition = TransitionState::Resolving;
        Some(token)
    }

    pub fn mark_ready(&mut self, prepared: PreparedBoundary) {
        if self.active_transition_id == Some(prepared.token.transition_id) {
            self.transition = TransitionState::Ready(prepared);
        }
    }

    pub fn mark_armed(&mut self, token: &BoundaryToken) {
        if self.active_transition_id == Some(token.transition_id) {
            self.transition = TransitionState::Armed;
        }
    }

    pub fn mark_awaiting_start(&mut self, token: &BoundaryToken) {
        if self.active_transition_id == Some(token.transition_id) {
            self.transition = TransitionState::AwaitingStart;
        }
    }

    /// Enter the single fresh-route fallback for `token`.
    ///
    /// Reuses the transition ID and both occurrence IDs on purpose: the
    /// fallback is another attempt at the *same* boundary, not a new one, so it
    /// must not be able to move to a different destination. Returns `None` when
    /// a fallback has already been used, which is what caps it at one.
    pub fn begin_fallback(&mut self, token: &BoundaryToken) -> Option<BoundaryToken> {
        if self.active_transition_id != Some(token.transition_id) {
            return None;
        }
        if matches!(self.transition, TransitionState::FallingBack { .. }) {
            return None;
        }
        if !self.token_is_live(token) {
            return None;
        }
        self.transition = TransitionState::FallingBack { attempt: 1 };
        Some(token.clone())
    }

    /// Whether `token` still describes the live zone, occurrences, and output.
    fn token_is_live(&self, token: &BoundaryToken) -> bool {
        if !token.is_current(
            &self.zone_id,
            self.zone_revision,
            self.player_epoch,
            self.output_config_revision,
        ) {
            return false;
        }
        let Some(current) = self.current.as_ref() else {
            return false;
        };
        let Some(head) = self.queue.front() else {
            return false;
        };
        // Occurrence identity, not source key: a queue holding the same track
        // twice must not let the second occurrence satisfy the first's token.
        token.current_item_id == current.id
            && token.next_item_id == head.id
            && token.current_source_key == current.source_key()
            && token.next_source_key == head.source_key()
    }

    /// Commit a destination that has provably reached the output.
    ///
    /// The `Started` event is required rather than inferred; nothing else in
    /// the system is allowed to move the queue. Idempotent by transition ID so
    /// a redelivered event, or a fallback `Started` arriving after the prepared
    /// one, cannot advance twice.
    pub fn commit_started(
        &mut self,
        token: &BoundaryToken,
        attempt: u8,
        cause: BoundaryCause,
        output_generation: u64,
        listens: &mut impl ListenRecorder,
        persistence: &mut impl QueuePersistence,
    ) -> CommitResult {
        if self.committed_transitions.contains(&token.transition_id) {
            return CommitResult::AlreadyCommitted;
        }
        if self.active_transition_id != Some(token.transition_id) {
            return CommitResult::Stale;
        }
        let expected_attempt = match &self.transition {
            TransitionState::Ready(_) | TransitionState::Armed | TransitionState::AwaitingStart => {
                0
            }
            TransitionState::FallingBack { attempt } => *attempt,
            _ => return CommitResult::Stale,
        };
        if attempt != expected_attempt {
            return CommitResult::Stale;
        }
        if !self.token_is_live(token) {
            return CommitResult::Stale;
        }
        let _ = output_generation;

        // From here the commit is irrevocable, so mark it before any side
        // effect. A panic or an error after this point must not leave the
        // transition committable a second time.
        self.committed_transitions.push(token.transition_id);
        self.transition = TransitionState::Completed;
        self.active_transition_id = None;

        if let Some(previous) = self.current.take()
            && !self.prior_listen_finalized
        {
            listens.finalize(&previous, cause);
        }
        self.prior_listen_finalized = true;

        let destination = self
            .queue
            .pop_front()
            .expect("token_is_live proved a queue head");
        listens.start(&destination);
        self.current = Some(destination);
        self.bump_zone_revision();
        self.prior_listen_finalized = false;

        self.write_queue(persistence);
        CommitResult::Committed
    }

    /// Both the prepared install and the single fallback failed.
    ///
    /// Playback stops with the destination still at the canonical queue head.
    /// Trying the item after it would be a silent skip: the user asked for this
    /// track, and Fozmo has no evidence the next one would fare any better.
    pub fn fail_transition(
        &mut self,
        token: &BoundaryToken,
        reason: BoundaryFailure,
        listens: &mut impl ListenRecorder,
    ) -> bool {
        if self.active_transition_id != Some(token.transition_id) {
            return false;
        }
        let detail = match &reason {
            BoundaryFailure::PreparationFailed { detail }
            | BoundaryFailure::SourceUnavailable { detail } => detail.clone(),
            BoundaryFailure::Stale => "the transition was superseded".to_string(),
            BoundaryFailure::Cancelled => "the transition was cancelled".to_string(),
        };
        let failure = if matches!(self.transition, TransitionState::FallingBack { .. }) {
            TransitionFailure::PreparedAndFallbackFailed { detail }
        } else {
            TransitionFailure::FallbackFailed { detail }
        };
        self.transition = TransitionState::Failed(failure);
        self.active_transition_id = None;

        // The source really did end, so its listen is over whether or not
        // anything followed it. Finalize once and leave the queue alone.
        if let Some(previous) = self.current.take()
            && !self.prior_listen_finalized
        {
            listens.finalize(&previous, BoundaryCause::NaturalEof);
        }
        self.prior_listen_finalized = true;
        true
    }

    fn write_queue(&mut self, persistence: &mut impl QueuePersistence) {
        let snapshot = self.queue.iter().cloned().collect::<Vec<_>>();
        match persistence.persist(&self.zone_id, &snapshot) {
            Ok(()) => self.pending_persist = None,
            Err(error) => {
                // One pending snapshot, replaced rather than queued: only the
                // latest queue is worth writing, and an unbounded retry list
                // would let a slow disk turn into a growing backlog.
                self.pending_persist = Some(PendingSnapshot {
                    queue: snapshot,
                    attempts: 1,
                    next_attempt_at: Instant::now() + PERSIST_RETRY_BACKOFF[0],
                    last_error: error,
                });
            }
        }
    }

    pub fn pending_persist_error(&self) -> Option<&str> {
        self.pending_persist
            .as_ref()
            .map(|pending| pending.last_error.as_str())
    }

    /// Retry a queue snapshot that failed to write.
    ///
    /// Never touches the in-memory queue, so however many times this runs the
    /// commit that produced the snapshot stays exactly one pop.
    pub fn retry_pending_persist(
        &mut self,
        now: Instant,
        persistence: &mut impl QueuePersistence,
    ) -> bool {
        let Some(pending) = self.pending_persist.as_mut() else {
            return false;
        };
        if now < pending.next_attempt_at {
            return false;
        }
        match persistence.persist(&self.zone_id, &pending.queue) {
            Ok(()) => {
                self.pending_persist = None;
                true
            }
            Err(error) => {
                pending.attempts += 1;
                pending.last_error = error;
                let backoff = PERSIST_RETRY_BACKOFF
                    [(pending.attempts - 1).min(PERSIST_RETRY_BACKOFF.len() - 1)];
                pending.next_attempt_at = now + backoff;
                false
            }
        }
    }

    /// Remove queue occurrences the Apple helper rejected.
    ///
    /// Applied before any token is minted, so a rejected tail never becomes
    /// somebody's `next_item_id`. Returns the removed identities.
    pub fn remove_occurrences(&mut self, removed: &[PlaybackItemId]) -> Vec<PlaybackItemId> {
        if removed.is_empty() {
            return Vec::new();
        }
        let mut dropped = Vec::new();
        self.queue.retain(|item| {
            if removed.contains(&item.id) {
                dropped.push(item.id);
                false
            } else {
                true
            }
        });
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::identity::RestartReason;
    use crate::protocol::SourceRef;

    #[derive(Default)]
    struct RecordedListens {
        events: Vec<String>,
    }

    impl ListenRecorder for RecordedListens {
        fn finalize(&mut self, item: &PlaybackItem, cause: BoundaryCause) {
            self.events
                .push(format!("finalize {} {cause:?}", item.id.get()));
        }

        fn start(&mut self, item: &PlaybackItem) {
            self.events.push(format!("start {}", item.id.get()));
        }
    }

    #[derive(Default)]
    struct RecordedQueue {
        writes: Vec<Vec<PlaybackItemId>>,
        fail_next: usize,
    }

    impl QueuePersistence for RecordedQueue {
        fn persist(&mut self, _zone_id: &str, queue: &[PlaybackItem]) -> Result<(), String> {
            if self.fail_next > 0 {
                self.fail_next -= 1;
                return Err("disk is read-only".to_string());
            }
            self.writes.push(queue.iter().map(|item| item.id).collect());
            Ok(())
        }
    }

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

    fn qobuz(track_id: u64) -> SourceRef {
        SourceRef::QobuzTrack {
            track_id,
            title: None,
            artist: None,
            album: None,
            album_id: None,
            image_url: None,
            duration_secs: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn coordinator(sources: Vec<SourceRef>) -> TransitionCoordinator {
        let mut items = PlaybackItem::assign_all(sources);
        let current = items.remove(0);
        let mut coordinator = TransitionCoordinator::new("zone-1");
        coordinator.accept_queue(Some(current), items);
        coordinator
    }

    fn commit(
        coordinator: &mut TransitionCoordinator,
        token: &BoundaryToken,
        attempt: u8,
        listens: &mut RecordedListens,
        persistence: &mut RecordedQueue,
    ) -> CommitResult {
        coordinator.commit_started(
            token,
            attempt,
            BoundaryCause::NaturalEof,
            1,
            listens,
            persistence,
        )
    }

    #[test]
    fn a_started_event_advances_the_queue_by_exactly_one_occurrence() {
        let mut coordinator = coordinator(vec![local(1), local(2), qobuz(3)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue::default();

        let result = commit(&mut coordinator, &token, 0, &mut listens, &mut persistence);

        assert_eq!(result, CommitResult::Committed);
        assert_eq!(coordinator.current().unwrap().id, token.next_item_id);
        assert_eq!(coordinator.queue().count(), 1);
        assert_eq!(
            listens.events,
            vec![
                format!("finalize {} NaturalEof", token.current_item_id.get()),
                format!("start {}", token.next_item_id.get()),
            ]
        );
    }

    /// A redelivered `Started` — a retry, a duplicated ticket read — must be
    /// inert. Popping twice here silently skips a track.
    #[test]
    fn a_repeated_started_event_does_not_advance_twice() {
        let mut coordinator = coordinator(vec![local(1), local(2), local(3)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue::default();

        assert_eq!(
            commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
            CommitResult::Committed
        );
        assert_eq!(
            commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
            CommitResult::AlreadyCommitted
        );
        assert_eq!(coordinator.queue().count(), 1);
    }

    /// The duplicate-track case occurrence IDs exist for. Both entries are
    /// `local:1`, so a source-key commit would advance to whichever it found
    /// first.
    #[test]
    fn duplicate_occurrences_commit_by_identity_rather_than_source_key() {
        let mut coordinator = coordinator(vec![local(1), local(1), local(1)]);
        let first = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&first);
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue::default();

        assert_eq!(
            commit(&mut coordinator, &first, 0, &mut listens, &mut persistence),
            CommitResult::Committed
        );
        let second = coordinator.begin_transition().expect("a second transition");
        assert_ne!(second.current_item_id, first.current_item_id);
        assert_eq!(second.current_item_id, first.next_item_id);
        assert_eq!(second.current_source_key, second.next_source_key);
        coordinator.mark_armed(&second);

        assert_eq!(
            commit(&mut coordinator, &second, 0, &mut listens, &mut persistence),
            CommitResult::Committed
        );
        assert_eq!(coordinator.queue().count(), 0);
    }

    #[test]
    fn a_queue_replacement_between_eof_and_installation_makes_the_token_stale() {
        let mut coordinator = coordinator(vec![local(1), local(2)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);
        let replacement = PlaybackItem::assign_all([qobuz(9), qobuz(10)]);
        let mut replacement = replacement.into_iter();
        coordinator.accept_queue(replacement.next(), replacement.collect());
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue::default();

        assert_eq!(
            commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
            CommitResult::Stale
        );
        assert!(listens.events.is_empty());
    }

    #[test]
    fn a_seek_or_stop_between_eof_and_installation_makes_the_token_stale() {
        for reason in [CancelReason::Seek, CancelReason::Stop] {
            let mut coordinator = coordinator(vec![local(1), local(2)]);
            let token = coordinator.begin_transition().expect("a transition");
            coordinator.mark_armed(&token);
            coordinator.invalidate(reason);
            let mut listens = RecordedListens::default();
            let mut persistence = RecordedQueue::default();

            assert_eq!(
                commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
                CommitResult::Stale,
                "{reason:?} must invalidate the prepared boundary"
            );
        }
    }

    #[test]
    fn an_output_configuration_change_after_preparation_makes_the_token_stale() {
        let mut coordinator = coordinator(vec![local(1), local(2)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_ready(PreparedBoundary {
            token: token.clone(),
            class: BoundaryClass::SeamlessSameCarrier {
                renderer_reconfigured: false,
            },
            prepared_at: Instant::now(),
        });
        coordinator.advance_output_config();
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue::default();

        assert_eq!(
            commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
            CommitResult::Stale
        );
    }

    /// Exactly one fresh-route attempt per boundary, reusing both occurrence
    /// identities so it cannot land on a different track.
    #[test]
    fn the_fallback_reuses_the_transition_and_runs_only_once() {
        let mut coordinator = coordinator(vec![local(1), local(2)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);

        let fallback = coordinator.begin_fallback(&token).expect("one fallback");
        assert_eq!(fallback.transition_id, token.transition_id);
        assert_eq!(fallback.current_item_id, token.current_item_id);
        assert_eq!(fallback.next_item_id, token.next_item_id);
        assert_eq!(
            coordinator.transition(),
            &TransitionState::FallingBack { attempt: 1 }
        );

        assert!(coordinator.begin_fallback(&token).is_none());
    }

    #[test]
    fn the_fallback_commits_only_on_its_own_attempt_number() {
        let mut coordinator = coordinator(vec![local(1), local(2)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);
        coordinator.begin_fallback(&token).expect("one fallback");
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue::default();

        assert_eq!(
            commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
            CommitResult::Stale
        );
        assert_eq!(
            commit(&mut coordinator, &token, 1, &mut listens, &mut persistence),
            CommitResult::Committed
        );
    }

    /// Prepared failed, the single fallback failed. The destination stays at the
    /// head so the user can retry it; quietly starting the track after it would
    /// be a skip nobody asked for.
    #[test]
    fn a_prepared_failure_followed_by_a_fallback_failure_leaves_the_head_queued() {
        let mut coordinator = coordinator(vec![local(1), local(2), local(3)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);
        coordinator.begin_fallback(&token).expect("one fallback");
        let mut listens = RecordedListens::default();

        assert!(coordinator.fail_transition(
            &token,
            BoundaryFailure::SourceUnavailable {
                detail: "no route to the stream host".to_string(),
            },
            &mut listens,
        ));

        assert_eq!(coordinator.queue_head().unwrap().id, token.next_item_id);
        assert_eq!(coordinator.queue().count(), 2);
        assert!(coordinator.current().is_none());
        assert_eq!(
            listens.events,
            vec![format!(
                "finalize {} NaturalEof",
                token.current_item_id.get()
            )]
        );
        assert!(matches!(
            coordinator.transition(),
            TransitionState::Failed(TransitionFailure::PreparedAndFallbackFailed { .. })
        ));
    }

    /// A failed install must not consume the item after the destination either.
    #[test]
    fn a_failed_transition_never_consumes_the_following_item() {
        let mut coordinator = coordinator(vec![local(1), local(2), local(3)]);
        let before = coordinator.queue().map(|item| item.id).collect::<Vec<_>>();
        let token = coordinator.begin_transition().expect("a transition");
        let mut listens = RecordedListens::default();

        coordinator.fail_transition(
            &token,
            BoundaryFailure::PreparationFailed {
                detail: "decoder refused the stream".to_string(),
            },
            &mut listens,
        );

        let after = coordinator.queue().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(before, after);
    }

    /// A write failure must not be paid for with a second pop. The retry
    /// rewrites the same snapshot until it lands.
    #[test]
    fn a_persistence_failure_retries_the_snapshot_without_popping_twice() {
        let mut coordinator = coordinator(vec![local(1), local(2), local(3)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue {
            fail_next: 1,
            ..RecordedQueue::default()
        };

        assert_eq!(
            commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
            CommitResult::Committed
        );
        assert_eq!(coordinator.queue().count(), 1);
        assert!(coordinator.pending_persist_error().is_some());
        assert!(persistence.writes.is_empty());

        let later = Instant::now() + Duration::from_secs(1);
        assert!(coordinator.retry_pending_persist(later, &mut persistence));

        assert!(coordinator.pending_persist_error().is_none());
        assert_eq!(coordinator.queue().count(), 1);
        assert_eq!(persistence.writes.len(), 1);
        assert_eq!(
            listens
                .events
                .iter()
                .filter(|e| e.starts_with("start"))
                .count(),
            1
        );
    }

    #[test]
    fn a_persistence_retry_before_its_backoff_elapses_does_nothing() {
        let mut coordinator = coordinator(vec![local(1), local(2)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_armed(&token);
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue {
            fail_next: 1,
            ..RecordedQueue::default()
        };
        commit(&mut coordinator, &token, 0, &mut listens, &mut persistence);

        assert!(!coordinator.retry_pending_persist(Instant::now(), &mut persistence));
        assert!(coordinator.pending_persist_error().is_some());
    }

    #[test]
    fn rejected_tail_occurrences_are_removed_before_a_token_is_minted() {
        let mut coordinator = coordinator(vec![local(1), local(2), local(3), local(4)]);
        let rejected = coordinator.queue().nth(1).unwrap().id;

        assert_eq!(coordinator.remove_occurrences(&[rejected]), vec![rejected]);

        let remaining = coordinator.queue().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(remaining.len(), 2);
        assert!(!remaining.contains(&rejected));
    }

    #[test]
    fn a_protected_restart_still_commits_through_the_same_ticket() {
        let mut coordinator = coordinator(vec![local(1), local(2)]);
        let token = coordinator.begin_transition().expect("a transition");
        coordinator.mark_ready(PreparedBoundary {
            token: token.clone(),
            class: BoundaryClass::ProtectedRestart {
                reason: RestartReason::AppleFormatUnknown,
                output_reopened: true,
            },
            prepared_at: Instant::now(),
        });
        let mut listens = RecordedListens::default();
        let mut persistence = RecordedQueue::default();

        assert_eq!(
            commit(&mut coordinator, &token, 0, &mut listens, &mut persistence),
            CommitResult::Committed
        );
    }
}
