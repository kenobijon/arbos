// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ArbosFFI",
    platforms: [.iOS(.v17)],
    products: [
        .library(name: "ArbosFFI", type: .dynamic, targets: ["ArbosFFI"]),
    ],
    targets: [
        .binaryTarget(name: "ArbosFFIRust", path: "ArbosFFIRust.xcframework"),
        .target(
            name: "ArbosFFI",
            dependencies: ["ArbosFFIRust"],
            path: "Sources/ArbosFFI"
        ),
    ]
)
