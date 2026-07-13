import Foundation

@MainActor
final class BonjourPublisher: NSObject {
    private var services: [NetService] = []

    func start(port: Int32 = 3001) {
        stop()
        let instanceName = "Fozmo — \(LauncherPaths.localHostName)"
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "development"
        var txtValues: [String: Data] = [
            "path": Data("/".utf8),
            "version": Data(version.utf8),
            "pairing": Data("required".utf8),
            "base_url": Data(LauncherPaths.lanURL.absoluteString.utf8),
        ]
        if let fallback = LauncherPaths.lanIPURL {
            txtValues["fallback_url"] = Data(fallback.absoluteString.utf8)
        }
        let txt = NetService.data(fromTXTRecord: txtValues)

        for type in ["_fozmo._tcp.", "_http._tcp."] {
            let service = NetService(domain: "local.", type: type, name: instanceName, port: port)
            service.setTXTRecord(txt)
            service.publish()
            services.append(service)
        }
    }

    func stop() {
        services.forEach { service in
            service.stop()
        }
        services.removeAll()
    }
}
