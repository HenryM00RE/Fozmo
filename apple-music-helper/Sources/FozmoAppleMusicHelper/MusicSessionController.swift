import AppKit
import Foundation
import MusicKit

/// MusicKit authorization, catalog, and queue-playlist bridge.
///
/// Audio playback deliberately belongs to Music.app and Fozmo Capture. This
/// helper never creates a MusicKit player or renderer queue: an
/// `ApplicationMusicPlayer` was measured rendering AAC rather than Apple
/// Lossless, which breaks Fozmo's bit-perfect guarantee.
///
/// What it does own is the Fozmo queue playlist. Music.app's AppleScript
/// `current track` cannot see catalog tracks that are not in the library, so
/// Fozmo promotes each queued catalog song to a library item inside one
/// dedicated playlist. That makes `database ID` a usable identity and lets
/// Music.app advance the queue internally, which is gapless.
@MainActor
final class MusicSessionController {
    private let sessionID: String
    private let sendEvent: (HelperEvent) -> Void
    private let musicKitProvisioned =
        (Bundle.main.object(forInfoDictionaryKey: "FozmoMusicKitProvisioned") as? Bool) == true
    private let authorizationWindow = AuthorizationWindowController()
    private var accepted = false
    private var subscriptionCanPlay: Bool?
    private var commandLedger = HelperCommandLedger()

    init(sessionID: String, sendEvent: @escaping (HelperEvent) -> Void) {
        self.sessionID = sessionID
        self.sendEvent = sendEvent
    }

    func sendHello(token: String) {
        var event = HelperEvent(type: "hello")
        event.sessionID = sessionID
        event.token = token
        event.pid = ProcessInfo.processInfo.processIdentifier
        event.bundleID = Bundle.main.bundleIdentifier ?? helperBundleIdentifier
        event.helperVersion =
            Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
            ?? "0.2.0"
        event.musicKitEntitled = musicKitProvisioned
        event.capabilities = [
            "authorize",
            "lookup_song",
            "lookup_album",
            "lookup_albums",
            "lookup_albums_by_upc",
            "search_albums",
            "search_songs",
            "library_status",
            "stage_or_adopt_queue",
            "targeted_library_playlist",
            "owned_playlist_inventory",
            "ensure_queue_folder",
        ]
        sendEvent(event)
    }

    func handle(frame: Data) {
        let command: IncomingCommand
        do {
            command = try JSONDecoder().decode(IncomingCommand.self, from: frame)
        } catch {
            sendError(
                commandID: nil,
                code: "helper_protocol_mismatch",
                message: "The helper received an invalid command.",
                retryable: false
            )
            return
        }
        guard command.version == helperProtocolVersion, command.sessionID == sessionID else {
            sendError(
                commandID: command.id,
                code: "helper_protocol_mismatch",
                message: "The helper command used the wrong session or protocol.",
                retryable: false
            )
            return
        }
        switch commandLedger.begin(commandID: command.id) {
        case .cached(let cached):
            sendEvent(cached)
            return
        case .inFlight:
            return
        case .start:
            break
        }

        if command.type == "accept" {
            guard command.protocolVersion == helperProtocolVersion else {
                sendError(
                    commandID: command.id,
                    code: "helper_protocol_mismatch",
                    message: "Fozmo and the helper use different protocol versions.",
                    retryable: false
                )
                return
            }
            accepted = true
            sendStatus(type: "ready", commandID: command.id)
            return
        }
        guard accepted else {
            sendError(
                commandID: command.id,
                code: "helper_protocol_mismatch",
                message: "Fozmo has not accepted the helper session.",
                retryable: false
            )
            return
        }

        switch command.type {
        case "authorize":
            authorize(commandID: command.id, presentUI: command.presentUI ?? true)
        case "get_status":
            refreshSubscriptionAndSendStatus(commandID: command.id)
        case "lookup_song":
            lookupSong(command)
        case "lookup_album":
            lookupAlbum(command)
        case "lookup_albums":
            lookupAlbums(command)
        case "lookup_albums_by_upc":
            lookupAlbumsByUPC(command)
        case "search_albums":
            searchAlbums(command)
        case "search_songs":
            searchSongs(command)
        case "library_status":
            reportLibraryStatus(command)
        case "stage_or_adopt_queue":
            stageOrAdoptQueue(command)
        case "get_library_playlist":
            getLibraryPlaylist(command)
        case "queue_playlist_inventory":
            queuePlaylistInventory(command)
        case "ensure_queue_folder":
            ensureQueueFolder(command)
        case "shutdown":
            var event = HelperEvent(type: "will_exit")
            event.commandID = command.id
            event.sessionID = sessionID
            sendAndCache(event, commandID: command.id)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
                NSApp.terminate(nil)
            }
        default:
            sendError(
                commandID: command.id,
                code: "apple_music_unavailable",
                message: "That helper command is unavailable.",
                retryable: false
            )
        }
    }

    func connectionClosed() {
        NSApp.terminate(nil)
    }

    private func authorize(commandID: String, presentUI: Bool) {
        guard musicKitProvisioned else {
            sendError(
                commandID: commandID,
                code: "musickit_capability_unavailable",
                message:
                    "This helper is not signed with a development profile for the MusicKit-enabled App ID.",
                retryable: false
            )
            return
        }
        if presentUI {
            authorizationWindow.show()
        }
        Task { @MainActor in
            let authorization = await MusicAuthorization.request()
            if authorization == .authorized {
                subscriptionCanPlay = try? await MusicSubscription.current.canPlayCatalogContent
            } else {
                subscriptionCanPlay = false
            }
            authorizationWindow.hide()
            var event = statusEvent(type: "authorization_changed", commandID: commandID)
            event.authorization = AuthorizationLabel.string(for: authorization)
            sendAndCache(event, commandID: commandID)
        }
    }

    private func refreshSubscriptionAndSendStatus(commandID: String) {
        Task { @MainActor in
            if MusicAuthorization.currentStatus == .authorized {
                subscriptionCanPlay = try? await MusicSubscription.current.canPlayCatalogContent
            } else {
                subscriptionCanPlay = false
            }
            sendStatus(type: "ready", commandID: commandID)
        }
    }

    private func lookupSong(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard let songID = CatalogInput.normalizedID(command.songID) else {
            sendError(
                commandID: command.id,
                code: "song_not_found",
                message: "Enter a valid Apple Music song ID.",
                retryable: false
            )
            return
        }
        let storefront = CatalogInput.normalizedStorefront(command.storefront)
        Task { @MainActor in
            do {
                var request = MusicCatalogResourceRequest<Song>(
                    matching: \.id,
                    equalTo: MusicItemID(songID)
                )
                request.limit = 1
                request.properties = [.albums, .audioVariants]
                let response = try await request.response()
                guard let song = response.items.first else {
                    throw HelperMusicError.songNotFound
                }
                var event = statusEvent(type: "catalog_song", commandID: command.id)
                event.catalogSong = CatalogSongPayload(song: song, storefront: storefront)
                sendAndCache(event, commandID: command.id)
            } catch HelperMusicError.songNotFound {
                sendError(
                    commandID: command.id,
                    code: "song_not_found",
                    message: "Apple Music could not find that song ID.",
                    retryable: false
                )
            } catch {
                sendError(
                    commandID: command.id,
                    code: "catalog_lookup_failed",
                    message: "Apple Music could not load that song.",
                    retryable: true
                )
            }
        }
    }

    private func lookupAlbum(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard let albumID = CatalogInput.normalizedID(command.albumID) else {
            sendError(
                commandID: command.id,
                code: "album_not_found",
                message: "Enter a valid Apple Music album ID.",
                retryable: false
            )
            return
        }
        let storefront = CatalogInput.normalizedStorefront(command.storefront)
        Task { @MainActor in
            do {
                var request = MusicCatalogResourceRequest<Album>(
                    matching: \.id,
                    equalTo: MusicItemID(albumID)
                )
                request.limit = 1
                request.properties = [.tracks, .audioVariants]
                let response = try await request.response()
                guard let album = response.items.first else {
                    throw HelperMusicError.albumNotFound
                }
                var event = statusEvent(type: "catalog_album", commandID: command.id)
                event.catalogAlbum = CatalogAlbumPayload(album: album, storefront: storefront)
                sendAndCache(event, commandID: command.id)
            } catch HelperMusicError.albumNotFound {
                sendError(
                    commandID: command.id,
                    code: "album_not_found",
                    message: "Apple Music could not find that album ID.",
                    retryable: false
                )
            } catch {
                sendError(
                    commandID: command.id,
                    code: "catalog_lookup_failed",
                    message: "Apple Music could not load that album.",
                    retryable: true
                )
            }
        }
    }

    private func lookupAlbums(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard let albumIDs = CatalogInput.normalizedAlbumIDs(command.albumIDs) else {
            sendError(
                commandID: command.id,
                code: "album_ids_invalid",
                message: "Enter between 1 and 6 valid Apple Music album IDs.",
                retryable: false
            )
            return
        }
        let storefront = CatalogInput.normalizedStorefront(command.storefront)
        Task { @MainActor in
            do {
                var request = MusicCatalogResourceRequest<Album>(
                    matching: \.id,
                    memberOf: albumIDs.map { MusicItemID($0) }
                )
                request.limit = albumIDs.count
                request.properties = [.tracks, .audioVariants]
                let response = try await request.response()
                let albumsByID = Dictionary(
                    response.items.map { ($0.id.rawValue, $0) },
                    uniquingKeysWith: { first, _ in first }
                )
                var event = statusEvent(type: "catalog_albums", commandID: command.id)
                event.catalogAlbums = albumIDs.compactMap { albumID in
                    albumsByID[albumID].map {
                        CatalogAlbumPayload(album: $0, storefront: storefront)
                    }
                }
                sendAndCache(event, commandID: command.id)
            } catch {
                sendError(
                    commandID: command.id,
                    code: "catalog_lookup_failed",
                    message: "Apple Music could not load those albums.",
                    retryable: true
                )
            }
        }
    }

    private func lookupAlbumsByUPC(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard let upcVariants = CatalogInput.normalizedUPCVariants(command.upc) else {
            sendError(
                commandID: command.id,
                code: "album_upc_invalid",
                message: "Enter a valid UPC, EAN, or GTIN.",
                retryable: false
            )
            return
        }
        let storefront = CatalogInput.normalizedStorefront(command.storefront)
        Task { @MainActor in
            do {
                var resolved: [Album] = []
                for upc in upcVariants {
                    resolved = try await catalogAlbums(matchingUPC: upc)
                    if !resolved.isEmpty { break }
                }
                let albums = resolved
                    .prefix(CatalogInput.maximumAlbumLookupCount)
                    .map { CatalogAlbumPayload(album: $0, storefront: storefront) }
                var event = statusEvent(type: "catalog_albums", commandID: command.id)
                event.catalogAlbums = albums
                sendAndCache(event, commandID: command.id)
            } catch {
                sendError(
                    commandID: command.id,
                    code: "catalog_lookup_failed",
                    message: "Apple Music could not look up that UPC.",
                    retryable: true
                )
            }
        }
    }

    private func catalogAlbums(matchingUPC upc: String) async throws -> [Album] {
        var request = MusicCatalogResourceRequest<Album>(
            matching: \.upc,
            equalTo: upc
        )
        request.limit = CatalogInput.maximumAlbumLookupCount
        request.properties = [.tracks, .audioVariants]
        return Array(try await request.response().items)
    }

    private func searchAlbums(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard
            let term = CatalogInput.normalizedSearchTerm(command.term),
            let limit = CatalogInput.normalizedSearchLimit(command.limit)
        else {
            sendError(
                commandID: command.id,
                code: "catalog_search_term_invalid",
                message: "Enter a valid Apple Music search term and result limit.",
                retryable: false
            )
            return
        }
        let storefront = CatalogInput.normalizedStorefront(command.storefront)
        Task { @MainActor in
            do {
                var request = MusicCatalogSearchRequest(term: term, types: [Album.self])
                request.limit = limit
                let response = try await request.response()
                var event = statusEvent(type: "catalog_search", commandID: command.id)
                event.catalogSearch = CatalogSearchPayload(
                    term: term,
                    storefront: storefront,
                    songs: [],
                    albums: response.albums.map {
                        CatalogAlbumPayload(album: $0, storefront: storefront)
                    }
                )
                sendAndCache(event, commandID: command.id)
            } catch {
                sendError(
                    commandID: command.id,
                    code: "catalog_search_failed",
                    message: "Apple Music could not search the album catalog.",
                    retryable: true
                )
            }
        }
    }

    private func searchSongs(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard
            let term = CatalogInput.normalizedSearchTerm(command.term),
            let limit = CatalogInput.normalizedSearchLimit(command.limit)
        else {
            sendError(
                commandID: command.id,
                code: "catalog_search_term_invalid",
                message: "Enter a valid Apple Music search term and result limit.",
                retryable: false
            )
            return
        }
        let storefront = CatalogInput.normalizedStorefront(command.storefront)
        Task { @MainActor in
            do {
                var request = MusicCatalogSearchRequest(term: term, types: [Album.self, Song.self])
                request.limit = limit
                let response = try await request.response()
                var event = statusEvent(type: "catalog_search", commandID: command.id)
                event.catalogSearch = CatalogSearchPayload(
                    term: term,
                    storefront: storefront,
                    songs: response.songs.map {
                        CatalogSongPayload(song: $0, storefront: storefront)
                    },
                    albums: response.albums.map {
                        CatalogAlbumPayload(album: $0, storefront: storefront)
                    }
                )
                sendAndCache(event, commandID: command.id)
            } catch {
                sendError(
                    commandID: command.id,
                    code: "catalog_search_failed",
                    message: "Apple Music could not search the catalog.",
                    retryable: true
                )
            }
        }
    }

    private func reportLibraryStatus(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        Task { @MainActor in
            var canWrite = true
            var blockedReason: String?
            do {
                // Listing playlists is the cheapest call that still exercises
                // the music-user token the writes depend on.
                _ = try await AppleMusicWebAPI.playlists()
            } catch {
                canWrite = false
                blockedReason = Self.libraryWriteFailureReason(error)
            }
            var event = statusEvent(type: "library_status", commandID: command.id)
            event.libraryStatus = LibraryStatusPayload(
                canPlayCatalogContent: subscriptionCanPlay == true,
                canWriteLibrary: canWrite,
                playlistName: "Fozmo A/B",
                blockedReason: blockedReason
            )
            sendAndCache(event, commandID: command.id)
        }
    }

    private func getLibraryPlaylist(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard let playlistID = CatalogInput.normalizedID(command.webPlaylistID) else {
            sendError(
                commandID: command.id,
                code: "queue_playlist_id_invalid",
                message: "A Web playlist ID is required.",
                retryable: false
            )
            return
        }
        Task { @MainActor in
            do {
                let playlist = try await AppleMusicWebAPI.playlist(playlistID: playlistID)
                var event = statusEvent(type: "library_playlist", commandID: command.id)
                if let playlist {
                    event.libraryPlaylist = LibraryPlaylistPayload(
                        id: playlist.id,
                        name: playlist.name,
                        description: playlist.description,
                        canEdit: playlist.canEdit
                    )
                }
                sendAndCache(event, commandID: command.id)
            } catch {
                sendError(
                    commandID: command.id,
                    code: "queue_playlist_lookup_failed",
                    message: Self.libraryWriteFailureReason(error),
                    retryable: true
                )
            }
        }
    }

    private func queuePlaylistInventory(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        Task { @MainActor in
            do {
                let candidates = try await AppleMusicWebAPI.playlists().filter { playlist in
                    playlist.name == "Fozmo"
                        || playlist.name == "Fozmo A"
                        || playlist.name == "Fozmo B"
                        || playlist.description?.hasPrefix("fozmo.queue.v") == true
                }
                var event = statusEvent(
                    type: "queue_playlist_inventory",
                    commandID: command.id
                )
                event.playlistInventory = candidates.map { playlist in
                    LibraryPlaylistPayload(
                        id: playlist.id,
                        name: playlist.name,
                        description: playlist.description,
                        canEdit: playlist.canEdit
                    )
                }
                sendAndCache(event, commandID: command.id)
            } catch {
                sendError(
                    commandID: command.id,
                    code: "queue_playlist_inventory_failed",
                    message: Self.libraryWriteFailureReason(error),
                    retryable: true
                )
            }
        }
    }

    private func ensureQueueFolder(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard let owner = CatalogInput.normalizedID(command.installationOwner) else {
            sendError(
                commandID: command.id,
                code: "queue_folder_owner_invalid",
                message: "The installation owner is required for the Fozmo folder.",
                retryable: false
            )
            return
        }
        let description = "fozmo.queue.folder.v1;owner=\(owner)"
        Task { @MainActor in
            do {
                let matches = try await AppleMusicWebAPI.playlistFolders().filter {
                    $0.name == "Fozmo" && $0.description == description
                }
                guard matches.count <= 1 else {
                    sendError(
                        commandID: command.id,
                        code: "queue_folder_ambiguous",
                        message: "Apple Music exposed duplicate owned Fozmo folders.",
                        retryable: false
                    )
                    return
                }
                let folderID: String
                if let existing = matches.first {
                    folderID = existing.id
                } else {
                    folderID = try await AppleMusicWebAPI.createPlaylistFolder(
                        name: "Fozmo",
                        description: description
                    )
                }
                var event = statusEvent(type: "queue_folder", commandID: command.id)
                event.libraryPlaylist = LibraryPlaylistPayload(
                    id: folderID,
                    name: "Fozmo",
                    description: description,
                    canEdit: true
                )
                sendAndCache(event, commandID: command.id)
            } catch {
                sendError(
                    commandID: command.id,
                    code: "queue_folder_failed",
                    message: Self.libraryWriteFailureReason(error),
                    retryable: true
                )
            }
        }
    }

    /// Materialize one immutable queue generation, or adopt the exact
    /// generation after an ambiguous response or helper restart.
    ///
    /// `allowCreate` is true only on the generation's first durable attempt.
    /// Once Fozmo has recorded that the POST may have been sent, every retry is
    /// a read-only search by the exact description below.
    private func stageOrAdoptQueue(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard
            let songIDs = CatalogInput.normalizedQueueSongIDs(command.songIDs),
            let operationID = CatalogInput.normalizedID(command.operationID),
            operationID == command.id,
            let generation = CatalogInput.normalizedID(command.generation),
            let rawSlot = CatalogInput.normalizedID(command.slot)?.lowercased(),
            rawSlot == "a" || rawSlot == "b",
            let fingerprint = CatalogInput.normalizedID(command.fingerprint),
            let installationOwner = CatalogInput.normalizedID(command.installationOwner),
            let allowCreate = command.allowCreate
        else {
            sendError(
                commandID: command.id,
                code: "queue_stage_invalid",
                message: "The immutable Apple Music queue request is incomplete.",
                retryable: false
            )
            return
        }
        let slot = rawSlot.uppercased()
        let slotName = "Fozmo \(slot)"
        let description =
            "fozmo.queue.v5;owner=\(installationOwner);slot=\(slot);generation=\(generation);fingerprint=\(fingerprint)"

        Task { @MainActor in
            do {
                let taskStarted = ContinuousClock.now
                var phaseTimingsMS: [String: Int] = [:]
                let songs = try await catalogSongs(for: songIDs)
                phaseTimingsMS["catalog_resolution"] =
                    Self.elapsedMilliseconds(since: taskStarted)
                guard !songs.isEmpty else { throw HelperMusicError.songNotFound }
                let resolved = Set(songs.map(\.id.rawValue))
                let acceptedEntries = songs.map { song in
                    QueueEntryPayload(
                        songID: song.id.rawValue,
                        title: song.title,
                        artist: song.artistName,
                        durationSecs: song.duration,
                        catalogSongID: song.id.rawValue,
                        albumTitle: song.albumTitle,
                        discNumber: song.discNumber,
                        trackNumber: song.trackNumber,
                        storefront: nil
                    )
                }
                let rejected = songIDs.filter { !resolved.contains($0) }
                let deadline = ContinuousClock.now.advanced(by: .seconds(60))
                var playlistID = CatalogInput.normalizedID(command.knownWebPlaylistID)
                var lastError: Error?
                let initialAction = QueueStagePlan.initialAction(
                    allowCreate: allowCreate,
                    knownWebPlaylistID: playlistID
                )
                var mustEnumerate = initialAction == .enumerateForAdoption

                if initialAction == .targetedLookup, let knownID = playlistID {
                    do {
                        let known = try await AppleMusicWebAPI.playlist(playlistID: knownID)
                        if known?.name != slotName || known?.description != description {
                            playlistID = nil
                            mustEnumerate = true
                        }
                    } catch {
                        lastError = error
                        playlistID = nil
                        mustEnumerate = true
                    }
                }

                if playlistID == nil, initialAction == .create {
                    // Fresh generations create first. Account-wide enumeration
                    // is reserved for the ambiguous-response recovery path.
                    do {
                        playlistID = try await AppleMusicWebAPI.createPlaylist(
                            name: slotName,
                            description: description,
                            catalogSongIDs: songs.map(\.id.rawValue),
                            parentFolderID: CatalogInput.normalizedID(command.parentFolderID)
                        )
                        phaseTimingsMS["playlist_create"] =
                            Self.elapsedMilliseconds(since: taskStarted)
                        var created = statusEvent(
                            type: "queue_generation_created",
                            commandID: command.id
                        )
                        created.stageOrAdopt = StageOrAdoptPayload(
                            slotName: slotName,
                            generation: generation,
                            operationID: operationID,
                            fingerprint: fingerprint,
                            requestedCount: songIDs.count,
                            acceptedEntries: acceptedEntries,
                            rejectedSongIDs: rejected,
                            webPlaylistID: playlistID,
                            serverCatalogIDs: [],
                            phaseTimingsMS: phaseTimingsMS
                        )
                        sendEvent(created)
                    } catch {
                        lastError = error
                        mustEnumerate = true
                    }
                }

                while ContinuousClock.now < deadline {
                    if playlistID == nil, mustEnumerate {
                        do {
                            let playlists = try await AppleMusicWebAPI.playlists()
                            let matches = playlists.filter {
                                $0.name == slotName && $0.description == description
                            }
                            if matches.count > 1 {
                                sendError(
                                    commandID: command.id,
                                    code: "queue_generation_ambiguous",
                                    message:
                                        "Apple Music exposed more than one playlist for the same Fozmo queue generation.",
                                    retryable: false
                                )
                                return
                            }
                            if let adopted = matches.first {
                                playlistID = adopted.id
                                phaseTimingsMS["playlist_adoption"] =
                                    Self.elapsedMilliseconds(since: taskStarted)
                            }
                        } catch {
                            lastError = error
                        }
                    }

                    if let playlistID {
                        do {
                            let tracks = try await AppleMusicWebAPI.playlistTracks(
                                playlistID: playlistID
                            )
                            if tracks.count == songs.count {
                                phaseTimingsMS["server_verification"] =
                                    Self.elapsedMilliseconds(since: taskStarted)
                                var event = statusEvent(
                                    type: "queue_generation_staged",
                                    commandID: command.id
                                )
                                event.stageOrAdopt = StageOrAdoptPayload(
                                    slotName: slotName,
                                    generation: generation,
                                    operationID: operationID,
                                    fingerprint: fingerprint,
                                    requestedCount: songIDs.count,
                                    acceptedEntries: acceptedEntries,
                                    rejectedSongIDs: rejected,
                                    webPlaylistID: playlistID,
                                    serverCatalogIDs: tracks.map(\.catalogID),
                                    phaseTimingsMS: phaseTimingsMS
                                )
                                sendAndCache(event, commandID: command.id)
                                return
                            }
                        } catch {
                            // The playlist itself may be visible before its
                            // ordered track relationship. Keep adopting.
                            lastError = error
                        }
                    }
                    try await Task.sleep(for: .milliseconds(500))
                }

                let detail = lastError.map(Self.libraryWriteFailureReason) ?? ""
                sendError(
                    commandID: command.id,
                    code: "queue_generation_not_visible",
                    message:
                        "Apple Music did not materialize Fozmo's immutable queue generation in time."
                        + (detail.isEmpty ? "" : " \(detail)"),
                    retryable: true
                )
            } catch HelperMusicError.songNotFound {
                sendError(
                    commandID: command.id,
                    code: "song_not_found",
                    message: "Apple Music could not find the queue's first song.",
                    retryable: false
                )
            } catch {
                sendError(
                    commandID: command.id,
                    code: "queue_stage_failed",
                    message: Self.libraryWriteFailureReason(error),
                    retryable: true
                )
            }
        }
    }

    private static func elapsedMilliseconds(
        since start: ContinuousClock.Instant
    ) -> Int {
        let components = start.duration(to: ContinuousClock.now).components
        let millisecondsFromSeconds = components.seconds * 1_000
        let millisecondsFromAttoseconds = components.attoseconds / 1_000_000_000_000_000
        return Int(millisecondsFromSeconds + millisecondsFromAttoseconds)
    }

    /// Catalog songs for `songIDs`, in the requested order.
    ///
    /// A batch resource request does not promise response order, and playlist
    /// order is playback order, so reorder against the request.
    private func catalogSongs(for songIDs: [String]) async throws -> [Song] {
        var request = MusicCatalogResourceRequest<Song>(
            matching: \.id,
            memberOf: songIDs.map { MusicItemID($0) }
        )
        request.limit = songIDs.count
        let response = try await request.response()
        var byID: [String: Song] = [:]
        for song in response.items where byID[song.id.rawValue] == nil {
            byID[song.id.rawValue] = song
        }
        return songIDs.compactMap { byID[$0] }
    }

    /// A library write can fail because Sync Library is off, because the
    /// music-user token is missing, or transiently. Only the first is
    /// actionable and MusicKit does not expose it, so name the setting for the
    /// permission-shaped failures.
    ///
    /// Everything else carries Apple's own status and body. Blaming Sync
    /// Library for every failure sent users to a setting that was already on
    /// while the real cause — a rejected request body, a 500, an unreachable
    /// endpoint — stayed invisible: the helper is launched through `open`, so
    /// its stderr reaches no log Fozmo can read, and this string is the only
    /// channel the failure has.
    private static func libraryWriteFailureReason(_ error: Error) -> String {
        switch error {
        case AppleMusicWebAPI.Failure.http(let status, let body)
        where status == 401 || status == 403:
            return
                "Apple Music refused Fozmo's library update (HTTP \(status)). Adding subscription tracks to a library needs Sync Library: turn on Music → Settings → General → Sync Library, re-authorize Apple Music in Fozmo, then retry.\(Self.appleDetail(body))"
        case AppleMusicWebAPI.Failure.http(let status, let body):
            return
                "Apple Music rejected Fozmo's queue playlist request with HTTP \(status).\(Self.appleDetail(body))"
        case AppleMusicWebAPI.Failure.malformedResponse:
            return
                "Apple Music returned a queue playlist response Fozmo could not read."
        default:
            return
                "Fozmo could not reach Apple Music to update its queue playlist: \(error.localizedDescription)"
        }
    }

    /// Apple's error body, trimmed to something a message can carry.
    ///
    /// The bodies are JSON with a `detail` worth reading; the full document is
    /// far too long for a UI string, and truncating is better than dropping it.
    private static func appleDetail(_ body: String) -> String {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "" }
        let clipped =
            trimmed.count > 400
            ? String(trimmed.prefix(400)) + "…"
            : trimmed
        return " Apple reported: \(clipped)"
    }

    private func validateCatalogAccess(commandID: String) -> Bool {
        guard musicKitProvisioned else {
            sendError(
                commandID: commandID,
                code: "musickit_capability_unavailable",
                message:
                    "This helper is not signed with a development profile for the MusicKit-enabled App ID.",
                retryable: false
            )
            return false
        }
        guard MusicAuthorization.currentStatus == .authorized else {
            sendError(
                commandID: commandID,
                code: "music_authorization_not_determined",
                message: "Authorize Apple Music before using the catalog.",
                retryable: false
            )
            return false
        }
        guard subscriptionCanPlay == true else {
            sendError(
                commandID: commandID,
                code: "subscription_required",
                message: "This Apple Music account cannot use catalog content.",
                retryable: false
            )
            return false
        }
        return true
    }

    private func sendStatus(type: String, commandID: String?) {
        sendAndCache(statusEvent(type: type, commandID: commandID), commandID: commandID)
    }

    private func statusEvent(type: String, commandID: String?) -> HelperEvent {
        var event = HelperEvent(type: type)
        event.commandID = commandID
        event.sessionID = sessionID
        event.authorization = AuthorizationLabel.string(for: MusicAuthorization.currentStatus)
        event.canPlayCatalogContent = subscriptionCanPlay
        return event
    }

    private func sendError(
        commandID: String?,
        code: String,
        message: String,
        retryable: Bool
    ) {
        var event = HelperEvent(type: "helper_error")
        event.commandID = commandID
        event.sessionID = sessionID
        event.code = code
        event.message = message
        event.retryable = retryable
        sendAndCache(event, commandID: commandID)
    }

    private func sendAndCache(_ event: HelperEvent, commandID: String?) {
        if let commandID {
            commandLedger.complete(event, commandID: commandID)
        }
        sendEvent(event)
    }
}

private enum HelperMusicError: Error {
    case songNotFound
    case albumNotFound
}
