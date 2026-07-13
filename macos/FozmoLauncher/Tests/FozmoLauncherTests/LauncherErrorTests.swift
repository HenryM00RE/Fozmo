@testable import FozmoLauncher
import XCTest

final class LauncherErrorTests: XCTestCase {
    func testMissingExecutableDescriptionIncludesThePath() {
        let error = LauncherError.missingExecutable("/missing/fozmo-server")

        XCTAssertEqual(
            error.errorDescription,
            "Required executable is missing or not executable: /missing/fozmo-server"
        )
    }

    func testBackupFailureDescriptionRetainsTheActionableCause() {
        let error = LauncherError.backupFailed("endpoint returned HTTP 503")

        XCTAssertEqual(
            error.errorDescription,
            "The pre-update backup failed: endpoint returned HTTP 503"
        )
    }
}
