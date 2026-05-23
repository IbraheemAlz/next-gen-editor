# OOXML (ECMA-376) Compliance Roadmap

**Status:** Approved. Phase 1 shipped (commit `669b155`, 2026-05-23). Phase 2
in flight.
**Starting point:** `v0.5.0-beta.3`. `crates/format-docx` already handles the
OPC ZIP pass-through, `<w:p>` / `<w:r>` boundaries, and a subset of `<w:rPr>`
(bold, italic, underline, strike, color, highlight / `<w:shd>`, `<w:rFonts>`).
The round-trip harness asserts sibling entries are byte-identical and
`document.xml` drift is bounded by `2 × |inserted_text_bytes|`.
**Spec corpus:** `/home/ibrahim/Downloads/ECMA-376-1_5th_edition_december_2016/`
— PDF, RELAX-NG Strict schemas, XML Schema Strict, DrawingML geometries,
WordprocessingML art borders.

---

## 0. Scope discipline

ECMA-376 Part 1 is ~5500 pages and Part 4 ("Transitional Migration Features")
adds backwards-compatibility tags Word still writes today. We will *not* aim
at 100 % schema conformance — no real implementation has. The goal is
**enterprise-grade interoperability with Microsoft Word 365 + LibreOffice
Writer** on the document features users care about: text, styles, lists,
tables, sections, images, hyperlinks, fields, comments, and tracked changes.

Every phase obeys five invariants:

1. **Round-trip first.** A feature lands only when the read → write → re-read
   harness preserves both engine state and sibling entries.
2. **Pass-through over re-serialization.** Any archive part we do not fully
   model is carried verbatim in `DocxArchive.other_entries`. We *never*
   regenerate a part we cannot regenerate exactly.
3. **Additive bridge / engine schema.** New enum variants and struct fields
   are added with defaults; existing variants and consumers keep working.
4. **TDD.** A fixture and a failing assertion land before the code that
   makes them pass.
5. **No regression of the current `2 × N` document.xml bound** on the
   existing fixtures. New features that need a looser bound must justify it
   in the roundtrip harness with a per-fixture override.

### 0.1. Decisions log

Frozen after Phase 1 sign-off (2026-05-23). Supersedes the §6 open questions.

1. **Microsoft Word access.** Ground-truth fixtures are authored against
   **Office 365 Web** running in a dedicated Windows VM. **LibreOffice
   Writer** is the secondary baseline, exposing vendor-specific schema
   deviations.
2. **Phase 5 block-enum RFC.** Reaffirmed. A separate
   `PHASE_5_BLOCK_ENUM_RFC.md` will land before Phase 5 kickoff to scope
   the cascade across `engine`, `engine-wasm`, `bridge`, `layout`,
   `render`, `format-pdf`, and the TS shell.
3. **Schema validation.** Always validate against the **ECMA-376 Part 4
   Transitional** schemas, never Strict. Word's emissions are
   Transitional; Strict would be over-strict relative to the reference
   implementation.
4. **PDF table export.** Deferred to **Phase 5b**. Phase 5 ships
   `.docx` table read / write / on-screen render only; PDF table export
   does not block the Phase 5 release.
5. **Round-trip strictness.** **Strict zero-drift on unmutated
   paragraphs** is mandatory. The Phase 3 per-paragraph dirty tracking +
   passthrough optimisation are non-negotiable — template corruption on
   save is the failure mode they exist to prevent.

---

## 1. Architectural strategy

### 1.1 Module layout for `crates/format-docx`

Today the crate is three files (`lib.rs`, `reader.rs`, `writer.rs`). We
refactor into a per-part module tree before adding new parsers:

```
crates/format-docx/
├── Cargo.toml
└── src/
    ├── lib.rs                      # re-exports + public API
    ├── error.rs                    # DocxError (unchanged, split out)
    ├── opc/
    │   ├── mod.rs                  # OPC abstractions
    │   ├── archive.rs              # DocxArchive (pass-through container)
    │   ├── content_types.rs        # [Content_Types].xml (read-only parse)
    │   └── relationships.rs        # _rels/*.xml.rels typed model
    ├── parts/
    │   ├── mod.rs
    │   ├── document.rs             # word/document.xml (current reader split)
    │   ├── styles.rs               # word/styles.xml          (Phase 3)
    │   ├── numbering.rs            # word/numbering.xml       (Phase 4)
    │   ├── settings.rs             # word/settings.xml        (Phase 6)
    │   ├── theme.rs                # word/theme/theme1.xml    (Phase 7)
    │   ├── header.rs / footer.rs   # word/header*.xml         (Phase 6)
    │   ├── footnotes.rs / endnotes.rs                         (Phase 8)
    │   ├── comments.rs                                        (Phase 8)
    │   └── people.rs / commentsExtended.rs                    (Phase 8)
    ├── schema/
    │   ├── mod.rs                  # shared constants (NS_W, NS_R, ...)
    │   ├── ct_p.rs                 # CT_P (paragraph) helpers
    │   ├── ct_r.rs                 # CT_R (run) helpers
    │   ├── ct_ppr.rs               # CT_PPr      (Phase 2)
    │   ├── ct_rpr.rs               # CT_RPr      (today, lifted from reader.rs)
    │   ├── ct_tbl.rs               # CT_Tbl      (Phase 5)
    │   └── ct_drawing.rs           # DrawingML wrapper (Phase 7)
    ├── style_resolver.rs           # cascade engine             (Phase 3)
    ├── reader.rs                   # public read_docx, orchestrates parts
    └── writer.rs                   # public write_docx, orchestrates parts
```

`opc/` knows nothing about wordprocessing; it speaks parts, relationships and
content types. `parts/` parses individual XML members into typed structs.
`schema/` holds the reusable `CT_*` element helpers shared across part
parsers (e.g. a `<w:rPr>` lives inside `<w:r>`, `<w:pPr>/<w:rPr>` (paragraph
mark), and inside `<w:style w:type="paragraph">` style definitions; one
`ct_rpr` module serves all three).

### 1.2 Mapping OOXML hierarchy → engine's flat model

Word's formatting cascade (per the spec, §17.7.2):

```
Document Defaults  →  Table Style  →  Numbering Style  →
Paragraph Style    →  Character Style →  Direct Para Props →  Direct Run Props
```

Our `engine::SpanStyle` is flat: every property is `Option<T>`, "set" or
"not set". To bridge the two we add a **`StyleResolver`** (Phase 3) that:

1. Parses `styles.xml` into a `StyleTable { defaults, by_id: HashMap<StyleId, StyleDef> }`.
2. Resolves `<w:basedOn>` chains (with cycle protection) into a fully merged
   `StyleDef` per id.
3. On read, **bakes** the resolved cascade into `engine::SpanStyle` /
   `engine::ParaProperties` so the engine still sees a flat document. The
   user's view of formatting matches what Word shows.
4. On write, the original `styles.xml` is preserved verbatim
   (pass-through); we emit only direct formatting (`<w:rPr>` / `<w:pPr>`).
   This is *lossy in the OOXML sense* — re-saving a file may convert a
   stylesheet-driven paragraph into directly-formatted text on the runs we
   actually mutated — but it is *not* lossy to the user, because the
   rendered output is unchanged.

This trade-off (preserve cascade artifacts, write direct formatting) matches
what LibreOffice does and is the only realistic path without a full visual
model of every Word style.

#### Engine additions, by phase

| Phase | Engine type | Why |
|------|-------------|-----|
| 2 | `Paragraph.props: ParaProperties` (alignment moves in; indent, spacing, direction, line-height-override added) | `<w:pPr>` needs more than alignment. |
| 3 | none | Style cascade is resolved in `format-docx`, baked into existing `SpanStyle`. |
| 4 | `Paragraph.list_item: Option<ListItem>` | `<w:numPr>` reference + resolved marker text. |
| 5 | `enum Block { Paragraph(Paragraph), Table(Table) }`; `DocumentTree.blocks: Vector<Block>` | Tables are block-level. **This is the biggest engine change in the roadmap.** |
| 6 | `Section`, `HeaderFooter` | `<w:sectPr>`, `word/header*.xml`. |
| 7 | `enum Inline { Text(StyleRun), Image(ImageRef), ... }` *or* `Block::Image` for floating | DrawingML embedding. |
| 8 | `Hyperlink`, `Field`, `Comment`, `Footnote`, `Revision` | Trailing features. |

Phase 5's `Block` refactor is large enough that it cascades into layout,
render, and the bridge (`Command::*`). We sequence list support (Phase 4)
*before* it so list items remain flat paragraphs with a marker, and the
block enum lands once.

### 1.3 Pass-through invariant — extended

The current rule ("everything except `word/document.xml` is verbatim") relaxes
phase by phase. The order in which a part graduates from *pass-through* to
*modelled* is the canonical view of project progress:

| Part | Phase 0 | Phase 2 | Phase 3 | Phase 4 | Phase 5 | Phase 6 | Phase 7 | Phase 8 |
|------|---------|---------|---------|---------|---------|---------|---------|---------|
| `[Content_Types].xml`                | PT  | PT  | PT  | PT  | M (R/O)  | M (R/W)  | M (R/W)  | M (R/W)  |
| `_rels/.rels`                        | PT  | PT  | PT  | PT  | PT  | PT  | M (R/W)  | M (R/W)  |
| `word/document.xml`                  | M (R/W partial)  | M (R/W +pPr)  | M (R/W +sty refs)  | M (R/W +numPr)  | M (R/W +tbl)  | M (R/W +sectPr)  | M (R/W +drawing)  | M (R/W +fields)  |
| `word/_rels/document.xml.rels`       | PT  | PT  | PT  | PT  | PT  | PT  | M (R/W)  | M (R/W)  |
| `word/styles.xml`                    | PT  | PT  | M (R)  | M (R)  | M (R)  | M (R)  | M (R)  | M (R)  |
| `word/numbering.xml`                 | PT  | PT  | PT  | M (R)  | M (R)  | M (R)  | M (R)  | M (R)  |
| `word/settings.xml`                  | PT  | PT  | PT  | PT  | PT  | M (R)  | M (R)  | M (R)  |
| `word/theme/theme1.xml`              | PT  | PT  | PT  | PT  | PT  | PT  | M (R)  | M (R)  |
| `word/header*.xml` / `footer*.xml`   | PT  | PT  | PT  | PT  | PT  | M (R/W)  | M (R/W)  | M (R/W)  |
| `word/media/*`                       | PT  | PT  | PT  | PT  | PT  | PT  | PT (bytes preserved; rels modelled)  | PT  |
| `word/footnotes.xml` / `endnotes.xml`| PT  | PT  | PT  | PT  | PT  | PT  | PT  | M (R/W)  |
| `word/comments*.xml`                 | PT  | PT  | PT  | PT  | PT  | PT  | PT  | M (R/W)  |

`PT` = pass-through verbatim. `M (R)` = modelled, read-only (parsed but
re-emitted verbatim on write — we read the styles into our resolver but never
mutate the stylesheet). `M (R/W)` = modelled, read and re-serialized.

The reason `styles.xml` stays read-only forever is mass: a typical Word
template has 200+ style definitions, most of which we do not visually model
(e.g. `IntenseQuoteReference`). Re-serializing would lose properties.
Reading them into the resolver is enough to render correctly.

---

## 2. Phased roadmap

Each phase delivers a self-contained increment: it lands with fixtures,
unit tests, round-trip harness expansion, and a beta cut on the
`v0.6.x-beta.*` train (Phase 5 = `v0.7.x` because of the block-enum refactor).

### Phase 1 — OPC foundation & test harness expansion

**Goal:** No new wire features. Refactor for the long tail; expand testing
infrastructure so every later phase plugs in cleanly.

Deliverables:
- Module split per §1.1 (no behavior change). `cargo test --workspace`,
  `wasm-pack test`, the visual-diff farm, and `tools/roundtrip` all stay
  green at every commit.
- `opc::content_types::parse` and `opc::relationships::parse` — read-only
  typed views (we still write the bytes verbatim).
- A `DocxArchive::part_by_name(&str) -> Option<&[u8]>` accessor so later
  phases can fetch siblings without walking the `Vec`.
- `tools/roundtrip` learns a **fixture directory mode**: point it at
  `crates/format-docx/tests/fixtures/`, walk every `.docx`, run R→W→R and
  byte-diff. Per-fixture overrides in `fixtures/_manifest.json` for the
  document.xml drift bound.
- `crates/format-docx/tests/fixtures/` directory + the corpus that already
  exists from Phase 1 / D5.3 perf fixtures is mirrored / pointed-at, plus
  ~12 atomic Word-generated fixtures (see §3).
- CI gate: `tools/roundtrip --fixtures` non-zero on regression.

Exit gate: refactor PR is byte-stable on the entire existing fixture set
(harness must report zero new diffs).

**Effort:** ~1 week. **Risk:** low — pure refactor.

### Phase 2 — Paragraph properties (`<w:pPr>`)

**Goal:** Paragraph alignment (already in engine, but the writer doesn't
emit it and the reader doesn't parse `<w:pPr>` at all), plus indent,
spacing, direction, and a per-paragraph line-height override.

Engine change: `Paragraph` gains a `props: ParaProperties` struct.
`Paragraph.alignment` moves into it; the old field is removed (with a brief
shim during the transition PR if call sites need staggering).

```rust
pub struct ParaProperties {
    pub alignment: Option<Alignment>,        // <w:jc>
    pub indent: Indent,                      // <w:ind>
    pub spacing: Spacing,                    // <w:spacing>
    pub direction: Option<TextDirection>,    // <w:bidi>
    pub line_height: Option<LineHeight>,     // <w:spacing w:line>
    pub keep_next: bool,                     // <w:keepNext>
    pub keep_lines: bool,                    // <w:keepLines>
    pub page_break_before: bool,             // <w:pageBreakBefore>
}
```

`Indent.start_twips` etc. uses Word's twip unit (1/1440 inch) verbatim;
conversion to layout pixels happens in `engine-wasm` once.

Parser scope: every `<w:pPr>` child we model goes into `ParaProperties`;
unknown children are dropped on read but preserved-on-pass-through is not
applicable here because they are inside `document.xml`. We document the
*omitted-child set* per phase in `crates/format-docx/PROPS_COVERAGE.md`.

Writer: emits `<w:pPr>` in the schema-mandated child order before
`<w:r>` runs.

Fixtures: `pPr_jc_{start,center,end,justify}.docx`,
`pPr_ind_firstline.docx`, `pPr_spacing_before_after.docx`,
`pPr_bidi_rtl.docx`, `pPr_keepnext.docx`.

Layout impact: `Indent`, `Spacing` and `LineHeight` need plumbing into
`crates/layout/src/paragraph.rs` (`indent` and `spacing` become
`ParagraphBox` geometry; the existing `Backlog #5` dynamic line-height work
covers `LineHeight`).

**Effort:** ~2 weeks. **Risk:** medium — layout plumbing is non-trivial.

### Phase 3 — Styles & inheritance (`styles.xml`)

**Goal:** Parse `word/styles.xml` and resolve the cascade so a Word
document that gets all its formatting from styles (the common case for
templates) renders correctly.

Deliverables:
- `parts::styles::StyleTable` — `Document Defaults`, paragraph styles,
  character styles, table styles (parsed but unused until Phase 5),
  numbering styles (parsed but unused until Phase 4), latent styles.
- `style_resolver::StyleResolver`:
  - resolves `<w:basedOn>` chains with cycle detection (cap depth at
    spec's implementation guidance of 10),
  - applies the §17.7.2 cascade order to produce a "computed style" per
    `(paragraph style id, run style id, direct props)` triple,
  - implements OOXML **toggle property** semantics (b, i, caps, smallCaps,
    emboss, imprint, outline, shadow, strike, dstrike, vanish): toggles
    XOR up the chain rather than overriding.
- Reader integration: when a `<w:p>`'s `<w:pPr>/<w:pStyle>` or a `<w:r>`'s
  `<w:rPr>/<w:rStyle>` references a style id, the resolver looks it up and
  the resulting properties are folded into `ParaProperties` / `SpanStyle`
  before they reach the engine.
- Writer: emits only direct formatting; `<w:pStyle>` / `<w:rStyle>` refs
  are **dropped on write**. This is a documented, intentional lossy
  conversion (see §1.2). The original `styles.xml` is still preserved
  verbatim so re-opening in Word still has the style catalogue available
  — paragraphs we mutated will look directly-formatted, paragraphs we
  didn't touch are unchanged (sibling pass-through preserves them).

  Wait — that last clause is wrong. We *do* re-emit every paragraph in the
  writer today, so an untouched paragraph also loses its style reference.
  Phase 3 must therefore introduce a **passthrough optimization**: track,
  per paragraph, whether the engine has mutated it since load; if not,
  re-emit the original raw `<w:p>...</w:p>` byte range verbatim. This
  requires the reader to record `(start_offset, end_offset)` of each
  `<w:p>` in `document.xml` and the engine to track a `dirty: bool` (or
  better, a generation counter) per paragraph.

  This is the trickiest engineering item in the roadmap and is what makes
  the round-trip diff bound stay tight on stylesheet-heavy real-world
  documents.

Fixtures: a Word document built from "Heading 1", "Heading 2", "Quote",
"Intense Quote" styles, plus character-style overrides; we assert the
resolved `SpanStyle` matches what Word renders. Plus the passthrough test:
load → save without editing → byte-identical `document.xml`.

**Effort:** ~3-4 weeks. **Risk:** high — toggle semantics + passthrough
optimization + cycle handling.

### Phase 4 — Lists & numbering (`numbering.xml`)

**Goal:** Bulleted and numbered lists, including multi-level outlines,
roman numerals, restart-numbering, and custom bullet glyphs.

Deliverables:
- `parts::numbering::NumberingDefinitions` — `abstractNum` definitions
  (level templates, numFmt, lvlText, lvlJc, pStyle link, indent) + `num`
  instances (binding `numId → abstractNumId` with optional level overrides).
- `Paragraph.list_item: Option<ListItem>`:
  ```rust
  pub struct ListItem { pub num_id: u32, pub ilvl: u8 }
  ```
- Numbering resolver in `format-docx` that, given the doc's paragraphs and
  the `NumberingDefinitions`, computes the rendered marker text per
  paragraph (`"1."`, `"a)"`, `"•"`, `"I."`, `"1.1.2."` for outlines) and
  hands the engine `Paragraph.list_item` plus a `Paragraph.resolved_marker:
  Option<String>` (cached; recomputed on edit).
- Layout: `LineBox` gains an optional `marker_run` rendered to the left of
  (LTR) or right of (RTL) the line's text origin, inside the paragraph's
  indent. (Reuses Phase 2 indent.)
- Writer: emit `<w:pPr>/<w:numPr>/<w:ilvl><w:numId>` references; the
  `numbering.xml` itself stays passthrough (read-only).

Fixtures: bullet list, numbered list, nested 3-level outline,
restart-numbering, lettered list, roman list, custom bullet glyph.

**Effort:** ~2-3 weeks. **Risk:** medium — list rendering interacts with
indent geometry and BiDi (RTL lists put the marker on the right).

### Phase 5 — Tables (`<w:tbl>`)

**Goal:** Block-level tables with rows, cells, grid spans, vertical merges,
borders, shading, and cell-level paragraph content.

This is the largest phase because it forces the engine block-enum refactor.

Deliverables:
- Engine refactor:
  ```rust
  pub enum Block {
      Paragraph(Paragraph),
      Table(Table),
  }
  pub struct DocumentTree { pub blocks: Vector<Block> }
  pub struct Table {
      pub grid: Vec<TwipUnits>,         // <w:tblGrid>
      pub props: TableProperties,        // <w:tblPr>
      pub rows: Vec<TableRow>,
  }
  pub struct TableRow {
      pub props: RowProperties,          // <w:trPr>
      pub cells: Vec<TableCell>,
  }
  pub struct TableCell {
      pub props: CellProperties,         // <w:tcPr>: gridSpan, vMerge, borders, shd
      pub blocks: Vec<Block>,            // cells contain blocks (nested tables allowed)
  }
  ```
- All `Vector<Paragraph>` consumers update — bridge commands, hit-testing,
  a11y tree, geometry traversal, undo snapshots, html exporter,
  `format-pdf` exporter. **This is the cascade work.** We sequence each
  cross-crate update in its own PR behind the new enum, with the old
  `Paragraph`-only paths kept building via a `Block::as_paragraph()`
  convenience until everything compiles, then the convenience is removed.
- `parts::document` grows table parsing.
- `crates/layout` gains a `TableBox` that wraps `ParagraphBox`es per cell,
  computes column widths from `tblGrid` + cell overrides, handles
  `vMerge`, and stacks rows vertically. Borders render in the existing
  Canvas2D + Vello backends.
- `format-pdf` mirrors the table layout into its display list.
- A11y: each `<w:tbl>` becomes a `role="table"` with `<tr>`/`<td>` overlays.
- Writer: `<w:tbl>` re-emission preserves grid + cell spans; byte-stable
  on the passthrough optimization from Phase 3 when the table is
  unmutated.

Fixtures: `table_2x2.docx`, `table_grid_span.docx`, `table_vmerge.docx`,
`table_borders_double.docx`, `table_shaded_header.docx`,
`table_nested.docx`, `table_in_rtl_doc.docx`.

This phase ships as `v0.7.0-beta.1`.

**Effort:** ~6-8 weeks. **Risk:** very high — single biggest engine touch
since Phase 4 (UI shell).

### Phase 6 — Sections, headers, footers, settings

**Goal:** Multi-section documents with per-section page geometry, headers,
and footers. The `<w:sectPr>` we currently emit as `<w:sectPr/>` (empty)
becomes meaningful.

Deliverables:
- `parts::settings` (read-only model of `word/settings.xml`).
- `parts::header` / `parts::footer` — parse `word/header*.xml` and
  `word/footer*.xml` (one per section + type: default / first / even).
- Engine: `DocumentTree.sections: Vec<Section>`, a section spans a range
  of blocks and carries page geometry (page size, margins) and header /
  footer block refs.
- Layout: existing `PageBox` learns about section page sizes; pagination
  becomes section-aware (today the whole document is one A4 section).
- Render: headers / footers are drawn into the page margin areas at
  paint-time (the engine doesn't lay them out per page, only once per
  section).
- Writer: `<w:sectPr>` re-emitted with the real properties; header /
  footer files re-serialized.
- Content types + rels become modelled (R/W) here because adding a new
  section with a new header creates a new part and a new rel.

Fixtures: `sections_two_page_sizes.docx`, `header_footer_default.docx`,
`header_footer_first_page_different.docx`, `footer_with_page_number_field.docx`.

**Effort:** ~3 weeks. **Risk:** medium-high — pagination changes ripple
into the perf harness; we must keep insert-latency p95 under budget with
multi-section docs.

### Phase 7 — Media, hyperlinks, DrawingML

**Goal:** Embedded images and external hyperlinks. DrawingML's full
shape / chart / SmartArt surface is out of scope; we handle the inline-
image subset (`<w:drawing>/<wp:inline>/<a:graphic>/<pic:pic>`) plus
`<w:hyperlink>`.

Deliverables:
- `opc::relationships` becomes R/W: adding an image creates a rel id, a
  media file under `word/media/`, and a `<w:drawing>` reference.
- `parts::document` parses `<w:drawing>` inline-image runs and
  `<w:hyperlink>` runs.
- Engine: `Inline` enum becomes possible (`StyleRun` is renamed
  `Inline::Span` and a new `Inline::Image(ImageRef)` joins it), OR
  images stay paragraph-block-attached and inline images are handled as
  a special character + sidecar. We pick the simpler one (sidecar) when
  the time comes — decision deferred to Phase 7 design doc.
- `crates/render` learns to draw a `<canvas>`-blittable image (PNG/JPEG
  decoded via the `image` crate at load time).
- `format-pdf` embeds images as `/Image` XObjects.
- Hyperlink overlays in the DOM a11y tree become real `<a href>` elements
  (the existing `AccessibilityTreeDelta` machinery extends).

Fixtures: `image_inline_png.docx`, `image_inline_jpeg.docx`,
`hyperlink_external.docx`, `hyperlink_internal_bookmark.docx`.

**Effort:** ~3-4 weeks. **Risk:** medium.

### Phase 8 — Comments, footnotes, fields, tracked changes

**Goal:** The trailing high-value features that round out the
"interoperable Word document" claim.

Deliverables:
- `parts::footnotes` / `parts::endnotes`, with engine `Footnote` /
  `Endnote` referenced from inline `Inline::NoteRef`.
- `parts::comments` (and `commentsExtended`, `commentsIds`, `people`),
  with engine `Comment` ranges + an authoring/threading model.
- Field handling: `<w:fldSimple>` (e.g. `PAGE`, `DATE`) computed at
  render time; complex fields (`<w:fldChar>` begin / separate / end)
  parsed into a `Field` model.
- Revision marks: `<w:ins>`, `<w:del>`, `<w:moveFrom>`, `<w:moveTo>`,
  `<w:rPrChange>`, `<w:pPrChange>` — both read (so we can render Word's
  tracked-changes view) and write (so commands like
  `Command::AcceptRevision` round-trip).

Fixtures: `footnote_simple.docx`, `comment_threaded.docx`,
`field_page_number.docx`, `tracked_insert.docx`, `tracked_delete.docx`.

**Effort:** ~4-6 weeks. **Risk:** high — tracked changes touches every
edit command in the bridge.

### Phase 9 — Long tail & conformance harness (out of master roadmap)

Math (`<m:oMath>`), SmartArt, embedded charts, OLE objects, custom XML,
content controls (`<w:sdt>`), forms protection, password-protected
documents, and the Part 4 "Transitional" tag set. These land on demand,
each as its own mini-RFC, and are tracked in `BACKLOG.md` rather than
this roadmap.

---

## 3. Testing strategy

### 3.1 Fixture corpus

`crates/format-docx/tests/fixtures/` becomes the canonical store. Each
fixture is a real `.docx` file, generated by:

1. **Microsoft Word 365** (primary). Open Word, type the exact content
   the fixture name implies, save as `.docx`. Word's emissions are the
   ground truth for round-trip behavior — we test against what Word
   actually writes, not against the spec's idealised form.
2. **LibreOffice Writer** (secondary). For each Word fixture we add a
   `_lo.docx` sibling produced by LibreOffice with equivalent content.
   This catches Word-specific assumptions and exposes both vendors'
   schema deviations.
3. **Hand-crafted** (rare). Only for malicious / fuzz seeds or for
   schema cases neither editor emits (used as `fuzz/corpus/`).

A `fixtures/_manifest.json` describes every fixture:

```json
{
  "bold_italic.docx": {
    "generator": "word365",
    "phase_introduced": 2,
    "asserts": {
      "paragraphs": 1,
      "spans_in_para_0": [
        { "range": [0, 4],  "bold": true,  "italic": false },
        { "range": [4, 9],  "bold": true,  "italic": true  },
        { "range": [9, 14], "bold": false, "italic": true  }
      ]
    },
    "roundtrip": { "document_xml_drift_bound": "2N" }
  }
}
```

Initial fixtures, by phase:

| Phase | Fixtures (atomic — one feature each) |
|-------|--------------------------------------|
| 1 (seed) | `simple_text.docx`, `simple_arabic.docx`, `simple_xml_escapes.docx` |
| 2 | `pPr_jc_*.docx`, `pPr_ind_firstline.docx`, `pPr_spacing.docx`, `pPr_bidi_rtl.docx` |
| 3 | `style_heading1.docx`, `style_quote_basedon_normal.docx`, `style_toggle_bold_chain.docx`, `style_only_doc_defaults.docx` |
| 4 | `list_bullet_simple.docx`, `list_numbered_simple.docx`, `list_outline_3level.docx`, `list_restart_numbering.docx`, `list_rtl_bullet.docx` |
| 5 | `table_2x2.docx`, `table_grid_span.docx`, `table_vmerge.docx`, `table_borders_double.docx`, `table_nested.docx`, `table_in_rtl_doc.docx` |
| 6 | `sections_two_page_sizes.docx`, `header_footer_default.docx`, `header_footer_first_page_different.docx` |
| 7 | `image_inline_png.docx`, `image_inline_jpeg.docx`, `hyperlink_external.docx`, `hyperlink_internal_bookmark.docx` |
| 8 | `footnote_simple.docx`, `comment_threaded.docx`, `field_page_number.docx`, `tracked_insert.docx`, `tracked_delete.docx` |

### 3.2 Round-trip harness extension (`tools/roundtrip`)

Today the harness handcrafts a `DocumentTree` and asserts byte bounds.
It grows two new modes:

- **`--fixtures <dir>`** — walks the corpus, runs `read_docx →
  build_minimal_docx → read_docx` (or `write_docx` against the original
  archive for sibling-passthrough cases). Per fixture:
  1. Sibling entries byte-identical (existing rule).
  2. `document.xml` drift ≤ `_manifest.json`'s per-fixture bound (defaults
     to `2 × |inserted_text_bytes|`; for stylesheet-heavy fixtures where
     Phase 3's passthrough optimization is active, the bound is `0` —
     we re-emit the original `<w:p>` byte range verbatim).
  3. Second-read DocumentTree equality to first-read (semantic
     round-trip), including the resolved `SpanStyle` / `ParaProperties` /
     `Block` tree.

- **`--fixtures --mutate <edit-script>`** — for round-trip-after-edit
  coverage. The edit script (a tiny DSL: `insert "X" at para 0 pos 5`,
  `apply bold to para 0 range 0..5`, `delete para 1`) runs against the
  loaded doc; we then write, re-read, and assert the engine state matches
  the mutated state exactly. This catches the "lost data on save after
  edit" class of bug that round-trip-only misses.

### 3.3 Per-phase TDD loop

For every new feature inside a phase:

1. Save a fixture from Word that exercises the feature in isolation.
2. Add the manifest entry with hand-verified expected assertions
   (typed out by reading what Word actually produced — we never trust
   our own parser as the oracle).
3. Run `tools/roundtrip --fixtures` — assertions fail because the parser
   doesn't model the feature yet.
4. Implement the parser. Round-trip passes.
5. Implement the writer (or document the passthrough). Round-trip plus
   sibling byte-diff passes.
6. Run `--mutate` with a relevant edit script. Mutation round-trip
   passes.
7. Add a `cargo test` unit test in the relevant `parts::*` module that
   asserts the parser handles the smallest-possible XML snippet directly
   (a 5-line `<w:p>...</w:p>` literal), so future regressions get a
   pinpoint failure, not just a corpus-level diff.

### 3.4 Schema validation gate (advisory, not blocking)

ECMA-376 ships RELAX-NG Strict and XML Schema Strict at
`/home/ibrahim/Downloads/ECMA-376-1_5th_edition_december_2016/OfficeOpenXML-{RELAXNG,XMLSchema}-Strict.zip`.
We add an **advisory** CI step that validates every emitted `document.xml`
against the **Transitional** schema (Word's actual dialect is Transitional,
not Strict, so we must validate against the right one — we'll fetch
ECMA-376 Part 4's transitional XSDs separately).

Tooling: `xmllint --schema` or a Rust crate (`xmlschema-rs`). The gate
warns but does not block — Word's own output sometimes violates the
schema, so blocking would force us to be stricter than the reference
implementation.

### 3.5 Fuzzing

`fuzz/docx_reader` already exists. Per-phase additions:

- Phase 1: corpus expansion — every fixture becomes a fuzz seed.
- Phase 3: a new `fuzz/style_resolver` target that drops in random
  `<w:basedOn>` cycles, deep chains, missing parents.
- Phase 5: `fuzz/table_parser` — malformed grid spans, negative
  widths, deeply nested tables.
- Phase 8: `fuzz/field_parser` — malformed `<w:fldChar>` interleavings.

### 3.6 Visual sign-off

Each phase's `phase-N-signoff.md` checklist includes a manual step:
*open the largest fixture in Microsoft Word after our writer's output and
verify it opens without a repair dialog.* Word's "this file has been
repaired" warning is the gold standard for "you shipped invalid OOXML".

---

## 4. Cross-cutting concerns

### 4.1 Bridge schema evolution

Every phase adds bridge commands and events **additively**:

| Phase | New `Command` variants | New `Event` variants |
|-------|------------------------|----------------------|
| 2 | `ApplyParaProperties` | (none — `SelectionChanged` already covers) |
| 3 | (none — styles are read-side only) | (none) |
| 4 | `ApplyListItem`, `RemoveListItem`, `IndentListItem`, `OutdentListItem` | (none) |
| 5 | `InsertTable`, `DeleteTable`, `InsertTableRow`, `MergeTableCells`, ... | `TableChanged` |
| 6 | `InsertSectionBreak`, `EditHeader`, `EditFooter` | `SectionChanged` |
| 7 | `InsertImage`, `InsertHyperlink`, `RemoveHyperlink` | (none) |
| 8 | `InsertComment`, `ResolveComment`, `InsertFootnote`, `InsertField`, `AcceptRevision`, `RejectRevision` | `CommentChanged`, `RevisionChanged` |

Old variants are never renamed or removed.

### 4.2 Engine snapshot / undo

Every phase that adds engine state widens the `UndoStack` snapshot. The
existing bounded-depth-100 invariant holds; per-snapshot memory grows but
the `im::Vector` structural sharing keeps the cost sub-linear in document
size. We add a perf-harness regression to assert undo / redo p95 stays
under budget at each phase boundary.

### 4.3 Telemetry (D5.7 hook)

We add per-phase telemetry events: `DocxParseTimingsMs` (per part),
`DocxStyleCascadeDepth`, `DocxTableCellsCount`. These ride the existing
mock transport from D5.7; once the live collector lands, the OOXML
roadmap is one of its primary consumers.

### 4.4 Documentation

- `crates/format-docx/PROPS_COVERAGE.md` — a living matrix of every
  `<w:pPr>` and `<w:rPr>` child element and whether we read it, write it,
  or drop it. Updated every phase.
- `crates/format-docx/PASSTHROUGH.md` — the current §1.3 table, updated
  every phase.
- A short `docs/ooxml-cheatsheet.md` for the engine team mapping our
  flat `SpanStyle` / `ParaProperties` fields to their OOXML origin.

---

## 5. Phase exit gates (summary)

| Phase | Engineering exit gate | Approx. duration | Beta cut |
|-------|----------------------|------------------|----------|
| 1 | Module refactor; corpus + harness running on existing fixtures; CI gate green | 1 wk | (no cut) |
| 2 | `<w:pPr>` r/w; layout plumbed; ~6 new fixtures green | 2 wk | `v0.6.0-beta.1` |
| 3 | Style cascade resolved; passthrough optimization stable; stylesheet-heavy fixtures byte-identical | 3-4 wk | `v0.6.0-beta.2` |
| 4 | Lists rendered; numbering resolver correct on outlines; ~5 new fixtures green | 2-3 wk | `v0.6.0-beta.3` |
| 5 | Block enum landed across all crates; tables rendered; ~6 new fixtures green; Word opens output without repair | 6-8 wk | `v0.7.0-beta.1` |
| 6 | Sections / headers / footers; pagination correct; perf p95 in budget | 3 wk | `v0.7.0-beta.2` |
| 7 | Images embed + render; hyperlinks live in a11y tree | 3-4 wk | `v0.7.0-beta.3` |
| 8 | Footnotes / comments / fields / revisions r/w | 4-6 wk | `v0.8.0-beta.1` |

Total: **roughly 24-32 weeks of engineering work**, sequenced so we can
ship a beta and gather user feedback at every phase boundary.

---

## 6. Open questions for human review

1. **Microsoft Word access.** Phase 3 onward assumes we can author Word
   fixtures on demand. Confirm Word license / workflow.
2. **Phase 5 block-enum refactor blast radius.** This will touch
   `crates/engine`, `crates/engine-wasm`, `crates/bridge`,
   `crates/layout`, `crates/render`, `crates/format-pdf`, the TS shell,
   and every existing fixture. Worth a separate design RFC before Phase 5
   kickoff (a `PHASE_5_BLOCK_ENUM_RFC.md`).
3. **Strict vs Transitional schemas.** Word writes Transitional. We need
   to either fetch Part 4 transitional XSDs or accept that we cannot
   schema-validate Word's output. Recommended: do the latter, validate
   our writer's output against Transitional only.
4. **PDF export under tables.** `format-pdf` currently expects flat
   paragraphs. Phase 5's PDF table support is the longest tail inside
   Phase 5; if PDF export of tables is not a launch requirement, we can
   ship Phase 5 with PDF export *of tables specifically* deferred to a
   Phase 5b.
5. **Round-trip strictness with `styles.xml` passthrough.** The
   per-paragraph dirty tracking in Phase 3 is the linchpin that keeps
   real-world `.docx` files byte-stable on save. If we relax this — i.e.
   accept some drift on untouched paragraphs — Phase 3 effort drops by
   ~1.5 weeks. Recommended: keep strict; the drift bound is a key
   selling point.
