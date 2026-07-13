//! MIT-side facade for the standalone AirPlay helper.
//!
//! This module contains no DNS-SD parsing, AirPlay network target, pairing,
//! encryption, RTSP, RTP, or codec implementation. Receivers cross the process
//! boundary as opaque IDs with coarse display/support state. Opening a stream
//! sends only an ID to the helper and writes documented standard PCM.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(all(not(test), feature = "airplay_helper"))]
use std::thread;
#[cfg(all(not(test), feature = "airplay_helper"))]
use std::time::Duration;

#[cfg(feature = "airplay_helper")]
pub(crate) mod helper_client;
pub mod pcm;
pub mod sender;

pub const AIRPLAY_DEVICE_PREFIX: &str = "AirPlay Helper:";
pub const AIRPLAY_SAMPLE_RATE: u32 = 44_100;
pub const AIRPLAY_BIT_DEPTH: u8 = 16;
pub const AIRPLAY2_GROUP_UNSUPPORTED_MESSAGE: &str =
    "AirPlay 2 groups/stereo pairs are not supported yet. Select a single speaker.";
pub const AIRPLAY2_ACCESS_UNSUPPORTED_MESSAGE: &str = "Connection refused. In Apple Home, set Speaker & TV Access to Anyone on the Same Network, then try again.";
pub const AIRPLAY_PASSWORD_UNSUPPORTED_MESSAGE: &str =
    "Password/PIN-protected AirPlay receivers are not supported yet.";
pub const AIRPLAY_FAIRPLAY_UNSUPPORTED_MESSAGE: &str = concat!(
    "FairPlay-only AirPlay receivers are not supported yet. ",
    "Use the system AirPlay/CoreAudio output for this receiver."
);
pub const AIRPLAY2_FEATURE_DISABLED_MESSAGE: &str =
    "The standalone AirPlay helper is disabled, missing, or incompatible.";

const AIRPLAY_DEVICE_VOLUME_EXPONENT: f32 = 2.0;

static TRUSTED_AIRPLAY_TARGETS: OnceLock<Mutex<HashMap<String, AirPlayTarget>>> = OnceLock::new();
static KNOWN_AIRPLAY_TARGETS: OnceLock<Mutex<HashMap<String, AirPlayTarget>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AirPlayServiceKind {
    #[default]
    Raop,
    AirPlay2,
}

/// Coarse receiver state safe to retain in the MIT process.
///
/// The opaque ID is meaningful only to the running helper. It is not a host,
/// MAC address, DNS-SD service record, or connection target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AirPlayTarget {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub service_kind: AirPlayServiceKind,
    #[serde(default = "default_supported")]
    pub supported: bool,
    #[serde(default)]
    pub unsupported_reason: Option<String>,
}

fn default_supported() -> bool {
    true
}

impl AirPlayTarget {
    pub fn unsupported_reason(&self) -> Option<String> {
        (!self.supported).then(|| {
            self.unsupported_reason
                .clone()
                .unwrap_or_else(|| "AirPlay receiver is unsupported".to_string())
        })
    }

    pub fn prefers_airplay2_transport(&self) -> bool {
        self.service_kind == AirPlayServiceKind::AirPlay2
    }
}

#[derive(Debug, Clone)]
pub struct AirPlayReceiver {
    pub target: AirPlayTarget,
    pub online: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AirPlayHelperStatus {
    Ready,
    Degraded,
    Missing,
    Incompatible,
    Disabled,
}

impl AirPlayHelperStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Missing => "missing",
            Self::Incompatible => "incompatible",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone)]
pub struct AirPlayRegistry {
    receivers: Arc<Mutex<HashMap<String, AirPlayReceiver>>>,
    helper_status: Arc<Mutex<AirPlayHelperStatus>>,
}

impl Default for AirPlayRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AirPlayRegistry {
    pub fn new() -> Self {
        let registry = Self {
            receivers: Arc::new(Mutex::new(HashMap::new())),
            helper_status: Arc::new(Mutex::new(if cfg!(feature = "airplay_helper") {
                AirPlayHelperStatus::Missing
            } else {
                AirPlayHelperStatus::Disabled
            })),
        };
        #[cfg(all(not(test), feature = "airplay_helper"))]
        registry.spawn_helper_polling();
        registry
    }

    pub fn receivers(&self) -> Vec<AirPlayReceiver> {
        self.receivers.lock().unwrap().values().cloned().collect()
    }

    pub fn trusted_target_from_device_name(&self, name: &str) -> Option<AirPlayTarget> {
        let id = parse_device_id(name)?;
        self.receivers
            .lock()
            .unwrap()
            .get(&id)
            .filter(|receiver| receiver.online && receiver.target.supported)
            .map(|receiver| receiver.target.clone())
    }

    pub fn is_trusted_device_name(&self, name: &str) -> bool {
        self.trusted_target_from_device_name(name).is_some()
    }

    pub fn helper_status(&self) -> AirPlayHelperStatus {
        *self.helper_status.lock().unwrap()
    }

    #[cfg(test)]
    pub fn set_receivers_for_test(&self, receivers: Vec<AirPlayReceiver>) {
        replace_receivers(&self.receivers, receivers);
    }

    #[cfg(all(not(test), feature = "airplay_helper"))]
    fn spawn_helper_polling(&self) {
        let receivers = Arc::clone(&self.receivers);
        let status = Arc::clone(&self.helper_status);
        thread::Builder::new()
            .name("AirPlayHelperDiscovery".to_string())
            .spawn(move || {
                loop {
                    let next_status = match helper_client::request(
                        fozmo_airplay_protocol::Command::ListReceivers,
                    ) {
                        Ok(fozmo_airplay_protocol::ResponsePayload::Receivers {
                            receivers: helper_receivers,
                        }) => {
                            replace_receivers(
                                &receivers,
                                helper_receivers
                                    .into_iter()
                                    .map(|receiver| AirPlayReceiver {
                                        target: AirPlayTarget {
                                            id: receiver.id,
                                            name: receiver.name,
                                            service_kind: match receiver.service_kind {
                                                fozmo_airplay_protocol::ServiceKind::Raop => {
                                                    AirPlayServiceKind::Raop
                                                }
                                                fozmo_airplay_protocol::ServiceKind::AirPlay2 => {
                                                    AirPlayServiceKind::AirPlay2
                                                }
                                            },
                                            supported: receiver.supported,
                                            unsupported_reason: receiver.unsupported_reason,
                                        },
                                        online: receiver.online,
                                    })
                                    .collect(),
                            );
                            AirPlayHelperStatus::Ready
                        }
                        Ok(_) => AirPlayHelperStatus::Degraded,
                        Err(error) => {
                            mark_all_offline(&receivers);
                            match error.kind {
                                helper_client::ErrorKind::Missing => AirPlayHelperStatus::Missing,
                                helper_client::ErrorKind::Incompatible => {
                                    AirPlayHelperStatus::Incompatible
                                }
                                helper_client::ErrorKind::Unavailable
                                | helper_client::ErrorKind::Protocol => {
                                    AirPlayHelperStatus::Degraded
                                }
                            }
                        }
                    };
                    *status.lock().unwrap() = next_status;
                    thread::sleep(Duration::from_secs(2));
                }
            })
            .expect("failed to spawn AirPlay helper polling thread");
    }
}

fn replace_receivers(
    receivers: &Arc<Mutex<HashMap<String, AirPlayReceiver>>>,
    incoming: Vec<AirPlayReceiver>,
) {
    let mut guard = receivers.lock().unwrap();
    for receiver in guard.values_mut() {
        receiver.online = false;
    }
    for receiver in incoming {
        guard.insert(receiver.target.id.clone(), receiver);
    }
    publish_trusted_targets(&guard);
}

#[cfg(feature = "airplay_helper")]
#[allow(dead_code)]
fn mark_all_offline(receivers: &Arc<Mutex<HashMap<String, AirPlayReceiver>>>) {
    let mut guard = receivers.lock().unwrap();
    for receiver in guard.values_mut() {
        receiver.online = false;
    }
    publish_trusted_targets(&guard);
}

fn trusted_targets() -> &'static Mutex<HashMap<String, AirPlayTarget>> {
    TRUSTED_AIRPLAY_TARGETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn known_targets() -> &'static Mutex<HashMap<String, AirPlayTarget>> {
    KNOWN_AIRPLAY_TARGETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn publish_trusted_targets(receivers: &HashMap<String, AirPlayReceiver>) {
    let mut known = known_targets().lock().unwrap();
    known.clear();
    for receiver in receivers.values() {
        known.insert(receiver.target.id.clone(), receiver.target.clone());
    }
    let mut trusted = trusted_targets().lock().unwrap();
    trusted.clear();
    for receiver in receivers
        .values()
        .filter(|receiver| receiver.online && receiver.target.supported)
    {
        trusted.insert(receiver.target.id.clone(), receiver.target.clone());
    }
}

pub fn is_airplay_device_name(name: &str) -> bool {
    name.trim_start().starts_with(AIRPLAY_DEVICE_PREFIX)
}

/// Produce a device selector containing only the helper's opaque receiver ID.
pub fn target_device_name(target: &AirPlayTarget) -> String {
    format!(
        "{AIRPLAY_DEVICE_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(target.id.as_bytes())
    )
}

fn parse_device_id(name: &str) -> Option<String> {
    let encoded = name.trim().strip_prefix(AIRPLAY_DEVICE_PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).ok()?;
    let id = String::from_utf8(bytes).ok()?;
    (!id.is_empty()).then_some(id)
}

/// Parse a selector only when the ID is in the helper-populated trust set.
pub fn parse_target_device_name(name: &str) -> Option<AirPlayTarget> {
    let id = parse_device_id(name)?;
    known_targets().lock().unwrap().get(&id).cloned()
}

pub fn parse_trusted_target_device_name(name: &str) -> Option<AirPlayTarget> {
    let id = parse_device_id(name)?;
    trusted_targets().lock().unwrap().get(&id).cloned()
}

pub fn unsupported_target_reason(device_name: Option<&str>) -> Option<String> {
    let device_name = device_name?;
    if !is_airplay_device_name(device_name) {
        return None;
    }
    match parse_target_device_name(device_name) {
        Some(target) => target.unsupported_reason(),
        None => Some("AirPlay receiver is unavailable or unsupported".to_string()),
    }
}

pub fn device_volume_to_transport_volume(volume: f32) -> f32 {
    normalize_volume(volume).powf(AIRPLAY_DEVICE_VOLUME_EXPONENT)
}

pub fn transport_volume_to_device_volume(volume: f32) -> f32 {
    normalize_volume(volume).powf(1.0 / AIRPLAY_DEVICE_VOLUME_EXPONENT)
}

pub fn normalize_device_volume(volume: f32) -> Option<f32> {
    volume.is_finite().then(|| volume.clamp(0.0, 1.0))
}

pub fn is_permanent_open_error(device_name: Option<&str>, message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    device_name.is_some_and(is_airplay_device_name)
        && (lower.contains("unknown_receiver")
            || lower.contains("unsupported")
            || lower.contains("incompatible")
            || lower.contains("not implemented"))
}

fn normalize_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub fn receiver_zone_id(target_id: &str) -> String {
    format!("airplay-{target_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str) -> AirPlayTarget {
        AirPlayTarget {
            id: id.to_string(),
            name: "Living Room".to_string(),
            service_kind: AirPlayServiceKind::AirPlay2,
            supported: true,
            unsupported_reason: None,
        }
    }

    #[test]
    fn selector_contains_only_opaque_id() {
        let target = target("opaque-receiver-7");
        let selector = target_device_name(&target);
        assert!(selector.starts_with(AIRPLAY_DEVICE_PREFIX));
        assert!(!selector.contains("Living Room"));
        assert!(!selector.contains("host"));
        assert_eq!(
            parse_device_id(&selector).as_deref(),
            Some("opaque-receiver-7")
        );
    }

    #[test]
    fn forged_or_unpublished_id_does_not_parse_as_target() {
        let selector = target_device_name(&target("forged"));
        trusted_targets().lock().unwrap().clear();
        known_targets().lock().unwrap().clear();
        assert_eq!(parse_target_device_name(&selector), None);
    }

    #[test]
    fn registry_publishes_only_online_supported_ids() {
        let registry = AirPlayRegistry::new();
        let trusted = target("trusted");
        let unsupported = AirPlayTarget {
            id: "unsupported".into(),
            name: "Pair".into(),
            service_kind: AirPlayServiceKind::AirPlay2,
            supported: false,
            unsupported_reason: Some(AIRPLAY2_GROUP_UNSUPPORTED_MESSAGE.into()),
        };
        registry.set_receivers_for_test(vec![
            AirPlayReceiver {
                target: trusted.clone(),
                online: true,
            },
            AirPlayReceiver {
                target: unsupported.clone(),
                online: true,
            },
        ]);
        assert_eq!(
            registry.trusted_target_from_device_name(&target_device_name(&trusted)),
            Some(trusted)
        );
        assert_eq!(
            registry.trusted_target_from_device_name(&target_device_name(&unsupported)),
            None
        );
    }

    #[test]
    fn volume_curve_round_trips() {
        for volume in [0.0, 0.1, 0.5, 1.0] {
            let round_trip =
                transport_volume_to_device_volume(device_volume_to_transport_volume(volume));
            assert!((round_trip - volume).abs() < 0.0001);
        }
    }
}
