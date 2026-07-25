import Foundation
import MusicKit
import XCTest
@testable import FozmoAppleMusicHelper

final class ModelsTests: XCTestCase {
    func testCatalogCommandDecodesVersionedWireNames() throws {
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
        XCTAssertEqual(command.version, 2)
        XCTAssertEqual(command.id, "cmd-9")
        XCTAssertEqual(command.albumID, "album-1")
        XCTAssertEqual(command.storefront, "nz")
    }

    func testAuthorizationLabelsAreStableProtocolValues() {
        XCTAssertEqual(AuthorizationLabel.string(for: .notDetermined), "not_determined")
        XCTAssertEqual(AuthorizationLabel.string(for: .denied), "denied")
        XCTAssertEqual(AuthorizationLabel.string(for: .restricted), "restricted")
        XCTAssertEqual(AuthorizationLabel.string(for: .authorized), "authorized")
    }

    func testEventEncodingOmitsAbsentAndPlaybackFields() throws {
        var event = HelperEvent(type: "ready")
        event.sessionID = "am-test"
        event.authorization = "authorized"
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(event)) as? [String: Any]
        )
        XCTAssertEqual(object["v"] as? Int, 2)
        XCTAssertNil(object["token"])
        XCTAssertNil(object["playback_state"])
        XCTAssertNil(object["now_playing"])
        XCTAssertNil(object["queue_revision"])
    }

    func testCatalogSearchPayloadUsesStableWireFields() throws {
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
              "editorial_notes_standard": "A landmark electronic album.",
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
        XCTAssertEqual((search["songs"] as? [[String: Any]])?.first?["song_id"] as? String, "song-1")
        XCTAssertEqual(
            (search["albums"] as? [[String: Any]])?.first?["album_id"] as? String,
            "album-1"
        )
    }

    func testCommandResponseCacheIsIdempotentAndBounded() {
        var cache = HelperResponseCache(limit: 2)
        var first = HelperEvent(type: "ready")
        first.commandID = "cmd-z"
        var replacement = HelperEvent(type: "ready")
        replacement.commandID = "cmd-z"
        replacement.authorization = "authorized"
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
    }

    func testCommandLedgerSuppressesDuplicatesAndReplaysCompletion() {
        var ledger = HelperCommandLedger(limit: 2)
        var completed = HelperEvent(type: "catalog_song")
        completed.commandID = "cmd-1"

        XCTAssertEqual(ledger.begin(commandID: "cmd-1"), .start)
        XCTAssertEqual(ledger.begin(commandID: "cmd-1"), .inFlight)
        ledger.complete(completed, commandID: "cmd-1")
        XCTAssertEqual(ledger.begin(commandID: "cmd-1"), .cached(completed))
    }

    func testCatalogInputsAndPayloadNormalizeDTOFields() throws {
        XCTAssertEqual(CatalogInput.normalizedID("  song-1 \n"), "song-1")
        XCTAssertNil(CatalogInput.normalizedID(" \n "))
        XCTAssertEqual(CatalogInput.normalizedStorefront(" NZ "), "nz")
        XCTAssertEqual(CatalogInput.normalizedStorefront(nil), "current")
        XCTAssertEqual(CatalogInput.normalizedSearchLimit(nil), 10)
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
        XCTAssertEqual(object["album_title"] as? String, "Album")
        XCTAssertEqual(object["audio_variants"] as? [String], ["lossless"])
    }
}
