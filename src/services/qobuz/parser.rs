#[cfg(test)]
use super::model::QobuzRadioRecommendation;
use super::model::{
    QobuzAlbum, QobuzAlbumDetail, QobuzAlbumPageResponse, QobuzArtist, QobuzContributorCredit,
    QobuzFeaturedPlaylistsResponse, QobuzGenre, QobuzHomeArtist, QobuzHomeSection, QobuzPlaylist,
    QobuzPlaylistTag, QobuzTrack,
};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

pub(crate) fn push_album_home_section(
    sections: &mut Vec<QobuzHomeSection>,
    discovery_albums: &mut Vec<QobuzAlbum>,
    id: &str,
    title: &str,
    subtitle: Option<&str>,
    albums: Vec<QobuzAlbum>,
) {
    let albums = dedupe_home_albums(albums);
    if albums.is_empty() {
        return;
    }
    discovery_albums.extend(albums.iter().cloned());
    sections.push(QobuzHomeSection {
        id: id.to_string(),
        title: title.to_string(),
        subtitle: subtitle.map(str::to_string),
        item_type: "album".to_string(),
        albums,
        artists: Vec::new(),
        playlists: Vec::new(),
    });
}

pub(crate) fn push_playlist_home_section(
    sections: &mut Vec<QobuzHomeSection>,
    id: &str,
    title: &str,
    subtitle: Option<&str>,
    playlists: Vec<QobuzPlaylist>,
) {
    let playlists = dedupe_home_playlists(playlists);
    if playlists.is_empty() {
        return;
    }
    sections.push(QobuzHomeSection {
        id: id.to_string(),
        title: title.to_string(),
        subtitle: subtitle.map(str::to_string),
        item_type: "playlist".to_string(),
        albums: Vec::new(),
        artists: Vec::new(),
        playlists,
    });
}

fn dedupe_home_albums(albums: Vec<QobuzAlbum>) -> Vec<QobuzAlbum> {
    let mut seen = HashSet::new();
    albums
        .into_iter()
        .filter(|album| seen.insert(album.id.clone()))
        .collect()
}

fn dedupe_home_playlists(playlists: Vec<QobuzPlaylist>) -> Vec<QobuzPlaylist> {
    let mut seen = HashSet::new();
    playlists
        .into_iter()
        .filter(|playlist| seen.insert(playlist.id.clone()))
        .collect()
}

pub(crate) fn albums_from_home_response(json: &Value) -> Vec<QobuzAlbum> {
    let mut albums = Vec::new();

    if let Some(value) = json.get("albums") {
        albums.extend(albums_from_home_container(value));
    }
    if let Some(value) = json.get("items") {
        albums.extend(albums_from_home_container(value));
    }
    if let Some(value) = json.get("album")
        && let Some(album) = parse_album(value)
    {
        albums.push(album);
    }
    if let Some(containers) = json.get("containers").and_then(Value::as_object) {
        for container in containers.values() {
            if let Some(data) = container.get("data") {
                albums.extend(albums_from_home_container(data));
            }
        }
    }
    if albums.is_empty()
        && let Some(album) = parse_album(json).filter(|album| album.title != "Untitled")
    {
        albums.push(album);
    }

    dedupe_home_albums(albums)
}

pub(crate) fn album_page_response_from_home_response(
    json: &Value,
    limit: u32,
    offset: u32,
) -> QobuzAlbumPageResponse {
    let albums = albums_from_home_response(json);
    let count = albums.len() as u32;
    let raw_count = raw_album_item_count(json).unwrap_or(count);
    let total = album_collection_total(json);
    let has_more = total
        .map(|total| offset.saturating_add(raw_count) < total)
        .unwrap_or(raw_count >= limit);

    QobuzAlbumPageResponse {
        albums,
        limit,
        offset,
        count,
        total,
        has_more,
    }
}

fn albums_from_home_container(value: &Value) -> Vec<QobuzAlbum> {
    value
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(parse_album)
        .collect()
}

pub(crate) fn playlists_from_featured_response(json: &Value) -> Vec<QobuzPlaylist> {
    let mut playlists = Vec::new();

    if let Some(value) = json.get("playlists") {
        playlists.extend(playlists_from_container(value));
    }
    if let Some(value) = json.get("items") {
        playlists.extend(playlists_from_container(value));
    }
    if let Some(value) = json.get("playlist")
        && let Some(playlist) = parse_playlist(value)
    {
        playlists.push(playlist);
    }
    if let Some(containers) = json.get("containers").and_then(Value::as_object) {
        for container in containers.values() {
            if let Some(data) = container.get("data") {
                playlists.extend(playlists_from_container(data));
            }
        }
    }
    if playlists.is_empty()
        && let Some(playlist) =
            parse_playlist(json).filter(|playlist| playlist.title != "Untitled playlist")
    {
        playlists.push(playlist);
    }

    dedupe_home_playlists(playlists)
}

pub(crate) fn featured_playlists_response_from_featured_response(
    json: &Value,
    limit: u32,
    offset: u32,
) -> QobuzFeaturedPlaylistsResponse {
    let playlists = playlists_from_featured_response(json);
    let count = playlists.len() as u32;
    let total = featured_playlists_total(json);
    let has_more = total
        .map(|total| offset.saturating_add(count) < total)
        .unwrap_or(count >= limit);

    QobuzFeaturedPlaylistsResponse {
        playlists,
        limit,
        offset,
        count,
        total,
        has_more,
    }
}

fn featured_playlists_total(json: &Value) -> Option<u32> {
    qobuz_collection_total(
        json,
        &["playlists", "items", "data"],
        &["total", "count", "maximum_items", "maximumItems"],
    )
}

fn album_collection_total(json: &Value) -> Option<u32> {
    qobuz_collection_total(
        json,
        &["albums", "items", "data"],
        &["total", "maximum_items", "maximumItems"],
    )
}

fn qobuz_collection_total(json: &Value, root_keys: &[&str], total_keys: &[&str]) -> Option<u32> {
    root_keys
        .iter()
        .filter_map(|key| json.get(*key))
        .chain(std::iter::once(json))
        .find_map(|value| qobuz_u32(value, total_keys))
        .or_else(|| {
            json.get("containers")
                .and_then(Value::as_object)
                .and_then(|containers| {
                    containers.values().find_map(|container| {
                        container
                            .get("data")
                            .and_then(|data| qobuz_u32(data, total_keys))
                    })
                })
        })
}

fn raw_album_item_count(json: &Value) -> Option<u32> {
    if let Some(value) = json.get("albums")
        && let Some(count) = raw_home_container_count(value)
    {
        return Some(count);
    }
    if let Some(value) = json.get("items")
        && let Some(count) = raw_home_container_count(value)
    {
        return Some(count);
    }
    if json.get("album").is_some() {
        return Some(1);
    }
    if let Some(containers) = json.get("containers").and_then(Value::as_object) {
        let count = containers
            .values()
            .filter_map(|container| container.get("data"))
            .filter_map(raw_home_container_count)
            .sum::<u32>();
        if count > 0 {
            return Some(count);
        }
    }
    None
}

fn raw_home_container_count(value: &Value) -> Option<u32> {
    value
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(|items| items.len() as u32)
}

fn playlists_from_container(value: &Value) -> Vec<QobuzPlaylist> {
    value
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(parse_playlist)
        .collect()
}

pub(crate) fn tracks_from_playlist_response(json: &Value) -> Vec<QobuzTrack> {
    playlist_track_items(json)
        .into_iter()
        .filter_map(parse_playlist_track_item)
        .collect()
}

pub(crate) fn playlist_tags_from_response(json: &Value) -> Vec<QobuzPlaylistTag> {
    json.get("tags")
        .and_then(Value::as_array)
        .or_else(|| json.get("items").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(parse_playlist_tag)
        .collect()
}

fn parse_playlist_tag(item: &Value) -> Option<QobuzPlaylistTag> {
    let id = item
        .get("slug")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let label = playlist_tag_label(item).unwrap_or_else(|| id.replace('-', " "));
    Some(QobuzPlaylistTag { id, label })
}

fn playlist_tag_label(item: &Value) -> Option<String> {
    item.get("name")
        .and_then(|value| {
            value.as_str().map(str::to_string).or_else(|| {
                value
                    .get("en")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| first_string_in_object(value.as_object()))
            })
        })
        .or_else(|| {
            item.get("name_json")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|value| {
                    value
                        .get("en")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| first_string_in_object(value.as_object()))
                })
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn first_string_in_object(object: Option<&Map<String, Value>>) -> Option<String> {
    object?.values().find_map(Value::as_str).map(str::to_string)
}

pub(crate) fn genres_from_response(json: &Value) -> Vec<QobuzGenre> {
    let roots = json
        .get("genres")
        .or_else(|| json.get("items"))
        .unwrap_or(json);
    let mut genres = Vec::new();
    collect_genres(roots, None, &mut genres);

    let mut seen = HashSet::new();
    genres
        .into_iter()
        .filter(|genre| seen.insert(genre.id))
        .collect()
}

fn collect_genres(value: &Value, parent_id: Option<u64>, genres: &mut Vec<QobuzGenre>) {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        for item in items {
            collect_genres(item, parent_id, genres);
        }
        return;
    }
    if let Some(items) = value.as_array() {
        for item in items {
            collect_genres(item, parent_id, genres);
        }
        return;
    }

    let id = qobuz_u64(value, &["id", "genre_id"]);
    let label = value
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| value.get("label").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let (Some(id), Some(label)) = (id, label) {
        genres.push(QobuzGenre {
            id,
            label,
            parent_id,
        });
        for key in ["children", "genres", "subgenres"] {
            if let Some(children) = value.get(key) {
                collect_genres(children, Some(id), genres);
            }
        }
    }
}

fn playlist_track_items(json: &Value) -> Vec<&Value> {
    if let Some(items) = json
        .get("tracks")
        .and_then(|tracks| tracks.get("items"))
        .and_then(Value::as_array)
    {
        return items.iter().collect();
    }
    if let Some(items) = json.get("tracks").and_then(Value::as_array) {
        return items.iter().collect();
    }
    if let Some(items) = json.get("items").and_then(Value::as_array) {
        return items.iter().collect();
    }
    Vec::new()
}

fn parse_playlist_track_item(item: &Value) -> Option<QobuzTrack> {
    parse_track(item.get("track").unwrap_or(item))
}

pub(crate) fn artists_from_home_albums(
    albums: &[QobuzAlbum],
    limit: usize,
) -> Vec<QobuzHomeArtist> {
    let mut seen = HashSet::new();
    let mut artists = Vec::new();

    for album in albums {
        let name = album.artist.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("unknown artist") {
            continue;
        }
        let key = name.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        artists.push(QobuzHomeArtist {
            id: album.artist_id,
            name: name.to_string(),
            image_url: album.image_url.clone(),
            subtitle: Some(album.title.clone()),
        });
        if artists.len() >= limit {
            break;
        }
    }

    artists
}

pub(crate) fn parse_track(item: &Value) -> Option<QobuzTrack> {
    let id = qobuz_u64(item, &["id"])?;
    let artist_obj = item.get("performer").or_else(|| item.get("artist"));
    let performer = artist_obj
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown artist");
    let artist_id = artist_obj
        .and_then(|v| v.get("id"))
        .and_then(qobuz_u64_value);
    let album = item.get("album");
    let album_title = album
        .and_then(|v| v.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown album");
    let album_id = album.and_then(|v| v.get("id")).and_then(|v| {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    });
    let image_url = album
        .and_then(|v| v.get("image"))
        .and_then(|v| {
            v.get("large")
                .or_else(|| v.get("thumbnail"))
                .or_else(|| v.get("small"))
        })
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(QobuzTrack {
        id,
        title: qobuz_track_display_title(item),
        artist: performer.to_string(),
        artist_id,
        album: album_title.to_string(),
        album_id,
        track_number: qobuz_u32(item, &["track_number", "trackNumber", "position", "number"]),
        disc_number: qobuz_u32(
            item,
            &[
                "media_number",
                "mediaNumber",
                "disc_number",
                "discNumber",
                "volume_number",
                "volumeNumber",
            ],
        ),
        duration: item.get("duration").and_then(Value::as_u64).unwrap_or(0) as u32,
        image_url,
        maximum_sampling_rate: item.get("maximum_sampling_rate").and_then(Value::as_f64),
        maximum_bit_depth: item
            .get("maximum_bit_depth")
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        hires: item.get("hires").and_then(Value::as_bool).unwrap_or(false),
        streamable: item
            .get("streamable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        composer: qobuz_name_field(item, "composer"),
        work: qobuz_string_field(item, &["work", "work_title"]),
        isrc: qobuz_string_field(item, &["isrc"]),
        copyright: qobuz_string_field(item, &["copyright"]),
        performers_raw: qobuz_string_field(item, &["performers"]),
        credits: qobuz_string_field(item, &["performers"])
            .map(|p| parse_qobuz_performers(&p))
            .unwrap_or_default(),
        play_count: 0,
        last_played_at: None,
        listened_secs: 0.0,
    })
}

pub(crate) fn parse_playlist(item: &Value) -> Option<QobuzPlaylist> {
    let id = item.get("id").and_then(|v| {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    })?;
    let title = qobuz_string_field(item, &["name", "title"])
        .unwrap_or_else(|| "Untitled playlist".to_string());
    let image_url = qobuz_image_url(item.get("image_rectangle"), &[])
        .or_else(|| qobuz_image_url(item.get("image_rectangle_mini"), &[]))
        .or_else(|| {
            qobuz_image_url(
                item.get("image").or_else(|| item.get("picture")),
                &["extralarge", "large", "medium", "thumbnail", "small"],
            )
        });
    let owner = item
        .get("owner")
        .or_else(|| item.get("user"))
        .or_else(|| item.get("author"))
        .and_then(|v| {
            v.get("name")
                .and_then(Value::as_str)
                .or_else(|| v.get("display_name").and_then(Value::as_str))
                .or_else(|| v.as_str())
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Some(QobuzPlaylist {
        id,
        title,
        description: qobuz_string_field(item, &["description", "about"]),
        owner,
        image_url,
        tracks_count: item
            .get("tracks_count")
            .or_else(|| item.get("track_count"))
            .or_else(|| item.get("tracks").and_then(|tracks| tracks.get("total")))
            .and_then(qobuz_u64_value)
            .map(|v| v as u32),
        duration: item
            .get("duration")
            .or_else(|| item.get("duration_seconds"))
            .and_then(qobuz_u64_value)
            .map(|v| v as u32),
        updated_at: qobuz_string_field(item, &["updated_at", "updated", "last_update"]),
    })
}

pub(crate) fn radio_suggest_body(
    seed_track_id: u64,
    exclude_track_ids: &[u64],
    limit: u32,
) -> Value {
    json!({
        "limit": limit.clamp(1, 500),
        "listened_tracks_ids": Value::Array(
            exclude_track_ids
                .iter()
                .copied()
                .map(|id| Value::Number(id.into()))
                .collect(),
        ),
        "track_to_analysed": [
            {
                "track_id": seed_track_id,
                "artist_id": 0,
                "genre_id": 0,
                "label_id": 0,
            }
        ],
    })
}

pub(crate) fn radio_artist_candidates_from_search(
    seed_artist_name: &str,
    artists: &[QobuzArtist],
) -> Vec<(u64, String)> {
    let seed_artist_name = seed_artist_name.trim();
    if seed_artist_name.is_empty() {
        return Vec::new();
    }
    artists
        .iter()
        .find(|artist| artist.name.eq_ignore_ascii_case(seed_artist_name))
        .or_else(|| artists.first())
        .filter(|artist| artist.id > 0)
        .map(|artist| vec![(artist.id, artist.name.clone())])
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn parse_radio_recommendation(
    response: &Value,
    exclude_track_ids: &[u64],
) -> Option<QobuzRadioRecommendation> {
    let excluded: HashSet<u64> = exclude_track_ids.iter().copied().collect();
    let algorithm = response
        .get("algorithm")
        .and_then(Value::as_str)
        .map(str::to_string);
    let track = radio_track_items(response)
        .into_iter()
        .filter_map(parse_track)
        .find(|track| track.streamable && !excluded.contains(&track.id))?;
    Some(QobuzRadioRecommendation { track, algorithm })
}

pub(crate) fn radio_track_items(response: &Value) -> Vec<&Value> {
    if let Some(items) = response
        .get("tracks")
        .and_then(|tracks| tracks.get("items"))
        .and_then(Value::as_array)
    {
        return items.iter().collect();
    }
    if let Some(items) = response.get("tracks").and_then(Value::as_array) {
        return items.iter().collect();
    }
    if let Some(items) = response.get("items").and_then(Value::as_array) {
        return items.iter().collect();
    }
    Vec::new()
}

/// Extract a `QobuzAlbum` from a JSON object. Works for both `/album/search`
/// results (where the album is one entry in `albums.items[]`) and `/album/get`
/// (where the album is the response root).
pub(crate) fn parse_album(item: &Value) -> Option<QobuzAlbum> {
    let id = item.get("id").and_then(|v| {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    })?;
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled")
        .to_string();
    let artist_value = item
        .get("artist")
        .or_else(|| item.get("performer"))
        .or_else(|| {
            item.get("artists")
                .and_then(Value::as_array)
                .and_then(|artists| artists.first())
        });
    let artist = artist_value
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown artist")
        .to_string();
    let artist_id = artist_value
        .and_then(|v| v.get("id"))
        .and_then(qobuz_u64_value);
    let image_url = item
        .get("image")
        .and_then(|v| {
            v.get("large")
                .or_else(|| v.get("thumbnail"))
                .or_else(|| v.get("small"))
                .or_else(|| v.get("extralarge"))
        })
        .and_then(Value::as_str)
        .map(str::to_string);
    let release_date = item
        .get("release_date_original")
        .and_then(Value::as_str)
        .or_else(|| {
            item.get("dates")
                .and_then(|dates| dates.get("original").or_else(|| dates.get("stream")))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let year = release_date
        .as_deref()
        .and_then(|s| s.get(..4))
        .and_then(|s| s.parse::<i32>().ok());
    let genre_value = item.get("genre");
    let genre = genre_value
        .and_then(|v| v.get("name").and_then(Value::as_str).or_else(|| v.as_str()))
        .map(str::to_string);
    let genre_id = genre_value
        .and_then(|v| v.get("id").or_else(|| v.get("genre_id")))
        .and_then(qobuz_u64_value)
        .or_else(|| item.get("genre_id").and_then(qobuz_u64_value));
    let label = item
        .get("label")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(QobuzAlbum {
        id,
        title,
        artist,
        artist_id,
        image_url,
        release_date,
        year,
        tracks_count: item
            .get("tracks_count")
            .or_else(|| item.get("track_count"))
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        duration: item
            .get("duration")
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        maximum_sampling_rate: item
            .get("maximum_sampling_rate")
            .and_then(Value::as_f64)
            .or_else(|| {
                item.get("audio_info")
                    .and_then(|audio| audio.get("maximum_sampling_rate"))
                    .and_then(Value::as_f64)
            }),
        maximum_bit_depth: item
            .get("maximum_bit_depth")
            .and_then(Value::as_u64)
            .or_else(|| {
                item.get("audio_info")
                    .and_then(|audio| audio.get("maximum_bit_depth"))
                    .and_then(Value::as_u64)
            })
            .map(|v| v as u32),
        hires: item
            .get("hires")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                item.get("audio_info")
                    .and_then(|audio| audio.get("maximum_bit_depth"))
                    .and_then(Value::as_u64)
                    .is_some_and(|depth| depth > 16)
            }),
        genre,
        genre_id,
        label,
        release_type: item
            .get("release_type")
            .or_else(|| item.get("product_type"))
            .and_then(Value::as_str)
            .map(str::to_string),
        version: item
            .get("version")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        description: item
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
        upc: item
            .get("upc")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    })
}

fn qobuz_image_url(value: Option<&Value>, keys: &[&str]) -> Option<String> {
    let value = value?;
    value.as_str().map(str::to_string).or_else(|| {
        value
            .as_array()
            .and_then(|items| items.first())
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                keys.iter()
                    .find_map(|key| value.get(*key).and_then(Value::as_str))
                    .map(str::to_string)
            })
    })
}

pub(crate) fn qobuz_sized_cover_url(url: &str, size: u32) -> String {
    const COVER_PREFIX: &str = "https://static.qobuz.com/images/covers/";
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with(COVER_PREFIX) {
        return url.to_string();
    }

    let Some(base) = lower.strip_suffix(".jpg") else {
        return url.to_string();
    };
    let Some(suffix_start) = base.rfind('_') else {
        return url.to_string();
    };
    let suffix = &base[suffix_start + 1..];
    if suffix != "org" && suffix != "max" && !suffix.chars().all(|c| c.is_ascii_digit()) {
        return url.to_string();
    }

    format!("{}_{}.jpg", &url[..suffix_start], size.clamp(64, 1600))
}

fn qobuz_standard_cover_url(url: &str) -> String {
    qobuz_sized_cover_url(url, 600)
}

pub(crate) fn standardize_qobuz_album_detail_covers(detail: &mut QobuzAlbumDetail) {
    detail.album.image_url = detail
        .album
        .image_url
        .as_deref()
        .map(qobuz_standard_cover_url);
    for track in &mut detail.tracks {
        track.image_url = track.image_url.as_deref().map(qobuz_standard_cover_url);
    }
}

pub(crate) fn parse_artist(item: &Value) -> Option<QobuzArtist> {
    let id = qobuz_u64(item, &["id"])?;
    let name = item
        .get("name")
        .and_then(|v| {
            v.as_str().map(str::to_string).or_else(|| {
                // The newer artist payloads sometimes nest the display name under `name.display`.
                v.get("display").and_then(Value::as_str).map(str::to_string)
            })
        })
        .unwrap_or_else(|| "Unknown artist".to_string());
    let image_url = qobuz_image_url(
        item.get("picture"),
        &["large", "extralarge", "medium", "small", "thumbnail"],
    )
    .or_else(|| {
        qobuz_image_url(
            item.get("image"),
            &["large", "extralarge", "medium", "small", "thumbnail"],
        )
    });
    let genre = item
        .get("genres_list")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .map(|s| s.split('/').next_back().unwrap_or(s).to_string())
        .or_else(|| {
            item.get("genre")
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let albums_count = item
        .get("albums_count")
        .and_then(Value::as_u64)
        .map(|v| v as u32);
    let biography = item.get("biography").and_then(|v| {
        v.get("content")
            .or_else(|| v.get("summary"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.as_str().map(str::to_string))
    });

    Some(QobuzArtist {
        id,
        name,
        image_url,
        genre,
        albums_count,
        biography,
    })
}

/// Tracks inside `/album/get` don't carry album metadata on each item — it lives
/// on the parent. Synthesize the track view by pulling per-track fields and
/// borrowing album-level values for everything else.
pub(crate) fn parse_track_in_album(item: &Value, album: &QobuzAlbum) -> Option<QobuzTrack> {
    let id = qobuz_u64(item, &["id"])?;
    let performer_obj = item.get("performer").or_else(|| item.get("artist"));
    let performer = performer_obj
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| album.artist.clone());
    let artist_id = performer_obj
        .and_then(|v| v.get("id"))
        .and_then(qobuz_u64_value);
    Some(QobuzTrack {
        id,
        title: qobuz_track_display_title(item),
        artist: performer,
        artist_id,
        album: album.title.clone(),
        album_id: Some(album.id.clone()),
        track_number: qobuz_u32(item, &["track_number", "trackNumber", "position", "number"]),
        disc_number: qobuz_u32(
            item,
            &[
                "media_number",
                "mediaNumber",
                "disc_number",
                "discNumber",
                "volume_number",
                "volumeNumber",
            ],
        ),
        duration: item.get("duration").and_then(Value::as_u64).unwrap_or(0) as u32,
        image_url: album.image_url.clone(),
        maximum_sampling_rate: item
            .get("maximum_sampling_rate")
            .and_then(Value::as_f64)
            .or(album.maximum_sampling_rate),
        maximum_bit_depth: item
            .get("maximum_bit_depth")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .or(album.maximum_bit_depth),
        hires: item
            .get("hires")
            .and_then(Value::as_bool)
            .unwrap_or(album.hires),
        streamable: item
            .get("streamable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        composer: qobuz_name_field(item, "composer"),
        work: qobuz_string_field(item, &["work", "work_title"]),
        isrc: qobuz_string_field(item, &["isrc"]),
        copyright: qobuz_string_field(item, &["copyright"]),
        performers_raw: qobuz_string_field(item, &["performers"]),
        credits: qobuz_string_field(item, &["performers"])
            .map(|p| parse_qobuz_performers(&p))
            .unwrap_or_default(),
        play_count: 0,
        last_played_at: None,
        listened_secs: 0.0,
    })
}

pub(crate) fn merge_qobuz_track_detail(mut base: QobuzTrack, enriched: QobuzTrack) -> QobuzTrack {
    if !enriched.title.trim().is_empty() {
        base.title = enriched.title;
    }
    if !enriched.artist.trim().is_empty() && enriched.artist != "Unknown artist" {
        base.artist = enriched.artist;
    }
    base.artist_id = enriched.artist_id.or(base.artist_id);
    if !enriched.album.trim().is_empty() && enriched.album != "Unknown album" {
        base.album = enriched.album;
    }
    base.album_id = enriched.album_id.or(base.album_id);
    base.track_number = enriched.track_number.or(base.track_number);
    base.disc_number = enriched.disc_number.or(base.disc_number);
    if enriched.duration > 0 {
        base.duration = enriched.duration;
    }
    base.image_url = enriched.image_url.or(base.image_url);
    base.maximum_sampling_rate = enriched
        .maximum_sampling_rate
        .or(base.maximum_sampling_rate);
    base.maximum_bit_depth = enriched.maximum_bit_depth.or(base.maximum_bit_depth);
    base.hires = enriched.hires || base.hires;
    base.streamable = enriched.streamable;
    base.composer = enriched.composer.or(base.composer);
    base.work = enriched.work.or(base.work);
    base.isrc = enriched.isrc.or(base.isrc);
    base.copyright = enriched.copyright.or(base.copyright);
    base.performers_raw = enriched.performers_raw.or(base.performers_raw);
    if !enriched.credits.is_empty() {
        base.credits = enriched.credits;
    }
    base
}

fn qobuz_string_field(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn qobuz_name_field(item: &Value, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.get("name").and_then(Value::as_str).or_else(|| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(crate) fn parse_qobuz_performers(performers: &str) -> Vec<QobuzContributorCredit> {
    let mut credits: Vec<QobuzContributorCredit> = Vec::new();
    for part in performers.split(" - ") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, roles)) = part.split_once(", ") else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let roles: Vec<String> = roles
            .split(", ")
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(str::to_string)
            .collect();
        if roles.is_empty() {
            continue;
        }
        if let Some(existing) = credits
            .iter_mut()
            .find(|credit| credit.name.eq_ignore_ascii_case(name))
        {
            for role in roles {
                if !existing
                    .roles
                    .iter()
                    .any(|existing_role| existing_role.eq_ignore_ascii_case(&role))
                {
                    existing.roles.push(role);
                }
            }
        } else {
            credits.push(QobuzContributorCredit {
                name: name.to_string(),
                roles,
            });
        }
    }
    credits
}

fn qobuz_track_display_title(item: &Value) -> String {
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Untitled");
    let Some(version) = qobuz_track_version(item) else {
        return title.to_string();
    };

    let title_key = normalize_qobuz_title_piece(title);
    let version_key = normalize_qobuz_title_piece(&version);
    if version_key.is_empty() || title_key.contains(&version_key) {
        title.to_string()
    } else if version.starts_with('(') && version.ends_with(')') {
        format!("{title} {version}")
    } else {
        format!("{title} ({version})")
    }
}

fn qobuz_track_version(item: &Value) -> Option<String> {
    [
        "version",
        "version_title",
        "versionTitle",
        "title_version",
        "titleVersion",
        "subtitle",
    ]
    .iter()
    .filter_map(|key| item.get(*key).and_then(Value::as_str))
    .map(str::trim)
    .find(|s| !s.is_empty())
    .map(str::to_string)
}

fn qobuz_u32(item: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        let v = item.get(*key)?;
        v.as_u64()
            .map(|n| n as u32)
            .or_else(|| v.as_str()?.trim().parse::<u32>().ok())
    })
}

fn qobuz_u64(item: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(qobuz_u64_value))
}

fn qobuz_u64_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
}

fn normalize_qobuz_title_piece(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
