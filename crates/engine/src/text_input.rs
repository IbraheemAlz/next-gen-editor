//! Issue #467 — the input boundary's text normalisation.
//!
//! Text reaches the model from many doors (typing, a clipboard paste, an
//! IME commit, a comment body, a `.txt` / `.html` file). Every character the
//! model stores is eventually written into a WordprocessingML part, and
//! XML 1.0 cannot carry most C0 control characters, `U+FFFE` or `U+FFFF`
//! (not even as a character reference): a pasted `\u{1}` used to reach
//! `<w:t>` raw and the saved `document.xml` was not well-formed — Word
//! reported the file as corrupt, and since issue #468 the save gate refuses
//! the save outright.
//!
//! This module is the ONE definition of what the engine accepts. Every
//! entry point calls it (the dispatcher's command pre-pass in `engine-wasm`
//! `text_input_guard.rs` for typed / pasted / composed / commented text, the
//! HTML parser for entity-decoded text, the `.txt` opener for file text) —
//! never a per-command copy of the table.
//!
//! | input                              | body text                | single line   |
//! |------------------------------------|--------------------------|---------------|
//! | `U+0009` TAB, `U+000A` LF, `U+000D` CR | kept                 | → space       |
//! | `U+000B` VERTICAL TAB              | → `U+2028` (line break)  | → space       |
//! | `U+000C` FORM FEED                 | kept (the model's page break) | → space  |
//! | other C0 (`U+0000..=U+001F`)       | dropped                  | dropped       |
//! | `U+FFFE`, `U+FFFF`                 | refused / lossy: `U+FFFD` | same         |
//!
//! The model spells a manual line break as `U+2028` and a manual page break
//! as `U+000C` (`run_content`, the writer's `<w:br/>` / `<w:br
//! w:type="page"/>`), so mapping Word's paste behaviour (a vertical tab is
//! a soft return, a form feed a page break) is a character-for-character
//! translation. `U+007F` and the C1 range are legal XML characters and are
//! kept. Lone surrogates cannot exist in a Rust `String`; the shell refuses
//! them before the command is sent (the wasm-bindgen string glue would
//! otherwise turn them into `U+FFFD`).
//!
//! **Refused vs. repaired.** Text the user typed or pasted is *refused*
//! when it holds a noncharacter (`U+FFFE` / `U+FFFF`): there is no faithful
//! reading, and nothing is inserted. Text the user cannot retype — a file's
//! bytes, an HTML entity decoded by the parser — is *repaired* (the
//! `_lossy` variants map the same characters to `U+FFFD`), because refusing
//! a whole document for one stray byte helps nobody.

use std::borrow::Cow;
use std::fmt;

/// The model's spelling of a manual line break (`<w:br/>`).
pub const LINE_BREAK: char = '\u{2028}';
/// The model's spelling of a manual page break (`<w:br w:type="page"/>`).
pub const PAGE_BREAK: char = '\u{000C}';
/// What a character XML cannot carry becomes when it is repaired.
pub const REPLACEMENT: char = '\u{FFFD}';

/// XML 1.0 `Char`: tab, LF, CR, and everything from `U+0020` except the
/// surrogates (not representable as a `char`) and `U+FFFE` / `U+FFFF`.
///
/// The single predicate behind the input boundary and the writer's
/// last-line defence.
pub fn is_xml_char(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}'
    )
}

/// The first character of a text argument the engine refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputTextRefusal {
    /// The refused character (`U+FFFE` or `U+FFFF`).
    pub ch: char,
    /// Its byte offset in the offending string.
    pub offset: usize,
}

impl fmt::Display for InputTextRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "text contains U+{:04X} at byte {}, which is not a character a document can hold \
             (nothing was applied)",
            self.ch as u32, self.offset
        )
    }
}

impl std::error::Error for InputTextRefusal {}

/// What a character becomes.
enum Action {
    Keep,
    Drop,
    Map(char),
    Refuse,
}

/// Which kind of string is being cleaned.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Document content: tabs and line / page breaks are meaningful.
    Body,
    /// A one-line value (an author, a field instruction, a file name):
    /// whitespace controls collapse to a space.
    Line,
}

fn action(c: char, kind: Kind) -> Action {
    match c {
        '\u{FFFE}' | '\u{FFFF}' => Action::Refuse,
        '\t' | '\n' | '\r' | '\u{B}' | '\u{C}' if kind == Kind::Line => Action::Map(' '),
        '\t' | '\n' | '\r' | PAGE_BREAK => Action::Keep,
        '\u{B}' => Action::Map(LINE_BREAK),
        c if (c as u32) < 0x20 => Action::Drop,
        _ => Action::Keep,
    }
}

/// `true` when `s` needs no change at all (the overwhelmingly common case:
/// nothing is allocated).
fn is_clean(s: &str, kind: Kind) -> bool {
    s.chars().all(|c| matches!(action(c, kind), Action::Keep))
}

fn clean(s: &str, kind: Kind, lossy: bool) -> Result<Cow<'_, str>, InputTextRefusal> {
    if is_clean(s, kind) {
        return Ok(Cow::Borrowed(s));
    }
    let mut out = String::with_capacity(s.len());
    for (offset, c) in s.char_indices() {
        match action(c, kind) {
            Action::Keep => out.push(c),
            Action::Drop => {}
            Action::Map(m) => out.push(m),
            Action::Refuse if lossy => out.push(REPLACEMENT),
            Action::Refuse => return Err(InputTextRefusal { ch: c, offset }),
        }
    }
    Ok(Cow::Owned(out))
}

/// Normalise typed / pasted / composed BODY text (see the module table).
/// Borrows when nothing changes. `U+FFFE` / `U+FFFF` is refused.
pub fn sanitize_input_text(s: &str) -> Result<Cow<'_, str>, InputTextRefusal> {
    clean(s, Kind::Body, false)
}

/// [`sanitize_input_text`] for text the user cannot retype (a file's bytes,
/// a decoded HTML entity): a noncharacter becomes `U+FFFD` instead of a
/// refusal. Never fails.
pub fn sanitize_input_text_lossy(s: &str) -> Cow<'_, str> {
    match clean(s, Kind::Body, true) {
        Ok(c) => c,
        Err(_) => unreachable!("the lossy variant maps every refusal"),
    }
}

/// Normalise a ONE-LINE value (an author, a field instruction, a file
/// name, a style id): whitespace controls become a space, other C0 controls
/// are dropped. `U+FFFE` / `U+FFFF` is refused.
pub fn sanitize_input_line(s: &str) -> Result<Cow<'_, str>, InputTextRefusal> {
    clean(s, Kind::Line, false)
}

/// Normalise `field` in place (body text); see [`sanitize_input_text`].
pub fn sanitize_text_in_place(field: &mut String) -> Result<(), InputTextRefusal> {
    if let Cow::Owned(cleaned) = sanitize_input_text(field)? {
        *field = cleaned;
    }
    Ok(())
}

/// Normalise `field` in place (one line); see [`sanitize_input_line`].
pub fn sanitize_line_in_place(field: &mut String) -> Result<(), InputTextRefusal> {
    if let Cow::Owned(cleaned) = sanitize_input_line(field)? {
        *field = cleaned;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_is_borrowed() {
        for s in [
            "",
            "plain",
            "a\tb\nc\rd",
            "مرحبا\u{200F}",
            "x\u{7F}\u{85}y",
            "\u{1F600}",
        ] {
            assert!(
                matches!(sanitize_input_text(s), Ok(Cow::Borrowed(_))),
                "{s:?}"
            );
        }
    }

    #[test]
    fn body_table() {
        assert_eq!(sanitize_input_text("a\u{1}b").unwrap(), "ab");
        assert_eq!(sanitize_input_text("\u{0}\u{8}\u{E}\u{1F}x").unwrap(), "x");
        assert_eq!(sanitize_input_text("a\u{B}b").unwrap(), "a\u{2028}b");
        assert_eq!(sanitize_input_text("a\u{C}b").unwrap(), "a\u{C}b");
        assert_eq!(sanitize_input_text("a\tb\nc\r").unwrap(), "a\tb\nc\r");
        // Legal-but-discouraged XML characters stay.
        assert_eq!(
            sanitize_input_text("\u{7F}\u{85}\u{FEFF}").unwrap(),
            "\u{7F}\u{85}\u{FEFF}"
        );
    }

    #[test]
    fn noncharacters_are_refused_with_their_offset() {
        let err = sanitize_input_text("ab\u{FFFE}c").unwrap_err();
        assert_eq!(
            err,
            InputTextRefusal {
                ch: '\u{FFFE}',
                offset: 2
            }
        );
        let err = sanitize_input_text("é\u{FFFF}").unwrap_err();
        assert_eq!(
            err,
            InputTextRefusal {
                ch: '\u{FFFF}',
                offset: 2
            }
        );
        assert!(err.to_string().contains("U+FFFF"));
        // Other noncharacters are legal XML characters.
        assert!(sanitize_input_text("\u{FDD0}\u{1FFFE}").is_ok());
    }

    #[test]
    fn lossy_variant_repairs_instead_of_refusing() {
        assert_eq!(
            sanitize_input_text_lossy("a\u{1}\u{FFFE}b\u{B}\u{FFFF}"),
            "a\u{FFFD}b\u{2028}\u{FFFD}"
        );
    }

    #[test]
    fn line_values_collapse_whitespace_controls() {
        assert_eq!(
            sanitize_input_line("Ann\nSmith\t\u{B}\u{1}").unwrap(),
            "Ann Smith  "
        );
        assert!(sanitize_input_line("x\u{FFFE}").is_err());
    }

    #[test]
    fn in_place_helpers_only_touch_dirty_strings() {
        let mut s = String::from("ok");
        sanitize_text_in_place(&mut s).unwrap();
        assert_eq!(s, "ok");
        let mut s = String::from("a\u{2}b");
        sanitize_text_in_place(&mut s).unwrap();
        assert_eq!(s, "ab");
        let mut s = String::from("a\u{FFFF}");
        assert!(sanitize_text_in_place(&mut s).is_err());
        assert_eq!(s, "a\u{FFFF}", "a refusal leaves the field alone");
        let mut s = String::from("a\nb");
        sanitize_line_in_place(&mut s).unwrap();
        assert_eq!(s, "a b");
    }

    #[test]
    fn xml_char_predicate_matches_the_spec() {
        for c in [
            '\t',
            '\n',
            '\r',
            ' ',
            'a',
            '\u{D7FF}',
            '\u{E000}',
            '\u{FFFD}',
            '\u{10000}',
            '\u{10FFFF}',
        ] {
            assert!(is_xml_char(c), "{c:?}");
        }
        for c in [
            '\u{0}', '\u{1}', '\u{B}', '\u{C}', '\u{1F}', '\u{FFFE}', '\u{FFFF}',
        ] {
            assert!(!is_xml_char(c), "{c:?}");
        }
    }
}
