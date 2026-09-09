// EchoWall offline-only replacements for the ArgmaxCore/WhisperKit types used
// by the pinned SpeakerKit source subset. No downloader or network transport is
// present in this target.

import Accelerate
import CoreML
import Foundation
import os.lock

@propertyWrapper
public struct Protected<Value>: @unchecked Sendable {
    private final class Storage: @unchecked Sendable {
        var lock = os_unfair_lock()
        var value: Value

        init(_ value: Value) { self.value = value }
    }

    private let storage: Storage

    public init(wrappedValue: Value) {
        storage = Storage(wrappedValue)
    }

    public var wrappedValue: Value {
        get {
            os_unfair_lock_lock(&storage.lock)
            defer { os_unfair_lock_unlock(&storage.lock) }
            return storage.value
        }
        nonmutating set {
            os_unfair_lock_lock(&storage.lock)
            storage.value = newValue
            os_unfair_lock_unlock(&storage.lock)
        }
    }
}

public extension Protected where Value: ExpressibleByNilLiteral {
    init() { self.init(wrappedValue: nil) }
}

public extension MLModel {
    func asyncPrediction(
        from input: MLFeatureProvider,
        options: MLPredictionOptions = MLPredictionOptions()
    ) async throws -> MLFeatureProvider {
        try await prediction(from: input, options: options)
    }
}

public final class Logging: @unchecked Sendable {
    public enum LogLevel: Sendable {
        case none
        case error
        case warning
        case info
        case debug
    }

    public static let shared = Logging()
    public var logLevel: LogLevel = .none

    public static func debug(_ message: @autoclosure () -> String) {}
    public static func info(_ message: @autoclosure () -> String) {}
    public static func warning(_ message: @autoclosure () -> String) {}
    public static func error(_ message: @autoclosure () -> String) {}
}

@frozen
public enum ModelState: CustomStringConvertible, Equatable, Hashable, Sendable {
    case unloading
    case unloaded
    case loading
    case loaded
    case prewarming
    case prewarmed
    case downloading
    case downloaded

    public var description: String {
        switch self {
        case .unloading: "Unloading"
        case .unloaded: "Unloaded"
        case .loading: "Loading"
        case .loaded: "Loaded"
        case .prewarming: "Prewarming"
        case .prewarmed: "Prewarmed"
        case .downloading: "Downloading"
        case .downloaded: "Downloaded"
        }
    }
}

public struct ModelDownloadConfig: Sendable {
    public let downloadBase: String?
    public let modelRepo: String
    public let modelToken: String?
    public let modelFolder: String?
    public let useBackgroundSession: Bool
    public let endpoint: String
    public let revision: String

    public init(
        downloadBase: String? = nil,
        modelRepo: String,
        modelToken: String? = nil,
        modelFolder: String? = nil,
        useBackgroundSession: Bool = false,
        endpoint: String = "offline",
        revision: String = "pinned"
    ) {
        self.downloadBase = downloadBase
        self.modelRepo = modelRepo
        self.modelToken = modelToken
        self.modelFolder = modelFolder
        self.useBackgroundSession = useBackgroundSession
        self.endpoint = endpoint
        self.revision = revision
    }
}

public struct ModelInfo: CustomStringConvertible, CustomDebugStringConvertible, Sendable {
    public let version: String?
    public let variant: String?
    public let name: String
    public let computeUnits: MLComputeUnits

    public init(
        version: String? = nil,
        variant: String? = nil,
        name: String,
        computeUnits: MLComputeUnits
    ) {
        self.version = version
        self.variant = variant
        self.name = name
        self.computeUnits = computeUnits
    }

    public func modelURL(baseURL: URL) -> URL {
        var result = baseURL.appendingPathComponent(name)
        if let version { result.appendPathComponent(version) }
        if let variant { result.appendPathComponent(variant) }
        return result
    }

    public var downloadPattern: String {
        "\(name)/\(version ?? "*")/\(variant ?? "*")/*"
    }

    public var description: String {
        [name, version, variant].compactMap { $0 }.joined(separator: "/")
    }

    public var debugDescription: String { description }
}

public enum OfflineModelError: Error {
    case networkForbidden
    case modelUnavailable
}

public final class ModelDownloader: @unchecked Sendable {
    public init(config: ModelDownloadConfig) {}

    public func resolveRepo(
        patterns: [String],
        downloadBase: URL?,
        download: Bool,
        progressCallback: ((Progress) -> Void)?
    ) async throws -> URL {
        throw OfflineModelError.networkForbidden
    }
}

@available(macOS 13, iOS 16, watchOS 10, visionOS 1, *)
public protocol ModelLoader: AnyObject, Sendable {
    var modelFolder: String? { get }
    func resolveModels(
        downloader: ModelDownloader,
        progressCallback: ((Progress) -> Void)?
    ) async throws -> String
    func load(from modelPath: String, prewarm: Bool) async throws
    func unload() async
}

@available(macOS 13, iOS 16, watchOS 10, visionOS 1, *)
open class ModelManager: @unchecked Sendable {
    public private(set) var modelState: ModelState = .unloaded
    public private(set) var modelPath: URL?
    public let downloader: ModelDownloader
    public let loader: ModelLoader
    public var modelFolder: URL? {
        loader.modelFolder.map { URL(fileURLWithPath: $0) }
    }

    public init(loader: ModelLoader, downloader: ModelDownloader) {
        self.loader = loader
        self.downloader = downloader
    }

    public func ensureModelsLoaded() async throws {
        if modelState == .loaded { return }
        try await downloadModels()
        try await loadModels()
    }

    public func downloadModels(progressCallback: ((Progress) -> Void)? = nil) async throws {
        guard let folder = loader.modelFolder else {
            throw OfflineModelError.networkForbidden
        }
        modelPath = URL(fileURLWithPath: folder)
        modelState = .downloaded
    }

    public func loadModels() async throws {
        guard let path = modelPath?.path ?? loader.modelFolder else {
            throw OfflineModelError.modelUnavailable
        }
        modelState = .loading
        do {
            try await loader.load(from: path, prewarm: false)
            modelState = .loaded
        } catch {
            modelState = .unloaded
            throw error
        }
    }

    public func unloadModels() async {
        modelState = .unloading
        await loader.unload()
        modelState = .unloaded
    }
}

public struct ModelUtilities {
    public static func detectModelURL(inFolder path: URL, named modelName: String) -> URL {
        let compiled = path.appendingPathComponent("\(modelName).mlmodelc")
        if FileManager.default.fileExists(atPath: compiled.path) { return compiled }
        return path.appendingPathComponent("\(modelName).mlpackage/Data/com.apple.CoreML/model.mlmodel")
    }
}

public enum WhisperKit {
    public static let sampleRate = 16_000
}

public struct WordTiming: Hashable, Codable, Sendable {
    public var word: String
    public var tokens: [Int]
    public var start: Float
    public var end: Float
    public var probability: Float

    public init(word: String, tokens: [Int], start: Float, end: Float, probability: Float) {
        self.word = word
        self.tokens = tokens
        self.start = start
        self.end = end
        self.probability = probability
    }
}

public struct TranscriptionSegment: Hashable, Codable, Sendable {
    public var id: Int
    public var seek: Int
    public var start: Float
    public var end: Float
    public var text: String
    public var tokens: [Int]
    public var tokenLogProbs: [[Int: Float]]
    public var temperature: Float
    public var avgLogprob: Float
    public var compressionRatio: Float
    public var noSpeechProb: Float
    public var words: [WordTiming]?

    public init(
        id: Int = 0,
        seek: Int = 0,
        start: Float = 0,
        end: Float = 0,
        text: String = "",
        tokens: [Int] = [],
        tokenLogProbs: [[Int: Float]] = [[:]],
        temperature: Float = 1,
        avgLogprob: Float = 0,
        compressionRatio: Float = 1,
        noSpeechProb: Float = 0,
        words: [WordTiming]? = nil
    ) {
        self.id = id
        self.seek = seek
        self.start = start
        self.end = end
        self.text = text
        self.tokens = tokens
        self.tokenLogProbs = tokenLogProbs
        self.temperature = temperature
        self.avgLogprob = avgLogprob
        self.compressionRatio = compressionRatio
        self.noSpeechProb = noSpeechProb
        self.words = words
    }
}

public final class TranscriptionResult: @unchecked Sendable {
    public var segments: [TranscriptionSegment]

    public init(segments: [TranscriptionSegment]) {
        self.segments = segments
    }
}

public enum AudioProcessor {
    public static func padOrTrimAudio(
        fromArray audioArray: [Float],
        startAt startIndex: Int = 0,
        toLength frameLength: Int = 480_000,
        saveSegment: Bool = false
    ) -> MLMultiArray? {
        guard startIndex >= 0, startIndex < audioArray.count,
              let output = try? MLMultiArray(
                shape: [NSNumber(value: frameLength)],
                dataType: .float32
              ) else { return nil }
        let count = min(frameLength, audioArray.count - startIndex)
        let destination = output.dataPointer.assumingMemoryBound(to: Float.self)
        audioArray.withUnsafeBufferPointer { source in
            destination.initialize(from: source.baseAddress!.advanced(by: startIndex), count: count)
        }
        if count < frameLength {
            vDSP_vclr(destination.advanced(by: count), 1, vDSP_Length(frameLength - count))
        }
        return output
    }
}
