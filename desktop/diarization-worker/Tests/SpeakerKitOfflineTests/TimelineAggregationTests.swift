import XCTest
@testable import SpeakerKitOffline

final class TimelineAggregationTests: XCTestCase {
    func testExactOriginCollisionCountsDistinctPhysicalWindows() {
        let input = collisionFixture(tailStartFrame: 16_000, tailActive: false)
        let actual = reconstruct(input)
        XCTAssertEqual(actual.observationCounts[58], 3)
        XCTAssertEqual(actual.support[0][58], 1.0 / 3.0, accuracy: 1e-7)
        XCTAssertEqual(actual.binary[0][58], 0)
    }

    func testNearOriginCollisionUsesObservationsNotQuantizedFrameIdentity() {
        // 496001 source frames produce a suffix starting at16001. Both the
        // original 1-second window and the suffix project onto frame58.
        let input = collisionFixture(tailStartFrame: 16_001, tailActive: false)
        XCTAssertEqual(input[1].diarizationFrameOffset(framesPerWindow: 589, windowSeconds: 10), 58)
        XCTAssertEqual(input[2].diarizationFrameOffset(framesPerWindow: 589, windowSeconds: 10), 58)
        let actual = reconstruct(input)
        XCTAssertEqual(actual.observationCounts[58], 3)
        XCTAssertEqual(actual.support[0][58], 1.0 / 3.0, accuracy: 1e-7)
        XCTAssertEqual(actual.binary[0][58], 0)
    }

    func testLegacyProjectedStartVotingRemainsUnchanged() {
        let physical = collisionFixture(tailStartFrame: 16_001, tailActive: false)
        let legacy = physical.map {
            SpeakerEmbedding(embedding: $0.embedding, activeFrames: $0.activeFrames,
                windowIndex: $0.windowIndex, speakerIndex: $0.speakerIndex,
                clusterId: $0.clusterId, nonOverlappedFrameRatio: 1,
                exactWindowOrigin: $0.exactWindowOrigin)
        }
        let actual = reconstruct(legacy)
        XCTAssertEqual(actual.observationCounts[58], 2)
        XCTAssertEqual(actual.support[0][58], 0.5)
        XCTAssertEqual(actual.binary[0][58], 1)
    }

    func testAllActiveAliasedWindowsCannotInflateSupportAboveOne() {
        var input = collisionFixture(tailStartFrame: 16_001, tailActive: true)
        input[0] = observation(chunk: 0, window: 0, startFrame: nil, active: [58])
        let actual = reconstruct(input)
        XCTAssertEqual(actual.observationCounts[58], 3)
        XCTAssertEqual(actual.support[0][58], 1)
        XCTAssertTrue(actual.support.flatMap { $0 }.allSatisfy { (0...1).contains($0) })
        XCTAssertEqual(actual.binary[0][58], 1)
    }

    func testTwoSpeakersFromSamePhysicalWindowShareOneDenominator() {
        let one = observation(chunk: 1, window: 0, startFrame: 16_001, active: [0], speaker: 0, cluster: 0)
        let two = observation(chunk: 1, window: 0, startFrame: 16_001, active: [0], speaker: 1, cluster: 1)
        let actual = reconstruct([one, two], speakers: 2, exclusive: false)
        XCTAssertEqual(actual.observationCounts[58], 1)
        XCTAssertEqual(actual.support[0][58], 1)
        XCTAssertEqual(actual.support[1][58], 1)
        XCTAssertEqual(actual.binary[0][58], 1)
        XCTAssertEqual(actual.binary[1][58], 1)
    }

    func testLocalMasksMergedToSameGlobalSpeakerCastOnlyOneVote() {
        let one = observation(chunk: 1, window: 0, startFrame: 16_001, active: [0], speaker: 0)
        let two = observation(chunk: 1, window: 0, startFrame: 16_001, active: [0, 1], speaker: 1)
        let actual = reconstruct([one, two])
        XCTAssertEqual(actual.observationCounts[58], 1)
        XCTAssertEqual(actual.support[0][58], 1)
        XCTAssertEqual(actual.support[0][59], 1)
        XCTAssertTrue(actual.support[0].allSatisfy { (0...1).contains($0) })
    }

    func testAggregationDoesNotDependOnBatchCompletionOrder() {
        let input = collisionFixture(tailStartFrame: 16_001, tailActive: false)
        let expected = reconstruct(input)
        for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
            let actual = reconstruct(order.map { input[$0] })
            XCTAssertEqual(actual.observationCounts, expected.observationCounts)
            XCTAssertEqual(actual.support, expected.support)
            XCTAssertEqual(actual.binary, expected.binary)
        }
    }

    func testEqualTimelineAndLocalSpeakerKeysHavePhysicalWindowTiebreaker() {
        let original = observation(chunk: 0, window: 1, startFrame: nil, active: [0])
        let suffix = observation(chunk: 1, window: 0, startFrame: 16_000, active: [0])
        let laterClip = observation(chunk: 0, window: 1, startFrame: nil, active: [0], clip: 1)
        XCTAssertEqual(original.timelineStartSeconds, suffix.timelineStartSeconds)
        XCTAssertEqual(original.speakerIndex, suffix.speakerIndex)
        let input = [original, suffix, laterClip]
        let expected = input.map(\.observation)
        for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
            let actual = order.map { input[$0] }.sorted(by: SpeakerEmbedding.timelineOrder)
            XCTAssertEqual(actual.map(\.observation), expected)
        }
    }

    func testLegacyEquivalentSortKeysKeepOriginalComparator() {
        let left = SpeakerEmbedding(embedding: [1, 0], activeFrames: [1], windowIndex: 1,
                                    speakerIndex: 0, nonOverlappedFrameRatio: 1)
        let right = SpeakerEmbedding(embedding: [0, 1], activeFrames: [1], windowIndex: 1,
                                     speakerIndex: 0, nonOverlappedFrameRatio: 1)
        XCTAssertFalse(SpeakerEmbedding.timelineOrder(left, right))
        XCTAssertFalse(SpeakerEmbedding.timelineOrder(right, left))
    }

    private func collisionFixture(tailStartFrame: Int, tailActive: Bool) -> [SpeakerEmbedding] {
        [observation(chunk: 0, window: 0, startFrame: nil, active: [200]),
         observation(chunk: 0, window: 1, startFrame: nil, active: [0]),
         observation(chunk: 1, window: 0, startFrame: tailStartFrame, active: tailActive ? [0] : [200])]
    }

    private func observation(chunk: Int, window: Int, startFrame: Int?, active: [Int],
                             speaker: Int = 0, cluster: Int = 0, clip: Int = 0) -> SpeakerEmbedding {
        var mask = Array(repeating: Float(0), count: 589)
        for index in active { mask[index] = 1 }
        return SpeakerEmbedding(embedding: [1, 0], activeFrames: mask,
            windowIndex: chunk * 21 + window, speakerIndex: speaker, clusterId: cluster,
            nonOverlappedFrameRatio: 1,
            exactWindowOrigin: startFrame.map { SpeakerSourcePosition(frame: $0, sampleRate: 16_000) },
            observation: SpeakerWindowObservation(clipIndex: clip, chunkIndex: chunk, windowIndex: window))
    }

    private func reconstruct(_ input: [SpeakerEmbedding], speakers: Int = 1, exclusive: Bool = true) -> SpeakerTimelineAggregation {
        SpeakerTimelineAggregation.reconstruct(embeddings: input, speakerCount: speakers,
            frameCount: 1_000, framesPerWindow: 589, windowSeconds: 10, useExclusiveReconciliation: exclusive)
    }
}
