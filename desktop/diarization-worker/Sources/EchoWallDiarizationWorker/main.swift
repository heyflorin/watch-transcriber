import CoreML
import CryptoKit
import Darwin
import FluidAudioOffline
import Foundation
import SpeakerKitOffline

private let appDataRootEnvironment = "ECHOWALL_APP_DATA_ROOT"
private let protocolVersion = 2
private let legacyQualityPreset = "fluid-community-v1"
private let selectedQualityPreset = "fluid-step015-embed040-v1"
private let maximumRequestBytes = 256 * 1024
private let maximumResponseBytes = 4 * 1024 * 1024
private let maximumModelFiles = 64
private let maximumModelBytes: UInt64 = 1024 * 1024 * 1024
private let maximumAudioBytes: UInt64 = 512 * 1024 * 1024
private let maximumDurationMilliseconds: UInt64 = 5 * 60 * 60 * 1000
private let maximumSegments = 20_000
private let maximumSpeakers: UInt32 = 16

private let requiredModelFiles: Set<String> = [
  "speaker-diarization/Embedding.mlmodelc/analytics/coremldata.bin",
  "speaker-diarization/Embedding.mlmodelc/coremldata.bin",
  "speaker-diarization/Embedding.mlmodelc/metadata.json",
  "speaker-diarization/Embedding.mlmodelc/model.mil",
  "speaker-diarization/Embedding.mlmodelc/weights/weight.bin",
  "speaker-diarization/FBank.mlmodelc/analytics/coremldata.bin",
  "speaker-diarization/FBank.mlmodelc/coremldata.bin",
  "speaker-diarization/FBank.mlmodelc/metadata.json",
  "speaker-diarization/FBank.mlmodelc/model.mil",
  "speaker-diarization/FBank.mlmodelc/weights/weight.bin",
  "speaker-diarization/PldaRho.mlmodelc/analytics/coremldata.bin",
  "speaker-diarization/PldaRho.mlmodelc/coremldata.bin",
  "speaker-diarization/PldaRho.mlmodelc/metadata.json",
  "speaker-diarization/PldaRho.mlmodelc/model.mil",
  "speaker-diarization/PldaRho.mlmodelc/weights/weight.bin",
  "speaker-diarization/Segmentation.mlmodelc/analytics/coremldata.bin",
  "speaker-diarization/Segmentation.mlmodelc/coremldata.bin",
  "speaker-diarization/Segmentation.mlmodelc/metadata.json",
  "speaker-diarization/Segmentation.mlmodelc/model.mil",
  "speaker-diarization/Segmentation.mlmodelc/weights/weight.bin",
  "speaker-diarization/plda-parameters.json",
]

private let speakerKitRequiredModelFiles: Set<String> = [
  "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/analytics/coremldata.bin",
  "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/coremldata.bin",
  "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/metadata.json",
  "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/model.mil",
  "speaker_clusterer/pyannote-v4/W32A32/PldaProjector.mlmodelc/weights/weight.bin",
  "speaker_clusterer/pyannote-v4/W32A32/README.txt",
  "speaker_embedder/pyannote-v3/W8A16/README.txt",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/analytics/coremldata.bin",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/coremldata.bin",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/metadata.json",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/model.mil",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedder.mlmodelc/weights/weight.bin",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/analytics/coremldata.bin",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/coremldata.bin",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/metadata.json",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/model.mil",
  "speaker_embedder/pyannote-v3/W8A16/SpeakerEmbedderPreprocessor.mlmodelc/weights/weight.bin",
  "speaker_segmenter/pyannote-v3/W32A32/README.txt",
  "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/analytics/coremldata.bin",
  "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/coremldata.bin",
  "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/metadata.json",
  "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/model.mil",
  "speaker_segmenter/pyannote-v3/W32A32/SpeakerSegmenter.mlmodelc/weights/weight.bin",
  "speaker_segmenter/pyannote-v3/W8A16/README.txt",
  "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/analytics/coremldata.bin",
  "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/coremldata.bin",
  "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/metadata.json",
  "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/model.mil",
  "speaker_segmenter/pyannote-v3/W8A16/SpeakerSegmenter.mlmodelc/weights/weight.bin",
]

private struct ModelFileIdentity: Codable, Hashable {
  let relativePath: String
  let sha256: String
  let sizeBytes: UInt64
}

private struct DiarizationRequest: Codable {
  let schemaVersion: Int
  let recordingId: UUID
  let packId: String
  let qualityPreset: String
  let modelFiles: [ModelFileIdentity]
  let audioRelativePath: String
  let audioSha256: String
  let audioSizeBytes: UInt64
  let audioDurationMs: UInt64
  let expectedSpeakerCount: UInt32?
}

private struct DiarizationSegment: Codable {
  let startMs: UInt64
  let endMs: UInt64
  let speakerSlot: UInt32
  let confidenceMilli: UInt16
}

private struct DiarizationResponse: Codable {
  let schemaVersion: Int
  let recordingId: UUID
  let packId: String
  let qualityPreset: String
  let audioSha256: String
  let speakerCount: UInt32
  let segments: [DiarizationSegment]
}

private struct VerifiedFile {
  let root: URL
  let url: URL
  let identity: ModelFileIdentity
}

private enum WorkerFailure: Error {
  case code(String)

  var safeCode: String {
    switch self {
    case .code(let value): value
    }
  }
}

@main
private struct EchoWallDiarizationWorker {
  static func main() async {
    do {
      try await runOnce()
    } catch let failure as WorkerFailure {
      writeError(failure.safeCode)
      exit(EXIT_FAILURE)
    } catch {
      writeError("worker_failed")
      exit(EXIT_FAILURE)
    }
  }

  private static func runOnce() async throws {
    guard CommandLine.arguments.count == 1 else {
      throw WorkerFailure.code("arguments_forbidden")
    }
    let parentGuard: ParentLifetimeGuard?
    do {
      parentGuard = try ParentLifetimeGuard.bindFromEnvironment()
    } catch let error as ParentLifetimeError {
      throw WorkerFailure.code(error.safeCode)
    }
    defer { parentGuard?.stop() }
    guard let suppliedRoot = ProcessInfo.processInfo.environment[appDataRootEnvironment] else {
      throw WorkerFailure.code("root_missing")
    }
    let root = try canonicalDirectory(URL(fileURLWithPath: suppliedRoot, isDirectory: true))
    let input = try readBoundedInput()
    try requireClosedRequest(input)
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let request: DiarizationRequest
    do {
      request = try decoder.decode(DiarizationRequest.self, from: input)
    } catch {
      throw WorkerFailure.code("invalid_request")
    }
    try validate(request)

    let packRoot = try resolveDirectory(
      root: root,
      relativePath: "models/diarization/\(request.packId)"
    )
    let modelFiles = try verifyModelFiles(packRoot: packRoot, request: request)
    let audioRelative =
      "inbox/\(request.recordingId.uuidString.lowercased())/\(request.audioRelativePath)"
    let audio = try verifyFile(
      root: root,
      identity: ModelFileIdentity(
        relativePath: audioRelative,
        sha256: request.audioSha256,
        sizeBytes: request.audioSizeBytes
      )
    )

    let response =
      if SpeakerKitWorkerPreset(rawValue: request.qualityPreset) != nil {
        try await processSpeakerKit(packRoot: packRoot, audio: audio.url, request: request)
      } else {
        try await processFluidAudio(packRoot: packRoot, audio: audio.url, request: request)
      }

    try reverify(modelFiles)
    _ = try verifyFile(root: root, identity: audio.identity)
    let encoder = JSONEncoder()
    encoder.keyEncodingStrategy = .convertToSnakeCase
    encoder.outputFormatting = [.sortedKeys]
    let output: Data
    do {
      output = try encoder.encode(response)
    } catch {
      throw WorkerFailure.code("invalid_output")
    }
    guard !output.isEmpty, output.count + 1 <= maximumResponseBytes else {
      throw WorkerFailure.code("response_too_large")
    }
    var framed = output
    framed.append(0x0A)
    try FileHandle.standardOutput.write(contentsOf: framed)
  }
}

private func processFluidAudio(
  packRoot: URL,
  audio: URL,
  request: DiarizationRequest
) async throws -> DiarizationResponse {
  var config: OfflineDiarizerConfig
  switch request.qualityPreset {
  case legacyQualityPreset:
    config = OfflineDiarizerConfig()
  case selectedQualityPreset:
    config = OfflineDiarizerConfig(
      segmentationStepRatio: 0.15,
      minSegmentDuration: 0.4
    )
  default:
    throw WorkerFailure.code("invalid_request")
  }
  if let expected = request.expectedSpeakerCount {
    config = config.withSpeakers(exactly: Int(expected))
  }
  let modelConfiguration = MLModelConfiguration()
  modelConfiguration.computeUnits = .all
  let models: OfflineDiarizerModels
  do {
    models = try await OfflineDiarizerModels.load(
      from: packRoot,
      configuration: modelConfiguration
    )
  } catch {
    throw WorkerFailure.code("model_load_failed")
  }
  let manager = OfflineDiarizerManager(config: config)
  manager.initialize(models: models)
  let result: FluidAudioOffline.DiarizationResult
  do {
    result = try await manager.process(audio)
  } catch {
    throw WorkerFailure.code("inference_failed")
  }
  return try makeResponse(result: result, request: request)
}

private func processSpeakerKit(
  packRoot: URL,
  audio: URL,
  request: DiarizationRequest
) async throws -> DiarizationResponse {
  guard let preset = SpeakerKitWorkerPreset(rawValue: request.qualityPreset) else {
    throw WorkerFailure.code("invalid_request")
  }
  let source: DiskBackedAudioSampleSource
  do {
    source = try AudioSourceFactory()
      .makeDiskBackedSource(from: audio, targetSampleRate: 16_000).source
  } catch {
    throw WorkerFailure.code("inference_failed")
  }
  guard source.sampleCount > 0 else {
    throw WorkerFailure.code("inference_failed")
  }
  var samples = [Float](repeating: 0, count: source.sampleCount)
  do {
    try samples.withUnsafeMutableBufferPointer { buffer in
      guard let baseAddress = buffer.baseAddress else {
        throw WorkerFailure.code("inference_failed")
      }
      try source.copySamples(into: baseAddress, offset: 0, count: buffer.count)
    }
  } catch let failure as WorkerFailure {
    throw failure
  } catch {
    throw WorkerFailure.code("inference_failed")
  }

  let config = SpeakerKitOffline.PyannoteConfig(
    modelFolder: packRoot.path,
    download: false,
    load: true,
    verbose: false,
    logLevel: .none,
    fullRedundancy: true
  )
  let speakerKit: SpeakerKitOffline.SpeakerKit
  do {
    speakerKit = try await SpeakerKitOffline.SpeakerKit(config)
  } catch {
    throw WorkerFailure.code("model_load_failed")
  }
  let options = SpeakerKitOffline.PyannoteDiarizationOptions(
    numberOfSpeakers: request.expectedSpeakerCount.map(Int.init),
    clusterDistanceThreshold: 0.6,
    useExclusiveReconciliation: true,
    tailContextPolicy: preset.tailContextPolicy
  )
  let result: SpeakerKitOffline.DiarizationResult
  do {
    result = try await speakerKit.diarize(audioArray: samples, options: options)
  } catch {
    throw WorkerFailure.code("inference_failed")
  }
  return try makeSpeakerKitResponse(result: result, request: request)
}

private func readBoundedInput() throws -> Data {
  var input = Data()
  while input.count <= maximumRequestBytes {
    let remaining = maximumRequestBytes + 1 - input.count
    guard let chunk = try FileHandle.standardInput.read(upToCount: min(8 * 1024, remaining)),
      !chunk.isEmpty
    else {
      break
    }
    input.append(chunk)
  }
  guard !input.isEmpty, input.count <= maximumRequestBytes else {
    throw WorkerFailure.code("request_too_large")
  }
  return input
}

private func requireClosedRequest(_ data: Data) throws {
  let object: Any
  do {
    object = try JSONSerialization.jsonObject(with: data)
  } catch {
    throw WorkerFailure.code("invalid_request")
  }
  guard let root = object as? [String: Any],
    Set(root.keys)
      == Set([
        "schema_version", "recording_id", "pack_id", "model_files",
        "quality_preset",
        "audio_relative_path", "audio_sha256", "audio_size_bytes",
        "audio_duration_ms", "expected_speaker_count",
      ]),
    let files = root["model_files"] as? [[String: Any]],
    files.allSatisfy({
      Set($0.keys) == Set(["relative_path", "sha256", "size_bytes"])
    })
  else {
    throw WorkerFailure.code("invalid_request")
  }
}

private func validate(_ request: DiarizationRequest) throws {
  guard request.schemaVersion == protocolVersion,
    validIdentifier(request.packId),
    ([legacyQualityPreset, selectedQualityPreset].contains(request.qualityPreset)
      || SpeakerKitWorkerPreset(rawValue: request.qualityPreset) != nil),
    ((SpeakerKitWorkerPreset(rawValue: request.qualityPreset) != nil && request.packId == "speakerkit-v1")
      || (SpeakerKitWorkerPreset(rawValue: request.qualityPreset) == nil && request.packId == "fluid-v1")),
    !request.modelFiles.isEmpty,
    request.modelFiles.count <= maximumModelFiles,
    validRelativeAudioPath(request.audioRelativePath),
    validSha256(request.audioSha256),
    request.audioSizeBytes > 0,
    request.audioSizeBytes < maximumAudioBytes,
    request.audioDurationMs > 0,
    request.audioDurationMs < maximumDurationMilliseconds,
    request.expectedSpeakerCount.map({ $0 > 0 && $0 <= maximumSpeakers }) ?? true
  else {
    throw WorkerFailure.code("invalid_request")
  }
  var paths = Set<String>()
  var totalBytes: UInt64 = 0
  for file in request.modelFiles {
    let addition = totalBytes.addingReportingOverflow(file.sizeBytes)
    guard validRelativePath(file.relativePath),
      validSha256(file.sha256),
      file.sizeBytes > 0,
      paths.insert(file.relativePath).inserted,
      !addition.overflow
    else {
      throw WorkerFailure.code("invalid_request")
    }
    totalBytes = addition.partialValue
    guard totalBytes <= maximumModelBytes else {
      throw WorkerFailure.code("request_too_large")
    }
  }
  let expectedFiles =
    SpeakerKitWorkerPreset(rawValue: request.qualityPreset) != nil
    ? speakerKitRequiredModelFiles : requiredModelFiles
  guard paths == expectedFiles else {
    throw WorkerFailure.code("invalid_model_pack")
  }
}

private func verifyModelFiles(
  packRoot: URL,
  request: DiarizationRequest
) throws -> [VerifiedFile] {
  try request.modelFiles.map { try verifyFile(root: packRoot, identity: $0) }
}

private func reverify(_ files: [VerifiedFile]) throws {
  for file in files {
    _ = try verifyFile(
      root: file.root,
      identity: file.identity
    )
  }
}

private func verifyFile(root: URL, identity: ModelFileIdentity) throws -> VerifiedFile {
  let candidate = try resolveRegularFile(root: root, relativePath: identity.relativePath)
  let attributes = try FileManager.default.attributesOfItem(atPath: candidate.path)
  guard let size = attributes[.size] as? NSNumber,
    size.uint64Value == identity.sizeBytes,
    try sha256(candidate) == identity.sha256
  else {
    throw WorkerFailure.code("identity_mismatch")
  }
  return VerifiedFile(root: root, url: candidate, identity: identity)
}

private func sha256(_ url: URL) throws -> String {
  let handle = try FileHandle(forReadingFrom: url)
  defer { try? handle.close() }
  var digest = SHA256()
  while let chunk = try handle.read(upToCount: 1024 * 1024), !chunk.isEmpty {
    digest.update(data: chunk)
  }
  return digest.finalize().map { String(format: "%02x", $0) }.joined()
}

private func canonicalDirectory(_ url: URL) throws -> URL {
  let values = try url.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
  guard values.isDirectory == true, values.isSymbolicLink != true else {
    throw WorkerFailure.code("invalid_root")
  }
  return url.resolvingSymlinksInPath().standardizedFileURL
}

private func resolveDirectory(root: URL, relativePath: String) throws -> URL {
  guard validRelativePath(relativePath) else {
    throw WorkerFailure.code("invalid_path")
  }
  var candidate = root
  for component in relativePath.split(separator: "/") {
    candidate.appendPathComponent(String(component), isDirectory: true)
    let values = try candidate.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
    guard values.isDirectory == true, values.isSymbolicLink != true else {
      throw WorkerFailure.code("invalid_path")
    }
  }
  let resolved = candidate.resolvingSymlinksInPath().standardizedFileURL
  guard contained(resolved, by: root) else {
    throw WorkerFailure.code("invalid_path")
  }
  return resolved
}

private func resolveRegularFile(root: URL, relativePath: String) throws -> URL {
  guard validRelativePath(relativePath) else {
    throw WorkerFailure.code("invalid_path")
  }
  let parts = relativePath.split(separator: "/").map(String.init)
  var candidate = root
  for (index, component) in parts.enumerated() {
    candidate.appendPathComponent(component, isDirectory: index + 1 < parts.count)
    let values = try candidate.resourceValues(
      forKeys: [.isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey]
    )
    guard values.isSymbolicLink != true,
      index + 1 < parts.count ? values.isDirectory == true : values.isRegularFile == true
    else {
      throw WorkerFailure.code("invalid_path")
    }
  }
  let resolved = candidate.resolvingSymlinksInPath().standardizedFileURL
  guard contained(resolved, by: root) else {
    throw WorkerFailure.code("invalid_path")
  }
  return resolved
}

private func contained(_ candidate: URL, by root: URL) -> Bool {
  let rootPath = root.standardizedFileURL.path
  let candidatePath = candidate.standardizedFileURL.path
  return candidatePath == rootPath || candidatePath.hasPrefix(rootPath + "/")
}

private func makeResponse(
  result: FluidAudioOffline.DiarizationResult,
  request: DiarizationRequest
) throws -> DiarizationResponse {
  let ordered = result.segments.sorted { left, right in
    if left.startTimeSeconds == right.startTimeSeconds {
      return left.endTimeSeconds < right.endTimeSeconds
    }
    return left.startTimeSeconds < right.startTimeSeconds
  }
  var slotBySpeaker: [String: UInt32] = [:]
  var segments: [DiarizationSegment] = []
  var previousEnd: UInt64 = 0
  for segment in ordered {
    guard segment.startTimeSeconds.isFinite, segment.endTimeSeconds.isFinite,
      segment.qualityScore.isFinite,
      segment.startTimeSeconds >= 0,
      segment.endTimeSeconds > segment.startTimeSeconds
    else {
      throw WorkerFailure.code("invalid_output")
    }
    let start = UInt64((Double(segment.startTimeSeconds) * 1000).rounded())
    let end = min(
      UInt64((Double(segment.endTimeSeconds) * 1000).rounded()),
      request.audioDurationMs
    )
    guard start >= previousEnd, end > start else {
      throw WorkerFailure.code("segment_overlap")
    }
    let slot: UInt32
    if let existing = slotBySpeaker[segment.speakerId] {
      slot = existing
    } else {
      let next = UInt32(slotBySpeaker.count + 1)
      guard next <= maximumSpeakers else {
        throw WorkerFailure.code("invalid_output")
      }
      slotBySpeaker[segment.speakerId] = next
      slot = next
    }
    let confidence = UInt16(
      (Double(segment.qualityScore.clamped(to: 0...1)) * 1000).rounded()
    )
    segments.append(
      DiarizationSegment(
        startMs: start,
        endMs: end,
        speakerSlot: slot,
        confidenceMilli: confidence
      )
    )
    previousEnd = end
  }
  guard !segments.isEmpty, segments.count <= maximumSegments else {
    throw WorkerFailure.code("invalid_output")
  }
  let speakerCount = UInt32(slotBySpeaker.count)
  guard speakerCount > 0,
    request.expectedSpeakerCount.map({ $0 == speakerCount }) ?? true
  else {
    throw WorkerFailure.code("speaker_count_mismatch")
  }
  return DiarizationResponse(
    schemaVersion: protocolVersion,
    recordingId: request.recordingId,
    packId: request.packId,
    qualityPreset: request.qualityPreset,
    audioSha256: request.audioSha256,
    speakerCount: speakerCount,
    segments: segments
  )
}

private func makeSpeakerKitResponse(
  result: SpeakerKitOffline.DiarizationResult,
  request: DiarizationRequest
) throws -> DiarizationResponse {
  let ordered = result.segments.sorted { left, right in
    if left.startTime == right.startTime {
      return left.endTime < right.endTime
    }
    return left.startTime < right.startTime
  }
  var slotBySpeaker: [Int: UInt32] = [:]
  var segments: [DiarizationSegment] = []
  var previousEnd: UInt64 = 0
  for segment in ordered {
    guard let speakerId = segment.speaker.speakerId,
      speakerId >= 0,
      segment.startTime.isFinite,
      segment.endTime.isFinite,
      segment.qualityScore.isFinite,
      segment.startTime >= 0,
      segment.endTime > segment.startTime
    else {
      throw WorkerFailure.code("invalid_output")
    }
    let start = UInt64((Double(segment.startTime) * 1000).rounded())
    let end = min(
      UInt64((Double(segment.endTime) * 1000).rounded()),
      request.audioDurationMs
    )
    guard start >= previousEnd, end > start else {
      throw WorkerFailure.code("segment_overlap")
    }
    let slot: UInt32
    if let existing = slotBySpeaker[speakerId] {
      slot = existing
    } else {
      let next = UInt32(slotBySpeaker.count + 1)
      guard next <= maximumSpeakers else {
        throw WorkerFailure.code("invalid_output")
      }
      slotBySpeaker[speakerId] = next
      slot = next
    }
    let confidence = UInt16(
      (Double(segment.qualityScore.clamped(to: 0...1)) * 1000).rounded()
    )
    segments.append(
      DiarizationSegment(
        startMs: start,
        endMs: end,
        speakerSlot: slot,
        confidenceMilli: confidence
      )
    )
    previousEnd = end
  }
  guard !segments.isEmpty, segments.count <= maximumSegments else {
    throw WorkerFailure.code("invalid_output")
  }
  let speakerCount = UInt32(slotBySpeaker.count)
  guard speakerCount > 0,
    request.expectedSpeakerCount.map({ $0 == speakerCount }) ?? true
  else {
    throw WorkerFailure.code("speaker_count_mismatch")
  }
  return DiarizationResponse(
    schemaVersion: protocolVersion,
    recordingId: request.recordingId,
    packId: request.packId,
    qualityPreset: request.qualityPreset,
    audioSha256: request.audioSha256,
    speakerCount: speakerCount,
    segments: segments
  )
}

private func validIdentifier(_ value: String) -> Bool {
  guard (2...128).contains(value.utf8.count),
    value.utf8.first.map({ $0 >= 97 && $0 <= 122 }) == true
  else { return false }
  return value.utf8.allSatisfy { byte in
    (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57)
      || byte == 45 || byte == 46 || byte == 95
  }
}

private func validSha256(_ value: String) -> Bool {
  value.utf8.count == 64
    && value.utf8.allSatisfy { byte in
      (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
    }
}

private func validRelativeAudioPath(_ value: String) -> Bool {
  guard validRelativePath(value), let suffix = value.split(separator: ".").last else {
    return false
  }
  return ["wav", "m4a", "mp3"].contains(suffix.lowercased())
}

private func validRelativePath(_ value: String) -> Bool {
  guard !value.isEmpty, value.utf8.count <= 2048,
    !value.hasPrefix("/"), !value.contains("\\")
  else { return false }
  let parts = value.split(separator: "/", omittingEmptySubsequences: false)
  return !parts.isEmpty
    && parts.allSatisfy { part in
      !part.isEmpty && part != "." && part != ".." && !part.contains("\0")
    }
}

private func writeError(_ code: String) {
  let safe =
    code.utf8.allSatisfy { byte in
      (byte >= 97 && byte <= 122) || byte == 95
    } ? code : "worker_failed"
  let data = Data("echowall_diarization_worker_error:\(safe)\n".utf8)
  try? FileHandle.standardError.write(contentsOf: data)
}

extension Comparable {
  fileprivate func clamped(to range: ClosedRange<Self>) -> Self {
    min(max(self, range.lowerBound), range.upperBound)
  }
}
