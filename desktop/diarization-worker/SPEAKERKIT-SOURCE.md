# SpeakerKit offline diarization source subset

`Sources/SpeakerKitOffline` is derived from Argmax OSS SpeakerKit at commit
`ea872ffd35705aa757f33033500b9b0d40bd38df`. The retained upstream code is MIT
licensed; see `SPEAKERKIT-LICENSE`.

EchoWall removes the ArgmaxCore and WhisperKit module dependencies and supplies
small offline-only compatibility types in `OfflineShims.swift`. Its model
resolver accepts only a caller-provided local directory; the downloader stub
always fails and contains no HTTP, URLSession, model-hub, credential, or cache
implementation. The final worker target must continue to pass the existing
CFNetwork/Network/URLSession and downloader-marker gates.

Only the Pyannote segmentation, WeSpeaker embedding, PLDA/VBx clustering,
exclusive timeline reconstruction, and required math/model types are intended
to remain after integration. Transcript-merging, RTTM formatting, generic model
management, and other compatibility-only declarations should be removed once
the direct worker entrypoint is complete.

The separately installed Core ML assets are pinned to
`argmaxinc/speakerkit-coreml` revision
`86ec9c929b52208b6656eb6a6361ed0d822a1f78`. Their upstream notices identify
Pyannote segmentation-3.0 (MIT), WeSpeaker/VoxCeleb weights (CC BY 4.0), and VBx
(Apache-2.0). The Rust model catalog owns exact file paths, sizes, hashes,
license disclosure, explicit installation, and removal.

The worker accepts two explicitly versioned presets with the same pinned model
files and clustering threshold 0.6. `speakerkit-pyannote-v3-exclusive-v1` keeps
the original fixed-stride, zero-padded tail behavior.
`speakerkit-pyannote-v3-exclusive-tail-context-v2` replaces a partial last chunk
with the exact final 30 seconds of original decoded PCM when the source is at
least 30 seconds long. Earlier chunk geometry and v1 timestamp arithmetic stay
unchanged. The replaced chunk carries its integer source-frame start through
embedding and reconstruction before projection to the model's frame grid;
its origin is never inferred from the nominal chunk stride. Sources shorter
than 30 seconds retain necessary padding. A request's preset is echoed in its
response; existing jobs must not be silently relabeled as v2.

The bounded public `mixed_05` diagnostic motivates v2 but is not corpus-wide
acceptance. Version selection and model lifecycle remain Rust/App-owned.
`TailContextTests` checks preset routing, legacy geometry/projection, exact
source samples, short/exact/fractional tails, coverage and absolute offsets
without running a model or accessing audio files.

V2 also retains a physical `(clip, chunk, window)` observation identity. Its
overlap support counts each physical window once, including different windows
that share an absolute start or quantize to the same model frame. Local masks
assigned to one global speaker are combined within that observation before
voting. The same identity deterministically breaks equal-time/local-speaker
clustering sort ties. V1 keeps its original projected-start grouping and sort
equivalence. `TimelineAggregationTests` exercises the production aggregation
and comparator for exact/near-origin collisions and reversed batch orders.
