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
