import SwiftUI

struct LauncherMenu: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        Group {
            Label(model.status.label, systemImage: model.status.symbolName)
            Text(model.airPlayStatus.label)
                .foregroundStyle(.secondary)

            Divider()

            Button("Open Fozmo") { model.openFozmo() }
                .keyboardShortcut("o")
                .disabled(model.status != .running)

            Menu("Addresses") {
                Button("Copy Local Address") { model.copyAddress(LauncherPaths.localURL) }
                Button("Copy LAN Address") { model.copyAddress(LauncherPaths.lanURL) }
                if let fallback = LauncherPaths.lanIPURL {
                    Button("Copy IP Fallback") { model.copyAddress(fallback) }
                }
            }

            Button("Remote Access…") { model.openRemoteAccessSettings() }
                .disabled(model.status != .running)

            Button("Pair a Device…") { model.showPairingLink() }
                .disabled(model.status != .running || !model.lanEnabled)

            Divider()

            Toggle(
                "Allow LAN Access",
                isOn: Binding(
                    get: { model.lanEnabled },
                    set: { model.setLANEnabled($0) }
                )
            )
            Toggle(
                "Require LAN Authentication",
                isOn: Binding(
                    get: { model.lanAuthenticationRequired },
                    set: { model.setLANAuthenticationRequired($0) }
                )
            )
            .disabled(!model.lanEnabled)
            if model.lanEnabled && !model.lanAuthenticationRequired {
                Label("LAN access is unauthenticated", systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
            }
            Toggle(
                "Launch at Login",
                isOn: Binding(
                    get: { model.launchAtLogin },
                    set: { model.setLaunchAtLogin($0) }
                )
            )

            UpdateMenu(updateManager: model.updateManager)

            Divider()

            Button("Start Server") { model.start(openBrowser: false) }
                .disabled(!model.canStart)
            Button("Stop Server") { model.stop() }
                .disabled(!model.canStop)
            Button("Restart Server") { model.restart() }
                .disabled(!model.canStop)

            Menu("Support") {
                Button("Copy fozmoctl Path") { model.copyCLIPath() }
                Button("Show Data") { model.showDataDirectory() }
                Button("Show Logs") { model.showLogDirectory() }
                if let error = model.lastError {
                    Divider()
                    Text(error)
                }
            }

            Divider()
            Button("Quit Fozmo") { model.quit() }
                .keyboardShortcut("q")
        }
    }
}

private struct UpdateMenu: View {
    @ObservedObject var updateManager: UpdateManager

    var body: some View {
        Button(updateManager.updateAvailable ? "Install Available Update…" : "Check for Updates…") {
            updateManager.checkForUpdates()
        }
        .disabled(!updateManager.isEnabled)

        if updateManager.updateAvailable {
            Text(updateManager.statusText)
        }
    }
}
