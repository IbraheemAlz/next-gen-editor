//! Epic #555 rows 20 / 22 — the ONE word model (clean-room cursor memo
//! §5 / §6; issue #598).
//!
//! A paragraph's text is cut into UNITS: UAX #29 word segments
//! (`split_word_bound_indices` — a contraction `isn't`, a number `3.14`,
//! one ideograph, a letter with its combining marks, an Arabic word with
//! its tatweel and harakat all stay whole) classified as
//!
//! - **text** — a segment with a letter or a digit;
//! - **punctuation** — a class of its own (symbols too);
//! - **separators** — horizontal white space: space, tab, no-break space;
//! - **objects** — a non-text element, a word by itself: an inline
//!   object's `U+FFFC` sentinel, and a line / page break (the model's
//!   `U+2028` / `U+000C`, any UAX #29 newline).
//!
//! Adjacent punctuation and adjacent separators merge into one unit each.
//! There are no script-specific rules.
//!
//! A WORD is a non-separator unit plus the separators that follow it;
//! separators at the very start of the text are a word of their own. So
//! `word, next` holds the words `word`, `, ` and `next`, and every byte
//! belongs to exactly one word. This is what
//!
//! - a double-click selects (memo §6: "a double-clicked word includes its
//!   trailing spaces"; a double-click ON the spaces selects the word they
//!   trail) — `Paragraph::word_bounds` → [`word_at`];
//! - a drag after a double-click grows by (engine-wasm
//!   `SelectUnitsInPage`, through the same `word_bounds`);
//! - Ctrl+Right steps to the end of ([`word_stop_forward`]: the start of
//!   the next word) and Ctrl+Left to the start of ([`word_stop_backward`])
//!   — memo §5: Ctrl+Right skips a run of one class, then the separators
//!   after it; Ctrl+Left skips separators, then a run of one class;
//! - Ctrl+Delete / Ctrl+Backspace delete up to (the same two stops).
//!
//! Never classify words anywhere else: every caller goes through this
//! module.

use unicode_segmentation::UnicodeSegmentation;

/// The class of a word unit (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordClass {
    Text,
    Punctuation,
    Separator,
    Object,
}

/// One unit of [`word_units`]: `[start, end)` and its class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordUnit {
    pub start: usize,
    pub end: usize,
    pub class: WordClass,
}

/// A UAX #29 newline (WB3a / WB3b): a break, never a separator. The model
/// writes a line break as `U+2028` and a page break as `U+000C`.
fn is_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

fn classify(segment: &str) -> WordClass {
    if segment.starts_with('\u{FFFC}') || segment.chars().all(is_break) {
        WordClass::Object
    } else if segment.chars().all(char::is_whitespace) {
        WordClass::Separator
    } else if segment.chars().any(char::is_alphanumeric) {
        WordClass::Text
    } else {
        WordClass::Punctuation
    }
}

/// The units of `text`, in logical order, covering it (module docs).
pub fn word_units(text: &str) -> Vec<WordUnit> {
    let mut units: Vec<WordUnit> = Vec::new();
    for (start, segment) in text.split_word_bound_indices() {
        let class = classify(segment);
        let end = start + segment.len();
        match units.last_mut() {
            Some(last)
                if last.class == class
                    && matches!(class, WordClass::Punctuation | WordClass::Separator) =>
            {
                last.end = end;
            }
            _ => units.push(WordUnit { start, end, class }),
        }
    }
    units
}

/// The words of `text` — `[start, end)` each, in logical order, covering
/// it: every non-separator unit with the separators after it (module
/// docs).
pub fn words(text: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for unit in word_units(text) {
        match out.last_mut() {
            Some(last) if unit.class == WordClass::Separator => last.1 = unit.end,
            _ => out.push((unit.start, unit.end)),
        }
    }
    out
}

/// The word holding byte `off` of `text`; at (or past) the end of the
/// text, its last word. `(0, 0)` for an empty text. What a double-click
/// on the character at `off` selects.
pub fn word_at(text: &str, off: usize) -> (usize, usize) {
    let words = words(text);
    words
        .iter()
        .copied()
        .find(|w| off < w.1)
        .or_else(|| words.last().copied())
        .unwrap_or((0, 0))
}

/// Where Ctrl+Right (logical forward) stops from byte `off` of `text`: the
/// end of the word holding `off` — the start of the next word, punctuation
/// run or object, else the end of the text. Also the far end of a
/// Ctrl+Delete.
pub fn word_stop_forward(text: &str, off: usize) -> usize {
    words(text)
        .into_iter()
        .find(|w| off < w.1)
        .map_or(text.len(), |w| w.1)
}

/// Where Ctrl+Left (logical backward) stops from byte `off` of `text`: the
/// start of the word holding the character before `off` (`0` at the
/// start). Also the near end of a Ctrl+Backspace.
pub fn word_stop_backward(text: &str, off: usize) -> usize {
    words(text)
        .into_iter()
        .rev()
        .find(|w| w.0 < off)
        .map_or(0, |w| w.0)
}

/// Issue #598 — the end of the part of `[start, end)` that text typed over
/// it replaces. A selection that ends in separators after a word, right
/// before the next word — what a double-click selects — keeps those
/// separators, so typing over a double-clicked word does not glue the new
/// text to the next word (`one two three`: a double-click on `two` selects
/// `two `; typing `X` gives `one X three`, never `one Xthree`). `end`
/// itself when the selection is only separators, reaches the end of the
/// text or is followed by more separators, or when `typed` is empty or
/// ends in a separator of its own.
pub fn typing_replace_end(text: &str, start: usize, end: usize, typed: &str) -> usize {
    if start >= end
        || end >= text.len()
        || typed.chars().next_back().is_none_or(char::is_whitespace)
    {
        return end;
    }
    let units = word_units(text);
    let ends_in_separators = units
        .iter()
        .find(|u| u.start < end && end <= u.end)
        .filter(|u| u.class == WordClass::Separator && u.end == end);
    let followed_by_a_word = units
        .iter()
        .find(|u| u.start == end)
        .is_some_and(|u| u.class != WordClass::Separator);
    match ends_in_separators {
        Some(sep) if followed_by_a_word && sep.start > start => sep.start,
        _ => end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_texts(text: &str) -> Vec<&str> {
        words(text).into_iter().map(|(s, e)| &text[s..e]).collect()
    }

    fn selected(text: &str, off: usize) -> &str {
        let (s, e) = word_at(text, off);
        &text[s..e]
    }

    #[test]
    fn a_word_carries_its_trailing_separators_and_punctuation_is_its_own() {
        assert_eq!(word_texts("word, next"), vec!["word", ", ", "next"]);
        assert_eq!(word_texts("a   b"), vec!["a   ", "b"]);
        assert_eq!(word_texts("  lead in"), vec!["  ", "lead ", "in"]);
        assert_eq!(word_texts("wait... go"), vec!["wait", "... ", "go"]);
        assert_eq!(word_texts("tab\there"), vec!["tab\t", "here"]);
        assert_eq!(word_texts("nb\u{A0}sp"), vec!["nb\u{A0}", "sp"]);
        assert_eq!(word_texts(""), Vec::<&str>::new());
    }

    #[test]
    fn a_double_click_selects_the_word_under_it_with_its_trailing_spaces() {
        let text = "word, next";
        for off in 0..4 {
            assert_eq!(selected(text, off), "word", "at {off}");
        }
        assert_eq!(selected(text, 4), ", ", "the comma");
        assert_eq!(selected(text, 5), ", ", "the space after the comma");
        assert_eq!(selected(text, 6), "next");
        assert_eq!(selected(text, 10), "next", "the end of the text");
        assert_eq!(selected("hello world", 2), "hello ");
        assert_eq!(
            selected("hello world", 5),
            "hello ",
            "a space belongs to the word it trails"
        );
        assert_eq!(selected("  lead", 1), "  ", "leading spaces are their own");
        assert_eq!(word_at("", 0), (0, 0));
        assert_eq!(word_at("abc", 99), (0, 3));
    }

    /// No script-specific rule: an Arabic word with a tatweel and harakat
    /// is one text unit (the marks never split it), the Arabic comma is
    /// punctuation.
    #[test]
    fn arabic_with_tatweel_and_harakat_is_one_word() {
        let w1 = "كتـــاب";
        let w2 = "جَدِّيدٌ";
        let text = format!("{w1}، {w2} end");
        assert_eq!(word_texts(&text), vec![w1, "، ", &format!("{w2} "), "end"]);
        /* Every byte offset inside the first word (on a char boundary)
        selects the whole word: a tatweel or a mark is never a seam. */
        for (i, _) in w1.char_indices() {
            assert_eq!(selected(&text, i), w1, "at {i}");
        }
        let w2_at = text.find(w2).expect("w2");
        for (i, _) in w2.char_indices() {
            assert_eq!(selected(&text, w2_at + i), format!("{w2} "), "at {i}");
        }
    }

    /// A non-text element is a word by itself: an inline object, a line
    /// break, a page break.
    #[test]
    fn objects_and_breaks_are_words_by_themselves() {
        assert_eq!(
            word_texts("a\u{FFFC}\u{FFFC} b"),
            vec!["a", "\u{FFFC}", "\u{FFFC} ", "b"]
        );
        assert_eq!(
            word_texts("end\u{2028}next"),
            vec!["end", "\u{2028}", "next"]
        );
        assert_eq!(selected("end \u{2028}next", 1), "end ");
        assert_eq!(word_texts("one\u{000C}two"), vec!["one", "\u{000C}", "two"]);
    }

    /// The motions step over the same words: Ctrl+Right lands on a word's
    /// end, Ctrl+Left on the start of the word before the caret — for
    /// every offset of every sample.
    #[test]
    fn the_word_stops_are_the_word_edges() {
        for text in [
            "word, next",
            "a   b",
            "  lead in  ",
            "wait... go!",
            "isn't 3.14 done",
            "a\u{FFFC}\u{FFFC}b",
            "كتـــاب، جدّيد end",
            "end\u{2028}next",
            "",
        ] {
            for off in 0..=text.len() {
                if !text.is_char_boundary(off) {
                    continue;
                }
                if off < text.len() {
                    assert_eq!(word_stop_forward(text, off), word_at(text, off).1);
                } else {
                    assert_eq!(word_stop_forward(text, off), text.len());
                }
                if off > 0 {
                    let before = text[..off].char_indices().next_back().expect("char").0;
                    assert_eq!(word_stop_backward(text, off), word_at(text, before).0);
                } else {
                    assert_eq!(word_stop_backward(text, off), 0);
                }
            }
        }
    }

    #[test]
    fn typing_over_a_double_clicked_word_keeps_its_trailing_spaces() {
        let text = "word, next";
        /* `, ` before `next`: the comma is replaced, the space stays. */
        assert_eq!(typing_replace_end(text, 4, 6, "X"), 5);
        let text = "one two three";
        assert_eq!(typing_replace_end(text, 4, 8, "X"), 7, "`two ` keeps ` `");
        assert_eq!(typing_replace_end(text, 0, 8, "X"), 7, "a drag too");
        assert_eq!(typing_replace_end(text, 4, 7, "X"), 7, "no space selected");
        assert_eq!(typing_replace_end(text, 3, 4, "X"), 4, "only a space");
        assert_eq!(typing_replace_end(text, 4, 8, "X "), 8, "typed space");
        assert_eq!(typing_replace_end(text, 4, 8, ""), 8, "nothing typed");
        assert_eq!(typing_replace_end(text, 8, 13, "X"), 13, "to the end");
        assert_eq!(
            typing_replace_end("one two  three", 4, 8, "X"),
            8,
            "more separators follow"
        );
        assert_eq!(typing_replace_end(text, 4, 4, "X"), 4, "a caret");
    }
}
