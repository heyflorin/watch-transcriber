// Narrow Core Audio process-tap bridge for EchoWall Meeting capture.
// Lifecycle shape adapted from insidegui/AudioCap (BSD-2-Clause); EchoWall
// owns process selection, recording clocks, durable segments, and recovery.

import AVFoundation
import AudioToolbox
import CoreAudio
import Darwin
import Foundation

typealias AudioCallback =
  @convention(c) (
    UnsafeMutableRawPointer?, UnsafePointer<Float>?, UInt32, UInt32, Double, UInt64
  ) -> Void

extension AudioObjectID {
  fileprivate static let system = AudioObjectID(kAudioObjectSystemObject)
  fileprivate static let unknown = AudioObjectID(kAudioObjectUnknown)

  fileprivate func read<T>(
    _ selector: AudioObjectPropertySelector,
    defaultValue: T,
    qualifierData: UnsafeRawPointer? = nil,
    qualifierSize: UInt32 = 0
  ) -> T? {
    var address = AudioObjectPropertyAddress(
      mSelector: selector,
      mScope: kAudioObjectPropertyScopeGlobal,
      mElement: kAudioObjectPropertyElementMain
    )
    var size: UInt32 = 0
    guard
      AudioObjectGetPropertyDataSize(
        self, &address, qualifierSize, qualifierData, &size) == noErr
    else { return nil }
    var value = defaultValue
    let status = withUnsafeMutablePointer(to: &value) { pointer in
      AudioObjectGetPropertyData(
        self, &address, qualifierSize, qualifierData, &size, pointer)
    }
    return status == noErr ? value : nil
  }

  fileprivate static func processObject(for pid: pid_t) -> AudioObjectID? {
    var suppliedPID = pid
    return withUnsafePointer(to: &suppliedPID) { pointer in
      AudioObjectID.system.read(
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        defaultValue: AudioObjectID.unknown,
        qualifierData: pointer,
        qualifierSize: UInt32(MemoryLayout<pid_t>.size)
      )
    }
  }

  fileprivate static func processObjects() -> [AudioObjectID] {
    var address = AudioObjectPropertyAddress(
      mSelector: kAudioHardwarePropertyProcessObjectList,
      mScope: kAudioObjectPropertyScopeGlobal,
      mElement: kAudioObjectPropertyElementMain
    )
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(.system, &address, 0, nil, &size) == noErr else {
      return []
    }
    var values = [AudioObjectID](
      repeating: .unknown,
      count: Int(size) / MemoryLayout<AudioObjectID>.size
    )
    guard AudioObjectGetPropertyData(.system, &address, 0, nil, &size, &values) == noErr else {
      return []
    }
    return values.filter { $0 != .unknown }
  }

  fileprivate func processPID() -> pid_t? {
    read(kAudioProcessPropertyPID, defaultValue: pid_t(-1)).flatMap { $0 > 0 ? $0 : nil }
  }

  fileprivate func processBundleID() -> String? {
    guard let value: CFString = read(kAudioProcessPropertyBundleID, defaultValue: "" as CFString)
    else { return nil }
    let string = value as String
    return string.isEmpty ? nil : string
  }

  fileprivate func tapFormat() -> AudioStreamBasicDescription? {
    read(kAudioTapPropertyFormat, defaultValue: AudioStreamBasicDescription())
  }
}

private func decodeCString(_ pointer: UnsafePointer<CChar>?) -> String? {
  guard let pointer else { return nil }
  var bytes = [UInt8]()
  bytes.reserveCapacity(128)
  for index in 0..<512 {
    let value = pointer[index]
    if value == 0 { break }
    bytes.append(UInt8(bitPattern: value))
  }
  guard !bytes.isEmpty, bytes.count < 512 else { return nil }
  return String(bytes: bytes, encoding: .utf8)
}

private func isDescendantProcess(_ candidate: pid_t, of selected: pid_t) -> Bool {
  guard candidate > 1, selected > 1 else { return false }
  var current = candidate
  var visited = Set<pid_t>()
  for _ in 0..<32 {
    guard current > 1, visited.insert(current).inserted else { return false }
    if current == selected { return true }
    var info = proc_bsdinfo()
    let size = proc_pidinfo(
      current,
      PROC_PIDTBSDINFO,
      0,
      &info,
      Int32(MemoryLayout<proc_bsdinfo>.size)
    )
    guard size == Int32(MemoryLayout<proc_bsdinfo>.size) else { return false }
    current = pid_t(info.pbi_ppid)
  }
  return false
}

@available(macOS 14.2, *)
private final class EchoWallProcessTap {
  private let callback: AudioCallback
  private let context: UnsafeMutableRawPointer?
  private let queue = DispatchQueue(label: "ai.ax.echowall.process-tap")
  private var tapID = AudioObjectID.unknown
  private var aggregateID = AudioObjectID.unknown
  private var ioProcID: AudioDeviceIOProcID?
  private var running = false

  init(
    processID: pid_t, bundleID: String, context: UnsafeMutableRawPointer?,
    callback: @escaping AudioCallback
  ) throws {
    self.callback = callback
    self.context = context
    var prepared = false
    defer {
      if !prepared { cleanup() }
    }

    let selectedPrefix = bundleID + "."
    var objects = AudioObjectID.processObjects().filter { object in
      object.processPID().isSome { process in
        process == processID || isDescendantProcess(process, of: processID)
      }
        || object.processBundleID().isSome { value in
          value == bundleID || value.hasPrefix(selectedPrefix)
        }
    }
    if objects.isEmpty, let exact = AudioObjectID.processObject(for: processID), exact != .unknown {
      objects = [exact]
    }
    objects = Array(Set(objects)).sorted()
    guard !objects.isEmpty else { throw ProcessTapError.processUnavailable }

    let tap = CATapDescription(stereoMixdownOfProcesses: objects)
    tap.uuid = UUID()
    tap.isPrivate = true
    tap.muteBehavior = .unmuted
    var createdTap = AudioObjectID.unknown
    guard AudioHardwareCreateProcessTap(tap, &createdTap) == noErr,
      createdTap != .unknown
    else { throw ProcessTapError.tapCreation }
    tapID = createdTap

    guard var streamDescription = tapID.tapFormat(),
      streamDescription.mSampleRate > 0,
      streamDescription.mChannelsPerFrame > 0,
      let format = AVAudioFormat(streamDescription: &streamDescription),
      format.commonFormat == .pcmFormatFloat32
    else { throw ProcessTapError.invalidFormat }
    // A process tap is itself a complete aggregate input. Binding the private
    // aggregate to the current default output adds an unrelated device clock
    // and makes capture fail when an HDMI/AirPlay output disappears. Keep this
    // aggregate tap-only so selected-process capture survives output changes.
    let description: [String: Any] = [
      kAudioAggregateDeviceNameKey: "EchoWall Private Process Tap",
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
    guard
      AudioHardwareCreateAggregateDevice(
        description as CFDictionary, &createdAggregate) == noErr,
      createdAggregate != .unknown
    else { throw ProcessTapError.aggregateCreation }
    aggregateID = createdAggregate

    let callback = self.callback
    let context = self.context
    let sampleRate = format.sampleRate
    let channels = Int(format.channelCount)
    let ioBlock: AudioDeviceIOBlock = { _, inputData, inputTime, _, _ in
      guard
        let buffer = AVAudioPCMBuffer(
          pcmFormat: format,
          bufferListNoCopy: inputData,
          deallocator: nil
        ), let channelData = buffer.floatChannelData
      else { return }
      let frames = Int(buffer.frameLength)
      guard frames > 0 else { return }
      var interleaved = [Float](repeating: 0, count: frames * channels)
      if format.isInterleaved {
        interleaved.withUnsafeMutableBufferPointer { output in
          output.baseAddress?.update(from: channelData[0], count: output.count)
        }
      } else {
        for frame in 0..<frames {
          for channel in 0..<channels {
            interleaved[frame * channels + channel] = channelData[channel][frame]
          }
        }
      }
      let hostTime =
        inputTime.pointee.mHostTime > 0
        ? AudioConvertHostTimeToNanos(inputTime.pointee.mHostTime) : 0
      interleaved.withUnsafeBufferPointer { samples in
        callback(
          context,
          samples.baseAddress,
          UInt32(frames),
          UInt32(channels),
          sampleRate,
          hostTime
        )
      }
    }
    guard
      AudioDeviceCreateIOProcIDWithBlock(
        &ioProcID,
        aggregateID,
        queue,
        ioBlock
      ) == noErr
    else { throw ProcessTapError.ioCreation }
    prepared = true
  }

  func start() throws {
    guard !running else { return }
    guard AudioDeviceStart(aggregateID, ioProcID) == noErr else {
      throw ProcessTapError.ioStart
    }
    running = true
  }

  func pause() throws {
    guard running else { return }
    guard AudioDeviceStop(aggregateID, ioProcID) == noErr else {
      throw ProcessTapError.ioStop
    }
    running = false
  }

  func cleanup() {
    if aggregateID != .unknown {
      if running {
        _ = AudioDeviceStop(aggregateID, ioProcID)
        running = false
      }
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
  }

  deinit { cleanup() }
}

private enum ProcessTapError: Error {
  case processUnavailable
  case tapCreation
  case invalidFormat
  case aggregateCreation
  case ioCreation
  case ioStart
  case ioStop
}

extension Optional {
  fileprivate func isSome(_ predicate: (Wrapped) -> Bool) -> Bool {
    map(predicate) ?? false
  }
}

@_cdecl("echowall_process_tap_create")
func echowallProcessTapCreate(
  processID: Int32,
  bundleID: UnsafePointer<CChar>?,
  context: UnsafeMutableRawPointer?,
  callback: AudioCallback?,
  output: UnsafeMutablePointer<UnsafeMutableRawPointer?>?
) -> Int32 {
  guard #available(macOS 14.2, *) else { return 1 }
  guard processID > 0,
    let bundleID = decodeCString(bundleID),
    !bundleID.isEmpty,
    bundleID.utf8.count <= 512,
    let callback,
    let output
  else { return 2 }
  do {
    let tap = try EchoWallProcessTap(
      processID: processID,
      bundleID: bundleID,
      context: context,
      callback: callback
    )
    try tap.start()
    output.pointee = Unmanaged.passRetained(tap).toOpaque()
    return 0
  } catch {
    return 3
  }
}

@_cdecl("echowall_process_tap_pause")
func echowallProcessTapPause(_ pointer: UnsafeMutableRawPointer?) -> Int32 {
  guard #available(macOS 14.2, *), let pointer else { return 1 }
  do {
    try Unmanaged<EchoWallProcessTap>.fromOpaque(pointer).takeUnretainedValue().pause()
    return 0
  } catch {
    return 2
  }
}

@_cdecl("echowall_process_tap_resume")
func echowallProcessTapResume(_ pointer: UnsafeMutableRawPointer?) -> Int32 {
  guard #available(macOS 14.2, *), let pointer else { return 1 }
  do {
    try Unmanaged<EchoWallProcessTap>.fromOpaque(pointer).takeUnretainedValue().start()
    return 0
  } catch {
    return 2
  }
}

@_cdecl("echowall_process_tap_destroy")
func echowallProcessTapDestroy(_ pointer: UnsafeMutableRawPointer?) {
  guard #available(macOS 14.2, *), let pointer else { return }
  let tap = Unmanaged<EchoWallProcessTap>.fromOpaque(pointer).takeRetainedValue()
  tap.cleanup()
}
