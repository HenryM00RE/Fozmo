use super::trace::*;
use super::*;

impl UpnpRendererService {
    pub fn prepare_source(&self, source: UpnpSource, target: &UpnpRendererTarget) -> UpnpAsset {
        self.evict_expired_assets();
        match source {
            UpnpSource::RemoteStream {
                id,
                source_ref,
                stream_url,
                mime_type,
                byte_len,
                art_url,
                tags,
                source_rate,
                source_bits,
                qobuz_resolve_ms,
                asset_registration_ms,
            } => UpnpAsset {
                id,
                source_ref,
                stream_url,
                mime_type: if mime_type.trim().is_empty() {
                    "audio/flac".to_string()
                } else {
                    mime_type
                },
                byte_len,
                art_url,
                title: tags.title,
                artist: tags.artist,
                album: tags.album,
                duration_secs: tags.duration_secs,
                source_rate,
                target_rate: upnp_target_rate(target, source_rate),
                source_bits,
                target_bits: upnp_target_bits(target, source_bits),
                active_output_mode: None,
                qobuz_resolve_ms,
                asset_registration_ms,
                render_signature: None,
                configured_render_signature: None,
                render_ms: None,
                prepare_ms: None,
                cache_hit: None,
                render_or_stream_plan: None,
                cache_lookup_ms: None,
                cache_wait_ms: None,
            },
            UpnpSource::GeneratedDspStream {
                id,
                zone_id,
                source_ref,
                mime_type,
                tags,
                source_rate,
                source_bits,
                target_rate,
                target_bits,
                active_output_mode,
                byte_len,
                dop_lead_in_data_len,
                target,
                playback_config,
            } => {
                let registration_started = Instant::now();
                let mut token = [0_u8; 18];
                OsRng.fill_bytes(&mut token);
                let token = URL_SAFE_NO_PAD.encode(token);
                let stream_url = format!(
                    "{}/upnp/stream/{}?token={}",
                    self.public_base_url.trim_end_matches('/'),
                    urlencoding::encode(&id),
                    token
                );
                let expires_at = Instant::now() + UPNP_ASSET_TTL;
                let mut streams = self.generated_dsp_streams.lock().unwrap();
                let cached =
                    streams
                        .entry(id.clone())
                        .or_insert_with(|| CachedGeneratedDspStream {
                            tokens: Vec::new(),
                            stream: UpnpGeneratedDspStream {
                                source_ref: source_ref.clone(),
                                zone_id: zone_id.clone(),
                                mime_type: mime_type.clone(),
                                byte_len,
                                source_rate,
                                source_bits,
                                target_rate,
                                target_bits,
                                active_output_mode: active_output_mode.clone(),
                                dop_lead_in_data_len,
                                target: target.clone(),
                                playback_config: playback_config.clone(),
                                expires_at,
                            },
                        });
                cached.stream = UpnpGeneratedDspStream {
                    source_ref: source_ref.clone(),
                    zone_id,
                    mime_type: mime_type.clone(),
                    byte_len,
                    source_rate,
                    source_bits,
                    target_rate,
                    target_bits,
                    active_output_mode: active_output_mode.clone(),
                    dop_lead_in_data_len,
                    target: target.clone(),
                    playback_config,
                    expires_at,
                };
                cached.tokens.push(token);

                UpnpAsset {
                    id,
                    source_ref,
                    stream_url,
                    mime_type,
                    byte_len,
                    art_url: None,
                    title: tags.title,
                    artist: tags.artist,
                    album: tags.album,
                    duration_secs: tags.duration_secs,
                    source_rate,
                    target_rate,
                    source_bits,
                    target_bits,
                    active_output_mode,
                    qobuz_resolve_ms: None,
                    asset_registration_ms: Some(elapsed_ms(registration_started)),
                    render_signature: None,
                    configured_render_signature: None,
                    render_ms: None,
                    prepare_ms: None,
                    cache_hit: None,
                    render_or_stream_plan: None,
                    cache_lookup_ms: None,
                    cache_wait_ms: None,
                }
            }
            UpnpSource::LocalFile {
                source_ref,
                path,
                tags,
                cover,
                byte_len,
                source_rate,
                source_bits,
            } => {
                let registration_started = Instant::now();
                let is_probe = source_ref.local_track_id() == Some(-1);
                let id = upnp_asset_id_for_path(&path);
                let mime_type = audio_content_type_from_path(&path)
                    .unwrap_or("audio/flac")
                    .to_string();
                let mut token = [0_u8; 18];
                OsRng.fill_bytes(&mut token);
                let token = URL_SAFE_NO_PAD.encode(token);
                let stream_url = format!(
                    "{}/upnp/stream/{}?token={}",
                    self.public_base_url.trim_end_matches('/'),
                    urlencoding::encode(&id),
                    token
                );
                let art_url = cover.as_ref().map(|_| {
                    format!(
                        "{}/upnp/art/{}?token={}",
                        self.public_base_url.trim_end_matches('/'),
                        urlencoding::encode(&id),
                        token
                    )
                });
                let expires_at = Instant::now() + UPNP_ASSET_TTL;
                let mut assets = self.assets.lock().unwrap();
                let cached = assets.entry(id.clone()).or_insert_with(|| CachedAsset {
                    path: path.clone(),
                    tokens: Vec::new(),
                    art: None,
                    mime_type: mime_type.clone(),
                    byte_len,
                    is_probe,
                    active_output_mode: None,
                    target_bits: upnp_target_bits(target, source_bits),
                    expires_at,
                });
                cached.path = path;
                cached.art = cover;
                cached.mime_type = mime_type.clone();
                cached.byte_len = byte_len;
                cached.is_probe = is_probe;
                cached.target_bits = upnp_target_bits(target, source_bits);
                cached.expires_at = expires_at;
                cached.tokens.push(token);

                UpnpAsset {
                    id,
                    source_ref,
                    stream_url,
                    mime_type,
                    byte_len,
                    art_url,
                    title: tags.title,
                    artist: tags.artist,
                    album: tags.album,
                    duration_secs: tags.duration_secs,
                    source_rate,
                    target_rate: upnp_target_rate(target, source_rate),
                    source_bits,
                    target_bits: upnp_target_bits(target, source_bits),
                    active_output_mode: None,
                    qobuz_resolve_ms: None,
                    asset_registration_ms: Some(elapsed_ms(registration_started)),
                    render_signature: None,
                    configured_render_signature: None,
                    render_ms: None,
                    prepare_ms: None,
                    cache_hit: None,
                    render_or_stream_plan: None,
                    cache_lookup_ms: None,
                    cache_wait_ms: None,
                }
            }
        }
    }

    pub fn asset_for_request(&self, asset_id: &str, token: &str) -> Option<UpnpCachedAsset> {
        let assets = self.assets.lock().unwrap();
        let asset = assets.get(asset_id)?;
        if asset.expires_at <= Instant::now() || !constant_time_token_matches(&asset.tokens, token)
        {
            return None;
        }
        Some(UpnpCachedAsset {
            path: asset.path.clone(),
            is_probe: asset.is_probe,
            active_output_mode: asset.active_output_mode.clone(),
            target_bits: asset.target_bits,
            mime_type: asset.mime_type.clone(),
        })
    }

    pub fn update_cached_asset_signal_path(&self, asset: &UpnpAsset) {
        let mut assets = self.assets.lock().unwrap();
        if let Some(cached) = assets.get_mut(&asset.id) {
            cached.active_output_mode = asset.active_output_mode.clone();
            cached.target_bits = asset.target_bits;
            cached.mime_type = asset.mime_type.clone();
        }
    }

    pub fn generated_dsp_stream_for_request(
        &self,
        asset_id: &str,
        token: &str,
    ) -> Option<UpnpGeneratedDspStream> {
        let streams = self.generated_dsp_streams.lock().unwrap();
        let stream = streams.get(asset_id)?;
        (stream.stream.expires_at > Instant::now()
            && constant_time_token_matches(&stream.tokens, token))
        .then(|| stream.stream.clone())
    }

    pub fn register_remote_stream(
        &self,
        asset_id: &str,
        art: Option<TrackCover>,
        mime_type: String,
        byte_len: Option<u64>,
        qobuz_format_id: Option<u32>,
    ) -> String {
        self.evict_expired_remote_streams();
        let mut token = [0_u8; 18];
        OsRng.fill_bytes(&mut token);
        let token = URL_SAFE_NO_PAD.encode(token);
        let expires_at = Instant::now() + UPNP_ASSET_TTL;
        let mut remote_streams = self.remote_streams.lock().unwrap();
        let cached = remote_streams
            .entry(asset_id.to_string())
            .or_insert_with(|| CachedRemoteStream {
                tokens: Vec::new(),
                art: None,
                mime_type: "audio/flac".to_string(),
                byte_len: None,
                qobuz_format_id: None,
                expires_at,
            });
        cached.art = art;
        cached.mime_type = if mime_type.trim().is_empty() {
            "audio/flac".to_string()
        } else {
            mime_type
        };
        cached.byte_len = byte_len;
        cached.qobuz_format_id = qobuz_format_id;
        cached.expires_at = expires_at;
        cached.tokens.push(token.clone());
        token
    }

    pub fn remote_stream_token_valid(&self, asset_id: &str, token: &str) -> bool {
        let remote_streams = self.remote_streams.lock().unwrap();
        remote_streams.get(asset_id).is_some_and(|asset| {
            asset.expires_at > Instant::now() && constant_time_token_matches(&asset.tokens, token)
        })
    }

    pub fn remote_stream_metadata_for_request(
        &self,
        asset_id: &str,
        token: &str,
    ) -> Option<UpnpRemoteStreamMetadata> {
        let remote_streams = self.remote_streams.lock().unwrap();
        remote_streams.get(asset_id).and_then(|asset| {
            (asset.expires_at > Instant::now() && constant_time_token_matches(&asset.tokens, token))
                .then(|| UpnpRemoteStreamMetadata {
                    mime_type: asset.mime_type.clone(),
                    byte_len: asset.byte_len,
                    qobuz_format_id: asset.qobuz_format_id,
                })
        })
    }

    pub fn art_metadata_for_request(&self, asset_id: &str, token: &str) -> Option<(String, usize)> {
        self.art_for_request(asset_id, token)
            .map(|cover| (cover.mime, cover.data.len()))
    }

    pub fn art_for_request(&self, asset_id: &str, token: &str) -> Option<TrackCover> {
        let assets = self.assets.lock().unwrap();
        if let Some(asset) = assets.get(asset_id)
            && asset.expires_at > Instant::now()
            && constant_time_token_matches(&asset.tokens, token)
        {
            return asset.art.clone();
        }
        drop(assets);
        let remote_streams = self.remote_streams.lock().unwrap();
        remote_streams.get(asset_id).and_then(|asset| {
            (asset.expires_at > Instant::now() && constant_time_token_matches(&asset.tokens, token))
                .then(|| asset.art.clone())
                .flatten()
        })
    }

    pub fn current_source_for_key(&self, zone_id: &str, key: &str) -> Option<SourceRef> {
        let sessions = self.sessions.lock().unwrap();
        let current = sessions.get(zone_id)?.current.as_ref()?;
        (current.source_ref.key() == key).then(|| current.source_ref.clone())
    }

    pub fn current_art_for_key(&self, zone_id: &str, key: &str) -> Option<TrackCover> {
        let current = {
            let sessions = self.sessions.lock().unwrap();
            let current = sessions.get(zone_id)?.current.as_ref()?;
            (current.source_ref.key() == key).then(|| current.clone())?
        };
        self.art_for_current_asset(&current.id)
    }

    pub(super) fn art_for_current_asset(&self, asset_id: &str) -> Option<TrackCover> {
        let assets = self.assets.lock().unwrap();
        if let Some(asset) = assets.get(asset_id)
            && asset.expires_at > Instant::now()
        {
            return asset.art.clone();
        }
        drop(assets);
        let remote_streams = self.remote_streams.lock().unwrap();
        remote_streams.get(asset_id).and_then(|asset| {
            (asset.expires_at > Instant::now())
                .then(|| asset.art.clone())
                .flatten()
        })
    }

    pub(super) fn evict_expired_assets(&self) {
        let now = Instant::now();
        self.assets
            .lock()
            .unwrap()
            .retain(|_, asset| asset.expires_at > now);
        self.generated_dsp_streams
            .lock()
            .unwrap()
            .retain(|_, stream| stream.stream.expires_at > now);
        self.evict_expired_stream_trace_contexts();
    }

    pub(super) fn evict_expired_remote_streams(&self) {
        let now = Instant::now();
        self.remote_streams
            .lock()
            .unwrap()
            .retain(|_, asset| asset.expires_at > now);
        self.evict_expired_stream_trace_contexts();
    }

    pub(super) fn evict_expired_stream_trace_contexts(&self) {
        let now = Instant::now();
        self.stream_trace_contexts
            .lock()
            .unwrap()
            .retain(|_, context| context.expires_at > now);
    }
}

pub(super) fn upnp_target_rate(target: &UpnpRendererTarget, source_rate: u32) -> u32 {
    if source_rate == 0 {
        target.max_sample_rate
    } else {
        target.max_sample_rate.min(source_rate)
    }
}

pub(super) fn upnp_target_bits(target: &UpnpRendererTarget, source_bits: u32) -> u32 {
    if source_bits == 0 {
        target.max_bit_depth as u32
    } else {
        (target.max_bit_depth as u32).min(source_bits)
    }
}

pub(super) fn upnp_asset_id_for_path(path: &std::path::Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    URL_SAFE_NO_PAD.encode(&digest[..18])
}

pub(super) fn audio_content_type_from_path(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())?
        .as_str()
    {
        "flac" => Some("audio/flac"),
        "wav" => Some("audio/wav"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "aiff" | "aif" => Some("audio/aiff"),
        "dsf" => Some("audio/x-dsf"),
        "dff" => Some("audio/x-dff"),
        "ogg" => Some("audio/ogg"),
        _ => None,
    }
}
