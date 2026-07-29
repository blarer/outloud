//! Type text at a given inter-character interval, then exit.
//!
//! A one-shot primitive for measuring the pacing floor against
//! destinations that Accessibility cannot read back, notably a terminal's
//! tty line buffer. `scripts/pacing-floor-terminal.sh` drives it and
//! verifies the result against a file, which is ground truth in a way that
//! an AX read-back is not.
//!
//!     type_at <interval_us> <text>
//!
//! Types into the FOCUSED window. Intended for scripted use.

// `ax_edit::synth` only exists behind `#[cfg(target_os = "macos")]` in
// lib.rs, but `cargo clippy --all-targets` compiles every example on every
// platform, so an ungated body fails the Linux job while building fine on a
// Mac. The gate lives here rather than in Cargo.toml because
// `required-features` expresses a *feature* constraint, not a target one:
// there is no feature to require, the split is by OS. A stub main keeps the
// example honest off-macOS instead of silently doing nothing.
#[cfg(target_os = "macos")]
fn main() {
    let mut args = std::env::args().skip(1);
    let us: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: type_at <interval_us> <text>");
    let text: String = args.collect::<Vec<_>>().join(" ");
    assert!(!text.is_empty(), "usage: type_at <interval_us> <text>");

    if let Err(e) = ax_edit::synth::type_text_paced(&text, std::time::Duration::from_micros(us)) {
        eprintln!("type_at: {e}");
        std::process::exit(1);
    }
}

/// Off macOS there is no synthetic-keys backend, so say so loudly rather
/// than pretend the text was typed.
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("type_at is macOS-only: ax_edit::synth has no backend on this platform");
    std::process::exit(1);
}
