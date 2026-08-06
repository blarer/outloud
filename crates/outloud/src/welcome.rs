//! First-run permission walkthrough.
//!
//! WHY this exists: OutLoud needs two separate TCC grants, and without both
//! it starts cleanly, shows its menu bar icon, and does nothing at all when
//! the hotkey is held. Every failure mode is silent, and the two permissions
//! live in *different* System Settings panes with names that do not obviously
//! correspond to "typing" or "hearing".
//!
//! The menu already names the missing grant and deep-links to its pane, but a
//! menu is pull, not push: it helps someone who already suspects a permission
//! problem. Someone installing for the first time has no such suspicion. They
//! hold the key, nothing happens, and the app looks broken.
//!
//! So the walkthrough is push, and it is driven by observation rather than
//! instruction. Each step opens the exact pane, waits, and then *re-checks*
//! the permission rather than asking "did that work?" A user who believes
//! they granted something and did not is the case that produces "I did what
//! it said and it still doesn't work", and asking them cannot detect it.
//!
//! The pure decision logic is separated from the dialogs so the sequencing is
//! testable without a display, a TCC database, or a human to click.

/// What the walkthrough should do next, given what is currently granted.
///
/// Deliberately an enum over "which dialog to show" rather than a bool per
/// permission: the ORDER matters (Input Monitoring first, because a dead
/// hotkey hides every other success) and an enum makes the order a property
/// of one function that a test can drive through every state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing is missing. Show the "you're ready" message, or nothing at all
    /// if this user has already been welcomed.
    Ready { first_run: bool },
    /// The hotkey cannot fire. Always addressed first.
    NeedInputMonitoring,
    /// The words cannot be typed into other apps.
    NeedAccessibility,
}

/// The permissions the walkthrough cares about, as observed from the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grants {
    pub input_monitoring: bool,
    pub accessibility: bool,
}

/// Decide the next step.
///
/// Input Monitoring is checked before Accessibility for a reason that is not
/// arbitrary: with Accessibility alone the hotkey never fires, so the user
/// sees *nothing*, and concludes the app does not work. With Input Monitoring
/// alone the overlay appears and the cat reacts to their voice, so they can
/// see the app is alive while the second grant is still missing. Ordering the
/// silent failure first means the app is never in a state where it looks dead.
pub fn next_step(grants: Grants, already_welcomed: bool) -> Step {
    if !grants.input_monitoring {
        return Step::NeedInputMonitoring;
    }
    if !grants.accessibility {
        return Step::NeedAccessibility;
    }
    Step::Ready {
        first_run: !already_welcomed,
    }
}

/// Whether the walkthrough should run at all on this launch.
///
/// Not gated on first run alone. A grant can be revoked, and an ad-hoc
/// rebuild voids both of them, so a user who has been welcomed can still
/// arrive at a dead hotkey; that is precisely when they most need the pane
/// opened for them. Gated instead on "something is missing, OR this is the
/// first launch", which covers the new user and the broken-again user with
/// one rule.
pub fn should_run(grants: Grants, already_welcomed: bool) -> bool {
    !already_welcomed || !grants.input_monitoring || !grants.accessibility
}

/// The pane anchor and the words for a step.
///
/// Kept beside the decision logic rather than in the dialog code so the
/// message a user sees is covered by the same tests as the sequencing. The
/// wording targets someone who has never heard of TCC: no "grant", no
/// "permission dialog", no pane names used as if they were self-explanatory.
pub struct Prompt {
    pub pane: &'static str,
    pub title: &'static str,
    pub body: &'static str,
    pub button: &'static str,
}

pub fn prompt_for(step: Step) -> Option<Prompt> {
    match step {
        Step::NeedInputMonitoring => Some(Prompt {
            pane: "Privacy_ListenEvent",
            title: "One switch to turn on",
            // Naming the switch by its exact on-screen label, and saying where
            // the window will appear, because the settings window opens BEHIND
            // nothing but is easy to lose on a big screen.
            body: "OutLoud needs permission to notice when you hold the \
                   dictation key.\n\n\
                   I'll open Settings for you. Find OutLoud in the list and \
                   turn its switch ON.\n\n\
                   Then come back here.",
            button: "Open Settings",
        }),
        Step::NeedAccessibility => Some(Prompt {
            pane: "Privacy_Accessibility",
            title: "One more switch",
            body: "Now OutLoud needs permission to type the words for you.\n\n\
                   Same thing: find OutLoud in the list and turn its switch \
                   ON.\n\n\
                   Then come back here.",
            button: "Open Settings",
        }),
        Step::Ready { .. } => None,
    }
}

/// Where the "this user has been welcomed" marker lives.
///
/// Beside the config rather than in it: the config file is documented as the
/// user's to edit, and a bookkeeping flag appearing in it invites the question
/// "what is this and can I delete it?" Deleting this file simply replays the
/// welcome, which is a safe and even useful thing to do.
#[cfg(target_os = "macos")]
fn marker_path() -> Option<std::path::PathBuf> {
    config::user_config_path().map(|p| {
        p.parent()
            .unwrap_or(std::path::Path::new("."))
            .join(".welcomed")
    })
}

/// Run the walkthrough. Returns once every permission is granted or the user
/// asks to be left alone.
///
/// Blocking, and called before the daemon binds anything: the whole point is
/// that the user fixes the permissions BEFORE the first keypress that would
/// otherwise silently do nothing.
#[cfg(target_os = "macos")]
pub fn run_if_needed(observe: impl Fn() -> Grants) {
    // A terminal launch is a developer launch. They have stderr, they have
    // doctor, and a modal dialog in front of a test run is an obstruction.
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return;
    }

    let marker = marker_path();
    let welcomed = marker.as_ref().is_some_and(|p| p.exists());

    if !should_run(observe(), welcomed) {
        return;
    }

    if !welcomed {
        // The app has no Dock icon, so without this it is invisible: a user
        // who double-clicks it sees literally nothing happen.
        let _ = dialog(HELLO_TITLE, HELLO_BODY, &["Let's go"]);
    }

    // Loop rather than a fixed sequence: granting Input Monitoring can reveal
    // that Accessibility is also missing, and a user can toggle the wrong
    // switch and need the same step again. The loop ends on observation, not
    // on a count of dialogs shown.
    loop {
        let step = next_step(observe(), true);
        let Some(prompt) = prompt_for(step) else {
            break;
        };

        let choice = dialog(prompt.title, prompt.body, &[prompt.button, "Later"]);
        if choice.as_deref() == Some("Later") {
            // Leaving without the grants is allowed. The menu bar still names
            // what is missing, so this is a deferral rather than a dead end,
            // and trapping someone in a modal loop is worse than a half-set-up
            // app they can finish later.
            return;
        }

        crate::menuhost::open_privacy_pane(prompt.pane);

        // Wait on the user, then VERIFY. Asking "did it work?" cannot detect
        // the common failure, which is granting the switch to the wrong app or
        // to a stale bundle entry: the user sincerely believes they did it.
        let _ = dialog(WAITING_TITLE, WAITING_BODY, &["Done"]);

        // Still missing after they said Done? Say so plainly rather than
        // silently re-showing the same dialog, which reads as the app
        // ignoring them. `next_step` is not enough here: it would move on to
        // the OTHER permission and leave this one quietly unfixed.
        let still = observe();
        let granted = match step {
            Step::NeedInputMonitoring => still.input_monitoring,
            Step::NeedAccessibility => still.accessibility,
            Step::Ready { .. } => true,
        };
        if !granted {
            let again = dialog(RETRY_TITLE, RETRY_BODY, &["Try again", "Later"]);
            if again.as_deref() != Some("Try again") {
                return;
            }
        }
    }

    // Every grant is in place. Mark it, then tell them how to actually use it:
    // the permissions are a means, and a walkthrough that ends on "you're
    // configured" has not told anyone what to do next.
    if let Some(p) = marker {
        let _ = std::fs::write(&p, "");
    }
    let _ = dialog(DONE_TITLE, DONE_BODY, &["Got it"]);
}

#[cfg(not(target_os = "macos"))]
pub fn run_if_needed(_observe: impl Fn() -> Grants) {
    // The walkthrough is entirely about macOS TCC panes.
}

/// Every dialog the walkthrough can show, as (title, body, buttons).
///
/// Constants rather than literals at each call site so a test can compile all
/// of them. The osascript failure path is silent by design, so a prompt with an
/// unbalanced quote would not error: it would just never appear, and the user
/// would never be asked for the permission it exists to request.
pub const DIALOGS: &[(&str, &str, &[&str])] = &[
    (HELLO_TITLE, HELLO_BODY, &["Let's go"]),
    (WAITING_TITLE, WAITING_BODY, &["Done"]),
    (RETRY_TITLE, RETRY_BODY, &["Try again", "Later"]),
    (DONE_TITLE, DONE_BODY, &["Got it"]),
];

const HELLO_TITLE: &str = "Hi! I'm OutLoud.";
const HELLO_BODY: &str = "I turn your voice into text anywhere you can type.\n\n\
     I need to ask for two permissions first. It takes about a minute, and \
     I'll open each screen for you.";

const WAITING_TITLE: &str = "Waiting for you";
const WAITING_BODY: &str = "Turn the switch ON for OutLoud, then click Done here.\n\n\
     (If you don't see OutLoud in the list, click the + button and choose \
     OutLoud from Applications.)";

const RETRY_TITLE: &str = "Not quite yet";
const RETRY_BODY: &str = "I still can't see that permission. This usually means the switch is \
     off, or it got turned on for a different copy of OutLoud.\n\n\
     Want to try once more?";

const DONE_TITLE: &str = "You're all set";
const DONE_BODY: &str = "Hold the right Option key (just right of the space bar), say \
     something, then let go.\n\n\
     Your words appear wherever your cursor is. Try it in Messages.\n\n\
     I live in the menu bar at the top of the screen. Click the cat any time.";

/// Build the AppleScript for a dialog.
///
/// Separated from running it so the generated script can be checked without a
/// display. An unescaped quote or brace produces a script that fails to
/// compile, and the failure path is silent by design (a dialog that cannot be
/// shown must not change the exit path), so a malformed prompt would simply
/// mean the user is never asked for the permission at all.
pub fn dialog_script(title: &str, body: &str, buttons: &[&str]) -> String {
    fn escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }
    let list = buttons
        .iter()
        .map(|b| format!("\"{}\"", escape(b)))
        .collect::<Vec<_>>()
        .join(", ");
    let default = buttons.first().map(|b| escape(b)).unwrap_or_default();
    format!(
        "display dialog \"{}\" with title \"{}\" buttons {{{}}} \
         default button \"{}\" with icon note",
        escape(body),
        escape(title),
        list,
        default,
    )
}

/// Extract the pressed button from osascript's output.
///
/// Split out for the same reason as the script builder: the format is
/// osascript's, not ours, and a parser that silently returns `None` would make
/// every button read as a cancel.
pub fn parse_button(stdout: &str) -> Option<String> {
    stdout
        .split("button returned:")
        .nth(1)
        .map(|s| s.trim().to_string())
}

/// A modal dialog; returns the button label pressed, or `None` if it could not
/// be shown or was dismissed.
///
/// osascript rather than AppKit because this runs before `NSApplication` is
/// configured, and because an accessory app showing its own alert has to
/// activate itself, which steals focus from the field the user is typing in.
#[cfg(target_os = "macos")]
fn dialog(title: &str, body: &str, buttons: &[&str]) -> Option<String> {
    let out = std::process::Command::new("osascript")
        .args(["-e", &dialog_script(title, body, buttons)])
        .output()
        .ok()?;
    if !out.status.success() {
        // Cancel and close both land here; both mean "stop".
        return None;
    }
    parse_button(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(input: bool, ax: bool) -> Grants {
        Grants {
            input_monitoring: input,
            accessibility: ax,
        }
    }

    #[test]
    fn the_silent_failure_is_addressed_first() {
        // Both missing: Input Monitoring wins, because without it the user
        // sees nothing at all and concludes the app is broken.
        assert_eq!(
            next_step(grants(false, false), false),
            Step::NeedInputMonitoring
        );
        // And it still wins when only it is missing, even for a user who has
        // been welcomed before: a revoked grant is the same dead hotkey.
        assert_eq!(
            next_step(grants(false, true), true),
            Step::NeedInputMonitoring
        );
    }

    #[test]
    fn accessibility_is_asked_for_only_once_the_hotkey_can_fire() {
        assert_eq!(
            next_step(grants(true, false), false),
            Step::NeedAccessibility
        );
    }

    #[test]
    fn a_fully_granted_first_launch_still_says_hello() {
        // Someone reinstalling over existing grants gets no pane to open, but
        // they should still be told the app is running and which key to hold:
        // it has no Dock icon, so an app that says nothing is invisible.
        assert_eq!(
            next_step(grants(true, true), false),
            Step::Ready { first_run: true }
        );
    }

    #[test]
    fn a_welcomed_user_with_both_grants_is_left_alone() {
        assert_eq!(
            next_step(grants(true, true), true),
            Step::Ready { first_run: false }
        );
        assert!(!should_run(grants(true, true), true));
    }

    #[test]
    fn a_revoked_grant_reopens_the_walkthrough_for_an_old_user() {
        // The case an ad-hoc rebuild produces every single time, and the one a
        // first-run-only flag would miss.
        assert!(should_run(grants(false, true), true));
        assert!(should_run(grants(true, false), true));
    }

    #[test]
    fn every_actionable_step_names_a_pane_and_the_ready_step_does_not() {
        for step in [Step::NeedInputMonitoring, Step::NeedAccessibility] {
            let p = prompt_for(step).expect("an actionable step must have a prompt");
            assert!(p.pane.starts_with("Privacy_"), "{}", p.pane);
            assert!(!p.body.is_empty());
        }
        assert!(prompt_for(Step::Ready { first_run: true }).is_none());
    }

    #[test]
    fn the_two_panes_are_different() {
        // They are genuinely separate permissions, and an early version of the
        // docs sent users to Accessibility for both. The hotkey stayed dead
        // and the advice looked correct, which is the worst combination.
        let a = prompt_for(Step::NeedInputMonitoring).unwrap().pane;
        let b = prompt_for(Step::NeedAccessibility).unwrap().pane;
        assert_ne!(a, b);
    }

    #[test]
    fn a_pressed_button_is_read_back_not_mistaken_for_a_cancel() {
        // osascript's own format. Getting this wrong would make every button
        // read as a cancel, so the walkthrough would exit at the first click.
        assert_eq!(
            parse_button("button returned:Let's go\n").as_deref(),
            Some("Let's go")
        );
        // Cancel produces no such line, and must stay None.
        assert_eq!(parse_button(""), None);
    }

    /// Every dialog string must produce a script osascript can actually
    /// compile.
    ///
    /// This is the check that a human eyeballing the dialogs cannot make.
    /// `dialog` swallows failure on purpose, because a dialog that cannot be
    /// shown must not change the exit path, so a prompt containing an
    /// unbalanced quote does not error anywhere: it silently never appears,
    /// and the user is never asked for the permission it exists to request.
    /// Two of these strings already contain apostrophes.
    ///
    /// `osacompile` parses without displaying, so this needs no screen and
    /// interrupts nobody.
    #[test]
    #[cfg(target_os = "macos")]
    fn every_dialog_compiles_as_applescript() {
        for (title, body, buttons) in DIALOGS {
            let script = dialog_script(title, body, buttons);
            let out = std::process::Command::new("osacompile")
                .args(["-o", "/dev/null", "-e", &script])
                .output()
                .expect("osacompile is present on every macOS");
            assert!(
                out.status.success(),
                "the {title:?} dialog is not valid AppleScript, so it would \
                 never appear:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_prompts_that_open_panes_compile_too() {
        // The two permission prompts are built from `prompt_for` rather than
        // the DIALOGS table, so they need their own pass or the table would
        // vouch for strings it does not contain.
        for step in [Step::NeedInputMonitoring, Step::NeedAccessibility] {
            let p = prompt_for(step).unwrap();
            let script = dialog_script(p.title, p.body, &[p.button, "Later"]);
            let out = std::process::Command::new("osacompile")
                .args(["-o", "/dev/null", "-e", &script])
                .output()
                .expect("osacompile is present on every macOS");
            assert!(
                out.status.success(),
                "the {:?} prompt is not valid AppleScript:\n{}",
                p.title,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_quote_in_a_prompt_would_be_caught() {
        // Proves the compile check above is not vacuous: without escaping,
        // this input produces a script that osacompile rejects, which is
        // exactly the class of defect the check exists to find.
        let script = dialog_script("t", "she said \"hello\" loudly", &["OK"]);
        let out = std::process::Command::new("osacompile")
            .args(["-o", "/dev/null", "-e", &script])
            .output()
            .expect("osacompile is present on every macOS");
        assert!(
            out.status.success(),
            "escaping failed for a quoted body, so the dialog would never show"
        );
    }
}
