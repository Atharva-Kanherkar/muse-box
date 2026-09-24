// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "MuseBox",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "muse-box", targets: ["MuseBox"]),
    ],
    targets: [
        // Pure logic: palette, dithering, idle clock, lyrics, beat tracking.
        // Everything here is a port of (or a peer to) the backend and is unit tested.
        .target(name: "MuseBoxCore"),
        .executableTarget(
            name: "MuseBox",
            dependencies: ["MuseBoxCore"],
            linkerSettings: [
                .linkedFramework("ScriptingBridge"),
                .linkedFramework("CoreAudio"),
                .linkedFramework("ServiceManagement"),
            ]
        ),
        .testTarget(name: "MuseBoxCoreTests", dependencies: ["MuseBoxCore"]),
    ],
    // AppKit, Core Audio and ScriptingBridge predate strict concurrency; the
    // threading here is explicit (one queue per subsystem) instead.
    swiftLanguageModes: [.v5]
)
