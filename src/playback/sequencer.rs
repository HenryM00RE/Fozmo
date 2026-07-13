use crate::app::identity;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(crate) const PLAYBACK_CLIENT_HEADER: &str = identity::PLAYBACK_CLIENT_HEADER;
pub(crate) const PLAYBACK_SEQUENCE_HEADER: &str = identity::PLAYBACK_SEQUENCE_HEADER;
pub(crate) const MAX_PLAYBACK_SEQUENCE_CLIENTS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaybackRequestSequence {
    pub(crate) client_id: String,
    pub(crate) sequence: u64,
}

impl PlaybackRequestSequence {
    pub(crate) fn new(client_id: impl Into<String>, sequence: u64) -> Self {
        Self {
            client_id: client_id.into().chars().take(128).collect(),
            sequence,
        }
    }
}

#[derive(Clone, Default)]
pub struct PlaybackCommandSequencer {
    sequences: Arc<Mutex<HashMap<String, (u64, Instant)>>>,
}

impl PlaybackCommandSequencer {
    pub(crate) fn accept(&self, request: Option<&PlaybackRequestSequence>) -> bool {
        let Some(request) = request else {
            return true;
        };
        let mut sequences = self.sequences.lock().unwrap();
        if sequences
            .get(&request.client_id)
            .is_some_and(|(latest, _)| request.sequence < *latest)
        {
            return false;
        }
        sequences.insert(
            request.client_id.clone(),
            (request.sequence, Instant::now()),
        );
        if sequences.len() > MAX_PLAYBACK_SEQUENCE_CLIENTS
            && let Some(remove_key) = sequences
                .iter()
                .min_by_key(|(_, (_, last_seen))| *last_seen)
                .map(|(client, _)| client.clone())
        {
            sequences.remove(&remove_key);
        }
        true
    }

    pub(crate) fn is_current(&self, expected: &PlaybackRequestSequence) -> bool {
        self.sequences
            .lock()
            .unwrap()
            .get(&expected.client_id)
            .is_none_or(|(latest, _)| expected.sequence >= *latest)
    }

    pub(crate) fn is_stale(&self, request: Option<&PlaybackRequestSequence>) -> bool {
        let Some(request) = request else {
            return false;
        };
        self.sequences
            .lock()
            .unwrap()
            .get(&request.client_id)
            .is_some_and(|(latest, _)| request.sequence < *latest)
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> HashMap<String, u64> {
        self.sequences
            .lock()
            .unwrap()
            .iter()
            .map(|(client, (sequence, _))| (client.clone(), *sequence))
            .collect()
    }
}
