// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "LocalFleet",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "LocalFleet",
            path: "Sources/LocalFleet"
        ),
        .testTarget(
            name: "LocalFleetTests",
            dependencies: ["LocalFleet"],
            path: "Tests/LocalFleetTests"
        )
    ]
)
