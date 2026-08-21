# ONLYOFFICE — sanitized product & architecture reference

**Clean-room status.** Produced by an isolated Reader agent per
`plans/cleanroom/PROTOCOL.md`. Contains feature inventories, user-visible
behavior, subsystem-level architecture, and methods described as prose only.
No source code, no internal identifiers, no constant tables crossed the wall.
Repo-relative paths are pointers for future Reader dives, not implementation
references. The machine-readable index lives in
`plans/cleanroom/onlyoffice.yaml` (provenance block included there).

**Sources studied (read-only):**

- `onlyoffice-sdkjs` — the JavaScript editor engine (AGPL-3.0). The word
  processor lives in `word/`, shared machinery in `common/`.
- `onlyoffice-core` — the C++ server core (AGPL-3.0): format conversion
  (x2t), OOXML read/write libraries, font machinery.

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
binary) at the price of silent loss for exotic content.

---

## 2. Feature inventory

Maturity is judged from what is visibly implemented in the tree: **full**
(complete, mature subsystem), **partial** (present with visible gaps),
**absent** (no meaningful implementation found).

### Text / run formatting — full
Bold/italic/underline/strikethrough/double-strike, sub/superscript, caps and
small caps, highlight, character shading, font color incl. theme colors,
character spacing, ligature control (per-run setting for which ligature
classes to apply), vertical position. Faces are real (family/style resolved
through the font manager with fallback), not synthesized unless the face is
missing.

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

### Tables — full
Table grid model with declared column grid reconciled against actual cell
spans; fixed and AutoFit layout algorithms (min/max content widths per
column, percent and fixed and auto width resolution); horizontal and
vertical merges; nested tables; repeated header rows; row splitting across
pages (honoring per-row "can't split" and vertical-merge groups); floating
(positioned) tables with text wrap; table styles with conditional formatting
bands (first/last row/column, banding); cell text direction (vertical text)
and per-cell margins/borders/shading with border conflict resolution.
Notable gap: **no RTL table support** (`bidiVisual` visual column
mirroring not implemented in the word editor).

### Sections / page layout — full
Section breaks (next page, continuous, even, odd, column); per-section page
size, orientation, margins, gutter (incl. RTL gutter position); multiple
columns with unequal widths and column breaks; title-page flag; page
borders (with display options); line numbering per section; per-section
footnote/endnote properties; section-aware vertical text alignment.

### Headers / footers — full
First/even/odd variants, link-to-previous inheritance as slot absence,
different-first-page per section, watermark discovery/editing inside the
header, header/footer content is a full document content object (tables,
images, fields all work). Growth of a header pushes body content down;
page-count-dependent content triggers a dedicated re-layout pass.

### Fields / TOC — full (core set), partial (breadth)
Complex fields use the OOXML three-character model (`w:fldChar`
begin/separate/end) with an instruction-text parser producing typed field
objects. Implemented instructions include (alphabetically) ADDIN, ASK,
DATE, FORMCHECKBOX, FORMDROPDOWN, FORMTEXT, HYPERLINK, MERGEFIELD,
NOTEREF, NUMPAGES, PAGE, PAGEREF, REF, SEQ, STYLEREF, TIME, TOC, and `=`
expression formulas
(a real expression parser with a function library: SUM, AVERAGE, IF, MOD,
ROUND, etc., with table cell-reference arguments). TOC insertion/update is
complete (heading-style collection, page numbers, hyperlinks, leader tabs,
outline levels); table-of-figures via SEQ. Word's long tail of other field
types is not modeled. Simple fields (`w:fldSimple`) are supported and
converted to the complex representation internally.

### Footnotes / endnotes — full
Reference marks with auto-numbering (per-page/per-section restart, custom
marks, number formats), layout negotiation that reserves page-bottom space
line-by-line (see §4.7), footnotes continuing across pages/columns with
continuation separator and continuation notice special stories, endnotes at
section/document end, footnote position options (page bottom vs below
text). Column-aware: footnotes land under the column that references them.

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
core.

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

### RTL / CTL & i18n — substantial but younger; one visible gap: kashida
Complex-script shaping (Arabic joining, marks, clusters) is native through
HarfBuzz-in-WASM; script itemization and font fallback per script; a
streaming BiDi reordering pass at layout time (strong/weak/neutral classes
with bracket data derived from Unicode tables — a pragmatic subset of UAX #9
rather than the full algorithm); paragraph-level RTL with mirrored indents,
alignment, tabs, and numbering; RTL section gutter; RTL-aware caret
movement and selection. **No kashida/tatweel justification** (justified
Arabic stretches spaces only), and **no RTL tables** (see above). Hebrew /
Arabic UI localization handled by the outer shell.

### Clipboard — full
Copy/cut/paste with multiple flavors: an internal binary format (lossless
within the suite, enabling cross-editor paste), generated HTML for external
targets, and plain text; paste imports external HTML (Word, Google Docs
output) through a large normalization pipeline; images and drawings
round-trip; paste special options surfaced by the shell.

### Find / replace — full
Document-order search across body, headers/footers, footnotes, text boxes;
case and whole-word options; pattern support for special characters;
highlight-all with viewport-aware rendering; replace/replace-all built on
the same ranged-match model that survives relayout.

### Spellcheck — full
Hunspell compiled to WASM in a dedicated worker; per-language dictionaries
loaded on demand; per-paragraph incremental checking (only dirty paragraphs
are re-collected and re-checked, batched on idle); wavy underlines painted
by the canvas layer; suggestions in context menu; user dictionary.

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

### Accessibility — partial (limited)
No native DOM accessibility tree for the document (canvas rendering hides
content from AT by default). There is a self-voicing "speech worker" layer:
editor events (typed text, caret moves by char/word/line, selection
changes) are converted to spoken text via speech synthesis. High-contrast
support is theme-level. This is markedly weaker than a DOM-overlay a11y
tree and is a competitive weakness worth noting.

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
  list detection), math autocorrect, first-letter exception lists.
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
  loads), font directory scanning/name resolution, raster/vector graphics
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
   restart-current-page…) drive the page loop's control flow.

7. **Footnote space negotiation per line.** As each body line is placed,
   the footnotes it references attempt to reserve height at the page
   bottom (shrinking the body's Y limit). If reservation fails, that line
   moves to the next column/page (dragging the paragraph per keep rules).
   Footnotes too tall for the remaining page split and continue on the
   next page, introducing continuation-separator/notice stories. Endnotes
   register during layout and flow as a trailing story after the body.

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

10. **Streaming pragmatic BiDi.** Instead of full UAX #9 with explicit
    embedding levels, layout feeds visual-run assembly through a small
    streaming reorderer: strong-RTL, strong-LTR, and neutral/weak item
    buffers with flush rules (numbers grouped LTR inside RTL, paired
    brackets from Unicode data, paragraph direction as base). Simpler than
    the full algorithm, covers the common cases; worth knowing as a
    contrast to our full UAX #9 approach (ours is more correct; theirs is
    cheaper and streaming).

11. **Interned, compiled formatting.** Effective properties (defaults →
    style chain → direct) are compiled lazily and cached; compiled and
    stored property objects are deduplicated through a ref-counted intern
    table keyed by their serialized bytes. Large documents with repetitive
    formatting pay near-zero marginal memory per run.

12. **Fields as sentinel triples + typed instructions.** Complex fields
    live in the run stream as begin/separate/end sentinel items (matching
    `<w:fldChar>`); a parser turns instruction text into typed field
    objects; a field stack rebuilt during layout tracks nesting. Page-
    dependent values (PAGE) paint from the layout position; aggregate
    values (NUMPAGES) get a dedicated post-pass: after pagination
    stabilizes, headers/footers containing page-count items re-lay with
    the final count. TOC update is a field update that regenerates its
    result content between separate and end.

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

19. **Table layout in two phases.** Phase 1 normalizes the grid: declared
    `<w:tblGrid>` widths reconciled against actual cell spans and
    before/after row offsets, then min-content/max-content/preferred
    widths per column computed from cell content, then a distribution
    step resolves fixed/percent/auto to final column widths (scaling to
    the table's declared width when present). Phase 2 flows rows into
    page fragments: repeated header rows re-inserted per page, vertical-
    merge groups treated as units, row split across pages by splitting
    each cell's content with per-cell continuation state, borders
    resolved by a conflict pass (row/cell/table precedence).

20. **Incremental ancillary indexes.** Heading outline, bookmark table,
    form registry, merge-field registry, and the review-change navigator
    are maintained as side indexes updated during recalculation or by
    change-type triggers — not recomputed by document scans — so
    navigation panels stay live in large documents.

21. **As-you-type spell/hyphenation batching.** Spellcheck and
    hyphenation run per paragraph, only for paragraphs marked dirty by
    the change stream, batched behind idle timers, with results cached on
    the paragraph until the next edit that touches it.

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
| Shaping | HarfBuzz WASM, reference-size scaling, grapheme interning | rustybuzz per-size |
| BiDi | Pragmatic streaming subset | Full UAX #9 per line |
| Docx fidelity | Full regeneration via internal binary | Byte-preserving siblings |
| Undo | Invertible change objects (also collab wire format) | Immutable snapshots |
| Collab | Object-id-addressed change streaming + locks | (future) |
| A11y | Self-voicing speech layer | DOM overlay tree |
| Kashida | Absent | Present (P1–P5 bands) |
| RTL tables | Absent | (check our status) |

Their deepest, hardest-won machinery — the three-tier relayout,
change-log-driven dirtying, footnote negotiation, and table AutoFit /
row-splitting — maps directly onto problems still ahead of us and is
indexed for targeted future dives in `onlyoffice.yaml`.
