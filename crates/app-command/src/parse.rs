//! The grammar: two verbs, nothing else.
//!
//! Deliberately tiny. The proposal's point 2 argues for "a small enumerated
//! set parsed deterministically, with everything else refused out loud",
//! because the measured alternative was 10 silently wrong results in 55.
//!
//! # Why the verb must be at the start
//!
//! This is the single most important rule in the file, and it is the reason
//! `docs/competitive-analysis.md` item 14 declined the general case:
//!
//! > Aqua's version relies on an LLM deciding whether the user *meant* the
//! > command, which a suffix match cannot replicate safely: "tell them I'll
//! > send it tomorrow" would fire.
//!
//! The same trap exists here. A sentence someone dictates into a document
//! can easily contain the word "open":
//!
//! ```text
//! "the shop will open at nine"
//! "I'll open the attachment later"
//! "keep an open mind about Firefox"
//! ```
//!
//! None of those is a command, and all of them contain a command verb next
//! to a plausible noun. So the verb is matched **only at the start of the
//! utterance**, and the remainder must resolve to an installed application.
//! Both conditions, not either.
//!
//! That is still not sufficient on its own, which is why command mode is an
//! explicit mode (proposal point 1): "open Firefox" IS a sentence someone
//! could dictate. The grammar being anchored reduces the hazard; only the
//! mode removes it. Neither is redundant.

use crate::registry::{Registry, Resolution};
use crate::AppCommand;

/// Why a phrase did not become a command.
///
/// Every variant is something to **say to the user**, not a log line. A
/// refusal the user does not hear is indistinguishable from the feature
/// being broken, and leaves them repeating themselves at a machine that has
/// already decided not to act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No command verb at the start of the utterance. Almost always
    /// ordinary speech that reached the parser.
    NotACommand,
    /// A verb, but no application named after it ("open", "switch to").
    NoAppNamed,
    /// A verb and a name, but nothing installed matches.
    UnknownApp { spoken: String },
    /// Several installed applications match equally well.
    Ambiguous {
        spoken: String,
        candidates: Vec<String>,
    },
    /// The registry is empty, so no command could ever match. Distinguished
    /// from `UnknownApp` because the cause and the fix are different: this
    /// is a setup problem, not a mis-hearing.
    NoRegistry,
}

impl Refusal {
    /// A spoken-aloud explanation.
    ///
    /// Phrased as the system declining, never as the user erring: the
    /// recognizer is at least as likely to be at fault as the speaker.
    pub fn say(&self) -> String {
        match self {
            Self::NotACommand => "That is not a command I know.".into(),
            Self::NoAppNamed => "Which application?".into(),
            Self::UnknownApp { spoken } => {
                format!("I could not find an application called {spoken}.")
            }
            Self::Ambiguous { spoken, candidates } => {
                format!("{spoken} could be {}. Which one?", join_or(candidates))
            }
            Self::NoRegistry => "I have no list of applications, so I cannot open anything.".into(),
        }
    }
}

/// "a, b or c", for reading aloud.
fn join_or(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} or {b}"),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// Every accepted verb, longest first.
///
/// Longest-first matters: "switch to" must be tried before any future
/// "switch", or the shorter form would strip the verb and leave "to slack"
/// as the application name.
const VERBS: &[&str] = &[
    "switch to",
    "switch back to",
    "go to",
    "open",
    "launch",
    "start",
];

/// Parse a spoken phrase into a command, or explain why not.
///
/// The caller must already have decided the user is in command mode. This
/// function does not and cannot know that, and applying it to dictation
/// would be the ambient-command mistake the design rejects.
pub fn parse(utterance: &str, registry: &Registry) -> Result<AppCommand, Refusal> {
    // Normalize what the recognizer tends to add: trailing punctuation from
    // prose-tuned decoding, and filler politeness.
    let cleaned = utterance
        .trim()
        .trim_end_matches(['.', '!', '?', ','])
        .trim();
    let lower = cleaned.to_lowercase();
    let lower = lower
        .strip_prefix("please ")
        .unwrap_or(&lower)
        .trim()
        .to_string();

    // Sort verbs by length so that a longer phrase wins over a prefix of
    // itself. Done here rather than by hand-ordering the constant, because a
    // hand-ordered list is one careless insertion away from being wrong.
    let mut verbs: Vec<&str> = VERBS.to_vec();
    verbs.sort_by_key(|v| std::cmp::Reverse(v.len()));

    let rest = verbs.iter().find_map(|verb| {
        // Anchored at the START. See the module comment: a verb anywhere
        // else is overwhelmingly likely to be ordinary speech.
        let r = lower.strip_prefix(verb)?;
        // The verb must be a whole word: "opening the door" must not parse
        // as the verb "open" plus "ing the door". Requiring a space (or end
        // of input) after the verb is what enforces that.
        if r.is_empty() || r.starts_with(' ') {
            Some(r.trim())
        } else {
            None
        }
    });

    let Some(rest) = rest else {
        return Err(Refusal::NotACommand);
    };

    // Strip a leading article, which people say and recognizers transcribe:
    // "open the calculator".
    let name = rest
        .strip_prefix("the ")
        .or_else(|| rest.strip_prefix("a "))
        .unwrap_or(rest)
        .trim();

    // A bare article is not an application name. Without this, "open the"
    // asked the registry to resolve "the", which reported
    // `UnknownApp { spoken: "the" }` and would have said "I could not find
    // an application called the." The user said something incomplete, so the
    // useful answer is "Which application?".
    if name.is_empty() || matches!(name, "the" | "a" | "an") {
        return Err(Refusal::NoAppNamed);
    }
    // Checked AFTER the verb parse so that ordinary speech still reports
    // "not a command" rather than a confusing setup error.
    if registry.is_empty() {
        return Err(Refusal::NoRegistry);
    }

    match registry.resolve(name) {
        Resolution::One(app) => Ok(AppCommand::Launch(app.clone())),
        Resolution::Ambiguous(apps) => Err(Refusal::Ambiguous {
            spoken: name.to_string(),
            candidates: apps.iter().map(|a| a.name.clone()).collect(),
        }),
        Resolution::NoMatch => Err(Refusal::UnknownApp {
            spoken: name.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::App;

    fn reg(names: &[&str]) -> Registry {
        Registry::new(
            names
                .iter()
                .map(|n| App {
                    name: (*n).to_string(),
                    launch_id: format!("/Applications/{n}.app"),
                })
                .collect(),
        )
    }

    fn launched(utterance: &str, r: &Registry) -> String {
        match parse(utterance, r) {
            Ok(AppCommand::Launch(app)) => app.name,
            other => panic!("expected a launch for {utterance:?}, got {other:?}"),
        }
    }

    #[test]
    fn every_verb_launches() {
        let r = reg(&["Firefox"]);
        for phrase in [
            "open Firefox",
            "launch Firefox",
            "start Firefox",
            "switch to Firefox",
            "go to Firefox",
            "switch back to Firefox",
        ] {
            assert_eq!(launched(phrase, &r), "Firefox", "failed on {phrase:?}");
        }
    }

    #[test]
    fn recognizer_noise_is_tolerated() {
        let r = reg(&["Firefox"]);
        // Prose-tuned decoding adds a full stop; people add politeness.
        assert_eq!(launched("Open Firefox.", &r), "Firefox");
        assert_eq!(launched("please open firefox", &r), "Firefox");
        assert_eq!(launched("  open   Firefox  ", &r), "Firefox");
        assert_eq!(launched("open the Firefox", &r), "Firefox");
    }

    // --- the safety-critical cases ---------------------------------------
    //
    // Each of these is a sentence a user could plausibly dictate. If any of
    // them parses, dictation can silently launch applications.

    #[test]
    fn a_verb_in_the_middle_of_a_sentence_is_not_a_command() {
        let r = reg(&["Firefox", "Mail"]);
        for prose in [
            "the shop will open at nine",
            "I'll open the attachment later",
            "keep an open mind about Firefox",
            "tell them I'll start Mail tomorrow",
            "we should switch to Firefox next quarter",
            "did you open Firefox",
        ] {
            assert_eq!(
                parse(prose, &r),
                Err(Refusal::NotACommand),
                "DANGEROUS: dictation parsed as a command: {prose:?}"
            );
        }
    }

    #[test]
    fn a_verb_that_is_only_a_word_prefix_is_not_a_command() {
        // "opening" starts with "open" but is not the verb.
        let r = reg(&["Firefox"]);
        assert_eq!(
            parse("opening Firefox was a mistake", &r),
            Err(Refusal::NotACommand)
        );
        assert_eq!(parse("starting Firefox now", &r), Err(Refusal::NotACommand));
    }

    #[test]
    fn longest_verb_wins_so_the_app_name_survives() {
        // The bug: matching "switch" before "switch to" leaves "to slack".
        let r = reg(&["Slack"]);
        assert_eq!(launched("switch to Slack", &r), "Slack");
    }

    #[test]
    fn a_verb_with_no_app_asks_which_one() {
        let r = reg(&["Firefox"]);
        assert_eq!(parse("open", &r), Err(Refusal::NoAppNamed));
        assert_eq!(parse("switch to", &r), Err(Refusal::NoAppNamed));
        assert_eq!(parse("open the", &r), Err(Refusal::NoAppNamed));
    }

    #[test]
    fn an_uninstalled_app_refuses_and_names_what_it_heard() {
        let r = reg(&["Firefox"]);
        match parse("open Photoshop", &r) {
            Err(Refusal::UnknownApp { spoken }) => assert_eq!(spoken, "photoshop"),
            other => panic!("expected UnknownApp, got {other:?}"),
        }
    }

    #[test]
    fn ambiguity_refuses_and_offers_the_candidates() {
        let r = reg(&["Visual Studio Code", "Visual Paradigm"]);
        match parse("open visual", &r) {
            Err(Refusal::Ambiguous { candidates, .. }) => {
                assert_eq!(candidates.len(), 2);
                // The spoken refusal must actually name them, or the user
                // has no way to disambiguate.
                let said = Refusal::Ambiguous {
                    spoken: "visual".into(),
                    candidates,
                }
                .say();
                assert!(said.contains("Visual Studio Code"), "got: {said}");
                assert!(said.contains("Visual Paradigm"), "got: {said}");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_registry_says_so_rather_than_blaming_the_user() {
        let r = reg(&[]);
        assert_eq!(parse("open Firefox", &r), Err(Refusal::NoRegistry));
        // But ordinary speech still reports the ordinary refusal.
        assert_eq!(parse("the shop will open", &r), Err(Refusal::NotACommand));
    }

    #[test]
    fn every_refusal_says_something_useful() {
        // A refusal the user cannot hear is indistinguishable from a bug.
        for r in [
            Refusal::NotACommand,
            Refusal::NoAppNamed,
            Refusal::UnknownApp { spoken: "x".into() },
            Refusal::Ambiguous {
                spoken: "x".into(),
                candidates: vec!["A".into(), "B".into()],
            },
            Refusal::NoRegistry,
        ] {
            let said = r.say();
            assert!(!said.is_empty(), "{r:?} says nothing");
            assert!(
                said.ends_with(['.', '?']),
                "{r:?} is not a sentence: {said}"
            );
        }
    }

    #[test]
    fn join_or_reads_naturally() {
        assert_eq!(join_or(&["A".into()]), "A");
        assert_eq!(join_or(&["A".into(), "B".into()]), "A or B");
        assert_eq!(join_or(&["A".into(), "B".into(), "C".into()]), "A, B or C");
    }
}
