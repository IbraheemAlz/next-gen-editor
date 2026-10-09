//! Issue #295 — a run's tracked formatting change (`<w:rPrChange>`) read
//! as a [`engine::RevisionKind::FormatChange`] revision over the run's
//! text, so review lists it and accept / reject resolve it.
//!
//! The element itself keeps riding the run's grab bag verbatim (issue
//! #84): the bag is what the writer re-emits, and its `w:id` is made
//! package-unique at save time (`writer::revision_ids`) — a run split in
//! two writes it once with its own id and once with a fresh one. The
//! revision carries what accept / reject need: the change's `w:id` /
//! `w:author` / `w:date`, and in `prev_attrs` the formatting it recorded
//! (the nested `<w:rPr>`), resolved like a live run's. Accepting drops the
//! bag element (`SpanStyle::for_typing`); rejecting restores
//! `prev_attrs`.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::schema::ct_rpr::{apply_rpr, attr_val, rpr_child_is_modeled};
use crate::schema::grab_bag::{NamespaceScope, slice_fragment, stash};
use crate::style_resolver::StyleResolver;
use engine::SpanStyle;

/// The revision template (`start` / `end` are the caller's: the run's
/// text range) for the `<w:rPrChange>` start tag `e` whose whole element
/// is `frag` (self-closing or not). `baseline` is the paragraph-mark run
/// formatting the live run resolved against.
pub(crate) fn format_change_revision(
    e: &BytesStart<'_>,
    frag: &[u8],
    ns: &NamespaceScope,
    resolver: &StyleResolver<'_>,
    baseline: &SpanStyle,
) -> engine::Revision {
    let (direct, r_style) = recorded_rpr(frag, ns);
    let mut prev = resolver.resolve_run(baseline.clone(), r_style.as_deref(), direct);
    /* Issue #104 — like a live run, the recorded formatting keeps its
    character-style id, so a rejected change writes `<w:rStyle>` back
    instead of flattening the style into direct formatting. */
    prev.char_style = r_style;
    engine::Revision {
        start: 0,
        end: 0,
        kind: engine::RevisionKind::FormatChange,
        author: attr_val(e, b"w:author").unwrap_or_default(),
        date: attr_val(e, b"w:date").unwrap_or_default(),
        id: attr_val(e, b"w:id").and_then(|v| v.trim().parse().ok()),
        prev_attrs: Some(prev),
        move_name: None,
    }
}

/// The run formatting `<w:rPrChange>`'s nested `<w:rPr>` records: its
/// modeled children folded like a live `<w:rPr>`, the unmodeled ones
/// verbatim in the grab bag, and its `<w:rStyle>`.
fn recorded_rpr(frag: &[u8], ns: &NamespaceScope) -> (SpanStyle, Option<String>) {
    let mut style = SpanStyle::default();
    let mut r_style = None;
    let mut reader = Reader::from_reader(frag);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    /* 1 inside `<w:rPrChange>`, 2 inside its `<w:rPr>`. */
    let mut depth = 0u32;
    loop {
        let start = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(c)) if depth == 2 => {
                let name = c.name().as_ref().to_vec();
                let modeled = rpr_child_is_modeled(&name);
                if modeled {
                    apply_rpr(&name, &c, &mut style);
                }
                let end = c.to_end().into_owned();
                let mut skip = Vec::new();
                if reader.read_to_end_into(end.name(), &mut skip).is_err() {
                    break;
                }
                if !modeled
                    && let Some(bytes) =
                        slice_fragment(frag, start, reader.buffer_position() as usize)
                {
                    stash(&mut style.grab_bag, bytes, ns);
                }
            }
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::Empty(c)) if depth == 2 => {
                let name = c.name();
                if name.as_ref() == b"w:rStyle" {
                    r_style = attr_val(&c, b"w:val");
                } else if rpr_child_is_modeled(name.as_ref()) {
                    apply_rpr(name.as_ref(), &c, &mut style);
                } else if let Some(bytes) =
                    slice_fragment(frag, start, reader.buffer_position() as usize)
                {
                    stash(&mut style.grab_bag, bytes, ns);
                }
            }
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (style, r_style)
}
