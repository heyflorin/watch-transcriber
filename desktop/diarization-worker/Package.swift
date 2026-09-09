// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "EchoWallDiarizationWorker",
    platforms: [.macOS(.v14)],
    products: [
        .executable(
            name: "echowall-diarization-worker",
            targets: ["EchoWallDiarizationWorker"]
        )
    ],
    targets: [
        .target(
            name: "FastClusterWrapper",
            path: "Sources/FastClusterWrapper",
            publicHeadersPath: "include"
        ),
        .target(
            name: "FluidAudioOffline",
            dependencies: ["FastClusterWrapper"],
            path: "Sources/FluidAudioOffline"
        ),
        .target(
            name: "SpeakerKitOffline",
            path: "Sources/SpeakerKitOffline"
        ),
        .executableTarget(
            name: "EchoWallDiarizationWorker",
            dependencies: ["FluidAudioOffline", "SpeakerKitOffline"],
            // @main owns the async entrypoint even with multiple source files.
            swiftSettings: [.unsafeFlags(["-parse-as-library"])]
        ),
        .testTarget(
            name: "FluidAudioOfflineTests",
            dependencies: ["FluidAudioOffline"]
        ),
        .testTarget(
            name: "SpeakerCentroidDiagnosticTests",
            dependencies: ["FluidAudioOffline", "SpeakerKitOffline"]
        ),
        .testTarget(
            name: "SpeakerKitOfflineTests",
            dependencies: ["SpeakerKitOffline", "EchoWallDiarizationWorker"]
        )
    ],
    cxxLanguageStandard: .cxx17
)
