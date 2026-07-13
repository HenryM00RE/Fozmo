use super::auth::{auth_headers, build_url};
use super::model::{QobuzRadioRecommendation, QobuzTrack};
use super::parser::{
    merge_qobuz_track_detail, parse_track, radio_artist_candidates_from_search, radio_suggest_body,
    radio_track_items,
};
use super::{QobuzService, qobuz_reqwest_error};
use serde_json::Value;
use std::collections::HashSet;

impl QobuzService {
    pub async fn radio_next(
        &self,
        seed_track_id: u64,
        exclude_track_ids: &[u64],
        limit: u32,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(|| "Log in to Qobuz before using Radio".to_string())?;
        let tokens = self.ensure_tokens().await?;
        let limit = limit.clamp(1, 500);
        let mut excludes = exclude_track_ids.to_vec();
        if !excludes.contains(&seed_track_id) {
            excludes.push(seed_track_id);
        }

        let mut errors = Vec::new();

        match self
            .radio_track_next(
                seed_track_id,
                &excludes,
                limit,
                &tokens.app_id,
                &session.user_auth_token,
            )
            .await
        {
            Ok(Some(next)) => {
                eprintln!(
                    "qobuz: radio /radio/track next for {seed_track_id} -> {} - {}",
                    next.track.artist, next.track.title
                );
                return Ok(Some(next));
            }
            Ok(None) => eprintln!(
                "qobuz: /radio/track returned no playable recommendation for {seed_track_id}"
            ),
            Err(err) => {
                eprintln!("qobuz: /radio/track failed for {seed_track_id}: {err}");
                errors.push(format!("/radio/track: {err}"));
            }
        }

        match self
            .dynamic_radio_next(
                seed_track_id,
                &excludes,
                limit,
                &tokens.app_id,
                &session.user_auth_token,
            )
            .await
        {
            Ok(Some(next)) => {
                eprintln!(
                    "qobuz: radio dynamic/suggest next for {seed_track_id} -> {} - {}",
                    next.track.artist, next.track.title
                );
                return Ok(Some(next));
            }
            Ok(None) => eprintln!(
                "qobuz: dynamic/suggest returned no playable recommendation for {seed_track_id}"
            ),
            Err(err) => {
                eprintln!("qobuz: dynamic/suggest failed for {seed_track_id}: {err}");
                errors.push(format!("dynamic/suggest: {err}"));
            }
        }

        match self
            .artist_pool_radio_next(
                seed_track_id,
                &excludes,
                limit,
                &tokens.app_id,
                &session.user_auth_token,
            )
            .await
        {
            Ok(Some(next)) => {
                eprintln!(
                    "qobuz: radio artist fallback next for {seed_track_id} -> {} - {}",
                    next.track.artist, next.track.title
                );
                return Ok(Some(next));
            }
            Ok(None) => eprintln!(
                "qobuz: artist fallback returned no playable recommendation for {seed_track_id}"
            ),
            Err(err) => {
                eprintln!("qobuz: artist fallback failed for {seed_track_id}: {err}");
                errors.push(format!("artist fallback: {err}"));
            }
        }

        if errors.is_empty() {
            eprintln!("qobuz: radio returned no playable recommendation for {seed_track_id}");
        } else {
            eprintln!(
                "qobuz: radio returned no playable recommendation for {seed_track_id} ({})",
                errors.join("; ")
            );
        }
        Ok(None)
    }

    pub async fn radio_next_for_artist_name(
        &self,
        seed_artist_name: &str,
        exclude_track_ids: &[u64],
        limit: u32,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let seed_artist_name = seed_artist_name.trim();
        if seed_artist_name.is_empty() {
            return Ok(None);
        }
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(|| "Log in to Qobuz before using Radio".to_string())?;
        let tokens = self.ensure_tokens().await?;
        let limit = limit.clamp(1, 500);
        let search = self.search_artists(seed_artist_name, 5).await?;
        let artist_ids = radio_artist_candidates_from_search(seed_artist_name, &search.artists);

        if artist_ids.is_empty() {
            eprintln!("qobuz: radio found no artist match for {seed_artist_name}");
            return Ok(None);
        }

        self.artist_pool_radio_next_for_artists(
            artist_ids,
            exclude_track_ids,
            limit,
            &tokens.app_id,
            &session.user_auth_token,
        )
        .await
    }

    async fn radio_track_next(
        &self,
        seed_track_id: u64,
        exclude_track_ids: &[u64],
        limit: u32,
        app_id: &str,
        auth_token: &str,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let json = self
            .radio_get_json(
                "/radio/track",
                &[("track_id", seed_track_id.to_string())],
                app_id,
                auth_token,
            )
            .await?;
        self.radio_recommendation_from_response(
            &json,
            exclude_track_ids,
            Some("qobuz-radio-track"),
            limit,
        )
        .await
    }

    async fn radio_artist_next(
        &self,
        artist_id: u64,
        exclude_track_ids: &[u64],
        limit: u32,
        app_id: &str,
        auth_token: &str,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let json = self
            .radio_get_json(
                "/radio/artist",
                &[("artist_id", artist_id.to_string())],
                app_id,
                auth_token,
            )
            .await?;
        self.radio_recommendation_from_response(
            &json,
            exclude_track_ids,
            Some("qobuz-radio-artist"),
            limit,
        )
        .await
    }

    async fn dynamic_radio_next(
        &self,
        seed_track_id: u64,
        exclude_track_ids: &[u64],
        limit: u32,
        app_id: &str,
        auth_token: &str,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let body = radio_suggest_body(seed_track_id, exclude_track_ids, limit);
        let json: Value = self
            .http
            .post(build_url("/dynamic/suggest"))
            .headers(auth_headers(app_id, auth_token)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz radio request failed", e))?
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error("Qobuz radio response was not JSON", e))?;

        if json.get("status").and_then(Value::as_str) == Some("error") {
            let message = json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Qobuz radio failed");
            return Err(crate::diagnostics::logging::sanitize_error(message));
        }

        self.radio_recommendation_from_response(
            &json,
            exclude_track_ids,
            Some("dynamic-suggest"),
            limit,
        )
        .await
    }

    async fn artist_pool_radio_next(
        &self,
        seed_track_id: u64,
        exclude_track_ids: &[u64],
        limit: u32,
        app_id: &str,
        auth_token: &str,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let seed_track = self.track_detail(seed_track_id).await?;
        let mut artist_ids = Vec::new();
        if let Some(artist_id) = seed_track.artist_id {
            artist_ids.push((artist_id, seed_track.artist.clone()));
        }
        if artist_ids.is_empty() && !seed_track.artist.trim().is_empty() {
            let search = self.search_artists(&seed_track.artist, 5).await?;
            if let Some(artist) = search
                .artists
                .iter()
                .find(|artist| artist.name.eq_ignore_ascii_case(&seed_track.artist))
                .or_else(|| search.artists.first())
            {
                artist_ids.push((artist.id, artist.name.clone()));
            }
        }

        self.artist_pool_radio_next_for_artists(
            artist_ids,
            exclude_track_ids,
            limit,
            app_id,
            auth_token,
        )
        .await
    }

    async fn artist_pool_radio_next_for_artists(
        &self,
        artist_ids: Vec<(u64, String)>,
        exclude_track_ids: &[u64],
        limit: u32,
        app_id: &str,
        auth_token: &str,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let limit = limit.clamp(1, 500);
        for (artist_id, artist_name) in artist_ids {
            match self
                .radio_artist_next(artist_id, exclude_track_ids, limit, app_id, auth_token)
                .await
            {
                Ok(Some(next)) => return Ok(Some(next)),
                Ok(None) => {}
                Err(err) => eprintln!("qobuz: /radio/artist({artist_id}) failed: {err}"),
            }

            let artist_tracks = self
                .fetch_artist_tracks(artist_id, limit.max(30), Some(auth_token), app_id)
                .await
                .unwrap_or_else(|err| {
                    eprintln!("qobuz: artist/get tracks failed for {artist_id}: {err}");
                    Vec::new()
                });
            if let Some(track) = self
                .first_playable_radio_track(artist_tracks, exclude_track_ids, limit)
                .await
            {
                return Ok(Some(QobuzRadioRecommendation {
                    track,
                    algorithm: Some("artist-tracks".to_string()),
                }));
            }

            let similar = self
                .fetch_similar_artists(artist_id, 8, Some(auth_token), app_id)
                .await
                .unwrap_or_else(|err| {
                    eprintln!(
                        "qobuz: similar artists failed for {artist_name} ({artist_id}): {err}"
                    );
                    Vec::new()
                });

            let mut seen_artist_ids = HashSet::new();
            for artist in similar {
                if artist.id == 0 || artist.id == artist_id || !seen_artist_ids.insert(artist.id) {
                    continue;
                }
                let tracks = self
                    .fetch_artist_tracks(artist.id, 30, Some(auth_token), app_id)
                    .await
                    .unwrap_or_else(|err| {
                        eprintln!(
                            "qobuz: similar artist tracks failed for {}: {err}",
                            artist.id
                        );
                        Vec::new()
                    });
                if let Some(track) = self
                    .first_playable_radio_track(tracks, exclude_track_ids, limit)
                    .await
                {
                    return Ok(Some(QobuzRadioRecommendation {
                        track,
                        algorithm: Some("similar-artist-tracks".to_string()),
                    }));
                }
            }
        }

        Ok(None)
    }

    async fn radio_get_json(
        &self,
        path: &str,
        query: &[(&str, String)],
        app_id: &str,
        auth_token: &str,
    ) -> Result<Value, String> {
        let response = self
            .http
            .get(build_url(path))
            .headers(auth_headers(app_id, auth_token)?)
            .query(query)
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error(&format!("Qobuz {path} request failed"), e))?;
        let status = response.status();
        let json: Value = response
            .json()
            .await
            .map_err(|e| qobuz_reqwest_error(&format!("Qobuz {path} response was not JSON"), e))?;

        if !status.is_success() || json.get("status").and_then(Value::as_str) == Some("error") {
            let message = json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Qobuz radio failed");
            return Err(format!(
                "{} (HTTP {status})",
                crate::diagnostics::logging::sanitize_error(message)
            ));
        }

        Ok(json)
    }

    async fn radio_recommendation_from_response(
        &self,
        response: &Value,
        exclude_track_ids: &[u64],
        fallback_algorithm: Option<&str>,
        limit: u32,
    ) -> Result<Option<QobuzRadioRecommendation>, String> {
        let algorithm = response
            .get("algorithm")
            .and_then(Value::as_str)
            .or(fallback_algorithm)
            .map(str::to_string);
        let tracks: Vec<QobuzTrack> = radio_track_items(response)
            .into_iter()
            .filter_map(parse_track)
            .collect();
        Ok(self
            .first_playable_radio_track(tracks, exclude_track_ids, limit)
            .await
            .map(|track| QobuzRadioRecommendation { track, algorithm }))
    }

    async fn first_playable_radio_track(
        &self,
        tracks: Vec<QobuzTrack>,
        exclude_track_ids: &[u64],
        limit: u32,
    ) -> Option<QobuzTrack> {
        let excluded: HashSet<u64> = exclude_track_ids.iter().copied().collect();
        let mut seen = HashSet::new();
        let mut fallback = None;

        for track in tracks.into_iter().take(limit.clamp(1, 500) as usize) {
            if track.id == 0
                || !track.streamable
                || excluded.contains(&track.id)
                || !seen.insert(track.id)
            {
                continue;
            }

            match self.track_detail(track.id).await {
                Ok(enriched) if enriched.streamable => {
                    return Some(merge_qobuz_track_detail(track, enriched));
                }
                Ok(_) => {}
                Err(err) => {
                    eprintln!(
                        "qobuz: track/get({}) radio enrichment failed: {err}",
                        track.id
                    );
                    if fallback.is_none() {
                        fallback = Some(track);
                    }
                }
            }
        }

        fallback
    }

    async fn fetch_artist_tracks(
        &self,
        artist_id: u64,
        limit: u32,
        auth_token: Option<&str>,
        app_id: &str,
    ) -> Result<Vec<QobuzTrack>, String> {
        let json = self
            .get_value_with_optional_auth(
                "/artist/get",
                vec![
                    ("artist_id", artist_id.to_string()),
                    ("extra", "tracks".to_string()),
                    ("limit", limit.clamp(1, 500).to_string()),
                    ("offset", "0".to_string()),
                ],
                app_id,
                auth_token,
                "Qobuz artist tracks request failed",
                "Qobuz artist tracks response was not JSON",
                "Qobuz artist tracks failed",
            )
            .await?;

        Ok(json
            .get("tracks")
            .and_then(|tracks| tracks.get("items"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_track)
            .collect())
    }
}
