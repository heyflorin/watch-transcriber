@preconcurrency import AVFoundation
import Foundation
import Testing

@testable import FluidAudioOffline

@Test("disk-backed conversion unlinks private PCM before inference")
func diskBackedConversionUnlinksPrivatePCM() throws {
  let temporaryDirectory = FileManager.default.temporaryDirectory
  let before = try diarizationTemporaryFiles(in: temporaryDirectory)
  let input = temporaryDirectory.appendingPathComponent("echowall-audio-test-\(UUID()).wav")
  defer { try? FileManager.default.removeItem(at: input) }

  let format = try #require(
    AVAudioFormat(
      commonFormat: .pcmFormatFloat32,
      sampleRate: 16_000,
      channels: 1,
      interleaved: false
    )
  )
  let buffer = try #require(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 16_000))
  buffer.frameLength = 16_000
  let samples = try #require(buffer.floatChannelData?.pointee)
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
  #expect(source.sampleCount == 16_000)
  var copied = [Float](repeating: 0, count: 512)
  try copied.withUnsafeMutableBufferPointer { pointer in
    try source.copySamples(into: pointer.baseAddress!, offset: 0, count: pointer.count)
  }
  #expect(copied.contains { abs($0) > 0.001 })
  source.cleanup()
  #expect(try diarizationTemporaryFiles(in: temporaryDirectory) == before)
}

private func diarizationTemporaryFiles(in directory: URL) throws -> Set<String> {
  Set(
    try FileManager.default.contentsOfDirectory(atPath: directory.path)
      .filter { $0.hasPrefix("echowall-diarization-") && $0.hasSuffix(".raw") }
  )
}
