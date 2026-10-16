//! Issue #467 — the command-boundary text guard.
//!
//! Text reaches the model through many commands (`InsertText`, a paste, an
//! IME composition, a comment, a replace). XML cannot carry most C0 control
//! characters or `U+FFFE` / `U+FFFF`, so a character like a pasted `\u{1}`
//! used to land raw in `<w:t>` and make the saved `document.xml` malformed.
//!
//! [`sanitize_command`] is the dispatcher's pre-pass (right after the #407
//! finiteness guard, before the story / protection gates): it normalises
//! every text-bearing argument in place through the ONE table in
//! [`engine::text_input`] — control characters dropped, a vertical tab a
//! line break, a form feed a page break — and refuses a `U+FFFE` / `U+FFFF`
//! with `Event::Error { kind: InvalidArgument }` (nothing applied). The
//! handlers see only clean text, so no handler (story, tracked, interactive,
//! composition) carries its own copy.
//!
//! Issue #485 — the set of fields is NOT a list in this file. It is the
//! per-command declaration in `bridge::command_meta!` (`; (field: class,
//! ...)`), walked through the macro-generated `Command::visit_text_mut`:
//! body text (`InsertText`, `ReplaceRange`, `UpdateComposition`,
//! `PastePlain`, `RenderPage`, a comment / reply body) is
//! `TextClass::Body`; the one-line values that end up in the package
//! (review author and date, comment authors, a field instruction, an
//! opened file's name, style ids / names, font family and language names,
//! an image's MIME type) are `TextClass::Line`; a field that never reaches
//! the document (a registry key, a password) or is cleaned elsewhere
//! (`PasteHtml` by the HTML parser, a `.txt` / `.html` open by the opener,
//! a `.docx` by the reader's well-formedness repair) is declared `Exempt`
//! with its reason. A new command or field cannot be forgotten: the macro
//! does not compile an unclassified variant and `bridge`'s
//! `every_command_string_field_is_classified` fails an unclassified field.
//! The writer's last-line defence (`format_docx`
//! `WriteNote::ForbiddenCharactersReplaced`, reported on
//! `Event::DocumentSaved.notes`) and the #468 save gate remain behind it.

use std::ops::ControlFlow;

use bridge::{Command, ErrorKind, Event, TextClass};
use engine::text_input::{InputTextRefusal, sanitize_line_in_place, sanitize_text_in_place};

/// A refused argument: which field of the command, and why.
struct Refused {
    field: &'static str,
    why: InputTextRefusal,
}

/// Normalise every declared text field of `cmd` in place (the walk stops at
/// the first refusal).
fn sanitize_fields(cmd: &mut Command) -> Result<(), Refused> {
    let mut refused: Option<Refused> = None;
    let _ = cmd.visit_text_mut(&mut |field, class, s| {
        let result = match class {
            TextClass::Body => sanitize_text_in_place(s),
            TextClass::Line => sanitize_line_in_place(s),
            /* A nested payload reports its leaves with their own class;
            an exempt field never reaches the document. */
            TextClass::Nested | TextClass::Exempt(_) => return ControlFlow::Continue(()),
        };
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(why) => {
                refused = Some(Refused { field, why });
                ControlFlow::Break(())
            }
        }
    });
    refused.map_or(Ok(()), Err)
}

/// Normalise every text-bearing argument of `cmd` in place; a refusal is the
/// typed `InvalidArgument` reply naming the command, the field and the
/// character (the dispatcher stamps the wire name on it).
pub(crate) fn sanitize_command(cmd: &mut Command) -> Result<(), Box<Event>> {
    sanitize_fields(cmd).map_err(|r| {
        Box::new(Event::error_kind(
            ErrorKind::InvalidArgument,
            format!("{}: {}: {}", cmd.kind().name(), r.field, r.why),
        ))
    })
}
