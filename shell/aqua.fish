# aqua shell-bridge: fish binding (T2 edit-by-voice).
#
# Installed as a symlink in ~/.config/fish/conf.d/, which fish sources
# automatically. Uses the `commandline` builtin for both read and write, so
# fish remains the owner of its buffer and its undo (ctrl-z) keeps working.
#
# Requires only: fish 3+, base64, nc. Never executes the rewritten line:
# `commandline -r` replaces text, it never submits it.

# Socket path must match shell_bridge::server::default_socket_path.
if not set -q AQUA_BRIDGE_SOCKET
    if set -q XDG_RUNTIME_DIR
        set -g AQUA_BRIDGE_SOCKET $XDG_RUNTIME_DIR/aqua/shell.sock
    else if set -q TMPDIR
        set -g AQUA_BRIDGE_SOCKET $TMPDIR/aqua/shell.sock
    else
        set -g AQUA_BRIDGE_SOCKET /tmp/aqua-(id -u)/aqua/shell.sock
    end
end

function _aqua_edit
    if not test -S $AQUA_BRIDGE_SOCKET
        commandline -f repaint
        echo "aqua: bridge not running (no socket at $AQUA_BRIDGE_SOCKET)" >&2
        return 1
    end

    # -b: the whole buffer including continuation lines. base64 keeps its
    # newlines inside one protocol line; tr strips GNU base64's wrapping.
    set -l buf (commandline -b | string collect)
    set -l b64 (printf %s $buf | base64 | tr -d '\n')
    # fish's cursor offset counts characters, the protocol's fish unit.
    set -l cur (commandline -C)

    set -l reply (printf 'EDIT v1 fish %d %s\n' $cur $b64 | nc -U -w 2 $AQUA_BRIDGE_SOCKET 2>/dev/null)
    if test -z "$reply"
        echo "aqua: bridge did not answer" >&2
        return 1
    end

    set -l parts (string split ' ' -- $reply)
    switch $parts[1]
        case REPLACE
            # parts: REPLACE cursor_bytes cursor_chars b64buffer
            set -l new_buf (printf %s $parts[4] | base64 -d | string collect)
            commandline -r -- $new_buf
            commandline -C $parts[3]
            commandline -f repaint
        case NOOP
            echo "aqua:" (printf %s $parts[2] | base64 -d) >&2
        case ERR
            echo "aqua error:" (printf %s $parts[2] | base64 -d) >&2
            return 1
        case '*'
            echo "aqua: unexpected reply $parts[1]" >&2
            return 1
    end
end

# Same default chord as bash/zsh: ctrl-x ctrl-a, unbound in stock fish.
if status is-interactive
    if set -q AQUA_BRIDGE_KEY
        bind $AQUA_BRIDGE_KEY _aqua_edit
    else
        bind \cx\ca _aqua_edit
    end
end
