import Foundation
import MusicKit
import XCTest
@testable import FozmoAppleMusicHelper

final class ModelsTests: XCTestCase {
    func testQueueCommandDecodesVersionedWireNames() throws {
        let data = Data(
            """
            {
              "v": 2,
              "id": "cmd-7",
              "type": "set_queue",
              "session_id": "am-test",
              "queue_revision": 9,
              "items": [{"song_id": "2037093408", "storefront": "nz", "segment_index": 4}],
              "start_index": 0
            }
            """.utf8
        )
        let command = try JSONDecoder().decode(IncomingCommand.self, from: data)
        XCTAssertEqual(command.version, 2)
        XCTAssertEqual(command.id, "cmd-7")
        XCTAssertEqual(command.queueRevision, 9)
        XCTAssertEqual(
            command.items,
            [QueueItem(songID: "2037093408", storefront: "nz", segmentIndex: 4)]
        )
    }

    func testAuthorizationLabelsAreStableProtocolValues() {
        XCTAssertEqual(AuthorizationLabel.string(for: .notDetermined), "not_determined")
        XCTAssertEqual(AuthorizationLabel.string(for: .denied), "denied")
        XCTAssertEqual(AuthorizationLabel.string(for: .restricted), "restricted")
        XCTAssertEqual(AuthorizationLabel.string(for: .authorized), "authorized")
    }

    func testEventEncodingDoesNotIncludeAbsentSecrets() throws {
        var event = HelperEvent(type: "ready")
        event.sessionID = "am-test"
        event.authorization = "authorized"
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(event)) as? [String: Any]
        )
        XCTAssertEqual(object["v"] as? Int, 2)
        XCTAssertNil(object["token"])
        XCTAssertNil(object["now_playing"])
    }

    func testQueueItemsPreserveDuplicateSongOccurrencesBySegmentIndex() throws {
        let data = Data(
            """
            {
              "v": 2,
              "id": "cmd-8",
              "type": "set_queue",
              "session_id": "am-test",
              "queue_revision": 10,
              "items": [
                {"song_id": "same", "segment_index": 0},
                {"song_id": "same", "segment_index": 1}
              ],
              "start_index": 0
            }
            """.utf8
        )

        let command = try JSONDecoder().decode(IncomingCommand.self, from: data)

        XCTAssertEqual(command.items?.map(\.songID), ["same", "same"])
        XCTAssertEqual(command.items?.map(\.segmentIndex), [0, 1])
    }

    func testQueueFinishedReasonUsesStableWireName() throws {
        var event = HelperEvent(type: "queue_finished")
        event.sessionID = "am-test"
        event.queueRevision = 12
        event.segmentIndex = 2
        event.songID = "song-2"
        event.playbackPosition = 181.25
        event.finishReason = "completed"

        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(event)) as? [String: Any]
        )

        XCTAssertEqual(object["type"] as? String, "queue_finished")
        XCTAssertEqual(object["queue_revision"] as? UInt64, 12)
        XCTAssertEqual(object["segment_index"] as? Int, 2)
        XCTAssertEqual(object["finish_reason"] as? String, "completed")
    }

    func testCatalogCommandDecodesWithoutTokenFields() throws {
        let data = Data(
            """
            {
              "v": 2,
              "id": "cmd-9",
              "type": "lookup_album",
              "session_id": "am-test",
              "album_id": "album-1",
              "storefront": "nz"
            }
            """.utf8
        )

        let command = try JSONDecoder().decode(IncomingCommand.self, from: data)

        XCTAssertEqual(command.albumID, "album-1")
        XCTAssertEqual(command.storefront, "nz")
    }

    func testCatalogSearchCommandAndPayloadUseStableWireFields() throws {
        let data = Data(
            """
            {
              "v": 2,
              "id": "cmd-search",
              "type": "search_songs",
              "session_id": "am-test",
              "term": "Björk Jóga",
              "limit": 8,
              "storefront": "nz"
            }
            """.utf8
        )

        let command = try JSONDecoder().decode(IncomingCommand.self, from: data)
        XCTAssertEqual(command.term, "Björk Jóga")
        XCTAssertEqual(command.limit, 8)

        let song = CatalogSongPayload(
            songID: "song-1",
            storefront: "nz",
            albumID: nil,
            title: "Jóga",
            artist: "Björk",
            albumTitle: "Homogenic",
            albumArtist: "Björk",
            durationSecs: 312,
            trackNumber: 2,
            discNumber: 1,
            isrc: nil,
            artworkURL: "https://example.test/joga.jpg"
        )
        let albumData = Data(
            """
            {
              "album_id": "album-1",
              "storefront": "nz",
              "title": "Homogenic",
              "artist": "Björk",
              "artwork_url": "https://example.test/homogenic.jpg",
              "audio_variants": [],
              "tracks": []
            }
            """.utf8
        )
        let album = try JSONDecoder().decode(CatalogAlbumPayload.self, from: albumData)
        var event = HelperEvent(type: "catalog_search")
        event.catalogSearch = CatalogSearchPayload(
            term: "Björk Jóga",
            storefront: "nz",
            songs: [song],
            albums: [album]
        )
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(event)) as? [String: Any]
        )
        let search = try XCTUnwrap(object["catalog_search"] as? [String: Any])
        XCTAssertEqual(search["term"] as? String, "Björk Jóga")
        XCTAssertEqual((search["songs"] as? [[String: Any]])?.first?["song_id"] as? String, "song-1")
        XCTAssertEqual(
            (search["albums"] as? [[String: Any]])?.first?["album_id"] as? String,
            "album-1"
        )
    }

    func testQueuePlanValidationRejectsStaleAndMalformedPlans() {
        let first = QueueItem(songID: "same", storefront: "nz", segmentIndex: 0)
        let second = QueueItem(songID: "same", storefront: "nz", segmentIndex: 1)

        XCTAssertEqual(
            QueuePlanValidator.validate(
                revision: 8,
                currentRevision: 7,
                items: [first, second],
                startIndex: 1
            ),
            ValidatedQueuePlan(revision: 8, items: [first, second], startIndex: 1)
        )
        XCTAssertNil(
            QueuePlanValidator.validate(
                revision: 7,
                currentRevision: 7,
                items: [first],
                startIndex: 0
            )
        )
        XCTAssertNil(
            QueuePlanValidator.validate(
                revision: 8,
                currentRevision: 7,
                items: [first, QueueItem(songID: "other", storefront: nil, segmentIndex: 0)],
                startIndex: 0
            )
        )
        XCTAssertNil(
            QueuePlanValidator.validate(
                revision: 8,
                currentRevision: 7,
                items: [first],
                startIndex: 2
            )
        )
        XCTAssertNil(
            QueuePlanValidator.validate(
                revision: 8,
                currentRevision: 7,
                items: [QueueItem(songID: "  ", storefront: nil, segmentIndex: 0)],
                startIndex: 0
            )
        )
    }

    func testQueueIndexTransitionsAreForwardUniqueAndKnown() {
        let valid: Set<Int> = [3, 4, 5]

        XCTAssertTrue(
            QueueIndexTransition.shouldEmit(previous: nil, next: 3, validSegments: valid)
        )
        XCTAssertTrue(
            QueueIndexTransition.shouldEmit(previous: 3, next: 4, validSegments: valid)
        )
        XCTAssertFalse(
            QueueIndexTransition.shouldEmit(previous: 4, next: 4, validSegments: valid)
        )
        XCTAssertFalse(
            QueueIndexTransition.shouldEmit(previous: 4, next: 3, validSegments: valid)
        )
        XCTAssertFalse(
            QueueIndexTransition.shouldEmit(previous: 4, next: 9, validSegments: valid)
        )
    }

    func testUnknownMusicKitEntryFallsBackToTheNextMatchingSongSegment() {
        let items = [
            QueueItem(songID: "same", storefront: "nz", segmentIndex: 3),
            QueueItem(songID: "other", storefront: "nz", segmentIndex: 4),
            QueueItem(songID: "same", storefront: "nz", segmentIndex: 5),
        ]

        XCTAssertEqual(
            QueueEntrySegmentResolver.resolve(
                mappedSegment: nil,
                songID: "same",
                currentSegment: 3,
                items: items
            ),
            5
        )
        XCTAssertEqual(
            QueueEntrySegmentResolver.resolve(
                mappedSegment: 4,
                songID: "same",
                currentSegment: 3,
                items: items
            ),
            4
        )
    }

    func testObservedStopSelectsCompletedReasonOnlyAfterActivePlayback() {
        XCTAssertEqual(
            QueueFinishReason.forPlaybackTransition(previous: "playing", current: "stopped"),
            "completed"
        )
        XCTAssertEqual(
            QueueFinishReason.forPlaybackTransition(previous: "paused", current: "stopped"),
            "completed"
        )
        XCTAssertNil(
            QueueFinishReason.forPlaybackTransition(previous: "stopped", current: "stopped")
        )
        XCTAssertNil(
            QueueFinishReason.forPlaybackTransition(previous: "playing", current: "paused")
        )
    }

    func testAudioVariantPolicyAllowsOnlyLosslessVariants() {
        XCTAssertTrue(AudioVariantPolicy.permitsLosslessPlayback(.lossless))
        XCTAssertTrue(AudioVariantPolicy.permitsLosslessPlayback(.highResolutionLossless))
        XCTAssertFalse(AudioVariantPolicy.permitsLosslessPlayback(.lossyStereo))
        XCTAssertFalse(AudioVariantPolicy.permitsLosslessPlayback(.dolbyAtmos))
        XCTAssertFalse(AudioVariantPolicy.permitsLosslessPlayback(nil))
        XCTAssertTrue(
            AudioVariantPolicy.catalogOffersLosslessPlayback([.lossyStereo, .lossless])
        )
        XCTAssertFalse(AudioVariantPolicy.catalogOffersLosslessPlayback([.lossyStereo]))
        XCTAssertFalse(AudioVariantPolicy.catalogOffersLosslessPlayback(nil))
        XCTAssertEqual(AudioVariantPolicy.label(for: .lossyStereo), "lossyStereo")
    }

    func testActiveAudioVariantGateDefersMissingMetadataButRejectsExplicitLossyPlayback() {
        XCTAssertEqual(
            AudioVariantPolicy.activePlaybackDisposition(nil),
            .requiresDecoderProof
        )
        XCTAssertEqual(
            AudioVariantPolicy.activePlaybackDisposition(.lossless),
            .confirmedLossless
        )
        XCTAssertEqual(
            AudioVariantPolicy.activePlaybackDisposition(.highResolutionLossless),
            .confirmedLossless
        )
        XCTAssertEqual(
            AudioVariantPolicy.activePlaybackDisposition(.lossyStereo),
            .reject
        )
        XCTAssertEqual(
            AudioVariantPolicy.activePlaybackDisposition(.dolbyAtmos),
            .reject
        )
    }

    func testCompletionWatchdogAcceptsNaturalPausedEndButNotExplicitPause() {
        var watchdog = QueueCompletionWatchdog()
        XCTAssertFalse(
            watchdog.observe(
                playbackState: "playing",
                position: 99.5,
                duration: 100,
                isFinalEntry: true,
                explicitPause: false
            )
        )
        XCTAssertTrue(
            watchdog.observe(
                playbackState: "paused",
                position: 100,
                duration: 100,
                isFinalEntry: true,
                explicitPause: false
            )
        )

        watchdog.reset()
        XCTAssertFalse(
            watchdog.observe(
                playbackState: "paused",
                position: 100,
                duration: 100,
                isFinalEntry: true,
                explicitPause: true
            )
        )
    }

    func testCompletionWatchdogAcceptsAStuckPlayingTerminalPosition() {
        var watchdog = QueueCompletionWatchdog()
        for _ in 0..<2 {
            XCTAssertFalse(
                watchdog.observe(
                    playbackState: "playing",
                    position: 100,
                    duration: 100,
                    isFinalEntry: true,
                    explicitPause: false
                )
            )
        }
        XCTAssertTrue(
            watchdog.observe(
                playbackState: "playing",
                position: 100,
                duration: 100,
                isFinalEntry: true,
                explicitPause: false
            )
        )
    }

    func testCommandResponseCacheIsIdempotentAndEvictsInsertionOrder() {
        var cache = HelperResponseCache(limit: 2)
        var first = HelperEvent(type: "ready")
        first.commandID = "cmd-z"
        var replacement = HelperEvent(type: "ready")
        replacement.commandID = "cmd-z"
        replacement.queueRevision = 2
        var second = HelperEvent(type: "ready")
        second.commandID = "cmd-a"
        var third = HelperEvent(type: "ready")
        third.commandID = "cmd-b"

        cache.store(first, for: "cmd-z")
        cache.store(replacement, for: "cmd-z")
        cache.store(second, for: "cmd-a")

        XCTAssertEqual(cache.count, 2)
        XCTAssertEqual(cache.event(for: "cmd-z"), replacement)

        cache.store(third, for: "cmd-b")

        XCTAssertNil(cache.event(for: "cmd-z"))
        XCTAssertEqual(cache.event(for: "cmd-a"), second)
        XCTAssertEqual(cache.event(for: "cmd-b"), third)
    }

    func testCommandLedgerSuppressesInFlightDuplicatesAndReplaysCompletion() {
        var ledger = HelperCommandLedger(limit: 2)
        var completed = HelperEvent(type: "queue_prepared")
        completed.commandID = "cmd-1"
        completed.queueRevision = 4

        XCTAssertEqual(ledger.begin(commandID: "cmd-1"), .start)
        XCTAssertEqual(ledger.begin(commandID: "cmd-1"), .inFlight)

        ledger.complete(completed, commandID: "cmd-1")

        XCTAssertEqual(ledger.begin(commandID: "cmd-1"), .cached(completed))
    }

    func testCatalogInputsAndPayloadUseStableNormalizedDTOFields() throws {
        XCTAssertEqual(CatalogInput.normalizedID("  song-1 \n"), "song-1")
        XCTAssertNil(CatalogInput.normalizedID(" \n "))
        XCTAssertEqual(CatalogInput.normalizedStorefront(" NZ "), "nz")
        XCTAssertEqual(CatalogInput.normalizedStorefront(nil), "current")
        XCTAssertEqual(CatalogInput.normalizedSearchTerm("  Björk – Jóga "), "Björk – Jóga")
        XCTAssertNil(CatalogInput.normalizedSearchTerm(" \n "))
        XCTAssertNil(CatalogInput.normalizedSearchTerm(String(repeating: "x", count: 201)))
        XCTAssertEqual(CatalogInput.normalizedSearchLimit(nil), 10)
        XCTAssertEqual(CatalogInput.normalizedSearchLimit(25), 25)
        XCTAssertNil(CatalogInput.normalizedSearchLimit(0))
        XCTAssertNil(CatalogInput.normalizedSearchLimit(26))

        let payload = CatalogSongPayload(
            songID: "song-1",
            storefront: "nz",
            albumID: "album-1",
            title: "Title",
            artist: "Artist",
            albumTitle: "Album",
            albumArtist: "Album Artist",
            durationSecs: 123.5,
            trackNumber: 2,
            discNumber: 1,
            isrc: "NZABC2600001",
            artworkURL: "https://example.test/art.jpg",
            audioVariants: ["lossless"]
        )
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(payload)) as? [String: Any]
        )

        XCTAssertEqual(object["song_id"] as? String, "song-1")
        XCTAssertEqual(object["album_id"] as? String, "album-1")
        XCTAssertEqual(object["album_title"] as? String, "Album")
        XCTAssertEqual(object["album_artist"] as? String, "Album Artist")
        XCTAssertEqual(object["duration_secs"] as? Double, 123.5)
        XCTAssertEqual(object["track_number"] as? Int, 2)
        XCTAssertEqual(object["disc_number"] as? Int, 1)
        XCTAssertEqual(object["artwork_url"] as? String, "https://example.test/art.jpg")
        XCTAssertEqual(object["audio_variants"] as? [String], ["lossless"])
    }
}
