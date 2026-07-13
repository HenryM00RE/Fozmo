import Darwin
import Foundation
import SystemConfiguration

enum LauncherPaths {
    private static let environment = ProcessInfo.processInfo.environment

    static let dataRoot: URL = {
        if let value = environment["FOZMO_DATA_DIR"], !value.isEmpty {
            return URL(fileURLWithPath: value, isDirectory: true)
        }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Fozmo", isDirectory: true)
    }()

    static let cacheRoot: URL = {
        if let value = environment["FOZMO_CACHE_DIR"], !value.isEmpty {
            return URL(fileURLWithPath: value, isDirectory: true)
        }
        return FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Fozmo", isDirectory: true)
    }()

    static let logRoot: URL = {
        if let value = environment["FOZMO_LOG_DIR"], !value.isEmpty {
            return URL(fileURLWithPath: value, isDirectory: true)
        }
        return FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Logs/Fozmo", isDirectory: true)
    }()

    static let resourceRoot: URL = {
        if let value = environment["FOZMO_RESOURCE_DIR"], !value.isEmpty {
            return URL(fileURLWithPath: value, isDirectory: true)
        }
        return Bundle.main.resourceURL ?? URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
    }()

    static let runtimeRoot: URL = FileManager.default.temporaryDirectory
        .appendingPathComponent("com.fozmo.app-\(getuid())", isDirectory: true)

    static let localURL = URL(string: "http://localhost:3001")!
    static let remoteAccessURL = URL(string: "http://localhost:3001/#/settings/remote")!

    static var localHostName: String {
        let raw = (SCDynamicStoreCopyLocalHostName(nil) as String?) ?? Host.current().localizedName ?? "fozmo"
        let normalized = raw.lowercased().map { character -> Character in
            character.isLetter || character.isNumber || character == "-" ? character : "-"
        }
        let compact = String(normalized).trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        return compact.isEmpty ? "fozmo" : compact
    }

    static var lanURL: URL {
        URL(string: "http://\(localHostName).local:3001")!
    }

    static var lanIPAddress: String? {
        var addressList: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&addressList) == 0, let first = addressList else { return nil }
        defer { freeifaddrs(addressList) }

        var candidates: [(priority: Int, address: String)] = []
        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let interface = cursor {
            defer { cursor = interface.pointee.ifa_next }
            guard let address = interface.pointee.ifa_addr,
                  address.pointee.sa_family == UInt8(AF_INET)
            else { continue }

            let flags = Int32(interface.pointee.ifa_flags)
            guard flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0 else { continue }

            var buffer = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            let result = getnameinfo(
                address,
                socklen_t(address.pointee.sa_len),
                &buffer,
                socklen_t(buffer.count),
                nil,
                0,
                NI_NUMERICHOST
            )
            guard result == 0 else { continue }
            let name = String(cString: interface.pointee.ifa_name)
            let priority = name == "en0" ? 0 : (name == "en1" ? 1 : 2)
            candidates.append((priority, String(cString: buffer)))
        }
        return candidates.sorted { $0.priority < $1.priority }.first?.address
    }

    static var lanIPURL: URL? {
        lanIPAddress.flatMap { URL(string: "http://\($0):3001") }
    }

    static var serverExecutable: URL {
        if let value = environment["FOZMO_SERVER_EXECUTABLE"], !value.isEmpty {
            return URL(fileURLWithPath: value)
        }
        return Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/fozmo-server")
    }

    static var cliExecutable: URL {
        Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/fozmoctl")
    }

    static var airPlayExecutable: URL {
        if let value = environment["FOZMO_AIRPLAY_HELPER_EXECUTABLE"], !value.isEmpty {
            return URL(fileURLWithPath: value)
        }
        return Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/fozmo-airplay-helper")
    }

    static var ffmpegExecutable: URL {
        if let value = environment["FOZMO_FFMPEG_PATH"], !value.isEmpty {
            return URL(fileURLWithPath: value)
        }
        return Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/ffmpeg")
    }

    static let airPlayControlSocket = runtimeRoot.appendingPathComponent("airplay.sock")
    static let airPlayAudioSocket = runtimeRoot.appendingPathComponent("pcm.sock")

    static func prepareDirectories(createDataRoot: Bool = true) throws {
        try FileManager.default.createDirectory(
            at: dataRoot.deletingLastPathComponent(),
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        var directories = [cacheRoot, logRoot, runtimeRoot]
        if createDataRoot { directories.insert(dataRoot, at: 0) }
        for directory in directories {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700]
            )
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
        }
    }
}

final class LauncherLock {
    private var descriptor: Int32 = -1

    init?(url: URL) {
        descriptor = Darwin.open(url.path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR)
        guard descriptor >= 0 else { return nil }
        guard flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            Darwin.close(descriptor)
            descriptor = -1
            return nil
        }

        let pid = "\(getpid())\n"
        _ = ftruncate(descriptor, 0)
        pid.withCString { pointer in
            _ = Darwin.write(descriptor, pointer, strlen(pointer))
        }
    }

    deinit {
        if descriptor >= 0 {
            _ = flock(descriptor, LOCK_UN)
            Darwin.close(descriptor)
        }
    }
}
