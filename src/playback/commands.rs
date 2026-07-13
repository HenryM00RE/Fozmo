use crate::app::state::AppState;
pub(crate) use crate::playback::sequencer::PlaybackRequestSequence;

#[cfg(test)]
use crate::playback::sequencer::MAX_PLAYBACK_SEQUENCE_CLIENTS;

pub(crate) fn accept_playback_request_sequence(
    state: &AppState,
    request: Option<&PlaybackRequestSequence>,
) -> bool {
    state.playback_sequencer().accept(request)
}

pub(crate) fn is_current_playback_request_sequence(
    state: &AppState,
    request: Option<&PlaybackRequestSequence>,
) -> bool {
    request.is_none_or(|request| state.playback_sequencer().is_current(request))
}

pub(crate) fn is_current_playback_sequence(
    state: &AppState,
    expected: &PlaybackRequestSequence,
) -> bool {
    state.playback_sequencer().is_current(expected)
}

pub(crate) fn playback_request_sequence_is_stale(
    state: &AppState,
    request: Option<&PlaybackRequestSequence>,
) -> bool {
    state.playback_sequencer().is_stale(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::app_state;
    use std::time::Duration;

    #[test]
    fn playback_sequence_eviction_removes_least_recently_seen_client() {
        let state = app_state("playback-sequence-lru");
        assert!(accept_playback_request_sequence(
            &state,
            Some(&playback_sequence("client-a", 1))
        ));
        std::thread::sleep(Duration::from_millis(1));
        assert!(accept_playback_request_sequence(
            &state,
            Some(&playback_sequence("client-b", 1))
        ));
        std::thread::sleep(Duration::from_millis(1));
        assert!(accept_playback_request_sequence(
            &state,
            Some(&playback_sequence("client-a", 2))
        ));
        std::thread::sleep(Duration::from_millis(1));

        for idx in 0..(MAX_PLAYBACK_SEQUENCE_CLIENTS - 1) {
            assert!(accept_playback_request_sequence(
                &state,
                Some(&playback_sequence(&format!("client-{idx}"), 1))
            ));
        }

        let sequences = state.playback_sequencer().snapshot();
        assert!(!sequences.contains_key("client-b"));
        assert_eq!(sequences.get("client-a").copied(), Some(2));
    }

    #[test]
    fn stale_playback_sequence_is_rejected() {
        let state = app_state("playback-sequence-stale");
        let latest = playback_sequence("client-a", 2);
        let stale = playback_sequence("client-a", 1);

        assert!(accept_playback_request_sequence(&state, Some(&latest)));

        assert!(playback_request_sequence_is_stale(&state, Some(&stale)));
        assert!(!is_current_playback_request_sequence(&state, Some(&stale)));
        assert!(!accept_playback_request_sequence(&state, Some(&stale)));
        assert!(is_current_playback_request_sequence(&state, None));
    }

    fn playback_sequence(client: &str, sequence: u64) -> PlaybackRequestSequence {
        PlaybackRequestSequence::new(client, sequence)
    }
}
