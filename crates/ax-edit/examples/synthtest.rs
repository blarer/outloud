//! Manual verification harness for `ax_edit::synth::type_text`.
//!
//! Run it, then focus a terminal running `cat > /tmp/term_sink.txt` (or any
//! text field) within the countdown. It types a known string; compare what
//! arrived against what was sent. This exists because the failure mode being
//! guarded against is *dropped characters*, which only reproduces against a
//! real tty and cannot be asserted from a unit test.
//!
//! cargo run -p ax-edit --example synthtest -- "text to type"

// `ax_edit::synth` is macOS-only, so the body cannot even parse a reference
// to it elsewhere. An example is still built by `cargo clippy --all-targets`
// on every platform, which is how this failed the Linux job while compiling
// fine on a Mac. A required-features gate in Cargo.toml would not help:
// the split here is by target_os, not by feature.
#[cfg(target_os = "macos")]
fn main() {
    let text = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "the quick brown fox jumps over the lazy dog 0123456789".into());
    let delay_ms: u64 = std::env::var("SYNTH_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2500);

    eprintln!("focus the destination now; typing in {delay_ms}ms");
    std::thread::sleep(std::time::Duration::from_millis(delay_ms));

    let started = std::time::Instant::now();
    match ax_edit::synth::type_text(&text) {
        Ok(()) => eprintln!(
            "typed {} chars in {:?}: {text:?}",
            text.chars().count(),
            started.elapsed()
        ),
        Err(e) => eprintln!("failed: {e}"),
    }
}

/// Off macOS there is no synthetic-keys backend to exercise.
///
/// Says so rather than silently doing nothing, because someone running this
/// on Linux deserves to know the harness is a no-op there rather than that
/// their keystrokes vanished.
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("synthtest is macOS-only: ax_edit::synth has no backend on this platform");
}
