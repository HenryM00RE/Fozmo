import AppKit
import Darwin
import Foundation

private struct HelperLaunchBootstrap: Decodable {
    let socketPath: String
    let token: String
    let sessionID: String
}

@main
enum FozmoAppleMusicHelperMain {
    static func main() {
        let application = NSApplication.shared
        let delegate = HelperAppDelegate()
        application.delegate = delegate
        application.setActivationPolicy(.accessory)
        withExtendedLifetime(delegate) {
            application.run()
        }
    }
}

@MainActor
final class HelperAppDelegate: NSObject, NSApplicationDelegate {
    private var ipc: HelperIPC?
    private var musicSession: MusicSessionController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let environment = ProcessInfo.processInfo.environment
        let bootstrap: HelperLaunchBootstrap
        do {
            bootstrap = try Self.launchBootstrap(from: environment)
        } catch {
            showFatalError("Fozmo did not provide a valid private helper session.")
            return
        }
        guard
            !bootstrap.socketPath.isEmpty,
            !bootstrap.token.isEmpty,
            !bootstrap.sessionID.isEmpty
        else {
            showFatalError("Fozmo did not provide a private helper session.")
            return
        }
        let socketPath = bootstrap.socketPath
        let token = bootstrap.token
        let sessionID = bootstrap.sessionID
        unsetenv("FOZMO_APPLE_MUSIC_BOOTSTRAP")
        unsetenv("FOZMO_APPLE_MUSIC_SOCKET")
        unsetenv("FOZMO_APPLE_MUSIC_TOKEN")
        unsetenv("FOZMO_APPLE_MUSIC_SESSION_ID")

        do {
            let ipc = try HelperIPC(socketPath: socketPath)
            let musicSession = MusicSessionController(sessionID: sessionID) {
                [weak ipc] event in
                ipc?.send(event)
            }
            self.ipc = ipc
            self.musicSession = musicSession
            ipc.start(
                onFrame: { [weak musicSession] frame in
                    Task { @MainActor in
                        musicSession?.handle(frame: frame)
                    }
                },
                onDisconnect: { [weak musicSession] in
                    Task { @MainActor in
                        musicSession?.connectionClosed()
                    }
                }
            )
            musicSession.sendHello(token: token)
        } catch {
            showFatalError("Fozmo could not open the private helper connection.")
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        ipc?.close()
    }

    private static func launchBootstrap(
        from environment: [String: String]
    ) throws -> HelperLaunchBootstrap {
        if let bootstrapPath = environment["FOZMO_APPLE_MUSIC_BOOTSTRAP"],
            !bootstrapPath.isEmpty
        {
            defer {
                try? FileManager.default.removeItem(atPath: bootstrapPath)
            }
            let data = try Data(contentsOf: URL(fileURLWithPath: bootstrapPath))
            return try JSONDecoder().decode(HelperLaunchBootstrap.self, from: data)
        }
        return HelperLaunchBootstrap(
            socketPath: environment["FOZMO_APPLE_MUSIC_SOCKET"] ?? "",
            token: environment["FOZMO_APPLE_MUSIC_TOKEN"] ?? "",
            sessionID: environment["FOZMO_APPLE_MUSIC_SESSION_ID"] ?? ""
        )
    }

    private func showFatalError(_ message: String) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = "Fozmo Apple Music Helper"
        alert.informativeText = message
        alert.runModal()
        NSApp.terminate(nil)
    }
}
