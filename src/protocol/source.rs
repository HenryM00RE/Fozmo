use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RadioSeedContext {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub mbid: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RadioContext {
    pub provider: String,
    pub anchor: RadioSeedContext,
    pub last_seed: RadioSeedContext,
    #[serde(default)]
    pub hop: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct PlaylistContext {
    pub playlist_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRef {
    LocalTrack {
        track_id: i64,
        #[serde(default)]
        file_name: Option<String>,
        title: Option<String>,
        artist: Option<String>,
        #[serde(default)]
        album: Option<String>,
        #[serde(default)]
        album_artist: Option<String>,
        #[serde(default)]
        album_id: Option<i64>,
        #[serde(default)]
        art_id: Option<i64>,
        #[serde(default)]
        duration_secs: Option<f64>,
        #[serde(default)]
        ext_hint: Option<String>,
        #[serde(default)]
        radio: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radio_context: Option<RadioContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        playlist_context: Option<PlaylistContext>,
    },
    QobuzTrack {
        track_id: u64,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        #[serde(default)]
        album_id: Option<String>,
        image_url: Option<String>,
        #[serde(default)]
        duration_secs: Option<f64>,
        #[serde(default)]
        radio: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radio_context: Option<RadioContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        playlist_context: Option<PlaylistContext>,
    },
}

impl SourceRef {
    pub fn kind(&self) -> &'static str {
        match self {
            SourceRef::LocalTrack { .. } => "local_track",
            SourceRef::QobuzTrack { .. } => "qobuz_track",
        }
    }

    pub fn local_track_id(&self) -> Option<i64> {
        match self {
            SourceRef::LocalTrack { track_id, .. } => Some(*track_id),
            SourceRef::QobuzTrack { .. } => None,
        }
    }

    pub fn qobuz_track_id(&self) -> Option<u64> {
        match self {
            SourceRef::QobuzTrack { track_id, .. } => Some(*track_id),
            SourceRef::LocalTrack { .. } => None,
        }
    }

    pub fn key(&self) -> String {
        match self {
            SourceRef::LocalTrack { track_id, .. } => format!("local:{track_id}"),
            SourceRef::QobuzTrack { track_id, .. } => format!("qobuz:{track_id}"),
        }
    }

    pub fn is_radio(&self) -> bool {
        matches!(
            self,
            SourceRef::LocalTrack { radio: true, .. } | SourceRef::QobuzTrack { radio: true, .. }
        )
    }

    pub fn playlist_id(&self) -> Option<&str> {
        match self {
            SourceRef::LocalTrack {
                playlist_context, ..
            }
            | SourceRef::QobuzTrack {
                playlist_context, ..
            } => playlist_context
                .as_ref()
                .map(|context| context.playlist_id.trim())
                .filter(|id| !id.is_empty()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn property_local_source_protocol_round_trips_arbitrary_identity(
            track_id in any::<i64>(),
            title in proptest::option::of(".{0,128}"),
            radio in any::<bool>()
        ) {
            let value = serde_json::json!({
                "kind": "local_track",
                "track_id": track_id,
                "file_name": null,
                "title": title,
                "artist": null,
                "radio": radio
            });
            let parsed: SourceRef = serde_json::from_value(value).unwrap();
            prop_assert_eq!(parsed.local_track_id(), Some(track_id));
            prop_assert_eq!(parsed.is_radio(), radio);
            let reparsed: SourceRef = serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
            prop_assert_eq!(reparsed.local_track_id(), Some(track_id));
        }
    }

    #[test]
    fn legacy_local_track_defaults_radio_to_false() {
        let source: SourceRef = serde_json::from_value(serde_json::json!({
            "kind": "local_track",
            "track_id": 7,
            "file_name": "track.flac",
            "title": "Track",
            "artist": "Artist"
        }))
        .unwrap();

        assert!(!source.is_radio());
        assert_eq!(source.kind(), "local_track");
        assert_eq!(source.local_track_id(), Some(7));
        assert_eq!(source.qobuz_track_id(), None);
    }

    #[test]
    fn local_track_radio_round_trips() {
        let source = SourceRef::LocalTrack {
            track_id: 7,
            file_name: Some("track.flac".to_string()),
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: true,
            radio_context: None,
            playlist_context: None,
        };

        let body = serde_json::to_string(&source).unwrap();
        let restored: SourceRef = serde_json::from_str(&body).unwrap();

        assert!(restored.is_radio());
    }

    #[test]
    fn local_track_radio_context_round_trips() {
        let source = SourceRef::LocalTrack {
            track_id: 7,
            file_name: Some("track.flac".to_string()),
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: true,
            radio_context: Some(RadioContext {
                provider: "lastfm".to_string(),
                anchor: RadioSeedContext {
                    title: Some("Anchor".to_string()),
                    artist: Some("Anchor Artist".to_string()),
                    mbid: None,
                },
                last_seed: RadioSeedContext {
                    title: Some("Seed".to_string()),
                    artist: Some("Seed Artist".to_string()),
                    mbid: None,
                },
                hop: 2,
            }),
            playlist_context: None,
        };

        let body = serde_json::to_string(&source).unwrap();
        let restored: SourceRef = serde_json::from_str(&body).unwrap();

        let SourceRef::LocalTrack { radio_context, .. } = restored else {
            panic!("expected local source");
        };
        let context = radio_context.unwrap();
        assert_eq!(context.provider, "lastfm");
        assert_eq!(context.anchor.title.as_deref(), Some("Anchor"));
        assert_eq!(context.last_seed.artist.as_deref(), Some("Seed Artist"));
        assert_eq!(context.hop, 2);
    }
}
