use crate::AirPlayTarget;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fozmo_airplay_protocol::{Receiver, ServiceKind};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::thread;

const AIRPLAY2_SERVICE_TYPE: &str = "_airplay._tcp.local.";
const RAOP_SERVICE_TYPE: &str = "_raop._tcp.local.";
const AIRPLAY_ENCRYPTION_NONE: u8 = 0;

#[derive(Clone)]
pub struct Discovery {
    receivers: Arc<Mutex<HashMap<String, ReceiverEntry>>>,
}

#[derive(Clone)]
struct ReceiverEntry {
    target: AirPlayTarget,
    online: bool,
    services: HashMap<String, AirPlayTarget>,
}

impl Discovery {
    pub fn start() -> Self {
        let discovery = Self {
            receivers: Arc::new(Mutex::new(HashMap::new())),
        };
        discovery.spawn(AIRPLAY2_SERVICE_TYPE, "HelperAirPlay2Discovery");
        discovery.spawn(RAOP_SERVICE_TYPE, "HelperRaopDiscovery");
        discovery
    }

    pub fn receivers(&self) -> Vec<Receiver> {
        let guard = self.receivers.lock().unwrap();
        let mut receivers = guard
            .iter()
            .map(|(id, entry)| wire_receiver(id, entry))
            .collect::<Vec<_>>();
        receivers.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        receivers
    }

    pub fn online_target(&self, receiver_id: &str) -> Option<AirPlayTarget> {
        self.receivers
            .lock()
            .unwrap()
            .get(receiver_id)
            .filter(|entry| entry.online)
            .map(|entry| entry.target.clone())
    }

    fn spawn(&self, service_type: &'static str, thread_name: &'static str) {
        let receivers = Arc::clone(&self.receivers);
        thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || run_browser(service_type, receivers))
            .expect("failed to spawn AirPlay discovery thread");
    }
}

fn run_browser(service_type: &'static str, receivers: Arc<Mutex<HashMap<String, ReceiverEntry>>>) {
    let daemon = match ServiceDaemon::new() {
        Ok(daemon) => daemon,
        Err(error) => {
            eprintln!("airplay helper: failed to create mDNS daemon: {error}");
            return;
        }
    };
    let events = match daemon.browse(service_type) {
        Ok(events) => events,
        Err(error) => {
            eprintln!("airplay helper: failed to browse {service_type}: {error}");
            return;
        }
    };
    while let Ok(event) = events.recv() {
        match event {
            ServiceEvent::ServiceResolved(service) => {
                if let Some(receiver) = receiver_from_service(service_type, &service) {
                    upsert(&mut receivers.lock().unwrap(), receiver);
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                remove_service(&mut receivers.lock().unwrap(), &fullname);
            }
            _ => {}
        }
    }
}

fn upsert(receivers: &mut HashMap<String, ReceiverEntry>, incoming: AirPlayTarget) {
    // Never expose a receiver's MAC/device ID as the process-boundary handle.
    // The helper alone retains network identity and resolves this stable,
    // domain-separated opaque token back to its internal target.
    let id = opaque_receiver_id(&incoming.id);
    let service_name = incoming.service_name.clone();
    let entry = receivers.entry(id).or_insert_with(|| ReceiverEntry {
        target: incoming.clone(),
        online: true,
        services: HashMap::new(),
    });
    entry.services.insert(service_name, incoming);
    if let Some(target) = preferred_target(&entry.services) {
        entry.target = target;
        entry.online = true;
    }
}

fn remove_service(receivers: &mut HashMap<String, ReceiverEntry>, service_name: &str) {
    for entry in receivers.values_mut() {
        if entry.services.remove(service_name).is_some() {
            if let Some(target) = preferred_target(&entry.services) {
                entry.target = target;
                entry.online = true;
            } else {
                entry.online = false;
            }
        }
    }
}

fn preferred_target(services: &HashMap<String, AirPlayTarget>) -> Option<AirPlayTarget> {
    services
        .values()
        .max_by(|left, right| {
            preference(left)
                .cmp(&preference(right))
                .then_with(|| left.service_name.cmp(&right.service_name))
        })
        .cloned()
}

fn preference(target: &AirPlayTarget) -> u8 {
    match target.service_kind {
        ServiceKind::AirPlay2 => 3,
        ServiceKind::Raop if modern_raop_endpoint(target) => 1,
        ServiceKind::Raop => 2,
    }
}

fn modern_raop_endpoint(target: &AirPlayTarget) -> bool {
    target.service_kind == ServiceKind::Raop
        && !target.encryption_types.contains(&1)
        && target
            .model
            .as_deref()
            .is_some_and(|model| model.starts_with("AudioAccessory"))
        && (target.port == 7000
            || target
                .encryption_types
                .iter()
                .any(|kind| matches!(kind, 3 | 5)))
}

fn wire_receiver(opaque_id: &str, entry: &ReceiverEntry) -> Receiver {
    let unsupported_reason = entry.target.unsupported_reason().map(str::to_string);
    Receiver {
        id: opaque_id.to_string(),
        name: entry.target.name.clone(),
        service_kind: if entry.target.prefers_airplay2_transport() {
            ServiceKind::AirPlay2
        } else {
            ServiceKind::Raop
        },
        online: entry.online,
        supported: unsupported_reason.is_none(),
        unsupported_reason,
    }
}

fn opaque_receiver_id(internal_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"fozmo-airplay-receiver-v1\0");
    hasher.update(internal_id.as_bytes());
    format!("receiver-{}", URL_SAFE_NO_PAD.encode(hasher.finalize()))
}

fn receiver_from_service(service_type: &str, service: &ResolvedService) -> Option<AirPlayTarget> {
    if !service.is_valid() {
        return None;
    }
    let address = preferred_address(service.get_addresses())?;
    if service_type == AIRPLAY2_SERVICE_TYPE {
        return Some(airplay2_target(service, address));
    }
    Some(raop_target(service, address))
}

fn raop_target(service: &ResolvedService, address: IpAddr) -> AirPlayTarget {
    let instance_id = raop_instance_device_id(service.get_fullname());
    let device_id = service
        .get_property_val_str("deviceid")
        .or_else(|| service.get_property_val_str("id"))
        .map(normalize_id)
        .filter(|id| !id.is_empty())
        .or(instance_id);
    let encryption_types = service
        .get_property_val_str("et")
        .unwrap_or("")
        .split(',')
        .filter_map(|value| value.parse::<u8>().ok())
        .collect::<HashSet<_>>();
    let requires_encryption = !encryption_types.contains(&AIRPLAY_ENCRYPTION_NONE);
    let mut encryption_types = encryption_types.into_iter().collect::<Vec<_>>();
    encryption_types.sort_unstable();
    AirPlayTarget {
        id: device_id.unwrap_or_else(|| stable_id(service.get_fullname())),
        name: raop_display_name(service.get_fullname()),
        host: address.to_string(),
        port: service.get_port(),
        model: service
            .get_property_val_str("am")
            .or_else(|| service.get_property_val_str("md"))
            .map(str::to_string),
        service_name: service.get_fullname().to_string(),
        password_protected: bool_property(service, "pw"),
        requires_encryption,
        encryption_types,
        service_kind: ServiceKind::Raop,
        device_id: service
            .get_property_val_str("deviceid")
            .or_else(|| service.get_property_val_str("id"))
            .map(str::to_string),
        features: service
            .get_property_val_str("ft")
            .or_else(|| service.get_property_val_str("features"))
            .map(str::to_string),
        source_version: service
            .get_property_val_str("vs")
            .or_else(|| service.get_property_val_str("vn"))
            .map(str::to_string),
        grouped: false,
        group_id: None,
        group_public_name: None,
        parent_group_id: None,
        tight_sync_id: None,
    }
}

fn airplay2_target(service: &ResolvedService, address: IpAddr) -> AirPlayTarget {
    let id = service
        .get_property_val_str("deviceid")
        .or_else(|| service.get_property_val_str("id"))
        .map(normalize_id)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| stable_id(service.get_fullname()));
    AirPlayTarget {
        id,
        name: display_name(service.get_fullname(), AIRPLAY2_SERVICE_TYPE),
        host: address.to_string(),
        port: service.get_port(),
        model: service.get_property_val_str("model").map(str::to_string),
        service_name: service.get_fullname().to_string(),
        password_protected: bool_property(service, "pw"),
        requires_encryption: false,
        encryption_types: Vec::new(),
        service_kind: ServiceKind::AirPlay2,
        device_id: service
            .get_property_val_str("deviceid")
            .or_else(|| service.get_property_val_str("id"))
            .map(str::to_string),
        features: service
            .get_property_val_str("features")
            .or_else(|| service.get_property_val_str("ft"))
            .map(str::to_string),
        source_version: service.get_property_val_str("srcvers").map(str::to_string),
        grouped: ["gpn", "pgid", "pgcgl"].iter().any(|key| {
            service
                .get_property_val_str(key)
                .is_some_and(|value| !value.trim().is_empty() && value != "0")
        }),
        group_id: service.get_property_val_str("gid").map(str::to_string),
        group_public_name: service.get_property_val_str("gpn").map(str::to_string),
        parent_group_id: service.get_property_val_str("pgid").map(str::to_string),
        tight_sync_id: service.get_property_val_str("tsid").map(str::to_string),
    }
}

fn bool_property(service: &ResolvedService, key: &str) -> bool {
    service
        .get_property_val_str(key)
        .is_some_and(|value| matches!(value, "true" | "1"))
}

fn preferred_address(addresses: &HashSet<mdns_sd::ScopedIp>) -> Option<IpAddr> {
    let mut first = None;
    for address in addresses {
        let ip = address.to_ip_addr();
        first.get_or_insert(ip);
        if ip.is_ipv4() {
            return Some(ip);
        }
    }
    first
}

fn raop_display_name(fullname: &str) -> String {
    let instance = display_name(fullname, RAOP_SERVICE_TYPE);
    instance
        .split_once('@')
        .map(|(_, name)| name.to_string())
        .unwrap_or(instance)
}

fn raop_instance_device_id(fullname: &str) -> Option<String> {
    let instance = fullname
        .strip_suffix(RAOP_SERVICE_TYPE)
        .unwrap_or(fullname)
        .trim_end_matches('.');
    let (mac, _) = instance.split_once('@')?;
    let id = normalize_id(mac);
    (!id.is_empty()).then_some(id)
}

fn display_name(fullname: &str, service_type: &str) -> String {
    fullname
        .strip_suffix(service_type)
        .unwrap_or(fullname)
        .trim_end_matches('.')
        .replace("\\032", " ")
        .trim()
        .to_string()
}

fn normalize_id(id: &str) -> String {
    id.chars()
        .filter(|character| character.is_ascii_hexdigit())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

fn stable_id(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..9])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: ServiceKind, service: &str) -> AirPlayTarget {
        AirPlayTarget {
            id: "same".into(),
            name: "Kitchen".into(),
            host: "192.0.2.1".into(),
            port: 7000,
            model: None,
            service_name: service.into(),
            password_protected: false,
            requires_encryption: false,
            encryption_types: vec![],
            service_kind: kind,
            device_id: None,
            features: None,
            source_version: None,
            grouped: false,
            group_id: None,
            group_public_name: None,
            parent_group_id: None,
            tight_sync_id: None,
        }
    }

    #[test]
    fn airplay2_wins_same_receiver_collision() {
        let mut services = HashMap::new();
        services.insert("raop".into(), target(ServiceKind::Raop, "raop"));
        services.insert("ap2".into(), target(ServiceKind::AirPlay2, "ap2"));
        assert_eq!(
            preferred_target(&services).unwrap().service_kind,
            ServiceKind::AirPlay2
        );
    }

    #[test]
    fn wire_ids_are_stable_opaque_tokens() {
        assert_eq!(normalize_id("AA:bb:01-ff"), "aabb01ff");
        assert!(!stable_id("service.local").contains('.'));
        let opaque = opaque_receiver_id("aa:bb:cc:dd:ee:ff");
        assert_eq!(opaque, opaque_receiver_id("aa:bb:cc:dd:ee:ff"));
        assert_ne!(opaque, opaque_receiver_id("aa:bb:cc:dd:ee:00"));
        assert!(opaque.starts_with("receiver-"));
        assert!(!opaque.contains("aabbccddeeff"));
    }
}
