//! Environmental diagnostics for the edit-by-voice spike.
//!
//! Almost every bug hit during M0 was environmental, not logical: TCC judging
//! the terminal instead of the binary, ad-hoc signatures silently revoking
//! grants on rebuild, windows hiding on another Space. Each presented as a
//! generic failure ("API error -25204") that cost hours to decode. This crate
//! exists so that class of failure surfaces as a *named next action* instead.
//!
//! The design has three legs:
//!
//! 1. [`Check`]: a probe that returns Pass/Warn/Fail plus a remedy string.
//!    The remedy rule is absolute: it must name the exact next action, never
//!    just restate the failure.
//! 2. [`ErrorClass`]: every failure is classified as Environment, Permission,
//!    Configuration, or Bug. Only `Bug` is worth a GitHub issue; the taxonomy
//!    keeps environmental noise out of the tracker.
//! 3. [`timing`] and [`redact`]: numeric regression detection and a bug-report
//!    bundler that strips everything the user typed, because this tool sees
//!    everything the user types.

/// The bundle identifier the shipped application is signed with.
///
/// This is duplicated in `scripts/bundle-outloud-macos.sh`, which is the file
/// that actually writes it into `Info.plist`; a shell script cannot read a
/// Rust constant, so the two are kept in step by a test in `checks.rs` that
/// parses the script.
///
/// It exists as a constant because the alternative already failed. The value
/// was written out by hand in the diagnostics, the remedy strings, and the
/// microphone probe. Two product renames later, those copies still said
/// `dev.aquaoss.spike` while the app shipped as `dev.hexavoice.hexad`, so the
/// doctor told users to run a `tccutil reset` against an identifier no
/// installed app had. `tccutil` prints "Successfully reset" for an unknown
/// identifier, so the advice looked like it worked and changed nothing.
pub const BUNDLE_ID: &str = "dev.outloud.outloud";

pub mod checks;
pub mod redact;
pub mod replay;
pub mod timing;

use std::collections::HashMap;
use std::fmt;

/// Who is responsible for a failure. This decides where the report goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// The machine or session is unsuitable (wrong OS version, no display,
    /// SSH session, missing hardware). Fix the environment, not the code.
    Environment,
    /// A permission the OS requires has not been granted, or was granted to
    /// the wrong responsible process. Fix in System Settings, not in code.
    Permission,
    /// The install is wrong (unbundled binary, ad-hoc signature, missing
    /// model files). Fix by reinstalling or re-running setup.
    Configuration,
    /// The code itself misbehaved. The only class worth a GitHub issue.
    Bug,
}

impl fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ErrorClass::Environment => "environment",
            ErrorClass::Permission => "permission",
            ErrorClass::Configuration => "configuration",
            ErrorClass::Bug => "bug",
        };
        write!(f, "{s}")
    }
}

impl ErrorClass {
    /// Whether this failure belongs in the issue tracker. Everything else is
    /// fixed on the user's machine, and filing it wastes both sides' time.
    pub fn worth_a_github_issue(&self) -> bool {
        matches!(self, ErrorClass::Bug)
    }
}

/// Classify an [`ax_edit::AxError`] into the taxonomy.
///
/// This is the bridge between the low-level API and the "should you file an
/// issue" question. Note `Api(code)` maps to `Bug`: by the time a raw code
/// escapes `ax-edit`'s own translation layer, it is genuinely unexpected.
pub fn classify_ax_error(err: &ax_edit::AxError) -> ErrorClass {
    use ax_edit::AxError::*;
    match err {
        NotTrusted => ErrorClass::Permission,
        // No focus / no text / read-only are properties of whatever app the
        // user is in, not of this tool. Environmental by definition.
        NoFocusedElement | NoTextValue | NotSettable => ErrorClass::Environment,
        Unsupported => ErrorClass::Environment,
        Api(_) => ErrorClass::Bug,
    }
}

/// Outcome severity of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Pass,
    /// Works, but a known trap is armed (e.g. ad-hoc signature: fine now,
    /// breaks on next rebuild).
    Warn,
    Fail,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        };
        write!(f, "{s}")
    }
}

/// Result of running one check.
#[derive(Debug, Clone)]
pub struct CheckOutcome {
    pub status: Status,
    /// What was observed, in one line. May contain paths/titles; the redactor
    /// scrubs it before it leaves the machine.
    pub detail: String,
    /// The exact next action. Required for Warn and Fail; the constructors
    /// enforce it so a lazy "permission denied" cannot be produced.
    pub remedy: Option<String>,
    /// Failure classification. Present exactly when status is not Pass.
    pub class: Option<ErrorClass>,
}

impl CheckOutcome {
    pub fn pass(detail: impl Into<String>) -> Self {
        CheckOutcome {
            status: Status::Pass,
            detail: detail.into(),
            remedy: None,
            class: None,
        }
    }
    pub fn warn(detail: impl Into<String>, class: ErrorClass, remedy: impl Into<String>) -> Self {
        CheckOutcome {
            status: Status::Warn,
            detail: detail.into(),
            remedy: Some(remedy.into()),
            class: Some(class),
        }
    }
    pub fn fail(detail: impl Into<String>, class: ErrorClass, remedy: impl Into<String>) -> Self {
        CheckOutcome {
            status: Status::Fail,
            detail: detail.into(),
            remedy: Some(remedy.into()),
            class: Some(class),
        }
    }
}

/// The environment a check reads, captured once so checks are deterministic
/// and unit-testable: tests hand in a synthetic env instead of mutating the
/// process's real one (which is racy and global).
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub vars: HashMap<String, String>,
}

impl Env {
    /// Snapshot the real process environment.
    pub fn capture() -> Self {
        Env {
            vars: std::env::vars().collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Env {
            vars: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

/// One diagnostic probe.
pub trait Check {
    /// Stable short name, used in report output.
    fn name(&self) -> &'static str;
    /// Run the probe. Must not panic and must not block for long: a doctor
    /// that hangs is worse than the failure it diagnoses.
    fn run(&self, env: &Env) -> CheckOutcome;
}

/// A named check result, as produced by [`run_all`].
pub struct Report {
    pub name: &'static str,
    pub outcome: CheckOutcome,
}

/// The full registry of checks, in the order a human should read them:
/// permissions first (the most common failure), then install identity, then
/// session/environment, then resources.
pub fn registry() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(checks::AccessibilityPermission),
        // Directly after Accessibility: they are adjacent in the user's mind,
        // both are required, and missing this one is indistinguishable from a
        // crash.
        Box::new(checks::InputMonitoringPermission),
        Box::new(checks::MicrophonePermission),
        Box::new(checks::CodeSignature),
        Box::new(checks::BundleLaunch),
        Box::new(checks::WindowVisibility),
        Box::new(checks::ChromiumOptIn),
        Box::new(checks::DisplayServer),
        Box::new(checks::TerminalEmulator),
        Box::new(checks::Clipboard),
        Box::new(checks::AudioInput),
        Box::new(checks::ModelFiles),
        Box::new(checks::DiskSpace),
        Box::new(checks::CpuFeatures),
        Box::new(checks::PlatformVersion),
    ]
}

/// Run every registered check against the given environment.
pub fn run_all(env: &Env) -> Vec<Report> {
    registry()
        .iter()
        .map(|c| Report {
            name: c.name(),
            outcome: c.run(env),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bug_class_deserves_an_issue() {
        assert!(ErrorClass::Bug.worth_a_github_issue());
        assert!(!ErrorClass::Environment.worth_a_github_issue());
        assert!(!ErrorClass::Permission.worth_a_github_issue());
        assert!(!ErrorClass::Configuration.worth_a_github_issue());
    }

    #[test]
    fn ax_errors_classify_sanely() {
        use ax_edit::AxError::*;
        assert_eq!(classify_ax_error(&NotTrusted), ErrorClass::Permission);
        assert_eq!(
            classify_ax_error(&NoFocusedElement),
            ErrorClass::Environment
        );
        assert_eq!(classify_ax_error(&NotSettable), ErrorClass::Environment);
        assert_eq!(classify_ax_error(&Unsupported), ErrorClass::Environment);
        // A raw code escaping ax-edit's translation means our mapping missed
        // a case: that IS our bug, so route it to the tracker.
        assert_eq!(classify_ax_error(&Api(-25200)), ErrorClass::Bug);
    }

    #[test]
    fn failures_always_carry_remedy_and_class() {
        let f = CheckOutcome::fail("x", ErrorClass::Permission, "do y");
        assert!(f.remedy.is_some());
        assert_eq!(f.class, Some(ErrorClass::Permission));
        let p = CheckOutcome::pass("ok");
        assert!(p.remedy.is_none() && p.class.is_none());
    }

    #[test]
    fn registry_is_nonempty_and_uniquely_named() {
        let reg = registry();
        assert!(reg.len() >= 14);
        let mut names: Vec<_> = reg.iter().map(|c| c.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), reg.len(), "duplicate check names");
    }
}
