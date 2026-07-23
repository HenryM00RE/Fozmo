use super::{PersistedSettings, RemoteAccessSettings};
use std::path::Path;

pub(crate) fn parse_settings(path: &Path, body: &str) -> Result<PersistedSettings, String> {
    let mut settings: PersistedSettings = serde_json::from_str(body)
        .map_err(|error| format!("parse settings {:?}: {error}", path))?;
    settings.normalize_profiles();
    for playback in settings.zone_settings.values_mut() {
        playback.normalize_names();
    }
    Ok(settings)
}

/// Normalize and validate remote access settings before they are persisted.
///
/// Empty-string optional fields are normalized to `None`. `external_host` is a
/// display hint only, so no reachability validation happens here.
pub fn validate_remote_access(
    settings: &mut RemoteAccessSettings,
    app_port: u16,
) -> Result<(), String> {
    settings.external_host = normalize_optional(settings.external_host.take());
    settings.custom_cert_path = normalize_optional(settings.custom_cert_path.take());
    settings.custom_key_path = normalize_optional(settings.custom_key_path.take());

    if settings.port == app_port {
        return Err(format!(
            "remote access port {} collides with the app port",
            settings.port
        ));
    }
    if settings.custom_cert_path.is_some() != settings.custom_key_path.is_some() {
        return Err("custom_cert_path and custom_key_path must be configured together".to_string());
    }
    Ok(())
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ListeningProfile;

    #[test]
    fn malformed_settings_body_is_rejected() {
        let error = parse_settings(Path::new("malformed-settings.json"), "{ nope").unwrap_err();
        assert!(error.contains("malformed-settings.json"));
    }

    #[test]
    fn invalid_profiles_are_normalized_with_valid_active_fallback() {
        let settings = PersistedSettings {
            profiles: Some(vec![
                ListeningProfile {
                    id: String::new(),
                    name: "Missing id".to_string(),
                    color: "#4f84a5".to_string(),
                    image: None,
                    recent_searches: Vec::new(),
                },
                ListeningProfile {
                    id: "night".to_string(),
                    name: "Night".to_string(),
                    color: "#59806c".to_string(),
                    image: None,
                    recent_searches: Vec::new(),
                },
            ]),
            active_profile_id: Some("missing".to_string()),
            ..PersistedSettings::default()
        };

        let profiles = settings.normalized_profiles();

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "night");
        assert_eq!(settings.active_profile_id(), "night");
    }

    #[test]
    fn remote_access_defaults_to_disabled() {
        let settings = RemoteAccessSettings::default();

        assert!(!settings.enabled);
        assert_eq!(settings.port, 8443);
        assert!(settings.external_host.is_none());
        assert!(settings.custom_cert_path.is_none());
        assert!(settings.custom_key_path.is_none());
        assert!(!PersistedSettings::default().remote_access.enabled);
    }

    #[test]
    fn partial_remote_access_json_preserves_safe_defaults() {
        let settings = parse_settings(
            Path::new("partial-remote-access.json"),
            r#"{ "remote_access": { "port": 9443 } }"#,
        )
        .unwrap();

        assert!(!settings.remote_access.enabled);
        assert_eq!(settings.remote_access.port, 9443);

        let missing = parse_settings(Path::new("no-remote-access.json"), "{}").unwrap();
        assert!(!missing.remote_access.enabled);
        assert_eq!(missing.remote_access.port, 8443);
    }

    #[test]
    fn remote_access_port_collision_is_rejected() {
        let mut settings = RemoteAccessSettings {
            port: 3000,
            ..RemoteAccessSettings::default()
        };

        assert!(validate_remote_access(&mut settings, 3000).is_err());
        assert!(validate_remote_access(&mut settings, 3001).is_ok());
    }

    #[test]
    fn one_sided_custom_cert_config_is_rejected() {
        let mut cert_only = RemoteAccessSettings {
            custom_cert_path: Some("/tls/cert.pem".to_string()),
            ..RemoteAccessSettings::default()
        };
        assert!(validate_remote_access(&mut cert_only, 3000).is_err());

        let mut key_only = RemoteAccessSettings {
            custom_key_path: Some("/tls/key.pem".to_string()),
            ..RemoteAccessSettings::default()
        };
        assert!(validate_remote_access(&mut key_only, 3000).is_err());

        let mut both = RemoteAccessSettings {
            custom_cert_path: Some("/tls/cert.pem".to_string()),
            custom_key_path: Some("/tls/key.pem".to_string()),
            ..RemoteAccessSettings::default()
        };
        assert!(validate_remote_access(&mut both, 3000).is_ok());
    }

    #[test]
    fn empty_remote_access_strings_normalize_to_none() {
        let mut settings = RemoteAccessSettings {
            external_host: Some("  ".to_string()),
            custom_cert_path: Some(String::new()),
            custom_key_path: Some(String::new()),
            ..RemoteAccessSettings::default()
        };

        validate_remote_access(&mut settings, 3000).unwrap();

        assert!(settings.external_host.is_none());
        assert!(settings.custom_cert_path.is_none());
        assert!(settings.custom_key_path.is_none());
    }

    #[test]
    fn partial_hegel_settings_json_uses_current_defaults() {
        let settings = parse_settings(
            Path::new("partial-hegel-settings.json"),
            r#"{
                "hegel": {
                    "enabled": true,
                    "host": "192.168.1.50"
                }
            }"#,
        )
        .unwrap();

        assert!(settings.hegel.enabled);
        assert_eq!(settings.hegel.host.as_deref(), Some("192.168.1.50"));
        assert_eq!(settings.hegel.port, 50001);
        assert_eq!(settings.hegel.input, 9);
        assert_eq!(settings.hegel.default_volume, 20);
        assert_eq!(settings.hegel.max_volume, 50);
    }

    #[test]
    fn legacy_playback_json_falls_back_for_zone_settings() {
        let settings = parse_settings(
            Path::new("legacy-playback-settings.json"),
            r#"{
                "filter_type": "Minimum16k",
                "target_rate": 384000,
                "upsampling_enabled": false,
                "exclusive": true,
                "dither_mode": "Tpdf",
                "output_mode": "Dsd256",
                "dsd_modulator": "EcBeam2",
                "dsd_rules_enabled": true,
                "dsd_rules": [
                    {
                        "source_rate": 176400,
                        "filter_type": "Minimum16k",
                        "output_mode": "Dsd128"
                    }
                ],
                "headroom_db": -6.0,
                "device_name": "Legacy DAC",
                "volume": 0.42
            }"#,
        )
        .unwrap();

        let playback = settings.playback_for_zone("local-core");

        assert_eq!(playback.filter_type.as_deref(), Some("Minimum16k"));
        assert_eq!(playback.target_rate, Some(384_000));
        assert_eq!(playback.upsampling_enabled, Some(false));
        assert_eq!(playback.exclusive, Some(true));
        assert_eq!(playback.dither_mode.as_deref(), Some("Tpdf"));
        assert_eq!(playback.output_mode.as_deref(), Some("Dsd256"));
        assert_eq!(playback.dsd_modulator.as_deref(), Some("7th-order-search"));
        assert!(playback.dsd_rules_enabled);
        assert_eq!(playback.dsd_rules.len(), 1);
        assert_eq!(playback.dsd_rules[0].source_rate, 176_400);
        assert_eq!(playback.headroom_db, Some(-6.0));
        assert_eq!(playback.device_name.as_deref(), Some("Legacy DAC"));
        assert_eq!(playback.volume, Some(0.42));
    }
}
