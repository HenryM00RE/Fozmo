use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const API_ROOT: &str = "https://ws.audioscrobbler.com/2.0/";
const USER_AGENT: &str = crate::app::identity::USER_AGENT;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SEED_TITLE_BYTES: usize = 512;
const MAX_SEED_ARTIST_BYTES: usize = 512;
const MAX_SEED_MBID_BYTES: usize = 128;
const LASTFM_REQUEST_FAILED: &str = "Last.fm similar tracks request failed";
const LASTFM_NON_JSON_RESPONSE: &str = "Last.fm returned a non-JSON response";

#[derive(Clone)]
pub struct LastFmService {
    http: Client,
    api_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastFmSeed {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub mbid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LastFmTrack {
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub mbid: Option<String>,
    #[serde(default)]
    pub artist_mbid: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub match_score: Option<f64>,
    #[serde(default)]
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LastFmSimilarTracksResponse {
    pub seed: LastFmSeed,
    pub tracks: Vec<LastFmTrack>,
}

impl LastFmService {
    pub fn new() -> Result<Self, String> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("create Last.fm HTTP client: {e}"))?;
        Ok(Self {
            http,
            api_root: API_ROOT.to_string(),
        })
    }

    pub async fn similar_tracks(
        &self,
        api_key: &str,
        seed: &LastFmSeed,
        limit: u32,
    ) -> Result<LastFmSimilarTracksResponse, String> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err("Last.fm API key is not configured".to_string());
        }
        let seed = seed.normalized()?;
        let response = self.similar_tracks_once(api_key, &seed, limit).await?;
        if !response.tracks.is_empty() {
            return Ok(response);
        }

        let Some(fallback_seed) = seed.without_trailing_feature_credit() else {
            return Ok(response);
        };
        match self
            .similar_tracks_once(api_key, &fallback_seed, limit)
            .await
        {
            Ok(fallback) if !fallback.tracks.is_empty() => Ok(fallback),
            _ => Ok(response),
        }
    }

    async fn similar_tracks_once(
        &self,
        api_key: &str,
        seed: &LastFmSeed,
        limit: u32,
    ) -> Result<LastFmSimilarTracksResponse, String> {
        let mut query = vec![
            ("method", "track.getsimilar".to_string()),
            ("api_key", api_key.to_string()),
            ("format", "json".to_string()),
            ("autocorrect", "1".to_string()),
            ("limit", limit.clamp(1, 50).to_string()),
        ];
        if let Some(mbid) = seed.mbid.as_deref() {
            query.push(("mbid", mbid.to_string()));
        } else {
            query.push(("track", seed.title.clone().unwrap_or_default()));
            query.push(("artist", seed.artist.clone().unwrap_or_default()));
        }

        let response = self
            .http
            .get(&self.api_root)
            .query(&query)
            .send()
            .await
            .map_err(|_| LASTFM_REQUEST_FAILED.to_string())?;
        let status = response.status();
        let json: Value = response
            .json()
            .await
            .map_err(|_| LASTFM_NON_JSON_RESPONSE.to_string())?;

        if let Some(error) = parse_lastfm_error_json(&json) {
            return Err(redact_api_key(&error, api_key));
        }
        if !status.is_success() {
            return Err(format!("Last.fm similar tracks returned HTTP {status}"));
        }

        let tracks = parse_similar_tracks_json(&json)?;
        Ok(LastFmSimilarTracksResponse {
            seed: seed.clone(),
            tracks,
        })
    }
}

impl LastFmSeed {
    pub fn normalized(&self) -> Result<Self, String> {
        let title = bounded_seed_field(self.title.as_deref(), "title", MAX_SEED_TITLE_BYTES)?;
        let artist = bounded_seed_field(self.artist.as_deref(), "artist", MAX_SEED_ARTIST_BYTES)?;
        let mbid = bounded_seed_field(self.mbid.as_deref(), "MBID", MAX_SEED_MBID_BYTES)?;
        if mbid.is_none() && (title.is_none() || artist.is_none()) {
            return Err("Last.fm seed requires title and artist, or a track MBID".to_string());
        }
        Ok(Self {
            title,
            artist,
            mbid,
        })
    }

    fn without_trailing_feature_credit(&self) -> Option<Self> {
        if self.mbid.is_some() {
            return None;
        }
        let title = title_without_trailing_feature_credit(self.title.as_deref()?)?;
        Some(Self {
            title: Some(title),
            artist: self.artist.clone(),
            mbid: None,
        })
    }
}

fn bounded_seed_field(
    value: Option<&str>,
    name: &str,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if value.is_some_and(|value| value.len() > max_bytes) {
        return Err(format!(
            "Last.fm seed {name} exceeds the {max_bytes} byte limit"
        ));
    }
    Ok(value.map(str::to_string))
}

fn redact_api_key(message: &str, api_key: &str) -> String {
    if api_key.is_empty() || !message.contains(api_key) {
        return message.to_string();
    }
    message.replace(api_key, "[redacted]")
}

fn title_without_trailing_feature_credit(title: &str) -> Option<String> {
    let title = title.trim();
    let (open, close) = match title.chars().last()? {
        ')' => ('(', ')'),
        ']' => ('[', ']'),
        '}' => ('{', '}'),
        _ => return None,
    };
    let open_index = title.rfind(open)?;
    let credit = title[open_index + open.len_utf8()..title.len() - close.len_utf8()]
        .trim()
        .to_ascii_lowercase();
    let is_feature_credit = ["feat ", "feat. ", "featuring ", "ft ", "ft. "]
        .iter()
        .any(|prefix| credit.starts_with(prefix));
    if !is_feature_credit {
        return None;
    }
    let base_title = title[..open_index].trim();
    (!base_title.is_empty()).then(|| base_title.to_string())
}

pub(crate) fn parse_lastfm_error_json(value: &Value) -> Option<String> {
    let code = value.get("error")?;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Last.fm request failed");
    Some(match code.as_i64() {
        Some(code) => format!("{message} (Last.fm error {code})"),
        None => message.to_string(),
    })
}

pub(crate) fn parse_similar_tracks_json(value: &Value) -> Result<Vec<LastFmTrack>, String> {
    let similar = value
        .get("similartracks")
        .ok_or_else(|| "Last.fm response missing similartracks".to_string())?;
    let Some(track_value) = similar.get("track") else {
        return Ok(Vec::new());
    };
    let tracks = match track_value {
        Value::Array(items) => items.iter().filter_map(parse_lastfm_track).collect(),
        Value::Object(_) => parse_lastfm_track(track_value).into_iter().collect(),
        _ => Vec::new(),
    };
    Ok(tracks)
}

fn parse_lastfm_track(value: &Value) -> Option<LastFmTrack> {
    let title = string_field(value, "name")?;
    let artist_value = value.get("artist")?;
    let artist = artist_value
        .as_str()
        .map(str::to_string)
        .or_else(|| string_field(artist_value, "name"))?;
    Some(LastFmTrack {
        title,
        artist,
        mbid: string_field(value, "mbid"),
        artist_mbid: string_field(artist_value, "mbid"),
        url: string_field(value, "url"),
        match_score: numeric_value(value.get("match")),
        image_url: best_image_url(value.get("image")),
    })
}

fn best_image_url(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_array)?
        .iter()
        .rev()
        .filter_map(|image| string_field(image, "#text"))
        .find(|url| !url.trim().is_empty())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    trimmed_non_empty(value.get(key)?.as_str())
}

fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn numeric_value(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    const SENTINEL_API_KEY: &str = "SECRET_LASTFM_API_KEY_12345";

    fn test_service(api_root: String) -> LastFmService {
        LastFmService {
            http: Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            api_root,
        }
    }

    fn test_seed() -> LastFmSeed {
        LastFmSeed {
            title: Some("Believe".to_string()),
            artist: Some("Cher".to_string()),
            mbid: None,
        }
    }

    async fn one_response_server(content_type: &str, body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let content_type = content_type.to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{address}/2.0/")
    }

    #[test]
    fn lastfm_seed_accepts_title_artist() {
        let seed = LastFmSeed {
            title: Some("  Believe ".to_string()),
            artist: Some(" Cher ".to_string()),
            mbid: None,
        }
        .normalized()
        .unwrap();

        assert_eq!(seed.title.as_deref(), Some("Believe"));
        assert_eq!(seed.artist.as_deref(), Some("Cher"));
        assert_eq!(seed.mbid, None);
    }

    #[test]
    fn lastfm_seed_requires_track_identity() {
        let seed = LastFmSeed {
            title: Some("Believe".to_string()),
            artist: None,
            mbid: None,
        };

        assert!(seed.normalized().is_err());
    }

    #[test]
    fn lastfm_seed_accepts_mbid_without_title_artist() {
        let seed = LastFmSeed {
            title: None,
            artist: None,
            mbid: Some(" abc ".to_string()),
        }
        .normalized()
        .unwrap();

        assert_eq!(seed.mbid.as_deref(), Some("abc"));
    }

    #[test]
    fn lastfm_seed_falls_back_from_trailing_feature_credit() {
        let seed = LastFmSeed {
            title: Some("Demon Days (featuring the London Community Gospel Choir)".to_string()),
            artist: Some("Gorillaz".to_string()),
            mbid: None,
        };

        let fallback = seed.without_trailing_feature_credit().unwrap();

        assert_eq!(fallback.title.as_deref(), Some("Demon Days"));
        assert_eq!(fallback.artist.as_deref(), Some("Gorillaz"));
    }

    #[test]
    fn lastfm_seed_preserves_non_feature_brackets() {
        let seed = LastFmSeed {
            title: Some("Song (Live at Wembley)".to_string()),
            artist: Some("Artist".to_string()),
            mbid: None,
        };

        assert!(seed.without_trailing_feature_credit().is_none());
    }

    #[test]
    fn lastfm_seed_with_mbid_does_not_use_title_fallback() {
        let seed = LastFmSeed {
            title: Some("Song [feat. Guest]".to_string()),
            artist: Some("Artist".to_string()),
            mbid: Some("track-mbid".to_string()),
        };

        assert!(seed.without_trailing_feature_credit().is_none());
    }

    #[test]
    fn lastfm_seed_fields_are_bounded() {
        for seed in [
            LastFmSeed {
                title: Some("t".repeat(MAX_SEED_TITLE_BYTES + 1)),
                artist: Some("Artist".to_string()),
                mbid: None,
            },
            LastFmSeed {
                title: Some("Title".to_string()),
                artist: Some("a".repeat(MAX_SEED_ARTIST_BYTES + 1)),
                mbid: None,
            },
            LastFmSeed {
                title: None,
                artist: None,
                mbid: Some("m".repeat(MAX_SEED_MBID_BYTES + 1)),
            },
        ] {
            let error = seed.normalized().unwrap_err();
            assert!(error.contains("exceeds"));
            assert!(error.contains("byte limit"));
        }
    }

    #[tokio::test]
    async fn lastfm_send_errors_do_not_expose_api_key_or_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let service = test_service(format!("http://{address}/2.0/"));

        let error = service
            .similar_tracks(SENTINEL_API_KEY, &test_seed(), 10)
            .await
            .unwrap_err();

        assert_eq!(error, LASTFM_REQUEST_FAILED);
        assert!(!error.contains(SENTINEL_API_KEY));
        assert!(!error.contains("api_key="));
        assert!(!error.contains("http://"));
    }

    #[tokio::test]
    async fn lastfm_decode_errors_do_not_expose_api_key_or_url() {
        let api_root = one_response_server("text/plain", "not json".to_string()).await;
        let service = test_service(api_root);

        let error = service
            .similar_tracks(SENTINEL_API_KEY, &test_seed(), 10)
            .await
            .unwrap_err();

        assert_eq!(error, LASTFM_NON_JSON_RESPONSE);
        assert!(!error.contains(SENTINEL_API_KEY));
        assert!(!error.contains("api_key="));
        assert!(!error.contains("http://"));
    }

    #[tokio::test]
    async fn lastfm_json_errors_redact_an_echoed_api_key() {
        let body = json!({
            "error": 10,
            "message": format!("Invalid API key {SENTINEL_API_KEY}")
        })
        .to_string();
        let api_root = one_response_server("application/json", body).await;
        let service = test_service(api_root);

        let error = service
            .similar_tracks(SENTINEL_API_KEY, &test_seed(), 10)
            .await
            .unwrap_err();

        assert_eq!(error, "Invalid API key [redacted] (Last.fm error 10)");
        assert!(!error.contains(SENTINEL_API_KEY));
    }

    #[test]
    fn parses_lastfm_error_json() {
        let error = parse_lastfm_error_json(&json!({
            "error": 10,
            "message": "Invalid API key"
        }))
        .unwrap();

        assert_eq!(error, "Invalid API key (Last.fm error 10)");
    }

    #[test]
    fn parses_similar_track_response() {
        let tracks = parse_similar_tracks_json(&json!({
            "similartracks": {
                "track": [{
                    "name": "Ray of Light",
                    "mbid": "",
                    "match": "10.95",
                    "url": "https://www.last.fm/music/Madonna/_/Ray+of+Light",
                    "artist": {
                        "name": "Madonna",
                        "mbid": "79239441-bfd5-4981-a70c-55c3f15c1287"
                    },
                    "image": [
                        { "#text": "small.jpg", "size": "small" },
                        { "#text": "large.jpg", "size": "large" }
                    ]
                }]
            }
        }))
        .unwrap();

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "Ray of Light");
        assert_eq!(tracks[0].artist, "Madonna");
        assert_eq!(
            tracks[0].artist_mbid.as_deref(),
            Some("79239441-bfd5-4981-a70c-55c3f15c1287")
        );
        assert_eq!(tracks[0].match_score, Some(10.95));
        assert_eq!(tracks[0].image_url.as_deref(), Some("large.jpg"));
    }
}
