import Foundation

/// Versioned treatment of the segmenter's final partial chunk.
public enum SpeakerKitTailContextPolicy: Equatable, Sendable {
    /// Original fixed-stride chunks, with zero padding of a partial final chunk.
    case paddedV1
    /// For audio at least one chunk long, use the exact final full chunk of real audio.
    /// Shorter audio still requires the original model-input padding.
    case endAlignedV2
}

struct SpeakerChunkPlan: Equatable, Sendable {
    let sourceFrames: Range<Int>
    /// Present only when the final chunk departs from the original fixed-stride grid.
    let exactStartFrame: Int?

    static func make(
        audioFrames: Int,
        maximumChunkFrames: Int,
        strideOffsetFrames: Int,
        windowPadding: Int = 0,
        tailContextPolicy: SpeakerKitTailContextPolicy = .paddedV1
    ) -> [Self] {
        precondition(audioFrames >= 0 && maximumChunkFrames > 0)
        precondition(strideOffsetFrames >= 0 && strideOffsetFrames < maximumChunkFrames)
        precondition(windowPadding >= 0 && windowPadding <= audioFrames)
        var result: [Self] = []
        var end = 0
        // Preserve the v1 loop and its windowPadding boundary exactly.
        while end < audioFrames - windowPadding {
            let start = max(end - strideOffsetFrames, 0)
            end = start + min(maximumChunkFrames, audioFrames - start)
            result.append(Self(sourceFrames: start..<end, exactStartFrame: nil))
        }
        if tailContextPolicy == .endAlignedV2,
           audioFrames >= maximumChunkFrames,
           let last = result.last,
           last.sourceFrames.count < maximumChunkFrames {
            let start = audioFrames - maximumChunkFrames
            result[result.count - 1] = Self(sourceFrames: start..<audioFrames, exactStartFrame: start)
        }
        return result
    }
}

/// A source position stays in integer PCM frames until diarization-frame projection.
struct SpeakerSourcePosition: Equatable, Sendable {
    let frame: Int
    let sampleRate: Int

    init(frame: Int, sampleRate: Int) {
        precondition(frame >= 0 && sampleRate > 0)
        self.frame = frame
        self.sampleRate = sampleRate
    }

    func windowOrigin(index: Int, strideSeconds: Float) -> Self {
        precondition(index >= 0 && strideSeconds.isFinite && strideSeconds >= 0)
        let relativeFrames = Int((Double(index) * Double(strideSeconds) * Double(sampleRate)).rounded())
        return Self(frame: frame + relativeFrames, sampleRate: sampleRate)
    }

    var seconds: Double { Double(frame) / Double(sampleRate) }

    func diarizationFrameOffset(framesPerWindow: Int, windowSeconds: Float) -> Int {
        precondition(framesPerWindow > 0 && windowSeconds.isFinite && windowSeconds > 0)
        return Int(Double(framesPerWindow) / Double(windowSeconds) * seconds)
    }
}

/// Identifies one model observation even when another window has the same
/// absolute start or projects onto the same diarization frame.
struct SpeakerWindowObservation: Hashable, Comparable, Sendable {
    let clipIndex: Int
    let chunkIndex: Int
    let windowIndex: Int

    static func < (left: Self, right: Self) -> Bool {
        (left.clipIndex, left.chunkIndex, left.windowIndex)
            < (right.clipIndex, right.chunkIndex, right.windowIndex)
    }
}
