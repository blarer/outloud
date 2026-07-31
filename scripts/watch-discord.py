#!/usr/bin/env python3
"""Watch Discord's message box while a real dictation happens.

Everything measured so far drove the pipeline directly, which proves the
write works but says nothing about what Discord does with it afterwards.
This samples the focused field ~10x/second and prints only CHANGES, so the
sequence is visible: text appearing, text being cleared by Discord's own
reconciliation, or never arriving at all.

Run it, then hold the hotkey and speak into Discord.
"""
import subprocess
import sys
import time

POLL_S = 0.1
DURATION_S = float(sys.argv[1]) if len(sys.argv) > 1 else 45.0

READ_FIELD = '''
tell application "System Events"
    tell process "Discord"
        try
            get value of attribute "AXValue" of (value of attribute "AXFocusedUIElement")
        on error
            return "<<no focused element>>"
        end try
    end tell
end tell
'''

FRONT_APP = '''
tell application "System Events"
    get name of first application process whose frontmost is true
end tell
'''


def osa(script: str) -> str:
    r = subprocess.run(["osascript", "-e", script], capture_output=True, text=True)
    return (r.stdout or r.stderr).strip()


def main() -> None:
    print(f"watching Discord's focused field for {DURATION_S:.0f}s")
    print("hold the hotkey and speak into Discord now\n")
    start = time.time()
    last_val = object()
    last_app = object()

    while time.time() - start < DURATION_S:
        t = time.time() - start
        app = osa(FRONT_APP)
        if app != last_app:
            print(f"[{t:6.2f}s] frontmost -> {app}")
            last_app = app
        val = osa(READ_FIELD)
        if val != last_val:
            shown = val if len(val) <= 90 else val[:87] + "..."
            print(f"[{t:6.2f}s] field({len(val):4d}) {shown!r}")
            last_val = val
        time.sleep(POLL_S)

    print("\nwatch finished")


if __name__ == "__main__":
    main()
