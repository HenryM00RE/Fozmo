import AppKit
import Darwin
import Foundation

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
        guard
            let socketPath = environment["FOZMO_APPLE_MUSIC_SOCKET"],
            let token = environment["FOZMO_APPLE_MUSIC_TOKEN"],
            let sessionID = environment["FOZMO_APPLE_MUSIC_SESSION_ID"],
            !socketPath.isEmpty,
            !token.isEmpty,
            !sessionID.isEmpty
        else {
            showFatalError("Fozmo did not provide a private helper session.")
            return
        }
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
