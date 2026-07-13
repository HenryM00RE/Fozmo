use super::{
    AlbumSummary, ArtistSummary, Library, LibraryBrowseFacets, LibraryBrowsePage,
    LibraryBrowseQuery, LibraryFacetOption, TrackSummary, album_from_row, collect_rows,
};
use rusqlite::{Connection, params_from_iter, types::Value};
use std::collections::HashMap;

const DEFAULT_LIMIT: i64 = 48;
const MAX_LIMIT: i64 = 200;
const MAX_ALBUM_LIMIT: i64 = 10_000;

impl Library {
    #[allow(dead_code)]
    pub fn browse_albums(
        &self,
        query: LibraryBrowseQuery,
    ) -> Result<LibraryBrowsePage<AlbumSummary>, String> {
        let query = normalize_query(query, DEFAULT_LIMIT, MAX_ALBUM_LIMIT);
        let profile_id = self.active_profile_id();
        self.browse_albums_for_profile(&profile_id, query)
    }

    pub fn browse_albums_for_profile(
        &self,
        profile_id: &str,
        query: LibraryBrowseQuery,
    ) -> Result<LibraryBrowsePage<AlbumSummary>, String> {
        let query = normalize_query(query, DEFAULT_LIMIT, MAX_ALBUM_LIMIT);
        let conn = self.conn.lock().unwrap();
        let total = browse_album_total(&conn, profile_id, &query)?;
        let items = browse_album_items(&conn, profile_id, &query)?;
        let facets = if query.include_facets {
            Some(album_facets_from_sql(&conn)?)
        } else {
            None
        };

        Ok(LibraryBrowsePage {
            items,
            total,
            limit: query.limit,
            offset: query.offset,
            has_more: query.offset + query.limit < total,
            facets,
        })
    }

    #[allow(dead_code)]
    pub fn browse_tracks(
        &self,
        query: LibraryBrowseQuery,
    ) -> Result<LibraryBrowsePage<TrackSummary>, String> {
        let profile_id = self.active_profile_id();
        self.browse_tracks_for_profile(&profile_id, query)
    }

    pub fn browse_tracks_for_profile(
        &self,
        profile_id: &str,
        query: LibraryBrowseQuery,
    ) -> Result<LibraryBrowsePage<TrackSummary>, String> {
        let query = normalize_query(query, 50, MAX_LIMIT);
        let mut tracks = self.song_tracks_for_profile(profile_id)?;
        let facets = track_facets(&tracks);

        tracks.retain(|track| track_matches(track, &query));
        sort_tracks(&mut tracks, &query);
        let total = tracks.len() as i64;
        let mut items = page_items(tracks, &query);
        self.attach_preferred_play_sources(&mut items)?;

        Ok(LibraryBrowsePage {
            items,
            total,
            limit: query.limit,
            offset: query.offset,
            has_more: query.offset + query.limit < total,
            facets: Some(facets),
        })
    }

    #[allow(dead_code)]
    pub fn browse_artists(
        &self,
        query: LibraryBrowseQuery,
    ) -> Result<LibraryBrowsePage<ArtistSummary>, String> {
        let query = normalize_query(query, DEFAULT_LIMIT, MAX_LIMIT);
        let profile_id = self.active_profile_id();
        self.browse_artists_for_profile(&profile_id, query)
    }

    pub fn browse_artists_for_profile(
        &self,
        profile_id: &str,
        query: LibraryBrowseQuery,
    ) -> Result<LibraryBrowsePage<ArtistSummary>, String> {
        let query = normalize_query(query, DEFAULT_LIMIT, MAX_LIMIT);
        let mut artists = self.artists_for_profile(profile_id)?;

        artists.retain(|artist| {
            query
                .q
                .as_deref()
                .map(|q| normalize_search_text(&artist.name).contains(&normalize_search_text(q)))
                .unwrap_or(true)
        });
        sort_artists(&mut artists, &query);
        let total = artists.len() as i64;
        let items = page_items(artists, &query);

        Ok(LibraryBrowsePage {
            items,
            total,
            limit: query.limit,
            offset: query.offset,
            has_more: query.offset + query.limit < total,
            facets: Some(LibraryBrowseFacets::default()),
        })
    }
}

fn browse_album_total(
    conn: &Connection,
    profile_id: &str,
    query: &LibraryBrowseQuery,
) -> Result<i64, String> {
    let mut values = vec![Value::from(profile_id.to_string())];
    let where_clause = album_where_clause(query, &mut values);
    let sql = format!(
        r#"
        WITH track_meta AS ({album_track_meta_sql})
        SELECT COUNT(*)
        FROM albums a
        LEFT JOIN track_meta m ON m.album_id = a.id
        WHERE {where_clause}
        "#,
        album_track_meta_sql = album_track_meta_sql(),
    );
    conn.query_row(&sql, params_from_iter(values.iter()), |row| row.get(0))
        .map_err(|e| format!("album browse count: {e}"))
}

fn browse_album_items(
    conn: &Connection,
    profile_id: &str,
    query: &LibraryBrowseQuery,
) -> Result<Vec<AlbumSummary>, String> {
    let mut values = vec![Value::from(profile_id.to_string())];
    let where_clause = album_where_clause(query, &mut values);
    let order_clause = album_order_clause(query);
    values.push(Value::from(query.limit));
    values.push(Value::from(query.offset));
    let sql = format!(
        r#"
        WITH track_meta AS ({album_track_meta_sql})
        SELECT a.id, a.title, a.album_artist, a.year, a.track_count,
               COALESCE(a.canonical_art_id, a.art_id), a.confidence, a.match_status,
               a.primary_version_id, a.qobuz_album_id, a.qobuz_match_status,
               a.qobuz_match_confidence, a.canonical_art_id, a.original_year, a.mb_barcode,
               CASE WHEN COALESCE(a.canonical_art_id, a.art_id) IS NULL
                          AND a.qobuz_album_id IS NOT NULL
                          AND (a.qobuz_match_status = 'matched'
                               OR (a.qobuz_match_status = 'needs_review'
                                   AND COALESCE(a.qobuz_match_confidence, 0) >= 80))
                    THEN json_extract(a.qobuz_payload_json, '$.album.image_url') END
        FROM albums a
        LEFT JOIN track_meta m ON m.album_id = a.id
        WHERE {where_clause}
        ORDER BY {order_clause}
        LIMIT ? OFFSET ?
        "#,
        album_track_meta_sql = album_track_meta_sql(),
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("album browse query: {e}"))?;
    collect_rows(
        stmt.query_map(params_from_iter(values.iter()), album_from_row)
            .map_err(|e| format!("album browse map: {e}"))?,
    )
}

fn album_track_meta_sql() -> &'static str {
    r#"
    SELECT t.album_id,
           MAX(t.sample_rate) AS max_sample_rate,
           MAX(t.bit_depth) AS max_bit_depth,
           SUM(COALESCE(ph.listened_secs, 0.0)) AS listened_secs
    FROM tracks t
    LEFT JOIN (
        SELECT local_track_id, MIN(recording_id) AS recording_id
        FROM version_tracks
        WHERE local_track_id IS NOT NULL
          AND recording_id IS NOT NULL
        GROUP BY local_track_id
    ) tr ON tr.local_track_id = t.id
    LEFT JOIN (
        SELECT CASE
                   WHEN recording_id IS NOT NULL THEN 'recording:' || recording_id
                   ELSE 'source:' || source_key
               END AS history_key,
               SUM(COALESCE(played_secs, 0.0)) AS listened_secs
        FROM playback_history
        WHERE profile_id = ? AND played_secs IS NOT NULL
        GROUP BY history_key
    ) ph ON ph.history_key = COALESCE('recording:' || tr.recording_id, 'source:local:' || t.id)
    WHERE t.album_id IS NOT NULL
      AND COALESCE(t.status, 'available') = 'available'
    GROUP BY t.album_id
    "#
}

fn album_where_clause(query: &LibraryBrowseQuery, values: &mut Vec<Value>) -> String {
    let mut clauses = Vec::new();
    if let Some(q) = query.q.as_deref() {
        let like = format!("%{}%", q.to_lowercase());
        if let Some(fts_query) = search_fts_query(q) {
            clauses.push(
                r#"
                a.id IN (
                    SELECT album_id FROM albums_fts WHERE albums_fts MATCH ?
                    UNION
                    SELECT id FROM albums
                    WHERE lower(title) LIKE ?
                       OR lower(COALESCE(album_artist, '')) LIKE ?
                       OR lower(COALESCE(qobuz_album_id, '')) LIKE ?
                       OR lower(COALESCE(mb_barcode, '')) LIKE ?
                )
                "#
                .to_string(),
            );
            values.push(Value::from(fts_query));
        } else {
            clauses.push(
                r#"
                (lower(a.title) LIKE ?
                 OR lower(COALESCE(a.album_artist, '')) LIKE ?
                 OR lower(COALESCE(a.qobuz_album_id, '')) LIKE ?
                 OR lower(COALESCE(a.mb_barcode, '')) LIKE ?)
                "#
                .to_string(),
            );
        }
        values.push(Value::from(like.clone()));
        values.push(Value::from(like.clone()));
        values.push(Value::from(like.clone()));
        values.push(Value::from(like));
    }
    if let Some(genre) = query.genre.as_deref() {
        clauses.push(
            "EXISTS (SELECT 1 FROM tracks tg WHERE tg.album_id = a.id AND lower(trim(tg.genre)) = lower(?))"
                .to_string(),
        );
        values.push(Value::from(genre.to_string()));
    }
    if let Some(decade) = query.decade {
        clauses.push("((COALESCE(a.original_year, a.year) / 10) * 10) = ?".to_string());
        values.push(Value::from(decade));
    }
    if let Some(quality) = query.quality.as_deref() {
        clauses.push(format!("{} = ?", album_quality_sql("m")));
        values.push(Value::from(quality.to_string()));
    }
    if let Some(source) = query.source.as_deref() {
        clauses.push(format!("{} = ?", album_source_sql("a")));
        values.push(Value::from(source.to_string()));
    }
    if clauses.is_empty() {
        "1 = 1".to_string()
    } else {
        clauses.join(" AND ")
    }
}

fn album_order_clause(query: &LibraryBrowseQuery) -> &'static str {
    let descending = query.direction.as_deref() != Some("asc");
    match (query.sort.as_deref().unwrap_or("popularity"), descending) {
        ("name", true) => "lower(a.title) DESC, a.id ASC",
        ("name", false) => "lower(a.title) ASC, a.id ASC",
        ("releaseDate" | "release_date", true) => {
            "COALESCE(a.original_year, a.year, 0) DESC, lower(a.title) ASC, a.id ASC"
        }
        ("releaseDate" | "release_date", false) => {
            "COALESCE(a.original_year, a.year, 0) ASC, lower(a.title) ASC, a.id ASC"
        }
        (_, false) => "COALESCE(m.listened_secs, 0.0) ASC, lower(a.title) ASC, a.id ASC",
        _ => "COALESCE(m.listened_secs, 0.0) DESC, lower(a.title) ASC, a.id ASC",
    }
}

fn album_quality_sql(alias: &str) -> String {
    format!(
        r#"
        CASE
            WHEN COALESCE({alias}.max_sample_rate, 0) > 48000
              OR COALESCE({alias}.max_bit_depth, 0) > 16 THEN 'hires'
            WHEN COALESCE({alias}.max_sample_rate, 0) > 0
              OR COALESCE({alias}.max_bit_depth, 0) > 0 THEN 'cd'
            ELSE 'unknown'
        END
        "#
    )
}

fn album_source_sql(alias: &str) -> String {
    format!(
        r#"
        CASE
            WHEN {alias}.qobuz_match_status = 'matched'
              OR NULLIF(TRIM(COALESCE({alias}.qobuz_album_id, '')), '') IS NOT NULL THEN 'qobuz_linked'
            WHEN {alias}.match_status = 'needs_review'
              OR {alias}.qobuz_match_status = 'needs_review' THEN 'needs_review'
            ELSE 'local'
        END
        "#
    )
}

fn album_facets_from_sql(conn: &Connection) -> Result<LibraryBrowseFacets, String> {
    Ok(LibraryBrowseFacets {
        genres: album_genre_facets(conn)?,
        decades: album_decade_facets(conn)?,
        qualities: album_quality_facets(conn)?,
        sources: album_source_facets(conn)?,
    })
}

fn album_genre_facets(conn: &Connection) -> Result<Vec<LibraryFacetOption>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT trim(genre) AS value, COUNT(DISTINCT album_id) AS count
            FROM tracks
            WHERE album_id IS NOT NULL AND NULLIF(trim(genre), '') IS NOT NULL
            GROUP BY trim(genre)
            ORDER BY count DESC, value ASC
            LIMIT 12
            "#,
        )
        .map_err(|e| format!("album genre facets: {e}"))?;
    collect_rows(
        stmt.query_map([], |row| {
            let value: String = row.get(0)?;
            Ok(LibraryFacetOption {
                label: value.clone(),
                value,
                count: row.get(1)?,
            })
        })
        .map_err(|e| format!("album genre facets map: {e}"))?,
    )
}

fn album_decade_facets(conn: &Connection) -> Result<Vec<LibraryFacetOption>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT CAST(((COALESCE(original_year, year) / 10) * 10) AS TEXT) AS value,
                   COUNT(*) AS count
            FROM albums
            WHERE COALESCE(original_year, year) IS NOT NULL
            GROUP BY value
            ORDER BY count DESC, value ASC
            LIMIT 12
            "#,
        )
        .map_err(|e| format!("album decade facets: {e}"))?;
    collect_rows(
        stmt.query_map([], |row| {
            let value: String = row.get(0)?;
            Ok(LibraryFacetOption {
                label: format!("{value}s"),
                value,
                count: row.get(1)?,
            })
        })
        .map_err(|e| format!("album decade facets map: {e}"))?,
    )
}

fn album_quality_facets(conn: &Connection) -> Result<Vec<LibraryFacetOption>, String> {
    let sql = format!(
        r#"
        WITH m AS (
            SELECT album_id, MAX(sample_rate) AS max_sample_rate, MAX(bit_depth) AS max_bit_depth
            FROM tracks
            WHERE album_id IS NOT NULL
            GROUP BY album_id
        ),
        q AS (
            SELECT {quality} AS value
            FROM albums a
            LEFT JOIN m ON m.album_id = a.id
        )
        SELECT value, COUNT(*) AS count
        FROM q
        GROUP BY value
        "#,
        quality = album_quality_sql("m"),
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("album quality facets: {e}"))?;
    let mut options = collect_rows(
        stmt.query_map([], |row| {
            let value: String = row.get(0)?;
            Ok(LibraryFacetOption {
                label: quality_label(&value).to_string(),
                value,
                count: row.get(1)?,
            })
        })
        .map_err(|e| format!("album quality facets map: {e}"))?,
    )?;
    options.sort_by_key(|option| quality_order(&option.value));
    Ok(options)
}

fn album_source_facets(conn: &Connection) -> Result<Vec<LibraryFacetOption>, String> {
    let sql = format!(
        r#"
        SELECT {source} AS value, COUNT(*) AS count
        FROM albums a
        GROUP BY value
        "#,
        source = album_source_sql("a"),
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("album source facets: {e}"))?;
    let mut options = collect_rows(
        stmt.query_map([], |row| {
            let value: String = row.get(0)?;
            Ok(LibraryFacetOption {
                label: source_label(&value).to_string(),
                value,
                count: row.get(1)?,
            })
        })
        .map_err(|e| format!("album source facets map: {e}"))?,
    )?;
    options.sort_by_key(|option| source_order(&option.value));
    Ok(options)
}

fn quality_order(value: &str) -> usize {
    match value {
        "hires" => 0,
        "cd" => 1,
        "lossy" => 2,
        _ => 3,
    }
}

fn source_order(value: &str) -> usize {
    match value {
        "local" => 0,
        "qobuz_linked" => 1,
        "needs_review" => 2,
        _ => 3,
    }
}

fn normalize_query(
    mut query: LibraryBrowseQuery,
    default_limit: i64,
    max_limit: i64,
) -> LibraryBrowseQuery {
    query.limit = if query.limit <= 0 {
        default_limit
    } else {
        query.limit
    }
    .min(max_limit);
    query.offset = query.offset.max(0);
    query.q = clean_filter(query.q);
    query.sort = clean_filter(query.sort);
    query.direction = clean_filter(query.direction);
    query.genre = clean_filter(query.genre);
    query.quality = clean_filter(query.quality).map(|value| value.to_ascii_lowercase());
    query.source = clean_filter(query.source).map(|value| value.to_ascii_lowercase());
    query
}

fn clean_filter(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "all")
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

fn page_items<T>(items: Vec<T>, query: &LibraryBrowseQuery) -> Vec<T> {
    items
        .into_iter()
        .skip(query.offset as usize)
        .take(query.limit as usize)
        .collect()
}

fn track_matches(track: &TrackSummary, query: &LibraryBrowseQuery) -> bool {
    if let Some(q) = query.q.as_deref() {
        let haystack = normalize_search_text(
            &[
                track.title.as_str(),
                track.artist.as_deref().unwrap_or_default(),
                track.album.as_deref().unwrap_or_default(),
                track.album_artist.as_deref().unwrap_or_default(),
                track.genre.as_deref().unwrap_or_default(),
                track.composer.as_deref().unwrap_or_default(),
                track.file_name.as_str(),
            ]
            .join(" "),
        );
        if !haystack.contains(&normalize_search_text(q)) {
            return false;
        }
    }
    if let Some(genre) = query.genre.as_deref()
        && track
            .genre
            .as_deref()
            .map(|value| normalize_search_text(value) == normalize_search_text(genre))
            != Some(true)
    {
        return false;
    }
    if let Some(decade) = query.decade
        && track.year.map(|year| (year / 10) * 10) != Some(decade)
    {
        return false;
    }
    if let Some(quality) = query.quality.as_deref()
        && track_quality(track) != quality
    {
        return false;
    }
    true
}

fn sort_tracks(tracks: &mut [TrackSummary], query: &LibraryBrowseQuery) {
    let descending = query.direction.as_deref() != Some("asc");
    match query.sort.as_deref().unwrap_or("popularity") {
        "name" => tracks.sort_by(|a, b| compare_text(&a.title, &b.title, descending)),
        "releaseDate" | "release_date" => {
            tracks.sort_by(|a, b| compare_i32(a.year, b.year, descending))
        }
        _ => tracks.sort_by(|a, b| {
            compare_i64(a.play_count, b.play_count, descending)
                .then_with(|| compare_f64(a.listened_secs, b.listened_secs, descending))
                .then_with(|| compare_text(&a.title, &b.title, false))
        }),
    }
}

fn sort_artists(artists: &mut [ArtistSummary], query: &LibraryBrowseQuery) {
    let descending = query.direction.as_deref() != Some("asc");
    match query.sort.as_deref().unwrap_or("popularity") {
        "name" => artists.sort_by(|a, b| compare_text(&a.name, &b.name, descending)),
        "albums" => artists.sort_by(|a, b| compare_i64(a.album_count, b.album_count, descending)),
        "songs" => artists.sort_by(|a, b| compare_i64(a.track_count, b.track_count, descending)),
        _ => artists.sort_by(|a, b| {
            compare_f64(a.listened_secs, b.listened_secs, descending)
                .then_with(|| compare_i64(a.play_count, b.play_count, descending))
                .then_with(|| compare_text(&a.name, &b.name, false))
        }),
    }
}

fn compare_text(left: &str, right: &str, descending: bool) -> std::cmp::Ordering {
    let ordering = normalize_search_text(left).cmp(&normalize_search_text(right));
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn compare_i32(left: Option<i32>, right: Option<i32>, descending: bool) -> std::cmp::Ordering {
    let ordering = left.unwrap_or(0).cmp(&right.unwrap_or(0));
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn compare_i64(left: i64, right: i64, descending: bool) -> std::cmp::Ordering {
    let ordering = left.cmp(&right);
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn compare_f64(left: f64, right: f64, descending: bool) -> std::cmp::Ordering {
    let ordering = left
        .partial_cmp(&right)
        .unwrap_or(std::cmp::Ordering::Equal);
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn track_facets(tracks: &[TrackSummary]) -> LibraryBrowseFacets {
    let mut genres = HashMap::new();
    let mut decades = HashMap::new();
    let mut qualities = HashMap::new();
    for track in tracks {
        if let Some(genre) = track
            .genre
            .as_deref()
            .map(clean_label)
            .filter(|v| !v.is_empty())
        {
            *genres.entry(genre).or_insert(0) += 1;
        }
        if let Some(year) = track.year {
            *decades.entry(((year / 10) * 10).to_string()).or_insert(0) += 1;
        }
        *qualities.entry(track_quality(track)).or_insert(0) += 1;
    }
    LibraryBrowseFacets {
        genres: facet_options(genres, |value| value.to_string(), 12),
        decades: facet_options(decades, |value| format!("{value}s"), 12),
        qualities: quality_options(qualities),
        sources: Vec::new(),
    }
}

fn facet_options<F>(values: HashMap<String, i64>, label: F, limit: usize) -> Vec<LibraryFacetOption>
where
    F: Fn(&str) -> String,
{
    let mut options: Vec<_> = values
        .into_iter()
        .map(|(value, count)| LibraryFacetOption {
            label: label(&value),
            value,
            count,
        })
        .collect();
    options.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    options.truncate(limit);
    options
}

fn quality_options(values: HashMap<&'static str, i64>) -> Vec<LibraryFacetOption> {
    ["hires", "cd", "lossy", "unknown"]
        .into_iter()
        .filter_map(|value| {
            values.get(value).map(|count| LibraryFacetOption {
                value: value.to_string(),
                label: quality_label(value).to_string(),
                count: *count,
            })
        })
        .collect()
}

fn track_quality(track: &TrackSummary) -> &'static str {
    let sample_rate = track.sample_rate.unwrap_or(0);
    let bit_depth = track.bit_depth.unwrap_or(0);
    let format = track
        .format
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if sample_rate > 48_000 || bit_depth > 16 {
        "hires"
    } else if matches!(format.as_str(), "mp3" | "aac" | "ogg" | "opus" | "m4a") {
        "lossy"
    } else if sample_rate > 0 || bit_depth > 0 {
        "cd"
    } else {
        "unknown"
    }
}

fn quality_label(value: &str) -> &'static str {
    match value {
        "hires" => "Hi-res",
        "cd" => "CD quality",
        "lossy" => "Lossy",
        _ => "Unknown quality",
    }
}

fn source_label(value: &str) -> &'static str {
    match value {
        "qobuz_linked" => "Qobuz linked",
        "needs_review" => "Needs review",
        _ => "Local",
    }
}

fn clean_label(value: &str) -> String {
    value.trim().to_string()
}

fn normalize_search_text(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_browse_allows_large_library_pages() {
        let query = normalize_query(
            LibraryBrowseQuery {
                limit: 1_100,
                ..LibraryBrowseQuery::default()
            },
            DEFAULT_LIMIT,
            MAX_ALBUM_LIMIT,
        );

        assert_eq!(query.limit, 1_100);
    }

    #[test]
    fn standard_browse_limits_stay_capped() {
        let query = normalize_query(
            LibraryBrowseQuery {
                limit: 1_100,
                ..LibraryBrowseQuery::default()
            },
            DEFAULT_LIMIT,
            MAX_LIMIT,
        );

        assert_eq!(query.limit, MAX_LIMIT);
    }
}
