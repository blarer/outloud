//! Carrying out an [`EditIntent`] against the target text.
//!
//! Every function here returns `Option<String>`, and `None` is a real
//! answer, not an error to be papered over: it means "the command was
//! understood but does not apply to this text". The caller reports that,
//! which costs the user a glance. The alternative, editing something
//! adjacent and hoping, costs them their words.

use crate::{
    segment::{line_spans, list_units, paragraph_spans, sentence_spans, splice_out, word_spans},
    Anchor, Case, EditIntent, IdentCase, ListOp, Scope, TextUnit, Which,
};

/// Resolve a scope to a byte span of `text`.
///
/// `None` means the scope does not exist here (a fifth sentence in a
/// two-sentence field, or any line scope on unbroken dictated prose).
pub(crate) fn resolve(text: &str, scope: &Scope) -> Option<(usize, usize)> {
    let Scope::Unit { which, unit } = scope else {
        return Some((0, text.len()));
    };
    let spans = match unit {
        TextUnit::Sentence => sentence_spans(text),
        TextUnit::Word => word_spans(text),
        TextUnit::Line => line_spans(text)?,
        TextUnit::Paragraph => paragraph_spans(text)?,
        // A character scope only ever appears as "the first letter", so it
        // is the first char of the first word rather than of the raw text:
        // leading whitespace has no letter to capitalise.
        TextUnit::Character => {
            let (start, end) = pick(&word_spans(text), which)?;
            let first = text[start..end].chars().next()?;
            return Some((start, start + first.len_utf8()));
        }
    };
    pick(&spans, which)
}

fn pick(spans: &[(usize, usize)], which: &Which) -> Option<(usize, usize)> {
    match which {
        Which::Nth(n) => spans.get(*n).copied(),
        Which::Last => spans.last().copied(),
    }
}

/// See [`crate::apply`] for the contract.
pub(crate) fn apply(target: &str, intent: &EditIntent) -> Option<String> {
    match intent {
        EditIntent::DeleteScope(scope) => {
            let (start, end) = resolve(target, scope)?;
            if start == end {
                return None;
            }
            Some(splice_out(target, start, end))
        }
        EditIntent::Scoped { scope, intent } => {
            let (start, end) = resolve(target, scope)?;
            // The inner intent sees ONLY its scope, which is what makes
            // "in the last sentence change its to it's" leave earlier
            // occurrences of "its" alone.
            let inner = crate::apply(&target[start..end], intent)?;
            Some(format!("{}{inner}{}", &target[..start], &target[end..]))
        }
        EditIntent::Punctuate { mark, anchor } => punctuate(target, *mark, anchor),
        EditIntent::DeleteMark { mark, which } => delete_mark(target, *mark, which),
        EditIntent::Wrap { open, close } => {
            let trimmed = target.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Already wrapped: wrapping again is almost never what was
            // meant, and un-wrapping is a different command.
            if trimmed.starts_with(open.as_str()) && trimmed.ends_with(close.as_str()) {
                return None;
            }
            Some(format!("{open}{trimmed}{close}"))
        }
        EditIntent::Identifier(style) => identifier(target, *style),
        EditIntent::ListOp(op) => list_op(target, *op),
        // Undo is resolved by the caller's undo ring; there is no text
        // transformation to compute here.
        EditIntent::Undo(_) => None,
        EditIntent::Freeform { .. } => None,
        // Handled by the shipped paths in lib.rs.
        _ => None,
    }
}

fn punctuate(target: &str, mark: char, anchor: &Anchor) -> Option<String> {
    match anchor {
        Anchor::End => {
            let head = target.trim_end();
            if head.is_empty() {
                return None;
            }
            // Replace terminal punctuation rather than stacking it:
            // "add a question mark" on "...soon." means "...soon?".
            let head = head.trim_end_matches(['.', ',', '!', '?', ':', ';']);
            if head.is_empty() {
                return None;
            }
            Some(format!("{head}{mark}"))
        }
        Anchor::After(word) => {
            let end = find_fold_insensitive(target, word)?;
            // The character already sitting there is replaced, not
            // appended to, so "add a comma after today" against
            // "today. The" yields "today, The" and never "today,.".
            let existing = target[end..]
                .chars()
                .next()
                .filter(|c| matches!(c, '.' | ',' | '!' | '?' | ':' | ';'));
            let tail = end + existing.map_or(0, char::len_utf8);
            Some(format!("{}{mark}{}", &target[..end], &target[tail..]))
        }
    }
}

/// Byte offset just past the first case-insensitive occurrence of `needle`.
///
/// Matching walks the ORIGINAL's char boundaries and folds each char as it
/// goes, rather than searching a lowercased copy. `to_lowercase` changes
/// byte lengths per character (İ grows, Ǆ shrinks), so an offset found in a
/// lowered copy can land mid-character in the original and panic on slice.
/// The workspace fuzz suite found precisely that class in this crate once.
fn find_fold_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let want: Vec<char> = needle.to_lowercase().chars().collect();
    if want.is_empty() {
        return None;
    }
    'start: for (start, _) in haystack.char_indices() {
        let mut i = 0usize;
        for (offset, ch) in haystack[start..].char_indices() {
            for folded in ch.to_lowercase() {
                match want.get(i) {
                    Some(&expected) if expected == folded => i += 1,
                    // A char that folds into several (İ) must match all of
                    // them or the "match" ends mid-character.
                    _ => continue 'start,
                }
            }
            if i == want.len() {
                return Some(start + offset + ch.len_utf8());
            }
        }
    }
    None
}

fn delete_mark(target: &str, mark: char, which: &Which) -> Option<String> {
    let positions: Vec<usize> = target
        .char_indices()
        .filter(|(_, c)| *c == mark)
        .map(|(i, _)| i)
        .collect();
    let at = match which {
        Which::Nth(n) => *positions.get(*n)?,
        Which::Last => *positions.last()?,
    };
    Some(splice_out(target, at, at + mark.len_utf8()))
}

/// The words an identifier is built from: case-folded, stripped to
/// alphanumerics, empties dropped.
///
/// Folding happens BEFORE filtering, and the order is load-bearing. `İ` is
/// alphanumeric, but it lowercases to `i` plus COMBINING DOT ABOVE, and the
/// combining mark is category Mn. Filtering first lets the `İ` through and
/// then expands it into a stray mark inside the identifier.
fn identifier_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .flat_map(char::to_lowercase)
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn identifier(target: &str, style: IdentCase) -> Option<String> {
    let words = identifier_words(target);
    if words.is_empty() {
        return None;
    }
    let capitalise = |w: &str| {
        let mut chars = w.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    };
    Some(match style {
        IdentCase::Snake => words.join("_"),
        IdentCase::Kebab => words.join("-"),
        IdentCase::ScreamingSnake => words.join("_").to_uppercase(),
        IdentCase::Camel => {
            let mut out = words[0].clone();
            for word in &words[1..] {
                out.push_str(&capitalise(word));
            }
            out
        }
        IdentCase::Pascal => words.iter().map(|w| capitalise(w)).collect(),
    })
}

fn list_op(target: &str, op: ListOp) -> Option<String> {
    match op {
        ListOp::JoinLines => {
            if !target.contains('\n') {
                // Nothing to join. Reporting no-match is honest; returning
                // the input unchanged would flash a "done" chip for an edit
                // that never happened.
                return None;
            }
            let joined: Vec<&str> = target
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            Some(joined.join(" "))
        }
        ListOp::SplitSentences => {
            let units = list_split_units(target)?;
            Some(units.join("\n"))
        }
        ListOp::Bullet => {
            let units = list_split_units(target)?;
            Some(
                units
                    .iter()
                    .map(|u| format!("- {u}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        ListOp::Number => {
            let units = list_split_units(target)?;
            Some(
                units
                    .iter()
                    .enumerate()
                    .map(|(i, u)| format!("{}. {u}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    }
}

/// Units for a list operation, refusing when there is only one.
///
/// A one-item bulleted list is not what anyone asks for, and producing it
/// would consume the utterance while leaving the user's actual request
/// unserved.
fn list_split_units(target: &str) -> Option<Vec<String>> {
    let units = list_units(target);
    if units.len() < 2 {
        return None;
    }
    Some(units)
}

/// Casing applied to a whole (possibly scoped) piece of text.
pub(crate) fn recase(text: &str, case: Case) -> String {
    match case {
        Case::Upper => text.to_uppercase(),
        Case::Lower => text.to_lowercase(),
        Case::Title => text
            .split(' ')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        Case::Sentence => {
            let lower = text.to_lowercase();
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}
