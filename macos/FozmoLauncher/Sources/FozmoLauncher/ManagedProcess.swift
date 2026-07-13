import Darwin
import Foundation

final class RotatingLogSink {
    let pipe = Pipe()

    private let url: URL
    private let maximumBytes: UInt64
    private let retainedFiles: Int
    private let queue: DispatchQueue
    private var handle: FileHandle?
    private var byteCount: UInt64 = 0

    init(url: URL, maximumBytes: UInt64 = 10 * 1_024 * 1_024, retainedFiles: Int = 5) throws {
        self.url = url
        self.maximumBytes = maximumBytes
        self.retainedFiles = retainedFiles
        queue = DispatchQueue(label: "com.fozmo.log.\(url.lastPathComponent)")
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try openCurrentFile()

        pipe.fileHandleForReading.readabilityHandler = { [weak self] reader in
            let data = reader.availableData
            guard !data.isEmpty else {
                reader.readabilityHandler = nil
                return
            }
            self?.queue.async { self?.append(data) }
        }
    }

    private func openCurrentFile() throws {
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        handle = try FileHandle(forWritingTo: url)
        try handle?.seekToEnd()
        let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
        byteCount = (attributes?[.size] as? NSNumber)?.uint64Value ?? 0
    }

    private func append(_ data: Data) {
        do {
            if byteCount + UInt64(data.count) > maximumBytes {
                try rotate()
            }
            try handle?.write(contentsOf: data)
            byteCount += UInt64(data.count)
        } catch {
            // Process supervision must not fail because log rotation failed.
        }
    }

    private func rotate() throws {
        try handle?.synchronize()
        try handle?.close()
        handle = nil

        if retainedFiles > 1 {
            for index in stride(from: retainedFiles - 1, through: 1, by: -1) {
                let source = URL(fileURLWithPath: "\(url.path).\(index)")
                let destination = URL(fileURLWithPath: "\(url.path).\(index + 1)")
                try? FileManager.default.removeItem(at: destination)
                if FileManager.default.fileExists(atPath: source.path) {
                    try FileManager.default.moveItem(at: source, to: destination)
                }
            }
        }
        let firstArchive = URL(fileURLWithPath: "\(url.path).1")
        try? FileManager.default.removeItem(at: firstArchive)
        if FileManager.default.fileExists(atPath: url.path) {
            try FileManager.default.moveItem(at: url, to: firstArchive)
        }
        try openCurrentFile()
    }

    func finish() {
        pipe.fileHandleForReading.readabilityHandler = nil
        try? pipe.fileHandleForWriting.close()
        queue.sync {
            try? handle?.synchronize()
            try? handle?.close()
            handle = nil
        }
    }
}

final class ManagedProcess {
    struct Exit {
        let status: Int32
        let reason: Process.TerminationReason
    }

    let name: String
    private(set) var process: Process?
    private var logSink: RotatingLogSink?
    private var inputPipe: Pipe?

    init(name: String) {
        self.name = name
    }

    var isRunning: Bool { process?.isRunning == true }

    func start(
        executable: URL,
        arguments: [String],
        environment: [String: String],
        currentDirectory: URL,
        logURL: URL,
        onExit: @escaping (Exit) -> Void
    ) throws {
        guard process == nil || process?.isRunning == false else { return }
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw LauncherError.missingExecutable(executable.path)
        }

        let sink = try RotatingLogSink(url: logURL)
        let stdin = Pipe()
        let child = Process()
        child.executableURL = executable
        child.arguments = arguments
        child.environment = environment
        child.currentDirectoryURL = currentDirectory
        child.standardOutput = sink.pipe
        child.standardError = sink.pipe
        child.standardInput = stdin
        child.terminationHandler = { [weak self] finished in
            let exit = Exit(status: finished.terminationStatus, reason: finished.terminationReason)
            self?.logSink?.finish()
            self?.logSink = nil
            try? self?.inputPipe?.fileHandleForWriting.close()
            self?.inputPipe = nil
            onExit(exit)
        }

        try child.run()
        process = child
        logSink = sink
        inputPipe = stdin
    }

    func stop(timeout: TimeInterval, allowForce: Bool, completion: @escaping (Bool) -> Void) {
        guard let child = process, child.isRunning else {
            completion(true)
            return
        }

        // EOF is the primary graceful-shutdown signal. The retained write end
        // also closes automatically if the launcher crashes.
        try? inputPipe?.fileHandleForWriting.close()
        DispatchQueue.global(qos: .userInitiated).async {
            let deadline = Date().addingTimeInterval(timeout)
            let terminateAt = Date().addingTimeInterval(timeout / 2)
            while child.isRunning && Date() < deadline {
                if Date() >= terminateAt {
                    child.terminate()
                }
                Thread.sleep(forTimeInterval: 0.1)
            }

            if child.isRunning && allowForce {
                _ = Darwin.kill(child.processIdentifier, SIGKILL)
                let forceDeadline = Date().addingTimeInterval(2)
                while child.isRunning && Date() < forceDeadline {
                    Thread.sleep(forTimeInterval: 0.05)
                }
            }
            let stopped = !child.isRunning
            DispatchQueue.main.async { completion(stopped) }
        }
    }
}

enum LauncherError: LocalizedError {
    case missingExecutable(String)
    case startupTimeout
    case serverExited
    case backupFailed(String)

    var errorDescription: String? {
        switch self {
        case let .missingExecutable(path): "Required executable is missing or not executable: \(path)"
        case .startupTimeout: "Fozmo did not become ready within 30 seconds."
        case .serverExited: "The Fozmo server exited before it became ready."
        case let .backupFailed(message): "The pre-update backup failed: \(message)"
        }
    }
}
