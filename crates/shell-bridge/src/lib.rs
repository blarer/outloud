//! T2 shell integration: real edit-by-voice on the shell command line.
//!
//! A terminal exposes no writable accessibility field (M0 measured this),
//! so the only in-place, undo-preserving way to edit the current command
//! line is to cooperate with the shell's own line editor. This crate is the
//! daemon half of that cooperation; the plugins under `shell/` are the
//! shell half. `docs/shell-integration.md` is the spec.

// The protocol (framing, base64 payloads, cursor mapping) is pure and
// compiles everywhere, so its tests run on every platform.
pub mod protocol;

// The transport is a unix-domain socket and the peers are POSIX shells
// (bash/zsh/fish line editors), neither of which exists on Windows. The
// Windows equivalent would be a named pipe plus a PSReadLine module, which
// is a different design, not a port; until someone builds it, gating the
// modules keeps the workspace compiling for Windows targets without
// pretending the capability exists.
#[cfg(unix)]
pub mod peer;
#[cfg(unix)]
pub mod server;
