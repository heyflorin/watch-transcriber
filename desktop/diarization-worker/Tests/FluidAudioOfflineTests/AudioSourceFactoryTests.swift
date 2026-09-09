@preconcurrency import AVFoundation
import Foundation
import XCTest

@testable import FluidAudioOffline

final class AudioSourceFactoryTests: XCTestCase {
func testDiskBackedConversionUnlinksPrivatePCM() throws {
  let temporaryDirectory = FileManager.default.temporaryDirectory
  let before = try diarizationTemporaryFiles(in: temporaryDirectory)
  let input = temporaryDirectory.appendingPathComponent("echowall-audio-test-\(UUID()).wav")
  defer { try? FileManager.default.removeItem(at: input) }

  let format = try XCTUnwrap(
    AVAudioFormat(
      commonFormat: .pcmFormatFloat32,
      sampleRate: 16_000,
      channels: 1,
      interleaved: false
    )
  )
  let buffer = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 16_000))
  buffer.frameLength = 16_000
  let samples = try XCTUnwrap(buffer.floatChannelData?.pointee)
  for index in 0..<16_000 {
    samples[index] = Float(sin(Double(index) * 2 * .pi * 440 / 16_000)) * 0.1
  }
  var file: AVAudioFile? = try AVAudioFile(forWriting: input, settings: format.settings)
  try file?.write(from: buffer)
  file = nil

  let (source, _) = try AudioSourceFactory().makeDiskBackedSource(
    from: input,
    targetSampleRate: 16_000
  )
  XCTAssertEqual(source.sampleCount, 16_000)
  var copied = [Float](repeating: 0, count: 512)
  try copied.withUnsafeMutableBufferPointer { pointer in
    try source.copySamples(into: pointer.baseAddress!, offset: 0, count: pointer.count)
  }
  XCTAssertTrue(copied.contains { abs($0) > 0.001 })
  source.cleanup()
  XCTAssertEqual(try diarizationTemporaryFiles(in: temporaryDirectory), before)
}

}

private func diarizationTemporaryFiles(in directory: URL) throws -> Set<String> {
  Set(
    try FileManager.default.contentsOfDirectory(atPath: directory.path)
      .filter { $0.hasPrefix("echowall-diarization-") && $0.hasSuffix(".raw") }
  )
}
