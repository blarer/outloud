//! Prototype: how much of the "freeform" traffic is actually deterministic?
//!
//! The shipped parser sends 15 of the 54 corpus commands to a language model
//! that does not exist, and mis-parses 9 more literally ("add a period at the
//! end" appends the words "a period at the end"). This prototype is a
//! pre-pass in front of the shipped parser that resolves four families the
//! current grammar has no concept of:
//!
//!   1. scope    "delete the last sentence", "the first word", "that"
//!   2. punctuation  "add a period", "add a comma after X"
//!   3. wrapping "wrap this in quotes / backticks / parens"
//!   4. identifier case  snake / camel / kebab
//!   5. line ops "join these lines", "split into lines", "number these lines"
//!
//! None of it needs a model: every one has an exact, predictable output, and
//! predictability is what lets an edit apply instantly with undo rather than
//! going through a preview panel (docs/ux/03-edit-by-voice.md).
//!
//! Measured here against the same corpus the shipped parser was measured on,
//! so the coverage delta is a like-for-like number, not an estimate.

use std::time::Instant;

// ---------------------------------------------------------------- scopes

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Sentence,
    Word,
    Line,
    Paragraph,
    Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    First,
    Last,
    This,
}

/// A resolved span of the target text, as byte offsets.
fn resolve_scope(text: &str, which: Which, unit: Unit) -> Option<(usize, usize)> {
    match unit {
        Unit::Selection => Some((0, text.len())),
        Unit::Word => {
            let mut spans: Vec<(usize, usize)> = Vec::new();
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
            match which {
                Which::First => spans.first().copied(),
                Which::Last | Which::This => spans.last().copied(),
            }
        }
        Unit::Sentence => {
            // Split after . ! ? followed by whitespace-or-end. Crude on
            // "Dr. Smith", which is why the UX previews large blast radii.
            let mut spans = Vec::new();
            let mut start = 0usize;
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            for (idx, (i, c)) in bytes.iter().enumerate() {
                if matches!(c, '.' | '!' | '?') {
                    let end = i + c.len_utf8();
                    let next_is_space = bytes
                        .get(idx + 1)
                        .map(|(_, n)| n.is_whitespace())
                        .unwrap_or(true);
                    if next_is_space {
                        if text[start..end].trim().len() > 1 {
                            spans.push((start, end));
                        }
                        start = end;
                    }
                }
            }
            if text[start..].trim().len() > 1 {
                spans.push((start, text.len()));
            }
            let span = match which {
                Which::First => spans.first().copied(),
                Which::Last | Which::This => spans.last().copied(),
            }?;
            // Trim leading whitespace so a deleted sentence does not leave one.
            let lead = text[span.0..span.1].len() - text[span.0..span.1].trim_start().len();
            Some((span.0 + lead, span.1))
        }
        Unit::Line | Unit::Paragraph => {
            let sep = if unit == Unit::Line { "\n" } else { "\n\n" };
            let mut spans = Vec::new();
            let mut pos = 0usize;
            for part in text.split(sep) {
                spans.push((pos, pos + part.len()));
                pos += part.len() + sep.len();
            }
            match which {
                Which::First => spans.first().copied(),
                Which::Last | Which::This => spans.last().copied(),
            }
        }
    }
}

fn parse_scope(phrase: &str) -> Option<(Which, Unit)> {
    let unit = if phrase.contains("sentence") {
        Unit::Sentence
    } else if phrase.contains("paragraph") {
        Unit::Paragraph
    } else if phrase.contains("line") {
        Unit::Line
    } else if phrase.contains("word") {
        Unit::Word
    } else {
        return None;
    };
    let which = if phrase.contains("first") {
        Which::First
    } else if phrase.contains("last") {
        Which::Last
    } else if phrase.contains("this") || phrase.contains("that") {
        Which::This
    } else {
        return None;
    };
    Some((which, unit))
}

// ------------------------------------------------------------ extra intents

#[derive(Debug, Clone, PartialEq)]
enum Extra {
    DeleteScope(Which, Unit),
    RecaseScope(Which, Unit, &'static str),
    Punctuate(char),
    PunctuateAfter(char, String),
    Wrap(&'static str, &'static str),
    IdentCase(&'static str),
    JoinLines,
    SplitSentencesToLines,
    NumberLines,
    BulletLines,
    Undo,
}

fn parse_extra(utterance: &str) -> Option<Extra> {
    let u = utterance.trim().to_lowercase();

    if matches!(
        u.as_str(),
        "undo that" | "undo" | "never mind" | "nevermind" | "go back to the original"
    ) {
        return Some(Extra::Undo);
    }

    // punctuation: "add a period (at the end)", "add a comma after today"
    const PUNCT: [(&str, char); 6] = [
        ("period", '.'),
        ("full stop", '.'),
        ("comma", ','),
        ("question mark", '?'),
        ("exclamation mark", '!'),
        ("colon", ':'),
    ];
    if u.starts_with("add ") || u.starts_with("put ") || u.starts_with("insert ") {
        for (name, ch) in PUNCT {
            if u.contains(name) {
                if let Some(idx) = u.find(" after ") {
                    let anchor = u[idx + " after ".len()..].trim().to_string();
                    if !anchor.is_empty() {
                        return Some(Extra::PunctuateAfter(ch, anchor));
                    }
                }
                return Some(Extra::Punctuate(ch));
            }
        }
    }

    // wrapping
    if u.starts_with("wrap ") || u.starts_with("put ") || u.starts_with("surround ") {
        if u.contains("quote") {
            return Some(Extra::Wrap("\"", "\""));
        }
        if u.contains("backtick") || u.contains("code") {
            return Some(Extra::Wrap("`", "`"));
        }
        if u.contains("paren") || u.contains("bracket") {
            return Some(Extra::Wrap("(", ")"));
        }
        if u.contains("asterisk") || u.contains("bold") {
            return Some(Extra::Wrap("**", "**"));
        }
    }

    // identifier casing
    for (kw, style) in [
        ("snake case", "snake"),
        ("camel case", "camel"),
        ("kebab case", "kebab"),
        ("screaming snake", "screaming"),
    ] {
        if u.contains(kw) {
            return Some(Extra::IdentCase(style));
        }
    }

    // line operations
    if u.contains("join") && u.contains("line") {
        return Some(Extra::JoinLines);
    }
    if u.contains("split") && (u.contains("line") || u.contains("sentence")) {
        return Some(Extra::SplitSentencesToLines);
    }
    if u.contains("number") && u.contains("line") {
        return Some(Extra::NumberLines);
    }
    if u.contains("bullet") || (u.contains("bulleted") && u.contains("list")) {
        return Some(Extra::BulletLines);
    }

    // scoped delete: "delete the last sentence"
    for head in ["delete ", "remove ", "get rid of ", "scratch "] {
        if let Some(rest) = u.strip_prefix(head) {
            if let Some((w, unit)) = parse_scope(rest) {
                return Some(Extra::DeleteScope(w, unit));
            }
            if rest.trim() == "that" || rest.trim() == "this" {
                return Some(Extra::DeleteScope(Which::This, Unit::Selection));
            }
        }
    }

    // scoped recase: "capitalize the first word", "make the last line
    // title case", "uppercase the first letter"
    let style = if u.contains("title case") {
        Some("title")
    } else if u.contains("capitalize") || u.contains("uppercase") || u.contains("upper case") {
        Some("upper")
    } else if u.contains("lowercase") || u.contains("lower case") {
        Some("lower")
    } else {
        None
    };
    if let Some(style) = style {
        if u.contains("first letter") || u.contains("first character") {
            return Some(Extra::RecaseScope(Which::First, Unit::Word, "firstchar"));
        }
        if let Some((w, unit)) = parse_scope(&u) {
            // "capitalize the first word" means title-case that word, not
            // SHOUT it. Only an explicit "uppercase/all caps" means upper.
            let style = if style == "upper" && u.contains("capitalize") && !u.contains("all caps") {
                "firstchar"
            } else {
                style
            };
            return Some(Extra::RecaseScope(w, unit, style));
        }
    }

    None
}

// ------------------------------------------------------------------- apply

fn splice(text: &str, span: (usize, usize), replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..span.0]);
    out.push_str(replacement);
    out.push_str(&text[span.1..]);
    // Deleting leaves doubled spaces; collapse them like the shipped
    // Delete path already does.
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn recase(s: &str, style: &str) -> String {
    match style {
        "upper" => s.to_uppercase(),
        "lower" => s.to_lowercase(),
        "title" => s
            .split(' ')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        "firstchar" => {
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
        _ => s.to_string(),
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            // Fold case FIRST, then filter. Doing it the other way round
            // lets a character pass the alphanumeric test and then expand
            // into something that would not: `İ` is alphanumeric, but it
            // lowercases to `i` + COMBINING DOT ABOVE (U+0307), and the
            // combining mark is category Mn. That leaked a stray mark into
            // every generated identifier. Found by fuzzing, not by reading.
            w.chars()
                .flat_map(|c| c.to_lowercase())
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn apply_extra(text: &str, e: &Extra) -> Option<String> {
    Some(match e {
        Extra::Undo => return None, // handled by the undo ring, not a text edit
        Extra::DeleteScope(w, unit) => {
            let span = resolve_scope(text, *w, *unit)?;
            splice(text, span, "")
        }
        Extra::RecaseScope(w, unit, style) => {
            let span = resolve_scope(text, *w, *unit)?;
            let piece = recase(&text[span.0..span.1], style);
            let mut out = String::new();
            out.push_str(&text[..span.0]);
            out.push_str(&piece);
            out.push_str(&text[span.1..]);
            out
        }
        Extra::Punctuate(ch) => {
            let t = text.trim_end();
            let t = t.trim_end_matches(['.', ',', '!', '?', ':']);
            format!("{t}{ch}")
        }
        Extra::PunctuateAfter(ch, anchor) => {
            // Byte offsets found in a LOWERCASED copy are meaningless
            // against the original: `to_lowercase` changes byte length per
            // character (İ grows, Ǆ shrinks), so `lower.find(..)` can point
            // into the middle of a char in `text` and slicing panics. The
            // workspace fuzz suite caught exactly this class in the shipped
            // crate, and the prototype reproduced it: seed 0x577702c8129ff232,
            // target "ẞǄ🦀éσıﬀ?", anchor "ẞǆ\t".
            //
            // Match case-insensitively over the ORIGINAL's char boundaries
            // instead, comparing lowercase-folded char sequences.
            let anchor_lc: Vec<char> = anchor.to_lowercase().chars().collect();
            if anchor_lc.is_empty() {
                return None;
            }
            let mut end_byte = None;
            'outer: for (start, _) in text.char_indices() {
                let mut ai = 0usize;
                for (off, c) in text[start..].char_indices() {
                    for lc in c.to_lowercase() {
                        match anchor_lc.get(ai) {
                            Some(&want) if want == lc => ai += 1,
                            _ => continue 'outer,
                        }
                    }
                    if ai == anchor_lc.len() {
                        end_byte = Some(start + off + c.len_utf8());
                        break 'outer;
                    }
                }
                // Ran out of text before matching the whole anchor.
                continue 'outer;
            }
            let end = end_byte?;
            // Do not stack punctuation: "add a comma after today" against
            // "...today. The" must not produce "today,.".
            let existing = text[end..]
                .chars()
                .next()
                .filter(|c| matches!(c, '.' | ',' | '!' | '?' | ':' | ';'));
            let tail_start = end + existing.map_or(0, char::len_utf8);
            format!("{}{}{}", &text[..end], ch, &text[tail_start..])
        }
        Extra::Wrap(open, close) => format!("{open}{}{close}", text.trim()),
        Extra::IdentCase(style) => {
            let ws = words(text);
            if ws.is_empty() {
                return None;
            }
            match *style {
                "snake" => ws.join("_"),
                "kebab" => ws.join("-"),
                "screaming" => ws.join("_").to_uppercase(),
                "camel" => {
                    let mut out = ws[0].clone();
                    for w in &ws[1..] {
                        let mut c = w.chars();
                        if let Some(f) = c.next() {
                            out.push_str(&f.to_uppercase().collect::<String>());
                            out.push_str(c.as_str());
                        }
                    }
                    out
                }
                _ => return None,
            }
        }
        Extra::JoinLines => text.lines().map(str::trim).collect::<Vec<_>>().join(" "),
        Extra::SplitSentencesToLines => {
            let mut out = Vec::new();
            let mut start = 0;
            let chars: Vec<(usize, char)> = text.char_indices().collect();
            for (i, (b, c)) in chars.iter().enumerate() {
                if matches!(c, '.' | '!' | '?')
                    && chars
                        .get(i + 1)
                        .map(|(_, n)| n.is_whitespace())
                        .unwrap_or(true)
                {
                    let end = b + c.len_utf8();
                    out.push(text[start..end].trim().to_string());
                    start = end;
                }
            }
            if !text[start..].trim().is_empty() {
                out.push(text[start..].trim().to_string());
            }
            out.join("\n")
        }
        Extra::NumberLines => split_units(text)
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}. {}", i + 1, l))
            .collect::<Vec<_>>()
            .join("\n"),
        Extra::BulletLines => split_units(text)
            .iter()
            .map(|u| format!("- {u}"))
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// The units a list operation should act on: existing lines when the text
/// already has them, sentences otherwise. Dictated prose is one long line,
/// so "turn this into bullet points" has to mean sentences there or it
/// produces a one-item list, which is what the model-free path must not do.
fn split_units(text: &str) -> Vec<String> {
    if text.contains('\n') {
        return text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
    }
    let mut v = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (i, (b, c)) in chars.iter().enumerate() {
        if matches!(c, '.' | '!' | '?')
            && chars
                .get(i + 1)
                .map(|(_, n)| n.is_whitespace())
                .unwrap_or(true)
        {
            let end = b + c.len_utf8();
            v.push(text[start..end].trim().to_string());
            start = end;
        }
    }
    if !text[start..].trim().is_empty() {
        v.push(text[start..].trim().to_string());
    }
    v.retain(|s| !s.is_empty());
    v
}

// -------------------------------------------------------------------- main

/// A dictated paragraph: three sentences, no line breaks, inconsistent
/// capitalization. This is what the recognizer actually produces, and the
/// absence of line breaks is why list operations must fall back to sentences.
const SAMPLE: &str = "It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon";

/// What the pre-pass should do with each utterance.
enum Expect {
    /// Handled deterministically, producing exactly this text.
    Text(&'static str),
    /// Recognised, but resolved by the undo ring rather than a text edit.
    UndoRing,
    /// Correctly left for a language model: genuinely open-ended.
    Model,
}

/// The 21 commands the shipped parser gets wrong or punts, each paired with
/// the exact output a correct implementation produces, plus 4 controls that
/// must still escalate.
///
/// Expected strings are spelled out rather than computed, so a refactor that
/// changes behaviour fails here instead of quietly agreeing with itself.
fn cases() -> Vec<(&'static str, Expect)> {
    use Expect::*;
    vec![
        // --- shipped parser returns Freeform (feature simply missing) ---
        (
            "capitalize the first word",
            Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon"),
        ),
        (
            "join these lines",
            Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon"),
        ),
        (
            "split this into two lines",
            Text("It is really quite important that we should try to make sure the deploy happens today.\nThe customers might possibly be quite upset.\nwe should tell them soon"),
        ),
        (
            "make this a bulleted list",
            Text("- It is really quite important that we should try to make sure the deploy happens today.\n- The customers might possibly be quite upset.\n- we should tell them soon"),
        ),
        (
            "turn this into bullet points",
            Text("- It is really quite important that we should try to make sure the deploy happens today.\n- The customers might possibly be quite upset.\n- we should tell them soon"),
        ),
        (
            "number these lines",
            Text("1. It is really quite important that we should try to make sure the deploy happens today.\n2. The customers might possibly be quite upset.\n3. we should tell them soon"),
        ),
        (
            "wrap this in quotes",
            Text("\"It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon\""),
        ),
        (
            "wrap that in backticks",
            Text("`It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon`"),
        ),
        (
            "make it snake case",
            Text("it_is_really_quite_important_that_we_should_try_to_make_sure_the_deploy_happens_today_the_customers_might_possibly_be_quite_upset_we_should_tell_them_soon"),
        ),
        (
            "make it camel case",
            Text("itIsReallyQuiteImportantThatWeShouldTryToMakeSureTheDeployHappensTodayTheCustomersMightPossiblyBeQuiteUpsetWeShouldTellThemSoon"),
        ),
        (
            "make it kebab case",
            Text("it-is-really-quite-important-that-we-should-try-to-make-sure-the-deploy-happens-today-the-customers-might-possibly-be-quite-upset-we-should-tell-them-soon"),
        ),
        ("undo that", UndoRing),
        ("never mind", UndoRing),
        ("go back to the original", UndoRing),
        // --- shipped parser mis-parses these LITERALLY (silent wrong edit) ---
        (
            "delete the last sentence",
            Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset."),
        ),
        (
            "remove the first sentence",
            Text("The customers might possibly be quite upset. we should tell them soon"),
        ),
        (
            "delete the last word",
            Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them"),
        ),
        (
            // Shipped parser SHOUTS the whole field for this.
            "uppercase the first letter",
            Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon"),
        ),
        (
            // Shipped parser appends the words "a period at the end".
            "add a period at the end",
            Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon."),
        ),
        (
            "add a comma after today",
            Text("It is really quite important that we should try to make sure the deploy happens today, The customers might possibly be quite upset. we should tell them soon"),
        ),
        (
            "add a question mark",
            Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon?"),
        ),
        // --- controls: genuinely open-ended, must reach a model ---
        ("tighten this up", Model),
        ("make it more formal", Model),
        ("summarize this", Model),
        ("translate this to spanish", Model),
    ]
}

/// Run the pre-pass over `cases()` and check every result against its
/// expectation. Returns (handled, escalated, failures, mean_ns).
fn run(verbose: bool) -> (usize, usize, Vec<String>, f64) {
    let all = cases();
    let mut handled = 0;
    let mut escalated = 0;
    let mut failures = Vec::new();
    let mut total_ns = 0u128;

    for (utt, expect) in &all {
        let t = Instant::now();
        let extra = parse_extra(utt);
        let out = extra.as_ref().and_then(|e| apply_extra(SAMPLE, e));
        total_ns += t.elapsed().as_nanos();

        match (expect, &extra, &out) {
            (Expect::UndoRing, Some(Extra::Undo), _) => {
                handled += 1;
                if verbose {
                    println!("{utt:<34} [undo ring, no text edit]");
                }
            }
            (Expect::Text(want), Some(_), Some(got)) => {
                if got == want {
                    handled += 1;
                    if verbose {
                        println!("{utt:<34} {}", got.replace('\n', " | "));
                    }
                } else {
                    failures.push(format!("{utt:?}\n     want: {want:?}\n      got: {got:?}"));
                }
            }
            (Expect::Model, None, _) => {
                escalated += 1;
                if verbose {
                    println!("{utt:<34} -> model (correct: genuinely open-ended)");
                }
            }
            (Expect::Text(_), None, _) | (Expect::UndoRing, None, _) => {
                failures.push(format!("{utt:?} was not recognised by the pre-pass"));
            }
            (Expect::Model, Some(_), _) => {
                failures.push(format!(
                    "{utt:?} was captured by the pre-pass but is genuinely open-ended"
                ));
            }
            (_, Some(_), None) => {
                failures.push(format!("{utt:?} parsed but produced no output"));
            }
            // Everything else is a mismatch between what was expected and
            // what the pre-pass did; report rather than silently pass.
            (_, got, _) => {
                failures.push(format!("{utt:?} produced an unexpected parse: {got:?}"));
            }
        }
    }
    (
        handled,
        escalated,
        failures,
        total_ns as f64 / all.len() as f64,
    )
}

fn main() {
    println!("{:<34} deterministic result", "utterance");
    println!("{}", "-".repeat(120));
    let (handled, escalated, failures, mean_ns) = run(true);

    println!("\n{}", "=".repeat(120));
    println!("cases:                 {}", cases().len());
    println!("handled without model: {handled}");
    println!("escalated to model:    {escalated}");
    println!("mean parse+apply:      {:.1}us", mean_ns / 1000.0);

    if failures.is_empty() {
        println!("\nall expectations met.");
    } else {
        println!("\n{} FAILURES:", failures.len());
        for f in &failures {
            println!("  - {f}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headline claim of docs/investigations/edit-intent.md: the pre-pass
    /// resolves 21 of the 25 commands the shipped parser gets wrong or punts,
    /// and correctly leaves the other 4 to a model.
    #[test]
    fn resolves_twentyone_of_twentyfive_without_a_model() {
        let (handled, escalated, failures, _) = run(false);
        assert!(failures.is_empty(), "unmet expectations:\n{failures:#?}");
        assert_eq!(handled, 21, "deterministic coverage changed");
        assert_eq!(escalated, 4, "escalation set changed");
    }

    /// Sentence scoping must not be fooled by a decimal point or an
    /// abbreviation, which is the classic failure of naive splitting. These
    /// are the cases the real implementation has to handle before shipping;
    /// they are asserted as KNOWN-WRONG so the limitation is visible rather
    /// than discovered by a user losing text.
    #[test]
    fn known_limitation_sentence_splitting_is_naive() {
        let text = "Ship at 3.5 percent. Tell Dr. Smith we are done";
        let span = resolve_scope(text, Which::Last, Unit::Sentence)
            .expect("a last sentence should always resolve");
        let last = &text[span.0..span.1];
        // A correct splitter yields "Tell Dr. Smith we are done". This one
        // breaks after "Dr." because the period is followed by a space.
        assert_eq!(
            last, "Smith we are done",
            "if this now passes, the splitter was improved: update the doc's \
             known-limitations note"
        );
    }

    /// Deleting a scope must not leave doubled spaces or a dangling space
    /// before punctuation, the same cleanup the shipped Delete path does.
    #[test]
    fn scoped_delete_cleans_whitespace() {
        let text = "one two three. four five. six seven";
        let span = resolve_scope(text, Which::First, Unit::Sentence).unwrap();
        let got = splice(text, span, "");
        assert_eq!(got, "four five. six seven");
    }

    /// Identifier casing must drop punctuation, or "snake case" produces
    /// `today.the` and is unusable as an identifier. This is exactly where
    /// Qwen3-1.7B failed in the head-to-head.
    #[test]
    fn identifier_casing_drops_punctuation() {
        let text = "ship today. tell them";
        assert_eq!(
            apply_extra(text, &Extra::IdentCase("snake")).unwrap(),
            "ship_today_tell_them"
        );
        assert_eq!(
            apply_extra(text, &Extra::IdentCase("camel")).unwrap(),
            "shipTodayTellThem"
        );
    }

    /// Punctuation must never stack: "add a comma after X" where X is
    /// already followed by a period must replace it, not append to it.
    #[test]
    fn punctuation_does_not_stack() {
        let text = "we ship today. they wait";
        let got = apply_extra(text, &Extra::PunctuateAfter(',', "today".into())).unwrap();
        assert_eq!(got, "we ship today, they wait");
        assert!(!got.contains(",."), "punctuation stacked: {got:?}");
    }

    /// An anchor that is not present must fail rather than silently editing
    /// somewhere arbitrary.
    #[test]
    fn absent_anchor_reports_no_match() {
        assert!(
            apply_extra("we ship today", &Extra::PunctuateAfter(',', "zebra".into())).is_none(),
            "an absent anchor must not produce an edit"
        );
    }

    /// Non-ASCII input must not panic on byte-boundary slicing, the same
    /// class of bug the shipped crate's fuzz suite found.
    #[test]
    fn non_ascii_does_not_panic() {
        for text in [
            "İstanbul ΣΊΣΥΦΟΣ. Straße Ǆ",
            "日本語の文。もう一つ",
            "emoji 🎉 sentence. another 🚀 one",
        ] {
            for utt in [
                "delete the last sentence",
                "delete the last word",
                "make it snake case",
                "add a period at the end",
                "wrap this in quotes",
                "turn this into bullet points",
            ] {
                if let Some(e) = parse_extra(utt) {
                    let _ = apply_extra(text, &e);
                }
            }
        }
    }

    /// Empty and whitespace-only targets must not panic or fabricate text.
    #[test]
    fn degenerate_targets_are_safe() {
        for text in ["", "   ", "\n"] {
            for utt in [
                "delete the last sentence",
                "make it snake case",
                "undo that",
            ] {
                if let Some(e) = parse_extra(utt) {
                    let _ = apply_extra(text, &e);
                }
            }
        }
    }
}

/// Property tests, mirroring `tests/tests/fuzz_edits.rs`.
///
/// The hand-picked tests above prove the prototype does the right thing on
/// inputs I thought of. That is exactly the standard the shipped crate is
/// NOT held to: the workspace fuzz suite found both a panic and a silent
/// over-edit in `edit-intent` that no hand-written test caught. A prototype
/// whose whole selling point is "predictable, no model needed" has to clear
/// the same bar before anyone acts on it, because scope resolution does far
/// more byte-offset arithmetic than the shipped parser does.
///
/// Same deterministic xorshift PRNG and same hostile alphabet as the
/// workspace suite, so a failure reproduces from the printed state.
#[cfg(test)]
mod fuzz {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Multi-byte, case-length-changing, combining, zero-width, RTL, and
    /// sentence-punctuation pieces: the characters that break byte slicing.
    const ALPHABET: &[&str] = &[
        "a",
        "b",
        " ",
        ".",
        "!",
        "?",
        "\n",
        "\t",
        "İ",
        "ı",
        "ß",
        "ẞ",
        "Σ",
        "σ",
        "ς",
        "é",
        "e\u{301}",
        "日",
        "🦀",
        "👨\u{200d}👩\u{200d}👧",
        "\u{200b}",
        "م",
        "ر",
        "Ǆ",
        "ǅ",
        "ǆ",
        "ﬀ",
    ];

    fn random_string(rng: &mut Rng, max_pieces: usize) -> String {
        let pieces = rng.below(max_pieces + 1);
        (0..pieces)
            .map(|_| ALPHABET[rng.below(ALPHABET.len())])
            .collect()
    }

    /// Every command shape the pre-pass recognises, plus an anchor drawn
    /// from the hostile alphabet so `PunctuateAfter` gets fuzzed too.
    fn random_utterance(rng: &mut Rng) -> String {
        let anchor = random_string(rng, 3);
        let forms = [
            "delete the last sentence".to_string(),
            "remove the first sentence".to_string(),
            "delete the last word".to_string(),
            "capitalize the first word".to_string(),
            "uppercase the first letter".to_string(),
            "make the first line title case".to_string(),
            "add a period at the end".to_string(),
            format!("add a comma after {anchor}"),
            "wrap this in quotes".to_string(),
            "make it snake case".to_string(),
            "make it camel case".to_string(),
            "turn this into bullet points".to_string(),
            "number these lines".to_string(),
            "join these lines".to_string(),
            "split this into two lines".to_string(),
            "undo that".to_string(),
            random_string(rng, 5),
        ];
        forms[rng.below(forms.len())].clone()
    }

    #[test]
    fn arbitrary_unicode_never_panics() {
        let mut rng = Rng(0x5eed_1001);
        for i in 0..20_000 {
            let state = rng.0;
            let target = random_string(&mut rng, 12);
            let utterance = random_utterance(&mut rng);
            let result = std::panic::catch_unwind(|| {
                if let Some(e) = parse_extra(&utterance) {
                    let _ = apply_extra(&target, &e);
                }
            });
            assert!(
                result.is_ok(),
                "panic at iteration {i} (rng state {state:#x})\n\
                 target: {target:?}\nutterance: {utterance:?}"
            );
        }
    }

    /// Every scope span must land on real character boundaries and be
    /// orderable. A span that splits a multi-byte char would panic on
    /// slicing; one measured against the wrong string would silently edit
    /// the wrong region, which is the over-edit class the workspace suite
    /// exists to catch.
    #[test]
    fn resolved_scopes_are_valid_char_boundaries() {
        let mut rng = Rng(0x5eed_1002);
        for _ in 0..20_000 {
            let text = random_string(&mut rng, 14);
            for which in [Which::First, Which::Last, Which::This] {
                for unit in [
                    Unit::Sentence,
                    Unit::Word,
                    Unit::Line,
                    Unit::Paragraph,
                    Unit::Selection,
                ] {
                    if let Some((start, end)) = resolve_scope(&text, which, unit) {
                        assert!(start <= end, "inverted span {start}..{end} in {text:?}");
                        assert!(end <= text.len(), "span past end in {text:?}");
                        assert!(
                            text.is_char_boundary(start) && text.is_char_boundary(end),
                            "span {start}..{end} splits a char in {text:?}"
                        );
                    }
                }
            }
        }
    }

    /// A scoped delete must only ever REMOVE content. Whitespace collapsing
    /// means the output is not a literal substring, so the invariant is
    /// checked on non-whitespace characters, which a delete must never add.
    #[test]
    fn scoped_delete_never_adds_content() {
        let mut rng = Rng(0x5eed_1003);
        for _ in 0..20_000 {
            let text = random_string(&mut rng, 14);
            for unit in [Unit::Sentence, Unit::Word, Unit::Line] {
                let e = Extra::DeleteScope(Which::Last, unit);
                if let Some(after) = apply_extra(&text, &e) {
                    let before_n = text.chars().filter(|c| !c.is_whitespace()).count();
                    let after_n = after.chars().filter(|c| !c.is_whitespace()).count();
                    assert!(
                        after_n <= before_n,
                        "delete ADDED content: {text:?} -> {after:?}"
                    );
                }
            }
        }
    }

    /// Identifier casing must emit only lowercase alphanumerics and its own
    /// separator, or the result is not a usable identifier. This is the
    /// exact property Qwen3-1.7B violated by keeping sentence periods.
    #[test]
    fn identifier_casing_emits_only_identifier_characters() {
        let mut rng = Rng(0x5eed_1004);
        for _ in 0..10_000 {
            let text = random_string(&mut rng, 10);
            for (style, sep) in [("snake", '_'), ("kebab", '-')] {
                if let Some(out) = apply_extra(&text, &Extra::IdentCase(style)) {
                    assert!(
                        out.chars().all(|c| c.is_alphanumeric() || c == sep),
                        "{style} produced non-identifier chars: {out:?} from {text:?}"
                    );
                }
            }
        }
    }

    /// Punctuation must never stack, for any anchor, on any input.
    #[test]
    fn punctuation_never_stacks() {
        let mut rng = Rng(0x5eed_1005);
        for _ in 0..10_000 {
            let text = random_string(&mut rng, 12);
            if let Some(out) = apply_extra(&text, &Extra::Punctuate('.')) {
                assert!(
                    !out.ends_with(".."),
                    "stacked terminal punctuation: {out:?}"
                );
            }
        }
    }
}
