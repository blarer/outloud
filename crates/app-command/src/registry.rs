//! The closed set of applications a spoken command may act on.
//!
//! Point 3 of the proposal: "a registry of permitted actions, declared, not
//! inferred. A command that is not in the registry cannot be performed,
//! however confident anything is about what the user meant."
//!
//! For launching, the registry is the set of **installed applications**,
//! discovered per platform. That is a declared allow-list in the sense that
//! matters: the command carries a registry entry, so a phrase naming
//! something not installed produces a refusal rather than a launch attempt.
//! The alternative, passing the recognizer's words to a shell, is the
//! injection hole this design exists to avoid.

use std::fmt;

/// One launchable application.
///
/// `name` is what the user says. `launch_id` is what the platform needs,
/// and the two differ in ways that matter: on Linux the user says
/// "Text Editor" and the launcher needs `org.gnome.TextEditor.desktop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// Human-facing name, as the user would speak it ("Firefox").
    pub name: String,
    /// Platform handle used to launch: a bundle path or name on macOS, a
    /// desktop-entry id on Linux, an executable or shell verb on Windows.
    pub launch_id: String,
}

impl fmt::Display for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// Every application a command may name.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    apps: Vec<App>,
}

impl Registry {
    /// Build a registry from an explicit list.
    ///
    /// Used by tests and by the platform discovery code in `launch`.
    /// Deliberately the only constructor: discovery lives behind the
    /// `Launcher` so that this crate stays pure and testable on every
    /// platform, including the ones it cannot enumerate.
    pub fn new(apps: Vec<App>) -> Self {
        Self { apps }
    }

    /// The applications in the registry.
    pub fn apps(&self) -> &[App] {
        &self.apps
    }

    /// Whether the registry has no applications, in which case every command
    /// refuses and the caller should say so rather than looking broken.
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// Resolve a spoken application name to a registry entry.
    ///
    /// Three passes, narrowing deliberately:
    ///
    /// 1. **Exact**, case-insensitive. "firefox" is Firefox.
    /// 2. **Word-prefix**, so "visual studio" finds "Visual Studio Code"
    ///    and "calc" finds "Calculator". Anchored at a word boundary rather
    ///    than anywhere in the string, because an unanchored substring makes
    ///    short spoken words match absurd things: "mail" would hit
    ///    "Thunderbird Mail Client" and "Mailspring" equally.
    /// 3. **Phonetic**, via `config::fuzzy`, for recognizer damage. That
    ///    module exists because the recognizer turns "kubectl" into "cube
    ///    cuddle", and application names are exactly the out-of-vocabulary
    ///    case the proposal flags as the biggest reliability risk.
    ///
    /// Returns [`Resolution::Ambiguous`] rather than picking a winner when
    /// more than one application is equally good. That is the refusal rule:
    /// "move to..." has no undo, and neither does the user's attention.
    pub fn resolve(&self, spoken: &str) -> Resolution<'_> {
        let spoken = spoken.trim();
        if spoken.is_empty() {
            return Resolution::NoMatch;
        }
        let lower = spoken.to_lowercase();

        // 1. Exact.
        let exact: Vec<&App> = self
            .apps
            .iter()
            .filter(|a| a.name.to_lowercase() == lower)
            .collect();
        if let Some(found) = single(&exact) {
            return found;
        }

        // 2. Word-prefix. "code" matches "Visual Studio Code" because one of
        //    its words starts with it; it does not match "Xcode", because
        //    "xcode" has no word beginning "code". That distinction is the
        //    whole reason this is word-anchored.
        let prefix: Vec<&App> = self
            .apps
            .iter()
            .filter(|a| {
                let n = a.name.to_lowercase();
                n.starts_with(&lower)
                    || n.split(|c: char| !c.is_alphanumeric())
                        .any(|w| !w.is_empty() && w.starts_with(&lower))
            })
            .collect();
        if let Some(found) = single(&prefix) {
            return found;
        }
        // Several apps start with the same word. Prefer an exact whole-name
        // match if one is among them (handled above), otherwise refuse:
        // "visual" with both "Visual Studio Code" and "Visual Paradigm"
        // installed is genuinely ambiguous and the user must say more.
        if prefix.len() > 1 {
            return Resolution::Ambiguous(prefix.into_iter().collect());
        }

        // 3. Phonetic, for recognizer damage. Thresholded high: this pass
        //    exists to absorb mis-hearings of a name the user meant, not to
        //    find the nearest application to an unrelated word. Below the
        //    threshold the answer is "no match", which the caller reports as
        //    "I do not know that application".
        //
        //    Candidates that merely CONTAIN the spoken text are excluded,
        //    and that exclusion is the whole reason this pass is safe.
        //    Substring relationships are pass 2's business, and pass 2 is
        //    deliberately word-anchored; letting a fuzzy score resurrect
        //    what the anchored pass just rejected would silently undo that
        //    decision. Concretely: `similarity("code", "xcode")` is 0.92,
        //    because one leading letter is a small edit and the phonetic
        //    codes agree, so without this rule "open code" opens Xcode on a
        //    machine where the user meant VS Code and simply does not have
        //    it installed. A mis-hearing looks different: "slaq" is not a
        //    substring of "Slack", so it still matches.
        const MIN_SIMILARITY: f64 = 0.72;
        let mut best: Vec<(&App, f64)> = self
            .apps
            .iter()
            .filter(|a| !a.name.to_lowercase().contains(&lower))
            .map(|a| (a, config::fuzzy::similarity(&lower, &a.name.to_lowercase())))
            .filter(|(_, s)| *s >= MIN_SIMILARITY)
            .collect();
        best.sort_by(|a, b| b.1.total_cmp(&a.1));
        match best.as_slice() {
            [] => Resolution::NoMatch,
            [(app, _)] => Resolution::One(app),
            // A clear winner is one meaningfully better than the runner-up.
            // Without this margin, two similarly-named applications would
            // resolve by floating-point luck.
            [(app, s0), (_, s1), ..] if s0 - s1 > 0.08 => Resolution::One(app),
            many => Resolution::Ambiguous(many.iter().map(|(a, _)| *a).collect()),
        }
    }
}

/// Collapse a candidate list of exactly one into a resolution.
fn single<'a>(cands: &[&'a App]) -> Option<Resolution<'a>> {
    match cands {
        [one] => Some(Resolution::One(one)),
        _ => None,
    }
}

/// The outcome of resolving a spoken name against the registry.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// Exactly one application matched.
    One(&'a App),
    /// Several matched equally well. The caller must refuse and name them,
    /// so the user can say which one rather than being guessed at.
    Ambiguous(Vec<&'a App>),
    /// Nothing matched closely enough to be worth acting on.
    NoMatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(names: &[&str]) -> Registry {
        Registry::new(
            names
                .iter()
                .map(|n| App {
                    name: (*n).to_string(),
                    launch_id: format!("{n}.app"),
                })
                .collect(),
        )
    }

    #[test]
    fn exact_match_is_case_insensitive() {
        let r = reg(&["Firefox", "Slack"]);
        assert!(matches!(r.resolve("firefox"), Resolution::One(a) if a.name == "Firefox"));
        assert!(matches!(r.resolve("FIREFOX"), Resolution::One(a) if a.name == "Firefox"));
    }

    #[test]
    fn word_prefix_finds_a_longer_name() {
        let r = reg(&["Visual Studio Code", "Slack"]);
        assert!(
            matches!(r.resolve("visual studio"), Resolution::One(a) if a.name == "Visual Studio Code")
        );
        // Matches on an interior word, which is how people refer to this app.
        assert!(matches!(r.resolve("code"), Resolution::One(a) if a.name == "Visual Studio Code"));
    }

    #[test]
    fn prefix_is_anchored_at_word_boundaries_not_anywhere() {
        // The bug this prevents: an unanchored substring search makes "code"
        // match "Xcode", so "open code" could open the wrong IDE.
        let r = reg(&["Xcode"]);
        assert_eq!(r.resolve("code"), Resolution::NoMatch);
    }

    #[test]
    fn two_apps_sharing_a_prefix_refuse_rather_than_guess() {
        let r = reg(&["Visual Studio Code", "Visual Paradigm"]);
        match r.resolve("visual") {
            Resolution::Ambiguous(apps) => assert_eq!(apps.len(), 2),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_exact_name_wins_over_being_another_apps_prefix() {
        // "Mail" is installed and is also a prefix of "Mailspring". The user
        // saying "mail" means the one actually called Mail.
        let r = reg(&["Mail", "Mailspring"]);
        assert!(matches!(r.resolve("mail"), Resolution::One(a) if a.name == "Mail"));
    }

    #[test]
    fn phonetic_pass_absorbs_recognizer_damage() {
        // The failure mode the proposal flags as the biggest risk: ASR
        // mangling an application name it has never seen.
        let r = reg(&["Slack", "Calculator"]);
        assert!(matches!(r.resolve("slaq"), Resolution::One(a) if a.name == "Slack"));
    }

    #[test]
    fn an_unrelated_word_matches_nothing() {
        // The threshold must not turn every utterance into the nearest app.
        let r = reg(&["Firefox", "Slack"]);
        assert_eq!(r.resolve("photosynthesis"), Resolution::NoMatch);
        assert_eq!(r.resolve("the quarterly report"), Resolution::NoMatch);
    }

    #[test]
    fn empty_input_and_empty_registry_refuse() {
        assert_eq!(reg(&["Firefox"]).resolve(""), Resolution::NoMatch);
        assert_eq!(reg(&["Firefox"]).resolve("   "), Resolution::NoMatch);
        assert_eq!(reg(&[]).resolve("firefox"), Resolution::NoMatch);
        assert!(reg(&[]).is_empty());
    }
}
