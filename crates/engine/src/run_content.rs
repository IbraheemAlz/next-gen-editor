//! Issues #335 / #357 — how run-content elements that carry no text of
//! their own are encoded in [`crate::Paragraph::text`].
//!
//! The `.docx` reader maps each element onto one character; layout gives
//! the character its meaning; the writer turns the character back into
//! the element (never the raw character, which Word treats differently).
//!
//! | element               | character | layout                          |
//! |-----------------------|-----------|---------------------------------|
//! | `<w:softHyphen/>`     | U+00AD    | invisible break opportunity; a  |
//! |                       |           | synthetic hyphen when broken    |
//! | `<w:noBreakHyphen/>`  | U+2011    | a hyphen that never breaks      |

/// U+00AD SOFT HYPHEN — `<w:softHyphen/>` (ECMA-376 §17.3.3.29): an
/// optional hyphenation point. Invisible while the line does not break
/// there; a line that breaks right after it ends with a drawn hyphen that
/// is not part of the text.
pub const SOFT_HYPHEN: char = '\u{00AD}';

/// U+2011 NON-BREAKING HYPHEN — `<w:noBreakHyphen/>` (ECMA-376
/// §17.3.3.18): renders as a hyphen and is never a break opportunity
/// (UAX #14 class GL).
pub const NON_BREAKING_HYPHEN: char = '\u{2011}';
