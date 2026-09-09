import AVFAudio
import CryptoKit
import Foundation
import SwiftRs
import Tauri
import UIKit
import UniformTypeIdentifiers

private let echoWallAppIdentifier = "ai.ax.watch-transcriber"
private let minimumCaptureFreeBytes: Int64 = 1024 * 1024 * 1024

private func echoWallAppDataRoot(_ support: URL) -> URL {
  support.appendingPathComponent(echoWallAppIdentifier, isDirectory: true)
}

private struct StartCaptureArgs: Decodable {
  let recordingId: String
  let mode: String
}

private struct RecordingActionArgs: Decodable {
  let recordingId: String
}

private struct AcknowledgeSharedImportsArgs: Decodable {
  let importIds: [String]
}

private struct ExportAudioArgs: Decodable {
  let sourcePath: String
  let fileName: String
  let expectedSizeBytes: Int64
  let expectedSha256: String
}

private struct PendingExport {
  let invoke: Invoke
  let stagingDirectory: URL
  let expectedSizeBytes: Int64
  let expectedSha256: String
}

private struct CaptureSegment: Codable {
  let relativePath: String
  let durationMs: Int64
  let closedAt: String
}

private struct CaptureSnapshot: Codable {
  let schemaVersion: Int
  let recordingId: String
  var state: String
  let startedAt: String
  var endedAt: String?
  var segments: [CaptureSegment]
  var currentSegment: String?
  var warningCodes: [String]
}

private enum CaptureFailure: Error {
  case invalidRequest
  case permissionDenied
  case alreadyRecording
  case inactive
  case storageUnavailable
  case storageLow
  case recorderUnavailable

  var code: String {
    switch self {
    case .invalidRequest: return "invalid_request"
    case .permissionDenied: return "microphone_permission_denied"
    case .alreadyRecording: return "recording_already_active"
    case .inactive: return "recording_not_active"
    case .storageUnavailable: return "recording_storage_unavailable"
    case .storageLow: return "recording_storage_low"
    case .recorderUnavailable: return "recorder_unavailable"
    }
  }
}

private final class CaptureCoordinator: NSObject, AVAudioRecorderDelegate {
  static let shared = CaptureCoordinator()

  private let work = DispatchQueue(label: "ai.ax.echowall.capture")
  private let encoder = JSONEncoder()
  private var snapshot: CaptureSnapshot?
  private var root: URL?
  private var recorder: AVAudioRecorder?
  private var rollover: DispatchSourceTimer?
  private var observers: [NSObjectProtocol] = []

  override init() {
    encoder.outputFormatting = [.sortedKeys]
    super.init()
    cleanupAbandonedExports()
    recoverPersistedSessions()
  }

  func permissionState() -> String {
    switch AVAudioSession.sharedInstance().recordPermission {
    case .granted: return "granted"
    case .denied: return "denied"
    case .undetermined: return "undetermined"
    @unknown default: return "unknown"
    }
  }

  func requestPermission(_ completion: @escaping (String) -> Void) {
    AVAudioSession.sharedInstance().requestRecordPermission { granted in
      completion(granted ? "granted" : "denied")
    }
  }

  func preflight() -> [String: Any] {
    let audio = AVAudioSession.sharedInstance()
    let inputs = (audio.availableInputs ?? []).map { input in
      ["id": input.uid, "label": input.portName]
    }
    let availableBytes = availableStorageBytes()
    return [
      "supported": true,
      "mode": "voice_memo",
      "permission": permissionState(),
      "inputs": inputs,
      "backgroundContinuation": true,
      "segmentSeconds": 300,
      "storageAvailableBytes": availableBytes,
      "storageReady": availableBytes >= minimumCaptureFreeBytes,
    ]
  }

  func start(recordingId: String, mode: String) throws -> [String: Any] {
    try work.sync {
      guard UUID(uuidString: recordingId) != nil, mode == "voice_memo" else {
        throw CaptureFailure.invalidRequest
      }
      guard permissionState() == "granted" else {
        throw CaptureFailure.permissionDenied
      }
      guard availableStorageBytes() >= minimumCaptureFreeBytes else {
        throw CaptureFailure.storageLow
      }
      guard snapshot == nil else { throw CaptureFailure.alreadyRecording }

      let directory = try sessionDirectory(recordingId: recordingId)
      try FileManager.default.createDirectory(
        at: directory.appendingPathComponent("tracks", isDirectory: true),
        withIntermediateDirectories: true
      )
      try setProtection(directory)
      root = directory
      snapshot = CaptureSnapshot(
        schemaVersion: 1,
        recordingId: recordingId,
        state: "recording",
        startedAt: timestamp(),
        endedAt: nil,
        segments: [],
        currentSegment: nil,
        warningCodes: []
      )
      try activateAudioSession()
      try openSegment()
      installObservers()
      try appendEvent("recording_started")
      try persist()
      scheduleRollover()
      return statusPayload()
    }
  }

  func pause(recordingId: String) throws -> [String: Any] {
    try work.sync {
      try requireActive(recordingId)
      guard snapshot?.state == "recording" else { throw CaptureFailure.inactive }
      try closeSegment(reason: "paused")
      snapshot?.state = "paused"
      try appendEvent("recording_paused")
      try persist()
      return statusPayload()
    }
  }

  func resume(recordingId: String) throws -> [String: Any] {
    try work.sync {
      try requireActive(recordingId)
      guard ["paused", "interrupted"].contains(snapshot?.state ?? "") else {
        throw CaptureFailure.inactive
      }
      try activateAudioSession()
      snapshot?.state = "recording"
      try openSegment()
      try appendEvent("recording_resumed")
      try persist()
      scheduleRollover()
      return statusPayload()
    }
  }

  func stop(recordingId: String) throws -> [String: Any] {
    try work.sync {
      try requireActive(recordingId)
      if snapshot?.state == "recording" {
        try closeSegment(reason: "stopped")
      }
      rollover?.cancel()
      rollover = nil
      snapshot?.state = "finalizing"
      snapshot?.endedAt = timestamp()
      try appendEvent("recording_stopped")
      try persist()
      try? AVAudioSession.sharedInstance().setActive(
        false, options: [.notifyOthersOnDeactivation]
      )
      let result = statusPayload()
      removeObservers()
      recorder = nil
      root = nil
      snapshot = nil
      return result
    }
  }

  func status(recordingId: String) throws -> [String: Any] {
    try work.sync {
      if snapshot?.recordingId == recordingId { return statusPayload() }
      let directory = try sessionDirectory(recordingId: recordingId)
      let url = directory.appendingPathComponent("capture.json")
      guard let data = try? Data(contentsOf: url),
            var stored = try? JSONDecoder().decode(CaptureSnapshot.self, from: data)
      else { throw CaptureFailure.inactive }
      if ["recording", "paused"].contains(stored.state) {
        stored.state = "interrupted"
        if stored.currentSegment != nil,
           !stored.warningCodes.contains("unclosed_segment_after_restart") {
          stored.warningCodes.append("unclosed_segment_after_restart")
        }
        stored.currentSegment = nil
        if !stored.warningCodes.contains("process_restarted") {
          stored.warningCodes.append("process_restarted")
        }
        try encoder.encode(stored).write(to: url, options: [.atomic])
      }
      return payload(stored)
    }
  }

  private func recoverPersistedSessions() {
    guard let support = FileManager.default.urls(
      for: .applicationSupportDirectory, in: .userDomainMask
    ).first else { return }
    migrateLegacySessions(support)
    let recordings = nativeSessionsRoot(support)
    guard let directories = try? FileManager.default.contentsOfDirectory(
      at: recordings,
      includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey],
      options: [.skipsHiddenFiles]
    ) else { return }
    for directory in directories.prefix(512) {
      let values = try? directory.resourceValues(forKeys: [
        .isDirectoryKey, .isSymbolicLinkKey,
      ])
      guard values?.isDirectory == true, values?.isSymbolicLink != true else { continue }
      let url = directory.appendingPathComponent("capture.json")
      guard let data = try? Data(contentsOf: url),
            data.count <= 1_048_576,
            var stored = try? JSONDecoder().decode(CaptureSnapshot.self, from: data),
            ["recording", "paused", "interrupted"].contains(stored.state) else { continue }
      stored.state = "interrupted"
      if stored.currentSegment != nil,
         !stored.warningCodes.contains("unclosed_segment_after_restart") {
        stored.warningCodes.append("unclosed_segment_after_restart")
      }
      stored.currentSegment = nil
      if !stored.warningCodes.contains("process_restarted") {
        stored.warningCodes.append("process_restarted")
      }
      try? encoder.encode(stored).write(to: url, options: [.atomic])
    }
  }

  private func nativeSessionsRoot(_ support: URL) -> URL {
    echoWallAppDataRoot(support)
      .appendingPathComponent("native-capture", isDirectory: true)
      .appendingPathComponent("sessions", isDirectory: true)
  }

  private func migrateLegacySessions(_ support: URL) {
    let targetRoot = nativeSessionsRoot(support)
    try? FileManager.default.createDirectory(
      at: targetRoot, withIntermediateDirectories: true
    )
    let legacyRoots = [
      support.appendingPathComponent("recordings", isDirectory: true),
      support
        .appendingPathComponent("native-capture", isDirectory: true)
        .appendingPathComponent("sessions", isDirectory: true),
    ]
    var inspected = 0
    for legacy in legacyRoots {
      guard let directories = try? FileManager.default.contentsOfDirectory(
        at: legacy,
        includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey],
        options: [.skipsHiddenFiles]
      ) else { continue }
      for directory in directories where inspected < 512 {
        inspected += 1
        let values = try? directory.resourceValues(forKeys: [
          .isDirectoryKey, .isSymbolicLinkKey,
        ])
        guard values?.isDirectory == true,
              values?.isSymbolicLink != true,
              FileManager.default.fileExists(
                atPath: directory.appendingPathComponent("capture.json").path
              ),
              !FileManager.default.fileExists(
                atPath: directory.appendingPathComponent("recording.json").path
              ) else { continue }
        let target = targetRoot.appendingPathComponent(
          directory.lastPathComponent, isDirectory: true
        )
        guard !FileManager.default.fileExists(atPath: target.path) else { continue }
        try? FileManager.default.moveItem(at: directory, to: target)
      }
    }
  }

  private func cleanupAbandonedExports() {
    guard let cache = FileManager.default.urls(
      for: .cachesDirectory, in: .userDomainMask
    ).first else { return }
    let root = cache.appendingPathComponent("echowall-exports", isDirectory: true)
    guard let values = try? root.resourceValues(forKeys: [
      .isDirectoryKey, .isSymbolicLinkKey,
    ]), values.isDirectory == true, values.isSymbolicLink != true else { return }
    try? FileManager.default.removeItem(at: root)
  }

  private func activateAudioSession() throws {
    let audio = AVAudioSession.sharedInstance()
    try audio.setCategory(.record, mode: .spokenAudio, options: [.allowBluetoothHFP])
    try audio.setPreferredSampleRate(48_000)
    try audio.setActive(true)
  }

  private func openSegment() throws {
    guard let root, var value = snapshot else { throw CaptureFailure.inactive }
    let number = value.segments.count + 1
    let relative = String(format: "tracks/mic-%04d.m4a", number)
    let url = root.appendingPathComponent(relative)
    let settings: [String: Any] = [
      AVFormatIDKey: kAudioFormatMPEG4AAC,
      AVSampleRateKey: 48_000,
      AVNumberOfChannelsKey: 1,
      AVEncoderBitRateKey: 96_000,
      AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue,
    ]
    let next = try AVAudioRecorder(url: url, settings: settings)
    next.delegate = self
    next.isMeteringEnabled = true
    guard next.prepareToRecord(), next.record() else {
      throw CaptureFailure.recorderUnavailable
    }
    value.currentSegment = relative
    snapshot = value
    recorder = next
    try appendEvent("segment_started", fields: ["segment": relative])
  }

  private func closeSegment(reason: String) throws {
    guard let active = recorder, var value = snapshot,
          let relative = value.currentSegment else { return }
    let duration = Int64((active.currentTime * 1000).rounded())
    active.stop()
    recorder = nil
    value.currentSegment = nil
    if duration > 0 {
      value.segments.append(
        CaptureSegment(
          relativePath: relative,
          durationMs: duration,
          closedAt: timestamp()
        )
      )
    }
    snapshot = value
    try appendEvent(
      "segment_closed",
      fields: ["segment": relative, "duration_ms": duration, "reason": reason]
    )
    try persist()
  }

  private func scheduleRollover() {
    rollover?.cancel()
    let timer = DispatchSource.makeTimerSource(queue: work)
    timer.schedule(deadline: .now() + 300)
    timer.setEventHandler { [weak self] in
      guard let self, self.snapshot?.state == "recording" else { return }
      do {
        try self.closeSegment(reason: "rollover")
        try self.openSegment()
        try self.persist()
        self.scheduleRollover()
      } catch {
        self.failActiveCapture("segment_rollover_failed")
      }
    }
    timer.resume()
    rollover = timer
  }

  private func installObservers() {
    guard observers.isEmpty else { return }
    let center = NotificationCenter.default
    observers.append(
      center.addObserver(
        forName: AVAudioSession.interruptionNotification,
        object: nil,
        queue: nil
      ) { [weak self] _ in
        self?.work.async { self?.interrupt("audio_interruption") }
      }
    )
    observers.append(
      center.addObserver(
        forName: AVAudioSession.routeChangeNotification,
        object: nil,
        queue: nil
      ) { [weak self] _ in
        self?.work.async { self?.interrupt("audio_route_changed") }
      }
    )
  }

  private func removeObservers() {
    for observer in observers { NotificationCenter.default.removeObserver(observer) }
    observers.removeAll()
  }

  private func interrupt(_ code: String) {
    guard snapshot?.state == "recording" else { return }
    do {
      try closeSegment(reason: code)
      snapshot?.state = "interrupted"
      if !(snapshot?.warningCodes.contains(code) ?? true) {
        snapshot?.warningCodes.append(code)
      }
      try appendEvent("recording_interrupted", fields: ["code": code])
      try persist()
    } catch {
      failActiveCapture("interruption_checkpoint_failed")
    }
  }

  private func failActiveCapture(_ code: String) {
    recorder?.stop()
    recorder = nil
    snapshot?.currentSegment = nil
    snapshot?.state = "interrupted"
    if !(snapshot?.warningCodes.contains(code) ?? true) {
      snapshot?.warningCodes.append(code)
    }
    try? appendEvent("recording_interrupted", fields: ["code": code])
    try? persist()
  }

  func audioRecorderEncodeErrorDidOccur(
    _ recorder: AVAudioRecorder, error: Error?
  ) {
    work.async { [weak self] in self?.failActiveCapture("encoder_failed") }
  }

  private func requireActive(_ recordingId: String) throws {
    guard snapshot?.recordingId == recordingId else { throw CaptureFailure.inactive }
  }

  private func sessionDirectory(recordingId: String) throws -> URL {
    guard UUID(uuidString: recordingId) != nil else { throw CaptureFailure.invalidRequest }
    guard let support = FileManager.default.urls(
      for: .applicationSupportDirectory, in: .userDomainMask
    ).first else { throw CaptureFailure.storageUnavailable }
    return echoWallAppDataRoot(support)
      .appendingPathComponent("native-capture", isDirectory: true)
      .appendingPathComponent("sessions", isDirectory: true)
      .appendingPathComponent(recordingId.lowercased(), isDirectory: true)
  }

  private func availableStorageBytes() -> Int64 {
    guard let support = FileManager.default.urls(
      for: .applicationSupportDirectory, in: .userDomainMask
    ).first,
    let values = try? support.resourceValues(
      forKeys: [.volumeAvailableCapacityForImportantUsageKey]
    ) else { return -1 }
    return values.volumeAvailableCapacityForImportantUsage ?? -1
  }

  private func setProtection(_ directory: URL) throws {
    try (directory as NSURL).setResourceValue(
      URLFileProtection.completeUntilFirstUserAuthentication,
      forKey: .fileProtectionKey
    )
  }

  private func timestamp() -> String {
    ISO8601DateFormatter().string(from: Date())
  }

  private func persist() throws {
    guard let root, let snapshot else { throw CaptureFailure.inactive }
    try encoder.encode(snapshot).write(
      to: root.appendingPathComponent("capture.json"), options: [.atomic]
    )
  }

  private func appendEvent(
    _ kind: String, fields: [String: Any] = [:]
  ) throws {
    guard let root, let snapshot else { throw CaptureFailure.inactive }
    var event = fields
    event["schema_version"] = 1
    event["recording_id"] = snapshot.recordingId
    event["kind"] = kind
    event["occurred_at"] = timestamp()
    let data = try JSONSerialization.data(withJSONObject: event, options: [.sortedKeys])
    let url = root.appendingPathComponent("capture-events.ndjson")
    if !FileManager.default.fileExists(atPath: url.path) {
      FileManager.default.createFile(atPath: url.path, contents: nil)
    }
    let handle = try FileHandle(forWritingTo: url)
    handle.seekToEndOfFile()
    handle.write(data)
    handle.write(Data([0x0A]))
    handle.synchronizeFile()
    handle.closeFile()
  }

  private func statusPayload() -> [String: Any] {
    guard let snapshot else { return ["state": "idle"] }
    var result = payload(snapshot)
    if ["recording", "paused"].contains(snapshot.state) {
      recorder?.updateMeters()
      let decibels = recorder?.averagePower(forChannel: 0) ?? -160
      result["microphoneLevel"] = min(1.0, max(0.0, pow(10.0, Double(decibels) / 20.0)))
    }
    return result
  }

  private func payload(_ value: CaptureSnapshot) -> [String: Any] {
    let duration = value.segments.reduce(Int64(0)) { $0 + $1.durationMs }
    return [
      "recordingId": value.recordingId,
      "state": value.state,
      "startedAt": value.startedAt,
      "endedAt": value.endedAt as Any,
      "closedDurationMs": duration,
      "closedSegments": value.segments.map {
        ["relativePath": $0.relativePath, "durationMs": $0.durationMs]
      },
      "warnings": value.warningCodes,
      "currentSegment": value.currentSegment as Any,
    ]
  }
}

private enum SharedImportDrain {
  static let appGroup = "group.ai.ax.watch-transcriber"
  static let supportedExtensions = Set(["m4a", "mp3", "wav"])
  static let maximumBytes: Int64 = 512 * 1024 * 1024

  private static func roots() throws -> (source: URL, destination: URL) {
    guard let shared = FileManager.default.containerURL(
      forSecurityApplicationGroupIdentifier: appGroup
    ), let support = FileManager.default.urls(
      for: .applicationSupportDirectory, in: .userDomainMask
    ).first else { throw CaptureFailure.storageUnavailable }
    let source = shared.appendingPathComponent("share-inbox", isDirectory: true)
    let destination = echoWallAppDataRoot(support)
      .appendingPathComponent("shared-imports", isDirectory: true)
    try FileManager.default.createDirectory(
      at: source, withIntermediateDirectories: true
    )
    try FileManager.default.createDirectory(
      at: destination, withIntermediateDirectories: true
    )
    return (source, destination)
  }

  private static func stableIdentifier(_ source: URL) throws -> String {
    let raw = source.deletingPathExtension().lastPathComponent.lowercased()
    guard let uuid = UUID(uuidString: raw), uuid.uuidString.lowercased() == raw else {
      throw CaptureFailure.invalidRequest
    }
    return raw
  }

  private static func safeDisplayName(_ value: String, fallback: String) -> String {
    let scalars = value.unicodeScalars.filter {
      $0.value >= 32 && $0 != "/" && $0 != "\\"
    }
    let cleaned = String(String.UnicodeScalarView(scalars.prefix(160)))
      .trimmingCharacters(in: .whitespacesAndNewlines)
    return cleaned.isEmpty ? fallback : cleaned
  }

  private static func writeReceipt(
    root: URL, identifier: String, displayName: String, size: Int
  ) throws {
    let receipt: [String: Any] = [
      "schema_version": 1,
      "import_id": identifier,
      "display_name": safeDisplayName(
        displayName, fallback: "\(identifier).m4a"
      ),
      "size_bytes": size,
    ]
    let data = try JSONSerialization.data(withJSONObject: receipt, options: [.sortedKeys])
    try data.write(
      to: root.appendingPathComponent("\(identifier).json"), options: [.atomic]
    )
  }

  private static func receiptDisplayName(
    identifier: String, sourceRoot: URL, destinationRoot: URL, fallback: String
  ) -> String {
    for root in [destinationRoot, sourceRoot] {
      let url = root.appendingPathComponent("\(identifier).json")
      guard let values = try? url.resourceValues(forKeys: [
        .isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey,
      ]), values.isRegularFile == true,
            values.isSymbolicLink != true,
            let size = values.fileSize,
            (1...4_096).contains(size),
            let data = try? Data(contentsOf: url),
            let object = try? JSONSerialization.jsonObject(with: data),
            let receipt = object as? [String: Any],
            receipt["schema_version"] as? Int == 1,
            receipt["import_id"] as? String == identifier,
            let name = receipt["display_name"] as? String else { continue }
      return safeDisplayName(name, fallback: fallback)
    }
    return fallback
  }

  private static func validatedSource(_ source: URL) throws -> (suffix: String, size: Int) {
    let values = try source.resourceValues(forKeys: [
      .isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey,
    ])
    let suffix = source.pathExtension.lowercased()
    guard values.isRegularFile == true,
          values.isSymbolicLink != true,
          supportedExtensions.contains(suffix),
          let size = values.fileSize,
          size > 0,
          Int64(size) <= maximumBytes else { throw CaptureFailure.invalidRequest }
    return (suffix, size)
  }

  private static func stage(
    source: URL, identifier: String, destinationRoot: URL
  ) throws -> URL {
    let (suffix, size) = try validatedSource(source)
    let temporary = destinationRoot.appendingPathComponent("\(identifier).part")
    let destination = destinationRoot.appendingPathComponent("\(identifier).\(suffix)")
    if FileManager.default.fileExists(atPath: destination.path) {
      let copied = try validatedSource(destination)
      guard copied.suffix == suffix, copied.size == size else {
        throw CaptureFailure.storageUnavailable
      }
      return destination
    }
    try? FileManager.default.removeItem(at: temporary)
    try FileManager.default.copyItem(at: source, to: temporary)
    do {
      let copied = try temporary.resourceValues(forKeys: [.fileSizeKey]).fileSize
      guard copied == size else { throw CaptureFailure.storageUnavailable }
      let handle = try FileHandle(forWritingTo: temporary)
      handle.synchronizeFile()
      handle.closeFile()
      try FileManager.default.moveItem(at: temporary, to: destination)
      return destination
    } catch {
      try? FileManager.default.removeItem(at: temporary)
      throw error
    }
  }

  static func stagePicked(_ source: URL) throws -> [String: Any] {
    let (_, destinationRoot) = try roots()
    let identifier = UUID().uuidString.lowercased()
    let destination = try stage(
      source: source, identifier: identifier, destinationRoot: destinationRoot
    )
    let (_, size) = try validatedSource(destination)
    do {
      try writeReceipt(
        root: destinationRoot,
        identifier: identifier,
        displayName: source.lastPathComponent,
        size: size
      )
    } catch {
      try? FileManager.default.removeItem(at: destination)
      throw error
    }
    return [
      "importId": identifier,
      "path": destination.path,
      "displayName": source.lastPathComponent,
      "sizeBytes": size,
    ]
  }

  static func run() throws -> [[String: Any]] {
    let (sourceRoot, destinationRoot) = try roots()
    if FileManager.default.fileExists(atPath: sourceRoot.path) {
      let sources = try FileManager.default.contentsOfDirectory(
        at: sourceRoot,
        includingPropertiesForKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey],
        options: [.skipsHiddenFiles]
      ).sorted { $0.lastPathComponent < $1.lastPathComponent }
      let audioSources = sources.filter {
        supportedExtensions.contains($0.pathExtension.lowercased())
      }
      for source in audioSources.prefix(16) {
        let identifier = try stableIdentifier(source)
        _ = try stage(
          source: source, identifier: identifier, destinationRoot: destinationRoot
        )
      }
    }
    var results: [[String: Any]] = []
    let staged = try FileManager.default.contentsOfDirectory(
      at: destinationRoot,
      includingPropertiesForKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey],
      options: [.skipsHiddenFiles]
    ).sorted { $0.lastPathComponent < $1.lastPathComponent }
    for destination in staged where results.count < 16 {
      guard supportedExtensions.contains(destination.pathExtension.lowercased()) else { continue }
      let identifier = try stableIdentifier(destination)
      let (_, size) = try validatedSource(destination)
      results.append([
        "importId": identifier,
        "path": destination.path,
        "displayName": receiptDisplayName(
          identifier: identifier,
          sourceRoot: sourceRoot,
          destinationRoot: destinationRoot,
          fallback: destination.lastPathComponent
        ),
        "sizeBytes": size,
      ])
    }
    return results
  }

  static func acknowledge(importIds: [String]) throws -> Int {
    guard !importIds.isEmpty, importIds.count <= 16 else {
      throw CaptureFailure.invalidRequest
    }
    let (sourceRoot, destinationRoot) = try roots()
    var removed = 0
    for identifier in Set(importIds.map { $0.lowercased() }).sorted() {
      guard let uuid = UUID(uuidString: identifier),
            uuid.uuidString.lowercased() == identifier else {
        throw CaptureFailure.invalidRequest
      }
      for suffix in supportedExtensions.sorted() {
        // Remove the main-App staging copy first. If source cleanup then fails,
        // the retained App Group original recreates the staging copy next run.
        for candidate in [
          destinationRoot.appendingPathComponent("\(identifier).\(suffix)"),
          sourceRoot.appendingPathComponent("\(identifier).\(suffix)"),
        ] where FileManager.default.fileExists(atPath: candidate.path) {
          let values = try candidate.resourceValues(forKeys: [
            .isRegularFileKey, .isSymbolicLinkKey,
          ])
          guard values.isRegularFile == true, values.isSymbolicLink != true else {
            throw CaptureFailure.invalidRequest
          }
          try FileManager.default.removeItem(at: candidate)
          removed += 1
        }
      }
      for receipt in [
        destinationRoot.appendingPathComponent("\(identifier).json"),
        sourceRoot.appendingPathComponent("\(identifier).json"),
      ] where FileManager.default.fileExists(atPath: receipt.path) {
        let values = try receipt.resourceValues(forKeys: [
          .isRegularFileKey, .isSymbolicLinkKey,
        ])
        guard values.isRegularFile == true, values.isSymbolicLink != true else {
          throw CaptureFailure.invalidRequest
        }
        try FileManager.default.removeItem(at: receipt)
        removed += 1
      }
    }
    return removed
  }
}

final class CapturePlugin: Plugin, UIDocumentPickerDelegate {
  private var pendingPickerInvoke: Invoke?
  private var pendingExport: PendingExport?

  override init() {
    super.init()
    _ = CaptureCoordinator.shared
  }

  @objc override func checkPermissions(_ invoke: Invoke) {
    invoke.resolve(["microphone": CaptureCoordinator.shared.permissionState()])
  }

  @objc override func requestPermissions(_ invoke: Invoke) {
    CaptureCoordinator.shared.requestPermission { state in
      invoke.resolve(["microphone": state])
    }
  }

  @objc func preflight(_ invoke: Invoke) {
    invoke.resolve(CaptureCoordinator.shared.preflight())
  }

  @objc func start(_ invoke: Invoke) throws {
    do {
      let args = try invoke.parseArgs(StartCaptureArgs.self)
      invoke.resolve(
        try CaptureCoordinator.shared.start(
          recordingId: args.recordingId, mode: args.mode
        )
      )
    } catch let error as CaptureFailure {
      invoke.reject(error.code)
    } catch {
      invoke.reject("capture_start_failed")
    }
  }

  @objc func pause(_ invoke: Invoke) throws {
    performAction(invoke, action: CaptureCoordinator.shared.pause)
  }

  @objc func resume(_ invoke: Invoke) throws {
    performAction(invoke, action: CaptureCoordinator.shared.resume)
  }

  @objc func stop(_ invoke: Invoke) throws {
    performAction(invoke, action: CaptureCoordinator.shared.stop)
  }

  @objc func status(_ invoke: Invoke) throws {
    performAction(invoke, action: CaptureCoordinator.shared.status)
  }

  @objc func drainSharedImports(_ invoke: Invoke) {
    do {
      invoke.resolve(["items": try SharedImportDrain.run()])
    } catch {
      invoke.reject("shared_import_drain_failed")
    }
  }

  @objc func openAudioPicker(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.pendingPickerInvoke == nil,
            self.pendingExport == nil,
            let viewController = self.manager.viewController else {
        invoke.reject("audio_picker_unavailable")
        return
      }
      self.pendingPickerInvoke = invoke
      let picker: UIDocumentPickerViewController
      if #available(iOS 14.0, *) {
        picker = UIDocumentPickerViewController(
          forOpeningContentTypes: [UTType.audio], asCopy: false
        )
      } else {
        picker = UIDocumentPickerViewController(
          documentTypes: ["public.audio"], in: .open
        )
      }
      picker.delegate = self
      picker.allowsMultipleSelection = true
      picker.modalPresentationStyle = .fullScreen
      viewController.present(picker, animated: true)
    }
  }

  func documentPicker(
    _ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]
  ) {
    if let pending = pendingExport {
      pendingExport = nil
      completeExport(pending, destination: urls.first)
      return
    }
    guard let invoke = pendingPickerInvoke else { return }
    pendingPickerInvoke = nil
    guard !urls.isEmpty, urls.count <= 16 else {
      invoke.reject("audio_picker_count_invalid")
      return
    }
    var items: [[String: Any]] = []
    for url in urls {
      let scoped = url.startAccessingSecurityScopedResource()
      defer { if scoped { url.stopAccessingSecurityScopedResource() } }
      do {
        items.append(try SharedImportDrain.stagePicked(url))
      } catch {
        // Batch items are independent; Rust validates every durable success.
      }
    }
    guard !items.isEmpty else {
      invoke.reject("audio_picker_import_failed")
      return
    }
    invoke.resolve(["items": items, "rejected": urls.count - items.count])
  }

  func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    if let pending = pendingExport {
      pendingExport = nil
      try? FileManager.default.removeItem(at: pending.stagingDirectory)
      pending.invoke.resolve(["exported": false])
      return
    }
    pendingPickerInvoke?.reject("audio_picker_cancelled")
    pendingPickerInvoke = nil
  }

  @objc func exportAudio(_ invoke: Invoke) {
    do {
      let args = try invoke.parseArgs(ExportAudioArgs.self)
      let source = try validatedExportSource(args)
      let fileName = try validatedExportName(args.fileName)
      guard let cache = FileManager.default.urls(
        for: .cachesDirectory, in: .userDomainMask
      ).first else { throw CaptureFailure.storageUnavailable }
      let stagingDirectory = cache
        .appendingPathComponent("echowall-exports", isDirectory: true)
        .appendingPathComponent(UUID().uuidString.lowercased(), isDirectory: true)
      try FileManager.default.createDirectory(
        at: stagingDirectory, withIntermediateDirectories: true
      )
      let staged = stagingDirectory.appendingPathComponent(fileName)
      do {
        try FileManager.default.copyItem(at: source, to: staged)
        let stagedDigest = try exportDigest(staged)
        guard stagedDigest.size == args.expectedSizeBytes,
              stagedDigest.sha256 == args.expectedSha256 else {
          throw CaptureFailure.storageUnavailable
        }
      } catch {
        try? FileManager.default.removeItem(at: stagingDirectory)
        throw error
      }
      DispatchQueue.main.async {
        guard self.pendingPickerInvoke == nil,
              self.pendingExport == nil,
              let viewController = self.manager.viewController else {
          try? FileManager.default.removeItem(at: stagingDirectory)
          invoke.reject("audio_export_unavailable")
          return
        }
        self.pendingExport = PendingExport(
          invoke: invoke,
          stagingDirectory: stagingDirectory,
          expectedSizeBytes: args.expectedSizeBytes,
          expectedSha256: args.expectedSha256
        )
        let picker: UIDocumentPickerViewController
        if #available(iOS 14.0, *) {
          picker = UIDocumentPickerViewController(forExporting: [staged], asCopy: true)
        } else {
          picker = UIDocumentPickerViewController(url: staged, in: .exportToService)
        }
        picker.delegate = self
        picker.modalPresentationStyle = .fullScreen
        viewController.present(picker, animated: true)
      }
    } catch {
      invoke.reject("audio_export_rejected")
    }
  }

  private func validatedExportSource(_ args: ExportAudioArgs) throws -> URL {
    guard args.expectedSizeBytes > 0,
          args.expectedSizeBytes <= 512 * 1024 * 1024,
          args.expectedSha256.range(
            of: "^[0-9a-f]{64}$", options: .regularExpression
          ) != nil,
          let support = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask
          ).first else { throw CaptureFailure.invalidRequest }
    let root = echoWallAppDataRoot(support)
      .appendingPathComponent("inbox", isDirectory: true)
      .resolvingSymlinksInPath()
    let supplied = URL(fileURLWithPath: args.sourcePath).standardizedFileURL
    let source = supplied.resolvingSymlinksInPath()
    guard source.path == supplied.path,
          source.path.hasPrefix(root.path + "/") else {
      throw CaptureFailure.invalidRequest
    }
    let values = try source.resourceValues(forKeys: [
      .isRegularFileKey, .isSymbolicLinkKey,
    ])
    guard values.isRegularFile == true, values.isSymbolicLink != true else {
      throw CaptureFailure.invalidRequest
    }
    let digest = try exportDigest(source)
    guard digest.size == args.expectedSizeBytes,
          digest.sha256 == args.expectedSha256 else {
      throw CaptureFailure.invalidRequest
    }
    return source
  }

  private func validatedExportName(_ value: String) throws -> String {
    let suffix = URL(fileURLWithPath: value).pathExtension.lowercased()
    guard !value.isEmpty,
          value.utf8.count <= 255,
          !value.contains("/"),
          !value.contains("\\"),
          value.unicodeScalars.allSatisfy({ $0.value >= 32 }),
          ["m4a", "mp3", "wav"].contains(suffix) else {
      throw CaptureFailure.invalidRequest
    }
    return value
  }

  private func exportDigest(_ source: URL) throws -> (size: Int64, sha256: String) {
    let handle = try FileHandle(forReadingFrom: source)
    defer { handle.closeFile() }
    var hash = SHA256()
    var size: Int64 = 0
    while true {
      let data = handle.readData(ofLength: 64 * 1024)
      if data.isEmpty { break }
      size += Int64(data.count)
      guard size <= 512 * 1024 * 1024 else { throw CaptureFailure.invalidRequest }
      hash.update(data: data)
    }
    let value = hash.finalize().map { String(format: "%02x", $0) }.joined()
    return (size, value)
  }

  private func completeExport(_ pending: PendingExport, destination: URL?) {
    defer { try? FileManager.default.removeItem(at: pending.stagingDirectory) }
    guard let destination else {
      pending.invoke.reject("audio_export_missing_destination")
      return
    }
    let scoped = destination.startAccessingSecurityScopedResource()
    defer { if scoped { destination.stopAccessingSecurityScopedResource() } }
    do {
      let digest = try exportDigest(destination)
      guard digest.size == pending.expectedSizeBytes,
            digest.sha256 == pending.expectedSha256 else {
        throw CaptureFailure.storageUnavailable
      }
      pending.invoke.resolve(["exported": true])
    } catch {
      pending.invoke.reject("audio_export_failed_verification")
    }
  }

  @objc func acknowledgeSharedImports(_ invoke: Invoke) {
    do {
      let args = try invoke.parseArgs(AcknowledgeSharedImportsArgs.self)
      invoke.resolve(["removed": try SharedImportDrain.acknowledge(importIds: args.importIds)])
    } catch let error as CaptureFailure {
      invoke.reject(error.code)
    } catch {
      invoke.reject("shared_import_ack_failed")
    }
  }

  private func performAction(
    _ invoke: Invoke,
    action: (String) throws -> [String: Any]
  ) {
    do {
      let args = try invoke.parseArgs(RecordingActionArgs.self)
      invoke.resolve(try action(args.recordingId))
    } catch let error as CaptureFailure {
      invoke.reject(error.code)
    } catch {
      invoke.reject("capture_action_failed")
    }
  }
}

@_cdecl("init_plugin_capture")
func initCapturePlugin() -> Plugin {
  CapturePlugin()
}
