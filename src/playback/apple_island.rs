//! Choosing how much of a queue Music.app may be trusted to play by itself.
//!
//! Music.app advances gaplessly inside a container it owns, which is worth a
//! great deal — but only within one capture session, and a capture session is
//! fixed at one sample rate. So the run Fozmo hands it must be a *safe island*:
//! a contiguous prefix of Apple tracks that will all decode at the same rate,
//! in the same storefront, with no repeats.
//!
//! Everything the island is built from is a prediction. Apple publishes only a
//! coarse quality tier, so the only exact rate Fozmo ever has is one it
//! observed while a track actually played. Predictions are therefore fenced
//! hard — same storefront, recently observed, and observed under the current
//! macOS/Music.app/helper/driver context — and a live mismatch still closes the
//! capture gate and enters a protected restart. Strict certainty would mean
//! preflighting every track, which costs more than the boundary is worth.
//!
//! When in doubt, the island is one track. A one-track island always works; it
//! just gives up the native gapless boundary, which the cross-provider path
//! then handles.

use super::identity::PlaybackItem;
use crate::protocol::SourceRef;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// How long a verified format stays usable as a prediction.
const FORMAT_MAX_AGE_SECS: i64 = 7 * 24 * 60 * 60;

/// Cap on one island, mirroring the helper's playlist length limit.
const MAX_ISLAND_TRACKS: usize = 100;

/// The environment a verified format was observed in.
///
/// When any of this changes, Music.app may decode the same catalog item
/// differently, so every prediction observed beforehand becomes ineligible. The
/// rows stay in the database — they are still evidence — but they no longer
/// gate a boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppleFormatContext {
    pub macos_build: String,
    pub music_app_version: String,
    pub music_app_build: String,
    pub helper_protocol_version: u32,
    pub capture_driver_build: String,
    /// Unix seconds. Formats observed at or before this are ineligible,
    /// regardless of age. Advanced whenever the fingerprint changes.
    pub watermark_secs: i64,
}

impl AppleFormatContext {
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for part in [
            self.macos_build.as_str(),
            self.music_app_version.as_str(),
            self.music_app_build.as_str(),
            self.capture_driver_build.as_str(),
        ] {
            hasher.update(part.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(self.helper_protocol_version.to_be_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Move the watermark to `now` because the context changed.
    pub fn advance_watermark(&mut self, now_secs: i64) {
        self.watermark_secs = now_secs;
    }
}

/// A verified format as the island builder consumes it.
#[derive(Clone, Debug, PartialEq)]
pub struct AppleFormatPrediction {
    pub codec: String,
    pub sample_rate_hz: u32,
    pub storefront: Option<String>,
    pub observed_at_secs: i64,
}

impl From<crate::library::AppleMusicTrackFormatRecord> for AppleFormatPrediction {
    fn from(record: crate::library::AppleMusicTrackFormatRecord) -> Self {
        Self {
            codec: record.codec,
            // A row whose rate does not fit a `u32` is corrupt rather than
            // merely unusual, and zero is never eligible.
            sample_rate_hz: u32::try_from(record.sample_rate).unwrap_or(0),
            storefront: record.storefront,
            observed_at_secs: record.observed_at,
        }
    }
}

impl AppleFormatPrediction {
    /// Whether this row may be used to predict a boundary.
    fn is_eligible(
        &self,
        canonical_storefront: &str,
        context: &AppleFormatContext,
        now_secs: i64,
    ) -> bool {
        if !self.codec.eq_ignore_ascii_case("ALAC") {
            return false;
        }
        if self.sample_rate_hz == 0 {
            return false;
        }
        // A rate verified in one storefront says nothing about another's
        // mastering of the same recording.
        if self
            .storefront
            .as_deref()
            .map(canonical_storefront_of)
            .as_deref()
            != Some(canonical_storefront)
        {
            return false;
        }
        if self.observed_at_secs <= context.watermark_secs {
            return false;
        }
        now_secs.saturating_sub(self.observed_at_secs) <= FORMAT_MAX_AGE_SECS
    }
}

/// Where the island's required sample rate comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppleIslandRateBasis {
    /// A cold start: the head's own cached prediction defines the rate, and a
    /// head with no eligible prediction produces a one-track island.
    HeadPrediction,
    /// A buffered successor path: capture is already running at a known rate,
    /// so that rate is the requirement and even the head must match it.
    LiveCaptureRate(u32),
}

/// Why the island stopped where it did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IslandStop {
    /// Everything eligible was included.
    QueueExhausted,
    /// The head has no eligible prediction, so nothing may follow it.
    HeadRateUnknown,
    /// The next item is not an Apple Music track.
    NonAppleSuccessor,
    DifferentStorefront,
    /// A song ID already in the island. Repeats are split so each occurrence
    /// gets its own playlist entry and its own identity.
    RepeatedSongId,
    NoEligiblePrediction,
    RateMismatch {
        expected_hz: u32,
        candidate_hz: u32,
    },
    LengthLimit,
}

/// A contiguous run Music.app may own.
#[derive(Clone, Debug, PartialEq)]
pub struct SafeAppleIsland {
    /// Occurrences in the island, head first. Never empty.
    pub items: Vec<PlaybackItem>,
    /// Catalog song IDs in the same order, for the create request.
    pub song_ids: Vec<String>,
    /// The rate every member is predicted to decode at, when known.
    pub rate_hz: Option<u32>,
    /// Canonical storefront every member shares.
    pub storefront: String,
    pub stopped_by: IslandStop,
}

impl SafeAppleIsland {
    /// Whether the head has a native Apple successor inside this island, which
    /// is what decides how many verified entries startup must wait for.
    pub fn has_native_successor(&self) -> bool {
        self.items.len() > 1
    }
}

/// Lowercase, trimmed storefront. Apple accepts `NZ`, `nz`, and ` nz `, and
/// splitting an island on that difference would be a bug rather than caution.
pub fn canonical_storefront_of(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Look up a track's cached prediction.
pub trait AppleFormatLookup {
    fn prediction(&self, song_id: &str) -> Option<AppleFormatPrediction>;
}

impl<F> AppleFormatLookup for F
where
    F: Fn(&str) -> Option<AppleFormatPrediction>,
{
    fn prediction(&self, song_id: &str) -> Option<AppleFormatPrediction> {
        self(song_id)
    }
}

/// Build the run of Apple tracks Music.app may play by itself.
///
/// Replaces the old `queue_playlist_song_ids`, which stopped only at a
/// non-Apple source and a cached-rate change. It did not canonicalize
/// storefronts, split repeats, check how old a cached rate was, or require the
/// head to have a rate at all — each of which could produce an island whose
/// second track silently needed a different capture session.
pub fn build_safe_apple_island(
    head: &PlaybackItem,
    tail: &[PlaybackItem],
    account_storefront: &str,
    rate_basis: AppleIslandRateBasis,
    format_context: &AppleFormatContext,
    now_secs: i64,
    formats: &impl AppleFormatLookup,
) -> Option<SafeAppleIsland> {
    let (head_song_id, head_storefront) = apple_identity(&head.source, account_storefront)?;
    let head_prediction = formats
        .prediction(&head_song_id)
        .filter(|prediction| prediction.is_eligible(&head_storefront, format_context, now_secs));

    let head_only = |stopped_by: IslandStop| SafeAppleIsland {
        items: vec![head.clone()],
        song_ids: vec![head_song_id.clone()],
        rate_hz: match rate_basis {
            AppleIslandRateBasis::LiveCaptureRate(rate) => Some(rate),
            AppleIslandRateBasis::HeadPrediction => {
                head_prediction.as_ref().map(|p| p.sample_rate_hz)
            }
        },
        storefront: head_storefront.clone(),
        stopped_by,
    };

    let required_rate_hz = match rate_basis {
        AppleIslandRateBasis::LiveCaptureRate(rate) => rate,
        AppleIslandRateBasis::HeadPrediction => {
            // Without a rate for the head there is nothing for a successor to
            // match, and guessing would mean discovering the mismatch as an
            // audible glitch rather than as a protected restart.
            let Some(prediction) = head_prediction.as_ref() else {
                return Some(head_only(IslandStop::HeadRateUnknown));
            };
            prediction.sample_rate_hz
        }
    };

    // On the buffered successor path the head must match the live rate too:
    // capture is already open and cannot be reconfigured under it.
    if matches!(rate_basis, AppleIslandRateBasis::LiveCaptureRate(_))
        && head_prediction
            .as_ref()
            .is_none_or(|prediction| prediction.sample_rate_hz != required_rate_hz)
    {
        return Some(head_only(IslandStop::HeadRateUnknown));
    }

    let mut items = vec![head.clone()];
    let mut song_ids = vec![head_song_id.clone()];
    let mut seen = HashSet::from([head_song_id]);
    let mut stopped_by = IslandStop::QueueExhausted;

    for candidate in tail {
        if items.len() >= MAX_ISLAND_TRACKS {
            stopped_by = IslandStop::LengthLimit;
            break;
        }
        let Some((song_id, storefront)) = apple_identity(&candidate.source, account_storefront)
        else {
            stopped_by = IslandStop::NonAppleSuccessor;
            break;
        };
        if storefront != head_storefront {
            stopped_by = IslandStop::DifferentStorefront;
            break;
        }
        // The same song twice in one playlist makes Music.app's positional
        // identity ambiguous, so the island is split and the repeat starts a
        // new one with its own occurrence identity intact.
        if !seen.insert(song_id.clone()) {
            stopped_by = IslandStop::RepeatedSongId;
            break;
        }
        let Some(prediction) = formats
            .prediction(&song_id)
            .filter(|prediction| prediction.is_eligible(&storefront, format_context, now_secs))
        else {
            stopped_by = IslandStop::NoEligiblePrediction;
            break;
        };
        if prediction.sample_rate_hz != required_rate_hz {
            stopped_by = IslandStop::RateMismatch {
                expected_hz: required_rate_hz,
                candidate_hz: prediction.sample_rate_hz,
            };
            break;
        }
        items.push(candidate.clone());
        song_ids.push(song_id);
    }

    Some(SafeAppleIsland {
        items,
        song_ids,
        rate_hz: Some(required_rate_hz),
        storefront: head_storefront,
        stopped_by,
    })
}

/// Song ID and canonical storefront, resolving a missing storefront to the
/// account's.
///
/// A `SourceRef` built from a search result or a restored queue often has no
/// storefront at all; treating that as "different from the head" would split
/// every such island for no reason.
fn apple_identity(source: &SourceRef, account_storefront: &str) -> Option<(String, String)> {
    let SourceRef::AppleMusicTrack {
        song_id,
        storefront,
        ..
    } = source
    else {
        return None;
    };
    let song_id = song_id.trim();
    if song_id.is_empty() {
        return None;
    }
    let storefront = storefront
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(account_storefront);
    Some((song_id.to_string(), canonical_storefront_of(storefront)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const NOW: i64 = 1_800_000_000;

    fn context() -> AppleFormatContext {
        AppleFormatContext {
            macos_build: "24F74".to_string(),
            music_app_version: "1.5.1".to_string(),
            music_app_build: "15.1.101".to_string(),
            helper_protocol_version: 4,
            capture_driver_build: "2026.7".to_string(),
            watermark_secs: NOW - 30 * 24 * 60 * 60,
        }
    }

    fn apple(song_id: &str, storefront: Option<&str>) -> SourceRef {
        SourceRef::AppleMusicTrack {
            song_id: song_id.to_string(),
            storefront: storefront.map(str::to_string),
            title: Some(song_id.to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            album_artist: None,
            album_id: None,
            artwork_url: None,
            duration_secs: Some(200.0),
            track_number: None,
            disc_number: None,
            isrc: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn local(track_id: i64) -> SourceRef {
        SourceRef::LocalTrack {
            track_id,
            file_name: None,
            title: None,
            artist: None,
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn prediction(rate_hz: u32, storefront: &str) -> AppleFormatPrediction {
        AppleFormatPrediction {
            codec: "ALAC".to_string(),
            sample_rate_hz: rate_hz,
            storefront: Some(storefront.to_string()),
            observed_at_secs: NOW - 60,
        }
    }

    struct Formats(HashMap<String, AppleFormatPrediction>);

    impl AppleFormatLookup for Formats {
        fn prediction(&self, song_id: &str) -> Option<AppleFormatPrediction> {
            self.0.get(song_id).cloned()
        }
    }

    fn formats(entries: &[(&str, AppleFormatPrediction)]) -> Formats {
        Formats(
            entries
                .iter()
                .map(|(id, prediction)| (id.to_string(), prediction.clone()))
                .collect(),
        )
    }

    fn build(
        sources: Vec<SourceRef>,
        rate_basis: AppleIslandRateBasis,
        formats: &Formats,
    ) -> SafeAppleIsland {
        let items = PlaybackItem::assign_all(sources);
        build_safe_apple_island(
            &items[0],
            &items[1..],
            "nz",
            rate_basis,
            &context(),
            NOW,
            formats,
        )
        .expect("an Apple head")
    }

    #[test]
    fn a_run_of_matching_tracks_forms_one_island() {
        let island = build(
            vec![
                apple("1", Some("nz")),
                apple("2", Some("nz")),
                apple("3", Some("nz")),
            ],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "nz")),
                ("3", prediction(44_100, "nz")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1", "2", "3"]);
        assert_eq!(island.rate_hz, Some(44_100));
        assert_eq!(island.stopped_by, IslandStop::QueueExhausted);
        assert!(island.has_native_successor());
    }

    /// Nothing may follow a head whose rate is unknown: there is no rate for a
    /// successor to match, and guessing turns a protected restart into an
    /// audible glitch.
    #[test]
    fn an_unknown_head_always_produces_a_one_track_island() {
        let island = build(
            vec![apple("1", Some("nz")), apple("2", Some("nz"))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[("2", prediction(44_100, "nz"))]),
        );

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(island.stopped_by, IslandStop::HeadRateUnknown);
        assert_eq!(island.rate_hz, None);
        assert!(!island.has_native_successor());
    }

    #[test]
    fn a_rate_change_splits_the_island() {
        let island = build(
            vec![
                apple("1", Some("nz")),
                apple("2", Some("nz")),
                apple("3", Some("nz")),
            ],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(96_000, "nz")),
                ("3", prediction(44_100, "nz")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(
            island.stopped_by,
            IslandStop::RateMismatch {
                expected_hz: 44_100,
                candidate_hz: 96_000,
            }
        );
    }

    #[test]
    fn a_non_apple_successor_ends_the_island() {
        let island = build(
            vec![apple("1", Some("nz")), local(9), apple("2", Some("nz"))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "nz")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(island.stopped_by, IslandStop::NonAppleSuccessor);
    }

    /// A `SourceRef` from a search result or a restored queue often carries no
    /// storefront; splitting every such island would be a bug, not caution.
    #[test]
    fn a_missing_storefront_resolves_to_the_account_storefront() {
        let island = build(
            vec![apple("1", None), apple("2", None)],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "nz")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1", "2"]);
        assert_eq!(island.storefront, "nz");
    }

    #[test]
    fn storefront_case_and_padding_do_not_split_an_island() {
        let island = build(
            vec![apple("1", Some("NZ")), apple("2", Some(" nz "))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "NZ")),
                ("2", prediction(44_100, "nz")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1", "2"]);
    }

    #[test]
    fn genuinely_different_storefronts_split_the_island() {
        let island = build(
            vec![apple("1", Some("nz")), apple("2", Some("us"))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "us")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(island.stopped_by, IslandStop::DifferentStorefront);
    }

    /// A rate verified in one storefront says nothing about another's
    /// mastering, so the cached row is simply not applicable.
    #[test]
    fn a_prediction_from_another_storefront_is_ineligible() {
        let island = build(
            vec![apple("1", Some("nz")), apple("2", Some("nz"))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "us")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(island.stopped_by, IslandStop::NoEligiblePrediction);
    }

    /// The same song twice makes Music.app's positional identity ambiguous, so
    /// the island splits and each occurrence keeps its own identity.
    #[test]
    fn a_repeated_song_id_splits_before_the_helper_ever_sees_it() {
        let island = build(
            vec![
                apple("1", Some("nz")),
                apple("2", Some("nz")),
                apple("1", Some("nz")),
            ],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "nz")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1", "2"]);
        assert_eq!(island.stopped_by, IslandStop::RepeatedSongId);
    }

    #[test]
    fn a_stale_prediction_is_ineligible() {
        let mut stale = prediction(44_100, "nz");
        stale.observed_at_secs = NOW - FORMAT_MAX_AGE_SECS - 1;
        let island = build(
            vec![apple("1", Some("nz")), apple("2", Some("nz"))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[("1", prediction(44_100, "nz")), ("2", stale)]),
        );

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(island.stopped_by, IslandStop::NoEligiblePrediction);
    }

    /// A Music.app or macOS update can change how the same catalog item
    /// decodes, so every prediction observed beforehand stops gating boundaries
    /// even though the rows remain stored.
    #[test]
    fn a_build_context_change_invalidates_prior_eligibility() {
        let items = PlaybackItem::assign_all(vec![apple("1", Some("nz")), apple("2", Some("nz"))]);
        let formats = formats(&[
            ("1", prediction(44_100, "nz")),
            ("2", prediction(44_100, "nz")),
        ]);
        let mut context = context();
        context.advance_watermark(NOW);
        context.music_app_build = "1.5.2".to_string();

        let island = build_safe_apple_island(
            &items[0],
            &items[1..],
            "nz",
            AppleIslandRateBasis::HeadPrediction,
            &context,
            NOW,
            &formats,
        )
        .expect("an Apple head");

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(island.stopped_by, IslandStop::HeadRateUnknown);
    }

    #[test]
    fn the_format_context_fingerprint_tracks_every_component() {
        let base = context();
        for mutate in [
            |c: &mut AppleFormatContext| c.macos_build = "24G90".to_string(),
            |c: &mut AppleFormatContext| c.music_app_version = "1.6.0".to_string(),
            |c: &mut AppleFormatContext| c.music_app_build = "1.6.0.5".to_string(),
            |c: &mut AppleFormatContext| c.helper_protocol_version = 5,
            |c: &mut AppleFormatContext| c.capture_driver_build = "2026.8".to_string(),
        ] {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(base.fingerprint(), changed.fingerprint());
        }
    }

    #[test]
    fn a_lossy_or_unknown_codec_is_never_eligible() {
        let mut aac = prediction(44_100, "nz");
        aac.codec = "AAC".to_string();
        let island = build(
            vec![apple("1", Some("nz")), apple("2", Some("nz"))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[("1", prediction(44_100, "nz")), ("2", aac)]),
        );

        assert_eq!(island.song_ids, vec!["1"]);
        assert_eq!(island.stopped_by, IslandStop::NoEligiblePrediction);
    }

    /// The buffered successor path: capture is already open at a known rate and
    /// cannot be reconfigured underneath, so the head must match it too.
    #[test]
    fn the_live_capture_rate_is_the_requirement_on_the_buffered_path() {
        let matching = build(
            vec![apple("1", Some("nz")), apple("2", Some("nz"))],
            AppleIslandRateBasis::LiveCaptureRate(44_100),
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "nz")),
            ]),
        );
        assert_eq!(matching.song_ids, vec!["1", "2"]);

        let mismatched = build(
            vec![apple("1", Some("nz")), apple("2", Some("nz"))],
            AppleIslandRateBasis::LiveCaptureRate(96_000),
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(96_000, "nz")),
            ]),
        );
        assert_eq!(mismatched.song_ids, vec!["1"]);
        assert_eq!(mismatched.stopped_by, IslandStop::HeadRateUnknown);
        assert_eq!(mismatched.rate_hz, Some(96_000));
    }

    /// A song-level cache row is enough. The album is irrelevant here, and the
    /// old album-keyed lookup returned nothing for a track whose album ID was
    /// missing from the source.
    #[test]
    fn a_song_level_prediction_works_without_an_album() {
        let island = build(
            vec![apple("1", Some("nz")), apple("2", Some("nz"))],
            AppleIslandRateBasis::HeadPrediction,
            &formats(&[
                ("1", prediction(44_100, "nz")),
                ("2", prediction(44_100, "nz")),
            ]),
        );

        assert_eq!(island.song_ids, vec!["1", "2"]);
    }

    #[test]
    fn a_non_apple_head_has_no_island() {
        let items = PlaybackItem::assign_all(vec![local(1)]);

        assert!(
            build_safe_apple_island(
                &items[0],
                &[],
                "nz",
                AppleIslandRateBasis::HeadPrediction,
                &context(),
                NOW,
                &formats(&[]),
            )
            .is_none()
        );
    }
}
