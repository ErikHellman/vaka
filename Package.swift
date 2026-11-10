// swift-tools-version:5.9
// Root manifest so apps can depend on the iOS SDK by repository URL:
//   .package(url: "https://github.com/ErikHellman/vaka", from: "0.1.0")
// The SDK lives in sdks/ios (which has its own Package.swift for development and tests).
import PackageDescription

let package = Package(
    name: "Vaka",
    platforms: [.iOS(.v15), .macOS(.v12), .tvOS(.v15)],
    products: [
        .library(name: "Vaka", targets: ["Vaka"]),
    ],
    targets: [
        .target(
            name: "VakaNative",
            path: "sdks/ios/Sources/VakaNative",
            linkerSettings: [.linkedLibrary("z")]
        ),
        .target(name: "Vaka", dependencies: ["VakaNative"], path: "sdks/ios/Sources/Vaka"),
    ]
)
