// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "FozmoAppleMusicHelper",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(name: "FozmoAppleMusicHelper", targets: ["FozmoAppleMusicHelper"]),
    ],
    targets: [
        .executableTarget(name: "FozmoAppleMusicHelper"),
        .testTarget(
            name: "FozmoAppleMusicHelperTests",
            dependencies: ["FozmoAppleMusicHelper"]
        ),
    ],
    swiftLanguageModes: [.v5]
)
