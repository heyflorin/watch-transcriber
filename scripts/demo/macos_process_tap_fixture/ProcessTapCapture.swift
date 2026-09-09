// Core Audio process-tap lifecycle adapted from insidegui/AudioCap (BSD-2-Clause).
// This fabricated spike never ships in EchoWall; see AudioCap-LICENSE.

import AVFoundation
import AudioToolbox
import CoreAudio
import Foundation

private enum SpikeFailure: Error, CustomStringConvertible {
  case message(String)
  case status(String, OSStatus)

  var description: String {
    switch self {
    case .message(let message): message
    case .status(let operation, let status): "\(operation) failed with OSStatus \(status)"
    }
  }
}

extension AudioObjectID {
  fileprivate static let system = AudioObjectID(kAudioObjectSystemObject)
  fileprivate static let unknown = AudioObjectID(kAudioObjectUnknown)

  fileprivate func read<T>(
    _ selector: AudioObjectPropertySelector,
    defaultValue: T,
    qualifierData: UnsafeRawPointer? = nil,
    qualifierSize: UInt32 = 0
  ) throws -> T {
    var address = AudioObjectPropertyAddress(
      mSelector: selector,
      mScope: kAudioObjectPropertyScopeGlobal,
      mElement: kAudioObjectPropertyElementMain
    )
    var size: UInt32 = 0
    var status = AudioObjectGetPropertyDataSize(
      self, &address, qualifierSize, qualifierData, &size)
    guard status == noErr else {
      throw SpikeFailure.status("read property size", status)
    }
    var value = defaultValue
    status = withUnsafeMutablePointer(to: &value) { pointer in
      AudioObjectGetPropertyData(
        self, &address, qualifierSize, qualifierData, &size, pointer)
    }
    guard status == noErr else {
      throw SpikeFailure.status("read property", status)
    }
    return value
  }

  fileprivate static func processObject(for pid: pid_t) throws -> AudioObjectID {
    var suppliedPID = pid
    let object = try withUnsafePointer(to: &suppliedPID) { pointer in
      try AudioObjectID.system.read(
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        defaultValue: AudioObjectID.unknown,
        qualifierData: pointer,
        qualifierSize: UInt32(MemoryLayout<pid_t>.size)
      )
    }
    guard object != .unknown else {
      throw SpikeFailure.message("selected process has no Core Audio object")
    }
    return object
  }

  fileprivate func tapFormat() throws -> AudioStreamBasicDescription {
    try read(kAudioTapPropertyFormat, defaultValue: AudioStreamBasicDescription())
  }
}

@available(macOS 14.2, *)
private final class ProcessTapCapture {
  private var tapID = AudioObjectID.unknown
  private var aggregateID = AudioObjectID.unknown
  private var ioProcID: AudioDeviceIOProcID?
  private var file: AVAudioFile?
  private let queue = DispatchQueue(label: "ai.ax.echowall.process-tap-spike")
  private var callbackError = false

  func start(processID: pid_t, output: URL) throws {
    let processObject = try AudioObjectID.processObject(for: processID)
    let tap = CATapDescription(stereoMixdownOfProcesses: [processObject])
    tap.uuid = UUID()
    tap.isPrivate = true
    tap.muteBehavior = .unmuted
    var createdTap = AudioObjectID.unknown
    var status = AudioHardwareCreateProcessTap(tap, &createdTap)
    guard status == noErr, createdTap != .unknown else {
      throw SpikeFailure.status("create process tap", status)
    }
    tapID = createdTap

    var streamDescription = try tapID.tapFormat()
    guard let format = AVAudioFormat(streamDescription: &streamDescription),
      format.channelCount > 0,
      format.sampleRate > 0
    else {
      throw SpikeFailure.message("tap returned an invalid audio format")
    }
    let description: [String: Any] = [
      kAudioAggregateDeviceNameKey: "EchoWall Process Tap Spike",
      kAudioAggregateDeviceUIDKey: UUID().uuidString,
      kAudioAggregateDeviceIsPrivateKey: true,
      kAudioAggregateDeviceIsStackedKey: false,
      kAudioAggregateDeviceTapAutoStartKey: true,
      kAudioAggregateDeviceTapListKey: [
        [
          kAudioSubTapDriftCompensationKey: true,
          kAudioSubTapUIDKey: tap.uuid.uuidString,
        ]
      ],
    ]
    var createdAggregate = AudioObjectID.unknown
    status = AudioHardwareCreateAggregateDevice(
      description as CFDictionary, &createdAggregate)
    guard status == noErr, createdAggregate != .unknown else {
      throw SpikeFailure.status("create aggregate device", status)
    }
    aggregateID = createdAggregate

    file = try AVAudioFile(
      forWriting: output,
      settings: format.settings,
      commonFormat: .pcmFormatFloat32,
      interleaved: format.isInterleaved
    )
    status = AudioDeviceCreateIOProcIDWithBlock(
      &ioProcID,
      aggregateID,
      queue
    ) { [weak self] _, inputData, _, _, _ in
      guard let self, let file = self.file else { return }
      guard
        let buffer = AVAudioPCMBuffer(
          pcmFormat: format,
          bufferListNoCopy: inputData,
          deallocator: nil
        )
      else {
        self.callbackError = true
        return
      }
      do {
        try file.write(from: buffer)
      } catch {
        self.callbackError = true
      }
    }
    guard status == noErr else {
      throw SpikeFailure.status("create aggregate IOProc", status)
    }
    status = AudioDeviceStart(aggregateID, ioProcID)
    guard status == noErr else {
      throw SpikeFailure.status("start aggregate device", status)
    }
  }

  func stop() throws {
    if aggregateID != .unknown {
      let status = AudioDeviceStop(aggregateID, ioProcID)
      guard status == noErr else {
        throw SpikeFailure.status("stop aggregate device", status)
      }
    }
    file = nil
    if callbackError {
      throw SpikeFailure.message("tap callback could not write audio")
    }
    cleanup()
  }

  func cleanup() {
    if aggregateID != .unknown {
      if let ioProcID {
        _ = AudioDeviceDestroyIOProcID(aggregateID, ioProcID)
        self.ioProcID = nil
      }
      _ = AudioHardwareDestroyAggregateDevice(aggregateID)
      aggregateID = .unknown
    }
    if tapID != .unknown {
      _ = AudioHardwareDestroyProcessTap(tapID)
      tapID = .unknown
    }
    file = nil
  }

  deinit {
    cleanup()
  }
}

private func argument(_ name: String) throws -> String {
  guard let index = CommandLine.arguments.firstIndex(of: name),
    CommandLine.arguments.indices.contains(index + 1)
  else {
    throw SpikeFailure.message("missing required argument")
  }
  return CommandLine.arguments[index + 1]
}

private func toneAmplitude(
  samples: UnsafePointer<Float>,
  frames: Int,
  channels: Int,
  sampleRate: Double,
  frequency: Double
) -> Double {
  let trim = min(Int(sampleRate / 4), frames / 4)
  let usable = frames - trim * 2
  guard usable > 0 else { return 0 }
  var sinProjection = 0.0
  var cosProjection = 0.0
  for offset in 0..<usable {
    let frame = trim + offset
    var mono = 0.0
    for channel in 0..<channels {
      mono += Double(samples[frame * channels + channel])
    }
    mono /= Double(channels)
    let phase = 2 * Double.pi * frequency * Double(offset) / sampleRate
    sinProjection += mono * sin(phase)
    cosProjection += mono * cos(phase)
  }
  return 2 * hypot(sinProjection, cosProjection) / Double(usable)
}

private func analyze(_ url: URL) throws -> (frames: Int, selected: Double, leak: Double) {
  let file = try AVAudioFile(forReading: url)
  let format = file.processingFormat
  let frameCount = Int(file.length)
  guard frameCount > 0,
    frameCount <= Int(format.sampleRate * 1_800),
    let buffer = AVAudioPCMBuffer(
      pcmFormat: format,
      frameCapacity: AVAudioFrameCount(frameCount)
    )
  else {
    throw SpikeFailure.message("captured audio length is invalid")
  }
  try file.read(into: buffer)
  guard let channelData = buffer.floatChannelData else {
    throw SpikeFailure.message("captured audio is not Float32")
  }
  let frames = Int(buffer.frameLength)
  let channels = Int(format.channelCount)
  var interleaved = [Float](repeating: 0, count: frames * channels)
  for frame in 0..<frames {
    for channel in 0..<channels {
      interleaved[frame * channels + channel] = channelData[channel][frame]
    }
  }
  return interleaved.withUnsafeBufferPointer { pointer in
    (
      frames,
      toneAmplitude(
        samples: pointer.baseAddress!, frames: frames, channels: channels,
        sampleRate: format.sampleRate, frequency: 997),
      toneAmplitude(
        samples: pointer.baseAddress!, frames: frames, channels: channels,
        sampleRate: format.sampleRate, frequency: 443)
    )
  }
}

@main
private enum ProcessTapSpikeMain {
  static func main() {
    do {
      guard #available(macOS 14.2, *) else {
        throw SpikeFailure.message("macOS 14.2 is required")
      }
      guard CommandLine.arguments.count == 7,
        let pid = pid_t(try argument("--pid")),
        let duration = UInt64(try argument("--duration")),
        (3...1_800).contains(duration)
      else {
        throw SpikeFailure.message("invalid arguments")
      }
      let output = URL(fileURLWithPath: try argument("--output"))
      let capture = ProcessTapCapture()
      try capture.start(processID: pid, output: output)
      Thread.sleep(forTimeInterval: Double(duration))
      try capture.stop()
      let metrics = try analyze(output)
      print(
        String(
          format:
            "{\"duration_seconds\":%llu,\"frames\":%d,\"selected_997\":%.6f,\"unrelated_443\":%.6f}",
          duration, metrics.frames, metrics.selected, metrics.leak
        )
      )
      guard metrics.selected >= 0.01, metrics.selected >= metrics.leak * 4 else {
        throw SpikeFailure.message("selected process audio was missing or contaminated")
      }
    } catch {
      FileHandle.standardError.write(Data("ProcessTapSpike: \(error)\n".utf8))
      exit(1)
    }
  }
}
