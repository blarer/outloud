//! Manual verification harness for `ax_edit::synth::type_text`.
//!
//! Run it, then focus a terminal running `cat > /tmp/term_sink.txt` (or any
//! text field) within the countdown. It types a known string; compare what
//! arrived against what was sent. This exists because the failure mode being
//! guarded against is *dropped characters*, which only reproduces against a
//! real tty and cannot be asserted from a unit test.
//!
//! cargo run -p ax-edit --example synthtest -- "text to type"

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
