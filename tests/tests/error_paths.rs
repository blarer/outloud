//! Error-path propagation across crate boundaries.
//!
//! The taxonomy in `diag` (Environment / Permission / Configuration / Bug)
//! only works if errors keep their identity as they cross crate seams. The
//! moment `ax_edit::AxError::NotTrusted` gets flattened into a string on its
//! way through `text-target`, the doctor can no longer say "open System
//! Settings" and the user is back to decoding `-25204` by hand. These tests
//! pin the seams.

mod common;

use ax_edit::AxError;
use diag::{classify_ax_error, ErrorClass};
use text_target::TargetError;

#[test]
fn ax_errors_cross_into_text_target_without_losing_identity() {
    // The From impl is the seam. A NotTrusted must still be matchable as
    // NotTrusted after conversion, or the permission remedy is unreachable.
    let crossed: TargetError = AxError::NotTrusted.into();
    match crossed {
        TargetError::Ax(AxError::NotTrusted) => {}
        other => panic!("NotTrusted lost its identity crossing the seam: {other:?}"),
    }
}

#[test]
fn every_ax_error_variant_crosses_and_classifies() {
    // Walk every variant across text-target and back into diag's taxonomy.
    // A new AxError variant that panics or misclassifies here is caught at
    // the seam rather than in a user's bug report.
    let cases = [
        (AxError::NotTrusted, ErrorClass::Permission),
        (AxError::NoFocusedElement, ErrorClass::Environment),
        (AxError::NoTextValue, ErrorClass::Environment),
        (AxError::NotSettable, ErrorClass::Environment),
        (AxError::Unsupported, ErrorClass::Environment),
        (AxError::Api(-25204), ErrorClass::Bug),
    ];
    for (err, expect) in cases {
        let class = classify_ax_error(&err);
        assert_eq!(class, expect, "classification changed for {err:?}");
        // Crossing into text-target must preserve enough to re-classify.
        let crossed: TargetError = err.into();
        if let TargetError::Ax(inner) = crossed {
            assert_eq!(classify_ax_error(&inner), expect);
        } else {
            panic!("AxError crossed into a non-Ax TargetError");
        }
    }
}

#[test]
fn only_bug_class_failures_are_issue_worthy_after_crossing() {
    // The routing rule the whole taxonomy exists for: environmental noise
    // stays out of the tracker even after errors have crossed two crates.
    let env_err: TargetError = AxError::NoFocusedElement.into();
    if let TargetError::Ax(inner) = env_err {
        assert!(!classify_ax_error(&inner).worth_a_github_issue());
    }
    let bug_err: TargetError = AxError::Api(-1).into();
    if let TargetError::Ax(inner) = bug_err {
        assert!(classify_ax_error(&inner).worth_a_github_issue());
    }
}

#[test]
fn target_errors_render_a_reason_not_just_a_code() {
    // Debugging-doc rule: user-facing errors name the situation. Every
    // TargetError variant must produce a nonempty, non-numeric-only message.
    let errors: Vec<TargetError> = vec![
        TargetError::Unsupported("kitty needs allow_remote_control"),
        TargetError::NotReadable("OSC 52 is write-only"),
        TargetError::Transport("tmux exited 1".to_string()),
        AxError::NotTrusted.into(),
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed").into(),
    ];
    for err in errors {
        let msg = err.to_string();
        assert!(msg.len() > 5, "message too thin: `{msg}`");
        assert!(
            msg.chars().any(|c| c.is_alphabetic()),
            "message is just a code: `{msg}`"
        );
    }
}

#[test]
fn write_failures_recorded_for_replay_keep_their_class() {
    // Seam between a live failure and the replay artifact: the error class
    // must survive record -> serialize -> parse, because the class is what
    // tells the person reading the record whether to fix the environment or
    // the code.
    let err: TargetError = AxError::NotTrusted.into();
    let class = match &err {
        TargetError::Ax(inner) => classify_ax_error(inner),
        _ => unreachable!(),
    };
    let mut rec = diag::replay::SessionRecord::new();
    rec.record_write_err(&err.to_string(), class);
    let parsed = diag::replay::SessionRecord::parse(&rec.serialize()).unwrap();
    parsed.verify_consistency().unwrap();
    let write = parsed.write.unwrap();
    assert!(!write.ok);
    assert_eq!(write.class, Some(ErrorClass::Permission));
}

#[test]
fn recorded_error_details_are_scrubbed_of_identity() {
    // Error strings love to embed paths ("no socket at /Users/jane/...").
    // Recording one must scrub it even though the caller passed it raw.
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        // No HOME means nothing to scrub against; the redaction unit tests
        // cover the mechanism itself.
        eprintln!("skip: HOME not set, scrub target unavailable");
        return;
    }
    let mut rec = diag::replay::SessionRecord::new();
    rec.record_write_err(
        &format!("could not open socket at {home}/.aqua/text-target.sock"),
        ErrorClass::Configuration,
    );
    let text = rec.serialize();
    assert!(!text.contains(&home), "home path leaked:\n{text}");
    assert!(
        text.contains("~/.aqua"),
        "scrub should keep the shape:\n{text}"
    );
}
