use super::*;
use super::{discovery::*, probe::*, session::*, soap::*};

#[test]
fn upnp_location_must_match_responder_ip() {
    let responder = "192.168.1.23".parse().unwrap();

    assert!(validate_upnp_location("http://192.168.1.23:8080/desc.xml", responder).is_ok());
    assert!(validate_upnp_location("http://192.168.1.24:8080/desc.xml", responder).is_err());
    assert!(validate_upnp_location("http://renderer.local/desc.xml", responder).is_err());
    assert!(validate_upnp_location("http://user:pass@192.168.1.23/desc.xml", responder).is_err());
}

#[test]
fn protocol_info_infers_pcm_192_and_dsd64_from_advertised_tokens() {
    let caps = infer_capabilities(&[
        "http-get:*:audio/flac:DLNA.ORG_PN=FLAC_192_24;bitsPerSample=24;sampleFrequency=192000"
            .to_string(),
        "http-get:*:audio/x-dsf:DLNA.ORG_PN=DSD64;rate=2822400".to_string(),
    ]);

    assert_eq!(caps.max_sample_rate, 192_000);
    assert_eq!(caps.max_bit_depth, 24);
    assert_eq!(caps.max_dsd_rate, Some(64));
    assert_eq!(caps.detection_source, CapabilityDetectionSource::Advertised);
    assert_eq!(
        caps.pcm_containers,
        vec![UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Flac,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }]
    );
    assert!(!caps.needs_probe);
}

#[test]
fn protocol_info_infers_pcm_96_without_dsd() {
    let caps =
        infer_capabilities(&["http-get:*:audio/wav:DLNA.ORG_PN=WAV_96_24;bitDepth=24".to_string()]);

    assert_eq!(caps.max_sample_rate, 96_000);
    assert_eq!(caps.max_bit_depth, 24);
    assert_eq!(caps.max_dsd_rate, None);
    assert_eq!(caps.detection_source, CapabilityDetectionSource::Advertised);
    assert_eq!(
        caps.pcm_containers,
        vec![UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 96_000,
            max_bit_depth: 24,
        }]
    );
    assert!(!caps.needs_probe);
}

#[test]
fn sparse_upnp_protocol_info_falls_back_until_probed() {
    let caps = infer_capabilities(&["http-get:*:audio/flac:DLNA.ORG_OP=01".to_string()]);

    assert_eq!(caps.max_sample_rate, UPNP_FALLBACK_SAMPLE_RATE);
    assert_eq!(caps.max_bit_depth, UPNP_FALLBACK_BIT_DEPTH);
    assert_eq!(caps.max_dsd_rate, None);
    assert_eq!(caps.detection_source, CapabilityDetectionSource::Fallback);
    assert!(caps.needs_probe);
}

#[test]
fn flac_advertised_pcm_probe_tries_flac_before_wav() {
    let mut target = test_target();
    target.protocol_info = vec!["http-get:*:audio/flac:DLNA.ORG_OP=01".to_string()];

    let formats = pcm_probe_formats_for_candidate(
        &target,
        PcmProbeCandidate {
            sample_rate: 192_000,
            bit_depth: 24,
        },
    );

    assert_eq!(formats, vec![PcmProbeFormat::Flac, PcmProbeFormat::Wav]);
}

#[test]
fn sparse_upnp_discovery_does_not_start_automatic_probe() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let mut target = test_target();
    let caps = infer_capabilities(&target.protocol_info);
    target.max_sample_rate = caps.max_sample_rate;
    target.max_bit_depth = caps.max_bit_depth;
    target.max_dsd_rate = caps.max_dsd_rate;
    target.capability_detection_source = caps.detection_source;
    target.capability_detection_status = caps.detection_status;
    target.capability_detection_message = caps.detection_message;

    service.upsert_discovered_renderer(target.clone());

    assert!(service.capability_probe_tasks.lock().unwrap().is_empty());
    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(
        renderer.target.capability_detection_source,
        CapabilityDetectionSource::Fallback
    );
    assert_eq!(
        renderer.target.capability_detection_status,
        CapabilityDetectionStatus::Unknown
    );
}

#[test]
fn symlinked_existing_probe_file_is_rejected() {
    let dir = temp_probe_test_dir("upnp-probe-symlink");
    let target = dir.join("target.dsf");
    let link = dir.join("silence-dsd64.dsf");
    std::fs::write(&target, b"not a probe").unwrap();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let result = write_probe_dsf_file_at(&link, 64);
        assert!(result.is_err());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn non_regular_existing_probe_path_is_rejected() {
    let dir = temp_probe_test_dir("upnp-probe-directory");
    let path = dir.join("silence-dsd64.dsf");
    std::fs::create_dir(&path).unwrap();

    let result = write_probe_dsf_file_at(&path, 64);

    assert!(result.is_err());
    assert!(path.is_dir());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn existing_regular_probe_file_is_reused_only_after_validation() {
    let dir = temp_probe_test_dir("upnp-probe-existing");
    let path = dir.join("silence-dsd64.dsf");
    let expected = dsf_probe_bytes(64).unwrap();
    std::fs::write(&path, &expected).unwrap();

    write_probe_dsf_file_at(&path, 64).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), expected);

    std::fs::write(&path, b"wrong").unwrap();
    let result = write_probe_dsf_file_at(&path, 64);

    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"wrong");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn default_probe_cache_uses_private_unpredictable_directory() {
    let path = write_probe_dsf_file(64).unwrap();
    let parent = path.parent().unwrap();

    assert_ne!(parent.file_name().unwrap(), "fozmo-upnp-probes");
    assert!(probe_path_is_streamable(&path));
}

#[test]
fn same_id_different_origin_discovery_does_not_replace_renderer() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    service.upsert_discovered_renderer(target.clone());
    let mut spoof = target.clone();
    spoof.host = "192.168.1.66".to_string();
    spoof.av_transport_control_url = "http://192.168.1.66/AVTransport".to_string();
    spoof.model = Some("Different Model".to_string());
    spoof.max_sample_rate = 384_000;
    spoof.max_bit_depth = 32;

    service.upsert_discovered_renderer(spoof);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.host, "192.168.1.23");
    assert_eq!(
        renderer.target.av_transport_control_url,
        "http://192.168.1.23/AVTransport"
    );
    assert_eq!(renderer.target.max_sample_rate, 192_000);
    assert!(renderer.online);
}

#[test]
fn same_renderer_verified_on_new_ip_replaces_discovery_endpoint() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    assert!(service.upsert_discovered_renderer(target.clone()));
    let mut moved = target.clone();
    moved.host = "192.168.1.91".to_string();
    moved.port = 8080;
    moved.av_transport_control_url = "http://192.168.1.91:8080/AVTransport/ctrl".to_string();
    moved.rendering_control_url =
        Some("http://192.168.1.91:8080/RenderingControl/ctrl".to_string());
    moved.connection_manager_url =
        Some("http://192.168.1.91:8080/ConnectionManager/ctrl".to_string());

    assert!(service.upsert_discovered_renderer(moved));

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.host, "192.168.1.91");
    assert_eq!(renderer.target.port, 8080);
    assert_eq!(
        renderer.target.av_transport_control_url,
        "http://192.168.1.91:8080/AVTransport/ctrl"
    );
    assert!(renderer.online);
}

#[test]
fn same_id_same_host_new_port_discovery_refreshes_renderer() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    service.upsert_discovered_renderer(target.clone());
    let mut moved = target.clone();
    moved.port = 38400;
    moved.av_transport_control_url = "http://192.168.1.23:38400/AVTransport".to_string();
    moved.rendering_control_url = Some("http://192.168.1.23:38400/RenderingControl".to_string());
    moved.connection_manager_url = Some("http://192.168.1.23:38400/ConnectionManager".to_string());
    moved.max_sample_rate = 384_000;
    moved.max_bit_depth = 32;

    service.upsert_discovered_renderer(moved);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.port, 38400);
    assert_eq!(
        renderer.target.av_transport_control_url,
        "http://192.168.1.23:38400/AVTransport"
    );
    assert_eq!(renderer.target.max_sample_rate, 384_000);
    assert_eq!(renderer.target.max_bit_depth, 32);
    assert!(renderer.online);
}

#[test]
fn same_id_same_host_new_port_discovery_rejects_metadata_conflict() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    service.upsert_discovered_renderer(target.clone());
    let mut moved = target.clone();
    moved.name = "Different Renderer".to_string();
    moved.port = 38400;
    moved.av_transport_control_url = "http://192.168.1.23:38400/AVTransport".to_string();
    moved.max_sample_rate = 384_000;
    moved.max_bit_depth = 32;

    service.upsert_discovered_renderer(moved);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.port, 80);
    assert_eq!(
        renderer.target.av_transport_control_url,
        "http://192.168.1.23/AVTransport"
    );
    assert_eq!(renderer.target.max_sample_rate, 192_000);
    assert_eq!(renderer.target.max_bit_depth, 24);
    assert!(renderer.online);
}

#[test]
fn same_id_same_origin_discovery_can_refresh_renderer() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    service.upsert_discovered_renderer(target.clone());
    let mut refreshed = target.clone();
    refreshed.av_transport_control_url = "http://192.168.1.23/fresh/AVTransport".to_string();
    refreshed.max_sample_rate = 384_000;
    refreshed.max_bit_depth = 32;

    service.upsert_discovered_renderer(refreshed);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(
        renderer.target.av_transport_control_url,
        "http://192.168.1.23/fresh/AVTransport"
    );
    assert_eq!(renderer.target.max_sample_rate, 384_000);
    assert_eq!(renderer.target.max_bit_depth, 32);
}

#[test]
fn generic_lossless_protocol_info_stays_probeable_after_l16_48k_entries() {
    let caps = infer_capabilities(&[
        "http-get:*:audio/L16;rate=44100;channels=2:DLNA.ORG_PN=LPCM".to_string(),
        "http-get:*:audio/L16;rate=48000;channels=2:DLNA.ORG_PN=LPCM".to_string(),
        "http-get:*:audio/flac:*".to_string(),
        "http-get:*:audio/x-flac:*".to_string(),
        "http-get:*:audio/wav:*".to_string(),
    ]);

    assert_eq!(caps.max_sample_rate, 48_000);
    assert_eq!(caps.max_bit_depth, 16);
    assert_eq!(caps.detection_source, CapabilityDetectionSource::Fallback);
    assert!(caps.needs_probe);
}

#[test]
fn sparse_dsd_protocol_info_falls_back_until_probed() {
    let caps = infer_capabilities(&["http-get:*:audio/x-dsf:*".to_string()]);

    assert_eq!(caps.max_dsd_rate, None);
    assert_eq!(caps.detection_source, CapabilityDetectionSource::Fallback);
    assert!(caps.needs_probe);
}

#[test]
fn dsf_probe_bytes_have_dsf_header_and_idle_payload() {
    let bytes = dsf_probe_bytes(64).expect("DSF64 probe bytes");

    assert_eq!(&bytes[0..4], b"DSD ");
    assert_eq!(&bytes[28..32], b"fmt ");
    assert_eq!(&bytes[80..84], b"data");
    assert_eq!(
        u32::from_le_bytes(bytes[56..60].try_into().unwrap()),
        2_822_400
    );
    assert!(bytes[92..].iter().all(|byte| *byte == 0x69));
}

#[test]
fn generic_dsd_protocol_info_matches_dsf_assets() {
    assert!(protocol_info_mime_matches_asset("audio/dsd", "audio/x-dsf"));
    assert!(protocol_info_mime_matches_asset("audio/dsf", "audio/x-dsf"));
    assert!(protocol_info_mime_matches_asset(
        "audio/x-dsf",
        "audio/x-dsf"
    ));
    assert!(!protocol_info_mime_matches_asset(
        "audio/x-dff",
        "audio/x-dsf"
    ));
}

#[test]
fn high_rate_lossless_probe_result_allows_dsd_probe() {
    let mut target = test_target();
    target.protocol_info = vec!["http-get:*:audio/flac:*".to_string()];
    let result = UpnpCapabilityProbeResult {
        max_sample_rate: 192_000,
        max_bit_depth: 24,
        max_dsd_rate: None,
        detection_source: CapabilityDetectionSource::Probed,
        detection_status: CapabilityDetectionStatus::Complete,
        detection_message: None,
        basis: Some("test".to_string()),
        pcm_containers: vec![UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Flac,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }],
    };

    assert!(should_probe_dsd(&target, &result));
}

#[test]
fn capability_probe_cache_key_changes_with_protocol_info() {
    let mut target = test_target();
    let first = capability_probe_cache_key(&target);
    target
        .protocol_info
        .push("http-get:*:audio/flac:DLNA.ORG_PN=FLAC_192_24;sampleFrequency=192000".to_string());
    let second = capability_probe_cache_key(&target);

    assert_ne!(first, second);
}

#[test]
fn sparse_pcm_probe_ladder_starts_at_192_24_with_16_bit_fallbacks() {
    let target = test_target();
    let candidates = pcm_probe_candidates(&target);

    assert_eq!(
        candidates.first(),
        Some(&PcmProbeCandidate {
            sample_rate: 192_000,
            bit_depth: 24
        })
    );
    assert!(candidates.contains(&PcmProbeCandidate {
        sample_rate: 44_100,
        bit_depth: 16
    }));
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.sample_rate > 192_000)
    );
}

#[test]
fn probe_result_updates_target_capability_source() {
    let mut target = test_target();
    apply_probe_result_to_target(
        &mut target,
        UpnpCapabilityProbeResult {
            max_sample_rate: 192_000,
            max_bit_depth: 24,
            max_dsd_rate: None,
            detection_source: CapabilityDetectionSource::Probed,
            detection_status: CapabilityDetectionStatus::Complete,
            detection_message: None,
            basis: Some("test".to_string()),
            pcm_containers: vec![UpnpPcmContainerCapability {
                container: UpnpPcmContainer::Flac,
                max_sample_rate: 192_000,
                max_bit_depth: 24,
            }],
        },
    );

    assert_eq!(target.max_sample_rate, 192_000);
    assert_eq!(target.max_bit_depth, 24);
    assert_eq!(
        target.capability_detection_source,
        CapabilityDetectionSource::Probed
    );
}

#[test]
fn observed_high_rate_playback_promotes_pcm_but_leaves_dsd_unknown() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let mut target = test_target();
    target.max_sample_rate = UPNP_FALLBACK_SAMPLE_RATE;
    target.max_bit_depth = UPNP_FALLBACK_BIT_DEPTH;
    target.max_dsd_rate = None;
    target.capability_detection_source = CapabilityDetectionSource::Fallback;
    target.capability_detection_status = CapabilityDetectionStatus::Unknown;
    service.renderers.lock().unwrap().insert(
        target.id.clone(),
        UpnpRenderer {
            target: target.clone(),
            online: true,
        },
    );
    let asset = UpnpAsset {
        source_rate: 192_000,
        target_rate: 192_000,
        source_bits: 24,
        target_bits: 24,
        ..test_asset("qobuz-1-27")
    };

    service.promote_capabilities_from_observed_playback(&target, &asset);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.max_sample_rate, 192_000);
    assert_eq!(renderer.target.max_bit_depth, 24);
    assert_eq!(renderer.target.max_dsd_rate, None);
    assert_eq!(
        renderer.target.capability_detection_source,
        CapabilityDetectionSource::Probed
    );
    assert_eq!(
        renderer.target.capability_detection_status,
        CapabilityDetectionStatus::Unknown
    );
}

#[test]
fn observed_rendered_playback_promotes_transport_output_not_source() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let mut target = test_target();
    target.max_sample_rate = UPNP_FALLBACK_SAMPLE_RATE;
    target.max_bit_depth = UPNP_FALLBACK_BIT_DEPTH;
    target.capability_detection_source = CapabilityDetectionSource::Fallback;
    target.capability_detection_status = CapabilityDetectionStatus::Unknown;
    service.renderers.lock().unwrap().insert(
        target.id.clone(),
        UpnpRenderer {
            target: target.clone(),
            online: true,
        },
    );
    let asset = UpnpAsset {
        source_rate: 44_100,
        target_rate: 192_000,
        source_bits: 16,
        target_bits: 24,
        render_ms: Some(250),
        mime_type: "audio/wav".to_string(),
        ..test_asset("rendered-wav")
    };

    service.promote_capabilities_from_observed_playback(&target, &asset);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.max_sample_rate, 192_000);
    assert_eq!(renderer.target.max_bit_depth, 24);
    assert!(renderer.target.pcm_containers.iter().any(|capability| {
        capability.container == UpnpPcmContainer::Wav
            && capability.max_sample_rate == 192_000
            && capability.max_bit_depth == 24
    }));
}

#[test]
fn observed_rendered_dsd_playback_promotes_target_dsd_rate() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let mut target = test_target();
    target.max_dsd_rate = None;
    target.capability_detection_source = CapabilityDetectionSource::Fallback;
    target.capability_detection_status = CapabilityDetectionStatus::Unknown;
    service.renderers.lock().unwrap().insert(
        target.id.clone(),
        UpnpRenderer {
            target: target.clone(),
            online: true,
        },
    );
    let asset = UpnpAsset {
        source_rate: 44_100,
        target_rate: 5_644_800,
        source_bits: 24,
        target_bits: 1,
        render_ms: Some(500),
        mime_type: "audio/x-dsf".to_string(),
        ..test_asset("rendered-dsd")
    };

    service.promote_capabilities_from_observed_playback(&target, &asset);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.max_dsd_rate, Some(128));
    assert_eq!(
        renderer.target.capability_detection_status,
        CapabilityDetectionStatus::Complete
    );
}

#[test]
fn observed_unknown_source_metadata_does_not_promote() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let mut target = test_target();
    target.max_sample_rate = UPNP_FALLBACK_SAMPLE_RATE;
    target.max_bit_depth = UPNP_FALLBACK_BIT_DEPTH;
    target.capability_detection_source = CapabilityDetectionSource::Fallback;
    target.capability_detection_status = CapabilityDetectionStatus::Unknown;
    service.renderers.lock().unwrap().insert(
        target.id.clone(),
        UpnpRenderer {
            target: target.clone(),
            online: true,
        },
    );
    let asset = UpnpAsset {
        source_rate: 0,
        target_rate: 48_000,
        source_bits: 0,
        target_bits: 16,
        ..test_asset("local-unknown")
    };

    service.promote_capabilities_from_observed_playback(&target, &asset);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(renderer.target.max_sample_rate, UPNP_FALLBACK_SAMPLE_RATE);
    assert_eq!(renderer.target.max_bit_depth, UPNP_FALLBACK_BIT_DEPTH);
    assert_eq!(
        renderer.target.capability_detection_source,
        CapabilityDetectionSource::Fallback
    );
}

#[test]
fn observed_playback_does_not_promote_sonos_upnp() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let mut target = test_target();
    target.manufacturer = Some("Sonos, Inc.".to_string());
    target.name = "Sonos Five".to_string();
    target.max_sample_rate = crate::audio::sinks::sonos::SONOS_SAMPLE_RATE;
    target.max_bit_depth = crate::audio::sinks::sonos::SONOS_BIT_DEPTH;
    target.capability_detection_source = CapabilityDetectionSource::Advertised;
    target.capability_detection_status = CapabilityDetectionStatus::Complete;
    service.renderers.lock().unwrap().insert(
        target.id.clone(),
        UpnpRenderer {
            target: target.clone(),
            online: true,
        },
    );
    let asset = UpnpAsset {
        source_rate: 192_000,
        target_rate: 192_000,
        source_bits: 24,
        target_bits: 24,
        ..test_asset("qobuz-1-27")
    };

    service.promote_capabilities_from_observed_playback(&target, &asset);

    let renderer = service
        .renderers
        .lock()
        .unwrap()
        .get(&target.id)
        .cloned()
        .expect("renderer");
    assert_eq!(
        renderer.target.max_sample_rate,
        crate::audio::sinks::sonos::SONOS_SAMPLE_RATE
    );
    assert_eq!(
        renderer.target.max_bit_depth,
        crate::audio::sinks::sonos::SONOS_BIT_DEPTH
    );
}

#[test]
fn upnp_service_urls_must_stay_on_description_origin() {
    let root = Url::parse("http://192.168.1.23:8080/device/desc.xml").unwrap();
    let body = |control: &str| {
        format!(
            "<root><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>{control}</controlURL></service></root>"
        )
    };

    assert_eq!(
        service_control_url(
            &root,
            &body("/MediaRenderer/AVTransport/Control"),
            "AVTransport"
        )
        .as_deref(),
        Some("http://192.168.1.23:8080/MediaRenderer/AVTransport/Control")
    );
    assert_eq!(
        service_control_url(
            &root,
            &body("http://192.168.1.23:8080/AVTransport/Control"),
            "AVTransport"
        )
        .as_deref(),
        Some("http://192.168.1.23:8080/AVTransport/Control")
    );
    assert!(
        service_control_url(&root, &body("http://127.0.0.1:3001/admin"), "AVTransport").is_none()
    );
    assert!(
        service_control_url(
            &root,
            &body("http://192.168.1.23:9090/control"),
            "AVTransport"
        )
        .is_none()
    );
    assert!(
        service_control_url(
            &root,
            &body("https://192.168.1.23:8080/control"),
            "AVTransport"
        )
        .is_none()
    );
}

#[test]
fn transport_refresh_keeps_smooth_position_for_coarse_upnp_time() {
    let now = Instant::now();
    let started_at = now.checked_sub(Duration::from_millis(760)).unwrap();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(test_asset("smooth")),
        armed_next: None,
        state: "Playing".to_string(),
        started_at: Some(started_at),
        paused_position: 12.0,
        playback_polled_at: Some(started_at),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(12.0),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now,
    );

    assert!((session.paused_position - 12.76).abs() < 0.01);
    assert!((session_position_at(&session, now) - 12.76).abs() < 0.01);
}

#[test]
fn startup_refresh_ignores_implausibly_fast_initial_position() {
    let now = Instant::now();
    let startup_started = now.checked_sub(Duration::from_millis(900)).unwrap();
    let mut session = UpnpSession {
        play_id: 11,
        current: Some(test_asset("startup-fast")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 0.0,
        playback_polled_at: Some(startup_started),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: Some(UpnpStartup {
            play_id: 11,
            asset_id: "startup-fast".to_string(),
            started_at: startup_started,
            accepted_at: Some(startup_started),
            accepted_reason: Some("renderer_http_request".to_string()),
            confirmed_playing_at: None,
            failed: false,
            timed_out: false,
        }),
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(12.0),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now,
    );

    assert_eq!(session.state, "Playing");
    assert_eq!(session.paused_position, 0.0);
    assert!(session_position_at(&session, now) < 0.01);
}

#[test]
fn startup_refresh_anchors_ahead_position_to_first_media_acceptance() {
    let now = Instant::now();
    let startup_started = now.checked_sub(Duration::from_secs(10)).unwrap();
    let accepted_at = now.checked_sub(Duration::from_millis(100)).unwrap();
    let mut session = UpnpSession {
        play_id: 12,
        current: Some(test_asset("startup-delayed-media")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 0.0,
        playback_polled_at: Some(startup_started),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: Some(UpnpStartup {
            play_id: 12,
            asset_id: "startup-delayed-media".to_string(),
            started_at: startup_started,
            accepted_at: Some(accepted_at),
            accepted_reason: Some("local_media_dop_frame".to_string()),
            confirmed_playing_at: None,
            failed: false,
            timed_out: false,
        }),
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(10.0),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now,
    );

    assert_eq!(session.state, "Playing");
    assert_eq!(session.paused_position, 0.0);
    assert!(session_position_at(&session, now) < 0.01);
}

#[test]
fn transitioning_does_not_pull_resume_position_back_to_zero() {
    let now = Instant::now();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(test_asset("transition")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 3.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("TRANSITIONING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.0),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now,
    );

    assert_eq!(session.state, "Transitioning");
    assert_eq!(session.paused_position, 3.0);
    assert!(session.started_at.is_none());
}

#[test]
fn seeking_pending_clears_when_transport_reaches_target() {
    let now = Instant::now();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(test_asset("seek")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 120.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: Some("seeking".to_string()),
        transport_pending_position_secs: Some(120.0),
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(120.4),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now,
    );

    assert_eq!(session.state, "Playing");
    assert_eq!(session.transport_pending, None);
    assert_eq!(session.transport_pending_position_secs, None);
}

#[test]
fn seeking_pending_stays_until_transport_reaches_target() {
    let now = Instant::now();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(test_asset("seek-old-position")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 120.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: Some("seeking".to_string()),
        transport_pending_position_secs: Some(120.0),
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(30.0),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now,
    );

    assert_eq!(session.transport_pending.as_deref(), Some("seeking"));
    assert_eq!(session.transport_pending_position_secs, Some(120.0));
}

#[test]
fn seek_range_first_byte_clears_active_seek_pending() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let asset = test_asset("seek-range-ready");
    let zone_id = "zone-seek";
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    {
        let mut sessions = service.sessions.lock().unwrap();
        let session = sessions.get_mut(zone_id).expect("session");
        session.state = "Transitioning".to_string();
        session.transport_pending = Some("seeking".to_string());
        session.transport_pending_position_secs = Some(120.0);
    }

    service.mark_seek_attempt_started(zone_id, play_id);
    service.mark_renderer_http_request(&asset.id, "abc", "local_get", Some("bytes=4410000-"));
    service.mark_local_media_first_byte(&asset.id, "abc", Some("bytes=4410000-"), 206, Some(1));

    let sessions = service.sessions.lock().unwrap();
    let session = sessions.get(zone_id).expect("session");
    assert_eq!(session.state, "Playing");
    assert!(session.started_at.is_some());
    assert_eq!(session.transport_pending, None);
    assert_eq!(session.transport_pending_position_secs, None);
}

#[test]
fn seek_full_reopen_first_byte_clears_active_seek_pending() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let asset = test_asset("seek-full-reopen-ready");
    let zone_id = "zone-seek-full";
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    {
        let mut sessions = service.sessions.lock().unwrap();
        let session = sessions.get_mut(zone_id).expect("session");
        session.state = "Transitioning".to_string();
        session.transport_pending = Some("seeking".to_string());
        session.transport_pending_position_secs = Some(120.0);
    }

    service.mark_seek_attempt_started(zone_id, play_id);
    service.mark_renderer_http_request(&asset.id, "abc", "local_get", None);
    service.mark_local_media_first_byte(&asset.id, "abc", None, 200, Some(1));

    let sessions = service.sessions.lock().unwrap();
    let session = sessions.get(zone_id).expect("session");
    assert_eq!(session.state, "Playing");
    assert!(session.started_at.is_some());
    assert_eq!(session.transport_pending, None);
    assert_eq!(session.transport_pending_position_secs, None);
}

#[tokio::test]
async fn seek_settle_temporarily_blocks_next_handoff_arming() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-seek-settle";
    let current = test_asset("seek-settle-current");
    let current_key = current.source_ref.key();
    service.prime_session_for_play(zone_id, current);

    service.defer_next_handoff_after_seek(zone_id, Duration::from_secs(1));
    assert!(service.next_handoff_blocked_by_seek(zone_id));
    let error = service
        .arm_next_transport_uri(
            zone_id,
            &test_target(),
            &test_asset("seek-settle-next"),
            Some(&current_key),
        )
        .await
        .expect_err("next handoff must wait for seek settling");
    assert_eq!(error, "Playback changed");

    service.clear_seek_settling(zone_id);
    assert!(!service.next_handoff_blocked_by_seek(zone_id));
}

#[tokio::test]
async fn active_seek_reservation_blocks_handoff_until_settled() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-seek-reserved";
    let current = test_asset("seek-reserved-current");
    let current_key = current.source_ref.key();
    service.prime_session_for_play(zone_id, current);

    let reservation = service.begin_seek_reservation(zone_id);
    assert!(service.next_handoff_blocked_by_seek(zone_id));
    let error = service
        .arm_next_transport_uri(
            zone_id,
            &test_target(),
            &test_asset("seek-reserved-next"),
            Some(&current_key),
        )
        .await
        .expect_err("active seek reservation must reject next-track arming");
    assert_eq!(error, "Playback changed");

    service.finish_seek_reservation(zone_id, reservation, Some(Duration::from_secs(1)));
    assert!(!service.seek_reservation_active(zone_id));
    assert!(service.next_handoff_blocked_by_seek(zone_id));

    service.clear_seek_settling(zone_id);
    assert!(!service.next_handoff_blocked_by_seek(zone_id));
}

#[tokio::test]
async fn cancelled_seek_releases_its_reservation() {
    let service = Arc::new(UpnpRendererService::new("http://core.test".to_string()));
    let zone_id = "zone-seek-cancelled";
    let command_lock = service.command_lock_for_zone(zone_id);
    let held_command_lock = command_lock.lock().await;
    let seek_service = Arc::clone(&service);
    let target = test_target();

    let seek_task = tokio::spawn(async move { seek_service.seek(zone_id, &target, 12.0).await });
    for _ in 0..100 {
        if service.seek_reservation_active(zone_id) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(service.seek_reservation_active(zone_id));
    assert!(service.next_handoff_blocked_by_seek(zone_id));

    seek_task.abort();
    assert!(seek_task.await.unwrap_err().is_cancelled());
    drop(held_command_lock);

    assert!(!service.seek_reservation_active(zone_id));
    assert!(!service.next_handoff_blocked_by_seek(zone_id));
}

#[test]
fn superseded_seek_cannot_clear_newer_reservation() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-seek-superseded";

    let first = service.begin_seek_reservation(zone_id);
    let second = service.begin_seek_reservation(zone_id);
    assert_ne!(first, second);

    service.finish_seek_reservation(zone_id, first, Some(Duration::from_secs(1)));
    assert!(service.seek_reservation_matches(zone_id, second));
    assert!(service.next_handoff_blocked_by_seek(zone_id));

    service.finish_seek_reservation(zone_id, second, None);
    assert!(!service.next_handoff_blocked_by_seek(zone_id));
}

#[test]
fn seek_settle_reports_only_unexpired_cooldown() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-seek-remaining";

    service.defer_next_handoff_after_seek(zone_id, Duration::from_secs(1));
    assert!(service.seek_settle_remaining(zone_id).is_some());

    service.clear_seek_settling(zone_id);
    assert!(service.seek_settle_remaining(zone_id).is_none());
}

#[test]
fn starting_new_playback_clears_seek_settle_block() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-seek-new-play";

    service.begin_seek_reservation(zone_id);
    service.defer_next_handoff_after_seek(zone_id, Duration::from_secs(1));
    service.prime_session_for_play(zone_id, test_asset("seek-new-play"));

    assert!(!service.seek_reservation_active(zone_id));
    assert!(!service.next_handoff_blocked_by_seek(zone_id));
}

#[test]
fn seek_range_first_byte_ignores_range_before_active_seek() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let asset = test_asset("seek-range-before");
    let zone_id = "zone-seek-before";
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    service.mark_renderer_http_request(&asset.id, "abc", "local_get", Some("bytes=1000-"));
    {
        let mut sessions = service.sessions.lock().unwrap();
        let session = sessions.get_mut(zone_id).expect("session");
        session.state = "Transitioning".to_string();
        session.transport_pending = Some("seeking".to_string());
        session.transport_pending_position_secs = Some(120.0);
    }

    service.mark_seek_attempt_started(zone_id, play_id);
    service.mark_local_media_first_byte(&asset.id, "abc", Some("bytes=1000-"), 206, Some(1));

    let sessions = service.sessions.lock().unwrap();
    let session = sessions.get(zone_id).expect("session");
    assert_eq!(session.transport_pending.as_deref(), Some("seeking"));
    assert_eq!(session.transport_pending_position_secs, Some(120.0));
}

#[test]
fn seek_range_first_byte_ignores_stale_play_context() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let asset = test_asset("seek-range-stale");
    let zone_id = "zone-seek-stale";
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    {
        let mut sessions = service.sessions.lock().unwrap();
        let session = sessions.get_mut(zone_id).expect("session");
        session.play_id = play_id + 1;
        session.state = "Transitioning".to_string();
        session.transport_pending = Some("seeking".to_string());
        session.transport_pending_position_secs = Some(120.0);
    }

    service.mark_local_media_first_byte(&asset.id, "abc", Some("bytes=4410000-"), 206, Some(1));

    let sessions = service.sessions.lock().unwrap();
    let session = sessions.get(zone_id).expect("session");
    assert_eq!(session.transport_pending.as_deref(), Some("seeking"));
    assert_eq!(session.transport_pending_position_secs, Some(120.0));
}

#[test]
fn transport_refresh_preserves_completed_position_when_renderer_resets_to_start() {
    let now = Instant::now();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(UpnpAsset {
            duration_secs: Some(180.0),
            ..test_asset("complete")
        }),
        armed_next: None,
        state: "Playing".to_string(),
        started_at: now.checked_sub(Duration::from_secs(181)),
        paused_position: 0.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("STOPPED".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.0),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now,
    );

    assert_eq!(session.state, "Stopped");
    assert_eq!(session.paused_position, 180.0);
    assert!(session.started_at.is_none());
}

#[test]
fn transport_refresh_promotes_armed_next_when_renderer_uri_advances() {
    let now = Instant::now();
    let current = test_asset("current-uri");
    let mut next = test_asset("next-uri");
    if let SourceRef::LocalTrack {
        track_id, title, ..
    } = &mut next.source_ref
    {
        *track_id = 2;
        *title = Some("Next Track".to_string());
    }
    let next_key = next.source_ref.key();
    let next_uri = next.stream_url.clone();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(current),
        armed_next: Some(next),
        state: "Playing".to_string(),
        started_at: Some(now),
        paused_position: 179.5,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    let promoted = reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.42),
            duration_secs: Some(181.0),
            current_uri: Some(next_uri),
        },
        now,
    );

    assert_eq!(promoted.as_deref(), Some(next_key.as_str()));
    assert_eq!(
        session.current.as_ref().map(|asset| asset.id.as_str()),
        Some("next-uri")
    );
    assert!(session.armed_next.is_none());
    assert_eq!(session.state, "Playing");
    assert!((session.paused_position - 0.42).abs() < 0.01);
}

#[test]
fn transport_refresh_promotes_armed_next_when_kef_keeps_previous_uri() {
    let now = Instant::now();
    let current = UpnpAsset {
        duration_secs: Some(180.0),
        ..test_asset("current-kef-uri")
    };
    let current_uri = current.stream_url.clone();
    let mut next = test_asset("next-kef-uri");
    if let SourceRef::LocalTrack {
        track_id, title, ..
    } = &mut next.source_ref
    {
        *track_id = 2;
        *title = Some("Next Track".to_string());
    }
    let next_key = next.source_ref.key();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(current),
        armed_next: Some(next),
        state: "Playing".to_string(),
        started_at: Some(now),
        paused_position: 179.5,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    let promoted = reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.35),
            duration_secs: Some(181.0),
            current_uri: Some(current_uri),
        },
        now,
    );

    assert_eq!(promoted.as_deref(), Some(next_key.as_str()));
    assert_eq!(
        session.current.as_ref().map(|asset| asset.id.as_str()),
        Some("next-kef-uri")
    );
    assert!(session.armed_next.is_none());
    assert!((session.paused_position - 0.35).abs() < 0.01);
    assert_eq!(
        session
            .current
            .as_ref()
            .and_then(|asset| asset.duration_secs),
        Some(181.0)
    );
}

#[test]
fn seek_to_start_does_not_promote_armed_next() {
    let now = Instant::now();
    let current = UpnpAsset {
        duration_secs: Some(180.0),
        ..test_asset("seek-current")
    };
    let current_uri = current.stream_url.clone();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(current),
        armed_next: Some(test_asset("seek-next")),
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 179.5,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: Some("seeking".to_string()),
        transport_pending_position_secs: Some(0.0),
    };

    let promoted = reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.2),
            duration_secs: Some(180.0),
            current_uri: Some(current_uri),
        },
        now,
    );

    assert!(promoted.is_none());
    assert_eq!(
        session.current.as_ref().map(|asset| asset.id.as_str()),
        Some("seek-current")
    );
    assert!(session.armed_next.is_some());
}

#[test]
fn upnp_state_label_keeps_transitioning_distinct() {
    assert_eq!(upnp_state_label("TRANSITIONING"), "Transitioning");
    assert_eq!(upnp_state_label("PLAYING"), "Playing");
}

#[test]
fn transitioning_stale_track_uri_does_not_clear_current_asset() {
    let now = Instant::now();
    let mut session = UpnpSession {
        play_id: 0,
        current: Some(test_asset("new")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 0.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: None,
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    reconcile_session_with_transport(
        &mut session,
        UpnpTransportSnapshot {
            state: Some("TRANSITIONING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.0),
            duration_secs: None,
            current_uri: Some("http://core.test/upnp/stream/old?token=abc".to_string()),
        },
        now,
    );

    assert_eq!(
        session.current.as_ref().map(|asset| asset.id.as_str()),
        Some("new")
    );
}

#[test]
fn startup_refresh_timeout_is_inconclusive_until_deadline() {
    let now = Instant::now();
    let session = UpnpSession {
        play_id: 7,
        current: Some(test_asset("startup")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 0.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: Some(UpnpStartup {
            play_id: 7,
            asset_id: "startup".to_string(),
            started_at: now,
            accepted_at: None,
            accepted_reason: None,
            confirmed_playing_at: None,
            failed: false,
            timed_out: false,
        }),
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: None,
        transport_pending_position_secs: None,
    };

    assert!(startup_refresh_error_is_inconclusive(&session, now));
    assert!(startup_transport_snapshot_is_inconclusive(
        &session,
        &UpnpTransportSnapshot {
            state: Some("STOPPED".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.0),
            duration_secs: Some(180.0),
            current_uri: None,
        },
        now
    ));
}

#[test]
fn renderer_error_terminates_startup_and_clears_loading() {
    let now = Instant::now();
    let mut session = UpnpSession {
        play_id: 17,
        current: Some(test_asset("renderer-error")),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 0.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: Some(UpnpStartup {
            play_id: 17,
            asset_id: "renderer-error".to_string(),
            started_at: now,
            accepted_at: Some(now),
            accepted_reason: Some("local_media_first_byte".to_string()),
            confirmed_playing_at: None,
            failed: false,
            timed_out: false,
        }),
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: Some("loading".to_string()),
        transport_pending_position_secs: None,
    };
    let snapshot = UpnpTransportSnapshot {
        state: Some("STOPPED".to_string()),
        status: Some("ERROR_OCCURRED".to_string()),
        playback_speed: Some("1".to_string()),
        position_secs: Some(0.0),
        duration_secs: Some(404.0),
        current_uri: None,
    };

    let error = upnp_transport_snapshot_error(&snapshot).expect("renderer error");
    apply_upnp_transport_error(&mut session, &snapshot, &error);

    assert_eq!(session.state, "Stopped");
    assert_eq!(session.transport_pending, None);
    assert_eq!(session_transport_pending(&session), "none");
    assert_eq!(startup_phase(&session), "Failed");
    assert!(
        session.notice.as_deref().is_some_and(|notice| {
            notice.contains("ERROR_OCCURRED") && notice.contains("STOPPED")
        })
    );
}

#[test]
fn startup_refresh_treats_stale_playing_uri_as_inconclusive() {
    let now = Instant::now();
    let mut asset = test_asset("startup-new");
    asset.stream_url = "http://core.test/upnp/stream/startup-new?token=new".to_string();
    let session = UpnpSession {
        play_id: 7,
        current: Some(asset),
        armed_next: None,
        state: "Transitioning".to_string(),
        started_at: None,
        paused_position: 0.0,
        playback_polled_at: Some(now),
        playback_speed: None,
        volume: None,
        volume_polled_at: None,
        notice: None,
        startup: Some(UpnpStartup {
            play_id: 7,
            asset_id: "startup-new".to_string(),
            started_at: now,
            accepted_at: None,
            accepted_reason: None,
            confirmed_playing_at: None,
            failed: false,
            timed_out: false,
        }),
        reconfigure: UpnpReconfigureState::default(),
        transport_pending: Some("loading".to_string()),
        transport_pending_position_secs: None,
    };

    assert!(startup_transport_snapshot_is_inconclusive(
        &session,
        &UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(24.0),
            duration_secs: Some(180.0),
            current_uri: Some("http://core.test/upnp/stream/old?token=old".to_string()),
        },
        now
    ));
    assert!(!startup_transport_snapshot_is_inconclusive(
        &session,
        &UpnpTransportSnapshot {
            state: Some("PLAYING".to_string()),
            status: None,
            playback_speed: None,
            position_secs: Some(0.0),
            duration_secs: Some(180.0),
            current_uri: Some("http://core.test/upnp/stream/startup-new?token=new".to_string()),
        },
        now
    ));
}

#[test]
fn renderer_and_proxy_requests_are_recorded_per_play() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("qobuz-1-6");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);

    service.mark_renderer_http_request(&asset.id, "", "qobuz_get", None);
    service.mark_renderer_http_request(&asset.id, "", "qobuz_get", Some("bytes=0-"));
    service.mark_qobuz_proxy_first_byte(&asset.id, "", 1, None, 200, Some(1));

    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert_eq!(trace.play_id, play_id);
    assert_eq!(trace.renderer_requests.len(), 2);
    assert!(trace.first_renderer_request.is_some());
    assert_eq!(trace.qobuz_proxy_bytes.len(), 1);
    assert_eq!(trace.startup_phase, "Accepted");
    assert_eq!(
        trace.startup_confirmation.as_deref(),
        Some("qobuz_proxy_first_byte")
    );
}

#[test]
fn renderer_requests_use_token_to_disambiguate_replayed_asset() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let mut first_asset = test_asset("same-asset");
    first_asset.stream_url = "http://core.test/upnp/stream/same-asset?token=first".to_string();
    let mut second_asset = test_asset("same-asset");
    second_asset.stream_url = "http://core.test/upnp/stream/same-asset?token=second".to_string();
    let first_play_id = service.prime_session_for_play("zone-a", first_asset.clone());
    service.begin_play_trace("zone-a", first_play_id, &target, &first_asset);
    let second_play_id = service.prime_session_for_play("zone-b", second_asset.clone());
    service.begin_play_trace("zone-b", second_play_id, &target, &second_asset);

    service.mark_renderer_http_request("same-asset", "second", "local_get", Some("bytes=100-"));

    let first = service
        .diagnostics_for_zone("zone-a", "http://core.test".to_string(), target.clone())
        .last_play_trace
        .expect("first trace");
    let second = service
        .diagnostics_for_zone("zone-b", "http://core.test".to_string(), target)
        .last_play_trace
        .expect("second trace");
    assert_eq!(first.renderer_requests.len(), 0);
    assert_eq!(second.play_id, second_play_id);
    assert_eq!(second.renderer_requests.len(), 1);
    assert_eq!(
        second.renderer_requests[0].range.as_deref(),
        Some("bytes=100-")
    );
}

#[test]
fn next_handoff_trace_records_early_renderer_request() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let zone_id = "zone-upnp";
    let current_asset = test_asset("current-asset");
    let mut next_asset = test_asset("next-asset");
    next_asset.stream_url = "http://core.test/upnp/stream/next-asset?token=next".to_string();
    if let SourceRef::LocalTrack {
        track_id, title, ..
    } = &mut next_asset.source_ref
    {
        *track_id = 2;
        *title = Some("Next Track".to_string());
    }
    let play_id = service.prime_session_for_play(zone_id, current_asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &current_asset);

    service.record_next_handoff_prepared(zone_id, play_id, &next_asset);
    service.register_stream_trace_context(zone_id, play_id, &next_asset);
    service.record_next_handoff_armed(zone_id, play_id, &next_asset, &Ok(()));
    service.mark_first_playing_observed(zone_id);
    service.mark_renderer_http_request(&next_asset.id, "next", "local_get", Some("bytes=0-"));
    service.mark_next_handoff_promoted(zone_id, &next_asset.source_ref.key());

    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    let next = trace.next_handoff.expect("next handoff");
    assert_eq!(next.asset_id, "next-asset");
    assert!(next.ok);
    assert!(next.armed_at_ms.is_some());
    assert!(next.renderer_requested_at_ms.is_some());
    assert!(
        next.renderer_request_relative_to_eof_ms
            .is_some_and(|offset| offset < 0)
    );
    assert!(next.promoted_at_ms.is_some());
    assert_eq!(trace.renderer_requests.len(), 1);
    assert_eq!(trace.renderer_requests[0].asset_id, "next-asset");
}

#[test]
fn hegel_h390_dop_policy_skips_initial_stop_for_all_dsd_rates() {
    let target = hegel_h390_target();

    for (mode, rate) in [("Dsd64", 192_000), ("Dsd128", 384_000), ("Dsd256", 768_000)] {
        let asset = dop_asset("hegel-dop", mode, rate);
        let policy = dop_control_policy_for(&target, &asset);

        assert_eq!(policy, UpnpDopControlPolicy::HegelH390DopWav);
        assert!(policy.skips_initial_stop());
    }
}

#[test]
fn dop_trace_uses_dop_wav_container_label_for_dsd128() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = hegel_h390_target();
    let asset = dop_asset("trace-dop", "Dsd128", 384_000);
    let play_id = service.prime_session_for_play("zone-dop", asset.clone());

    service.begin_play_trace("zone-dop", play_id, &target, &asset);

    let trace = service
        .traces
        .lock()
        .unwrap()
        .get("zone-dop")
        .cloned()
        .unwrap();
    assert_eq!(trace.render_container.as_deref(), Some("dop_wav"));
}

#[test]
fn local_body_byte_does_not_confirm_startup_without_audio_payload() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = hegel_h390_target();
    let asset = dsd64_dop_asset("trace-dop-header");
    let play_id = service.prime_session_for_play("zone-dop-header", asset.clone());
    service.begin_play_trace("zone-dop-header", play_id, &target, &asset);

    service.mark_local_media_first_body_byte(&asset.id, "abc", Some("bytes=0-"), 206, Some(3));

    let diagnostics =
        service.diagnostics_for_zone("zone-dop-header", "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert!(trace.first_local_body_byte_ms.is_some());
    assert_eq!(trace.first_local_audio_payload_ms, None);
    assert_eq!(trace.startup_confirmation, None);
}

#[test]
fn dop_frame_confirms_startup_as_audio_payload() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = hegel_h390_target();
    let asset = dsd64_dop_asset("trace-dop-payload");
    let play_id = service.prime_session_for_play("zone-dop-payload", asset.clone());
    service.begin_play_trace("zone-dop-payload", play_id, &target, &asset);

    service.mark_local_media_dop_frame(&asset.id, "abc", Some("bytes=44-"), 206, Some(9));

    let diagnostics =
        service.diagnostics_for_zone("zone-dop-payload", "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert!(trace.first_local_audio_payload_ms.is_some());
    assert!(trace.first_local_dop_frame_ms.is_some());
    assert_eq!(
        trace.startup_confirmation.as_deref(),
        Some("local_media_dop_frame")
    );
}

#[test]
fn armed_next_can_promote_without_fresh_play() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = hegel_h390_target();
    let current = dsd64_dop_asset("current-dop");
    let next = dsd64_dop_asset("next-dop");
    let source = next.source_ref.clone();
    let zone_id = "zone-dop-next";
    let play_id = service.prime_session_for_play(zone_id, current.clone());
    service.begin_play_trace(zone_id, play_id, &target, &current);
    service.record_next_handoff_prepared(zone_id, play_id, &next);
    service.record_next_handoff_armed(zone_id, play_id, &next, &Ok(()));
    {
        let mut sessions = service.sessions.lock().unwrap();
        sessions.get_mut(zone_id).unwrap().armed_next = Some(next.clone());
    }

    assert!(service.has_armed_next_for_source(zone_id, &source));
    assert!(service.promote_armed_next_if_matches(zone_id, &source));
    assert!(!service.has_armed_next_for_source(zone_id, &source));

    let sessions = service.sessions.lock().unwrap();
    let session = sessions.get(zone_id).unwrap();
    assert_eq!(
        session.current.as_ref().map(|asset| asset.id.as_str()),
        Some("next-dop")
    );
    assert!(session.armed_next.is_none());
    drop(sessions);

    let trace = service
        .traces
        .lock()
        .unwrap()
        .get(zone_id)
        .cloned()
        .unwrap();
    assert!(trace.used_renderer_next);
    assert!(trace.handoff_promoted_without_play);
    assert!(
        trace
            .next_handoff
            .as_ref()
            .is_some_and(|next| next.promoted_without_play && next.promoted_at_ms.is_some())
    );
}

#[test]
fn completion_promotion_requires_an_early_renderer_request() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let current = test_asset("current-evidence");
    let mut next = test_asset("next-evidence");
    if let SourceRef::LocalTrack { track_id, .. } = &mut next.source_ref {
        *track_id = 2;
    }
    next.stream_url = "http://core.test/upnp/stream/next-evidence?token=next".to_string();
    let source = next.source_ref.clone();
    let zone_id = "zone-next-evidence";
    let play_id = service.prime_session_for_play(zone_id, current.clone());
    service.begin_play_trace(zone_id, play_id, &target, &current);
    service.record_next_handoff_prepared(zone_id, play_id, &next);
    service.register_stream_trace_context(zone_id, play_id, &next);
    service.record_next_handoff_armed(zone_id, play_id, &next, &Ok(()));
    service
        .sessions
        .lock()
        .unwrap()
        .get_mut(zone_id)
        .unwrap()
        .armed_next = Some(next.clone());

    assert!(!service.promote_armed_next_if_gapless_ready(zone_id, &source));
    assert!(service.has_armed_next_for_source(zone_id, &source));

    service.mark_renderer_http_request(&next.id, "next", "local_get", Some("bytes=0-"));

    assert!(service.promote_armed_next_if_gapless_ready(zone_id, &source));
    let trace = service
        .traces
        .lock()
        .unwrap()
        .get(zone_id)
        .cloned()
        .unwrap();
    assert_eq!(
        trace
            .next_handoff
            .as_ref()
            .and_then(|handoff| handoff.transition_path.as_deref()),
        Some("early_renderer_request")
    );
}

#[test]
fn fallback_handoff_diagnostics_survive_the_fresh_play_trace() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = test_target();
    let current = test_asset("current-fallback");
    let mut next = test_asset("next-fallback");
    if let SourceRef::LocalTrack { track_id, .. } = &mut next.source_ref {
        *track_id = 2;
    }
    let zone_id = "zone-next-fallback";
    let play_id = service.prime_session_for_play(zone_id, current.clone());
    service.begin_play_trace(zone_id, play_id, &target, &current);
    service.record_next_handoff_prepared(zone_id, play_id, &next);
    service.record_next_handoff_armed(zone_id, play_id, &next, &Err("unsupported".to_string()));
    service.mark_next_handoff_fallback(zone_id, &next.source_ref.key(), "renderer unsupported");

    let next_play_id = service.prime_session_for_play(zone_id, next.clone());
    service.begin_play_trace(zone_id, next_play_id, &target, &next);

    let trace = service
        .traces
        .lock()
        .unwrap()
        .get(zone_id)
        .cloned()
        .unwrap();
    let previous = trace.previous_handoff.expect("previous fallback handoff");
    assert_eq!(
        previous.transition_path.as_deref(),
        Some("fallback_auto_advance")
    );
    assert_eq!(
        previous.fallback_reason.as_deref(),
        Some("renderer unsupported")
    );
    assert!(previous.fresh_play_after_completion);
    assert!(
        trace
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("fallback auto-advance"))
    );
    assert!(
        service
            .sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .and_then(|session| session.notice.as_deref())
            .is_some_and(|notice| notice.contains("gapless handoff unavailable"))
    );
}

#[test]
fn next_uri_unsupported_renderer_skips_repeated_set_next_attempts() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let target = hegel_h390_target();
    let current = dsd64_dop_asset("current-no-next");
    let next = dsd64_dop_asset("next-no-next");
    let zone_id = "zone-no-next";
    let play_id = service.prime_session_for_play(zone_id, current.clone());
    service.begin_play_trace(zone_id, play_id, &target, &current);
    service.mark_next_uri_unsupported(&target);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime");
    let result = runtime.block_on(service.arm_next_transport_uri(zone_id, &target, &next, None));

    assert!(result.unwrap_err().contains("does not support"));
    let trace = service
        .traces
        .lock()
        .unwrap()
        .get(zone_id)
        .cloned()
        .unwrap();
    let next = trace.next_handoff.expect("next handoff fallback trace");
    assert_eq!(next.asset_id, "next-no-next");
    assert!(!next.ok);
    assert!(next.armed_at_ms.is_some());
    assert!(
        next.error
            .as_deref()
            .is_some_and(|error| error.contains("does not support SetNextAVTransportURI"))
    );
    assert!(trace.soap.is_empty());
}

#[tokio::test]
async fn kef_next_uri_is_disabled_to_preserve_seek_stability() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let mut target = test_target();
    target.name = "KEF LSX".to_string();
    target.manufacturer = Some("KEF".to_string());
    let zone_id = "zone-kef-seek-stability";
    let current = test_asset("kef-current");
    let next = test_asset("kef-next");
    service.prime_session_for_play(zone_id, current);

    let error = service
        .arm_next_transport_uri(zone_id, &target, &next, None)
        .await
        .expect_err("KEF next URI must remain unarmed");

    assert!(error.contains("preserve seek stability"));
    assert!(!service.has_armed_next_for_source(zone_id, &next.source_ref));
    assert!(service.next_uri_unsupported(&target));
}

#[test]
fn probe_acceptance_allows_renderer_get_after_play_error() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("probe-192");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    service.mark_renderer_http_request(&asset.id, "", "local_get", None);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime");
    let acceptance = runtime.block_on(service.probe_acceptance_from_play_result(
        zone_id,
        play_id,
        &target,
        Err("UPnP SOAP error 501: Action Failed".to_string()),
    ));

    assert!(acceptance.accepted);
    assert!(acceptance.renderer_get);
    assert_eq!(
        acceptance.evidence.as_deref(),
        Some("renderer_http_get_after_play_error")
    );
    assert_eq!(acceptance.error, None);
}

#[test]
fn dsd_probe_requires_playing_not_just_renderer_get() {
    let fetched_without_playing = UpnpProbeAcceptance {
        accepted: true,
        renderer_get: true,
        renderer_head: false,
        playing_observed: false,
        terminal_state: None,
        evidence: Some("renderer_http_get_after_play_error".to_string()),
        error: None,
    };
    assert!(!dsd_probe_accepted(&fetched_without_playing));

    let playing = UpnpProbeAcceptance {
        playing_observed: true,
        ..fetched_without_playing
    };
    assert!(dsd_probe_accepted(&playing));
}

#[test]
fn playback_does_not_accept_qobuz_proxy_byte_after_play_error_without_playing() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("hegel-qobuz");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    service.mark_renderer_http_request(&asset.id, "", "qobuz_get", None);
    service.mark_qobuz_proxy_first_byte(&asset.id, "", 1, None, 200, Some(1));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime");
    let accepted = runtime.block_on(service.play_error_accepted_after_renderer_fetch_with_grace(
        zone_id,
        play_id,
        &target,
        "UPnP SOAP error 501: Action Failed",
        Duration::ZERO,
    ));

    assert!(!accepted);
    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert_eq!(trace.startup_phase, "Accepted");
    assert_eq!(
        trace.startup_confirmation.as_deref(),
        Some("qobuz_proxy_first_byte")
    );
    assert_eq!(trace.notice, None);
}

#[test]
fn playback_does_not_accept_qobuz_get_after_play_error_without_proxy_byte() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("hegel-qobuz");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    service.mark_renderer_http_request(&asset.id, "", "qobuz_get", None);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime");
    let accepted = runtime.block_on(service.play_error_accepted_after_renderer_fetch_with_grace(
        zone_id,
        play_id,
        &target,
        "UPnP SOAP error 501: Action Failed",
        Duration::ZERO,
    ));

    assert!(!accepted);
    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert_eq!(trace.startup_confirmation, None);
}

#[test]
fn playback_does_not_accept_local_get_after_play_error() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("generated-dsp");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    service.mark_renderer_http_request(&asset.id, "", "local_get", None);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime");
    let accepted = runtime.block_on(service.play_error_accepted_after_renderer_fetch_with_grace(
        zone_id,
        play_id,
        &target,
        "UPnP SOAP error 501: Action Failed",
        Duration::ZERO,
    ));

    assert!(!accepted);
    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert_eq!(trace.startup_confirmation, None);
    assert_eq!(trace.notice, None);
}

#[test]
fn playback_does_not_accept_local_first_byte_after_play_error_without_playing() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("generated-dsp-first-byte");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    service.mark_renderer_http_request(&asset.id, "", "local_get", Some("bytes=0-"));
    service.mark_local_media_first_byte(&asset.id, "", Some("bytes=0-"), 206, Some(12));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime");
    let accepted = runtime.block_on(service.play_error_accepted_after_renderer_fetch_with_grace(
        zone_id,
        play_id,
        &target,
        "UPnP SOAP error 501: Action Failed",
        Duration::ZERO,
    ));

    assert!(!accepted);
    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert_eq!(
        trace.startup_confirmation.as_deref(),
        Some("local_media_first_byte")
    );
    assert_eq!(trace.notice, None);
}

#[test]
fn playback_accepts_play_error_after_confirmed_playing() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("generated-dsp-playing");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);
    service.mark_renderer_http_request(&asset.id, "", "local_get", Some("bytes=0-"));
    service.mark_local_media_first_byte(&asset.id, "", Some("bytes=0-"), 206, Some(12));
    {
        let mut sessions = service.sessions.lock().unwrap();
        let session = sessions.get_mut(zone_id).expect("session");
        session.state = "Playing".to_string();
        mark_session_startup_playing(session, Instant::now());
    }
    service.mark_first_playing_observed(zone_id);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime");
    let accepted = runtime.block_on(service.play_error_accepted_after_renderer_fetch_with_grace(
        zone_id,
        play_id,
        &target,
        "UPnP SOAP error 501: Action Failed",
        Duration::ZERO,
    ));

    assert!(accepted);
    service.finish_play_trace(zone_id, play_id, None);
    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert!(trace.total_elapsed_ms.is_some());
    assert_eq!(trace.startup_phase, "Playing");
    assert!(trace.first_playing_observed_ms.is_some());
    assert!(
        trace
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("renderer reported PLAYING"))
    );
}

#[test]
fn command_generation_zero_matches_until_explicit_command_starts() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";

    assert_eq!(service.current_command_generation(zone_id), 0);
    assert!(service.command_generation_matches(zone_id, 0));

    let generation = service.begin_prepare_command(zone_id);

    assert_eq!(generation, 1);
    assert!(!service.command_generation_matches(zone_id, 0));
    assert!(service.command_generation_matches(zone_id, generation));
}

#[test]
fn seek_trace_records_target_seconds() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let asset = test_asset("seek-trace");
    let play_id = service.prime_session_for_play(zone_id, asset.clone());
    service.begin_play_trace(zone_id, play_id, &target, &asset);

    service.record_seek_trace(
        zone_id,
        142.5,
        Some(true),
        Some("position_converged".to_string()),
        &Ok(String::new()),
    );

    let diagnostics = service.diagnostics_for_zone(zone_id, "http://core.test".to_string(), target);
    let trace = diagnostics.last_play_trace.expect("trace");
    assert_eq!(trace.seeks.len(), 1);
    assert_eq!(trace.seeks[0].target_secs, 142.5);
    assert!(trace.seeks[0].ok);
}

#[tokio::test]
async fn status_refresh_skips_when_upnp_command_lock_is_busy() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let target = test_target();
    let command_lock = service.command_lock_for_zone(zone_id);
    let _guard = command_lock.lock().await;

    let started = Instant::now();
    let result = service
        .refresh_playback_snapshot(zone_id, &target, Duration::ZERO)
        .await;

    assert!(result.is_ok());
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[test]
fn paused_session_is_available_for_manual_capability_probe() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    service.seed_playback_for_test(zone_id, test_asset("paused-track"), "Paused");

    assert!(service.session_idle_for_probe(zone_id));
}

#[test]
fn active_session_is_not_available_for_manual_capability_probe() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    service.seed_playback_for_test(zone_id, test_asset("playing-track"), "Playing");

    assert!(!service.session_idle_for_probe(zone_id));
    service.seed_playback_for_test(zone_id, test_asset("transitioning-track"), "Transitioning");
    assert!(!service.session_idle_for_probe(zone_id));
}

#[test]
fn reconfigure_snapshot_reports_pending_and_applied_signatures() {
    let service = UpnpRendererService::new("http://core.test".to_string());
    let zone_id = "zone-upnp";
    let mut asset = test_asset("playing-track");
    asset.render_signature = Some("old".to_string());
    asset.configured_render_signature = Some("old".to_string());
    service.seed_playback_for_test(zone_id, asset, "Playing");

    let generation = service.begin_reconfigure(zone_id);
    service.mark_reconfigure_status(zone_id, generation, "rendering");
    let pending = service.snapshot(zone_id).expect("snapshot");

    assert!(pending.restart_pending);
    assert_eq!(pending.render_status, "rendering");
    assert!(!pending.config_applied_to_current_playback);
    assert_eq!(pending.active_render_signature.as_deref(), Some("old"));

    service.finish_reconfigure(
        zone_id,
        generation,
        "applied",
        Some("old".to_string()),
        Some(12),
        Some(14),
        Some(true),
    );
    let applied = service.snapshot(zone_id).expect("snapshot");

    assert!(!applied.restart_pending);
    assert!(applied.config_applied_to_current_playback);
    assert_eq!(applied.last_render_ms, Some(12));
    assert_eq!(applied.last_prepare_ms, Some(14));
    assert_eq!(applied.last_cache_hit, Some(true));
}

#[test]
fn didl_metadata_includes_art_size_and_renderer_protocol_info_when_known() {
    let metadata = didl_metadata(
        &UpnpAsset {
            art_url: Some("http://core.test/upnp/art/new?token=abc".to_string()),
            byte_len: Some(12345),
            ..test_asset("new")
        },
        &test_target(),
    );

    assert!(metadata.contains(r#" size="12345""#));
    assert!(
        metadata.contains(
            "<upnp:albumArtURI>http://core.test/upnp/art/new?token=abc</upnp:albumArtURI>"
        )
    );
    assert!(metadata.contains(r#"protocolInfo="http-get:*:audio/flac:DLNA.ORG_OP=01""#));
}

#[test]
fn didl_metadata_includes_probe_pcm_rate_and_depth() {
    let mut asset = test_asset("probe-192");
    asset.source_ref = SourceRef::LocalTrack {
        track_id: -1,
        file_name: None,
        title: Some("Probe".to_string()),
        artist: Some("Fozmo".to_string()),
        album: Some("Output probe".to_string()),
        album_artist: None,
        album_id: None,
        art_id: None,
        duration_secs: Some(4.0),
        ext_hint: None,
        radio: false,
        radio_context: None,
        playlist_context: None,
    };
    asset.mime_type = "audio/wav".to_string();
    asset.source_rate = 192_000;
    asset.source_bits = 24;

    let metadata = didl_metadata(&asset, &test_target());

    assert!(metadata.contains(r#"sampleFrequency="192000""#));
    assert!(metadata.contains(r#"bitsPerSample="24""#));
    assert!(metadata.contains(r#"nrAudioChannels="2""#));
}

#[test]
fn didl_metadata_includes_rendered_pcm_output_rate_and_depth() {
    let mut asset = test_asset("rendered-192");
    asset.mime_type = "audio/flac".to_string();
    asset.source_rate = 48_000;
    asset.source_bits = 24;
    asset.target_rate = 192_000;
    asset.target_bits = 24;
    asset.render_ms = Some(10);

    let metadata = didl_metadata(&asset, &test_target());

    assert!(metadata.contains(r#"sampleFrequency="192000""#));
    assert!(metadata.contains(r#"bitsPerSample="24""#));
    assert!(metadata.contains(r#"nrAudioChannels="2""#));
}

#[test]
fn didl_metadata_uses_rendered_pcm_rate_even_when_below_source_rate() {
    let mut asset = test_asset("rendered-176");
    asset.mime_type = "audio/wav".to_string();
    asset.source_rate = 192_000;
    asset.source_bits = 24;
    asset.target_rate = 176_400;
    asset.target_bits = 24;
    asset.render_ms = Some(10);

    let metadata = didl_metadata(&asset, &test_target());

    assert!(metadata.contains(r#"sampleFrequency="176400""#));
    assert!(!metadata.contains(r#"sampleFrequency="192000""#));
    assert!(metadata.contains(r#"bitsPerSample="24""#));
    assert!(metadata.contains(r#"nrAudioChannels="2""#));
}

#[test]
fn upnp_playback_timeouts_are_shorter_than_legacy_soap_timeout() {
    assert_eq!(UPNP_SOAP_STOP_TIMEOUT, Duration::from_millis(1500));
    assert_eq!(UPNP_SOAP_SET_URI_TIMEOUT, Duration::from_secs(8));
    assert_eq!(UPNP_SOAP_PLAY_TIMEOUT, Duration::from_secs(8));
    assert!(UPNP_SOAP_STOP_TIMEOUT < UPNP_SOAP_ACTION_TIMEOUT);
    assert!(UPNP_SOAP_SET_URI_TIMEOUT < UPNP_SOAP_ACTION_TIMEOUT);
}

#[test]
fn diagnostics_warn_when_public_url_is_loopback() {
    let warnings = upnp_diagnostic_warnings("http://127.0.0.1:3000", &test_target());

    assert!(warnings.iter().any(|warning| warning.contains("loopback")));
}

fn test_asset(id: &str) -> UpnpAsset {
    UpnpAsset {
        id: id.to_string(),
        source_ref: SourceRef::LocalTrack {
            track_id: 1,
            file_name: None,
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: Some(180.0),
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        },
        stream_url: format!("http://core.test/upnp/stream/{id}?token=abc"),
        mime_type: "audio/flac".to_string(),
        byte_len: Some(1024),
        art_url: None,
        title: Some("Track".to_string()),
        artist: Some("Artist".to_string()),
        album: Some("Album".to_string()),
        duration_secs: Some(180.0),
        source_rate: 44_100,
        target_rate: 44_100,
        source_bits: 16,
        target_bits: 16,
        active_output_mode: None,
        qobuz_resolve_ms: None,
        asset_registration_ms: None,
        render_signature: Some(format!("sig-{id}")),
        configured_render_signature: Some(format!("sig-{id}")),
        render_ms: None,
        prepare_ms: None,
        cache_hit: None,
        render_or_stream_plan: None,
        cache_lookup_ms: None,
        cache_wait_ms: None,
    }
}

fn dsd64_dop_asset(id: &str) -> UpnpAsset {
    dop_asset(id, "Dsd64", 192_000)
}

fn dop_asset(id: &str, mode: &str, target_rate: u32) -> UpnpAsset {
    UpnpAsset {
        mime_type: "audio/wav".to_string(),
        source_rate: 96_000,
        target_rate,
        source_bits: 24,
        target_bits: 24,
        active_output_mode: Some(mode.to_string()),
        render_ms: Some(0),
        render_or_stream_plan: Some("progressive_wav_stream".to_string()),
        ..test_asset(id)
    }
}

fn temp_probe_test_dir(prefix: &str) -> PathBuf {
    let mut token = [0_u8; 8];
    OsRng.fill_bytes(&mut token);
    let dir = std::env::temp_dir().join(format!("{prefix}-{:x}", u64::from_le_bytes(token)));
    std::fs::create_dir(&dir).unwrap();
    dir
}

fn test_target() -> UpnpRendererTarget {
    UpnpRendererTarget {
        id: "renderer-1".to_string(),
        name: "KEF Test".to_string(),
        host: "192.168.1.23".to_string(),
        port: 80,
        model: Some("LS50 Wireless II".to_string()),
        manufacturer: Some("KEF".to_string()),
        av_transport_control_url: "http://192.168.1.23/AVTransport".to_string(),
        rendering_control_url: None,
        connection_manager_url: None,
        max_sample_rate: 192_000,
        max_bit_depth: 24,
        max_dsd_rate: None,
        capability_detection_source: CapabilityDetectionSource::Advertised,
        capability_detection_status: CapabilityDetectionStatus::Complete,
        capability_detection_message: None,
        protocol_info: vec!["http-get:*:audio/flac:DLNA.ORG_OP=01".to_string()],
        pcm_containers: Vec::new(),
    }
}

fn hegel_h390_target() -> UpnpRendererTarget {
    UpnpRendererTarget {
        name: "Hegel H390".to_string(),
        model: Some("H390".to_string()),
        manufacturer: Some("Hegel".to_string()),
        protocol_info: vec!["http-get:*:audio/wav:*".to_string()],
        pcm_containers: vec![UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }],
        max_dsd_rate: Some(64),
        ..test_target()
    }
}
