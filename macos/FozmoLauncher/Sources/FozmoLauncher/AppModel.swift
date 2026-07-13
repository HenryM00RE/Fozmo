import AppKit
import Foundation
import Security
import ServiceManagement

enum ServerStatus: Equatable {
    case stopped
    case starting
    case running
    case stopping
    case failed(String)

    var label: String {
        switch self {
        case .stopped: "Stopped"
        case .starting: "Starting…"
        case .running: "Running"
        case .stopping: "Stopping…"
        case let .failed(message): "Error: \(message)"
        }
    }

    var symbolName: String {
        switch self {
        case .running: "circle.fill"
        case .starting, .stopping: "circle.dotted"
        case .stopped: "circle"
        case .failed: "exclamationmark.triangle.fill"
        }
    }
}

enum AirPlayHelperStatus: Equatable {
    case ready
    case missing
    case degraded

    var label: String {
        switch self {
        case .ready: "AirPlay helper ready"
        case .missing: "AirPlay helper not installed"
        case .degraded: "AirPlay helper stopped"
        }
    }
}

struct PairingLink: Identifiable {
    let id = UUID()
    let localNetworkURL: URL
    let ipFallbackURL: URL?
    let expiresAt: Date
}

private struct PairingStartResponse: Decodable {
    let token: String
    let expiresAtUnixSecs: UInt64

    enum CodingKeys: String, CodingKey {
        case token
        case expiresAtUnixSecs = "expires_at_unix_secs"
    }
}

@MainActor
final class AppModel: ObservableObject, UpdatePreparationDelegate {
    static let shared = AppModel()

    @Published private(set) var status: ServerStatus = .stopped
    @Published private(set) var airPlayStatus: AirPlayHelperStatus = .missing
    @Published var pairingLink: PairingLink?
    @Published var lastError: String?
    @Published private(set) var launchAtLogin = SMAppService.mainApp.status == .enabled
    @Published private(set) var lanEnabled: Bool
    @Published private(set) var lanAuthenticationRequired: Bool
    @Published private(set) var isImportingWorkspace = false
    @Published private(set) var importStatusText = "Preparing import…"
    @Published private(set) var importError: String?

    let updateManager: UpdateManager

    private let server = ManagedProcess(name: "server")
    private let airPlayHelper = ManagedProcess(name: "airplay-helper")
    private let bonjourPublisher = BonjourPublisher()
    private let networkChangeMonitor = NetworkChangeMonitor()
    private var launcherLock: LauncherLock?
    private var pairingWindow: PairingWindowController?
    private var healthTask: Task<Void, Never>?
    private var expectedShutdown = false
    private var crashDates: [Date] = []
    private var pendingOpenBrowser = false
    private let launcherControlToken = AppModel.makeControlToken()

    private init() {
        let defaults = UserDefaults.standard
        if defaults.object(forKey: "lanEnabled") == nil {
            defaults.set(false, forKey: "lanEnabled")
        }
        lanEnabled = defaults.bool(forKey: "lanEnabled")
        // Existing preferences are preserved. Fresh installs start local-only;
        // users who explicitly enable LAN may also require authentication.
        lanAuthenticationRequired = defaults.bool(forKey: "lanAuthenticationRequired")
        updateManager = UpdateManager()
        updateManager.preparationDelegate = self
    }

    var isSafeToTerminate: Bool { !server.isRunning && !airPlayHelper.isRunning }
    var requiresFirstRunSetup: Bool {
        !FileManager.default.fileExists(atPath: LauncherPaths.dataRoot.appendingPathComponent("install.json").path)
    }
    var canStart: Bool { status == .stopped || isFailed }
    var canStop: Bool { server.isRunning || airPlayHelper.isRunning }
    private var isFailed: Bool {
        if case .failed = status { return true }
        return false
    }

    func claimPrimaryInstance() -> Bool {
        do {
            try LauncherPaths.prepareDirectories(createDataRoot: false)
        } catch {
            lastError = error.localizedDescription
            status = .failed(error.localizedDescription)
            return false
        }
        launcherLock = LauncherLock(
            url: LauncherPaths.dataRoot.deletingLastPathComponent().appendingPathComponent(".Fozmo.launcher.lock")
        )
        return launcherLock != nil
    }

    func start(openBrowser: Bool = false) {
        if status == .running {
            if openBrowser { openFozmo() }
            return
        }
        guard !server.isRunning else { return }

        do {
            try LauncherPaths.prepareDirectories()
            expectedShutdown = false
            pendingOpenBrowser = openBrowser
            lastError = nil
            status = .starting
            do {
                try startAirPlayHelperIfPresent()
            } catch {
                airPlayStatus = .degraded
                lastError = "AirPlay helper could not start: \(error.localizedDescription). Other outputs remain available."
            }
            try startServer()
            beginHealthPolling()
        } catch {
            status = .failed(error.localizedDescription)
            lastError = error.localizedDescription
            stopProcesses(allowForce: true) { _ in }
        }
    }

    func importExistingWorkspace(from source: URL, completion: @escaping (Bool) -> Void) {
        guard !isImportingWorkspace else { return }
        let source = source.standardizedFileURL
        guard source != LauncherPaths.dataRoot.standardizedFileURL else {
            importError = "Choose the old workspace, not Fozmo's new Application Support folder."
            completion(false)
            return
        }

        isImportingWorkspace = true
        importError = nil
        importStatusText = "Checking import support…"
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                guard try self.serverAdvertisesImportCommand() else {
                    throw LauncherError.backupFailed("This server build does not advertise --import-workspace; no files were changed.")
                }
                DispatchQueue.main.async { self.importStatusText = "Importing metadata and history…" }
                try self.runWorkspaceImport(source: source)
                guard FileManager.default.fileExists(
                    atPath: LauncherPaths.dataRoot.appendingPathComponent("install.json").path
                ) else {
                    throw LauncherError.backupFailed("Import finished without creating install.json; the staged data was not activated.")
                }
                DispatchQueue.main.async {
                    self.isImportingWorkspace = false
                    self.importStatusText = "Import complete"
                    completion(true)
                }
            } catch {
                DispatchQueue.main.async {
                    self.isImportingWorkspace = false
                    self.importError = error.localizedDescription
                    completion(false)
                }
            }
        }
    }

    private nonisolated func serverAdvertisesImportCommand() throws -> Bool {
        let process = Process()
        let output = Pipe()
        process.executableURL = LauncherPaths.serverExecutable
        process.arguments = ["--help"]
        process.standardOutput = output
        process.standardError = output
        try process.run()
        process.waitUntilExit()
        let text = String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        return process.terminationStatus == 0 && text.contains("--import-workspace")
    }

    private nonisolated func runWorkspaceImport(source: URL) throws {
        try LauncherPaths.prepareDirectories(createDataRoot: false)
        let process = Process()
        let log = Pipe()
        process.executableURL = LauncherPaths.serverExecutable
        process.arguments = ["--import-workspace", source.path]
        var environment = ProcessInfo.processInfo.environment
        environment["FOZMO_RESOURCE_DIR"] = LauncherPaths.resourceRoot.path
        environment["FOZMO_DATA_DIR"] = LauncherPaths.dataRoot.path
        environment["FOZMO_CACHE_DIR"] = LauncherPaths.cacheRoot.path
        environment["FOZMO_LOG_DIR"] = LauncherPaths.logRoot.path
        process.environment = environment
        process.currentDirectoryURL = LauncherPaths.dataRoot.deletingLastPathComponent()
        process.standardOutput = log
        process.standardError = log
        try process.run()
        let logData = log.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        if process.terminationStatus != 0 {
            let message = String(decoding: logData.suffix(2_000), as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            throw LauncherError.backupFailed(message.isEmpty ? "Importer exited with status \(process.terminationStatus)" : message)
        }
    }

    private func startAirPlayHelperIfPresent() throws {
        let executable = LauncherPaths.airPlayExecutable
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            airPlayStatus = .missing
            return
        }

        try? FileManager.default.removeItem(at: LauncherPaths.airPlayControlSocket)
        try? FileManager.default.removeItem(at: LauncherPaths.airPlayAudioSocket)
        try airPlayHelper.start(
            executable: executable,
            arguments: [
                "serve",
                "--socket", LauncherPaths.airPlayControlSocket.path,
            ],
            environment: childEnvironment(),
            currentDirectory: LauncherPaths.dataRoot,
            logURL: LauncherPaths.logRoot.appendingPathComponent("airplay-helper.log")
        ) { [weak self] exit in
            Task { @MainActor in
                guard let self else { return }
                if !self.expectedShutdown {
                    self.airPlayStatus = .degraded
                    self.lastError = "AirPlay helper exited with status \(exit.status); other outputs remain available."
                }
            }
        }
        airPlayStatus = .ready
    }

    private func startServer() throws {
        var arguments = [
            "--port=3001",
            lanAuthenticationRequired ? "--require-pairing" : "--no-require-pairing",
            "--exit-on-stdin-eof",
            "--no-core-mdns",
        ]
        arguments.append(lanEnabled ? "--lan" : "--local-only")
        try server.start(
            executable: LauncherPaths.serverExecutable,
            arguments: arguments,
            environment: childEnvironment(),
            currentDirectory: LauncherPaths.dataRoot,
            logURL: LauncherPaths.logRoot.appendingPathComponent("server.log")
        ) { [weak self] exit in
            Task { @MainActor in self?.serverExited(exit) }
        }
    }

    private func childEnvironment() -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        // A packaged launch is controlled by this menu app, not by shell
        // variables inherited from Terminal, launchd, or an older Fozmo
        // install. In particular, inherited FOZMO_LAN/PORT/CORE_MDNS values
        // must never override the visible menu state and explicit arguments.
        for key in environment.keys where key.hasPrefix("FOZMO_") || key.hasPrefix("UPSAMPLE_") {
            environment.removeValue(forKey: key)
        }
        environment["FOZMO_RESOURCE_DIR"] = LauncherPaths.resourceRoot.path
        environment["FOZMO_DATA_DIR"] = LauncherPaths.dataRoot.path
        environment["FOZMO_CACHE_DIR"] = LauncherPaths.cacheRoot.path
        environment["FOZMO_LOG_DIR"] = LauncherPaths.logRoot.path
        environment["FOZMO_PARENT_PID"] = String(getpid())
        environment["FOZMO_LAUNCHER_CONTROL_TOKEN"] = launcherControlToken
        environment["FOZMO_MDNS_HOSTNAME"] = LauncherPaths.localHostName
        environment["FOZMO_AIRPLAY_SOCKET"] = LauncherPaths.airPlayControlSocket.path
        if FileManager.default.isExecutableFile(atPath: LauncherPaths.ffmpegExecutable.path) {
            environment["FOZMO_FFMPEG_PATH"] = LauncherPaths.ffmpegExecutable.path
        }
        if let rendererURL = LauncherPaths.lanIPURL {
            environment["FOZMO_PUBLIC_BASE_URL"] = rendererURL.absoluteString
        }
        return environment
    }

    private func beginHealthPolling() {
        healthTask?.cancel()
        healthTask = Task { [weak self] in
            guard let self else { return }
            let deadline = Date().addingTimeInterval(30)
            while !Task.isCancelled && Date() < deadline {
                if !self.server.isRunning {
                    return
                }
                var request = URLRequest(url: LauncherPaths.localURL.appendingPathComponent("healthz"))
                request.timeoutInterval = 1.5
                if let (_, response) = try? await URLSession.shared.data(for: request),
                   let http = response as? HTTPURLResponse,
                   (200 ..< 300).contains(http.statusCode),
                   self.server.isRunning
                {
                    self.status = .running
                    if self.lanEnabled {
                        self.bonjourPublisher.start()
                        self.networkChangeMonitor.start { [weak self] in
                            guard let self, self.lanEnabled, self.status == .running else { return }
                            self.restart()
                        }
                    }
                    if self.pendingOpenBrowser {
                        self.pendingOpenBrowser = false
                        self.openFozmo()
                    }
                    return
                }
                try? await Task.sleep(for: .seconds(1))
            }
            if !Task.isCancelled { self.failStartup(LauncherError.startupTimeout) }
        }
    }

    private func failStartup(_ error: Error) {
        lastError = error.localizedDescription
        stopProcesses(allowForce: true) { [weak self] _ in
            self?.status = .failed(error.localizedDescription)
        }
    }

    private func serverExited(_ exit: ManagedProcess.Exit) {
        healthTask?.cancel()
        guard !expectedShutdown else {
            if !airPlayHelper.isRunning { status = .stopped }
            return
        }

        let now = Date()
        crashDates = crashDates.filter { now.timeIntervalSince($0) < 300 }
        crashDates.append(now)
        if crashDates.count <= 3 {
            status = .failed("Server exited with status \(exit.status); restarting…")
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(2))
                guard let self, !self.expectedShutdown else { return }
                self.start(openBrowser: false)
            }
        } else {
            status = .failed("Server stopped repeatedly. Open the logs for details.")
            lastError = "Automatic restart paused after three crashes in five minutes."
        }
    }

    func stop(completion: ((Bool) -> Void)? = nil) {
        stopProcesses(allowForce: true) { stopped in completion?(stopped) }
    }

    private func stopProcesses(allowForce: Bool, completion: @escaping (Bool) -> Void) {
        expectedShutdown = true
        bonjourPublisher.stop()
        networkChangeMonitor.stop()
        healthTask?.cancel()
        healthTask = nil
        status = .stopping

        server.stop(timeout: 10, allowForce: allowForce) { [weak self] serverStopped in
            guard let self else { completion(false); return }
            self.airPlayHelper.stop(timeout: 10, allowForce: allowForce) { helperStopped in
                let stopped = serverStopped && helperStopped
                if stopped {
                    self.status = .stopped
                    self.airPlayStatus = FileManager.default.isExecutableFile(atPath: LauncherPaths.airPlayExecutable.path) ? .degraded : .missing
                } else {
                    self.status = .failed("A child process did not stop safely.")
                }
                completion(stopped)
            }
        }
    }

    func restart() {
        stopProcesses(allowForce: true) { [weak self] stopped in
            guard stopped else { return }
            self?.start(openBrowser: false)
        }
    }

    func quit() {
        stopProcesses(allowForce: true) { _ in NSApp.terminate(nil) }
    }

    func prepareForSystemTermination(completion: @escaping (Bool) -> Void) {
        stopProcesses(allowForce: true, completion: completion)
    }

    func prepareForUpdate(completion: @escaping (Bool) -> Void) {
        Task {
            do {
                try await BackupManager().createPreUpdateBackup(controlToken: launcherControlToken)
                stopProcesses(allowForce: false, completion: completion)
            } catch {
                lastError = error.localizedDescription
                showError(title: "Update deferred", message: error.localizedDescription)
                completion(false)
            }
        }
    }

    func setLANEnabled(_ enabled: Bool) {
        guard lanEnabled != enabled else { return }
        lanEnabled = enabled
        UserDefaults.standard.set(enabled, forKey: "lanEnabled")
        if server.isRunning { restart() }
    }

    func setLANAuthenticationRequired(_ required: Bool) {
        guard lanAuthenticationRequired != required else { return }
        lanAuthenticationRequired = required
        UserDefaults.standard.set(required, forKey: "lanAuthenticationRequired")
        if server.isRunning { restart() }
    }

    func setLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            launchAtLogin = SMAppService.mainApp.status == .enabled
        } catch {
            launchAtLogin = SMAppService.mainApp.status == .enabled
            lastError = error.localizedDescription
            showError(title: "Launch at Login", message: error.localizedDescription)
        }
    }

    func openFozmo() {
        NSWorkspace.shared.open(LauncherPaths.localURL)
    }

    func openRemoteAccessSettings() {
        NSWorkspace.shared.open(LauncherPaths.remoteAccessURL)
    }

    func copyAddress(_ url: URL) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(url.absoluteString, forType: .string)
    }

    func copyCLIPath() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(LauncherPaths.cliExecutable.path, forType: .string)
    }

    func showDataDirectory() { NSWorkspace.shared.open(LauncherPaths.dataRoot) }
    func showLogDirectory() { NSWorkspace.shared.open(LauncherPaths.logRoot) }

    func requestPairingLink(completion: @escaping (Bool) -> Void) {
        Task {
            do {
                let localToken = try await issuePairingToken()
                let local = pairingURL(base: LauncherPaths.lanURL, token: localToken.token)
                var fallback: URL?
                var expiry = localToken.expiresAtUnixSecs
                if let ipURL = LauncherPaths.lanIPURL {
                    let fallbackToken = try await issuePairingToken()
                    fallback = pairingURL(base: ipURL, token: fallbackToken.token)
                    expiry = min(expiry, fallbackToken.expiresAtUnixSecs)
                }
                pairingLink = PairingLink(
                    localNetworkURL: local,
                    ipFallbackURL: fallback,
                    expiresAt: Date(timeIntervalSince1970: TimeInterval(expiry))
                )
                completion(true)
            } catch {
                lastError = error.localizedDescription
                showError(title: "Could not create pairing link", message: error.localizedDescription)
                completion(false)
            }
        }
    }

    func showPairingLink() {
        pairingLink = nil
        requestPairingLink { [weak self] succeeded in
            guard succeeded, let self else { return }
            let controller = PairingWindowController(model: self)
            pairingWindow = controller
            NSApp.activate(ignoringOtherApps: true)
            controller.showWindow(nil)
        }
    }

    private func issuePairingToken() async throws -> PairingStartResponse {
        var request = URLRequest(url: LauncherPaths.localURL.appendingPathComponent("api/pairing/start"))
        request.httpMethod = "POST"
        request.timeoutInterval = 5
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw URLError(.badServerResponse)
        }
        return try JSONDecoder().decode(PairingStartResponse.self, from: data)
    }

    private func pairingURL(base: URL, token: String) -> URL {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        let encoded = token.addingPercentEncoding(withAllowedCharacters: allowed) ?? token
        return URL(string: "\(base.absoluteString)/#/pair/\(encoded)")!
    }

    private static func makeControlToken() -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        precondition(status == errSecSuccess, "secure random generator unavailable")
        return Data(bytes)
            .base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    private func showError(title: String, message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }
}
