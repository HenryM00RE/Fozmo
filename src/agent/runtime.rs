use super::prefetch::AgentStreamHandle;
use crate::protocol::SourceRef;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub(super) struct AgentRuntimeState {
    pub(super) queue: VecDeque<SourceRef>,
    pub(super) prefetched: HashMap<String, AgentStreamHandle>,
    pub(super) engine_prefetched: Option<EnginePrefetchedSource>,
    pub(super) current_source: Option<SourceRef>,
    pub(super) current_started_at: Option<Instant>,
    pub(super) stream_base_url: Option<String>,
    pub(super) generation: u64,
    pub(super) loading_generation: Option<u64>,
    pub(super) prefetching_key: Option<String>,
    pub(super) was_active: bool,
    pub(super) skip_requested: bool,
    pub(super) repeat_one: bool,
}

impl AgentRuntimeState {
    pub(super) fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            prefetched: HashMap::new(),
            engine_prefetched: None,
            current_source: None,
            current_started_at: None,
            stream_base_url: None,
            generation: 0,
            loading_generation: None,
            prefetching_key: None,
            was_active: false,
            skip_requested: false,
            repeat_one: false,
        }
    }
}

pub(super) const AGENT_PENDING_START_GRACE: Duration = Duration::from_secs(20);

pub(super) struct EnginePrefetchedSource {
    pub(super) source: SourceRef,
    pub(super) buffered_bytes: u64,
    pub(super) observed_in_player_queue: bool,
}
