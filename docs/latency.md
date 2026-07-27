# Latency: measured breakdown of the OS-integration path

Measured on this machine (M-series, macOS 26) with criterion, 2026-07-27.
Reproduce with `./scripts/bench-latency.sh` (benches) and
`./scripts/bench-gate.sh` (regression gate). Target application: TextEdit
with a 44-character document focused, the native-AppKit baseline. All numbers
are criterion mean estimates unless marked cold.

## The headline finding: M0's "33ms read" was the cold path

M0 reported read 25-33ms. The steady-state truth is three orders of magnitude
smaller: a **warm** `snapshot_focused` costs **~155us**. The gap is the cost
of first contact: the accessibility connection to the target process is
established lazily on the first message, and every call in that first
conversation pays milliseconds instead of microseconds.

Cold one-shot measurements (first AX calls this process ever makes, so noisy
by nature, but consistently in this range across runs):

| Step (cold, first ever call) | Measured |
|---|---|
| resolve focused application | 8.6-11.3 ms |
| resolve focused element | 10.4-10.8 ms |
| first full snapshot | 4.8-6.6 ms |
| **cold total** | **~20-29 ms** |

That reproduces M0's number and locates it: the cost is per-target-process
connection setup, not per attribute. A long-running dictation daemon pays it
once per application, then runs at the warm numbers below. The M0 CLI paid it
on every invocation because every invocation was a fresh process.

## Warm per-call breakdown

| Call | Mean | Notes |
|---|---|---|
| `AXUIElementCreateSystemWide` | 49 ns | local, no IPC, confirmed |
| resolve app via system-wide `AXFocusedApplication` | 34-61 us | one IPC |
| resolve app via `CGWindowListCopyWindowInfo` fallback | 176-203 us | window-server IPC, 3-5x costlier |
| `AXFocusedUIElement` given app element | 16-22 us | one IPC |
| full resolution (app + focused element) | 69-90 us | |
| `AXRole` read | 25-26 us | |
| `AXValue` read (44 chars) | 28-30 us | |
| `AXSelectedText` read | 27-30 us | |
| `AXSelectedTextRange` read | 23-29 us | |
| `AXNumberOfCharacters` read | 22-28 us | |
| `AXTitle` on app element | 23-30 us | |
| **batched 5-attribute read** (`AXUIElementCopyMultipleAttributeValues`) | **40-52 us** | one IPC for all five |
| `AXUIElementIsAttributeSettable` (each) | 22-26 us | two per snapshot |
| write `AXValue` (44 chars, same text) | 264-269 us | includes target-side text work |
| `snapshot_focused` (full, per-attribute reads) | 216-218 us | before change |
| `snapshot_focused` (full, batched reads) | **154-155 us** | after change, **-28%** (p < 0.05) |

The shape is exactly the suspected one: every warm AX call costs ~22-30 us of
round-trip overhead *regardless of payload*. Latency is a function of round
trips, not bytes.

## Hypotheses tested

**1. Batch the attribute reads into one IPC. CONFIRMED, implemented.**
Five separate reads cost ~135 us; the batched call costs ~40-52 us (about 2x
one single read, not 5x). `snapshot_focused` now uses
`AXUIElementCopyMultipleAttributeValues` with a per-attribute-read fallback
for accessibility servers that reject the batch call. Measured effect on the
full snapshot: 216 us -> 155 us, -28% (criterion, p = 0.00). All 74
pre-existing tests pass. The win multiplies on the cold path and in slow
targets: in Chrome one attribute read costs 473 us while the whole 5-batch
costs 557 us, so batching there saves ~1.8 ms per snapshot.

**2. Cache the focused-application element. MEASURED, not implemented.**
Resolving the app costs 34-61 us warm; asking a cached app element for its
focused element costs 16-22 us. Saving: ~40-70 us per snapshot, roughly a
quarter of the current 155 us. It needs an AXObserver on focus-change plus a
run loop to invalidate the cache, which is real machinery and a staleness
risk (acting on the wrong application) for a sub-tenth-of-a-millisecond win.
Worth doing later in the daemon, where the run loop exists anyway and where
it also amortises the *cold* connection cost, which is the actually large
number. Not worth it in the CLI spike.

**3. Skip the `is_settable` probes. MEASURED, not implemented.**
The two probes cost ~46 us of the 155 us snapshot. They cannot be batched
(`AXUIElementIsAttributeSettable` has no multi variant). The write path
(`replace_focused`) could replace its probe with "attempt the write, fall
back on failure", but a failed `AXUIElementSetAttributeValue` may have
partial effects in hostile targets, and the probe result is also what
`TextSnapshot::strategy()` reports to callers. 46 us does not justify
changing observable semantics; revisit only if the snapshot budget ever
tightens below ~100 us.

**4. Window-list fallback is expensive relative to the primary route.
MEASURED, negative result worth knowing.** `CGWindowListCopyWindowInfo`
costs 176-203 us, 3-5x the system-wide route. On this machine the system-wide
`AXFocusedApplication` route *succeeds* (M0 observed it failing; that failure
mode is environmental, tied to trust attribution), so the fallback is dormant
and no change is warranted. Do not "optimise" by reordering the routes.

**5. Cross-application cost (warm, app-element reads).**

| Target | one `AXRole` read | batched 5 reads |
|---|---|---|
| Safari (WebKit) | 22 us | 49 us |
| Discord (Electron) | 20 us | 35 us |
| Chrome (Chromium) | **473 us** | **557 us** |

Warm Electron is not slow. Chrome's accessibility server is ~20x slower per
round trip than everything else, which makes the batch read the difference
between a 2.4 ms and a 0.6 ms five-attribute snapshot there.

## Regression gates

`cargo bench -p ax-edit --bench gate` (or `./scripts/bench-gate.sh`, which
stages a focused TextEdit first) samples 200 real snapshots through
`diag::timing::Recorder` and enforces:

| Metric | Budget | Measured now | Headroom |
|---|---|---|---|
| read p50 | 2 ms | 134 us | ~15x |
| read p99 | 50 ms | 2.6 ms | ~19x |

Budgets are deliberately loose: they exist to catch a lost batch read or an
added synchronous round trip (which shows up as tens of percent), not to flag
a busy CI machine. The gate exits 0 with an explanation when the environment
cannot produce a valid measurement, and exits 1 on a genuine breach.

## Recommendations, ranked by measured win

1. **Done: batched attribute reads** (-28% warm snapshot, ~-1.8 ms in Chrome,
   larger still on cold paths).
2. **For the daemon: keep the process alive and keep app elements warm.** The
   only millisecond-scale cost in this whole path is cold connection setup
   (~20-29 ms). A resident process pays it once per target application; that
   is the real fix for M0's 33 ms, and no micro-optimisation competes with it.
3. **Daemon-era: cache the app element with AXObserver invalidation**
   (~40-70 us warm, plus cold-path amortisation; needs the run loop the
   daemon will have anyway).
4. **Do not bother:** skipping settable probes (46 us, changes semantics),
   reordering resolution routes (fallback already dormant), payload-size
   optimisations (cost is per round trip, not per byte).
