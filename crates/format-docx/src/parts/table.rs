//! `<w:tbl>` parser (Phase 5 PR 2).
//!
//! Takes the raw `<w:tbl>...</w:tbl>` byte slice the document parser
//! captured for the passthrough optimisation and walks it into
//! `(Vec<i32> grid, TableProperties props, Vec<TableRow> rows)`.
//! Nested tables inside cells recurse through `parse_table_bytes`
//! again — straightforward because each `<w:tbl>` is a complete
//! well-formed XML fragment in the captured slice.
//!
//! The engine model treats `Table.dirty = false` + `source_xml = Some`
//! as the canonical state. Round-trip writes from `source_xml` verbatim
//! (Phase 3 passthrough); the parsed `rows` exist only so layout +
//! render can paint correctly, never for write-back at PR 2.

use crate::error::{DocxError, DocxWarning};
use crate::parts::document::is_block_level_marker;
use crate::schema::block_envelope::BlockEnvelopes;
use crate::schema::ct_ppr::parse_jc;
use crate::schema::ct_rpr::{attr_val, parse_hex_color, toggle_on};
use crate::schema::ct_tbl;
use crate::schema::grab_bag::{
    NamespaceScope, capture_subtree, slice_element, slice_fragment, stash,
};
use engine::{
    Block, BorderStroke, BorderStyle, CellBorders, CellMargins, CellWidth, GrabBag, RowHeight,
    Table, TableCell, TableProperties, TableRow, VMergeRole, VerticalAlign,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Issue #111 — deepest `<w:tbl>` nesting the typed model follows. The
/// outermost table is depth 0; a table nested at this depth or deeper is
/// kept as an opaque passthrough block (`source_xml` preserved verbatim,
/// `rows` empty) and reported through [`DocxWarning::TableNestingTooDeep`]
/// instead of recursing. Each level costs one `parse_table_bytes_at` frame
/// plus one copy of the remaining subtree, so the bound caps both stack
/// depth and the O(depth × size) re-parse — Apache POI's
/// `deep-table-cell.docx` (5000 levels, 1.2 MB) overflowed the native
/// stack after ~15 s of CPU without it. Word itself tolerates only a
/// handful of levels; 64 is far beyond any document authored by a human.
pub const MAX_TABLE_NESTING_DEPTH: u32 = 64;

/// Issue #84 — the grab-bag slot an unmodeled child of `parent`
/// (`w:tblPr` / `w:trPr` / `w:tcPr`) belongs to. `None` when the row /
/// cell it would attach to is not open (malformed nesting).
fn bag_for<'a>(
    parent: &[u8],
    props: &'a mut TableProperties,
    cur_row: &'a mut Option<TableRow>,
    cur_cell: &'a mut Option<TableCell>,
) -> Option<&'a mut Option<Box<GrabBag>>> {
    match parent {
        b"w:tblPr" => Some(&mut props.grab_bag),
        b"w:trPr" => cur_row.as_mut().map(|r| &mut r.props.grab_bag),
        b"w:tcPr" => cur_cell.as_mut().map(|c| &mut c.props.grab_bag),
        _ => None,
    }
}

/// Parse a `<w:tbl>...</w:tbl>` byte slice. Always starts with a
/// `<w:tbl>` opening tag; depth bookkeeping handles nested tables
/// inside cell content (each nested `<w:tbl>` produces a recursive
/// `parse_table_bytes` call when we hit the `<w:tc>` close).
///
/// `ns` is the enclosing part's root namespace scope (issue #84): the
/// slice has no root of its own, so foreign-prefixed grab-bag fragments
/// re-bind from the part that contained the table.
///
/// Non-fatal degradations (the [`MAX_TABLE_NESTING_DEPTH`] cap) are
/// discarded here; [`parse_table_bytes_with_warnings`] surfaces them.
pub fn parse_table_bytes(
    xml: &[u8],
    resolver: &crate::style_resolver::StyleResolver<'_>,
    ns: &NamespaceScope,
) -> Result<(Vec<i32>, TableProperties, Vec<TableRow>), DocxError> {
    let mut warnings = Vec::new();
    parse_table_bytes_at(xml, resolver, ns, 0, &mut warnings)
}

/// [`parse_table_bytes`], appending every non-fatal reader diagnostic to
/// `warnings`.
pub fn parse_table_bytes_with_warnings(
    xml: &[u8],
    resolver: &crate::style_resolver::StyleResolver<'_>,
    ns: &NamespaceScope,
    warnings: &mut Vec<DocxWarning>,
) -> Result<(Vec<i32>, TableProperties, Vec<TableRow>), DocxError> {
    parse_table_bytes_at(xml, resolver, ns, 0, warnings)
}

/// The recursive worker behind [`parse_table_bytes`]. `depth` is this
/// table's nesting level (outermost = 0); nested tables recurse at
/// `depth + 1` until [`MAX_TABLE_NESTING_DEPTH`], where the walk stops
/// and preserves the subtree opaquely.
fn parse_table_bytes_at(
    xml: &[u8],
    resolver: &crate::style_resolver::StyleResolver<'_>,
    ns: &NamespaceScope,
    depth: u32,
    warnings: &mut Vec<DocxWarning>,
) -> Result<(Vec<i32>, TableProperties, Vec<TableRow>), DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    let mut grid: Vec<i32> = Vec::new();
    let mut props = TableProperties::default();
    let mut rows: Vec<TableRow> = Vec::new();

    /* Element stack — `Vec<Vec<u8>>` keyed by element name. We push
    on Start, pop on End. Lets us decide context without juggling
    quick-xml's borrowed-event lifetimes. */
    let mut stack: Vec<Vec<u8>> = Vec::new();

    /* Current row + cell scratch. */
    let mut cur_row: Option<TableRow> = None;
    let mut cur_cell: Option<TableCell> = None;
    /* When we hit a `<w:p>` inside a cell, capture its source bytes the
    same way `parts::document` does; on `</w:p>` push a `Block::Paragraph`
    onto the cell with `source_xml` populated for the writer's
    passthrough. */
    let mut p_start_byte: Option<usize> = None;
    /* When we hit a nested `<w:tbl>` inside a cell, capture its source
    bytes; on the outermost `</w:tbl>` recurse via `parse_table_bytes`
    to build the nested `Block::Table`. We track depth so deeper nested
    `<w:tbl>`s don't trigger spurious flushes. */
    let mut nested_tbl_depth: u32 = 0;
    let mut nested_tbl_start: Option<usize> = None;

    let mut prev_pos: usize = 0;
    let mut in_table = false;
    /* Issue #120 — block-level passthrough inside the CURRENT cell (a
    `<w:sdt>` around cell paragraphs, bookmarks / whitespace between
    them); one tracker per `<w:tc>`, drained into the cell's blocks. */
    let mut cell_env = BlockEnvelopes::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_owned();
                let at_cell_level =
                    nested_tbl_depth == 0 && cur_cell.is_some() && p_start_byte.is_none();
                match name.as_slice() {
                    b"w:tbl" if !in_table => {
                        in_table = true;
                    }
                    b"w:tbl" if cur_cell.is_some() => {
                        /* Nested table inside a cell. */
                        if nested_tbl_depth == 0 {
                            nested_tbl_start = Some(prev_pos);
                            cell_env.note_block_start(prev_pos);
                        }
                        nested_tbl_depth += 1;
                    }
                    b"w:tr" if nested_tbl_depth == 0 => {
                        cur_row = Some(TableRow::default());
                    }
                    b"w:tc" if nested_tbl_depth == 0 => {
                        cur_cell = Some(TableCell::default());
                        cell_env = BlockEnvelopes::new();
                    }
                    b"w:p" if nested_tbl_depth == 0 && cur_cell.is_some() => {
                        p_start_byte = Some(prev_pos);
                        cell_env.note_block_start(prev_pos);
                    }
                    b"w:sdt" | b"w:customXml" if at_cell_level => {
                        cell_env.open_container(prev_pos);
                        if let Some(cell) = cur_cell.as_ref() {
                            cell_env.set_blocks_at_open(cell.blocks.len());
                        }
                    }
                    b"w:sdtPr" | b"w:sdtEndPr" | b"w:customXmlPr"
                        if at_cell_level && cell_env.in_container() =>
                    {
                        /* Property children of a cell-level container: never
                        live properties, always inside the envelope bytes. */
                        let _ = capture_subtree(xml, prev_pos, &mut reader, &e)?;
                        prev_pos = reader.buffer_position() as usize;
                        buf.clear();
                        continue;
                    }
                    _ => {}
                }
                /* Issue #84 — an unmodeled child of `<w:tblPr>` / `<w:trPr>`
                / `<w:tcPr>` (`<w:tblpPr>`, `<w:trPrChange>`, …): capture
                the whole subtree into the owning grab bag and skip it, so
                nothing inside is mistaken for a live property. */
                if nested_tbl_depth == 0
                    && let Some(parent) = stack.last()
                    && ct_tbl::child_is_modeled(parent, &name) == Some(false)
                {
                    if let Some(frag) = capture_subtree(xml, prev_pos, &mut reader, &e)?
                        && let Some(slot) = bag_for(parent, &mut props, &mut cur_row, &mut cur_cell)
                    {
                        stash(slot, frag, ns);
                    }
                    prev_pos = reader.buffer_position() as usize;
                    buf.clear();
                    continue;
                }
                if nested_tbl_depth == 0 {
                    handle_property_start(
                        &name,
                        &e,
                        &mut grid,
                        &mut props,
                        &mut cur_row,
                        &mut cur_cell,
                        &stack,
                    );
                }
                stack.push(name);
            }
            Event::Empty(e) => {
                let name = e.name().as_ref().to_owned();
                let at_cell_level =
                    nested_tbl_depth == 0 && cur_cell.is_some() && p_start_byte.is_none();
                if at_cell_level && name.as_slice() == b"w:p" {
                    /* Issue #120 — a self-closing `<w:p …/>` cell paragraph
                    (Word writes one for every empty cell): one `Empty`
                    event, so the Start / End arms never see it. Same
                    block as `<w:p></w:p>`, through the body parser. */
                    let end = reader.buffer_position() as usize;
                    cell_env.note_block_start(prev_pos);
                    if let Some(raw) = slice_element(xml, prev_pos, end, b"w:p")
                        && let Some(cell) = cur_cell.as_mut()
                    {
                        let mut p = parse_cell_paragraph(&raw, resolver, ns);
                        p.body_xml = cell_env.take_before();
                        cell.blocks.push(Block::Paragraph(p));
                        cell_env.note_block_end(end);
                    }
                    prev_pos = end;
                    buf.clear();
                    continue;
                }
                if at_cell_level && is_block_level_marker(&name) {
                    /* Issue #120 — a marker between two cell blocks. */
                    let end = reader.buffer_position() as usize;
                    if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                        cell_env.push_verbatim(frag);
                    }
                    prev_pos = end;
                    buf.clear();
                    continue;
                }
                if nested_tbl_depth == 0
                    && let Some(parent) = stack.last()
                    && ct_tbl::child_is_modeled(parent, &name) == Some(false)
                {
                    /* Issue #84 — unmodeled leaf child (`<w:tblLook>`,
                    `<w:bidiVisual>`, `<w:cnfStyle>`, `<w:noWrap>`, …) →
                    the owning grab bag, verbatim. */
                    let end = reader.buffer_position() as usize;
                    if let Some(frag) = slice_fragment(xml, prev_pos, end)
                        && let Some(slot) = bag_for(parent, &mut props, &mut cur_row, &mut cur_cell)
                    {
                        stash(slot, frag, ns);
                    }
                } else if nested_tbl_depth == 0 {
                    handle_property_empty(
                        &name,
                        &e,
                        &mut grid,
                        &mut props,
                        &mut cur_row,
                        &mut cur_cell,
                        &stack,
                    );
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:tbl" if nested_tbl_depth > 0 => {
                        nested_tbl_depth -= 1;
                        if nested_tbl_depth == 0
                            && let Some(start) = nested_tbl_start.take()
                            && let Some(cell) = cur_cell.as_mut()
                        {
                            let end = reader.buffer_position() as usize;
                            if let Some(raw) = slice_element(xml, start, end, b"w:tbl") {
                                if depth + 1 >= MAX_TABLE_NESTING_DEPTH {
                                    /* Issue #111 — Tier-3 opaque preservation.
                                    The subtree's bytes ride the passthrough
                                    (this table's `source_xml` and the
                                    enclosing ones' already contain them), so
                                    a resave is lossless; only the typed rows
                                    stop here. Never recurse past the cap. */
                                    warnings.push(DocxWarning::TableNestingTooDeep {
                                        limit: MAX_TABLE_NESTING_DEPTH,
                                    });
                                    cell.blocks.push(Block::Table(Table {
                                        grid: Vec::new(),
                                        props: TableProperties::default(),
                                        rows: Vec::new(),
                                        dirty: false,
                                        source_xml: Some(raw),
                                        body_xml: cell_env.take_before(),
                                    }));
                                    cell_env.note_block_end(end);
                                } else if let Ok((g, p, r)) =
                                    parse_table_bytes_at(&raw, resolver, ns, depth + 1, warnings)
                                {
                                    cell.blocks.push(Block::Table(Table {
                                        grid: g,
                                        props: p,
                                        rows: r,
                                        dirty: false,
                                        source_xml: Some(raw),
                                        body_xml: cell_env.take_before(),
                                    }));
                                    cell_env.note_block_end(end);
                                }
                            }
                        }
                    }
                    b"w:tbl" if in_table => {
                        in_table = false;
                    }
                    b"w:tr" if nested_tbl_depth == 0 => {
                        if let Some(row) = cur_row.take() {
                            rows.push(row);
                        }
                    }
                    /* Issue #120 — a cell-level container closes. */
                    b"w:sdt" | b"w:customXml"
                        if nested_tbl_depth == 0
                            && cur_cell.is_some()
                            && p_start_byte.is_none()
                            && cell_env.in_container() =>
                    {
                        let end = reader.buffer_position() as usize;
                        if let Some(cell) = cur_cell.as_mut() {
                            cell_env.close_container(xml, end, &mut cell.blocks);
                        }
                    }
                    b"w:tc" if nested_tbl_depth == 0 => {
                        if let Some(mut cell) = cur_cell.take()
                            && let Some(row) = cur_row.as_mut()
                        {
                            cell_env.finish(&mut cell.blocks);
                            row.cells.push(cell);
                        }
                    }
                    b"w:p"
                        if nested_tbl_depth == 0
                            && cur_cell.is_some()
                            && p_start_byte.is_some() =>
                    {
                        let p_end = reader.buffer_position() as usize;
                        let start = p_start_byte.take().unwrap();
                        if let Some(raw) = slice_element(xml, start, p_end, b"w:p") {
                            if let Some(cell) = cur_cell.as_mut() {
                                /* Issue #101 — cell paragraphs parse
                                through the body run parser (runs, rPr
                                grab bags, pictures, source bytes). */
                                let mut p = parse_cell_paragraph(&raw, resolver, ns);
                                p.body_xml = cell_env.take_before();
                                cell.blocks.push(Block::Paragraph(p));
                                cell_env.note_block_end(p_end);
                            }
                        }
                    }
                    _ => {}
                }
                if stack.last().map(|n| n.as_slice()) == Some(name.as_slice()) {
                    stack.pop();
                }
            }
            Event::Text(t)
                if nested_tbl_depth == 0
                    && cur_cell.is_some()
                    && p_start_byte.is_none()
                    && matches!(
                        stack.last().map(Vec::as_slice),
                        Some(b"w:tc" | b"w:sdtContent" | b"w:customXml")
                    )
                    && t.iter().all(u8::is_ascii_whitespace) =>
            {
                /* Issue #120 — whitespace between two cell blocks (a
                pretty-printed part) rides the following block. */
                let end = reader.buffer_position() as usize;
                if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                    cell_env.push_verbatim(frag);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        prev_pos = reader.buffer_position() as usize;
        buf.clear();
    }

    Ok((grid, props, rows))
}

/// Cell paragraph parser (issue #101). A cell `<w:p>` is parsed by the
/// SAME run-aware loop as a body paragraph (`parts::document`), so it
/// yields identical `StyleRun` spans (`<w:rPr>` via `schema::ct_rpr`,
/// unmodeled children into the run grab bags), inline objects (inline /
/// floating pictures, footnote refs), hyperlinks, tracked-change overlays,
/// fields, the resolved paragraph cascade + list binding, and its own
/// `source_xml` for the clean-paragraph passthrough.
///
/// Mechanism: the captured `<w:p>` bytes are re-rooted under a synthetic
/// `<w:document><w:body>` whose root re-declares the enclosing part's
/// namespace scope (`ns`), then handed to
/// [`crate::parts::document::parse_document_xml`]. The body parser's
/// reader offsets index the wrapper, where the paragraph sits verbatim, so
/// its `source_xml` capture is exactly `xml`; grab-bag capture checks
/// prefixes against the same root bindings the table walk uses. Before,
/// this helper kept only the concatenated text — editing a cell collapsed
/// the paragraph to one unstyled run and dropped `<w:drawing>` pictures.
///
/// Body-only state is discarded: a cell paragraph can never end a
/// document section (`section_end`), and comment ranges stay
/// body-paragraph-only as before.
fn parse_cell_paragraph(
    xml: &[u8],
    resolver: &crate::style_resolver::StyleResolver<'_>,
    ns: &NamespaceScope,
) -> engine::Paragraph {
    let mut wrapped: Vec<u8> = Vec::with_capacity(xml.len() + 256);
    wrapped.extend_from_slice(b"<w:document");
    if ns.uri("w").is_none() {
        wrapped.extend_from_slice(b" xmlns:w=\"");
        wrapped.extend_from_slice(crate::schema::NS_W.as_bytes());
        wrapped.push(b'"');
    }
    for (prefix, uri) in ns.declarations() {
        wrapped.extend_from_slice(b" xmlns:");
        wrapped.extend_from_slice(prefix.as_bytes());
        wrapped.extend_from_slice(b"=\"");
        wrapped.extend_from_slice(uri.as_bytes());
        wrapped.push(b'"');
    }
    wrapped.extend_from_slice(b"><w:body>");
    wrapped.extend_from_slice(xml);
    wrapped.extend_from_slice(b"</w:body></w:document>");

    let parsed = crate::parts::document::parse_document_xml(&wrapped, resolver)
        .ok()
        .and_then(|tree| match tree.blocks.front() {
            Some(Block::Paragraph(p)) => Some(p.clone()),
            _ => None,
        });
    match parsed {
        Some(mut p) => {
            p.section_end = None;
            p
        }
        /* The slice already parsed once inside the table walk, so this is
        unreachable in practice; never drop the cell's bytes regardless —
        a clean paragraph with `source_xml` rides the passthrough. */
        None => engine::Paragraph {
            source_xml: Some(xml.to_vec()),
            ..Default::default()
        },
    }
}

fn handle_property_start(
    name: &[u8],
    e: &BytesStart,
    grid: &mut Vec<i32>,
    props: &mut TableProperties,
    cur_row: &mut Option<TableRow>,
    cur_cell: &mut Option<TableCell>,
    stack: &[Vec<u8>],
) {
    let parent = stack.last().map(|n| n.as_slice()).unwrap_or(b"");
    match name {
        b"w:gridCol" => {
            if let Some(w) = attr_val(e, b"w:w").and_then(|v| v.parse().ok()) {
                grid.push(w);
            }
        }
        b"w:tcBorders" if cur_cell.is_some() => {
            if let Some(cell) = cur_cell.as_mut() {
                cell.props.borders = Some(CellBorders::default());
            }
        }
        b"w:tblBorders" => {
            props.borders = Some(CellBorders::default());
        }
        /* Phase 2 audit (gap B.1) — `<w:tcMar>` opens a per-cell
        margin override; children are `<w:top>` / `<w:left|start>` /
        `<w:bottom>` / `<w:right|end>` carrying `w:w` twips. The
        sentinel `Some(default)` here marks "cell specified an
        override" so the layout resolver knows to consult per-edge
        values; absent (`None`) keeps the table-level / Word-default
        fallback chain. */
        b"w:tcMar" if cur_cell.is_some() => {
            if let Some(cell) = cur_cell.as_mut() {
                cell.props.cell_margins = Some(CellMargins::default());
            }
        }
        _ => {
            handle_property_inner(name, e, grid, props, cur_row, cur_cell, parent);
        }
    }
}

fn handle_property_empty(
    name: &[u8],
    e: &BytesStart,
    grid: &mut Vec<i32>,
    props: &mut TableProperties,
    cur_row: &mut Option<TableRow>,
    cur_cell: &mut Option<TableCell>,
    stack: &[Vec<u8>],
) {
    let parent = stack.last().map(|n| n.as_slice()).unwrap_or(b"");
    match name {
        b"w:gridCol" => {
            if let Some(w) = attr_val(e, b"w:w").and_then(|v| v.parse().ok()) {
                grid.push(w);
            }
        }
        _ => {
            handle_property_inner(name, e, grid, props, cur_row, cur_cell, parent);
        }
    }
}

/// Shared between Start + Empty event handlers — every per-cell /
/// per-row / per-table property element that's a leaf in OOXML.
fn handle_property_inner(
    name: &[u8],
    e: &BytesStart,
    _grid: &mut Vec<i32>,
    props: &mut TableProperties,
    cur_row: &mut Option<TableRow>,
    cur_cell: &mut Option<TableCell>,
    parent: &[u8],
) {
    /* Cell-scoped properties (under `<w:tcPr>`). */
    if let Some(cell) = cur_cell.as_mut()
        && parent == b"w:tcPr"
    {
        match name {
            b"w:gridSpan" => {
                if let Some(v) = attr_val(e, b"w:val").and_then(|v| v.parse().ok()) {
                    cell.props.grid_span = v;
                }
            }
            b"w:vMerge" => {
                cell.props.v_merge = match attr_val(e, b"w:val").as_deref() {
                    Some("restart") => VMergeRole::Restart,
                    /* OOXML default for `<w:vMerge>` without `w:val` is
                    `continue` — Word's "merge with the cell above". */
                    _ => VMergeRole::Continue,
                };
            }
            b"w:vAlign" => {
                cell.props.v_align = match attr_val(e, b"w:val").as_deref() {
                    Some("center") => VerticalAlign::Center,
                    Some("bottom") => VerticalAlign::Bottom,
                    _ => VerticalAlign::Top,
                };
            }
            b"w:shd" => {
                cell.props.shading = attr_val(e, b"w:fill").and_then(|v| parse_hex_color(&v));
            }
            b"w:tcW" => {
                cell.props.width = parse_cell_width(e);
            }
            _ => {}
        }
        /* Border edges under `<w:tcBorders>`. */
        return;
    }
    /* Phase 2 audit (gap B.1) — per-cell `<w:tcMar>` edge values.
    `<w:w>` carries twips (default `w:type="dxa"`). Each edge is
    `Option<i32>` so an unset edge correctly inherits from the
    table default instead of being read as a literal zero. */
    if let Some(cell) = cur_cell.as_mut()
        && parent == b"w:tcMar"
    {
        let twips: i32 = attr_val(e, b"w:w")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let m = cell
            .props
            .cell_margins
            .get_or_insert_with(CellMargins::default);
        match name {
            b"w:top" => m.top_twips = Some(twips),
            b"w:bottom" => m.bottom_twips = Some(twips),
            b"w:left" | b"w:start" => m.left_twips = Some(twips),
            b"w:right" | b"w:end" => m.right_twips = Some(twips),
            _ => {}
        }
        return;
    }
    /* Phase 2 audit (gap B.2) — table-default `<w:tblCellMar>` edges.
    Children are `<w:top>` / `<w:left|start>` / `<w:bottom>` /
    `<w:right|end>` with `w:w` twips. */
    if parent == b"w:tblCellMar" {
        let twips: i32 = attr_val(e, b"w:w")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let m = &mut props.cell_margins;
        match name {
            b"w:top" => m.top_twips = Some(twips),
            b"w:bottom" => m.bottom_twips = Some(twips),
            b"w:left" | b"w:start" => m.left_twips = Some(twips),
            b"w:right" | b"w:end" => m.right_twips = Some(twips),
            _ => {}
        }
        return;
    }
    /* Cell border edges — parent must be `<w:tcBorders>` and we must
    have a current cell. */
    if let Some(cell) = cur_cell.as_mut()
        && parent == b"w:tcBorders"
    {
        let edge = match name {
            b"w:top" => Some(&mut cell.props.borders.as_mut().unwrap().top),
            b"w:left" | b"w:start" => Some(&mut cell.props.borders.as_mut().unwrap().left),
            b"w:bottom" => Some(&mut cell.props.borders.as_mut().unwrap().bottom),
            b"w:right" | b"w:end" => Some(&mut cell.props.borders.as_mut().unwrap().right),
            _ => None,
        };
        if let Some(edge_slot) = edge {
            *edge_slot = Some(parse_border_stroke(e));
        }
        return;
    }
    /* Row-scoped properties (under `<w:trPr>`). */
    if let Some(row) = cur_row.as_mut()
        && parent == b"w:trPr"
    {
        match name {
            b"w:trHeight" => {
                let twips = attr_val(e, b"w:val").and_then(|v| v.parse().ok());
                let rule = attr_val(e, b"w:hRule");
                if let Some(t) = twips {
                    row.props.height = Some(match rule.as_deref() {
                        Some("exact") => RowHeight::Exact { twips: t },
                        Some("atLeast") => RowHeight::AtLeast { twips: t },
                        _ => RowHeight::AtLeast { twips: t },
                    });
                }
            }
            b"w:cantSplit" => row.props.cant_split = true,
            b"w:tblHeader" => row.props.header = true,
            _ => {}
        }
        return;
    }
    /* Table-scoped (under `<w:tblPr>`). */
    if parent == b"w:tblPr" {
        match name {
            b"w:tblW" => props.width = parse_cell_width(e),
            b"w:tblInd" => {
                if let Some(v) = attr_val(e, b"w:w").and_then(|v| v.parse().ok()) {
                    props.indent_twips = v;
                }
            }
            b"w:tblStyle" => {
                props.table_style_id = attr_val(e, b"w:val");
            }
            /* Issue #84 — `<w:jc>` was write-only (the writer emits it
            from `alignment`, the reader never filled it). Reading it
            closes the asymmetry so it is modeled on both sides rather
            than bagged — a bagged copy plus an engine-set alignment
            would otherwise emit two `<w:jc>` children. */
            b"w:jc" => {
                props.alignment = attr_val(e, b"w:val").and_then(|v| parse_jc(&v));
            }
            /* Issue #79 — `<w:bidiVisual/>` (ST_OnOff toggle; an
            explicit `w:val="false"` reads as off). */
            b"w:bidiVisual" => props.bidi_visual = toggle_on(e),
            /* Audit gap A.M8 — `<w:tblLayout w:type="autofit|fixed"/>`. */
            b"w:tblLayout" => {
                props.layout = match attr_val(e, b"w:type").as_deref().map(str::trim) {
                    Some("fixed") => engine::TableLayout::Fixed,
                    _ => engine::TableLayout::Autofit,
                };
            }
            _ => {}
        }
        return;
    }
    /* Table-level borders. */
    if parent == b"w:tblBorders"
        && let Some(b) = props.borders.as_mut()
    {
        let edge = match name {
            b"w:top" => Some(&mut b.top),
            b"w:left" | b"w:start" => Some(&mut b.left),
            b"w:bottom" => Some(&mut b.bottom),
            b"w:right" | b"w:end" => Some(&mut b.right),
            b"w:insideH" => Some(&mut b.inside_h),
            b"w:insideV" => Some(&mut b.inside_v),
            _ => None,
        };
        if let Some(slot) = edge {
            *slot = Some(parse_border_stroke(e));
        }
    }
}

fn parse_cell_width(e: &BytesStart) -> Option<CellWidth> {
    let typ = attr_val(e, b"w:type").unwrap_or_else(|| "dxa".into());
    let val: Option<i32> = attr_val(e, b"w:w").and_then(|v| v.parse().ok());
    match typ.as_str() {
        "auto" => Some(CellWidth::Auto),
        "nil" => Some(CellWidth::Nil),
        "pct" => val.map(|v| CellWidth::Pct(v as u16)),
        _ => val.map(CellWidth::Dxa),
    }
}

fn parse_border_stroke(e: &BytesStart) -> BorderStroke {
    let style = match attr_val(e, b"w:val").as_deref().unwrap_or("single") {
        "single" => BorderStyle::Single,
        "double" => BorderStyle::Double,
        "dotted" => BorderStyle::Dotted,
        "dashed" => BorderStyle::Dashed,
        "none" | "nil" => BorderStyle::None,
        other => BorderStyle::Other(other.to_owned()),
    };
    let size_eighth_pt = attr_val(e, b"w:sz")
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let color = attr_val(e, b"w:color").and_then(|v| parse_hex_color(&v));
    BorderStroke {
        style,
        size_eighth_pt,
        color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::styles::StyleTable;
    use crate::style_resolver::StyleResolver;

    /// Tests built against a default (empty) style table — no pStyle
    /// chain, doc defaults only. Cell paragraphs still get resolved
    /// through the cascade now (audit A.M18); empty cascade reduces
    /// to the prior `Default::default()` behaviour bit-for-bit.
    fn empty_resolver() -> StyleTable {
        StyleTable::default()
    }

    #[test]
    fn parses_2x2_table_with_grid() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (grid, _props, rows) = parse_table_bytes(
            xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
        )
        .expect("parse");
        assert_eq!(grid, vec![2880, 2880]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells.len(), 2);
        let cell_text = |c: &TableCell| -> String {
            c.blocks
                .iter()
                .filter_map(|b| b.as_paragraph())
                .map(|p| p.text.clone())
                .collect()
        };
        assert_eq!(cell_text(&rows[0].cells[0]), "A1");
        assert_eq!(cell_text(&rows[0].cells[1]), "B1");
        assert_eq!(cell_text(&rows[1].cells[1]), "B2");
    }

    #[test]
    fn parses_grid_span_and_vmerge() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>merged</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>top</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>r1c2</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc><w:tc><w:p><w:r><w:t>r2c2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (_, _, rows) = parse_table_bytes(
            xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
        )
        .expect("parse");
        assert_eq!(rows[0].cells[0].props.grid_span, 2);
        assert_eq!(rows[1].cells[0].props.v_merge, VMergeRole::Restart);
        assert_eq!(rows[2].cells[0].props.v_merge, VMergeRole::Continue);
    }

    #[test]
    fn parses_cell_shading_and_borders() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="FFEB78"/><w:tcBorders><w:top w:val="double" w:sz="12" w:color="FF0000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (_, _, rows) = parse_table_bytes(
            xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
        )
        .expect("parse");
        let cell = &rows[0].cells[0];
        assert_eq!(cell.props.shading, Some([0xFF, 0xEB, 0x78, 0xFF]));
        let top = cell
            .props
            .borders
            .as_ref()
            .and_then(|b| b.top.as_ref())
            .unwrap();
        assert_eq!(top.style, BorderStyle::Double);
        assert_eq!(top.size_eighth_pt, 12);
        assert_eq!(top.color, Some([0xFF, 0x00, 0x00, 0xFF]));
    }

    /// Issue #32 — direct `<w:pPr>` children on cell paragraphs route
    /// through the shared `apply_ppr` surface (plus `pBdr` edges and
    /// `tabs` stops) instead of being dropped on the floor.
    #[test]
    fn cell_paragraph_reads_direct_ppr() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:pPr><w:jc w:val="center"/><w:ind w:start="720"/><w:bidi/><w:shd w:val="clear" w:fill="FFEB78"/><w:pBdr><w:top w:val="single" w:sz="8"/></w:pBdr><w:tabs><w:tab w:val="left" w:pos="1440"/></w:tabs></w:pPr><w:r><w:t>styled</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (_, _, rows) = parse_table_bytes(
            xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
        )
        .expect("parse");
        let para = rows[0].cells[0].blocks[0]
            .as_paragraph()
            .expect("cell paragraph");
        assert_eq!(para.props.alignment, Some(engine::Alignment::Center));
        assert_eq!(para.props.indent.start_twips, 720);
        assert_eq!(para.props.direction, Some(engine::TextDirection::Rtl));
        assert_eq!(para.props.shading, Some([0xFF, 0xEB, 0x78, 0xFF]));
        assert!(
            para.props.borders.as_ref().is_some_and(|b| b.top.is_some()),
            "pBdr top edge must survive"
        );
        assert_eq!(para.props.tab_stops.len(), 1);
        assert_eq!(
            para.direct_overrides.alignment,
            Some(engine::Alignment::Center),
            "direct overrides must be preserved for style re-application"
        );
    }

    /// Issue #84 — unmodeled children of every property container land
    /// in the owning grab bag, document order, containers captured whole;
    /// a cell paragraph's direct `<w:pPr>` (and its whole paragraph-mark
    /// `<w:rPr>`) bag the same way.
    #[test]
    fn captures_unmodeled_property_children_into_grab_bags() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblpPr w:leftFromText="180" w:vertAnchor="text"/><w:tblW w:w="0" w:type="auto"/><w:tblLook w:val="04A0"/><w:tblPrChange w:id="1" w:author="A" w:date="D"><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr></w:tblPrChange></w:tblPr><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:trPr><w:cnfStyle w:val="1"/><w:cantSplit/><w:jc w:val="right"/></w:trPr><w:tc><w:tcPr><w:tcW w:w="2880" w:type="dxa"/><w:textDirection w:val="btLr"/><w:vAlign w:val="bottom"/><w:hideMark/></w:tcPr><w:p><w:pPr><w:widowControl w:val="false"/><w:jc w:val="center"/><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:t>styled</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (_, props, rows) = parse_table_bytes(
            xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
        )
        .expect("parse");
        /* Modeled children still parse; the `tblPrChange` history does
        not override the live width. */
        assert_eq!(props.table_style_id.as_deref(), Some("TableGrid"));
        assert_eq!(props.width, Some(CellWidth::Auto));
        assert_eq!(
            GrabBag::fragments_of(&props.grab_bag),
            &[
                br#"<w:tblpPr w:leftFromText="180" w:vertAnchor="text"/>"#.to_vec(),
                br#"<w:tblLook w:val="04A0"/>"#.to_vec(),
                br#"<w:tblPrChange w:id="1" w:author="A" w:date="D"><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr></w:tblPrChange>"#.to_vec(),
            ]
        );
        let row = &rows[0];
        assert!(row.props.cant_split);
        assert_eq!(
            GrabBag::fragments_of(&row.props.grab_bag),
            &[
                br#"<w:cnfStyle w:val="1"/>"#.to_vec(),
                br#"<w:jc w:val="right"/>"#.to_vec()
            ]
        );
        let cell = &row.cells[0];
        assert_eq!(cell.props.v_align, VerticalAlign::Bottom);
        assert_eq!(
            GrabBag::fragments_of(&cell.props.grab_bag),
            &[
                br#"<w:textDirection w:val="btLr"/>"#.to_vec(),
                b"<w:hideMark/>".to_vec()
            ]
        );
        let para = cell.blocks[0].as_paragraph().expect("cell paragraph");
        assert_eq!(para.props.alignment, Some(engine::Alignment::Center));
        assert_eq!(
            GrabBag::fragments_of(&para.props.grab_bag),
            &[
                br#"<w:widowControl w:val="false"/>"#.to_vec(),
                b"<w:rPr><w:b/></w:rPr>".to_vec()
            ]
        );
        assert_eq!(
            para.direct_overrides.grab_bag, para.props.grab_bag,
            "bag rides direct_overrides for style re-application"
        );
    }

    /// Build a `<w:tbl>` nested `depth` levels deep (one row, one cell,
    /// one paragraph, one nested table per level).
    fn nested_table_xml(depth: usize) -> Vec<u8> {
        let mut out = String::from(
            r#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        );
        for level in 0..depth {
            if level > 0 {
                out.push_str("<w:tbl>");
            }
            out.push_str(r#"<w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc>"#);
            out.push_str(&format!("<w:p><w:r><w:t>level {level}</w:t></w:r></w:p>"));
        }
        for _ in 0..depth {
            out.push_str("</w:tc></w:tr></w:tbl>");
        }
        out.into_bytes()
    }

    /// Walk the single-cell chain: `(typed levels, innermost table)` where
    /// the innermost is the first opaque (row-less) nested table, if any.
    fn typed_chain(rows: &[TableRow]) -> (u32, Option<Table>) {
        let mut levels = 1;
        let mut cur = rows;
        loop {
            let nested = cur
                .first()
                .and_then(|r| r.cells.first())
                .and_then(|c| c.blocks.iter().find_map(Block::as_table));
            match nested {
                Some(t) if t.rows.is_empty() => return (levels, Some(t.clone())),
                Some(t) => {
                    levels += 1;
                    cur = &t.rows;
                }
                None => return (levels, None),
            }
        }
    }

    /// Issue #111 — Apache POI's `deep-table-cell.docx` nests 5000 tables.
    /// Unbounded recursion (one `parse_table_bytes` frame + one copy of the
    /// remaining subtree per level) overflowed the native stack after
    /// O(depth × size) work. The parser must terminate on a bounded stack:
    /// the typed model stops at `MAX_TABLE_NESTING_DEPTH`, the remaining
    /// subtree is preserved opaquely, and exactly one warning reports it.
    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let xml = nested_table_xml(5000);
        let mut warnings = Vec::new();
        let (_, _, rows) = parse_table_bytes_with_warnings(
            &xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
            &mut warnings,
        )
        .expect("parse");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            warnings,
            vec![DocxWarning::TableNestingTooDeep {
                limit: MAX_TABLE_NESTING_DEPTH
            }]
        );
        let (levels, opaque) = typed_chain(&rows);
        assert_eq!(
            levels, MAX_TABLE_NESTING_DEPTH,
            "typed rows stop at the cap"
        );
        let opaque = opaque.expect("the capped subtree is an opaque table block");
        let raw = opaque
            .source_xml
            .as_deref()
            .expect("source bytes preserved");
        assert!(raw.starts_with(b"<w:tbl>"));
        assert!(raw.ends_with(b"</w:tbl>"));
        assert!(
            raw.windows(b"level 4999".len()).any(|w| w == b"level 4999"),
            "the innermost level must survive inside the opaque bytes"
        );
    }

    /// A document that stays under the cap parses every level and emits
    /// no warning — the bound must not change ordinary behaviour.
    #[test]
    fn nesting_under_the_cap_is_fully_typed_without_warnings() {
        let depth = MAX_TABLE_NESTING_DEPTH as usize;
        let xml = nested_table_xml(depth);
        let mut warnings = Vec::new();
        let (_, _, rows) = parse_table_bytes_with_warnings(
            &xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
            &mut warnings,
        )
        .expect("parse");
        assert!(warnings.is_empty(), "{warnings:?}");
        let (levels, opaque) = typed_chain(&rows);
        assert_eq!(levels, MAX_TABLE_NESTING_DEPTH);
        assert!(opaque.is_none());
    }

    /// Issue #101 — a cell `<w:p>` reads through the body run parser: the
    /// cell paragraph is IDENTICAL (text, spans + rPr grab bags, inline
    /// pictures, hyperlinks, fields, props, source bytes) to what the same
    /// `<w:p>` yields as a body paragraph. Before, the cell parser kept
    /// only the concatenated text — one unstyled run, the picture gone.
    #[test]
    fn cell_paragraph_matches_the_body_parse_of_the_same_paragraph() {
        let p = concat!(
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Bold</w:t></w:r>"#,
            r#"<w:r><w:t xml:space="preserve"> plain </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:i/><w:color w:val="FF0000"/><w:sz w:val="28"/><w:lang w:val="en-GB"/><w14:glow w14:rad="1"/></w:rPr><w:t xml:space="preserve">red</w:t></w:r>"#,
            r#"<w:hyperlink r:id="rId9"><w:r><w:t xml:space="preserve">link</w:t></w:r></w:hyperlink>"#,
            r#"<w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="457200"/>"#,
            r#"<a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId5"/></pic:blipFill></pic:pic></a:graphicData></a:graphic>"#,
            r#"</wp:inline></w:drawing></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            "</w:p>",
        );
        let ns_decls = concat!(
            r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
            r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
            r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
            r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
            r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#,
        );
        let table = empty_resolver();
        let resolver = StyleResolver::new(&table);

        let body_xml = format!(
            "<w:document {ns_decls}><w:body>{p}<w:tbl><w:tblGrid><w:gridCol w:w=\"2880\"/></w:tblGrid><w:tr><w:tc>{p}</w:tc></w:tr></w:tbl><w:sectPr/></w:body></w:document>"
        );
        let doc = crate::parts::document::parse_document_xml(body_xml.as_bytes(), &resolver)
            .expect("parse document");
        let body = doc.blocks[0].as_paragraph().expect("body paragraph");
        let t = doc.blocks[1].as_table().expect("table");
        let cell = t.rows[0].cells[0].blocks[0]
            .as_paragraph()
            .expect("cell paragraph");

        assert_eq!(cell.text, body.text);
        assert_eq!(cell.text, "Bold plain redlink\u{FFFC}1");
        assert_eq!(cell.spans, body.spans);
        assert!(
            cell.spans.iter().any(|s| s.style.bold == Some(true)),
            "{:?}",
            cell.spans
        );
        let red = cell
            .spans
            .iter()
            .find(|s| s.style.color == Some([0xFF, 0, 0, 0xFF]))
            .expect("red span");
        assert_eq!(red.style.italic, Some(true));
        assert_eq!(red.style.font_size, Some(14.0));
        assert_eq!(
            GrabBag::fragments_of(&red.style.grab_bag),
            &[
                br#"<w:lang w:val="en-GB"/>"#.to_vec(),
                br#"<w14:glow w14:rad="1"/>"#.to_vec()
            ],
            "root-bound foreign rPr children ride the cell run's grab bag"
        );
        assert_eq!(cell.inline_objects, body.inline_objects);
        assert!(matches!(
            &cell.inline_objects[..],
            [o] if matches!(&o.kind, engine::InlineKind::Image { rel_id, .. } if rel_id == "rId5")
        ));
        assert_eq!(cell.hyperlinks, body.hyperlinks);
        assert_eq!(cell.hyperlinks.len(), 1);
        assert_eq!(cell.fields, body.fields);
        assert_eq!(cell.fields.len(), 1);
        assert_eq!(cell.props, body.props);
        assert_eq!(cell.direct_overrides, body.direct_overrides);
        assert_eq!(cell.source_xml.as_deref(), Some(p.as_bytes()));
        assert!(!cell.dirty);
        assert!(cell.section_end.is_none());
    }

    #[test]
    fn nested_table_recurses() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>outer</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>inner</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:tc></w:tr></w:tbl>"#;
        let (_, _, rows) = parse_table_bytes(
            xml,
            &StyleResolver::new(&empty_resolver()),
            &NamespaceScope::default(),
        )
        .expect("parse");
        let cell = &rows[0].cells[0];
        /* `outer` paragraph + nested table block. */
        assert_eq!(cell.blocks.len(), 2);
        assert!(matches!(cell.blocks[0], Block::Paragraph(_)));
        let inner = cell.blocks[1].as_table().expect("nested table");
        assert_eq!(inner.rows.len(), 1);
        assert_eq!(
            inner.rows[0].cells[0]
                .blocks
                .iter()
                .filter_map(Block::as_paragraph)
                .next()
                .unwrap()
                .text,
            "inner"
        );
    }
}
