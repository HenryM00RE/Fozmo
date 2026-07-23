import Foundation
import MusicKit
import XCTest
@testable import FozmoAppleMusicHelper

final class ModelsTests: XCTestCase {
    func testQueueCommandDecodesVersionedWireNames() throws {
        let data = Data(
            """
            {
              "v": 1,
              "id": "cmd-7",
              "type": "set_queue",
              "session_id": "am-test",
              "queue_revision": 9,
              "items": [{"song_id": "2037093408", "storefront": "nz"}],
              "start_index": 0
            }
            """.utf8
        )
        let command = try JSONDecoder().decode(IncomingCommand.self, from: data)
        XCTAssertEqual(command.version, 1)
        XCTAssertEqual(command.id, "cmd-7")
        XCTAssertEqual(command.queueRevision, 9)
        XCTAssertEqual(
            command.items,
            [QueueItem(songID: "2037093408", storefront: "nz")]
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
        XCTAssertEqual(object["v"] as? Int, 1)
        XCTAssertNil(object["token"])
        XCTAssertNil(object["now_playing"])
    }
}
