//! What the per-character pacing floor actually is, per destination.
//!
//! `KEY_INTERVAL` is 700us for every destination that takes the paced
//! typing path (`crates/ax-edit/src/synth.rs`). That number was chosen
//! because Terminal.app drops characters when events arrive faster than it
//! consumes them, and 700us stopped the dropping. It was never established
//! as the *minimum* that works, and it is paid per character: 0.7ms x 100
//! characters is 70ms of user-visible latency on every utterance that lands
//! on this path.
//!
//! This sweeps the interval downward against whatever is focused and
//! **verifies each attempt by reading the field back**, so the answer does
//! not depend on someone eyeballing a window afterwards.
//!
//!     cargo run --release -p ax-edit --example pacing_floor
//!
//! Focus a scratch text field first; there is a countdown. TextEdit is a
//! good default. Run it again focused on a terminal to get that class's
//! floor, which is the one 700us was actually chosen for.

use std::time::{Duration, Instant};

use ax_edit::{snapshot_focused, synth};

/// Typed at each candidate. Mixed classes because drops are not uniform:
/// digits and punctuation take different key codes than letters.
const PROBE: &str = "the quick brown fox jumps 0123456789 over the lazy dog";

/// Fastest last, so the run degrades in a visible direction.
const CANDIDATES_US: [u64; 8] = [700, 500, 400, 300, 200, 150, 100, 50];

/// How many times each candidate must succeed. A single clean pass proves
/// little: dropping is a race, and a race that fails one time in five is
/// still a bug that will find the user.
const TRIALS: usize = 5;

fn main() {
    println!("Focus a scratch text field (TextEdit is fine). Starting in 3s.");
    std::thread::sleep(Duration::from_secs(3));

    if snapshot_focused().is_err() {
        eprintln!("Cannot read the focused field. Grant Accessibility, or focus a text field.");
        std::process::exit(1);
    }

    println!("\n interval  trials  intact  ms/char  verdict");
    println!(" --------  ------  ------  -------  -------");

    let mut floor: Option<u64> = None;
    for us in CANDIDATES_US {
        let interval = Duration::from_micros(us);
        let mut intact = 0usize;
        let mut total_ms = 0.0;

        for _ in 0..TRIALS {
            let Ok(before) = snapshot_focused() else {
                continue;
            };
            let baseline = before.value.unwrap_or_default().chars().count();

            let started = Instant::now();
            if synth::type_text_paced(PROBE, interval).is_err() {
                continue;
            }
            total_ms += started.elapsed().as_secs_f64() * 1000.0;

            // Let the destination finish consuming before reading back:
            // otherwise a slow app looks like a dropped character.
            std::thread::sleep(Duration::from_millis(250));

            if let Ok(after) = snapshot_focused() {
                let got: String = after
                    .value
                    .unwrap_or_default()
                    .chars()
                    .skip(baseline)
                    .collect();
                if got.trim_end() == PROBE {
                    intact += 1;
                }
            }
            // Clear via AX rather than synthetic keys: the point is to
            // measure typing, so the cleanup must not itself type.
            let _ = ax_edit::replace_focused("");
            std::thread::sleep(Duration::from_millis(150));
        }

        let per_char = total_ms / TRIALS as f64 / PROBE.chars().count() as f64;
        let verdict = if intact == TRIALS { "clean" } else { "DROPS" };
        println!(" {us:>6}us  {TRIALS:>6}  {intact:>6}  {per_char:>7.3}  {verdict}");
        if intact == TRIALS {
            floor = Some(us);
        }
    }

    match floor {
        Some(us) => {
            let saved = (700 - us) as f64 * PROBE.chars().count() as f64 / 1000.0;
            println!(
                "\nFastest interval clean across {TRIALS} trials: {us}us.\n\
                 Shipped value is 700us. On a {}-character utterance that is\n\
                 {saved:.1}ms of latency currently being spent for nothing.",
                PROBE.chars().count()
            );
            if us == 700 {
                println!("(700us is already the floor here. Leave it alone.)");
            }
        }
        None => println!("\nEven 700us dropped characters here. Do NOT lower it for this app."),
    }
}
