// Development-only, public-corpus acoustic diagnostics. This test target is
// not a product, worker protocol extension, App dependency or release resource.
// Raw embedding vectors stay in memory; no text/reference enters inference.
import CryptoKit
import Darwin
import FluidAudioOffline
import Foundation
import SpeakerKitOffline
import XCTest

private enum DiagnosticError: Error { case rejected }
private let manifestHash = "c6615a07f8e80fbe75d3da65b75898423a716749750a1033bca0db70b4506280"
private let auditHash = "4cbf15eab57e221049c8d5dd777dcbea5eb7c461b327484c2ea5c4977266ae0a"

private struct Pair: Codable, Equatable {
  let leftSlot: Int
  let rightSlot: Int
  let cosineDistance: Float
}

private func pairs(result: SpeakerKitOffline.DiarizationResult, slots: [Int: Int]) throws -> [Pair] {
  guard !slots.isEmpty, slots.count <= 16, Set(slots.values).count == slots.count else {
    throw DiagnosticError.rejected
  }
  for speaker in slots.keys {
    guard let vector = result.speakerCentroidEmbeddings[speaker], !vector.isEmpty,
      vector.count <= 4096, vector.allSatisfy({ $0.isFinite }),
      vector.contains(where: { $0 != 0 })
    else { throw DiagnosticError.rejected }
  }
  let speakers = slots.keys.sorted { slots[$0]! < slots[$1]! }
  var output: [Pair] = []
  for (offset, left) in speakers.enumerated() {
    for right in speakers.dropFirst(offset + 1) {
      guard let distance = result.centroidCosineDistance(between: left, and: right),
        distance.isFinite, (0...2).contains(distance)
      else { throw DiagnosticError.rejected }
      output.append(Pair(leftSlot: slots[left]!, rightSlot: slots[right]!, cosineDistance: distance))
    }
  }
  return output
}

private func safeFile(root: URL, relative: String, maximum: UInt64) throws -> URL {
  let parts = relative.split(separator: "/", omittingEmptySubsequences: false)
  guard !parts.isEmpty, !relative.contains("\\"),
    parts.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." })
  else { throw DiagnosticError.rejected }
  var file = root
  for part in parts {
    file.appendPathComponent(String(part))
    let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
    guard attributes[.type] as? FileAttributeType != .typeSymbolicLink else {
      throw DiagnosticError.rejected
    }
  }
  let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
  guard file.resolvingSymlinksInPath().path == file.path,
    attributes[.type] as? FileAttributeType == .typeRegular,
    let size = attributes[.size] as? NSNumber, size.uint64Value <= maximum
  else { throw DiagnosticError.rejected }
  return file
}

private func digest(_ file: URL) throws -> String {
  let handle = try FileHandle(forReadingFrom: file)
  defer { try? handle.close() }
  var hash = SHA256()
  while let bytes = try handle.read(upToCount: 1024 * 1024), !bytes.isEmpty { hash.update(data: bytes) }
  return hash.finalize().map { String(format: "%02x", $0) }.joined()
}

private func readJSON(_ file: URL) throws -> [String: Any] {
  guard let value = try JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any] else {
    throw DiagnosticError.rejected
  }
  return value
}

private func publish(_ value: [String: Any], to file: URL) throws {
  let bytes = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys, .prettyPrinted])
  guard bytes.count <= 4 * 1024 * 1024 else { throw DiagnosticError.rejected }
  let temporary = file.deletingLastPathComponent().appendingPathComponent(".pending-\(UUID().uuidString)")
  let fd = open(temporary.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
  guard fd >= 0 else { throw DiagnosticError.rejected }
  defer { close(fd); unlink(temporary.path) }
  try bytes.withUnsafeBytes { buffer in
    var offset = 0
    while offset < buffer.count {
      let count = write(fd, buffer.baseAddress!.advanced(by: offset), buffer.count - offset)
      guard count > 0 else { throw DiagnosticError.rejected }
      offset += count
    }
  }
  guard fsync(fd) == 0, link(temporary.path, file.path) == 0 else { throw DiagnosticError.rejected }
  let directory = open(file.deletingLastPathComponent().path, O_RDONLY)
  guard directory >= 0 else { throw DiagnosticError.rejected }
  defer { close(directory) }
  guard fsync(directory) == 0 else { throw DiagnosticError.rejected }
}

private func requireNetworkDenied() throws {
  let fd = socket(AF_INET, SOCK_STREAM, 0)
  guard fd >= 0 else { throw DiagnosticError.rejected }
  defer { close(fd) }
  var address = sockaddr_in()
  address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
  address.sin_family = sa_family_t(AF_INET)
  address.sin_port = UInt16(9).bigEndian
  address.sin_addr.s_addr = inet_addr("127.0.0.1")
  let status = withUnsafePointer(to: &address) { pointer in
    pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
      connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
    }
  }
  let code = errno
  guard status == -1, code == EPERM || code == EACCES else { throw DiagnosticError.rejected }
}

private func runPublicDiagnostic() async throws {
  try requireNetworkDenied()
  var repo = URL(fileURLWithPath: #filePath)
  for _ in 0..<5 { repo.deleteLastPathComponent() }
  repo = repo.resolvingSymlinksInPath()
  let matrix = repo.appendingPathComponent("local-eval/matrix")
  let manifest = try safeFile(root: matrix, relative: "manifest.json", maximum: 1024 * 1024)
  let audit = try safeFile(root: matrix, relative: "outputs/source-duration-audit-v1.json", maximum: 512 * 1024)
  guard try digest(manifest) == manifestHash, try digest(audit) == auditHash,
    let cases = try readJSON(manifest)["cases"] as? [[String: Any]], cases.count == 44,
    let sourceRows = try readJSON(audit)["cases"] as? [[String: Any]], sourceRows.count == 44
  else { throw DiagnosticError.rejected }
  let catalogFile = try safeFile(root: repo, relative: "desktop/model-catalog/speakerkit-candidate-v1.json", maximum: 256 * 1024)
  let catalog = try readJSON(catalogFile)
  guard catalog["pack_id"] as? String == "speakerkit-v1",
    catalog["model_revision"] as? String == "86ec9c929b52208b6656eb6a6361ed0d822a1f78",
    let files = catalog["files"] as? [[String: Any]], files.count == 29
  else { throw DiagnosticError.rejected }
  let modelRoot = repo.appendingPathComponent("local-eval/speakerkit-worker-root")
  for file in files {
    guard let relative = file["install_path"] as? String,
      let hash = file["sha256"] as? String, let size = file["size_bytes"] as? NSNumber
    else { throw DiagnosticError.rejected }
    let model = try safeFile(root: modelRoot, relative: relative, maximum: size.uint64Value)
    guard try digest(model) == hash else { throw DiagnosticError.rejected }
  }
  let pack = modelRoot.appendingPathComponent("models/diarization/speakerkit-v1")
  let parent = matrix.appendingPathComponent("outputs/speaker-centroid-diagnostic-v1")
  if !FileManager.default.fileExists(atPath: parent.path) {
    try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
  }
  guard parent.resolvingSymlinksInPath().path == parent.path else { throw DiagnosticError.rejected }
  let output = parent.appendingPathComponent("run-\(UUID().uuidString.lowercased())")
  try FileManager.default.createDirectory(at: output, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
  try publish(["scope": "public-acoustic-centroid-diagnostic-not-runtime-merge-rule",
    "manifest_sha256": manifestHash, "audit_sha256": auditHash,
    "catalog_sha256": try digest(catalogFile), "cluster_distance_threshold": 0.6,
    "centroid_source": "finalAssignment", "full_redundancy": true, "number_of_speakers_hint": NSNull(),
    "raw_vectors_saved": false, "reference_used_for_inference": false, "network": "OS-denied",
    "source_sha256": try digest(URL(fileURLWithPath: #filePath)), "planned_cases": 44], to: output.appendingPathComponent("started.json"))

  for (ordinal, entry) in cases.enumerated() {
    guard let id = entry["case_id"] as? String,
      id.count <= 32, id.utf8.allSatisfy({ (48...57).contains($0) || (97...122).contains($0) || $0 == 95 }),
      let row = sourceRows.first(where: { $0["case_id"] as? String == id }),
      let audio = row["aac"] as? [String: Any], let sourceHash = audio["sha256"] as? String,
      let sourceSize = audio["size_bytes"] as? NSNumber,
      let duration = row["effective_duration_ms_ceil"] as? NSNumber
    else { throw DiagnosticError.rejected }
    let file = try safeFile(root: matrix, relative: "audio/\(id).m4a", maximum: sourceSize.uint64Value)
    guard try digest(file) == sourceHash else { throw DiagnosticError.rejected }
    let source = try AudioSourceFactory().makeDiskBackedSource(from: file, targetSampleRate: 16_000).source
    defer { source.cleanup() }
    guard source.sampleCount > 0, source.sampleCount < 18_000 * 16_000 else { throw DiagnosticError.rejected }
    var samples = [Float](repeating: 0, count: source.sampleCount)
    try samples.withUnsafeMutableBufferPointer { buffer in
      try source.copySamples(into: buffer.baseAddress!, offset: 0, count: buffer.count)
    }
    let kit = try await SpeakerKit(PyannoteConfig(modelFolder: pack.path, download: false, load: true,
      verbose: false, logLevel: .none, fullRedundancy: true))
    let result = try await kit.diarize(audioArray: samples, options: PyannoteDiarizationOptions(
      numberOfSpeakers: nil, clusterDistanceThreshold: 0.6, useExclusiveReconciliation: true))
    var slots: [Int: Int] = [:]
    var segments: [[String: Any]] = []
    var previousEnd: UInt64 = 0
    for segment in result.segments.sorted(by: { $0.startTime == $1.startTime ? $0.endTime < $1.endTime : $0.startTime < $1.startTime }) {
      guard let rawID = segment.speaker.speakerId, rawID >= 0,
        segment.startTime.isFinite, segment.endTime.isFinite, segment.qualityScore.isFinite,
        segment.startTime >= 0, segment.endTime > segment.startTime
      else { throw DiagnosticError.rejected }
      let start = UInt64((Double(segment.startTime) * 1000).rounded())
      let end = min(UInt64((Double(segment.endTime) * 1000).rounded()), duration.uint64Value)
      guard start >= previousEnd, end > start, segments.count < 20_000 else { throw DiagnosticError.rejected }
      if slots[rawID] == nil { slots[rawID] = slots.count + 1 }
      segments.append(["start_ms": start, "end_ms": end, "speaker_slot": slots[rawID]!,
        "confidence_milli": Int((Double(min(max(segment.qualityScore, 0), 1)) * 1000).rounded())])
      previousEnd = end
    }
    guard !segments.isEmpty, try digest(file) == sourceHash else { throw DiagnosticError.rejected }
    let pairRows = try pairs(result: result, slots: slots)
    let pairJSON = pairRows.map { ["left_slot": $0.leftSlot, "right_slot": $0.rightSlot,
      "cosine_distance": $0.cosineDistance] as [String: Any] }
    try publish(["case_id": id, "audio_sha256": sourceHash, "audio_duration_ms": duration,
      "decoded_samples": source.sampleCount, "speaker_count": slots.count, "segments": segments,
      "pairs": pairJSON, "raw_vectors_saved": false, "reference_used_for_inference": false],
      to: output.appendingPathComponent("\(id).json"))
    print("{\"centroid_diagnostic_completed_cases\":\(ordinal + 1)}")
  }
  for file in files {
    guard let relative = file["install_path"] as? String,
      let expectedHash = file["sha256"] as? String, let size = file["size_bytes"] as? NSNumber,
      try digest(safeFile(root: modelRoot, relative: relative, maximum: size.uint64Value)) == expectedHash
    else { throw DiagnosticError.rejected }
  }
  guard try digest(manifest) == manifestHash, try digest(audit) == auditHash else { throw DiagnosticError.rejected }
  try publish(["state": "complete", "cases": 44, "raw_vectors_saved": false,
    "scope": "diagnostic-only-not-a-merge-policy-or-quality-verdict"], to: output.appendingPathComponent("completed.json"))
}

// XCTest creates separate instances per method; no mutable test-instance state.
final class SpeakerCentroidDiagnosticTests: XCTestCase, @unchecked Sendable {
  func testPairsAreBoundedAnonymousAndContainNoRawVectors() throws {
    let result = SpeakerKitOffline.DiarizationResult(speakerCount: 3, totalFrames: 0, frameRate: 100, segments: [],
      speakerCentroidEmbeddings: [9: [1, 0], 2: [2, 0], 7: [0, 1]])
    let actual = try pairs(result: result, slots: [9: 1, 2: 2, 7: 3])
    XCTAssertEqual(actual, [Pair(leftSlot: 1, rightSlot: 2, cosineDistance: 0),
      Pair(leftSlot: 1, rightSlot: 3, cosineDistance: 1), Pair(leftSlot: 2, rightSlot: 3, cosineDistance: 1)])
    let encoded = try JSONEncoder().encode(actual)
    XCTAssertFalse(String(decoding: encoded, as: UTF8.self).contains("embedding"))
  }

  func testMissingIncompatibleOrNonfiniteCentroidsFailClosed() {
    for vectors: [Int: [Float]] in [[1: [1]], [1: [1], 2: [1, 0]], [1: [Float.nan], 2: [1]], [1: [0], 2: [1]]] {
      let result = SpeakerKitOffline.DiarizationResult(speakerCount: 2, totalFrames: 0, frameRate: 100, segments: [], speakerCentroidEmbeddings: vectors)
      XCTAssertThrowsError(try pairs(result: result, slots: [1: 1, 2: 2]))
    }
  }

  func testPublic44CentroidDiagnostic() async throws {
    guard ProcessInfo.processInfo.environment["ECHOWALL_SPEAKER_CENTROID_CONFIRM"] == "public-corpus-centroid-diagnostic-authorized" else {
      throw XCTSkip("explicit public-corpus and OS-network-deny confirmation required")
    }
    do { try await runPublicDiagnostic() }
    catch { XCTFail("public centroid diagnostic failed; retained files are diagnostic evidence only") }
  }
}
