use super::{ArtistSummary, Library, collect_rows};
use rusqlite::params;

impl Library {
    #[allow(dead_code)]
    pub fn artists(&self) -> Result<Vec<ArtistSummary>, String> {
        let profile_id = self.active_profile_id();
        self.artists_for_profile(&profile_id)
    }

    pub fn artists_for_profile(&self, profile_id: &str) -> Result<Vec<ArtistSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                WITH track_artist_links AS (
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
                        WHERE h.profile_id = ?1
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
                LEFT JOIN artist_counts ac ON ac.artist_key = lower(a.name)
                LEFT JOIN artist_popularity ph ON ph.artist_key = lower(a.name)
                ORDER BY lower(a.name)
                "#,
            )
            .map_err(|e| format!("artists query: {e}"))?;
        let rows = stmt
            .query_map(params![profile_id], |row| {
                Ok(ArtistSummary {
                    name: row.get(0)?,
                    album_count: row.get(1)?,
                    track_count: row.get(2)?,
                    play_count: row.get(3)?,
                    listened_secs: row.get(4)?,
                })
            })
            .map_err(|e| format!("artists map: {e}"))?;
        collect_rows(rows)
    }
}
