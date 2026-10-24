// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxCore",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxCore", targets: ["AmuxCore"]),
        // The values the Rust bridge hands over, generated from their Rust
        // definitions by `cargo run -p xtask -- swift-types`.
        .library(name: "AmuxValues", targets: ["AmuxValues"]),
    ],
    targets: [
        // Assembled by `just ios rust` from the Rust bridge; the recipes that
        // build or test this package produce it first.
        .binaryTarget(name: "AmuxApp", path: "../../../../target/ios/AmuxApp.xcframework"),
        // The only code in this app that runs before `main()`. It exists
        // in C because nothing written in Swift can: see LaunchClock.h.
        .target(name: "LaunchClock"),
        .target(name: "AmuxValues"),
        .target(name: "AmuxCore", dependencies: ["AmuxApp", "AmuxValues", "LaunchClock"]),
        .testTarget(name: "AmuxCoreTests", dependencies: ["AmuxCore"]),
    ],
    swiftLanguageModes: [.v6]
)
