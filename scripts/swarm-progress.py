#!/usr/bin/env python3
"""Render the Windows-parity swarm's progress as a single markdown file.

The agents run on the Mac and jcode scopes swarm membership by working
directory, so a jcode on the Windows box cannot see them. Their journals are
plain files though, so this turns them into something readable from anywhere.

Written to the repo so it syncs to the PC on the next git pull.
"""
import json
import pathlib
import time

SESSIONS = pathlib.Path.home() / ".jcode/sessions"
AGENTS = [
    ("raccoon", "session_raccoon_1785616884210_33a1eb69ccf9cb6d", "hotkey parity"),
    ("flamingo", "session_flamingo_1785616905150_e438b3f72ee4f3ec", "injection parity"),
    ("dolphin", "session_dolphin_1785616929378_35f392c1acf2a15f", "platform parity"),
]


def read(session_id):
    """Assistant text and reasoning, newest last, plus liveness."""
    path = SESSIONS / f"{session_id}.journal.jsonl"
    if not path.exists():
        return None, [], 0
    status, blocks, tools = None, [], 0
    for line in path.read_text(errors="ignore").splitlines():
        try:
            d = json.loads(line)
        except Exception:
            continue
        if "meta" in d:
            status = d["meta"].get("status")
        for m in d.get("append_messages") or []:
            if m.get("role") != "assistant":
                continue
            c = m.get("content")
            if not isinstance(c, list):
                continue
            for b in c:
                if not isinstance(b, dict):
                    continue
                if b.get("type") == "tool_use":
                    tools += 1
                elif b.get("type") in ("text", "reasoning_trace"):
                    t = b.get("text", "")
                    if len(t) > 120:
                        blocks.append(t)
    return status, blocks, tools


out = [
    "# Windows parity audit: live progress",
    "",
    f"Generated {time.strftime('%Y-%m-%d %H:%M:%S')} on the Mac.",
    "",
    "Three read-only auditors comparing the Windows build against macOS.",
    "None of them touch the Windows machine.",
    "",
]

for name, sid, role in AGENTS:
    status, blocks, tools = read(sid)
    out += [f"## {name} - {role}", "", f"status: **{status or 'unknown'}**, {tools} tool calls", ""]
    if blocks:
        out += ["Latest reasoning:", "", "> " + blocks[-1][:1200].replace("\n", "\n> "), ""]
    else:
        out += ["_No output yet._", ""]

dest = pathlib.Path("docs/windows-parity-progress.md")
dest.write_text("\n".join(out))
print(f"wrote {dest} ({len(out)} lines)")
