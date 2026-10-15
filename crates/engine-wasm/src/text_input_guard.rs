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
//! Covered here: body text (`InsertText`, `ReplaceRange`,
//! `UpdateComposition`, `PastePlain`, `RenderPage`, a comment / reply body)
//! and the one-line values that end up in the package (review author and
//! date, comment authors, a field instruction, an opened file's name, style
//! ids / names, font family and language names). `PasteHtml` is covered by
//! the HTML parser (`engine::html`, entity-decoded text is repaired) and a
//! `.txt` / `.html` open by the opener; a `.docx` by the reader's
//! well-formedness repair. A new text-bearing command MUST be added to
//! [`sanitize_command`]; the writer's last-line defence (`format_docx`
//! `WriteNote::ForbiddenCharactersReplaced`) and the #468 save gate keep a
//! missed one from corrupting a file.

use bridge::{
    BridgeSpanStylePatch, BridgeStyleProperties, Command, ErrorKind, Event, TextAttrsPatch,
};
use engine::text_input::{InputTextRefusal, sanitize_line_in_place, sanitize_text_in_place};

/// A refused argument: which field of the command, and why.
struct Refused {
    field: &'static str,
    why: InputTextRefusal,
}

fn body(field: &'static str, s: &mut String) -> Result<(), Refused> {
    sanitize_text_in_place(s).map_err(|why| Refused { field, why })
}

fn line(field: &'static str, s: &mut String) -> Result<(), Refused> {
    sanitize_line_in_place(s).map_err(|why| Refused { field, why })
}

fn opt_line(field: &'static str, s: &mut Option<String>) -> Result<(), Refused> {
    s.as_mut().map_or(Ok(()), |s| line(field, s))
}

fn attrs(a: &mut TextAttrsPatch) -> Result<(), Refused> {
    opt_line("attrs.font_family", &mut a.font_family)?;
    opt_line("attrs.language", &mut a.language)
}

fn span_patch(p: &mut BridgeSpanStylePatch) -> Result<(), Refused> {
    opt_line("properties.run_props.font_family", &mut p.font_family)
}

fn style_properties(p: &mut BridgeStyleProperties) -> Result<(), Refused> {
    if let Some(run) = p.run_props.as_mut() {
        span_patch(run)?;
    }
    opt_line("properties.based_on", &mut p.based_on)?;
    opt_line("properties.display_name", &mut p.display_name)
}

fn sanitize_fields(cmd: &mut Command) -> Result<(), Refused> {
    match cmd {
        Command::InsertText { text, .. }
        | Command::ReplaceRange { text, .. }
        | Command::UpdateComposition { text, .. }
        | Command::PastePlain { text }
        | Command::RenderPage { text, .. } => body("text", text),
        Command::InsertComment { text, author, .. }
        | Command::ReplyToComment { text, author, .. } => {
            body("text", text)?;
            line("author", author)
        }
        Command::SetReviewIdentity { author, date } => {
            line("author", author)?;
            line("date", date)
        }
        Command::SetFieldInstruction { instruction, .. } => line("instruction", instruction),
        Command::OpenDocument { name, .. } => opt_line("name", name),
        Command::ApplyStyle { style_id, .. } => opt_line("style_id", style_id),
        Command::ModifyStyle {
            style_id,
            properties,
        } => {
            line("style_id", style_id)?;
            style_properties(properties)
        }
        Command::ApplyFormatting { attrs: a, .. } => attrs(a),
        _ => Ok(()),
    }
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
