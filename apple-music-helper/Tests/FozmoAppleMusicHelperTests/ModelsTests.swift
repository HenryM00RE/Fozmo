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
            artworkURL: "https://example.test/art.jpg"
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
    }
}
