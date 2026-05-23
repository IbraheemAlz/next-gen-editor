# Deferred Features Tracker

**Status:** Live. Maintained alongside the codebase.
**Scope:** Phase 5 (block enum + tables) and earlier roadmap deferrals.
**Created:** 2026-05-23 (end of Phase 5 PR 3).
**Convention:** check items off (`- [x]`) as their PR lands. New
deferrals append to the relevant section; obsolete items move to the
"Resolved / withdrawn" section at the bottom with a one-line
rationale.

Per-section header tells you the **phase that owns the item** and the
**target PR**. "PR 3b" = polish PR after Phase 5a's three-PR cut.

---

## 1. Phase 5 PR 3b — TS shell UI + a11y

Engine + bridge are wire-ready. The remaining items are TypeScript /
DOM components plus the bridge wire shapes that the UI needs.

### TS shell — table UI

- [x] **"Insert Table" toolbar button.** Spawns a rows×cols picker
      grid, fires `Command::InsertTable { at, rows, cols }` at the
      current caret block. *Depends on:* nothing — the bridge command
      ships in PR 3. *Landed:* PR 3b — 8×8 hover picker; `at` is
      `BlockPath::top(caret.para + 1)` so the table lands right
      after the caret's paragraph (paragraph-flat addressing — the
      common case until the `BlockPath` migration).
- [ ] **Cell context menu.** Right-click inside a cell exposes Insert
      Row Above / Below, Insert Column Left / Right, Delete Row,
      Delete Column, Merge Cells, Split Cell, Cell Shading, Cell
      Borders. *Depends on:* hit-test recognising "click landed in
      cell `(r, c)` of table at `BlockPath::top(N)`" — see §4.
      *PR 3b lands the side-panel surface (next bullet) — the
      right-click context menu still needs the BlockPath caret.*
- [x] **Table-properties side panel.** Width, alignment, indent,
      table-wide borders. *Landed in PR 3b* as `TablePanel.tsx`:
      lists every table from the a11y stream, exposes insert/delete
      row & column, merge / split, cell shading and per-edge
      borders. `Command::SetTableProperties` (table-wide width /
      alignment / indent) is still pending — additive when needed.

### Cell-rectangular selection + cell navigation

> *PR 3b dependency check (2026-05-23):* every bullet in this
> subsection requires `LogicalPos` to address a cell — currently it
> is paragraph-flat and skips tables, so a caret physically cannot
> sit inside a cell. Pulled forward to **§2 (Phase 5 PR 4 —
> `BlockPath` migration)**; cell selection + Tab navigation land
> after PR 4.

- [ ] **`SelectionKind::TableCells` end-to-end.** Bridge schema
      change to widen `Selection` with a `kind` field +
      `{ table_path, from: (row, col), to: (row, col) }` payload.
      Engine state already has `Selection.kind` headroom. *Depends on:*
      §4 nested-`BridgeLogicalPos` migration so the anchor / caret
      positions can sit inside a cell.
- [ ] **`SelectionOverlay` cell-rect highlights.** DOM overlay paints
      a solid-fill rect per selected cell instead of per-line text
      spans. *Depends on:* the bullet above.
- [ ] **`Tab` / `Shift+Tab` cell navigation.** Caret in a cell + Tab
      → next cell in row-major order; Shift+Tab → previous;
      `Ctrl+Tab` inserts a literal `\t`. *Depends on:* the bullet
      above (caret must address a specific cell).
- [ ] **Drag-out-of-cell selection.** Anchor inside a cell, caret
      drags outside the table → fall back to `Linear` selection that
      covers every cell between, matching Word.

### A11y — fine-grained patches

- [x] **Full nested `<table><tr><td>` tree in `A11yTree`.**
      `A11yNode { Paragraph, Table }` enum widening per RFC §4.3.
      Tables emit `role="table"` with `<tr role="row">` and
      `<td role="gridcell">`; `aria-rowspan` / `aria-colspan` from
      `vMerge` / `gridSpan`. *Landed in PR 3b* — `A11yTree.nodes`
      replaces the flat `paragraphs` list, the reconciler in
      `ts/src/a11y/tree.ts` builds `<table>` / `<tr>` / `<td>` from
      `A11yTable` / `A11yRow` / `A11yCell`; Continue cells are
      suppressed and the Restart cell carries the resolved
      `aria-rowspan`.
- [ ] **`AccessibilityTreeDelta` fine-grained patches.** New variants
      `UpdateCell`, `InsertNode`, `DeleteNode` per RFC §4.3. PR 3
      keeps whole-tree replacement. *Depends on:* the bullet above.
- [ ] **Stable per-paragraph + per-cell IDs.** Currently each delta
      walks paragraphs by index; cells need stable IDs so the
      mirror DOM can patch in place without re-rendering siblings.
      Tracked separately in `BACKLOG.md` #10.

### Bridge wire shapes blocking commands

- [x] **`BridgeCellBorders` + `BridgeBorderStroke` mirror enums.**
      Activates the `Command::SetCellBorders` wire path. Engine
      method `set_cell_borders` already ships in PR 3.
      *Landed in PR 3b* — bridge ships `BridgeCellBorders`,
      `BridgeBorderStroke`, `BridgeBorderStyle`; engine-wasm wires
      `Command::SetCellBorders { table_path, row, col, borders }`
      onto `set_cell_borders`. `inside_h` / `inside_v` are not on
      the wire (table-level only — table-wide borders land with
      the `SetTableProperties` wrapper above).
- [ ] **`Command::SetTableProperties` wrapper.** Wire-shape for
      table-wide width / alignment / indent / borders (today only
      cell-level shading + borders are exposed). *Depends on:*
      previous bullet.

---

## 2. Phase 5 PR 4 — `BlockPath` migration of `BridgeLogicalPos`

PR 3 left `BridgeLogicalPos.para: u32` paragraph-flat as a
transitional shape. PR 4 widens it so the caret can sit inside a
cell.

- [ ] **Widen `BridgeLogicalPos` from `{ para, offset }` to
      `{ path: BlockPath, offset }`.** Engine-side `LogicalPos`
      mirrors. Every TS position consumer (caret, hit-test,
      selection, keyboard motion) updates in lock-step.
      *Depends on:* nothing engine-side; touches every TS shell
      component that constructs a position.
- [ ] **Hit-test recursion into cells.** `document_geometry`
      flattens cell paragraphs into `CaretSlot`s stamped with the
      full `BlockPath`. Today hit-tests treat tables as
      non-interactive regions.
- [ ] **Migrate `nth_paragraph` / `paragraph_count` /
      `paragraph_text` callers.** These are the last remnants of
      paragraph-flat addressing on the engine side; once
      `BlockPath` is the canonical position type they collapse to
      `BlockPath::root_paragraph(n)` adapters.

---

## 3. Phase 5b — PDF table export

- [ ] **`format-pdf::emit_table`.** Per RFC §3.2. Borders via
      `m` / `l` / `S` path ops; shading via `re` / `f`; cell content
      via `Tj` text-show with offset matrices. *Currently:* the PR 2
      stub silently skips tables and surfaces a warning in
      `PdfExportReport.skipped_blocks`. *Depends on:* nothing —
      pure additive within `format-pdf`.
- [ ] **veraPDF/A-1b validation on table-bearing fixtures.**
      `tools/pdf-validate/tables/` harness; assert tables in
      `table_2x2.docx`, `table_borders_double.docx`,
      `table_in_rtl_doc.docx` export cleanly. *Depends on:*
      previous bullet.
- [ ] **Page-break handling on PDF.** Inherit the Phase 5a "rows
      whole-only" rule on PDF too. *Depends on:* `emit_table`.

---

## 4. Phase 5c — Layout polish

The Phase 5a layout pass ships a deliberately incomplete vMerge model
and zero mid-row pagination support. Phase 5c closes the gap.

- [ ] **Two-pass vMerge height accumulation.** Phase 5a:
      `VMergeRole::Continue` cells contribute 0 to row height; the
      `Restart` cell paints at its own row's natural height. Phase
      5c: a second pass walks down the column from each Restart
      cell, sums every Continue row's height, stamps the total
      onto the Restart cell. Renderer's "Restart owns the merged
      region" is already correct — only the height math is missing.
      ~30 lines in `layout_table_box`. *Depends on:* nothing.
- [ ] **Mid-row pagination.** Split a tall cell across two pages,
      carry the residual into a continuation row on the next page.
      Requires a re-entrant layout pass + `<w:cantSplit/>`
      honouring. *Depends on:* multi-page page-build infra (the
      engine currently builds one page; multi-page is a parallel
      backlog item).
- [ ] **`<w:tblHeader/>` repeating header rows.** Header rows
      repeat at the top of every page after a break. *Depends on:*
      previous bullet.
- [ ] **`<w:cantSplit/>` per-row honouring.** Phase 5a treats this
      as implicit-on for every row. Phase 5c respects the flag
      explicitly. *Depends on:* mid-row pagination.
- [ ] **`CellWidth::Auto` / `Pct` auto-fit (`<w:tblLayout
      w:type="autofit"/>`).** Two-pass measure + balance + re-layout.
      Phase 5a ships literal `<w:tblGrid>` widths only. *Depends on:*
      nothing.
- [ ] **Per-row passthrough.** Today a single-cell edit dirties the
      whole containing table (writer regenerates ~200 KB of XML for
      a 200-row table edit). Phase 5c: per-row source-byte capture
      + per-row dirty tracking so the writer regenerates only the
      mutated rows. *Depends on:* nothing engine-side; reader needs
      to capture `<w:tr>` byte ranges in addition to `<w:tbl>`.
- [ ] **Whole-row + whole-column triple-click selection.** RFC §4.4
      bullet 3.
- [ ] **Vello backend mirror of `paint_table`.** Phase 5 PR 2 only
      touched Canvas2D `scene.rs`. The same `paint_block` API
      drops into `vello_scene` because both emit `DisplayCmd`.
      *Depends on:* nothing — straight port.

---

## 5. PR 1-2 nested-table follow-ups

- [ ] **Nested-cell mutation through deeper `BlockPath`s.**
      `top_level_block_index` resolver currently only handles
      `PathStep::Block(N)` as the first step. Recursive path
      walking (`PathStep::Cell { row, col }` + further
      `PathStep::Block(N)` descent) is needed for "edit a paragraph
      inside a cell of a nested table." *Depends on:* nothing
      engine-side; new tests required.
- [ ] **Full pPr / rPr / numPr cascade inside cell paragraphs.**
      Phase 5 PR 2's `parts::table::parse_cell_paragraph` extracts
      raw text + source bytes only. Full cascade requires refactoring
      `parts::document::parse_document_xml` to share its run-aware
      loop with cell content. *Depends on:* nothing — pure refactor.
- [ ] **Visual-diff goldens for tables.** RFC §3.3's 6 cases
      (`table_2x2`, `table_grid_span`, `table_vmerge`,
      `table_borders_double`, `table_shaded_header`,
      `table_in_rtl_doc`). PR 2 fixtures parse + lay out + paint
      correctly; only the browser-farm screenshots are missing.
      *Depends on:* `tools/visual-diff/golden/` capture run with
      the table fixtures activated.

---

## 6. Cross-phase reminders

Items deferred from earlier phases that interact with Phase 5+
follow-ups.

- [ ] **Phase 3 `styles.xml` cascade inside table cells.** The Phase
      3 cascade resolver runs over top-level paragraphs only; cell
      paragraphs currently see no cascade (PR 2's
      `parse_cell_paragraph` is a stub). *Owner:* same as "full
      pPr/rPr cascade inside cells" above.
- [ ] **Phase 4 numbering resolver inside cells.** `resolve_markers`
      iterates top-level paragraphs. Cell list items get no markers
      until the resolver descends into cell content. *Owner:* same.

---

## Resolved / withdrawn

(empty — additions move here as their owning PR lands)
