//! T2 shell integration: real edit-by-voice on the shell command line.
//!
//! A terminal exposes no writable accessibility field (M0 measured this),
//! so the only in-place, undo-preserving way to edit the current command
//! line is to cooperate with the shell's own line editor. This crate is the
//! daemon half of that cooperation; the plugins under `shell/` are the
//! shell half. `docs/shell-integration.md` is the spec.

pub mod peer;
pub mod protocol;
pub mod server;
