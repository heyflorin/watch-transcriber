//  For licensing see accompanying LICENSE.md file.
//  Copyright © 2026 Argmax, Inc. All rights reserved.

import Foundation

// MARK: - PyannoteDiarizer Configuration

struct DiarizerConfig {
    let segmenterModel: SpeakerSegmenterModel
    let embedderModel: SpeakerEmbedderModel
    let clusterer: any Clusterer
    let concurrentEmbedderWorkers: Int?
    let models: PyannoteModels?

    init(
        segmenterModel: SpeakerSegmenterModel,
        embedderModel: SpeakerEmbedderModel,
        clusterer: any Clusterer,
        concurrentEmbedderWorkers: Int? = nil,
        models: PyannoteModels? = nil
    ) {
        self.segmenterModel = segmenterModel
        self.embedderModel = embedderModel
        self.clusterer = clusterer
        self.concurrentEmbedderWorkers = concurrentEmbedderWorkers
        self.models = models
    }
}

// MARK: - PyannoteDiarizer

/// Pyannote-based speaker diarization implementation
@available(macOS 13, iOS 16, watchOS 10, visionOS 1, *)
public final class PyannoteDiarizer: Diarizer, @unchecked Sendable {
    private let loader: PyannoteModelLoader
    private let downloader: ModelDownloader
    private let diarizerActor: PyannoteDiarizerActor
    
    public var modelFolder: URL? {
        guard let path = loader.modelFolder else { return nil }
        return URL(fileURLWithPath: path)
    }
    
    public var modelState: ModelState {
        loader.models != nil ? .loaded : .unloaded
    }
    
    init(loader: PyannoteModelLoader, downloader: ModelDownloader, config: DiarizerConfig) {
        self.loader = loader
        self.downloader = downloader
        self.diarizerActor = PyannoteDiarizerActor(config: config)
    }
    
    public func downloadModels() async throws {
        try await downloadModels(progressCallback: nil)
    }

    public func downloadModels(progressCallback: (@Sendable (Progress) -> Void)?) async throws {
        _ = try await loader.resolveModels(downloader: downloader, progressCallback: progressCallback)
    }
    
    public func loadModels() async throws {
        let modelPath = try await loader.resolveModels(downloader: downloader, progressCallback: nil)
        try await loader.load(from: modelPath, prewarm: false)
    }
    
    public func unloadModels() async {
        await loader.unload()
    }

    public func diarize(
        audioArray: [Float],
        options: (any DiarizationOptions)? = nil,
        progressCallback: (@Sendable (Progress) -> Void)? = nil
    ) async throws -> DiarizationResult {
        try await diarizerActor.diarize(audioArray: audioArray, options: options, progressCallback: progressCallback)
    }
}

// MARK: - PyannoteDiarizerActor

@available(macOS 13, iOS 16, watchOS 10, visionOS 1, *)
actor PyannoteDiarizerActor {
    let config: DiarizerConfig

    private var audioLength: Int = 0
    private var timings: PyannoteDiarizationTimings
    private var modelLoadTime: CFAbsoluteTime
    private var diarizationTask: Task<Void, Error>?

    init(config: DiarizerConfig) {
        self.config = config
        self.timings = .init()
        self.modelLoadTime = 0
    }

    func loadModels() async throws {
        let loadSegmenter = config.segmenterModel.modelState != .loaded
        let loadEmbedder = config.embedderModel.modelState != .loaded
        guard loadSegmenter || loadEmbedder else {
            timings.modelLoading = modelLoadTime
            return
        }

        let startTime = CFAbsoluteTimeGetCurrent()
        if loadSegmenter {
            try await config.segmenterModel.loadModel()
        }
        if loadEmbedder {
            try await config.embedderModel.loadModel()
        }
        modelLoadTime = (CFAbsoluteTimeGetCurrent() - startTime) * 1_000
        timings.modelLoading = modelLoadTime
    }

    func prepareSeekClips(contentFrames: Int, options: PyannoteDiarizationOptions?) -> [(Int, Int)] {
        var seekPoints: [Int] = (options?.clipTimestamps ?? []).map { Int(round($0 * Float(WhisperKit.sampleRate))) }
        if seekPoints.isEmpty {
            seekPoints.append(0)
        }

        if seekPoints.count % 2 == 1 {
            seekPoints.append(contentFrames)
        }

        var seekClips: [(start: Int, end: Int)] = []
        for i in stride(from: 0, to: seekPoints.count, by: 2) {
            let start = seekPoints[i]
            let end = i + 1 < seekPoints.count ? seekPoints[i + 1] : contentFrames
            seekClips.append((start, end))
        }

        return seekClips
    }

    func reset() async {
        diarizationTask?.cancel()
        diarizationTask = nil
        await config.clusterer.reset()
        timings = .init()
    }

    func initialize(
        audioArray: [Float],
        options: PyannoteDiarizationOptions? = nil,
        progressCallback: (@Sendable (Progress) -> Void)? = nil
    ) async throws {
        await reset()
        timings.pipelineStart = CFAbsoluteTimeGetCurrent()

        try await loadModels()
        audioLength = audioArray.count

        let seekClips = prepareSeekClips(contentFrames: audioArray.count, options: options)
        Logging.debug("[PyannoteDiarizer] audioArray: \(audioArray.count), seekClips: \(seekClips)")

        let totalAudioSeconds = seekClips.reduce(0.0) { $0 + Double($1.1 - $1.0) / Double(WhisperKit.sampleRate) }

        let segmenterModel = config.segmenterModel
        let embedderModel = config.embedderModel
        let clusterer = config.clusterer
        let concurrentEmbedderWorkers = config.concurrentEmbedderWorkers

        timings.numberOfSegmenterWorkers = segmenterModel.concurrentWorkers

        // Fire the segmenting + embedding pipeline in the background; clusterSpeakers() awaits diarizationTask before clustering.
        let progressObj = Progress(totalUnitCount: 100)
        let progressReporter = ProgressReporter(progress: progressObj, callback: progressCallback)
        diarizationTask = Task {
            var processedAudioSeconds = 0.0

            for (clipIndex, (seekClipStart, seekClipEnd)) in seekClips.enumerated() {
                try Task.checkCancellation()

                let audioClip = Array(audioArray[seekClipStart..<seekClipEnd])
                let clipSeconds = Double(audioClip.count) / Double(WhisperKit.sampleRate)
                self.timings.inputAudioSeconds += clipSeconds

                let expectedChunks = max(1, segmenterModel.maxChunks(for: audioClip.count))
                let counter = EmbeddingBatchCounter()

                let embedderWorkerCount = concurrentEmbedderWorkers ?? min(8, max(2, Int(clipSeconds / 30.0)))
                self.timings.numberOfEmbedderWorkers = embedderWorkerCount

                let (outputStream, outputContinuation) = AsyncStream.makeStream(of: SpeakerSegmenterOutput.self)

                try await withThrowingTaskGroup(of: Void.self) { group in
                    for _ in 0..<embedderWorkerCount {
                        group.addTask { [processedAudioSeconds, progressReporter] in
                            for await output in outputStream {
                                guard !Task.isCancelled else { break }
                                let startTime = CFAbsoluteTimeGetCurrent()
                                do {
                                    let embeddings = try await embedderModel.embed(segmenterOutput: output)
                                    await clusterer.add(speakerEmbeddings: embeddings)
                                } catch {
                                    Logging.error("[PyannoteDiarizer] Error processing embeddings: \(error)")
                                }
                                let elapsed = (CFAbsoluteTimeGetCurrent() - startTime) * 1_000
                                await counter.add(time: elapsed)

                                let done = await counter.count
                                let fraction = (processedAudioSeconds + clipSeconds * Double(done) / Double(expectedChunks)) / max(totalAudioSeconds, 1)
                                let completed = Int64(min(max(fraction, 0), 0.99) * 100)
                                await progressReporter.report(completed: completed)
                            }
                        }
                    }

                    group.addTask {
                        let segmenterStart = CFAbsoluteTimeGetCurrent()
                        try await segmenterModel.predict(
                            audioArray: audioClip,
                            outputContinuation: outputContinuation,
                            tailContextPolicy: options?.tailContextPolicy ?? .paddedV1,
                            observationClipIndex: clipIndex
                        )
                        await counter.addSegmenterTime((CFAbsoluteTimeGetCurrent() - segmenterStart) * 1_000)
                    }

                    // Drain the group to propagate any error thrown by the segmenter model
                    for try await _ in group {}
                }

                let snap = await counter.snapshot()
                self.timings.segmenterTime += snap.segmenterTime
                self.timings.embedderTime += snap.embedderTime
                self.timings.numberOfChunks += snap.chunkCount
                processedAudioSeconds += clipSeconds
            }

            await progressReporter.report(completed: 100)
        }
    }

    func clusterSpeakers(with clusterer: Clusterer, options: PyannoteDiarizationOptions? = nil, progressCallback: (@Sendable (Progress) -> Void)? = nil) async throws -> DiarizationResult {
        let progressObj = Progress(totalUnitCount: 100)
        progressObj.completedUnitCount = 0
        progressCallback?(progressObj)
        try await diarizationTask?.value
        diarizationTask = nil

        progressObj.completedUnitCount = 50
        progressCallback?(progressObj)

        if !config.clusterer.isEqual(to: clusterer) {
            let embeddings = await config.clusterer.speakerEmbeddings()
            await clusterer.add(speakerEmbeddings: embeddings)
        }

        let startTime = CFAbsoluteTimeGetCurrent()
        let resolvedOptions = options ?? PyannoteDiarizationOptions()
        let clusteringConfig = clusterer.clusteringConfig(from: resolvedOptions)
        let clusteringResult = await clusterer.update(config: clusteringConfig)
        timings.numberOfEmbeddings = clusteringResult.speakerEmbeddings.count
        timings.clusteringTime = (CFAbsoluteTimeGetCurrent() - startTime) * 1_000

        progressObj.completedUnitCount = 80
        progressCallback?(progressObj)
        var diarizationResult = postProcess(speakerEmbeddings: clusteringResult.speakerEmbeddings,
                                            speakerCentroids: clusteringResult.speakerCentroids,
                                            originalLength: audioLength,
                                            useExclusiveReconciliation: resolvedOptions.useExclusiveReconciliation)
        timings.numberOfSpeakers = diarizationResult.speakerCount
        timings.fullPipeline = (CFAbsoluteTimeGetCurrent() - timings.pipelineStart) * 1_000

        Logging.debug(timings.debugDescription)

        diarizationResult.timings = timings
        progressObj.completedUnitCount = 100
        progressCallback?(progressObj)
        return diarizationResult
    }

    private func postProcess(speakerEmbeddings: [SpeakerEmbedding],
                             speakerCentroids: [Int: [Float]],
                             originalLength: Int,
                             useExclusiveReconciliation: Bool) -> DiarizationResult {
        let startTime = CFAbsoluteTimeGetCurrent()
        defer {
            let totalTime = (CFAbsoluteTimeGetCurrent() - startTime) * 1_000
            Logging.debug(String(format: "[SpeakerKit] Post processing time: %.2f ms", totalTime))
        }

        let diarizationFrameRate = config.segmenterModel.modelSampleRate
        guard !speakerEmbeddings.isEmpty else {
            return DiarizationResult(binaryMatrix: [], diarizationFrameRate: diarizationFrameRate)
        }

        let speakerCount = (speakerEmbeddings.map { $0.clusterId }.max() ?? 0) + 1
        let chunkLength = SpeakerSegmenterModel.chunkLengthInSeconds
        let maxChunks = config.segmenterModel.maxChunks(for: originalLength)

        let framesPerWindow = speakerEmbeddings[0].activeFrames.count
        let windowLength = config.segmenterModel.windowsLength
        let framesCount = framesPerWindow * Int(chunkLength / windowLength) * maxChunks

        Logging.debug("[PyannoteDiarizer] Speakers: \(speakerCount), maxChunks: \(maxChunks), framesPerWindow \(framesPerWindow)")

        let aggregation = SpeakerTimelineAggregation.reconstruct(
            embeddings: speakerEmbeddings,
            speakerCount: speakerCount,
            frameCount: framesCount,
            framesPerWindow: framesPerWindow,
            windowSeconds: windowLength,
            useExclusiveReconciliation: useExclusiveReconciliation
        )

        return DiarizationResult(
            binaryMatrix: aggregation.binary,
            supportMatrix: aggregation.support,
            diarizationFrameRate: diarizationFrameRate,
            speakerCentroidEmbeddings: speakerCentroids
        )
    }

    func diarize(audioArray: [Float], options: (any DiarizationOptions)?, progressCallback: (@Sendable (Progress) -> Void)?) async throws -> DiarizationResult {
        let opts = options as? PyannoteDiarizationOptions

        guard let progressCallback else {
            try await initialize(audioArray: audioArray, options: opts, progressCallback: nil)
            var result = try await clusterSpeakers(with: config.clusterer, options: opts, progressCallback: nil)
            if let minActiveOffset = opts?.minActiveOffset {
                result.updateSegments(minActiveOffset: minActiveOffset)
            }
            return result
        }

        // Use a single Progress across both phases so the caller never sees a backward jump.
        // initialize (segmenting + embedding) owns 0–85%, clusterSpeakers (clustering + post-processing) owns 85–100%.
        let diarizationProgress = Progress(totalUnitCount: 100)
        progressCallback(diarizationProgress)

        try await initialize(audioArray: audioArray, options: opts) { @Sendable child in
            let value = Int64(child.fractionCompleted * 85)
            if value > diarizationProgress.completedUnitCount {
                diarizationProgress.completedUnitCount = value
                progressCallback(diarizationProgress)
            }
        }

        var result = try await clusterSpeakers(with: config.clusterer, options: opts) { @Sendable child in
            let value = 85 + Int64(child.fractionCompleted * 15)
            if value > diarizationProgress.completedUnitCount {
                diarizationProgress.completedUnitCount = value
                progressCallback(diarizationProgress)
            }
        }

        if let minActiveOffset = opts?.minActiveOffset {
            result.updateSegments(minActiveOffset: minActiveOffset)
        }
        return result
    }
}

// MARK: - ProgressReporter

/// Serializes progress updates from concurrent workers onto a single Progress instance.
private actor ProgressReporter {
    private let progress: Progress
    private let callback: (@Sendable (Progress) -> Void)?

    init(progress: Progress, callback: (@Sendable (Progress) -> Void)?) {
        self.progress = progress
        self.callback = callback
    }

    func report(completed: Int64) {
        if completed > progress.completedUnitCount {
            progress.completedUnitCount = completed
            callback?(progress)
        }
    }
}

// MARK: - EmbeddingBatchCounter

private actor EmbeddingBatchCounter {
    private(set) var count: Int = 0
    private(set) var embedderTime: CFAbsoluteTime = 0
    private(set) var segmenterTime: CFAbsoluteTime = 0

    func add(time: CFAbsoluteTime) {
        count += 1
        embedderTime += time
    }

    func addSegmenterTime(_ time: CFAbsoluteTime) {
        segmenterTime += time
    }

    struct Snapshot {
        let count: Int
        let embedderTime: CFAbsoluteTime
        let segmenterTime: CFAbsoluteTime
        var chunkCount: Int { count }
    }

    func snapshot() -> Snapshot {
        Snapshot(count: count, embedderTime: embedderTime, segmenterTime: segmenterTime)
    }
}
