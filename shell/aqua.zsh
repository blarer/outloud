# aqua shell-bridge: zsh ZLE widget (T2 edit-by-voice).
#
# Sourced from .zshrc. Defines a widget that offers the current ZLE buffer
# to the bridge daemon and applies the returned rewrite through ZLE itself,
# which is what keeps zsh's own undo (^Xu / ^X^U) working.
#
# Requires only: zsh, base64, nc (all present on stock macOS and Linux).
# Never executes the rewritten line: the widget only assigns BUFFER/CURSOR.

# Socket path must match shell_bridge::server::default_socket_path.
# XDG_RUNTIME_DIR on Linux, TMPDIR on macOS, uid-scoped /tmp as last resort.
: ${AQUA_BRIDGE_SOCKET:="${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp/aqua-$UID}}/aqua/shell.sock"}

aqua-edit() {
  emulate -L zsh
  setopt no_unset pipe_fail

  if [[ ! -S $AQUA_BRIDGE_SOCKET ]]; then
    zle -M "aqua: bridge not running (no socket at $AQUA_BRIDGE_SOCKET)"
    return 1
  fi

  # base64 the buffer so newlines (multi-line commands, heredocs) survive
  # the line-framed protocol. tr strips the wrapping GNU base64 adds.
  local b64 reply
  b64=$(printf %s "$BUFFER" | base64 | tr -d '\n')

  # zsh's $CURSOR counts characters (with the default multibyte option),
  # which is exactly the protocol's zsh unit. One request, one reply,
  # connection closed; -w 2 keeps a wedged daemon from freezing the shell.
  reply=$(printf 'EDIT v1 zsh %d %s\n' "$CURSOR" "$b64" \
          | command nc -U -w 2 "$AQUA_BRIDGE_SOCKET" 2>/dev/null) || {
    zle -M "aqua: bridge did not answer"
    return 1
  }

  local verb rest
  verb=${reply%% *}
  rest=${reply#* }
  case $verb in
    REPLACE)
      local -a fields
      fields=(${(s: :)rest})
      # fields: 1=cursor_bytes 2=cursor_chars 3=b64 buffer. We take the
      # character cursor; the byte one is for readline clients.
      local new_buffer
      new_buffer=$(printf %s "${fields[3]}" | base64 -d) || {
        zle -M "aqua: bad payload from bridge"
        return 1
      }
      # Make the voice edit its own undo unit, so one ^Xu reverts exactly
      # this rewrite and nothing the user typed before it.
      (( $+widgets[split-undo] )) && zle split-undo
      BUFFER=$new_buffer
      CURSOR=${fields[2]}
      zle redisplay
      ;;
    NOOP)
      zle -M "aqua: $(printf %s "$rest" | base64 -d 2>/dev/null)"
      ;;
    ERR)
      zle -M "aqua error: $(printf %s "$rest" | base64 -d 2>/dev/null)"
      return 1
      ;;
    *)
      zle -M "aqua: unexpected reply '$verb'"
      return 1
      ;;
  esac
}

zle -N aqua-edit
# ^X^A by default: unbound in stock emacs and vi keymaps, so we never
# shadow a binding the user already relies on. Override AQUA_BRIDGE_KEY
# before sourcing to move it.
bindkey "${AQUA_BRIDGE_KEY:-^X^A}" aqua-edit
