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

- [x] **`SelectionKind::TableCells` end-to-end.** Bridge schema
      change to widen `Selection` with a `kind` field +
      `{ table_path, from_row, from_col, to_row, to_col }` payload.
      *Landed in PR 4* — `bridge::SelectionKind` enum + the new
      `Event::SelectionChanged.selection_kind` field;
      `derive_selection_kind` in engine-wasm classifies same-table
      drag selections.
- [x] **`SelectionOverlay` cell-rect highlights.** *Landed in PR 4*
      — `table_cell_rects` in engine-wasm emits one rect per spanned
      cell as the selection's `rects` payload when the selection
      kind is `TableCells`. The DOM overlay paints them with the
      same `selection-rect` CSS as text spans.
- [x] **`Tab` / `Shift+Tab` cell navigation.** Caret in a cell + Tab
      → next cell in row-major order; Shift+Tab → previous;
      Tab on the last cell appends a new row (Word default).
      *Landed in PR 4* — new `MoveDirection::NextCell` /
      `PrevCell`, wired through `HiddenInput`'s keydown handler.
      `Ctrl+Tab` still falls through to the textarea's literal tab.
- [ ] **Drag-out-of-cell selection.** Anchor inside a cell, caret
      drags outside the table → fall back to `Linear` selection that
      covers every cell between, matching Word. *Today:*
      `derive_selection_kind` returns `Linear` for cross-container
      endpoints — selection rects stop at the cell boundary; full
      "everything between" semantics ship with Phase 5c.

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

- [x] **Widen `BridgeLogicalPos` from `{ para, offset }` to
      `{ path: BlockPath, offset }`.** *Landed in PR 4* — both
      bridge and engine `LogicalPos` carry `BlockPath`. Every TS
      site (App seed, dnd, store, Toolbar, harness) builds paths
      via the `topPos(para, offset)` helper.
- [x] **Hit-test recursion into cells.** *Landed in PR 4* —
      `document_geometry` walks `LayoutBlock::Table` boxes,
      descending into every cell and emitting `LineGeom`s stamped
      with `[Block(t), Cell{r,c}, Block(p)]` paths. Clicks inside
      cells now resolve to cell-paragraph positions; clicks on
      borders/gutter snap to the nearest line via the same
      `nearest_line` walk that already handled inter-paragraph
      gaps.
- [x] **Migrate `nth_paragraph` / `paragraph_count` /
      `paragraph_text` callers.** *Landed in PR 4* — every
      interactive engine-wasm path resolves via the new
      `paragraph_at_path` / `path_to_last_top_paragraph` helpers;
      `nth_paragraph` / `paragraph_count` / `paragraph_text` stay
      as compatibility shims for the test corpus.

---

## 3. Phase 5b — PDF table export

- [x] **`format-pdf::emit_table`.** *Landed in Phase 5b.*
      `build_content` is now a three-pass walk — shading (`re`/`f`)
      → text envelope → borders (`m`/`l`/`S`) — that mirrors the
      Canvas2D `paint_table` layering. `emit_paragraph_text`
      extracted from the original glyph loop; `emit_table_text`
      recurses into every cell and re-enters it with the cell's
      absolute origin. Continue-cells skipped. Font collection and
      `/ToUnicode` harvesting both recurse through
      `for_each_paragraph`, so a cell's font is embedded and its
      text remains copyable.
- [x] **veraPDF/A-1b validation on table-bearing fixtures.**
      Structural PDF/A-1b markers pass (8/8) on the exported
      table-bearing seeded document. Native test
      `table_exports_with_shading_and_borders` builds a `TableBox`
      with shading + borders and asserts the PDF embeds the cell
      paragraph's font. veraPDF binary remains a host-tool
      dependency — `tools/pdf-validate` reports "skipped" when not
      on PATH, same as the Phase 5 release pipeline.
- [ ] **Multi-page PDF pagination + "rows whole-only" page-break.**
      *Deferred — blocked on Phase 6 (Sections & Paginator).* The
      engine builds a single `PageBox` today, so the
      "rows-whole-only" overflow rule has nothing to trigger
      against. `format-pdf::export_pdf` already accepts a
      `&[PageBox]` slice and emits one PDF page per entry (Phase 5b
      groundwork), so the moment Phase 6 lands a real paginator
      this becomes a one-line wiring change: feed the paginated
      page sequence in. Until then, single-page tables export
      cleanly under PR 5b and tall tables clip at the page bottom.
      *Unblocked by:* Phase 6 — Sections & Paginator.

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

## 6. Phase 6 — Sections, headers, footers, pagination

Items deferred during the Phase 6 cut (`feat(engine): implement Phase 6
sections, pagination, and multi-page layout`).

- [ ] **Header/footer text content wiring to the engine.** The
      `parts::header::HeaderPart` / `parts::footer::FooterPart` parsers
      land, and `Section.header_ref` / `footer_ref` carry the OOXML
      `r:id`. The engine still needs to resolve those refs against the
      archive's `word/_rels/document.xml.rels`, find the
      `word/header*.xml` / `word/footer*.xml` part, and route the
      parsed paragraphs into `PageBox.header` / `PageBox.footer` so
      the renderer paints them. Phase 6b fixes this partially — the
      first laid-out paragraph from each part renders as plain text;
      multi-paragraph headers + rich formatting + tables inside
      header/footer parts are not modelled. *Depends on:* the engine
      needs the archive's `other_entries` plumbed through `OpenDocx`,
      currently only the parsed `DocumentTree` reaches the engine.
- [ ] **`<w:sectPr w:type>` controls.** Every section break currently
      becomes a fresh page (`nextPage` semantics). `continuous`,
      `evenPage`, `oddPage`, `nextColumn` are parsed-but-ignored by
      the paginator. *Depends on:* `Section` carrying the type;
      paginator hook to skip the page break for `continuous`.
- [ ] **Programmatic Section mutations.** `Section` is read-only on
      the engine API surface: there is no `Command::SplitSection` /
      `Command::SetPageGeometry`. The `.docx` writer still emits the
      original `<w:sectPr/>` from the passthrough — engine-side
      edits to the section table never round-trip. *Depends on:*
      writer integration for `<w:sectPr>`; new command surface.
- [x] **TS canvas auto-resize to total document height.** Phase 6b —
      `EditorCanvas` consumes the new `Painted.document_height` /
      `page_count` fields and resizes its backing store + CSS height
      so the browser scrollbar exposes every page.
- [ ] **Mid-cell row splitting in tables.** A table cell taller than
      a full page currently overflows past the page footer. The
      paginator's `push_table_split` only splits at row boundaries;
      row-internal cell flowing (the rare "single-row table with
      hundreds of paragraphs in one cell" case) is deferred.
      *Depends on:* a cell-flow path through the paginator + per-cell
      breakable / unbreakable hint.
- [ ] **Header/footer rich formatting + per-page chrome.** Phase 6b
      paints header/footer plain text in the margin band. Multi-line
      headers, tab-aligned page numbers, `<w:fldSimple>` page-number
      fields, and the `first` / `even` / `default` header variants
      from `<w:headerReference w:type=...>` are not modelled.
      *Depends on:* the field calculator (deferred Phase 7+) and
      header-variant selection from page parity.

---

## 7. Phase 7 — Media, hyperlinks, DrawingML

Items deferred during the Phase 7 cut (`feat(engine): implement Phase 7
inline images and hyperlinks`).

- [ ] **Floating images (`<wp:anchor>`).** Phase 7 parses only inline
      images (`<wp:inline>`); the `<wp:anchor>` variants — text-wrap,
      `relativeFrom` anchoring, `behindDoc`, the
      `<wp:wrapSquare>` / `<wp:wrapTight>` polygons — are read and
      dropped. *Depends on:* a positioned-block layout layer (the
      paginator currently flows only inline content).
- [ ] **Image-bytes writer round-trip.** The reader stashes `word/media/*`
      via `other_entries`, so the writer emits the bytes verbatim — but
      the engine has no `InsertImage` / `ReplaceImage` command path that
      would re-emit edited media. *Depends on:* a new bridge command
      surface + writer-side `<w:drawing>` emission.
- [ ] **Vello image decoding.** The Vello backend paints inline images
      as gray placeholder rectangles. The Canvas2D path decodes via
      `createImageBitmap` and routes through `drawImage`; Vello needs
      its own `peniko::Image` resource pipeline (Backlog #4 — Vello
      activation). *Depends on:* the Vello activation sprint.
- [ ] **Internal hyperlink anchors (`<w:hyperlink w:anchor>`).** The
      parser captures only the external URL (`r:id` → rels target).
      Bookmark-style anchors (jumping to a paragraph inside the same
      document) drop on parse. *Depends on:* `<w:bookmarkStart>` /
      `<w:bookmarkEnd>` parsing + a logical-position resolver.
- [ ] **Inline-image edit operations.** `Command::InsertImage`,
      `DeleteImage`, and resize-by-handle are not wired. The Phase 7
      model is read-only on the engine API surface; image bytes ride
      the passthrough writer unchanged. *Depends on:* engine commands +
      writer media emission.
- [ ] **Click-to-open hyperlinks.** Hyperlinks render with style only;
      the engine does not surface their target through hit-testing, and
      the TS shell has no pointer handler that opens the URL on click
      (or Ctrl-click). *Depends on:* a new bridge event surface
      (e.g. `HyperlinkHit`) emitted from `Command::HitTest`.
- [ ] **EMU-precision image scaling.** Phase 7 takes `<wp:extent
      cx="..." cy="..."/>` at face value (no aspect-ratio enforcement,
      no clamping to page width, no `<a:srcRect>` cropping). Word lets
      an image exceed the content rect; we currently let it overflow.
      *Depends on:* a clamp + crop pass in `build_inline_object_infos`.

---

## 8. Phase 8a — Footnotes & comments

Items deferred during the Phase 8a cut (`feat(engine): implement
Phase 8a footnotes, paginator integration, and comments parser`).

- [ ] **True superscript footnote markers.** Phase 8a paints each inline
      footnote ref as a small colored rectangle (placeholder) at the
      reserved width. The footnote body in the bottom band carries the
      number prefix so readers still see the number. Real shaped
      superscript digits ship with Phase 8b. *Depends on:* a
      `DisplayCmd::DrawText` primitive that shapes a short string at a
      smaller font size on demand.
- [ ] **Footnote-body splitting across pages.** A footnote whose body
      is taller than the remaining content budget currently forces the
      referencing line onto the next page (the simple "defer" path).
      Word splits the body itself, continuing on the next page with the
      `id=-1` continuation separator. *Depends on:* a body-paragraph
      splitter inside the paginator's footnote band.
- [ ] **Rich body formatting + multi-paragraph footnotes.** Footnote
      bodies render as a single concatenated paragraph in plain text;
      `<w:rPr>` formatting + multi-paragraph layout are flattened.
      Likewise for comment bodies — the sidebar surfaces joined plain
      text only. *Depends on:* threading the cascade resolver through
      `parts::footnotes` / `parts::comments`.
- [ ] **Click-to-locate comment ranges in the canvas.** The sidebar
      lists comments + their `(block, offset)` spans, but the canvas
      does not highlight the range on hover or scroll the body into
      view on click. *Depends on:* a TS overlay layer keyed off
      `comments_snapshot` + `document_geometry`.
- [ ] **Comment threading (`<w:commentEx>`).** OOXML carries `done`
      state and reply parent ids in `commentsExtended.xml` — not yet
      parsed. *Depends on:* a `parts::comments_extended` reader +
      `CommentDef.parent_id` / `done` fields.
- [ ] **Engine-side footnote / comment mutations.** No
      `Command::InsertFootnote`, `DeleteFootnote`, `AddComment`,
      `ResolveComment`. The writer rides the passthrough for
      `footnotes.xml` and `comments.xml` so unedited entries round-
      trip byte-identical; engine-edited ones cannot round-trip yet.
      *Depends on:* writer integration for the two XML parts + new
      bridge commands.

---

## 9. Phase 8b — Tracked changes (revisions)

Items deferred during the Phase 8b cut (`feat(engine): implement Phase
8b tracked changes and revision rendering`).

- [ ] **Per-author multi-colour palette.** Phase 8b paints every
      insertion green and every deletion red, regardless of author.
      Word rotates a palette per reviewer (`<w:rsid>` linkage). The
      `Revision::author` string is already carried; a colour resolver
      keyed on it ships with 8c.
- [ ] **Engine-side accept / reject.** `Command::AcceptRevision` and
      `Command::RejectRevision` are not wired. The model retains
      deleted text via `Paragraph::revisions`; accept = strip the
      revision range + delete the bytes for `Delete`, drop the
      overlay for `Insert`. Reject = drop the bytes for `Insert`,
      drop the overlay for `Delete`. *Depends on:* new bridge
      commands + writer-side `<w:ins>` / `<w:del>` emission.
- [ ] **`Show Markup` toggle.** Markup is always on in this cut; the
      "Final" / "Original" viewing modes that hide either the inserts
      or the deletes are not exposed. *Depends on:* a render-time
      config bit threaded through `RenderConfig` + the overlay
      function gating on it.
- [ ] **Moved-text revisions (`<w:moveFrom>` / `<w:moveTo>`).** The
      parser handles `<w:ins>` / `<w:del>` only; tracked moves fall
      through and surface as a delete+insert pair. *Depends on:*
      additional `RevisionKind` variants + writer support.
- [ ] **Run-property revisions (`<w:rPrChange>`).** Inline formatting
      changes (bold added, color changed) are not modelled. The base
      run's current style wins; the historical "before" style is
      dropped on parse. *Depends on:* a `before_style: SpanStyle`
      field on `Revision` for `RprChange` variants.
- [ ] **Paragraph-property revisions (`<w:pPrChange>`).** Tracked
      changes to paragraph alignment / indent / spacing are dropped
      on parse. *Depends on:* paragraph-level revision overlays.
- [ ] **Canvas hover tooltips.** Engine exposes
      `revisions_snapshot()` and the TS shell can fetch it via
      `EngineClient.revisionsSnapshot()`, but no DOM overlay yet
      ties tooltip popups to revision ranges in canvas geometry.
      *Depends on:* a TS overlay component + `document_geometry`
      hover binding.
- [ ] **Writer-side `<w:ins>` / `<w:del>` emission.** Unedited
      revision-bearing paragraphs round-trip via the source-XML
      passthrough (`Paragraph.source_xml` carries the wrappers). An
      engine edit on a revision-bearing paragraph (currently any
      `InsertText` etc.) drops `source_xml` and the writer emits a
      passthrough-less serialization that strips the revision
      wrappers. *Depends on:* a `<w:ins>` / `<w:del>` re-emission
      pass in `writer.rs`.

---

## 10. Cross-phase reminders

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
