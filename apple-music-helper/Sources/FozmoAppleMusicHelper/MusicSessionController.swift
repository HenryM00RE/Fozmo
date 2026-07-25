import AppKit
import Foundation
import MusicKit

@MainActor
final class MusicSessionController {
    private let sessionID: String
    private let sendEvent: (HelperEvent) -> Void
    private let player = ApplicationMusicPlayer.shared
    private let musicKitProvisioned =
        (Bundle.main.object(forInfoDictionaryKey: "FozmoMusicKitProvisioned") as? Bool) == true
    private let authorizationWindow = AuthorizationWindowController()
    private var accepted = false
    private var queueRevision: UInt64 = 0
    private var latestAcceptedQueueRevision: UInt64 = 0
    private var queueItems: [QueueItem] = []
    private var queueEntrySegments: [String: Int] = [:]
    private var currentSegmentIndex: Int?
    private var subscriptionCanPlay: Bool?
    private var commandLedger = HelperCommandLedger()
    private var statusTimer: Timer?
    private var lastPlaybackState = "stopped"
    private var lastQueueEntryID: String?
    private var queueFinishedEmitted = false
    private var catalogWatchdogTicks = 0
    private var catalogWatchdogInFlight = false
    private var terminalCatalogFailureEmitted = false
    private var queuePreparationTask: Task<Void, Never>?
    private var playbackCommandGeneration: UInt64 = 0
    private var explicitPauseActive = false
    private var completionWatchdog = QueueCompletionWatchdog()

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
        // Keep the protocol-v2 field name for compatibility. MusicKit is an
        // App Service associated with the App ID, not a code-signing entitlement.
        event.musicKitEntitled = musicKitProvisioned
        event.capabilities = [
            "authorize",
            "lookup_song",
            "lookup_album",
            "search_songs",
            "queue",
            "play",
            "pause",
            "resume",
            "seek",
            "skip_next",
            "stop",
            "playback_time",
            "revisioned_events",
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
        case "lookup_song":
            lookupSong(command)
        case "lookup_album":
            lookupAlbum(command)
        case "search_songs":
            searchSongs(command)
        case "set_queue":
            prepareQueue(command)
        case "play", "resume":
            play(commandID: command.id)
        case "pause":
            invalidatePendingPlaybackCommand()
            explicitPauseActive = true
            completionWatchdog.reset()
            player.pause()
            sendPlaybackState(commandID: command.id)
        case "seek":
            seek(command)
        case "skip_next":
            skipNext(commandID: command.id)
        case "stop":
            stop(commandID: command.id, reason: "user_stopped")
        case "shutdown":
            cancelPendingQueuePreparation()
            invalidatePendingPlaybackCommand()
            player.stop()
            sendQueueFinished(reason: "user_stopped", commandID: nil)
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
        cancelPendingQueuePreparation()
        invalidatePendingPlaybackCommand()
        player.stop()
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
            terminalCatalogFailureEmitted = false
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
                var request = MusicCatalogSearchRequest(
                    term: term,
                    types: [Album.self, Song.self]
                )
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

    private func prepareQueue(_ command: IncomingCommand) {
        guard validateCatalogAccess(commandID: command.id) else { return }
        guard
            let plan = QueuePlanValidator.validate(
                revision: command.queueRevision,
                currentRevision: max(queueRevision, latestAcceptedQueueRevision),
                items: command.items,
                startIndex: command.startIndex
            )
        else {
            sendError(
                commandID: command.id,
                code: "queue_prepare_failed",
                message: "The Apple Music queue request is invalid or stale.",
                retryable: false
            )
            return
        }
        let revision = plan.revision
        let items = plan.items
        let startIndex = plan.startIndex
        latestAcceptedQueueRevision = revision
        invalidatePendingPlaybackCommand()
        let previousPreparation = queuePreparationTask
        previousPreparation?.cancel()
        queuePreparationTask = Task { @MainActor in
            if let previousPreparation {
                await previousPreparation.value
            }
            do {
                try Task.checkCancellation()
                guard revision == latestAcceptedQueueRevision else {
                    throw HelperMusicError.queueSuperseded
                }
                var songs: [Song] = []
                songs.reserveCapacity(items.count)
                for item in items {
                    var request = MusicCatalogResourceRequest<Song>(
                        matching: \.id,
                        equalTo: MusicItemID(item.songID)
                    )
                    request.limit = 1
                    request.properties = [.audioVariants]
                    let response = try await request.response()
                    try Task.checkCancellation()
                    guard revision == latestAcceptedQueueRevision else {
                        throw HelperMusicError.queueSuperseded
                    }
                    guard let song = response.items.first else {
                        throw HelperMusicError.songNotFound
                    }
                    guard
                        AudioVariantPolicy.catalogOffersLosslessPlayback(song.audioVariants)
                    else {
                        throw HelperMusicError.losslessCatalogUnavailable(song.id.rawValue)
                    }
                    songs.append(song)
                }
                if queueRevision > 0, !queueFinishedEmitted {
                    sendQueueFinished(reason: "replaced", commandID: nil)
                }
                player.queue = ApplicationMusicPlayer.Queue(
                    for: songs,
                    startingAt: songs[startIndex]
                )
                try await player.prepareToPlay()
                try Task.checkCancellation()
                guard revision == latestAcceptedQueueRevision else {
                    throw HelperMusicError.queueSuperseded
                }
                queueRevision = revision
                queueItems = items
                queueEntrySegments.removeAll(keepingCapacity: true)
                for (offset, entry) in player.queue.entries.enumerated()
                    where items.indices.contains(offset)
                {
                    queueEntrySegments[entry.id] = items[offset].segmentIndex
                }
                currentSegmentIndex = items[startIndex].segmentIndex
                lastQueueEntryID = player.queue.currentEntry?.id
                queueFinishedEmitted = false
                explicitPauseActive = false
                completionWatchdog.reset()
                catalogWatchdogTicks = 0
                terminalCatalogFailureEmitted = false
                var event = statusEvent(type: "queue_prepared", commandID: command.id)
                attachQueueContext(
                    to: &event,
                    segmentIndex: items[startIndex].segmentIndex,
                    song: songs[startIndex]
                )
                sendAndCache(event, commandID: command.id)
            } catch is CancellationError {
                sendQueueSuperseded(commandID: command.id)
            } catch HelperMusicError.queueSuperseded {
                sendQueueSuperseded(commandID: command.id)
            } catch HelperMusicError.songNotFound {
                sendError(
                    commandID: command.id,
                    code: "song_not_found",
                    message: "Apple Music could not find one of the requested songs.",
                    retryable: false
                )
            } catch HelperMusicError.losslessCatalogUnavailable(let songID) {
                sendError(
                    commandID: command.id,
                    code: "lossless_catalog_unavailable",
                    message:
                        "Apple Music does not advertise a Lossless or Hi-Res Lossless version for song \(songID).",
                    retryable: false
                )
            } catch {
                sendError(
                    commandID: command.id,
                    code: "queue_prepare_failed",
                    message: "Apple Music could not prepare that queue for playback.",
                    retryable: true
                )
            }
        }
    }

    private func play(commandID: String) {
        let generation = beginPlaybackCommand()
        let expectedRevision = queueRevision
        explicitPauseActive = false
        completionWatchdog.reset()
        let expectedSongID = queueItems.first {
            $0.segmentIndex == currentSegmentIndex
        }?.songID
        Task { @MainActor in
            do {
                // `prepareToPlay()` can expose the selected variant before
                // playback. Reject a known lossy selection before asking the
                // renderer to emit any audio. A missing value is deferred to
                // Fozmo's PID-scoped Apple Lossless decoder proof after start.
                let preparedSongMatches =
                    expectedSongID == nil || currentSong()?.id.rawValue == expectedSongID
                if preparedSongMatches,
                    AudioVariantPolicy.activePlaybackDisposition(player.state.audioVariant)
                        == .reject
                {
                    let activeVariant =
                        AudioVariantPolicy.label(for: player.state.audioVariant)
                        ?? "an unsupported audio variant"
                    throw HelperMusicError.losslessPlaybackUnavailable(
                        activeVariant
                    )
                }
                for attempt in 0..<2 {
                    try await player.play()
                    guard playbackCommandIsCurrent(generation, queueRevision: expectedRevision) else {
                        throw HelperMusicError.commandSuperseded
                    }
                    if try await waitForConfirmedPlayback(
                        generation: generation,
                        queueRevision: expectedRevision,
                        expectedSongID: expectedSongID
                    ) {
                        queueFinishedEmitted = false
                        sendPlaybackState(commandID: commandID)
                        return
                    }
                    if attempt == 0 {
                        continue
                    }
                }
                throw HelperMusicError.playbackDidNotStart
            } catch is CancellationError {
                sendPlaybackSuperseded(commandID: commandID)
            } catch HelperMusicError.commandSuperseded {
                sendPlaybackSuperseded(commandID: commandID)
            } catch HelperMusicError.losslessPlaybackUnavailable(let activeVariant) {
                player.stop()
                sendError(
                    commandID: commandID,
                    code: "lossless_playback_unavailable",
                    message:
                        "MusicKit selected \(activeVariant) for this track. Fozmo requires Lossless or Hi-Res Lossless and will not send lossy audio to the DSP.",
                    retryable: false,
                    audioVariant: activeVariant
                )
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

    private func seek(_ command: IncomingCommand) {
        guard let position = command.positionSecs, position.isFinite, position >= 0 else {
            sendError(
                commandID: command.id,
                code: "apple_music_seek_invalid",
                message: "Apple Music seek position is invalid.",
                retryable: false
            )
            return
        }
        completionWatchdog.reset()
        player.playbackTime = position
        var event = statusEvent(type: "playback_time", commandID: command.id)
        attachCurrentQueueContext(to: &event)
        sendAndCache(event, commandID: command.id)
    }

    private func skipNext(commandID: String) {
        let generation = beginPlaybackCommand()
        let expectedRevision = queueRevision
        explicitPauseActive = false
        completionWatchdog.reset()
        Task { @MainActor in
            do {
                try await player.skipToNextEntry()
                guard playbackCommandIsCurrent(generation, queueRevision: expectedRevision) else {
                    throw HelperMusicError.commandSuperseded
                }
                let entryID = player.queue.currentEntry?.id
                if let song = currentSong(),
                    let segmentIndex = QueueEntrySegmentResolver.resolve(
                        mappedSegment: entryID.flatMap { queueEntrySegments[$0] },
                        songID: song.id.rawValue,
                        currentSegment: currentSegmentIndex,
                        items: queueItems
                    ),
                    QueueIndexTransition.shouldEmit(
                        previous: currentSegmentIndex,
                        next: segmentIndex,
                        validSegments: Set(queueItems.map(\.segmentIndex))
                    )
                {
                    currentSegmentIndex = segmentIndex
                    lastQueueEntryID = entryID
                    if let entryID {
                        queueEntrySegments[entryID] = segmentIndex
                    }
                    var event = statusEvent(type: "entry_changed", commandID: commandID)
                    attachQueueContext(to: &event, segmentIndex: segmentIndex, song: song)
                    sendAndCache(event, commandID: commandID)
                } else {
                    sendQueueFinished(reason: "completed", commandID: commandID)
                }
            } catch HelperMusicError.commandSuperseded {
                sendPlaybackSuperseded(commandID: commandID)
            } catch {
                sendError(
                    commandID: commandID,
                    code: "skip_next_failed",
                    message: "Apple Music could not skip to the next queue entry.",
                    retryable: true
                )
            }
        }
    }

    private func stop(commandID: String, reason: String) {
        cancelPendingQueuePreparation()
        invalidatePendingPlaybackCommand()
        explicitPauseActive = false
        completionWatchdog.reset()
        player.stop()
        sendPlaybackState(commandID: commandID, clearNowPlaying: true)
        sendQueueFinished(reason: reason, commandID: nil)
    }

    private func beginPlaybackCommand() -> UInt64 {
        playbackCommandGeneration &+= 1
        return playbackCommandGeneration
    }

    private func invalidatePendingPlaybackCommand() {
        playbackCommandGeneration &+= 1
    }

    private func playbackCommandIsCurrent(_ generation: UInt64, queueRevision: UInt64) -> Bool {
        generation == playbackCommandGeneration && queueRevision == self.queueRevision
    }

    private func cancelPendingQueuePreparation() {
        queuePreparationTask?.cancel()
        queuePreparationTask = nil
    }

    private func waitForConfirmedPlayback(
        generation: UInt64,
        queueRevision: UInt64,
        expectedSongID: String?
    ) async throws -> Bool {
        for _ in 0..<30 {
            try Task.checkCancellation()
            guard playbackCommandIsCurrent(generation, queueRevision: queueRevision) else {
                throw HelperMusicError.commandSuperseded
            }
            let isExpectedSong =
                expectedSongID == nil || currentSong()?.id.rawValue == expectedSongID
            if player.state.playbackStatus == .playing, isExpectedSong {
                switch AudioVariantPolicy.activePlaybackDisposition(player.state.audioVariant) {
                case .confirmedLossless, .requiresDecoderProof:
                    // `nil` commonly persists throughout startup on macOS.
                    // Returning here only confirms that the requested item is
                    // rendering. The server still refuses the DSP handoff
                    // unless it observes a fresh ACAppleLosslessDecoder format
                    // event from this exact renderer process.
                    return true
                case .reject:
                    throw HelperMusicError.losslessPlaybackUnavailable(
                        AudioVariantPolicy.label(for: player.state.audioVariant)
                            ?? "an unsupported audio variant"
                    )
                }
            }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        return false
    }

    private func sendQueueSuperseded(commandID: String) {
        sendError(
            commandID: commandID,
            code: "queue_prepare_superseded",
            message: "A newer Apple Music queue replaced this request.",
            retryable: true
        )
    }

    private func sendPlaybackSuperseded(commandID: String) {
        sendError(
            commandID: commandID,
            code: "playback_command_superseded",
            message: "A newer Apple Music playback command replaced this request.",
            retryable: true
        )
    }

    private func sendStatus(type: String, commandID: String?) {
        var event = statusEvent(type: type, commandID: commandID)
        attachCurrentQueueContext(to: &event)
        sendAndCache(event, commandID: commandID)
    }

    private func statusEvent(type: String, commandID: String?) -> HelperEvent {
        var event = HelperEvent(type: type)
        event.commandID = commandID
        event.sessionID = sessionID
        event.authorization = AuthorizationLabel.string(for: MusicAuthorization.currentStatus)
        event.canPlayCatalogContent = subscriptionCanPlay
        event.playbackState = PlaybackLabel.string(for: player.state.playbackStatus)
        event.audioVariant = AudioVariantPolicy.label(for: player.state.audioVariant)
        event.playbackTimeSecs = player.playbackTime
        event.playbackPosition = player.playbackTime
        event.queueRevision = queueRevision
        event.nowPlaying = currentSong().map(NowPlayingPayload.init(song:))
        return event
    }

    private func sendPlaybackState(commandID: String?, clearNowPlaying: Bool = false) {
        var event = statusEvent(type: "playback_state_changed", commandID: commandID)
        attachCurrentQueueContext(to: &event)
        if clearNowPlaying {
            event.nowPlaying = nil
            event.songID = nil
        }
        lastPlaybackState = event.playbackState ?? "stopped"
        sendAndCache(event, commandID: commandID)
    }

    private func sendQueueFinished(reason: String, commandID: String?) {
        guard !queueFinishedEmitted, queueRevision > 0 else { return }
        queueFinishedEmitted = true
        var event = statusEvent(type: "queue_finished", commandID: commandID)
        event.finishReason = reason
        attachCurrentQueueContext(to: &event)
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
        monitorCatalogAccess()
        let playbackState = PlaybackLabel.string(for: player.state.playbackStatus)
        if playbackState == "interrupted" {
            var event = statusEvent(type: "interrupted", commandID: nil)
            event.code = "apple_music_interrupted"
            event.message = "MusicKit interrupted Apple Music playback."
            event.retryable = true
            attachCurrentQueueContext(to: &event)
            sendEvent(event)
        } else if playbackState != lastPlaybackState {
            let previous = lastPlaybackState
            lastPlaybackState = playbackState
            sendPlaybackState(commandID: nil)
            if let finishReason = QueueFinishReason.forPlaybackTransition(
                previous: previous,
                current: playbackState
            ) {
                sendQueueFinished(reason: finishReason, commandID: nil)
            }
        }

        let entryID = player.queue.currentEntry?.id
        if entryID != lastQueueEntryID {
            lastQueueEntryID = entryID
            if let song = currentSong(),
                let segmentIndex = QueueEntrySegmentResolver.resolve(
                    mappedSegment: entryID.flatMap { queueEntrySegments[$0] },
                    songID: song.id.rawValue,
                    currentSegment: currentSegmentIndex,
                    items: queueItems
                ),
                QueueIndexTransition.shouldEmit(
                    previous: currentSegmentIndex,
                    next: segmentIndex,
                    validSegments: Set(queueItems.map(\.segmentIndex))
                )
            {
                currentSegmentIndex = segmentIndex
                if let entryID {
                    queueEntrySegments[entryID] = segmentIndex
                }
                completionWatchdog.reset()
                var event = statusEvent(type: "entry_changed", commandID: nil)
                attachQueueContext(to: &event, segmentIndex: segmentIndex, song: song)
                sendEvent(event)
            }
        }

        let isFinalEntry =
            currentSegmentIndex != nil
            && currentSegmentIndex == queueItems.last?.segmentIndex
        if !queueFinishedEmitted,
            completionWatchdog.observe(
                playbackState: playbackState,
                position: player.playbackTime,
                duration: currentSong()?.duration,
                isFinalEntry: isFinalEntry,
                explicitPause: explicitPauseActive
            )
        {
            sendQueueFinished(reason: "completed", commandID: nil)
        }

        var timeEvent = statusEvent(type: "playback_time", commandID: nil)
        attachCurrentQueueContext(to: &timeEvent)
        sendEvent(timeEvent)
    }

    private func monitorCatalogAccess() {
        guard queueRevision > 0, !terminalCatalogFailureEmitted else { return }
        guard MusicAuthorization.currentStatus == .authorized else {
            terminalCatalogFailureEmitted = true
            sendError(
                commandID: nil,
                code: "music_authorization_revoked",
                message: "Apple Music authorization was revoked during playback.",
                retryable: false
            )
            player.stop()
            return
        }
        catalogWatchdogTicks += 1
        guard catalogWatchdogTicks % 10 == 0, !catalogWatchdogInFlight else { return }
        catalogWatchdogInFlight = true
        Task { @MainActor in
            defer { catalogWatchdogInFlight = false }
            let canPlay = try? await MusicSubscription.current.canPlayCatalogContent
            subscriptionCanPlay = canPlay
            guard canPlay == true else {
                terminalCatalogFailureEmitted = true
                sendError(
                    commandID: nil,
                    code: "subscription_required",
                    message: "The Apple Music subscription became unavailable during playback.",
                    retryable: true
                )
                player.stop()
                return
            }
        }
    }

    private func currentSong() -> Song? {
        guard let item = player.queue.currentEntry?.item else { return nil }
        guard case .song(let song) = item else { return nil }
        return song
    }

    private func currentSegment() -> Int? {
        if let entryID = player.queue.currentEntry?.id,
            let segment = queueEntrySegments[entryID]
        {
            return segment
        }
        return currentSegmentIndex
    }

    private func attachCurrentQueueContext(to event: inout HelperEvent) {
        attachQueueContext(
            to: &event,
            segmentIndex: currentSegment(),
            song: currentSong()
        )
    }

    private func attachQueueContext(
        to event: inout HelperEvent,
        segmentIndex: Int?,
        song: Song?
    ) {
        event.queueRevision = queueRevision
        event.segmentIndex = segmentIndex
        event.songID = song?.id.rawValue
        event.playbackPosition = player.playbackTime
        if let song {
            event.nowPlaying = NowPlayingPayload(song: song)
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
                message: "This Apple Music account cannot play catalog content.",
                retryable: false
            )
            return false
        }
        return true
    }

    private func sendError(
        commandID: String?,
        code: String,
        message: String,
        retryable: Bool,
        audioVariant: String? = nil
    ) {
        var event = HelperEvent(type: "helper_error")
        event.commandID = commandID
        event.sessionID = sessionID
        event.code = code
        event.message = message
        event.retryable = retryable
        event.playbackState = PlaybackLabel.string(for: player.state.playbackStatus)
        event.audioVariant =
            audioVariant ?? AudioVariantPolicy.label(for: player.state.audioVariant)
        attachCurrentQueueContext(to: &event)
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
    case queueSuperseded
    case commandSuperseded
    case playbackDidNotStart
    case losslessCatalogUnavailable(String)
    case losslessPlaybackUnavailable(String)
}
