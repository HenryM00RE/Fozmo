import Foundation

struct BackupManager {
    private struct BackupResponse: Decodable {
        let status: String?
    }

    func createPreUpdateBackup(controlToken: String) async throws {
        var request = URLRequest(
            url: URL(string: "http://127.0.0.1:3001/internal/launcher/backup")!
        )
        request.httpMethod = "POST"
        request.timeoutInterval = 60
        request.setValue(controlToken, forHTTPHeaderField: "x-fozmo-launcher-token")
        request.setValue("application/json", forHTTPHeaderField: "accept")

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse,
              (200 ..< 300).contains(http.statusCode)
        else {
            let code = (response as? HTTPURLResponse)?.statusCode ?? 0
            throw LauncherError.backupFailed("launcher backup endpoint returned HTTP \(code)")
        }

        // The path is intentionally not required or displayed. Require the
        // authenticated Fozmo endpoint's exact success body so a foreign
        // loopback service (or a future 200/error regression) cannot be
        // mistaken for a completed backup.
        guard !data.isEmpty else {
            throw LauncherError.backupFailed("launcher backup endpoint returned an empty success response")
        }
        let payload = try JSONDecoder().decode(BackupResponse.self, from: data)
        guard payload.status == "ok" else {
            throw LauncherError.backupFailed("launcher backup endpoint did not confirm a verified backup")
        }
    }
}
