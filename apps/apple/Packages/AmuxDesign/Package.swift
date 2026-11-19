// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxDesign",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxDesign", targets: ["AmuxDesign"]),
    ],
    dependencies: [
        // For the kinds of token code is coloured by, which the design
        // gives each a colour.
        .package(path: "../AmuxCore"),
    ],
    targets: [
        .target(
            name: "AmuxDesign",
            dependencies: [.product(name: "AmuxValues", package: "AmuxCore")],
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
