// swift-tools-version:5.9
import PackageDescription

// The Rust core is linked in, not vendored: `scripts/build-swift-bindings.sh`
// regenerates Generated/ and `scripts/build-mac-app.sh` passes the library
// search path, then assembles the .app bundle around the built executable.
let package = Package(
    name: "Setbuddy",
    platforms: [.macOS(.v14)],
    targets: [
        // The C shim for the UniFFI scaffolding.
        .systemLibrary(name: "setbuddyFFI", path: "Sources/setbuddyFFI"),

        // The generated Swift API. Lives outside Sources/ because it is a build
        // artifact, so the target points at it explicitly.
        .target(
            name: "SetbuddyCore",
            dependencies: ["setbuddyFFI"],
            path: "Generated",
            // The dylib is copied here for the bundler; it is not a build input.
            exclude: ["libsetbuddy_ffi.dylib"],
            sources: ["setbuddy.swift"]
        ),

        // The AVFoundation engine: a PlaybackEngine written in Swift, handed
        // to the Rust core. Its own target so it depends on the bindings and
        // nothing else — the views never touch an engine.
        .target(
            name: "SetbuddyAV",
            dependencies: ["SetbuddyCore"],
            path: "Sources/SetbuddyAV"
        ),

        // The app's model and views. A library rather than part of the
        // executable so it can be driven headlessly by tests — an executable
        // target cannot be imported.
        .target(
            name: "SetbuddyUI",
            dependencies: ["SetbuddyCore", "SetbuddyAV"],
            path: "Sources/SetbuddyUI"
        ),

        .executableTarget(
            name: "Setbuddy",
            dependencies: ["SetbuddyUI"],
            path: "Sources/Setbuddy",
            linkerSettings: [.linkedLibrary("setbuddy_ffi")]
        ),

        .testTarget(
            name: "SetbuddyUITests",
            dependencies: ["SetbuddyUI", "SetbuddyAV"],
            path: "Tests/SetbuddyUITests",
            linkerSettings: [.linkedLibrary("setbuddy_ffi")]
        ),
    ]
)
