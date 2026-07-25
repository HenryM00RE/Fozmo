import Foundation
import MusicKit

let helperProtocolVersion = 2
let helperBundleIdentifier = "com.fozmo.apple-music-helper"

struct QueueItem: Codable, Equatable {
    let songID: String
    let storefront: String?
    let segmentIndex: Int

    enum CodingKeys: String, CodingKey {
        case songID = "song_id"
        case storefront
        case segmentIndex = "segment_index"
    }
}

struct ValidatedQueuePlan: Equatable {
    let revision: UInt64
    let items: [QueueItem]
    let startIndex: Int
}

enum QueuePlanValidator {
    static func validate(
        revision: UInt64?,
        currentRevision: UInt64,
        items: [QueueItem]?,
        startIndex: Int?
    ) -> ValidatedQueuePlan? {
        guard
            let revision,
            revision > currentRevision,
            let items,
            !items.isEmpty,
            items.count <= 100,
            items.allSatisfy({ !$0.songID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }),
            Set(items.map(\.segmentIndex)).count == items.count,
            zip(items, items.dropFirst()).allSatisfy({
                $0.0.segmentIndex < $0.1.segmentIndex
            })
        else {
            return nil
        }
        let startIndex = startIndex ?? 0
        guard items.indices.contains(startIndex) else { return nil }
        return ValidatedQueuePlan(revision: revision, items: items, startIndex: startIndex)
    }
}

enum QueueIndexTransition {
    static func shouldEmit(previous: Int?, next: Int?, validSegments: Set<Int>) -> Bool {
        guard let next, validSegments.contains(next) else { return false }
        guard let previous else { return true }
        return next > previous
    }
}

enum QueueFinishReason {
    static func forPlaybackTransition(previous: String, current: String) -> String? {
        guard current == "stopped", previous == "playing" || previous == "paused" else {
            return nil
        }
        return "completed"
    }
}

struct HelperResponseCache {
    private let limit: Int
    private var values: [String: HelperEvent] = [:]
    private var insertionOrder: [String] = []

    init(limit: Int = 128) {
        self.limit = max(1, limit)
    }

    var count: Int {
        values.count
    }

    func event(for commandID: String) -> HelperEvent? {
        values[commandID]
    }

    mutating func store(_ event: HelperEvent, for commandID: String) {
        if values[commandID] == nil {
            insertionOrder.append(commandID)
        }
        values[commandID] = event
        while insertionOrder.count > limit {
            let expired = insertionOrder.removeFirst()
            values.removeValue(forKey: expired)
        }
    }
}

enum HelperCommandDisposition: Equatable {
    case start
    case inFlight
    case cached(HelperEvent)
}

struct HelperCommandLedger {
    private var responses: HelperResponseCache
    private var inFlight: Set<String> = []

    init(limit: Int = 128) {
        responses = HelperResponseCache(limit: limit)
    }

    mutating func begin(commandID: String) -> HelperCommandDisposition {
        if let event = responses.event(for: commandID) {
            return .cached(event)
        }
        if inFlight.contains(commandID) {
            return .inFlight
        }
        inFlight.insert(commandID)
        return .start
    }

    mutating func complete(_ event: HelperEvent, commandID: String) {
        inFlight.remove(commandID)
        responses.store(event, for: commandID)
    }
}

enum CatalogInput {
    static func normalizedID(_ value: String?) -> String? {
        value?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .nonEmpty
    }

    static func normalizedStorefront(_ value: String?) -> String {
        value?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .nonEmpty ?? "current"
    }

    static func normalizedSearchTerm(_ value: String?) -> String? {
        guard let value = normalizedID(value), value.count <= 200 else { return nil }
        guard !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
        else { return nil }
        return value
    }

    static func normalizedSearchLimit(_ value: Int?) -> Int? {
        let value = value ?? 10
        return (1...25).contains(value) ? value : nil
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
    let songID: String?
    let albumID: String?
    let term: String?
    let limit: Int?
    let storefront: String?
    let positionSecs: Double?

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
        case songID = "song_id"
        case albumID = "album_id"
        case term
        case limit
        case storefront
        case positionSecs = "position_secs"
    }
}

struct CatalogSongPayload: Codable, Equatable {
    let songID: String
    let storefront: String
    let albumID: String?
    let title: String
    let artist: String
    let albumTitle: String?
    let albumArtist: String?
    let durationSecs: Double?
    let trackNumber: Int?
    let discNumber: Int?
    let isrc: String?
    let artworkURL: String?

    enum CodingKeys: String, CodingKey {
        case songID = "song_id"
        case storefront
        case albumID = "album_id"
        case title
        case artist
        case albumTitle = "album_title"
        case albumArtist = "album_artist"
        case durationSecs = "duration_secs"
        case trackNumber = "track_number"
        case discNumber = "disc_number"
        case isrc
        case artworkURL = "artwork_url"
    }

    init(
        songID: String,
        storefront: String,
        albumID: String?,
        title: String,
        artist: String,
        albumTitle: String?,
        albumArtist: String?,
        durationSecs: Double?,
        trackNumber: Int?,
        discNumber: Int?,
        isrc: String?,
        artworkURL: String?
    ) {
        self.songID = songID
        self.storefront = storefront
        self.albumID = albumID
        self.title = title
        self.artist = artist
        self.albumTitle = albumTitle
        self.albumArtist = albumArtist
        self.durationSecs = durationSecs
        self.trackNumber = trackNumber
        self.discNumber = discNumber
        self.isrc = isrc
        self.artworkURL = artworkURL
    }

    init(
        song: Song,
        storefront: String,
        albumID: String? = nil,
        albumArtist: String? = nil
    ) {
        self.init(
            songID: song.id.rawValue,
            storefront: storefront,
            albumID: albumID ?? song.albums?.first?.id.rawValue,
            title: song.title,
            artist: song.artistName,
            albumTitle: song.albumTitle,
            albumArtist: albumArtist ?? song.albums?.first?.artistName,
            durationSecs: song.duration,
            trackNumber: song.trackNumber,
            discNumber: song.discNumber,
            isrc: song.isrc,
            artworkURL: song.artwork?.url(width: 1200, height: 1200)?.absoluteString
        )
    }
}

struct CatalogAlbumPayload: Codable, Equatable {
    let albumID: String
    let storefront: String
    let title: String
    let artist: String
    let upc: String?
    let releaseDate: String?
    let artworkURL: String?
    let audioVariants: [String]
    let tracks: [CatalogSongPayload]

    enum CodingKeys: String, CodingKey {
        case albumID = "album_id"
        case storefront
        case title
        case artist
        case upc
        case releaseDate = "release_date"
        case artworkURL = "artwork_url"
        case audioVariants = "audio_variants"
        case tracks
    }

    init(album: Album, storefront: String) {
        albumID = album.id.rawValue
        self.storefront = storefront
        title = album.title
        artist = album.artistName
        upc = album.upc
        releaseDate = album.releaseDate.map {
            ISO8601DateFormatter().string(from: $0)
        }
        artworkURL = album.artwork?.url(width: 1200, height: 1200)?.absoluteString
        audioVariants = (album.audioVariants ?? []).map { String(describing: $0) }
        tracks = (album.tracks ?? []).compactMap { track in
            guard case .song(let song) = track else { return nil }
            return CatalogSongPayload(
                song: song,
                storefront: storefront,
                albumID: album.id.rawValue,
                albumArtist: album.artistName
            )
        }
    }
}

struct CatalogSearchPayload: Codable, Equatable {
    let term: String
    let storefront: String
    let songs: [CatalogSongPayload]
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
    var segmentIndex: Int?
    var songID: String?
    var storefront: String?
    var playbackPosition: Double?
    var finishReason: String?
    var nowPlaying: NowPlayingPayload?
    var catalogSong: CatalogSongPayload?
    var catalogAlbum: CatalogAlbumPayload?
    var catalogSearch: CatalogSearchPayload?
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
        case segmentIndex = "segment_index"
        case songID = "song_id"
        case storefront
        case playbackPosition = "playback_position"
        case finishReason = "finish_reason"
        case nowPlaying = "now_playing"
        case catalogSong = "catalog_song"
        case catalogAlbum = "catalog_album"
        case catalogSearch = "catalog_search"
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

private extension String {
    var nonEmpty: String? {
        isEmpty ? nil : self
    }
}
