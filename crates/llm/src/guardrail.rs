//! Guardrails: the model must not silently rewrite more than asked.
//!
//! Sanitation (previous stage) fixes *formatting* misbehaviour. This module
//! rejects *content* misbehaviour: refusals, apologies, instruction echoes,
//! runaway length, and rewrites so large they cannot plausibly be the edit
//! the user asked for. The philosophy is asymmetric: a false rejection costs
//! the user one "try again", while a false acceptance pastes hallucinated
//! text into their document. So every bound errs toward rejecting.
//!
//! These checks run on the sanitized full output, after streaming completes.
//! Streamed tokens are preview-only and unvetted by design (rejecting
//! mid-stream would need the checks to be prefix-stable, which length ratios
//! are not).

/// Why an output was rejected. Each variant maps to different user-facing
/// advice, which is why this is an enum and not a message string.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejection {
    /// Output looks like a refusal or apology ("I'm sorry, but I can't...").
    /// Advice: rephrase the instruction.
    Refusal,
    /// Output is (mostly) the instruction repeated back, not a transformation.
    InstructionEcho,
    /// Output identical to the input: the model did nothing. Surfaced
    /// distinctly so the UI can say "no change suggested" instead of showing
    /// an empty diff.
    NoChange,
    /// Output length is implausible for the input length.
    LengthRatio { ratio: f64, max: f64 },
    /// The edit changed more of the text than the instruction plausibly asked
    /// for. `retained` is the fraction of original words surviving.
    ExcessiveRewrite { retained: f64, min_retained: f64 },
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::Refusal => write!(f, "model refused or apologised"),
            Rejection::InstructionEcho => write!(f, "model echoed the instruction"),
            Rejection::NoChange => write!(f, "model returned the input unchanged"),
            Rejection::LengthRatio { ratio, max } => {
                write!(
                    f,
                    "output/input length ratio {ratio:.2} exceeds bound {max:.2}"
                )
            }
            Rejection::ExcessiveRewrite {
                retained,
                min_retained,
            } => write!(
                f,
                "only {:.0}% of original words retained (minimum {:.0}%)",
                retained * 100.0,
                min_retained * 100.0
            ),
        }
    }
}

/// Tunable bounds. Defaults are deliberately loose enough for legitimate
/// restructuring ("turn this into bullet points" changes a lot) while
/// catching the model replacing the text with an essay.
#[derive(Debug, Clone)]
pub struct GuardrailConfig {
    /// Maximum `output_chars / input_chars`. "Expand on this" legitimately
    /// grows text, but a 1.7B model asked to tighten a sentence and
    /// returning 6x the input is generating, not editing.
    pub max_length_ratio: f64,
    /// Minimum ratio the other way, catching the model swallowing the text.
    /// Kept permissive: "tighten this up" on flabby prose can shrink 5x.
    pub min_length_ratio: f64,
    /// Minimum fraction of original words that must survive into the output
    /// for inputs long enough to measure (see `min_words_for_diff_check`).
    /// A rewrite retaining almost nothing has abandoned the user's content.
    /// Calibrated against aggressive-but-legitimate tightening, which can
    /// keep under 1 word in 5; wholesale topic replacement keeps ~0.
    pub min_word_retention: f64,
    /// Word-overlap checks are noise on short inputs (a 4-word sentence can
    /// legitimately share zero words with its formal rewrite), so the
    /// retention bound only applies at or above this many original words.
    pub min_words_for_diff_check: usize,
}

impl Default for GuardrailConfig {
    fn default() -> Self {
        Self {
            max_length_ratio: 4.0,
            min_length_ratio: 0.1,
            min_word_retention: 0.15,
            min_words_for_diff_check: 12,
        }
    }
}

/// Check a sanitized output. `None` means approved for preview.
pub fn check(
    original: &str,
    output: &str,
    instruction: &str,
    config: &GuardrailConfig,
) -> Option<Rejection> {
    if looks_like_refusal(output) {
        return Some(Rejection::Refusal);
    }
    if echoes_instruction(output, instruction) {
        return Some(Rejection::InstructionEcho);
    }
    if output.trim() == original.trim() {
        return Some(Rejection::NoChange);
    }

    // Length sanity. Chars not tokens: cheap, tokenizer-independent, and the
    // bound is coarse anyway.
    let in_len = original.trim().chars().count().max(1) as f64;
    let out_len = output.trim().chars().count() as f64;
    let ratio = out_len / in_len;
    if ratio > config.max_length_ratio || ratio < config.min_length_ratio {
        return Some(Rejection::LengthRatio {
            ratio,
            max: config.max_length_ratio,
        });
    }

    // Diff-size check: how much of the original survived? Word multiset
    // overlap is a deliberately crude diff. It needs no O(n^2) LCS, and
    // crudeness is fine because the bound is a tripwire for wholesale
    // replacement, not a similarity score.
    let orig_words = word_multiset(original);
    if orig_words.len() >= config.min_words_for_diff_check {
        // Count words in the output, then consume one count per surviving
        // original word so repeated words are not double-credited.
        let mut remaining: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let out_words = word_multiset(output);
        for w in &out_words {
            *remaining.entry(w.as_str()).or_insert(0) += 1;
        }
        let survived = orig_words
            .iter()
            .filter(|w| {
                if let Some(n) = remaining.get_mut(w.as_str()) {
                    if *n > 0 {
                        *n -= 1;
                        return true;
                    }
                }
                false
            })
            .count();
        let retained = survived as f64 / orig_words.len() as f64;
        if retained < config.min_word_retention {
            return Some(Rejection::ExcessiveRewrite {
                retained,
                min_retained: config.min_word_retention,
            });
        }
    }

    None
}

/// Lowercased alphanumeric words of the text, with counts folded into a
/// Vec/map pair below. Punctuation is stripped so "deploy." matches "deploy".
fn word_multiset(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Refusal/apology detection. Phrase-anchored to the *start* of the output,
/// because "I'm sorry" inside transformed text (the user may be editing an
/// apology email!) is legitimate content. Models front-load refusals.
fn looks_like_refusal(output: &str) -> bool {
    let head: String = output
        .trim_start()
        .chars()
        .take(80)
        .collect::<String>()
        .to_lowercase();
    const MARKERS: [&str; 10] = [
        "i'm sorry",
        "i am sorry",
        "i apologize",
        "i apologise",
        "i cannot",
        "i can't",
        "i can not",
        "as an ai",
        "i'm unable",
        "i am unable",
    ];
    MARKERS.iter().any(|m| head.starts_with(m))
}

/// Instruction-echo detection: output that is essentially the instruction
/// text means the model parroted instead of transforming.
fn echoes_instruction(output: &str, instruction: &str) -> bool {
    let out = normalize(output);
    let instr = normalize(instruction);
    if instr.is_empty() || out.is_empty() {
        return false;
    }
    // Exact or near-exact echo (echo plus trivial punctuation already
    // normalized away). Containment alone is not enough: a long output
    // containing the instruction words could be a legitimate transform.
    out == instr || (out.len() < instr.len() + 16 && out.contains(&instr))
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GuardrailConfig {
        GuardrailConfig::default()
    }

    #[test]
    fn accepts_reasonable_tightening() {
        let orig = "It is really quite important that we should try to make \
                    sure the deploy happens today without further delay.";
        let out = "The deploy must happen today.";
        assert_eq!(check(orig, out, "tighten this up", &cfg()), None);
    }

    #[test]
    fn accepts_bullet_point_restructure() {
        let orig = "We need to ship the feature, update the docs, and tell \
                    the customers about the change before Friday arrives.";
        let out = "- Ship the feature\n- Update the docs\n- Tell the customers\nAll before Friday.";
        assert_eq!(
            check(orig, out, "turn this into bullet points", &cfg()),
            None
        );
    }

    #[test]
    fn rejects_refusal() {
        assert_eq!(
            check("text", "I'm sorry, I can't do that.", "tighten", &cfg()),
            Some(Rejection::Refusal)
        );
    }

    #[test]
    fn rejects_apology_variants() {
        for r in [
            "I apologize, but this request",
            "I cannot rewrite this",
            "As an AI, I",
        ] {
            assert!(
                matches!(check("text", r, "x", &cfg()), Some(Rejection::Refusal)),
                "{r}"
            );
        }
    }

    #[test]
    fn sorry_inside_content_is_fine() {
        // The user might be editing an apology; only leading refusals count.
        let orig = "we regret the outage yesterday and will send credits to \
                    every affected customer account by end of week";
        let out = "We're sorry about yesterday's outage. Credits go to every \
                   affected customer account by end of week.";
        assert_eq!(check(orig, out, "make it more personal", &cfg()), None);
    }

    #[test]
    fn rejects_instruction_echo() {
        assert_eq!(
            check(
                "some original text",
                "Tighten this up.",
                "tighten this up",
                &cfg()
            ),
            Some(Rejection::InstructionEcho)
        );
    }

    #[test]
    fn rejects_unchanged_output() {
        assert_eq!(
            check("same text", "same text", "make it formal", &cfg()),
            Some(Rejection::NoChange)
        );
    }

    #[test]
    fn rejects_runaway_expansion() {
        let out = "word ".repeat(100);
        assert!(matches!(
            check("short input here", &out, "tighten", &cfg()),
            Some(Rejection::LengthRatio { .. })
        ));
    }

    #[test]
    fn rejects_near_total_deletion() {
        let orig = "a sentence that has a reasonable number of words in it for testing";
        assert!(matches!(
            check(orig, "ok", "tighten", &cfg()),
            Some(Rejection::LengthRatio { .. })
        ));
    }

    #[test]
    fn rejects_wholesale_replacement() {
        let orig = "the quarterly report shows revenue grew twelve percent \
                    while operating costs stayed flat across all regions";
        // Same length class, zero content overlap: the model wrote about
        // something else entirely.
        let out = "penguins huddle together during antarctic winters to share \
                   warmth and rotate positions constantly through storms";
        assert!(matches!(
            check(orig, out, "make it more formal", &cfg()),
            Some(Rejection::ExcessiveRewrite { .. })
        ));
    }

    #[test]
    fn short_inputs_skip_diff_check() {
        // 4 words: a formal rewrite may share nothing; must not be rejected.
        assert_eq!(
            check(
                "gonna fix it soon",
                "The repair will be completed shortly.",
                "formal",
                &cfg()
            ),
            None
        );
    }
}
