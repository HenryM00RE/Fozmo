import Foundation
import Sparkle

@MainActor
protocol UpdatePreparationDelegate: AnyObject {
    func prepareForUpdate(completion: @escaping (Bool) -> Void)
}

@MainActor
final class UpdateManager: NSObject, ObservableObject, SPUUpdaterDelegate {
    @Published private(set) var updateAvailable = false
    @Published private(set) var availableVersion: String?
    @Published private(set) var statusText = "Updates unavailable in this build"

    weak var preparationDelegate: UpdatePreparationDelegate?

    let isEnabled: Bool
    private var updaterController: SPUStandardUpdaterController?

    override init() {
        isEnabled = Bundle.main.object(forInfoDictionaryKey: "FozmoUpdatesEnabled") as? Bool == true
        super.init()

        guard isEnabled else { return }
        let controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: self,
            userDriverDelegate: nil
        )
        updaterController = controller
        controller.updater.automaticallyDownloadsUpdates = false
        statusText = "Automatic checks enabled"
    }

    func checkForUpdates() {
        guard let updaterController else { return }
        statusText = "Checking for updates…"
        updaterController.checkForUpdates(nil)
    }

    func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        updateAvailable = true
        availableVersion = item.displayVersionString
        statusText = "Version \(item.displayVersionString) is available"
    }

    func updaterDidNotFindUpdate(_ updater: SPUUpdater, error: Error) {
        updateAvailable = false
        availableVersion = nil
        statusText = "Fozmo is up to date"
    }

    func updater(
        _ updater: SPUUpdater,
        shouldPostponeRelaunchForUpdate item: SUAppcastItem,
        untilInvokingBlock installHandler: @escaping () -> Void
    ) -> Bool {
        guard let preparationDelegate else { return false }
        statusText = "Backing up data before update…"
        preparationDelegate.prepareForUpdate { [weak self] ready in
            Task { @MainActor in
                if ready {
                    self?.statusText = "Installing version \(item.displayVersionString)…"
                    installHandler()
                } else {
                    self?.statusText = "Update deferred; data was not changed"
                }
            }
        }
        return true
    }
}
