use fozmo_airplay_protocol::ServiceKind;

pub const AIRPLAY2_GROUP_UNSUPPORTED_MESSAGE: &str =
    "AirPlay 2 groups/stereo pairs are not supported yet. Select a single speaker.";
pub const AIRPLAY2_ACCESS_UNSUPPORTED_MESSAGE: &str = "Connection refused. In Apple Home, set Speaker & TV Access to Anyone on the Same Network, then try again.";
pub const AIRPLAY_PASSWORD_UNSUPPORTED_MESSAGE: &str =
    "Password/PIN-protected AirPlay receivers are not supported yet.";
pub const AIRPLAY_FAIRPLAY_UNSUPPORTED_MESSAGE: &str = concat!(
    "FairPlay-only AirPlay receivers are not supported yet. ",
    "Use the system AirPlay/CoreAudio output for this receiver."
);

const AIRPLAY_ENCRYPTION_NONE: u8 = 0;
const AIRPLAY_ENCRYPTION_RSA: u8 = 1;
const AIRPLAY_ENCRYPTION_MFI_SAP: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirPlayTarget {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub model: Option<String>,
    pub service_name: String,
    pub password_protected: bool,
    pub requires_encryption: bool,
    pub encryption_types: Vec<u8>,
    pub service_kind: ServiceKind,
    pub device_id: Option<String>,
    pub features: Option<String>,
    pub source_version: Option<String>,
    pub grouped: bool,
    pub group_id: Option<String>,
    pub group_public_name: Option<String>,
    pub parent_group_id: Option<String>,
    pub tight_sync_id: Option<String>,
}

impl AirPlayTarget {
    pub fn unsupported_reason(&self) -> Option<&'static str> {
        if self.service_kind == ServiceKind::AirPlay2 && self.grouped {
            return Some(AIRPLAY2_GROUP_UNSUPPORTED_MESSAGE);
        }
        if self.service_kind == ServiceKind::AirPlay2 && self.password_protected {
            return Some(AIRPLAY2_ACCESS_UNSUPPORTED_MESSAGE);
        }
        if self.password_protected {
            return Some(AIRPLAY_PASSWORD_UNSUPPORTED_MESSAGE);
        }
        if self.requires_unsupported_encryption() {
            return Some(AIRPLAY_FAIRPLAY_UNSUPPORTED_MESSAGE);
        }
        None
    }

    pub fn uses_rsa_encryption(&self) -> bool {
        self.requires_encryption
            && (self.encryption_types.is_empty()
                || self.encryption_types.contains(&AIRPLAY_ENCRYPTION_RSA))
    }

    pub fn requires_mfi_auth_setup(&self) -> bool {
        self.requires_encryption
            && self.encryption_types.contains(&AIRPLAY_ENCRYPTION_MFI_SAP)
            && !self.uses_rsa_encryption()
    }

    pub fn prefers_airplay2_transport(&self) -> bool {
        self.service_kind == ServiceKind::AirPlay2 || self.is_modern_raop_endpoint()
    }

    fn requires_unsupported_encryption(&self) -> bool {
        self.requires_encryption
            && !self.encryption_types.is_empty()
            && !self.encryption_types.contains(&AIRPLAY_ENCRYPTION_RSA)
            && !self.encryption_types.contains(&AIRPLAY_ENCRYPTION_NONE)
    }

    fn is_modern_raop_endpoint(&self) -> bool {
        self.service_kind == ServiceKind::Raop
            && !self.encryption_types.contains(&AIRPLAY_ENCRYPTION_RSA)
            && self
                .model
                .as_deref()
                .is_some_and(|model| model.starts_with("AudioAccessory"))
            && (self.port == 7000
                || self
                    .encryption_types
                    .iter()
                    .any(|kind| matches!(kind, 3 | 5)))
    }
}
