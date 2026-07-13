use crate::audio::{airplay, sonos};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub(super) fn normalized_zone_id(zone_id: &str) -> Option<String> {
    let trimmed = zone_id.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn default_zone_name() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "This PC".to_string())
        });
    let pretty = user
        .split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    format!("{}{}", first.to_uppercase(), chars.as_str().to_lowercase())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{pretty}'s PC Default")
}

pub fn local_device_zone_id(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    format!("local-{}", URL_SAFE_NO_PAD.encode(&digest[..9]))
}

pub(super) fn short_zone_name(name: &str) -> String {
    if let Some(target) = sonos::parse_target_device_name(name) {
        return target.name;
    }
    if let Some(target) = airplay::parse_target_device_name(name) {
        return target.name;
    }
    name.strip_prefix("ASIO: ")
        .unwrap_or(name)
        .replace("MacBook Pro Speakers", "MacBook speakers")
        .replace("Speakers (", "")
        .replace(')', "")
}
