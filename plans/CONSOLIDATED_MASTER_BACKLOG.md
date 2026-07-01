# Consolidated Master Backlog

> **DEPRECATED (2026-07-02).** This snapshot is historical context only.
> The authoritative deferred-scope list is [`BACKLOG.md`](BACKLOG.md);
> actionable gaps are tracked as GitHub Issues (labels `core-engine` /
> `ui` / `enhancement` / `tech-debt`). Do not add new items here.

**Generated:** 2026-05-25 (post Phase 5.5).
**Superseded at generation time (as the then-active source of truth):**
`BACKLOG.md`, `UX_BEHAVIOR_SPEC.md`, `ECMA_376_COMPLIANCE_AUDIT.md`,
`DEFERRED_FEATURES_TRACKER.md`, `MASTER_PLAN.md` §10 Open decisions —
an authority arrangement reverted by the 2026-07 audit (see banner).

**Convention.** Severity follows the audit's production-impact rubric:
`High` = breaks rendering or destroys user data, `Medium` = visible drift
or wrong-but-readable output, `Low` = niche / metadata-only. Within each
category, items list `High` → `Medium` → `Low`.

---

## 0. What recent commits resolved (status updates)

The five legacy trackers predate the Phase-1–5.5 remediation sprints. The
items below were still marked `[TODO]` / `[PARTIAL]` / `[HIDDEN GAP]` /
`[MODEL-ONLY]` in those files but are now `[DONE]`:

| Source file | Item | Resolution commit |
|---|---|---|
| `ECMA_376_COMPLIANCE_AUDIT.md` A.1 | `<w:rPr><w:sz>` / `<w:szCs>` parse + emit | `1b77b7a` |
| `ECMA_376_COMPLIANCE_AUDIT.md` A.2 | `<w:u>` underline variants (None / Single / Double / Dotted / Dashed / Wavy) end-to-end | `a9f7041` |
| `ECMA_376_COMPLIANCE_AUDIT.md` A.12 | `<w:br/>` (line) + `<w:br w:type="page"/>` (page break) parser + writer + layout + paginator-flush | `f021d3f`, `ac63db0` |
| `ECMA_376_COMPLIANCE_AUDIT.md` B.1 | `<w:tcPr><w:tcMar>` per-cell margins | `f021d3f` |
| `ECMA_376_COMPLIANCE_AUDIT.md` B.2 | `<w:tblPr><w:tblCellMar>` table-default cell margins (parse + layout + render offsets) | `f021d3f` |
| `ECMA_376_COMPLIANCE_AUDIT.md` C.1 | `<w:titlePg>` first-page header | `a9f7041` |
| `ECMA_376_COMPLIANCE_AUDIT.md` C.2 | `<w:evenAndOddHeaders>` setting | `a9f7041` |
| `ECMA_376_COMPLIANCE_AUDIT.md` C.3 | `<w:headerReference w:type>` discriminator (default / first / even, fallback per OOXML §17.10.3) | `a9f7041` |
| `ECMA_376_COMPLIANCE_AUDIT.md` D.1 | Complex-field state machine (`<w:fldChar>` + `<w:instrText>` + cached runs), paginator PAGE / NUMPAGES evaluation | `3c82068` |
| `ECMA_376_COMPLIANCE_AUDIT.md` E.1 | `<w:ins>` / `<w:del>` writer wrappers (revision round-trip on dirty save) | `1b77b7a` |
| `ECMA_376_COMPLIANCE_AUDIT.md` E.2 | `<w:ins w:id>` reader + fallback id on writer | `1b77b7a` |
| `BACKLOG.md` §10 | Fine-grained `AccessibilityTreeDelta` patches | Sprint 9 (pre-audit) |
| `DEFERRED_FEATURES_TRACKER.md` §6 (Phase 6) | Header/footer model widened to full `Vec<Paragraph>` (overlay, field-aware) | `0110a27` |
| `DEFERRED_FEATURES_TRACKER.md` §6 | Header/footer rich-formatting + `<w:fldSimple>` PAGE in headers | `0110a27` (PAGE works in body; header field eval reaches `LayoutField` via the upgrade) |
| `DEFERRED_FEATURES_TRACKER.md` §8b | Writer-side `<w:ins>` / `<w:del>` emission | `1b77b7a` |

These trackers should be considered stale until manually re-marked, OR
replaced by this document.

---

## 1. Cross-file contradictions found

Resolved here to avoid acting on stale signals:

1. **`DEFERRED_FEATURES_TRACKER.md` §6** flagged header/footer text content
   wiring as deferred. `MASTER_PLAN.md` §3 / §4 implicitly assumed it as
   part of multi-page layout. **Resolution:** shipped (commit `0110a27`).
2. **`UX_BEHAVIOR_SPEC.md` III** marks BiDi-aware arrow mapping as `[TODO]`
   and III.6 selection-across-seams as `[DONE]`. `BACKLOG.md` §7 marks
   discontinuous BiDi rects as shipped. **Resolution:** consistent — III.6
   is render-side; III.1–III.3 (visual-arrow remap) is still real `[TODO]`.
3. **`DEFERRED_FEATURES_TRACKER.md` §8b** says `<w:ins>`/`<w:del>` emission
   is deferred. The audit `E.1` agreed at the time but the gap is **now
   closed** (`1b77b7a`). Both files should be treated as historical.
4. **`MASTER_PLAN.md` §9 MVP gate** demands "100% of P0 OOXML features
   supported". The audit's `[HIDDEN GAP]` count was 18 of 41 at start;
   post-remediation, **8 of 41 remain visible defects** (see §G of the
   audit for the original count; the remaining items below for the
   current set).
5. **`UX_BEHAVIOR_SPEC.md` VIII Out-of-scope** lists "Track-changes
   redline view (data model exists; rendering / accept-reject UI does
   not)". The render side **is** now wired (insert = underline + green,
   delete = strike + red); the accept/reject UI is genuinely still
   deferred (DEFERRED §8b).
6. **`MASTER_PLAN.md` Open decision #2** (default Arabic font: Noto vs
   Amiri) is unresolved on paper but the engine now loads three families
   (Amiri, Liberation Sans, Noto Naskh Arabic) and the UI picker exposes
   all three — **decision is "ship all three"** by implementation,
   pending an explicit doc update.

---

## Category A: Core Standard Gaps (ECMA-376 Part 1)

Remaining unhandled tags / attributes from the WML compliance audit.
Source of truth: `ECMA_376_COMPLIANCE_AUDIT.md` minus §0 resolutions.

### A-High (production-blocking)

- **A.H1 — `<w:tcPr><w:vAlign>` rendering** (audit B.3). Parsed + writer
  round-trips; layout completely ignores. Every cell renders
  top-aligned. Fix: layout pass computes `(cell_inner_height -
  content_height) * factor` per `Top|Center|Bottom`, offsets content
  origin Y.
- **A.H2 — `<w:sectPr><w:cols>` multi-column layout** (audit C.4). No
  parser, no model, no layout. Two-column newsletters / résumés render
  as a single full-width column. Fix scope: column descriptor on
  `Section`, multi-column flow path in `Paginator` (split content area
  into N strips, snake-flow within each page).
- **A.H3 — `<w:rPr><w:caps>` / `<w:smallCaps>`** (audit A.8 subset).
  Visual mismatch: text the author wrote lowercase renders lowercase
  instead of uppercase. Spec-driven uppercase transform at shape time
  (smallCaps additionally needs a reduced cap-height for non-leading
  letters — best-effort with current font metrics).

### A-Medium (cosmetic drift)

- **A.M1 — `<w:rPr><w:vertAlign>` superscript / subscript reader**
  (audit A.8). Writer already emits it for footnote refs; reader never
  parses it. Footnotes auto-style; user-set super/sub on body text is
  dropped. Add to `apply_rpr` + thread through `SpanStyle` as
  `Option<VerticalScript>` (bridge enum exists).
- **A.M2 — `<w:rPr><w:rFonts w:asciiTheme>` theme bindings + non-
  whitelist font families** (audit A.9). Currently three families
  (Amiri, Liberation Sans, Noto Naskh Arabic) parse; everything else
  drops. `*Theme` attributes never read at all. Fix: open `font_family`
  to a free-form string; load on demand from a font cache; surface theme
  resolution against `word/theme/theme1.xml`.
- **A.M3 — `<w:pPr><w:tabs>` custom tab stops** (audit A.11). Layout
  engine has no tab-stop table. `<w:tab/>` runs (A.12 sibling, see A.M5)
  cannot position. Fix: tab-stop list on `ParaProperties`; line builder
  honours stops when shaping the tab cluster's advance.
- **A.M4 — `<w:pPr><w:pBdr>` paragraph borders** (audit A.11).
  Dirty paragraphs silently lose borders. Parse to a `CellBorders`-
  shaped sidecar on `ParaProperties`; render as the cell border path
  does (top/left/bottom/right strokes around the paragraph box).
- **A.M5 — `<w:tab/>` / `<w:sym>` / `<w:noBreakHyphen>` / `<w:softHyphen>`
  / `<w:cr>` run children** (audit A.12 remainder). Only `<w:br>`
  shipped. `<w:tab/>` needs A.M3 first. `<w:sym>` parsable via U+F000-
  range PUA mapping. `<w:noBreakHyphen>` = U+2011, `<w:softHyphen>` =
  U+00AD, `<w:cr>` = U+000D — same Unicode-mapping pattern as the
  `<w:br>` fix.
- **A.M6 — `<w:rPr><w:shd>` patterned shading attrs** (audit A.4). Only
  `w:fill` reads; `w:val` shading pattern (23 patterns) and `w:color`
  foreground drop. Patterns collapse to a solid fill. Mostly a render-
  side patterned-fill primitive task.
- **A.M7 — `<w:rPr><w:highlight>` element-identity preservation** (audit
  A.3). Highlight + `<w:shd>` both fold into `bg_color`. A
  `<w:highlight w:val="yellow"/>` round-trips as `<w:shd w:fill="FFFF00"/>`
  — same visual, different XML identity. Add a `bg_kind` discriminator
  on `SpanStyle` so the writer can pick the source element.
- **A.M8 — `<w:tcPr><w:tblLayout w:type="autofit">`** (audit B.6). Engine
  treats grid widths as fixed; autofit (Word's default) needs a two-
  pass measure + balance + re-layout. Listed in `DEFERRED §4` too.
- **A.M9 — `<w:trPr><w:tblHeader>` repeating header rows** (audit B.7,
  `DEFERRED §4`). Parser captures; paginator ignores. Multi-page tables
  lose their header on every page after the first.
- **A.M10 — `<w:sectPr><w:pgSz w:orient>`** (audit C.5). Metadata-only
  drop; visual layout usually still correct because Word swaps `w:w`/`w:h`
  on landscape selection. Affects Word's "Page Setup" dialog reading.
- **A.M11 — `<w:sectPr><w:pgNumType>`** (audit C.6). Cannot restart
  numbering at section boundary, cannot switch number format
  (`decimal` / `lowerRoman` / `upperLetter` ...). Without it,
  D.1's PAGE field works document-wide but cannot honour per-section
  numbering — which is the standard "Chapter 1: pages 1–10, Chapter 2:
  pages 1–8" layout.
- **A.M12 — `<w:sectPr><w:type w:val="continuous|oddPage|evenPage">`**
  (audit C.7, `DEFERRED §6`). Every section break forces a page flush.
  `continuous` (should flow inline) gets a hard page break instead.
- **A.M13 — `<w:rPr>` border style variants render** (audit B.11). Other
  border styles (`thick`, `triple`, `wave`, `dotDash`, …) drop to single
  stroke or no stroke. Renderer needs styled-stroke primitive.
- **A.M14 — `<w:tblGridChange>` grid-edit revision metadata** (audit
  B.5). Lost when a table is dirtied. Round-trip-only impact.
- **A.M15 — `<w:rsidR>` / `<w:rsidP>` / `<w:rsidRDefault>` / `<w:rsidRPr>`
  / `<w:rsidTr>` save IDs** (audit E.3, F.1). Three-way collaborative
  diff loses granular provenance on edited paragraphs.
- **A.M16 — `<w:moveFrom>` / `<w:moveTo>` tracked moves** (audit E.4,
  `DEFERRED §8b`). Drops to invisible on dirty paragraphs.
- **A.M17 — `<w:numPr>` inheritance from `<w:pStyle>`** (audit F.2). A
  list paragraph whose `<w:numPr>` lives on its paragraph style (e.g.
  built-in "List Paragraph") is not recognised as a list item.
- **A.M18 — Cascade through table cells** (`DEFERRED §10`). Phase-3
  style cascade + Phase-4 numbering resolver both run only on top-
  level paragraphs. Cell paragraphs see neither — `<w:pStyle>` /
  `<w:rStyle>` / `<w:numPr>` references inside cells silently fall
  back to defaults.

### A-Low (rarely-used / niche)

- **A.L1 — `<w:rPr>` remaining text-effect toggles** (audit A.8): `<w:dstrike>`,
  `<w:outline>`, `<w:shadow>`, `<w:emboss>`, `<w:imprint>`, `<w:vanish>`,
  `<w:specVanish>`, `<w:position>`, `<w:em>`, `<w:effect>`, `<w:kern>`,
  `<w:w>` (char scale), `<w:spacing>` (run-level char spacing),
  `<w:lang>`, `<w:noProof>`, `<w:rtl>` (run-level RTL force), `<w:bCs>`,
  `<w:iCs>`, `<w:cs>`. Drop silently on dirty save.
- **A.L2 — `<w:pPr>` remaining toggles** (audit A.11):
  `<w:textAlignment>`, `<w:contextualSpacing>`, `<w:widowControl>`,
  `<w:suppressAutoHyphens>`, `<w:adjustRightInd>`, `<w:autoSpaceDE>`,
  `<w:autoSpaceDN>`, `<w:snapToGrid>`. Subtle layout effects.
- **A.L3 — `<w:rPr><w:color w:themeColor>` advanced attrs** (audit
  A.10). Same theme-binding problem as A.M2.
- **A.L4 — `<w:trPr><w:cantSplit>` honouring** (audit B.8). Currently
  implicit-on for every row. Conservative; no data loss. Affects only
  tall-cell pagination ergonomics.
- **A.L5 — `<w:trPr><w:trHeight w:hRule="auto">`** (audit B.9). Bare
  `<w:trHeight w:val>` reads as `AtLeast`; spec default is `Auto`.
- **A.L6 — `<w:tblPrEx>` row-level table-property exceptions** (audit
  B.10). Per-row borders / shading overrides drop.
- **A.L7 — `<w:sectPr><w:lnNumType>` line numbering** (audit C.8).
  Legal-style line numbers in the margin.
- **A.L8 — `<w:sectPr><w:pgBorders>` page borders** (audit C.9).
  Decorative.
- **A.L9 — `<w:sectPr><w:vAlign>` page vertical alignment** (audit
  C.10).
- **A.L10 — `<w:sectPr><w:pgMar w:gutter>` / `w:mirrorMargins`** (audit
  C.11). Bind margin + book-style layout.
- **A.L11 — `<w:pPrChange>` / `<w:rPrChange>` / `<w:sectPrChange>` /
  `<w:tblPrChange>` / `<w:trPrChange>` / `<w:tcPrChange>` property-
  change revisions** (audit E.5, `DEFERRED §8b`).
- **A.L12 — `<w:proofErr>`, `<w:permStart>`, `<w:permEnd>`,
  `<w:bookmarkStart>`, `<w:bookmarkEnd>`** (audit F.3). Bookmarks
  become load-bearing once D.1's PAGEREF / REF subfields ship.
- **A.L13 — `<w:hyperlink w:anchor>` intra-doc anchors** (audit D.2).
  External URLs round-trip; internal anchors drop. Needs A.L12 first.

---

## Category B: Core UX & Interaction Gaps

Remaining leaf items from `UX_BEHAVIOR_SPEC.md` + UX-side deferrals from
`BACKLOG.md` and `DEFERRED_FEATURES_TRACKER.md`. Renumbered by current
priority, with the original spec section ID in parentheses.

### B-High (foundational interaction)

- **B.H1 — Grapheme-cluster Left/Right** (UX I.1). Caret currently
  steps by Unicode scalar (`char`), which can bisect Arabic harakat,
  Devanagari conjuncts, emoji ZWJ sequences. Spec requires UAX-#29
  grapheme cluster. Switch to `icu_segmenter::GraphemeClusterSegmenter`
  in `step_left` / `step_right`. Foundational: every other arrow-
  related spec assumes it.
- **B.H2 — Home / End / Ctrl+Home / Ctrl+End / PageUp / PageDown** (UX
  I.2, I.3.4). No keyboard primitives for line-start / line-end /
  document-start / document-end / page-scroll. Direction-aware (RTL Home
  lands on visual rightmost slot per UX III.4). Required for basic
  Word-class navigation.
- **B.H3 — BiDi-aware visual arrow mapping** (UX III.1, III.2, III.3).
  ArrowLeft / ArrowRight currently map to logical-backward / logical-
  forward regardless of paragraph direction. Spec mandates Word
  behaviour: ArrowLeft is **visual** left, swapping logical direction
  in RTL paragraphs. Cell-paragraph direction inheritance falls out of
  the same plumbing.

### B-Medium (rough edges in core flow)

- **B.M1 — UAX-#29 word boundaries** (UX II). Current word classifier
  is whitespace-only. Misses URL / "isn't" / `3.14` / CJK ideograph /
  emoji ZWJ cases. Swap inner classifier to
  `icu_segmenter::WordSegmenter`; the motion API stays stable.
- **B.M2 — Ctrl+Backspace / Ctrl+Delete word-delete TS wiring** (UX
  II.2). Engine ships `DeleteAtCaret { by_word: true }` both ways; TS
  shell doesn't dispatch them yet. Pure pointer.ts / keydown wiring.
- **B.M3 — Shift+Click extends selection** (UX I.3, IV.7). Currently
  every click resets anchor. Branch in `pointer.ts::onPointerDown` on
  `e.shiftKey` to dispatch `ExtendSelection` instead of `SetSelection`.
- **B.M4 — Caret rect orientation at BiDi seam** (UX III.5). Caret at
  the boundary between an LTR and RTL run has two valid visual
  positions; engine picks one arbitrarily. Target: track last-motion
  directional intent on `SelectionState`, render at the matching
  trailing edge.
- **B.M5 — Quadruple-click block select** (UX I.3). Whole cell content
  inside a table, whole table on the body, whole document elsewhere.
- **B.M6 — Paste-as-plain shortcut** (UX V.2). `Ctrl+Shift+V` should
  force the `text/plain` path even when `text/html` is present.
  Keybinding-only; engine already supports both paths.
- **B.M7 — Cell context menu** (`DEFERRED §1`). Right-click in a cell:
  Insert Row Above/Below, Insert Column Left/Right, Delete Row, Delete
  Column, Merge / Split, Cell Shading, Cell Borders. Engine commands
  exist; needs TS menu surface + the BlockPath-aware hit-test (already
  shipped in PR 4).
- **B.M8 — HTML clipboard fidelity beyond basic round-trip** (UX V.1,
  V.2, V.4). Tables emit but cell attributes (borders, shading) don't
  round-trip through HTML; CSS classes on incoming paste are ignored.
- **B.M9 — `Command::SetTableProperties` wrapper** (`DEFERRED §1`).
  Table-wide width / alignment / indent / borders are only set via
  cell-level commands today. Engine method to plumb in a single call;
  toolbar uses it.
- **B.M10 — Drag-out-of-cell selection** (`DEFERRED §1`). Anchor inside
  a cell, drag outside → today rects stop at the cell boundary; spec
  wants Word's "every cell + body between" semantics.
- **B.M11 — Per-author tracked-change colour palette** (`DEFERRED §8b`).
  Every insert renders green, every delete red. `Revision::author`
  string already carried — needs a colour resolver keyed on author.
- **B.M12 — Engine-side accept / reject revision** (`DEFERRED §8b`).
  `Command::AcceptRevision` / `RejectRevision` not wired. Model can
  represent the result (revision overlay drop + optional byte delete).
- **B.M13 — Click-to-open hyperlinks** (`DEFERRED §7`). Hyperlinks
  render with style only; no pointer handler surfaces the target on
  Ctrl-click. Needs a `HyperlinkHit` event from `HitTest`.
- **B.M14 — Internal hyperlink anchor navigation** (`DEFERRED §7`,
  audit D.2). Requires A.L12 (bookmarks) first.
- **B.M15 — Inline-image edit operations** (`DEFERRED §7`).
  `Command::InsertImage`, `DeleteImage`, resize-by-handle.

### B-Low (polish + opt-in)

- **B.L1 — Shift+End / Shift+Home** (UX IV.9). Inherits from B.H2.
- **B.L2 — UI override toggle for paragraph auto-direction** (BACKLOG
  §6 remainder). `Event::SelectionChanged.direction` still reports
  document direction, not caret-paragraph direction.
- **B.L3 — IME `target_range`-specific composition styling** (BACKLOG
  §8 remainder). Underline applies uniformly across the whole
  composition; sub-segment styling deferred.
- **B.L4 — Stable per-paragraph + per-cell IDs** (BACKLOG §10 remainder,
  `DEFERRED §1`). `diff_a11y` matches by content; a moved paragraph
  re-emits as remove + insert. Needs an id field on `engine::Paragraph`.
- **B.L5 — Cross-container body ↔ footer ↔ footnote selection** (UX
  IV.6). Anchor in body, caret in footer — currently illegal because
  `BlockPath` has no container tag. Defer until a concrete user case
  appears.
- **B.L6 — Canvas hover tooltips for revisions** (`DEFERRED §8b`).
  `revisions_snapshot()` is exposed; TS overlay component missing.
- **B.L7 — Show Markup toggle** (`DEFERRED §8b`). Final / Original view
  modes that hide either inserts or deletes.
- **B.L8 — Click-to-locate comment ranges** (`DEFERRED §8a`). Sidebar
  has the (block, offset) span; canvas does not highlight on hover or
  scroll on click.
- **B.L9 — True superscript footnote markers** (`DEFERRED §8a`).
  Currently a coloured rectangle placeholder. Needs
  `DisplayCmd::DrawText` primitive.
- **B.L10 — Engine-side footnote / comment mutations** (`DEFERRED §8a`).
  `Command::InsertFootnote`, `DeleteFootnote`, `AddComment`,
  `ResolveComment`. Reader is read-only on the API surface for these.
- **B.L11 — Comment threading (`<w:commentEx>`)** (`DEFERRED §8a`).
  `commentsExtended.xml` `done` state + reply parent ids.
- **B.L12 — Floating images (`<wp:anchor>` text-wrap)** (`DEFERRED §7`).
  Phase 7 parses inline images only.
- **B.L13 — EMU-precision image scaling** (`DEFERRED §7`). No aspect-
  ratio enforcement, no page-width clamping, no `<a:srcRect>` cropping.
- **B.L14 — Image-bytes writer round-trip** (`DEFERRED §7`). Reader
  stashes `word/media/*` via `other_entries`; engine has no
  `InsertImage` re-emit path.
- **B.L15 — Drag-and-drop selected text within document** (UX VIII).
  Word does it; explicitly out-of-scope until requested.
- **B.L16 — Smart quotes / autocorrect** (UX VIII).
- **B.L17 — Multi-caret editing** (UX VIII).

---

## Category C: Architectural Cleanups & Refactoring

Optimisation notes, infrastructure debt, and refactors that don't add
spec coverage but unblock or clarify future work.

### C-High (blocks production readiness)

- **C.H1 — Viewport culling for cold open** (BACKLOG §13 remainder).
  Cold open at 50 pages is ~4.7 s, over the 2.5 s `PHASE_5_HARDENING`
  §6 budget. Culling = lay out visible page only, defer the rest.
  Bigger architectural change than caching (`build_page`, hit-testing,
  `PageBox` contract all assume whole-doc layout).
- **C.H2 — Vello promoted to default + GPU-runner golden suite**
  (BACKLOG §4 remainder, `DEFERRED §4` Vello-paint-table). Vello path
  compiles and is reachable; CI lacks a WebGPU adapter so the GPU-
  specific golden corpus (`tools/visual-diff/golden/vello/`, ~0.5 %
  tolerance per `PHASE_3_RENDER_RTL.md` §2 D3.4) can't be generated
  in this environment. Canvas2D stays default until that lands. Vello
  also paints inline images as grey placeholders (no
  `peniko::Image` resource pipeline yet) and has no `paint_table`
  mirror.
- **C.H3 — D5.6 external security audit, D5.9 operator runbook, D5.10
  Arabic typography sign-off** (`CLAUDE.md` Phase-5 status). External
  / human deliverables blocking the `v0.1.0` MVP cut.

### C-Medium (correctness / honest accounting)

- **C.M1 — Two-pass vMerge height accumulation** (`DEFERRED §4`).
  Phase-5a: `VMergeRole::Continue` cells contribute 0 to row height;
  Restart cell paints at its own row's natural height. Need a second
  pass that sums merged-column heights into the Restart cell. ~30 lines
  in `layout_table_box`.
- **C.M2 — Mid-row pagination + `<w:cantSplit>` honouring** (`DEFERRED §4`
  + §6, audit B.8). Re-entrant layout pass that splits a tall cell
  across pages. Blocks A.M9 (`<w:tblHeader>` repeating rows) — header
  repeat doesn't fire until mid-row pagination is real.
- **C.M3 — Per-row passthrough on table edits** (`DEFERRED §4`). A
  single-cell edit dirties the whole table; writer regenerates ~200 KB
  of XML on a 200-row table. Per-row source-byte capture + per-row
  dirty tracking.
- **C.M4 — Nested-cell mutation through deeper `BlockPath`s**
  (`DEFERRED §5`). `top_level_block_index` resolver only handles
  `PathStep::Block(N)` as the first step. Recursive walking needed for
  edits inside cells of nested tables.
- **C.M5 — `Command::SplitSection` / `Command::SetPageGeometry`**
  (`DEFERRED §6`). `Section` is read-only on the engine API surface.
  Writer emits the original `<w:sectPr/>` from passthrough; engine
  edits don't round-trip.
- **C.M6 — `/W` glyph widths + font subsetting in PDF export**
  (`BACKLOG §3` remainder). Glyph advances ride per-glyph text
  matrices (correct but verbose); whole fonts embed rather than
  subsetting. Material file-size win.
- **C.M7 — PDF/A-2 / PDF/X conformance profiles** (`BACKLOG §3`).
  `PdfConformance::A2u` / `X3` currently fall back to plain PDF.
- **C.M8 — Multi-page PDF table pagination** (`DEFERRED §3`). Phase-5b
  groundwork done (`export_pdf` accepts `&[PageBox]`); needs a real
  paginated table flow to drive it. Unblocked now that Phase-6
  paginator ships — one-line wiring change.
- **C.M9 — Visual-diff goldens for tables** (`DEFERRED §5`). RFC §3.3's
  six cases (`table_2x2`, `table_grid_span`, `table_vmerge`,
  `table_borders_double`, `table_shaded_header`, `table_in_rtl_doc`).
  Fixtures parse + lay out + paint correctly; only the screenshots
  are missing.
- **C.M10 — Vello image decoding pipeline** (`DEFERRED §7`). Canvas2D
  decodes via `createImageBitmap` + `drawImage`; Vello paints grey
  placeholders. Needs `peniko::Image` resource path.
- **C.M11 — `Command::Recover` real implementation** (`CLAUDE.md` Phase
  5 status). Currently a stub — real recovery needs `Engine::snapshot()`;
  event-log snapshots are empty placeholders.
- **C.M12 — Telemetry pipeline real numbers** (`CLAUDE.md` Phase 5).
  `EngineStats.last_paint_ms` / `last_command_ms` and
  `Event::Painted.paint_ms` are `0.0` dummies. D5.7 collector wired;
  needs upstream measurement.
- **C.M13 — Header reader rich cascade** (`DEFERRED §6` remainder).
  `parse_header_xml` now reuses `parse_document_xml`, so style cascade,
  hyperlinks, revisions, fields all propagate. Remaining: tables
  inside headers/footers (the paragraph-only filter strips them; the
  paginator's band layout API has no table support yet).

### C-Low (housekeeping)

- **C.L1 — Stable per-paragraph IDs** (BACKLOG §10 remainder; also
  B.L4). Engine-side cleanup that unlocks better a11y diff + section /
  field navigation.
- **C.L2 — A11y `UpdateCell` / `InsertNode` / `DeleteNode` patches**
  (`DEFERRED §1`). Today whole-tree replacement. Depends on C.L1.
- **C.L3 — Programmatic field refresh** (audit D.1 follow-on).
  PAGE / NUMPAGES recompute per page via the layout pass; DATE / TIME
  evaluate to source-file cached value. A `Command::RefreshFields`
  surface would let the UI invalidate caches and re-evaluate.
- **C.L4 — Phase 5b sprint catch-up to ECMA-376 audit annotations**.
  The 12 backlog sprints (`v0.5.0-beta.2` → `v0.5.0-beta.3`) closed
  most of `BACKLOG.md` items 1–14; that file's "Still deferred" sub-
  bullets are the authoritative residual.
- **C.L5 — `MASTER_PLAN.md` Open Decisions** consolidation. Three of
  five (UI framework, default Arabic font, print pipeline) are decided
  by implementation but not marked. Yrs / CRDT and backend remain
  legitimately open.

---

## 2. Aggregate severity counts

After the §0 resolutions, the remaining defect surface:

| Category | High | Medium | Low | Total |
|---|---|---|---|---|
| A. ECMA-376 WML gaps | 3 | 18 | 13 | 34 |
| B. UX / interaction gaps | 3 | 15 | 17 | 35 |
| C. Architecture / debt | 3 | 13 | 5 | 21 |
| **Total remaining** | **9** | **46** | **35** | **90** |

The audit started at **24 visible WML defects**; resolved **11** to date
(footnoted in §0). Eight remain `High` or `Medium` in domain A; tables
section B.3 (vAlign render) is the only `High` that was correctly
identified pre-audit and is **still open**.

---

## 3. Recommended sprint ordering

A pragmatic 8-sprint plan focused on closing every `High`-severity item
before any `Medium` work — matching the audit's "additive, never break
CI" rule.

| Sprint | Items | Outcome |
|---|---|---|
| 1 | A.H1 (vAlign render) + A.H3 (caps/smallCaps) + A.M5 partial (`<w:tab/>` reader + writer; needs A.M3 for visible effect) | Pure visual-fidelity wins; no model surface changes. |
| 2 | A.H2 (`<w:cols>` multi-column) | Largest layout addition. New section / paginator path. |
| 3 | B.H1 (grapheme Left/Right) + B.H2 (Home/End/Ctrl+Home/Ctrl+End/PageUp/PageDown) + B.H3 (BiDi-aware arrows) | UX foundation. Every later UX item assumes these. |
| 4 | C.H1 (viewport culling) | Unblocks the §6 cold-open budget; biggest perf win remaining. |
| 5 | A.M1 (vertAlign reader) + A.M2 (`<w:rFonts>` theme + open font family) + A.M3 (`<w:tabs>`) + A.M4 (`<w:pBdr>`) | Round-trip-loss fixes for common formatting. |
| 6 | A.M8 (autofit) + A.M9 (`<w:tblHeader>`) + C.M1 (vMerge height) + C.M2 (mid-row pagination + cantSplit) | Tables to spec parity. |
| 7 | A.M11 (`<w:pgNumType>`) + A.M12 (`continuous` sectPr type) + A.M17 (numPr inheritance) + A.M18 (cascade in cells) | Section + cascade correctness. |
| 8 | B.M-series + B.L-series + C.M-series rollups | Final polish. Order within each by user-visible weight. |

`Low` severity items remain unscheduled until a concrete user case or
fidelity regression makes them load-bearing.

---

## 4. Definition of "100% feature-complete core"

The MVP gate per `MASTER_PLAN.md` §9 requires `100% of P0 OOXML features
supported`. With the §0 resolutions in place, **P0 = "every `High`-
severity item in §A + the `High`-severity items in §B/§C"** — the
sprint-1-through-sprint-4 set (9 items). Everything `Medium` is P1;
`Low` is P2.

When all 9 `High` items are closed AND C.H3 (external audit, runbook,
typography sign-off) returns clean, the core editor engine is genuinely
feature-complete for the MVP per the original plan.
