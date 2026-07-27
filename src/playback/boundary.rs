//! Durable boundary events, output-frame cursors, and transition markers.
//!
//! A boundary commit is the moment Fozmo's idea of what is playing changes. It
//! has to be driven by the destination audio actually reaching the output, and
//! it has to be impossible to lose. A broadcast channel gives neither: a slow
//! diagnostics subscriber can make the authoritative commit disappear, and a
//! subscriber that reconnects can see a commit twice.
//!
//! So each armed transition owns one [`BoundaryTicket`] — a single-consumer
//! queue that only the coordinator reads. Diagnostics get a separate broadcast
//! mirror which is allowed to lag and drop, because nothing depends on it.
//!
//! The "audio actually reached the output" half is [`TransitionMarkerSlot`].
//! The worker arms a marker at an absolute output frame; the audio callback
//! acknowledges it with plain atomic stores when its read cursor passes that
//! frame. The callback allocates nothing and sends on no channel, which is the
//! only acceptable shape for real-time code. The acknowledgement can be late
//! when destination audio was already buffered ahead of the marker, but it can
//! never be early, and "never early" is what makes it safe to commit on.

use super::identity::{
    BoundaryCause, BoundaryClass, BoundaryFailure, BoundaryToken, OutputGeneration,
};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, mpsc};

/// An event the Player raises about a boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerBoundaryEvent {
    /// The source ended and nothing installable was armed.
    NeedNext {
        token: BoundaryToken,
        cause: BoundaryCause,
        output_generation: u64,
        end_frame: u64,
    },
    /// The destination's first frame reached the output.
    Started {
        token: BoundaryToken,
        attempt: u8,
        class: BoundaryClass,
        output_generation: u64,
        render_start_frame: u64,
        /// Frames per second in the carrier domain that `render_start_frame`
        /// counts. For DoP this is the PCM carrier rate, such as 352_800, not
        /// the DSD wire rate; diagnostics report the wire rate separately.
        frame_rate_hz: u32,
    },
    /// The destination could not be started under this token and attempt.
    Failed {
        token: BoundaryToken,
        attempt: u8,
        reason: BoundaryFailure,
    },
}

impl PlayerBoundaryEvent {
    pub fn token(&self) -> &BoundaryToken {
        match self {
            Self::NeedNext { token, .. }
            | Self::Started { token, .. }
            | Self::Failed { token, .. } => token,
        }
    }
}

/// The producing half of a transition's authoritative event channel.
///
/// Cloneable so the Player worker and a fallback path can both raise events for
/// the same transition; the consuming half stays unique.
#[derive(Clone, Debug)]
pub struct BoundaryTicketSender {
    events: mpsc::UnboundedSender<PlayerBoundaryEvent>,
    mirror: broadcast::Sender<PlayerBoundaryEvent>,
}

impl BoundaryTicketSender {
    /// Publish an event. Returns false once the coordinator has dropped its
    /// ticket, which happens only after the transition is finished.
    pub fn emit(&self, event: PlayerBoundaryEvent) -> bool {
        // The mirror is observation only, so a lagging or absent diagnostics
        // subscriber must not affect the authoritative send.
        let _ = self.mirror.send(event.clone());
        self.events.send(event).is_ok()
    }
}

/// The authoritative, single-consumer half of a transition's event channel.
///
/// Not `Clone`: exactly one coordinator task consumes it, which is what makes
/// "commit on the first `Started` and only the first" expressible.
#[derive(Debug)]
pub struct BoundaryTicket {
    events: mpsc::UnboundedReceiver<PlayerBoundaryEvent>,
    mirror: broadcast::Sender<PlayerBoundaryEvent>,
}

impl BoundaryTicket {
    /// Create a ticket and its sender.
    pub fn new() -> (BoundaryTicket, BoundaryTicketSender) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (mirror, _) = broadcast::channel(64);
        (
            BoundaryTicket {
                events: events_rx,
                mirror: mirror.clone(),
            },
            BoundaryTicketSender {
                events: events_tx,
                mirror,
            },
        )
    }

    /// Await the next event, or `None` once every sender is gone.
    pub async fn recv(&mut self) -> Option<PlayerBoundaryEvent> {
        self.events.recv().await
    }

    /// Take an event without waiting.
    pub fn try_recv(&mut self) -> Option<PlayerBoundaryEvent> {
        self.events.try_recv().ok()
    }

    /// Subscribe a diagnostics observer.
    ///
    /// Observers may lag and lose events; that is the point of keeping them off
    /// the authoritative path.
    pub fn observe(&self) -> broadcast::Receiver<PlayerBoundaryEvent> {
        self.mirror.subscribe()
    }
}

/// Absolute frame write/read cursors for one physical output opening.
///
/// Frame numbers restart whenever CoreAudio hands back a new stream, so every
/// reading is qualified by the [`OutputGeneration`] it belongs to. A marker
/// armed under an older generation can therefore never be satisfied by frames
/// from a newer one.
#[derive(Debug)]
pub struct OutputFrameCursor {
    generation: AtomicU64,
    written: AtomicU64,
    read: AtomicU64,
}

impl OutputFrameCursor {
    pub fn new(generation: OutputGeneration) -> Self {
        Self {
            generation: AtomicU64::new(generation.get()),
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
        }
    }

    /// Begin a new physical opening, resetting both cursors.
    pub fn reopen(&self, generation: OutputGeneration) {
        self.written.store(0, Ordering::Release);
        self.read.store(0, Ordering::Release);
        self.generation.store(generation.get(), Ordering::Release);
    }

    pub fn generation(&self) -> OutputGeneration {
        OutputGeneration(self.generation.load(Ordering::Acquire))
    }

    /// Worker side: record frames handed to the ring.
    pub fn advance_written(&self, frames: u64) -> u64 {
        self.written.fetch_add(frames, Ordering::AcqRel) + frames
    }

    /// Audio-callback side: record frames consumed by the device.
    ///
    /// A plain atomic add. No allocation, no channel, no lock.
    pub fn advance_read(&self, frames: u64) -> u64 {
        self.read.fetch_add(frames, Ordering::AcqRel) + frames
    }

    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    pub fn read(&self) -> u64 {
        self.read.load(Ordering::Acquire)
    }
}

/// One armed transition marker, acknowledged from the audio callback.
///
/// Only one marker is live at a time: a zone has at most one armed transition,
/// and the coordinator will not arm the next until this one commits or is
/// cancelled.
#[derive(Debug, Default)]
pub struct TransitionMarkerSlot {
    /// Zero means "no marker armed".
    transition_id: AtomicU64,
    generation: AtomicU64,
    frame: AtomicU64,
    acknowledged_transition: AtomicU64,
    acknowledged_frame: AtomicU64,
}

/// An acknowledgement the worker lifted out of the marker slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionMarkerAck {
    pub transition_id: u64,
    pub output_generation: OutputGeneration,
    /// Absolute frame at which the destination's first frame was consumed.
    pub render_start_frame: u64,
}

impl TransitionMarkerSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm a marker at the absolute frame carrying the destination's first
    /// sample.
    ///
    /// Ordering matters: the frame and generation are published before the
    /// transition ID, so the callback can never observe a live transition ID
    /// paired with a stale frame.
    pub fn arm(&self, transition_id: u64, generation: OutputGeneration, frame: u64) {
        debug_assert_ne!(transition_id, 0, "transition IDs start at 1");
        self.frame.store(frame, Ordering::Release);
        self.generation.store(generation.get(), Ordering::Release);
        self.transition_id.store(transition_id, Ordering::Release);
    }

    /// Disarm without acknowledging, for a cancelled transition.
    pub fn clear(&self) {
        self.transition_id.store(0, Ordering::Release);
    }

    /// Audio-callback side: acknowledge if `read_frame` has reached the marker.
    ///
    /// Real-time safe — three loads and two stores in the worst case, and
    /// nothing at all once acknowledged. Returns whether this call was the one
    /// that acknowledged, purely so tests can assert it happens exactly once.
    pub fn acknowledge_through(&self, generation: OutputGeneration, read_frame: u64) -> bool {
        let transition_id = self.transition_id.load(Ordering::Acquire);
        if transition_id == 0 {
            return false;
        }
        if self.generation.load(Ordering::Acquire) != generation.get() {
            return false;
        }
        if read_frame < self.frame.load(Ordering::Acquire) {
            return false;
        }
        if self.acknowledged_transition.load(Ordering::Acquire) == transition_id {
            return false;
        }
        self.acknowledged_frame.store(read_frame, Ordering::Release);
        self.acknowledged_transition
            .store(transition_id, Ordering::Release);
        true
    }

    /// Worker side: take an acknowledgement, if one is waiting.
    ///
    /// Consuming disarms the slot so `Started` is emitted once per arming.
    pub fn take_acknowledgement(&self) -> Option<TransitionMarkerAck> {
        let acknowledged = self.acknowledged_transition.load(Ordering::Acquire);
        if acknowledged == 0 || acknowledged != self.transition_id.load(Ordering::Acquire) {
            return None;
        }
        let ack = TransitionMarkerAck {
            transition_id: acknowledged,
            output_generation: OutputGeneration(self.generation.load(Ordering::Acquire)),
            render_start_frame: self.acknowledged_frame.load(Ordering::Acquire),
        };
        self.transition_id.store(0, Ordering::Release);
        Some(ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::identity::{
        OutputConfigRevision, PlaybackItem, RestartReason, ZoneRevision,
    };
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

    fn token() -> BoundaryToken {
        let items = PlaybackItem::assign_all([local(1), local(2)]);
        BoundaryToken::new(
            "zone",
            ZoneRevision(1),
            &items[0],
            &items[1],
            0,
            OutputConfigRevision(0),
        )
    }

    fn started(token: BoundaryToken) -> PlayerBoundaryEvent {
        PlayerBoundaryEvent::Started {
            token,
            attempt: 0,
            class: BoundaryClass::SeamlessSameCarrier {
                renderer_reconfigured: false,
            },
            output_generation: 1,
            render_start_frame: 4_096,
            frame_rate_hz: 352_800,
        }
    }

    /// The failure the ticket exists to rule out: a diagnostics subscriber that
    /// goes away, or falls behind, must not be able to take the commit with it.
    #[tokio::test]
    async fn a_dropped_diagnostics_observer_cannot_lose_the_commit() {
        let (mut ticket, sender) = BoundaryTicket::new();
        let observer = ticket.observe();
        drop(observer);

        assert!(sender.emit(started(token())));

        assert!(matches!(
            ticket.recv().await,
            Some(PlayerBoundaryEvent::Started { .. })
        ));
    }

    /// A broadcast mirror with a full buffer drops for its observers only.
    #[tokio::test]
    async fn a_lagging_observer_does_not_block_the_authoritative_channel() {
        let (mut ticket, sender) = BoundaryTicket::new();
        let _observer = ticket.observe();

        for _ in 0..256 {
            assert!(sender.emit(started(token())));
        }

        let mut delivered = 0;
        while ticket.try_recv().is_some() {
            delivered += 1;
        }
        assert_eq!(delivered, 256);
    }

    #[tokio::test]
    async fn a_failed_boundary_reaches_the_coordinator_with_its_attempt() {
        let (mut ticket, sender) = BoundaryTicket::new();
        let token = token();
        sender.emit(PlayerBoundaryEvent::Failed {
            token: token.clone(),
            attempt: 1,
            reason: BoundaryFailure::PreparationFailed {
                detail: "decoder refused the stream".to_string(),
            },
        });

        let event = ticket.recv().await.expect("failure event");
        assert_eq!(event.token(), &token);
        assert!(matches!(
            event,
            PlayerBoundaryEvent::Failed { attempt: 1, .. }
        ));
    }

    /// The marker must be acknowledged only once the device has actually
    /// consumed the frame, however much destination audio is already buffered.
    #[test]
    fn a_marker_is_never_acknowledged_early() {
        let cursor = OutputFrameCursor::new(OutputGeneration(3));
        let slot = TransitionMarkerSlot::new();
        slot.arm(42, OutputGeneration(3), 10_000);

        cursor.advance_written(48_000);
        assert!(!slot.acknowledge_through(cursor.generation(), cursor.advance_read(4_000)));
        assert_eq!(slot.take_acknowledgement(), None);

        assert!(slot.acknowledge_through(cursor.generation(), cursor.advance_read(6_000)));
        assert_eq!(
            slot.take_acknowledgement(),
            Some(TransitionMarkerAck {
                transition_id: 42,
                output_generation: OutputGeneration(3),
                render_start_frame: 10_000,
            })
        );
    }

    #[test]
    fn a_marker_is_acknowledged_exactly_once_per_arming() {
        let slot = TransitionMarkerSlot::new();
        slot.arm(7, OutputGeneration(1), 100);

        assert!(slot.acknowledge_through(OutputGeneration(1), 128));
        assert!(!slot.acknowledge_through(OutputGeneration(1), 256));
        assert!(slot.take_acknowledgement().is_some());
        assert_eq!(slot.take_acknowledgement(), None);
    }

    /// Frames from a reopened output describe a different timeline, so they
    /// must not satisfy a marker armed against the previous opening.
    #[test]
    fn frames_from_a_newer_output_generation_cannot_satisfy_an_older_marker() {
        let cursor = OutputFrameCursor::new(OutputGeneration(1));
        let slot = TransitionMarkerSlot::new();
        slot.arm(9, OutputGeneration(1), 500);

        cursor.reopen(OutputGeneration(2));

        assert!(!slot.acknowledge_through(cursor.generation(), 10_000));
        assert_eq!(cursor.read(), 0);
        assert_eq!(slot.take_acknowledgement(), None);
    }

    #[test]
    fn a_cancelled_transition_disarms_its_marker() {
        let slot = TransitionMarkerSlot::new();
        slot.arm(11, OutputGeneration(1), 0);
        slot.clear();

        assert!(!slot.acknowledge_through(OutputGeneration(1), 1_000));
        assert_eq!(slot.take_acknowledgement(), None);
    }

    #[test]
    fn a_protected_restart_records_whether_the_output_reopened() {
        let class = BoundaryClass::ProtectedRestart {
            reason: RestartReason::AppleFormatMismatch,
            output_reopened: false,
        };

        assert_ne!(
            class,
            BoundaryClass::SeamlessSameCarrier {
                renderer_reconfigured: false
            }
        );
    }
}
