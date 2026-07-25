import AppKit
import Foundation
import MusicKit

/// MusicKit authorization and catalog bridge.
///
/// Audio playback deliberately belongs to Music.app and Fozmo Capture. This
/// helper never creates a MusicKit player or renderer queue.
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
        event.capabilities = ["authorize", "lookup_song", "lookup_album", "search_songs"]
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
        case "search_songs":
            searchSongs(command)
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
