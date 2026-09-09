// swift-tools-version:5.9
import PackageDescription

let package = Package(
  name: "tauri-plugin-echowall-capture",
  platforms: [.iOS(.v14)],
  products: [
    .library(
      name: "tauri-plugin-echowall-capture",
      type: .static,
      targets: ["tauri-plugin-echowall-capture"]
    )
  ],
  dependencies: [
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "tauri-plugin-echowall-capture",
      dependencies: [.byName(name: "Tauri")],
      path: "Sources"
    )
  ]
)
