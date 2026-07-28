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
    private static let folderBase =
        URL(string: "https://api.music.apple.com/v1/me/library/playlist-folders")!

    /// Catalog songs are added to a library playlist by catalog ID under the
    /// `songs` type; `library-songs` would require an already-imported item.
    private static let catalogSongType = "songs"

    struct Playlist: Equatable {
        let id: String
        let name: String
        let description: String?
        let canEdit: Bool
    }

    struct PlaylistTrack: Equatable {
        let catalogID: String?
    }

    struct PlaylistFolder: Equatable {
        let id: String
        let name: String
        let description: String?
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
                        description: descriptionText(attributes["description"]),
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

    /// Fetch one known Web playlist without enumerating the listener's
    /// library. A 404 is a normal reconciliation result.
    static func playlist(playlistID: String) async throws -> Playlist? {
        let url = base.appendingPathComponent(playlistID)
        do {
            let json = try await requestJSON(url: url, method: "GET", body: nil)
            guard let data = json["data"] as? [[String: Any]] else {
                throw Failure.malformedResponse
            }
            guard let entry = data.first else { return nil }
            return playlist(from: entry)
        } catch Failure.http(let status, _) where status == 404 {
            return nil
        }
    }

    static func playlistFolders() async throws -> [PlaylistFolder] {
        var found: [PlaylistFolder] = []
        var next: URL? = URL(string: "\(folderBase.absoluteString)?limit=100")
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
                    PlaylistFolder(
                        id: id,
                        name: name,
                        description: descriptionText(attributes["description"])
                    )
                )
            }
            next = (json["next"] as? String).flatMap {
                URL(string: $0.hasPrefix("http") ? $0 : "https://api.music.apple.com\($0)")
            }
        }
        return found
    }

    static func createPlaylistFolder(name: String, description: String) async throws -> String {
        let body: [String: Any] = [
            "attributes": [
                "name": name,
                "description": description,
            ]
        ]
        let json = try await requestJSON(url: folderBase, method: "POST", body: body)
        guard
            let data = json["data"] as? [[String: Any]],
            let id = data.first?["id"] as? String
        else {
            throw Failure.malformedResponse
        }
        return id
    }

    /// Ordered tracks Apple actually stored for a library playlist.
    ///
    /// Library-song IDs are private to the user's library. `catalogId` is the
    /// stable identity Fozmo can compare with the catalog IDs it requested.
    static func playlistTracks(playlistID: String) async throws -> [PlaylistTrack] {
        var found: [PlaylistTrack] = []
        var next: URL? =
            base
            .appendingPathComponent(playlistID)
            .appendingPathComponent("tracks")
        if let initial = next {
            var components = URLComponents(url: initial, resolvingAgainstBaseURL: false)
            components?.queryItems = [URLQueryItem(name: "limit", value: "100")]
            next = components?.url
        }
        while let url = next {
            let json = try await requestJSON(url: url, method: "GET", body: nil)
            guard let data = json["data"] as? [[String: Any]] else {
                throw Failure.malformedResponse
            }
            for entry in data {
                let attributes = entry["attributes"] as? [String: Any]
                let playParams = attributes?["playParams"] as? [String: Any]
                found.append(PlaylistTrack(catalogID: playParams?["catalogId"] as? String))
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
        catalogSongIDs: [String],
        parentFolderID: String? = nil
    ) async throws -> String {
        var relationships: [String: Any] = [
            "tracks": [
                "data": catalogSongIDs.map { ["id": $0, "type": catalogSongType] }
            ]
        ]
        if let parentFolderID {
            relationships["parent"] = [
                "data": [
                    "id": parentFolderID,
                    "type": "library-playlist-folders",
                ]
            ]
        }
        let body: [String: Any] = [
            "attributes": [
                "name": name,
                "description": description,
                "isPublic": false,
            ],
            "relationships": relationships,
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

    private static func descriptionText(_ value: Any?) -> String? {
        if let value = value as? String {
            return value
        }
        if let value = value as? [String: Any] {
            return value["standard"] as? String
        }
        return nil
    }

    private static func playlist(from entry: [String: Any]) -> Playlist? {
        guard
            let id = entry["id"] as? String,
            let attributes = entry["attributes"] as? [String: Any],
            let name = attributes["name"] as? String
        else { return nil }
        return Playlist(
            id: id,
            name: name,
            description: descriptionText(attributes["description"]),
            canEdit: attributes["canEdit"] as? Bool ?? false
        )
    }
}
