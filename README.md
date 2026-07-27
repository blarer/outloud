# Aqua OSS Spike (M0)

A four-week milestone-zero spike toward an open-source, fully-local alternative
to [Aqua Voice](https://withaqua.com). Research behind it lives in
`../aqua-voice-research/`.

## What this spike is for

The research concluded that the machine learning is the solved part. Local
speech recognition in 2026 (Parakeet TDT, Moonshine, whisper.cpp) already beats
Aqua's ~450ms insert latency while keeping audio on the device. Existing
open-source projects such as [Handy](https://github.com/cjpais/Handy) already
ship audio capture, voice activity detection, and multiple recognizer backends
under a permissive licence.

What no open-source project has solved is **edit-by-voice on text the user has
already committed**: selecting a sentence in any application, saying "change
hello to goodbye", and having it rewritten in place. That requires reading the
focused text field out of another process and writing a replacement back into
it, through each operating system's accessibility layer.

M0 exists to prove that one capability, in real applications, before any team is
hired or any recognizer is wired up. If this does not work reliably, the whole
product thesis is wrong and it is much better to learn that in week one.

## Layout

| Crate | Responsibility |
|---|---|
| `ax-edit` | Read and rewrite the focused text field via the macOS Accessibility API |
| `edit-intent` | Turn a spoken phrase into a deterministic text transformation |
| `spike-cli` | Harness that exercises both and measures the result |

The split is deliberate. `edit-intent` has no operating-system dependency, so it
runs and is tested anywhere, including CI. `ax-edit` isolates all the unsafe FFI
behind a small safe surface and returns `Unsupported` off macOS, so the Windows
and Linux backends can be added later without disturbing callers.

## Design decisions worth knowing

**A language model is the fallback, not the first resort.** Most real edit
commands are a small closed set: replace, delete, append, recase. A
deterministic parser handles them in microseconds with no GPU and no risk of a
model rewriting text nobody asked it to touch. Only genuinely open-ended
instructions ("tighten this up") escalate to a local model. This is what keeps
the latency budget dominated by speech recognition rather than by generation.

**Three rewrite strategies, best first.** Writing `AXSelectedText` goes through
the target application's own text system, so undo and change notifications keep
working. Writing `AXValue` works in simpler fields but usually resets undo.
Applications that expose text but refuse writes fall back to a synthesized
paste. `TextSnapshot::strategy()` reports which one a given field supports.

**Every accessibility call is time-bounded.** These calls are synchronous IPC
into another process. A spinning Electron renderer would otherwise hang the
dictation hotkey, which the user experiences as the application being broken.

## Running it

```bash
# Build and package. The .app bundle gives the TCC permission system a stable
# identity to attach to, which a bare ad-hoc-signed binary does not have.
./scripts/bundle-macos.sh

# Grant Accessibility permission. Opens the right pane and waits for the toggle.
./scripts/grant-accessibility.sh

BIN=dist/AquaSpike.app/Contents/MacOS/AquaSpike

$BIN probe                          # read the focused field right now
$BIN watch 500                      # poll it while you tab between applications
$BIN edit "change hello to goodbye" # full read-interpret-apply-write pipeline
$BIN matrix                         # the guided application test checklist
```

The intent parser needs no permission at all:

```bash
$BIN dry-run "change quick to slow"
$BIN dry-run "make it all caps"
$BIN dry-run "tighten this up"      # escalates to freeform
```

## M0 exit criteria

Run `$BIN matrix` for the checklist. In-place rewrite must succeed in at least
the native (TextEdit), browser (Safari), and Electron (VS Code, Slack) rows. A
read-only terminal is an acceptable paste-fallback row rather than a failure,
because terminals legitimately do not expose a writable text field.

Beyond that checklist, M0 is met when end-to-end finalization stays under 800ms
with a real recognizer attached. The `edit` command already prints a per-stage
timing breakdown, so that number is measured rather than estimated from the
first day.

## Testing

```bash
cargo test
```

`edit-intent` carries the interesting cases: commands whose search text contains
the joiner word ("change to do to todo"), case-insensitive matching because
speech recognition does not reproduce on-screen casing, whitespace cleanup after
a deletion, and non-ASCII input where lowercasing changes a string's byte length
and naive byte slicing would panic.

## Not yet built

- Speech recognition. M0 deliberately isolates the operating-system risk first.
- Windows (`UIAutomation` `TextPattern`) and Linux (`zwp_input_method_v2`)
  backends.
- The clipboard-paste fallback for read-only fields. The strategy is detected
  and reported; the fallback itself belongs in the application, which owns the
  clipboard.
- An undo stack. Aqua's edits are stackable; rewriting `AXValue` resets the host
  application's undo, so the client must keep its own.
