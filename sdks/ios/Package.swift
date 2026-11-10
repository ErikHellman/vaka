// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Vaka",
    platforms: [.iOS(.v15), .macOS(.v12), .tvOS(.v15)],
    products: [
        .library(name: "Vaka", targets: ["Vaka"]),
    ],
    targets: [
        // Async-signal-safe crash capture, loaded-image tracking and gzip, in C.
        .target(
            name: "VakaNative",
            linkerSettings: [.linkedLibrary("z")]
        ),
        .target(name: "Vaka", dependencies: ["VakaNative"]),
        .testTarget(
            name: "VakaTests",
            dependencies: ["Vaka", "VakaNative"],
            // So dladdr can name functions in the test binary on Linux (Apple exports them already).
            linkerSettings: [.unsafeFlags(["-Xlinker", "--export-dynamic", "-Xlinker", "--build-id"], .when(platforms: [.linux]))]
        ),
    ]
)
