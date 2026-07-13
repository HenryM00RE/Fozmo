use super::*;
use super::{probe::*, trace::*};

impl UpnpRendererService {
    pub(super) async fn soap_action(
        &self,
        target: &UpnpRendererTarget,
        control_url: &str,
        service: &str,
        action: &str,
        inner: &str,
    ) -> Result<String, String> {
        soap_action_url(&self.http, target, control_url, service, action, inner).await
    }

    // SOAP tracing keeps request identity, timeout, and retry attempt explicit at the network edge.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn traced_soap_action(
        &self,
        zone_id: &str,
        target: &UpnpRendererTarget,
        control_url: &str,
        service: &str,
        action: &str,
        inner: &str,
        timeout: Duration,
        attempt: u8,
    ) -> Result<String, String> {
        let started = Instant::now();
        let result = soap_action_url_with_timeout(
            &self.http,
            target,
            control_url,
            service,
            action,
            inner,
            timeout,
        )
        .await;
        let elapsed = elapsed_ms(started);
        self.record_soap_trace(
            zone_id,
            UpnpSoapTrace {
                action: action.to_string(),
                attempt,
                timeout_ms: timeout.as_millis() as u64,
                elapsed_ms: elapsed,
                ok: result.is_ok(),
                error: result.as_ref().err().cloned(),
            },
        );
        result
    }
}

pub(super) async fn soap_action_url(
    http: &Client,
    target: &UpnpRendererTarget,
    control_url: &str,
    service: &str,
    action: &str,
    inner: &str,
) -> Result<String, String> {
    soap_action_url_with_timeout(
        http,
        target,
        control_url,
        service,
        action,
        inner,
        UPNP_SOAP_ACTION_TIMEOUT,
    )
    .await
}

pub(super) async fn soap_action_url_with_timeout(
    http: &Client,
    target: &UpnpRendererTarget,
    control_url: &str,
    service: &str,
    action: &str,
    inner: &str,
    timeout: Duration,
) -> Result<String, String> {
    let envelope = soap_envelope(service, action, inner);
    let response = tokio::time::timeout(
        timeout,
        http.post(control_url)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPACTION", format!("\"{service}#{action}\""))
            .body(envelope)
            .send(),
    )
    .await
    .map_err(|_| {
        format!(
            "UPnP SOAP {action} request to {} timed out after {}ms",
            target.name,
            timeout.as_millis()
        )
    })?
    .map_err(|e| format!("UPnP SOAP {action} request to {} failed: {e}", target.name))?;
    let status = response.status();
    let body = tokio::time::timeout(
        timeout,
        read_bounded_response_body(response, UPNP_SOAP_MAX_BYTES, "UPnP SOAP response"),
    )
    .await
    .map_err(|_| {
        format!(
            "UPnP SOAP {action} response from {} timed out after {}ms",
            target.name,
            timeout.as_millis()
        )
    })??;
    if !status.is_success() {
        return Err(parse_soap_error(&body)
            .unwrap_or_else(|| format!("UPnP SOAP {action} failed with {status}")));
    }
    Ok(body)
}

pub(super) async fn read_bounded_response_body(
    mut response: reqwest::Response,
    max_bytes: usize,
    label: &str,
) -> Result<String, String> {
    if let Some(length) = response.content_length()
        && length > max_bytes as u64
    {
        return Err(format!("{label} is too large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("read {label}: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(format!("{label} is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|e| format!("decode {label}: {e}"))
}

pub(super) fn soap_envelope(service: &str, action: &str, inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{service}">{inner}</u:{action}></s:Body></s:Envelope>"#
    )
}

pub(super) fn didl_metadata(asset: &UpnpAsset, target: &UpnpRendererTarget) -> String {
    let title = asset.title.as_deref().unwrap_or("Track");
    let creator = asset.artist.as_deref().unwrap_or("");
    let album = asset.album.as_deref().unwrap_or("");
    let protocol_info = protocol_info_for_asset(asset, target);
    if protocol_info == format!("http-get:*:{}:*", asset.mime_type)
        && !target.protocol_info.is_empty()
        && !target.protocol_info.iter().any(|value| {
            protocol_info_mime(value)
                .is_some_and(|mime| protocol_info_mime_matches_asset(mime, &asset.mime_type))
        })
    {
        eprintln!(
            "upnp: renderer {} did not advertise exact protocolInfo MIME {}; using fallback",
            target.name, asset.mime_type
        );
    }
    let duration_attr = asset
        .duration_secs
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .map(|duration| format!(r#" duration="{}""#, format_hhmmss(duration)))
        .unwrap_or_default();
    let size_attr = asset
        .byte_len
        .map(|len| format!(r#" size="{len}""#))
        .unwrap_or_default();
    let audio_format_attrs = if asset_is_pcm(asset) {
        let rendered_pcm = asset.render_ms.is_some();
        let sample_rate = if rendered_pcm && asset.target_rate > 0 {
            asset.target_rate
        } else {
            asset.source_rate
        };
        let bit_depth = if rendered_pcm && asset.target_bits > 0 {
            asset.target_bits
        } else {
            asset.source_bits
        };
        if sample_rate > 0 && bit_depth > 1 {
            format!(
                r#" sampleFrequency="{}" bitsPerSample="{}" nrAudioChannels="2""#,
                sample_rate, bit_depth
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let art = asset
        .art_url
        .as_deref()
        .map(|url| format!("<upnp:albumArtURI>{}</upnp:albumArtURI>", xml_escape(url)))
        .unwrap_or_default();
    format!(
        r#"<DIDL-Lite xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"><item id="{}" parentID="0" restricted="true"><dc:title>{}</dc:title><dc:creator>{}</dc:creator><upnp:album>{}</upnp:album>{}<upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo="{}"{}{}{}>{}</res></item></DIDL-Lite>"#,
        xml_escape(&asset.id),
        xml_escape(title),
        xml_escape(creator),
        xml_escape(album),
        art,
        xml_escape(&protocol_info),
        duration_attr,
        size_attr,
        audio_format_attrs,
        xml_escape(&asset.stream_url),
    )
}

pub(super) fn protocol_info_for_asset(asset: &UpnpAsset, target: &UpnpRendererTarget) -> String {
    target
        .protocol_info
        .iter()
        .find(|value| {
            protocol_info_mime(value)
                .is_some_and(|mime| protocol_info_mime_matches_asset(mime, &asset.mime_type))
        })
        .cloned()
        .unwrap_or_else(|| format!("http-get:*:{}:*", asset.mime_type))
}

pub(super) fn protocol_info_mime(value: &str) -> Option<&str> {
    value
        .split(':')
        .nth(2)
        .map(str::trim)
        .filter(|mime| !mime.is_empty())
}

pub(super) fn protocol_info_mime_matches_asset(protocol_mime: &str, asset_mime: &str) -> bool {
    if protocol_mime.eq_ignore_ascii_case(asset_mime) {
        return true;
    }
    let protocol_mime = protocol_mime.to_ascii_lowercase();
    let asset_mime = asset_mime.to_ascii_lowercase();
    asset_mime == "audio/x-dsf" && (protocol_mime == "audio/dsf" || protocol_mime == "audio/dsd")
}

pub(super) fn asset_is_pcm(asset: &UpnpAsset) -> bool {
    upnp_pcm_container_from_mime(&asset.mime_type).is_some()
}

pub(super) fn upnp_diagnostic_warnings(
    public_base_url: &str,
    target: &UpnpRendererTarget,
) -> Vec<String> {
    let mut warnings = Vec::new();
    match Url::parse(public_base_url) {
        Ok(url) => {
            if url
                .host_str()
                .and_then(|host| host.parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
            {
                warnings.push(
                    "public_base_url points at loopback; a KEF renderer cannot fetch 127.0.0.1 from the speaker"
                        .to_string(),
                );
            }
            if url.scheme() != "http" {
                warnings.push("UPnP renderers usually require an http public_base_url".to_string());
            }
        }
        Err(e) => warnings.push(format!("public_base_url is not a valid URL: {e}")),
    }
    if target.protocol_info.is_empty() {
        warnings.push("renderer did not return ConnectionManager Sink protocol_info".to_string());
    } else if !target
        .protocol_info
        .iter()
        .any(|value| protocol_info_mime(value).is_some_and(|mime| mime == "audio/flac"))
    {
        warnings.push("renderer protocol_info does not advertise audio/flac".to_string());
    }
    warnings
}

pub(super) fn tag_text(body: &str, tag: &str) -> Option<String> {
    let mut reader = Reader::from_str(body);
    let mut inside = false;
    let mut content = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) if local_xml_name(start.name().as_ref()) == tag.as_bytes() => {
                inside = true;
                content.clear();
            }
            Ok(Event::Text(text)) if inside => content.push_str(&text.xml10_content().ok()?),
            Ok(Event::CData(text)) if inside => content.push_str(&text.xml10_content().ok()?),
            Ok(Event::End(end)) if local_xml_name(end.name().as_ref()) == tag.as_bytes() => {
                return Some(xml_unescape(content.trim()));
            }
            Ok(Event::Eof) => return None,
            Err(_) => return tag_text_fallback(body, tag),
            _ => {}
        }
    }
}

pub(super) fn tag_text_fallback(body: &str, tag: &str) -> Option<String> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let start_idx = body.find(&start)? + start.len();
    let end_idx = body[start_idx..].find(&end)? + start_idx;
    Some(xml_unescape(&body[start_idx..end_idx]))
}

pub(super) fn local_xml_name(name: &[u8]) -> &[u8] {
    name.iter()
        .rposition(|byte| *byte == b':')
        .map(|idx| &name[idx + 1..])
        .unwrap_or(name)
}

pub(super) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub(super) fn parse_soap_error(body: &str) -> Option<String> {
    let code = tag_text(body, "errorCode");
    let description = tag_text(body, "errorDescription");
    match (code, description) {
        (Some(code), Some(description)) => Some(format!("UPnP SOAP error {code}: {description}")),
        (Some(code), None) => Some(format!("UPnP SOAP error {code}")),
        _ => None,
    }
}

pub(super) fn parse_upnp_time(value: &str) -> Option<f64> {
    if value == "NOT_IMPLEMENTED" || value.trim().is_empty() {
        return None;
    }
    let parts: Vec<_> = value.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: f64 = parts[0].parse().ok()?;
    let minutes: f64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

pub(super) fn format_hhmmss(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}
