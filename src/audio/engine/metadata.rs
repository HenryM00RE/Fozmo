use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{
    MetadataOptions, MetadataReader, MetadataRevision, StandardTagKey, StandardVisualKey,
};
use symphonia::core::probe::Hint;
use symphonia_metadata::id3v2::Id3v2Reader;

const FLAC_STREAM_MARKER: &[u8; 4] = b"fLaC";
const FLAC_DIRECT_SCAN_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub duration_secs: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bits_per_sample: Option<u32>,
}

#[derive(Clone)]
pub struct TrackCover {
    pub mime: String,
    pub data: Vec<u8>,
}

/// Read tags and (optionally) front-cover art from a media file without setting up a decoder.
/// Used by the library list to display per-track artist/album/title + cover thumbnails.
pub fn read_track_metadata(path: &Path) -> (TrackTags, Option<TrackCover>) {
    let mut tags = TrackTags::default();
    let mut cover: Option<TrackCover> = None;

    let Ok(file) = File::open(path) else {
        return (tags, cover);
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return (tags, cover);
    };

    let mut found_front_cover = false;
    if let Some(rev) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
        merge_tags(&mut tags, rev);
        pick_cover(rev, &mut cover, &mut found_front_cover);
    }
    if let Some(rev) = probed.format.metadata().current() {
        merge_tags(&mut tags, rev);
        pick_cover(rev, &mut cover, &mut found_front_cover);
    }

    if let Some(track) = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
    {
        let sample_rate = track.codec_params.sample_rate;
        tags.sample_rate = sample_rate;
        tags.channels = track
            .codec_params
            .channels
            .map(|channels| channels.count() as u16);
        tags.bits_per_sample = track.codec_params.bits_per_sample;
        if let (Some(frames), Some(rate)) = (track.codec_params.n_frames, sample_rate)
            && rate > 0
        {
            tags.duration_secs = Some(frames as f64 / rate as f64);
        }
    }

    if super::session::is_flac_hint(path.extension().and_then(|ext| ext.to_str()))
        && let Some(info) = read_flac_streaminfo(path)
    {
        tags.sample_rate = tags.sample_rate.or(Some(info.sample_rate));
        tags.channels = tags.channels.or(Some(info.channels));
        tags.bits_per_sample = tags.bits_per_sample.or(Some(info.bits_per_sample));
    }

    if cover.is_none() && is_wave_path(path) {
        cover = read_riff_id3_cover(path);
    }

    if cover.is_none()
        && let Some(dir) = path.parent()
    {
        cover = load_folder_cover(dir);
    }

    (tags, cover)
}

fn is_wave_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
}

/// Some WAV taggers store ID3v2 metadata in a RIFF `id3 ` chunk. Symphonia's
/// RIFF reader exposes the text tags from these files but does not reliably
/// surface APIC artwork, so probe that bounded chunk directly as a fallback.
fn read_riff_id3_cover(path: &Path) -> Option<TrackCover> {
    const MAX_ID3_CHUNK_BYTES: u64 = crate::library::MAX_ARTWORK_BYTES as u64 + 1024 * 1024;

    let mut file = File::open(path).ok()?;
    let mut riff_header = [0_u8; 12];
    file.read_exact(&mut riff_header).ok()?;
    if &riff_header[..4] != b"RIFF" || &riff_header[8..] != b"WAVE" {
        return None;
    }

    loop {
        let mut chunk_header = [0_u8; 8];
        file.read_exact(&mut chunk_header).ok()?;
        let chunk_size = u32::from_le_bytes(chunk_header[4..8].try_into().ok()?) as u64;
        if chunk_header[..4].eq_ignore_ascii_case(b"id3 ") {
            if !(10..=MAX_ID3_CHUNK_BYTES).contains(&chunk_size) {
                return None;
            }
            let mut id3_header = [0_u8; 10];
            file.read_exact(&mut id3_header).ok()?;
            let tag_len = id3v2_tag_len(&id3_header)?;
            if tag_len as u64 > chunk_size || tag_len as u64 > MAX_ID3_CHUNK_BYTES {
                return None;
            }
            let mut id3 = Vec::with_capacity(tag_len);
            id3.extend_from_slice(&id3_header);
            id3.resize(tag_len, 0);
            file.read_exact(&mut id3[10..]).ok()?;
            if !id3v2_frames_are_bounded(&id3) {
                return None;
            }
            let mut stream = MediaSourceStream::new(Box::new(Cursor::new(id3)), Default::default());
            let mut reader = Id3v2Reader::new(&MetadataOptions::default());
            let revision = reader.read_all(&mut stream).ok()?;
            let mut cover = None;
            let mut found_front_cover = false;
            pick_cover(&revision, &mut cover, &mut found_front_cover);
            return cover;
        }
        let padded_size = chunk_size + (chunk_size & 1);
        file.seek(SeekFrom::Current(padded_size.try_into().ok()?))
            .ok()?;
    }
}

fn id3v2_tag_len(header: &[u8; 10]) -> Option<usize> {
    if &header[..3] != b"ID3" || !(2..=4).contains(&header[3]) {
        return None;
    }
    let payload_len = decode_synchsafe(&header[6..10])?;
    10_usize.checked_add(payload_len)
}

fn decode_synchsafe(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != 4 || bytes.iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    Some(
        bytes
            .iter()
            .fold(0_usize, |value, byte| (value << 7) | usize::from(*byte)),
    )
}

/// Validate frame boundaries before handing the tag to Symphonia. Its ID3
/// reader allocates a frame's declared size before a short read is reported,
/// so a bounded input stream alone is not sufficient for hostile metadata.
fn id3v2_frames_are_bounded(tag: &[u8]) -> bool {
    const MAX_METADATA_FRAME_BYTES: usize = 1024 * 1024;
    const MAX_PICTURE_FRAME_OVERHEAD: usize = 64 * 1024;

    if tag.len() < 10 || id3v2_tag_len(tag[..10].try_into().unwrap()) != Some(tag.len()) {
        return false;
    }
    // Conservatively reject layouts that require de-unsynchronising or
    // skipping extended headers. WAV artwork fallback is optional, and these
    // flags otherwise make pre-validating raw frame boundaries ambiguous.
    if tag[5] & 0xd0 != 0 {
        return false;
    }
    let version = tag[3];
    let frame_header_len = if version == 2 { 6 } else { 10 };
    let mut offset = 10;
    while offset < tag.len() {
        let remaining = &tag[offset..];
        if remaining.iter().all(|byte| *byte == 0) {
            return true;
        }
        if remaining.len() < frame_header_len {
            return false;
        }
        let id_len = if version == 2 { 3 } else { 4 };
        let id = &remaining[..id_len];
        if !id
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return false;
        }
        let frame_len = match version {
            2 => {
                (usize::from(remaining[3]) << 16)
                    | (usize::from(remaining[4]) << 8)
                    | usize::from(remaining[5])
            }
            3 => u32::from_be_bytes(remaining[4..8].try_into().unwrap()) as usize,
            4 => match decode_synchsafe(&remaining[4..8]) {
                Some(size) => size,
                None => return false,
            },
            _ => return false,
        };
        let picture_frame = id == b"APIC" || id == b"PIC";
        let max_frame_len = if picture_frame {
            crate::library::MAX_ARTWORK_BYTES.saturating_add(MAX_PICTURE_FRAME_OVERHEAD)
        } else {
            MAX_METADATA_FRAME_BYTES
        };
        if frame_len == 0
            || frame_len > max_frame_len
            || frame_len > remaining.len().saturating_sub(frame_header_len)
        {
            return false;
        }
        offset += frame_header_len + frame_len;
    }
    true
}

pub(super) fn collect_reader_metadata(
    format: &mut Box<dyn FormatReader>,
    probed_metadata: &mut Option<symphonia::core::probe::ProbedMetadata>,
    folder_for_cover: Option<&Path>,
) -> (TrackTags, Option<TrackCover>) {
    let mut tags = TrackTags::default();
    let mut cover: Option<TrackCover> = None;
    let mut found_front_cover = false;
    if let Some(metadata) = probed_metadata.as_mut()
        && let Some(rev) = metadata.get().as_ref().and_then(|m| m.current())
    {
        merge_tags(&mut tags, rev);
        pick_cover(rev, &mut cover, &mut found_front_cover);
    }
    if let Some(rev) = format.metadata().current() {
        merge_tags(&mut tags, rev);
        pick_cover(rev, &mut cover, &mut found_front_cover);
    }
    if cover.is_none()
        && let Some(dir) = folder_for_cover
    {
        cover = load_folder_cover(dir);
    }
    (tags, cover)
}

struct FlacStreamInfo {
    sample_rate: u32,
    bits_per_sample: u32,
    channels: u16,
}

fn read_flac_streaminfo(path: &Path) -> Option<FlacStreamInfo> {
    let mut file = File::open(path).ok()?;
    let offset = find_flac_marker_offset(&mut file, FLAC_DIRECT_SCAN_LIMIT)
        .ok()
        .flatten()?;
    file.seek(SeekFrom::Start(offset + FLAC_STREAM_MARKER.len() as u64))
        .ok()?;

    loop {
        let mut header = [0_u8; 4];
        file.read_exact(&mut header).ok()?;
        let is_last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7f;
        let block_len =
            ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;

        if block_type == 0 {
            if block_len < 18 {
                return None;
            }
            let mut data = vec![0_u8; block_len];
            file.read_exact(&mut data).ok()?;
            let packed = data[10..18]
                .iter()
                .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte));
            let sample_rate = ((packed >> 44) & 0x000f_ffff) as u32;
            let channels = (((packed >> 41) & 0x7) + 1) as u16;
            let bits_per_sample = (((packed >> 36) & 0x1f) + 1) as u32;
            if sample_rate == 0 || channels == 0 || !(4..=32).contains(&bits_per_sample) {
                return None;
            }
            return Some(FlacStreamInfo {
                sample_rate,
                bits_per_sample,
                channels,
            });
        }

        file.seek(SeekFrom::Current(block_len as i64)).ok()?;
        if is_last {
            break;
        }
    }

    None
}

/// Filenames (case-insensitive) we recognise as folder-level album art, in priority order.
const FOLDER_COVER_STEMS: &[&str] = &["cover", "folder", "front", "albumart", "album"];
const FOLDER_COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

fn load_folder_cover(dir: &Path) -> Option<TrackCover> {
    for stem in FOLDER_COVER_STEMS {
        for ext in FOLDER_COVER_EXTS {
            for variant in [
                dir.join(format!("{stem}.{ext}")),
                dir.join(format!("{stem}.{}", ext.to_ascii_uppercase())),
            ] {
                if variant.is_file()
                    && !std::fs::metadata(&variant)
                        .map(|metadata| metadata.len() as usize > crate::library::MAX_ARTWORK_BYTES)
                        .unwrap_or(false)
                    && let Ok(data) = std::fs::read(&variant)
                {
                    let mime = match ext.to_ascii_lowercase().as_str() {
                        "png" => "image/png",
                        "webp" => "image/webp",
                        _ => "image/jpeg",
                    };
                    if let Ok(cover) = crate::library::sanitize_raster_artwork(&data, Some(mime)) {
                        return Some(cover);
                    }
                }
            }
        }
    }
    None
}

pub(super) fn merge_tags(tags: &mut TrackTags, rev: &MetadataRevision) {
    for tag in rev.tags() {
        let Some(key) = tag.std_key else { continue };
        let value = tag.value.to_string().trim().to_string();
        if value.is_empty() {
            continue;
        }
        match key {
            StandardTagKey::TrackTitle if tags.title.is_none() => tags.title = Some(value),
            StandardTagKey::Artist if tags.artist.is_none() => tags.artist = Some(value),
            StandardTagKey::AlbumArtist => {
                if tags.album_artist.is_none() {
                    tags.album_artist = Some(value);
                }
            }
            StandardTagKey::Album if tags.album.is_none() => tags.album = Some(value),
            StandardTagKey::TrackNumber if tags.track_number.is_none() => {
                tags.track_number = parse_tag_number(&value);
            }
            StandardTagKey::DiscNumber if tags.disc_number.is_none() => {
                tags.disc_number = parse_tag_number(&value);
            }
            StandardTagKey::Date if tags.year.is_none() => {
                tags.year = parse_tag_year(&value);
            }
            StandardTagKey::Genre if tags.genre.is_none() => tags.genre = Some(value),
            StandardTagKey::Composer if tags.composer.is_none() => tags.composer = Some(value),
            _ => {}
        }
    }
}

pub(super) fn apply_fallback_tags(tags: &mut TrackTags, fallback: Option<TrackTags>) {
    let Some(fallback) = fallback else { return };
    if tags.title.as_deref().is_none_or(str::is_empty) {
        tags.title = fallback.title;
    }
    if tags.artist.as_deref().is_none_or(str::is_empty) {
        tags.artist = fallback.artist;
    }
    if tags.album.as_deref().is_none_or(str::is_empty) {
        tags.album = fallback.album;
    }
    if tags.album_artist.as_deref().is_none_or(str::is_empty) {
        tags.album_artist = fallback.album_artist;
    }
    if tags.duration_secs.is_none() {
        tags.duration_secs = fallback.duration_secs;
    }
    if tags.sample_rate.is_none() {
        tags.sample_rate = fallback.sample_rate;
    }
    if tags.channels.is_none() {
        tags.channels = fallback.channels;
    }
    if tags.bits_per_sample.is_none() {
        tags.bits_per_sample = fallback.bits_per_sample;
    }
}

fn parse_tag_number(value: &str) -> Option<u32> {
    value
        .split(['/', ' '])
        .next()
        .and_then(|v| v.trim().parse::<u32>().ok())
}

fn parse_tag_year(value: &str) -> Option<i32> {
    let year: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
    if year.len() >= 4 {
        year[..4].parse::<i32>().ok()
    } else {
        None
    }
}

fn pick_cover(rev: &MetadataRevision, current: &mut Option<TrackCover>, found_front: &mut bool) {
    for visual in rev.visuals() {
        let is_front = matches!(visual.usage, Some(StandardVisualKey::FrontCover));
        if *found_front && !is_front {
            continue;
        }
        if (current.is_none() || is_front)
            && let Ok(cover) =
                crate::library::sanitize_raster_artwork(&visual.data, Some(&visual.media_type))
        {
            *current = Some(cover);
            if is_front {
                *found_front = true;
                return;
            }
        }
    }
}

fn find_flac_marker_offset<R: Read + Seek + ?Sized>(
    source: &mut R,
    max_scan_bytes: u64,
) -> std::io::Result<Option<u64>> {
    super::session::find_flac_marker_offset(source, max_scan_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use symphonia::core::meta::{MetadataBuilder, Tag, Value, Visual};

    #[test]
    fn fallback_tags_fill_missing_technical_metadata_without_overwriting_probe_values() {
        let fallback = TrackTags {
            sample_rate: Some(48_000),
            channels: Some(2),
            bits_per_sample: Some(32),
            ..TrackTags::default()
        };
        let mut missing = TrackTags::default();

        apply_fallback_tags(&mut missing, Some(fallback.clone()));

        assert_eq!(missing.sample_rate, Some(48_000));
        assert_eq!(missing.channels, Some(2));
        assert_eq!(missing.bits_per_sample, Some(32));

        let mut probed = TrackTags {
            sample_rate: Some(96_000),
            channels: Some(6),
            bits_per_sample: Some(24),
            ..TrackTags::default()
        };

        apply_fallback_tags(&mut probed, Some(fallback));

        assert_eq!(probed.sample_rate, Some(96_000));
        assert_eq!(probed.channels, Some(6));
        assert_eq!(probed.bits_per_sample, Some(24));
    }

    #[test]
    fn flac_streaminfo_fallback_reads_bit_depth() {
        let sample_rate = 96_000_u64;
        let channels = 2_u64;
        let bits_per_sample = 24_u64;
        let packed = (sample_rate << 44) | ((channels - 1) << 41) | ((bits_per_sample - 1) << 36);
        let mut streaminfo = vec![0_u8; 34];
        streaminfo[10..18].copy_from_slice(&packed.to_be_bytes());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        bytes.extend_from_slice(&[0x80, 0x00, 0x00, 0x22]);
        bytes.extend_from_slice(&streaminfo);

        let path = std::env::temp_dir().join(format!(
            "fozmo-streaminfo-{}-{}.flac",
            std::process::id(),
            sample_rate,
        ));
        fs::write(&path, bytes).unwrap();

        let info = read_flac_streaminfo(&path).unwrap();

        let _ = fs::remove_file(&path);
        assert_eq!(info.sample_rate, 96_000);
        assert_eq!(info.bits_per_sample, 24);
        assert_eq!(info.channels, 2);
    }

    #[test]
    fn album_artist_metadata_does_not_replace_track_artist() {
        let mut builder = MetadataBuilder::new();
        builder
            .add_tag(Tag::new(
                Some(StandardTagKey::AlbumArtist),
                "ALBUMARTIST",
                Value::from("Various Artists"),
            ))
            .add_tag(Tag::new(
                Some(StandardTagKey::Artist),
                "ARTIST",
                Value::from("Track Artist"),
            ));
        let revision = builder.metadata();
        let mut tags = TrackTags::default();

        merge_tags(&mut tags, &revision);

        assert_eq!(tags.album_artist.as_deref(), Some("Various Artists"));
        assert_eq!(tags.artist.as_deref(), Some("Track Artist"));
    }

    fn tiny_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    fn synchsafe(value: usize) -> [u8; 4] {
        [
            ((value >> 21) & 0x7f) as u8,
            ((value >> 14) & 0x7f) as u8,
            ((value >> 7) & 0x7f) as u8,
            (value & 0x7f) as u8,
        ]
    }

    #[test]
    fn riff_id3_fallback_reads_apic_cover() {
        let png = tiny_png();
        let mut apic = vec![0]; // ISO-8859-1 text encoding.
        apic.extend_from_slice(b"image/png\0");
        apic.push(3); // Front cover.
        apic.push(0); // Empty description.
        apic.extend_from_slice(&png);

        let mut frame = Vec::new();
        frame.extend_from_slice(b"APIC");
        frame.extend_from_slice(&(apic.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&apic);

        let mut id3 = vec![b'I', b'D', b'3', 3, 0, 0];
        id3.extend_from_slice(&synchsafe(frame.len()));
        id3.extend_from_slice(&frame);

        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(
            &(4_u32 + 8 + id3.len() as u32 + (id3.len() as u32 & 1)).to_le_bytes(),
        );
        wav.extend_from_slice(b"WAVEid3 ");
        wav.extend_from_slice(&(id3.len() as u32).to_le_bytes());
        wav.extend_from_slice(&id3);
        if id3.len() & 1 != 0 {
            wav.push(0);
        }

        let path = std::env::temp_dir().join(format!(
            "fozmo-riff-id3-cover-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, wav).unwrap();

        let cover = read_riff_id3_cover(&path).expect("APIC cover");

        let _ = fs::remove_file(path);
        assert_eq!(cover.mime, "image/png");
        assert_eq!(cover.data, png);
    }

    #[test]
    fn riff_id3_fallback_rejects_tag_extending_past_chunk() {
        let frame = [b'A', b'P', b'I', b'C', 0, 0, 0, 1, 0, 0, 0];
        let mut id3 = vec![b'I', b'D', b'3', 3, 0, 0];
        id3.extend_from_slice(&synchsafe(frame.len()));
        id3.extend_from_slice(&frame);

        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(4_u32 + 8 + id3.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEid3 ");
        wav.extend_from_slice(&10_u32.to_le_bytes());
        wav.extend_from_slice(&id3);
        let path = std::env::temp_dir().join(format!(
            "fozmo-riff-id3-out-of-chunk-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, wav).unwrap();

        assert!(read_riff_id3_cover(&path).is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn riff_id3_fallback_rejects_frame_larger_than_tag() {
        let mut id3 = vec![b'I', b'D', b'3', 3, 0, 0];
        id3.extend_from_slice(&synchsafe(10));
        id3.extend_from_slice(b"APIC");
        id3.extend_from_slice(&u32::MAX.to_be_bytes());
        id3.extend_from_slice(&[0, 0]);

        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(4_u32 + 8 + id3.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEid3 ");
        wav.extend_from_slice(&(id3.len() as u32).to_le_bytes());
        wav.extend_from_slice(&id3);
        let path = std::env::temp_dir().join(format!(
            "fozmo-riff-id3-oversized-frame-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, wav).unwrap();

        assert!(read_riff_id3_cover(&path).is_none());
        let _ = fs::remove_file(path);
    }

    fn visual(media_type: &str, data: Vec<u8>, usage: Option<StandardVisualKey>) -> Visual {
        Visual {
            media_type: media_type.to_string(),
            dimensions: None,
            bits_per_pixel: None,
            color_mode: None,
            usage,
            tags: Vec::new(),
            data: data.into_boxed_slice(),
        }
    }

    #[test]
    fn pick_cover_rejects_active_embedded_artwork() {
        let mut builder = MetadataBuilder::new();
        builder
            .add_visual(visual(
                "text/html",
                b"<!doctype html><script>alert(1)</script>".to_vec(),
                Some(StandardVisualKey::FrontCover),
            ))
            .add_visual(visual(
                "image/svg+xml",
                br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#
                    .to_vec(),
                None,
            ));
        let revision = builder.metadata();
        let mut cover = None;
        let mut found_front_cover = false;

        pick_cover(&revision, &mut cover, &mut found_front_cover);

        assert!(cover.is_none());
        assert!(!found_front_cover);
    }

    #[test]
    fn pick_cover_accepts_allowlisted_raster_mime() {
        let mut builder = MetadataBuilder::new();
        builder.add_visual(visual(
            "image/png; charset=binary",
            tiny_png(),
            Some(StandardVisualKey::FrontCover),
        ));
        let revision = builder.metadata();
        let mut cover = None;
        let mut found_front_cover = false;

        pick_cover(&revision, &mut cover, &mut found_front_cover);

        let cover = cover.expect("valid raster artwork should be retained");
        assert_eq!(cover.mime, "image/png");
        assert!(found_front_cover);
    }

    #[test]
    fn pick_cover_rejects_active_mime_even_with_raster_bytes() {
        let mut builder = MetadataBuilder::new();
        builder.add_visual(visual(
            "text/html",
            tiny_png(),
            Some(StandardVisualKey::FrontCover),
        ));
        let revision = builder.metadata();
        let mut cover = None;
        let mut found_front_cover = false;

        pick_cover(&revision, &mut cover, &mut found_front_cover);

        assert!(cover.is_none());
        assert!(!found_front_cover);
    }

    #[test]
    fn pick_cover_rejects_oversized_embedded_artwork() {
        let mut builder = MetadataBuilder::new();
        builder.add_visual(visual(
            "image/png",
            vec![0_u8; crate::library::MAX_ARTWORK_BYTES + 1],
            Some(StandardVisualKey::FrontCover),
        ));
        let revision = builder.metadata();
        let mut cover = None;
        let mut found_front_cover = false;

        pick_cover(&revision, &mut cover, &mut found_front_cover);

        assert!(cover.is_none());
        assert!(!found_front_cover);
    }
}
