use super::*;
use super::{probe::*, soap::*};

impl UpnpRendererService {
    pub(super) fn spawn_discovery(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = discover_once(&service).await {
                    eprintln!("upnp: discovery failed: {e}");
                }
                tokio::time::sleep(UPNP_DISCOVERY_INTERVAL).await;
            }
        });
    }

    pub(super) fn upsert_discovered_renderer(&self, mut target: UpnpRendererTarget) -> bool {
        let cache_key = capability_probe_cache_key(&target);
        if let Some(cached) = self
            .capability_probe_cache
            .lock()
            .unwrap()
            .get(&cache_key)
            .cloned()
        {
            apply_probe_result_to_target(&mut target, cached);
        }

        if self
            .capability_probe_tasks
            .lock()
            .unwrap()
            .contains(&cache_key)
            && let Some(existing) = self.renderer_target(&target.id)
        {
            target.max_sample_rate = existing.max_sample_rate;
            target.max_bit_depth = existing.max_bit_depth;
            target.max_dsd_rate = existing.max_dsd_rate;
            target.capability_detection_source = existing.capability_detection_source;
            target.capability_detection_status = existing.capability_detection_status;
            target.capability_detection_message = existing.capability_detection_message;
        }

        let mut renderers = self.renderers.lock().unwrap();
        if let Some(existing) = renderers.get(&target.id) {
            match classify_upnp_target_refresh(&existing.target, &target) {
                UpnpTargetRefreshKind::SameOrigin | UpnpTargetRefreshKind::VerifiedEndpointMove => {
                }
                UpnpTargetRefreshKind::UnverifiedEndpointMove => {
                    eprintln!(
                        "upnp: renderer endpoint move could not be verified id={} existing_origin={} discovered_origin={}",
                        target.id,
                        upnp_target_origin_label(&existing.target),
                        upnp_target_origin_label(&target)
                    );
                    return false;
                }
                UpnpTargetRefreshKind::IdentityCollision => {
                    eprintln!(
                        "upnp: renderer identity collision id={} existing_origin={} discovered_origin={}",
                        target.id,
                        upnp_target_origin_label(&existing.target),
                        upnp_target_origin_label(&target)
                    );
                    return false;
                }
            }
        }
        renderers.insert(
            target.id.clone(),
            UpnpRenderer {
                target: target.clone(),
                online: true,
            },
        );
        true
    }
}

pub fn is_upnp_device_name(name: &str) -> bool {
    name.trim_start().starts_with(UPNP_DEVICE_PREFIX)
}

pub fn target_device_name(target: &UpnpRendererTarget) -> String {
    let body = serde_json::to_vec(target).unwrap_or_default();
    format!("{UPNP_DEVICE_PREFIX}{}", URL_SAFE_NO_PAD.encode(body))
}

pub fn parse_target_device_name(name: &str) -> Option<UpnpRendererTarget> {
    let encoded = name.trim().strip_prefix(UPNP_DEVICE_PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn target_capability_status_message(target: &UpnpRendererTarget) -> String {
    match target.capability_detection_status {
        CapabilityDetectionStatus::Complete => format!(
            "UPnP capabilities detected as up to {}/{}",
            target.max_bit_depth, target.max_sample_rate
        ),
        CapabilityDetectionStatus::Probing => "UPnP capability detection in progress".to_string(),
        CapabilityDetectionStatus::Deferred => target
            .capability_detection_message
            .clone()
            .unwrap_or_else(|| "UPnP capability detection deferred until idle".to_string()),
        CapabilityDetectionStatus::Failed | CapabilityDetectionStatus::Unknown => target
            .capability_detection_message
            .clone()
            .unwrap_or_else(|| {
                "UPnP capabilities unknown; using safe 16/48000 defaults until detected".to_string()
            }),
    }
}

pub fn receiver_zone_id(target_id: &str) -> String {
    format!("upnp-{target_id}")
}

pub fn upnp_target_origin_matches(
    stored: &UpnpRendererTarget,
    discovered: &UpnpRendererTarget,
) -> bool {
    stored.id == discovered.id
        && stored.host.eq_ignore_ascii_case(&discovered.host)
        && stored.port == discovered.port
        && upnp_av_transport_origin(stored) == upnp_av_transport_origin(discovered)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpnpTargetRefreshKind {
    SameOrigin,
    VerifiedEndpointMove,
    UnverifiedEndpointMove,
    IdentityCollision,
}

pub fn classify_upnp_target_refresh(
    stored: &UpnpRendererTarget,
    discovered: &UpnpRendererTarget,
) -> UpnpTargetRefreshKind {
    if stored.id != discovered.id {
        return UpnpTargetRefreshKind::IdentityCollision;
    }
    if upnp_target_origin_matches(stored, discovered) {
        return UpnpTargetRefreshKind::SameOrigin;
    }
    let same_host = stored.host.eq_ignore_ascii_case(&discovered.host);
    let metadata_verified = if same_host {
        upnp_target_stable_metadata_compatible(stored, discovered)
    } else {
        upnp_target_stable_identity_verified(stored, discovered)
    };
    if metadata_verified && upnp_target_control_urls_match_endpoint(discovered) {
        UpnpTargetRefreshKind::VerifiedEndpointMove
    } else if !same_host {
        UpnpTargetRefreshKind::IdentityCollision
    } else {
        UpnpTargetRefreshKind::UnverifiedEndpointMove
    }
}

fn upnp_target_stable_identity_verified(
    stored: &UpnpRendererTarget,
    discovered: &UpnpRendererTarget,
) -> bool {
    let required_match = |stored: Option<&str>, discovered: Option<&str>| {
        let stored = stored.map(str::trim).filter(|value| !value.is_empty());
        let discovered = discovered.map(str::trim).filter(|value| !value.is_empty());
        matches!((stored, discovered), (Some(left), Some(right)) if left.eq_ignore_ascii_case(right))
    };
    metadata_value_compatible(Some(&stored.name), Some(&discovered.name))
        && required_match(stored.model.as_deref(), discovered.model.as_deref())
        && required_match(
            stored.manufacturer.as_deref(),
            discovered.manufacturer.as_deref(),
        )
}

pub(super) fn upnp_target_stable_metadata_compatible(
    stored: &UpnpRendererTarget,
    discovered: &UpnpRendererTarget,
) -> bool {
    metadata_value_compatible(Some(&stored.name), Some(&discovered.name))
        && metadata_value_compatible(stored.model.as_deref(), discovered.model.as_deref())
        && metadata_value_compatible(
            stored.manufacturer.as_deref(),
            discovered.manufacturer.as_deref(),
        )
}

pub(super) fn metadata_value_compatible(stored: Option<&str>, discovered: Option<&str>) -> bool {
    let stored = stored.map(str::trim).filter(|value| !value.is_empty());
    let discovered = discovered.map(str::trim).filter(|value| !value.is_empty());
    match (stored, discovered) {
        (Some(stored), Some(discovered)) => stored.eq_ignore_ascii_case(discovered),
        _ => true,
    }
}

pub(super) fn upnp_target_control_urls_match_endpoint(target: &UpnpRendererTarget) -> bool {
    control_url_matches_endpoint(target, &target.av_transport_control_url)
        && target
            .rendering_control_url
            .as_deref()
            .map(|url| control_url_matches_endpoint(target, url))
            .unwrap_or(true)
        && target
            .connection_manager_url
            .as_deref()
            .map(|url| control_url_matches_endpoint(target, url))
            .unwrap_or(true)
}

pub(super) fn control_url_matches_endpoint(target: &UpnpRendererTarget, control_url: &str) -> bool {
    let Ok(url) = Url::parse(control_url) else {
        return false;
    };
    url.scheme() == "http"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(&target.host))
        && url.port_or_known_default() == Some(target.port)
}

pub fn upnp_target_origin_label(target: &UpnpRendererTarget) -> String {
    let endpoint = format!("{}:{}", target.host, target.port);
    match upnp_av_transport_origin(target) {
        Some((host, port)) if host != target.host.to_ascii_lowercase() || port != target.port => {
            format!("{endpoint} av={host}:{port}")
        }
        _ => endpoint,
    }
}

pub(super) fn upnp_av_transport_origin(target: &UpnpRendererTarget) -> Option<(String, u16)> {
    if let Ok(url) = Url::parse(&target.av_transport_control_url) {
        return Some((
            url.host_str()?.to_ascii_lowercase(),
            url.port_or_known_default()?,
        ));
    }
    Some((target.host.to_ascii_lowercase(), target.port))
}

pub(super) async fn discover_once(service: &UpnpRendererService) -> Result<(), String> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|e| format!("bind SSDP socket: {e}"))?;
    for search_target in [
        "urn:schemas-upnp-org:device:MediaRenderer:1",
        "upnp:rootdevice",
        "ssdp:all",
    ] {
        let message = ssdp_search_message(search_target);
        socket
            .send_to(
                message.as_bytes(),
                SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900),
            )
            .await
            .map_err(|e| format!("send SSDP search for {search_target}: {e}"))?;
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut seen_renderer_ids = HashSet::new();
    let mut seen_locations = HashSet::new();
    let mut buf = vec![0_u8; 4096];
    while tokio::time::Instant::now() < deadline {
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        let received = tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await;
        let (len, responder) = match received {
            Err(_) => break,
            Ok(Err(_)) => continue,
            Ok(Ok(received)) => received,
        };
        let body = String::from_utf8_lossy(&buf[..len]);
        let Some(location) = parse_ssdp_response(&body) else {
            continue;
        };
        if !seen_locations.insert(location.clone()) {
            continue;
        }
        if let Ok(target) = resolve_upnp_target(&service.http, &location, responder.ip()).await {
            let target_id = target.id.clone();
            if service.upsert_discovered_renderer(target) {
                seen_renderer_ids.insert(target_id);
            }
        }
    }
    let mut renderers = service.renderers.lock().unwrap();
    for renderer in renderers.values_mut() {
        renderer.online = seen_renderer_ids.contains(&renderer.target.id);
    }
    Ok(())
}

fn ssdp_search_message(search_target: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {search_target}\r\n\r\n"
    )
}

pub(super) async fn resolve_upnp_target(
    http: &Client,
    location: &str,
    responder_ip: IpAddr,
) -> Result<UpnpRendererTarget, String> {
    let url = validate_upnp_location(location, responder_ip)?;
    let body = fetch_description(http, url.clone()).await?;
    let host = url
        .host_str()
        .ok_or_else(|| "UPnP location URL is missing a host".to_string())?
        .to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    let udn = tag_text(&body, "UDN").unwrap_or_else(|| format!("uuid:{host}:{port}"));
    let id = udn.trim_start_matches("uuid:").to_string();
    let name = tag_text(&body, "friendlyName").unwrap_or_else(|| format!("UPnP {host}"));
    let model = tag_text(&body, "modelName");
    let manufacturer = tag_text(&body, "manufacturer");
    let av_transport_control_url = service_control_url(&url, &body, "AVTransport")
        .ok_or_else(|| "UPnP renderer does not expose AVTransport".to_string())?;
    let rendering_control_url = service_control_url(&url, &body, "RenderingControl");
    let connection_manager_url = service_control_url(&url, &body, "ConnectionManager");

    let protocol_info = if let Some(cm_url) = connection_manager_url.as_deref() {
        get_protocol_info(http, &host, port, cm_url)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let capabilities = infer_capabilities(&protocol_info);
    let is_sonos_renderer = manufacturer
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("sonos"));
    Ok(UpnpRendererTarget {
        id,
        name,
        host,
        port,
        model,
        manufacturer,
        av_transport_control_url,
        rendering_control_url,
        connection_manager_url,
        max_sample_rate: if is_sonos_renderer {
            crate::audio::sinks::sonos::SONOS_SAMPLE_RATE
        } else {
            capabilities.max_sample_rate
        },
        max_bit_depth: if is_sonos_renderer {
            crate::audio::sinks::sonos::SONOS_BIT_DEPTH
        } else {
            capabilities.max_bit_depth
        },
        max_dsd_rate: if is_sonos_renderer {
            None
        } else {
            capabilities.max_dsd_rate
        },
        capability_detection_source: if is_sonos_renderer {
            CapabilityDetectionSource::Advertised
        } else if capabilities.needs_probe {
            CapabilityDetectionSource::Fallback
        } else {
            capabilities.detection_source
        },
        capability_detection_status: if is_sonos_renderer {
            CapabilityDetectionStatus::Complete
        } else {
            capabilities.detection_status
        },
        capability_detection_message: if is_sonos_renderer {
            Some("Sonos UPnP transport limit".to_string())
        } else {
            capabilities.detection_message
        },
        protocol_info,
        pcm_containers: if is_sonos_renderer {
            Vec::new()
        } else {
            capabilities.pcm_containers
        },
    })
}

pub(super) async fn get_protocol_info(
    http: &Client,
    host: &str,
    port: u16,
    control_url: &str,
) -> Result<Vec<String>, String> {
    let target = UpnpRendererTarget {
        id: format!("{host}:{port}"),
        name: "UPnP".to_string(),
        host: host.to_string(),
        port,
        model: None,
        manufacturer: None,
        av_transport_control_url: control_url.to_string(),
        rendering_control_url: None,
        connection_manager_url: Some(control_url.to_string()),
        max_sample_rate: UPNP_FALLBACK_SAMPLE_RATE,
        max_bit_depth: UPNP_FALLBACK_BIT_DEPTH,
        max_dsd_rate: None,
        capability_detection_source: CapabilityDetectionSource::Fallback,
        capability_detection_status: CapabilityDetectionStatus::Unknown,
        capability_detection_message: None,
        protocol_info: Vec::new(),
        pcm_containers: Vec::new(),
    };
    let body = soap_action_url(
        http,
        &target,
        control_url,
        "urn:schemas-upnp-org:service:ConnectionManager:1",
        "GetProtocolInfo",
        "",
    )
    .await?;
    Ok(tag_text(&body, "Sink")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect())
}

pub(super) fn validate_upnp_location(location: &str, responder_ip: IpAddr) -> Result<Url, String> {
    let url = Url::parse(location).map_err(|e| format!("invalid UPnP location URL: {e}"))?;
    if url.scheme() != "http" {
        return Err("UPnP location URL must use http".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("UPnP location URL must not contain credentials".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "UPnP location URL is missing a host".to_string())?;
    let host_ip: IpAddr = host
        .parse()
        .map_err(|_| "UPnP location host must be an IP address".to_string())?;
    if host_ip != responder_ip {
        return Err("UPnP location host must match the SSDP responder".to_string());
    }
    Ok(url)
}

pub(super) async fn fetch_description(http: &Client, url: Url) -> Result<String, String> {
    let mut response = tokio::time::timeout(UPNP_DESCRIPTION_TIMEOUT, http.get(url).send())
        .await
        .map_err(|_| "fetch UPnP device description timed out".to_string())?
        .map_err(|e| format!("fetch UPnP device description: {e}"))?;
    if let Some(length) = response.content_length()
        && length > UPNP_DESCRIPTION_MAX_BYTES as u64
    {
        return Err("UPnP device description is too large".to_string());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("read UPnP device description: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > UPNP_DESCRIPTION_MAX_BYTES {
            return Err("UPnP device description is too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|e| format!("decode UPnP device description: {e}"))
}

pub(super) fn service_control_url(
    root_url: &Url,
    body: &str,
    service_name: &str,
) -> Option<String> {
    let mut remaining = body;
    while let Some(start) = remaining.find("<service") {
        remaining = &remaining[start..];
        let Some(end_rel) = remaining.find("</service>") else {
            break;
        };
        let block = &remaining[..end_rel + "</service>".len()];
        let service_type = tag_text(block, "serviceType").unwrap_or_default();
        if service_type.contains(service_name) {
            let control = tag_text(block, "controlURL")?;
            return checked_service_control_url(root_url, control.trim());
        }
        remaining = &remaining[end_rel + "</service>".len()..];
    }
    None
}

pub(super) fn checked_service_control_url(root_url: &Url, control_url: &str) -> Option<String> {
    let url = root_url.join(control_url).ok()?;
    if url.scheme() != "http" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.host_str()? != root_url.host_str()? {
        return None;
    }
    if url.port_or_known_default() != root_url.port_or_known_default() {
        return None;
    }
    Some(url.to_string())
}

pub(super) fn parse_ssdp_response(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim().to_string())
    })
}
