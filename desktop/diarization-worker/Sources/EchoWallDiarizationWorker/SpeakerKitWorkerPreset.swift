import SpeakerKitOffline

enum SpeakerKitWorkerPreset: String, CaseIterable {
    case exclusiveV1 = "speakerkit-pyannote-v3-exclusive-v1"
    case tailContextV2 = "speakerkit-pyannote-v3-exclusive-tail-context-v2"

    var tailContextPolicy: SpeakerKitTailContextPolicy {
        switch self {
        case .exclusiveV1: .paddedV1
        case .tailContextV2: .endAlignedV2
        }
    }
}
