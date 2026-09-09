import CoreML
import XCTest
@testable import SpeakerKitOffline
@testable import EchoWallDiarizationWorker

final class TailContextTests: XCTestCase {
    private let rate = 16_000
    private let chunk = 480_000
    private let overlap = 144_000

    private func plan(_ frames: Int, _ policy: SpeakerKitTailContextPolicy, overlap: Int = 144_000) -> [SpeakerChunkPlan] {
        SpeakerChunkPlan.make(audioFrames: frames, maximumChunkFrames: chunk,
                              strideOffsetFrames: overlap, tailContextPolicy: policy)
    }

    func testPresetSelectionIsExplicitAndDefaultsKeepV1() {
        XCTAssertEqual(PyannoteDiarizationOptions().tailContextPolicy, .paddedV1)
        XCTAssertEqual(SpeakerKitWorkerPreset(rawValue: "speakerkit-pyannote-v3-exclusive-v1")?.tailContextPolicy, .paddedV1)
        XCTAssertEqual(SpeakerKitWorkerPreset(rawValue: "speakerkit-pyannote-v3-exclusive-tail-context-v2")?.tailContextPolicy, .endAlignedV2)
        XCTAssertEqual(SpeakerKitWorkerPreset.allCases.count, 2)
        for unsupported in ["speakerkit-pyannote-v3-exclusive-v2", "speakerkit-pyannote-v3-exclusive-tail-context-v1", "fluid-step015-embed040-v1", ""] {
            XCTAssertNil(SpeakerKitWorkerPreset(rawValue: unsupported))
        }
    }

    func testShortAudioAndExactFullChunksKeepLegacyGeometry() {
        for frames in [0, 1, chunk - 1, chunk, 816_000, 1_152_000] {
            XCTAssertEqual(plan(frames, .endAlignedV2), plan(frames, .paddedV1))
            XCTAssertTrue(plan(frames, .endAlignedV2).allSatisfy { $0.exactStartFrame == nil })
        }
        XCTAssertEqual(plan(0, .paddedV1), [])
        XCTAssertEqual(plan(chunk - 1, .endAlignedV2).map(\.sourceFrames), [0..<(chunk - 1)])
        XCTAssertEqual(plan(chunk, .endAlignedV2).map(\.sourceFrames), [0..<chunk])
        XCTAssertEqual(plan(816_000, .endAlignedV2).map(\.sourceFrames), [0..<480_000, 336_000..<816_000])
        XCTAssertEqual(plan(1_152_000, .endAlignedV2).map(\.sourceFrames), [0..<480_000, 336_000..<816_000, 672_000..<1_152_000])
    }

    func testV1PartialTailRetainsExactHistoricalRanges() {
        XCTAssertEqual(plan(480_001, .paddedV1).map(\.sourceFrames), [0..<480_000, 336_000..<480_001])
        XCTAssertEqual(plan(816_001, .paddedV1).map(\.sourceFrames), [0..<480_000, 336_000..<816_000, 672_000..<816_001])
        let historical = plan(11_618_761, .paddedV1)
        XCTAssertEqual(historical.count, 35)
        XCTAssertEqual(historical.last?.sourceFrames, 11_424_000..<11_618_761)
        XCTAssertTrue(historical.allSatisfy { $0.exactStartFrame == nil })
    }

    func testOneFrameBeyondFullChunkUsesExactEndAlignedSource() {
        let firstTail = plan(480_001, .endAlignedV2)
        XCTAssertEqual(firstTail.map(\.sourceFrames), [0..<480_000, 1..<480_001])
        XCTAssertEqual(firstTail.map(\.exactStartFrame), [nil, 1])
        let laterTail = plan(816_001, .endAlignedV2)
        XCTAssertEqual(laterTail.map(\.sourceFrames), [0..<480_000, 336_000..<816_000, 336_001..<816_001])
        XCTAssertEqual(laterTail.map(\.exactStartFrame), [nil, nil, 336_001])
    }

    func testMixed05FinalContextLeavesEveryEarlierChunkUnchanged() {
        let old = plan(11_618_761, .paddedV1)
        let new = plan(11_618_761, .endAlignedV2)
        XCTAssertEqual(Array(new.dropLast()), Array(old.dropLast()))
        XCTAssertEqual(new.count, 35)
        XCTAssertEqual(new.last?.sourceFrames, 11_138_761..<11_618_761)
        XCTAssertEqual(new.last?.exactStartFrame, 11_138_761)
        XCTAssertEqual(new[new.count - 2].sourceFrames, 11_088_000..<11_568_000)
    }

    func testChunkCoverageHasNoGapsExtraSamplesOrArtificialPaddingWhenFullContextExists() {
        for offset in [0, overlap] {
            for frames in [1, chunk - 1, chunk, chunk + 1, 815_999, 816_000, 816_001, 1_151_999, 11_618_761] {
                let chunks = plan(frames, .endAlignedV2, overlap: offset)
                XCTAssertEqual(chunks.first?.sourceFrames.lowerBound, 0)
                XCTAssertEqual(chunks.last?.sourceFrames.upperBound, frames)
                for (left, right) in zip(chunks, chunks.dropFirst()) {
                    XCTAssertLessThanOrEqual(right.sourceFrames.lowerBound, left.sourceFrames.upperBound)
                    XCTAssertGreaterThanOrEqual(right.sourceFrames.lowerBound, left.sourceFrames.lowerBound)
                }
                for item in chunks {
                    XCTAssertGreaterThanOrEqual(item.sourceFrames.lowerBound, 0)
                    XCTAssertLessThanOrEqual(item.sourceFrames.upperBound, frames)
                    XCTAssertEqual(item.sourceFrames.count, min(frames, chunk))
                }
            }
        }
    }

    func testSelectedWaveformIsBitExactSourceSuffix() throws {
        let samples = (0..<(chunk + 173)).map { Float($0) }
        let final = try XCTUnwrap(plan(samples.count, .endAlignedV2).last)
        let selected = Array(samples[final.sourceFrames])
        let modelInput = try XCTUnwrap(AudioProcessor.padOrTrimAudio(fromArray: selected, toLength: chunk))
        let pointer = modelInput.dataPointer.assumingMemoryBound(to: Float.self)
        XCTAssertEqual(Array(UnsafeBufferPointer(start: pointer, count: chunk)).map(\.bitPattern),
                       Array(samples.suffix(chunk)).map(\.bitPattern))
    }

    func testShortInputStillUsesOnlyNecessaryTrailingZeroPadding() throws {
        let samples: [Float] = [0.25, -0.5, 0.75]
        let selected = try XCTUnwrap(plan(samples.count, .endAlignedV2).first)
        XCTAssertNil(selected.exactStartFrame)
        let input = try XCTUnwrap(AudioProcessor.padOrTrimAudio(fromArray: samples, toLength: chunk))
        let pointer = input.dataPointer.assumingMemoryBound(to: Float.self)
        XCTAssertEqual(Array(UnsafeBufferPointer(start: pointer, count: samples.count)), samples)
        XCTAssertTrue(UnsafeBufferPointer(start: pointer.advanced(by: samples.count), count: chunk - samples.count).allSatisfy { $0 == 0 })
    }

    func testFractionalChunkOriginsStayInSourceFramesThroughWindowProjection() {
        let origin = SpeakerSourcePosition(frame: 11_138_761, sampleRate: rate)
        XCTAssertEqual(origin.seconds, 696.1725625, accuracy: 1e-12)
        for index in 0...20 {
            let window = origin.windowOrigin(index: index, strideSeconds: 1)
            XCTAssertEqual(window.frame, 11_138_761 + index * rate)
            XCTAssertEqual(window.seconds, 696.1725625 + Double(index), accuracy: 1e-12)
            XCTAssertLessThanOrEqual(window.frame + 10 * rate, 11_618_761)
        }
        XCTAssertEqual(origin.windowOrigin(index: 20, strideSeconds: 1).frame + 10 * rate, 11_618_761)
        XCTAssertEqual(SpeakerSourcePosition(frame: 1, sampleRate: rate).windowOrigin(index: 20, strideSeconds: 1).frame, 320_001)
    }

    func testReconstructionUsesExactOriginInsteadOfRoundedOrNominalChunkSeconds() {
        let legacy = embedding(windowIndex: 714)
        let actual = SpeakerSourcePosition(frame: 11_138_761, sampleRate: rate)
        let adjusted = embedding(windowIndex: 714, origin: actual)
        XCTAssertEqual(legacy.diarizationFrameOffset(framesPerWindow: 589, windowSeconds: 10), 42_054)
        XCTAssertEqual(adjusted.diarizationFrameOffset(framesPerWindow: 589, windowSeconds: 10), 41_004)
        XCTAssertEqual(adjusted.timelineStartSeconds, 696.1725625, accuracy: 1e-12)
        XCTAssertLessThan(adjusted.timelineStartSeconds, embedding(windowIndex: 697).timelineStartSeconds)
        let rounded = embedding(windowIndex: 696)
        XCTAssertNotEqual(adjusted.diarizationFrameOffset(framesPerWindow: 589, windowSeconds: 10),
                          rounded.diarizationFrameOffset(framesPerWindow: 589, windowSeconds: 10))
    }

    func testLegacyFrameProjectionCharacterization() {
        let golden = [(0, 0), (1, 58), (10, 589), (30, 1_767), (713, 41_995), (714, 42_054), (18_000, 1_060_200)]
        for (seconds, expectedFrame) in golden {
            let old = embedding(windowIndex: seconds)
            XCTAssertEqual(old.diarizationFrameOffset(framesPerWindow: 589, windowSeconds: 10), expectedFrame)
            XCTAssertEqual(old.timelineStartSeconds, Double(seconds))
        }
    }

    func testWindowPaddingKeepsExistingStopBoundary() {
        let legacy = SpeakerChunkPlan.make(audioFrames: 500_000, maximumChunkFrames: chunk,
            strideOffsetFrames: overlap, windowPadding: 30_000, tailContextPolicy: .paddedV1)
        let newer = SpeakerChunkPlan.make(audioFrames: 500_000, maximumChunkFrames: chunk,
            strideOffsetFrames: overlap, windowPadding: 30_000, tailContextPolicy: .endAlignedV2)
        XCTAssertEqual(legacy, newer)
        XCTAssertEqual(legacy.map(\.sourceFrames), [0..<480_000])
    }

    private func embedding(windowIndex: Int, origin: SpeakerSourcePosition? = nil) -> SpeakerEmbedding {
        SpeakerEmbedding(embedding: [1, 0], activeFrames: [1], windowIndex: windowIndex,
                         speakerIndex: 0, nonOverlappedFrameRatio: 1, exactWindowOrigin: origin)
    }
}
