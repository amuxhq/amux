// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxFeatures",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxFeatures", targets: ["AmuxFeatures"]),
    ],
    dependencies: [
        .package(path: "../AmuxCore"),
        .package(path: "../AmuxDesign"),
        .package(path: "../AmuxPush"),
    ],
    targets: [
        .target(
            name: "AmuxFeatures",
            dependencies: [
                "AmuxDesign", .product(name: "AmuxCore", package: "AmuxCore"),
                .product(name: "AmuxWords", package: "AmuxCore"),
            ]),
        .testTarget(
            name: "AmuxFeaturesTests",
            dependencies: [
                "AmuxFeatures", .product(name: "AmuxPush", package: "AmuxPush"),
                .product(name: "AmuxFixtures", package: "AmuxCore"),
            ]),
    ],
    swiftLanguageModes: [.v6]
)
