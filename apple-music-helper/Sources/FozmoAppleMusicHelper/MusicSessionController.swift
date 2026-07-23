import AppKit
import Foundation
import MusicKit

@MainActor
final class MusicSessionController {
    private let sessionID: String
    private let sendEvent: (HelperEvent) -> Void
    private let player = ApplicationMusicPlayer.shared
    private let musicKitEntitled =
        (Bundle.main.object(forInfoDictionaryKey: "FozmoMusicKitEntitled") as? Bool) == true
    private let authorizationWindow = AuthorizationWindowController()
    private var accepted = false
    private var queueRevision: UInt64 = 0
    private var subscriptionCanPlay: Bool?
    private var cachedResponses: [String: HelperEvent] = [:]
    private var statusTimer: Timer?
    private var lastPlaybackState = "stopped"
    private var lastNowPlayingID: String?

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
            ?? "0.1.0"
        event.musicKitEntitled = musicKitEntitled
        event.capabilities = [
            "authorize",
            "queue",
            "play",
            "pause",
            "resume",
            "stop",
            "playback_time",
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
        if let cached = cachedResponses[command.id] {
            sendEvent(cached)
            return
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
            startStatusTimer()
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
        case "set_queue":
            prepareQueue(command)
        case "play", "resume":
            play(commandID: command.id)
        case "pause":
            player.pause()
            sendPlaybackState(commandID: command.id)
        case "stop":
            player.stop()
            sendPlaybackState(commandID: command.id, clearNowPlaying: true)
        case "shutdown":
            player.stop()
            statusTimer?.invalidate()
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
        player.stop()
        NSApp.terminate(nil)
    }

    private func authorize(commandID: String, presentUI: Bool) {
        guard musicKitEntitled else {
            sendError(
                commandID: commandID,
                code: "musickit_capability_unavailable",
                message: "This helper build is not signed with the MusicKit capability.",
                retryable: false
            )
            return
        }
        if presentUI {
            authorizationWindow.show()
        }
        Task { @MainActor in
            let authorization: MusicAuthorization.Status
            if MusicAuthorization.currentStatus == .notDetermined {
                authorization = await MusicAuthorization.request()
            } else {
                authorization = MusicAuthorization.currentStatus
            }
            if authorization == .authorized {
                do {
                    subscriptionCanPlay = try await MusicSubscription.current.canPlayCatalogContent
                } catch {
                    subscriptionCanPlay = nil
                }
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

    private func prepareQueue(_ command: IncomingCommand) {
        guard musicKitEntitled else {
            sendError(
                commandID: command.id,
                code: "musickit_capability_unavailable",
                message: "This helper build is not signed with the MusicKit capability.",
                retryable: false
            )
            return
        }
        guard MusicAuthorization.currentStatus == .authorized else {
            sendError(
                commandID: command.id,
                code: "music_authorization_not_determined",
                message: "Authorize Apple Music before preparing a song.",
                retryable: false
            )
            return
        }
        guard subscriptionCanPlay == true else {
            sendError(
                commandID: command.id,
                code: "subscription_required",
                message: "This Apple Music account cannot play catalog content.",
                retryable: false
            )
            return
        }
        guard
            let revision = command.queueRevision,
            revision > queueRevision,
            let items = command.items,
            !items.isEmpty,
            items.count <= 100
        else {
            sendError(
                commandID: command.id,
                code: "queue_prepare_failed",
                message: "The Apple Music queue request is invalid or stale.",
                retryable: false
            )
            return
        }
        let startIndex = command.startIndex ?? 0
        guard items.indices.contains(startIndex) else {
            sendError(
                commandID: command.id,
                code: "queue_prepare_failed",
                message: "The Apple Music queue start position is invalid.",
                retryable: false
            )
            return
        }
        Task { @MainActor in
            do {
                var songs: [Song] = []
                songs.reserveCapacity(items.count)
                for item in items {
                    var request = MusicCatalogResourceRequest<Song>(
                        matching: \.id,
                        equalTo: MusicItemID(item.songID)
                    )
                    request.limit = 1
                    let response = try await request.response()
                    guard let song = response.items.first else {
                        throw HelperMusicError.songNotFound
                    }
                    songs.append(song)
                }
                player.queue = ApplicationMusicPlayer.Queue(
                    for: songs,
                    startingAt: songs[startIndex]
                )
                try await player.prepareToPlay()
                queueRevision = revision
                var event = statusEvent(type: "queue_prepared", commandID: command.id)
                event.queueRevision = revision
                event.nowPlaying = NowPlayingPayload(song: songs[startIndex])
                lastNowPlayingID = songs[startIndex].id.rawValue
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
                    code: "queue_prepare_failed",
                    message: "Apple Music could not prepare that song for playback.",
                    retryable: true
                )
            }
        }
    }

    private func play(commandID: String) {
        Task { @MainActor in
            do {
                try await player.play()
                sendPlaybackState(commandID: commandID)
            } catch {
                sendError(
                    commandID: commandID,
                    code: "song_not_playable",
                    message: "Apple Music could not start this song.",
                    retryable: true
                )
            }
        }
    }

    private func sendStatus(type: String, commandID: String?) {
        let event = statusEvent(type: type, commandID: commandID)
        sendAndCache(event, commandID: commandID)
    }

    private func statusEvent(type: String, commandID: String?) -> HelperEvent {
        var event = HelperEvent(type: type)
        event.commandID = commandID
        event.sessionID = sessionID
        event.authorization = AuthorizationLabel.string(for: MusicAuthorization.currentStatus)
        event.canPlayCatalogContent = subscriptionCanPlay
        event.playbackState = PlaybackLabel.string(for: player.state.playbackStatus)
        event.playbackTimeSecs = player.playbackTime
        event.queueRevision = queueRevision
        event.nowPlaying = currentNowPlaying()
        return event
    }

    private func sendPlaybackState(commandID: String?, clearNowPlaying: Bool = false) {
        var event = statusEvent(type: "playback_state_changed", commandID: commandID)
        if clearNowPlaying {
            event.nowPlaying = nil
            lastNowPlayingID = nil
        }
        lastPlaybackState = event.playbackState ?? "stopped"
        sendAndCache(event, commandID: commandID)
    }

    private func startStatusTimer() {
        statusTimer?.invalidate()
        statusTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) {
            [weak self] _ in
            MainActor.assumeIsolated {
                self?.publishObservedState()
            }
        }
    }

    private func publishObservedState() {
        let playbackState = PlaybackLabel.string(for: player.state.playbackStatus)
        if playbackState != lastPlaybackState {
            lastPlaybackState = playbackState
            sendPlaybackState(commandID: nil)
        }

        let nowPlaying = currentNowPlaying()
        if nowPlaying?.songID != lastNowPlayingID {
            lastNowPlayingID = nowPlaying?.songID
            var event = statusEvent(type: "now_playing_changed", commandID: nil)
            event.nowPlaying = nowPlaying
            sendEvent(event)
        }

        var timeEvent = HelperEvent(type: "playback_time")
        timeEvent.sessionID = sessionID
        timeEvent.playbackTimeSecs = player.playbackTime
        timeEvent.playbackState = playbackState
        sendEvent(timeEvent)
    }

    private func currentNowPlaying() -> NowPlayingPayload? {
        guard let item = player.queue.currentEntry?.item else { return nil }
        switch item {
        case .song(let song):
            return NowPlayingPayload(song: song)
        case .musicVideo:
            return nil
        @unknown default:
            return nil
        }
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
            cachedResponses[commandID] = event
            if cachedResponses.count > 128, let oldest = cachedResponses.keys.sorted().first {
                cachedResponses.removeValue(forKey: oldest)
            }
        }
        sendEvent(event)
    }
}

private enum HelperMusicError: Error {
    case songNotFound
}
