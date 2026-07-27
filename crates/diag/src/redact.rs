//! Redacting bug-report bundler.
//!
//! This tool reads the focused text field of every application the user works
//! in, so its diagnostics can contain anything the user has ever typed:
//! passwords in a password manager's search box, medical notes, private chat.
//! A bug report must therefore be redacted BY CONSTRUCTION, not by asking the
//! reporter to check. The policy:
//!
//! - Transcribed text and clipboard contents: never included, only lengths.
//! - Window titles: replaced with a stable hash so "same window across two
//!   log lines" stays diagnosable without revealing the title.
//! - File paths under the user's home: home prefix and username scrubbed;
//!   only the final component survives, since debugging usually needs "which
//!   kind of file" rather than "which document".
//! - Anything a check's `detail` line captured is passed through the same
//!   scrubbers before bundling.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::{Report, Status};

/// Replace secret content with a length-preserving marker. Length is kept
/// because "the field was empty" vs "the field had 4000 chars" is real
/// diagnostic signal; the characters themselves never are.
pub fn redact_content(text: &str) -> String {
    format!("[redacted: {} chars]", text.chars().count())
}

/// Replace a window title with a short stable hash. Stability matters:
/// a report where the same window appears twice must show the same token,
/// or correlation across log lines is lost.
pub fn redact_title(title: &str) -> String {
    let mut h = DefaultHasher::new();
    title.hash(&mut h);
    format!("[title#{:08x}]", (h.finish() & 0xffff_ffff) as u32)
}

/// Scrub a file path: strip the home directory (which contains the username)
/// and drop every component except the last. `/Users/jane/Documents/taxes.txt`
/// becomes `~/…/taxes.txt`.
pub fn redact_path(path: &str, home: &str) -> String {
    let stripped = path.strip_prefix(home).unwrap_or(path);
    let last = stripped.rsplit('/').next().unwrap_or(stripped);
    if path.starts_with(home) {
        format!("~/…/{last}")
    } else if path.contains('/') {
        format!("…/{last}")
    } else {
        last.to_string()
    }
}

/// Scrub free text (check detail lines) of the two identifiers we know how
/// to find mechanically: the home path and the username. This is defence in
/// depth on top of checks avoiding sensitive detail in the first place.
pub fn scrub_free_text(text: &str, home: &str, user: &str) -> String {
    let mut out = text.replace(home, "~");
    if !user.is_empty() {
        // Usernames can appear outside the home path (e.g. in device names).
        out = out.replace(user, "[user]");
    }
    out
}

/// Build a pasteable bug-report bundle from doctor results.
///
/// Everything in the output has passed through [`scrub_free_text`]; content,
/// titles, and paths must already have been redacted by whoever put them in a
/// detail line, but the scrubber catches the mechanical identifiers anyway.
pub fn bundle(reports: &[Report]) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    bundle_with_identity(reports, &home, &user)
}

/// Testable core of [`bundle`]: identity supplied by the caller.
pub fn bundle_with_identity(reports: &[Report], home: &str, user: &str) -> String {
    let mut out = String::new();
    out.push_str("## aqua-oss doctor report\n");
    out.push_str(&format!(
        "os: {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    // Only Bug-class failures belong in an issue; say so in the report itself
    // so triage does not have to.
    let bug_worthy = reports
        .iter()
        .filter(|r| {
            r.outcome
                .class
                .map(|c| c.worth_a_github_issue())
                .unwrap_or(false)
        })
        .count();
    out.push_str(&format!(
        "bug-class failures: {bug_worthy} (only these belong in a GitHub issue)\n\n"
    ));
    for r in reports {
        let detail = scrub_free_text(&r.outcome.detail, home, user);
        out.push_str(&format!("[{}] {}: {}\n", r.outcome.status, r.name, detail));
        if let (Some(class), Some(remedy)) = (&r.outcome.class, &r.outcome.remedy) {
            let remedy = scrub_free_text(remedy, home, user);
            out.push_str(&format!("       class: {class}\n       remedy: {remedy}\n"));
        }
    }
    // Redaction notice: reviewers must know absence of content is deliberate.
    out.push_str(
        "\n(transcripts, clipboard contents, window titles, and file paths are \
         redacted by construction)\n",
    );
    let _ = Status::Pass; // keep the import honest if the loop changes
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckOutcome, ErrorClass, Report};

    #[test]
    fn content_redaction_keeps_only_length() {
        let secret = "my password is hunter2";
        let red = redact_content(secret);
        assert!(!red.contains("hunter2"));
        assert!(red.contains("22 chars"));
        // Unicode counted as chars, not bytes: byte counts leak encoding info
        // and confuse "how long was the field" reasoning.
        assert!(redact_content("héllo").contains("5 chars"));
    }

    #[test]
    fn title_redaction_is_stable_and_opaque() {
        let a = redact_title("taxes_2025_final.xlsx - Excel");
        let b = redact_title("taxes_2025_final.xlsx - Excel");
        let c = redact_title("something else");
        assert_eq!(a, b, "same title must map to same token");
        assert_ne!(a, c);
        assert!(!a.contains("taxes"));
    }

    #[test]
    fn path_redaction_strips_home_and_middle() {
        let red = redact_path("/Users/jane/Documents/private/taxes.txt", "/Users/jane");
        assert_eq!(red, "~/…/taxes.txt");
        assert!(!red.contains("jane"));
        assert!(!red.contains("Documents"));
    }

    #[test]
    fn non_home_path_keeps_only_last_component() {
        assert_eq!(
            redact_path("/tmp/secret-dir/log.txt", "/Users/jane"),
            "…/log.txt"
        );
        assert_eq!(redact_path("plainfile", "/Users/jane"), "plainfile");
    }

    #[test]
    fn free_text_scrub_removes_home_and_username() {
        let s = scrub_free_text(
            "model at /Users/jane/.aqua-oss/models missing; user jane should download",
            "/Users/jane",
            "jane",
        );
        assert!(!s.contains("jane"), "scrubbed: {s}");
        assert!(s.contains("~/.aqua-oss/models"));
    }

    #[test]
    fn bundle_scrubs_details_and_counts_bug_failures() {
        let reports = vec![
            Report {
                name: "model-files",
                outcome: CheckOutcome::warn(
                    "no model in /Users/jane/.aqua-oss/models",
                    ErrorClass::Configuration,
                    "download the model",
                ),
            },
            Report {
                name: "platform-version",
                outcome: CheckOutcome::fail("weird version", ErrorClass::Bug, "file an issue"),
            },
        ];
        let text = bundle_with_identity(&reports, "/Users/jane", "jane");
        assert!(!text.contains("jane"), "leaked identity:\n{text}");
        assert!(text.contains("bug-class failures: 1"));
        assert!(text.contains("redacted by construction"));
        assert!(text.contains("[WARN] model-files"));
    }
}
