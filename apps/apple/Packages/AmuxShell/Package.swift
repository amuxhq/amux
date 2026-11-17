// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxShell",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxShell", targets: ["AmuxShell"]),
    ],
    dependencies: [
        .package(path: "../AmuxCore"),
        .package(path: "../AmuxDesign"),
        .package(path: "../AmuxFeatures"),
    ],
    targets: [
        // The shell is the only place that navigates. Screens are functions of
        // state and say what happened; where that leads is decided here.
        .target(
            name: "AmuxShell",
            dependencies: [
                "AmuxCore", "AmuxDesign", "AmuxFeatures", .product(name: "AmuxWords", package: "AmuxCore"),
            ]),
        .testTarget(name: "AmuxShellTests", dependencies: ["AmuxShell"]),
    ],
    swiftLanguageModes: [.v6]
)
