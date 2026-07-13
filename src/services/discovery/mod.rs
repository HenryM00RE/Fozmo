use crate::app::identity;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

pub struct CoreMdnsAdvertisement {
    _daemon: ServiceDaemon,
}

pub fn advertise_core(
    instance_name: &str,
    port: u16,
    public_base_url: &str,
    pairing_required: bool,
) -> Result<CoreMdnsAdvertisement, String> {
    let mdns = ServiceDaemon::new().map_err(|e| format!("start mDNS advertiser: {e}"))?;
    let hostname = format!("{}.local.", local_hostname_label());
    let pairing = if pairing_required {
        "required"
    } else {
        "optional"
    };
    let browser_base_url = local_browser_base_url(port);
    let properties = [
        ("path", "/"),
        ("version", env!("CARGO_PKG_VERSION")),
        ("pairing", pairing),
        ("base_url", browser_base_url.as_str()),
        ("fallback_url", public_base_url),
    ];
    for service_type in [
        identity::MDNS_CORE_SERVICE_TYPE,
        identity::MDNS_HTTP_SERVICE_TYPE,
    ] {
        let service = ServiceInfo::new(
            service_type,
            instance_name,
            &hostname,
            "",
            port,
            &properties[..],
        )
        .map_err(|e| format!("create {service_type} mDNS service info: {e}"))?
        .enable_addr_auto();
        mdns.register(service)
            .map_err(|e| format!("register {service_type} mDNS service: {e}"))?;
    }
    Ok(CoreMdnsAdvertisement { _daemon: mdns })
}

pub fn default_public_base_url(port: u16, lan_enabled: bool) -> String {
    if lan_enabled && let Some(ip) = primary_lan_ip() {
        return format!("http://{ip}:{port}");
    }
    format!("http://127.0.0.1:{port}")
}

pub fn primary_lan_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(addr) if !addr.ip().is_loopback() => Some(*addr.ip()),
        _ => None,
    }
}

/// Return the addresses currently assigned to local interfaces. These are
/// used for strict Host-header validation; accepting any syntactically valid
/// IP would leave the LAN listener exposed to DNS rebinding through an
/// attacker-selected numeric host.
pub fn active_interface_ips() -> Vec<IpAddr> {
    let mut addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|address| !address.is_unspecified())
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    addresses
}

pub fn local_hostname_label() -> String {
    let configured = std::env::var(identity::env_key("MDNS_HOSTNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty());
    let candidate = configured.unwrap_or_else(|| hostname_fallback(identity::APP_SLUG));
    let candidate = candidate
        .trim()
        .trim_end_matches('.')
        .strip_suffix(".local")
        .unwrap_or(candidate.trim().trim_end_matches('.'));
    let label = sanitize_dns_label(candidate).to_ascii_lowercase();
    if label.is_empty() {
        identity::APP_SLUG.to_string()
    } else {
        label
    }
}

pub fn local_browser_hostname() -> String {
    format!("{}.local", local_hostname_label())
}

pub fn local_browser_base_url(port: u16) -> String {
    format!("http://{}:{port}", local_browser_hostname())
}

/// Hostnames that identify this Mac and may safely be used by same-origin LAN
/// browsers. The OS hostname is trusted because it is local machine state; we
/// deliberately do not accept arbitrary DNS names merely because they resolve
/// to an active interface address, which would reopen DNS rebinding.
pub fn trusted_browser_hostnames() -> Vec<String> {
    let mut hostnames = vec![local_browser_hostname()];
    if let Some(hostname) = system_hostname() {
        hostnames.push(hostname);
    }
    hostnames.sort_unstable();
    hostnames.dedup();
    hostnames
}

pub fn system_hostname() -> Option<String> {
    platform_system_hostname().and_then(|hostname| canonical_system_hostname(&hostname))
}

#[cfg(target_os = "macos")]
fn platform_system_hostname() -> Option<String> {
    let mut buffer = [0 as libc::c_char; 256];
    // SAFETY: `buffer` is writable for the supplied length. It is initialized
    // with NUL bytes so a successful, non-truncated result always has a
    // terminator that can be located without reading beyond the array.
    if unsafe { libc::gethostname(buffer.as_mut_ptr(), buffer.len()) } != 0 {
        return None;
    }
    let end = buffer.iter().position(|byte| *byte == 0)?;
    let bytes = buffer[..end]
        .iter()
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8(bytes).ok()
}

#[cfg(not(target_os = "macos"))]
fn platform_system_hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
}

fn canonical_system_hostname(value: &str) -> Option<String> {
    if value.is_empty() || value.trim() != value || !value.is_ascii() {
        return None;
    }
    let hostname = value
        .strip_suffix('.')
        .unwrap_or(value)
        .to_ascii_lowercase();
    if hostname.is_empty()
        || hostname.len() > 253
        || hostname.parse::<IpAddr>().is_ok()
        || !hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        return None;
    }
    Some(hostname)
}

pub fn hostname_fallback(default_name: &str) -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| default_name.to_string())
}

fn sanitize_dns_label(value: &str) -> String {
    let label = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if label.is_empty() {
        format!("{}-core", identity::APP_SLUG)
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_base_url_uses_local_hostname_and_fixed_port() {
        let url = local_browser_base_url(3001);
        assert!(url.starts_with("http://"));
        assert!(url.ends_with(".local:3001"));
    }

    #[test]
    fn dns_labels_are_normalized_without_branding_an_arbitrary_suffix() {
        assert_eq!(sanitize_dns_label("Studio Mac.local"), "Studio-Mac-local");
        assert_eq!(sanitize_dns_label("---"), "fozmo-core");
    }

    #[test]
    fn system_hostname_normalization_is_strict_and_dns_safe() {
        assert_eq!(
            canonical_system_hostname("Core.Example.Test.").as_deref(),
            Some("core.example.test")
        );
        assert_eq!(
            canonical_system_hostname("studio-mac").as_deref(),
            Some("studio-mac")
        );
        for invalid in [
            "",
            " core.example.test",
            "core..example.test",
            "-core.example.test",
            "core-.example.test",
            "core.example.test/path",
            "user@core.example.test",
            "192.168.1.30",
        ] {
            assert_eq!(
                canonical_system_hostname(invalid),
                None,
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn trusted_browser_hostnames_include_local_and_system_names_without_duplicates() {
        let hostnames = trusted_browser_hostnames();
        assert!(hostnames.contains(&local_browser_hostname()));
        if let Some(hostname) = system_hostname() {
            assert!(hostnames.contains(&hostname));
        }
        assert!(hostnames.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
