use super::{PlaybackConfig, SourceRef, SyncSignalPath};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum CoreToAgentCommand {
    PlaySource {
        source_ref: SourceRef,
        queue: Vec<SourceRef>,
        playback_config: PlaybackConfig,
        stream_base_url: String,
    },
    PreFetch {
        source_ref: SourceRef,
        stream_base_url: String,
    },
    Pause,
    Resume,
    Stop,
    Next,
    Seek {
        seconds: f64,
    },
    SetQueue {
        queue: Vec<SourceRef>,
    },
    SetLoopMode {
        repeat_one: bool,
    },
    SetPlaybackConfig {
        playback_config: PlaybackConfig,
    },
    /// Browser-agent liveness marker. It deliberately carries no state and is
    /// sent only to browser agents served by the matching web bundle.
    Heartbeat,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutputDeviceCapabilities {
    pub name: String,
    #[serde(default)]
    pub backend: Option<String>,
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
    #[serde(default)]
    pub supports_dsd128: bool,
    #[serde(default)]
    pub supports_dsd256: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentCapabilities {
    pub output_devices: Vec<String>,
    #[serde(default)]
    pub output_device_capabilities: Vec<OutputDeviceCapabilities>,
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
    pub exclusive_supported: bool,
    #[serde(default)]
    pub supports_dsd128: bool,
    #[serde(default)]
    pub supports_dsd256: bool,
    /// True for in-browser agents. Browser zones are private to the browser
    /// session that registered them: they are hidden from other clients and
    /// can never become the server-wide active zone.
    #[serde(default)]
    pub browser: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum AgentToCoreMessage {
    Register {
        agent_id: String,
        name: String,
        capabilities: AgentCapabilities,
    },
    PlaybackState(AgentPlaybackState),
    BufferState(AgentBufferState),
    SyncSignalPath(SyncSignalPath),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AgentPlaybackState {
    pub state: String,
    /// Stable identity for the source actually rendered by the agent. Older
    /// agents omit this and remain supported through metadata inference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_source: Option<SourceRef>,
    pub file_name: Option<String>,
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub source_rate: u32,
    pub target_rate: u32,
    pub source_bits: u32,
    pub target_bits: u32,
    pub volume: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AgentBufferState {
    pub buffered_next: Option<String>,
    pub prefetching: bool,
    pub buffered_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_playback_state_without_current_source_remains_compatible() {
        let playback: AgentPlaybackState = serde_json::from_value(serde_json::json!({
            "state": "Playing",
            "file_name": "Artist - Track",
            "track_title": "Track",
            "track_artist": "Artist",
            "track_album": "Album",
            "position_secs": 1.0,
            "duration_secs": 180.0,
            "source_rate": 44100,
            "target_rate": 44100,
            "source_bits": 16,
            "target_bits": 24,
            "volume": 1.0
        }))
        .unwrap();

        assert!(playback.current_source.is_none());
    }

    #[test]
    fn heartbeat_uses_stable_wire_name() {
        let value = serde_json::to_value(CoreToAgentCommand::Heartbeat).unwrap();
        assert_eq!(value, serde_json::json!({ "type": "heartbeat" }));
    }
}
