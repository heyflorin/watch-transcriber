# FluidAudio offline diarization source subset

`Sources/FluidAudioOffline` and `Sources/FastClusterWrapper` are a deliberately
small source subset of FluidInference/FluidAudio at commit
`5c19d5e12320e22bbfb7a1877b089d2665a69add`.

EchoWall keeps only the offline Pyannote/WeSpeaker/PLDA/VBx pipeline, local
Core ML model loading, disk-backed audio input, and fastcluster bridge. The
upstream model registry, HTTP downloader, CLI, ASR, VAD, and TTS sources are
not included in the worker target. `OfflineDiarizerModels.load` was narrowed to
load the four preinstalled `.mlmodelc` directories directly, and automatic
model preparation/download recovery and arbitrary embedding export were removed
from `OfflineDiarizerManager`. Disk-backed conversion was changed to map and
immediately unlink a mode-0600 temporary file, so a native abort cannot leave a
named private PCM artifact behind.

The subset remains licensed under the upstream Apache License 2.0 in
`FLUIDAUDIO-LICENSE`; the retained fastcluster and VBx notices are under
`ThirdPartyLicenses`.
