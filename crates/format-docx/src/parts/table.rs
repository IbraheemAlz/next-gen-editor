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

use crate::error::DocxError;
use crate::schema::ct_rpr::{attr_val, parse_hex_color};
use engine::{
    Block, BorderStroke, BorderStyle, CellBorders, CellWidth, RowHeight, Table, TableCell,
    TableProperties, TableRow, VMergeRole, VerticalAlign,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Parse a `<w:tbl>...</w:tbl>` byte slice. Always starts with a
/// `<w:tbl>` opening tag; depth bookkeeping handles nested tables
/// inside cell content (each nested `<w:tbl>` produces a recursive
/// `parse_table_bytes` call when we hit the `<w:tc>` close).
pub fn parse_table_bytes(
    xml: &[u8],
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

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:tbl" if !in_table => {
                        in_table = true;
                    }
                    b"w:tbl" if cur_cell.is_some() => {
                        /* Nested table inside a cell. */
                        if nested_tbl_depth == 0 {
                            nested_tbl_start = Some(prev_pos);
                        }
                        nested_tbl_depth += 1;
                    }
                    b"w:tr" if nested_tbl_depth == 0 => {
                        cur_row = Some(TableRow::default());
                    }
                    b"w:tc" if nested_tbl_depth == 0 => {
                        cur_cell = Some(TableCell::default());
                    }
                    b"w:p" if nested_tbl_depth == 0 && cur_cell.is_some() => {
                        p_start_byte = Some(prev_pos);
                    }
                    _ => {}
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
                if nested_tbl_depth == 0 {
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
                            if start < end && end <= xml.len() {
                                let raw = xml[start..end].to_vec();
                                if let Ok((g, p, r)) = parse_table_bytes(&raw) {
                                    cell.blocks.push(Block::Table(Table {
                                        grid: g,
                                        props: p,
                                        rows: r,
                                        dirty: false,
                                        source_xml: Some(raw),
                                    }));
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
                    b"w:tc" if nested_tbl_depth == 0 => {
                        if let Some(cell) = cur_cell.take()
                            && let Some(row) = cur_row.as_mut()
                        {
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
                        if start < p_end && p_end <= xml.len() {
                            let raw = xml[start..p_end].to_vec();
                            if let Some(cell) = cur_cell.as_mut() {
                                /* Phase 5 PR 2 — cell paragraphs are
                                parsed via the existing single-paragraph
                                helper, but for simplicity we extract
                                the visible text and stash source bytes
                                for round-trip. Full cascade + numbering
                                inside cells lands in Phase 5 PR 3. */
                                cell.blocks
                                    .push(Block::Paragraph(parse_cell_paragraph(&raw)));
                            }
                        }
                    }
                    _ => {}
                }
                if stack.last().map(|n| n.as_slice()) == Some(name.as_slice()) {
                    stack.pop();
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

/// Cell paragraph parser (Phase 5 PR 2 minimal). Extracts the
/// concatenated visible text + captures the raw `<w:p>` bytes for
/// round-trip. Full pPr / rPr / numPr cascade inside cells deferred
/// to Phase 5 PR 3 (a refactor of `parts::document::parse_document_xml`
/// to share its run-aware loop with cell content).
fn parse_cell_paragraph(xml: &[u8]) -> engine::Paragraph {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut text = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == b"w:t" => in_text = true,
            Ok(Event::End(e)) if e.name().as_ref() == b"w:t" => in_text = false,
            Ok(Event::Text(t)) if in_text => {
                if let Ok(s) = t.unescape() {
                    text.push_str(&s);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    engine::Paragraph {
        text,
        spans: Vec::new(),
        props: Default::default(),
        list_item: None,
        resolved_marker: None,
        /* Cell paragraphs ride their parent table's passthrough — the
        cell's raw `<w:p>` bytes are inside `Table.source_xml`. We don't
        store a per-paragraph `source_xml` here because PR 2 always
        emits the whole table verbatim on save. */
        dirty: false,
        source_xml: Some(xml.to_vec()),
        inline_objects: Vec::new(),
        hyperlinks: Vec::new(),
        revisions: Vec::new(),
        fields: Vec::new(),
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

    #[test]
    fn parses_2x2_table_with_grid() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (grid, _props, rows) = parse_table_bytes(xml).expect("parse");
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
        let (_, _, rows) = parse_table_bytes(xml).expect("parse");
        assert_eq!(rows[0].cells[0].props.grid_span, 2);
        assert_eq!(rows[1].cells[0].props.v_merge, VMergeRole::Restart);
        assert_eq!(rows[2].cells[0].props.v_merge, VMergeRole::Continue);
    }

    #[test]
    fn parses_cell_shading_and_borders() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="FFEB78"/><w:tcBorders><w:top w:val="double" w:sz="12" w:color="FF0000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (_, _, rows) = parse_table_bytes(xml).expect("parse");
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

    #[test]
    fn nested_table_recurses() {
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>outer</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>inner</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:tc></w:tr></w:tbl>"#;
        let (_, _, rows) = parse_table_bytes(xml).expect("parse");
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
