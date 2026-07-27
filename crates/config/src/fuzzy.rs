//! Shared string-similarity primitives.
//!
//! Two consumers with the same need: `validate` wants "did you mean
//! `hotkey`?" for a mistyped config key, and `vocab` wants to recognize that
//! the recognizer's "cube cuddle" was an attempt at "kubectl". Both reduce to
//! edit distance plus a phonetic equivalence class, so the code lives once,
//! here, where it can be tested exhaustively.

/// Damerau-Levenshtein distance (optimal string alignment variant).
///
/// Transpositions count as one edit because both of our inputs are produced
/// by humans (typos swap adjacent letters) or by a recognizer (which garbles
/// order), and charging two edits for a swap makes realistic mistakes look
/// farther away than they are.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    // Full matrix rather than two rows because transposition lookback needs
    // row i-2; the strings here are config keys and vocabulary terms, so the
    // quadratic space is trivially small.
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[n][m]
}

/// A compact consonant-skeleton phonetic code, in the soundex/metaphone
/// family but tuned for *recognizer* confusions rather than surname lookup:
/// the recognizer hears sounds, so "cube cuddle" and "kubectl" should land on
/// the same or nearly the same code even though their spellings are far
/// apart.
///
/// Rules: lowercase; map letters to sound classes (k/c/q/g→K, s/z/x→S,
/// d/t→T, b/p→P, f/v→F, m/n→M, l→L, r→R); keep the first vowel only if it
/// starts the word; collapse runs; drop everything else. Digits pass through
/// because technical terms embed them meaningfully (s3, i18n).
pub fn phonetic(s: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        let mapped = match c {
            'k' | 'c' | 'q' | 'g' => Some('K'),
            's' | 'z' | 'x' | 'j' => Some('S'),
            'd' | 't' => Some('T'),
            'b' | 'p' => Some('P'),
            'f' | 'v' | 'w' => Some('F'),
            'm' | 'n' => Some('M'),
            'l' => Some('L'),
            'r' => Some('R'),
            'h' | 'y' => None,
            'a' | 'e' | 'i' | 'o' | 'u' => {
                // A leading vowel is audible and distinguishes e.g. "async"
                // from "sink"; interior vowels are what recognizers vary
                // most, so they are dropped.
                if first {
                    Some('A')
                } else {
                    None
                }
            }
            '0'..='9' => Some(c),
            _ => None,
        };
        first = false;
        if let Some(m) = mapped {
            if !out.ends_with(m) {
                out.push(m);
            }
        }
    }
    out
}

/// How alike two terms sound, 0.0 (unrelated) to 1.0 (identical), combining
/// spelling distance and phonetic-code distance. The phonetic half is
/// weighted higher because our dominant error source mangles sound-alike
/// words with wildly different spellings.
pub fn similarity(a: &str, b: &str) -> f64 {
    let spell = normalized(a, b);
    let sound = normalized(&phonetic(a), &phonetic(b));
    0.4 * spell + 0.6 * sound
}

fn normalized(a: &str, b: &str) -> f64 {
    let len = a.chars().count().max(b.chars().count());
    if len == 0 {
        return 1.0;
    }
    1.0 - (edit_distance(a, b) as f64 / len as f64)
}

/// The closest candidate to `input`, if any is close enough to be a useful
/// suggestion. Used for did-you-mean on unknown config keys. The threshold
/// is distance ≤ 3 *and* under half the input length, so "hotkye"→"hotkey"
/// suggests but a completely unrelated key stays unsuggested rather than
/// sending the user down a wrong path.
pub fn closest<'a, I>(input: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut best: Option<(&str, usize)> = None;
    for cand in candidates {
        let d = edit_distance(&input.to_lowercase(), &cand.to_lowercase());
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((cand, d));
        }
    }
    let (cand, d) = best?;
    let limit = (input.chars().count() / 2).clamp(2, 3);
    (d <= limit).then_some(cand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_basics() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn transposition_is_one_edit() {
        assert_eq!(edit_distance("hotkye", "hotkey"), 1);
    }

    #[test]
    fn phonetic_collapses_recognizer_mangles() {
        // The canonical failure this feature exists for.
        assert!(edit_distance(&phonetic("cubecuddle"), &phonetic("kubectl")) <= 1);
        assert_eq!(phonetic("their"), phonetic("there"));
    }

    #[test]
    fn phonetic_keeps_leading_vowel_and_digits() {
        assert_ne!(phonetic("async"), phonetic("sink"));
        assert_eq!(phonetic("s3"), "S3");
    }

    #[test]
    fn similarity_orders_sensibly() {
        assert!(similarity("kubectl", "cubecuddle") > similarity("kubectl", "sandwich"));
        assert_eq!(similarity("same", "same"), 1.0);
    }

    #[test]
    fn closest_suggests_typos_only() {
        let keys = ["hotkey", "microphone", "language"];
        assert_eq!(closest("hotkye", keys), Some("hotkey"));
        assert_eq!(closest("mikrophone", keys), Some("microphone"));
        // Unrelated input must not produce a misleading suggestion.
        assert_eq!(closest("zzzzzzzz", keys), None);
    }

    #[test]
    fn closest_short_inputs_use_tight_limit() {
        // Two edits on a three-letter input is a different word, not a typo.
        assert_eq!(closest("xyq", ["abc"]), None);
    }
}
