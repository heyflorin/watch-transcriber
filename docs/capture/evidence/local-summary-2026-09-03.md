# Local summary evidence — 2026-09-03

## Scope

This evidence covers only the Apple-Silicon local summary worker and fabricated
bilingual text. It contains no personal transcript, summary, title, path,
credential, signed URL, or provider call. It does not prove semantic parity
with Gemini or 飞书妙记.

## Selection

- Host: Apple M4 Max, 128GiB unified memory, macOS 26.6.2.
- Model: Qwen3.8-27B, Unsloth Dynamic `UD-Q4_K_XL` GGUF.
- Source revision: `4ca720788d1e01f1bff70c033e0d0028fd02e502`.
- File size: 17,559,178,144 bytes.
- SHA-256: `3f227079003add2511437e5b1e94812e363385225bf6a9b47b0054a72bc8b01e`.
- Runtime: exact `llama-cpp-2` 0.1.156, Metal, independent one-shot Rust
  process. Ollama 0.33.1 was used only as a same-weight development oracle.
- Rejected precision: NVIDIA NVFP4, because NVIDIA documents native NVFP4 as a
  Blackwell compute-capability 10.0+ path; it has no native Metal advantage.
- Rejected default size: Q4_K_M saves only 1,094,737,920 bytes relative to
  `UD-Q4_K_XL`; on the named 128GiB host that saving does not justify choosing
  the lower quality tier.

## Fabricated Ollama oracle

One short bilingual meeting fixture included a release date, a named speaker
slot with a validation deadline, an unchanged budget, and two explicit action
items. Aggregate result:

| Measurement | Result |
|---|---:|
| Total duration | 17.05s |
| Model load | 4.62s |
| Prompt evaluation | 91.28 tokens/s |
| Generation | 21.59 tokens/s |
| Structured fields present | 7/7 |
| Fabricated facts retained on manual check | 4/4 |
| Action items retained | 2/2 |

## App one-shot worker

The worker received the same fabricated facts through the closed v1 request and
ran under a macOS sandbox profile containing `(deny network*)`.

| Measurement | Result |
|---|---:|
| Exit | 0 |
| Wall time | 21.81s |
| Peak RSS | 21,832,056,832 bytes |
| Swap | 0 |
| Request/response identity match | pass |
| Structured fields present | 7/7 |
| Key points | 3 English / 3 Chinese |
| Action items | 2 |
| Outbound network | denied; worker completed |

The 5.1MB arm64 Mach-O imports Metal, MetalKit, Accelerate, Foundation,
CoreFoundation, system C/C++, and Objective-C libraries. `otool` and `nm` found
no CFNetwork, Network, URLSession, socket, connect, or curl dependency/symbol.
The macOS debug bundle contains all three sidecars plus the llama.cpp and
llama-cpp-rs licenses, remains 98MiB without model weights, keeps the main
executable deployment target at macOS 13.0, and passes an explicit deep ad-hoc
signature verification after the development build. A later current-source
universal App and DMG passed Developer ID signing, independent
notarization/stapling, worker privacy/network scans, and Gatekeeper; see
`macos-distribution-bundle-2026-09-03.md`.

## Failure found and contained

An initial GBNF sampler reached model load, context creation, prompt decode, and
grammar creation, then aborted across the llama.cpp/Rust FFI boundary with a
foreign exception. The Tauri process was not involved and no response was
checkpointed. Because that abort cannot be caught safely by the current Rust
binding, the grammar sampler was removed. The retained boundary uses an exact
schema-directed prompt, at most two deterministic attempts, typed closed JSON
decoding, field/category/array bounds, and a second identity validation in the
main Rust process. Invalid output remains a retryable local job failure.

## User-authorized latest-recording A/B

The latest archive entry was processed locally without printing its title,
transcript, reference summary, or candidate summary. The input transcript was
22,027 UTF-8 bytes. The one-shot worker completed in 117.85 seconds at
21,865,857,024-byte peak RSS with zero swap and returned all seven fields: two
summaries, six English and six Chinese key points, and five action items.

A second local Qwen3.8 pass independently scored the candidate and the existing
summary against the stored transcript. This is a same-model directional audit,
not independent quality evidence:

| Aggregate score | Local candidate | Existing summary |
|---|---:|---:|
| Faithfulness | 98 | 98 |
| Important-fact coverage | 95 | 95 |
| Named-term recall | 95 | 95 |
| Action-item precision | 100 | 95 |
| Action-item recall | 90 | 90 |
| Bilingual consistency | 100 | 98 |
| Unsupported claims | 0 | 0 |
| Missing critical facts | 1 | 1 |

The result supports retaining Qwen3.8-27B as the candidate. It does not satisfy
the frozen independent corpus/human-review parity gate and must not be reported
as proof that local summary matches Gemini or 妙记 in general.

## Complete embedded-Rust route

The default-ignored App integration test used all 23 exact catalog files and a
short fabricated English WAV. The entire test process ran under `(deny
network*)` with an empty `ProcessingCredentialsState`:

```text
import → Rust ledger → Whisper/Metal → FluidAudio diarization
  → Qwen3.8 summary → local note/manifest/audio → local audio hash proof
```

The route passed in 168.96 seconds after model verification, ended in
`Complete`, recorded `whisper_local`, `qwen_local`, and `local_archive`, and had
no TOS object or 妙记 task. The generated manifest entry contained no `r2_key`
or `r2_generation`, the local canonical-backup locator was present, and the
compiled viewer reopened from the new local archive. Peak child RSS was
21,838,528,512 bytes with zero swap. This proves end-to-end offline mechanics
and replay boundaries, not semantic parity or long-form performance.

After local completion, an explicit `back_up_local_recording_to_cloud` action
can reuse the exact stored transcript, Qwen summary, and audio in a second
publication generation. Unit evidence proves this adds only GitHub/R2 publish
and backup verification calls—no TOS upload, 妙记 task, Gemini summary, or local
model rerun. The fabricated browser smoke covers the separate “备份到私有云”
consent action and the credential-free first-run local archive path.

## Long-context map/reduce proof

A fabricated two-hour transcript containing 152,553 UTF-8 bytes exceeded the
worker's deterministic single-chunk threshold. Under the same `(deny
network*)` sandbox, the worker executed two map chunks and one reduce pass and
returned all seven structured fields.

| Measurement | Result |
|---|---:|
| Exit | 0 |
| Wall time | 272.18s |
| Peak RSS | 21,902,671,872 bytes |
| Swap | 0 |
| Map / reduce calls | 2 / 1 |
| Structured fields present | 7/7 |
| Key points | 4 English / 4 Chinese |
| Action items | 1 |
| Outbound network | denied; worker completed |

Development-only stage logging exposed only stage names and counts, not
transcript or summary text. This proves the bounded recursive route executes
without network access; it does not establish semantic quality on a real
two-hour meeting.

## Open gates

- Freeze and pass an independent summary-quality matrix across Mandarin,
  English, mixed-language, short, overlap, and long-form strata.
- Repeat the two-hour end-to-end network-denied App route with nonrepetitive
  synthetic speech and the complete local archive proof.
