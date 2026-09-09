import Foundation

/// Production overlap voting, shared with deterministic reconstruction tests.
struct SpeakerTimelineAggregation {
    let support: [[Float]]
    let binary: [[Int]]
    let observationCounts: [Float]

    static func reconstruct(
        embeddings: [SpeakerEmbedding],
        speakerCount: Int,
        frameCount: Int,
        framesPerWindow: Int,
        windowSeconds: Float,
        useExclusiveReconciliation: Bool
    ) -> Self {
        var support = Array(repeating: Array(repeating: Float(0), count: frameCount), count: speakerCount)
        var counts = Array(repeating: Float(0), count: frameCount)
        var seenLegacyStarts: Set<Int> = []
        var windows: [SpeakerWindowObservation: [SpeakerEmbedding]] = [:]

        for embedding in embeddings {
            guard embedding.clusterId >= 0 && embedding.clusterId < speakerCount else { continue }
            if let observation = embedding.observation {
                windows[observation, default: []].append(embedding)
                continue
            }
            // Keep v1's numerator, projected-start denominator and Float
            // arithmetic unchanged for retained requests.
            let start = embedding.diarizationFrameOffset(framesPerWindow: framesPerWindow, windowSeconds: windowSeconds)
            for (index, value) in embedding.activeFrames.enumerated() {
                let offset = start + index
                guard offset >= 0 && offset < frameCount else { continue }
                if value != 0 { support[embedding.clusterId][offset] += 1 }
                if !seenLegacyStarts.contains(start) { counts[offset] += 1 }
            }
            seenLegacyStarts.insert(start)
        }

        for observation in windows.keys.sorted() {
            let members = windows[observation]!
            let first = members[0]
            let start = first.diarizationFrameOffset(framesPerWindow: framesPerWindow, windowSeconds: windowSeconds)
            let length = first.activeFrames.count
            var activeByCluster: [Int: [Bool]] = [:]
            for member in members {
                // Every local-speaker mask from one physical window has the
                // same origin and length. Combine masks assigned to the same
                // global speaker so that one observation casts at most one vote.
                precondition(member.activeFrames.count == length)
                precondition(member.diarizationFrameOffset(framesPerWindow: framesPerWindow, windowSeconds: windowSeconds) == start)
                var active = activeByCluster[member.clusterId] ?? Array(repeating: false, count: length)
                for (index, value) in member.activeFrames.enumerated() where value != 0 {
                    active[index] = true
                }
                activeByCluster[member.clusterId] = active
            }
            for index in 0..<length {
                let offset = start + index
                guard offset >= 0 && offset < frameCount else { continue }
                counts[offset] += 1
                for (cluster, active) in activeByCluster where active[index] {
                    support[cluster][offset] += 1
                }
            }
        }

        for frame in 0..<frameCount where counts[frame] > 0 {
            for speaker in 0..<speakerCount { support[speaker][frame] /= counts[frame] }
        }

        var binary = Array(repeating: Array(repeating: 0, count: frameCount), count: speakerCount)
        for frame in 0..<frameCount where counts[frame] > 0 {
            let activeCount = (0..<speakerCount).map { Int(round(support[$0][frame])) }.reduce(0, +)
            let topK = useExclusiveReconciliation ? min(activeCount, 1) : activeCount
            let ordered = (0..<speakerCount).map { (speaker: $0, value: support[$0][frame]) }
                .sorted { $0.value > $1.value }
            for speaker in ordered.prefix(topK) { binary[speaker.speaker][frame] = 1 }
        }
        return Self(support: support, binary: binary, observationCounts: counts)
    }
}
