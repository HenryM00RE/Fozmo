use super::{
    ArtistSummary, Library, LibrarySearchResponse, album_from_row, collect_rows, track_from_row,
};
use rusqlite::{Connection, params};

impl Library {
    #[allow(dead_code)]
    pub fn search(&self, query: &str) -> Result<LibrarySearchResponse, String> {
        let profile_id = self.active_profile_id();
        self.search_for_profile(&profile_id, query)
    }

    pub fn search_for_profile(
        &self,
        profile_id: &str,
        query: &str,
    ) -> Result<LibrarySearchResponse, String> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            let mut tracks: Vec<_> = self
                .tracks_for_profile(profile_id)?
                .into_iter()
                .take(25)
                .collect();
            self.attach_preferred_play_sources(&mut tracks)?;
            return Ok(LibrarySearchResponse {
                query: String::new(),
                albums: self.albums()?.into_iter().take(12).collect(),
                artists: self
                    .artists_for_profile(profile_id)?
                    .into_iter()
                    .take(12)
                    .collect(),
                tracks,
            });
        }

        let needle = SearchNeedle::new(trimmed);
        let fts_query = search_fts_query(trimmed);
        let like = format!("%{}%", trimmed.to_lowercase());
        let conn = self.conn.lock().unwrap();

        let mut albums = search_albums(&conn, fts_query.as_deref(), &like)?;
        albums.sort_by(|a, b| {
            album_search_score(b, &needle)
                .cmp(&album_search_score(a, &needle))
                .then_with(|| album_sort_label(a).cmp(&album_sort_label(b)))
        });
        albums.truncate(30);

        let mut artists = search_artists(&conn, fts_query.as_deref(), &like, profile_id)?;
        artists.sort_by(|a, b| {
            artist_search_score(b, &needle)
                .cmp(&artist_search_score(a, &needle))
                .then_with(|| b.play_count.cmp(&a.play_count))
                .then_with(|| {
                    b.listened_secs
                        .partial_cmp(&a.listened_secs)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| normalize_search_text(&a.name).cmp(&normalize_search_text(&b.name)))
        });
        artists.truncate(30);

        let mut tracks = search_tracks(&conn, fts_query.as_deref(), &like, profile_id, self)?;
        tracks.sort_by(|a, b| {
            track_search_score(b, &needle)
                .cmp(&track_search_score(a, &needle))
                .then_with(|| b.play_count.cmp(&a.play_count))
                .then_with(|| b.last_played_at.cmp(&a.last_played_at))
                .then_with(|| track_sort_label(a).cmp(&track_sort_label(b)))
        });
        tracks.truncate(50);
        drop(conn);
        self.attach_preferred_play_sources(&mut tracks)?;

        Ok(LibrarySearchResponse {
            query: trimmed.to_string(),
            albums,
            artists,
            tracks,
        })
    }

    pub(super) fn attach_preferred_play_sources(
        &self,
        tracks: &mut [super::TrackSummary],
    ) -> Result<(), String> {
        for track in tracks {
            track.preferred_play_source = self.preferred_play_source_for_local_track(track.id)?;
        }
        Ok(())
    }
}

fn search_albums(
    conn: &Connection,
    fts_query: Option<&str>,
    like: &str,
) -> Result<Vec<super::AlbumSummary>, String> {
    if let Some(fts_query) = fts_query {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, title, album_artist, year, track_count, COALESCE(canonical_art_id, art_id), confidence, match_status, primary_version_id,
                       qobuz_album_id, qobuz_match_status, qobuz_match_confidence, canonical_art_id, original_year, mb_barcode,
                       CASE WHEN COALESCE(canonical_art_id, art_id) IS NULL
                                 AND qobuz_album_id IS NOT NULL
                                 AND (qobuz_match_status = 'matched'
                                      OR (qobuz_match_status = 'needs_review'
                                          AND COALESCE(qobuz_match_confidence, 0) >= 80))
                            THEN json_extract(qobuz_payload_json, '$.album.image_url') END
                FROM albums
                WHERE id IN (
                    SELECT album_id FROM albums_fts WHERE albums_fts MATCH ?1
                    UNION
                    SELECT id FROM albums
                    WHERE lower(title) LIKE ?2 OR lower(COALESCE(album_artist, '')) LIKE ?2
                )
                LIMIT 200
                "#,
            )
            .map_err(|e| format!("album search: {e}"))?;
        return collect_rows(
            stmt.query_map(params![fts_query, like], album_from_row)
                .map_err(|e| format!("album search map: {e}"))?,
        );
    }

    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, title, album_artist, year, track_count, COALESCE(canonical_art_id, art_id), confidence, match_status, primary_version_id,
                   qobuz_album_id, qobuz_match_status, qobuz_match_confidence, canonical_art_id, original_year, mb_barcode,
                   CASE WHEN COALESCE(canonical_art_id, art_id) IS NULL
                             AND qobuz_album_id IS NOT NULL
                             AND (qobuz_match_status = 'matched'
                                  OR (qobuz_match_status = 'needs_review'
                                      AND COALESCE(qobuz_match_confidence, 0) >= 80))
                        THEN json_extract(qobuz_payload_json, '$.album.image_url') END
            FROM albums
            WHERE lower(title) LIKE ?1 OR lower(COALESCE(album_artist, '')) LIKE ?1
            LIMIT 200
            "#,
        )
        .map_err(|e| format!("album search: {e}"))?;
    collect_rows(
        stmt.query_map([like], album_from_row)
            .map_err(|e| format!("album search map: {e}"))?,
    )
}

fn search_artists(
    conn: &Connection,
    fts_query: Option<&str>,
    like: &str,
    profile_id: &str,
) -> Result<Vec<ArtistSummary>, String> {
    let candidate_sql = if fts_query.is_some() {
        r#"
        WITH candidate_artists AS (
            SELECT artist_id FROM artists_fts WHERE artists_fts MATCH ?1
            UNION
            SELECT id FROM artists WHERE lower(name) LIKE ?2
        ),
        "#
    } else {
        r#"
        WITH candidate_artists AS (
            SELECT id AS artist_id FROM artists WHERE lower(name) LIKE ?1
        ),
        "#
    };
    let sql = format!(
        r#"
        {candidate_sql}
        track_artist_links AS (
            SELECT lower(artist) AS artist_key, album_id, id
            FROM tracks
            WHERE NULLIF(TRIM(artist), '') IS NOT NULL
            UNION
            SELECT lower(album_artist) AS artist_key, album_id, id
            FROM tracks
            WHERE NULLIF(TRIM(album_artist), '') IS NOT NULL
        ),
        artist_counts AS (
            SELECT artist_key,
                   COUNT(DISTINCT album_id) AS album_count,
                   COUNT(id) AS track_count
            FROM track_artist_links
            GROUP BY artist_key
        ),
        artist_popularity AS (
            SELECT lower(display_artist) AS artist_key,
                   SUM(CASE WHEN counted = 1 THEN 1 ELSE 0 END) AS play_count,
                   SUM(COALESCE(played_secs, 0.0)) AS listened_secs
            FROM (
                SELECT
                    COALESCE(
                        NULLIF(TRIM(t.album_artist), ''),
                        NULLIF(TRIM(albums.album_artist), ''),
                        NULLIF(TRIM(t.artist), ''),
                        NULLIF(TRIM(h.artist), '')
                    ) AS display_artist,
                    h.counted,
                    h.played_secs
                FROM playback_history h
                LEFT JOIN tracks t ON h.source_key GLOB 'local:[0-9]*'
                                  AND t.id = CAST(substr(h.source_key, 7) AS INTEGER)
                LEFT JOIN albums ON albums.id = t.album_id
                WHERE h.profile_id = ?{profile_param}
                  AND h.played_secs IS NOT NULL
                  AND h.played_secs > 0
            )
            WHERE display_artist IS NOT NULL
            GROUP BY lower(display_artist)
        )
        SELECT
            a.name,
            COALESCE(ac.album_count, 0) AS album_count,
            COALESCE(ac.track_count, 0) AS track_count,
            COALESCE(ph.play_count, 0) AS play_count,
            COALESCE(ph.listened_secs, 0.0) AS listened_secs
        FROM artists a
        JOIN candidate_artists ca ON ca.artist_id = a.id
        LEFT JOIN artist_counts ac ON ac.artist_key = lower(a.name)
        LEFT JOIN artist_popularity ph ON ph.artist_key = lower(a.name)
        LIMIT 200
        "#,
        profile_param = if fts_query.is_some() { 3 } else { 2 }
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("artist search: {e}"))?;
    let rows = if let Some(fts_query) = fts_query {
        stmt.query_map(params![fts_query, like, profile_id], artist_from_row)
            .map_err(|e| format!("artist search map: {e}"))?
    } else {
        stmt.query_map(params![like, profile_id], artist_from_row)
            .map_err(|e| format!("artist search map: {e}"))?
    };
    collect_rows(rows)
}

fn search_tracks(
    conn: &Connection,
    fts_query: Option<&str>,
    like: &str,
    profile_id: &str,
    library: &Library,
) -> Result<Vec<super::TrackSummary>, String> {
    if let Some(fts_query) = fts_query {
        let mut stmt = conn
            .prepare(
                library
                    .track_select_sql_for_profile(
                        profile_id,
                        "WHERE t.id IN (
                        SELECT track_id FROM tracks_fts WHERE tracks_fts MATCH ?1
                        UNION
                        SELECT id FROM tracks
                        WHERE lower(title) LIKE ?2
                           OR lower(COALESCE(artist, '')) LIKE ?2
                           OR lower(COALESCE(album, '')) LIKE ?2
                           OR lower(COALESCE(album_artist, '')) LIKE ?2
                           OR lower(COALESCE(composer, '')) LIKE ?2
                           OR lower(COALESCE(genre, '')) LIKE ?2
                           OR lower(file_name) LIKE ?2
                    )
                    LIMIT 250",
                    )
                    .as_str(),
            )
            .map_err(|e| format!("track search: {e}"))?;
        return collect_rows(
            stmt.query_map(params![fts_query, like], track_from_row)
                .map_err(|e| format!("track search map: {e}"))?,
        );
    }

    let mut stmt = conn
        .prepare(
            library
                .track_select_sql_for_profile(
                    profile_id,
                    "WHERE lower(t.title) LIKE ?1
                   OR lower(COALESCE(t.artist, '')) LIKE ?1
                   OR lower(COALESCE(t.album, '')) LIKE ?1
                   OR lower(COALESCE(t.album_artist, '')) LIKE ?1
                   OR lower(COALESCE(t.composer, '')) LIKE ?1
                   OR lower(COALESCE(t.genre, '')) LIKE ?1
                   OR lower(t.file_name) LIKE ?1
                 LIMIT 250",
                )
                .as_str(),
        )
        .map_err(|e| format!("track search: {e}"))?;
    collect_rows(
        stmt.query_map([like], track_from_row)
            .map_err(|e| format!("track search map: {e}"))?,
    )
}

fn artist_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtistSummary> {
    Ok(ArtistSummary {
        name: row.get(0)?,
        album_count: row.get(1)?,
        track_count: row.get(2)?,
        play_count: row.get(3)?,
        listened_secs: row.get(4)?,
    })
}

#[derive(Debug)]
struct SearchNeedle {
    value: String,
    tokens: Vec<String>,
}

impl SearchNeedle {
    fn new(value: &str) -> Self {
        let value = normalize_search_text(value);
        let tokens = value.split_whitespace().map(str::to_string).collect();
        Self { value, tokens }
    }
}

fn album_search_score(album: &super::AlbumSummary, needle: &SearchNeedle) -> i64 {
    let title = field_score(&album.title, needle);
    let artist = album
        .album_artist
        .as_deref()
        .map(|value| field_score(value, needle).min(92))
        .unwrap_or(0);
    title.max(artist)
}

fn artist_search_score(artist: &ArtistSummary, needle: &SearchNeedle) -> i64 {
    let base = field_score(&artist.name, needle);
    base + if base >= 110 { 12 } else { 0 }
}

fn track_search_score(track: &super::TrackSummary, needle: &SearchNeedle) -> i64 {
    let title = field_score(&track.title, needle);
    let artist = track
        .artist
        .as_deref()
        .map(|value| field_score(value, needle).min(94))
        .unwrap_or(0);
    let album = track
        .album
        .as_deref()
        .map(|value| field_score(value, needle).min(84))
        .unwrap_or(0);
    let album_artist = track
        .album_artist
        .as_deref()
        .map(|value| field_score(value, needle).min(76))
        .unwrap_or(0);
    let metadata = [
        track.composer.as_deref(),
        track.genre.as_deref(),
        Some(track.file_name.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(|value| field_score(value, needle).min(42))
    .max()
    .unwrap_or(0);
    title.max(artist).max(album).max(album_artist).max(metadata)
}

fn field_score(value: &str, needle: &SearchNeedle) -> i64 {
    if needle.value.is_empty() {
        return 0;
    }
    let normalized = normalize_search_text(value);
    if normalized.is_empty() {
        return 0;
    }
    if normalized == needle.value {
        return 120;
    }
    if normalized.starts_with(&format!("{} ", needle.value))
        || normalized.starts_with(&needle.value)
    {
        return 104;
    }
    if word_boundary_contains(&normalized, &needle.value) {
        return 88;
    }
    if normalized.contains(&needle.value) {
        return 70;
    }
    if needle.tokens.len() > 1 && needle.tokens.iter().all(|token| normalized.contains(token)) {
        return 48;
    }
    0
}

fn word_boundary_contains(text: &str, query: &str) -> bool {
    format!(" {text} ").contains(&format!(" {query} "))
}

fn album_sort_label(album: &super::AlbumSummary) -> String {
    format!(
        "{} {}",
        normalize_search_text(album.album_artist.as_deref().unwrap_or("")),
        normalize_search_text(&album.title)
    )
}

fn track_sort_label(track: &super::TrackSummary) -> String {
    format!(
        "{} {} {:03} {}",
        normalize_search_text(track.artist.as_deref().unwrap_or("")),
        normalize_search_text(track.album.as_deref().unwrap_or("")),
        track.track_number.unwrap_or(0),
        normalize_search_text(&track.title)
    )
}

fn search_fts_query(value: &str) -> Option<String> {
    let tokens = search_tokens(value);
    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .into_iter()
            .map(|token| format!("{token}*"))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn search_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in value.chars() {
        if c.is_alphanumeric() {
            for lower in c.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn normalize_search_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        push_folded_char(&mut out, c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_folded_char(out: &mut String, c: char) {
    for lower in c.to_lowercase() {
        match lower {
            '&' => out.push_str(" and "),
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
            'ď' | 'đ' | 'ð' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
            'ĥ' | 'ħ' => out.push('h'),
            'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
            'ĵ' => out.push('j'),
            'ķ' => out.push('k'),
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
            'ŕ' | 'ŗ' | 'ř' => out.push('r'),
            'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
            'ß' => out.push_str("ss"),
            'ţ' | 'ť' | 'ŧ' => out.push('t'),
            'þ' => out.push_str("th"),
            'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ŵ' => out.push('w'),
            'ý' | 'ÿ' | 'ŷ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            _ if lower.is_alphanumeric() => out.push(lower),
            _ => out.push(' '),
        }
    }
}
