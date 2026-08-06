//! Does the daemon actually migrate the model directory when it starts?
//!
//! config::paths has unit tests for the migration itself. They cannot catch
//! the failure that matters here: a migration that is never called. The call
//! is one line in main.rs, nothing else references it, and "the tests pass
//! while the feature is dead" is the defect this repository keeps shipping.
//!
//! So these run the real binary with a temporary HOME and read the
//! filesystem afterwards.
//!
//! `--once --asr bogus` is deliberate: it gets past argument parsing, runs
//! the startup migration, then fails when it tries to build an unknown
//! recognizer. No microphone, no hotkey, no single-instance lock, and
//! nothing typed into whatever window the developer has focused.

use std::path::Path;
use std::process::Command;

fn run_with_home(home: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_outloud"))
        .args(["--once", "--asr", "bogus", "--no-overlay"])
        .env("HOME", home)
        .output()
        .expect("the daemon binary should be runnable");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn temp_home(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "outloud-migration-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

#[test]
fn a_real_launch_moves_a_pre_rename_model_directory() {
    let home = temp_home("legacy");
    let legacy = home.join(".aqua-oss").join("models");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("whisper-base.en"), b"weights").unwrap();

    let (_ok, stderr) = run_with_home(&home);

    assert!(
        !home.join(".aqua-oss").exists(),
        "the old directory should be gone after a launch:\n{stderr}"
    );
    // The bytes must survive, unchanged and un-redownloaded. This is the
    // whole point: these files are gigabytes in real use.
    assert_eq!(
        std::fs::read(home.join(".outloud/models/whisper-base.en")).unwrap(),
        b"weights"
    );
    // And the user is told, because 1.4GB moving without a word is worse
    // than the move itself.
    assert!(
        stderr.contains("moved models from"),
        "the migration must announce itself:\n{stderr}"
    );

    // Second launch: nothing left to do, and nothing said about it.
    let (_ok, stderr2) = run_with_home(&home);
    assert!(
        !stderr2.contains("moved models"),
        "an already-migrated home must stay silent:\n{stderr2}"
    );
}

#[test]
fn a_launch_with_both_directories_destroys_neither() {
    let home = temp_home("both");
    let legacy = home.join(".aqua-oss").join("models");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("old"), b"old").unwrap();
    let current = home.join(".outloud").join("models");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::write(current.join("new"), b"new").unwrap();

    let (_ok, stderr) = run_with_home(&home);

    assert_eq!(std::fs::read(legacy.join("old")).unwrap(), b"old");
    assert_eq!(std::fs::read(current.join("new")).unwrap(), b"new");
    assert!(
        stderr.contains("still exists alongside"),
        "the leftover directory must be reported, not silently ignored:\n{stderr}"
    );
}

#[test]
fn a_launch_on_a_fresh_home_creates_no_model_directory() {
    // A user on macOS 26 may never download a model at all. An empty
    // ~/.outloud in their home directory claims otherwise.
    let home = temp_home("fresh");

    let (_ok, stderr) = run_with_home(&home);

    assert!(
        !home.join(".outloud").exists(),
        "a fresh home must not gain a model directory:\n{stderr}"
    );
    assert!(!stderr.contains("moved models"), "{stderr}");
}
