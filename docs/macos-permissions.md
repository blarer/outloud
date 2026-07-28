# Platform notes: macOS permissions

Findings from the M0 spike that will shape the shipping product's onboarding.
These cost real hours to discover, which is exactly what a milestone-zero spike
is for.

## The grant follows the responsible process, not the binary

macOS attributes an Accessibility grant to a process's *responsible process*,
which is the application the system considers to have launched it. A binary
executed directly from a shell inherits the terminal as its responsible process.
The system then checks the **terminal's** permission and ignores the binary's
own grant entirely.

The symptom is maddening and non-obvious: the app appears in System Settings
with its toggle switched on, and every call still fails.

The fix is to launch through LaunchServices, which makes the app responsible for
itself:

```bash
open -a dist/AquaSpike.app --args probe
```

`scripts/run.sh` wraps this. Because LaunchServices detaches the process from
the terminal, the binary mirrors its output to the file named by
`OUTLOUD_SPIKE_LOG` and the script tails it.

This is not a workaround. It is how every real user starts a real application,
so the shipping product is unaffected. It only bites during development, and it
will bite every engineer who joins the project, which is why it is written down.

## The grant is pinned to the code signature

TCC records the approval against the binary's `cdhash`. Re-signing invalidates
it, so this sequence silently breaks:

1. Build and sign the bundle.
2. Grant Accessibility permission.
3. Rebuild, which re-signs with a new `cdhash`.
4. Every call now fails while the toggle still reads "on".

During development, reset and re-grant rather than trying to reason about it:

```bash
tccutil reset Accessibility dev.hexavoice.spike
```

For the shipping product this disappears once builds are signed with a stable
Developer ID certificate, because the requirement is then pinned to the team
identifier rather than to a per-build hash. Budget for the certificate early;
it is not only a distribution concern, it is a permissions concern.

## Error codes worth translating

| Code | Constant | What it actually means here |
|---|---|---|
| `-25204` | `kAXErrorCannotComplete` | Almost always "not trusted", not "busy" |
| `-25211` | `kAXErrorAPIDisabled` | Accessibility disabled for this process |
| `-25212` | `kAXErrorNoValue` | Attribute absent; normal, not an error |
| `-25205` | `kAXErrorAttributeUnsupported` | Element does not offer this attribute |

`ax-edit` maps the first two onto `AxError::NotTrusted`, because reporting them
as generic API failures sends people debugging the wrong problem. The last two
are mapped to `Ok(None)`, since an element legitimately not having an attribute
is an expected outcome rather than a failure.

## Calls must be time-bounded

The accessibility API is synchronous IPC into another application. A busy or
hung target, typically a spinning Electron renderer, will block the caller
indefinitely. In a dictation tool that means the user's hotkey appears to do
nothing at all.

`ax-edit` sets a 500ms messaging timeout on the system-wide element, which is
inherited by every element derived from it.

## Consequences for the product

1. **Onboarding is a first-class feature, not a footnote.** The permission is
   deliberately un-grantable by software; a human must click it. The most a tool
   can do is remove every step of friction around that click, then verify and
   confirm success. `scripts/grant-accessibility.sh` is the prototype of that
   flow.
2. **Get the Developer ID certificate before the first external tester.** Ad-hoc
   signing makes the permission fragile in a way that will read to users as the
   application being broken.
3. **Every error message must name the next action.** "accessibility API error
   -25204" is useless; "permission not granted, open this pane" is not.
