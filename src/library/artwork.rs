use super::{Library, now_secs};
use crate::audio::player::TrackCover;
use image::ImageFormat;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// Filenames (without extension) we treat as folder-level album art, in priority order.
const FOLDER_COVER_STEMS: &[&str] = &["cover", "folder", "front", "albumart", "album"];
const FOLDER_COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];
pub(crate) const MAX_ARTWORK_BYTES: usize = 50 * 1024 * 1024;
const MAX_ARTWORK_PIXELS: u64 = 64_000_000;
static NEXT_ARTWORK_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub(super) struct PreparedArtwork {
    hash: String,
    mime: String,
    file_name: String,
    width: Option<i64>,
    height: Option<i64>,
}

impl Library {
    pub(super) fn save_artwork(&self, cover: &TrackCover, source: &str) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        self.save_artwork_with_conn(&conn, cover, source)
    }

    pub(super) fn save_artwork_with_conn(
        &self,
        conn: &rusqlite::Connection,
        cover: &TrackCover,
        source: &str,
    ) -> Result<i64, String> {
        let prepared = self.prepare_artwork(cover)?;
        Self::save_prepared_artwork_with_conn(conn, &prepared, source)
    }

    /// Validate, hash, and persist artwork bytes without holding SQLite.
    /// The resulting descriptor is small and can be inserted in a later write
    /// transaction without retaining the potentially large image buffer.
    pub(super) fn prepare_artwork(&self, cover: &TrackCover) -> Result<PreparedArtwork, String> {
        let safe_mime = safe_raster_artwork_mime(&cover.data, &cover.mime)
            .ok_or_else(|| "Album art must be a JPEG, PNG, or WebP image".to_string())?;
        let mut hasher = Sha256::new();
        hasher.update(&cover.data);
        let hash = format!("{:x}", hasher.finalize());
        let ext = art_extension(safe_mime);
        let file_name = format!("{hash}.{ext}");
        let path = self.art_dir.join(&file_name);
        write_artwork_atomically(&path, &cover.data)?;
        let (width, height) = image_dimensions(&cover.data);
        Ok(PreparedArtwork {
            hash,
            mime: safe_mime.to_string(),
            file_name,
            width,
            height,
        })
    }

    pub(super) fn save_prepared_artwork_with_conn(
        conn: &rusqlite::Connection,
        prepared: &PreparedArtwork,
        source: &str,
    ) -> Result<i64, String> {
        let now = now_secs();
        conn.execute(
            r#"
            INSERT INTO artworks (hash, mime, path, source, width, height, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(hash) DO UPDATE SET
                width = COALESCE(artworks.width, excluded.width),
                height = COALESCE(artworks.height, excluded.height)
            "#,
            params![
                prepared.hash,
                prepared.mime,
                prepared.file_name,
                source,
                prepared.width,
                prepared.height,
                now
            ],
        )
        .map_err(|e| format!("insert artwork: {e}"))?;
        conn.query_row(
            "SELECT id FROM artworks WHERE hash = ?1",
            [&prepared.hash],
            |row| row.get(0),
        )
        .map_err(|e| format!("select artwork: {e}"))
    }

    #[cfg(test)]
    pub(crate) fn insert_unsafe_artwork_for_test(&self, mime: &str, data: &[u8]) -> i64 {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash = format!("{:x}", hasher.finalize());
        let file_name = format!("{hash}.bin");
        let path = self.art_dir.join(&file_name);
        fs::write(&path, data).unwrap();
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO artworks (hash, mime, path, source, width, height, created_at)
            VALUES (?1, ?2, ?3, 'test', NULL, NULL, ?4)
            "#,
            params![hash, mime, file_name, now],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Convert legacy absolute or nested artwork paths into unique filenames
    /// relative to the managed artwork root. If the root changed, the original
    /// file is copied before the database reference is updated.
    pub(super) fn normalize_artwork_paths(&self) -> Result<(), String> {
        let rows = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT id, path FROM artworks")
                .map_err(|error| format!("inspect artwork paths: {error}"))?;
            let mapped = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| format!("read artwork paths: {error}"))?;
            let mut rows = Vec::new();
            for row in mapped {
                rows.push(row.map_err(|error| format!("read artwork path row: {error}"))?);
            }
            rows
        };

        let mut updates = Vec::new();
        for (id, stored) in rows {
            let stored_path = PathBuf::from(&stored);
            let source = if stored_path.is_absolute() {
                stored_path.clone()
            } else {
                self.art_dir.join(&stored_path)
            };
            let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !stored_path.is_absolute() && stored_path == Path::new(file_name) {
                continue;
            }
            let mut managed_name = format!("normalized-{id}-{file_name}");
            let mut suffix = 2_u32;
            while self.art_dir.join(&managed_name).exists() {
                managed_name = format!("normalized-{id}-{suffix}-{file_name}");
                suffix += 1;
            }
            let destination = self.art_dir.join(&managed_name);
            if !destination.exists() && source.is_file() {
                fs::copy(&source, &destination).map_err(|error| {
                    format!("move artwork into managed root {:?}: {error}", destination)
                })?;
            }
            if destination.is_file() {
                updates.push((id, managed_name));
            }
        }
        if !updates.is_empty() {
            let conn = self.conn.lock().unwrap();
            for (id, file_name) in updates {
                conn.execute(
                    "UPDATE artworks SET path = ?2 WHERE id = ?1",
                    params![id, file_name],
                )
                .map_err(|error| format!("normalize artwork path: {error}"))?;
            }
        }
        Ok(())
    }

    pub(super) fn load_folder_cover(&self, dir: &Path) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().unwrap();
        self.load_folder_cover_with_conn(&conn, dir)
    }

    pub(super) fn load_folder_cover_with_conn(
        &self,
        conn: &rusqlite::Connection,
        dir: &Path,
    ) -> Result<Option<i64>, String> {
        if let Some(cover) = read_folder_cover(dir) {
            return self
                .save_artwork_with_conn(conn, &cover, "folder")
                .map(Some);
        }
        Ok(None)
    }

    pub(super) fn set_album_art_with_conn(
        conn: &rusqlite::Connection,
        album_id: i64,
        art_id: i64,
    ) -> Result<(), String> {
        conn.execute(
            "UPDATE albums SET art_id = ?2, updated_at = ?3 WHERE id = ?1 AND COALESCE(art_locked, 0) = 0 AND COALESCE(art_id, -1) != ?2",
            params![album_id, art_id, now_secs()],
        )
        .map_err(|e| format!("album art set: {e}"))?;
        Ok(())
    }

    /// Promote `new_art_id` to the album's canonical cover if it has more
    /// pixels than whatever the album currently displays
    /// (`COALESCE(canonical_art_id, art_id)`). Never touches `art_id`, so
    /// file/folder-derived art is preserved and the upgrade is reversible.
    /// Returns true when the canonical art was updated.
    pub(super) fn apply_canonical_art_if_better(
        &self,
        album_id: i64,
        new_art_id: i64,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let (current, locked): (Option<i64>, i64) = conn
            .query_row(
                "SELECT COALESCE(canonical_art_id, art_id), COALESCE(art_locked, 0) FROM albums WHERE id = ?1",
                [album_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("canonical art lookup: {e}"))?
            .unwrap_or((None, 0));
        if locked != 0 {
            return Ok(false);
        }
        let better = match current {
            None => true,
            Some(current_id) => {
                current_id != new_art_id
                    && artwork_score_with_conn(&conn, new_art_id)?
                        > artwork_score_with_conn(&conn, current_id)?
            }
        };
        if better {
            conn.execute(
                "UPDATE albums SET canonical_art_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![album_id, new_art_id, now_secs()],
            )
            .map_err(|e| format!("canonical art update: {e}"))?;
        }
        Ok(better)
    }

    pub(super) fn apply_album_art_if_empty_with_conn(
        conn: &rusqlite::Connection,
        album_id: i64,
        art_id: i64,
    ) -> Result<(), String> {
        conn.execute(
            "UPDATE albums SET art_id = COALESCE(art_id, ?2), updated_at = ?3 WHERE id = ?1",
            params![album_id, art_id, now_secs()],
        )
        .map_err(|e| format!("album art update: {e}"))?;
        Ok(())
    }
}

fn write_artwork_atomically(path: &Path, data: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let temp_id = NEXT_ARTWORK_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artwork");
    let temp_path = path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        temp_id
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|error| format!("create artwork staging file: {error}"))?;
    if let Err(error) = file.write_all(data) {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(format!("write artwork staging file: {error}"));
    }
    drop(file);
    if let Err(error) = fs::rename(&temp_path, path) {
        let destination_won_race = path.exists();
        let _ = fs::remove_file(&temp_path);
        if !destination_won_race {
            return Err(format!("publish artwork: {error}"));
        }
    }
    Ok(())
}

/// Read and validate folder artwork without touching SQLite.
///
/// Library scans use this before opening their write transaction so filesystem
/// latency and image decoding cannot hold the shared database mutex.
pub(super) fn read_folder_cover(dir: &Path) -> Option<TrackCover> {
    for path in find_folder_cover_candidates(dir) {
        if fs::metadata(&path)
            .map(|metadata| metadata.len() as usize > MAX_ARTWORK_BYTES)
            .unwrap_or(false)
        {
            continue;
        }
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(_) => continue,
        };
        let mime = mime_for_extension(path.extension().and_then(|e| e.to_str())).to_string();
        if let Ok(cover) = sanitize_raster_artwork(&data, Some(&mime)) {
            return Some(cover);
        }
    }
    None
}

pub(super) fn best_artwork_id_with_conn(
    conn: &rusqlite::Connection,
    left: Option<i64>,
    right: Option<i64>,
) -> Result<Option<i64>, String> {
    match (left, right) {
        (None, None) => Ok(None),
        (Some(id), None) | (None, Some(id)) => Ok(Some(id)),
        (Some(left_id), Some(right_id)) => {
            let left_score = artwork_score_with_conn(conn, left_id)?;
            let right_score = artwork_score_with_conn(conn, right_id)?;
            if left_score >= right_score {
                Ok(Some(left_id))
            } else {
                Ok(Some(right_id))
            }
        }
    }
}

fn artwork_score_with_conn(conn: &rusqlite::Connection, art_id: i64) -> Result<i64, String> {
    let row: Option<(Option<i64>, Option<i64>, String)> = conn
        .query_row(
            "SELECT width, height, source FROM artworks WHERE id = ?1",
            [art_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("art dimensions: {e}"))?;
    let Some((Some(width), Some(height), source)) = row else {
        return Ok(0);
    };
    if width <= 0 || height <= 0 {
        return Ok(0);
    }

    let long = width.max(height) as f64;
    let short = width.min(height) as f64;
    let square_ratio = short / long;
    let square_penalty = if square_ratio >= 0.95 {
        100
    } else if square_ratio >= 0.85 {
        82
    } else if square_ratio >= 0.70 {
        55
    } else {
        25
    };
    let source_weight = match source.as_str() {
        "user_upload" | "user" => 140,
        "itunes" => 120,
        "qobuz" => 112,
        "folder" => 104,
        "embedded" => 95,
        _ => 100,
    };
    let shortest_side = width.min(height).min(2000);
    let floor_penalty = if shortest_side >= 1400 {
        100
    } else if shortest_side >= 500 {
        86
    } else {
        58
    };

    Ok(shortest_side * shortest_side * source_weight * square_penalty * floor_penalty / 1_000_000)
}

fn find_folder_cover_candidates(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file() && is_supported_artwork_path(path))
            .collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    });

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for stem in FOLDER_COVER_STEMS {
        for path in &entries {
            if path_stem_matches(path, stem) && seen.insert(path.clone()) {
                out.push(path.clone());
            }
        }
    }
    for path in entries {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
    out
}

fn is_supported_artwork_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            FOLDER_COVER_EXTS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
        .unwrap_or(false)
}

fn path_stem_matches(path: &Path, expected: &str) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

fn mime_for_extension(ext: Option<&str>) -> &'static str {
    match ext.map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}

fn art_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

pub(super) fn validate_uploaded_cover(
    data: Vec<u8>,
    supplied_mime: Option<&str>,
) -> Result<TrackCover, String> {
    let detected_mime = validate_raster_artwork(&data, supplied_mime)?;
    Ok(TrackCover {
        mime: detected_mime.to_string(),
        data,
    })
}

pub(crate) fn sanitize_raster_artwork(
    data: &[u8],
    supplied_mime: Option<&str>,
) -> Result<TrackCover, String> {
    let detected_mime = validate_raster_artwork(data, supplied_mime)?;
    Ok(TrackCover {
        mime: detected_mime.to_string(),
        data: data.to_vec(),
    })
}

fn validate_raster_artwork(
    data: &[u8],
    supplied_mime: Option<&str>,
) -> Result<&'static str, String> {
    if data.is_empty() {
        return Err("Cover image is empty".to_string());
    }
    if data.len() > MAX_ARTWORK_BYTES {
        return Err("Cover image is too large".to_string());
    }
    let supplied_mime = match supplied_mime {
        Some(mime) => Some(
            canonical_safe_mime(mime)
                .ok_or_else(|| "Album art must be a JPEG, PNG, or WebP image".to_string())?,
        ),
        None => None,
    };
    let detected_mime = detect_safe_raster_mime(data)
        .ok_or_else(|| "Album art bytes are not a supported raster image".to_string())?;
    if supplied_mime.is_some_and(|mime| mime != detected_mime) {
        return Err("Album art bytes do not match the declared image type".to_string());
    }
    decode_safe_raster(data, detected_mime)?;
    Ok(detected_mime)
}

pub(crate) fn safe_raster_artwork_mime(data: &[u8], stored_mime: &str) -> Option<&'static str> {
    validate_raster_artwork(data, Some(stored_mime)).ok()
}

fn canonical_safe_mime(mime: &str) -> Option<&'static str> {
    match mime
        .split(';')
        .next()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("image/jpeg") | Some("image/jpg") => Some("image/jpeg"),
        Some("image/png") => Some("image/png"),
        Some("image/webp") => Some("image/webp"),
        _ => None,
    }
}

fn detect_safe_raster_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

fn decode_safe_raster(data: &[u8], mime: &str) -> Result<(), String> {
    let format = match mime {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::WebP,
        _ => return Err("Unsupported album art image type".to_string()),
    };
    let image = image::load_from_memory_with_format(data, format)
        .map_err(|_| "Album art could not be decoded as a safe raster image".to_string())?;
    let pixels = u64::from(image.width()) * u64::from(image.height());
    if pixels > MAX_ARTWORK_PIXELS {
        return Err("Cover image dimensions are too large".to_string());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn tiny_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    #[test]
    fn sanitizer_rejects_active_content() {
        assert!(
            sanitize_raster_artwork(
                b"<!doctype html><script>alert(1)</script>",
                Some("text/html")
            )
            .is_err()
        );
        assert!(
            sanitize_raster_artwork(
                br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
                Some("image/svg+xml"),
            )
            .is_err()
        );
    }

    #[test]
    fn sanitizer_canonicalizes_valid_raster_artwork() {
        let cover =
            sanitize_raster_artwork(&tiny_png(), Some("image/png; charset=binary")).unwrap();

        assert_eq!(cover.mime, "image/png");
    }

    #[test]
    fn sanitizer_accepts_large_artwork_within_current_limit() {
        let mut data = tiny_png();
        data.resize(8 * 1024 * 1024 + 1, 0);

        let cover = sanitize_raster_artwork(&data, Some("image/png")).unwrap();

        assert_eq!(cover.mime, "image/png");
    }

    #[test]
    fn sanitizer_rejects_oversized_artwork_before_copying() {
        let data = vec![0_u8; MAX_ARTWORK_BYTES + 1];

        assert!(sanitize_raster_artwork(&data, Some("image/png")).is_err());
    }

    #[test]
    fn concurrent_content_addressed_artwork_writes_publish_complete_file() {
        let id = NEXT_ARTWORK_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fozmo-artwork-write-{}-{id}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let destination = dir.join("same-hash.png");
        let data = tiny_png();

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| write_artwork_atomically(&destination, &data).unwrap());
            }
        });

        assert_eq!(fs::read(&destination).unwrap(), data);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}

pub(super) fn image_dimensions(data: &[u8]) -> (Option<i64>, Option<i64>) {
    imagesize::blob_size(data)
        .ok()
        .map(|size| (Some(size.width as i64), Some(size.height as i64)))
        .unwrap_or((None, None))
}
