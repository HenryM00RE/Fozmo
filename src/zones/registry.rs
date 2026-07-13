use crate::audio::output::device_caps::AudioDeviceCapabilities;
use crate::audio::player::Player;
use crate::protocol::{
    AgentBufferState, AgentCapabilities, AgentPlaybackState, CoreToAgentCommand, SourceRef,
    SyncSignalPath,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(super) type ZoneState = ZoneRegistry;

pub(super) struct ZoneRegistry {
    pub(super) active_zone_id: String,
    pub(super) preferred_active_zone_id: Option<String>,
    pub(super) local_zones: HashMap<String, LocalZoneEntry>,
    pub(super) local_device_capabilities: HashMap<String, AudioDeviceCapabilities>,
    pub(super) agents: HashMap<String, AgentEntry>,
    /// Monotonic id handed to each agent WebSocket registration so a stale
    /// socket's cleanup cannot unregister a newer connection's zones.
    pub(super) agent_connection_counter: u64,
}

pub(super) struct LocalZoneEntry {
    pub(super) name: String,
    pub(super) device_name: Option<String>,
    pub(super) player: Arc<Player>,
    pub(super) enabled: bool,
    pub(super) online: bool,
    pub(super) status_message: Option<String>,
}

pub(super) struct AgentEntry {
    pub(super) agent_id: String,
    /// The registering connection's id; see `agent_connection_counter`.
    pub(super) connection_id: u64,
    pub(super) agent_name: String,
    pub(super) name: String,
    pub(super) output_device: Option<String>,
    pub(super) capabilities: AgentCapabilities,
    pub(super) tx: mpsc::UnboundedSender<CoreToAgentCommand>,
    pub(super) browser: bool,
    pub(super) enabled: bool,
    pub(super) playback: Option<AgentPlaybackState>,
    pub(super) buffer: Option<AgentBufferState>,
    pub(super) signal_path: Option<SyncSignalPath>,
    pub(super) queued_sources: Vec<SourceRef>,
    pub(super) prefetched_source: Option<String>,
}
