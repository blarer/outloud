# Does OutLoud use the Apple Neural Engine?

Short answer: **yes, indirectly, on the path that actually ships today** — and
there is no line of our code that asks for it.

This is worth writing down because "do we use the ANE" is a question with two
different honest answers depending on whether you mean *our code* or *the
machine at runtime*, and conflating them produces either a false boast or a
false denial.

## What our code contains

Nothing. There is no CoreML, no `MLComputeUnits`, no `AppleNeuralEngine`
framework link, and no ANE tuning anywhere in the workspace. Searching for
those symbols returns only two hits, both in planning documents describing
future work.

The Swift helper links exactly two relevant frameworks:

```
$ otool -L crates/asr/helper/aqua-speech-helper
    /System/Library/Frameworks/Speech.framework/...
    /usr/lib/swift/libswiftMetal.dylib (weak)
```

`Speech.framework` and a weak Metal reference. No ANE framework.

## What the machine does at runtime

The ANE is used anyway, because Apple's `SpeechTranscriber` dispatches to it
internally. Verified by watching `aned`, the Apple Neural Engine daemon, while
running a transcription:

```
$ log stream --predicate 'process == "aned"' --style compact
aned[545] (ANEServices) Found matching service: ANEDriver
aned[545] (ANEServices) Total num of devices 2
aned[545] (ANEServices) ANEServicesDevice::ANEServicesDeviceOpen, usage type: 2
aned[545] (ANEServices) ANEDriver Device Open succeeded with usage type: 2
aned[545] ... invalidated because the client process (pid 67109) ... exited
```

The device is opened when our helper starts and released when it exits.

The causal link is established by a control pair rather than by assertion,
because ANE traffic from unrelated system services would otherwise be easy to
mistake for our own:

| Run | Backend | ANE device opens |
|---|---|---|
| Idle, 4 seconds, no dictation | — | **0** |
| `--once --say "..."` | `--asr apple` (default) | **1** |
| `--once --say "..."` | `--asr mock` | **0** |

Same binary, same audio path, same injection path. The only variable is the
recognizer, and the ANE follows it exactly.

## Why this matters

**For latency.** Our measured 118-296ms finalize time is achieved on a
dedicated inference accelerator, not the CPU. That is a large part of why a
local recognizer beats a cloud round trip, and it means the number will not
transfer unchanged to Intel Macs or to Linux and Windows, which have no
equivalent. Any published benchmark has to name the hardware.

**For battery.** The ANE is far more efficient than doing the same work on CPU
or GPU. This is the right place for continuous dictation to run.

**For the open-weights backends.** Parakeet TDT and whisper.cpp are currently
stubs. When they land they will *not* automatically get the ANE:

- whisper.cpp reaches it only through its optional Core ML encoder, which
  requires shipping a separately converted `.mlmodelc` and is Apple-only.
- ONNX Runtime reaches it only through the CoreML execution provider, and only
  for operators CoreML can lower. Parakeet's TDT decoder is unlikely to fully
  qualify.
- MLX targets the GPU, not the ANE.

So the honest framing is that our *fastest* path is ANE-accelerated and our
*portable* path will not be. That is a real trade-off between speed and
portability, and it should be stated rather than smoothed over.

**For the "fully local" claim.** Unaffected. ANE inference is on-device by
definition. Nothing here weakens the privacy story.

## What we do not control

Because the dispatch happens inside `SpeechTranscriber`, we cannot request the
ANE, forbid it, or observe which layers ran where. Apple decides. If a future
macOS release changes that policy, our latency changes with it and we will find
out from the benchmark, not from a release note. The latency regression gate
(`cargo bench -p ax-edit --bench gate`) exists partly for this reason.

## Reproducing this

```bash
# Terminal 1
log stream --predicate 'process == "aned"' --style compact

# Terminal 2
cargo run --release -p outloud -- --once --say "hello" --no-overlay   # expect ANE traffic
cargo run --release -p outloud -- --once --say "hello" --asr mock --no-overlay  # expect none
```
