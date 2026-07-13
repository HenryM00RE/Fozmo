// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "FozmoLauncher",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(name: "FozmoLauncher", targets: ["FozmoLauncher"]),
    ],
    dependencies: [
        .package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.9.4"),
    ],
    targets: [
        .executableTarget(
            name: "FozmoLauncher",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle"),
            ],
            swiftSettings: [
                .define("FOZMO_LAUNCHER"),
            ]
        ),
        .testTarget(
            name: "FozmoLauncherTests",
            dependencies: ["FozmoLauncher"]
        ),
    ],
    swiftLanguageModes: [.v5]
)
