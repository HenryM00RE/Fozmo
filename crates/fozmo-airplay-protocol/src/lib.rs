//! Process-independent AirPlay helper protocol.
//!
//! This crate is intentionally MIT licensed and contains data contracts only.
//! It does not implement or link any AirPlay protocol.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PROTOCOL_VERSION: u16 = 1;
pub const PCM_SAMPLE_RATE: u32 = 44_100;
pub const PCM_CHANNELS: u8 = 2;
pub const PCM_BITS_PER_SAMPLE: u8 = 16;
pub const DEFAULT_SOCKET_ENV: &str = "FOZMO_AIRPLAY_SOCKET";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    #[default]
    Raop,
    AirPlay2,
}

/// Coarse, display-safe receiver information. Network connection parameters,
/// TXT records, pairing data, and transport feature masks intentionally never
/// cross the process boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receiver {
    pub id: String,
    pub name: String,
    pub service_kind: ServiceKind,
    pub online: bool,
    pub supported: bool,
    pub unsupported_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Artwork {
    pub mime: String,
    /// Standard base64-encoded image bytes. Kept out of the PCM stream so the
    /// stream remains a documented, reusable audio format.
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub artwork: Option<Artwork>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Hello,
    ListReceivers,
    Open {
        receiver_id: String,
        metadata: Metadata,
        initial_volume: Option<f32>,
    },
    Pause {
        stream_id: String,
    },
    Resume {
        stream_id: String,
    },
    Flush {
        stream_id: String,
    },
    SetVolume {
        stream_id: String,
        volume: f32,
    },
    SetMetadata {
        stream_id: String,
        metadata: Metadata,
    },
    Close {
        stream_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlRequest {
    pub version: u16,
    pub request_id: u64,
    #[serde(flatten)]
    pub command: Command,
}

impl ControlRequest {
    pub fn new(request_id: u64, command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            command,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsePayload {
    Hello {
        protocol_version: u16,
        helper_version: String,
        capabilities: Vec<String>,
    },
    Receivers {
        receivers: Vec<Receiver>,
    },
    Opened {
        stream_id: String,
        pcm_socket: PathBuf,
    },
    Ack,
    Volume {
        volume: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlResponse {
    pub version: u16,
    pub request_id: u64,
    #[serde(flatten)]
    pub result: ControlResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ControlResult {
    Ok { payload: ResponsePayload },
    Error { code: String, message: String },
}

impl ControlResponse {
    pub fn ok(request_id: u64, payload: ResponsePayload) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            result: ControlResult::Ok { payload },
        }
    }

    pub fn error(request_id: u64, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            result: ControlResult::Error {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamAttach {
    pub version: u16,
    #[serde(rename = "type")]
    pub kind: String,
    pub stream_id: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub byte_order: String,
}

impl StreamAttach {
    pub fn new(stream_id: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            kind: "stream_attach".to_string(),
            stream_id: stream_id.into(),
            sample_rate: PCM_SAMPLE_RATE,
            channels: PCM_CHANNELS,
            bits_per_sample: PCM_BITS_PER_SAMPLE,
            byte_order: "little".to_string(),
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != PROTOCOL_VERSION {
            return Err("incompatible protocol version");
        }
        if self.kind != "stream_attach" {
            return Err("invalid stream attachment type");
        }
        if self.sample_rate != PCM_SAMPLE_RATE
            || self.channels != PCM_CHANNELS
            || self.bits_per_sample != PCM_BITS_PER_SAMPLE
            || self.byte_order != "little"
        {
            return Err("unsupported PCM format");
        }
        Ok(())
    }
}

pub fn default_control_socket() -> PathBuf {
    if let Some(path) = std::env::var_os(DEFAULT_SOCKET_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    std::env::temp_dir()
        .join("fozmo-airplay")
        .join("control.sock")
}

pub fn pcm_socket_for(control_socket: &Path) -> PathBuf {
    control_socket
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("pcm.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_round_trip_is_versioned_and_tagged() {
        let request = ControlRequest::new(
            42,
            Command::Open {
                receiver_id: "opaque-1".into(),
                metadata: Metadata::default(),
                initial_volume: Some(0.5),
            },
        );
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"type\":\"open\""));
        assert_eq!(
            serde_json::from_str::<ControlRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn stream_header_rejects_format_drift() {
        let mut attach = StreamAttach::new("stream-1");
        assert_eq!(attach.validate(), Ok(()));
        attach.sample_rate = 48_000;
        assert_eq!(attach.validate(), Err("unsupported PCM format"));
    }

    #[test]
    fn commands_expose_receiver_id_but_no_network_target() {
        let json = serde_json::to_value(ControlRequest::new(
            7,
            Command::Open {
                receiver_id: "opaque".into(),
                metadata: Metadata::default(),
                initial_volume: None,
            },
        ))
        .unwrap();
        assert!(json.get("receiver_id").is_some());
        assert!(json.get("host").is_none());
        assert!(json.get("port").is_none());
    }

    #[test]
    fn receiver_wire_shape_is_coarse_and_display_only() {
        let receiver = Receiver {
            id: "opaque".into(),
            name: "Kitchen".into(),
            service_kind: ServiceKind::AirPlay2,
            online: true,
            supported: true,
            unsupported_reason: None,
        };
        let json = serde_json::to_value(receiver).unwrap();
        for forbidden in [
            "host",
            "port",
            "features",
            "encryption_types",
            "device_id",
            "group_id",
            "service_name",
        ] {
            assert!(json.get(forbidden).is_none(), "wire leaked {forbidden}");
        }
        assert!(json.get("network_address").is_none());
    }
}
