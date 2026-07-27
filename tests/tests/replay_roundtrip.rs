//! Record on one machine, replay on another: the diag::replay round trip
//! against the real transport selector.
//!
//! This is the workflow that turns "only reproduces on one user's machine"
//! into a test: their `--record` artifact carries the facts, we reconstruct
//! an environment from those facts here, run the current selector against
//! it, and diff the answer against what their build did. The tty-vs-
//! destination bug is replayed below exactly that way.

mod common;

use common::{env_from_record, record_env, SimEnv};
use diag::replay::SessionRecord;
use text_target::detect::select;

/// Simulate a full recorded session on "the user's machine" described by
/// `env`, then hand back only the serialized artifact, as if attached to an
/// issue. Everything after this call knows nothing but the record.
fn record_session_on(env: &SimEnv) -> String {
    let mut rec = SessionRecord::new();
    record_env(&mut rec, env);
    let selection = select(env);
    rec.record_transport(selection.name, selection.reason);
    rec.record_ax(
        "AXTextArea",
        "Untitled - TextEdit",
        "the quick brown fox",
        "set-selected-text",
    );
    rec.record_intent("replace", "quick", "slow");
    rec.record_transform("the quick brown fox", "the slow brown fox");
    rec.record_write_ok("set-selected-text");
    rec.record_timing("read", std::time::Duration::from_micros(30_000));
    rec.record_timing("write", std::time::Duration::from_micros(13_400));
    rec.serialize()
}

#[test]
fn a_recorded_session_replays_to_the_same_selection() {
    // Healthy case: same code, same facts, same answer. If this ever fails,
    // selection has become nondeterministic, which would make every replay
    // diagnosis untrustworthy.
    let user_env = SimEnv {
        destination_is_terminal: true,
        has_display: false,
        ..Default::default()
    }
    .with_var("TMUX", "present")
    .with_command("tmux");

    let artifact = record_session_on(&user_env);

    // The "our machine" side: only the artifact crosses.
    let rec = SessionRecord::parse(&artifact).expect("artifact parses");
    rec.verify_consistency()
        .expect("artifact is internally consistent");
    let replay_env = env_from_record(&rec);
    let replayed = select(&replay_env);
    assert!(
        rec.compare_selection(replayed.name).is_none(),
        "selection diverged on identical facts"
    );
}

#[test]
fn replaying_the_tty_bug_produces_a_divergence_that_names_it() {
    // Reconstruction of today's bug. The buggy selector asked "does OUR
    // process have a tty" (yes: launched from a shell) instead of "is the
    // DESTINATION a terminal" (no: the user was in a browser), so it picked
    // tmux and typed into an unwatched shell. Forge the record a buggy
    // build would have produced: correct facts, wrong conclusion.
    let user_env = SimEnv::desktop_trusted()
        .with_var("TMUX", "present")
        .with_var("TERM", "xterm-256color")
        .with_command("tmux");
    // destination_is_terminal: false -- the fact was always right; the buggy
    // code just consulted its own tty instead of this fact.

    let mut rec = SessionRecord::new();
    record_env(&mut rec, &user_env);
    rec.record_transport("tmux", "process has a controlling terminal"); // the bug
    let artifact = rec.serialize();

    // Replay against the FIXED selector.
    let parsed = SessionRecord::parse(&artifact).unwrap();
    let replayed = select(&env_from_record(&parsed));
    let divergence = parsed
        .compare_selection(replayed.name)
        .expect("fixed selector must disagree with the buggy recording");
    assert_eq!(divergence.recorded, "tmux");
    assert_eq!(divergence.replayed, "macos-ax");
    // The Display output is the line that goes in the issue.
    let msg = divergence.to_string();
    assert!(msg.contains("tmux") && msg.contains("macos-ax"), "{msg}");
}

#[test]
fn the_artifact_is_safe_to_attach_to_a_public_issue() {
    // The property the whole module exists for: a record produced from a
    // session full of private content contains none of it. Anything found
    // here is a redaction hole, which is a release blocker, not a nit.
    let mut rec = SessionRecord::new();
    record_env(&mut rec, &SimEnv::desktop_trusted());
    rec.record_transport("macos-ax", "accessibility trusted");
    rec.record_ax(
        "AXTextArea",
        "medical-history-draft.docx - Word",
        "patient presents with confidential symptoms",
        "set-value",
    );
    rec.record_intent("replace", "confidential symptoms", "redacted things");
    rec.record_transform(
        "patient presents with confidential symptoms",
        "patient presents with redacted things",
    );
    let artifact = rec.serialize();

    for secret in [
        "medical",
        "docx",
        "patient",
        "confidential",
        "symptoms",
        "redacted things",
    ] {
        assert!(
            !artifact.contains(secret),
            "redaction hole, leaked `{secret}`:\n{artifact}"
        );
    }
    // And the diagnostic signal is still there.
    assert!(artifact.contains("ax.role AXTextArea"));
    assert!(artifact.contains("transform."));
}

#[test]
fn replayed_timings_flow_into_the_budget_machinery() {
    // A user's "it feels slow" report replays into the same p50/p99 harness
    // the nightly gates use, so the record can answer "was it actually over
    // budget" without the user's machine.
    let artifact = record_session_on(&SimEnv::desktop_trusted());
    let rec = SessionRecord::parse(&artifact).unwrap();
    let recorder = rec.timings_recorder();
    let summary = recorder.summary();
    let read = summary
        .iter()
        .find(|s| s.stage == diag::timing::Stage::Read)
        .expect("read timing survived the trip");
    // M0's AX-read gate is 50ms at p95; the recorded 30ms must sit inside.
    assert!(!read.over_budget(std::time::Duration::from_millis(50)));
    assert!(read.over_budget(std::time::Duration::from_millis(10)));
}

#[test]
fn a_truncated_artifact_fails_loudly_not_subtly() {
    // Users paste partial files. A record that parses but lies is worse
    // than one that refuses: verify_consistency must catch geometry damage
    // even when line-level parsing succeeds.
    let artifact = record_session_on(&SimEnv::desktop_trusted());
    // Corrupt the transform section by dropping its last line.
    let corrupted: String = artifact
        .lines()
        .filter(|l| !l.starts_with("transform.inserted_chars"))
        .map(|l| format!("{l}\n"))
        .collect();
    let rec = SessionRecord::parse(&corrupted).expect("line format still parses");
    assert!(
        rec.verify_consistency().is_err(),
        "damaged geometry must be rejected"
    );
}
