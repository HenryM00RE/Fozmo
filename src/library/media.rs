use super::{
    Library, clean_artist_display_value, now_secs, path_file_name_and_ext,
    resolve_local_track_display_tags,
    scanner::{folder_cover_dirs_for_path, path_album_fallback_for_dirs},
};
use crate::protocol::SourceRef;
use image::codecs::jpeg::JpegEncoder;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

type SourceRefTrackRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<f64>,
);
type TrackDisplayTags = (Option<String>, Option<String>, Option<String>);
type TrackDisplayTagRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
type AlbumArtRefRow = (i64, Option<i64>, Option<i64>);

impl Library {
    pub fn art(&self, art_id: i64) -> Result<Option<(String, Vec<u8>)>, String> {
        if let Some(art) = self.read_art_bytes(art_id)? {
            return Ok(Some(art));
        }
        self.recover_album_art_for_broken_id(art_id)
    }

    fn read_art_bytes(&self, art_id: i64) -> Result<Option<(String, Vec<u8>)>, String> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT mime, path FROM artworks WHERE id = ?1",
                [art_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("art lookup: {e}"))?;
        drop(conn);
        let Some((mime, path)) = row else {
            return Ok(None);
        };
        let path = self.resolve_artwork_path(&path);
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(_) => return Ok(None),
        };
        let Some(safe_mime) = crate::library::safe_raster_artwork_mime(&data, &mime) else {
            return Ok(None);
        };
        Ok(Some((safe_mime.to_string(), data)))
    }

    fn recover_album_art_for_broken_id(
        &self,
        broken_art_id: i64,
    ) -> Result<Option<(String, Vec<u8>)>, String> {
        for (album_id, local_art_id, _canonical_art_id) in self.albums_for_art_id(broken_art_id)? {
            if let Some(local_art_id) = local_art_id.filter(|id| *id != broken_art_id)
                && let Some(art) = self.read_art_bytes(local_art_id)?
            {
                self.replace_album_art_reference(album_id, local_art_id)?;
                return Ok(Some(art));
            }

            if let Some(folder_art_id) = self.load_album_folder_cover(album_id)? {
                self.replace_album_art_reference(album_id, folder_art_id)?;
                if let Some(art) = self.read_art_bytes(folder_art_id)? {
                    return Ok(Some(art));
                }
            }
        }
        Ok(None)
    }

    fn albums_for_art_id(&self, art_id: i64) -> Result<Vec<AlbumArtRefRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT DISTINCT a.id, a.art_id, a.canonical_art_id
                FROM albums a
                LEFT JOIN tracks t ON t.album_id = a.id
                WHERE a.art_id = ?1 OR a.canonical_art_id = ?1 OR t.art_id = ?1
                ORDER BY CASE WHEN a.canonical_art_id = ?1 THEN 0 ELSE 1 END, a.id
                "#,
            )
            .map_err(|e| format!("album art refs: {e}"))?;
        let rows = stmt
            .query_map([art_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(|e| format!("album art refs map: {e}"))?;
        let mut refs = Vec::new();
        for row in rows {
            refs.push(row.map_err(|e| format!("album art refs row: {e}"))?);
        }
        Ok(refs)
    }

    fn load_album_folder_cover(&self, album_id: i64) -> Result<Option<i64>, String> {
        let paths: Vec<PathBuf> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT path FROM tracks WHERE album_id = ?1 ORDER BY disc_number, track_number, path")
                .map_err(|e| format!("album art track paths: {e}"))?;
            let rows = stmt
                .query_map([album_id], |row| row.get::<_, String>(0))
                .map_err(|e| format!("album art track path map: {e}"))?;
            let mut paths = Vec::new();
            for row in rows {
                paths.push(PathBuf::from(
                    row.map_err(|e| format!("album art track path row: {e}"))?,
                ));
            }
            paths
        };
        let music_dirs = self.music_dirs();
        let mut seen = HashSet::new();
        for path in paths {
            let fallback = path_album_fallback_for_dirs(&music_dirs, &path);
            for dir in folder_cover_dirs_for_path(&path, &fallback) {
                if seen.insert(dir.clone())
                    && let Some(art_id) = self.load_folder_cover(&dir)?
                {
                    return Ok(Some(art_id));
                }
            }
        }
        Ok(None)
    }

    fn replace_album_art_reference(&self, album_id: i64, art_id: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE albums SET art_id = ?2, canonical_art_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![album_id, art_id, now_secs()],
        )
        .map_err(|e| format!("recover album art: {e}"))?;
        conn.execute(
            "UPDATE tracks SET art_id = ?2, updated_at = ?3 WHERE album_id = ?1",
            params![album_id, art_id, now_secs()],
        )
        .map_err(|e| format!("recover track art: {e}"))?;
        Self::sync_local_versions_for_album_with_conn(&conn, album_id)?;
        Ok(())
    }

    pub fn art_thumbnail(
        &self,
        art_id: i64,
        size: u32,
    ) -> Result<Option<(String, Vec<u8>)>, String> {
        let size = size.clamp(64, 1024);
        let cache_dir = self.thumbnail_cache_dir.clone();
        let cache_path = cache_dir.join(format!("{art_id}-{size}.jpg"));
        if let Ok(data) = fs::read(&cache_path) {
            if crate::library::safe_raster_artwork_mime(&data, "image/jpeg").is_some() {
                return Ok(Some(("image/jpeg".to_string(), data)));
            }
            let _ = fs::remove_file(&cache_path);
        }

        let Some((_mime, data)) = self.art(art_id)? else {
            return Ok(None);
        };
        let image = match image::load_from_memory(&data) {
            Ok(image) => image,
            Err(_) => return Ok(None),
        };
        let thumb = image.thumbnail(size, size).to_rgb8();
        let mut encoded = Vec::new();
        JpegEncoder::new_with_quality(&mut encoded, 84)
            .encode_image(&thumb)
            .map_err(|e| format!("encode thumbnail: {e}"))?;
        fs::create_dir_all(&cache_dir).map_err(|e| format!("create thumbnail dir: {e}"))?;
        fs::write(&cache_path, &encoded).map_err(|e| format!("write thumbnail: {e}"))?;
        Ok(Some(("image/jpeg".to_string(), encoded)))
    }

    fn resolve_artwork_path(&self, stored: &str) -> PathBuf {
        let path = PathBuf::from(stored);
        if path.is_absolute() {
            path
        } else {
            self.art_dir.join(path)
        }
    }

    pub fn track_path(&self, track_id: i64) -> Result<Option<PathBuf>, String> {
        let conn = self.conn.lock().unwrap();
        let path: Option<String> = conn
            .query_row("SELECT path FROM tracks WHERE id = ?1", [track_id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| format!("track path: {e}"))?;
        Ok(path.map(PathBuf::from))
    }

    pub fn track_id_for_file_name(&self, file_name: &str) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id FROM tracks WHERE file_name = ?1 LIMIT 1",
            [file_name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("track id for file name: {e}"))
    }

    pub fn source_ref_for_track_id(&self, track_id: i64) -> Result<Option<SourceRef>, String> {
        let conn = self.conn.lock().unwrap();
        let row: Option<SourceRefTrackRow> = conn
            .query_row(
                r#"
                SELECT t.path, t.title, t.artist, t.album, t.album_artist, a.album_artist, a.title,
                       t.album_id, COALESCE(t.art_id, a.canonical_art_id, a.art_id), t.duration_secs
                FROM tracks t
                LEFT JOIN albums a ON a.id = t.album_id
                WHERE t.id = ?1
                "#,
                [track_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("track source ref: {e}"))?;
        let Some((
            path,
            title,
            artist,
            album,
            track_album_artist,
            album_artist,
            album_title,
            album_id,
            art_id,
            duration_secs,
        )) = row
        else {
            return Ok(None);
        };
        let (file_name, ext_hint) = path_file_name_and_ext(&path);
        let resolved_album_artist = clean_artist_display_value(track_album_artist.clone())
            .or_else(|| clean_artist_display_value(album_artist.clone()));
        let (title, artist, album) = resolve_local_track_display_tags(
            title,
            artist,
            album,
            track_album_artist,
            album_artist,
            album_title,
        );
        Ok(Some(SourceRef::LocalTrack {
            track_id,
            file_name,
            title,
            artist,
            album,
            album_artist: resolved_album_artist,
            album_id,
            art_id,
            duration_secs,
            ext_hint,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }))
    }

    /// Look up DB-backed title/artist/album for a file basename.
    pub fn tags_for_file_name(
        &self,
        file_name: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        self.track_id_for_file_name(file_name)
            .ok()
            .flatten()
            .and_then(|track_id| self.tags_for_track_id(track_id).ok().flatten())
            .unwrap_or((None, None, None))
    }

    pub fn tags_for_track_id(&self, track_id: i64) -> Result<Option<TrackDisplayTags>, String> {
        let conn = self.conn.lock().unwrap();
        let row: Option<TrackDisplayTagRow> = conn
            .query_row(
                r#"
                SELECT t.title, t.artist, t.album, t.album_artist, a.album_artist, a.title
                FROM tracks t
                LEFT JOIN albums a ON a.id = t.album_id
                WHERE t.id = ?1
                "#,
                [track_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("track tags by id: {e}"))?;
        Ok(row.map(
            |(title, artist, album, track_album_artist, album_artist, album_title)| {
                resolve_local_track_display_tags(
                    title,
                    artist,
                    album,
                    track_album_artist,
                    album_artist,
                    album_title,
                )
            },
        ))
    }

    pub fn cover_for_track_path(&self, path: &str) -> Result<Option<(String, Vec<u8>)>, String> {
        let art_id: Option<i64> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT COALESCE(t.art_id, a.art_id)
                 FROM tracks t
                 LEFT JOIN albums a ON a.id = t.album_id
                 WHERE t.path = ?1",
                [path],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(|e| format!("cover lookup: {e}"))?
            .flatten()
        };
        let Some(art_id) = art_id else {
            return Ok(None);
        };
        self.art(art_id)
    }
}
