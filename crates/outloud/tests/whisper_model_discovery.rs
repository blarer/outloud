//! `--asr whisper` must find a model the documented ways actually put one,
//! not just the ones a developer happens to type by hand.
//!
//! `discover_whisper_model` (`crates/outloud/src/main.rs`) used to search the
//! executable's directory and the current directory, but never
//! `config::model_dir()` (`~/.outloud/models`), which is the ONE path
//! `asr::models::fetch` actually writes to and the path every documented
//! `curl`/fetch instruction in `docs/investigations/whisper-spike.md` and
//! `docs/asr-integration.md` names. A user who followed those instructions to
//! the letter got the "needs a model" error anyway, because the daemon never
//! looked where its own model manager puts things. This is the actual defect
//! blocking `--asr whisper` on a fresh Linux machine, model download included.
//!
//! Like `model_dir_migration.rs`, this runs the real binary with a temporary
//! `HOME` rather than unit-testing `discover_whisper_model` directly, because
//! the function reads `std::env::current_dir`/`current_exe` and a unit test
//! cannot isolate those without changing global process state that races
//! every other `#[test]` in the same binary (cargo runs unit tests
//! multi-threaded, one process). A separate integration binary needs no lock.
//!
//! This build has no `whisper` feature (CI's default build; see
//! `crates/asr/Cargo.toml` for why that stays off by default), so
//! construction always succeeds (the stub buffers audio and does nothing
//! useful with it) and finalize always fails once a model path was handed
//! in. What the found-a-model test asserts on is therefore the "outloud:
//! using model <path>" line `discover_whisper_model` prints on a hit: that
//! line is written synchronously, before the model is ever handed to the
//! recognizer, so it is unaffected by whether the stub backend can finish an
//! utterance. The eventual finalize failure surfaces via
//! `engine.transition(OverlayState::Error, ...)`, rendered by a background
//! thread that polls the shared state at 33ms intervals purely for the
//! interactive overlay/log display; under `--once` that thread can lose the
//! race against process exit (observed directly: five back-to-back manual
//! runs against the real cache dir printed "using model ..." every time, but
//! the state-log line only sometimes made it out before the process exited).
//! Asserting on that racy line here would be testing an unrelated timing
//! quirk, not model discovery, so this test does not.

use std::path::Path;
use std::process::Command;

fn run_with_home(home: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_outloud"))
        .args([
            "--once",
            "--asr",
            "whisper",
            "--wav",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../asr/testdata/quick-brown-fox.wav"
            ),
            "--no-overlay",
        ])
        .env("HOME", home)
        // Isolates this run from whatever real model the developer running
        // the suite happens to have set for their own shell; the whole point
        // is to prove discovery works from HOME alone.
        .env_remove("OUTLOUD_WHISPER_MODEL")
        .output()
        .expect("the daemon binary should be runnable");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn temp_home(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "outloud-whisper-discovery-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

#[test]
fn a_model_placed_exactly_where_the_model_manager_fetches_it_is_found() {
    let home = temp_home("cache-hit");
    let models = home.join(".outloud").join("models");
    std::fs::create_dir_all(&models).unwrap();
    // Named exactly as `asr::models::fetch` names it: after the registry
    // `id` (`whisper-base.en`), not after the upstream `ggml-*.bin` file it
    // downloaded from. A scan that only recognises the `ggml-` naming
    // would miss every model the app's own fetcher produced.
    std::fs::write(
        models.join("whisper-base.en"),
        b"not a real model, just proving discovery",
    )
    .unwrap();

    let (_ok, stderr) = run_with_home(&home);

    // This build has no whisper feature, so the run can never fully
    // succeed; the only claim made here is that discovery found the
    // cache-dir model and said so, synchronously and unconditionally,
    // before anything about finalizing was decided. See the module doc for
    // why the eventual failure text is not asserted on.
    assert!(
        stderr.contains(&models.join("whisper-base.en").display().to_string()),
        "discovery should have found and announced the cache-dir model:\n{stderr}"
    );
    // The regression this test exists to catch: before the fix, a model
    // sitting in exactly this directory was invisible to discovery and
    // produced the "needs a model" remedy instead.
    assert!(
        !stderr.contains("needs a model"),
        "a model sitting in ~/.outloud/models should never be reported as missing:\n{stderr}"
    );
}

#[test]
fn a_partial_download_in_the_cache_dir_is_not_mistaken_for_a_model() {
    let home = temp_home("partial-only");
    let models = home.join(".outloud").join("models");
    std::fs::create_dir_all(&models).unwrap();
    // `asr::models::fetch` writes here mid-download and only renames to the
    // final name after the checksum passes. A resumable download in
    // progress must not be handed to whisper.cpp as a finished model.
    std::fs::write(models.join("whisper-base.en.partial"), b"still downloading").unwrap();

    let (ok, stderr) = run_with_home(&home);

    assert!(!ok, "no usable model exists yet:\n{stderr}");
    assert!(
        stderr.contains("needs a model"),
        "an in-progress download must not satisfy discovery:\n{stderr}"
    );
}

#[test]
fn an_empty_home_reports_the_exact_fetch_command_naming_the_cache_dir() {
    let home = temp_home("empty");

    let (ok, stderr) = run_with_home(&home);

    assert!(!ok, "no model anywhere:\n{stderr}");
    // The error must name a real, runnable command, not just a URL to go
    // figure out yourself. This is the difference between a user unblocking
    // themselves in one paste and filing an issue.
    assert!(stderr.contains("curl"), "{stderr}");
    assert!(
        stderr.contains(&home.join(".outloud").join("models").display().to_string()),
        "the remedy must name the actual cache dir under this HOME, not a \
         generic path the user then has to translate:\n{stderr}"
    );
}
