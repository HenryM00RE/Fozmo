use super::cache::ARTIST_DETAIL_TTL;
use super::model::{
    QobuzAlbum, QobuzArtist, QobuzArtistCore, QobuzArtistDetail, QobuzArtistSimilar,
    QobuzArtistTopTracks, QobuzTrack,
};
use super::parser::{parse_album, parse_artist};
use super::{QobuzService, qobuz_reqwest_error};
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

impl QobuzService {
    /// Fast core: a single Qobuz `artist/get` call. This is what QBZ does and
    /// is the only round-trip needed to paint the hero, bio, and discography.
    /// Cached so repeat visits are instant.
    pub async fn artist_core(&self, artist_id: u64) -> Result<QobuzArtistCore, String> {
        {
            let cache = self.artist_detail_cache.read().await;
            if let Some((stored_at, detail)) = cache.get(&artist_id)
                && stored_at.elapsed() < ARTIST_DETAIL_TTL
            {
                let d = (**detail).clone();
                return Ok(QobuzArtistCore {
                    artist: d.artist,
                    albums: d.albums,
                });
            }
        }

        let (artist, albums) = self.fetch_artist_get(artist_id).await?;
        Ok(QobuzArtistCore { artist, albums })
    }

    pub async fn artist_top_tracks(&self, artist_id: u64) -> Result<QobuzArtistTopTracks, String> {
        if let Some(top_tracks) = self.cached_artist_top_tracks(artist_id).await {
            return Ok(QobuzArtistTopTracks { top_tracks });
        }

        // Cache hit short-circuit.
        {
            let cache = self.artist_detail_cache.read().await;
            if let Some((stored_at, detail)) = cache.get(&artist_id)
                && stored_at.elapsed() < ARTIST_DETAIL_TTL
                && !detail.top_tracks.is_empty()
            {
                return Ok(QobuzArtistTopTracks {
                    top_tracks: detail.top_tracks.clone(),
                });
            }
        }

        // We need the artist's name for the popularity lookup. Use the cached
        // core if we have one, else fetch fresh.
        let core = self.artist_core(artist_id).await?;
        let top_tracks = self.resolve_top_tracks(&core.artist.name).await;
        self.store_artist_top_tracks(artist_id, top_tracks.clone())
            .await;
        Ok(QobuzArtistTopTracks { top_tracks })
    }

    pub async fn artist_similar(&self, artist_id: u64) -> Result<QobuzArtistSimilar, String> {
        {
            let cache = self.artist_detail_cache.read().await;
            if let Some((stored_at, detail)) = cache.get(&artist_id)
                && stored_at.elapsed() < ARTIST_DETAIL_TTL
                && !detail.similar.is_empty()
            {
                return Ok(QobuzArtistSimilar {
                    similar: detail.similar.clone(),
                });
            }
        }

        let tokens = self.ensure_tokens().await?;
        let session = self.session.read().await.clone();
        let auth = session.as_ref().map(|s| s.user_auth_token.as_str());

        let similar = self
            .fetch_similar_artists(artist_id, 20, auth, &tokens.app_id)
            .await
            .unwrap_or_else(|err| {
                eprintln!("qobuz: getSimilarArtists failed for {artist_id}: {err}");
                Vec::new()
            });
        Ok(QobuzArtistSimilar { similar })
    }

    /// Single-shot helper: bundles core + top tracks + similar. Still available
    /// for the cache-warming path, but the frontend should prefer the split
    /// endpoints for progressive loading.
    pub async fn artist_detail(&self, artist_id: u64) -> Result<QobuzArtistDetail, String> {
        // Cache lookup — returns immediately on a recent hit.
        {
            let cache = self.artist_detail_cache.read().await;
            if let Some((stored_at, detail)) = cache.get(&artist_id)
                && stored_at.elapsed() < ARTIST_DETAIL_TTL
            {
                return Ok((**detail).clone());
            }
        }

        let tokens = self.ensure_tokens().await?;
        let session = self.session.read().await.clone();
        let auth = session.as_ref().map(|s| s.user_auth_token.as_str());

        let (artist, albums) = self.fetch_artist_get(artist_id).await?;

        let similar_fut = async {
            self.fetch_similar_artists(artist_id, 20, auth, &tokens.app_id)
                .await
                .unwrap_or_else(|err| {
                    eprintln!("qobuz: getSimilarArtists failed for {artist_id}: {err}");
                    Vec::new()
                })
        };

        let cached_top_tracks = self.cached_artist_top_tracks(artist_id).await;
        let top_tracks_fut = async {
            match cached_top_tracks {
                Some(tracks) => tracks,
                None => self.resolve_top_tracks(&artist.name).await,
            }
        };

        let (top_tracks, similar) = futures_util::future::join(top_tracks_fut, similar_fut).await;

        let detail = QobuzArtistDetail {
            artist,
            top_tracks,
            albums,
            similar,
        };

        self.store_artist_top_tracks(artist_id, detail.top_tracks.clone())
            .await;
        self.store_artist_in_cache(artist_id, &detail).await;
        Ok(detail)
    }

    async fn fetch_artist_get(
        &self,
        artist_id: u64,
    ) -> Result<(QobuzArtist, Vec<QobuzAlbum>), String> {
        let json = self
            .optional_get_value(
                "/artist/get",
                vec![
                    ("artist_id", artist_id.to_string()),
                    (
                        "extra",
                        "albums,playlists,tracks_appears_on,albums_with_last_release,focus"
                            .to_string(),
                    ),
                    ("limit", "50".to_string()),
                    ("offset", "0".to_string()),
                ],
                "Qobuz artist get failed",
                "Qobuz artist get response was not JSON",
                "Qobuz artist get failed",
            )
            .await?;

        let artist = parse_artist(&json)
            .ok_or_else(|| "Qobuz artist response missing required fields".to_string())?;

        let albums: Vec<QobuzAlbum> = json
            .get("albums")
            .and_then(|a| a.get("items"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_album)
            .collect();

        Ok((artist, albums))
    }

    async fn resolve_top_tracks(&self, artist_name: &str) -> Vec<QobuzTrack> {
        match self.top_tracks_via_listenbrainz(artist_name).await {
            Ok(tracks) => tracks,
            Err(err) => {
                eprintln!(
                    "qobuz: listenbrainz top-tracks failed for '{}': {} — falling back to Qobuz artist ranking",
                    artist_name, err
                );
                self.top_tracks_via_qobuz_search(artist_name)
                    .await
                    .unwrap_or_default()
            }
        }
    }

    async fn store_artist_in_cache(&self, artist_id: u64, detail: &QobuzArtistDetail) {
        let mut cache = self.artist_detail_cache.write().await;
        cache.insert(artist_id, (Instant::now(), Arc::new(detail.clone())));
        let cutoff = ARTIST_DETAIL_TTL * 2;
        cache.retain(|_, (t, _)| t.elapsed() < cutoff);
    }

    pub(super) async fn fetch_similar_artists(
        &self,
        artist_id: u64,
        limit: u32,
        auth_token: Option<&str>,
        app_id: &str,
    ) -> Result<Vec<QobuzArtist>, String> {
        let json = self
            .get_value_with_optional_auth(
                "/artist/getSimilarArtists",
                vec![
                    ("artist_id", artist_id.to_string()),
                    ("limit", limit.to_string()),
                    ("offset", "0".to_string()),
                ],
                app_id,
                auth_token,
                "Qobuz similar artists request failed",
                "Qobuz similar artists response was not JSON",
                "Qobuz similar artists failed",
            )
            .await?;

        Ok(json
            .get("artists")
            .and_then(|a| a.get("items"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_artist)
            .collect())
    }

    /// Resolve popular tracks for an artist using ListenBrainz (sister project of
    /// MusicBrainz). We look up the MBID via MusicBrainz, ask ListenBrainz for the
    /// most-listened recordings, then search Qobuz for each title so the UI gets
    /// rich `QobuzTrack`s (cover art, duration, hi-res flag).
    async fn top_tracks_via_listenbrainz(
        &self,
        artist_name: &str,
    ) -> Result<Vec<QobuzTrack>, String> {
        // 1. MusicBrainz artist search → MBID
        let mb_url = "https://musicbrainz.org/ws/2/artist/";
        let mb_resp: Value = self
            .http
            .get(mb_url)
            .header(
                reqwest::header::USER_AGENT,
                crate::app::identity::USER_AGENT,
            )
            .query(&[
                ("query", format!("artist:\"{}\"", artist_name)),
                ("fmt", "json".to_string()),
                ("limit", "5".to_string()),
            ])
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("musicbrainz artist search failed", e))?
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error("musicbrainz artist response was not JSON", e))?;

        let mbid = mb_resp
            .get("artists")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|a| a.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("no MBID found for '{}'", artist_name))?
            .to_string();

        // 2. ListenBrainz sitewide popularity for this artist. The right endpoint
        // is `/popularity/top-recordings-for-artist/{mbid}` — `/stats/artist/...`
        // is user-scoped and returns nothing for unauthenticated calls.
        let lb_url = format!(
            "https://api.listenbrainz.org/1/popularity/top-recordings-for-artist/{}",
            mbid
        );
        let lb_resp: Result<Value, String> = async {
            self.http
                .get(&lb_url)
                .send()
                .await
                .map_err(|e| qobuz_reqwest_error("listenbrainz popularity request failed", e))?
                .error_for_status()
                .map_err(|e| qobuz_reqwest_error("listenbrainz popularity request failed", e))?
                .json()
                .await
                .map_err(|e| qobuz_reqwest_error("listenbrainz response was not JSON", e))
        }
        .await;

        // The popularity endpoint returns either a top-level array or
        // { "payload": [...] } depending on version. Handle both.
        let titles = match lb_resp {
            Ok(response) => listenbrainz_popular_titles(&response),
            Err(error) => {
                eprintln!(
                    "qobuz: listenbrainz popularity unavailable for {artist_name}: {error}; trying artist radio ranking"
                );
                Vec::new()
            }
        };

        let titles = if titles.is_empty() {
            self.top_track_titles_via_listenbrainz_radio(&mbid).await?
        } else {
            titles
        };

        // 3. Resolve each popular title to a Qobuz track. We do this with
        // parallel per-title searches because Qobuz's broad artist search
        // returns only the top 25 results — popular catalog tracks on
        // ListenBrainz aren't always in that window. This is more HTTP calls
        // but they run concurrently and (since top-tracks is loaded in the
        // background by the frontend) don't block the artist page paint.
        let artist_norm = normalize_top_track_text(artist_name);

        let lookups = titles.iter().map(|title| {
            let title_norm = normalize_top_track_text(title);
            let artist_norm = artist_norm.clone();
            let query = format!("{} {}", artist_name, title);
            async move {
                let res = self.search_tracks(&query).await.ok()?;
                res.tracks.into_iter().find(|t| {
                    let a = normalize_top_track_text(&t.artist);
                    let artist_match = a == artist_norm;
                    let tn = normalize_top_track_text(&t.title);
                    let title_match =
                        tn == title_norm || tn.contains(&title_norm) || title_norm.contains(&tn);
                    artist_match && title_match
                })
            }
        });
        let resolved: Vec<Option<QobuzTrack>> = futures_util::future::join_all(lookups).await;

        let mut seen = std::collections::HashSet::new();
        let tracks: Vec<QobuzTrack> = resolved
            .into_iter()
            .flatten()
            .filter(|t| seen.insert(t.id))
            .take(9)
            .collect();

        if tracks.is_empty() {
            return Err("none of the listenbrainz top titles matched on Qobuz".to_string());
        }
        Ok(tracks)
    }

    async fn top_track_titles_via_listenbrainz_radio(
        &self,
        artist_mbid: &str,
    ) -> Result<Vec<String>, String> {
        let url = format!("https://api.listenbrainz.org/1/lb-radio/artist/{artist_mbid}");
        let radio: Value = self
            .http
            .get(url)
            .query(&[
                ("mode", "easy"),
                ("max_similar_artists", "0"),
                ("max_recordings_per_artist", "100"),
                ("pop_begin", "0"),
                ("pop_end", "100"),
            ])
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("listenbrainz artist radio request failed", e))?
            .error_for_status()
            .map_err(|e| qobuz_reqwest_error("listenbrainz artist radio request failed", e))?
            .json()
            .await
            .map_err(|e| {
                qobuz_reqwest_error("listenbrainz artist radio response was not JSON", e)
            })?;
        let recording_mbids = ranked_listenbrainz_recording_mbids(&radio, artist_mbid, 20);
        if recording_mbids.is_empty() {
            return Err("listenbrainz artist radio returned no recordings".to_string());
        }

        let metadata: Value = self
            .http
            .get("https://api.listenbrainz.org/1/metadata/recording/")
            .query(&[
                ("recording_mbids", recording_mbids.join(",")),
                ("inc", "artist release".to_string()),
            ])
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("listenbrainz recording metadata request failed", e))?
            .error_for_status()
            .map_err(|e| qobuz_reqwest_error("listenbrainz recording metadata request failed", e))?
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error("listenbrainz recording metadata was not JSON", e))?;
        let titles = listenbrainz_metadata_titles(&metadata, &recording_mbids, 12);
        if titles.is_empty() {
            return Err("listenbrainz recording metadata returned no titles".to_string());
        }
        Ok(titles)
    }

    async fn top_tracks_via_qobuz_search(
        &self,
        artist_name: &str,
    ) -> Result<Vec<QobuzTrack>, String> {
        let artist_norm = normalize_top_track_text(artist_name);
        let mut seen_titles = std::collections::HashSet::new();
        Ok(self
            .search_tracks(artist_name)
            .await?
            .tracks
            .into_iter()
            .filter(|track| normalize_top_track_text(&track.artist) == artist_norm)
            .filter(|track| seen_titles.insert(normalize_top_track_text(&track.title)))
            .take(9)
            .collect())
    }
}

fn normalize_top_track_text(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn listenbrainz_popular_titles(response: &Value) -> Vec<String> {
    response
        .as_array()
        .or_else(|| response.get("payload").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|recording| {
            recording
                .get("recording_name")
                .or_else(|| recording.get("track_name"))
                .or_else(|| recording.get("title"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .take(12)
        .collect()
}

fn ranked_listenbrainz_recording_mbids(
    response: &Value,
    artist_mbid: &str,
    limit: usize,
) -> Vec<String> {
    let mut recordings = response
        .get(artist_mbid)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    recordings.sort_by_key(|recording| {
        std::cmp::Reverse(
            recording
                .get("total_listen_count")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
    });
    recordings
        .into_iter()
        .filter_map(|recording| {
            recording
                .get("recording_mbid")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .take(limit)
        .collect()
}

fn listenbrainz_metadata_titles(
    metadata: &Value,
    ranked_mbids: &[String],
    limit: usize,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ranked_mbids
        .iter()
        .filter_map(|mbid| {
            metadata
                .get(mbid)
                .and_then(|item| item.get("recording"))
                .and_then(|recording| recording.get("name"))
                .and_then(Value::as_str)
        })
        .filter(|title| seen.insert(normalize_top_track_text(title)))
        .map(str::to_string)
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listenbrainz_radio_candidates_are_ranked_by_global_listens() {
        let artist_mbid = "radiohead-mbid";
        let response = json!({
            (artist_mbid): [
                { "recording_mbid": "album-cut", "total_listen_count": 5_000 },
                { "recording_mbid": "creep", "total_listen_count": 2_700_000 },
                { "recording_mbid": "karma-police", "total_listen_count": 1_400_000 }
            ]
        });

        assert_eq!(
            ranked_listenbrainz_recording_mbids(&response, artist_mbid, 9),
            vec!["creep", "karma-police", "album-cut"]
        );
    }

    #[test]
    fn listenbrainz_metadata_titles_preserve_popularity_order_and_dedupe() {
        let ranked = vec![
            "creep-main".to_string(),
            "karma-police".to_string(),
            "creep-alt".to_string(),
        ];
        let metadata = json!({
            "creep-main": { "recording": { "name": "Creep" } },
            "karma-police": { "recording": { "name": "Karma Police" } },
            "creep-alt": { "recording": { "name": "Creep" } }
        });

        assert_eq!(
            listenbrainz_metadata_titles(&metadata, &ranked, 9),
            vec!["Creep", "Karma Police"]
        );
    }

    #[test]
    fn popularity_titles_accept_the_documented_array_shape() {
        let response = json!([
            { "recording_name": "Creep" },
            { "recording_name": "No Surprises" }
        ]);
        assert_eq!(
            listenbrainz_popular_titles(&response),
            vec!["Creep", "No Surprises"]
        );
    }
}
