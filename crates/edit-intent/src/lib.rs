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

/// What the user asked for, once their words have been understood.
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

/// Parse a spoken instruction into an [`EditIntent`].
///
/// Returns a deterministic intent when the phrasing matches a known command
/// shape, and [`EditIntent::Freeform`] otherwise so the caller can escalate to
/// a language model.
pub fn parse(utterance: &str) -> EditIntent {
    let trimmed = utterance.trim();
    let lower = trimmed.to_lowercase();

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
    if lower.len() == original.len() && original.is_char_boundary(start) && original.is_char_boundary(end) {
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
/// language model, and for edits whose search text is not present, so the
/// caller can tell the user the command did not match instead of silently
/// doing nothing.
pub fn apply(target: &str, intent: &EditIntent) -> Option<String> {
    match intent {
        EditIntent::Replace { from, to } => {
            let replaced = replace_case_insensitive(target, from, to)?;
            Some(replaced)
        }
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
        EditIntent::Recase(case) => Some(recase(target, *case)),
        EditIntent::Freeform { .. } => None,
    }
}

/// Case-insensitive replace, because speech recognition does not reliably
/// reproduce the casing of what is on screen. Returns `None` when there is
/// nothing to replace.
fn replace_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> Option<String> {
    if needle.is_empty() {
        return None;
    }
    let hay_lower = haystack.to_lowercase();
    let needle_lower = needle.to_lowercase();
    if !hay_lower.contains(&needle_lower) {
        return None;
    }

    // Only trust byte offsets when lowercasing preserved the layout of both
    // strings; otherwise fall back to an exact-case replace to stay correct.
    if hay_lower.len() != haystack.len() || needle_lower.len() != needle.len() {
        return if haystack.contains(needle) {
            Some(haystack.replace(needle, replacement))
        } else {
            None
        };
    }

    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0;
    while let Some(found) = hay_lower[cursor..].find(&needle_lower) {
        let start = cursor + found;
        out.push_str(&haystack[cursor..start]);
        out.push_str(replacement);
        cursor = start + needle_lower.len();
    }
    out.push_str(&haystack[cursor..]);
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
    out.replace(" ,", ",")
        .replace(" .", ".")
        .trim()
        .to_string()
}

fn recase(text: &str, case: Case) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_change_to() {
        assert_eq!(
            parse("change hello to goodbye"),
            EditIntent::Replace {
                from: "hello".into(),
                to: "goodbye".into()
            }
        );
    }

    #[test]
    fn parses_replace_with() {
        assert_eq!(
            parse("replace foo with bar"),
            EditIntent::Replace {
                from: "foo".into(),
                to: "bar".into()
            }
        );
    }

    #[test]
    fn splits_on_last_joiner_so_search_text_survives() {
        // The word "to" appears inside the search text; a naive first-match
        // split would produce from="" and lose the command.
        assert_eq!(
            parse("change to do to todo"),
            EditIntent::Replace {
                from: "to do".into(),
                to: "todo".into()
            }
        );
    }

    #[test]
    fn parses_delete_and_append() {
        assert_eq!(
            parse("delete the last sentence"),
            EditIntent::Delete {
                text: "the last sentence".into()
            }
        );
        assert_eq!(
            parse("append and thanks"),
            EditIntent::Append {
                text: "and thanks".into()
            }
        );
    }

    #[test]
    fn parses_casing() {
        assert_eq!(parse("make it all caps"), EditIntent::Recase(Case::Upper));
        assert_eq!(parse("title case please"), EditIntent::Recase(Case::Title));
    }

    #[test]
    fn unknown_phrasing_becomes_freeform() {
        let intent = parse("tighten this up and make it sound friendlier");
        assert!(matches!(intent, EditIntent::Freeform { .. }));
    }

    #[test]
    fn apply_replaces_ignoring_case() {
        let intent = EditIntent::Replace {
            from: "hello".into(),
            to: "goodbye".into(),
        };
        assert_eq!(
            apply("Hello world, hello again", &intent).unwrap(),
            "goodbye world, goodbye again"
        );
    }

    #[test]
    fn apply_reports_no_match() {
        let intent = EditIntent::Replace {
            from: "absent".into(),
            to: "x".into(),
        };
        assert!(apply("nothing here", &intent).is_none());
    }

    #[test]
    fn apply_delete_cleans_up_whitespace() {
        let intent = EditIntent::Delete {
            text: "very ".into(),
        };
        assert_eq!(apply("a very long day", &intent).unwrap(), "a long day");
    }

    #[test]
    fn apply_append_spaces_correctly() {
        let intent = EditIntent::Append {
            text: "world".into(),
        };
        assert_eq!(apply("hello", &intent).unwrap(), "hello world");
        assert_eq!(apply("hello ", &intent).unwrap(), "hello world");
        assert_eq!(apply("", &intent).unwrap(), "world");
    }

    #[test]
    fn freeform_has_no_deterministic_application() {
        let intent = EditIntent::Freeform {
            instruction: "make it nicer".into(),
        };
        assert!(apply("text", &intent).is_none());
    }

    #[test]
    fn non_ascii_input_does_not_panic() {
        // Turkish, German, and Greek all have characters whose lowercase form
        // differs in byte length, which is exactly where naive byte slicing
        // panics.
        for utterance in [
            "change İstanbul to Istanbul",
            "replace STRASSE with Straße",
            "delete ΣΊΣΥΦΟΣ",
            "change Ǆ to dz",
        ] {
            let intent = parse(utterance);
            let _ = apply("İstanbul STRASSE ΣΊΣΥΦΟΣ Ǆ", &intent);
        }
    }

    #[test]
    fn recase_variants() {
        assert_eq!(recase("hello world", Case::Upper), "HELLO WORLD");
        assert_eq!(recase("HELLO WORLD", Case::Lower), "hello world");
        assert_eq!(recase("hello world", Case::Title), "Hello World");
        assert_eq!(recase("hELLO WORLD", Case::Sentence), "Hello world");
    }
}
