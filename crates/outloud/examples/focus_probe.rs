//! Prove `focus_moved_to` reads live focus, not just its pure comparison.
//!
//! WHY this exists as an example rather than a test: the unit tests cover
//! `focus_changed`, which is pure and provable anywhere. They say nothing
//! about whether the wrapper actually asks the accessibility layer what has
//! focus right now, and that lookup is the half that can silently break.
//!
//! Three attempts to observe the warning during a real dictation all lost
//! the race: the AppleScript that raises a window takes longer than the
//! ~200ms between key-up and the write, so focus never moved inside the
//! window that matters. That is a testing limitation, not evidence the
//! feature works, and shipping on it would have been a guess.
//!
//! This removes the race by lying about the target instead of moving the
//! window: claim an app that cannot possibly hold focus, and a working
//! implementation must report whatever genuinely does.

fn main() {
    // Nothing can be focused under this name, so any correct implementation
    // that reads live focus must report a difference.
    let impossible = Some("NoSuchAppCouldEverBeFocused");

    match outloud::inject::focus_moved_to(impossible) {
        Some(now) => {
            println!("PASS: read live focus, reported {now:?}");

            // And the converse: told the truth about the target, it must
            // stay silent. A warning that fires when nothing moved would be
            // worse than none, since it would train the user to ignore it.
            match outloud::inject::focus_moved_to(Some(&now)) {
                None => println!("PASS: silent when focus did not move"),
                Some(other) => {
                    println!("FAIL: claimed a move to {other:?} when nothing moved")
                }
            }
        }
        None => println!(
            "FAIL: returned None for an impossible target, so it is not \
             reading live focus (or nothing is focused right now)"
        ),
    }
}
