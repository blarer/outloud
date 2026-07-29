//! Does an `AXValue` write really destroy the app's undo, while an
//! `AXSelectedText` write really preserves it?
//!
//! `ax-edit`'s own doc comments assert both ("Preserves undo in most
//! apps" / "usually clobbers undo"), and `inject.rs` now routes a
//! selection write away from the `AXValue` path on the strength of that
//! claim. A comment is not evidence, and the cost of the claim being
//! wrong is the user's paragraph, so this measures it against a live
//! TextEdit window.
//!
//! It is a manual example, not a test: it needs a focused TextEdit
//! document and the Accessibility grant, neither of which CI has.
//!
//! ```text
//! cargo run -p ax-edit --example undo_semantics
//! ```
//!
//! TextEdit only, and it creates its own document rather than writing
//! into whatever happens to be focused.

fn main() {
    #[cfg(not(target_os = "macos"))]
    eprintln!("macOS only");

    #[cfg(target_os = "macos")]
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::process::Command;
    use std::thread::sleep;
    use std::time::Duration;

    const ORIGINAL: &str = "The customers might possibly be quite upset about this.";

    fn osa(script: &str) -> String {
        let out = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .expect("osascript");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn setup() {
        osa(&format!(
            r#"tell application "TextEdit"
                 activate
                 if (count of documents) = 0 then make new document
                 set text of front document to "{ORIGINAL}"
               end tell"#
        ));
        sleep(Duration::from_millis(400));
    }

    fn read_doc() -> String {
        osa(r#"tell application "TextEdit" to get text of front document"#)
    }

    fn select_all() {
        osa(r#"tell application "TextEdit" to activate"#);
        sleep(Duration::from_millis(300));
        osa(r#"tell application "System Events" to keystroke "a" using command down"#);
        sleep(Duration::from_millis(300));
    }

    fn undo() {
        osa(r#"tell application "TextEdit" to activate"#);
        sleep(Duration::from_millis(300));
        osa(r#"tell application "System Events" to keystroke "z" using command down"#);
        sleep(Duration::from_millis(500));
    }

    pub fn run() {
        if !ax_edit::is_trusted(false) {
            eprintln!("not trusted for accessibility; grant it and re-run");
            return;
        }

        // --- Case 1: AXSelectedText, the path a selection write takes.
        setup();
        select_all();
        let snap = ax_edit::snapshot_focused().expect("snapshot");
        println!(
            "focused: app={:?} role={} selected_text_settable={} value_settable={}",
            snap.app, snap.role, snap.selected_text_settable, snap.value_settable
        );
        let strategy = ax_edit::replace_focused("REPLACED VIA SELECTION").expect("write");
        sleep(Duration::from_millis(300));
        println!("case 1: strategy={strategy}, doc now {:?}", read_doc());
        undo();
        let after_undo = read_doc();
        println!("case 1: after Cmd+Z {after_undo:?}");
        println!(
            "case 1 verdict: undo {} the original\n",
            if after_undo.contains("customers might possibly") {
                "RESTORED"
            } else {
                "DID NOT restore"
            }
        );

        // --- Case 2: AXValue, the whole-field write. No selection, so
        // `replace_focused` falls to the SetValue branch.
        setup();
        // Click into the document without selecting, so AXSelectedText is
        // empty and the SetValue branch is the one taken.
        osa(r#"tell application "TextEdit" to activate"#);
        sleep(Duration::from_millis(300));
        osa(r#"tell application "System Events" to key code 124"#); // right arrow
        sleep(Duration::from_millis(300));
        let strategy = ax_edit::replace_focused("REPLACED VIA VALUE").expect("write");
        sleep(Duration::from_millis(300));
        println!("case 2: strategy={strategy}, doc now {:?}", read_doc());
        undo();
        let after_undo = read_doc();
        println!("case 2: after Cmd+Z {after_undo:?}");
        println!(
            "case 2 verdict: undo {} the original",
            if after_undo.contains("customers might possibly") {
                "RESTORED"
            } else {
                "DID NOT restore"
            }
        );

        // --- Case 3: SCOPE, which is the property that actually
        // justifies routing a selection write away from the AXValue
        // branch. The replacement text is SELECTION-SIZED, so the two
        // strategies do not differ by a little: SetSelectedText replaces
        // the selected word, SetValue replaces the ENTIRE DOCUMENT with
        // that one word.
        //
        // Case 2 already showed the shape of it (a whole document became
        // "REPLACED VIA VALUE"); this pins it against a partial selection,
        // which is the configuration `inject.rs` actually faces.
        setup();
        select_word_at_start();
        let snap = ax_edit::snapshot_focused().expect("snapshot");
        println!("case 3: selection = {:?}", snap.selected_text);
        let strategy = ax_edit::replace_focused("WORD").expect("write");
        sleep(Duration::from_millis(300));
        let doc = read_doc();
        println!("case 3: strategy={strategy}, doc now {doc:?}");
        // A selection-scoped write turns the document into the original
        // with exactly the selected substring swapped out. Computed from
        // the snapshot rather than hardcoded, and direction-agnostic:
        // Cmd+A then Shift+Left anchors at either end depending on how
        // the keystrokes land, so assuming a leading selection produced a
        // false "DESTROYED" verdict on a run that was in fact correct.
        let sel = snap.selected_text.as_deref().unwrap_or("");
        let expected_if_scoped = ORIGINAL.replace(sel, "WORD");
        println!(
            "case 3 verdict: selection {sel:?} -> the unselected remainder {}",
            if !sel.is_empty() && doc == expected_if_scoped {
                "SURVIVED (the write was scoped to the selection)"
            } else if sel.is_empty() {
                "N/A: the selection did not land, so this run repeated case 2"
            } else {
                "WAS DESTROYED (a selection-sized payload replaced the whole field)"
            }
        );
        undo();
        println!("case 3: after Cmd+Z {:?}", read_doc());
    }

    /// Select just the leading words, so a write's SCOPE is observable:
    /// the unselected remainder either survives or does not.
    ///
    /// Built by SHRINKING a Cmd+A selection rather than growing one from
    /// the caret. Both alternatives were tried against a live TextEdit
    /// and neither landed from a scripted run: Home + Shift+Option+Right
    /// and TextEdit's own `set selection to characters 1 thru 13` both
    /// left `AXSelectedText` empty. Cmd+A does land (case 1 proves it),
    /// so the selection is made there and trimmed back with Shift+Left.
    fn select_word_at_start() {
        osa(r#"tell application "TextEdit" to activate"#);
        sleep(Duration::from_millis(400));
        osa(r#"tell application "System Events" to keystroke "a" using command down"#);
        sleep(Duration::from_millis(400));
        // Trim the tail off the full selection, leaving a genuine partial
        // one whose unselected remainder is observable after a write.
        for _ in 0..30 {
            osa(r#"tell application "System Events" to key code 123 using {shift down}"#);
        }
        sleep(Duration::from_millis(500));
    }
}
