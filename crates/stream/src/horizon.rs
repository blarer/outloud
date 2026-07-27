//! Stability-based commit horizon.
//!
//! The core correctness rule of streaming dictation: **committed text is
//! never retracted**. A streaming recognizer revises its hypothesis freely
//! ("recognise speech" -> "wreck a nice beach"), and any word already
//! injected into the user's document would have to be visibly rewritten.
//! So nothing is committed until it has *proven* stable: a prefix is
//! committable only once the last `stability` consecutive hypotheses agree
//! on it. This is the local-agreement (LocalAgreement-n) policy from the
//! simultaneous-translation literature, which the UX doc names the "commit
//! horizon": committed text runs roughly one phrase behind the audio, the
//! churning tail stays in the overlay.
//!
//! Two safety properties, enforced structurally and property-tested:
//!
//! 1. **Monotonic commits.** The committed prefix only ever grows. Even if
//!    the recognizer later disagrees with committed text, we do not retract
//!    it mid-stream (the final pass applies one consolidated correction,
//!    per the UX doc "append-only writes"). `update` can therefore never
//!    return a committed string that is not an extension of the last one.
//! 2. **Boundary-aligned commits.** The horizon never commits half a word
//!    (or half a grapheme). Committing "recogni" and leaving "se" in the
//!    tail would look like a typo the app cannot un-see. Commits are
//!    trimmed back to the last UAX #29 word boundary, which also does the
//!    right thing for CJK, where each syllable is its own boundary unit.

use crate::diff::word_boundaries;
use unicode_segmentation::UnicodeSegmentation;

/// Tunables for the stability policy.
#[derive(Debug, Clone)]
pub struct HorizonConfig {
    /// How many consecutive hypotheses must agree on a prefix before it is
    /// committed. 1 commits every partial immediately (only safe for a
    /// finalizer's output); 2-3 is the practical streaming range: each
    /// increment trades ~one partial interval of extra lag for one more
    /// chance to catch a revision.
    pub stability: usize,
    /// How many trailing *words* of an otherwise-stable prefix are held
    /// back anyway. Recognizers revise most near the audio frontier even
    /// when two partials happen to agree, so holding the last word or two
    /// out of the commit is cheap insurance against agreeing-by-accident.
    pub lookback_words: usize,
}

impl Default for HorizonConfig {
    fn default() -> Self {
        Self {
            stability: 3, // "stable across N consecutive hypotheses" (UX doc)
            lookback_words: 1,
        }
    }
}

/// What one hypothesis update yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HorizonUpdate {
    /// Text newly promoted to committed by this hypothesis (a suffix of the
    /// total committed text). Empty when nothing new stabilized.
    pub newly_committed: String,
    /// The unstable remainder of the current hypothesis, for the overlay's
    /// dim ghost tail. Never written to the document.
    pub tail: String,
}

/// Tracks hypothesis agreement and decides what is safe to commit.
#[derive(Debug, Clone)]
pub struct CommitHorizon {
    config: HorizonConfig,
    /// Everything committed so far. Grows monotonically; the type has no
    /// operation that shrinks it, which is how the never-retract property
    /// is made structural rather than aspirational.
    committed: String,
    /// The last `stability - 1` hypotheses, oldest first, against which the
    /// next hypothesis is checked for agreement.
    recent: Vec<String>,
}

impl CommitHorizon {
    pub fn new(config: HorizonConfig) -> Self {
        Self {
            config,
            committed: String::new(),
            recent: Vec::new(),
        }
    }

    /// All text committed so far.
    pub fn committed(&self) -> &str {
        &self.committed
    }

    /// Feed the next whole-hypothesis partial. Returns what newly became
    /// committable and the still-unstable tail.
    pub fn update(&mut self, hypothesis: &str) -> HorizonUpdate {
        self.recent.push(hypothesis.to_string());
        let window = self.config.stability.max(1);
        if self.recent.len() > window {
            let drop = self.recent.len() - window;
            self.recent.drain(..drop);
        }

        let mut stable_len = if self.recent.len() < window {
            // Not enough history to prove anything stable yet.
            0
        } else {
            // Byte length of the grapheme-aligned prefix all recent
            // hypotheses agree on.
            self.agreed_prefix_len()
        };

        // Hold back the lookback words, then trim to a word boundary so we
        // never commit a fragment of a word.
        stable_len = self.retreat_words(hypothesis, stable_len, self.config.lookback_words);
        let bounds = word_boundaries(hypothesis);
        while stable_len > 0 && !bounds.contains(&stable_len) {
            stable_len = *bounds.range(..stable_len).next_back().unwrap_or(&0);
        }

        // Monotonicity: if the recognizer rewrote text we already
        // committed, we do not follow it down. The committed prefix stands;
        // only hypothesis text *beyond* it can extend the commit, and only
        // when the hypothesis still starts with what we committed. When it
        // does not (a total rewrite), nothing new commits and the whole
        // divergent hypothesis rides in the tail until the final pass.
        let newly_committed =
            if hypothesis.starts_with(&self.committed) && stable_len > self.committed.len() {
                let new = hypothesis[self.committed.len()..stable_len].to_string();
                self.committed.push_str(&new);
                new
            } else {
                String::new()
            };

        let tail = if let Some(rest) = hypothesis.strip_prefix(self.committed.as_str()) {
            rest.to_string()
        } else {
            // Hypothesis disagrees with committed text. Show its divergent
            // remainder past the longest agreeing prefix so the overlay
            // still reflects what the recognizer currently believes.
            hypothesis.to_string()
        };

        HorizonUpdate {
            newly_committed,
            tail,
        }
    }

    /// End of utterance: everything else in `final_text` becomes committed
    /// (the finalizer's transcript replaces hypotheses wholesale, so
    /// stability no longer applies). Returns the not-yet-committed suffix
    /// when the final text extends the committed prefix, or `None` when the
    /// final text *contradicts* committed text, in which case the caller
    /// must apply a correction diff instead of an append.
    pub fn finish(&mut self, final_text: &str) -> Option<String> {
        self.recent.clear();
        match final_text.strip_prefix(self.committed.as_str()) {
            Some(rest) => {
                let rest = rest.to_string();
                self.committed = final_text.to_string();
                Some(rest)
            }
            None => {
                self.committed = final_text.to_string();
                None
            }
        }
    }

    /// Reset for the next utterance.
    pub fn reset(&mut self) {
        self.committed.clear();
        self.recent.clear();
    }

    /// Longest byte prefix (grapheme-aligned) shared by every retained
    /// hypothesis.
    fn agreed_prefix_len(&self) -> usize {
        let first = match self.recent.first() {
            Some(f) => f,
            None => return 0,
        };
        let mut len = first.len();
        for other in &self.recent[1..] {
            len = len.min(common_grapheme_prefix(first, other));
        }
        len
    }

    /// Move `len` back by `words` UAX #29 word segments of `text` (skipping
    /// whitespace-only segments, which are separators rather than content).
    fn retreat_words(&self, text: &str, len: usize, words: usize) -> usize {
        let mut len = len.min(text.len());
        // Align down to a char boundary before slicing, defensively.
        while len > 0 && !text.is_char_boundary(len) {
            len -= 1;
        }
        for _ in 0..words {
            let segs: Vec<(usize, &str)> = text[..len].split_word_bound_indices().collect();
            // Drop the last non-whitespace segment plus trailing separators.
            let mut cut = len;
            for &(start, seg) in segs.iter().rev() {
                cut = start;
                if !seg.trim().is_empty() {
                    break;
                }
            }
            if cut == len {
                break;
            }
            len = cut;
        }
        len
    }
}

/// Byte length of the longest common prefix in whole grapheme clusters.
fn common_grapheme_prefix(a: &str, b: &str) -> usize {
    let mut len = 0;
    let mut ga = a.graphemes(true);
    let mut gb = b.graphemes(true);
    loop {
        match (ga.next(), gb.next()) {
            (Some(x), Some(y)) if x == y => len += x.len(),
            _ => return len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn horizon(stability: usize, lookback: usize) -> CommitHorizon {
        CommitHorizon::new(HorizonConfig {
            stability,
            lookback_words: lookback,
        })
    }

    #[test]
    fn nothing_commits_before_stability_window_fills() {
        let mut h = horizon(3, 0);
        assert_eq!(h.update("hello").newly_committed, "");
        assert_eq!(h.update("hello world").newly_committed, "");
        // Third hypothesis: all three agree on "hello", which is now proven.
        let u = h.update("hello world again");
        assert_eq!(u.newly_committed, "hello");
        assert_eq!(u.tail, " world again");
    }

    #[test]
    fn revision_before_commit_never_reaches_the_document() {
        let mut h = horizon(2, 0);
        h.update("recognise speech");
        // Total rewrite while still unstable: nothing was committed, so
        // nothing has to be retracted.
        let u = h.update("wreck a nice beach");
        assert_eq!(h.committed(), "");
        assert_eq!(u.tail, "wreck a nice beach");
    }

    #[test]
    fn committed_prefix_survives_a_later_contradiction() {
        let mut h = horizon(2, 0);
        h.update("hello world");
        h.update("hello world");
        assert_eq!(h.committed(), "hello world");
        // The recognizer now disagrees with committed text. We hold.
        let u = h.update("yellow world");
        assert_eq!(u.newly_committed, "");
        assert_eq!(h.committed(), "hello world");
    }

    #[test]
    fn lookback_holds_the_frontier_word() {
        let mut h = horizon(2, 1);
        h.update("hello world");
        let u = h.update("hello world");
        // "world" is stable by agreement but held back by lookback.
        assert_eq!(u.newly_committed, "hello ");
        assert_eq!(u.tail, "world");
    }

    #[test]
    fn commits_never_split_a_word() {
        let mut h = horizon(2, 0);
        h.update("thermodynamics");
        h.update("thermodynamite"); // agree on "thermodynami", mid-word
        assert_eq!(h.committed(), "", "partial-word agreement must not commit");
    }

    #[test]
    fn cjk_commits_at_syllable_granularity() {
        let mut h = horizon(2, 0);
        h.update("今天天气很好");
        let u = h.update("今天天气不错");
        assert_eq!(u.newly_committed, "今天天气");
        assert_eq!(u.tail, "不错");
    }

    #[test]
    fn finish_extends_committed_text() {
        let mut h = horizon(2, 0);
        h.update("hello world");
        h.update("hello world");
        let rest = h.finish("hello world today.");
        assert_eq!(rest, Some(" today.".to_string()));
        assert_eq!(h.committed(), "hello world today.");
    }

    #[test]
    fn finish_reports_contradiction_for_correction_diff() {
        let mut h = horizon(2, 0);
        h.update("recognise speech now");
        h.update("recognise speech now");
        assert_eq!(h.committed(), "recognise speech now");
        let rest = h.finish("wreck a nice beach now");
        assert_eq!(rest, None, "caller must diff, not append");
        assert_eq!(h.committed(), "wreck a nice beach now");
    }

    #[test]
    fn stability_one_commits_immediately() {
        let mut h = horizon(1, 0);
        let u = h.update("hello world");
        assert_eq!(u.newly_committed, "hello world");
    }

    #[test]
    fn reset_clears_for_next_utterance() {
        let mut h = horizon(1, 0);
        h.update("hello");
        h.reset();
        assert_eq!(h.committed(), "");
    }
}
