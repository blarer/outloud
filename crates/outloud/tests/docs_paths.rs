//! Do the commands and paths our documentation tells users to run exist?
//!
//! This exists because the same defect shipped twice. The README told users
//! to run a script that did not build the speech helper, so a fresh clone
//! produced a daemon that could not transcribe. That was fixed by naming the
//! right script. Hours later a rename moved the script and left the README
//! pointing at the old name, and the documented install broke again in
//! exactly the same place:
//!
//! ```text
//! $ ./scripts/bundle-aquad-macos.sh
//! bash: ./scripts/bundle-aquad-macos.sh: No such file or directory
//! ```
//!
//! Nothing failed when that happened. Not the build, not the test suite, not
//! clippy. The first person to find out would have been a beta user, who is
//! the one person who cannot fix it.
//!
//! These tests are deliberately shallow: they check that referenced files
//! exist, not that the instructions are correct. A cheap check that runs on
//! every commit beats a thorough one nobody runs, and "the path in the README
//! is real" is the specific thing that broke twice.

use std::path::{Path, PathBuf};

/// Repository root, from this test's manifest directory.
fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/crates/outloud; go up twice.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives two levels below the repo root")
        .to_path_buf()
}

/// Every `scripts/...` path mentioned in a document must exist.
///
/// Matches the shape used in prose and fenced blocks alike (`./scripts/x.sh`
/// or `scripts/x.sh`), stopping at the first character that cannot appear in
/// one of our script names.
fn scripts_referenced_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (idx, _) in text.match_indices("scripts/") {
        let rest = &text[idx + "scripts/".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
            .collect();
        // Only shell scripts: `scripts/` also appears in prose like
        // "the scripts/ directory", which has no file to check.
        if name.ends_with(".sh") {
            found.push(format!("scripts/{name}"));
        }
    }
    found.sort();
    found.dedup();
    found
}

#[test]
fn every_script_the_readme_names_exists() {
    let root = repo_root();
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md is readable");

    let referenced = scripts_referenced_in(&readme);
    assert!(
        !referenced.is_empty(),
        "no scripts found in the README; the extractor is probably broken, \
         which would make this test silently vacuous"
    );

    let missing: Vec<_> = referenced
        .iter()
        .filter(|rel| !root.join(rel).exists())
        .collect();
    assert!(
        missing.is_empty(),
        "the README tells users to run scripts that do not exist: {missing:?}. \
         A rename most likely moved them. This is the exact defect that shipped \
         twice: nothing else in the test suite notices, and the first person to \
         find out is a beta user who cannot fix it."
    );
}

#[test]
fn every_script_the_quickstart_and_contributing_name_exists() {
    let root = repo_root();
    // The quickstart is the copy-pasteable path; CONTRIBUTING is the one a
    // new contributor follows. Both go stale the same way as the README.
    for doc in ["docs/macos-quickstart.md", "CONTRIBUTING.md"] {
        let path = root.join(doc);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue; // A doc that does not exist is not this test's business.
        };
        let missing: Vec<_> = scripts_referenced_in(&text)
            .into_iter()
            .filter(|rel| !root.join(rel).exists())
            .collect();
        assert!(
            missing.is_empty(),
            "{doc} names scripts that do not exist: {missing:?}"
        );
    }
}

/// The install path is load-bearing: it is the first thing a stranger runs,
/// and if it names the wrong script they get a daemon that cannot transcribe.
/// Pinned by name so a rename has to update this test consciously rather than
/// leaving the README silently wrong.
#[test]
fn the_readme_install_path_names_the_bundler_that_builds_the_speech_helper() {
    let root = repo_root();
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md is readable");

    let bundler = scripts_referenced_in(&readme)
        .into_iter()
        .find(|s| s.contains("bundle-") && s.contains("macos"))
        .expect("the README must tell users which script packages the app");

    let script = std::fs::read_to_string(root.join(&bundler))
        .unwrap_or_else(|e| panic!("{bundler} is named by the README but unreadable: {e}"));

    // A bare `cargo build` does not produce the Swift helper, so whichever
    // script the README points at must compile it. This is the actual
    // requirement; the filename is incidental.
    assert!(
        script.contains("swiftc"),
        "the README's install script ({bundler}) does not invoke swiftc, so a \
         fresh clone will produce a daemon with no recognizer. Either it is \
         pointing at the wrong bundler, or the helper build was removed."
    );
}

#[test]
fn extractor_finds_scripts_and_ignores_prose() {
    // Guards the tests above from silently passing on an empty match set.
    let found = scripts_referenced_in(
        "run ./scripts/doctor.sh and `scripts/verify-head.sh`, see the scripts/ directory",
    );
    assert_eq!(found, vec!["scripts/doctor.sh", "scripts/verify-head.sh"]);
}
