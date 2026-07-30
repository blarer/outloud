//! The spoken grammar: utterance in, [`EditIntent`] out.
//!
//! Two rules govern everything here, and they pull in opposite directions.
//!
//! **Recognise more.** An utterance that does not parse becomes
//! [`EditIntent::Freeform`], which needs a language model that may not be
//! present. Every phrasing recognised here is an edit that happens instantly
//! and predictably instead.
//!
//! **Refuse when unsure.** A phrasing that could plausibly mean two
//! different edits must NOT parse. Silently performing the wrong edit is
//! strictly worse than declining, because the user sees a confident result
//! and has to notice the damage themselves. Every place this file returns
//! `None` on a phrase it *almost* understood is deliberate, and says why.
//!
//! The grammar is prefix- and keyword-anchored rather than regex- or
//! model-driven so that reading a rule tells you exactly which utterances it
//! claims, which is the property that makes the "refuse when unsure" rule
//! auditable at all.

use crate::{Anchor, Case, EditIntent, IdentCase, ListOp, Scope, TextUnit, UndoDepth, Which};

/// Spoken names for punctuation, longest-first so "exclamation mark" is not
/// shadowed by a shorter entry that happens to be a prefix of it.
const MARKS: &[(&str, char)] = &[
    ("exclamation mark", '!'),
    ("exclamation point", '!'),
    ("question mark", '?'),
    ("full stop", '.'),
    ("semicolon", ';'),
    ("semi colon", ';'),
    ("period", '.'),
    ("comma", ','),
    ("colon", ':'),
];

/// Words that name a position within the text.
const ORDINALS: &[(&str, Which)] = &[
    ("first", Which::Nth(0)),
    ("second", Which::Nth(1)),
    ("third", Which::Nth(2)),
    ("fourth", Which::Nth(3)),
    ("fifth", Which::Nth(4)),
    ("last", Which::Last),
    ("final", Which::Last),
    // "this sentence" spoken at a selection means the sentence in hand.
    // With no better information the last one is the one being spoken
    // about, which is also what it degenerates to when the selection holds
    // exactly one sentence: the common case.
    ("this", Which::Last),
    ("that", Which::Last),
];

const UNITS: &[(&str, TextUnit)] = &[
    ("sentences", TextUnit::Sentence),
    ("sentence", TextUnit::Sentence),
    ("paragraphs", TextUnit::Paragraph),
    ("paragraph", TextUnit::Paragraph),
    ("lines", TextUnit::Line),
    ("line", TextUnit::Line),
    ("words", TextUnit::Word),
    ("word", TextUnit::Word),
    ("letter", TextUnit::Character),
    ("character", TextUnit::Character),
    ("char", TextUnit::Character),
];

/// Number words that would change the meaning of a command we would
/// otherwise honour. "split this into two lines" names a count we cannot
/// possibly satisfy from a sentence splitter, so the whole command is
/// refused rather than answered with a different number of lines.
const COUNT_WORDS: &[&str] = &[
    "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];

/// Tokenise on whitespace, dropping the punctuation a recogniser adds so
/// "in the last sentence, change its to it's" tokenises the same way the
/// comma-free version does.
fn tokens(phrase: &str) -> Vec<&str> {
    phrase
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| matches!(c, ',' | '.' | '!' | '?' | ';' | ':')))
        .filter(|t| !t.is_empty())
        .collect()
}

/// Read a scope phrase from the START of `tokens`, returning the scope and
/// how many tokens it consumed.
///
/// Accepts `[the] (first|second|..|last|this) (sentence|word|line|...)`.
/// Anything else consumes nothing, so callers can cheaply ask "does this
/// begin with a scope?" without a second grammar.
fn leading_scope(tokens: &[&str]) -> Option<(Scope, usize)> {
    let mut i = 0;
    if tokens.get(i) == Some(&"the") {
        i += 1;
    }
    let which = ORDINALS
        .iter()
        .find(|(word, _)| tokens.get(i) == Some(word))
        .map(|(_, w)| *w)?;
    i += 1;
    // "next to last" and friends are real English but rare enough that
    // guessing at them is not worth the chance of getting one wrong.
    let unit = UNITS
        .iter()
        .find(|(word, _)| tokens.get(i) == Some(word))
        .map(|(_, u)| *u)?;
    i += 1;
    Some((Scope::Unit { which, unit }, i))
}

/// A scope phrase anywhere in the utterance, used by commands whose scope
/// does not lead ("make the first line title case").
///
/// Returns `None` when there is more than one, because "make the first word
/// of the last sentence title case" names a scope this grammar cannot
/// resolve and must not half-resolve.
///
/// Scanning skips past a match rather than advancing one token, because
/// "the first letter" contains "first letter" and would otherwise count as
/// two overlapping scopes and refuse a command it understands perfectly.
fn only_scope(tokens: &[&str]) -> Option<Scope> {
    let mut found = None;
    let mut i = 0;
    while i < tokens.len() {
        if let Some((scope, used)) = leading_scope(&tokens[i..]) {
            if found.is_some() {
                return None;
            }
            found = Some(scope);
            i += used;
        } else {
            i += 1;
        }
    }
    found
}

/// Does the utterance name a scope we did not consume? If so the command
/// must be refused: honouring the verb while ignoring the scope is exactly
/// the failure that made `uppercase the first letter` shout the whole field.
///
/// Requires BOTH a position word and a unit word, because a unit word alone
/// is often just vocabulary: "sentence case please" names no scope at all,
/// and refusing it would break a command that has always worked.
fn mentions_unconsumed_scope(tokens: &[&str]) -> bool {
    let names_unit = tokens.iter().any(|t| UNITS.iter().any(|(w, _)| w == t));
    let names_position = tokens
        .iter()
        .any(|t| ORDINALS.iter().any(|(w, _)| w == t) && *t != "this" && *t != "that");
    names_unit && names_position
}

/// Punctuation named in the utterance, if exactly one is.
///
/// Exactly one, because "change the period to a comma" names two and means
/// something this grammar does not implement.
///
/// Matches are blanked out of a working copy as they are found, so
/// "semicolon" does not also register as the "colon" it literally contains.
/// The table is longest-first for the same reason.
fn only_mark(lower: &str) -> Option<char> {
    let mut remaining = lower.to_string();
    let mut found = None;
    for (name, ch) in MARKS {
        while let Some(at) = remaining.find(name) {
            if found.is_some_and(|f| f != *ch) {
                return None;
            }
            found = Some(*ch);
            remaining.replace_range(at..at + name.len(), " ");
        }
    }
    found
}

/// What the extended grammar concluded about an utterance.
///
/// The middle variant is the one that matters. The original literal verbs
/// are greedy (`delete ` plus anything is a valid `Delete`), so a rule that
/// merely returns "not mine" on a phrase it recognised-but-distrusted would
/// hand that phrase straight to a literal reading, which is the outcome the
/// refusal existed to prevent. [`Decision::Refuse`] stops the utterance here
/// and sends it to [`crate::EditIntent::Freeform`] instead, where the
/// delivery path's safety rules apply.
pub(crate) enum Decision {
    Intent(EditIntent),
    Refuse,
    /// Not one of the extended shapes; try the literal verbs.
    Pass,
}

impl Decision {
    /// `Some(intent)` short-circuits, `None` continues to the next rule.
    fn from_rule(rule: Option<EditIntent>) -> Option<Self> {
        rule.map(Decision::Intent)
    }
}

/// The entry point. See [`crate::parse`] for the contract.
pub(crate) fn parse(trimmed: &str, lower: &str) -> Decision {
    let toks = tokens(lower);
    if toks.is_empty() {
        return Decision::Pass;
    }

    // Undo first: these are short, fixed phrases, and letting any later rule
    // see them risks a literal reading ("scratch that" deleting every
    // occurrence of the word "that", which is what the original grammar did).
    if let Some(depth) = parse_undo(&toks) {
        return Decision::Intent(EditIntent::Undo(depth));
    }

    // Each rule answers `None` for "not my shape" and is tried in turn.
    // Order matters only where two rules could both claim an utterance;
    // `parse_delete_unit` precedes `parse_delete_mark` because "delete the
    // last sentence" must not be read as a search for a mark.
    let rules: [&dyn Fn() -> Option<Decision>; 10] = [
        &|| parse_scoped_prefix(trimmed, &toks),
        &|| parse_delete_unit(&toks),
        &|| parse_punctuation(lower, &toks),
        &|| Decision::from_rule(parse_delete_mark(&toks)),
        &|| parse_wrap(lower, &toks),
        &|| parse_identifier_case(lower, &toks),
        &|| Decision::from_rule(parse_list_op(lower, &toks)),
        &|| parse_scoped_recase(lower, &toks),
        &|| unresolved_mark(lower, &toks),
        &|| ordinal_occurrence(&toks),
    ];
    rules
        .iter()
        .find_map(|rule| rule())
        .unwrap_or(Decision::Pass)
}

/// Last line of defence: an utterance that NAMES a punctuation mark but was
/// claimed by none of the rules above.
///
/// "remove the comma" against text holding three of them, and "change the
/// period to a comma", both land here. Left alone they reach the literal
/// verbs, which read them as a search for the words "the comma" and a
/// replacement of the words "the period" - confident edits of text that has
/// nothing to do with what was asked. Refusing sends them to the safety
/// path instead.
///
/// Deliberately narrow: it only fires when a mark is named by NAME, so
/// ordinary prose containing the word "comma" as content is untouched
/// unless it is also shaped like a command.
fn unresolved_mark(lower: &str, toks: &[&str]) -> Option<Decision> {
    const VERBS: &[&str] = &[
        "add", "put", "insert", "delete", "remove", "change", "replace", "swap", "move", "make",
        "get",
    ];
    if !VERBS.contains(toks.first()?) {
        return None;
    }
    // `only_mark` returning None on a text that mentions one means it
    // mentioned two, which is likewise a command this grammar lacks.
    let names_a_mark = MARKS.iter().any(|(name, _)| lower.contains(name));
    names_a_mark.then_some(Decision::Refuse)
}

/// "change the second the to a": an ordinal that picks one OCCURRENCE of a
/// word rather than a unit of text.
///
/// `docs/ux/03-edit-by-voice.md` promises this eventually ("change the
/// second teh to the"), and this grammar does not implement it. The literal
/// verb would read the operand as the phrase "the second the" and search for
/// it as text, which on some inputs finds a match and edits the wrong place.
/// Refuse until the feature exists.
///
/// The definite article is REQUIRED, and that is what keeps this rule from
/// eating ordinary commands. "change second to 2nd" is someone replacing the
/// word "second"; "change the second X to Y" is ordinal targeting. Without
/// the article test this rule refused the former, which a workspace pipeline
/// test caught.
fn ordinal_occurrence(toks: &[&str]) -> Option<Decision> {
    const VERBS: &[&str] = &[
        "change",
        "replace",
        "delete",
        "remove",
        "swap",
        "capitalize",
    ];
    if !VERBS.contains(toks.first()?) {
        return None;
    }
    let rest = toks.get(1..)?.strip_prefix(&["the"][..])?;
    let leads_with_ordinal = rest.first().is_some_and(|t| {
        ORDINALS
            .iter()
            .any(|(w, _)| w == t && *w != "this" && *w != "that")
    });
    // A leading ordinal followed by a UNIT was already handled above, so
    // anything reaching here names an occurrence.
    leads_with_ordinal.then_some(Decision::Refuse)
}

fn parse_undo(toks: &[&str]) -> Option<UndoDepth> {
    match toks.join(" ").as_str() {
        // "scratch that" is a dictation convention for undo, not a request
        // to delete the word "that". The original grammar read it
        // literally, which deletes every "that" in the field.
        "undo" | "undo that" | "undo the last edit" | "never mind" | "nevermind"
        | "scratch that" | "scratch this" => Some(UndoDepth::One),
        "undo everything" | "go back to the original" | "revert everything" | "start over" => {
            Some(UndoDepth::All)
        }
        _ => None,
    }
}

/// "in the last sentence, change its to it's": a scope that narrows an
/// otherwise ordinary command.
///
/// The inner command is parsed by the full grammar, and anything it cannot
/// execute is refused rather than approximated.
fn parse_scoped_prefix(trimmed: &str, toks: &[&str]) -> Option<Decision> {
    let rest = match toks.first()? {
        &"in" | &"within" | &"inside" => &toks[1..],
        _ => return None,
    };
    let (scope, used) = leading_scope(rest)?;
    let tail = &rest[used..];
    if tail.is_empty() {
        return Some(Decision::Refuse);
    }
    // Re-slice the ORIGINAL text rather than rejoining lowercased tokens,
    // so "in the last sentence change its to It's" keeps the user's casing
    // in the replacement. Falling back to the lowercased tokens would
    // silently downcase the replacement, which is a wrong edit rather than a
    // refusal, so it is not an acceptable fallback.
    let inner = tail_of_original(trimmed, toks.len(), tail.len())?;
    match crate::parse(&inner) {
        // A scope wrapped around an instruction we cannot execute is not
        // progress; it moves the guess one level down.
        EditIntent::Freeform { .. } => Some(Decision::Refuse),
        // Nesting a scope inside a scope has no agreed meaning.
        EditIntent::Scoped { .. } => Some(Decision::Refuse),
        other => Some(Decision::Intent(EditIntent::Scoped {
            scope,
            intent: Box::new(other),
        })),
    }
}

/// The last `tail` whitespace-separated words of the ORIGINAL utterance.
///
/// Tokenisation happens on a lowercased copy, and `to_lowercase` does not
/// preserve byte lengths, so offsets cannot cross between the two. Counting
/// words backwards from the end does, because case folding never inserts or
/// removes whitespace.
///
/// `total` is the tokeniser's count, and it is checked against the original's
/// word count rather than assumed equal: [`tokens`] drops tokens that become
/// empty after punctuation is stripped, so an utterance containing a stray
/// standalone comma has fewer tokens than words. When the two disagree the
/// alignment is not trustworthy and the caller must refuse.
fn tail_of_original(original: &str, total: usize, tail: usize) -> Option<String> {
    let words: Vec<&str> = original.split_whitespace().collect();
    if words.len() != total || tail > words.len() {
        return None;
    }
    Some(words[words.len() - tail..].join(" "))
}

/// "delete the last sentence", "delete everything".
fn parse_delete_unit(toks: &[&str]) -> Option<Decision> {
    const HEADS: &[&[&str]] = &[
        &["delete"],
        &["remove"],
        &["get", "rid", "of"],
        &["cut"],
        &["erase"],
    ];
    let rest = HEADS
        .iter()
        .find_map(|head| toks.strip_prefix(*head))
        .filter(|rest| !rest.is_empty())?;

    if let Some((scope, used)) = leading_scope(rest) {
        // Trailing words we did not understand ("delete the last sentence
        // and the one before it") change the meaning. Refuse rather than
        // fall through, or the literal verb would search for the whole
        // phrase as text.
        if used != rest.len() {
            return Some(Decision::Refuse);
        }
        return Some(Decision::Intent(EditIntent::DeleteScope(scope)));
    }
    // Whole-target deletes. Bare "delete this" is deliberately NOT here: it
    // is also a literal request to remove the word "this", and the literal
    // reading fails safe (no match, nothing written) where this one wipes
    // the selection.
    match rest.join(" ").as_str() {
        "everything" | "all of this" | "all of it" | "the whole thing" => {
            Some(Decision::Intent(EditIntent::DeleteScope(Scope::Whole)))
        }
        _ => None,
    }
}

/// "add a period at the end", "put a comma after today".
fn parse_punctuation(lower: &str, toks: &[&str]) -> Option<Decision> {
    // "at" is not a synonym for "add"; it is what the recognizer returns
    // for it. Measured on this machine, "add a period at the end" comes
    // back as "At a period at the end." on every run, across two voices.
    // Rejecting the head word there does not produce a refusal, it
    // produces a Freeform, and a Freeform with a selection live used to
    // overwrite the user's text with the words of their own command.
    //
    // Safe to accept because the rest of the rule still has to find a
    // punctuation mark and a placement it can compute; "at the office"
    // matches no mark and falls through untouched.
    const HEADS: &[&str] = &["add", "at", "put", "insert", "stick", "append"];
    if !HEADS.contains(toks.first()?) {
        return None;
    }
    let mark = only_mark(lower)?;

    // Placements this rule cannot compute must be refused, not silently
    // demoted to "at the end". "add a comma before customers" put the
    // comma at the far end of the field, which is a confident wrong edit
    // of exactly the kind this whole grammar exists to eliminate.
    const UNSUPPORTED_PLACEMENTS: &[&str] = &["before", "between", "around", "each", "every"];
    if toks.iter().any(|t| UNSUPPORTED_PLACEMENTS.contains(t)) {
        return Some(Decision::Refuse);
    }

    if let Some(idx) = toks.iter().position(|t| *t == "after") {
        let anchor: Vec<&str> = toks[idx + 1..].to_vec();
        // "after the word today" and "after today" mean the same thing.
        let anchor = anchor
            .strip_prefix(&["the", "word"][..])
            .or_else(|| anchor.strip_prefix(&["the"][..]))
            .unwrap_or(&anchor);
        if anchor.is_empty() {
            return Some(Decision::Refuse);
        }
        // "add a comma after the second sentence" names a scope this rule
        // does not resolve; the anchor would become the literal words.
        if mentions_unconsumed_scope(anchor) {
            return Some(Decision::Refuse);
        }
        return Some(Decision::Intent(EditIntent::Punctuate {
            mark,
            anchor: Anchor::After(anchor.join(" ")),
        }));
    }

    // Anything mentioning a unit is a scoped request ("add a period to the
    // end of the second sentence") that this rule cannot place correctly.
    // Refusing matters more here than anywhere else: the literal `add `
    // verb would otherwise append the words of the command, which is the
    // exact bug this rule exists to fix.
    if mentions_unconsumed_scope(toks) {
        return Some(Decision::Refuse);
    }
    Some(Decision::Intent(EditIntent::Punctuate {
        mark,
        anchor: Anchor::End,
    }))
}

/// "remove the last comma".
///
/// Requires an explicit position: "remove the comma" is a different command
/// when the text holds three of them, and choosing one would be a guess.
fn parse_delete_mark(toks: &[&str]) -> Option<EditIntent> {
    const HEADS: &[&[&str]] = &[&["delete"], &["remove"], &["get", "rid", "of"]];
    let rest = HEADS.iter().find_map(|head| toks.strip_prefix(*head))?;
    let rest = rest.strip_prefix(&["the"][..]).unwrap_or(rest);
    let (which_word, mark_words) = rest.split_first()?;
    let which = ORDINALS
        .iter()
        .find(|(w, _)| w == which_word)
        .map(|(_, w)| *w)?;
    let mark = only_mark(&mark_words.join(" "))?;
    Some(EditIntent::DeleteMark { mark, which })
}

/// "wrap this in quotes", "put that in backticks".
fn parse_wrap(lower: &str, toks: &[&str]) -> Option<Decision> {
    const HEADS: &[&str] = &["wrap", "surround", "put", "enclose"];
    if !HEADS.contains(toks.first()?) {
        return None;
    }
    if !toks.iter().any(|t| matches!(*t, "in" | "with" | "inside")) {
        return None;
    }
    // A scope changes which text gets wrapped, and this rule always wraps
    // the whole target.
    if mentions_unconsumed_scope(toks) {
        return Some(Decision::Refuse);
    }
    let (open, close) = if lower.contains("single quote") {
        ("'", "'")
    } else if lower.contains("quote") {
        ("\"", "\"")
    } else if lower.contains("backtick") || lower.contains("back tick") || lower.contains("code") {
        ("`", "`")
    } else if lower.contains("square bracket") {
        ("[", "]")
    } else if lower.contains("curly") || lower.contains("brace") {
        ("{", "}")
    } else if lower.contains("paren") || lower.contains("bracket") {
        ("(", ")")
    } else if lower.contains("bold") || lower.contains("double asterisk") {
        ("**", "**")
    } else if lower.contains("asterisk") || lower.contains("star") {
        ("*", "*")
    } else {
        return None;
    };
    Some(Decision::Intent(EditIntent::Wrap {
        open: open.into(),
        close: close.into(),
    }))
}

fn parse_identifier_case(lower: &str, toks: &[&str]) -> Option<Decision> {
    // Ordered so "screaming snake case" is not claimed by "snake case".
    const STYLES: &[(&str, IdentCase)] = &[
        ("screaming snake", IdentCase::ScreamingSnake),
        ("constant case", IdentCase::ScreamingSnake),
        ("snake case", IdentCase::Snake),
        ("camel case", IdentCase::Camel),
        ("pascal case", IdentCase::Pascal),
        ("upper camel case", IdentCase::Pascal),
        ("kebab case", IdentCase::Kebab),
        ("dash case", IdentCase::Kebab),
        ("slug", IdentCase::Kebab),
    ];
    let style = STYLES
        .iter()
        .find(|(kw, _)| lower.contains(kw))
        .map(|(_, s)| *s)?;
    // Identifier casing destroys every space and every punctuation mark in
    // the target, so a scoped request ("make the last word snake case")
    // must not be applied to everything.
    if mentions_unconsumed_scope(toks) {
        return Some(Decision::Refuse);
    }
    Some(Decision::Intent(EditIntent::Identifier(style)))
}

fn parse_list_op(lower: &str, toks: &[&str]) -> Option<EditIntent> {
    let has = |w: &str| toks.contains(&w);
    // A spoken count is a promise about the result that a sentence splitter
    // cannot keep: "split this into two lines" on three sentences would
    // produce three. Refuse rather than answer a different question.
    let counted = toks.iter().any(|t| {
        COUNT_WORDS.contains(t) || (t.len() <= 2 && t.chars().all(|c| c.is_ascii_digit()))
    });

    if (has("join") || lower.contains("one line") || lower.contains("single line"))
        && (has("line") || has("lines") || has("them") || has("these"))
    {
        return Some(EditIntent::ListOp(ListOp::JoinLines));
    }
    if counted {
        return None;
    }
    if lower.contains("bullet") {
        return Some(EditIntent::ListOp(ListOp::Bullet));
    }
    if lower.contains("number") && (has("lines") || has("list") || has("sentences")) {
        return Some(EditIntent::ListOp(ListOp::Number));
    }
    if lower.contains("numbered list") {
        return Some(EditIntent::ListOp(ListOp::Number));
    }
    if has("split") && (has("line") || has("lines") || has("sentence") || has("sentences")) {
        return Some(EditIntent::ListOp(ListOp::SplitSentences));
    }
    if lower.contains("own line") {
        return Some(EditIntent::ListOp(ListOp::SplitSentences));
    }
    None
}

/// "capitalize the first word", "uppercase the first letter", "make the
/// last line title case".
fn parse_scoped_recase(lower: &str, toks: &[&str]) -> Option<Decision> {
    let Some(scope) = only_scope(toks) else {
        // No scope, or more than one. More than one ("the first word of the
        // last sentence") must not fall through to the unscoped casing
        // rule, which would rewrite the whole field.
        return mentions_unconsumed_scope(toks).then_some(Decision::Refuse);
    };
    // Explicit style words first; "capitalize" is handled last because it
    // is the ambiguous one.
    let case = if lower.contains("title case") || lower.contains("titlecase") {
        Case::Title
    } else if lower.contains("sentence case") {
        Case::Sentence
    } else if lower.contains("uppercase")
        || lower.contains("upper case")
        || lower.contains("all caps")
        || lower.contains("shout")
    {
        Case::Upper
    } else if lower.contains("lowercase") || lower.contains("lower case") {
        Case::Lower
    } else if lower.contains("capitalize") || lower.contains("capitalise") {
        // "capitalize the first word" means raise its initial letter.
        // "capitalize the first sentence" could equally mean Title Case
        // Every Word Of It, so only the unambiguous units are accepted.
        match scope {
            Scope::Unit {
                unit: TextUnit::Word | TextUnit::Character,
                ..
            } => Case::Title,
            _ => return Some(Decision::Refuse),
        }
    } else {
        // A scope was named but no casing style: not this rule's command.
        // It still must not reach the unscoped casing rule, which ignores
        // scopes entirely.
        return Some(Decision::Refuse);
    };
    Some(Decision::Intent(EditIntent::Scoped {
        scope,
        intent: Box::new(EditIntent::Recase(case)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_phrases_need_both_a_position_and_a_unit() {
        assert!(leading_scope(&tokens("the last sentence")).is_some());
        assert!(leading_scope(&tokens("last word")).is_some());
        assert!(leading_scope(&tokens("the sentence")).is_none());
        assert!(leading_scope(&tokens("the last")).is_none());
    }

    #[test]
    fn two_scopes_in_one_command_are_refused() {
        // "the first word of the last sentence" is a scope this grammar
        // cannot resolve, and resolving half of it would edit the wrong
        // region confidently.
        assert!(only_scope(&tokens(
            "make the first word of the last sentence title case"
        ))
        .is_none());
    }

    #[test]
    fn a_semicolon_is_not_read_as_a_colon() {
        assert_eq!(only_mark("add a semicolon"), Some(';'));
        assert_eq!(only_mark("add a colon"), Some(':'));
        // Two different marks named: not this grammar's command.
        assert_eq!(only_mark("change the period to a comma"), None);
    }
}
