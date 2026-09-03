// swift-tools-version:5.9
import PackageDescription

// The Rust core is linked in, not vendored: `scripts/build-swift-bindings.sh`
// regenerates Generated/ and `scripts/build-mac-app.sh` passes the library
// search path, then assembles the .app bundle around the built executable.
let package = Package(
    name: "Setwave",
    platforms: [.macOS(.v14)],
    targets: [
        // The C shim for the UniFFI scaffolding.
        .systemLibrary(name: "setwaveFFI", path: "Sources/setwaveFFI"),

        // The generated Swift API. Lives outside Sources/ because it is a build
        // artifact, so the target points at it explicitly.
        .target(
            name: "SetwaveCore",
            dependencies: ["setwaveFFI"],
            path: "Generated",
            // The dylib is copied here for the bundler; it is not a build input.
            exclude: ["libsetwave_ffi.dylib"],
            sources: ["setwave.swift"]
        ),

        // The app's model and views. A library rather than part of the
        // executable so it can be driven headlessly by tests — an executable
        // target cannot be imported.
        .target(
            name: "SetwaveUI",
            dependencies: ["SetwaveCore"],
            path: "Sources/SetwaveUI"
        ),

        .executableTarget(
            name: "Setwave",
            dependencies: ["SetwaveUI"],
            path: "Sources/Setwave",
            linkerSettings: [.linkedLibrary("setwave_ffi")]
        ),

        .testTarget(
            name: "SetwaveUITests",
            dependencies: ["SetwaveUI"],
            path: "Tests/SetwaveUITests",
            linkerSettings: [.linkedLibrary("setwave_ffi")]
        ),
    ]
)
