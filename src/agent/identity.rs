use crate::app::identity;
use md5::Digest;
use rand::RngCore;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn resolve_core_url() -> String {
    explicit_core_url().unwrap_or_else(|| {
        eprintln!(
            "agent: no core URL supplied; using http://127.0.0.1:3000. \
Pass --core-url or FOZMO_CORE_URL to connect to a LAN core."
        );
        "http://127.0.0.1:3000".to_string()
    })
}

fn explicit_core_url() -> Option<String> {
    std::env::var(identity::env_key("CORE_URL"))
        .ok()
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            std::env::args().find_map(|arg| {
                arg.strip_prefix("--core-url=")
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .map(str::to_string)
            })
        })
}

pub(super) fn agent_ws_url(core_url: &str) -> Result<String, String> {
    let mut base = core_url.trim_end_matches('/').to_string();
    if base.starts_with("https://") {
        base = base.replacen("https://", "wss://", 1);
    } else if base.starts_with("http://") {
        base = base.replacen("http://", "ws://", 1);
    } else {
        base = format!("ws://{base}");
    }
    Ok(format!("{base}/api/agent/ws"))
}

pub(super) fn stable_agent_id(name: &str) -> String {
    if let Some(agent_id) = std::env::var(identity::env_key("AGENT_ID"))
        .ok()
        .and_then(|id| normalize_agent_id(&id))
    {
        return agent_id;
    }

    if let Some(path) = agent_id_file_path() {
        if let Ok(existing) = fs::read_to_string(&path)
            && let Some(agent_id) = normalize_agent_id(&existing)
        {
            return agent_id;
        }

        let agent_id = generate_agent_id();
        if write_agent_id_file(&path, &agent_id).is_ok() {
            return agent_id;
        }
        eprintln!(
            "agent: could not persist agent id at {}; using hostname fallback",
            path.display()
        );
    }

    let seed = format!("{name}:{}", hostname_fallback());
    format!("agent-{:x}", md5::Md5::digest(seed.as_bytes()))
}

fn normalize_agent_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("agent-") {
        Some(trimmed.to_string())
    } else {
        Some(format!("agent-{trimmed}"))
    }
}

fn generate_agent_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut body = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        body.push_str(&format!("{byte:02x}"));
    }
    format!("agent-{body}")
}

fn agent_id_file_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(identity::env_key("AGENT_ID_FILE")) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    agent_config_dir().map(|dir| dir.join("agent-id"))
}

fn agent_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .map(|dir| dir.join(identity::DATA_DIR_NAME))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|home| {
            home.join("Library")
                .join("Application Support")
                .join(identity::DATA_DIR_NAME)
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".config"))
            })
            .map(|dir| dir.join(identity::APP_SLUG))
    }
}

fn write_agent_id_file(path: &Path, agent_id: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{agent_id}\n"))
}

pub(super) fn hostname_fallback() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Windows PC".to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize_agent_id;

    #[test]
    fn agent_ids_are_normalized_once() {
        assert_eq!(
            normalize_agent_id("0123456789abcdef").as_deref(),
            Some("agent-0123456789abcdef")
        );
        assert_eq!(
            normalize_agent_id(" agent-existing ").as_deref(),
            Some("agent-existing")
        );
        assert!(normalize_agent_id("   ").is_none());
    }
}
