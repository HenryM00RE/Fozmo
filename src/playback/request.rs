use crate::playback::sequencer::PlaybackRequestSequence;
use crate::protocol::SourceRef;
use crate::services::qobuz::QobuzPlayRequest;

#[derive(Clone, Debug)]
pub(crate) struct PlaybackRequest {
    pub(crate) profile_id: String,
    pub(crate) source: SourceRef,
    pub(crate) queue: Vec<SourceRef>,
    pub(crate) radio_auto: bool,
    pub(crate) guard: PlaybackGuard,
    pub(crate) qobuz_request: Option<Box<QobuzPlayRequest>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PlaybackGuard {
    expected_sequence: Option<PlaybackRequestSequence>,
}

impl PlaybackGuard {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn from_expected_sequence(
        expected_sequence: Option<PlaybackRequestSequence>,
    ) -> Self {
        Self { expected_sequence }
    }

    pub(crate) fn is_current(&self, state: &crate::app::state::AppState) -> bool {
        self.expected_sequence
            .as_ref()
            .is_none_or(|expected| state.playback_sequencer().is_current(expected))
    }

    pub(crate) fn expected_sequence(&self) -> Option<&PlaybackRequestSequence> {
        self.expected_sequence.as_ref()
    }
}

impl PlaybackRequest {
    pub(crate) fn source_fields(&self) -> (&'static str, Option<i64>, Option<u64>, usize) {
        (
            self.source.kind(),
            self.source.local_track_id(),
            self.source.qobuz_track_id(),
            self.queue.len(),
        )
    }
}
