# Edit-by-voice: what works, what does not, and what to do about it

**Date:** 2026-07-29
**Machine:** Apple M4 Pro, 24 GB, macOS 26.5.2 (25F84), Xcode SDK 26.2
**Scope:** `crates/edit-intent`, `crates/llm`, `crates/ax-edit`, `crates/text-target`,
`crates/outloud/src/inject.rs`, `crates/outloud/src/pipeline.rs`, `docs/ux/`

Every number below was produced by a command run on this machine during the
investigation. Commands are named next to their results so they can be
re-run. Nothing here is estimated unless it says so.

---

## Summary

1. **The deterministic path works and is fast.** Parse is 3-52us; end-to-end
   dictation measured 152-174ms, matching the documented 116-268ms.
2. **Freeform edits do not reach a model, and on macOS they never even
   report that.** The `llm` crate is not referenced anywhere in the daemon.
   The `FreeformUnsupported` outcome exists, but the macOS delivery path
   returns before it can be produced, so a freeform edit is silently
   *dictated as text* into the user's document instead.
3. **The `llm` crate is real, complete, and works.** It builds, loads
   Qwen3-1.7B on Metal, and produces guarded output. Measured warm TTFT
   226ms (short input) to 292ms (paragraph), total 312ms to 1225ms.
4. **But its output quality is not good enough to ship as the answer.** On a
   24-request sweep, Qwen3-1.7B returned the input **verbatim** on 25-50% of
   requests depending on input size; a larger 216-request run put the pooled
   rate at **36.6%**, falling only to 24.1% with the best prompt. Two causes
   found, one fixable but not sufficient.
5. **Apple's Foundation Models framework is present but unavailable** on this
   machine: `appleIntelligenceNotEnabled`. A working Rust -> Swift C-ABI
   spike (`docs/investigations/fm-spike/`) proves the integration links,
   runs, and degrades cleanly in 2.3ms, so it is the strongest long-term
   option and costs no download. Its output quality remains unmeasured, and
   it cannot be a hard dependency.
6. **The deterministic parser can absorb most of the current "freeform"
   traffic.** A prototype pre-pass handles **21 of 25** commands the shipped
   parser fails, in **~2us**, with output asserted against exact expected
   strings. On those same commands the model was correct **10% of the time
   at 324-429ms**.

**Top recommendation: extend the deterministic parser first (Task 4), and
fix the silent-dictation bug. The LLM is a second step, not the first one.**

### Direct answer to Task 3: the smallest change that makes freeform work

Task 3 asked for the smallest change that makes *freeform* edits work for
real, so here it is plainly, separated from the recommendation above about
what to do *first*. These are different questions and the doc should not
conflate them.

The smallest change that genuinely enables freeform, in dependency order:

| Step | Change | Cost |
|---|---|---|
| 1 | `llm = { path = "../llm" }` in `crates/outloud/Cargo.toml`, behind a `llama` feature | minutes |
| 2 | Lazily load `LlamaTransformer` on first freeform request, keep it resident | ~1 hour |
| 3 | Route `EditIntent::Freeform` in `inject.rs` to `llm::transform` instead of `insert_with_fallback` | ~1 hour |
| 4 | **Preview panel** (`docs/ux/03`): new overlay state, streaming render, apply/retry/cancel by voice and key | **the real cost, days** |
| 5 | Drop system-prompt rule 5; hoist `new_context` out of the request | ~1 hour, measured payoffs |

Steps 1-3 and 5 are roughly a day. **Step 4 is unavoidable and dominates.**
`PreviewedEdit` deliberately exposes no apply method precisely so generated
text cannot reach a document without a preview, and that is the right
design, so "wire up the LLM" cannot be done without building the panel.

**The honest caveat, and why this is not my top recommendation:** doing all
of that lands a feature that, measured on this machine, returns the user's
text unchanged **roughly a third of the time** (36.6% pooled over 216
requests, 24.1% with the prompt fix). The plumbing is cheap; the plumbing
is also not the problem. Fixing rule 5 (step 5) cuts the echo rate by a
measured 1.5x (36.6% -> 24.1%, n=432, p=0.005), which is the single
highest-leverage change inside the LLM path, but
it does not reach shippable on its own. Steps 1-3 + 5 without step 4 is not
a legitimate shortcut: it would mean writing unvetted model output straight
into documents.

---

## 1. What works today vs what does not

### Measured: the deterministic path

`spike-cli dry-run`, release build, `OUTLOUD_NO_INJECT=1` throughout:

| Utterance | Parsed as | Result | Parse time |
|---|---|---|---|
| `change quick to slow` | `Replace` | `the slow brown fox...` | 52us (first, cold) |
| `swap fox for cat` | `Replace` | `the quick brown cat...` | 3us |
| `delete really` | `Delete` | applied | 5us |
| `add and thanks` | `Append` | `...lazy dog and thanks` | 4us |
| `make it all caps` | `Recase(Upper)` | `THE QUICK BROWN FOX...` | 4us |
| `make it title case` | `Recase(Title)` | `The Quick Brown Fox...` | 3us |

These are correct and effectively free. The README's claim that literal
commands work is accurate.

### Measured: end-to-end dictation baseline

```
OUTLOUD_NO_INJECT=1 ./target/release/outloud --once --say "<27-word sentence>"
```
Five consecutive runs: **174, 158, 152, 158, 159 ms** (release->text, all
finalize; inject 0.0ms because delivery was suppressed). Consistent with the
116-268ms figure in the brief. This is the budget any LLM work must respect.

### The freeform path: three distinct failures

**(a) The `llm` crate is not wired in at all.** Confirmed by grep across
`crates/outloud/src` and `crates/spike-cli/src`: zero references to `llm::`.
`crates/outloud/Cargo.toml` does not depend on `llm`. The crate is an island.

**(b) On macOS, a freeform edit is silently dictated into the document.**
This is the most serious finding and it is a behaviour bug, not a missing
feature. In `inject.rs::deliver`, the macOS `Mode::Edit` arm does:

```rust
if let EditIntent::Freeform { .. } = &intent {
    return insert_with_fallback(text);   // <- inserts the COMMAND as text
}
```

So saying "tighten this up" with text selected does not report anything. It
types the words *"tighten this up"* into the user's document. The
`FreeformUnsupported` outcome and its careful pipeline message
("freeform edit ... needs the local LLM ... -> rephrase as
change/replace/delete/add/case") are **unreachable on macOS**: only
`payload_for` produces it, and `payload_for` is used by `deliver_via_tiers`,
which is `#[cfg(not(target_os = "macos"))]`.

The code comment explains the reasoning honestly, and the reasoning is
sound for its actual case: a stale selection plus ordinary dictation must
not be refused. But the fix conflated two things. "tighten this up" spoken
at a selection is not ordinary dictation, and inserting it is the one
outcome the user definitely did not want. `docs/ux/03` promises a preview
panel here; what ships is a silent wrong write.

**(c) `OUTLOUD_NO_INJECT=1` cannot exercise the edit path at all.** The
env-var check in `deliver` returns `Outcome::Suppressed` *before* mode
dispatch and before `edit_intent::parse` is ever called:

```rust
if std::env::var_os("OUTLOUD_NO_INJECT").is_some_and(|v| v == "1") {
    return Outcome::Suppressed { text: text.to_string() };
}
// ... terminal staging, mode dispatch, intent parsing all below
```

Confirmed two ways. First by running the daemon: `outloud --once --say
"tighten this up"` reported `suppressed (OUTLOUD_NO_INJECT)` with no intent
parsing. Second by a test added during this investigation,
`crates/outloud/tests/no_inject_guard.rs`, which feeds a well-formed edit
command (`change some to other`) against a selection that *does* contain the
search text, and asserts the outcome carries the raw transcript rather than
the rewritten text `other prose`. It passes, which proves no edit was
computed.

The guard is correct and necessary (it was added in d495b47 precisely to
stop tests typing into applications). The problem is where it sits: because
it precedes the decision logic, the safe measurement mode cannot exercise or
regression-test any edit-by-voice behaviour, and every `deliver`-based
assertion in `inject.rs`'s unit tests becomes vacuous whenever the guard is
set, since `Suppressed` trivially satisfies "not `FreeformUnsupported`".
Notably `unrecognised_phrase_with_a_selection_is_dictated`, the test pinning
finding (b)'s behaviour, is one of those. The `payload_for` tests are pure
and remain meaningful.

### Measured: parser correctness over a 55-command corpus

A corpus of realistic edit commands run through the shipped parser. The
harness is `crates/edit-intent/examples/shipped_parser_corpus.rs`, and it
carries 4 tests asserting these exact counts, so the table cannot drift from
what the parser does:

| Outcome | Count |
|---|---|
| Deterministic and correct | 15 |
| Deterministic but **silently wrong** | 10 |
| Should be deterministic, escalated to absent model | 15 |
| Correctly escalated (genuinely needs a model) | 15 |
| Open-ended phrase hijacked by a literal parse | 0 |

A note on how these were classified, because it changed the numbers: the
first version of this harness *guessed* at "is this result wrong?" using
keyword heuristics (`utterance.contains("period")` and friends), which made
the counts an artifact of the heuristic. Re-running with an explicit
per-case verdict found **10 silently-wrong cases, not 9**, and a corpus of
55, not 54. The extra case is `make the first line title case`, which
title-cases the entire field while appearing to succeed.

The silently-wrong bucket is the interesting one, because those edits look
like they worked. All verified against the live binary:

```
$ spike-cli dry-run "add a period at the end"
intent:  append "a period at the end"
result:  "the quick brown fox jumps over the lazy dog a period at the end"

$ spike-cli dry-run "add a comma after dog"
intent:  append "a comma after dog"
result:  "the quick brown fox jumps over the lazy dog a comma after dog"

$ spike-cli dry-run "uppercase the first letter"
intent:  recase to Upper
result:  "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG"     # whole field!

$ spike-cli dry-run "delete the last sentence"
intent:  delete "the last sentence"
result:  (nothing matched)
```

`uppercase the first letter` shouting the entire field is the worst of
these: the parser's `parse_case` matches on `contains("uppercase")` with no
regard for scope, so a narrow request becomes a total rewrite.

---

## 2. Assessing `crates/llm`

### What is in it

~1300 lines, well-factored, and genuinely good work:

- `Transformer` trait with a streaming method, plus `MockTransformer`.
- `prompt.rs`: constant system prompt, BEGIN/END delimiters, instruction
  after text, `/no_think` for Qwen3.
- `sanitize.rs`: strips think blocks, preambles, whole-output fences, quotes.
- `guardrail.rs`: refusal / echo / no-change / length-ratio / word-retention.
- `models.rs`: resumable download, SHA256 verify, atomic rename.
- `llama_backend.rs`: llama.cpp via `llama-cpp-2`, Metal, full GPU offload.
- 39 tests pass (`cargo test -p llm -p edit-intent`), none needing a model.

The safety design is the strongest part. `PreviewedEdit` deliberately has no
apply method, which structurally enforces the docs/ux/03 promise that
generated text always previews.

### Does it work? Yes.

Built with `cargo build -p llm --features llama --release` (needed `cmake`;
`python3 -m pip install --user cmake` provided 4.4.0). Build took 35s. The
model was already cached at `~/.aqua-oss/models/qwen3-1.7b-q4km-gguf`
(1.28 GB).

Note: `models.rs::default_cache_dir()` still returns `~/.aqua-oss/models`
while `docs/llm.md` claims `~/.outloud/models`. The rename missed this.

### Measured latency (new: `examples/bench_investigation.rs`)

24 requests, Qwen3-1.7B Q4_K_M, Metal, full offload:

| Metric | p50 | p90 | max |
|---|---|---|---|
| TTFT | 264ms | 295ms | 677ms |
| Total | 746ms | 1046ms | 1055ms |
| Total, short input (9 words) | 351ms | 452ms | |
| Total, long input (63 words) | 915ms | 1050ms | |

Model load: **8.6s cold in that run, 200ms warm** (mmap, warm page cache).

### Measured quality: this is the problem

Same harness, 24 requests: **11 guardrail rejections (46%)**, every single
one `NoChange` - the model returned its input untouched.

A size sweep (`examples/size_sweep.rs`, 12 runs per size, 4 instructions)
isolates it:

| Input size | Words | Usable | Echoed input | p50 total |
|---|---|---|---|---|
| 1 sentence | 9 | 75% | 25% | 312ms |
| 2 sentences | 25 | 75% | 25% | 457ms |
| paragraph | 63 | 50% | 50% | 810ms |
| long paragraph | 111 | 50% | 50% | 1225ms |

**A do-nothing rate in this band is not a shippable feature**, even behind a
preview panel. The preview makes it safe; it does not make it useful. A
larger 216-request run (below) pins the pooled rate at 36.6% for the shipped
prompt, and 24.1% even with the best prompt measured.

### Cause 1 (found and fixed in prototype): system-prompt rule 5

`prompt::SYSTEM_PROMPT` rule 5 reads:

> If the instruction cannot be applied, output the original text unchanged.

A 1.7B model treats this as standing permission to do nothing, and "tighten
this up" is exactly the soft instruction it decides it cannot apply. Ablation
(`examples/prompt_ablation.rs`, 12 runs per variant, `LlamaTransformer` given
a new `with_system_prompt` builder so variants are measurable rather than
arguable):

| Variant | Returned input unchanged | Rejected | Mean ms |
|---|---|---|---|
| shipped | 6-7 / 12 | 6-7 / 12 | 381-424 |
| rule 5 removed | 4 / 12 | 4 / 12 | 354-363 |
| rule 5 replaced with "you MUST change the text" | **2-3 / 12** | 2-3 / 12 | 374-377 |
| + two few-shot examples | 5 / 12 | 5 / 12 | 436 |

Removing rule 5 and replacing it with an explicit obligation is the largest
single improvement available inside the LLM path. **The 12-run ablation
above was underpowered, so it was re-run properly rather than trusted.**
Few-shot examples did *not* help and cost 60ms of extra prefill.

#### Confirming the rule-5 effect is real, not sampling noise

12 samples per variant at p~0.5 carries a standard error near 14 percentage
points, which cannot distinguish "halved" from "got lucky". So
`examples/rule5_confirm.rs` re-runs just the two variants that matter over
four input sizes and six soft instructions, with a two-proportion z-test:

| n per variant | shipped echo | rule-5-removed echo | z | two-sided p | verdict |
|---|---|---|---|---|---|
| 72 | 34.7% | 20.8% | 1.86 | 0.063 | **not significant** |
| 216 | 36.6% | 24.1% | 2.83 | **0.005** | significant |

The first confirmation run **failed to reach significance**, so the sample
was increased rather than the favourable earlier result kept. At n=432 total
the effect is real: a **12.5 point absolute reduction, 1.5x relative**.

Note this corrects the ablation's apparent effect size: "roughly halves" was
an artifact of small samples. The honest figure is **1.5x, not 2x**, and
even at 24.1% the model still returns the user's text unchanged on nearly
one request in four. The prompt fix is worth making and does not come close
to rescuing the feature.

### Cause 2 (not fixable by prompting): the model is too small

Even with the best prompt, long inputs still echo 50% of the time. That is a
capability ceiling of a 1.7B model on soft rewrite instructions, not a
prompting problem. A 4B model would likely do better and would cost roughly
2.5x the RAM and latency.

### Found: a 45ms per-request self-inflicted latency cost

`docs/llm.md` states the constant system prompt exists so "llama.cpp's prompt
cache keeps its KV prefix warm across requests, which is most of the
time-to-first-token win". That win is **not being collected**:
`transform_streaming` calls `model.new_context(..)` on every single request,
and a fresh context has an empty KV cache.

Measured (`examples/ctx_cost.rs`, 205-token prompt, 8 runs, two separate
invocations to show the run-to-run spread):

| Stage | p50 (run 1) | p50 (run 2) |
|---|---|---|
| `new_context()` | **39ms** | **34ms** |
| prefill decode (205 tokens) | 1ms | 1ms |
| first token sample | 136ms | 160ms |
| sum | 177ms | 195ms |

Against a measured warm TTFT of 226ms for the same short input
(`examples/ttft_breakdown.rs`), those stages account for most of it. Prefill
itself is 1ms, so the constant-prompt KV-cache rationale in `docs/llm.md` is
worth far less than the doc implies: the actual cost is context
*construction*, not prefill.

On reconciling the TTFT figures quoted in this document: `ttft_breakdown`
measures **226ms p50 short (9 words) and 292ms p50 long (63 words)**, which
brackets `bench_investigation`'s **264ms p50** because that harness pools
both sizes. Prompt length is the whole difference. Everything in this section
moves by tens of ms with machine load, so treat these as a band rather than
constants; the ratios, not the absolute values, are the finding.

The obvious objection is that a reused context must have its KV cache
cleared between requests, and if clearing costs what construction saved the
fix is worthless. Measured directly (`examples/ctx_reuse.rs`, 10 runs each,
varied inputs so neither arrangement gets an unfair identical-prompt cache
hit, with `clear_kv_cache_seq` inside the timed region):

| Arrangement | p50 |
|---|---|
| fresh context per request (ships today) | 241ms |
| one context reused, KV cache cleared | **195ms** |

**A real 45ms saving, 19% of the prefill path.** Clearing is cheap. This is
a measurement, not a projection.

### On-device options for Apple Silicon

**Apple Foundation Models (macOS 26+).** Present on this machine:
`/System/Library/Frameworks/FoundationModels.framework`, and it compiles
against the 26.2 SDK. But:

```
$ swiftc probe.swift && ./probe
UNAVAILABLE: appleIntelligenceNotEnabled
```

`SystemLanguageModel.default.availability` reports the user has not enabled
Apple Intelligence. 23 supported languages are advertised. `modelmanagerd`,
`siriinferenced`, and `generativeexperiencesd` are all running.

Assessment: **strategically the best option and tactically not sufficient
alone.** It ships with the OS (no 1.28 GB download), is Apple-optimised,
runs on the ANE, and is exactly aligned with the existing decision to depend
on Apple's `SpeechTranscriber`. But it requires macOS 26+, eligible hardware,
*and* a user opt-in this machine does not have, so it can only ever be a
preferred backend behind the `Transformer` trait, never the only one.

#### The integration path was built and run, not assumed

A recommendation to add a Swift shim is worth little if nobody has tried it,
so a working spike lives at `docs/investigations/fm-spike/` (run
`./build.sh`). It is ~85 lines of Swift exposing three C-ABI symbols
(`outloud_fm_availability`, `outloud_fm_transform`, `outloud_fm_free`),
built with `swiftc -emit-library -static`, linked from a Rust binary and
called across the FFI boundary. Measured on this machine:

```
outloud_fm_availability() = 1 (Apple Intelligence not enabled) in 8.7ms
outloud_fm_transform()    = NULL                                in 2.3ms
```

Three things this establishes that reading documentation could not:

1. **It links and runs.** Rust -> Swift -> FoundationModels works with a
   static archive and the two system frameworks.
2. **Degradation is clean.** With the model unavailable, `transform` returns
   null in 2.3ms rather than hanging or trapping, which is precisely the
   behaviour a fallback to llama.cpp depends on. Availability is queryable
   without the user having opted in, so the daemon can choose its backend at
   startup and say something honest.
3. **One real gotcha, found by hitting it.** A Rust binary carries no Swift
   rpath, so the first build died in dyld before `main()` with
   `Library not loaded: @rpath/libswift_Concurrency.dylib`. The fix is
   `-Wl,-rpath,/usr/lib/swift` in `build.rs`. Any real integration pays this,
   and it also implies the shipping app bundle needs its Swift runtime story
   settled before this backend can be released.

What is still unmeasured: **output quality and latency**, because the model
cannot run on this machine without the user enabling Apple Intelligence, and
that is their decision to make on their own machine, not something an
investigation should toggle. So Foundation Models remains the recommended
*direction* on structural grounds (no download, OS-integrated, ANE) rather
than on measured quality. That gap should be closed before committing to it.
Its guided-generation and tool-calling APIs are irrelevant here; plain text
transformation is all we need.

**llama.cpp (current choice).** Works today, measured above, cross-platform,
one C library, matches the whisper.cpp decision. The 1.28 GB download and
~1.5 GB resident are real costs for a dictation tool.

**MLX.** Faster on M-series for some workloads, but Python-first with lagging
Swift/C APIs, and macOS-only. `docs/llm.md`'s reasoning for skipping it still
holds, and nothing measured here changes it.

### Cost to wire it up

The `Transformer` trait, guardrails, sanitation, model download, and preview
type all exist. Wiring is genuinely small:

- add `llm = { path = "../llm" }` to `crates/outloud/Cargo.toml`
- lazily load the model on first freeform request, keep it resident
- route `EditIntent::Freeform` in `inject.rs` to `llm::transform`
- build the preview panel from `docs/ux/03` (this is the real work: it is a
  new overlay state with voice + key controls, not a one-liner)
- gate the whole thing behind the `llama` cargo feature and a config flag

Estimate: the plumbing is a day. **The preview panel is the actual cost**,
and it is unavoidable, because `PreviewedEdit` correctly refuses to let
generated text be applied without one.

---

## 3. Latency budget: does a local model fit?

Dictation is 152-174ms measured. A freeform edit's honest budget is
different from dictation's, because the user has explicitly asked for a
rewrite and `docs/ux/03` already commits to previewing it. The research
target quoted in `docs/llm.md` (TTFT <= 300ms, full rewrite <= 2s) is
reasonable and **is currently met**: measured TTFT p50 264ms, total p90
1046ms.

But three things must hold, and only the first is true today:

1. **The deterministic path must never wait on the model.** True by
   construction: only `Freeform` escalates.
2. **The model must be resident.** Load is 8.6s cold / 200ms warm. A
   first-use 8.6s stall with no UI is unacceptable; it needs either
   predictive load at hotkey-down or an honest "loading" panel, both of
   which `docs/ux/03` already specifies.
3. **1.5 GB resident must be justified.** For a menu-bar dictation tool this
   is a large permanent cost for a feature that currently does nothing
   useful roughly a quarter to a third of the time.

With `new_context` hoisted out of the request, TTFT should drop by the
measured 45ms: roughly 180ms for a short selection and 220ms for a
paragraph, both inside the <=300ms target. **Latency is not the blocker.
Output quality is.**

---

## 4. Freeform edits that need no LLM

This is where the leverage is. A prototype pre-pass
(`crates/edit-intent/examples/scope_prototype.rs`) was written and measured
against 25 of the commands the shipped parser gets wrong or punts. It adds
five families the current grammar has no concept of:

1. **Scope**: `first`/`last`/`this` x `sentence`/`word`/`line`/`paragraph`
2. **Punctuation**: `add a period`, `add a comma after <anchor>`
3. **Wrapping**: quotes, backticks, parens, bold
4. **Identifier case**: snake, camel, kebab, screaming snake
5. **Line ops**: join, split into lines, number, bullet

Result: **21 of 25 handled, 4 correctly escalated, mean parse+apply 1.8-5.2us
across six runs.**
Every one of the 21 outputs is asserted against an exact expected string by
a test, not inspected by eye. Sample:

```
delete the last sentence      -> "...deploy happens today. The customers might possibly be quite upset."
remove the first sentence     -> "The customers might possibly be quite upset. we should tell them soon"
delete the last word          -> "...we should tell them"
capitalize the first word     -> "It is really quite important..."   (was: "IT is really...")
add a period at the end       -> "...we should tell them soon."
add a comma after today       -> "...deploy happens today, The customers..."
wrap this in quotes           -> "\"It is really quite important...soon\""
make it snake case            -> it_is_really_quite_important_that_we...
make it camel case            -> itIsReallyQuiteImportantThatWe...
turn this into bullet points  -> "- It is really...today.\n- The customers...upset.\n- we should tell them soon"
number these lines            -> "1. It is really...\n2. The customers...\n3. we should tell them soon"
undo that / never mind        -> [undo ring, no text edit]
tighten this up               -> model (correct: genuinely open-ended)
```

Three bugs were found and fixed during the prototype, which is itself
evidence the work is non-trivial and deserves real tests: `capitalize the
first word` shouted the word, `add a comma after today` produced `today,.`,
and `number these lines` produced a single item on unbroken dictated prose
(dictation has no line breaks, so list ops must fall back to sentences).

### The prototype is machine-verified, not eyeballed

The prototype lives at `crates/edit-intent/examples/scope_prototype.rs` and
compiles as part of the workspace, so it cannot silently rot. Every one of
the 25 cases is paired with the **exact expected output string**, spelled
out rather than computed, and 8 tests run against it:

```
$ cargo run -p edit-intent --release --example scope_prototype
cases:                 25
handled without model: 21
escalated to model:    4
mean parse+apply:      1.8us
all expectations met.

$ cargo test -p edit-intent --example scope_prototype
test result: ok. 8 passed; 0 failed
```

The tests cover the coverage claim itself, whitespace cleanup after a scoped
delete, punctuation not stacking, an absent anchor reporting no-match rather
than editing arbitrarily, identifier casing dropping punctuation (the exact
thing Qwen3-1.7B got wrong), non-ASCII input not panicking on byte
boundaries (Turkish/Greek/CJK/emoji, the bug class the shipped crate's fuzz
suite already found once), and degenerate empty/whitespace targets.

**Known limitation, pinned by a deliberately failing-if-fixed test:**
sentence splitting is naive. `"Tell Dr. Smith we are done"` splits after
`Dr.`, and decimals like `3.5` are at risk. The test
`known_limitation_sentence_splitting_is_naive` asserts the *wrong* answer on
purpose, so it fails the moment someone improves the splitter and forces the
doc to be updated. A shipping implementation needs an abbreviation list and
a digit-boundary rule before scoped sentence deletes can be trusted; until
then, `docs/ux/03`'s preview-on-large-blast-radius rule is what protects the
user.

### Head-to-head: model vs parser on the SAME commands

The decisive experiment (`examples/det_vs_model.rs`). Each case carries the
exact string a correct deterministic implementation produces, so scoring is
string comparison, not judgement. 10 commands x 3 repeats:

| | Deterministic | Qwen3-1.7B |
|---|---|---|
| Correct | 21/21, asserted | **10% (3/30)** |
| Latency p50 | **~2us** | 324-429ms |
| Latency p90 | ~5us | 366-459ms |
| Predictable | yes, by construction | no |
| Needs preview panel | no | yes |
| Needs 1.28 GB download | no | yes |

The head-to-head was run twice. The 3/30 correct figure reproduced exactly;
the latency p50 moved between 324ms and 429ms across the two runs, which is
machine load, not a property of the model. Either way it is five orders of
magnitude above the deterministic path.

Where the model failed:

```
delete the last sentence  -> returned input unchanged (0/3)
delete the last word      -> returned input unchanged (0/3)
wrap this in quotes       -> returned input unchanged (0/3)
make it kebab case        -> returned input unchanged (0/3)
make it snake case        -> "we_should_ship_today.the_customers..."  (kept the periods)
make it camel case        -> "WeShouldShipToday.TheCustomers..."      (kept the periods, wrong initial case)
number these lines        -> numbered the whole text as item 1 (0/3)
turn this into bullet points -> dropped the sentence-final periods
capitalize the first word -> capitalized every sentence AND rewrote "lets" to "Let's"
add a period at the end   -> correct (3/3)
```

The last one is instructive: the model "fixed" `lets` -> `Let's` when asked
only to capitalize the first word. That is precisely the unasked-for rewrite
the guardrails exist to catch, and the word-retention guardrail is too coarse
to catch a single-word change.

**The parser was correct on every case; the model was correct on 10% of
them, at five orders of magnitude more latency. This is not a close call.**

---

## Recommendations, in priority order

### 1. Fix the silent-dictation bug (highest priority, smallest change)

A freeform edit spoken at a selection currently types the command into the
user's document. Distinguish "unrecognised phrase, probably dictation with a
stale selection" from "recognisably an edit request we cannot serve". A
freeform *instruction* (imperative verb + no literal operands: tighten,
formalize, summarize, rephrase) should reach `FreeformUnsupported` and its
existing honest message; anything else keeps today's insert behaviour. This
is a guard in one `if` in `inject.rs`, plus a test.

### 2. Extend the deterministic parser (highest value)

Land the five families above in `crates/edit-intent` as a scope-aware layer:
`EditIntent` grows a `scope: Option<Scope>` and the new operations become
variants. This converts 21 of 25 currently-broken commands into instant,
predictable, undoable edits at microsecond cost, fixes 10 silent misparses
including the field-shouting `uppercase the first letter`, and needs no
download, no preview panel, no RAM, and no new dependency. It also makes
`docs/ux/03`'s promised scope narrowing ("in the last sentence, change its to
it's") real.

Budget honestly: this is a few days of careful work with a fuzz suite, not an
afternoon. The three bugs found in a 500-line prototype are the evidence.

### 3. Fix the two measured defects in `crates/llm`

Both are small and both are worth doing regardless of when the crate is
wired in:
- Drop system-prompt rule 5, replace with an explicit obligation to change
  the text. Measured over 432 requests: echo rate 36.6% -> 24.1%
  (1.5x, p=0.005). Real but not sufficient on its own.
- Hoist `new_context` out of `transform_streaming` and clear the KV cache
  between requests instead. Measured A/B: **45ms saved per request**, 19% of
  the prefill path.
- Also fix `default_cache_dir()`: `~/.aqua-oss/models` -> `~/.outloud/models`.

### 4. Make `OUTLOUD_NO_INJECT=1` exercise the decision logic

Move the check from the top of `deliver` down to the transports, so intent
parsing, mode dispatch, and outcome selection all run and get reported. Right
now the safe measurement path cannot see any of the behaviour this
investigation was asked to measure.

### 5. Wire the LLM last, behind a feature flag, with Foundation Models as
the preferred backend

When freeform does land: add a `FoundationModelsTransformer` behind the
existing `Transformer` trait (Swift shim, macOS 26+, zero download, ANE), fall
back to the llama.cpp backend, and degrade to the honest
`FreeformUnsupported` message when neither is available. Build the preview
panel from `docs/ux/03`. Do not ship Qwen3-1.7B as the only backend at a
measured do-nothing rate of 36.6% (24.1% even with the prompt fix).

---

## Confidence

**High** on: the deterministic path's behaviour and speed; the silent-
dictation bug; `NO_INJECT` short-circuiting; the `llm` crate being unwired;
the llama.cpp latency numbers; the model's echo rate (n=216 per prompt
variant); Foundation Models being present but unavailable; that a Swift-shim
integration links, runs, and degrades cleanly; the parser-vs-model
head-to-head. All directly measured, most re-run several times, and the two
behaviour findings and the coverage claim are now pinned by tests.

**Corrected during the investigation, and worth flagging as a caution about
the rest:** the prompt-ablation claim that removing rule 5 "roughly halves"
the echo rate was an artifact of 12-sample runs. Re-tested at n=216 per
variant, the honest figure is 1.5x (36.6% -> 24.1%, p=0.005), and the first
confirmation attempt at n=72 was *not* significant (p=0.063). Any other
figure here drawn from a dozen samples deserves the same treatment before it
is leaned on.

**Medium** on: how much a 4B model would improve the echo rate (not
measured, no 4B model downloaded); the true cost estimate for the preview
panel (read the spec, did not build it).

**Unmeasured, and flagged rather than glossed:** Foundation Models' output
*quality and latency*. The framework is present and the integration works,
but the model itself cannot run without the user enabling Apple
Intelligence on their own machine. The recommendation to prefer it rests on
structural properties (ships with the OS, no download, ANE, matches the
existing `SpeechTranscriber` dependency), not on measured output. Anyone
acting on recommendation 5 should close that gap first.

**Deliberately not claimed**: the 55-command corpus is my construction, not
observed user traffic. The 21/25 coverage figure is a statement about that
corpus. It is a reasonable corpus, drawn from `docs/ux/03`'s own examples and
common editing operations, but real usage would shift the mix.

## Artifacts added by this investigation

New examples under `crates/llm/examples/` (all `required-features = ["llama"]`,
so CI never builds them):

- `bench_investigation.rs` - latency distribution over a realistic case set
- `prompt_ablation.rs` - four system-prompt variants, measured head to head
- `size_sweep.rs` - usable-output rate vs input size
- `det_vs_model.rs` - model vs deterministic on the same commands
- `ttft_breakdown.rs` - warm TTFT by input size, reconciling the figures
- `ctx_cost.rs` - where TTFT actually goes
- `ctx_reuse.rs` - fresh-context vs reused-context A/B
- `rule5_confirm.rs` - powered re-test of the rule-5 prompt effect with a
  two-proportion z-test (the 12-run ablation was underpowered)

One small library change: `LlamaTransformer::with_system_prompt`, so prompt
changes can be A/B measured against a live model instead of argued about.

Two regression tests under `crates/outloud/tests/`, both pure (no transport,
no accessibility grant, cannot type anywhere), pinning findings (b) and (c):

- `freeform_path_divergence.rs` - `payload_for` reports freeform edits as
  unsupported, and also cannot distinguish a rewrite request from prose,
  which is the imprecision that makes the macOS blanket-insert defensible
  and defines what a fix has to disambiguate.
- `no_inject_guard.rs` - the `OUTLOUD_NO_INJECT` guard returns before any
  edit is computed. Lives in its own integration binary because it mutates a
  process-global env var; putting it in `inject.rs`'s unit tests races the
  sibling tests that call `deliver`, which was verified by doing it and
  watching `edit_with_absent_search_text_reports_no_match` fail.

Plus three spikes preserved for whoever picks this up:

- `crates/edit-intent/examples/shipped_parser_corpus.rs` - the 55-command
  corpus behind the parser-correctness table, with 4 tests asserting the
  exact counts and the two worst misparses.
- `crates/edit-intent/examples/scope_prototype.rs` - the parser extension,
  with 8 tests asserting the 21/25 coverage claim, whitespace and
  punctuation correctness, non-ASCII safety, and one deliberately
  failing-if-fixed test pinning the naive sentence splitter.
- `docs/investigations/fm-spike/` - a working Rust -> Swift ->
  FoundationModels C-ABI shim (`./build.sh` builds and runs it), proving the
  recommended integration path links, runs, and degrades cleanly.

All harnesses cited above were re-verified to build and run at the end of the
investigation. `cargo test` is green for `edit-intent`, `llm`, and `outloud`
(15 suites), and `cargo fmt --check` and `cargo clippy` are clean on every
file touched here.

Two caveats about the wider tree, neither mine: `crates/audio` has an
in-progress example from another agent that does not compile, and
`crates/config` had 3 failing tests at the time of writing from another
agent's in-flight `silence-timeout-ms` work. `crates/outloud/src/inject.rs`
and `crates/edit-intent/src` (the files every finding here rests on) were
confirmed byte-unmodified, and the two new tests still pass against the
other agent's concurrent changes to `pipeline.rs`.

The prototype lives at `crates/edit-intent/examples/scope_prototype.rs`,
compiles with the workspace, and carries its own tests. It should be moved
into the crate proper (behind a real `Scope` type on `EditIntent`) when
recommendation 2 is taken.

## Not done, deliberately

No fix was landed for findings (a), (b), or the parser extension. This was
an investigation, and each of those is a product decision with a UX surface
attached: (b) needs a rule for telling a rewrite request from prose, and the
parser extension changes `EditIntent`'s shape. Both deserve their own review
rather than being smuggled in under a research task. The two tests added are
characterization tests: they assert what the code does today, so they pass
now and will fail loudly when someone changes it on purpose.
