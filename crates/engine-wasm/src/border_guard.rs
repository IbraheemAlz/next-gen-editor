//! Issue #503 (residue) - the command-boundary range guard for a border
//! stroke's width.
//!
//! `BridgeBorderStroke.size_eighth_pt` is a `u16`, so `SetCellBorders` /
//! `SetParagraphBorders` took up to 8191 pt for a line (Word draws 1/4 pt
//! to 6 pt). The `.docx` reader and HTML paste CLAMP into that range
//! (`engine::border_bounds`); this guard is the command path's twin, run by
//! `Engine::apply_command` right after the point guard:
//!
//! | command | field | accepted |
//! | --- | --- | --- |
//! | `SetCellBorders` / `SetParagraphBorders` | `borders.{top,left,bottom,right}.size_eighth_pt` | `2 ..= 48` for a drawn line, `0 ..= 48` for style `None` |
//!
//! A value outside is refused with `Event::Error { kind: OutOfRange }`
//! naming the command, the field and the value; nothing is applied (the
//! #341 "error => no mutation" invariant).
//!
//! A NEW command field that carries a [`bridge::BridgeBorderStroke`] must be
//! added to [`first_refused`].

use bridge::{BridgeBorderStroke, BridgeBorderStyle, BridgeCellBorders, Command, ErrorKind, Event};
use engine::{BorderStyle, border_size_in_bound, border_size_range};

/// A refused field: its name, value and the range it should have held.
struct Refused {
    field: String,
    value: u16,
    range: (u16, u16),
}

fn engine_style(style: &BridgeBorderStyle) -> BorderStyle {
    match style {
        BridgeBorderStyle::None => BorderStyle::None,
        _ => BorderStyle::Single,
    }
}

fn stroke(field: &str, s: &Option<BridgeBorderStroke>) -> Option<Refused> {
    let s = s.as_ref()?;
    let style = engine_style(&s.style);
    (!border_size_in_bound(&style, s.size_eighth_pt)).then(|| Refused {
        field: format!("{field}.size_eighth_pt"),
        value: s.size_eighth_pt,
        range: border_size_range(&style),
    })
}

fn borders(base: &str, b: &BridgeCellBorders) -> Option<Refused> {
    stroke(&format!("{base}.top"), &b.top)
        .or_else(|| stroke(&format!("{base}.left"), &b.left))
        .or_else(|| stroke(&format!("{base}.bottom"), &b.bottom))
        .or_else(|| stroke(&format!("{base}.right"), &b.right))
}

fn first_refused(cmd: &Command) -> Option<Refused> {
    match cmd {
        Command::SetCellBorders { borders: b, .. }
        | Command::SetParagraphBorders { borders: b, .. } => borders("borders", b),
        _ => None,
    }
}

/// `Ok` when every border stroke width `cmd` carries is inside Word's
/// range; otherwise the typed `OutOfRange` reply (the dispatcher stamps the
/// wire name on it).
pub(crate) fn check_command(cmd: &Command) -> Result<(), Box<Event>> {
    match first_refused(cmd) {
        None => Ok(()),
        Some(r) => Err(Box::new(Event::error_kind(
            ErrorKind::OutOfRange,
            format!(
                "{}: {} {} is out of range ({}..={} eighths of a point, \
                 Word's border width limits)",
                cmd.kind().name(),
                r.field,
                r.value,
                r.range.0,
                r.range.1
            ),
        ))),
    }
}
