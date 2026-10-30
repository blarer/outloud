//! `--command` must never launch anything from a sentence a user dictated.
//!
//! The unit tests in `app-command` prove the grammar refuses prose. This
//! file proves the *shipped binary* does, through the real argument parser
//! and the real registry on whatever machine is running the tests.
//!
//! That distinction has already cost this project real bugs: several
//! defects this month were correct logic that never reached the user, or
//! reached them through a path nothing tested. A grammar that refuses
//! "the shop will open at nine" is worth nothing if the flag plumbing
//! around it resolves differently.
//!
//! Every assertion here is about REFUSAL, and the one test that exercises
//! the success path uses `--command-dry-run`. Nothing in this file launches
//! an application: the first draft of the last test did, and opened Chess
//! on the machine running it. A test suite that opens windows is a test
//! suite people disable.

use std::process::Command;

/// Run `outloud --command PHRASE` and return (exit code, stdout).
///
/// `dry` resolves without launching, for the cases that assert on a
/// successful resolution.
fn run(flag: &str, phrase: &str) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_outloud"))
        .args([flag, phrase])
        .output()
        .expect("run the outloud binary");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

/// The real flag, for refusal cases. Safe because a refusal launches
/// nothing by definition, which is what the assertions check.
fn command(phrase: &str) -> (Option<i32>, String) {
    run("--command", phrase)
}

#[test]
fn dictated_prose_is_refused_by_the_real_binary() {
    // Each of these is a sentence somebody could plausibly say into a
    // document while dictating. If any launches an application, dictation
    // can act on the machine, which is the single failure this feature is
    // designed around.
    //
    // The assertion is "nothing was launched", NOT "refused for reason X".
    // That distinction came out of a real failure here: "please start the
    // meeting without me" refuses, but as UnknownApp rather than
    // NotACommand, because `please` is stripped as politeness and `start`
    // is then genuinely sentence-initial. Both outcomes are safe, and a
    // test pinned to one reason was asserting an implementation detail
    // while claiming to assert a safety property.
    for prose in [
        "the shop will open at nine",
        "I'll open the attachment later",
        "keep an open mind about it",
        "we should switch to something else next quarter",
        "opening the file was a mistake",
        "did you open the window",
        "please start the meeting without me",
        // The dangerous shape: prose ending in a real application name.
        "I would love it if you would open Calculator",
        "remind me to open Calculator",
        "tell them I'll start Chess tomorrow",
    ] {
        let (code, stdout) = command(prose);
        assert_eq!(
            code,
            Some(1),
            "DANGEROUS: {prose:?} did not refuse; it said {stdout:?}"
        );
        // The success path always prints "opening X", so its absence is
        // direct evidence that nothing was launched.
        assert!(
            !stdout.contains("opening "),
            "DANGEROUS: {prose:?} reported a launch: {stdout:?}"
        );
    }
}

#[test]
fn an_imperative_with_politeness_still_works() {
    // The flip side of the test above: "please open X" IS a command, and
    // stripping the politeness is deliberate. Dry run, so nothing opens.
    //
    // This test exists to stop someone "fixing" the politeness strip after
    // reading the comment above and concluding it is a hazard. It is not:
    // a bare imperative with no trailing context is exactly what a user in
    // command mode says.
    let out = Command::new(env!("CARGO_BIN_EXE_outloud"))
        .args(["--list-apps"])
        .output()
        .expect("run the outloud binary");
    if !out.status.success() {
        return; // no desktop applications here; nothing to assert against
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(first) = stdout.lines().nth(1).map(str::trim) else {
        return;
    };
    let (code, said) = run("--command-dry-run", &format!("please open {first}"));
    assert_eq!(code, Some(0), "'please open {first}' refused: {said}");
    assert!(said.starts_with("opening "), "unexpected output: {said}");
}

#[test]
fn a_verb_with_no_application_asks_which_one() {
    for phrase in ["open", "switch to", "open the"] {
        let (code, stdout) = command(phrase);
        assert_eq!(code, Some(1), "{phrase:?} should refuse");
        assert!(
            stdout.contains("Which application"),
            "{phrase:?} said {stdout:?}"
        );
    }
}

#[test]
fn an_application_that_is_not_installed_is_refused() {
    // A name no machine has, so this is a stable assertion rather than one
    // that depends on what the test runner happens to have installed.
    let (code, stdout) = command("open Zzyzx Quuxinator 9000");
    assert_eq!(code, Some(1));
    // Two acceptable refusals, and which one depends on the environment:
    //
    //   "could not find"   a registry exists and this is not in it
    //   "no list of"       the registry is EMPTY, so nothing could match
    //
    // The second is the case in the Nix build sandbox, which has no
    // /Applications and no /usr/share/applications, and it is why this test
    // failed CI while passing on both a Mac and a Linux box with a desktop.
    // Pinning the message to "could not find" was asserting that the test
    // machine has applications installed, which is not a property of the
    // code under test.
    //
    // What matters is that it refused and launched nothing. Both messages
    // are accurate about their own situation, which is the point of having
    // NoRegistry as a distinct variant.
    assert!(
        stdout.contains("could not find") || stdout.contains("no list of"),
        "unexpected refusal: {stdout:?}"
    );
    assert!(!stdout.contains("opening "), "DANGEROUS: reported a launch");
}

#[test]
fn list_apps_agrees_with_what_commands_can_open() {
    // --list-apps exists so a user can tell "not installed" apart from "the
    // feature is broken". If it disagreed with the registry the parser uses,
    // it would be actively misleading.
    let out = Command::new(env!("CARGO_BIN_EXE_outloud"))
        .arg("--list-apps")
        .output()
        .expect("run the outloud binary");
    let stdout = String::from_utf8_lossy(&out.stdout);

    if !out.status.success() {
        // Correct on a machine with no desktop applications, which includes
        // some CI runners. The message must say so rather than looking like
        // a crash.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("no applications found"),
            "--list-apps failed for an unexpected reason: {stderr}"
        );
        return;
    }

    let names: Vec<&str> = stdout
        .lines()
        .skip(1) // the "N applications:" header
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        !names.is_empty(),
        "--list-apps succeeded but listed nothing"
    );

    // Every listed application must be openable by name. This is the real
    // check: it runs the whole path (parse, resolve, refuse-or-accept) over
    // the actual registry, so a name the launcher cannot resolve fails here.
    //
    // DRY RUN, so this resolves without opening anything. Written with the
    // real flag first, which opened Chess and would have opened two dozen
    // more.
    //
    // Ambiguity is an acceptable outcome: "Calculator" and "LibreOffice
    // Calc" both exist on the Linux test box, and asking which is correct
    // behaviour, not a failure. What must NOT happen is "could not find",
    // which would mean the list and the resolver disagree.
    for name in names.iter().take(25) {
        let (_, said) = run("--command-dry-run", &format!("open {name}"));
        assert!(
            !said.contains("could not find"),
            "--list-apps offers {name:?} but a command for it cannot resolve: {said:?}"
        );
    }
}
