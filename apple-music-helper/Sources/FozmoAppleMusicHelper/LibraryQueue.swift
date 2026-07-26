import Foundation
import MusicKit

/// Apple Music Web API access for the Fozmo queue playlist.
///
/// MusicKit's `MusicLibrary` write API — `add`, `createPlaylist`, `edit` — is
/// `@available(macOS, unavailable)`, so the helper cannot promote a catalog
/// song to a library item through Swift. `MusicDataRequest` is available on
/// macOS and signs requests with the developer and music-user tokens of the
/// provisioned app, which leaves the Web API as the only route.
///
/// Fozmo needs library items because Music.app's AppleScript `current track`
/// cannot see catalog tracks that are not in the library, and because Music.app
/// only advances a queue gaplessly when playback started from a container it
/// owns.
enum AppleMusicWebAPI {
    private static let base = URL(string: "https://api.music.apple.com/v1/me/library/playlists")!

    /// Catalog songs are added to a library playlist by catalog ID under the
    /// `songs` type; `library-songs` would require an already-imported item.
    private static let catalogSongType = "songs"

    struct Playlist: Equatable {
        let id: String
        let name: String
        let canEdit: Bool
    }

    enum Failure: Error {
        /// Apple rejected the request; `status` distinguishes an auth problem
        /// from a transient one.
        case http(status: Int, body: String)
        case malformedResponse
    }

    static func playlists() async throws -> [Playlist] {
        var found: [Playlist] = []
        var next: URL? = URL(string: "\(base.absoluteString)?limit=100")
        // The library can hold more playlists than one page, and the Fozmo
        // playlist is not guaranteed to be on the first one.
        while let url = next {
            let json = try await requestJSON(url: url, method: "GET", body: nil)
            guard let data = json["data"] as? [[String: Any]] else {
                throw Failure.malformedResponse
            }
            for entry in data {
                guard
                    let id = entry["id"] as? String,
                    let attributes = entry["attributes"] as? [String: Any],
                    let name = attributes["name"] as? String
                else { continue }
                found.append(
                    Playlist(
                        id: id,
                        name: name,
                        canEdit: attributes["canEdit"] as? Bool ?? false
                    )
                )
            }
            next = (json["next"] as? String).flatMap {
                URL(string: $0.hasPrefix("http") ? $0 : "https://api.music.apple.com\($0)")
            }
        }
        return found
    }

    /// Create the queue playlist holding exactly `catalogSongIDs`, in order.
    ///
    /// Creating with the tracks attached is a single round trip and leaves no
    /// window where the playlist exists but is empty, which Fozmo would
    /// otherwise be able to observe and start playing.
    static func createPlaylist(
        name: String,
        description: String,
        catalogSongIDs: [String]
    ) async throws -> String {
        let body: [String: Any] = [
            "attributes": [
                "name": name,
                "description": description,
                "isPublic": false,
            ],
            "relationships": [
                "tracks": [
                    "data": catalogSongIDs.map { ["id": $0, "type": catalogSongType] }
                ]
            ],
        ]
        let json = try await requestJSON(url: base, method: "POST", body: body)
        guard
            let data = json["data"] as? [[String: Any]],
            let id = data.first?["id"] as? String
        else {
            throw Failure.malformedResponse
        }
        return id
    }

    static func addTracks(playlistID: String, catalogSongIDs: [String]) async throws {
        let url = base.appendingPathComponent(playlistID).appendingPathComponent("tracks")
        let body: [String: Any] = [
            "data": catalogSongIDs.map { ["id": $0, "type": catalogSongType] }
        ]
        _ = try await requestJSON(url: url, method: "POST", body: body)
    }

    @discardableResult
    private static func requestJSON(
        url: URL,
        method: String,
        body: [String: Any]?
    ) async throws -> [String: Any] {
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = method
        if let body {
            urlRequest.httpBody = try JSONSerialization.data(withJSONObject: body)
            urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let response = try await MusicDataRequest(urlRequest: urlRequest).response()
        let status = response.urlResponse.statusCode
        guard (200..<300).contains(status) else {
            throw Failure.http(
                status: status,
                body: String(data: response.data, encoding: .utf8) ?? ""
            )
        }
        // `POST .../tracks` answers 204 with no body, and a create answers 201
        // with one. Both are successes.
        guard !response.data.isEmpty else { return [:] }
        guard
            let json = try JSONSerialization.jsonObject(with: response.data) as? [String: Any]
        else {
            throw Failure.malformedResponse
        }
        return json
    }
}
