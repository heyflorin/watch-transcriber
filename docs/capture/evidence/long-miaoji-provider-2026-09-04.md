# Long Miaoji provider and upload-cancel evidence — 2026-09-04

## Scope and provider contract

This proof exercised only the App-owned Rust TOS and Miaoji path. It used a
fabricated two-system-voice file rendered directly to disk with `say -o`; no
audio was sent to an output device. The test made no Gemini, GitHub, R2, or
other archive call, and it did not read a personal recording.

The current official Miaoji API document states that an offline input must be
smaller than 1 GB and no longer than two hours. It also recommends a polling
interval greater than 30 seconds:
<https://www.volcengine.com/docs/6561/1798094?lang=zh>.

The checked-in harness therefore requires two independent exact guards, creates
a 7,140-second AAC file, validates it through the normal App importer, and polls
every 31 seconds. It refuses to start if either live-provider authorization or
the file-only/no-playback confirmation is absent.

## Passing live result

`scripts/demo/test_long_miaoji_provider.sh` completed against the configured
live TOS bucket and `volc.lark.minutes` endpoint:

| Check | Result |
|---|---:|
| Input duration | 7,140.064 seconds |
| Input size | 2,325,081 bytes |
| Miaoji polling | 4 queries, 31-second interval |
| Provider elapsed time | 107 seconds |
| Test elapsed time | 121.38 seconds |
| Transcript segments | 50 |
| Speaker-labelled segments | 50 |
| Temporary TOS version | exact delete + absent HEAD |
| Local temporary root | absent after pass |

Only the aggregate row above was printed. Credentials, signed URLs, object
identity, transcript text, and provider payloads were not printed or retained.

## Upload-cancel race found and fixed

Adding the long live fixture exposed an existing concurrency test failure. If
`begin_upload` won a race with `cancel`, the ledger entered `Uploading` without
a TOS receipt. `cancel` then tried to persist `CanceledAfterUpload`, while the
ledger validator correctly required that state to own a receipt. Cancellation
returned `invalid_ledger` and the durable state remained `Uploading`.

The corrected contract is:

1. canceling an in-flight upload persists `canceling_upload`;
2. one per-recording TOS-operation guard prevents a second PUT, an early HEAD,
   or duplicate cleanup while the owned network future is still running;
3. a late successful PUT checkpoints its exact version as
   `canceled_after_upload`, after which cancellation deletes that version once;
4. recovery after an interrupted/failed PUT performs HEAD only, never PUT;
5. production requires three absent HEAD results separated by two seconds
   before checkpointing `canceled_before_upload`;
6. App launch resumes either pending reconciliation or exact cleanup.

Focused tests cover the late-receipt and verified-absence branches, the
in-flight wait boundary, exact cleanup ownership, and the prohibition on a
second PUT. All seven cancellation-focused tests pass. The formerly flaky
two-store race then passed 500 consecutive direct iterations.

After the later quality harnesses, the complete current App suite contains 227
library tests: 216 pass and eleven explicit permission/model/live-provider tests
are ignored by default. The
three physical macOS integration tests remain explicitly ignored, and the
example test passes. Clippy with warnings denied also passes.

This closes the long Miaoji provider/rate fixture. It does not satisfy the
separate 12.5-hour, five-stratum local-vs-Miaoji quality matrix or its required
independent human transcript/speaker and blinded-summary adjudication.
