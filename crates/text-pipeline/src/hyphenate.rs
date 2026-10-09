//! Issue #326 — Liang pattern hyphenation (Frank M. Liang, *Word
//! Hy-phen-a-tion by Com-put-er*, Stanford 1983 — the algorithm TeX uses),
//! re-implemented from the published description.
//!
//! A [`Hyphenator`] is a pattern table plus an exception list, loaded from a
//! TeX hyphenation file (`\patterns{…}` / `\hyphenation{…}`). Patterns are
//! letter strings with digits between the letters (`.hy3p`, `4ph`); `.`
//! anchors a pattern at a word edge. To hyphenate a word, every pattern
//! that matches a substring of `.word.` contributes its digits to the
//! inter-letter positions it covers, the maximum wins, and an ODD value
//! marks a permitted break. Exceptions (`as-so-ciate`) replace the pattern
//! result for the exact word.
//!
//! ## Data (data-driven: a language is one pattern file)
//!
//! * `en-US` — `data/hyph-en-us.tex`, vendored VERBATIM from the
//!   hyph-utf8 project (<https://github.com/hyphenation/tex-hyphen>,
//!   `hyph-utf8/tex/generic/hyph-utf8/patterns/tex/hyph-en-us.tex`, commit
//!   `dce23b4eb2df218ec6893b851562316d7cf37355`, sha256
//!   `f4ffcd96c5cbc886bdad23f95dcae8edc3cd3620eae62f7946eceda97c4e68f8`):
//!   Gerard D.C. Kuiken's `ushyphmax` patterns plus Knuth's `hyphen.tex`
//!   patterns and exception list. Copyright (C) 1990, 2004, 2005 Gerard
//!   D.C. Kuiken; licence: "Copying and distribution of this file, with or
//!   without modification, are permitted in any medium without royalty
//!   provided the copyright notice and this notice are preserved." — the
//!   file keeps both. Minimum fragment lengths: the file's own
//!   `hyphenmins` (left 2, right 3).
//!
//! Adding a language = adding its pattern file to [`Hyphenator::for_language`].
//! Arabic-script text is never hyphenated (Kashida justification is its
//! stretch mechanism): callers skip complex-script words, and no Arabic
//! pattern set is registered.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The vendored en-US pattern file (see the module docs for provenance).
const EN_US_TEX: &str = include_str!("../data/hyph-en-us.tex");

/// A loaded pattern table + exception list for one language.
#[derive(Debug)]
pub struct Hyphenator {
    /// Pattern letters → the digit at each of the `letters + 1` positions.
    patterns: HashMap<Box<str>, Box<[u8]>>,
    /// Longest pattern, in characters (bounds the substring scan).
    max_len: usize,
    /// Exception word (lowercase) → permitted break positions (character
    /// indices: a break before the character at that index).
    exceptions: HashMap<Box<str>, Box<[usize]>>,
    /// Minimum characters before the first break.
    pub left_min: usize,
    /// Minimum characters after the last break.
    pub right_min: usize,
}

impl Hyphenator {
    /// Parse a TeX hyphenation file: the tokens of every `\patterns{…}`
    /// group are patterns, those of every `\hyphenation{…}` group are
    /// exceptions; `%` starts a comment.
    pub fn from_tex(src: &str, left_min: usize, right_min: usize) -> Hyphenator {
        #[derive(PartialEq)]
        enum Group {
            None,
            Patterns,
            Exceptions,
        }
        let mut group = Group::None;
        let mut patterns: HashMap<Box<str>, Box<[u8]>> = HashMap::new();
        let mut exceptions: HashMap<Box<str>, Box<[usize]>> = HashMap::new();
        let mut max_len = 0;
        for line in src.lines() {
            let line = line.split('%').next().unwrap_or("");
            for token in line.split_whitespace() {
                match token {
                    "\\patterns{" => group = Group::Patterns,
                    "\\hyphenation{" => group = Group::Exceptions,
                    "}" => group = Group::None,
                    t if group == Group::Patterns => {
                        let (letters, values) = parse_pattern(t);
                        max_len = max_len.max(letters.chars().count());
                        patterns.insert(letters.into(), values.into());
                    }
                    t if group == Group::Exceptions => {
                        let mut word = String::new();
                        let mut breaks = Vec::new();
                        for ch in t.chars() {
                            if ch == '-' {
                                breaks.push(word.chars().count());
                            } else {
                                word.extend(ch.to_lowercase());
                            }
                        }
                        exceptions.insert(word.into(), breaks.into());
                    }
                    _ => {}
                }
            }
        }
        Hyphenator {
            patterns,
            max_len,
            exceptions,
            left_min,
            right_min,
        }
    }

    /// The American English hyphenator (parsed once, on first use).
    pub fn en_us() -> &'static Hyphenator {
        static EN_US: OnceLock<Hyphenator> = OnceLock::new();
        EN_US.get_or_init(|| Hyphenator::from_tex(EN_US_TEX, 2, 3))
    }

    /// The hyphenator for a BCP 47 / `w:lang` tag (`en-US`, `en_us`, …),
    /// `None` for a language without registered patterns. This cut ships
    /// `en-US` only; other English variants (`en-GB`, …) are NOT mapped onto
    /// it — their hyphenation differs (`pro-cess` vs `proc-ess`).
    pub fn for_language(tag: &str) -> Option<&'static Hyphenator> {
        let tag = tag.trim().replace('_', "-").to_ascii_lowercase();
        match tag.as_str() {
            "en-us" => Some(Hyphenator::en_us()),
            _ => None,
        }
    }

    /// Permitted break positions in `word` — character indices `i` such
    /// that the word may break between `chars[i - 1]` and `chars[i]` —
    /// ascending, honouring [`Self::left_min`] / [`Self::right_min`].
    /// `word` must be letters only; anything else returns no break.
    pub fn hyphenate(&self, word: &str) -> Vec<usize> {
        let chars: Vec<char> = word.chars().flat_map(char::to_lowercase).collect();
        let n = chars.len();
        if n < self.left_min + self.right_min || !chars.iter().all(|c| c.is_alphabetic()) {
            return Vec::new();
        }
        let in_bounds = |i: &usize| *i >= self.left_min && *i <= n - self.right_min;
        let lower: String = chars.iter().collect();
        if let Some(breaks) = self.exceptions.get(lower.as_str()) {
            return breaks.iter().copied().filter(in_bounds).collect();
        }
        /* `.word.` — the dots are the word-edge anchors. */
        let dotted: Vec<char> = std::iter::once('.')
            .chain(chars.iter().copied())
            .chain(std::iter::once('.'))
            .collect();
        /* values[k] = the digit between dotted[k-1] and dotted[k]. */
        let mut values = vec![0u8; dotted.len() + 1];
        let mut key = String::new();
        for start in 0..dotted.len() {
            key.clear();
            for &ch in dotted.iter().skip(start).take(self.max_len) {
                key.push(ch);
                if let Some(v) = self.patterns.get(key.as_str()) {
                    for (k, &d) in v.iter().enumerate() {
                        let slot = &mut values[start + k];
                        *slot = (*slot).max(d);
                    }
                }
            }
        }
        /* A break before word character `i` sits between dotted[i] and
        dotted[i + 1]: values[i + 1]. */
        (1..n)
            .filter(|i| values[i + 1] % 2 == 1)
            .filter(in_bounds)
            .collect()
    }
}

/// `"hy3ph"` → (`"hyph"`, `[0, 0, 3, 0, 0]`): the letters and the digit at
/// each of the `letters + 1` inter-letter positions (0 when absent).
fn parse_pattern(token: &str) -> (String, Vec<u8>) {
    let mut letters = String::new();
    let mut values = vec![0u8];
    for ch in token.chars() {
        if let Some(d) = ch.to_digit(10) {
            if let Some(last) = values.last_mut() {
                *last = d as u8;
            }
        } else {
            letters.push(ch);
            values.push(0);
        }
    }
    (letters, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(h: &Hyphenator, word: &str) -> String {
        let breaks = h.hyphenate(word);
        let mut out = String::new();
        for (i, ch) in word.chars().enumerate() {
            if breaks.contains(&i) {
                out.push('-');
            }
            out.push(ch);
        }
        out
    }

    #[test]
    fn patterns_parse_with_their_digits() {
        assert_eq!(parse_pattern(".hy3p"), (".hyp".into(), vec![0, 0, 0, 3, 0]));
        assert_eq!(parse_pattern("4ph"), ("ph".into(), vec![4, 0, 0]));
        assert_eq!(parse_pattern("z2z3w"), ("zzw".into(), vec![0, 2, 3, 0]));
    }

    /// Liang's own examples and TeX's classic outputs for US English.
    #[test]
    fn en_us_hyphenates_like_tex() {
        let h = Hyphenator::en_us();
        assert!(h.patterns.len() > 4000, "{}", h.patterns.len());
        assert_eq!(split(h, "hyphenation"), "hy-phen-ation");
        /* right_min 3 forbids "com-put-er" (TeX gives the same). */
        assert_eq!(split(h, "computer"), "com-puter");
        assert_eq!(
            split(h, "incomprehensibilities"),
            "in-com-pre-hen-si-bil-i-ties"
        );
        assert_eq!(split(h, "Typesetting"), "Type-set-ting");
        /* Exceptions win over the patterns. */
        assert_eq!(split(h, "associate"), "as-so-ciate");
        assert_eq!(split(h, "table"), "ta-ble");
        assert_eq!(split(h, "project"), "project");
    }

    #[test]
    fn minimum_fragments_and_non_words() {
        let h = Hyphenator::en_us();
        /* Too short for left 2 + right 3. */
        assert!(h.hyphenate("the").is_empty());
        assert!(h.hyphenate("abcd").is_empty());
        /* Not letters only. */
        assert!(h.hyphenate("e-mail").is_empty());
        assert!(h.hyphenate("2026").is_empty());
        for w in ["hyphenation", "incomprehensibilities", "computer"] {
            let n = w.chars().count();
            for b in h.hyphenate(w) {
                assert!(b >= 2 && b <= n - 3, "{w}: {b}");
            }
        }
    }

    #[test]
    fn only_en_us_is_registered() {
        assert!(Hyphenator::for_language("en-US").is_some());
        assert!(Hyphenator::for_language("en_us").is_some());
        assert!(Hyphenator::for_language("ar-SA").is_none());
        assert!(Hyphenator::for_language("en-GB").is_none());
        assert!(Hyphenator::for_language("").is_none());
    }
}
