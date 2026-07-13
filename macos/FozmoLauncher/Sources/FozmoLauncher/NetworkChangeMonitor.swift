import Foundation
import Network

final class NetworkChangeMonitor {
    private let queue = DispatchQueue(label: "com.fozmo.network-monitor")
    private var pathMonitor: NWPathMonitor?
    private var pollTimer: DispatchSourceTimer?
    private var debounceWork: DispatchWorkItem?
    private var lastAddress: String?

    func start(onAddressChange: @escaping () -> Void) {
        stop()
        lastAddress = LauncherPaths.lanIPAddress

        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] _ in
            self?.inspectAddress(onAddressChange: onAddressChange)
        }
        monitor.start(queue: queue)
        pathMonitor = monitor

        // NWPath catches interface changes; polling also catches a DHCP lease
        // changing the address while the same interface remains preferred.
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 10, repeating: 10)
        timer.setEventHandler { [weak self] in
            self?.inspectAddress(onAddressChange: onAddressChange)
        }
        timer.resume()
        pollTimer = timer
    }

    func stop() {
        pathMonitor?.cancel()
        pathMonitor = nil
        pollTimer?.cancel()
        pollTimer = nil
        debounceWork?.cancel()
        debounceWork = nil
    }

    private func inspectAddress(onAddressChange: @escaping () -> Void) {
        let current = LauncherPaths.lanIPAddress
        guard current != lastAddress else { return }
        lastAddress = current
        debounceWork?.cancel()
        let work = DispatchWorkItem {
            DispatchQueue.main.async(execute: onAddressChange)
        }
        debounceWork = work
        queue.asyncAfter(deadline: .now() + 3, execute: work)
    }
}
