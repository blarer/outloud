//! False-refusal / destructive-miss rates for the freeform classifier,
//! measured over a corpus built to be adversarial in BOTH directions.
//!
//! The classifier trades one error for the other on purpose (a wrong
//! refusal costs one retry, a wrong overwrite costs a paragraph), so the
//! only honest way to report it is to measure both rates rather than
//! assert the happy cases. This corpus is therefore deliberately nasty:
//!
//! - the prose all STARTS WITH a verb from the ambiguous list, which is
//!   the exact shape most likely to be misread as an instruction;
//! - the instructions are terse real phrasings, several without the
//!   pronoun the corroboration rule leans on.
//!
//! ```text
//! cargo run -p outloud --example freeform_stress
//! ```
//!
//! Pure: no window, no transport, no accessibility grant.
//!
//! Caveat worth stating plainly: this corpus is a construction, not
//! observed user traffic, so the RATES describe this corpus. What it can
//! honestly establish is the presence or absence of destructive misses,
//! which is the failure mode that cannot be tolerated at all.

use edit_intent::EditIntent;
use outloud::freeform::{classify, FreeformDisposition};

const SELECTION: &str = "It is really quite important that we ship today.";

/// Dictation that opens with a verb from the ambiguous list. Every one of
/// these must still be written; a refusal here is the regression the old
/// blanket-insert behaviour existed to prevent.
const PROSE: &[&str] = &[
    "make sure you lock the door before you leave",
    "make it happen by Friday please",
    "turn the volume down a bit",
    "fix that leak in the kitchen sink this weekend",
    "clean the garage on Saturday",
    "improve our onboarding numbers this quarter",
    "trim the hedges before the guests arrive",
    "correct me if I am wrong but the meeting moved",
    "edit the video and send it to Sam",
    "revise the budget with the new headcount",
    "adjust the thermostat to sixty eight",
    "clarify with legal whether we can ship this",
    "soften the lighting in the studio",
    "strengthen the mounting bracket with another bolt",
    "polish the silver before the dinner",
    "turn left at the second light and park behind the building",
    "fix the login bug and add tests",
    "we should tell them soon",
    "the meeting is at three tomorrow afternoon",
];

/// Instructions about the selection. Any one of these being WRITTEN is a
/// destructive miss: the user's selected text is replaced by the words
/// describing what they wanted done to it.
const INSTRUCTIONS: &[&str] = &[
    "make this more concise",
    "make it sound friendlier",
    "clean this up a bit",
    "fix these typos",
    "improve the phrasing here",
    "summarize the above",
    "rewrite this",
    "reword that please",
    "shorten this",
    "proofread it",
    "make that more formal",
    "fix the punctuation",
    "correct the spelling",
    "trim this down",
    "tighten this up",
];

/// Would the TRANSCRIPT itself be written into the document? True only
/// when the deterministic parser punted AND the classifier said dictate.
fn transcript_reaches_the_document(utterance: &str) -> bool {
    let freeform = matches!(
        edit_intent::parse(utterance.trim_end_matches(['.', '!', '?', ','])),
        EditIntent::Freeform { .. }
    );
    let dictated = matches!(
        classify(utterance, SELECTION),
        FreeformDisposition::Dictate { .. }
    );
    freeform && dictated
}

fn main() {
    let mut false_refusals = Vec::new();
    for p in PROSE {
        if !transcript_reaches_the_document(p) {
            false_refusals.push(*p);
        }
    }
    let mut destructive_misses = Vec::new();
    for i in INSTRUCTIONS {
        if transcript_reaches_the_document(i) {
            destructive_misses.push(*i);
        }
    }

    println!("prose n={}", PROSE.len());
    for p in &false_refusals {
        println!("  FALSE REFUSAL (costs one retry): {p}");
    }
    println!(
        "  false-refusal rate: {}/{}",
        false_refusals.len(),
        PROSE.len()
    );

    println!("instructions n={}", INSTRUCTIONS.len());
    for i in &destructive_misses {
        println!("  DESTRUCTIVE MISS (costs a paragraph): {i}");
    }
    println!(
        "  destructive-miss rate: {}/{}",
        destructive_misses.len(),
        INSTRUCTIONS.len()
    );

    assert!(
        destructive_misses.is_empty(),
        "a destructive miss is not a tolerable error rate; it silently loses the user's text"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The asymmetry, enforced. Destructive misses must be ZERO; false
    /// refusals are permitted but bounded, so a future tightening of the
    /// verb lists cannot quietly turn dictation off.
    #[test]
    fn no_instruction_is_ever_written_into_the_document() {
        let misses: Vec<_> = INSTRUCTIONS
            .iter()
            .filter(|i| transcript_reaches_the_document(i))
            .collect();
        assert!(
            misses.is_empty(),
            "these instructions would replace the user's selection with themselves: {misses:?}"
        );
    }

    #[test]
    fn false_refusals_stay_rare_on_verb_initial_prose() {
        let refusals: Vec<_> = PROSE
            .iter()
            .filter(|p| !transcript_reaches_the_document(p))
            .collect();
        // This corpus is built so that almost every sentence STARTS with
        // an ambiguous verb, which is the hardest case there is. A couple
        // of refusals is acceptable (one retry each); a majority would
        // mean dictation had effectively stopped working.
        assert!(
            refusals.len() * 4 <= PROSE.len(),
            "{}/{} verb-initial sentences were refused, which is the \
             \"app stopped transcribing\" regression: {refusals:?}",
            refusals.len(),
            PROSE.len()
        );
    }
}
