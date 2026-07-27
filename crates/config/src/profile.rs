//! Per-application profiles: "the right output differs by destination"
//! (docs/ux/05) without making the user think about it.
//!
//! A profile is a matcher plus a set of key overrides. Matching is a pure
//! function of an [`AppIdentity`] snapshot, so every precedence rule here is
//! unit-testable without a window server.

use crate::schema::Value;
use std::collections::BTreeMap;

/// What we know about the frontmost application at dictation time. All
/// fields optional because platforms differ: macOS has bundle ids, Linux has
/// window classes, a headless SSH session has only a process name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppIdentity {
    /// macOS bundle identifier, e.g. "com.apple.Terminal".
    pub bundle_id: Option<String>,
    /// Executable name, e.g. "nvim".
    pub process_name: Option<String>,
    /// X11/Wayland window class, e.g. "Alacritty".
    pub window_class: Option<String>,
}

/// How a profile decides whether it applies. Specificity order, strongest
/// first: bundle id > process name > window class. Bundle ids are stable and
/// unique per app; process names collide (every Electron app is "Electron"
/// under some tools); window classes are the loosest, set by toolkits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    BundleId(String),
    ProcessName(String),
    WindowClass(String),
}

impl Matcher {
    /// Specificity rank, higher wins. Public so conflict reports can explain
    /// *why* one profile beat another in the same words the code uses.
    pub fn specificity(&self) -> u8 {
        match self {
            Matcher::BundleId(_) => 3,
            Matcher::ProcessName(_) => 2,
            Matcher::WindowClass(_) => 1,
        }
    }

    /// The kind name used in config files and error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Matcher::BundleId(_) => "bundle-id",
            Matcher::ProcessName(_) => "process-name",
            Matcher::WindowClass(_) => "window-class",
        }
    }

    pub fn pattern(&self) -> &str {
        match self {
            Matcher::BundleId(s) | Matcher::ProcessName(s) | Matcher::WindowClass(s) => s,
        }
    }

    /// Pure match test. Comparison is case-insensitive because bundle ids
    /// are case-insensitive in practice and window class casing varies by
    /// toolkit; a profile that fails on "Alacritty" vs "alacritty" would be
    /// a support ticket, not a feature.
    ///
    /// A trailing `*` makes the pattern a prefix match, so
    /// `bundle-id = "com.jetbrains.*"` covers every JetBrains IDE with one
    /// profile. That is the entire wildcard grammar on purpose: full globs
    /// invite unreadable matchers.
    pub fn matches(&self, app: &AppIdentity) -> bool {
        let field = match self {
            Matcher::BundleId(_) => &app.bundle_id,
            Matcher::ProcessName(_) => &app.process_name,
            Matcher::WindowClass(_) => &app.window_class,
        };
        let Some(actual) = field else { return false };
        let actual = actual.to_lowercase();
        let pattern = self.pattern().to_lowercase();
        match pattern.strip_suffix('*') {
            Some(prefix) => actual.starts_with(prefix),
            None => actual == pattern,
        }
    }
}

/// A named profile: matcher plus overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The `[profile.<name>]` table name; used in provenance and conflicts.
    pub name: String,
    pub matcher: Matcher,
    /// Key -> value overrides, keys from the main schema.
    pub overrides: BTreeMap<String, Value>,
}

/// Why a particular profile was selected over the others. Returned alongside
/// the winner so "which one wins and why" is answerable without reading
/// source code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinReason {
    OnlyMatch,
    /// Won on matcher kind (bundle id beats process name beats window class).
    MoreSpecificKind {
        beat: String,
    },
    /// Same kind; an exact pattern beats a `*` prefix pattern.
    ExactOverPrefix {
        beat: String,
    },
    /// Same kind and same exactness; a longer prefix beats a shorter one
    /// ("com.jetbrains.intellij*" over "com.jetbrains.*").
    LongerPrefix {
        beat: String,
    },
    /// Full tie; first in file order wins, deterministically, and the loser
    /// is named so the user can reorder or delete.
    FileOrder {
        beat: String,
    },
}

/// Select the winning profile for `app`, with the reason it won.
///
/// Precedence, documented and tested in this order:
/// 1. matcher kind specificity (bundle-id > process-name > window-class)
/// 2. exact pattern over `*` prefix pattern
/// 3. longer prefix over shorter
/// 4. file order (earlier wins)
///
/// Only the single winner applies; profiles do not stack. Stacking reads
/// nicely in a design doc and is undebuggable in a bug report, because the
/// effective value would come from an unbounded chain.
pub fn select<'a>(profiles: &'a [Profile], app: &AppIdentity) -> Option<(&'a Profile, WinReason)> {
    let mut matching = profiles.iter().filter(|p| p.matcher.matches(app));
    let mut winner = matching.next()?;
    let mut reason = WinReason::OnlyMatch;
    for contender in matching {
        match rank(contender).cmp(&rank(winner)) {
            std::cmp::Ordering::Greater => {
                reason = explain(contender, winner);
                winner = contender;
            }
            std::cmp::Ordering::Equal => {
                // Ties keep the earlier profile (file order) but record that
                // a tie happened so callers can surface it.
                if matches!(reason, WinReason::OnlyMatch) {
                    reason = WinReason::FileOrder {
                        beat: contender.name.clone(),
                    };
                }
            }
            std::cmp::Ordering::Less => {
                if matches!(reason, WinReason::OnlyMatch) {
                    reason = explain(winner, contender);
                }
            }
        }
    }
    Some((winner, reason))
}

/// Comparable rank tuple: (kind specificity, exactness, prefix length).
fn rank(p: &Profile) -> (u8, u8, usize) {
    let pat = p.matcher.pattern();
    let exact = u8::from(!pat.ends_with('*'));
    let prefix_len = pat.trim_end_matches('*').len();
    (p.matcher.specificity(), exact, prefix_len)
}

fn explain(winner: &Profile, loser: &Profile) -> WinReason {
    let (wk, we, wl) = rank(winner);
    let (lk, le, ll) = rank(loser);
    if wk != lk {
        WinReason::MoreSpecificKind {
            beat: loser.name.clone(),
        }
    } else if we != le {
        WinReason::ExactOverPrefix {
            beat: loser.name.clone(),
        }
    } else if wl != ll {
        WinReason::LongerPrefix {
            beat: loser.name.clone(),
        }
    } else {
        WinReason::FileOrder {
            beat: loser.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(bundle: Option<&str>, process: Option<&str>, class: Option<&str>) -> AppIdentity {
        AppIdentity {
            bundle_id: bundle.map(Into::into),
            process_name: process.map(Into::into),
            window_class: class.map(Into::into),
        }
    }

    fn profile(name: &str, matcher: Matcher) -> Profile {
        Profile {
            name: name.into(),
            matcher,
            overrides: BTreeMap::new(),
        }
    }

    #[test]
    fn matcher_is_pure_and_case_insensitive() {
        let m = Matcher::BundleId("com.apple.Terminal".into());
        assert!(m.matches(&app(Some("com.apple.terminal"), None, None)));
        assert!(!m.matches(&app(Some("com.googlecode.iterm2"), None, None)));
        // Missing field never matches; it must not panic or guess.
        assert!(!m.matches(&app(None, Some("Terminal"), None)));
    }

    #[test]
    fn prefix_wildcard_matches_prefix_only() {
        let m = Matcher::BundleId("com.jetbrains.*".into());
        assert!(m.matches(&app(Some("com.jetbrains.intellij"), None, None)));
        assert!(m.matches(&app(Some("com.jetbrains.CLion"), None, None)));
        assert!(!m.matches(&app(Some("org.jetbrains.compose"), None, None)));
    }

    #[test]
    fn each_matcher_kind_reads_its_own_field() {
        let by_proc = Matcher::ProcessName("nvim".into());
        let by_class = Matcher::WindowClass("Alacritty".into());
        let a = app(None, Some("nvim"), Some("alacritty"));
        assert!(by_proc.matches(&a));
        assert!(by_class.matches(&a));
        // A process-name pattern must not accidentally match a window class.
        assert!(!Matcher::ProcessName("alacritty".into()).matches(&app(
            None,
            None,
            Some("alacritty")
        )));
    }

    #[test]
    fn bundle_id_beats_process_name_beats_window_class() {
        let profiles = vec![
            profile("by-class", Matcher::WindowClass("alacritty".into())),
            profile("by-proc", Matcher::ProcessName("alacritty".into())),
            profile("by-bundle", Matcher::BundleId("org.alacritty".into())),
        ];
        let a = app(Some("org.alacritty"), Some("alacritty"), Some("Alacritty"));
        let (winner, reason) = select(&profiles, &a).unwrap();
        assert_eq!(winner.name, "by-bundle");
        assert!(matches!(reason, WinReason::MoreSpecificKind { .. }));

        let no_bundle = app(None, Some("alacritty"), Some("Alacritty"));
        let (winner, _) = select(&profiles, &no_bundle).unwrap();
        assert_eq!(winner.name, "by-proc");
    }

    #[test]
    fn exact_beats_prefix_and_longer_prefix_beats_shorter() {
        let profiles = vec![
            profile("all-jb", Matcher::BundleId("com.jetbrains.*".into())),
            profile("clion", Matcher::BundleId("com.jetbrains.clion".into())),
        ];
        let a = app(Some("com.jetbrains.clion"), None, None);
        let (winner, reason) = select(&profiles, &a).unwrap();
        assert_eq!(winner.name, "clion");
        assert!(matches!(reason, WinReason::ExactOverPrefix { .. }));

        let profiles = vec![
            profile("all-jb", Matcher::BundleId("com.jetbrains.*".into())),
            profile(
                "intellij-family",
                Matcher::BundleId("com.jetbrains.intellij*".into()),
            ),
        ];
        let a = app(Some("com.jetbrains.intellij.ce"), None, None);
        let (winner, reason) = select(&profiles, &a).unwrap();
        assert_eq!(winner.name, "intellij-family");
        assert!(matches!(reason, WinReason::LongerPrefix { .. }));
    }

    #[test]
    fn full_tie_falls_back_to_file_order_and_names_the_loser() {
        let profiles = vec![
            profile("first", Matcher::ProcessName("nvim".into())),
            profile("second", Matcher::ProcessName("nvim".into())),
        ];
        let a = app(None, Some("nvim"), None);
        let (winner, reason) = select(&profiles, &a).unwrap();
        assert_eq!(winner.name, "first");
        assert_eq!(
            reason,
            WinReason::FileOrder {
                beat: "second".into()
            }
        );
    }

    #[test]
    fn no_match_returns_none() {
        let profiles = vec![profile(
            "term",
            Matcher::BundleId("com.apple.terminal".into()),
        )];
        assert!(select(&profiles, &app(Some("com.apple.mail"), None, None)).is_none());
    }

    #[test]
    fn single_match_reports_only_match() {
        let profiles = vec![
            profile("term", Matcher::BundleId("com.apple.terminal".into())),
            profile("mail", Matcher::BundleId("com.apple.mail".into())),
        ];
        let (winner, reason) =
            select(&profiles, &app(Some("com.apple.terminal"), None, None)).unwrap();
        assert_eq!(winner.name, "term");
        assert_eq!(reason, WinReason::OnlyMatch);
    }
}
