# outloud shell-bridge: bash readline binding (T2 edit-by-voice).
#
# Sourced from .bashrc. Uses `bind -x`, which runs a shell function with
# READLINE_LINE / READLINE_POINT bound to the live buffer; assigning them
# writes back through readline itself, so readline undo (C-_) still works.
#
# Requires only: bash 4+, base64, nc. Never executes the rewritten line:
# the binding only assigns READLINE_LINE, it never invokes accept-line.

# Socket path must match shell_bridge::server::default_socket_path.
# AQUA_BRIDGE_SOCKET is the product's previous name for this variable and is
# still honored so an existing install's environment keeps working; it will
# be dropped once no pre-rename installs remain.
: "${OUTLOUD_BRIDGE_SOCKET:=${AQUA_BRIDGE_SOCKET:-${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp/outloud-$UID}}/outloud/shell.sock}}"

_outloud_edit() {
  [ -S "$OUTLOUD_BRIDGE_SOCKET" ] || {
    printf '\noutloud: bridge not running (no socket at %s)\n' "$OUTLOUD_BRIDGE_SOCKET" >&2
    return 1
  }

  # base64 keeps multi-line buffers inside one protocol line; tr strips the
  # wrapping GNU base64 inserts every 76 chars.
  local b64 reply
  b64=$(printf %s "$READLINE_LINE" | base64 | tr -d '\n')

  # READLINE_POINT is readline's rl_point: a BYTE offset. The protocol's
  # bash unit is bytes for exactly this reason; the daemon normalizes.
  reply=$(printf 'EDIT v1 bash %d %s\n' "$READLINE_POINT" "$b64" \
          | nc -U -w 2 "$OUTLOUD_BRIDGE_SOCKET" 2>/dev/null) || {
    printf '\noutloud: bridge did not answer\n' >&2
    return 1
  }

  local verb cursor_bytes cursor_chars payload
  read -r verb cursor_bytes cursor_chars payload <<EOF
$reply
EOF
  case $verb in
    REPLACE)
      READLINE_LINE=$(printf %s "$payload" | base64 -d) || return 1
      # Bytes, matching rl_point. The daemon already clamped it.
      READLINE_POINT=$cursor_bytes
      ;;
    NOOP)
      # cursor_bytes holds the b64 reason for single-payload verbs.
      printf '\noutloud: %s\n' "$(printf %s "$cursor_bytes" | base64 -d 2>/dev/null)" >&2
      ;;
    ERR)
      printf '\noutloud error: %s\n' "$(printf %s "$cursor_bytes" | base64 -d 2>/dev/null)" >&2
      return 1
      ;;
    *)
      printf '\noutloud: unexpected reply %s\n' "$verb" >&2
      return 1
      ;;
  esac
}

# Same default chord as the zsh widget; unbound in stock readline.
# `bind -x` only works in interactive shells, so guard for scripts that
# source bashrc non-interactively. AQUA_BRIDGE_KEY is honored for installs
# that predate the rename.
if [[ $- == *i* ]]; then
  bind -x "\"${OUTLOUD_BRIDGE_KEY:-${AQUA_BRIDGE_KEY:-\C-x\C-a}}\": _outloud_edit"
fi
