//! Vocabulary: user terms that bias recognition and post-correct output.
//!
//! Unlimited size, plain text, one entry per line, because "your data stays
//! yours, in a file you can read" is the product position (Aqua Voice caps this at
//! 800 entries on Pro and 5 on free; we cap it at nothing).
//!
//! Line grammar (docs/configuration.md has the user-facing version):
//!
//! ```text
//! # comment
//! kubectl                          # bias-only: prefer this word
//! dash dash force -> --force       # replacement rule
//! my address -> 12 Elm St          # auto-expansion (same mechanism)
//! Kubernetes -> Kubernetes [case]  # flag: preserve the written casing
//! ```
//!
//! Correction has two engines, applied in order:
//! 1. **Exact replacement**: the spoken form, matched case-insensitively on
//!    word boundaries, becomes the written form.
//! 2. **Fuzzy bias correction**: a word (or adjacent word pair) that *sounds
//!    like* a bias term, within a similarity threshold, is rewritten to it.
//!    This is how "cube cuddle" becomes "kubectl" without an entry for every
//!    possible mangling.

use crate::fuzzy;

/// Per-entry behavior flags, written as `[flag]` suffixes on the line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryFlags {
    /// Keep the written form's casing exactly, even mid-sentence where the
    /// formatter would otherwise lowercase it ("GitHub", not "github").
    pub preserve_case: bool,
    /// Strip punctuation adjacent to the match before replacing, for terms
    /// the recognizer tends to end with a period ("kubectl." -> "kubectl").
    pub strip_punctuation: bool,
}

/// One vocabulary entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What the user says (or what the recognizer should be biased toward).
    pub spoken: String,
    /// What lands in the text. Equal to `spoken` for bias-only entries.
    pub written: String,
    pub flags: EntryFlags,
    /// Line number in the source file, for error reporting and provenance.
    pub line: usize,
}

impl Entry {
    /// Bias-only entries have no rewrite of their own; they exist to feed
    /// the recognizer's bias list and the fuzzy corrector.
    pub fn is_bias_only(&self) -> bool {
        self.spoken == self.written
    }
}

/// A parse problem in a vocabulary file. Warnings, never fatal: one bad line
/// must not disable the other ten thousand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabWarning {
    pub line: usize,
    pub message: String,
}

/// A parsed vocabulary set (one file).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vocabulary {
    pub entries: Vec<Entry>,
    pub warnings: Vec<VocabWarning>,
}

/// Similarity threshold for fuzzy correction. High enough that ordinary
/// English does not get "corrected" into jargon (a false positive rewrites
/// text the user meant), low enough to catch real recognizer manglings. The
/// realistic-mangle tests below pin this behavior on both sides.
const FUZZY_THRESHOLD: f64 = 0.63;

/// Words shorter than this are never fuzzy-corrected: "cat" is one edit from
/// too many things.
const MIN_FUZZY_LEN: usize = 5;

/// Load the named vocabulary sets from the vocabulary folder.
///
/// A set named `team-names` is the file `team-names.txt` beside the user's
/// config. Missing files are skipped rather than reported as errors: the
/// config names sets by intent, and a set the user has not written yet is a
/// normal state, not a misconfiguration.
///
/// Returns the merged vocabulary, so callers get one object regardless of how
/// many sets are active, and `None` when nothing is active. `None` rather than
/// an empty `Vocabulary` so the caller can skip the correction pass entirely
/// on the overwhelmingly common path where no sets are configured.
pub fn load_sets(names: &[String]) -> Option<Vocabulary> {
    if names.is_empty() {
        return None;
    }
    let dir = crate::vocabulary_dir()?;
    let loaded: Vec<Vocabulary> = names
        .iter()
        .filter_map(|name| {
            // Reject anything that could escape the vocabulary folder. The
            // names come from a config file, which is user-editable, and a
            // set called "../../.ssh/id_rsa" must not be readable as a
            // vocabulary.
            if name.contains('/') || name.contains('\\') || name.contains("..") {
                return None;
            }
            std::fs::read_to_string(dir.join(format!("{name}.txt"))).ok()
        })
        .map(|text| Vocabulary::parse(&text))
        .collect();
    if loaded.is_empty() {
        return None;
    }
    let refs: Vec<&Vocabulary> = loaded.iter().collect();
    Some(Vocabulary::merge(&refs))
}

impl Vocabulary {
    /// Parse a vocabulary file. Never fails: bad lines become warnings.
    pub fn parse(text: &str) -> Vocabulary {
        let mut vocab = Vocabulary::default();
        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Flags first, so "a -> b [case]" parses the arrow correctly.
            let (line, flags) = parse_flags(line, line_no, &mut vocab.warnings);
            let (spoken, written) = match line.split_once("->") {
                Some((s, w)) => (s.trim(), w.trim()),
                None => (line, line),
            };
            if spoken.is_empty() || written.is_empty() {
                vocab.warnings.push(VocabWarning {
                    line: line_no,
                    message: format!("incomplete rule \"{raw}\": both sides of \"->\" need text"),
                });
                continue;
            }
            vocab.entries.push(Entry {
                spoken: spoken.to_string(),
                written: written.to_string(),
                flags,
                line: line_no,
            });
        }
        vocab
    }

    /// Merge several sets into one working vocabulary. Later sets win on a
    /// duplicate spoken form, mirroring config layer precedence, so a
    /// profile-activated set can override a global one.
    pub fn merge(sets: &[&Vocabulary]) -> Vocabulary {
        let mut merged = Vocabulary::default();
        for set in sets {
            for entry in &set.entries {
                if let Some(existing) = merged
                    .entries
                    .iter_mut()
                    .find(|e| e.spoken.eq_ignore_ascii_case(&entry.spoken))
                {
                    *existing = entry.clone();
                } else {
                    merged.entries.push(entry.clone());
                }
            }
        }
        merged
    }

    /// Terms to feed the recognizer's bias list: every spoken form, plus
    /// written forms that are single words (multi-word expansions are not
    /// bias candidates; the recognizer works word-by-word).
    pub fn bias_terms(&self) -> Vec<&str> {
        let mut terms: Vec<&str> = Vec::new();
        for e in &self.entries {
            terms.push(e.spoken.as_str());
            if !e.written.contains(' ') && e.written != e.spoken {
                terms.push(e.written.as_str());
            }
        }
        terms
    }

    /// Post-correct recognizer output: exact replacement rules first, then
    /// fuzzy bias correction. Returns the corrected text and a record of
    /// what changed, so the UI's diff chip (docs/ux/05: capture at the point
    /// of failure) can show its work.
    pub fn correct(&self, text: &str) -> (String, Vec<Correction>) {
        let mut corrections = Vec::new();
        let text = self.apply_replacements(text, &mut corrections);
        let text = self.apply_fuzzy(&text, &mut corrections);
        (text, corrections)
    }

    /// Exact rules: replace spoken-form occurrences (case-insensitive, word
    /// boundaries) with the written form. Longest spoken form first, so
    /// "dash dash force" wins over a hypothetical "dash dash" rule.
    fn apply_replacements(&self, text: &str, log: &mut Vec<Correction>) -> String {
        let mut rules: Vec<&Entry> = self.entries.iter().filter(|e| !e.is_bias_only()).collect();
        rules.sort_by_key(|e| std::cmp::Reverse(e.spoken.len()));

        let mut out = text.to_string();
        for rule in rules {
            let mut result = String::with_capacity(out.len());
            let mut rest = out.as_str();
            let needle = rule.spoken.to_lowercase();
            loop {
                let hay = rest.to_lowercase();
                let Some(pos) = hay.find(&needle) else {
                    result.push_str(rest);
                    break;
                };
                let end = pos + needle.len();
                // Word-boundary check: the match must not sit inside a word,
                // or "cat -> feline" would maul "concatenate".
                let before_ok =
                    pos == 0 || !rest[..pos].chars().next_back().unwrap().is_alphanumeric();
                let after_ok =
                    end == rest.len() || !rest[end..].chars().next().unwrap().is_alphanumeric();
                if before_ok && after_ok {
                    result.push_str(&rest[..pos]);
                    let mut written = rule.written.clone();
                    if !rule.flags.preserve_case {
                        written = match_case(&rest[pos..end], &written);
                    }
                    log.push(Correction {
                        from: rest[pos..end].to_string(),
                        to: written.clone(),
                        rule_line: rule.line,
                        kind: CorrectionKind::Replacement,
                    });
                    result.push_str(&written);
                    if rule.flags.strip_punctuation {
                        // Drop punctuation the recognizer glued onto the term.
                        rest = rest[end..].trim_start_matches(['.', ',', '!', '?']);
                        continue;
                    }
                } else {
                    result.push_str(&rest[..end]);
                }
                rest = &rest[end..];
            }
            out = result;
        }
        out
    }

    /// Fuzzy correction against bias terms: single words and adjacent word
    /// pairs (the recognizer splits unknown terms into two real words, as in
    /// "cube cuddle"), keeping whichever candidate sounds closest.
    fn apply_fuzzy(&self, text: &str, log: &mut Vec<Correction>) -> String {
        let bias: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.spoken.chars().count() >= MIN_FUZZY_LEN)
            .collect();
        if bias.is_empty() {
            return text.to_string();
        }
        // Words that ARE a vocabulary term, verbatim. A pair whose first
        // word is already correct must not be fuzzy-merged: "GitHub is"
        // sounds a lot like "github", but the user said two words and got
        // both right.
        let exact_terms: Vec<String> = self
            .entries
            .iter()
            .flat_map(|e| [e.spoken.to_lowercase(), e.written.to_lowercase()])
            .collect();

        // Tokenize preserving separators so reassembly is lossless.
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut out: Vec<String> = Vec::with_capacity(words.len());
        let mut i = 0;
        while i < words.len() {
            // Try the pair first: a two-word mangle of one term must beat a
            // one-word partial match ("cube" alone should not become
            // "kubectl" if "cuddle" follows and the pair matches better).
            let word_is_exact = |w: &str| {
                exact_terms
                    .iter()
                    .any(|t| t == &strip_word(w).to_lowercase())
            };
            let pair = if i + 1 < words.len()
                && !word_is_exact(words[i])
                && !word_is_exact(words[i + 1])
            {
                Some(format!(
                    "{} {}",
                    strip_word(words[i]),
                    strip_word(words[i + 1])
                ))
            } else {
                None
            };
            let single = strip_word(words[i]);

            let best_pair = pair.as_deref().and_then(|p| best_match(&bias, p));
            let best_single = best_match(&bias, &single);

            match (best_pair, best_single) {
                (Some((entry, ps)), single_result)
                    if single_result.is_none_or(|(_, ss)| ps >= ss) =>
                {
                    let original = format!("{} {}", words[i], words[i + 1]);
                    let replacement = carry_punctuation(words[i + 1], &entry.written, entry.flags);
                    log.push(Correction {
                        from: original,
                        to: replacement.clone(),
                        rule_line: entry.line,
                        kind: CorrectionKind::Fuzzy,
                    });
                    out.push(replacement);
                    i += 2;
                }
                (_, Some((entry, _))) => {
                    let replacement = carry_punctuation(words[i], &entry.written, entry.flags);
                    // A word that already matches its own entry is not a
                    // correction. The exact-replacement pass runs first, so
                    // its output arrives here looking like a fuzzy candidate
                    // for the term it just produced, and logging that reports
                    // "BROWNIE -> BROWNIE" to a user who would reasonably
                    // wonder what changed.
                    if replacement != words[i] {
                        log.push(Correction {
                            from: words[i].to_string(),
                            to: replacement.clone(),
                            rule_line: entry.line,
                            kind: CorrectionKind::Fuzzy,
                        });
                    }
                    out.push(replacement);
                    i += 1;
                }
                _ => {
                    out.push(words[i].to_string());
                    i += 1;
                }
            }
        }
        out.join(" ")
    }
}

/// The best bias entry for a candidate word/pair, if above threshold.
fn best_match<'a>(bias: &[&'a Entry], candidate: &str) -> Option<(&'a Entry, f64)> {
    if candidate.chars().count() < MIN_FUZZY_LEN {
        return None;
    }
    let lower = candidate.to_lowercase();
    let mut best: Option<(&Entry, f64)> = None;
    for entry in bias {
        let spoken_lower = entry.spoken.to_lowercase();
        // An exact (case-insensitive) hit is not a correction at all.
        if lower == spoken_lower {
            return None;
        }
        // A word that is a strict prefix of the term is more likely the base
        // word said deliberately than a mangle: "system" must not become
        // "systemd", "postgres" must not become "PostgreSQL". Genuine
        // manglings add or garble sounds; they do not truncate cleanly.
        if !lower.contains(' ') && spoken_lower.starts_with(&lower) {
            continue;
        }
        // Compare against the spoken form with spaces removed too, so
        // "cube cuddle" (as one candidate string) measures against
        // "kubectl" directly.
        let spoken = entry.spoken.to_lowercase();
        let score = fuzzy::similarity(&lower.replace(' ', ""), &spoken.replace(' ', ""));
        if score >= FUZZY_THRESHOLD && best.is_none_or(|(_, b)| score > b) {
            best = Some((entry, score));
        }
    }
    best
}

/// Strip leading/trailing punctuation for matching purposes.
fn strip_word(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric()).to_string()
}

/// Re-attach trailing punctuation from the original token, unless the entry
/// asks for punctuation stripping.
fn carry_punctuation(original: &str, written: &str, flags: EntryFlags) -> String {
    if flags.strip_punctuation {
        return written.to_string();
    }
    let trailing: String = original
        .chars()
        .rev()
        .take_while(|c| !c.is_alphanumeric())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{written}{trailing}")
}

/// Give `written` the casing shape of `matched`: if the user's speech was
/// recognized capitalized (sentence start), the replacement should be too,
/// unless the entry preserves case.
fn match_case(matched: &str, written: &str) -> String {
    let first_upper = matched.chars().next().is_some_and(|c| c.is_uppercase());
    if first_upper {
        let mut chars = written.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    } else {
        written.to_string()
    }
}

fn parse_flags<'a>(
    line: &'a str,
    line_no: usize,
    warnings: &mut Vec<VocabWarning>,
) -> (&'a str, EntryFlags) {
    let mut flags = EntryFlags::default();
    let mut rest = line;
    while let Some(open) = rest.rfind('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        if open + close + 1 != rest.trim_end().len() {
            break; // bracket not at end of line: part of the text itself
        }
        let flag = &rest[open + 1..open + close];
        match flag {
            "case" => flags.preserve_case = true,
            "strip-punct" => flags.strip_punctuation = true,
            other => {
                let suggestion = fuzzy::closest(other, ["case", "strip-punct"]);
                warnings.push(VocabWarning {
                    line: line_no,
                    message: match suggestion {
                        Some(s) => format!("unknown flag [{other}]; did you mean [{s}]?"),
                        None => format!(
                            "unknown flag [{other}]; valid flags are [case] and [strip-punct]"
                        ),
                    },
                });
            }
        }
        rest = rest[..open].trim_end();
    }
    (rest, flags)
}

/// What kind of rule produced a correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionKind {
    Replacement,
    Fuzzy,
}

/// One applied correction, for the diff chip and for debugging "why did my
/// text change".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub from: String,
    pub to: String,
    /// The vocabulary file line that fired.
    pub rule_line: usize,
    pub kind: CorrectionKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(text: &str) -> Vocabulary {
        Vocabulary::parse(text)
    }

    #[test]
    fn parses_bias_replacement_and_flags() {
        let v =
            vocab("# code terms\nkubectl\ndash dash force -> --force\nGitHub -> GitHub [case]\n");
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
        assert_eq!(v.entries.len(), 3);
        assert!(v.entries[0].is_bias_only());
        assert_eq!(v.entries[1].written, "--force");
        assert!(v.entries[2].flags.preserve_case);
    }

    #[test]
    fn bad_lines_warn_but_never_disable_the_file() {
        let v = vocab("kubectl\n-> broken\ngood -> fine\n");
        assert_eq!(v.warnings.len(), 1);
        assert_eq!(v.warnings[0].line, 2);
        assert_eq!(v.entries.len(), 2);
    }

    #[test]
    fn unknown_flag_gets_did_you_mean() {
        let v = vocab("GitHub -> GitHub [caes]\n");
        assert!(v.warnings[0].message.contains("did you mean [case]"));
    }

    #[test]
    fn replacement_is_word_bounded_and_case_matched() {
        let v = vocab("cat -> feline\n");
        let (out, log) = v.correct("Cat sat. concatenate stays.");
        assert_eq!(out, "Feline sat. concatenate stays.");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, CorrectionKind::Replacement);
    }

    #[test]
    fn spoken_command_replacement() {
        let v = vocab("dash dash force -> --force\n");
        let (out, _) = v.correct("run it with dash dash force please");
        assert_eq!(out, "run it with --force please");
    }

    #[test]
    fn preserve_case_flag_wins_over_sentence_casing() {
        let v = vocab("github -> GitHub [case]\n");
        let (out, _) = v.correct("github is down");
        assert_eq!(out, "GitHub is down");
    }

    #[test]
    fn fuzzy_fixes_realistic_recognizer_manglings() {
        // The headline cases from real dictation of technical terms.
        let v = vocab("kubectl\nsystemd\nnginx\nPostgreSQL\n");
        for (mangled, fixed) in [
            ("cube cuddle", "kubectl"),
            ("cube control", "kubectl"),
            ("system dee", "systemd"),
            ("engine ex", "nginx"),
            ("postgres sequel", "PostgreSQL"),
        ] {
            let (out, log) = v.correct(mangled);
            assert_eq!(out, fixed, "mangle {mangled:?} not corrected");
            assert!(log.iter().all(|c| c.kind == CorrectionKind::Fuzzy));
        }
    }

    #[test]
    fn fuzzy_corrects_mid_sentence_and_keeps_punctuation() {
        let v = vocab("kubectl\n");
        let (out, _) = v.correct("then run cube cuddle, and wait");
        assert_eq!(out, "then run kubectl, and wait");
    }

    #[test]
    fn fuzzy_leaves_ordinary_english_alone() {
        let v = vocab("kubectl\nsystemd\nnginx\ngrep\n");
        // Sentences with words in the same phonetic neighborhood must NOT be
        // rewritten: a false positive corrupts the user's actual words.
        for text in [
            "the cat sat on the mat",
            "please cuddle the baby",
            "the system decides",
            "great news everyone",
        ] {
            let (out, log) = v.correct(text);
            assert_eq!(out, text, "false positive: {log:?}");
        }
    }

    #[test]
    fn fuzzy_skips_short_words_entirely() {
        let v = vocab("grep\nsed\n");
        // Both under MIN_FUZZY_LEN: never fuzz, only exact bias.
        let (out, _) = v.correct("grab the seed packet");
        assert_eq!(out, "grab the seed packet");
    }

    #[test]
    fn exact_term_is_not_logged_as_a_correction() {
        let v = vocab("kubectl\n");
        let (out, log) = v.correct("kubectl get pods");
        assert_eq!(out, "kubectl get pods");
        assert!(log.is_empty());
    }

    #[test]
    fn merge_later_sets_override_earlier() {
        let base = vocab("ok -> okay\n");
        let team = vocab("ok -> OK [case]\n");
        let merged = Vocabulary::merge(&[&base, &team]);
        assert_eq!(merged.entries.len(), 1);
        assert_eq!(merged.entries[0].written, "OK");
    }

    #[test]
    fn bias_terms_include_single_word_written_forms() {
        let v = vocab("k eight s -> k8s\ndash dash force -> --force\n");
        let terms = v.bias_terms();
        assert!(terms.contains(&"k8s"));
        // Single-word written forms are bias candidates too.
        assert!(terms.contains(&"--force"));
        assert!(terms.contains(&"dash dash force"));
    }

    #[test]
    fn unlimited_entries_no_cap() {
        // The competitive point: parse 10,000 entries without complaint.
        let big: String = (0..10_000).map(|i| format!("term{i}\n")).collect();
        let v = vocab(&big);
        assert_eq!(v.entries.len(), 10_000);
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn correction_log_names_the_rule_line() {
        let v = vocab("# header\nkubectl\n");
        let (_, log) = v.correct("run cube cuddle now");
        assert_eq!(log[0].rule_line, 2);
    }

    /// A word that already equals its vocabulary entry is not a correction.
    ///
    /// The exact-replacement pass runs before the fuzzy pass, so its output
    /// arrives at the fuzzy stage as a perfect match for the term it just
    /// produced. Logging that reported "BROWNIE -> BROWNIE" in a live run,
    /// which tells a user something happened while showing no change.
    #[test]
    fn an_unchanged_word_is_not_reported_as_corrected() {
        let vocab = Vocabulary::parse("brown -> BROWNIE\n");
        let (text, corrections) = vocab.correct("the dog is brown");

        assert_eq!(text, "the dog is BROWNIE");
        assert_eq!(
            corrections.len(),
            1,
            "one word changed, so exactly one correction: {corrections:?}"
        );
        assert_eq!(corrections[0].from, "brown");
        assert_eq!(corrections[0].to, "BROWNIE");

        // And a transcript that already contains the written form reports
        // nothing at all.
        let (text, corrections) = vocab.correct("the dog is BROWNIE");
        assert_eq!(text, "the dog is BROWNIE");
        assert!(
            corrections.is_empty(),
            "nothing changed, so nothing to report: {corrections:?}"
        );
    }
}
