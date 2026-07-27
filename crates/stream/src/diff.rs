//! Word-level minimal edit between two hypothesis strings.
//!
//! Why this exists: rewriting a whole text field on every partial destroys
//! the caret, resets scroll position, and flickers. The transport needs the
//! *smallest contiguous splice* that turns what the field currently shows
//! into what it should show next.
//!
//! Why word-level and not character-level: a character-level diff of
//! "recognise speech" -> "wreck a nice beach" produces a mid-word splice
//! ("recogni" kept, "se spee" replaced...) that reads as garbage while it
//! lands. Replacing whole words is what a human editor would do, and it is
//! what looks intentional. So the common prefix/suffix is first computed on
//! extended grapheme clusters (never splitting an emoji ZWJ sequence or a
//! combining mark from its base), then *widened* outward to the nearest
//! UAX #29 word boundary shared by both strings.
//!
//! Why UAX #29 word boundaries and not whitespace: CJK has no spaces. UAX
//! #29 segmentation treats each Han/Kana syllable as its own word-bound
//! unit, so Chinese and Japanese hypotheses still get per-character edit
//! granularity instead of one giant "word" spanning the whole sentence.

use std::collections::BTreeSet;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// One contiguous splice: delete `range` (byte offsets into the *old*
/// string), insert `insert` in its place. The only edit shape every
/// transport tier can express (AX selected-range replace, IME commit,
/// key events as backspaces + typing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Byte range of the old string to remove. Always lies on grapheme
    /// (hence char) boundaries, so slicing with it can never panic.
    pub range: Range<usize>,
    /// Replacement text.
    pub insert: String,
}

impl Edit {
    /// Apply to the string this edit was computed against.
    pub fn apply(&self, old: &str) -> String {
        let mut out = String::with_capacity(old.len() + self.insert.len());
        out.push_str(&old[..self.range.start]);
        out.push_str(&self.insert);
        out.push_str(&old[self.range.end..]);
        out
    }

    /// True when applying would change nothing.
    pub fn is_noop(&self) -> bool {
        self.range.is_empty() && self.insert.is_empty()
    }

    /// Shift both offsets by `by` bytes. Used to translate a region-local
    /// edit into whole-field coordinates.
    pub fn offset(mut self, by: usize) -> Edit {
        self.range = (self.range.start + by)..(self.range.end + by);
        self
    }
}

/// Byte length of the longest common prefix of `a` and `b`, measured in
/// whole grapheme clusters. Cluster-wise comparison is what keeps a diff
/// from ever splitting "e" + U+0301 or a flag emoji in half.
fn grapheme_common_prefix(a: &str, b: &str) -> usize {
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

/// Byte length of the longest common suffix of `a` and `b` in whole
/// grapheme clusters, where both slices are the remainders *after* the
/// common prefix (so prefix and suffix can never overlap).
fn grapheme_common_suffix(a: &str, b: &str) -> usize {
    let ga: Vec<&str> = a.graphemes(true).collect();
    let gb: Vec<&str> = b.graphemes(true).collect();
    let mut len = 0;
    let mut i = ga.len();
    let mut j = gb.len();
    while i > 0 && j > 0 && ga[i - 1] == gb[j - 1] {
        len += ga[i - 1].len();
        i -= 1;
        j -= 1;
    }
    len
}

/// Every UAX #29 word-boundary byte position in `text`, including 0 and
/// `text.len()`. For CJK each syllable contributes a boundary, which is
/// exactly the granularity we want there.
pub(crate) fn word_boundaries(text: &str) -> BTreeSet<usize> {
    let mut set: BTreeSet<usize> = text.split_word_bound_indices().map(|(i, _)| i).collect();
    set.insert(0);
    set.insert(text.len());
    set
}

/// The minimal word-aligned splice turning `old` into `new`.
///
/// Guarantees, relied on by property tests:
/// - `minimal_edit(old, new).apply(old) == new`, for *any* pair.
/// - The returned range lies on grapheme boundaries of `old`, and the
///   splice endpoints lie on word boundaries of both strings whenever the
///   texts differ at all (so a mid-word change replaces the whole word).
/// - `old == new` yields a no-op edit.
pub fn minimal_edit(old: &str, new: &str) -> Edit {
    if old == new {
        return Edit {
            range: old.len()..old.len(),
            insert: String::new(),
        };
    }
    let p = grapheme_common_prefix(old, new);
    let s = grapheme_common_suffix(&old[p..], &new[p..]);

    // Widen the splice outward to a word boundary shared by both strings.
    // Widening (never narrowing) preserves the apply-roundtrip property:
    // any common prefix/suffix we give back is re-inserted verbatim.
    let old_bounds = word_boundaries(old);
    let new_bounds = word_boundaries(new);
    let p = old_bounds
        .iter()
        .rev()
        .find(|&&b| b <= p && new_bounds.contains(&b))
        .copied()
        .unwrap_or(0);
    // Suffix boundaries are measured from the end so the same offset can be
    // required of both strings even though their lengths differ.
    let s = (0..=s)
        .rev()
        .find(|&k| old_bounds.contains(&(old.len() - k)) && new_bounds.contains(&(new.len() - k)))
        .unwrap_or(0);

    Edit {
        range: p..(old.len() - s),
        insert: new[p..new.len() - s].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(old: &str, new: &str) -> Edit {
        let e = minimal_edit(old, new);
        assert_eq!(e.apply(old), new, "apply must reproduce the new text");
        e
    }

    #[test]
    fn identical_is_noop() {
        assert!(roundtrip("hello world", "hello world").is_noop());
    }

    #[test]
    fn pure_append_is_end_insertion() {
        let e = roundtrip("hello ", "hello world");
        assert_eq!(e.range, 6..6);
        assert_eq!(e.insert, "world");
    }

    #[test]
    fn mid_word_growth_replaces_the_word_not_the_field() {
        // "wor" growing into "world": the splice covers only that word.
        let e = roundtrip("hello wor", "hello world");
        assert_eq!(e.range, 6..9);
        assert_eq!(e.insert, "world");
    }

    #[test]
    fn the_canonical_revision() {
        // The classic recognizer flip. The shared "ch" tail of
        // "speech"/"beach" must not produce a mid-word splice.
        let e = roundtrip("recognise speech", "wreck a nice beach");
        assert_eq!(e.range, 0..16, "no shared word, whole text replaced");
    }

    #[test]
    fn single_word_change_in_the_middle() {
        let e = roundtrip("change hello to goodbye", "change hallo to goodbye");
        assert_eq!(&"change hello to goodbye"[e.range.clone()], "hello");
        assert_eq!(e.insert, "hallo");
    }

    #[test]
    fn cjk_single_character_edit() {
        // No spaces anywhere; UAX #29 still gives per-syllable boundaries.
        let old = "我想吃饭";
        let new = "我想吃面";
        let e = roundtrip(old, new);
        assert_eq!(&old[e.range.clone()], "饭");
        assert_eq!(e.insert, "面");
    }

    #[test]
    fn emoji_zwj_sequence_is_never_split() {
        let old = "hi 👩‍👩‍👧‍👦 there";
        let new = "hi 👨‍👨‍👦 there";
        let e = roundtrip(old, new);
        // The splice must cover the whole ZWJ family, not a piece of it.
        assert!(old.is_char_boundary(e.range.start));
        assert!(old.is_char_boundary(e.range.end));
        assert_eq!(&old[e.range.clone()], "👩‍👩‍👧‍👦");
    }

    #[test]
    fn combining_mark_stays_with_its_base() {
        // "e" vs "e\u{301}" are different grapheme clusters; the diff must
        // treat them atomically rather than "keeping" the shared base e.
        let old = "cafe";
        let new = "cafe\u{301}";
        let e = roundtrip(old, new);
        assert_eq!(&old[e.range.clone()], "cafe");
        assert_eq!(e.insert, "cafe\u{301}");
    }

    #[test]
    fn total_rewrite() {
        let e = roundtrip("abc def", "完全不同的句子");
        assert_eq!(e.range, 0..7);
    }

    #[test]
    fn empty_to_text_and_back() {
        assert_eq!(roundtrip("", "hello").range, 0..0);
        assert_eq!(roundtrip("hello", "").range, 0..5);
    }
}
