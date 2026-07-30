//! What the snapshot reports about the focused application.
//!
//! Profiles match on bundle identifiers, so the snapshot has to carry one.
//! This prints what it actually resolves for whatever is focused, because
//! "the field exists" and "the field holds the right value" are different
//! claims and only the second one makes profiles work.
//!
//!     cargo run --release -p ax-edit --example whoami
//!
//! Focus the app you want to write a profile for, then run it.

fn main() {
    // A moment to switch to the app being identified, since running this
    // from a terminal would otherwise just report the terminal.
    let delay = std::time::Duration::from_secs(3);
    eprintln!(
        "Focus the app to identify. Reading in {}s...",
        delay.as_secs()
    );
    std::thread::sleep(delay);

    match ax_edit::snapshot_focused() {
        Ok(snap) => {
            println!("app title : {:?}", snap.app);
            println!("bundle id : {:?}", snap.bundle_id);
            println!("role      : {}", snap.role);
            match snap.bundle_id {
                Some(id) => {
                    println!("\nProfile stanza for this app:\n");
                    println!("[profile.my-rule]");
                    println!("match.bundle-id = \"{id}\"");
                }
                None => println!(
                    "\nNo bundle id: this process has no bundle. Match it with\n\
                     `match.process-name` instead."
                ),
            }
        }
        Err(e) => {
            eprintln!("could not read the focused element: {e}");
            eprintln!("Grant Accessibility, and focus a text field.");
            std::process::exit(1);
        }
    }
}
