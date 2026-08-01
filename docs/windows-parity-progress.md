# Windows parity audit: live progress

Generated 2026-08-01 16:44:13 on the Mac.

Three read-only auditors comparing the Windows build against macOS.
None of them touch the Windows machine.

## raccoon - hotkey parity

status: **Active**, 19 tool calls

Latest reasoning:

> I'm tracing the hotkey event pipeline—specifically where FrontendEvent gets generated from HotkeyEvent and whether that conversion happens asynchronously or blocks the hook thread. I need to find the bridging code that connects these two event types.

## flamingo - injection parity

status: **Active**, 26 tool calls

Latest reasoning:

> I'm realizing that `select()` and `detect()` are only used by spike-cli, not the actual daemon pipeline—the Windows dictation path goes through `deliver_via_tiers` in inject.rs, which manually handles UIA, SendInput, and Clipboard operations without ever consulting `accepts()`.
> 
> Now I need to check whether the hardcoded 150ms clipboard settle time was actually measured for Windows, and compare it against the macOS paste implementation to see what sleep duration it uses.

## dolphin - platform parity

status: **Active**, 36 tool calls

Latest reasoning:

> Windows target installed. Try check without the "2>&1 | grep" piping that trips the blocker (looks like it's blocking due to /dev/null false positive from tail maybe). Let me retry plainly.
