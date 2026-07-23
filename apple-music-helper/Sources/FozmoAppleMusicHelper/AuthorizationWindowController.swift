import AppKit

@MainActor
final class AuthorizationWindowController {
    private var window: NSWindow?

    func show() {
        if window == nil {
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 430, height: 180),
                styleMask: [.titled, .closable],
                backing: .buffered,
                defer: false
            )
            window.title = "Authorize Apple Music"
            window.isReleasedWhenClosed = false

            let label = NSTextField(wrappingLabelWithString:
                "Fozmo needs Apple Music access to prepare and play the song ID you select. Complete the Apple authorization prompt, then return to Fozmo."
            )
            label.font = .systemFont(ofSize: 14)
            label.textColor = .labelColor
            label.translatesAutoresizingMaskIntoConstraints = false

            let content = NSView()
            content.addSubview(label)
            NSLayoutConstraint.activate([
                label.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 28),
                label.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -28),
                label.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            ])
            window.contentView = content
            self.window = window
        }
        NSApp.activate(ignoringOtherApps: true)
        window?.center()
        window?.makeKeyAndOrderFront(nil)
    }

    func hide() {
        window?.orderOut(nil)
    }
}
