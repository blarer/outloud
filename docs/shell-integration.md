# Shell integration (T2): edit-by-voice on the command line

This document specifies the shell-bridge: the mechanism that gives OutLoud
read-and-rewrite access to the one real text field a terminal has, the
shell's line buffer. It implements tier T2 of the ladder in
`ux/04-terminal-and-headless.md`. No GUI dictation tool (Aqua Voice, Wispr
Flow) and no open-source tool does this; a terminal exposes no writable
accessibility field (M0 measured), so the only in-place, undo-preserving
edit path is cooperating with the line editor itself.

## Architecture

```mermaid
sequenceDiagram
    participant V as voice pipeline
    participant D as shell-bridge daemon
    participant S as shell plugin (ZLE / readline / fish)
    V->>D: INTENT "change prod to staging"
    Note over D: staged, 30s TTL
    S->>D: EDIT v1 zsh <cursor> <b64 buffer>   (user presses ^X^A)
    Note over D: edit-intent parse + apply,<br/>cursor remapped
    D->>S: REPLACE <cur_bytes> <cur_chars> <b64 new buffer>
    Note over S: BUFFER=$new; CURSOR=$cur<br/>shell's own undo records it
```

The inversion that makes this work: **only the shell process can touch its
line editor's state**, so the daemon never pushes. The voice pipeline
*stages* an intent; the shell *pulls* it by offering its buffer when the
user presses the plugin's key. The shell applies the returned rewrite by
assigning its own buffer variable, which means the edit goes through the
editor's normal state transitions and its native undo (`^Xu` in zsh, `C-_`
in readline, `ctrl-z` in fish) reverts it. This is the terminal equivalent
of preferring `AXSelectedText` over `AXValue` in the GUI tiers.

Pieces:

- `crates/shell-bridge`: the daemon (`shell-bridge serve`), a one-shot CLI
  client (`intent`, `status`, `peek`), and the installer (`install`).
- `shell/outloud.zsh`, `shell/outloud.bash`, `shell/outloud.fish`: the plugins.
- `shell/demo.sh`: end-to-end interactive demo in a throwaway ZDOTDIR.
- `shell/verify-zsh.exp`: automated pty-level verification.

An intent is consumed by the first `EDIT` that arrives, and expires after
30 seconds, so a stale utterance can never fire into a later command line.

## Protocol

Unix stream socket. One request line, one response line, connection closed.
The controlling constraint is that a client must be writable in shell with
only `printf`, `base64`, and `nc -U`. That rules out JSON, length-prefixed
framing, and multi-round-trip handshakes. Payloads are base64 because
command lines legally contain newlines and the protocol is line-framed;
base64 also removes every quoting question a shell could get wrong.

Requests:

| Line | From | Meaning |
|---|---|---|
| `EDIT v1 <shell> <cursor> <b64 buffer>` | shell plugin | offer the buffer, apply any staged intent |
| `INTENT <b64 utterance>` | voice pipeline | stage an edit (30s TTL, replaces previous) |
| `STATUS` | anyone (same uid) | daemon state, human-readable |
| `PEEK` | voice pipeline | last buffer a shell offered |

Responses (exactly one line):

| Line | Meaning |
|---|---|
| `REPLACE <cursor_bytes> <cursor_chars> <b64 buffer>` | apply this buffer, move cursor |
| `NOOP <b64 reason>` | nothing to do (no intent staged, or intent didn't match) |
| `OK` | intent accepted |
| `BUFFER <b64 buffer>` | PEEK result |
| `STATUS <b64 text>` | STATUS result |
| `ERR <b64 reason>` | malformed request or rejected peer |

`<shell>` is `bash`, `zsh`, `fish`, or `other`, and it exists because the
shells disagree about what a cursor is: readline's `READLINE_POINT` counts
**bytes**, zsh's `$CURSOR` and fish's `commandline -C` count **characters**.
The daemon normalizes on receipt and `REPLACE` carries the new cursor in
both units so no plugin ever does unicode arithmetic in shell.

Lines are capped at 1 MiB; longer requests are dropped as confused or
hostile. Socket path: `$XDG_RUNTIME_DIR/outloud/shell.sock` (Linux), else
`$TMPDIR/outloud/shell.sock` (macOS), else `/tmp/outloud-$UID/outloud/shell.sock`.
Plugins and daemon both honor `$OUTLOUD_BRIDGE_SOCKET` as an override.

Cursor placement after an edit is reconstructed by diffing old and new
buffers (longest common prefix/suffix): a cursor before the change stays, a
cursor after it keeps its distance from the end, a cursor inside the change
lands at the end of the replacement, which is where a human resumes typing.

## Threat model

The socket is a command-staging surface: anything that can speak on it can
put text one keypress away from execution in the user's shell. Defenses,
in depth:

1. **Filesystem**: parent directory `0700`, socket `0600`, both set before
   the first `accept`. The `/tmp` fallback path embeds the uid so two users
   can never collide on a predictable path.
2. **Peer credentials**: every connection is checked with kernel-reported
   peer credentials (`LOCAL_PEERCRED`/`xucred` on macOS, `SO_PEERCRED`/`ucred`
   on Linux) before a single byte is read. Only the daemon's own euid is
   accepted. Root is deliberately rejected too: root has legitimate ways to
   act as the user, so a root connection here is at best confused tooling
   and at worst a container uid-mapping surprise. On platforms with no
   credential API the daemon refuses all connections rather than guessing.
3. **Never auto-execute**: this is a protocol-level invariant, not a
   plugin courtesy. There is no verb that requests execution, and every
   plugin applies `REPLACE` purely by assigning its editor's buffer
   variable (`BUFFER`/`READLINE_LINE`/`commandline -r`), never by invoking
   accept-line or sending `\r`. The human always presses Enter. Any future
   "run it" voice command must be implemented outside this protocol with
   its own opt-in gate (see `ux/04-terminal-and-headless.md`).
4. **Stale-intent containment**: intents expire after 30 seconds and are
   consumed by the first `EDIT`, matched or not. A voice command can never
   lie in wait for a later, unrelated command line.
5. **Input hygiene**: base64/UTF-8 validation on every payload, 1 MiB line
   cap, 2 s read timeouts so a wedged client cannot hold the daemon, and a
   bind that refuses when another live daemon owns the socket.

Residual risk, stated honestly: any process running *as the user* can
connect (same-uid peers are exactly the trust boundary of a user account;
such a process could equally write to the user's rc files). And the daemon
rewrites text that the user must still review before pressing Enter, so a
malicious *staged intent* from a compromised same-uid process could place
`; rm -rf ~` into a line. The mitigation is the invariant above: nothing is
executed without the human pressing Enter on a visible line.

## Per-shell notes and traps

### zsh (`shell/outloud.zsh`)

- ZLE widget over `$BUFFER`/`$CURSOR`, both of which cover the *whole*
  multi-line buffer, so heredocs and `for` loops edit correctly.
- `$CURSOR` counts characters when `MULTIBYTE` is set (the default);
  matches the protocol's zsh unit.
- `zle split-undo` before assigning `BUFFER` makes the voice edit its own
  undo unit: one `^Xu` reverts exactly the rewrite, verified live.
- `emulate -L zsh` insulates the widget from user options (`sh_word_split`
  would corrupt the reply parsing otherwise).
- Trap: `zle -M` output is transient; do not `echo` from a widget or the
  prompt corrupts.
- Default binding `^X^A`, chosen because it is unbound in stock emacs and
  vi keymaps. Override with `OUTLOUD_BRIDGE_KEY` before sourcing.

### bash (`shell/outloud.bash`)

- `bind -x` runs the function with `READLINE_LINE`/`READLINE_POINT` live;
  assignment writes back through readline, so `C-_` undo works.
- `READLINE_POINT` is a **byte** offset (readline's `rl_point`), hence the
  `bash` cursor unit in the protocol. The daemon returns `cursor_bytes`
  and the plugin uses that field.
- Trap: `bind -x` fails in non-interactive shells; the binding is guarded
  with `[[ $- == *i* ]]` so sourcing bashrc from scripts stays silent.
- Trap: bash 3.2 (macOS system bash) lacks usable `bind -x` semantics;
  require bash 4+. macOS users on zsh (the default since Catalina) are
  unaffected.
- Multi-line: readline buffers are logically single-line except within
  quoted continuations, which arrive in `READLINE_LINE` with embedded
  newlines and survive via base64.

### fish (`shell/outloud.fish`)

- `commandline -b` reads the full buffer, `commandline -r` replaces it,
  `commandline -C` gets/sets the cursor in characters, `commandline -f
  repaint` redraws. All state stays inside fish, so fish undo works.
- Installed as a symlink into `~/.config/fish/conf.d/`, which fish sources
  automatically: no rc editing at all.
- Trap: fish list-of-lines semantics. `commandline` output is a list, one
  element per line; `string collect` rejoins it or a multi-line buffer
  silently loses its newlines.
- Not yet live-verified here (no fish on this machine); syntax follows the
  documented builtins and the shared protocol is exercised by the other
  two shells and the Rust tests.

## Installation UX

```console
$ cargo run -p shell-bridge -- install        # detects $SHELL
installed into /Users/you/.zshrc
restart your shell or: source /Users/you/.zshrc
```

What it does, per shell:

- **zsh/bash**: appends one guarded, marker-tagged line to the rc:
  `# outloud shell-bridge` + `[ -f .../outloud.zsh ] && source .../outloud.zsh`.
  Idempotent (the marker is checked). It appends rather than prepending or
  managing the file because frameworks (oh-my-zsh, prezto, bash-it,
  zinit) all funnel through the same rc; appending after them means our
  `bindkey` runs last and survives their keymap resets. If a framework
  user prefers plugin-manager form, the file *is* a valid oh-my-zsh
  plugin: `ln -s .../shell ~/.oh-my-zsh/custom/plugins/outloud` (zsh loads
  `outloud.zsh` via the plugin's file glob) and add `outloud` to `plugins=(...)`.
- **fish**: symlinks `outloud.fish` into `conf.d/`, the native drop-in dir.
- `--shell`, `--rc`, and `--plugin-dir` override detection for scripted or
  remote installs. `ZDOTDIR` is honored, which is also how the test suite
  installs into a throwaway config.

## Demo

```console
$ shell/demo.sh
```

Builds the bridge, starts it on a private socket, pre-stages
`change prod-web to staging-web`, and drops you into an interactive zsh
whose config lives entirely in a temp ZDOTDIR (your dotfiles untouched).
Type `kubectl get pods --namespace prod-web`, press `^X^A`, watch the line
rewrite in place. `^Xu` undoes it. From another terminal you can stage
further edits: `shell-bridge intent "delete --output wide" --socket <path>`.

Automated equivalent (what CI would run): `shell/verify-zsh.exp` drives a
real interactive zsh through a pty, asserts the rewrite appeared, and
confirms undo restored the original by making the plugin re-offer its
buffer and asking the daemon what it saw.

## Adding a new shell

A client is ~20 lines. It must:

1. Read its editor's full buffer and cursor when the user triggers it.
2. Send `EDIT v1 other <cursor-chars> <base64 buffer>\n` to the socket
   (`nc -U`, or the language's socket API) and read one reply line.
   Register a new shell name in `protocol.rs::Shell` if the cursor unit
   is bytes rather than characters.
3. On `REPLACE`, assign the decoded buffer and cursor **through the
   editor's own state**, never by retyping or pasting, and never submit
   the line. On `NOOP`/`ERR`, show the decoded reason.

PowerShell (`PSConsoleReadLine::GetBufferState`/`Replace`) and any
readline-embedding REPL (`psql`, `python`) fit the same shape; see the
compat matrix rows for the entry points.
