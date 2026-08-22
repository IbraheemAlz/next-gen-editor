# ONLYOFFICE — sanitized product & architecture reference (v2)

**Clean-room status.** Produced by isolated Reader agents per
`plans/cleanroom/PROTOCOL.md`. Contains feature inventories, user-visible
behavior, subsystem-level architecture, and methods described as prose only.
No source code, no internal identifiers, no constant tables crossed the wall.
Repo-relative paths are pointers for future Reader dives, not implementation
references. The machine-readable index lives in
`plans/cleanroom/onlyoffice.yaml` (provenance block included there).

This is **v2**: every claim in v1 was re-verified by independent Reader
passes over the current trees; refuted or amended statements are corrected
in place, and the RTL/Arabic, fields, footnotes, and tables sections are
substantially deepened. Section 7 lists what changed and why.

**Sources studied (read-only):**

- `onlyoffice-sdkjs` — the JavaScript editor engine (AGPL-3.0). The word
  processor lives in `word/`, shared machinery in `common/`.
  Provenance: branch develop @ 75a9683e0f34472d646642f7a9b7f81dfb91aa23
  (2026-05-26), studied 2026-08-22.
- `onlyoffice-core` — the C++ server core (AGPL-3.0): format conversion
  (x2t), OOXML read/write libraries, font machinery.
  Provenance: branch develop @ acdc1e39627638e1fa7b16c90f3c0da450a31256
  (2026-05-26), studied 2026-08-22.

Both checkouts are shallow clones (no git history); in-tree changelog files
are stale. Version-timeline facts below therefore come from the public
DocumentServer changelog (v7.0.0 → v9.4.0, fetched 2026-08-22) and public
release notes, not from the trees.

---

## 1. Product overview

ONLYOFFICE Docs is a browser-based office suite (word processor,
spreadsheet, presentation, PDF, diagram editors) sharing one JS engine
codebase. The word processor is a **fully client-side, paged, canvas-rendered
WYSIWYG editor**: the entire document model, style resolution, text shaping,
line breaking, pagination, and painting run in browser JavaScript. There is
no DOM-based text rendering and no iframe-hosted contenteditable document —
the page is painted onto HTML5 `<canvas>` 2D contexts, with a separate
overlay canvas for selection, caret, and collaborative cursors, and ordinary
DOM only for chrome (rulers, scrollbars, popups). Keyboard and IME input
arrive through a hidden textarea / contenteditable element positioned at the
caret — the same pattern our shell uses.

**Where layout truth lives: the client.** The C++ server core never lays out
text for the editor. Its job is format conversion: the `x2t` tool converts
`.docx` (and every other input format) into a compact **internal binary
snapshot format** which the browser downloads and deserializes into the JS
model; on save, the editor produces that binary again and the server converts
it back to `.docx`. A headless variant (the "doct renderer") embeds a JS VM
on the server and runs the *same* editor engine without a screen to produce
PDFs and support the document-builder scripting product — so even server-side
rendering reuses the client layout code rather than a second implementation.

**Internal units.** The editor's model and layout geometry are expressed in
millimeters (typographic points are converted at the boundary); the painting
layer applies a mm→pixel transform per zoom/DPI. All text measurement flows
through a font engine compiled to WebAssembly (FreeType + HarfBuzz behind a
JS wrapper), so metrics are identical across browsers and identical between
the interactive client and the headless server renderer — this cross-machine
determinism is what makes their server-side PDF path and pagination
consistency work.

**Fidelity strategy: regeneration, not preservation.** A `.docx` opened in
the editor is fully parsed into the internal model; saving re-serializes
every part from the model. Unknown or unmodeled constructs generally do not
survive a round trip. This is the opposite of our byte-preserving sibling
strategy and is worth knowing as a competitive differentiator: their
approach buys a simple uniform pipeline (every format becomes the same
binary) at the price of silent loss for exotic content. Two verified
casualties of this strategy appear below: the RTL-table flag
(`<w:bidiVisual>`) and kashida justification values are both destroyed by an
editor open/save cycle.

---

## 2. Feature inventory

Maturity is judged from what is visibly implemented in the tree: **full**
(complete, mature subsystem), **partial** (present with visible gaps),
**absent** (no meaningful implementation found).

### Text / run formatting — full
Bold/italic/underline/strikethrough/double-strike, sub/superscript, caps and
small caps, highlight, character shading, font color incl. theme colors,
character spacing, ligature control, vertical position. Faces are real
(family/style resolved through the font manager with fallback), not
synthesized unless the face is missing.

Shaping-feature fine print (verified): the shaping wrapper exposes exactly
five user-controllable OpenType features — the four ligature classes
(standard, contextual, historical, discretionary) mirroring the
`w14:ligatures` value set, plus kerning — and passes all five explicitly,
zeroed when unset. Consequences: kerning is off everywhere (no run property
in the editor model requests it; the OOXML `w:kern` property exists only in
the C++ format DOM), and contextual ligatures are off by default. Standard
ligatures are force-enabled for Arabic/Syriac-script runs regardless of the
run setting (papering over font-setting mistakes; tied to a public bug
number). Nonzero letter-spacing disables all ligature classes on that run.
The shaping call hard-codes English as the BCP-47 language, so
locale-dependent (`locl`) font behavior can never trigger. `w14:cntxtAlts`
is parsed and round-tripped by the conversion layer but never applied as a
shaping feature. No stylistic sets or number-form/spacing controls exist.

### Paragraph formatting — full
Alignment (incl. justify), indents (with RTL mirroring), line spacing modes,
spacing before/after with contextual suppression, borders and shading, tab
stops with all alignments and leaders, keep-with-next / keep-lines /
widow-orphan / page-break-before, paragraph frames (used for drop caps),
automatic hyphenation (pattern-based, per-language, opt-in), per-paragraph
RTL direction.

### Styles — full
Paragraph, character, table, and numbering styles; document defaults; the
full based-on inheritance chain; linked styles; quick-style gallery data.
Style resolution is "compile on demand and cache" (see methods §4.11).

### Lists / numbering — full
Multilevel numbering with abstract definitions plus per-list overrides,
restart rules, legal numbering, bullet and numbered presets, RTL-aware
number text rendering, list-level tab/indent interaction per OOXML rules.
Number formats include Arabic alphabetic and abjad letter sequences and two
Hebrew formats (genuinely generated, not just parsed), plus a large
East-Asian set. Caveat: list-number text is painted codepoint-by-codepoint
(bidi-reordered and bracket-mirrored but **unshaped**), so single-letter
abjad values render fine while multi-letter values would render unjoined.
List autoformat recognizes Arabic-Indic and Extended Arabic-Indic digits, so
typing "١." can trigger automatic numbered-list detection.

### Tables — full
Table grid model with declared column grid reconciled against actual cell
spans; fixed and AutoFit layout; horizontal and vertical merges; nested
tables; repeated header rows; row splitting across pages; floating
(positioned) tables with text wrap; table styles with conditional formatting
bands; cell text direction (vertical text) and per-cell margins/borders/
shading with border conflict resolution. Verified detail:

- **Column widths — a fixed pass that always runs plus a conditional
  AutoFit pass.** The declared-grid reconciliation (grid columns widened by
  explicit single-span cell widths, then a monotonic widening walk over rows
  for multi-span cells and before/after row offsets, then proportional
  rescale to any declared table width) always runs and alone defines
  fixed-layout tables. The content-based min/max pass runs only in autofit
  mode — and for a **nested** table the governing mode is the **top-level
  ancestor table's** layout mode, not its own. An explicitly set cell width
  *replaces* the column's max-content (explicit beats content, the Word
  behavior); a third metric tier (min content ignoring preferred widths)
  drives overflow fallbacks; multi-span distribution is deliberately
  order-dependent (swapping two rows can change the computed grid); min/max
  content values are clamped to Word's publicly documented 22-inch cap. The
  distribution is recognizably the CSS2 auto-table algorithm extended with
  Word's preferred-width tiers. There is no Word-style "AutoFit to
  Contents / AutoFit to Window" command — only the fixed/autofit layout
  property plus distribute-rows/columns operations.
- **Row splitting**: a row splits only if every participating merge-start,
  non-vertical-text cell can place some content on the current page;
  otherwise all cells restart on the next page. Fixed and at-least height
  rows that don't fit move whole. Rotated-text cells never split.
  **The OOXML per-row can't-split property (`<w:cantSplit>`) is parsed,
  stored, and round-tripped but never consulted by pagination, and no UI or
  API sets it — row-split suppression is effectively unimplemented.** (v1
  wrongly said splitting honors it.)
- **Repeated header rows**: top-level **inline** tables only — never nested
  (deliberate Word parity) and never floating. Rows whose vertical merges
  cross the header boundary are trimmed from the header block; a header
  block too tall for a page is skipped on that page; headers are re-laid
  from per-page clones outside undo history. Cells keep two resolved
  top-border variants (against the true previous row vs the repeated
  header) so continuation pages draw correct borders.
- **Floating tables**: anchor machinery shared with drawings
  (page/margin/column × align-or-offset), rectangular both-sides wrap only,
  overlap avoidance honoring `<w:tblOverlap>`, Word-parity quirks
  deliberately replicated (table indent added to a floating table's X;
  header/footer-anchored floating tables get an unbounded bottom).
  Multi-page floating tables flow but never repeat headers.
- **Conditional bands**: the six table-look flags layered per ECMA-376
  application order, band membership by floor-division of the band size,
  with Word-parity subtleties: row-band counting starts after the header
  row only when the first-row flag is on; the analogous column shift
  requires the style to actually define a non-empty first-column
  conditional style; cell text direction is never inherited from a table
  style; the stored per-cell conditional bitmask (`<w:cnfStyle>`) is
  transported but never consumed — conditions recompute from live position.
- **Borders**: two models. With cell spacing there is **no** conflict pass
  (each cell draws its own borders plus the table's outer border). In the
  collapsed model, an explicitly set cell border beats the table border, a
  none border loses to any set border, and ties resolve by the ECMA-376
  comparison (wider wins, then the spec's weighted-brightness cascade).
  Resolved borders are stored per cell edge as per-grid-segment arrays and
  inset content metrics before layout. (v1's "row/cell/table precedence"
  wording was wrong — OOXML has no row borders.)

Notable gap: **no RTL table support** — and worse than v1 recorded:
`<w:bidiVisual>` is fully parsed/round-tripped by the C++ conversion core
(docx DOM read/write with revision merge, .doc/RTF/ODF-import mappings, and
a dedicated internal-binary record), but the JS word editor neither models,
deserializes, lays out, nor draws it — the editor's binary-format table
property vocabulary stops one record short of the bidiVisual record the C++
side defines, so unknown-record skipping **silently drops the property on
any editor open/save cycle**. Pure C++ format-to-format conversion preserves
it; the editor destroys it.

### Sections / page layout — full
Section breaks (next page, continuous, even, odd, column); per-section page
size, orientation, margins, gutter (incl. RTL gutter position); multiple
columns with unequal widths and column breaks; title-page flag; page
borders (with display options); line numbering per section; per-section
footnote/endnote properties; section-aware vertical text alignment.
Gutter placement resolution (verified): document-level gutter-at-top wins
(gutter added to the top margin); otherwise the gutter goes to the right
edge when the section's `<w:rtlGutter>` is set, else the left; mirror
margins swap the side on odd pages. Gap: section-level `<w:bidi>` (RTL
column fill order for multi-column sections) is conversion-layer-only —
the editor always fills columns left-to-right.

### Headers / footers — full
First/even/odd variants, link-to-previous inheritance as slot absence,
different-first-page per section, watermark discovery/editing inside the
header, header/footer content is a full document content object (tables,
images, fields all work). Growth of a header pushes body content down;
page-count-dependent content triggers a dedicated re-layout pass (each
header/footer object keeps a registry of page-count-dependent elements
populated during layout).

### Fields / TOC — full (core set), partial (breadth)
Complex fields use the OOXML three-character model (`w:fldChar`
begin/separate/end) with a single instruction-text parser (one keyword
dispatch) producing typed field objects. Keywords that produce typed
instructions: PAGE, PAGEREF, TOC, REF, NOTEREF, NUMPAGES, HYPERLINK, SEQ,
STYLEREF, TIME, DATE, ADDIN, MERGEFIELD, FORMTEXT, FORMCHECKBOX, and
leading-`=` expression formulas. Any **unrecognized keyword falls back to
REF parsing** (mirroring Word's implicit-REF shorthand); an empty
instruction becomes an empty-target REF.

Two v1 corrections at the behavioral level:

- **ASK is effectively unreachable**: a typed ASK instruction and its parse
  routine exist, but the keyword dispatch matches a misspelled token
  ("ASC"), so a genuine ASK instruction from a real document takes the
  unknown-keyword fallback and parses as a REF of its bookmark argument.
  A live typo-bug in the newest public code.
- **FORMDROPDOWN is never typed**: a field-kind constant and one predicate
  exist, but no parser branch produces that kind — a FORMDROPDOWN
  instruction falls into the REF fallback. The legacy form-field data
  payload (OOXML `w:ffData`, including the dropdown entry list) is fully
  modeled and round-tripped, so the data survives, but the runtime never
  operates the field as a dropdown.

Simple fields (`w:fldSimple`) are **not** simply converted to the complex
representation (v1 was wrong): they load into a dedicated inline
field-wrapper object — a third representation holding the cached result
runs, typed by the same parser. Special cases: simple PAGE/NUMPAGES fields
are replaced at load by dedicated page-number/page-count run items;
remaining simple PAGE/NUMPAGES wrappers in headers/footers are converted to
complex fields on open (auto-update of simple fields in headers/footers is
unimplemented, per an in-tree note); a simple field whose instruction can't
be typed is unwrapped to plain content, dropping field-ness.

Formula fields: the expression engine's function library is the complete
Word-standard set of 18 (ABS, AND, AVERAGE, COUNT, DEFINED, FALSE, IF, INT,
MAX, MIN, MOD, NOT, OR, PRODUCT, ROUND, SIGN, SUM, TRUE), with bookmark
arguments, table cell-name and cell-range arguments, and numeric-picture
result formatting.

TOC breadth (public field-syntax facts): heading-level range, explicit
style list with levels, hyperlink entries, per-level page-number omission,
separators, tab preservation, line-break removal, and the caption-sequence
switches (table of figures via SEQ) are handled. **Not parsed** (in-code
to-do): bookmark-scoped collection (\b), sequence separators (\d),
TC-entry-based collection (\f, \l), sequence-prefixed page numbers (\s),
web-view leader hiding (\z), applied outline level (\u) — so TC-entry
tables of contents are unsupported. PAGEREF: hyperlink and above/below
switches. REF: hyperlink, delimiter, number-context variants, position.
NOTEREF: hyperlink, mark formatting, position. DATE/TIME: date-time
picture. PAGE/NUMPAGES: number-format keywords. HYPERLINK: target, tooltip,
in-document anchor (other switches deliberately ignored). MERGEFIELD: field
name only (before/after-text switches unparsed). The general MERGEFORMAT
switch is recognized and stored as a flag.

The field-update action recomputes: PAGE, TOC, PAGEREF, NUMPAGES, formulas,
SEQ, STYLEREF, TIME, DATE, REF, NOTEREF, FORMCHECKBOX, FORMTEXT, ADDIN.
Word's long tail (SECTIONPAGES, AUTONUM family, DOCPROPERTY, metadata
fields, QUOTE, INCLUDETEXT/INCLUDEPICTURE, SYMBOL, EQ, GOTOBUTTON,
MACROBUTTON, CITATION/BIBLIOGRAPHY, INDEX/XE, TA/TC) has no runtime model.

### Footnotes / endnotes — full
Reference marks with auto-numbering, layout negotiation that reserves
page-bottom space line-by-line (see §4.7), footnotes continuing across
pages/columns, endnotes at section/document end, footnote position options
(page bottom vs below text). Column-aware: the controller keeps one slot
per body column per page; placed notes, block heights, carry-over lists,
separator snapshots are all per-column. Verified details and corrections:

- **The continuation notice is model-only.** The "continued on next page"
  story has a slot beside the two separators (default empty, copied and
  serialized, per-column snapshot slot), but **no layout or paint path ever
  lays it out or draws it** — only the separator and continuation-separator
  stories render. Same status for endnotes. (v1 listed the notice as a
  working feature.)
- Numbering: footnotes restart per page, per section, or run continuously;
  endnotes are continuous (default) or per-section. Word-compat nuance
  encoded in both: a custom start number only takes effect in continuous
  mode. Reference-mark formats implemented: decimal, upper/lower roman,
  upper/lower letter; anything else — including Chicago symbol marks —
  falls back to decimal at the mark level (the shared list-numbering
  machinery supports Chicago, but footnote marks don't route through it).
  Custom marks: stored on the reference; custom-marked notes are skipped by
  the auto-numbering sequence.
- Position quirk replicated from observed Word behavior: the section-end /
  document-end position values (meaningless for footnotes) act as
  beneath-text; anything else — even out-of-spec values — acts as
  page-bottom. Notes are laid during body flow relative to reference lines,
  then the whole column block is shifted in a post-step (under body content
  for beneath-text; pinned to the content-frame bottom for page-bottom).
- Tables: the row loop snapshots the footnote controller's column state per
  row (rolled back when a row moves), reduces row content bottoms by the
  footnote height, and registers bottom limits for fixed-height rows; a
  cross-cell edge case (a later reference geometrically higher in another
  cell of the same row) is handled by clamping and carry-over appending.
  Only the top-level document negotiates; nested-table content defers to
  its outer container.
- Fast-path interaction: the line- and paragraph-level fast relayout tiers
  **skip footnote renegotiation and mark renumbering entirely**; correctness
  relies on the fast tiers' bounds/end-state verification demoting to full
  reflow (which redoes negotiation), not on re-negotiating.

### Comments — full
Ranged comments anchored by paired range markers in the flow (positions
survive edits), replies, resolve state, per-user attribution, time stamps,
quote text capture, on-canvas anchors and highlight, comments in headers
and text boxes, mention data carried for the shell to consume.

### Track changes (review) — full
Insert/delete/format/property changes on runs, paragraphs, tables and
drawings; **move detection with paired move-from/move-to marked ranges**;
per-user colors; accept/reject per change, per selection, or all; a
change-navigation manager that walks changes in document order; display of
deleted text inline with strikethrough; review info (author + timestamp,
plus a "previous state" chain so a tracked change over a tracked change is
representable). Tracked changes interact correctly with collaboration (a
reviewing user's changes stream like any other edit).

### Images / drawing objects / text wrap — full
Inline and floating (anchored) drawings; the full DrawingML shape model
(preset geometries, custom paths, fills incl. gradient/pattern/picture,
line styles, shadows, groups, connectors); text boxes with internal
document content; all wrap types (in-line, square, tight, through,
top-and-bottom, behind, in-front) with wrap polygons (auto-computed and
user-editable); anchoring to page/margin/paragraph/character with all
alignment and offset options; z-order; rotation/flip with correct wrap
interaction; animated GIF playback; OLE objects rendered from cached
images with embedded data preserved for re-edit; SmartArt rendered from
its layout definition (editable at shape level). Metafile (WMF/EMF)
rasterization happens server-side during conversion.

### Charts — full
A native chart rendering engine draws every major chart family (bar, line,
pie, area, scatter, stock, surface, radar, combo, and the newer "chartex"
types) on canvas from the chart part's model; chart data is edited through
an embedded spreadsheet component; styles/presets included. This is a
shared-suite subsystem, not word-specific.

### Math / equations — full
An OMML (Office Math) content model and layout engine: fractions, radicals,
n-ary operators, delimiters, accents, limits, matrices, boxes, bars —
recursively laid out inside paragraph lines with correct baseline math.
Input via linear **UnicodeMath and LaTeX parsers** plus math autocorrect;
conversion between professional and linear display. Legacy binary equation
(Equation Editor 3) content is converted to OMML during import by the C++
core. In BiDi context, math content is an opaque LTR island (the reorder
flow flushes around it; hit-testing recurses into its own bounds).

### Content controls / forms — full (their flagship differentiator)
Inline and block structured document tags: rich/plain text, checkbox,
combo/dropdown, date picker, picture; plus a "fixed form" layer for
fillable-form authoring (fixed-size fields, comb-of-characters text,
required flags, field keys/tags, roles) that exports to their own form
format and to PDF forms. A dedicated forms manager tracks all form fields,
supports a form-filling mode with its own restricted undo, and drives
form data extraction.

### Mail merge — full
MERGEFIELD-based: a recipient data source (spreadsheet) is attached, field
names map to columns, preview iterates records, and the merged output is
produced server-side (docx/pdf/email targets). The editor tracks all merge
fields in a registry for fast replace/preview.

### RTL / CTL & i18n — substantial but young (rewritten for v2)

**Shaping (genuinely strong).** Complex-script shaping is stock upstream
HarfBuzz compiled to WASM (the C++ tree fetches upstream at a pinned commit
with only a FreeType-glue patch; HarfBuzz is fetched at build time, not
vendored), with FreeType-based glyph positioning — so Arabic joining,
contextual forms, mandatory ligatures, and mark/diacritic (harakat)
placement are real HarfBuzz output. The buffer uses monotone-grapheme
clustering; each base+marks cluster becomes one interned grapheme storing
every glyph with its offsets. GDEF glyph classes and HarfBuzz
unsafe-to-break flags are exported back to JS, but the break flags are
unused (explicit in-tree to-do). The document model stays strictly
per-codepoint: a multi-codepoint cluster's width is divided **evenly**
across its codepoints (caret slots inside a lam-alef exist at even
fractions — typographically arbitrary but selection-complete).

**Shaping segmentation.** The shaping buffer flushes (ending the
joining/ligature context) on: any space (words shape independently), script
change, direction change, font change (including mid-word fallback
substitution), a shaping-relevant property change, crossing an inline
container boundary (an Arabic word half inside a hyperlink loses its join),
and a max buffer length. The "same shaping context" comparison deliberately
checks only shaping-relevant properties — changing color, underline,
highlight, or language mid-word does **not** break Arabic joining (a
genuinely thoughtful detail worth adopting).

**Script itemization.** Per-codepoint script from HarfBuzz's lookup, with
two pragmatic overrides: COMMON characters inside a complex-script-flagged
run stay with the surrounding script; and a hard-coded codepoint range from
the Arabic comma (U+060C) through U+074A is force-tagged Arabic *before*
the lookup — a workaround referencing two public tracker bugs about
word-splitting, with a to-do admitting it should be replaced by a real bidi
algorithm. That blunt range covers the entire Syriac block, so Syriac text
shapes under an Arabic script tag. The same override is duplicated in the
spreadsheet cell renderer's fragment shaper.

**Font slots and Arabic fallback.** An OOXML font-slot classifier
(ascii/hAnsi/eastAsia per a lookup table following the public ECMA TC45
disposition DR 09-0040, extended empirically) maps Hebrew/Arabic/Syriac/
Thaana and Arabic Presentation Forms into the *ascii* slot; the
complex-script (cs) slot activates only when the run carries the CS or RTL
flag — matching Word's model (`w:rFonts` cs + `w:bCs`/`w:iCs`/`w:szCs` as
run flags, not per codepoint). The toolbar mirrors this: with the caret in
CS text it shows the CS/bidi font, bold/italic, size, and language.
Runtime fallback is (a) per-codepoint cmap-coverage substitution among
loaded fonts (a buffered word migrates wholesale to the substitute when it
fits, mitigating mid-word splits — but a mid-word font switch still breaks
joining), plus (b) a server-generated Unicode-range→font table whose
hand-ordered priority list contains **no dedicated Arabic font** — Arabic
fallback is "whatever general-purpose font covers U+0600 wins", functional
but never typography-aware. Spaces are always measured in the ascii-slot
font even between CS-slot words. The default theme maps the Arab script
tag to Arial.

**BiDi (pragmatic streaming subset of UAX #9 — confirmed, sharpened).**
Character *classification* is full UAX #9: a per-codepoint table generated
from Unicode data covers every bidirectional class including the explicit
embedding/override codes and the isolates, plus a separate bracket-pair
table from Unicode bracket data. *Resolution* is the subset: a small
streaming reorderer buffers strong-RTL, strong-LTR, and neutral/weak items
with flush rules — neutrals take the paragraph base direction (from
`<w:bidi>`); European-digit sequences group as LTR islands inside RTL, with
digit separators absorbed only strictly between digits; tab/break/paragraph
mark are strong in the base direction (the pilcrow lands at the visual end
of an RTL paragraph). **No embedding-level stack exists**: the explicit
directional formatting characters (LRM/RLM/LRE/RLE/LRO/RLO/PDF, isolates)
are classified but have zero consumers — they degrade to their base class.
**Bracket-pair data is not used for UAX #9 rule N0**: it is consulted only
at *paint* time to substitute the mirrored counterpart glyph when a paired
bracket flows RTL (body text and list numbers); non-bracket Bidi_Mirrored
characters are never mirrored, and it is glyph-substitution-by-codepoint,
not the font's rtlm feature. A full UAX #9 engine exists in the C++ tree
only as part of a vendored ICU distribution that nothing outside vendored
code ever calls; the XPS importer applies file-stored bidi levels without
computing any. There is **no first-strong auto-direction**: paragraph RTL
comes only from the file or the explicit UI/API toggle (contrast: our
engine ships paragraph auto-direction). A per-character RTL flag is
computed and stored but never read (vestigial).

**The load-bearing pattern — one reorderer, replayed by every consumer.**
Layout X assignment, glyph painting, decoration/highlight painting,
selection-rect building, caret positioning, hit-testing, and list-number
rendering all re-run the same streaming reorderer over the same logical
item stream. No stored visual-order structure exists; visual order is
always recomputed. Caret, selection, and pixels therefore cannot disagree
in mixed-direction text — consistency by construction. (Our analogue:
derive caret slots, selection rects, and hit-testing from the same
flattened geometry the renderer consumes; never let a second reordering
implementation exist.)

**Editing behavior in mixed text.** Arrow movement is purely logical-order
with a paragraph-level key flip: in an RTL paragraph, Left runs the
internal forward path and Right backward, decided solely by the paragraph's
base direction (the run under the caret is irrelevant); word-wise movement
flips identically; Backspace/Delete/Home/End are purely logical; Up/Down
are coordinate-based. There is a single caret (no split caret). Two
refinements: the caret calculator tracks the element logically preceding
the caret for the ambiguous between-opposite-directions case, and an
as-you-type rule pins the caret visually after the just-typed character so
Arabic typing at a direction boundary doesn't jump. Selection replays the
reorderer per line and epsilon-merges adjacent rects, so discontinuous BiDi
selection emerges for free. Cluster discipline: the caret never lands
between a base and its combining marks; marks delete with their base;
hit-testing snaps out of cluster interiors.

**Mirrored geometry.** RTL paragraphs mirror indents (left+first-line to
the right edge), fill wrap-cutout ranges right-to-left, mirror alignment
(stored alignment is *logical* for document paragraphs, with left/right
swapped to visual terms at the UI boundary in both directions; shape text
stores *visual* alignment and swaps at toggle/recalc time — two coexisting
conventions), overhang trailing-space width past the mirrored edge, resolve
tabs in a reflected coordinate space measured from the right margin
(keeping one tab algorithm for both directions, including Word's
clamp-to-margin quirk), mirror paragraph borders/shading extents, mirror
click-and-type dead zones and auto-alignment, and mirror the
formatting-mark glyphs (tab arrows, break marks). The canvas ruler receives
the direction flag and moves its zero origin to the right margin. List
numbers render at the visual right end with mirrored `w:lvlJc` anchoring.
Direction survives Enter-with-style-advance and propagates into Text Art
inserted from an RTL paragraph.

**Typing and language plumbing.** Typed complex-script codepoints auto-set
the CS run flag and split runs; keyboard-layout changes (polled from the
shell via a public query) stamp run language; applying a language splits
mixed-direction content into bidi-language vs regular-language by strong
direction. But typing never stamps run-level `<w:rtl>` (explicit in-tree
placeholder). The document-default bidi language is Arabic (Saudi Arabia).
A boot-config RTL-interface flag (plus runtime setter) makes sdkjs mirror
its own canvas-drawn chrome — rulers, style-gallery previews, scroll/page
layout — so UI RTL is not purely a shell concern.

**Numerals.** A document-level numeral-display setting (matching Word's
option) accepts western and hindi; "context" is defined in the enum but
explicitly rejected. In hindi mode ASCII digits are substituted at display
time with Arabic-Indic digits (the model keeps ASCII). PDF form fields have
their own per-field version.

**The two confirmed gaps (re-verified at zero hits, both trees):**

1. **No kashida/tatweel justification, anywhere.** Justify distributes
   leftover width evenly across inter-word spaces (trailing spaces
   excluded); a per-letter channel exists only for the no-spaces CJK case
   and is not offered to Arabic. The WASM shaper has no elongation hooks;
   no JSTF/jalt/stch machinery exists. Worse than v1 recorded: the OOXML
   kashida `w:jc` values (lowKashida/mediumKashida/highKashida) are parsed
   by the C++ OOXML DOM but fall through the docx→internal-binary
   conversion's default branch to **left alignment** — a kashida-justified
   Arabic document opens start-aligned, *not even justified*, and (given
   regeneration saves) loses the setting permanently. distribute and
   thaiDistribute degrade to plain justify. The RTF layer round-trips its
   kashida-percentage keywords at format level only; the ODF converter maps
   them to nothing.
2. **No RTL tables** — see the Tables section: `<w:bidiVisual>` is absent
   from the editor and destroyed by an editor round trip.

**Line breaking.** No UAX #14: per-character may-break-after flags computed
at insertion (spaces primary; break-after hyphen/em-dash; East-Asian
break-anywhere with kinsoku can't-start/can't-end tables; NBSP never;
pattern hyphenation adds soft points — none for Arabic, correctly). For
Arabic this yields space-only breaks, behaviorally correct. Refinements:
a line never breaks between a base and its marks, and the emergency path
for an over-wide word walks candidates backward and **re-shapes the
sub-range at each candidate** so both fragments get correct contextual
forms (the exported unsafe-to-break flags are not yet consulted — to-do).

**Version timeline (public DocumentServer changelog + release notes; the
machinery landed across 8.x, the polish across 9.0):** 7.2 (2022) fonts/
ligatures era begins (HarfBuzz shaping, Bengali/Sinhala improvements);
7.5 (2023) automatic hyphenation; 8.0 (early 2024) beta RTL UI + partial
bidirectional text, Arabic UI translation; 8.0.1 Arabic diacritic and
mixed-direction fixes; 8.1 (mid 2024) paragraph Text Direction control,
the neutral/weak-class algorithm, RTL paragraph fitting, more ligature
languages; 8.2 (late 2024) spreadsheet sheet-RTL, Arabic numbered-list
presets; 8.3 (early 2025) main-paragraph-direction control, RTL embedded
viewer, then heavy 8.3.x RTL bugfix waves (indents/ruler/tabs, caret in
mixed text, RTL lists, default-on Arabic standard ligatures, Arabic date
formats, Indic-digit autonumbering); 9.0 (mid 2025) the polish wave —
Arabic spellcheck (sixteen dialects), direction-aware arrow navigation,
RTL numbering previews, RTL TextArt/borders/fill, Hindi digits option,
spreadsheet bidi display, PDF-editor RTL; 9.1 per-cell text direction
(spreadsheet); 9.2–9.4 (through Aug 2026) maintenance only. A user's
"improved dramatically since version 9" matches when it stopped feeling
broken; the machinery is 8.x.

**Comparative note:** our full per-line UAX #9, kashida bands, and
tatweel-glyph justification already exceed them on justification and bidi
correctness. Where they are ahead of a naive implementation — worth
adopting as behaviors from spec sources, not their code — is the
run-property-tolerant shaping segmentation, cluster-aware caret discipline,
the CS-slot property model, draw-time bracket mirroring, as-you-type caret
pinning, and the mirrored-space tab trick.

### Clipboard — full
Copy/cut/paste with multiple flavors: an internal binary format (lossless
within the suite, enabling cross-editor paste), generated HTML for external
targets, and plain text; paste imports external HTML (Word, Google Docs
output) through a large normalization pipeline; images and drawings
round-trip; paste special options surfaced by the shell. Verified gap: the
HTML pipeline (paste normalization and copy generation) has no handling of
dir attributes or CSS direction — RTL-ness does not survive HTML
interchange; only the internal binary flavor preserves it.

### Find / replace — full
Document-order search across body, headers/footers, footnotes, text boxes;
case and whole-word options; pattern support for special characters;
highlight-all with viewport-aware rendering; replace/replace-all built on
the same ranged-match model that survives relayout.

### Spellcheck — full
Hunspell compiled to WASM in a dedicated worker; per-language dictionaries
loaded on demand (Arabic dictionary covering sixteen dialects since v9.0);
per-paragraph incremental checking (only dirty paragraphs are re-collected
and re-checked, batched on idle); wavy underlines painted by the canvas
layer; suggestions in context menu; user dictionary.

### Collaboration — full (server-dependent)
Change-based synchronization through the document server over WebSocket.
Two modes: **strict** (users take object-level locks — paragraph, drawing,
header — and merge on explicit save) and **fast** (real-time streaming of
change objects, the default). Every model object carries a globally unique
id in a registry; changes address objects by id, so remote changes apply
without positional transforms; content-position bookkeeping handles
same-object concurrent edits. Foreign cursors/selections are painted with
user labels. Collaborative undo rewinds only your own changes by inverting
them and fixing up subsequent content positions; a "deleted text recovery"
subsystem lets undo restore text another user deleted. Version history
navigates saved server versions and replays change points to show
differences with per-user coloring.

### Document protection / permissions — full
Password-based document protection modes (read-only, comments-only,
forms-only, tracked-changes-forced) matching OOXML settings; ranged edit
permissions (permission-range markers in the flow) so specific users/groups
may edit only inside granted ranges; content-control locking (can't delete /
can't edit content).

### Compare / combine — full
Word-style document comparison (a diff over paragraphs/runs producing
tracked insert/delete/move changes attributed to the compared author) and
document combining that merges two change sets. Runs client-side on two
loaded models.

### Plugins / macros — full
An iframe-sandboxed plugin framework with a published API (insert content,
read selection, add UI panels/buttons, OLE plugin objects); JS macros with a
document-object-model-like scripting API (shared with the document-builder
product) and a macro recorder that captures API calls; plugins and macros
are suite-wide.

### Accessibility — partial (limited); rewritten for v2
v1 called this a "self-voicing speech-synthesis layer" — wrong on
mechanism: sdkjs never calls the Web Speech API and cannot self-voice.
What exists is a **screen-reader announcement bridge**: when enabled, a
visually-hidden DOM element configured as an assertive, atomic ARIA live
region is attached beside the hidden input area; editor events are diffed
from selection-state snapshots into short localized text descriptions and
written into that element, which the user's **own screen reader** voices
(per-platform repeated-announcement workarounds are handled). Coverage:
typed characters, caret moves by char/word, selection grow/shrink,
story-part transitions (footnote/header-footer/drawing/main),
line/document start-end, page up/down, drawing selection with alt text,
slide selection, and cell/range/sheet navigation with values — suite-wide,
with per-editor description generators. Toggled via a public API hook
(called by the outer web-apps shell) and an in-editor shortcut (public
docs: Ctrl+Alt+Z). Public release notes: landed as experimental ~v7.3
(early 2023), default-on ~v8.2 (late 2024).

The absence half of v1 stands: beyond that single live region there is
**no accessibility tree for document content** — no DOM/ARIA mirror of the
canvas document, no ARIA on the hidden input, no DOM text layer in the
sdkjs PDF viewer, no platform-AT bridges — so assistive technology cannot
independently navigate or re-read document structure; it only receives
event-driven announcements. High-contrast support is theme-level. The C++
PDF writer can emit a tagged-PDF scaffold (marked flag + structure-tree
root) but no populated content structure. This remains markedly weaker
than a DOM-overlay a11y tree and is a competitive weakness worth noting.

### Print / PDF export — full
Painting is backend-abstracted: the same draw calls target the screen
canvas or a **recording backend** that captures a compact command stream
("metafile"); print/PDF export replays that stream — client-side into a
printable representation, or server-side where the headless renderer runs
the same engine to emit final PDF (with font embedding handled by the C++
side). Selection-only and current-page printing supported. PDF forms
export flows through the same path with form-field annotations.

### Other notable breadth
- **Bookmarks & cross-references** — full (REF/PAGEREF/NOTEREF + UI).
- **Hyperlinks** — full, incl. heading anchors.
- **Document outline / navigation panel** — full (heading tree maintained
  incrementally during recalculation).
- **Autocorrect** — full: as-you-type (capitalization, hyperlink
  detection, fraction/ordinal substitution, smart quotes, bulleted/numbered
  list detection incl. Arabic-Indic digits), math autocorrect, first-letter
  exception lists.
- **Custom XML parts** — stored and round-tripped; content-control data
  binding appears limited — partial.
- **Watermarks** — full (header-hosted shape with editing UI).
- **Line numbering** — full.
- **Drop caps** — full (via paragraph frames).
- **Text effects (WordArt)** — partial (geometry-warped text supported;
  some effect types simplified).
- **Encrypted documents** — password open/save handled by the C++ core
  (agile/standard OOXML encryption) — full at the conversion layer.
- **Legacy formats** — .doc, .rtf, .odt, .txt, .html, .epub, .fb2, and
  more via the server conversion hub — full (conversion-level).
- **PDF viewer/editor** — a separate editor in the same repo (out of scope
  here, but shares the drawing/font stack).

---

## 3. Architecture map

### 3.1 The two repos

- **sdkjs (JS)** — everything interactive: document model, editing,
  layout, painting, input, collaboration client, field/TOC logic, review,
  forms, search, spellcheck driver, clipboard, plugin host, API facade
  consumed by the web UI shell (a separate repo builds the toolbar/dialog
  chrome).
- **core (C++)** — everything file-format: OOXML DOM read/write libraries
  (docx/xlsx/pptx and flat variants), the internal-binary serializers, the
  x2t conversion orchestrator, legacy format converters (.doc binary, RTF,
  ODF, HTML, EPUB…), zip handling, crypto (password-protected files), the
  font engine (FreeType + HarfBuzz; also compiled to the WASM module sdkjs
  loads — HarfBuzz fetched from upstream at a pinned commit at build time),
  font directory scanning/name resolution, raster/vector graphics
  backends, PDF read/write, and the headless V8-embedding renderer that
  runs sdkjs server-side (document builder, PDF generation, thumbnails).

### 3.2 sdkjs word-processor subsystems (concept level)

1. **Model layer** (`word/Editor/`): a tree — document → top-level elements
   (paragraphs, tables, block content controls) → runs and inline items.
   Paragraph text is stored as run arrays of typed items (text, space, tab,
   break, field chars, references, separators as distinct item kinds).
   Every object registers in a global id table. All mutation goes through
   change objects (see undo/collab). Sections are properties attached to
   paragraphs (mirroring `<w:sectPr>` placement), aggregated into a
   section-info index.
2. **Style/property resolution**: direct formatting over style chain over
   document defaults, compiled lazily into flattened property objects,
   cached per node and invalidated by change tracking; formatting objects
   are interned (ref-counted, keyed by their serialized bytes) to cut
   memory.
3. **Layout/recalculation** (the heart): per-element layout results are
   stored on the elements themselves as arrays of per-page fragments; the
   document keeps a page array with element start indices. Recalculation is
   demand-driven from the change log (see §4.1–4.4). Line model: a line is
   a set of horizontal ranges (segments), which unifies columns and
   float-wrap cutouts. Floating objects register wrap intervals; tight/
   through wrap uses per-shape wrap polygons.
4. **Text pipeline**: codepoint buffers → script/direction/font
   itemization → shaping in the WASM font engine (HarfBuzz) → interned
   grapheme clusters with advances → line breaking against range widths
   (with hyphenation hook) → BiDi reordering at line assembly → painting.
   Shaping is done once per font at a reference size and scaled (see §4.8).
5. **Painting**: an abstract 2D graphics interface in document units (mm)
   with implementations for screen canvas, the recording metafile backend
   (print/PDF), thumbnail generation, and native (desktop) surfaces. Page
   bitmaps are cached with a lock/reuse counter; an overlay canvas draws
   selections, carets, foreign cursors, and tracking adorners without
   repainting pages.
6. **Input**: hidden textarea/contenteditable captures keystrokes and IME
   composition (composition text is inserted provisionally and finalized on
   composition end); pointer events hit-test through the layout tree.
   Focus routing uses a controller pattern: the document delegates
   editing/selection operations to whichever content controller is active
   (main body, header/footer, footnote, endnote, drawing text, form).
7. **Undo/history**: a change log of typed, invertible change objects
   grouped into user-action points; description-only vs content changes are
   distinguished; points can be merged (typing runs) or marked temporary
   (composition). The same change objects are the collaboration wire format
   and drive recalculation dirtying — one representation, three consumers.
8. **Collaboration client**: serializes change points to a binary stream,
   ships via the coauthoring WebSocket API, applies foreign changes by
   object id, manages locks in strict mode, maintains a synced-index into
   the shared change list, and implements collaborative undo (see §2
   Collaboration).
9. **Serialization**: reader/writer for the internal binary snapshot
   (shared format with the C++ side, versioned); a JSON round-trip layer
   for the plugin/builder APIs; HTML/text emitters for clipboard.
10. **API facade**: a very large method surface consumed by the web UI
    shell; also the scripting/document-builder API — same model, different
    entry points.

### 3.3 Data flow in/out

- **Open**: shell asks document server → server runs x2t: `.docx` →
  OOXML DOM (C++) → internal binary + media files → browser downloads →
  JS deserializer builds the model → fonts load (font list + files served;
  measurements via WASM font engine) → recalculation from page 0 (timer-
  sliced) → pages paint as they become ready.
- **Save**: editor serializes model → binary uploaded → x2t: binary →
  OOXML DOM → `.docx`. Autosave streams change points instead of full
  snapshots; the server can rebuild the latest state as base-snapshot +
  changes (this is also the crash-recovery story).
- **Collaboration**: change points broadcast through the server to peers;
  the server persists the change list; joining clients get snapshot +
  pending changes.
- **Print/PDF**: paint into the recording backend → command stream →
  (client) print pipeline or (server) headless renderer → PDF with
  embedded fonts.

---

## 4. Methods worth learning

Prose descriptions of mechanisms observed; no implementation detail beyond
the idea.

1. **Three-tier incremental relayout.** Tier 1: if all pending changes
   touch a single run and the reshaped/re-measured content still fits the
   same line-range region with unchanged bounds, patch geometry in place
   and repaint one page. Tier 2: if changes are confined to one paragraph,
   re-lay only that paragraph; accept the result only if its page count,
   outer bounds, and end-state match the previous layout (this tier is
   limited to paragraphs spanning few pages — beyond that it isn't worth
   it). Tier 3: full reflow from a computed start page.
   The fast tiers are *verified optimistically* — they run the real layout
   and compare invariants, falling back to tier 3 on any mismatch, so
   correctness never depends on predicting whether the fast path is safe.
   (Verified nuance: the fast tiers skip footnote renegotiation and mark
   renumbering entirely — the verification/demotion contract is what keeps
   footnote-affecting edits correct.)

2. **The undo log doubles as the dirty tracker.** Every mutation creates a
   typed change object that knows its owning element; recalculation reads
   the list of not-yet-processed changes and derives the minimal restart
   point. One representation feeds undo/redo, collaboration sync, *and*
   layout invalidation — they never disagree about what changed.

3. **Restart-point computation folds document structure in.** The restart
   element index is the minimum over: earliest inline content change;
   earliest section whose header/footer changed (mapped to the first page
   that *uses* that header, walking the section list to respect
   inheritance and title-page flags); then backed up over preceding
   keep-with-next paragraphs (with a bounded lookback) so keep-chains
   re-flow together. Restart begins at the page containing that element,
   not at the element itself.

4. **Timer-sliced, interruptible, resumable pagination.** Full reflow is a
   loop over pages; after a small synchronous head start (so the visible
   area updates immediately), it reschedules itself on short timers,
   repainting each page as it completes. A new edit
   during a long reflow cancels the pending timer and restarts from the
   earlier of (old restart, new restart). Pages already flowed remain
   valid because per-element layout fragments persist. A generation
   counter invalidates stale loops.

5. **Per-element per-page layout fragments.** Each paragraph/table stores
   its own array of page fragments (lines with metrics for paragraphs; row
   spans for tables), each fragment knowing its start position within the
   element. The document page array only stores element start indices and
   frames. Reflow of page N reuses fragments of elements that start before
   N untouched. This is the data structure that makes both resumable
   pagination and fast tiers possible.

6. **Speculative placement with save/restore.** Elements can snapshot
   their layout state cheaply; pagination "tries" placements (a table row
   on the current page, a paragraph under a footnote reservation) and
   rolls back to the snapshot when a downstream rule (footnote space,
   keep rules, orphan control) rejects the placement. Explicit result
   flags from element layout (continue-next-element, move-to-next-page,
   restart-current-page…) drive the page loop's control flow. Verified
   extension: table row layout snapshots the footnote controller's
   per-column state per row and rolls it back when a row moves pages.

7. **Footnote space negotiation per line.** As each body line is placed,
   footnote references collected on that line negotiate height at the page
   bottom (per page, per column — the controller keeps one slot per body
   column): new notes lay into the space between the column's current
   footnote-block height and (column bottom minus line bottom); success
   shrinks the body's Y limit for later lines. Refusal — only when the
   first note of the batch cannot place even one line — moves the body
   line to the next column/page (a page's first line is exempt unless
   footnotes already occupy the column). A partially fitting note keeps
   its placed part; the remainder carries over to the next column/page,
   prefixed by the continuation-separator story (first non-carried notes
   get the separator story). Both separator stories are full note-content
   objects re-laid per column. The whole column block is position-shifted
   in a post-step (page-bottom default vs beneath-text). Endnotes register
   during layout but reserve no space; they flow as a trailing story after
   the body at section/document end.

8. **Shape once at a reference size, scale everywhere.** All shaping and
   glyph metrics are computed at one large fixed font size and scaled
   linearly to the run's actual size (and zoom). One shaping result
   serves any size; caches never fragment by size. (Trade-off: ignores
   size-specific hinting — acceptable for print-metric fidelity.)

9. **Global grapheme interning.** A shaped cluster (font, glyph ids,
   advances, offsets) is registered once in a global table and referenced
   by integer id; run items store grapheme id + scaled width. Measurement,
   painting, and PDF export all consume interned graphemes. Identical text
   in identical fonts costs shaping once per document session.

10. **Streaming pragmatic BiDi — one reorderer, many replays.** Instead of
    full UAX #9 with embedding levels, layout feeds visual-run assembly
    through a small streaming reorderer: strong-RTL, strong-LTR, and
    neutral/weak buffers with flush rules (neutrals to the paragraph base
    direction, numbers grouped LTR inside RTL with separators absorbed
    only between digits, tab/break/pilcrow strong in the base direction).
    Classification data is full UAX #9 (generated from Unicode data,
    explicit/isolate codes included) but the explicit codes are never
    interpreted, and bracket-pair data serves only draw-time glyph
    mirroring — not rule N0. The same reorderer is replayed by layout,
    painting, decorations, selection rects, caret geometry, hit-testing,
    and list numbers, so interaction and pixels agree by construction.
    Simpler than the full algorithm, covers the common cases; ours is more
    correct, theirs is cheaper and streaming — but the single-reorderer
    replay discipline is the idea worth keeping.

11. **Interned, compiled formatting.** Effective properties (defaults →
    style chain → direct) are compiled lazily and cached; compiled and
    stored property objects are deduplicated through a ref-counted intern
    table keyed by their serialized bytes. Large documents with repetitive
    formatting pay near-zero marginal memory per run.

12. **Fields as sentinel triples + typed instructions.** Complex fields
    live in the run stream as begin/separate/end sentinel items (matching
    `<w:fldChar>`); a parser turns instruction text into typed field
    objects (unknown keywords fall back to implicit-REF parsing); a field
    stack rebuilt during layout tracks nesting. Page-dependent values
    (PAGE) paint from the layout position; aggregate values (NUMPAGES) get
    a dedicated post-pass: after pagination stabilizes, headers/footers
    containing page-count items re-lay with the final count (each header
    keeps a registry of page-count-dependent elements populated during
    layout). TOC update is a field update that regenerates its result
    content between separate and end. Simple fields are a third, distinct
    inline-wrapper representation (see §2 Fields).

13. **Object-id-addressed changes.** Every model object has a session-
    unique id in a global registry; every change object addresses its
    target by id, not by path. Remote changes and undo apply without
    positional transformation; only same-container content arrays need
    index bookkeeping. This is the load-bearing idea of their entire
    collaboration and undo architecture.

14. **Collaborative undo by inversion + fixup.** Undo in a live session
    inverts your own most recent change point and adjusts content-array
    positions of your inverted changes against foreign changes that
    arrived after them; a recovery subsystem can resurrect text deleted
    by another user. Version history reuses the same machinery: replay
    the change stream to any point, coloring changes by author.

15. **Versioned binary snapshot with documented compat bumps.** The
    internal binary format carries a version; each historical bump is
    annotated with its migration rule (e.g., "older files assume X for
    this then-missing property"). Readers apply per-version defaults, so
    old snapshots (and old autosave change streams) stay loadable.
    (Counterpoint observed: the same wire format is where `<w:bidiVisual>`
    dies — the C++ writer defines a record the JS reader lacks, and
    unknown-record skipping silently drops it.)

16. **Layout policies as swappable strategy objects.** Print view vs
    reflowable read view (custom page width, no headers/footers) are
    interchangeable layout-policy objects over the same model — pagination
    logic queries the policy for page frames instead of hardcoding
    geometry. Cheap to add "web view" style modes later.

17. **Backend-abstracted painting with a recording backend.** All drawing
    goes through an abstract mm-unit 2D interface; one implementation
    records the command stream instead of rasterizing. Print, PDF export,
    and server-side rendering replay the recording — pixel pipeline and
    export pipeline can never disagree about layout. (Analogous to our
    DisplayList, but their recording is also the wire format to the
    server.)

18. **Page bitmap cache.** Rendered page images are cached with a lock
    flag and an unused counter; scrolling re-blits cached bitmaps and
    only repaints dirty pages; the cache evicts by unused count. Overlay
    (selection/caret/collab cursors) is a separate canvas so overlay
    changes never invalidate page pixels.

19. **Table layout in two phases (amended).** Phase 1 computes the grid:
    a declared-grid reconciliation pass that always runs (declared
    `<w:tblGrid>` widths widened by explicit cell widths and before/after
    row offsets via a monotonic walk, then proportional rescale to any
    declared table width) — this alone defines fixed-layout tables; a
    content-based AutoFit pass (min-content / max-content / preferred per
    column, explicit widths replacing max-content, a min-ignoring-preferred
    tier for overflow, order-dependent multi-span distribution) runs only
    when the top-level table's mode is autofit, distributing per the CSS2
    auto-table idea extended with preferred-width tiers. Phase 2 flows rows
    into page fragments: repeated header rows re-inserted per page from
    clones (top-level inline tables only), vertical-merge groups treated as
    units laid at the group's final row (retroactively growing earlier
    rows), row split across pages by splitting each cell's content with
    per-cell continuation state — a row splits only when every
    participating cell places something, and the can't-split row flag is
    transported but never consulted. Borders: explicit-cell-beats-table
    then the ECMA-376 width/brightness comparison in the collapsed model;
    no conflict pass in spacing tables; resolved per grid segment and fed
    into geometry before content layout.

20. **Incremental ancillary indexes.** Heading outline, bookmark table,
    form registry, merge-field registry, and the review-change navigator
    are maintained as side indexes updated during recalculation or by
    change-type triggers — not recomputed by document scans — so
    navigation panels stay live in large documents.

21. **As-you-type spell/hyphenation batching.** Spellcheck and
    hyphenation run per paragraph, only for paragraphs marked dirty by
    the change stream, batched behind idle timers, with results cached on
    the paragraph until the next edit that touches it.

22. **Shaping-context equality by shaping-relevant properties only.** The
    "can these two runs shape as one word?" comparison checks only the
    properties that affect shaping (faces, sizes and their complex-script
    twins, spacing, small caps, position, CS/RTL flags, vanish, ligature
    setting, the relevant font slot) — color, underline, highlight, and
    language changes mid-word do not break Arabic joining. Cheap to state,
    easy to get wrong by comparing whole property objects.

23. **Cluster-aware caret discipline with even width split.** Multi-
    codepoint clusters attach their drawn grapheme to one end item and mark
    the rest as continuations (base / ligature / ligature-continuation /
    combining-mark type tags); the cluster's width divides evenly across
    its codepoints. Hit-testing and arrows snap out of base+mark interiors;
    marks delete with their base; every codepoint keeps a selection rect
    and an (arbitrary but stable) caret slot inside ligatures.

24. **Mirrored-space tab resolution.** For RTL paragraphs the pen X is
    reflected into a coordinate space measured from the right edge, tab
    stops resolve with the ordinary LTR algorithm there, and the result
    reflects back — one tab implementation serves both directions,
    including the margin-clamp quirks.

25. **Emergency line breaks re-shape fragments.** When a word exceeds the
    line, candidate break positions are walked backward and the sub-range
    is re-shaped at each candidate so both fragments carry correct
    contextual forms (Arabic joining stays right across an emergency
    break); mark items are never separated from their bases.

---

## 5. Stability mechanisms (observed)

- **Interruptible everything.** Long pagination is sliced and cancellable
  (§4.4); spellcheck and TOC updates batch on idle; the editor stays
  responsive during full-document reflow of very large files.
- **Optimistic fast paths verified, not trusted.** Fast relayout tiers
  self-check bounds and end-state and demote to full reflow on mismatch —
  a stale-layout bug becomes a performance blip, not corruption.
- **Change stream as recovery log.** Autosave persists change points to
  the server continuously; a crashed/disconnected session recovers as
  base snapshot + replayed changes. Reconnection logic re-syncs from the
  synced-index; unsent local changes are bounded and reported.
- **Locks and a global lock gate.** In strict collaboration, edits are
  gated by lock acquisition callbacks; a global lock freezes the model
  during critical async phases (image upload, save) to prevent interleaved
  mutation.
- **Format versioning with per-version defaults** (§4.15) instead of
  migration tools — old documents and old change streams load in new
  clients.
- **Enumerated error codes** surfaced through API events to the shell
  (open failure, conversion error, save error, VKey/rights errors), with
  the shell deciding on reload/limited-mode; conversion errors on the
  server fail the job rather than emitting broken documents.
- **Undo-point hygiene.** Temporary points for IME composition (dropped or
  merged on commit), grouped points for compound actions, and a
  description/content change distinction that keeps no-op changes from
  dirtying layout.
- **Encrypted/protected files** handled at the conversion boundary (C++
  crypt reader), so the editor model never sees undecryptable bytes.

---

## 6. Contrast with our architecture (orientation only)

| Topic | ONLYOFFICE | Us |
|---|---|---|
| Layout language | JS (single thread + workers for fonts/spell) | Rust→WASM in a worker |
| Rendering | Canvas 2D, mm units, page bitmap cache | Canvas 2D/Vello, DisplayList, device px |
| Shaping | Stock HarfBuzz WASM (pinned upstream), reference-size scaling, grapheme interning | rustybuzz per-size |
| BiDi | Streaming subset: full class tables, no embedding levels, explicit codes inert, bracket mirroring at draw time only | Full UAX #9 per line |
| Docx fidelity | Full regeneration via internal binary (bidiVisual and kashida jc destroyed on round trip) | Byte-preserving siblings |
| Undo | Invertible change objects (also collab wire format) | Immutable snapshots |
| Collab | Object-id-addressed change streaming + locks | (future) |
| A11y | Single ARIA live-region announcement bridge; no doc a11y tree | DOM overlay tree |
| Kashida | Absent (import degrades kashida jc to left alignment) | Present (P1–P5 bands, tatweel glyph) |
| RTL tables | Absent (property lost on editor round trip) | (check our status) |
| Auto paragraph direction | Absent (explicit toggle only) | Present (first-strong) |

Their deepest, hardest-won machinery — the three-tier relayout,
change-log-driven dirtying, footnote negotiation, and table AutoFit /
row-splitting — maps directly onto problems still ahead of us and is
indexed for targeted future dives in `onlyoffice.yaml`.

---

## 7. Revision notes (v2)

Each v1 statement that changed, and why (all changes verified by
independent Reader passes over the current trees; refutation attempts run
with recorded search terms and hit counts).

1. **Accessibility (§2) — v1's "self-voicing speech-synthesis layer" was
   wrong on mechanism.** The Web Speech API appears nowhere in sdkjs. The
   layer is an ARIA live-region announcement bridge voiced by the user's
   own screen reader — a minimal but real native-AT integration point,
   suite-wide, toggleable (shell hook + Ctrl+Alt+Z). The "no accessibility
   tree for document content" half of the claim was confirmed. Section
   rewritten; contrast-table row updated.

2. **Kashida (§2 RTL) — confirmed, but v1 understated the damage.**
   "Justified Arabic stretches spaces only" stands (zero kashida/tatweel
   machinery in either tree; the per-letter distribution channel is a CJK
   accommodation not offered to Arabic). New: the OOXML kashida `w:jc`
   values fall through the docx→internal-binary conversion's default
   branch to **left alignment** — kashida-justified documents open
   start-aligned, not even justified, and lose the setting on save;
   distribute/thaiDistribute degrade to plain justify.

3. **RTL tables (§2 Tables) — confirmed and extended.** v1 said
   `bidiVisual` mirroring is "not implemented"; verified reality is
   stronger: the JS word editor neither models, deserializes, lays out,
   nor draws it, and because the editor's binary vocabulary lacks the
   record the C++ side defines, the property is **silently dropped on any
   editor open/save cycle**. The C++ conversion core fully round-trips it
   (docx DOM, .doc/RTF/ODF-import mappings, revision merge).

4. **BiDi depth (§2, §4.10) — confirmed as a pragmatic subset, mechanism
   corrected.** v1 implied bracket data participates in reordering; it is
   consumed only for draw-time mirrored-glyph substitution (not UAX #9
   rule N0), and non-bracket Bidi_Mirrored characters are never mirrored.
   New precision: classification tables are full UAX #9 (explicit and
   isolate codes included) but those codes have zero consumers — no
   embedding-level stack exists. The vendored ICU in the C++ tree contains
   a full UAX #9 engine that nothing calls; the XPS importer applies
   file-stored levels without computing any.

5. **RTL/CTL section — rewritten and expanded** with verified shaping
   details (five-feature model with everything else on HarfBuzz defaults;
   liga force-on for Arabic/Syriac; kerning off everywhere; language
   hard-coded to English so `locl` never fires; letter-spacing kills
   ligatures), font-slot/fallback mechanics (no Arabic-aware fallback
   priority), editing behavior (logical arrows with paragraph-level key
   flip; as-you-type caret pinning; cluster discipline), mirrored geometry
   inventory (tabs via reflected space, trailing-space overhang, mirrored
   formatting marks/ruler/click-and-type), no first-strong auto-direction,
   typing never stamps run `<w:rtl>` (in-tree to-do), the U+060C–U+074A
   force-Arabic workaround that mis-tags Syriac, numeral display setting
   (western/hindi; "context" rejected), HTML clipboard losing RTL-ness,
   and section-level `<w:bidi>` column order being conversion-only.

6. **Version timeline added (public changelog, fetched 2026-08-22):** RTL
   machinery landed across 8.0–8.3 (beta RTL UI + partial bidi 8.0;
   paragraph direction control + neutral/weak algorithm 8.1; sheet RTL
   8.2; main direction control 8.3 + heavy 8.3.x fix waves), polish in
   9.0 (Arabic spellcheck, direction-aware arrows, RTL TextArt/borders,
   Hindi digits, spreadsheet bidi display), maintenance-only 9.2–9.4.
   Screen-reader support: experimental ~v7.3, default-on ~v8.2.

7. **Fields (§2) — three corrections.** ASK: typed instruction exists but
   the keyword dispatch matches a misspelled token, so real ASK fields
   parse as REF (live bug). FORMDROPDOWN: never produced by the parser
   (data payload round-trips; runtime treats the instruction as REF).
   Simple fields: not "converted to the complex representation" — they
   load into a distinct third inline-wrapper representation, with
   PAGE/NUMPAGES special-cased to dedicated run items, header/footer
   PAGE/NUMPAGES wrappers converted to complex on open, and untypeable
   simple fields unwrapped. Added: unknown-keyword→REF fallback, the
   exhaustive 18-function formula library, the exact TOC switch gaps
   (\b, \d, \f, \l, \s, \z, \u unparsed — no TC-entry TOCs), and the
   field-update roster.

8. **Footnotes (§2, §4.7) — one correction, several refinements.** The
   continuation *notice* story is model-only (never laid out or drawn) —
   v1 listed it as working. Refinements: per-page/per-column negotiation
   mechanics, refusal-only-on-first-note rule, first-line exemption
   dropped when footnotes occupy the column, the position-value Word
   quirk, custom-start-only-in-continuous-mode nuance, mark formats
   limited to decimal/roman/letter (Chicago falls back to decimal), table
   row snapshot/rollback of footnote state, and fast tiers skipping
   renegotiation (correctness via verify-then-demote).

9. **Tables (§2, §4.19) — one refuted claim, several amendments.**
   Refuted: row splitting does **not** honor per-row "can't split" — the
   property is transported but never consulted, and no UI sets it.
   Amended: border resolution is explicit-cell-beats-table then the
   ECMA-376 width/brightness comparison (no "row" precedence — OOXML has
   no row borders), and spacing tables have no conflict pass at all; the
   declared-grid pass always runs while AutoFit is conditional with the
   top-level ancestor's mode governing nested tables; explicit cell widths
   replace max-content; distribution is order-dependent with a
   min-ignoring-preferred overflow tier and the 22-inch cap; header
   repetition qualifiers (inline top-level only, merge-crossing rows
   trimmed, skip-when-too-tall, per-page clones).

10. **Ligature control (§2 Text) — deepened.** v1's "per-run setting for
    which ligature classes to apply" holds, plus: all five exposed
    features explicitly zeroed when unset (kerning off everywhere, clig
    off by default), standard ligatures force-on for Arabic/Syriac,
    letter-spacing disables ligatures, `w14:cntxtAlts` round-trips but is
    never applied, shaping language hard-coded.

11. **Font fallback (§2 RTL) — made precise.** v1's "font fallback per
    script" is really three mechanisms: the OOXML font-slot classifier
    (Arabic ranges in the ascii slot unless the CS/RTL run flag routes to
    the cs slot), per-codepoint cmap-coverage substitution among loaded
    fonts, and a server-generated range→font table with a hand-ordered
    priority list containing no dedicated Arabic font. Script itemization
    drives shaping segmentation, not font choice.

12. **Methods added (§4.22–4.25):** shaping-context equality by
    shaping-relevant properties only; cluster-aware caret discipline with
    even width split; mirrored-space tab resolution; emergency line breaks
    that re-shape fragments. §4.10 reframed around the "one reorderer,
    replayed by every consumer" consistency-by-construction pattern.

13. **Lists (§2) — added** Arabic alphabetic/abjad and Hebrew number
    generation (real), the unshaped codepoint-by-codepoint number
    painting caveat, and Arabic-Indic digit list autoformat.

14. **Provenance updated** to develop-branch checkouts of 2026-05-26
    (sdkjs 75a9683e…, core acdc1e39…), studied 2026-08-22. Both are
    shallow clones; in-tree changelogs are stale, so all version facts
    are sourced from the public DocumentServer changelog.
