import AppKit
import SwiftUI

private enum LaunchContext {
    // keyAELaunchedAsLogInItem ('lgit') is present in the open-application
    // event when launchd starts a registered login item.
    static var isLoginItem: Bool {
        if ProcessInfo.processInfo.arguments.contains("--login-item") { return true }
        let keyword: AEKeyword = 0x6C67_6974
        return NSAppleEventManager.shared().currentAppleEvent?
            .paramDescriptor(forKeyword: keyword) != nil
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let model = AppModel.shared
    private var awaitingTermination = false
    private var onboardingWindow: OnboardingWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        NSApp.disableRelaunchOnLogin()

        guard model.claimPrimaryInstance() else {
            NSWorkspace.shared.open(LauncherPaths.localURL)
            NSApp.terminate(nil)
            return
        }
        if model.requiresFirstRunSetup {
            let controller = OnboardingWindowController(model: model) { [weak self] in
                self?.onboardingWindow?.close()
                self?.onboardingWindow = nil
                self?.model.start(openBrowser: !LaunchContext.isLoginItem)
            }
            onboardingWindow = controller
            NSApp.activate(ignoringOtherApps: true)
            controller.showWindow(nil)
        } else {
            model.start(openBrowser: !LaunchContext.isLoginItem)
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        model.openFozmo()
        return false
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if model.isSafeToTerminate { return .terminateNow }
        guard !awaitingTermination else { return .terminateLater }
        awaitingTermination = true
        model.prepareForSystemTermination { stopped in
            self.awaitingTermination = false
            sender.reply(toApplicationShouldTerminate: stopped)
        }
        return .terminateLater
    }
}

@main
struct FozmoLauncherApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel.shared

    var body: some Scene {
        MenuBarExtra {
            LauncherMenu()
                .environmentObject(model)
        } label: {
            Image(systemName: model.updateManager.updateAvailable ? "hifispeaker.2.fill" : "hifispeaker.2")
                .accessibilityLabel("Fozmo — \(model.status.label)")
        }
        .menuBarExtraStyle(.menu)

    }
}
