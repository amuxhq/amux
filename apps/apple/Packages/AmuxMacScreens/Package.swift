// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxMacScreens",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "AmuxMacScreens", targets: ["AmuxMacScreens"]),
        .library(name: "AmuxMacStories", targets: ["AmuxMacStories"]),
    ],
    dependencies: [
        .package(path: "../AmuxCore"),
    ],
    targets: [
        // The Mac app's screens, drawn from values and reporting through
        // actions. Only the values: nothing here may reach `AmuxCore`, whose
        // Rust bridge would make every storybook build link it.
        .target(
            name: "AmuxMacScreens",
            dependencies: [.product(name: "AmuxValues", package: "AmuxCore")]),
        // The named screens the storybook draws. The app does not link this.
        .target(
            name: "AmuxMacStories",
            dependencies: ["AmuxMacScreens", .product(name: "AmuxValues", package: "AmuxCore")]),
    ],
    swiftLanguageModes: [.v6]
)
