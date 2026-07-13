import AppKit
import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI

struct PairingView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 16) {
            if let link = model.pairingLink {
                Text("Pair a device")
                    .font(.title2.weight(.semibold))
                Text("Scan this code from a device on the same network.")
                    .foregroundStyle(.secondary)

                if let image = QRCode.image(for: link.localNetworkURL.absoluteString) {
                    Image(nsImage: image)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 220, height: 220)
                        .accessibilityLabel("QR code for \(link.localNetworkURL.absoluteString)")
                }

                Text(link.localNetworkURL.absoluteString)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .lineLimit(2)

                HStack {
                    Button("Copy .local Link") { model.copyAddress(link.localNetworkURL) }
                    if let fallback = link.ipFallbackURL {
                        Button("Copy IP Fallback") { model.copyAddress(fallback) }
                    }
                }

                Text("Single use · Expires \(link.expiresAt.formatted(date: .omitted, time: .shortened))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ProgressView("Creating pairing link…")
            }
        }
        .padding(24)
        .frame(minWidth: 420, minHeight: 390)
    }
}

@MainActor
final class PairingWindowController: NSWindowController {
    init(model: AppModel) {
        let root = PairingView().environmentObject(model)
        let hosting = NSHostingController(rootView: root)
        let window = NSWindow(contentViewController: hosting)
        window.title = "Pair a Device"
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false
        window.center()
        super.init(window: window)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private enum QRCode {
    static func image(for value: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(value.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let transformed = output.transformed(by: CGAffineTransform(scaleX: 12, y: 12))
        let context = CIContext(options: [.useSoftwareRenderer: false])
        guard let cgImage = context.createCGImage(transformed, from: transformed.extent) else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: transformed.extent.width, height: transformed.extent.height))
    }
}
