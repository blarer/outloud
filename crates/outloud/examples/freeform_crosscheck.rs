//! How the deterministic parser and the freeform classifier compose.
//!
//! Two layers guard the same hazard from different sides, and they are
//! owned by different parts of the codebase, so it is worth being able to
//! see the whole decision at once:
//!
//! 1. `edit_intent::parse` absorbs what it can into a deterministic
//!    command. Anything it handles never reaches layer 2 at all.
//! 2. `outloud::freeform::classify` decides what happens to the rest:
//!    dictate it, or refuse it visibly and write nothing.
//!
//! The property that matters is that no row can be BOTH "freeform" and
//! "dictated" while being an instruction about the selection, because
//! that combination is the reported corruption (the words describing the
//! edit replacing the text they described).
//!
//! ```text
//! cargo run -p outloud --example freeform_crosscheck
//! ```
//!
//! Pure: parses and classifies strings, touches no window, writes
//! nothing, and needs no accessibility grant.

use edit_intent::EditIntent;
use outloud::freeform::{classify, FreeformDisposition};

const SELECTED: &str = "The customers might possibly be quite upset about this.";

/// Instructions about the selection. Whatever layer catches them, the
/// user's text must survive.
const INSTRUCTIONS: &[&str] = &[
    "tighten this up",
    "make it more formal",
    "summarize this",
    "translate this to spanish",
    "make this shorter",
    "fix the grammar",
    "clean it up",
    "rephrase that",
    "turn this into bullet points",
];

/// Prose. Must be written, or the app looks like it stopped
/// transcribing.
const PROSE: &[&str] = &[
    "we should tell them soon",
    "fix the login bug and add tests",
    "the meeting is at three tomorrow afternoon",
    "this is just a normal sentence",
    "make sure the deploy happens today",
    "turn left at the second light",
];

fn main() {
    println!(
        "{:<45} {:<9} {:<8} disposition",
        "utterance", "freeform", "refused"
    );
    let mut wrong = 0;
    for (utterance, is_instruction) in INSTRUCTIONS
        .iter()
        .map(|u| (u, true))
        .chain(PROSE.iter().map(|u| (u, false)))
    {
        let intent = edit_intent::parse(utterance.trim_end_matches(['.', '!', '?', ',']));
        let freeform = matches!(intent, EditIntent::Freeform { .. });
        let refused = matches!(
            classify(utterance, SELECTED),
            FreeformDisposition::RewriteRequest { .. }
        );

        // "Written" means the transcript reaches the document. That
        // happens only when the parser punted AND the classifier said
        // dictate.
        let transcript_is_written = freeform && !refused;
        let ok = transcript_is_written != is_instruction;
        if !ok {
            wrong += 1;
        }

        let disposition = if !freeform {
            "parser handled it deterministically"
        } else if refused {
            "REFUSED, nothing written"
        } else {
            "dictated"
        };
        println!(
            "{:<45} {:<9} {:<8} {}{}",
            utterance,
            freeform,
            refused,
            disposition,
            if ok { "" } else { "   <-- WRONG" }
        );
    }
    println!("\nwrong: {wrong}");
    assert_eq!(wrong, 0, "a case landed on the wrong side of the line");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The composition property, as a test so it runs in CI rather than
    /// only when someone remembers to run the example.
    ///
    /// An instruction about the selection must never have its TRANSCRIPT
    /// written to the document. It may be absorbed by the parser (which
    /// writes a rewritten selection, not the command) or refused by the
    /// classifier (which writes nothing). What it may not be is dictated.
    #[test]
    fn an_instruction_transcript_is_never_written() {
        for utterance in INSTRUCTIONS {
            let freeform = matches!(
                edit_intent::parse(utterance.trim_end_matches(['.', '!', '?', ','])),
                EditIntent::Freeform { .. }
            );
            let dictated = matches!(
                classify(utterance, SELECTED),
                FreeformDisposition::Dictate { .. }
            );
            assert!(
                !(freeform && dictated),
                "{utterance:?} would be written into the document verbatim, \
                 replacing the text it was describing"
            );
        }
    }

    /// The mirror. Prose must reach the document: the parser must not
    /// hijack it into a command, and the classifier must not refuse it.
    #[test]
    fn prose_still_reaches_the_document() {
        for utterance in PROSE {
            let freeform = matches!(
                edit_intent::parse(utterance.trim_end_matches(['.', '!', '?', ','])),
                EditIntent::Freeform { .. }
            );
            let dictated = matches!(
                classify(utterance, SELECTED),
                FreeformDisposition::Dictate { .. }
            );
            assert!(
                freeform && dictated,
                "{utterance:?} would not be dictated (freeform={freeform}, \
                 dictated={dictated}); ordinary transcription regressed"
            );
        }
    }
}
