import AppKit
import AudioToolbox
import AVFoundation
import CoreAudio
import Foundation

enum FixtureFailure: Error, CustomStringConvertible {
  case argument(String)
  case audio(String, OSStatus)

  var description: String {
    switch self {
    case let .argument(message): message
    case let .audio(operation, status): "\(operation) failed with OSStatus \(status)"
    }
  }
}

func argument(_ name: String) throws -> String {
  guard let index = CommandLine.arguments.firstIndex(of: name),
        CommandLine.arguments.indices.contains(index + 1)
  else {
    throw FixtureFailure.argument("missing \(name)")
  }
  return CommandLine.arguments[index + 1]
}

func audioDevice(named target: String) throws -> AudioDeviceID {
  var devicesAddress = AudioObjectPropertyAddress(
    mSelector: kAudioHardwarePropertyDevices,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain
  )
  var byteCount: UInt32 = 0
  var status = AudioObjectGetPropertyDataSize(
    AudioObjectID(kAudioObjectSystemObject),
    &devicesAddress,
    0,
    nil,
    &byteCount
  )
  guard status == noErr else { throw FixtureFailure.audio("list audio devices", status) }
  var devices = [AudioDeviceID](
    repeating: kAudioObjectUnknown,
    count: Int(byteCount) / MemoryLayout<AudioDeviceID>.size
  )
  status = devices.withUnsafeMutableBytes { bytes in
    AudioObjectGetPropertyData(
      AudioObjectID(kAudioObjectSystemObject),
      &devicesAddress,
      0,
      nil,
      &byteCount,
      bytes.baseAddress!
    )
  }
  guard status == noErr else { throw FixtureFailure.audio("read audio devices", status) }

  for device in devices {
    var nameAddress = AudioObjectPropertyAddress(
      mSelector: kAudioObjectPropertyName,
      mScope: kAudioObjectPropertyScopeGlobal,
      mElement: kAudioObjectPropertyElementMain
    )
    var name: CFString = "" as CFString
    var nameSize = UInt32(MemoryLayout<CFString>.size)
    status = withUnsafeMutablePointer(to: &name) { pointer in
      AudioObjectGetPropertyData(device, &nameAddress, 0, nil, &nameSize, pointer)
    }
    if status == noErr, name as String == target {
      return device
    }
  }
  throw FixtureFailure.argument("audio device not found: \(target)")
}

func run() throws {
  let deviceName = try argument("--device")
  guard let frequency = Double(try argument("--frequency")), frequency >= 20, frequency <= 20_000
  else {
    throw FixtureFailure.argument("frequency must be between 20 and 20000 Hz")
  }
  guard let duration = Double(try argument("--duration")), duration > 0, duration <= 7_300
  else {
    throw FixtureFailure.argument("duration must be between 0 and 7300 seconds")
  }

  let application = NSApplication.shared
  application.setActivationPolicy(.accessory)
  let window = NSWindow(
    contentRect: NSRect(x: 0, y: 0, width: 2, height: 2),
    styleMask: [.borderless],
    backing: .buffered,
    defer: false
  )
  window.alphaValue = 0.01
  window.ignoresMouseEvents = true
  window.orderFrontRegardless()
  let engine = AVAudioEngine()
  let player = AVAudioPlayerNode()
  engine.attach(player)
  let device = try audioDevice(named: deviceName)
  guard let audioUnit = engine.outputNode.audioUnit else {
    throw FixtureFailure.argument("AVAudioEngine output unit is unavailable")
  }
  var selectedDevice = device
  let selectionStatus = AudioUnitSetProperty(
    audioUnit,
    kAudioOutputUnitProperty_CurrentDevice,
    kAudioUnitScope_Global,
    0,
    &selectedDevice,
    UInt32(MemoryLayout<AudioDeviceID>.size)
  )
  guard selectionStatus == noErr else {
    throw FixtureFailure.audio("select output device", selectionStatus)
  }

  let sampleRate = 48_000.0
  let channels: AVAudioChannelCount = 2
  guard let format = AVAudioFormat(
    commonFormat: .pcmFormatFloat32,
    sampleRate: sampleRate,
    channels: channels,
    interleaved: false
  ), let buffer = AVAudioPCMBuffer(
    pcmFormat: format,
    frameCapacity: AVAudioFrameCount(sampleRate)
  ), let channelData = buffer.floatChannelData
  else {
    throw FixtureFailure.argument("unable to allocate synthetic audio buffer")
  }
  buffer.frameLength = buffer.frameCapacity
  for frame in 0 ..< Int(buffer.frameLength) {
    let value = Float(sin(2.0 * Double.pi * frequency * Double(frame) / sampleRate) * 0.15)
    for channel in 0 ..< Int(channels) {
      channelData[channel][frame] = value
    }
  }

  engine.connect(player, to: engine.mainMixerNode, format: format)
  player.scheduleBuffer(buffer, at: nil, options: .loops)
  try engine.start()
  player.play()
  RunLoop.current.run(until: Date().addingTimeInterval(duration))
  player.stop()
  engine.stop()
  window.close()
}

do {
  try run()
} catch {
  FileHandle.standardError.write(Data("SyntheticTone: \(error)\n".utf8))
  exit(1)
}
