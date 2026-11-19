// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxDesign",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxDesign", targets: ["AmuxDesign"]),
    ],
    targets: [
        .target(
            name: "AmuxDesign",
            resources: [.copy("Resources/Fonts")]
        ),
        .testTarget(
            name: "AmuxDesignTests",
            dependencies: ["AmuxDesign"],
            resources: [.copy("TokenTable.txt")]
        ),
    ],
    swiftLanguageModes: [.v6]
)
