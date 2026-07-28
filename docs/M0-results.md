# M0 results

The question M0 was built to answer: **can we read and rewrite the focused text
field, in place, inside other applications?**

**Yes.** Verified end to end, with timings well inside budget.

## The end-to-end result

In TextEdit, with the sentence "the quick brown fox jumps over the lazy dog" in
the document:

```
$ spike-cli edit --after 5 change quick to slow

heard:   "change quick to slow"
intent:  replace "quick" with "slow"
scope:   whole field
before:  "the quick brown fox jumps over the lazy dog"
after:   "the slow brown fox jumps over the lazy dog"

timing:  read 33.5ms | parse 7us | apply 1us | write 13.4ms
wrote via set-value
```

The document visibly changed. This is the capability no open-source dictation
tool currently has.

## Latency

| Stage | Measured | Share of an 800ms budget |
|---|---|---|
| Read focused field | 25-33ms | ~4% |
| Parse spoken command | 2-39us | negligible |
| Apply transformation | ~1us | negligible |
| Write back | 13.4ms | ~2% |
| **Total** | **~47ms** | **~6%** |

The operating-system integration consumes about 6% of the budget. Speech
recognition will dominate, which is the right shape: it means the remaining
latency work belongs to the recognizer, where the research already identified
stacks that fit (Moonshine partials at 150-250ms, Parakeet TDT finalizing).

## Application coverage

| Application | Family | Result |
|---|---|---|
| TextEdit | native AppKit | **pass**, read and write, `set-selected-text` available |
| Safari (address bar) | native chrome | **pass**, writable |
| Safari (web content) | WebKit page | **pass**, page `AXTextArea` found with live contents, writable |
| Terminal | terminal emulator | no text field, uses paste fallback (expected) |
| Notes, Mail | native | not confirmed, no windows on the test Space |
| Chrome | Chromium | not confirmed, no windows on any reachable Space |

Native and browser rows pass, including real web content rather than only the
browser's own chrome. The unconfirmed rows are a test-environment limitation:
the window server does not expose windows that live on another Space. That does
not affect the product, which only ever acts on the focused application on the
current Space.

## What made this hard

Four findings, each of which would have cost a new engineer a day.

**1. The system-wide element does not work.**
`AXUIElementCreateSystemWide()` then `AXFocusedUIElement` is what almost every
example shows. On current macOS it returns `kAXErrorCannotComplete` (-25204)
even for a fully trusted process. The route that works is to resolve the focused
*application* first, then ask it for its focused element. This alone probably
explains why the capability is missing from open-source tools: it looks
impossible until you stop using the documented shortcut.

**2. TCC grants follow the responsible process.**
A binary run from a shell is judged against the terminal's permission, not its
own. The application appears in System Settings with its toggle on and every
call still fails. Launching through LaunchServices fixes it, and is how real
users start real applications anyway.

**3. Ad-hoc signatures make grants fragile.**
TCC pins the approval to the binary's `cdhash`, so every rebuild silently
revokes it while the toggle continues to read "on". A Developer ID certificate
is needed early, for permissions rather than for distribution.

**4. Applications hang their windows off `AXWindows`, not `AXChildren`.**
An application element's children are its menu bar. Walking children finds
thousands of menu items and zero text fields.

Chromium additionally requires setting the private `AXManualAccessibility`
attribute before it exposes any tree at all. This is implemented but not yet
confirmed against a live Chrome window: on the test machine Chrome reported
zero windows from every Space that could be reached, so the opt-in never had a
tree to act on. The code path is written and needs one clean test on a machine
with a Chrome window on the active Space.

## What this means for the plan

The M0 thesis holds. The operating-system integration, which the research called
the hard part, is proven on macOS at 6% of the latency budget. The risks that
remain are the ones the plan already named: Wayland injection, Electron
coverage, and edit accuracy at scale.

Recommended next steps, in order:

1. Confirm the Chromium path against a live Chrome or VS Code window. The
   `AXManualAccessibility` opt-in is written and needs one clean test.
2. Attach a recognizer. Parakeet TDT via ONNX, following Handy's existing
   integration, and measure the real end-to-end number.
3. Build the edit-accuracy evaluation harness before adding more commands. The
   plan calls for this at M0 precisely so accuracy does not plateau unnoticed.
4. Implement the clipboard-paste fallback for read-only fields.
5. Add an undo stack. Writing `AXValue` resets the host application's undo, so
   the client must keep its own to match Hexavoice's stackable edits.
