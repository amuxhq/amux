// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxPush",
    platforms: [.iOS(.v26), .macOS(.v26)],
    products: [
        .library(name: "AmuxPush", targets: ["AmuxPush"]),
    ],
    targets: [
        // What opening a push takes and nothing else. The notification
        // extension links this, and it has a few megabytes to live in, so
        // nothing here may reach the Rust bridge or the app's other packages.
        .target(name: "AmuxPush"),
        .testTarget(
            name: "AmuxPushTests", dependencies: ["AmuxPush"],
            resources: [.copy("Fixtures")]),
    ],
    swiftLanguageModes: [.v6]
)
