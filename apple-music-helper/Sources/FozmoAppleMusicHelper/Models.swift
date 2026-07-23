import Foundation
import MusicKit

let helperProtocolVersion = 1
let helperBundleIdentifier = "com.fozmo.apple-music-helper"

struct QueueItem: Codable, Equatable {
    let songID: String
    let storefront: String?

    enum CodingKeys: String, CodingKey {
        case songID = "song_id"
        case storefront
    }
}

struct IncomingCommand: Decodable, Equatable {
    let version: Int
    let id: String
    let type: String
    let sessionID: String
    let protocolVersion: Int?
    let presentUI: Bool?
    let queueRevision: UInt64?
    let items: [QueueItem]?
    let startIndex: Int?

    enum CodingKeys: String, CodingKey {
        case version = "v"
        case id
        case type
        case sessionID = "session_id"
        case protocolVersion = "protocol_version"
        case presentUI = "present_ui"
        case queueRevision = "queue_revision"
        case items
        case startIndex = "start_index"
    }
}

struct NowPlayingPayload: Codable, Equatable {
    let songID: String
    let title: String
    let artist: String
    let album: String?
    let durationSecs: Double?

    enum CodingKeys: String, CodingKey {
        case songID = "song_id"
        case title
        case artist
        case album
        case durationSecs = "duration_secs"
    }

    init(song: Song) {
        songID = song.id.rawValue
        title = song.title
        artist = song.artistName
        album = song.albumTitle
        durationSecs = song.duration
    }
}

struct HelperEvent: Encodable, Equatable {
    let version: Int
    let type: String
    var id: String?
    var commandID: String?
    var sessionID: String?
    var token: String?
    var pid: Int32?
    var bundleID: String?
    var helperVersion: String?
    var musicKitEntitled: Bool?
    var capabilities: [String]?
    var protocolVersion: Int?
    var authorization: String?
    var canPlayCatalogContent: Bool?
    var playbackState: String?
    var playbackTimeSecs: Double?
    var queueRevision: UInt64?
    var nowPlaying: NowPlayingPayload?
    var code: String?
    var message: String?
    var retryable: Bool?

    enum CodingKeys: String, CodingKey {
        case version = "v"
        case type
        case id
        case commandID = "command_id"
        case sessionID = "session_id"
        case token
        case pid
        case bundleID = "bundle_id"
        case helperVersion = "helper_version"
        case musicKitEntitled = "musickit_entitled"
        case capabilities
        case protocolVersion = "protocol_version"
        case authorization
        case canPlayCatalogContent = "can_play_catalog_content"
        case playbackState = "playback_state"
        case playbackTimeSecs = "playback_time_secs"
        case queueRevision = "queue_revision"
        case nowPlaying = "now_playing"
        case code
        case message
        case retryable
    }

    init(type: String) {
        version = helperProtocolVersion
        self.type = type
    }
}

enum AuthorizationLabel {
    static func string(for status: MusicAuthorization.Status) -> String {
        switch status {
        case .notDetermined:
            return "not_determined"
        case .denied:
            return "denied"
        case .restricted:
            return "restricted"
        case .authorized:
            return "authorized"
        @unknown default:
            return "unknown"
        }
    }
}

enum PlaybackLabel {
    static func string(for status: MusicPlayer.PlaybackStatus) -> String {
        switch status {
        case .stopped:
            return "stopped"
        case .playing:
            return "playing"
        case .paused:
            return "paused"
        case .interrupted:
            return "interrupted"
        case .seekingForward:
            return "seeking_forward"
        case .seekingBackward:
            return "seeking_backward"
        @unknown default:
            return "unknown"
        }
    }
}
