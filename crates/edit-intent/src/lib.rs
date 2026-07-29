//! Turning a spoken phrase into an edit.
//!
//! Aqua's edit-by-voice feature has two halves: reaching into the focused field
//! (handled by `ax-edit`) and deciding what the user meant. This module is the
//! second half.
//!
//! The design choice that matters here is that a language model is *not* the
//! first resort. Most real edit commands are a small, closed set of literal
//! operations, and a deterministic parser handles them with no model latency,
//! no GPU, and no chance of the model rewriting text the user did not ask it to
//! touch. The model is the fallback for genuinely open-ended instructions.
//!
//! That split is also what keeps the latency budget honest: the common case
//! costs microseconds, so the end-to-end time is dominated by speech
//! recognition rather than by generation.
//!
//! # Why the grammar is wider than the four original verbs
//!
//! `docs/investigations/edit-intent.md` measured a 55-command corpus through
//! the original grammar: 15 correct, 10 **silently wrong**, and 15 that had an
//! exact deterministic answer but escalated to a model that is not wired up.
//! Two failure shapes drove this crate's expansion.
//!
//! The first is scope. `uppercase the first letter` matched on the substring
//! `uppercase` with no regard for what followed and SHOUTED the entire field.
//! A command that names a unit it does not resolve is now refused outright
//! rather than applied to everything.
//!
//! The second is literalism. `add a period at the end` appended the *words*
//! "a period at the end", and `delete the last sentence` searched for that
//! phrase as text. Both now have first-class intents.
//!
//! # The rule that governs additions
//!
//! **A phrasing that could plausibly mean two different edits must not
//! parse.** Escalating costs the user a rephrase; guessing costs them their
//! words, and they may not notice. Several rules in [`parse`] therefore refuse
//! phrasings they *nearly* understand, and each says why at the refusal site.

mod apply;
mod parse;
mod segment;

/// What the user asked for, once their words have been understood.
///
/// The first five variants are the original grammar and are unchanged; the
/// rest were added to absorb commands that previously became
/// [`EditIntent::Freeform`] or, worse, were mis-read literally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditIntent {
    /// Replace every occurrence of `from` with `to`.
    Replace { from: String, to: String },
    /// Remove every occurrence of `text`.
    Delete { text: String },
    /// Insert `text` at the end of the target.
    Append { text: String },
    /// Change the casing of the whole target.
    Recase(Case),
    /// Remove a whole unit of text ("delete the last sentence").
    DeleteScope(Scope),
    /// Apply `intent` to only part of the target ("in the last sentence,
    /// change its to it's"). The inner intent never sees the rest of the
    /// text, which is what makes the narrowing real rather than cosmetic.
    Scoped {
        scope: Scope,
        intent: Box<EditIntent>,
    },
    /// Place a punctuation mark ("add a period at the end").
    Punctuate { mark: char, anchor: Anchor },
    /// Remove a punctuation mark by position ("remove the last comma").
    DeleteMark { mark: char, which: Which },
    /// Surround the target ("wrap this in quotes").
    Wrap { open: String, close: String },
    /// Rewrite the target as a programming identifier ("make it snake case").
    Identifier(IdentCase),
    /// Restructure the target as lines or a list ("number these lines").
    ListOp(ListOp),
    /// Revert previous edits. Carries no text transformation: the caller's
    /// undo ring owns the previous states, so [`apply`] returns `None` and
    /// delivery routes on the variant instead.
    Undo(UndoDepth),
    /// An open-ended instruction that only a language model can carry out.
    Freeform { instruction: String },
}

/// Casing transformations that come up constantly in dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    Upper,
    Lower,
    Title,
    Sentence,
}

/// Which unit of text a spoken scope refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextUnit {
    Sentence,
    Word,
    Line,
    Paragraph,
    /// Only ever reached through "the first letter/character".
    Character,
}

/// Which occurrence of a unit a scope names.
///
/// There is no `First` variant: "first" is `Nth(0)`, so the ordinal family
/// ("the second sentence") shares one code path with it and cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Nth(usize),
    Last,
}

/// The region of the target an intent applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everything the caller handed us: the selection, or the whole field.
    Whole,
    Unit {
        which: Which,
        unit: TextUnit,
    },
}

/// Where a punctuation mark goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// After the last non-whitespace character of the target.
    End,
    /// Immediately after the first occurrence of this text.
    After(String),
}

/// Identifier conventions, which are what "make it snake case" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentCase {
    Snake,
    ScreamingSnake,
    Kebab,
    Camel,
    Pascal,
}

/// Whole-target restructurings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOp {
    /// Collapse line breaks into spaces.
    JoinLines,
    /// One sentence per line.
    SplitSentences,
    /// Prefix each unit with "- ".
    Bullet,
    /// Prefix each unit with "1. ", "2. ", ...
    Number,
}

/// How far back an undo request reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoDepth {
    /// "undo that": pop one edit.
    One,
    /// "go back to the original": rewind this field's whole stack.
    All,
}

/// Parse a spoken instruction into an [`EditIntent`].
///
/// Returns a deterministic intent when the phrasing matches a known command
/// shape, and [`EditIntent::Freeform`] otherwise so the caller can escalate to
/// a language model.
///
/// # Rule order
///
/// The extended grammar runs **before** the original literal verbs, because
/// the literal verbs are greedy: `delete ` followed by anything at all is a
/// valid `Delete`, so `delete the last sentence` would be claimed as a search
/// for that phrase before any scope rule could see it. Ordering it this way
/// makes the extended rules the only ones that need to be conservative, and
/// the literal verbs keep their existing behaviour on everything the extended
/// grammar declines.
///
/// A rule that recognises an utterance but distrusts it returns
/// [`parse::Decision::Refuse`], which goes straight to `Freeform` rather than
/// falling through. Falling through would hand exactly the phrasings we
/// refused to the greedy literal verbs, which is the failure being fixed.
pub fn parse(utterance: &str) -> EditIntent {
    let trimmed = utterance.trim();
    let lower = trimmed.to_lowercase();

    match parse::parse(trimmed, &lower) {
        parse::Decision::Intent(intent) => return intent,
        parse::Decision::Refuse => {
            return EditIntent::Freeform {
                instruction: trimmed.to_string(),
            }
        }
        parse::Decision::Pass => {}
    }

    // "change X to Y" / "replace X with Y" / "make X into Y"
    for (head, joiner) in [
        ("change ", " to "),
        ("replace ", " with "),
        ("make ", " into "),
        ("swap ", " for "),
    ] {
        if let Some(rest) = lower.strip_prefix(head) {
            // Split on the last occurrence so a joiner appearing inside the
            // search text does not truncate it: "change to do to to-do".
            if let Some(idx) = rest.rfind(joiner) {
                let from = slice_original(trimmed, head.len(), head.len() + idx);
                let to = slice_original(trimmed, head.len() + idx + joiner.len(), trimmed.len());
                if !from.trim().is_empty() && !to.trim().is_empty() {
                    return EditIntent::Replace {
                        from: from.trim().to_string(),
                        to: to.trim().to_string(),
                    };
                }
            }
        }
    }

    // "delete X" / "remove X" / "get rid of X"
    for head in ["delete ", "remove ", "get rid of ", "scratch "] {
        if let Some(rest) = lower.strip_prefix(head) {
            if !rest.trim().is_empty() {
                let text = slice_original(trimmed, head.len(), trimmed.len());
                return EditIntent::Delete {
                    text: text.trim().to_string(),
                };
            }
        }
    }

    // "add X" / "append X"
    for head in ["append ", "add ", "also add "] {
        if let Some(rest) = lower.strip_prefix(head) {
            if !rest.trim().is_empty() {
                let text = slice_original(trimmed, head.len(), trimmed.len());
                return EditIntent::Append {
                    text: text.trim().to_string(),
                };
            }
        }
    }

    // Casing commands are phrased many ways; match on the salient keywords.
    // Anything naming a scope was already handled (or deliberately refused)
    // above, so a match here really does mean the whole target.
    if let Some(case) = parse_case(&lower) {
        return EditIntent::Recase(case);
    }

    EditIntent::Freeform {
        instruction: trimmed.to_string(),
    }
}

fn parse_case(lower: &str) -> Option<Case> {
    const UPPER: [&str; 3] = ["uppercase", "all caps", "capitalize everything"];
    const LOWER: [&str; 2] = ["lowercase", "all lowercase"];
    const TITLE: [&str; 2] = ["title case", "titlecase"];
    const SENTENCE: [&str; 2] = ["sentence case", "sentencecase"];

    if UPPER.iter().any(|k| lower.contains(k)) {
        return Some(Case::Upper);
    }
    if LOWER.iter().any(|k| lower.contains(k)) {
        return Some(Case::Lower);
    }
    if TITLE.iter().any(|k| lower.contains(k)) {
        return Some(Case::Title);
    }
    if SENTENCE.iter().any(|k| lower.contains(k)) {
        return Some(Case::Sentence);
    }
    None
}

/// Take a byte range that was located in the lowercased copy and return the
/// corresponding text from the original.
///
/// `to_lowercase` can change a string's byte length for some characters, so the
/// indices are only reliable when the two strings agree in length. When they do
/// not, fall back to the lowercased text, which is still a usable command
/// string and is vastly better than panicking on a byte boundary.
fn slice_original(original: &str, start: usize, end: usize) -> String {
    let lower = original.to_lowercase();
    if lower.len() == original.len()
        && original.is_char_boundary(start)
        && original.is_char_boundary(end)
    {
        return original[start..end].to_string();
    }
    lower
        .get(start..end)
        .map(str::to_string)
        .unwrap_or_else(|| lower.clone())
}

/// Apply a deterministic intent to `target`.
///
/// Returns `None` for [`EditIntent::Freeform`], which by definition needs a
/// language model, for [`EditIntent::Undo`], which the caller's undo ring
/// resolves, and for edits that do not apply to this text (search text
/// absent, scope that does not exist, list operation on a single unit), so
/// the caller can tell the user the command did not match instead of silently
/// doing nothing.
pub fn apply(target: &str, intent: &EditIntent) -> Option<String> {
    match intent {
        EditIntent::Replace { from, to } => replace_case_insensitive(target, from, to),
        EditIntent::Delete { text } => {
            let removed = replace_case_insensitive(target, text, "")?;
            // Deleting a word leaves a double space behind, which no user wants.
            Some(collapse_spaces(&removed))
        }
        EditIntent::Append { text } => {
            if target.is_empty() {
                Some(text.clone())
            } else if target.ends_with(' ') {
                Some(format!("{target}{text}"))
            } else {
                Some(format!("{target} {text}"))
            }
        }
        EditIntent::Recase(case) => Some(apply::recase(target, *case)),
        EditIntent::Freeform { .. } => None,
        other => apply::apply(target, other),
    }
}

/// Case-insensitive replace, because speech recognition does not reliably
/// reproduce the casing of what is on screen. Returns `None` when there is
/// nothing to replace.
///
/// Lowercasing can change a string's byte length per character (İ grows, ẞ
/// shrinks), and the shifts can cancel out so that total lengths still agree.
/// Byte offsets found in the lowercased copy are therefore never trusted
/// against the original; instead every lowered byte carries a map back to the
/// original character it came from, so a match located in lowered space is
/// removed from the original along real character boundaries. The workspace
/// fuzz suite (`tests/tests/fuzz_edits.rs`) found both the panic and a
/// silent over-edit in the previous length-equality heuristic.
fn replace_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> Option<String> {
    if needle.is_empty() {
        return None;
    }
    let needle_lower = needle.to_lowercase();

    // origin[i] = byte offset in `haystack` of the character that produced
    // lowered byte i. Adjacent lowered bytes from the same original char
    // share an origin, which is also how expansion boundaries are detected.
    let mut lowered = String::with_capacity(haystack.len());
    let mut origin: Vec<usize> = Vec::with_capacity(haystack.len());
    for (orig_start, ch) in haystack.char_indices() {
        for lc in ch.to_lowercase() {
            let start = lowered.len();
            lowered.push(lc);
            origin.extend(std::iter::repeat_n(orig_start, lowered.len() - start));
        }
    }

    // A lowered position is a real character boundary only when the original
    // char changes there. A match that starts or ends mid-expansion (e.g.
    // the combining dot that İ lowers into) is not a real occurrence.
    let is_boundary =
        |pos: usize| pos == 0 || pos == lowered.len() || origin[pos] != origin[pos - 1];

    let mut out = String::with_capacity(haystack.len());
    let mut orig_cursor = 0;
    let mut cursor = 0;
    let mut matched = false;
    while let Some(found) = lowered[cursor..].find(&needle_lower) {
        let start = cursor + found;
        let end = start + needle_lower.len();
        if !(is_boundary(start) && is_boundary(end)) {
            // Partial-character match: step past it and keep scanning.
            cursor = start + lowered[start..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let orig_start = origin[start];
        let orig_end = if end == lowered.len() {
            haystack.len()
        } else {
            origin[end]
        };
        out.push_str(&haystack[orig_cursor..orig_start]);
        out.push_str(replacement);
        orig_cursor = orig_end;
        cursor = end;
        matched = true;
    }
    if !matched {
        return None;
    }
    out.push_str(&haystack[orig_cursor..]);
    Some(out)
}

fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    for ch in text.chars() {
        let is_space = ch == ' ';
        if is_space && last_was_space {
            continue;
        }
        out.push(ch);
        last_was_space = is_space;
    }
    // Removing a trailing word leaves a dangling space before punctuation.
    out.replace(" ,", ",").replace(" .", ".").trim().to_string()
}

#[cfg(test)]
mod tests;
