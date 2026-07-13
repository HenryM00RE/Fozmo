import AppKit
import SwiftUI

struct OnboardingView: View {
    @ObservedObject var model: AppModel
    let onFinished: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Welcome to Fozmo")
                .font(.largeTitle.weight(.semibold))
            Text("Fozmo keeps your library metadata, history, settings and artwork in Application Support so replacing the app cannot erase them.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            GroupBox {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Data location")
                        .font(.headline)
                    Text(LauncherPaths.dataRoot.path)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(4)
            }

            if model.isImportingWorkspace {
                ProgressView(model.importStatusText)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }

            if let error = model.importError {
                Text(error)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Button("Import Existing Workspace…") { selectWorkspace() }
                    .disabled(model.isImportingWorkspace)
                Spacer()
                Button("Start Fresh") { onFinished() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(model.isImportingWorkspace)
            }
        }
        .padding(28)
        .frame(width: 540)
    }

    private func selectWorkspace() {
        let panel = NSOpenPanel()
        panel.title = "Choose the existing Fozmo workspace"
        panel.prompt = "Import"
        panel.message = "Choose the folder containing settings.json and library/. The original folder will not be changed or deleted."
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let source = panel.url else { return }
        model.importExistingWorkspace(from: source) { succeeded in
            if succeeded { onFinished() }
        }
    }
}

@MainActor
final class OnboardingWindowController: NSWindowController {
    init(model: AppModel, onFinished: @escaping () -> Void) {
        let root = OnboardingView(model: model, onFinished: onFinished)
        let hosting = NSHostingController(rootView: root)
        let window = NSWindow(contentViewController: hosting)
        window.title = "Set Up Fozmo"
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false
        window.center()
        super.init(window: window)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}
