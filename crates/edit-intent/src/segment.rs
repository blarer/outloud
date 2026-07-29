//! Cutting text into the units a spoken scope refers to.
//!
//! "delete the last sentence" is only as safe as the answer to "where does
//! the last sentence start". Every function here returns byte spans into the
//! original string, never a rebuilt copy, because the caller has to splice
//! around the span and any rebuild would silently normalise text the user
//! did not ask us to touch.
//!
//! The bar for a segmenter that feeds a *delete* is higher than for one that
//! feeds a highlight: getting it wrong destroys words. So the sentence
//! splitter refuses the two cases a naive `split('.')` gets wrong on real
//! dictation (abbreviations and initials), and the line/paragraph splitters
//! refuse to answer at all when the text contains no line breaks, rather
//! than quietly treating the whole field as "the first line".

/// Words that end in a period without ending a sentence.
///
/// Deliberately short rather than exhaustive, and the asymmetry is the
/// reason: a MISSING entry only makes a scoped delete take less than the
/// user meant, while a WRONG entry suppresses a real sentence break and
/// makes it take more. So anything that is also an ordinary English word is
/// excluded, however common the abbreviation is. "no." (as in number) is the
/// one that costs the most: "the answer is no. we should tell them" would
/// become a single sentence, and "delete the last sentence" would take the
/// lot.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "inc", "ltd",
    "corp", "dept", "fig", "approx", "ave", "rd", "blvd", "a.m", "p.m", "u.s", "u.k", "cf",
];

/// Is the word ending at byte `period_start` (pointing just before the
/// period) one that conventionally carries a period of its own?
///
/// Also treats a single UPPERCASE letter as non-terminal, which is what makes
/// "J. R. Tolkien wrote this" one sentence instead of three. The case test is
/// load-bearing: without it, any sentence whose last word is a single letter
/// stops being a sentence ("the grade is a. next question" would become one).
/// Initials are conventionally capitalised, so requiring it costs nothing real
/// and removes a whole class of false merge.
fn period_is_part_of_a_word(text: &str, period_start: usize) -> bool {
    let preceding = &text[..period_start];
    let word_start = preceding
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word = &preceding[word_start..];
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if chars.next().is_none() {
        return first.is_uppercase();
    }
    // "e.g" arrives here with its internal period still attached, which is
    // why the table carries the dotted forms too.
    ABBREVIATIONS.contains(&word.to_lowercase().as_str())
}

/// Sentence spans, each covering its own terminal punctuation.
///
/// A break happens at `.`/`!`/`?` when the next character is whitespace or
/// the text ends. Note what is deliberately NOT a rule: "the next word is
/// lowercase, so this cannot be a sentence break". Speech recognition
/// produces exactly that shape constantly ("...upset. we should tell them"),
/// and honouring it would refuse the most common real break there is.
pub(crate) fn sentence_spans(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut spans = Vec::new();
    let mut start = 0usize;
    for (idx, (byte, ch)) in chars.iter().enumerate() {
        if !matches!(ch, '.' | '!' | '?') {
            continue;
        }
        let next_is_break = chars
            .get(idx + 1)
            .is_none_or(|(_, next)| next.is_whitespace());
        if !next_is_break {
            // "3.5" and "e.g." live here: the period has a non-space after
            // it, so it was never a sentence end.
            continue;
        }
        if *ch == '.' && period_is_part_of_a_word(text, *byte) {
            continue;
        }
        let end = byte + ch.len_utf8();
        if !text[start..end].trim().is_empty() {
            spans.push((start, end));
        }
        start = end;
    }
    if !text[start..].trim().is_empty() {
        spans.push((start, text.len()));
    }
    // Leading whitespace belongs to the gap between sentences, not to the
    // sentence, or deleting one leaves the space that preceded it behind.
    spans
        .into_iter()
        .map(|(s, e)| {
            let lead = text[s..e].len() - text[s..e].trim_start().len();
            (s + lead, e)
        })
        .collect()
}

/// Whitespace-delimited word spans, punctuation included.
///
/// "delete the last word" on `...tell them soon.` should take the period
/// with it: the user is removing a word, and leaving its punctuation
/// stranded is never what they meant.
pub(crate) fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    spans
}

/// Line spans, or `None` when the text has no line breaks at all.
///
/// Refusing is the whole point. Dictated prose is one unbroken run, so
/// "delete the first line" against it would resolve to the entire field and
/// wipe everything the user has. An intent that cannot be resolved becomes
/// a reported no-match, which costs the user a glance; guessing costs them
/// their text.
pub(crate) fn line_spans(text: &str) -> Option<Vec<(usize, usize)>> {
    if !text.contains('\n') {
        return None;
    }
    Some(split_spans(text, "\n"))
}

/// Paragraph spans, or `None` when the text has no blank-line separator,
/// for the same reason [`line_spans`] refuses.
pub(crate) fn paragraph_spans(text: &str) -> Option<Vec<(usize, usize)>> {
    if !text.contains("\n\n") {
        return None;
    }
    Some(split_spans(text, "\n\n"))
}

fn split_spans(text: &str, sep: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut pos = 0usize;
    for part in text.split(sep) {
        spans.push((pos, pos + part.len()));
        pos += part.len() + sep.len();
    }
    spans.retain(|(s, e)| !text[*s..*e].trim().is_empty());
    spans
}

/// Remove `start..end` and repair the seam it leaves.
///
/// The prototype this replaces collapsed the result with
/// `split_whitespace().join(" ")`, which flattens every newline and every
/// run of indentation in the *whole* document as a side effect of deleting
/// one sentence. That is an unrequested edit outside the requested span,
/// which is exactly what the over-edit gate in
/// `docs/planning/03-definition-of-done.md` forbids. So the repair here is
/// strictly local: it only touches whitespace that now abuts the cut.
pub(crate) fn splice_out(text: &str, start: usize, end: usize) -> String {
    let head = &text[..start];
    let tail = &text[end..];
    if head.trim().is_empty() {
        return tail.trim_start().to_string();
    }
    if tail.trim().is_empty() {
        return head.trim_end().to_string();
    }
    let head = head.trim_end_matches([' ', '\t']);
    let tail = tail.trim_start_matches([' ', '\t']);
    // A line break on either side already separates the two halves, and so
    // does punctuation that opens the tail; inserting a space would be an
    // edit nobody asked for.
    let needs_space = !head.ends_with('\n')
        && !tail.starts_with('\n')
        && !tail.starts_with([',', '.', '!', '?', ';', ':']);
    if needs_space {
        format!("{head} {tail}")
    } else {
        format!("{head}{tail}")
    }
}

/// The units a list operation acts on: real lines when the text has them,
/// sentences otherwise.
///
/// Speech recognition emits no line breaks, so "turn this into bullet
/// points" on dictated prose has to mean sentences or it produces a
/// one-item list, which is a result no user would accept from a feature
/// whose selling point is predictability.
pub(crate) fn list_units(text: &str) -> Vec<String> {
    if text.contains('\n') {
        return text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
    }
    sentence_spans(text)
        .into_iter()
        .map(|(s, e)| text[s..e].trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentences(text: &str) -> Vec<&str> {
        sentence_spans(text)
            .into_iter()
            .map(|(s, e)| &text[s..e])
            .collect()
    }

    #[test]
    fn abbreviations_do_not_end_a_sentence() {
        // The prototype's splitter cut after "Dr." and handed back
        // "Smith we are done" as the last sentence, which as a *delete*
        // would have destroyed the wrong words.
        assert_eq!(
            sentences("Ship at 3.5 percent. Tell Dr. Smith we are done"),
            vec!["Ship at 3.5 percent.", "Tell Dr. Smith we are done"]
        );
    }

    #[test]
    fn initials_do_not_end_a_sentence() {
        assert_eq!(
            sentences("J. R. Tolkien wrote this. It is long"),
            vec!["J. R. Tolkien wrote this.", "It is long"]
        );
    }

    #[test]
    fn a_lowercase_single_letter_still_ends_a_sentence() {
        // Initials are capitalised; a lone lowercase letter is an ordinary
        // word. Without the case test, "a." merged the two sentences and a
        // scoped delete took both.
        assert_eq!(
            sentences("the grade is a. next question"),
            vec!["the grade is a.", "next question"]
        );
    }

    #[test]
    fn decimals_do_not_end_a_sentence() {
        assert_eq!(
            sentences("we grew 3.5 percent"),
            vec!["we grew 3.5 percent"]
        );
    }

    #[test]
    fn a_lowercase_next_word_still_ends_a_sentence() {
        // Dictation capitalises inconsistently; refusing to break here
        // would refuse the most common real break in the corpus.
        assert_eq!(
            sentences("they are upset. we should tell them"),
            vec!["they are upset.", "we should tell them"]
        );
    }

    #[test]
    fn question_and_exclamation_end_sentences() {
        assert_eq!(
            sentences("are we ready? yes! ship it"),
            vec!["are we ready?", "yes!", "ship it"]
        );
    }

    #[test]
    fn line_scopes_refuse_unbroken_prose() {
        assert!(line_spans("one long dictated run of words").is_none());
        assert!(paragraph_spans("one\ntwo").is_none());
        assert!(line_spans("one\ntwo").is_some());
    }

    #[test]
    fn splice_repairs_only_the_seam() {
        assert_eq!(splice_out("a  b  c", 3, 4), "a c");
        // The newline three lines away must survive deleting a word.
        assert_eq!(splice_out("one two\nthree", 4, 7), "one\nthree");
        // Punctuation must not be pushed away from the word it follows.
        assert_eq!(splice_out("keep this, and that", 5, 9), "keep, and that");
    }

    #[test]
    fn list_units_fall_back_to_sentences_without_line_breaks() {
        assert_eq!(list_units("one. two. three"), vec!["one.", "two.", "three"]);
        assert_eq!(list_units("one\n\ntwo"), vec!["one", "two"]);
    }
}
