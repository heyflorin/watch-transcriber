import Foundation

public protocol AudioSampleSource: Sendable {
  var sampleCount: Int { get }
  func copySamples(
    into destination: UnsafeMutablePointer<Float>,
    offset: Int,
    count: Int
  ) throws
}

public struct ArrayAudioSampleSource: AudioSampleSource {
  private let samples: [Float]

  public init(samples: [Float]) {
    self.samples = samples
  }

  public var sampleCount: Int {
    samples.count
  }

  public func copySamples(
    into destination: UnsafeMutablePointer<Float>,
    offset: Int,
    count: Int
  ) throws {
    guard count > 0 else { return }
    guard !samples.isEmpty else { return }
    let clampedOffset = max(0, offset)
    guard clampedOffset < samples.count else { return }
    let available = min(samples.count - clampedOffset, count)
    samples.withUnsafeBufferPointer { pointer in
      destination.update(
        from: pointer.baseAddress!.advanced(by: clampedOffset),
        count: available
      )
    }
  }
}

public struct DiskBackedAudioSampleSource: AudioSampleSource {
  private let mappedData: Data
  private let floatStride = MemoryLayout<Float>.stride

  public let sampleCount: Int

  init(mappedData: Data) {
    self.mappedData = mappedData
    self.sampleCount = mappedData.count / floatStride
  }

  public func copySamples(
    into destination: UnsafeMutablePointer<Float>,
    offset: Int,
    count: Int
  ) throws {
    guard count > 0 else { return }
    guard sampleCount > 0 else { return }
    let clampedOffset = max(0, offset)
    guard clampedOffset < sampleCount else { return }
    let available = min(sampleCount - clampedOffset, count)
    mappedData.withUnsafeBytes { rawBuffer in
      let floatBuffer = rawBuffer.bindMemory(to: Float.self)
      destination.update(
        from: floatBuffer.baseAddress!.advanced(by: clampedOffset),
        count: available
      )
    }
  }

  /// The mapped file is unlinked before this value is returned. Releasing
  /// `mappedData` is sufficient even if the worker terminates abnormally.
  public func cleanup() {}
}
