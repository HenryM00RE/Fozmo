//! Verifying that a queue generation is really what Fozmo asked for.
//!
//! Two independent checks, because the two systems that hold the playlist can
//! disagree and each can be wrong on its own:
//!
//! * **Server order.** The Apple Music Web API is asked for the playlist's
//!   ordered track relationship and each library track's
//!   `playParams.catalogId`. The complete accepted catalog-ID list must match
//!   in exact order. Apple can silently substitute or drop a track, and only
//!   this catches it.
//! * **Music.app visible prefix.** Music.app receives library items through
//!   sync, so its copy of the playlist fills in over time — the `1/10` case
//!   that used to look like an eight-second failure. A prefix is only "visible"
//!   while *every* position matches its accepted entry on title, artist, album,
//!   disc, track, and duration.
//!
//! A raw track count never establishes readiness. Music.app will happily report
//! ten tracks while several are still placeholders, wrong, or out of order, and
//! starting playback there plays the wrong music.

use super::model::AppleQueueEntry;

/// Duration match tolerance: one second, or one percent, whichever is larger.
///
/// Apple's catalog duration and Music.app's decoded duration differ slightly
/// for the same track, and the difference scales with length, so a flat
/// tolerance either rejects long tracks or accepts wrong short ones.
const DURATION_TOLERANCE_SECS: f64 = 1.0;
const DURATION_TOLERANCE_FRACTION: f64 = 0.01;

/// One playlist row as Music.app exposes it, read positionally.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MusicAppPlaylistTrack {
    /// Music.app's own identity for the track, which the transport matches
    /// `current track` against.
    pub database_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: f64,
    pub disc_number: u32,
    pub track_number: u32,
}

/// Why a position stopped the visible prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PrefixStop {
    /// Music.app has not received this position yet.
    NotYetVisible,
    /// A required field was empty, so the row cannot be trusted.
    MissingField {
        field: &'static str,
    },
    Mismatch {
        field: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisiblePrefix {
    /// Number of leading positions that match their accepted entry exactly.
    pub length: usize,
    /// Why position `length` did not match, or `None` when everything matched.
    pub stopped_by: Option<PrefixStop>,
    /// Database IDs of the verified prefix, in order.
    pub database_ids: Vec<String>,
}

/// How many leading Music.app rows match the accepted entries exactly.
///
/// Strictly a prefix: a hole at position 2 stops the count even if positions 3
/// and 4 happen to be right, because Music.app's transport plays in order and
/// would reach the hole.
pub(crate) fn visible_prefix(
    accepted: &[AppleQueueEntry],
    observed: &[MusicAppPlaylistTrack],
) -> VisiblePrefix {
    let mut database_ids = Vec::new();
    for (index, entry) in accepted.iter().enumerate() {
        let Some(track) = observed.get(index) else {
            return VisiblePrefix {
                length: index,
                stopped_by: Some(PrefixStop::NotYetVisible),
                database_ids,
            };
        };
        if let Err(stop) = matches_entry(entry, track) {
            return VisiblePrefix {
                length: index,
                stopped_by: Some(stop),
                database_ids,
            };
        }
        database_ids.push(track.database_id.clone());
    }
    VisiblePrefix {
        length: accepted.len(),
        stopped_by: None,
        database_ids,
    }
}

fn matches_entry(entry: &AppleQueueEntry, track: &MusicAppPlaylistTrack) -> Result<(), PrefixStop> {
    if track.database_id.trim().is_empty() {
        return Err(PrefixStop::MissingField {
            field: "database_id",
        });
    }
    if track.title.trim().is_empty() {
        return Err(PrefixStop::MissingField { field: "title" });
    }
    if track.artist.trim().is_empty() {
        return Err(PrefixStop::MissingField { field: "artist" });
    }
    if !comparable(&entry.title, &track.title) {
        return Err(PrefixStop::Mismatch { field: "title" });
    }
    if !comparable(&entry.artist, &track.artist) {
        return Err(PrefixStop::Mismatch { field: "artist" });
    }
    if let Some(album) = entry.album_title.as_deref() {
        if track.album.trim().is_empty() {
            return Err(PrefixStop::MissingField { field: "album" });
        }
        if !comparable(album, &track.album) {
            return Err(PrefixStop::Mismatch { field: "album" });
        }
    }
    // Disc and track numbers are integers Apple and Music.app agree on exactly.
    // Any difference means this is a different pressing or a different track.
    if let Some(disc) = entry.disc_number
        && disc != track.disc_number
    {
        return Err(PrefixStop::Mismatch {
            field: "disc_number",
        });
    }
    if let Some(number) = entry.track_number
        && number != track.track_number
    {
        return Err(PrefixStop::Mismatch {
            field: "track_number",
        });
    }
    if let Some(duration) = entry.duration_secs {
        if !track.duration_secs.is_finite() || track.duration_secs <= 0.0 {
            return Err(PrefixStop::MissingField { field: "duration" });
        }
        if !durations_agree(duration, track.duration_secs) {
            return Err(PrefixStop::Mismatch { field: "duration" });
        }
    }
    Ok(())
}

fn durations_agree(expected: f64, observed: f64) -> bool {
    let tolerance = DURATION_TOLERANCE_SECS.max(expected.abs() * DURATION_TOLERANCE_FRACTION);
    (expected - observed).abs() <= tolerance
}

/// Compare two strings the way a human would consider them the same title.
///
/// Trimmed, whitespace-collapsed, case-folded, and with Latin diacritics and
/// typographic punctuation folded away. The folding matters because Apple's
/// catalog metadata and Music.app's library copy of the same track routinely
/// differ in apostrophe style and accent composition, and rejecting on that
/// would stall the prefix on a track that is in fact correct.
fn comparable(left: &str, right: &str) -> bool {
    normalize(left) == normalize(right)
}

fn normalize(value: &str) -> String {
    let mut folded = String::with_capacity(value.len());
    for character in value.chars() {
        for lowered in character.to_lowercase() {
            match lowered {
                // Typographic variants of characters that carry no meaning for
                // identity.
                '\u{2019}' | '\u{2018}' | '\u{02bc}' | '\'' => {}
                '\u{2013}' | '\u{2014}' => folded.push('-'),
                '&' => folded.push_str(" and "),
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => folded.push('a'),
                'æ' => folded.push_str("ae"),
                'ç' | 'ć' | 'č' => folded.push('c'),
                'ď' | 'đ' | 'ð' => folded.push('d'),
                'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => folded.push('e'),
                'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => folded.push('i'),
                'ĺ' | 'ļ' | 'ľ' | 'ł' => folded.push('l'),
                'ñ' | 'ń' | 'ņ' | 'ň' => folded.push('n'),
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => folded.push('o'),
                'œ' => folded.push_str("oe"),
                'ŕ' | 'ŗ' | 'ř' => folded.push('r'),
                'ś' | 'ş' | 'š' => folded.push('s'),
                'ß' => folded.push_str("ss"),
                'ť' | 'ţ' => folded.push('t'),
                'þ' => folded.push_str("th"),
                'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => {
                    folded.push('u')
                }
                'ý' | 'ÿ' => folded.push('y'),
                'ź' | 'ż' | 'ž' => folded.push('z'),
                // Combining marks left over from decomposed forms.
                '\u{0300}'..='\u{036f}' => {}
                other => folded.push(other),
            }
        }
    }
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerOrderFailure {
    /// The server holds a different number of tracks than were accepted.
    LengthMismatch { expected: usize, observed: usize },
    /// A position holds a different catalog item.
    OrderMismatch {
        position: usize,
        expected: String,
        observed: String,
    },
    /// Apple returned a library track with no resolvable catalog ID, so its
    /// identity cannot be established at all.
    MissingCatalogId { position: usize },
}

/// Check the Web API's ordered track relationship against what was accepted.
///
/// Requires the *complete* accepted list in exact order. A prefix match is not
/// enough here: unlike Music.app, the server copy is not filling in over time,
/// so anything short or reordered is a real disagreement.
pub(crate) fn verify_server_order(
    accepted: &[AppleQueueEntry],
    server_catalog_ids: &[Option<String>],
) -> Result<(), ServerOrderFailure> {
    if accepted.len() != server_catalog_ids.len() {
        return Err(ServerOrderFailure::LengthMismatch {
            expected: accepted.len(),
            observed: server_catalog_ids.len(),
        });
    }
    for (position, (entry, observed)) in accepted.iter().zip(server_catalog_ids).enumerate() {
        let Some(observed) = observed.as_deref() else {
            return Err(ServerOrderFailure::MissingCatalogId { position });
        };
        let expected = entry.effective_catalog_id();
        if expected != observed {
            return Err(ServerOrderFailure::OrderMismatch {
                position,
                expected: expected.to_string(),
                observed: observed.to_string(),
            });
        }
    }
    Ok(())
}

/// How many verified entries playback needs before it may start.
///
/// The default is conservative on purpose. Starting a Music.app playlist and
/// relying on it to pick up entries that become visible *after* playback began
/// is a behaviour Apple does not document, and it varies by build — so it is
/// only allowed where a real qualification test has confirmed it on this exact
/// macOS and Music.app build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StartupReadinessPolicy {
    /// Whether an immediate native Apple successor follows the head inside this
    /// same island.
    pub has_native_successor: bool,
    /// Whether this exact build was qualified for adopting later-visible
    /// entries into an already-started transport.
    pub later_visible_adoption_qualified: bool,
}

impl StartupReadinessPolicy {
    /// Verified entries required before the transport may start.
    pub(crate) fn required_entries(self, island_len: usize) -> usize {
        if island_len <= 1 {
            return island_len;
        }
        if !self.has_native_successor {
            return 1;
        }
        // Without qualification, the successor must already be visible: if
        // Music.app will not adopt it later, starting early means stopping at
        // the end of track one.
        if self.later_visible_adoption_qualified {
            1
        } else {
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(song_id: &str, title: &str, duration: f64, track_number: u32) -> AppleQueueEntry {
        AppleQueueEntry {
            song_id: song_id.to_string(),
            title: title.to_string(),
            artist: "Daniel Avery".to_string(),
            duration_secs: Some(duration),
            catalog_song_id: Some(song_id.to_string()),
            album_title: Some("Drone Logic".to_string()),
            disc_number: Some(1),
            track_number: Some(track_number),
            storefront: Some("nz".to_string()),
        }
    }

    fn track(title: &str, duration: f64, track_number: u32) -> MusicAppPlaylistTrack {
        MusicAppPlaylistTrack {
            database_id: format!("db-{track_number}"),
            title: title.to_string(),
            artist: "Daniel Avery".to_string(),
            album: "Drone Logic".to_string(),
            duration_secs: duration,
            disc_number: 1,
            track_number,
        }
    }

    fn island() -> Vec<AppleQueueEntry> {
        vec![
            entry("1", "Water Jump", 312.0, 1),
            entry("2", "New Energy", 289.0, 2),
            entry("3", "Naive Response", 401.0, 3),
        ]
    }

    /// The failure the whole prefix check exists to prevent: the count is right
    /// and the content is not.
    #[test]
    fn a_correct_count_with_the_wrong_order_yields_only_the_valid_prefix() {
        let observed = vec![
            track("Water Jump", 312.0, 1),
            track("Naive Response", 401.0, 3),
            track("New Energy", 289.0, 2),
        ];

        let prefix = visible_prefix(&island(), &observed);

        assert_eq!(prefix.length, 1);
        assert_eq!(
            prefix.stopped_by,
            Some(PrefixStop::Mismatch { field: "title" })
        );
        assert_eq!(prefix.database_ids, vec!["db-1".to_string()]);
    }

    #[test]
    fn a_missing_required_field_stops_the_prefix() {
        let mut observed = vec![track("Water Jump", 312.0, 1), track("New Energy", 289.0, 2)];
        observed[1].artist = String::new();

        let prefix = visible_prefix(&island(), &observed);

        assert_eq!(prefix.length, 1);
        assert_eq!(
            prefix.stopped_by,
            Some(PrefixStop::MissingField { field: "artist" })
        );
    }

    #[test]
    fn an_empty_database_id_stops_the_prefix() {
        let mut observed = vec![track("Water Jump", 312.0, 1)];
        observed[0].database_id = String::new();

        assert_eq!(
            visible_prefix(&island(), &observed).stopped_by,
            Some(PrefixStop::MissingField {
                field: "database_id"
            })
        );
    }

    /// The `0 → 1/10 → 4/10 → 10/10` propagation the old code treated as an
    /// eight-second failure. Each stage is simply a longer valid prefix.
    #[test]
    fn partial_propagation_reports_a_growing_prefix_rather_than_a_failure() {
        let island = island();
        let full = vec![
            track("Water Jump", 312.0, 1),
            track("New Energy", 289.0, 2),
            track("Naive Response", 401.0, 3),
        ];

        assert_eq!(visible_prefix(&island, &[]).length, 0);
        assert_eq!(visible_prefix(&island, &full[..1]).length, 1);
        assert_eq!(visible_prefix(&island, &full[..2]).length, 2);
        let complete = visible_prefix(&island, &full);
        assert_eq!(complete.length, 3);
        assert_eq!(complete.stopped_by, None);
        assert_eq!(complete.database_ids, vec!["db-1", "db-2", "db-3"]);
    }

    #[test]
    fn a_not_yet_visible_position_is_distinguished_from_a_mismatch() {
        assert_eq!(
            visible_prefix(&island(), &[track("Water Jump", 312.0, 1)]).stopped_by,
            Some(PrefixStop::NotYetVisible)
        );
    }

    /// Apple's catalog metadata and Music.app's library copy differ in
    /// punctuation and accent composition for the same track; rejecting on that
    /// would stall a prefix that is actually correct.
    #[test]
    fn punctuation_and_accent_differences_still_match() {
        let mut entries = island();
        entries[0].title = "Mouth’s Cradle".to_string();
        entries[0].album_title = Some("Vökuró".to_string());
        let mut observed = track("Mouth's Cradle", 312.0, 1);
        observed.album = "Vokuro".to_string();

        assert_eq!(visible_prefix(&entries[..1], &[observed]).length, 1);
    }

    #[test]
    fn whitespace_and_case_differences_still_match() {
        let mut entries = island();
        entries[0].title = "  WATER   JUMP ".to_string();

        assert_eq!(
            visible_prefix(&entries[..1], &[track("water jump", 312.0, 1)]).length,
            1
        );
    }

    /// One second or one percent, whichever is greater — so a long track gets
    /// proportional slack and a short one does not get too much.
    #[test]
    fn duration_tolerance_is_the_greater_of_one_second_and_one_percent() {
        let short = entry("1", "Water Jump", 60.0, 1);
        assert_eq!(
            visible_prefix(
                std::slice::from_ref(&short),
                &[track("Water Jump", 60.9, 1)]
            )
            .length,
            1
        );
        assert_eq!(
            visible_prefix(&[short], &[track("Water Jump", 62.0, 1)]).length,
            0
        );

        let long = entry("1", "Water Jump", 600.0, 1);
        assert_eq!(
            visible_prefix(
                std::slice::from_ref(&long),
                &[track("Water Jump", 605.0, 1)]
            )
            .length,
            1
        );
        assert_eq!(
            visible_prefix(&[long], &[track("Water Jump", 615.0, 1)]).length,
            0
        );
    }

    #[test]
    fn disc_and_track_numbers_must_match_exactly() {
        let entries = island();
        let mut observed = track("Water Jump", 312.0, 1);
        observed.disc_number = 2;

        assert_eq!(
            visible_prefix(&entries[..1], &[observed]).stopped_by,
            Some(PrefixStop::Mismatch {
                field: "disc_number"
            })
        );
    }

    #[test]
    fn server_order_requires_the_complete_accepted_list_in_order() {
        let island = island();
        let ids = |values: &[&str]| {
            values
                .iter()
                .map(|value| Some(value.to_string()))
                .collect::<Vec<_>>()
        };

        assert_eq!(verify_server_order(&island, &ids(&["1", "2", "3"])), Ok(()));
        assert_eq!(
            verify_server_order(&island, &ids(&["1", "2"])),
            Err(ServerOrderFailure::LengthMismatch {
                expected: 3,
                observed: 2
            })
        );
        assert_eq!(
            verify_server_order(&island, &ids(&["1", "3", "2"])),
            Err(ServerOrderFailure::OrderMismatch {
                position: 1,
                expected: "2".to_string(),
                observed: "3".to_string(),
            })
        );
    }

    /// A library track with no `playParams.catalogId` has no identity Fozmo can
    /// check, which is a failure rather than a pass.
    #[test]
    fn a_library_track_without_a_catalog_id_fails_server_verification() {
        let island = island();

        assert_eq!(
            verify_server_order(
                &island,
                &[Some("1".to_string()), None, Some("3".to_string())]
            ),
            Err(ServerOrderFailure::MissingCatalogId { position: 1 })
        );
    }

    /// Apple substituting an equivalent catalog item is legitimate; the check
    /// compares what the helper accepted, not what was originally requested.
    #[test]
    fn a_substituted_catalog_id_verifies_against_what_was_accepted() {
        let mut island = island();
        island[1].catalog_song_id = Some("2-equivalent".to_string());

        assert_eq!(
            verify_server_order(
                &island,
                &[
                    Some("1".to_string()),
                    Some("2-equivalent".to_string()),
                    Some("3".to_string())
                ]
            ),
            Ok(())
        );
    }

    #[test]
    fn a_one_track_island_needs_one_verified_entry() {
        let policy = StartupReadinessPolicy {
            has_native_successor: false,
            later_visible_adoption_qualified: false,
        };

        assert_eq!(policy.required_entries(1), 1);
    }

    /// Without a passing qualification test for this exact build, an island
    /// with a native successor waits for two entries — otherwise Music.app may
    /// simply stop at the end of the first track.
    #[test]
    fn an_unqualified_build_waits_for_the_native_successor() {
        let policy = StartupReadinessPolicy {
            has_native_successor: true,
            later_visible_adoption_qualified: false,
        };

        assert_eq!(policy.required_entries(10), 2);
    }

    #[test]
    fn a_qualified_build_may_start_on_the_head_alone() {
        let policy = StartupReadinessPolicy {
            has_native_successor: true,
            later_visible_adoption_qualified: true,
        };

        assert_eq!(policy.required_entries(10), 1);
    }

    #[test]
    fn an_island_without_a_native_successor_never_waits_for_the_tail() {
        let policy = StartupReadinessPolicy {
            has_native_successor: false,
            later_visible_adoption_qualified: false,
        };

        assert_eq!(policy.required_entries(10), 1);
    }
}
