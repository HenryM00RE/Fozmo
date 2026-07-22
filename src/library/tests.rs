use super::artwork::image_dimensions;
use super::itunes_art::{itunes_result_matches, upgrade_artwork_url};
use super::matching::{
    confidence_score, dedupe_by_release_group, edition_score, extract_mb_tracks,
    filename_without_track_prefix, levenshtein, metabrainz_evidence_for_release,
    normalize_for_match, pair_tracks, release_track_count, verify_release_against_tracks,
};
use super::musicbrainz::{
    complete_local_track_number_order, infer_metabrainz_lookup_terms,
    track_assisted_metabrainz_queries,
};
use super::qobuz_sync::{
    normalize_barcode, ordered_qobuz_tracks, pair_qobuz_tracks, qobuz_evidence_complete,
    score_qobuz_album_match,
};
use super::scanner::{
    album_seed_for_path, modified_secs, path_album_fallback, title_from_file_name,
};
use super::*;
use crate::audio::player::TrackCover;
use crate::protocol::SourceRef;
use crate::services::qobuz::{QobuzAlbum, QobuzAlbumDetail, QobuzTrack};
use rusqlite::params;

#[test]
fn strips_leading_track_numbers_from_filenames() {
    assert_eq!(title_from_file_name("01. Brakhage.flac"), "Brakhage");
    assert_eq!(title_from_file_name("02 - Attenzione!.flac"), "Attenzione!");
    assert_eq!(title_from_file_name("01. 2 + 2 = 5.wav"), "2 + 2 = 5");
    assert_eq!(title_from_file_name("Packet.wav"), "Packet");
}

#[test]
fn normalizes_album_keys() {
    assert_eq!(
        normalize_key("Blue Lines (2012 Mix/Master)"),
        "blue lines 2012 mix master"
    );
}

#[test]
fn album_seed_uses_album_folder_above_disc_folder() {
    let music_dir = Path::new("/music");
    let path = music_dir
        .join("Radiohead")
        .join("Kid A")
        .join("DISK 1")
        .join("01 Everything In Its Right Place.wav");

    let seed = album_seed_for_path(music_dir, &path, &None, &None, &None, None);

    assert_eq!(seed.title, "Kid A");
    assert_eq!(seed.sort_key, "unknown-artist|kid a");
}

#[test]
fn album_seed_infers_artist_album_folder_when_tags_are_missing() {
    let music_dir = Path::new("/music");
    let path = music_dir
        .join("Radiohead - Amnesiac(WAV)")
        .join("01 Packt Like Sardines.wav");

    let seed = album_seed_for_path(music_dir, &path, &None, &None, &None, None);

    assert_eq!(seed.title, "Amnesiac");
    assert_eq!(seed.album_artist.as_deref(), Some("Radiohead"));
    assert_eq!(seed.sort_key, "radiohead|amnesiac");
    assert_eq!(seed.match_status, "needs_review");
}

#[test]
fn album_seed_keeps_embedded_album_over_folder_artist_title_parse() {
    let music_dir = Path::new("/music");
    let path = music_dir
        .join("Radiohead - Amnesiac(WAV)")
        .join("01 Packt Like Sardines.wav");

    let seed = album_seed_for_path(
        music_dir,
        &path,
        &Some("Embedded Album".to_string()),
        &None,
        &None,
        None,
    );

    assert_eq!(seed.title, "Embedded Album");
    assert_eq!(seed.album_artist, None);
}

#[test]
fn album_seed_splits_embedded_artist_album_title_when_artist_is_missing() {
    let music_dir = Path::new("/music");
    let path = music_dir
        .join("Radiohead - Amnesiac(WAV)")
        .join("01 Packt Like Sardines.wav");

    let seed = album_seed_for_path(
        music_dir,
        &path,
        &Some("Radiohead - Amnesiac(WAV)".to_string()),
        &None,
        &None,
        None,
    );

    assert_eq!(seed.title, "Amnesiac");
    assert_eq!(seed.album_artist.as_deref(), Some("Radiohead"));
    assert_eq!(seed.sort_key, "radiohead|amnesiac");
}

#[test]
fn album_seed_does_not_split_embedded_album_when_known_artist_disagrees() {
    let music_dir = Path::new("/music");
    let path = music_dir
        .join("Radiohead - Amnesiac(WAV)")
        .join("01 Packt Like Sardines.wav");

    let seed = album_seed_for_path(
        music_dir,
        &path,
        &Some("Radiohead - Amnesiac(WAV)".to_string()),
        &None,
        &Some("Different Artist".to_string()),
        None,
    );

    assert_eq!(seed.title, "Radiohead - Amnesiac(WAV)");
    assert_eq!(seed.album_artist.as_deref(), Some("Different Artist"));
}

#[test]
fn scan_links_artist_album_folder_without_embedded_tags() {
    let root = temp_test_dir("artist-album-folder-scan");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Radiohead - Amnesiac(WAV)");
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(
        album_dir.join("01 Packt Like Sardines.wav"),
        b"not a real wav",
    )
    .unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();

    library.scan().unwrap();

    let albums = library.albums().unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].title, "Amnesiac");
    assert_eq!(albums[0].album_artist.as_deref(), Some("Radiohead"));
    let tracks = library.tracks().unwrap();
    assert_eq!(tracks[0].album.as_deref(), Some("Amnesiac"));
    assert_eq!(tracks[0].album_artist.as_deref(), Some("Radiohead"));
    let artists = library.artists().unwrap();
    assert!(artists.iter().any(|artist| artist.name == "Radiohead"));
}

#[test]
fn rescan_repairs_empty_album_cover_from_fresh_track_art() {
    let root = temp_test_dir("rescan-embedded-cover-fallback");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Present Tense");
    std::fs::create_dir_all(&album_dir).unwrap();
    let track_path = album_dir.join("01 Present Tense.wav");
    std::fs::write(&track_path, b"not a real wav").unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();

    library.scan().unwrap();
    let album_id = library.albums().unwrap()[0].id;
    let track_id: i64 = {
        let conn = library.conn.lock().unwrap();
        conn.query_row("SELECT id FROM tracks LIMIT 1", [], |row| row.get(0))
            .unwrap()
    };
    let embedded_art_id = library
        .save_artwork(
            &TrackCover {
                mime: "image/png".to_string(),
                data: tiny_png(),
            },
            "embedded",
        )
        .unwrap();
    {
        let conn = library.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE tracks SET art_id = ?2, embedded_art = 1, bit_depth = 16, format = 'MP3' WHERE id = ?1",
            params![track_id, embedded_art_id],
        )
        .unwrap();
        assert_eq!(updated, 1);
    }

    let album = library.album_detail(album_id).unwrap().unwrap().album;

    assert_eq!(album.art_id, Some(embedded_art_id));
}

#[test]
fn album_detail_repairs_new_file_cover_after_initial_scan() {
    let root = temp_test_dir("album-detail-cover-repair");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Late Artwork");
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join("01 Track.wav"), tiny_wav()).unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();

    library.scan().unwrap();
    let album = library.albums().unwrap().remove(0);
    assert!(album.art_id.is_none());

    std::fs::write(album_dir.join("Cover.png"), tiny_png()).unwrap();
    let repaired = library.album_detail(album.id).unwrap().unwrap().album;

    assert!(repaired.art_id.is_some());
}

#[test]
fn scan_rebinds_folder_moves_to_existing_track_id() {
    let root = temp_test_dir("scan-rebinds-folder-move");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Artist").join("Album");
    std::fs::create_dir_all(&album_dir).unwrap();
    let original_path = album_dir.join("01 Intro.wav");
    std::fs::write(&original_path, b"not a real wav").unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();

    library.scan().unwrap();
    let original_track = library.tracks().unwrap().remove(0);
    let renamed_artist_dir = root.join("music").join("Renamed Artist");
    std::fs::rename(root.join("music").join("Artist"), &renamed_artist_dir).unwrap();
    let moved_path = renamed_artist_dir.join("Album").join("01 Intro.wav");

    let result = library.scan().unwrap();
    let tracks = library.tracks().unwrap();
    let (stored_id, stored_path, status): (i64, String, String) = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, path, status FROM tracks WHERE id = ?1",
            [original_track.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    };

    assert_eq!(result.removed, 0);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id, original_track.id);
    assert_eq!(stored_id, original_track.id);
    assert_eq!(
        std::fs::canonicalize(stored_path).unwrap(),
        std::fs::canonicalize(moved_path).unwrap()
    );
    assert_eq!(status, "available");
}

#[test]
fn scan_marks_missing_tracks_without_deleting_identity() {
    let root = temp_test_dir("scan-marks-missing");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Artist").join("Album");
    std::fs::create_dir_all(&album_dir).unwrap();
    let track_path = album_dir.join("01 Intro.wav");
    std::fs::write(&track_path, b"not a real wav").unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();

    library.scan().unwrap();
    let track_id = library.tracks().unwrap()[0].id;
    std::fs::remove_file(track_path).unwrap();

    let result = library.scan().unwrap();
    let public_tracks = library.tracks().unwrap();
    let status: (String, Option<i64>) = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT status, missing_since FROM tracks WHERE id = ?1",
            [track_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };

    assert_eq!(result.removed, 1);
    assert!(public_tracks.is_empty());
    assert_eq!(status.0, "missing");
    assert!(status.1.is_some());
}

#[test]
fn artists_include_history_popularity_by_display_artist() {
    let library = test_library("artist-history-popularity");
    let now = now_secs();
    let (album_id, track_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO artists (name, sort_name, created_at)
            VALUES ('Popular Artist', 'popular artist', ?1),
                   ('Track Guest', 'track guest', ?1)
            "#,
            [now],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, album_artist, sort_key, confidence, match_status,
                track_count, created_at, updated_at
            )
            VALUES ('History Album', 'Popular Artist', 'popular artist|history album',
                    80, 'local', 1, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
            INSERT INTO tracks (
                path, file_name, size_bytes, modified_secs, title, artist,
                album, album_artist, album_id, embedded_art, created_at, updated_at
            )
            VALUES ('/tmp/artist-history-popularity/01.flac', '01.flac', 1, 1,
                    'Popular Song', 'Track Guest', 'History Album',
                    'Popular Artist', ?1, 0, ?2, ?2)
            "#,
            params![album_id, now],
        )
        .unwrap();
        (album_id, conn.last_insert_rowid())
    };

    for (played_secs, counted) in [(120.0, true), (45.0, false)] {
        library
            .record_playback_history(PlaybackHistoryInput {
                profile_id: None,
                source: SourceRef::LocalTrack {
                    track_id,
                    file_name: Some("01.flac".to_string()),
                    title: Some("Popular Song".to_string()),
                    artist: Some("Track Guest".to_string()),
                    album: Some("History Album".to_string()),
                    album_artist: Some("Popular Artist".to_string()),
                    album_id: Some(album_id),
                    art_id: None,
                    duration_secs: Some(180.0),
                    ext_hint: Some("flac".to_string()),
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                zone_id: "local-core".to_string(),
                zone_name: "Local".to_string(),
                played_secs: Some(played_secs),
                duration_secs: Some(180.0),
                completed: counted,
                counted,
                radio: false,
            })
            .unwrap();
    }

    let artists = library.artists().unwrap();
    let popular_artist = artists
        .iter()
        .find(|artist| artist.name == "Popular Artist")
        .unwrap();
    let track_guest = artists
        .iter()
        .find(|artist| artist.name == "Track Guest")
        .unwrap();

    assert_eq!(popular_artist.play_count, 1);
    assert_eq!(popular_artist.listened_secs, 165.0);
    assert_eq!(track_guest.play_count, 0);
    assert_eq!(track_guest.listened_secs, 0.0);
}

#[test]
fn path_album_fallback_extracts_disc_number() {
    let music_dir = Path::new("/music");
    let path = music_dir
        .join("Artist")
        .join("Album")
        .join("Disk 02")
        .join("01 Intro.wav");

    let fallback = path_album_fallback(music_dir, &path);

    assert_eq!(fallback.title.as_deref(), Some("Album"));
    assert_eq!(fallback.disc_number, Some(2));
    assert_eq!(fallback.disc_folder_name.as_deref(), Some("Disk 02"));
}

#[test]
fn scan_gate_rejects_duplicate_active_scan_and_reopens_after_finish() {
    let library = test_library("scan-gate");

    assert!(library.try_begin_scan());
    let progress = library.scan_progress();
    assert!(progress.running);
    assert_eq!(progress.phase, "preparing");
    assert!(!library.try_begin_scan());
    assert!(library.scan().unwrap_err().contains("already running"));

    library.finish_active_scan(LibraryScanResult {
        scanned: 0,
        updated: 0,
        removed: 0,
    });
    let progress = library.scan_progress();
    assert!(!progress.running);
    assert!(progress.last_result.is_some());

    assert!(library.try_begin_scan());
    library.fail_active_scan("test scan failure");
    let progress = library.scan_progress();
    assert!(!progress.running);
    assert_eq!(progress.phase, "error");
    assert_eq!(progress.error.as_deref(), Some("test scan failure"));
}

#[test]
fn scan_groups_untagged_wav_disc_folders_under_album() {
    let root = temp_test_dir("wav-disc-scan");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Artist").join("Album");
    let disc1 = album_dir.join("DISK 1");
    let disc2 = album_dir.join("DISK 2");
    std::fs::create_dir_all(&disc1).unwrap();
    std::fs::create_dir_all(&disc2).unwrap();
    std::fs::write(disc1.join("01 Alpha.wav"), b"not a real wav").unwrap();
    std::fs::write(disc2.join("01 Beta.wav"), b"not a real wav").unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();

    let result = library.scan().unwrap();
    let albums = library.albums().unwrap();
    let detail = library.album_detail(albums[0].id).unwrap().unwrap();
    let discs: Vec<Option<i64>> = detail
        .tracks
        .iter()
        .map(|track| track.disc_number)
        .collect();

    assert_eq!(result.scanned, 2);
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].title, "Album");
    assert_eq!(discs, vec![Some(1), Some(2)]);
}

#[test]
fn scan_repairs_existing_disc_folder_album_split() {
    let root = temp_test_dir("wav-disc-repair");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Artist").join("Album");
    let disc1 = album_dir.join("DISK 1");
    let disc2 = album_dir.join("DISK 2");
    std::fs::create_dir_all(&disc1).unwrap();
    std::fs::create_dir_all(&disc2).unwrap();
    let path1 = disc1.join("01 Alpha.wav");
    let path2 = disc2.join("01 Beta.wav");
    std::fs::write(&path1, b"not a real wav").unwrap();
    std::fs::write(&path2, b"not a real wav").unwrap();
    let metadata1 = std::fs::metadata(&path1).unwrap();
    let metadata2 = std::fs::metadata(&path2).unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, sort_key, confidence, match_status, track_count, created_at, updated_at
                )
                VALUES ('DISK 1', 'unknown-artist|disk 1', 45, 'needs_review', 1, ?1, ?1),
                       ('DISK 2', 'unknown-artist|disk 2', 45, 'needs_review', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, album, album_id,
                    embedded_art, created_at, updated_at
                )
                VALUES (?1, '01 Alpha.wav', ?2, ?3, 'Alpha', 'DISK 1', 1, 0, ?7, ?7),
                       (?4, '01 Beta.wav', ?5, ?6, 'Beta', 'DISK 2', 2, 0, ?7, ?7)
                "#,
            params![
                path1.to_string_lossy(),
                metadata1.len() as i64,
                modified_secs(&metadata1).unwrap(),
                path2.to_string_lossy(),
                metadata2.len() as i64,
                modified_secs(&metadata2).unwrap(),
                now,
            ],
        )
        .unwrap();
    }

    let result = library.scan().unwrap();
    let albums = library.albums().unwrap();
    let detail = library.album_detail(albums[0].id).unwrap().unwrap();

    assert_eq!(result.updated, 2);
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].title, "Album");
    assert_eq!(
        detail
            .tracks
            .iter()
            .map(|track| track.disc_number)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
}

#[test]
fn scan_merges_existing_album_title_disc_suffixes() {
    let root = temp_test_dir("title-disc-suffix-merge");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Maria de Buenos Aires");
    let disc1 = album_dir.join("Disc One Files");
    let disc2 = album_dir.join("Disc Two Files");
    std::fs::create_dir_all(&disc1).unwrap();
    std::fs::create_dir_all(&disc2).unwrap();
    let path1 = disc1.join("01 Alevare.wav");
    let path2 = disc2.join("01 Tangata del alba.wav");
    std::fs::write(&path1, b"not a real wav").unwrap();
    std::fs::write(&path2, b"not a real wav").unwrap();
    let path1 = std::fs::canonicalize(path1).unwrap();
    let path2 = std::fs::canonicalize(path2).unwrap();
    let metadata1 = std::fs::metadata(&path1).unwrap();
    let metadata2 = std::fs::metadata(&path2).unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    id, title, album_artist, sort_key, confidence, match_status, track_count,
                    created_at, updated_at
                )
                VALUES
                    (1, 'María de Buenos Aires (disc 1)', 'Astor Piazzolla & Horacio Ferrer',
                     'astor piazzolla horacio ferrer|maria de buenos aires disc 1',
                     80, 'local', 1, ?1, ?1),
                    (2, 'María de Buenos Aires (disc 2)', 'Astor Piazzolla & Horacio Ferrer',
                     'astor piazzolla horacio ferrer|maria de buenos aires disc 2',
                     80, 'local', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, album, album_artist,
                    track_number, album_id, embedded_art, created_at, updated_at
                )
                VALUES
                    (?1, '01 Alevare.wav', ?2, ?3, 'Alevare',
                     'María de Buenos Aires (disc 1)', 'Astor Piazzolla & Horacio Ferrer',
                     1, 1, 0, ?7, ?7),
                    (?4, '01 Tangata del alba.wav', ?5, ?6, 'Tangata del alba',
                     'María de Buenos Aires (disc 2)', 'Astor Piazzolla & Horacio Ferrer',
                     1, 2, 0, ?7, ?7)
                "#,
            params![
                path1.to_string_lossy(),
                metadata1.len() as i64,
                modified_secs(&metadata1).unwrap(),
                path2.to_string_lossy(),
                metadata2.len() as i64,
                modified_secs(&metadata2).unwrap(),
                now,
            ],
        )
        .unwrap();
    }

    library.scan().unwrap();

    let albums = library.albums().unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].title, "María de Buenos Aires");
    assert_eq!(
        albums[0].album_artist.as_deref(),
        Some("Astor Piazzolla & Horacio Ferrer")
    );
    assert_eq!(albums[0].track_count, 2);
    let mut tracks = library.tracks().unwrap();
    tracks.sort_by_key(|track| (track.disc_number, track.track_number));
    assert_eq!(
        tracks
            .iter()
            .map(|track| (
                track.album.as_deref(),
                track.disc_number,
                track.track_number
            ))
            .collect::<Vec<_>>(),
        vec![
            (Some("María de Buenos Aires"), Some(1), Some(1)),
            (Some("María de Buenos Aires"), Some(2), Some(1)),
        ]
    );
}

#[test]
fn scan_does_not_strip_single_album_title_disc_suffix() {
    let root = temp_test_dir("title-disc-suffix-single");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Radiohead - In Rainbows (Disk 2)");
    std::fs::create_dir_all(&album_dir).unwrap();
    let path = album_dir.join("01 Mk 1.wav");
    std::fs::write(&path, b"not a real wav").unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    id, title, album_artist, sort_key, confidence, match_status, track_count,
                    created_at, updated_at
                )
                VALUES (
                    1, 'In Rainbows (Disk 2)', 'Radiohead',
                    'radiohead|in rainbows disk 2', 80, 'local', 1, ?1, ?1
                )
                "#,
            [now],
        )
        .unwrap();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, album, album_artist,
                    track_number, album_id, embedded_art, created_at, updated_at
                )
                VALUES (
                    ?1, '01 Mk 1.wav', ?2, ?3, 'Mk 1', 'In Rainbows (Disk 2)',
                    'Radiohead', 1, 1, 0, ?4, ?4
                )
                "#,
            params![
                path.to_string_lossy(),
                metadata.len() as i64,
                modified_secs(&metadata).unwrap(),
                now,
            ],
        )
        .unwrap();
    }

    library.scan().unwrap();

    let albums = library.albums().unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].title, "In Rainbows (Disk 2)");
    let detail = library.album_detail(albums[0].id).unwrap().unwrap();
    assert_eq!(
        detail.tracks[0].album.as_deref(),
        Some("In Rainbows (Disk 2)")
    );
    assert_eq!(detail.tracks[0].disc_number, None);
}

#[test]
fn scores_exact_album_and_artist_highly() {
    assert_eq!(
        confidence_score("Anima", "ANIMA", Some("Thom Yorke"), Some("Thom Yorke")),
        95
    );
}

#[test]
fn normalize_for_match_handles_ampersand_and_separators() {
    assert_eq!(
        normalize_for_match("Dollars & Cents"),
        normalize_for_match("Dollars and Cents")
    );
    assert_eq!(
        normalize_for_match("Pulk/Pull Revolving Doors"),
        normalize_for_match("Pulk_Pull Revolving Doors")
    );
}

#[test]
fn filename_prefix_stripper_handles_common_patterns() {
    assert_eq!(
        filename_without_track_prefix("08 Dollars & Cents.wav"),
        "Dollars & Cents"
    );
    assert_eq!(
        filename_without_track_prefix("03 - Pulk_Pull Revolving Doors.flac"),
        "Pulk_Pull Revolving Doors"
    );
    assert_eq!(
        filename_without_track_prefix("11 Life in a Glasshouse.wav"),
        "Life in a Glasshouse"
    );
}

#[test]
fn levenshtein_distance_matches_known_pairs() {
    assert_eq!(levenshtein("packt", "packet"), 1);
    assert_eq!(levenshtein("crushd", "crushed"), 1);
    assert_eq!(levenshtein("kitten", "sitting"), 3);
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("same", "same"), 0);
}

fn search_release(id: &str, rg: &str, track_count: i64, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "title": "Dots and Loops",
        "status": status,
        "track-count": track_count,
        "release-group": {"id": rg},
    })
}

fn release_detail(tracks: &[(&str, i64, f64)]) -> serde_json::Value {
    serde_json::json!({
        "id": "release-1",
        "title": "Homogenic",
        "status": "Official",
        "media": [{
            "position": 1,
            "tracks": tracks.iter().map(|(title, position, length_secs)| serde_json::json!({
                "title": title,
                "position": position,
                "length": (length_secs * 1000.0) as i64,
            })).collect::<Vec<_>>(),
        }],
    })
}

#[test]
fn edition_score_prefers_matching_track_count_and_official_status() {
    let exact = search_release("r1", "rg1", 10, "Official");
    let deluxe = search_release("r2", "rg1", 14, "Official");
    let bootleg = search_release("r3", "rg1", 10, "Bootleg");
    assert!(edition_score(90, &exact, 10) > edition_score(90, &deluxe, 10));
    assert!(edition_score(90, &exact, 10) > edition_score(90, &bootleg, 10));
    // A weaker text match with the right track count beats a perfect text
    // match with a very different track count.
    assert!(edition_score(85, &exact, 10) > edition_score(100, &deluxe, 10));
}

#[test]
fn release_group_dedupe_keeps_best_pressing_per_group() {
    let scored = vec![
        (80, search_release("r1", "rg1", 10, "Official")),
        (95, search_release("r2", "rg1", 10, "Official")),
        (90, search_release("r3", "rg2", 12, "Official")),
    ];
    let out = dedupe_by_release_group(scored);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].1.get("id").and_then(|v| v.as_str()), Some("r2"));
    assert_eq!(out[1].1.get("id").and_then(|v| v.as_str()), Some("r3"));
}

#[test]
fn release_track_count_handles_search_and_detail_shapes() {
    assert_eq!(
        release_track_count(&search_release("r1", "rg1", 10, "Official")),
        Some(10)
    );
    let detail = release_detail(&[("Hunter", 1, 240.0), ("Joga", 2, 307.0)]);
    assert_eq!(release_track_count(&detail), Some(2));
}

#[test]
fn metabrainz_folder_inference_strips_format_suffix() {
    let local = album("Radiohead - Amnesiac(WAV)", None, None);
    let inference =
        infer_metabrainz_lookup_terms(&local, Some("Radiohead - Amnesiac(WAV)".to_string()));

    assert_eq!(inference.artist.as_deref(), Some("Radiohead"));
    assert_eq!(inference.album, "Amnesiac");
    assert_eq!(
        inference.search_queries[0],
        "release:\"Amnesiac\" AND artist:\"Radiohead\""
    );
}

#[test]
fn metabrainz_folder_inference_handles_artist_dash_without_left_space() {
    let local = album("Atoms for Peace- AMOK", Some("Atoms For Peace"), None);
    let inference =
        infer_metabrainz_lookup_terms(&local, Some("Atoms for Peace- AMOK".to_string()));

    assert_eq!(inference.artist.as_deref(), Some("Atoms For Peace"));
    assert_eq!(inference.album, "AMOK");
    assert_eq!(
        inference.search_queries[0],
        "release:\"AMOK\" AND artist:\"Atoms For Peace\""
    );
}

#[test]
fn metabrainz_folder_inference_preserves_meaningful_parentheses() {
    let local = album("Radiohead - Street Spirit (Fade Out) (1996)", None, None);
    let inference = infer_metabrainz_lookup_terms(
        &local,
        Some("Radiohead - Street Spirit (Fade Out) (1996)".to_string()),
    );

    assert_eq!(inference.artist.as_deref(), Some("Radiohead"));
    assert_eq!(inference.album, "Street Spirit (Fade Out) (1996)");
}

#[test]
fn metabrainz_folder_inference_searches_colon_subtitle_variant() {
    let local = album("Hail to the Thief", Some("Radiohead"), None);
    let inference = infer_metabrainz_lookup_terms(
        &local,
        Some("Radiohead - Hail to the Thief (Live Recordings 2003-2009)".to_string()),
    );

    assert_eq!(
        inference.album,
        "Hail to the Thief (Live Recordings 2003-2009)"
    );
    assert!(
        inference.search_queries.contains(
            &"release:\"Hail to the Thief: Live Recordings 2003-2009\" AND artist:\"Radiohead\""
                .to_string()
        )
    );
}

#[test]
fn metabrainz_track_assisted_queries_use_filename_track_order() {
    let local = album("Hail To the Theif", Some("Radiohead"), None);
    let inference = infer_metabrainz_lookup_terms(
        &local,
        Some("Radiohead - Hail To the Theif(WAV)".to_string()),
    );
    let tracks = vec![
        ft(
            2,
            "A Punch Up at a Wedding",
            "11. A PUNCH UP AT A WEDDING.WAV",
        ),
        ft(1, "2 + 2 = 5", "01. 2 + 2 = 5.WAV"),
    ];

    let queries = track_assisted_metabrainz_queries(&inference, &tracks);

    assert_eq!(queries[0], "artist:\"Radiohead\" AND \"2 + 2 = 5\"");
    assert!(queries.contains(&"artist:\"Radiohead\" AND tracks:2".to_string()));
}

#[test]
fn extract_mb_tracks_reads_lengths_in_seconds() {
    let release = release_detail(&[("Joga", 1, 307.0)]);
    let tracks = extract_mb_tracks(&release);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].length_secs, Some(307.0));
}

#[test]
fn verify_release_accepts_matching_edition() {
    let release = release_detail(&[("Hunter", 1, 240.0), ("Joga", 2, 307.0)]);
    let mut t1 = tagged_ft(1, "Hunter", "01 Hunter.flac", 1);
    t1.duration_secs = Some(241.2);
    let mut t2 = tagged_ft(2, "Joga", "02 Joga.flac", 2);
    t2.duration_secs = Some(305.8);
    let evidence = verify_release_against_tracks(&release, &[t1, t2]);
    assert!(evidence.track_count_match);
    assert_eq!(evidence.paired, 2);
    assert_eq!(evidence.duration_within, 2);
    assert!(evidence.pass);
}

#[test]
fn verify_release_rejects_wrong_track_count() {
    // Deluxe edition with a bonus track vs. a 2-track local album.
    let release = release_detail(&[
        ("Hunter", 1, 240.0),
        ("Joga", 2, 307.0),
        ("Bonus Remix", 3, 412.0),
    ]);
    let mut t1 = tagged_ft(1, "Hunter", "01 Hunter.flac", 1);
    t1.duration_secs = Some(240.0);
    let mut t2 = tagged_ft(2, "Joga", "02 Joga.flac", 2);
    t2.duration_secs = Some(307.0);
    let evidence = verify_release_against_tracks(&release, &[t1, t2]);
    assert!(!evidence.track_count_match);
    assert!(!evidence.pass);
}

#[test]
fn verify_release_rejects_duration_mismatch() {
    // Same titles and count, but a different edit — durations way off.
    let release = release_detail(&[("Hunter", 1, 240.0), ("Joga", 2, 307.0)]);
    let mut t1 = tagged_ft(1, "Hunter", "01 Hunter.flac", 1);
    t1.duration_secs = Some(290.0);
    let mut t2 = tagged_ft(2, "Joga", "02 Joga.flac", 2);
    t2.duration_secs = Some(355.0);
    let evidence = verify_release_against_tracks(&release, &[t1, t2]);
    assert!(evidence.track_count_match);
    assert_eq!(evidence.duration_within, 0);
    assert!(!evidence.pass);
}

#[test]
fn verify_release_passes_without_durations() {
    // Untagged WAV rips often have no duration metadata in the DB; count and
    // title pairing alone must be able to clear the bar.
    let release = release_detail(&[("Hunter", 1, 240.0), ("Joga", 2, 307.0)]);
    let tracks = vec![
        tagged_ft(1, "Hunter", "01 Hunter.wav", 1),
        tagged_ft(2, "Joga", "02 Joga.wav", 2),
    ];
    let evidence = verify_release_against_tracks(&release, &tracks);
    assert_eq!(evidence.duration_checked, 0);
    assert!(evidence.pass);
}

#[test]
fn metabrainz_evidence_rejects_deluxe_track_count_as_strict() {
    let release = release_detail(&[
        ("Airbag", 1, 284.0),
        ("Paranoid Android", 2, 383.0),
        ("Bonus Track", 3, 300.0),
    ]);
    let mut t1 = tagged_ft(1, "Airbag", "01 Airbag.wav", 1);
    t1.duration_secs = Some(284.0);
    let mut t2 = tagged_ft(2, "Paranoid Android", "02 Paranoid Android.wav", 2);
    t2.duration_secs = Some(383.0);

    let evidence = metabrainz_evidence_for_release(&release, &[t1, t2]);

    assert!(!evidence.track_count_match);
    assert!(!evidence.auto_apply_eligible);
    assert!(
        evidence
            .warnings
            .iter()
            .any(|w| w == "Track count mismatch")
    );
}

#[test]
fn metabrainz_evidence_keeps_bootlegs_review_only() {
    let mut release = release_detail(&[("Airbag", 1, 284.0), ("Paranoid Android", 2, 383.0)]);
    release["status"] = serde_json::json!("Bootleg");
    let mut t1 = tagged_ft(1, "Airbag", "01 Airbag.wav", 1);
    t1.duration_secs = Some(284.0);
    let mut t2 = tagged_ft(2, "Paranoid Android", "02 Paranoid Android.wav", 2);
    t2.duration_secs = Some(383.0);

    let evidence = metabrainz_evidence_for_release(&release, &[t1, t2]);

    assert_eq!(evidence.release_status.as_deref(), Some("Bootleg"));
    assert!(evidence.track_count_match);
    assert!(!evidence.auto_apply_eligible);
    assert!(
        evidence
            .warnings
            .iter()
            .any(|w| w == "Bootleg / review only")
    );
}

#[test]
fn metabrainz_pairing_rejects_same_positions_with_wrong_titles() {
    let release = release_detail(&[
        ("Planet Telex", 1, 260.0),
        ("The Bends", 2, 240.0),
        ("High and Dry", 3, 257.0),
    ]);
    let tracks = vec![
        tagged_ft(1, "2 + 2 = 5 (Live)", "01 2 + 2 = 5 (Live).flac", 1),
        tagged_ft(
            2,
            "Sit Down. Stand Up (Live)",
            "02 Sit Down Stand Up (Live).flac",
            2,
        ),
        tagged_ft(
            3,
            "Sail to the Moon (Live)",
            "03 Sail to the Moon (Live).flac",
            3,
        ),
    ];

    let evidence = metabrainz_evidence_for_release(&release, &tracks);

    assert!(evidence.track_count_match);
    assert_eq!(evidence.paired_tracks, 0);
    assert!(!evidence.auto_apply_eligible);
}

#[test]
fn pair_tracks_accepts_live_suffix_with_matching_title_words() {
    let mb = vec![mb("2 + 2 = 5", 1, 1)];
    let mut local = tagged_ft(1, "2 + 2 = 5 (Live)", "01 2 + 2 = 5 (Live).flac", 1);
    local.artist = Some("Radiohead".to_string());
    let pairings = pair_tracks(&[local], &mb);

    assert_eq!(pairings.len(), 1);
}

#[tokio::test]
#[ignore = "requires the live musicbrainz.org API; run explicitly as an integration check"]
async fn metabrainz_test_unknown_album_does_not_store_candidates() {
    let library = test_library("metabrainz-readonly");
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (title, sort_key, confidence, match_status, track_count, created_at, updated_at)
            VALUES ('Unknown Album', 'unknown|readonly', 20, 'needs_review', 0, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        conn.last_insert_rowid()
    };

    let response = library
        .test_metabrainz_album(
            album_id,
            MetaBrainzTestRequest {
                refresh: Some(true),
            },
        )
        .await
        .unwrap()
        .unwrap();

    assert!(response.best_candidate.is_none());
    assert_eq!(library.match_candidates(album_id).unwrap().len(), 0);
}

#[tokio::test]
#[ignore = "requires the live musicbrainz.org API; run explicitly as an integration check"]
async fn metabrainz_test_uses_primary_local_version_tracks() {
    let library = test_library("metabrainz-primary-version");
    let now = now_secs();
    let (album_id, primary_version_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, album_artist, sort_key, confidence, match_status,
                track_count, created_at, updated_at
            )
            VALUES ('Unknown Album', 'Björk', 'bjork|unknown album',
                    20, 'needs_review', 4, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();

        let mut track_ids = Vec::new();
        for (folder, sample_rate, bit_depth) in [("cd", 44_100, 16), ("hires", 96_000, 24)] {
            for (track_number, title) in [(1, "Hidden Place"), (2, "Cocoon")] {
                conn.execute(
                    r#"
                    INSERT INTO tracks (
                        path, file_name, size_bytes, modified_secs, title, artist,
                        album, album_artist, track_number, disc_number, duration_secs,
                        sample_rate, bit_depth, format, album_id, embedded_art,
                        created_at, updated_at
                    )
                    VALUES (?1, ?2, 1, 1, ?3, 'Björk',
                            'Unknown Album', 'Björk', ?4, 1, 200.0,
                            ?5, ?6, 'FLAC', ?7, 0, ?8, ?8)
                    "#,
                    params![
                        format!("/tmp/metabrainz-primary-version/{folder}/{track_number:02}.flac"),
                        format!("{track_number:02} {title}.flac"),
                        title,
                        track_number,
                        sample_rate,
                        bit_depth,
                        album_id,
                        now,
                    ],
                )
                .unwrap();
                track_ids.push((
                    folder,
                    conn.last_insert_rowid(),
                    track_number,
                    title,
                    sample_rate,
                    bit_depth,
                ));
            }
        }

        let mut primary_version_id = 0;
        for (folder, sample_rate, bit_depth, source_label) in [
            ("cd", 44_100, 16, "CD quality"),
            ("hires", 96_000, 24, "Hi-Res"),
        ] {
            conn.execute(
                r#"
                INSERT INTO album_versions (
                    album_id, provider, provider_id, title, artist, track_count,
                    format, sample_rate, bit_depth, source_label, status,
                    payload_json, created_at, updated_at
                )
                VALUES (?1, 'local', ?2, 'Unknown Album', 'Björk', 2,
                        'FLAC', ?3, ?4, ?5, 'available', '{}', ?6, ?6)
                "#,
                params![album_id, folder, sample_rate, bit_depth, source_label, now],
            )
            .unwrap();
            let version_id = conn.last_insert_rowid();
            if folder == "hires" {
                primary_version_id = version_id;
            }
            for (_, track_id, track_number, title, track_sample_rate, track_bit_depth) in track_ids
                .iter()
                .filter(|(track_folder, ..)| *track_folder == folder)
            {
                conn.execute(
                    r#"
                    INSERT INTO version_tracks (
                        version_id, provider_track_id, local_track_id, title, artist,
                        track_number, disc_number, duration_secs, sample_rate, format,
                        bit_depth, status, created_at, updated_at
                    )
                    VALUES (?1, ?2, ?3, ?4, 'Björk', ?5, 1, 200.0, ?6, 'FLAC',
                            ?7, 'available', ?8, ?8)
                    "#,
                    params![
                        version_id,
                        track_id.to_string(),
                        track_id,
                        title,
                        track_number,
                        track_sample_rate,
                        track_bit_depth,
                        now,
                    ],
                )
                .unwrap();
            }
        }
        conn.execute(
            "UPDATE albums SET primary_version_id = ?2 WHERE id = ?1",
            params![album_id, primary_version_id],
        )
        .unwrap();
        (album_id, primary_version_id)
    };

    let response = library
        .test_metabrainz_album(album_id, MetaBrainzTestRequest::default())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.tracks.len(), 2);
    assert_eq!(
        response.version.as_ref().map(|version| version.id),
        Some(primary_version_id)
    );
    assert!(
        response
            .tracks
            .iter()
            .all(|track| track.sample_rate == Some(96_000) && track.bit_depth == Some(24))
    );
}

/// End-to-end Amnesiac scenario: WAV files with no TITLE tag (titles
/// derived from filename), an ampersand mismatch, a slash/underscore swap,
/// and stylized MB spellings. All 11 should pair.
#[test]
fn pair_tracks_handles_amnesiac_wav_quirks() {
    let mb = vec![
        mb("Packt Like Sardines in a Crushd Tin Box", 1, 1),
        mb("Pyramid Song", 2, 1),
        mb("Pulk/Pull Revolving Doors", 3, 1),
        mb("You and Whose Army?", 4, 1),
        mb("I Might Be Wrong", 5, 1),
        mb("Knives Out", 6, 1),
        mb("Morning Bell/Amnesiac", 7, 1),
        mb("Dollars and Cents", 8, 1),
        mb("Hunting Bears", 9, 1),
        mb("Like Spinning Plates", 10, 1),
        mb("Life in a Glasshouse", 11, 1),
    ];
    // Simulated DB after a WAV scan: titles came from the filename (no track
    // numbers stored), so Pass 1 has nothing to lean on.
    let files = vec![
        ft(
            1,
            "Packet Like Sardines in a Crushed Tin Box",
            "01 Packet Like Sardines in a Crushed Tin Box.wav",
        ),
        ft(2, "Pyramid Song", "02 Pyramid Song.wav"),
        ft(
            3,
            "Pull Pulk Revolving Doors",
            "03 Pulk_Pull Revolving Doors.wav",
        ),
        ft(4, "You And Whose Army", "04 You and Whose Army.wav"),
        ft(5, "I Might Be Wrong", "05 I Might Be Wrong.wav"),
        ft(6, "Knives Out", "06 Knives Out.wav"),
        ft(7, "Morning Bell_Amnesiac", "07 Morning Bell_Amnesiac.wav"),
        ft(8, "Dollars & Cents", "08 Dollars & Cents.wav"),
        ft(9, "Hunting Bears", "09 Hunting Bears.wav"),
        ft(10, "Like Spinning Plates", "10 Like Spinning Plates.wav"),
        ft(11, "Life in a Glasshouse", "11 Life in a Glasshouse.wav"),
    ];
    let pairings = pair_tracks(&files, &mb);
    assert_eq!(
        pairings.len(),
        11,
        "all 11 tracks should pair; got {}: {:?}",
        pairings.len(),
        pairings
            .iter()
            .map(|p| (p.file_index, p.mb_index, p.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn metabrainz_apply_order_prefers_complete_wav_filename_numbers() {
    let mut files = vec![
        ft(10, "Bloom", "01 - Bloom.wav"),
        ft(11, "The Daily Mail", "02 - The Daily Mail.wav"),
        ft(12, "Feral", "03 - Feral.wav"),
        ft(13, "Little by Little", "04 - Little by Little.wav"),
        ft(14, "Codex", "05 - Codex.wav"),
    ];
    files[1].track_number = Some(5);
    files[4].track_number = Some(2);

    let order = complete_local_track_number_order(&files).expect("complete local WAV order");

    assert_eq!(order.get(&10), Some(&1));
    assert_eq!(order.get(&11), Some(&2));
    assert_eq!(order.get(&12), Some(&3));
    assert_eq!(order.get(&13), Some(&4));
    assert_eq!(order.get(&14), Some(&5));
}

#[test]
fn metabrainz_apply_order_rejects_incomplete_filename_numbers() {
    let files = vec![
        ft(10, "Bloom", "01 - Bloom.wav"),
        ft(11, "The Daily Mail", "02 - The Daily Mail.wav"),
        ft(12, "Feral", "Feral.wav"),
    ];

    assert!(complete_local_track_number_order(&files).is_none());
}

fn numbered_qtrack(id: u64, title: &str, n: u32) -> QobuzTrack {
    let mut track = qtrack(id, title, "Homogenic", "Björk");
    track.track_number = Some(n);
    track.disc_number = Some(1);
    track
}

#[test]
fn itunes_artwork_url_upgrades_size_segment() {
    let url100 = "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg";
    assert_eq!(
        upgrade_artwork_url(url100, "3000x3000bb").as_deref(),
        Some("https://is1-ssl.mzstatic.com/image/thumb/Music/v4/ab/cd/source/3000x3000bb.jpg")
    );
    assert_eq!(
        upgrade_artwork_url(url100, "100000x100000-999").as_deref(),
        Some(
            "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/ab/cd/source/100000x100000-999.jpg"
        )
    );
    // Unrecognized shapes are left alone rather than guessed at.
    assert_eq!(
        upgrade_artwork_url("https://example.com/cover", "3000x3000bb"),
        None
    );
}

#[test]
fn itunes_artwork_url_rejects_untrusted_sources() {
    let cases = [
        "http://is1-ssl.mzstatic.com/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://127.0.0.1/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://192.168.1.5/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://169.254.169.254/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://mzstatic.com.evil.test/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://user:pass@is1-ssl.mzstatic.com/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://is1-ssl.mzstatic.com:444/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://is1-ssl.mzstatic.com/not-art/Music/v4/ab/cd/source/100x100bb.jpg",
        "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/ab/cd/source/100x100bb.svg",
        "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/ab/cd/source/100x100bb.jpg?target=evil",
    ];

    for url in cases {
        assert_eq!(upgrade_artwork_url(url, "3000x3000bb"), None, "{url}");
    }
}

#[test]
fn itunes_search_result_requires_exact_title_artist_and_count() {
    let result = serde_json::json!({
        "wrapperType": "collection",
        "collectionName": "Homogenic",
        "artistName": "Björk",
        "trackCount": 10,
        "artworkUrl100": "https://example.com/100x100bb.jpg",
    });
    assert!(itunes_result_matches(
        &result,
        Some("Björk"),
        "Homogenic",
        10
    ));
    // Track count disagreement (different edition) is rejected.
    assert!(!itunes_result_matches(
        &result,
        Some("Björk"),
        "Homogenic",
        12
    ));
    // Fuzzy-but-not-exact titles are rejected; wrong covers are worse than none.
    assert!(!itunes_result_matches(
        &result,
        Some("Björk"),
        "Homogenic Live",
        10
    ));
    assert!(!itunes_result_matches(
        &result,
        Some("Bjork Tribute"),
        "Homogenic",
        10
    ));
    // No artist to verify against → no match.
    assert!(!itunes_result_matches(&result, None, "Homogenic", 10));
}

#[test]
fn barcode_normalization_matches_upc_and_ean_variants() {
    // EAN-13 vs UPC-12: same code, leading zero.
    assert_eq!(
        normalize_barcode("0827954030621"),
        normalize_barcode("827954030621")
    );
    assert_eq!(
        normalize_barcode("0 82795-40306 2 1"),
        normalize_barcode("827954030621")
    );
    assert_ne!(
        normalize_barcode("827954030621"),
        normalize_barcode("827954030638")
    );
}

#[test]
fn qobuz_evidence_complete_accepts_exact_edition() {
    let detail = QobuzAlbumDetail {
        album: qalbum("q1", "Homogenic", "Björk", Some(1997)),
        tracks: vec![
            numbered_qtrack(11, "Hunter", 1),
            numbered_qtrack(12, "Joga", 2),
        ],
    };
    let locals = vec![
        tagged_ft(1, "Hunter", "01 Hunter.flac", 1),
        tagged_ft(2, "Joga", "02 Joga.flac", 2),
    ];
    assert!(qobuz_evidence_complete(
        &album("Homogenic", Some("Björk"), Some(1997)),
        &locals,
        &detail
    ));
}

#[test]
fn qobuz_evidence_complete_rejects_deluxe_edition_and_wrong_artist() {
    let deluxe = QobuzAlbumDetail {
        album: qalbum("q1", "Homogenic", "Björk", Some(1997)),
        tracks: vec![
            numbered_qtrack(11, "Hunter", 1),
            numbered_qtrack(12, "Joga", 2),
            numbered_qtrack(13, "Joga (Remix)", 3),
        ],
    };
    let locals = vec![
        tagged_ft(1, "Hunter", "01 Hunter.flac", 1),
        tagged_ft(2, "Joga", "02 Joga.flac", 2),
    ];
    let summary = album("Homogenic", Some("Björk"), Some(1997));
    assert!(!qobuz_evidence_complete(&summary, &locals, &deluxe));

    let wrong_artist = QobuzAlbumDetail {
        album: qalbum("q2", "Homogenic", "Some Tribute Band", Some(2005)),
        tracks: vec![
            numbered_qtrack(11, "Hunter", 1),
            numbered_qtrack(12, "Joga", 2),
        ],
    };
    assert!(!qobuz_evidence_complete(&summary, &locals, &wrong_artist));
    // Album with no artist at all can never clear the gate.
    assert!(!qobuz_evidence_complete(
        &album("Homogenic", None, None),
        &locals,
        &wrong_artist
    ));
}

#[test]
fn qobuz_evidence_complete_rejects_numbered_but_wrong_track_titles() {
    let wrong_titles = QobuzAlbumDetail {
        album: qalbum("q3", "Homogenic", "Björk", Some(1997)),
        tracks: vec![
            numbered_qtrack(11, "Unrelated Song", 1),
            numbered_qtrack(12, "Another Song", 2),
        ],
    };
    let locals = vec![
        tagged_ft(1, "Hunter", "01 Hunter.flac", 1),
        tagged_ft(2, "Joga", "02 Joga.flac", 2),
    ];

    assert!(!qobuz_evidence_complete(
        &album("Homogenic", Some("Björk"), Some(1997)),
        &locals,
        &wrong_titles
    ));
}

#[test]
fn qobuz_barcode_match_still_requires_supporting_evidence() {
    let library = test_library("qobuz-barcode-supporting-evidence");
    let mut summary = album("Homogenic", Some("Björk"), Some(1997));
    summary.mb_barcode = Some("0827954030621".to_string());
    let mut wrong_album = qalbum("q4", "Debut", "Björk", Some(1993));
    wrong_album.upc = Some("827954030621".to_string());
    let detail = QobuzAlbumDetail {
        album: wrong_album,
        tracks: vec![
            numbered_qtrack(31, "Human Behaviour", 1),
            numbered_qtrack(32, "Venus as a Boy", 2),
        ],
    };
    let locals = vec![
        tagged_ft(1, "Hunter", "01 Hunter.flac", 1),
        tagged_ft(2, "Joga", "02 Joga.flac", 2),
    ];

    let assessment = library.qobuz_link_assessment_for_metadata(&summary, &locals, &detail);

    assert_eq!(assessment.barcode_match, Some(true));
    assert!(!assessment.auto_link);
}

#[test]
fn qobuz_score_accepts_exact_album() {
    let album = album("Homogenic", Some("Björk"), Some(1997));
    let local = vec![
        tagged_ft(1, "Hunter", "01 Hunter.flac", 1),
        tagged_ft(2, "Jóga", "02 Joga.flac", 2),
        tagged_ft(3, "Unravel", "03 Unravel.flac", 3),
    ];
    let qobuz = qalbum("qbz1", "Homogenic", "Björk", Some(1997));
    let qtracks = vec![
        qtrack(11, "Hunter", "Homogenic", "Björk"),
        qtrack(12, "Jóga", "Homogenic", "Björk"),
        qtrack(13, "Unravel", "Homogenic", "Björk"),
    ];
    assert!(score_qobuz_album_match(&album, &local, &qobuz, &qtracks) >= 95);
}

#[test]
fn qobuz_score_penalizes_wrong_edition() {
    let album = album("Homogenic", Some("Björk"), Some(1997));
    let local = vec![
        tagged_ft(1, "Hunter", "01 Hunter.flac", 1),
        tagged_ft(2, "Jóga", "02 Joga.flac", 2),
        tagged_ft(3, "Unravel", "03 Unravel.flac", 3),
    ];
    let qobuz = qalbum("qbz2", "Homogenic Live", "Björk", Some(2003));
    let qtracks = vec![
        qtrack(21, "Overture", "Homogenic Live", "Björk"),
        qtrack(22, "All Is Full of Love", "Homogenic Live", "Björk"),
        qtrack(23, "Pluto", "Homogenic Live", "Björk"),
    ];
    assert!(score_qobuz_album_match(&album, &local, &qobuz, &qtracks) < 70);
}

#[test]
fn qobuz_pairing_falls_back_to_title_for_missing_numbers() {
    let local = vec![
        ft(1, "Hunter", "01 Hunter.flac"),
        ft(2, "Joga", "02 Joga.flac"),
    ];
    let qtracks = vec![
        qtrack(11, "Hunter", "Homogenic", "Björk"),
        qtrack(12, "Jóga", "Homogenic", "Björk"),
    ];
    let pairs = pair_qobuz_tracks(&local, &qtracks);
    assert_eq!(pairs.len(), 2);
    assert!(pairs.iter().all(|p| p.confidence >= 80));
}

#[test]
fn qobuz_pairing_leaves_extra_qobuz_track_unmapped() {
    let local = vec![tagged_ft(1, "Hunter", "01 Hunter.flac", 1)];
    let qtracks = vec![
        qtrack(11, "Hunter", "Homogenic", "Björk"),
        qtrack(12, "Jóga", "Homogenic", "Björk"),
    ];
    let pairs = pair_qobuz_tracks(&local, &qtracks);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].qobuz_track_id, "11");
}

#[test]
fn image_dimensions_handles_invalid_cover_bytes() {
    assert_eq!(image_dimensions(b"not an image"), (None, None));
}

#[test]
fn qobuz_album_lookup_falls_back_to_version_rows() {
    let library = test_library("qobuz-version-lookup");
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, year, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Dots And Loops', 'Stereolab', 'stereolab|dots and loops',
                        1997, 100, 'local', 0, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO album_versions (
                    album_id, provider, provider_id, title, artist, year, track_count,
                    format, sample_rate, bit_depth, source_label, status,
                    payload_json, created_at, updated_at
                )
                VALUES (?1, 'qobuz', 'yntirca1wv5oc', 'Dots And Loops', 'Stereolab',
                        1997, 10, 'FLAC', 44100, 16, 'Qobuz', 'available', '{}', ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        album_id
    };

    let detail = library
        .album_by_qobuz_id("qobuz:album:yntirca1wv5oc")
        .unwrap()
        .unwrap();

    assert_eq!(detail.album.id, album_id);
    assert!(detail.versions.iter().any(|v| v.provider == "qobuz"));
}

#[test]
fn qobuz_matched_album_is_not_counted_as_unmatched() {
    let library = test_library("qobuz-summary-match");
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    qobuz_album_id, qobuz_match_status, qobuz_match_confidence,
                    track_count, created_at, updated_at
                )
                VALUES ('Matched', 'Artist', 'artist|matched', 80, 'local',
                        'qbz-matched', 'matched', 95, 0, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Local Only', 'Artist', 'artist|local only', 80, 'local', 0, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
    }

    let summary = library.summary().unwrap();
    assert_eq!(summary.albums, 2);
    assert_eq!(summary.unmatched_albums, 1);
}

#[test]
fn summary_propagates_database_failures_instead_of_returning_zeroes() {
    let library = test_library("summary-database-error");
    library
        .conn
        .lock()
        .unwrap()
        .execute("DROP TABLE albums", [])
        .unwrap();

    let error = library.summary().unwrap_err();
    assert!(error.contains("count library albums"), "{error}");
}

#[tokio::test(flavor = "current_thread")]
async fn async_library_boundary_runs_work_off_the_tokio_worker() {
    let library = std::sync::Arc::new(test_library("async-blocking-boundary"));
    let request_thread = std::thread::current().id();

    let database_thread = library
        .run_blocking(|_| Ok(std::thread::current().id()))
        .await
        .unwrap();
    let second_database_thread = library
        .run_blocking(|_| Ok(std::thread::current().id()))
        .await
        .unwrap();

    assert_ne!(database_thread, request_thread);
    assert_eq!(database_thread, second_database_thread);
}

#[test]
fn qobuz_review_candidate_does_not_become_canonical_metadata() {
    let library = test_library("qobuz-review-canonical");
    let now = now_secs();
    let detail = QobuzAlbumDetail {
        album: qalbum("qbz-review", "Remote Album", "Remote Artist", Some(2001)),
        tracks: vec![qtrack(11, "Remote Track", "Remote Album", "Remote Artist")],
    };
    let payload = serde_json::to_string(&detail).unwrap();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, year, confidence, match_status,
                    qobuz_album_id, qobuz_match_status, qobuz_match_confidence,
                    qobuz_payload_json, track_count, created_at, updated_at
                )
                VALUES ('Local Album', 'Local Artist', 'local artist|local album',
                        2000, 80, 'local', 'qbz-review', 'needs_review', 74,
                        ?1, 0, ?2, ?2)
                "#,
            params![payload, now],
        )
        .unwrap();
        conn.last_insert_rowid()
    };

    let local = library.album(album_id).unwrap().unwrap();
    assert!(library.canonical_album(&local).unwrap().is_none());
    assert!(library.canonical_tracks(&local, &[]).unwrap().is_empty());

    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            "UPDATE albums SET qobuz_match_status = 'matched' WHERE id = ?1",
            [album_id],
        )
        .unwrap();
    }
    let matched = library.album(album_id).unwrap().unwrap();
    let canonical = library.canonical_album(&matched).unwrap().unwrap();
    assert_eq!(canonical.title, "Remote Album");
}

#[test]
fn local_playback_metadata_uses_edited_album_fields() {
    let library = test_library("local-playback-edits");
    let now = now_secs();
    let (album_id, track_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Original Album', NULL, 'unknown-artist|original album',
                        70, 'local', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, album_id, embedded_art, created_at, updated_at
                )
                VALUES ('/tmp/local-playback-edits/01.wav', '01.wav', 1, 1,
                        'Song', NULL, 'Original Album', NULL, ?1, 0, ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        (album_id, conn.last_insert_rowid())
    };

    library
        .record_playback_history(PlaybackHistoryInput {
            profile_id: None,
            source: SourceRef::LocalTrack {
                track_id,
                file_name: Some("song.wav".to_string()),
                title: Some("Song".to_string()),
                artist: None,
                album: Some("Original Album".to_string()),
                album_artist: None,
                album_id: Some(album_id),
                art_id: None,
                duration_secs: Some(180.0),
                ext_hint: Some("wav".to_string()),
                radio: false,
                radio_context: None,
                playlist_context: None,
            },
            zone_id: "local-core".to_string(),
            zone_name: "Local".to_string(),
            played_secs: Some(60.0),
            duration_secs: Some(180.0),
            completed: false,
            counted: true,
            radio: false,
        })
        .unwrap();

    library
        .update_album(
            album_id,
            AlbumEdit {
                title: Some("Edited Album".to_string()),
                album_artist: Some("Edited Artist".to_string()),
                year: None,
                tracks: Vec::new(),
            },
        )
        .unwrap();

    let tags = library.tags_for_track_id(track_id).unwrap().unwrap();
    assert_eq!(tags.0.as_deref(), Some("Song"));
    assert_eq!(tags.1.as_deref(), Some("Edited Artist"));
    assert_eq!(tags.2.as_deref(), Some("Edited Album"));

    let source = library.source_ref_for_track_id(track_id).unwrap().unwrap();
    match source {
        SourceRef::LocalTrack { artist, album, .. } => {
            assert_eq!(artist.as_deref(), Some("Edited Artist"));
            assert_eq!(album.as_deref(), Some("Edited Album"));
        }
        SourceRef::QobuzTrack { .. } => panic!("expected local track source"),
    }

    let recent = library.recent_playback_history(10, false).unwrap();
    assert_eq!(recent[0].artist.as_deref(), Some("Edited Artist"));
    assert_eq!(recent[0].album.as_deref(), Some("Edited Album"));
}

#[test]
fn manual_album_edit_updates_and_clears_year() {
    let library = test_library("manual-edit-year");
    let now = now_secs();
    let (album_id, track_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, year, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Vespertine', 'Björk', 'bjork|vespertine', 2019,
                        80, 'local', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, year, album_id, embedded_art, created_at, updated_at
                )
                VALUES ('/tmp/manual-edit-year/01.wav', '01.wav', 1, 1,
                        'Hidden Place', 'Björk', 'Vespertine', 'Björk', 2019,
                        ?1, 0, ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        (album_id, conn.last_insert_rowid())
    };

    library
        .update_album(
            album_id,
            AlbumEdit {
                title: None,
                album_artist: None,
                year: Some(Some(2001)),
                tracks: Vec::new(),
            },
        )
        .unwrap();

    let album = library.album(album_id).unwrap().unwrap();
    assert_eq!(album.year, Some(2001));
    let track_year: Option<i32> = {
        let conn = library.conn.lock().unwrap();
        conn.query_row("SELECT year FROM tracks WHERE id = ?1", [track_id], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert_eq!(track_year, Some(2001));

    library
        .update_album(
            album_id,
            AlbumEdit {
                title: None,
                album_artist: None,
                year: Some(None),
                tracks: Vec::new(),
            },
        )
        .unwrap();

    let album = library.album(album_id).unwrap().unwrap();
    assert_eq!(album.year, None);
}

#[test]
fn manual_album_edit_refreshes_search_index_and_local_versions() {
    let library = test_library("manual-edit-syncs-derived-rows");
    let now = now_secs();
    let (album_id, track_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Original Album', 'Original Artist', 'original artist|original album',
                        80, 'local', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, track_number, disc_number, album_id,
                    embedded_art, created_at, updated_at
                )
                VALUES ('/tmp/manual-edit-syncs-derived-rows/01.wav', '01.wav', 1, 1,
                        'Original Song', 'Original Artist', 'Original Album',
                        'Original Artist', 1, 1, ?1, 0, ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        (album_id, conn.last_insert_rowid())
    };

    library
        .update_album(
            album_id,
            AlbumEdit {
                title: Some("Edited Album".to_string()),
                album_artist: Some("Edited Artist".to_string()),
                year: None,
                tracks: vec![TrackEdit {
                    id: track_id,
                    title: "Edited Song".to_string(),
                    artist: Some("Edited Track Artist".to_string()),
                    track_number: Some(2),
                    disc_number: Some(1),
                }],
            },
        )
        .unwrap();

    let (fts_title, fts_artist, fts_album, fts_album_artist): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT title, artist, album, album_artist FROM tracks_fts WHERE track_id = ?1",
            [track_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(fts_title, "Edited Song");
    assert_eq!(fts_artist.as_deref(), Some("Edited Track Artist"));
    assert_eq!(fts_album.as_deref(), Some("Edited Album"));
    assert_eq!(fts_album_artist.as_deref(), Some("Edited Artist"));

    let version_track: (String, Option<String>, Option<i64>) = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            r#"
            SELECT vt.title, vt.artist, vt.track_number
            FROM version_tracks vt
            JOIN album_versions v ON v.id = vt.version_id
            WHERE v.album_id = ?1 AND vt.local_track_id = ?2
            "#,
            params![album_id, track_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(version_track.0, "Edited Song");
    assert_eq!(version_track.1.as_deref(), Some("Edited Track Artist"));
    assert_eq!(version_track.2, Some(2));
}

#[test]
fn rescan_preserves_curated_album_and_track_metadata() {
    let root = temp_test_dir("rescan-preserves-curated");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Artist").join("Raw Album");
    std::fs::create_dir_all(&album_dir).unwrap();
    let path = album_dir.join("01 Raw Title.wav");
    std::fs::write(&path, b"not a real wav").unwrap();
    let canonical_path = std::fs::canonicalize(&path).unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Curated Album', 'Curated Artist', 'curated artist|curated album',
                        100, 'matched', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, track_number, disc_number, album_id,
                    embedded_art, created_at, updated_at
                )
                VALUES (?1, '01 Raw Title.wav', 1, 0, 'Curated Title',
                        'Curated Track Artist', 'Curated Album', 'Curated Artist',
                        7, 2, ?2, 0, ?3, ?3)
                "#,
            params![canonical_path.to_string_lossy(), album_id, now],
        )
        .unwrap();
        album_id
    };

    let result = library.scan().unwrap();
    let detail = library.album_detail(album_id).unwrap().unwrap();

    assert_eq!(result.updated, 1);
    assert_eq!(detail.album.title, "Curated Album");
    assert_eq!(detail.album.album_artist.as_deref(), Some("Curated Artist"));
    assert_eq!(detail.tracks[0].title, "Curated Title");
    assert_eq!(
        detail.tracks[0].artist.as_deref(),
        Some("Curated Track Artist")
    );
    assert_eq!(detail.tracks[0].track_number, Some(7));
    assert_eq!(detail.tracks[0].disc_number, Some(2));
    assert_eq!(detail.tracks[0].duration_secs, None);
    assert_eq!(metadata.len(), b"not a real wav".len() as u64);
}

#[test]
fn user_uploaded_cover_overrides_existing_canonical_art() {
    let library = test_library("user-cover-canonical");
    let now = now_secs();
    let existing_art = library
        .save_artwork(
            &TrackCover {
                mime: "image/png".to_string(),
                data: tiny_blue_png(),
            },
            "itunes",
        )
        .unwrap();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    canonical_art_id, track_count, created_at, updated_at
                )
                VALUES ('Album', 'Artist', 'artist|album', 100, 'matched',
                        ?1, 0, ?2, ?2)
                "#,
            params![existing_art, now],
        )
        .unwrap();
        conn.last_insert_rowid()
    };

    let cover_data = tiny_png();
    let detail = library
        .set_album_cover(album_id, cover_data, "image/png")
        .unwrap()
        .unwrap();
    let canonical_art_id: Option<i64> = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT canonical_art_id FROM albums WHERE id = ?1",
            [album_id],
            |row| row.get(0),
        )
        .unwrap()
    };

    assert_ne!(detail.album.art_id, Some(existing_art));
    assert_eq!(canonical_art_id, detail.album.art_id);
}

#[test]
fn user_uploaded_cover_rejects_svg_content() {
    let library = test_library("user-cover-rejects-svg");
    let album_id = insert_minimal_album(&library, "SVG Album");
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;

    let error = library
        .set_album_cover(album_id, svg.to_vec(), "image/svg+xml")
        .unwrap_err();

    assert!(error.contains("JPEG, PNG, or WebP"));
}

#[test]
fn user_uploaded_cover_rejects_spoofed_raster_mime() {
    let library = test_library("user-cover-rejects-spoofed-mime");
    let album_id = insert_minimal_album(&library, "Spoofed Album");
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;

    let error = library
        .set_album_cover(album_id, svg.to_vec(), "image/jpeg")
        .unwrap_err();

    assert!(error.contains("bytes are not a supported raster image"));
}

#[test]
fn user_uploaded_cover_stores_canonical_safe_mime() {
    let library = test_library("user-cover-canonical-mime");
    let album_id = insert_minimal_album(&library, "Canonical MIME Album");

    let detail = library
        .set_album_cover(album_id, tiny_png(), "image/png; charset=binary")
        .unwrap()
        .unwrap();
    let art_id = detail.album.art_id.unwrap();
    let (mime, width, height): (String, Option<i64>, Option<i64>) = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT mime, width, height FROM artworks WHERE id = ?1",
            [art_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    };

    assert_eq!(mime, "image/png");
    assert_eq!(width, Some(1));
    assert_eq!(height, Some(1));
}

#[test]
fn user_uploaded_cover_locks_out_automatic_canonical_upgrades() {
    let library = test_library("user-cover-locks-auto-upgrades");
    let album_id = insert_minimal_album(&library, "Locked Cover Album");
    let detail = library
        .set_album_cover(album_id, tiny_png(), "image/png")
        .unwrap()
        .unwrap();
    let user_art_id = detail.album.art_id.unwrap();
    let bigger_art_id = library
        .save_artwork(
            &TrackCover {
                mime: "image/png".to_string(),
                data: png_of_size(1600, 1600, [0, 0, 255, 255]),
            },
            "itunes",
        )
        .unwrap();

    let improved = library
        .apply_canonical_art_if_better(album_id, bigger_art_id)
        .unwrap();
    let (canonical_art_id, art_locked): (Option<i64>, i64) = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT canonical_art_id, art_locked FROM albums WHERE id = ?1",
            [album_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };

    assert!(!improved);
    assert_eq!(canonical_art_id, Some(user_art_id));
    assert_eq!(art_locked, 1);
}

#[test]
fn folder_cover_detection_handles_case_and_album_named_images() {
    let library = test_library("folder-cover-case");
    let root = temp_test_dir("folder-cover-case-art");
    let mixed_case_dir = root.join("mixed");
    let album_named_dir = root.join("album-named");
    std::fs::create_dir_all(&mixed_case_dir).unwrap();
    std::fs::create_dir_all(&album_named_dir).unwrap();
    std::fs::write(mixed_case_dir.join("Cover.PNG"), tiny_png()).unwrap();
    std::fs::write(album_named_dir.join("ANIMA.png"), tiny_blue_png()).unwrap();

    let mixed_case_art = library.load_folder_cover(&mixed_case_dir).unwrap();
    let album_named_art = library.load_folder_cover(&album_named_dir).unwrap();

    assert!(mixed_case_art.is_some());
    assert!(album_named_art.is_some());
    assert_ne!(mixed_case_art, album_named_art);
}

#[test]
fn stale_canonical_art_recovers_from_album_folder_cover() {
    let root = temp_test_dir("stale-canonical-folder-cover");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Artist").join("Album");
    std::fs::create_dir_all(&album_dir).unwrap();
    let track_path = album_dir.join("01 Intro.wav");
    std::fs::write(&track_path, b"not a real wav").unwrap();
    std::fs::write(album_dir.join("Artwork.png"), tiny_png()).unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();
    let stale_art_id = library
        .save_artwork(
            &TrackCover {
                mime: "image/png".to_string(),
                data: tiny_blue_png(),
            },
            "itunes",
        )
        .unwrap();
    let stale_path: String = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT path FROM artworks WHERE id = ?1",
            [stale_art_id],
            |row| row.get(0),
        )
        .unwrap()
    };
    std::fs::remove_file(root.join("art").join(stale_path)).unwrap();
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    canonical_art_id, track_count, created_at, updated_at
                )
                VALUES ('Album', 'Artist', 'artist|album', 100, 'matched',
                        ?1, 1, ?2, ?2)
                "#,
            params![stale_art_id, now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, album_id, embedded_art, created_at, updated_at
                )
                VALUES (?1, '01 Intro.wav', 1, 1, 'Intro', 'Artist', 'Album',
                        'Artist', ?2, 0, ?3, ?3)
                "#,
            params![track_path.to_string_lossy(), album_id, now],
        )
        .unwrap();
        album_id
    };

    let recovered = library.art(stale_art_id).unwrap();
    let detail = library.album_detail(album_id).unwrap().unwrap();

    assert_eq!(
        recovered.map(|(mime, _)| mime).as_deref(),
        Some("image/png")
    );
    assert!(detail.album.art_id.is_some());
    assert_ne!(detail.album.art_id, Some(stale_art_id));
    assert_eq!(detail.album.canonical_art_id, detail.album.art_id);
}

#[test]
fn reset_album_to_file_tags_clears_stale_canonical_art() {
    let root = temp_test_dir("reset-clears-stale-canonical");
    let music_dir = root.join("music");
    let album_dir = music_dir.join("Artist").join("Album");
    std::fs::create_dir_all(&album_dir).unwrap();
    let track_path = album_dir.join("01 Intro.wav");
    std::fs::write(&track_path, b"not a real wav").unwrap();
    std::fs::write(album_dir.join("Cover.png"), tiny_png()).unwrap();
    let library = Library::new(root.join("library.db"), vec![music_dir], root.join("art")).unwrap();
    let stale_art_id = library
        .save_artwork(
            &TrackCover {
                mime: "image/png".to_string(),
                data: tiny_blue_png(),
            },
            "itunes",
        )
        .unwrap();
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    canonical_art_id, track_count, created_at, updated_at
                )
                VALUES ('Album', 'Artist', 'artist|album', 100, 'matched',
                        ?1, 1, ?2, ?2)
                "#,
            params![stale_art_id, now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, album_id, embedded_art, created_at, updated_at
                )
                VALUES (?1, '01 Intro.wav', 1, 1, 'Intro', 'Artist', 'Album',
                        'Artist', ?2, 0, ?3, ?3)
                "#,
            params![track_path.to_string_lossy(), album_id, now],
        )
        .unwrap();
        album_id
    };

    let detail = library.reset_album_to_file_tags(album_id).unwrap().unwrap();
    let canonical_art_id: Option<i64> = {
        let conn = library.conn.lock().unwrap();
        conn.query_row(
            "SELECT canonical_art_id FROM albums WHERE id = ?1",
            [album_id],
            |row| row.get(0),
        )
        .unwrap()
    };

    assert!(detail.album.art_id.is_some());
    assert_ne!(detail.album.art_id, Some(stale_art_id));
    assert_eq!(canonical_art_id, None);
}

#[test]
fn save_artwork_refuses_unsafe_cover_values() {
    let library = test_library("save-artwork-refuses-unsafe");
    let error = library
        .save_artwork(
            &TrackCover {
                mime: "text/html".to_string(),
                data: b"<!doctype html><script>alert(1)</script>".to_vec(),
            },
            "embedded",
        )
        .unwrap_err();

    assert!(error.contains("JPEG, PNG, or WebP"));
}

#[test]
fn art_lookup_hides_preexisting_unsafe_rows() {
    let library = test_library("art-lookup-hides-unsafe");
    let art_id = library
        .insert_unsafe_artwork_for_test("text/html", b"<!doctype html><script>alert(1)</script>");

    assert!(library.art(art_id).unwrap().is_none());
    assert!(library.art_thumbnail(art_id, 256).unwrap().is_none());
}

#[test]
fn canonical_tracks_keep_local_bit_depth_when_qobuz_linked() {
    let library = test_library("canonical-local-bit-depth");
    let now = now_secs();
    let detail = QobuzAlbumDetail {
        album: qalbum("qbz-bit-depth", "Homogenic", "Björk", Some(1997)),
        tracks: vec![numbered_qtrack(11, "Hunter", 1)],
    };
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, year, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Homogenic', 'Björk', 'bjork|homogenic', 1997, 100,
                        'matched', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, track_number, disc_number, duration_secs,
                    sample_rate, bit_depth, format, album_id, embedded_art,
                    created_at, updated_at
                )
                VALUES ('/tmp/canonical-local-bit-depth/01.flac', '01.flac', 1, 1,
                        'Hunter', 'Björk', 'Homogenic', 'Björk', 1, 1, 240.0,
                        192000, 24, 'FLAC', ?1, 0, ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        album_id
    };

    library
        .link_qobuz_album(album_id, &detail, None, 100, "matched")
        .unwrap();
    let detail = library.album_detail(album_id).unwrap().unwrap();
    let canonical = detail.canonical_tracks;

    assert_eq!(canonical[0].sample_rate, Some(192000));
    assert_eq!(canonical[0].bit_depth, Some(24));
}

#[test]
fn canonical_tracks_combine_local_and_qobuz_playback_when_linked() {
    let library = test_library("canonical-linked-playback");
    let now = now_secs();
    let mut qobuz_track = qtrack(9901, "Human Behaviour", "Debut", "Björk");
    qobuz_track.track_number = Some(1);
    qobuz_track.disc_number = Some(1);
    let detail = QobuzAlbumDetail {
        album: qalbum("debut-hires", "Debut", "Björk", Some(1993)),
        tracks: vec![qobuz_track],
    };
    let (album_id, local_track_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, year, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Debut', 'Björk', 'bjork|debut', 1993, 100,
                        'matched', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, track_number, disc_number, duration_secs,
                    sample_rate, bit_depth, format, album_id, embedded_art,
                    created_at, updated_at
                )
                VALUES ('/tmp/canonical-linked-playback/01.flac', '01.flac', 1, 1,
                        'Human Behaviour', 'Björk', 'Debut', 'Björk', 1, 1, 252.0,
                        44100, 16, 'FLAC', ?1, 0, ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        (album_id, conn.last_insert_rowid())
    };

    library
        .record_playback_history(PlaybackHistoryInput {
            profile_id: None,
            source: SourceRef::LocalTrack {
                track_id: local_track_id,
                file_name: Some("01.flac".to_string()),
                title: Some("Human Behaviour".to_string()),
                artist: Some("Björk".to_string()),
                album: Some("Debut".to_string()),
                album_artist: Some("Björk".to_string()),
                album_id: Some(album_id),
                art_id: None,
                duration_secs: Some(252.0),
                ext_hint: Some("flac".to_string()),
                radio: false,
                radio_context: None,
                playlist_context: None,
            },
            zone_id: "local-core".to_string(),
            zone_name: "Local".to_string(),
            played_secs: Some(252.0),
            duration_secs: Some(252.0),
            completed: true,
            counted: true,
            radio: false,
        })
        .unwrap();
    for _ in 0..2 {
        library
            .record_playback_history(PlaybackHistoryInput {
                profile_id: None,
                source: SourceRef::QobuzTrack {
                    track_id: 9901,
                    title: Some("Human Behaviour".to_string()),
                    artist: Some("Björk".to_string()),
                    album: Some("Debut".to_string()),
                    album_id: Some("debut-hires".to_string()),
                    image_url: None,
                    duration_secs: Some(252.0),
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                zone_id: "qobuz-core".to_string(),
                zone_name: "Qobuz".to_string(),
                played_secs: Some(252.0),
                duration_secs: Some(252.0),
                completed: true,
                counted: true,
                radio: false,
            })
            .unwrap();
    }

    library
        .link_qobuz_album(album_id, &detail, None, 100, "matched")
        .unwrap();
    let detail = library.album_detail(album_id).unwrap().unwrap();
    let canonical = detail.canonical_tracks;

    assert_eq!(canonical[0].play_count, 3);
    assert_eq!(canonical[0].listened_secs, 756.0);
    assert_eq!(detail.tracks[0].id, local_track_id);
    assert_eq!(detail.tracks[0].play_count, 3);
    assert_eq!(detail.tracks[0].listened_secs, 756.0);
}

#[test]
fn primary_qobuz_track_for_local_track_respects_streaming_primary_version() {
    let library = test_library("lastfm-primary-qobuz-track");
    let mut qobuz_track = qtrack(9901, "Venus as a Boy", "Debut", "Björk");
    qobuz_track.track_number = Some(1);
    qobuz_track.disc_number = Some(1);
    let detail = QobuzAlbumDetail {
        album: qalbum("debut-hires", "Debut", "Björk", Some(1993)),
        tracks: vec![qobuz_track],
    };
    let (album_id, local_track_id) = insert_debut_fixture(&library, "lastfm-primary-qobuz-track");

    library
        .link_qobuz_album(album_id, &detail, None, 100, "matched")
        .unwrap();
    {
        let conn = library.conn.lock().unwrap();
        let qobuz_version_id: i64 = conn
            .query_row(
                "SELECT id FROM album_versions WHERE album_id = ?1 AND provider = 'qobuz'",
                [album_id],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE albums SET primary_version_id = ?2 WHERE id = ?1",
            params![album_id, qobuz_version_id],
        )
        .unwrap();
    }

    let primary = library
        .primary_qobuz_track_for_local_track(local_track_id)
        .unwrap()
        .unwrap();

    assert_eq!(primary.id, 9901);
    assert_eq!(primary.maximum_bit_depth, Some(24));
}

#[test]
fn preferred_play_source_for_local_track_uses_qobuz_primary_version() {
    let library = test_library("preferred-primary-qobuz");
    let mut qobuz_track = qtrack(9902, "Venus as a Boy", "Debut", "Björk");
    qobuz_track.track_number = Some(1);
    qobuz_track.disc_number = Some(1);
    let detail = QobuzAlbumDetail {
        album: qalbum("debut-hires", "Debut", "Björk", Some(1993)),
        tracks: vec![qobuz_track],
    };
    let (album_id, local_track_id) = insert_debut_fixture(&library, "preferred-primary-qobuz");

    library
        .link_qobuz_album(album_id, &detail, None, 100, "matched")
        .unwrap();
    let qobuz_version_id = album_version_id(&library, album_id, "qobuz");
    library
        .set_primary_version(album_id, qobuz_version_id)
        .unwrap();

    let preferred = library
        .preferred_play_source_for_local_track(local_track_id)
        .unwrap()
        .unwrap();

    match preferred {
        ResolvedPlaySource::Qobuz {
            track_id,
            format_id,
            ..
        } => {
            assert_eq!(track_id, 9902);
            assert_eq!(format_id, Some(7));
        }
        other => panic!("expected qobuz source, got {other:?}"),
    }
}

#[test]
fn preferred_play_source_for_local_track_keeps_local_primary_version() {
    let library = test_library("preferred-primary-local");
    let mut qobuz_track = qtrack(9903, "Venus as a Boy", "Debut", "Björk");
    qobuz_track.track_number = Some(1);
    qobuz_track.disc_number = Some(1);
    let detail = QobuzAlbumDetail {
        album: qalbum("debut-hires", "Debut", "Björk", Some(1993)),
        tracks: vec![qobuz_track],
    };
    let (album_id, local_track_id) = insert_debut_fixture(&library, "preferred-primary-local");

    library
        .link_qobuz_album(album_id, &detail, None, 100, "matched")
        .unwrap();
    let local_version_id = album_version_id(&library, album_id, "local");
    library
        .set_primary_version(album_id, local_version_id)
        .unwrap();

    let preferred = library
        .preferred_play_source_for_local_track(local_track_id)
        .unwrap()
        .unwrap();

    match preferred {
        ResolvedPlaySource::Local { track_id, .. } => assert_eq!(track_id, local_track_id),
        other => panic!("expected local source, got {other:?}"),
    }
}

#[test]
fn resolve_album_playback_uses_qobuz_primary_version_when_selected() {
    let library = test_library("resolve-album-playback-qobuz-primary");
    let mut qobuz_track = qtrack(9904, "Venus as a Boy", "Debut", "Björk");
    qobuz_track.track_number = Some(1);
    qobuz_track.disc_number = Some(1);
    let detail = QobuzAlbumDetail {
        album: qalbum("debut-hires", "Debut", "Björk", Some(1993)),
        tracks: vec![qobuz_track],
    };
    let (album_id, _) = insert_debut_fixture(&library, "resolve-album-playback-qobuz-primary");

    library
        .link_qobuz_album(album_id, &detail, None, 100, "matched")
        .unwrap();
    let qobuz_version_id = album_version_id(&library, album_id, "qobuz");
    library
        .set_primary_version(album_id, qobuz_version_id)
        .unwrap();

    let plan = library
        .resolve_album_playback(album_id, 0, false, None)
        .unwrap()
        .unwrap();

    assert_eq!(plan.sources.len(), 1);
    match &plan.sources[0] {
        ResolvedPlaySource::Qobuz {
            track_id,
            format_id,
            ..
        } => {
            assert_eq!(*track_id, 9904);
            assert_eq!(*format_id, Some(7));
        }
        other => panic!("expected qobuz source, got {other:?}"),
    }
}

#[test]
fn album_playback_from_middle_does_not_wrap_to_earlier_tracks() {
    let library = test_library("album-playback-no-wrap");
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Ten Songs', 'Local Artist', 'local artist|ten songs',
                        90, 'local', 10, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        for track_number in 1..=10 {
            conn.execute(
                r#"
                    INSERT INTO tracks (
                        path, file_name, size_bytes, modified_secs, title, artist,
                        album, album_artist, track_number, disc_number, album_id,
                        embedded_art, created_at, updated_at
                    )
                    VALUES (?1, ?2, 1, 1, ?3, 'Local Artist',
                            'Ten Songs', 'Local Artist', ?4, 1, ?5, 0, ?6, ?6)
                    "#,
                params![
                    format!("/tmp/album-playback-no-wrap/{track_number:02}.wav"),
                    format!("{track_number:02}.wav"),
                    format!("Song {track_number}"),
                    track_number,
                    album_id,
                    now,
                ],
            )
            .unwrap();
        }
        album_id
    };

    let plan = library
        .resolve_album_playback(album_id, 3, false, None)
        .unwrap()
        .unwrap();
    let file_names: Vec<String> = plan
        .sources
        .into_iter()
        .map(|source| match source {
            ResolvedPlaySource::Local { file_name, .. } => file_name,
            ResolvedPlaySource::Qobuz { .. } => panic!("expected local source"),
        })
        .collect();

    assert_eq!(
        file_names,
        vec![
            "04.wav", "05.wav", "06.wav", "07.wav", "08.wav", "09.wav", "10.wav"
        ]
    );
}

#[test]
fn local_album_versions_split_by_quality_without_duplicate_display_tracks() {
    let library = test_library("local-album-quality-versions");
    let now = now_secs();
    let cd_art_id = library.insert_unsafe_artwork_for_test("image/png", b"cd cover");
    let hires_art_id = library.insert_unsafe_artwork_for_test("image/png", b"hires cover");
    let (album_id, legacy_version_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    art_id, track_count, created_at, updated_at
                )
                VALUES ('Live 2003 (Disc 2)', 'Coldplay', 'coldplay|live 2003 disc 2',
                        90, 'local', ?1, 4, ?2, ?2)
                "#,
            params![cd_art_id, now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        for (prefix, sample_rate, bit_depth) in [("cd", 44_100, 16), ("hires", 96_000, 24)] {
            let track_art_id = if prefix == "hires" {
                hires_art_id
            } else {
                cd_art_id
            };
            for (track_number, title) in [(1, "Politik"), (2, "God Put a Smile Upon Your Face")] {
                conn.execute(
                    r#"
                        INSERT INTO tracks (
                            path, file_name, size_bytes, modified_secs, title, artist,
                            album, album_artist, track_number, disc_number, duration_secs,
                            sample_rate, bit_depth, format, album_id, art_id, embedded_art,
                            created_at, updated_at
                        )
                        VALUES (?1, ?2, 1, 1, ?3, 'Coldplay',
                                'Live 2003 (Disc 2)', 'Coldplay', ?4, 1, 200.0,
                                ?5, ?6, 'FLAC', ?7, ?8, 0, ?9, ?9)
                        "#,
                    params![
                        format!(
                            "/tmp/local-album-quality-versions/{prefix}/{track_number:02}.flac"
                        ),
                        format!("{track_number:02} {title}.flac"),
                        title,
                        track_number,
                        sample_rate,
                        bit_depth,
                        album_id,
                        track_art_id,
                        now,
                    ],
                )
                .unwrap();
            }
        }

        conn.execute(
            r#"
                INSERT INTO album_versions (
                    album_id, provider, provider_id, title, artist, track_count,
                    source_label, status, payload_json, created_at, updated_at
                )
                VALUES (?1, 'local', 'local', 'Live 2003 (Disc 2)', 'Coldplay', 4,
                        'Library', 'available', '{}', ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        let legacy_version_id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE albums SET primary_version_id = ?2 WHERE id = ?1",
            params![album_id, legacy_version_id],
        )
        .unwrap();
        let track_rows = {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT id, title, artist, track_number, disc_number, duration_secs,
                           sample_rate, format, bit_depth
                    FROM tracks
                    WHERE album_id = ?1
                    "#,
                )
                .unwrap();
            let rows = stmt
                .query_map([album_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<f64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                    ))
                })
                .unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap()
        };
        for (
            track_id,
            title,
            artist,
            track_number,
            disc_number,
            duration_secs,
            sample_rate,
            format,
            bit_depth,
        ) in track_rows
        {
            conn.execute(
                r#"
                    INSERT INTO version_tracks (
                        version_id, provider_track_id, local_track_id, title, artist,
                        track_number, disc_number, duration_secs, sample_rate, format,
                        bit_depth, status, created_at, updated_at
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                            'available', ?12, ?12)
                    "#,
                params![
                    legacy_version_id,
                    track_id.to_string(),
                    track_id,
                    title,
                    artist,
                    track_number,
                    disc_number,
                    duration_secs,
                    sample_rate,
                    format,
                    bit_depth,
                    now,
                ],
            )
            .unwrap();
        }
        (album_id, legacy_version_id)
    };

    {
        let conn = library.conn.lock().unwrap();
        Library::sync_local_versions_with_conn(&conn).unwrap();
    }

    let detail = library.album_detail(album_id).unwrap().unwrap();
    let local_versions: Vec<&AlbumVersionSummary> = detail
        .versions
        .iter()
        .filter(|version| version.provider == "local")
        .collect();
    let plan = library
        .resolve_album_playback(album_id, 0, false, None)
        .unwrap()
        .unwrap();
    let file_names: Vec<String> = plan
        .sources
        .into_iter()
        .map(|source| match source {
            ResolvedPlaySource::Local { file_name, .. } => file_name,
            ResolvedPlaySource::Qobuz { .. } => panic!("expected local source"),
        })
        .collect();

    assert_eq!(local_versions.len(), 2);
    assert_eq!(detail.album.primary_version_id, Some(legacy_version_id));
    assert!(
        local_versions
            .iter()
            .any(|version| version.id == legacy_version_id
                && version.sample_rate == Some(96_000)
                && version.bit_depth == Some(24))
    );
    assert_eq!(
        local_versions
            .iter()
            .map(|version| (version.sample_rate, version.bit_depth, version.track_count))
            .collect::<Vec<_>>(),
        vec![(Some(96_000), Some(24), 2), (Some(44_100), Some(16), 2)]
    );
    assert_eq!(
        local_versions
            .iter()
            .map(|version| (version.sample_rate, version.bit_depth, version.art_id))
            .collect::<Vec<_>>(),
        vec![
            (Some(96_000), Some(24), Some(hires_art_id)),
            (Some(44_100), Some(16), Some(cd_art_id))
        ]
    );
    assert_eq!(detail.tracks.len(), 2);
    assert!(
        detail
            .tracks
            .iter()
            .all(|track| track.sample_rate == Some(96_000))
    );
    assert_eq!(
        file_names,
        vec!["01 Politik.flac", "02 God Put a Smile Upon Your Face.flac"]
    );

    let songs = library
        .browse_tracks(LibraryBrowseQuery {
            limit: 20,
            ..LibraryBrowseQuery::default()
        })
        .unwrap();
    assert_eq!(songs.total, 2);
    assert_eq!(songs.items.len(), 2);
    assert!(songs.items.iter().all(|track| {
        track.sample_rate == Some(96_000)
            && track.bit_depth == Some(24)
            && track.art_id == Some(hires_art_id)
    }));
}

#[test]
fn album_tracks_fall_back_to_leading_filename_numbers() {
    let library = test_library("album-filename-track-order");
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, sort_key, confidence, match_status, track_count, created_at, updated_at
            )
            VALUES ('No Metadata', 'no metadata', 20, 'local', 3, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        for (file_name, title) in [
            ("03.flac", "Alpha"),
            ("01.flac", "Zulu"),
            ("02.flac", "Middle"),
        ] {
            conn.execute(
                r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, album,
                    album_id, embedded_art, created_at, updated_at
                )
                VALUES (?1, ?2, 1, 1, ?3, 'No Metadata', ?4, 0, ?5, ?5)
                "#,
                params![
                    format!("/tmp/album-filename-track-order/{file_name}"),
                    file_name,
                    title,
                    album_id,
                    now,
                ],
            )
            .unwrap();
        }
        album_id
    };

    let detail = library.album_detail(album_id).unwrap().unwrap();
    assert_eq!(
        detail
            .tracks
            .iter()
            .map(|track| track.file_name.as_str())
            .collect::<Vec<_>>(),
        vec!["01.flac", "02.flac", "03.flac"]
    );
}

#[test]
fn autometa_status_is_tracked_per_local_version() {
    let library = test_library("autometa-version-status");
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Vespertine', 'Björk', 'bjork|vespertine',
                        90, 'local', 4, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        for (folder, sample_rate) in [("cd-rip-a", 44_100), ("cd-rip-b", 48_000)] {
            for (track_number, title) in [(1, "Hidden Place"), (2, "Cocoon")] {
                conn.execute(
                    r#"
                        INSERT INTO tracks (
                            path, file_name, size_bytes, modified_secs, title, artist,
                            album, album_artist, track_number, disc_number, duration_secs,
                            sample_rate, bit_depth, format, album_id, embedded_art,
                            created_at, updated_at
                        )
                        VALUES (?1, ?2, 1, 1, ?3, 'Björk',
                                'Vespertine', 'Björk', ?4, 1, 200.0,
                                ?5, 16, 'FLAC', ?6, 0, ?7, ?7)
                        "#,
                    params![
                        format!("/tmp/autometa-version-status/{folder}/{track_number:02}.flac"),
                        format!("{track_number:02} {title}.flac"),
                        title,
                        track_number,
                        sample_rate,
                        album_id,
                        now,
                    ],
                )
                .unwrap();
            }
        }
        album_id
    };

    {
        let conn = library.conn.lock().unwrap();
        Library::sync_local_versions_with_conn(&conn).unwrap();
    }
    let versions = library
        .album_versions(album_id)
        .unwrap()
        .into_iter()
        .filter(|version| version.provider == "local")
        .collect::<Vec<_>>();
    assert_eq!(versions.len(), 2);
    assert!(
        versions
            .iter()
            .all(|v| v.musicbrainz_match_status.is_none())
    );
    let qobuz_version_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO album_versions (
                    album_id, provider, provider_id, title, artist, track_count,
                    status, payload_json, created_at, updated_at
                )
                VALUES (?1, 'qobuz', 'qobuz-vespertine', 'Vespertine',
                        'Björk', 2, 'available', '{}', ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let missing_version_id = versions.iter().map(|version| version.id).max().unwrap() + 10_000;

    library
        .set_version_musicbrainz_status(
            versions[0].id,
            "matched",
            Some("mb-release-a"),
            Some("MusicBrainz matched"),
        )
        .unwrap();
    library
        .set_version_qobuz_status(versions[1].id, "needs_review", Some("Qobuz score too low"))
        .unwrap();
    let missing_mb_error = library
        .set_version_musicbrainz_status(
            missing_version_id,
            "matched",
            Some("missing-release"),
            Some("should fail"),
        )
        .unwrap_err();
    let qobuz_provider_mb_error = library
        .set_version_musicbrainz_status(
            qobuz_version_id,
            "matched",
            Some("wrong-provider-release"),
            Some("should fail"),
        )
        .unwrap_err();
    let qobuz_provider_error = library
        .set_version_qobuz_status(qobuz_version_id, "matched", Some("should fail"))
        .unwrap_err();
    let missing_qobuz_error = library
        .set_version_qobuz_status(missing_version_id, "matched", Some("should fail"))
        .unwrap_err();
    assert!(
        missing_mb_error.contains("changed 0 rows"),
        "{missing_mb_error}"
    );
    assert!(
        qobuz_provider_mb_error.contains("changed 0 rows"),
        "{qobuz_provider_mb_error}"
    );
    assert!(
        qobuz_provider_error.contains("changed 0 rows"),
        "{qobuz_provider_error}"
    );
    assert!(
        missing_qobuz_error.contains("changed 0 rows"),
        "{missing_qobuz_error}"
    );

    let refreshed = library.album_versions(album_id).unwrap();
    let first = refreshed
        .iter()
        .find(|version| version.id == versions[0].id)
        .unwrap();
    let second = refreshed
        .iter()
        .find(|version| version.id == versions[1].id)
        .unwrap();
    assert_eq!(first.musicbrainz_match_status.as_deref(), Some("matched"));
    assert_eq!(
        first.musicbrainz_release_id.as_deref(),
        Some("mb-release-a")
    );
    assert!(first.musicbrainz_tagged_at.is_some());
    assert_eq!(first.qobuz_match_status.as_deref(), None);
    assert_eq!(second.musicbrainz_match_status.as_deref(), None);
    assert_eq!(second.qobuz_match_status.as_deref(), Some("needs_review"));
    assert!(second.qobuz_tagged_at.is_none());
}

#[test]
fn autometa_progress_counts_skipped_and_results() {
    let library = test_library("autometa-progress");
    assert!(library.begin_autometa_progress(3));
    assert!(!library.begin_autometa_progress(1));
    library.set_autometa_current("Album", "Library 16/44.1");
    library.update_autometa_progress("Skipped".to_string(), |progress| {
        progress.skipped += 1;
        progress.musicbrainz_matched += 1;
    });
    library.update_autometa_progress("Matched".to_string(), |progress| {
        progress.exact_matched += 1;
        progress.musicbrainz_matched += 1;
    });
    library.update_autometa_progress("Needs review".to_string(), |progress| {
        progress.no_proper_match += 1;
        progress.musicbrainz_matched += 1;
    });
    library.update_autometa_progress("Error".to_string(), |progress| {
        progress.errors += 1;
    });
    let running = library.autometa_progress();
    assert!(running.running);
    assert_eq!(running.processed, 3);
    assert_eq!(running.total, 3);
    assert_eq!(running.skipped, 1);
    assert_eq!(running.exact_matched, 1);
    assert_eq!(running.qobuz_matched, 0);
    assert_eq!(running.musicbrainz_matched, 3);
    assert_eq!(running.no_proper_match, 1);
    assert_eq!(running.errors, 1);
    assert_eq!(running.current_album.as_deref(), Some("Album"));
    library.finish_autometa_progress();
    assert!(!library.autometa_progress().running);
    assert_eq!(
        library.autometa_progress().last_result.as_deref(),
        Some("Completed with 1 error")
    );
}

#[test]
fn autometa_job_pause_is_recovered_as_interrupted_on_startup() {
    let library = test_library("autometa-job-recovery");
    let now = now_secs();
    let (album_id, version_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, album_artist, sort_key, confidence, match_status,
                track_count, created_at, updated_at
            )
            VALUES ('Post', 'Björk', 'bjork|post', 90, 'local', 1, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
            INSERT INTO album_versions (
                album_id, provider, provider_id, title, artist, track_count,
                source_label, status, payload_json, created_at, updated_at
            )
            VALUES (?1, 'local', 'local-post', 'Post', 'Björk', 1,
                    'Library 16/44.1', 'available', '{}', ?2, ?2)
            "#,
            params![album_id, now],
        )
        .unwrap();
        (album_id, conn.last_insert_rowid())
    };

    let progress = library.create_autometa_job("remaining", true).unwrap();
    let job_id = progress.job_id.unwrap();
    assert_eq!(progress.status, "running");
    assert_eq!(progress.total, 1);

    let paused = library.set_autometa_job_status(job_id, "paused").unwrap();
    assert_eq!(paused.status, "paused");
    library.recover_interrupted_autometa_jobs().unwrap();
    let recovered = library.autometa_job_progress(job_id).unwrap();
    assert_eq!(recovered.status, "interrupted");
    let items = library.autometa_job_items(job_id, None).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].album_id, album_id);
    assert_eq!(items[0].version_id, version_id);
    assert_eq!(recovered.processed, 0);
    assert_eq!(recovered.remaining, 1);
    assert!(
        library
            .create_autometa_job("remaining", true)
            .unwrap_err()
            .contains("recoverable")
    );
    let (resumed, should_spawn) = library.resume_autometa_job(job_id).unwrap();
    assert_eq!(resumed.status, "running");
    assert!(should_spawn);
    let (_, duplicate_should_spawn) = library.resume_autometa_job(job_id).unwrap();
    assert!(!duplicate_should_spawn);
}

#[test]
fn autometa_item_claim_and_worker_failure_are_persisted_atomically() {
    let library = test_library("autometa-atomic-claim-failure");
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, album_artist, sort_key, confidence, match_status,
                track_count, created_at, updated_at
            )
            VALUES ('Homogenic', 'Björk', 'bjork|homogenic', 90, 'local', 1, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
            INSERT INTO album_versions (
                album_id, provider, provider_id, title, artist, track_count,
                source_label, status, payload_json, created_at, updated_at
            )
            VALUES (?1, 'local', 'local-homogenic', 'Homogenic', 'Björk', 1,
                    'Library 16/44.1', 'available', '{}', ?2, ?2)
            "#,
            params![album_id, now],
        )
        .unwrap();
    }

    let progress = library.create_autometa_job("remaining", false).unwrap();
    let job_id = progress.job_id.unwrap();
    let claimed = library
        .claim_autometa_work_item(job_id)
        .unwrap()
        .expect("work item should be claimed");
    assert!(library.claim_autometa_work_item(job_id).unwrap().is_none());

    let items = library.autometa_job_items(job_id, None).unwrap();
    assert_eq!(items[0].id, claimed.item_id);
    assert_eq!(items[0].status, "processing");
    assert_eq!(items[0].attempts, 1);

    library
        .fail_autometa_job(job_id, "provider connection failed")
        .unwrap();
    let failed = library.autometa_job_progress(job_id).unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.error.as_deref(), Some("provider connection failed"));
    assert_eq!(failed.processed, 1);
    assert_eq!(failed.errors, 1);
    let items = library.autometa_job_items(job_id, None).unwrap();
    assert_eq!(items[0].status, "error");
    assert_eq!(
        items[0].message.as_deref(),
        Some("provider connection failed")
    );
    assert!(items[0].finished_at.is_some());
}

#[test]
fn autometa_remaining_includes_stale_matched_musicbrainz_status() {
    let library = test_library("autometa-stale-matched");
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, album_artist, sort_key, confidence, match_status,
                track_count, created_at, updated_at
            )
            VALUES ('Debut', 'Björk', 'bjork|debut', 90, 'local', 1, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
            INSERT INTO album_versions (
                album_id, provider, provider_id, title, artist, track_count,
                source_label, status, payload_json, musicbrainz_match_status,
                musicbrainz_release_id, created_at, updated_at
            )
            VALUES (?1, 'local', 'local-debut', 'Debut', 'Björk', 1,
                    'Library 16/44.1', 'available', '{}', 'matched',
                    'matched', ?2, ?2)
            "#,
            params![album_id, now],
        )
        .unwrap();
    }

    let progress = library.create_autometa_job("remaining", false).unwrap();

    assert_eq!(progress.status, "running");
    assert_eq!(progress.total, 1);
}

#[test]
fn local_album_versions_keep_mixed_quality_tracks_in_same_folder_together() {
    let library = test_library("local-album-mixed-quality-folder");
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Post', 'Björk', 'bjork|post',
                        90, 'local', 3, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        for (track_number, title, sample_rate, bit_depth) in [
            (1, "Army of Me", 96_000, 24),
            (2, "Hyperballad", 44_100, 16),
            (3, "The Modern Things", 96_000, 24),
        ] {
            conn.execute(
                r#"
                    INSERT INTO tracks (
                        path, file_name, size_bytes, modified_secs, title, artist,
                        album, album_artist, track_number, disc_number, duration_secs,
                        sample_rate, bit_depth, format, album_id, embedded_art,
                        created_at, updated_at
                    )
                    VALUES (?1, ?2, 1, 1, ?3, 'Björk',
                            'Post', 'Björk', ?4, 1, 200.0,
                            ?5, ?6, 'FLAC', ?7, 0, ?8, ?8)
                    "#,
                params![
                    format!("/tmp/local-album-mixed-quality-folder/Post/{track_number:02}.flac"),
                    format!("{track_number:02} {title}.flac"),
                    title,
                    track_number,
                    sample_rate,
                    bit_depth,
                    album_id,
                    now,
                ],
            )
            .unwrap();
        }
        album_id
    };

    {
        let conn = library.conn.lock().unwrap();
        Library::sync_local_versions_with_conn(&conn).unwrap();
    }

    let detail = library.album_detail(album_id).unwrap().unwrap();
    let local_versions: Vec<&AlbumVersionSummary> = detail
        .versions
        .iter()
        .filter(|version| version.provider == "local")
        .collect();
    let sample_rates = detail
        .tracks
        .iter()
        .map(|track| track.sample_rate)
        .collect::<Vec<_>>();

    assert_eq!(local_versions.len(), 1);
    assert_eq!(local_versions[0].track_count, 3);
    assert_eq!(local_versions[0].sample_rate, Some(96_000));
    assert_eq!(local_versions[0].bit_depth, Some(24));
    assert_eq!(detail.album.primary_version_id, Some(local_versions[0].id));
    assert_eq!(detail.tracks.len(), 3);
    assert_eq!(sample_rates, vec![Some(96_000), Some(44_100), Some(96_000)]);
}

#[test]
fn local_album_versions_split_same_quality_by_album_folder() {
    let library = test_library("local-album-folder-versions");
    let now = now_secs();
    let album_id = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Frank', 'Amy Winehouse', 'amy winehouse|frank',
                        90, 'local', 4, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        for (folder, duration_offset) in [("Frank - CD Rip", 0.0), ("Frank - Backup Rip", 1.0)] {
            for (track_number, title, duration) in [
                (1, "Intro / Stronger Than Me", 233.0),
                (2, "You Sent Me Flying / Cherry", 409.0),
            ] {
                conn.execute(
                    r#"
                        INSERT INTO tracks (
                            path, file_name, size_bytes, modified_secs, title, artist,
                            album, album_artist, track_number, disc_number, duration_secs,
                            sample_rate, bit_depth, format, album_id, embedded_art,
                            created_at, updated_at
                        )
                        VALUES (?1, ?2, 1, 1, ?3, 'Amy Winehouse',
                                'Frank', 'Amy Winehouse', ?4, 1, ?5,
                                44100, 16, 'FLAC', ?6, 0, ?7, ?7)
                        "#,
                    params![
                        format!("/tmp/local-album-folder-versions/{folder}/{track_number:02}.flac"),
                        format!("{track_number:02} {title}.flac"),
                        title,
                        track_number,
                        duration + duration_offset,
                        album_id,
                        now,
                    ],
                )
                .unwrap();
            }
        }
        album_id
    };

    {
        let conn = library.conn.lock().unwrap();
        Library::sync_local_versions_with_conn(&conn).unwrap();
    }

    let detail = library.album_detail(album_id).unwrap().unwrap();
    let local_versions: Vec<&AlbumVersionSummary> = detail
        .versions
        .iter()
        .filter(|version| version.provider == "local")
        .collect();
    let track_numbers = detail
        .tracks
        .iter()
        .map(|track| track.track_number)
        .collect::<Vec<_>>();

    assert_eq!(local_versions.len(), 2);
    assert_eq!(
        local_versions
            .iter()
            .map(|version| (version.sample_rate, version.bit_depth, version.track_count))
            .collect::<Vec<_>>(),
        vec![(Some(44_100), Some(16), 2), (Some(44_100), Some(16), 2)]
    );
    assert_eq!(detail.tracks.len(), 2);
    assert_eq!(track_numbers, vec![Some(1), Some(2)]);
}

#[test]
fn recent_history_includes_active_zero_second_play() {
    let library = test_library("recent-history-live");
    library
        .record_playback_history(PlaybackHistoryInput {
            profile_id: None,
            source: SourceRef::LocalTrack {
                track_id: 1,
                file_name: Some("finished.flac".to_string()),
                title: Some("Finished".to_string()),
                artist: Some("Local Artist".to_string()),
                album: Some("Local Album".to_string()),
                album_artist: Some("Local Artist".to_string()),
                album_id: None,
                art_id: None,
                duration_secs: Some(180.0),
                ext_hint: None,
                radio: false,
                radio_context: None,
                playlist_context: None,
            },
            zone_id: "local-core".to_string(),
            zone_name: "Local".to_string(),
            played_secs: Some(60.0),
            duration_secs: Some(180.0),
            completed: false,
            counted: true,
            radio: false,
        })
        .unwrap();

    let live = vec![PlaybackHistoryInput {
        profile_id: None,
        source: SourceRef::QobuzTrack {
            track_id: 42,
            title: Some("Just Started".to_string()),
            artist: Some("Qobuz Artist".to_string()),
            album: Some("Qobuz Album".to_string()),
            album_id: Some("album-42".to_string()),
            image_url: None,
            duration_secs: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        },
        zone_id: "local-core".to_string(),
        zone_name: "Local".to_string(),
        played_secs: Some(0.0),
        duration_secs: Some(180.0),
        completed: false,
        counted: false,
        radio: false,
    }];

    let recent = library
        .recent_playback_history_with_live(10, &live, true)
        .unwrap();
    assert_eq!(recent[0].id, -1);
    assert_eq!(recent[0].title.as_deref(), Some("Just Started"));
    assert_eq!(recent[0].played_secs, Some(0.0));
    assert!(
        recent
            .iter()
            .any(|entry| entry.id > 0 && entry.title.as_deref() == Some("Finished"))
    );
}

#[test]
fn recent_history_can_exclude_radio_plays() {
    let library = test_library("recent-history-radio-filter");
    library
        .record_playback_history(PlaybackHistoryInput {
            profile_id: None,
            source: SourceRef::QobuzTrack {
                track_id: 1,
                title: Some("Radio Pick".to_string()),
                artist: Some("Qobuz Artist".to_string()),
                album: Some("Radio Album".to_string()),
                album_id: Some("radio-album".to_string()),
                image_url: None,
                duration_secs: None,
                radio: true,
                radio_context: None,
                playlist_context: None,
            },
            zone_id: "local-core".to_string(),
            zone_name: "Local".to_string(),
            played_secs: Some(60.0),
            duration_secs: Some(180.0),
            completed: false,
            counted: true,
            radio: false,
        })
        .unwrap();
    library
        .record_playback_history(PlaybackHistoryInput {
            profile_id: None,
            source: SourceRef::QobuzTrack {
                track_id: 2,
                title: Some("Requested Pick".to_string()),
                artist: Some("Qobuz Artist".to_string()),
                album: Some("Requested Album".to_string()),
                album_id: Some("requested-album".to_string()),
                image_url: None,
                duration_secs: None,
                radio: false,
                radio_context: None,
                playlist_context: None,
            },
            zone_id: "local-core".to_string(),
            zone_name: "Local".to_string(),
            played_secs: Some(60.0),
            duration_secs: Some(180.0),
            completed: false,
            counted: true,
            radio: false,
        })
        .unwrap();

    let hidden = vec![PlaybackHistoryInput {
        profile_id: None,
        source: SourceRef::QobuzTrack {
            track_id: 3,
            title: Some("Live Radio Pick".to_string()),
            artist: Some("Qobuz Artist".to_string()),
            album: Some("Live Radio Album".to_string()),
            album_id: Some("live-radio-album".to_string()),
            image_url: None,
            duration_secs: None,
            radio: true,
            radio_context: None,
            playlist_context: None,
        },
        zone_id: "local-core".to_string(),
        zone_name: "Local".to_string(),
        played_secs: Some(0.0),
        duration_secs: Some(180.0),
        completed: false,
        counted: false,
        radio: false,
    }];
    let recent = library
        .recent_playback_history_with_live(10, &hidden, false)
        .unwrap();

    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].title.as_deref(), Some("Requested Pick"));
    assert!(!recent[0].radio);
}

#[test]
fn top_history_songs_are_profile_scoped_and_roll_up_song_versions() {
    let library = test_library("top-history-profile-rollup");
    for input in [
        qobuz_history_input(
            Some("henry"),
            1,
            "Same Song",
            "Artist",
            "Album A",
            120.0,
            true,
            false,
        ),
        qobuz_history_input(
            Some("henry"),
            2,
            "Same Song",
            "Artist",
            "Album B",
            60.0,
            true,
            false,
        ),
        qobuz_history_input(
            Some("henry"),
            3,
            "Other Song",
            "Artist",
            "Album C",
            300.0,
            true,
            false,
        ),
        qobuz_history_input(
            Some("dad"),
            4,
            "Dad Song",
            "Dad Artist",
            "Dad Album",
            900.0,
            true,
            false,
        ),
    ] {
        library.record_playback_history(input).unwrap();
    }

    let henry = library
        .top_history_songs_for_profile("henry", "all", 25, false)
        .unwrap();
    assert_eq!(henry.items.len(), 2);
    assert_eq!(henry.items[0].rank, 1);
    assert_eq!(henry.items[0].title, "Same Song");
    assert_eq!(henry.items[0].artist.as_deref(), Some("Artist"));
    assert_eq!(henry.items[0].album.as_deref(), Some("Album A"));
    assert_eq!(henry.items[0].source_key, "qobuz:1");
    assert_eq!(henry.items[0].play_count, 2);
    assert!((henry.items[0].listened_secs - 180.0).abs() < f64::EPSILON);

    let dad = library
        .top_history_songs_for_profile("dad", "all", 25, false)
        .unwrap();
    assert_eq!(dad.items.len(), 1);
    assert_eq!(dad.items[0].title, "Dad Song");
}

#[test]
fn top_history_songs_show_most_played_version_and_keep_live_versions_separate() {
    let library = test_library("top-history-most-played-version");
    for input in [
        qobuz_history_input(
            None,
            21,
            "Venus as a Boy",
            "Björk",
            "Debut",
            120.0,
            true,
            false,
        ),
        qobuz_history_input(
            None,
            21,
            "Venus as a Boy",
            "Björk",
            "Debut",
            120.0,
            true,
            false,
        ),
        qobuz_history_input(
            None,
            22,
            "Venus as a Boy",
            "Björk",
            "Greatest Hits",
            120.0,
            true,
            false,
        ),
        qobuz_history_input(
            None,
            23,
            "Venus as a Boy (Live)",
            "Björk",
            "Debut Live",
            120.0,
            true,
            false,
        ),
    ] {
        library.record_playback_history(input).unwrap();
    }
    let now = now_secs();
    set_history_played_at(&library, "qobuz:21", now - 120);
    set_history_played_at(&library, "qobuz:22", now - 30);
    set_history_played_at(&library, "qobuz:23", now - 10);

    let top = library
        .top_history_songs_for_profile(crate::settings::DEFAULT_PROFILE_ID, "all", 25, false)
        .unwrap();
    assert_eq!(top.items.len(), 2);
    assert_eq!(top.items[0].title, "Venus as a Boy");
    assert_eq!(top.items[0].album.as_deref(), Some("Debut"));
    assert_eq!(top.items[0].source_key, "qobuz:21");
    assert_eq!(top.items[0].play_count, 3);
    assert_eq!(top.items[1].title, "Venus as a Boy (Live)");

    let stats = library
        .listening_history_stats_with_live("all", &[])
        .unwrap();
    assert_eq!(stats.top_songs.len(), 2);
    assert_eq!(stats.top_songs[0].name, "Venus as a Boy");
    assert_eq!(stats.top_songs[0].album.as_deref(), Some("Debut"));
    assert_eq!(stats.top_songs[0].play_count, 3);
    assert!(
        stats
            .top_songs
            .iter()
            .any(|item| item.name == "Venus as a Boy (Live)" && item.play_count == 1)
    );
}

#[test]
fn top_history_songs_honor_range_and_radio_filter() {
    let library = test_library("top-history-range-radio");
    for input in [
        qobuz_history_input(
            Some("henry"),
            10,
            "Old Song",
            "Artist",
            "Old Album",
            600.0,
            true,
            false,
        ),
        qobuz_history_input(
            Some("henry"),
            11,
            "Radio Song",
            "Artist",
            "Radio Album",
            60.0,
            true,
            true,
        ),
        qobuz_history_input(
            Some("henry"),
            12,
            "Requested Song",
            "Artist",
            "Requested Album",
            60.0,
            true,
            false,
        ),
    ] {
        library.record_playback_history(input).unwrap();
    }
    set_history_played_at(&library, "qobuz:10", now_secs() - 8 * 86_400);

    let week = library
        .top_history_songs_for_profile("henry", "week", 25, false)
        .unwrap();
    assert_eq!(week.range, "week");
    assert!(!week.items.iter().any(|item| item.title == "Old Song"));
    assert!(week.items.iter().any(|item| item.title == "Radio Song"));
    assert!(week.items.iter().any(|item| item.title == "Requested Song"));

    let without_radio = library
        .top_history_songs_for_profile("henry", "week", 25, true)
        .unwrap();
    assert_eq!(without_radio.items.len(), 1);
    assert_eq!(without_radio.items[0].title, "Requested Song");
}

#[test]
fn history_export_can_filter_entries_by_start_time() {
    let library = test_library("history-export-range");
    for input in [
        qobuz_history_input(
            Some("henry"),
            31,
            "Old Song",
            "Artist",
            "Old Album",
            120.0,
            true,
            false,
        ),
        qobuz_history_input(
            Some("henry"),
            32,
            "Recent Song",
            "Artist",
            "Recent Album",
            120.0,
            true,
            false,
        ),
    ] {
        library.record_playback_history(input).unwrap();
    }
    let now = now_secs();
    set_history_played_at(&library, "qobuz:31", now - 8 * 86_400);

    let all = library
        .export_playback_history_for_profile("henry")
        .unwrap();
    let last_week = library
        .export_playback_history_for_profile_since("henry", Some(now - 7 * 86_400))
        .unwrap();

    assert_eq!(all.entries.len(), 2);
    assert_eq!(last_week.entries.len(), 1);
    assert_eq!(last_week.entries[0].title.as_deref(), Some("Recent Song"));
}

#[test]
fn top_history_albums_keep_qobuz_releases_separate_by_album_id() {
    let library = test_library("top-history-qobuz-album-ids");
    for (track_id, album_id, played_secs) in
        [(101, "debut-studio", 120.0), (202, "debut-live", 300.0)]
    {
        library
            .record_playback_history(PlaybackHistoryInput {
                profile_id: None,
                source: SourceRef::QobuzTrack {
                    track_id,
                    title: Some(format!("Track {track_id}")),
                    artist: Some("Björk".to_string()),
                    album: Some("Debut".to_string()),
                    album_id: Some(album_id.to_string()),
                    image_url: None,
                    duration_secs: Some(180.0),
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                zone_id: "local-core".to_string(),
                zone_name: "Local".to_string(),
                played_secs: Some(played_secs),
                duration_secs: Some(180.0),
                completed: true,
                counted: true,
                radio: false,
            })
            .unwrap();
    }

    let stats = library
        .listening_history_stats_with_live("all", &[])
        .unwrap();
    let debut_albums = stats
        .top_albums
        .iter()
        .filter(|item| item.name == "Debut" && item.subtitle.as_deref() == Some("Björk"))
        .collect::<Vec<_>>();

    assert_eq!(debut_albums.len(), 2);
    assert_eq!(
        debut_albums[0].qobuz_album_id.as_deref(),
        Some("debut-live")
    );
    assert!((debut_albums[0].listened_secs - 300.0).abs() < f64::EPSILON);
    assert_eq!(
        debut_albums[1].qobuz_album_id.as_deref(),
        Some("debut-studio")
    );
    assert!((debut_albums[1].listened_secs - 120.0).abs() < f64::EPSILON);
}

#[test]
fn top_history_albums_roll_up_linked_local_and_qobuz_album_versions() {
    let library = test_library("top-history-linked-local-qobuz");
    let now = now_secs();
    let (album_id, local_cd_track_id, local_hires_track_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, album_artist, sort_key, confidence, match_status,
                qobuz_album_id, qobuz_match_status, track_count, created_at, updated_at
            )
            VALUES ('Debut', 'Björk', 'bjork|debut', 100, 'matched',
                    'debut-studio', 'matched', 2, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        for (provider_id, sample_rate, bit_depth) in [
            ("local-cd", 44_100_i64, 16_i64),
            ("local-hires", 96_000_i64, 24_i64),
        ] {
            conn.execute(
                r#"
                INSERT INTO album_versions (
                    album_id, provider, provider_id, title, artist, track_count,
                    sample_rate, bit_depth, created_at, updated_at
                )
                VALUES (?1, 'local', ?2, 'Debut', 'Björk', 2, ?3, ?4, ?5, ?5)
                "#,
                params![album_id, provider_id, sample_rate, bit_depth, now],
            )
            .unwrap();
        }
        let mut track_ids = Vec::new();
        for (path, title, sample_rate, bit_depth) in [
            (
                "/tmp/top-history-linked-local-qobuz/cd/01.flac",
                "Human Behaviour",
                44_100_i64,
                16_i64,
            ),
            (
                "/tmp/top-history-linked-local-qobuz/hires/02.flac",
                "Venus as a Boy",
                96_000_i64,
                24_i64,
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist, album, album_artist,
                    track_number, disc_number, duration_secs, sample_rate, bit_depth, format,
                    album_id, embedded_art, created_at, updated_at
                )
                VALUES (?1, 'track.flac', 1, ?2, ?3, 'Björk', 'Debut', 'Björk',
                        1, 1, 180.0, ?4, ?5, 'flac', ?6, 0, ?2, ?2)
                "#,
                params![path, now, title, sample_rate, bit_depth, album_id],
            )
            .unwrap();
            track_ids.push(conn.last_insert_rowid());
        }
        (album_id, track_ids[0], track_ids[1])
    };

    for (track_id, title, played_secs) in [
        (local_cd_track_id, "Human Behaviour", 90.0),
        (local_hires_track_id, "Venus as a Boy", 70.0),
    ] {
        library
            .record_playback_history(PlaybackHistoryInput {
                profile_id: None,
                source: SourceRef::LocalTrack {
                    track_id,
                    file_name: Some("track.flac".to_string()),
                    title: Some(title.to_string()),
                    artist: Some("Björk".to_string()),
                    album: Some("Debut".to_string()),
                    album_artist: Some("Björk".to_string()),
                    album_id: Some(album_id),
                    art_id: None,
                    duration_secs: Some(180.0),
                    ext_hint: Some("flac".to_string()),
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                zone_id: "local-core".to_string(),
                zone_name: "Local".to_string(),
                played_secs: Some(played_secs),
                duration_secs: Some(180.0),
                completed: true,
                counted: true,
                radio: false,
            })
            .unwrap();
    }

    for (track_id, album_id, played_secs) in
        [(303, "debut-studio", 80.0), (404, "debut-live", 300.0)]
    {
        library
            .record_playback_history(PlaybackHistoryInput {
                profile_id: None,
                source: SourceRef::QobuzTrack {
                    track_id,
                    title: Some(format!("Qobuz Track {track_id}")),
                    artist: Some("Björk".to_string()),
                    album: Some("Debut".to_string()),
                    album_id: Some(album_id.to_string()),
                    image_url: None,
                    duration_secs: Some(180.0),
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
                zone_id: "local-core".to_string(),
                zone_name: "Local".to_string(),
                played_secs: Some(played_secs),
                duration_secs: Some(180.0),
                completed: true,
                counted: true,
                radio: false,
            })
            .unwrap();
    }

    let stats = library
        .listening_history_stats_with_live("all", &[])
        .unwrap();
    let studio = stats
        .top_albums
        .iter()
        .find(|item| item.qobuz_album_id.as_deref() == Some("debut-studio"))
        .unwrap();
    let live = stats
        .top_albums
        .iter()
        .find(|item| item.qobuz_album_id.as_deref() == Some("debut-live"))
        .unwrap();

    assert_eq!(studio.name, "Debut");
    assert_eq!(studio.album_id, Some(album_id));
    assert!((studio.listened_secs - 240.0).abs() < f64::EPSILON);
    assert_eq!(studio.play_count, 3);
    assert_eq!(live.name, "Debut");
    assert!(live.album_id.is_none());
    assert!((live.listened_secs - 300.0).abs() < f64::EPSILON);
    assert_eq!(live.play_count, 1);
}

#[test]
fn recent_albums_are_persisted_from_playback_source() {
    let library = test_library("recent-albums-source");
    library
        .record_recent_album_for_source(
            None,
            &SourceRef::QobuzTrack {
                track_id: 42,
                title: Some("Song".to_string()),
                artist: Some("Artist".to_string()),
                album: Some("Album".to_string()),
                album_id: Some("album-42".to_string()),
                image_url: Some("https://example.test/cover.jpg".to_string()),
                duration_secs: None,
                radio: false,
                radio_context: None,
                playlist_context: None,
            },
        )
        .unwrap();

    let recent = library.recent_albums(50).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].title, "Album");
    assert_eq!(recent[0].album_artist, "Artist");
    assert_eq!(recent[0].qobuz_album_id.as_deref(), Some("album-42"));
    assert!(recent[0].is_qobuz);
}

#[test]
fn recent_albums_collapse_linked_qobuz_versions_to_local_album() {
    let library = test_library("recent-albums-linked-qobuz");
    let now = now_secs();
    let detail = QobuzAlbumDetail {
        album: qalbum("debut-hires", "Debut", "Björk", Some(1993)),
        tracks: vec![numbered_qtrack(9901, "Human Behaviour", 1)],
    };
    let (album_id, track_id) = {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
                INSERT INTO albums (
                    title, album_artist, sort_key, year, confidence, match_status,
                    track_count, created_at, updated_at
                )
                VALUES ('Debut', 'Björk', 'bjork|debut', 1993, 100,
                        'matched', 1, ?1, ?1)
                "#,
            [now],
        )
        .unwrap();
        let album_id = conn.last_insert_rowid();
        conn.execute(
            r#"
                INSERT INTO tracks (
                    path, file_name, size_bytes, modified_secs, title, artist,
                    album, album_artist, track_number, disc_number, duration_secs,
                    sample_rate, bit_depth, format, album_id, embedded_art,
                    created_at, updated_at
                )
                VALUES ('/tmp/recent-albums-linked-qobuz/01.flac', '01.flac', 1, 1,
                        'Human Behaviour', 'Björk', 'Debut', 'Björk', 1, 1, 252.0,
                        44100, 16, 'FLAC', ?1, 0, ?2, ?2)
                "#,
            params![album_id, now],
        )
        .unwrap();
        (album_id, conn.last_insert_rowid())
    };
    library
        .link_qobuz_album(album_id, &detail, None, 100, "matched")
        .unwrap();

    library
        .record_recent_album_for_source(
            None,
            &SourceRef::QobuzTrack {
                track_id: 9901,
                title: Some("Human Behaviour".to_string()),
                artist: Some("Björk".to_string()),
                album: Some("Debut".to_string()),
                album_id: Some("debut-hires".to_string()),
                image_url: None,
                duration_secs: Some(252.0),
                radio: false,
                radio_context: None,
                playlist_context: None,
            },
        )
        .unwrap();
    library
        .record_recent_album_for_source(
            None,
            &SourceRef::LocalTrack {
                track_id,
                file_name: Some("01.flac".to_string()),
                title: Some("Human Behaviour".to_string()),
                artist: Some("Björk".to_string()),
                album: Some("Debut".to_string()),
                album_artist: Some("Björk".to_string()),
                album_id: Some(album_id),
                art_id: None,
                duration_secs: Some(252.0),
                ext_hint: Some("flac".to_string()),
                radio: false,
                radio_context: None,
                playlist_context: None,
            },
        )
        .unwrap();

    let recent = library.recent_albums(50).unwrap();
    let album_id_string = album_id.to_string();
    assert_eq!(recent.len(), 1);
    assert_eq!(
        recent[0].album_id.as_deref(),
        Some(album_id_string.as_str())
    );
    assert!(!recent[0].is_qobuz);
}

#[test]
fn recent_albums_keep_only_latest_fifty() {
    let library = test_library("recent-albums-limit");
    for idx in 0..55 {
        library
            .record_recent_album_for_source(
                None,
                &SourceRef::QobuzTrack {
                    track_id: idx,
                    title: Some(format!("Song {idx}")),
                    artist: Some("Artist".to_string()),
                    album: Some(format!("Album {idx}")),
                    album_id: Some(format!("album-{idx}")),
                    image_url: None,
                    duration_secs: None,
                    radio: false,
                    radio_context: None,
                    playlist_context: None,
                },
            )
            .unwrap();
    }

    let recent = library.recent_albums(100).unwrap();
    assert_eq!(recent.len(), 50);
    assert_eq!(recent[0].title, "Album 54");
    assert!(recent.iter().all(|album| album.title != "Album 0"));
}

fn mb(title: &str, position: i64, disc: i64) -> MbTrack {
    MbTrack {
        recording_id: None,
        disc,
        position,
        title: title.to_string(),
        artist: Some("Radiohead".to_string()),
        length_secs: None,
    }
}

fn ft(id: i64, title: &str, file_name: &str) -> TrackSummary {
    TrackSummary {
        id,
        file_name: file_name.to_string(),
        title: title.to_string(),
        artist: None,
        album: None,
        album_artist: None,
        track_number: None, // simulating WAV-without-tag case
        disc_number: None,
        year: None,
        genre: None,
        composer: None,
        duration_secs: None,
        sample_rate: None,
        bit_depth: None,
        channels: None,
        format: Some("WAV".to_string()),
        album_id: Some(1),
        art_id: None,
        play_count: 0,
        last_played_at: None,
        listened_secs: 0.0,
        preferred_play_source: None,
    }
}

fn tagged_ft(id: i64, title: &str, file_name: &str, track_number: i64) -> TrackSummary {
    let mut track = ft(id, title, file_name);
    track.track_number = Some(track_number);
    track.disc_number = Some(1);
    track.artist = Some("Björk".to_string());
    track.album = Some("Homogenic".to_string());
    track.album_artist = Some("Björk".to_string());
    track
}

fn album(title: &str, artist: Option<&str>, year: Option<i32>) -> AlbumSummary {
    AlbumSummary {
        id: 1,
        title: title.to_string(),
        album_artist: artist.map(str::to_string),
        year,
        original_year: None,
        track_count: 0,
        art_id: None,
        confidence: 0,
        match_status: "local".to_string(),
        primary_version_id: None,
        qobuz_album_id: None,
        qobuz_match_status: None,
        qobuz_match_confidence: None,
        canonical_art_id: None,
        image_url: None,
        mb_barcode: None,
    }
}

fn qalbum(id: &str, title: &str, artist: &str, year: Option<i32>) -> QobuzAlbum {
    QobuzAlbum {
        id: id.to_string(),
        title: title.to_string(),
        artist: artist.to_string(),
        artist_id: None,
        image_url: None,
        release_date: year.map(|y| format!("{y}-01-01")),
        year,
        tracks_count: None,
        duration: None,
        maximum_sampling_rate: Some(96.0),
        maximum_bit_depth: Some(24),
        hires: true,
        genre: None,
        genre_id: None,
        label: None,
        release_type: None,
        version: None,
        description: None,
        upc: None,
    }
}

fn qtrack(id: u64, title: &str, album: &str, artist: &str) -> QobuzTrack {
    QobuzTrack {
        id,
        title: title.to_string(),
        artist: artist.to_string(),
        artist_id: None,
        album: album.to_string(),
        album_id: Some("qbz1".to_string()),
        track_number: None,
        disc_number: None,
        duration: 180,
        image_url: None,
        maximum_sampling_rate: Some(96.0),
        maximum_bit_depth: Some(24),
        hires: true,
        streamable: true,
        composer: None,
        work: None,
        isrc: None,
        copyright: None,
        performers_raw: None,
        credits: Vec::new(),
        play_count: 0,
        last_played_at: None,
        listened_secs: 0.0,
    }
}

fn insert_debut_fixture(library: &Library, slug: &str) -> (i64, i64) {
    let now = now_secs();
    let conn = library.conn.lock().unwrap();
    conn.execute(
        r#"
        INSERT INTO albums (
            title, album_artist, sort_key, year, confidence, match_status,
            track_count, created_at, updated_at
        )
        VALUES ('Debut', 'Björk', 'bjork|debut', 1993, 100, 'matched', 1, ?1, ?1)
        "#,
        [now],
    )
    .unwrap();
    let album_id = conn.last_insert_rowid();
    conn.execute(
        r#"
        INSERT INTO tracks (
            path, file_name, size_bytes, modified_secs, title, artist,
            album, album_artist, track_number, disc_number, duration_secs,
            sample_rate, bit_depth, format, album_id, embedded_art,
            created_at, updated_at
        )
        VALUES (?1, '01.flac', 1, 1, 'Venus as a Boy', 'Björk', 'Debut',
                'Björk', 1, 1, 240.0, 44100, 16, 'FLAC', ?2, 0, ?3, ?3)
        "#,
        params![format!("/tmp/{slug}/01.flac"), album_id, now],
    )
    .unwrap();
    (album_id, conn.last_insert_rowid())
}

fn album_version_id(library: &Library, album_id: i64, provider: &str) -> i64 {
    let conn = library.conn.lock().unwrap();
    conn.query_row(
        "SELECT id FROM album_versions WHERE album_id = ?1 AND provider = ?2",
        params![album_id, provider],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn orders_cached_qobuz_tracks_by_disc_and_track() {
    let mut track_3 = qtrack(3, "Track 3", "Album", "Artist");
    track_3.track_number = Some(3);
    track_3.disc_number = Some(1);
    let mut track_1 = qtrack(1, "Track 1", "Album", "Artist");
    track_1.track_number = Some(1);
    track_1.disc_number = Some(1);
    let mut disc_2_track_1 = qtrack(21, "Disc 2 Track 1", "Album", "Artist");
    disc_2_track_1.track_number = Some(1);
    disc_2_track_1.disc_number = Some(2);
    let mut track_2 = qtrack(2, "Track 2", "Album", "Artist");
    track_2.track_number = Some(2);
    track_2.disc_number = Some(1);

    let tracks = vec![track_3, track_1, disc_2_track_1, track_2];
    let ids: Vec<u64> = ordered_qobuz_tracks(&tracks)
        .into_iter()
        .map(|track| track.id)
        .collect();

    assert_eq!(ids, vec![1, 2, 3, 21]);
}

#[test]
fn lastfm_local_match_uses_normalized_title_and_artist() {
    let library = test_library("lastfm-local-match");
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO albums (
                title, album_artist, sort_key, confidence, match_status, track_count, created_at, updated_at
            )
            VALUES ('Ray of Light', 'Madonna', 'madonna|ray of light', 100, 'matched', 1, ?1, ?1)
            "#,
            [now],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO tracks (
                path, file_name, size_bytes, modified_secs, title, artist, album, album_artist,
                album_id, embedded_art, created_at, updated_at
            )
            VALUES (
                '/tmp/ray-of-light.flac', 'Ray of Light.flac', 1, ?1, 'Ray Of Light',
                'Madonna', 'Ray of Light', 'Madonna', 1, 0, ?1, ?1
            )
            "#,
            [now],
        )
        .unwrap();
    }

    let matched = library
        .find_track_by_title_artist("ray-of-light", "madonna")
        .unwrap()
        .unwrap();

    assert_eq!(matched.title, "Ray Of Light");
    assert_eq!(matched.artist.as_deref(), Some("Madonna"));
}

#[test]
fn tracks_by_artist_matches_track_and_album_artist_fields() {
    let library = test_library("tracks-by-artist");
    let now = now_secs();
    {
        let conn = library.conn.lock().unwrap();
        insert_search_album(
            &conn,
            1,
            "Artist Album",
            Some("Album Fallback"),
            2024,
            3,
            now,
        );
        insert_search_track(
            &conn,
            1,
            "track-artist.flac",
            "Track Artist Match",
            Some("Needle Artist"),
            Some("Artist Album"),
            None,
            1,
            Some(1),
            now,
        );
        insert_search_track(
            &conn,
            2,
            "album-artist.flac",
            "Album Artist Match",
            Some("Someone Else"),
            Some("Artist Album"),
            Some("Needle Artist"),
            2,
            Some(1),
            now,
        );
        insert_search_track(
            &conn,
            3,
            "album-fallback.flac",
            "Album Fallback Match",
            None,
            Some("Artist Album"),
            None,
            3,
            Some(1),
            now,
        );
    }

    let direct = library.tracks_by_artist("needle artist").unwrap();
    let fallback = library.tracks_by_artist("album fallback").unwrap();

    assert_eq!(direct.len(), 2);
    assert!(
        direct
            .iter()
            .any(|track| track.title == "Track Artist Match")
    );
    assert!(
        direct
            .iter()
            .any(|track| track.title == "Album Artist Match")
    );
    let fallback_titles = fallback
        .iter()
        .map(|track| track.title.as_str())
        .collect::<Vec<_>>();
    assert!(fallback_titles.contains(&"Track Artist Match"));
    assert!(fallback_titles.contains(&"Album Fallback Match"));
    assert!(!fallback_titles.contains(&"Album Artist Match"));
}

#[test]
fn tracks_by_artist_ignores_empty_artist() {
    let library = test_library("tracks-by-empty-artist");

    assert!(library.tracks_by_artist("   ").unwrap().is_empty());
}

#[test]
fn library_search_scores_direct_matches_and_diacritics() {
    let library = test_library("library-search-quality");
    seed_search_quality_library(&library);

    let thom = library.search("thom yorke").unwrap();
    assert_eq!(
        thom.artists.first().map(|artist| artist.name.as_str()),
        Some("Thom Yorke")
    );
    assert!(thom.albums.iter().any(|album| album.title == "ANIMA"));
    assert!(
        thom.tracks
            .iter()
            .position(|track| track.artist.as_deref() == Some("Thom Yorke"))
            < thom
                .tracks
                .iter()
                .position(|track| track.title == "Traffic Lights (feat. Thom Yorke)")
    );

    let weird_fishes = library.search("weird fishes").unwrap();
    assert_eq!(
        weird_fishes
            .tracks
            .first()
            .map(|track| track.title.as_str()),
        Some("Weird Fishes/Arpeggi")
    );

    let in_rainbows = library.search("In Rainbows").unwrap();
    assert_eq!(
        in_rainbows.albums.first().map(|album| album.title.as_str()),
        Some("In Rainbows")
    );
    assert_eq!(
        in_rainbows
            .tracks
            .first()
            .and_then(|track| track.album.as_deref()),
        Some("In Rainbows")
    );

    let bjork = library.search("Bjork").unwrap();
    assert_eq!(
        bjork.artists.first().map(|artist| artist.name.as_str()),
        Some("Björk")
    );
    assert_eq!(
        bjork.albums.first().map(|album| album.title.as_str()),
        Some("Homogenic")
    );

    let atoms = library.search("Atoms for peace").unwrap();
    assert_eq!(
        atoms.artists.first().map(|artist| artist.name.as_str()),
        Some("Atoms For Peace")
    );
    assert!(atoms.albums.iter().any(|album| album.title == "Amok"));
}

#[test]
fn browse_albums_pages_searches_and_omits_followup_facets() {
    let library = test_library("browse-albums-paged");
    seed_search_quality_library(&library);
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            "UPDATE tracks SET genre = 'Electronic', sample_rate = 96000, bit_depth = 24 WHERE album_id = 1",
            [],
        )
        .unwrap();
    }

    let first = library
        .browse_albums(LibraryBrowseQuery {
            limit: 2,
            sort: Some("name".to_string()),
            direction: Some("asc".to_string()),
            include_facets: true,
            ..LibraryBrowseQuery::default()
        })
        .unwrap();
    assert_eq!(first.total, 7);
    assert_eq!(first.items.len(), 2);
    assert!(first.has_more);
    assert!(
        first
            .facets
            .as_ref()
            .is_some_and(|facets| facets.qualities.iter().any(|facet| facet.value == "hires"))
    );

    let second = library
        .browse_albums(LibraryBrowseQuery {
            limit: 2,
            offset: 2,
            sort: Some("name".to_string()),
            direction: Some("asc".to_string()),
            include_facets: false,
            ..LibraryBrowseQuery::default()
        })
        .unwrap();
    assert_eq!(second.items.len(), 2);
    assert!(second.facets.is_none());
    assert_ne!(first.items[0].id, second.items[0].id);

    let search = library
        .browse_albums(LibraryBrowseQuery {
            q: Some("rainbows".to_string()),
            limit: 10,
            include_facets: false,
            ..LibraryBrowseQuery::default()
        })
        .unwrap();
    assert!(
        search
            .items
            .iter()
            .any(|album| album.title == "In Rainbows")
    );

    let filtered = library
        .browse_albums(LibraryBrowseQuery {
            genre: Some("Electronic".to_string()),
            quality: Some("hires".to_string()),
            limit: 10,
            include_facets: false,
            ..LibraryBrowseQuery::default()
        })
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].title, "ANIMA");
}

#[test]
fn qobuz_associated_album_uses_remote_cover_only_when_local_art_is_missing() {
    let library = test_library("qobuz-cover-fallback");
    let missing_art_id = insert_minimal_album(&library, "Live 2003");
    let local_art_album_id = insert_minimal_album(&library, "Local Cover");
    let uncertain_album_id = insert_minimal_album(&library, "Uncertain Candidate");
    let local_art_id = library
        .save_artwork(
            &TrackCover {
                mime: "image/png".to_string(),
                data: tiny_png(),
            },
            "embedded",
        )
        .unwrap();
    let qobuz_cover = "https://static.qobuz.com/images/covers/live-2003.jpg";
    let payload = serde_json::json!({ "album": { "image_url": qobuz_cover } }).to_string();
    {
        let conn = library.conn.lock().unwrap();
        conn.execute(
            "UPDATE albums SET qobuz_album_id = 'qobuz-live-2003', qobuz_match_status = 'needs_review', qobuz_match_confidence = 100, qobuz_payload_json = ?2 WHERE id = ?1",
            params![missing_art_id, payload],
        )
        .unwrap();
        conn.execute(
            "UPDATE albums SET art_id = ?2, qobuz_album_id = 'qobuz-local-cover', qobuz_match_status = 'matched', qobuz_payload_json = ?3 WHERE id = ?1",
            params![local_art_album_id, local_art_id, payload],
        )
        .unwrap();
        conn.execute(
            "UPDATE albums SET qobuz_album_id = 'qobuz-uncertain', qobuz_match_status = 'needs_review', qobuz_match_confidence = 79, qobuz_payload_json = ?2 WHERE id = ?1",
            params![uncertain_album_id, payload],
        )
        .unwrap();
    }

    let page = library
        .browse_albums(LibraryBrowseQuery {
            limit: 10,
            include_facets: false,
            ..LibraryBrowseQuery::default()
        })
        .unwrap();
    let missing_art = page
        .items
        .iter()
        .find(|album| album.id == missing_art_id)
        .unwrap();
    let local_art = page
        .items
        .iter()
        .find(|album| album.id == local_art_album_id)
        .unwrap();
    let uncertain = page
        .items
        .iter()
        .find(|album| album.id == uncertain_album_id)
        .unwrap();

    assert_eq!(missing_art.art_id, None);
    assert_eq!(missing_art.image_url.as_deref(), Some(qobuz_cover));
    assert_eq!(local_art.art_id, Some(local_art_id));
    assert_eq!(local_art.image_url, None);
    assert_eq!(uncertain.image_url, None);
}

fn seed_search_quality_library(library: &Library) {
    let now = now_secs();
    let conn = library.conn.lock().unwrap();
    insert_search_artist(&conn, "Thom Yorke", now);
    insert_search_artist(&conn, "Radiohead", now);
    insert_search_artist(&conn, "Björk", now);
    insert_search_artist(&conn, "Brant Bjork", now);
    insert_search_artist(&conn, "Atoms For Peace", now);
    insert_search_artist(&conn, "Thom Yorke Colin Greenwood Jonny Greenwood", now);

    insert_search_album(&conn, 1, "ANIMA", Some("Thom Yorke"), 2019, 9, now);
    insert_search_album(&conn, 2, "The Eraser", Some("Thom Yorke"), 2006, 9, now);
    insert_search_album(&conn, 3, "In Rainbows", Some("Radiohead"), 2007, 10, now);
    insert_search_album(
        &conn,
        4,
        "In Rainbows (Disk 2)",
        Some("Radiohead"),
        2007,
        8,
        now,
    );
    insert_search_album(&conn, 5, "Homogenic", Some("Björk"), 1997, 10, now);
    insert_search_album(&conn, 6, "Amok", Some("Atoms For Peace"), 2013, 9, now);
    insert_search_album(
        &conn,
        7,
        "Atoms for Peace",
        Some("Tom Caufield"),
        2024,
        1,
        now,
    );

    insert_search_track(
        &conn,
        1,
        "traffic.flac",
        "Traffic",
        Some("Thom Yorke"),
        Some("ANIMA"),
        Some("Thom Yorke"),
        1,
        Some(1),
        now,
    );
    insert_search_track(
        &conn,
        2,
        "traffic-lights.flac",
        "Traffic Lights (feat. Thom Yorke)",
        Some("Flea"),
        Some("Honora"),
        Some("Flea"),
        1,
        None,
        now,
    );
    insert_search_track(
        &conn,
        3,
        "weird-fishes.flac",
        "Weird Fishes/Arpeggi",
        Some("Radiohead"),
        Some("In Rainbows"),
        Some("Radiohead"),
        4,
        Some(3),
        now,
    );
    insert_search_track(
        &conn,
        4,
        "15-step.flac",
        "15 Step",
        Some("Radiohead"),
        Some("In Rainbows"),
        Some("Radiohead"),
        1,
        Some(3),
        now,
    );
    insert_search_track(
        &conn,
        5,
        "bachelorette.flac",
        "Bachelorette",
        Some("Björk"),
        Some("Homogenic"),
        Some("Björk"),
        4,
        Some(5),
        now,
    );
    insert_search_track(
        &conn,
        6,
        "atoms-for-peace.flac",
        "Atoms for Peace",
        Some("Thom Yorke"),
        Some("The Eraser"),
        Some("Thom Yorke"),
        6,
        Some(2),
        now,
    );
}

fn insert_search_artist(conn: &rusqlite::Connection, name: &str, now: i64) {
    conn.execute(
        "INSERT INTO artists (name, sort_name, created_at) VALUES (?1, ?2, ?3)",
        params![name, normalize_key(name), now],
    )
    .unwrap();
}

fn insert_search_album(
    conn: &rusqlite::Connection,
    id: i64,
    title: &str,
    album_artist: Option<&str>,
    year: i32,
    track_count: i64,
    now: i64,
) {
    conn.execute(
        r#"
        INSERT INTO albums (
            id, title, album_artist, sort_key, year, confidence, match_status,
            track_count, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, 100, 'matched', ?6, ?7, ?7)
        "#,
        params![
            id,
            title,
            album_artist,
            format!(
                "{}|{}",
                album_artist
                    .map(normalize_key)
                    .unwrap_or_else(|| "unknown-artist".to_string()),
                normalize_key(title)
            ),
            year,
            track_count,
            now
        ],
    )
    .unwrap();
}

// Search fixture rows keep the indexed track fields explicit at each test call site.
#[allow(clippy::too_many_arguments)]
fn insert_search_track(
    conn: &rusqlite::Connection,
    id: i64,
    file_name: &str,
    title: &str,
    artist: Option<&str>,
    album: Option<&str>,
    album_artist: Option<&str>,
    track_number: i64,
    album_id: Option<i64>,
    now: i64,
) {
    conn.execute(
        r#"
        INSERT INTO tracks (
            id, path, file_name, size_bytes, modified_secs, title, artist, album, album_artist,
            track_number, album_id, embedded_art, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?4, ?4)
        "#,
        params![
            id,
            format!("/tmp/search-quality/{file_name}"),
            file_name,
            now,
            title,
            artist,
            album,
            album_artist,
            track_number,
            album_id
        ],
    )
    .unwrap();
}

#[test]
fn artwork_path_normalization_keeps_same_named_images_distinct() {
    let root = temp_test_dir("artwork-normalization-collision");
    let external_a = root.join("external-a");
    let external_b = root.join("external-b");
    std::fs::create_dir_all(&external_a).unwrap();
    std::fs::create_dir_all(&external_b).unwrap();
    std::fs::write(external_a.join("cover.jpg"), b"image-a").unwrap();
    std::fs::write(external_b.join("cover.jpg"), b"image-b").unwrap();
    let library = Library::new(
        root.join("library.db"),
        vec![root.join("music")],
        root.join("art"),
    )
    .unwrap();
    {
        let conn = library.conn.lock().unwrap();
        for (hash, path) in [
            ("hash-a", external_a.join("cover.jpg")),
            ("hash-b", external_b.join("cover.jpg")),
        ] {
            conn.execute(
                "INSERT INTO artworks (hash, mime, path, source, created_at) VALUES (?1, 'image/jpeg', ?2, 'legacy', ?3)",
                params![hash, path.to_string_lossy(), now_secs()],
            )
            .unwrap();
        }
    }

    library.normalize_artwork_paths().unwrap();

    let paths = {
        let conn = library.conn.lock().unwrap();
        let mut statement = conn
            .prepare("SELECT path FROM artworks ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(paths.len(), 2);
    assert_ne!(paths[0], paths[1]);
    assert_eq!(
        std::fs::read(root.join("art").join(&paths[0])).unwrap(),
        b"image-a"
    );
    assert_eq!(
        std::fs::read(root.join("art").join(&paths[1])).unwrap(),
        b"image-b"
    );
    drop(library);
    let _ = std::fs::remove_dir_all(root);
}

fn test_library(name: &str) -> Library {
    let root = temp_test_dir(name);
    Library::new(
        root.join("library.db"),
        vec![root.join("music")],
        root.join("art"),
    )
    .unwrap()
}

#[test]
fn music_folders_can_be_cleared() {
    let library = test_library("music-folders-can-be-cleared");

    library.set_music_dirs(Vec::new());

    assert!(library.music_dirs().is_empty());
}

fn insert_minimal_album(library: &Library, title: &str) -> i64 {
    let now = now_secs();
    let conn = library.conn.lock().unwrap();
    conn.execute(
        r#"
        INSERT INTO albums (
            title, album_artist, sort_key, confidence, match_status,
            track_count, created_at, updated_at
        )
        VALUES (?1, 'Artist', ?2, 100, 'matched', 0, ?3, ?3)
        "#,
        params![title, format!("artist|{}", normalize_key(title)), now],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn tiny_png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .unwrap();
    cursor.into_inner()
}

fn tiny_wav() -> Vec<u8> {
    let samples = [0_u8, 0];
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36_u32 + samples.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&8_000_u32.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    wav.extend_from_slice(&samples);
    wav
}

fn tiny_blue_png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255]));
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .unwrap();
    cursor.into_inner()
}

fn png_of_size(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .unwrap();
    cursor.into_inner()
}

// Test fixture builder mirrors the Qobuz playback history fields under assertion.
#[allow(clippy::too_many_arguments)]
fn qobuz_history_input(
    profile_id: Option<&str>,
    track_id: u64,
    title: &str,
    artist: &str,
    album: &str,
    played_secs: f64,
    counted: bool,
    radio: bool,
) -> PlaybackHistoryInput {
    PlaybackHistoryInput {
        profile_id: profile_id.map(str::to_string),
        source: SourceRef::QobuzTrack {
            track_id,
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            album_id: Some(format!("album-{track_id}")),
            image_url: None,
            duration_secs: Some(180.0),
            radio,
            radio_context: None,
            playlist_context: None,
        },
        zone_id: "local-core".to_string(),
        zone_name: "Local".to_string(),
        played_secs: Some(played_secs),
        duration_secs: Some(180.0),
        completed: counted,
        counted,
        radio,
    }
}

fn set_history_played_at(library: &Library, source_key: &str, played_at: i64) {
    library
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE playback_history SET played_at = ?1 WHERE source_key = ?2",
            params![played_at, source_key],
        )
        .unwrap();
}

fn temp_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fozmo-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
