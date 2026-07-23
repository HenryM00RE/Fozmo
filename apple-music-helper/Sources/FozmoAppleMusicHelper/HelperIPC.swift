import Darwin
import Foundation

enum HelperIPCError: LocalizedError {
    case invalidSocketPath
    case connectFailed(Int32)
    case disconnected
    case invalidFrameLength(Int)

    var errorDescription: String? {
        switch self {
        case .invalidSocketPath:
            return "The private IPC socket path is invalid."
        case .connectFailed(let code):
            return "The private IPC connection failed (\(code))."
        case .disconnected:
            return "The private IPC connection closed."
        case .invalidFrameLength(let length):
            return "The private IPC frame length is invalid (\(length))."
        }
    }
}

final class HelperIPC {
    static let maximumMessageBytes = 1024 * 1024

    private let fileDescriptor: Int32
    private let writeQueue = DispatchQueue(label: "com.fozmo.apple-music-helper.ipc.write")
    private let readQueue = DispatchQueue(label: "com.fozmo.apple-music-helper.ipc.read")
    private let encoder = JSONEncoder()
    private var closed = false
    private let closeLock = NSLock()

    init(socketPath: String) throws {
        guard !socketPath.isEmpty else {
            throw HelperIPCError.invalidSocketPath
        }
        let descriptor = socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw HelperIPCError.connectFailed(errno)
        }

        var address = sockaddr_un()
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        address.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(socketPath.utf8CString)
        let pathCapacity = MemoryLayout.size(ofValue: address.sun_path)
        guard pathBytes.count <= pathCapacity else {
            Darwin.close(descriptor)
            throw HelperIPCError.invalidSocketPath
        }
        withUnsafeMutableBytes(of: &address.sun_path) { destination in
            pathBytes.withUnsafeBytes { source in
                destination.copyBytes(from: source)
            }
        }
        let addressLength = socklen_t(MemoryLayout<sockaddr_un>.size)
        let result = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, addressLength)
            }
        }
        guard result == 0 else {
            let code = errno
            Darwin.close(descriptor)
            throw HelperIPCError.connectFailed(code)
        }
        fileDescriptor = descriptor
    }

    deinit {
        close()
    }

    func start(
        onFrame: @escaping (Data) -> Void,
        onDisconnect: @escaping () -> Void
    ) {
        readQueue.async { [weak self] in
            guard let self else { return }
            do {
                while true {
                    let header = try self.readExactly(4)
                    let length = header.withUnsafeBytes {
                        Int(UInt32(bigEndian: $0.loadUnaligned(as: UInt32.self)))
                    }
                    guard length > 0, length <= Self.maximumMessageBytes else {
                        throw HelperIPCError.invalidFrameLength(length)
                    }
                    onFrame(try self.readExactly(length))
                }
            } catch {
                self.close()
                onDisconnect()
            }
        }
    }

    func send<T: Encodable>(_ value: T) {
        writeQueue.async { [weak self] in
            guard let self else { return }
            do {
                let payload = try self.encoder.encode(value)
                guard !payload.isEmpty, payload.count <= Self.maximumMessageBytes else {
                    throw HelperIPCError.invalidFrameLength(payload.count)
                }
                var length = UInt32(payload.count).bigEndian
                let header = Data(bytes: &length, count: MemoryLayout<UInt32>.size)
                try self.writeAll(header)
                try self.writeAll(payload)
            } catch {
                self.close()
            }
        }
    }

    func close() {
        closeLock.lock()
        defer { closeLock.unlock() }
        guard !closed else { return }
        closed = true
        Darwin.shutdown(fileDescriptor, SHUT_RDWR)
        Darwin.close(fileDescriptor)
    }

    private func readExactly(_ count: Int) throws -> Data {
        var data = Data(count: count)
        var offset = 0
        while offset < count {
            let received = data.withUnsafeMutableBytes { buffer -> Int in
                guard let baseAddress = buffer.baseAddress else { return -1 }
                return Darwin.read(fileDescriptor, baseAddress.advanced(by: offset), count - offset)
            }
            if received == 0 {
                throw HelperIPCError.disconnected
            }
            if received < 0 {
                if errno == EINTR { continue }
                throw HelperIPCError.disconnected
            }
            offset += received
        }
        return data
    }

    private func writeAll(_ data: Data) throws {
        var offset = 0
        while offset < data.count {
            let written = data.withUnsafeBytes { buffer -> Int in
                guard let baseAddress = buffer.baseAddress else { return -1 }
                return Darwin.write(fileDescriptor, baseAddress.advanced(by: offset), data.count - offset)
            }
            if written < 0 {
                if errno == EINTR { continue }
                throw HelperIPCError.disconnected
            }
            if written == 0 {
                throw HelperIPCError.disconnected
            }
            offset += written
        }
    }
}
