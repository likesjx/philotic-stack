// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "PhiloticKit",
    platforms: [
        .macOS(.v14),
        .iOS(.v17),
    ],
    products: [
        .library(
            name: "PhiloticKit",
            targets: ["PhiloticKit"]
        )
    ],
    targets: [
        .target(
            name: "PhiloticKit",
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        .testTarget(
            name: "PhiloticKitTests",
            dependencies: ["PhiloticKit"],
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
    ]
)
