# LibreOffice Writer — sanitized product & architecture reference (v2)

**Clean-room status.** Produced by Reader agents per `plans/cleanroom/PROTOCOL.md`.
Contains no source code, no internal identifiers, no constant tables, no
special-case orderings. Directory/file paths are pointers for future Reader
dives only — Implementers must never open them. Provenance: LibreOffice core,
`https://github.com/LibreOffice/core.git`, commit
`3b30dd71049edf71a5c649c9aefa31538542c294` (master, 2026-08-21), MPL-2.0.
Studied 2026-08-22 (v1); v2 synthesis pass 2026-08-22 after an independent
verification round re-checked the kashida, RTL-geometry, and DOCX-pipeline
claims against the tree and two deep dives (CTL/Arabic end to end;
incremental relayout / footnotes / table splitting) were folded in. Changes
from v1 are listed in §7 "Revision notes (v2)".

**Audience.** The team building a Rust→WASM, canvas-rendered, paged word
processor with native RTL/Arabic support and byte-preserving `.docx`
round-trip.

---

## 1. Product overview — what Writer is architecturally

Writer is the word processor of LibreOffice; its core dates to ~1990 and
represents roughly 35 years of accumulated pagination knowledge. Three big
structural decisions organize everything:

### 1.1 Document model vs. layout model — a hard separation

- The **document model** is the persistent truth: a container of typed
  content nodes (paragraphs, tables, section boundaries, embedded-object
  placeholders) plus attribute pools, styles, numbering rules, fields,
  bookmarks, and redline (tracked-change) tables. It knows nothing about
  pages or lines.
- The **layout model** is a separate tree of "frames" — page, column,
  header, footer, footnote-container, footnote, body, floating-object,
  section, table, row, cell, and text-content frames. It is *derived* from
  the document model and is conceptually a cache: it can be discarded and
  rebuilt, and multiple views can hold layouts of the same document (with
  different display settings, e.g. show vs. hide tracked changes).
- One content node can be represented by a *chain* of layout frames when
  the content flows across pages/columns (master + follow frames). The
  reverse is also possible under hide-changes display: one layout frame
  can span *several* model paragraphs (see §4, method 13).

An interesting structural device in the model layer: content nodes live in
one flat array, with paired begin/end marker nodes encoding nesting (tables,
sections, header/footer text, footnote text). The array is partitioned into
a handful of fixed top-level regions — one for footnote text, one for
header/footer/floating-frame text, one for content deleted while change
tracking is on, and one for the visible body. So "off-page" text (a footnote
body, a header, a tracked deletion) is ordinary paragraph content stored in
a hidden region, edited by the same code paths as body text, and merely
*linked into* the visible flow by the layout. Tracked deletions are not
destroyed — they are moved to the hidden region, which is what makes
reject-change and undo cheap.

### 1.2 Formatting as pooled, inherited attribute sets

Every piece of formatting is an "item" (one typed property) held in
reference-counted pools; formatting states are item *sets* which chain to a
parent set. Style inheritance (character style → paragraph style → default)
and direct-formatting-over-style resolution both fall out of walking the
parent chain. Paragraph text carries a sorted array of attribute spans
("hints") over character ranges; span kinds include flat formatting spans,
properly-nested spans (hyperlinks, ruby, metadata ranges), and zero-length
"anchor" attributes that own a placeholder character in the text (fields,
footnote references, inline-anchored objects). This placeholder-character
technique keeps character indexing coherent: every inline object occupies
exactly one model character.

### 1.3 Rendering through device abstraction, not direct pixels

Layout computes geometry in document twips; painting walks the frame tree
and emits drawing operations against an abstract output device. The same
paint pass can target a window, record into a vector metafile (the print
and PDF path replays recorded metafiles into a PDF writer), or paint tiles
into caller-supplied pixel buffers (the "LibreOfficeKit" tiled-rendering
API that powers the browser-based Collabora Online — the closest analog to
our canvas approach). Text shaping is not done by Writer itself: the VCL
toolkit layer owns fonts, per-missing-glyph font fallback, bidi run
segmentation (ICU), and shaping (HarfBuzz on all platforms). Because the
justification decisions (including kashida positions) ride the draw call
into the device layer and are recorded into metafiles, screen, print, and
PDF output carry identical elongation and spacing — consistency by
construction (see §4A).

---

## 2. Feature inventory

Maturity: **full** = mature, decades of fixes; **partial** = works with
known gaps; **absent** = not present.

| Category | Summary | Maturity |
|---|---|---|
| Text/run formatting | Full character formatting: fonts, sizes, weight/posture, underline/strikethrough/overline variants, color + highlight, sub/superscript, letter spacing, kerning, case effects, hidden text, character borders/shading, OpenType feature selection via font-name syntax (with a feature-browsing dialog), character rotation, ruby text. | full |
| Paragraph formatting | Alignment (incl. block-justify with last-line options), indents, spacing, line spacing modes, tab stops with fill characters, borders/shading, hyphenation controls (Arabic-script languages, CJK, and Vietnamese are excluded from hyphenation by design — see §4A.5), widow/orphan, keep-with-next, break-before/after with page-style switch, drop caps, outline level, paragraph grid alignment (CJK). | full |
| Styles & inheritance | Character, paragraph, frame, page, list, and table styles; user + built-in; inheritance chains; conditional paragraph styles (style varies by context, e.g. in table vs. body); "autostyles" for direct formatting in ODF. Built-in styles carry a stable programmatic name distinct from the translated UI name. | full |
| Lists & numbering | Ten-level list styles; per-level number format, start value, prefix/suffix, alignment, position modes; list identity separate from list style (multiple lists can share a style); restart/continue controls; legal numbering; outline numbering tied to heading styles; per-node "is counted" toggle. Counting is maintained in a hierarchical numbering tree per list. Arabic alphabetic and abjad sequences plus Hebrew letters/numerals available as numbering types. | full |
| Tables | Nested tables, row split across pages with continuation rows, repeated header rows, vertical/horizontal cell merge, min/max two-pass auto-layout (AutoFit-like), fixed/relative widths, table styles (autoformat), per-cell formulas with a small spreadsheet-like engine, row-keep policies, tables in floating frames that split across pages, change tracking inside tables. Right-to-left visual column order (the `<w:bidiVisual>` equivalent) is supported end to end — model attribute, DOCX/RTF/binary-DOC round-trip with unit tests, mirrored layout, and direction-aware editing (see §4, method 20a). | full |
| Sections & page layout | Text sections: multi-column, protected, hidden/conditionally hidden, footnote/endnote collection at section end, file-linked and DDE-linked sections. Page styles (not sections) own page geometry: size, margins, columns, background, footnote area rules; page-style sequencing (first/left/right/next-style chains). OOXML section properties are mapped onto page-style + section machinery on import. | full |
| Headers & footers | Per page style; shared or distinct left/right; distinct first page; independent margins/height with dynamic spacing negotiation against body text. | full |
| Fields | Very large field inventory: page number/count, date/time, document info, references/cross-references, conditional text, hidden text/paragraph, variables (set/get/formula), input fields, database fields, user fields, script fields, drop-downs, chapter fields, authority (bibliography) entries. Expansion values are cached because correct evaluation may need an up-to-date layout. | full |
| TOC & indexes | TOC, alphabetical index, illustration/table/object indexes, user-defined indexes, bibliography; entries gathered from headings, index marks, captions; generated content is a read-only section with tab-stop-driven layout; hyperlinked entries. | full |
| Footnotes & endnotes | Footnotes at page bottom or beneath text; endnotes at document or section end; footnote area growth negotiation against body; footnotes split across pages with continuation notices; per-page or continuous numbering; separator line configuration. Full protocol in §4C. | full |
| Comments (annotations) | Anchored at a point or over a text range; threaded replies; resolve state; author/date; shown in a sidebar with connector lines; printable in margin modes; content is rich text held in a small embedded edit engine, not in the page flow. | full |
| Redlining (track changes) | Insert/delete/format/paragraph-format/table-row/move detection; stored as position-sorted range records with author/date/comment stacks (nested rejections); show-changes vs. hide-changes display both supported per view; accept/reject individually or all; compare-documents and merge-documents generate redlines. | full |
| Images, drawing, wrap | Images and drawing shapes anchored to page/paragraph/character/as-character/frame; wrap modes: none, parallel, left/right-only, through, transparent-through, contour wrap with editable wrap polygon; positioning rules relative to many reference frames (margins, page, paragraph area, character cell); captions; image cropping/rotation. | full |
| Text frames | Floating text frames with the full anchoring/wrap system; frames chainable (text overflows frame A into frame B); "text box" mode attaching a text frame to a drawing shape. | full |
| Charts & OLE | Embedded charts (native chart module) can pull data from Writer tables; generic OLE embedding with replacement images for unavailable components. | full |
| Math | Native formula editor (separate module) embedded as OLE; OOXML Math (OMML) import/export mapping. | full |
| Content controls & forms | Modern content controls: rich/plain text, checkbox, dropdown/combo, date picker, picture — mapped to `<w:sdt>` on import/export with in-document interactive widgets. Legacy form fields (fieldmarks) and full form-controls layer also present. | full |
| Mail merge | Database-driven merge (registered data sources), field insertion, condition evaluation, output to printer/file/e-mail; wizard UI. | full |
| RTL/CTL & i18n | Full CTL support: per-script font/size/weight (Western/CJK/CTL triplets on every run) with *bidi-aware* script classification (characters inside RTL runs are forced to the CTL slot; embedded digit runs inside Arabic too — see §4A.4); paragraph direction; bidi rendering per UAX #9; HarfBuzz shaping; kashida justification with one font-validated insertion point per word (Arabic priority scheme + a separate Syriac outside-in algorithm) rendered as real tatweel glyph runs (§4A.3); Hebrew/Arabic numbering options; RTL pages and RTL tables; two cursor-travel modes (logical/visual); digit-substitution modes; vertical writing modes for CJK; text grid pages; phonetic guides (ruby); locale-driven break iteration and calendars. | full |
| Clipboard | Rich multi-format transfer: native document fragment, ODF, RTF, HTML, plain text, images; paste-special; drag-and-drop with internal move optimization. | full |
| Find & replace | Plain, regex, and similarity (fuzzy) search; search by formatting attributes and styles; CJK transliteration-aware options; CTL options to ignore diacritics and ignore kashida (tatweel) when matching. | full |
| Spellcheck & autocorrect | Provider-based linguistics (spell, hyphenation, thesaurus, grammar) with async background checking painting error decorations; autocorrect: replacement tables, capitalization rules, smart quotes, word completion; grammar checking via pluggable providers. A user-visible infobar warns when a hyphenation dictionary for a text's language is missing. | full |
| Master documents | A container document aggregating sub-documents by reference, with shared styles/numbering and cross-document indexes/references; sections implement the aggregation. | full |
| Accessibility | Parallel accessibility tree mirroring the layout frame tree with events on layout change; screen-reader and platform bridges; document accessibility checker tool. | full |
| Print & PDF | Print via metafile record/replay; PDF export with tagged PDF (structure derived from layout + model), PDF/A profiles, PDF/UA, links, outlines, embedded fonts, form export. | full |
| Filters | ODF (reference format, via shared XML framework), DOCX (dedicated import module + dedicated export), legacy DOC binary (import/export), RTF (import/export), HTML (dated), plain text, Markdown (recent addition), EPUB (separate module), WordPerfect/Lotus via import libraries. OOXML's three kashida-flavored `w:jc` values (`lowKashida`/`mediumKashida`/`highKashida`) are imported as block-justify plus escalating word-spacing-maximum percentages and exported back by matching those percentages — an approximation, not distinct kashida-length engine modes. | full (HTML/Markdown partial) |
| Collaboration (real-time) | No CRDT/OT in core desktop; experimental Yjs-based collaborative editing scaffolding exists at the repo root (`README.yrs`); tiled-rendering multi-view editing is what powers Collabora Online's collaboration. | partial |
| Versions & compare | In-file document versions (snapshots stored in the container); compare/merge producing redlines. | full |
| Autosave & recovery | Timed autosave, crash-recovery journal + snapshot copies, emergency save on crash, salvage on next start; separate from user "save backup copy" option. | full |

---

## 3. Architecture map

### 3.1 Module layout (pointers)

| Subsystem | Where | Responsibility |
|---|---|---|
| Writer document model | `sw/source/core/doc`, `sw/source/core/docnode`, `sw/source/core/txtnode`, `sw/source/core/attr` | Document object + ~25 "manager" facets behind narrow interfaces (fields, redlines, lists, undo, timers, settings, statistics…); node array; paragraph text + attribute spans |
| Writer layout engine | `sw/source/core/layout` | Frame tree, pagination, invalidation/repair, tables, sections, footnotes, floating objects, painting |
| Text formatting | `sw/source/core/text` | Line building (portion model), justification, hyphenation, drop caps, bidi/script handling, redline display, text painting, cursor geometry |
| Anchored-object positioning | `sw/source/core/objectpositioning`, `sw/source/core/draw` | Position resolution for anchored images/shapes vs. text flow |
| Cursor/selection/find | `sw/source/core/crsr` | Cursor model over node/content positions, shells, find/replace |
| Editing facade | `sw/source/core/edit`, `sw/source/core/frmedt` | High-level edit operations used by UI shells |
| Undo | `sw/source/core/undo` | Inverse-operation objects + attribute history |
| Fields, TOC | `sw/source/core/fields`, `sw/source/core/tox`, `sw/source/core/doc/doctxm.cxx` | Field types/expansion; index generation |
| Numbering | `sw/source/core/SwNumberTree`, `sw/source/core/doc/number.cxx`, `sw/source/core/doc/list.cxx` | Hierarchical counting trees, list styles |
| Tables (model) | `sw/source/core/table`, `sw/source/core/doc/tblrwcl.cxx`, `sw/source/core/doc/htmltbl.cxx`, `sw/source/core/fields/cellfml.cxx` | Table structure, row/column ops, auto-layout, cell formulas |
| UNO API layer | `sw/source/core/unocore` | The scripting/API surface; also the substrate the DOCX importer drives |
| Accessibility | `sw/source/core/access` | Parallel a11y tree over layout |
| DOCX/RTF import | `sw/source/writerfilter` (`ooxml` tokenizer, `rtftok` tokenizer, `dmapper` domain mapper) | Tokenize → uniform property stream → document-API calls |
| DOC/DOCX/RTF export (+DOC import) | `sw/source/filter/ww8` | One document walker, per-format attribute-output backends |
| ODF filter | `xmloff` (generic) + `sw/source/filter/xml` (Writer part) | Reference-format import/export over the document API |
| Shared items/undo base | `svl` | Item/item-set pooling, generic undo manager base |
| Shared edit engine | `editeng` | Lightweight standalone rich-text engine used by comments, drawing text, and other apps; consumes the same shared kashida chooser as Writer since ~2024 |
| Toolkit: fonts/shaping/output | `vcl/source/text`, `vcl/source/font`, `vcl/source/gdi/CommonSalLayout.cxx`, `vcl/source/pdf` | Run segmentation, bidi (ICU), shaping (HarfBuzz), glyph fallback, kashida glyph insertion, output devices, PDF writer |
| Drawing model/renderer | `svx`, `drawinglayer`, `basegfx` | Shapes, primitive-based rendering pipeline |
| i18n services | `i18npool` (breakiterator, collator, calendars, locale data), `i18nlangtag`, `i18nutil` (incl. the shared kashida-position chooser and the bidi-aware script classifier, both extracted into unit-tested modules in the 2024–25 cycle) | Break iteration (grapheme/word/line/sentence), locale behavior, CTL utilities |
| Linguistics | `linguistic`, `lingucomponent` | Provider registry for spell/hyphenation/thesaurus/grammar |
| Tiled rendering / online | `libreofficekit`, `desktop/source/lib` | Paint-tile API, view isolation, callback-based invalidation to remote clients |
| Autosave/recovery | `framework/source/services/autorecovery.cxx`, `sfx2` | Timed snapshots, crash journal, recovery UI |

### 3.2 Data flow: edit → invalidate → layout → paint

1. **Edit.** UI shells call editing facades which mutate the document model
   inside an undo bracket. The model broadcasts change notifications to
   registered listeners — layout frames listen to their nodes.

2. **Invalidate.** Frames receiving notifications set validity flags false
   and propagate targeted invalidations. Nothing is recomputed at this
   point; invalidation is conditional-no-op when the flag is already
   invalid — idempotent and cheap, so over-invalidating is safe by design.
   Key structure (detail in §4B.1):
   - Each frame tracks **three orthogonal validity booleans** — position,
     size, and inner content area (the inner rectangle is stored *relative*
     to the outer one). Two invalidation tiers exist: a quiet tier that
     only clears a flag, and a notify tier that additionally registers
     dirtiness with the owning page.
   - Each **page** carries independent dirty-summary bits: body content
     dirty; layout structure dirty; three separate floating-object bits
     (object layout / object content / objects anchored as characters
     inside content), plus a supplementary bit for objects anchored to the
     page itself. The same per-page summary pattern is reused for
     background text services (spelling, smart tags, word completion
     harvesting, word count). Every notify-tier invalidation also arms a
     root-level "idle formatting needed" flag.
   - The root keeps a **single-slot fast path**: if exactly one content
     frame registered dirty since the last action and nothing structural
     did, the next interactive action formats just that frame and skips
     the page walk entirely — the plain-typing path. A second registrant
     or any structural invalidation downgrades to the general walk.
   - Attribute changes are batched: diffing old vs. new attribute sets
     accumulates a small effect mask (own inner area / size / position;
     own repaint; successor position; successor repaint), applied once.
   - Whole-tree invalidation helpers exist for document-global changes
     (compatibility settings, reference-device changes, view-option
     toggles, writing-direction changes), with skip-ahead bookkeeping so a
     table or section is invalidated once, not once per paragraph.

3. **Repair (layout action).** A short-lived, stack-scoped "layout action"
   object (one at a time per view) walks pages front to back, fixing
   invalid frames. Clean pages are skipped in O(1) each via the page
   summary bits. Per dirty page the nesting is: format objects anchored to
   the page itself; then repeatedly repair the page's structural subtree
   while the layout bit is set (each frame runs its own three-flag
   convergence loop: position → size → inner area, interleaved with flow
   decisions); then optimistically clear the content-related bits and
   format the page's content frames in flow order — if the content pass
   reports unfinished work the bits are re-set and the outer loop repeats.
   The walk **backtracks**: if an earlier page went dirty again (content
   flows backward), it backs up to the earliest such page. Off-screen
   pages can be skipped, but only after formatting the first body content
   of the *following* page once to stabilize the page break, and never
   when a neighboring page holds objects anchored to that content. The
   action is interruptible — it polls pending user input between
   formatting units — but on interrupt it does not simply return: it runs
   a bounded cleanup pass over the pages intersecting the visible area so
   the user never sees a torn visible page; everything else stays flagged
   for the idle machinery. Page creation/destruction mid-walk triggers a
   safe restart. It optionally paints as it goes, accumulating delta
   rectangles. Full protocol in §4B.2.

4. **Idle continuation.** A document-level idle task at the lowest
   scheduler priority resumes work when the app is idle. Its readiness
   probe reports not-ready (with an infinite timeout) while any view holds
   an open action bracket — a busy document never even wakes the
   scheduler. Each firing runs at most **one** job, then re-arms;
   priority: grammar-check kickoff > idle layout > field updates. The idle
   layout job first runs cheap per-page background text jobs over visible
   pages; only if none did work does it run a full repair action (idle +
   complete + paint-collecting, interruptible). It finishes with a scan of
   every page's summary bits, and only when all pages are clean does it
   reset the root flag and broadcast a "layout finished" document event.
   Detail in §4B.3.

5. **Paint.** Painting walks the frame tree, clipping to the invalidated
   rectangles collected by the layout action; text frames replay their
   cached line/portion structures; floating objects are painted in
   z-order layers relative to text.

### 3.3 Text formatting concept (the line builder)

A text frame formats its paragraph into a vertical stack of lines; each
line is a horizontal chain of typed **portions**: plain-text runs, glue
(expandable space), tabs, field expansions, footnote references,
fly-overlap gaps, hyphen portions, drop-cap portions, and "multiportions"
that nest a sub-line for bidi level changes, ruby, rotated text, or
two-line-in-one. Line building is a guess-and-fit process: measure text up
to an estimated break, consult the locale break iterator for the actual
opportunity, handle hyphenation and underflow/overflow corrections, then
finalize the line and record its height/ascent from the tallest portion.
Justification distributes extra width into glue portions — or, for
Arabic/Syriac text, into kashida insertion points chosen per word at
justification time (see §4A.3). The paragraph caches derived per-character
information (script runs, bidi levels, hidden ranges) used by formatting,
cursor travel, and painting alike; the *chosen* kashida positions are also
cached there, but they are a per-justification, font-dependent product
written back after line adjustment — not a once-per-revision precomputation
(v1 had this wrong; see §7). A paragraph also records the character range
touched since last format, so reformatting starts at the first affected
line and stops as soon as a line break re-synchronizes with the previous
result. Formatted line structures are kept in a bounded cache — rarely
painted paragraphs can drop them and rebuild on demand.

### 3.4 How filters plug in

All filters speak to the document through its API/model, never to layout.

- **ODF** is handled by a shared XML framework (streaming, context-stack
  parsing, one context class family per element group) with a Writer
  add-on part. Styles round-trip through the same property machinery the
  API exposes.
- **DOCX import** is three-stage: a tokenizer *generated at build time
  from a declarative grammar file* (`model.xml`, ~19.5k lines of OOXML
  namespaces/elements, processed by a set of Python generator scripts)
  turns OOXML parts into a uniform stream of typed tokens; a "domain
  mapper" consumes tokens, maintains property-map stacks
  (run/paragraph/table/section contexts), and issues document-API calls;
  the **RTF tokenizer is hand-written** (not grammar-generated) but emits
  the same token stream, so RTF and DOCX share the entire mapping layer.
  Import order quirks of OOXML (properties arriving before/after content)
  are absorbed by the mapper's context stacks. (Verified: both filter
  entry points reference the shared mapper factory; the module README
  states the shared-mapper design explicitly.)
- **DOCX/RTF export** live in the legacy DOC filter area: one tree walker
  traverses the model emitting semantic events to an abstract
  "attribute output" interface (on the order of 160 event kinds, most of
  them mandatory); DOCX, RTF, and binary DOC each implement that
  interface — exactly three backends. Export logic (what a property
  *means*) is written once.
- **Fidelity strategy: semantic mapping + grab bags.** LibreOffice does
  *not* preserve OOXML bytes. It maps what it understands into its model
  and stows constructs it recognizes but does not model into "interop
  grab bags" — opaque key/value bundles declared as optional public
  properties at the document, style, paragraph, run (character), and
  shape levels, and additionally on frames, text tables, table rows, and
  table cells — which the exporter re-serializes. Two verified nuances:
  population is *selective* (specific constructs are routed into bags case
  by case; elements absent from the import grammar are simply dropped, so
  this is not an automatic catch-all for arbitrary unknown XML), and
  re-emission is almost exclusively a DOCX-export concern (the RTF backend
  barely touches the bags). This recovers round-trip fidelity for known
  long-tail constructs but not byte identity; ordering and formatting of
  the XML are regenerated. (Our sibling-byte-identical approach is
  strictly stronger for untouched parts; the grab-bag idea is still
  relevant for attributes *within* parts we re-serialize.)

---

## 4. Methods worth learning

1. **Layout as a disposable cache with per-view instances.** The frame
   tree derives from the model and can be dropped/rebuilt wholesale;
   different views hold different layouts (e.g. one showing tracked
   changes, one hiding them) over one model. Keeps model mutation logic
   free of geometry, and makes "reflow everything" always a safe fallback.

2. **Flat node array with paired begin/end markers + hidden regions.**
   Nesting (tables, sections, footnote text, header/footer text) is
   encoded structurally in one array; footnote bodies, header/footer
   text, floating-frame text, and *tracked deletions* live in dedicated
   hidden top-level regions of the same array. Deleting under track
   changes is a *move* into the hidden region — cheap, undoable, and the
   deleted content remains fully editable model content.

3. **Three-flag invalidation with page dirty summaries and a page-order
   repair loop.** Frames track validity of position, size, and inner area
   separately; mutations only flip flags (idempotent, safe to
   over-invalidate). Pages summarize dirtiness in independent bits
   (content / structure / three floating-object variants / page-anchored
   objects) so the repair walker skips clean pages in O(1). A root-level
   single-slot fast path handles the "exactly one paragraph dirty" typing
   case without any page walk. The repair walker fixes pages front to
   back, backtracking when earlier pages re-dirty. Deep dive in §4B.

4. **Interruptible + idle-resumed layout.** The repair walker polls for
   user input; on interrupt it still completes a bounded cleanup of the
   visible pages (never leaving a torn page on screen), then leaves the
   rest flagged. A lowest-priority idle task whose readiness probe keeps
   it asleep while the document is busy runs one prioritized job per wake
   (grammar kickoff > idle layout > field update), visible pages first,
   and ends with a completion scan that broadcasts an explicit "layout
   finished" event. Direct analog of our lazy pagination + `ExpandLayout`.
   Deep dive in §4B.3.

5. **Master/follow flow chains for everything that flows.** Paragraphs,
   tables, and sections all split across pages/columns as chains of
   frames sharing one model object. Flow logic (can I move forward? must
   I move back? do widow/orphan/keep rules bind me?) is factored into a
   shared "flowable" behavior, written once for all flowing kinds. Moving
   *backward* (content returning to a previous page after a deletion) is
   an explicit, separately-handled operation with its own admission tests
   and anti-oscillation registries (§4B.4, §4D.11).

6. **Grow/shrink space negotiation.** A child needing space asks its
   parent to grow; the parent may grow itself (asking *its* parent),
   refuse (fixed-height page body), or grant partially — replies carry a
   reason code that upstream flow logic uses ("content must flow to a
   follow"). Footnote areas, section frames, table cells, and
   headers/footers all size themselves through this one protocol, which
   is what makes "footnotes squeeze the body text" and "cells grow rows
   grow tables" fall out uniformly.

7. **Table split with continuation rows.** When a table hits a page
   bottom, Writer runs a *two-attempt* protocol: first try splitting
   inside a row — the master keeps a partial row and a **layout-only
   continuation row** on the follow carries each cell's overflow (the
   model row is untouched, so editing during a split is safe); if a
   verification pass rejects the result, retry the whole split with
   in-row splitting disabled so the row moves whole to the follow; if the
   split itself reports failure, the whole table moves forward. Repeated
   header rows are cloned (layout-only) onto the follow — or deliberately
   omitted when the incoming row could not fit under them. Row-keep
   settings and row-spanning cells constrain the cut; span groups always
   move atomically. The full protocol — the roadmap design reference — is
   §4D.

8. **Footnote space negotiation.** Footnotes attach to the page or column
   ("note host") that renders their reference line; the host's footnote
   area grows bottom-up, shrinking the body, but never above the *bottom*
   of the reference line — the invariant is enforced as a "deadline"
   computed during line formatting, before the line is finalized. Two
   budgets bound the area: a page-style maximum height and a reserved
   minimum body fraction (a deliberate policy divergence from Word, which
   lets notes fill the page). Notes split across hosts as master/follow
   chains with configurable continuation notices; moving a reference
   migrates its notes via a collect → condense → re-insert protocol.
   Endnotes have three placement modes (dedicated end pages; section-end
   collection via a collector pass; a Word-compat continuous endnote
   section). The full protocol — the roadmap design reference — is §4C.

9. **Pagination loop watchdog with escalating response.** Pagination can
   oscillate (object wrap moves text, text moves the object…). A
   document-level watchdog counts repair rounds within a sliding page
   window; *progress in either direction resets the counter and the
   escalation stage*, so only genuine churn counts. Past a threshold (on
   the order of a few hundred rounds) it force-validates the churning
   pages' subtrees in escalating stages — ordinary frames first, then
   floating objects, finally everything including drawing positions —
   deliberately accepting stale geometry to guarantee termination. It is
   backed by several independent brakes: a near-threshold "light" stage
   that pins frames observed moving both directions in one round; a
   registry of paragraphs pushed forward by object positioning (refusing
   backward moves that would re-trigger the push); a backward-move
   suppressor keyed by frame identity plus a fingerprint of the target
   geometry (identical repeated attempts past a small cap are refused —
   an unchanged fingerprint proves no progress); a per-object
   "grows its table cell" counter; and hard iteration caps on every
   nested loop, convertible to hard failures in test mode so CI catches
   new loops instead of masking them. Detail in §4B.4.

10. **Persisted layout cache.** On save, page-break positions (per page: a
    body-relative node index, a paragraph-vs-table tag, and a split offset
    — character offset for paragraphs, cumulative row index for tables)
    plus per-page floating-object position/size records are written as a
    small versioned, self-delimiting binary stream stored as its own named
    stream in the ODF package. On load the frame builder consumes it:
    follow frames are created directly at the recorded split offsets
    *before* any formatting, pages are started per record, and cached
    object positions pre-assigned — reproducing pagination instantly,
    validated lazily afterwards. A sanity pass discards the whole cache on
    any inconsistent record; a lock/consume discipline prevents mutation
    races; version gates refuse newer caches and distrust data written by
    known-buggy old versions. **This mechanism is ODF-only** — foreign
    formats (DOC/DOCX/RTF) neither write nor synthesize it; what they feed
    instead is a statistics-based page-count estimate used to pre-allocate
    pages (for progress display and to avoid the pathological
    all-content-on-page-one start). Detail in §4B.5. (v1 overstated this;
    see §7.)

11. **Typed portion chains per line + reformat-range early-out.** (§3.3.)
    The two key wins: every inline special case (field, footnote ref,
    tab fill, bidi segment, ruby) is a first-class portion with its own
    measurement/paint behavior rather than an if-ladder in one function;
    and incremental reformat = start at first dirty line, stop at
    resynchronization.

12. **Justification-time kashida with font-validated positions.** (Fully
    rewritten from v1; deep dive in §4A.3.) The paragraph cache holds a
    cheap "contains kashida-capable script" flag (Arabic/Syriac
    detection); actual candidates are computed **per line at
    justification time**, because they are font-dependent: the font's
    minimum kashida (tatweel) width is queried first, and a missing or
    degenerate tatweel glyph disables kashida justification for that
    text. Per word, a shared i18n utility (used by both Writer and the
    edit engine) picks **one** insertion point: Arabic via a
    publicly-documented descending typographic priority scheme (the
    IE 5.5 "connection priority" rules) — a user-typed tatweel wins
    outright, then roughly seven classes keyed on Unicode joining
    properties, later-in-word positions preferred at equal priority, with
    documented amendments (no insertion before a word-final Yeh,
    ZWNJ-adjacent positions excluded, combining marks skipped, plus a
    last-resort tier accepting any position the shaping layer reports
    valid); Syriac via a separate outside-in algorithm. Validity comes
    from HarfBuzz's public safe-to-insert-tatweel glyph flag, read back
    from a trial word layout (fonts using AAT substitution tables cannot
    be validated and skip validation). Chosen points join spaces as equal
    expansion units; if the per-unit share falls below the widest needed
    kashida width, candidates are dropped until the rest fit. Surviving
    positions are cached back into the paragraph structure, and the
    identical width-array widening runs in the draw, measure, and
    hit-test paths — cursor/selection consistency falls out of shared
    measurement, **not** a validity-marking API (v1's claim of validity
    exposed to cursor logic described a since-removed pre-rework model).
    At render time the device layer inserts *actual repeated tatweel
    glyphs* sized to fill each gap, not just widened advances. Users
    force a point by typing a tatweel (in-text character, top priority)
    and suppress candidates adjacent to a ZWNJ; there is no per-position
    user-invalidation subsystem.

13. **Redline display: decorate-or-merge.** Show-changes mode renders
    insertions/deletions as styled ranges layered onto normal formatting
    by the line builder's attribute stack. Hide-changes mode is far more
    radical: the layout builds a *merged* text view in which one text
    frame can span several model paragraphs (a tracked paragraph-delete
    joins them), holding a mapping table from view-text offsets to
    (node, offset) model positions. All formatting/cursor/paint code
    works in view offsets and translates at the edges. This is the
    cleanest known answer to "hidden deletions must not affect
    pagination" and directly maps to our merged-story needs.

14. **Pooled items + parent-chained sets for style inheritance.**
    Refcount-pooled property objects deduplicate storage across the
    document; resolution order (direct → character style → paragraph
    style → defaults) is just parent-chain lookup; "what differs from the
    style" (needed for clean export) is set subtraction.

15. **One export walker, N attribute-output backends.** DOCX, RTF, and
    binary DOC exports share a single model traversal that raises
    semantic events; each format implements an output interface.
    Semantics ("a page break at a paragraph with a page-style switch
    means a new section") are encoded once. (Verified: exactly three
    backends derive from the abstract interface and from the shared
    export base.)

16. **Interop grab bags for unknown OOXML.** Constructs the importer
    recognizes but does not model are preserved as opaque key/value
    bundles hung on document/style/paragraph/run/shape — and also
    frame/table/row/cell — properties and re-emitted on export
    (essentially only by the DOCX backend). Population is selective, case
    by case; elements absent from the import grammar are dropped, so this
    is bounded long-tail fidelity, not a catch-all. For us: relevant
    inside re-serialized parts (`document.xml`), complementing
    byte-preservation of untouched siblings.

17. **Grammar-generated tokenizer + shared domain mapper.** The DOCX
    reader's tokenizer is generated at build time from a declarative
    grammar of OOXML namespaces/elements; the RTF tokenizer is
    hand-written but emits the same uniform token stream; one mapping
    layer with context stacks (run/paragraph/section/table) consumes both
    and calls the document API. Adding an element = grammar entry + one
    mapper case; the two formats share all semantics.

18. **Field expansion caching + positional update pass.** Fields cache
    their last expansion text because correct evaluation can require
    finished layout (page numbers, page counts). Updates run as explicit
    passes over a position-sorted field list (variables accumulate in
    document order); layout detects "an expansion changed after
    formatting" and schedules one more round. Chapter-dependent and
    page-dependent fields resolve against the layout, not the model.

19. **Undo as inverse-operation objects + attribute history + node
    stashing.** Each user action pushes an object able to undo and redo
    itself; formatting changes record prior attribute state in a history
    list; deleted content moves to a hidden node region owned by the undo
    system rather than being serialized. Compound actions bracket many
    steps under one user-visible entry with a rewritten description.

20. **Two composable direction abstractions (not one).** (Corrected from
    v1's single "direction-agnostic accessor layer".) Writer separates
    axis orientation from horizontal mirroring:
    - A **geometry-accessor table** — a set of function-pointer bundles
      covering rectangle get/set, margins, position construction, and
      axis difference/increment — selected per frame, abstracts
      *horizontal vs. three vertical* writing modes for pagination code.
      Horizontal RTL is **not** one of its variants.
    - Horizontal RTL is a separate **lazily-derived per-frame boolean**
      (computed from the resolved frame-direction attribute), applied via
      explicit conditional mirroring at a bounded set of sites —
      sibling/neighbour positioning (first cell/column anchored at the
      parent's right edge, successors leftward), cell growth direction,
      cursor clamping, border-edge classification — plus LTR↔RTL
      coordinate-swap helpers at the paragraph-frame boundary through
      which paint, cursor geometry, and hit-testing route.
    The transferable idea: RTL frames are laid out in logical LTR
    coordinates and mirrored at boundary crossings. It works at scale but
    is invasive — every feature must remember to mirror, and at least one
    hit-testing path in the tree carries an explicit note that
    mixed-direction handling there is approximate. If we can fold
    horizontal RTL into the accessor itself we would be structurally
    ahead of the reference.

20a. **RTL tables end to end** (verified as genuinely complete): the
    table's frame format carries the same frame-direction attribute used
    by pages/sections/frames (public API "WritingMode" property, with an
    inherit-from-environment value); `<w:bidiVisual>` maps to/from it in
    the DOCX filter (RTF and binary DOC symmetrically), with round-trip
    unit tests; layout anchors the first cell at the row's right edge and
    places successors leftward, invalidating the subtree recursively on
    direction change; editing is direction-aware throughout (Table
    Properties text-direction control, RTL-aware ruler column dragging,
    border-edge swapping, mirrored cursor clamping, RTL-aware row/column
    drag projection).

21. **Anchored-object positioning as fixpoint iteration with wrap
    caps.** Object position depends on text flow; text flow depends on
    object wrap. A dedicated positioning subsystem iterates to
    convergence, with explicit counters that stop an object from
    repeatedly pushing its anchor paragraph forward across pages.

22. **Comments outside the flow via an embedded mini-engine.** Comment
    bodies are small standalone rich-text engines (the shared edit
    engine), anchored to the model by a range-marking attribute; layout
    of comment bubbles is a UI-side sidebar concern, entirely outside
    pagination. Keeps arbitrary rich comment content from ever
    perturbing page layout.

---

## 4A. Deep dive: CTL/Arabic end to end

### 4A.1 Where shaping happens and which OpenType features are engaged

Shaping lives entirely in the toolkit (VCL) layer, below Writer. One
platform-independent layout class drives HarfBuzz on all platforms; shaper
preference is Graphite2 → OpenType → HarfBuzz's fallback shaper, so
Graphite fonts get their native engine. Per layout request: bidi runs (ICU
UBA over the requested range) → script sub-runs (from a script-run
itemizer whose results are kept in a global LRU cache keyed by the string,
explicitly to avoid quadratic re-itemization on large paragraphs) → for
vertical text, further subdivision by Unicode Vertical_Orientation
(UAX #50). Each sub-run is shaped with the *full paragraph string as
context* (begin/end-of-text flags set only at true string ends), so
joining behavior survives run boundaries — an Arabic run split by a style
change still connects correctly.

Feature engagement: no Arabic features are forced — HarfBuzz's per-script
shaper defaults own init/medi/fina/rlig. What the editor adds: a
kerning-off request disables kerning; a ligatures-off request disables
optional ligatures only — *required* ligatures deliberately stay on, so
Lam-Alef never breaks; user-specified OpenType features via a suffix
syntax on the font name (tag=value pairs plus a language-system override),
surfaced in a font-features dialog; and two HarfBuzz buffer flags —
produce safe-to-insert-tatweel (kashida validation) and produce
unsafe-to-concat (cache slicing). Synthetic bold/italic are applied at the
font-instance level and thus work for Arabic fonts too.

Shaped-run caching: whole-string layouts are cached and sliced into
substring layouts only at cluster boundaries HarfBuzz marked
safe-to-concat; otherwise reshape. Debug builds cross-verify sliced
results against fresh shapes.

Font fallback on missing glyphs expands the request to the whole grapheme
cluster (via the break iterator) so base + marks migrate to the fallback
font together; a curated known-good family list is intersected with
installed fonts plus a per-platform system hook; Private-Use-Area
codepoints are exempted.

### 4A.2 Diacritic/mark handling

Mark positioning is delegated to HarfBuzz (mark/mkmk from the font); the
editor's contribution is cluster discipline around it:

- **Justification never splits clusters.** Glyphs are grouped into
  grapheme clusters so marks travel with their base under advance
  adjustment. Because HarfBuzz cannot deliver cluster-grouped and
  character-level position data simultaneously, a self-documented
  workaround shapes a run a second time at character granularity to
  recover per-character mapping when a grapheme cluster spans adjacent
  sub-layouts — an honest, bounded approximation (an in-tree comment
  admits the side-by-side sub-layout model cannot handle a glyph whose
  advance was reordered into a different sub-layout).
- **Caret positions inside ligatures** come from the font's
  ligature-caret table (GDEF): a ligature's width is distributed over its
  grapheme clusters at the font-declared caret positions (scaled,
  reversed for RTL), falling back to even division. A caret inside
  Lam-Alef lands where the type designer said, not at width/2.
- **Kashida never separates marks**: candidate positions skip
  transparent-joining (mark) characters entirely — a candidate always
  refers to a base-to-base junction — and trailing vowel marks are
  ignored when finding the word end.
- Zero-width glyph portions (reference marks, TOC marks) are always
  painted even at zero width — a fix specifically for combining
  diacritics.

### 4A.3 Arabic justification end to end

**Decision phase (line adjuster).** Block-adjustment walks the finished
line. The line divides into independent justification segments at fixed
portions — tabs and floating-object gaps; each segment gets its own
expansion. A Word-compat setting suppresses justification after
centered/right/decimal tabs (a left tab re-enables it); another suppresses
justifying lines ended by a manual break. Justification *mode* is chosen
per portion by script + language: CJK non-Korean → inter-character
expansion; Korean → space-based; Thai → per-character-cell distribution
skipping above/below-base marks; Arabic/Syriac → kashida combined with
space expansion; else spaces.

**Kashida choice.** If the paragraph's script cache says it contains
Arabic/Syriac: per glue segment, a dictionary-word scanner iterates words;
per word, the layout is asked for a font-validated position map
(per-position safe-to-insert-tatweel from a trial word layout,
transparent characters skipped); then the shared i18n chooser picks **one
position per word**. Arabic uses the publicly documented IE 5.5
"connection priority" scheme (credited in-tree to the public khtt.net
writeup): a user-typed tatweel wins outright, then roughly seven
descending classes keyed on Unicode joining-group/joining-type
properties, ties resolved toward the word end, with documented
amendments — never before a final Yeh (with a narrow exception), never
adjacent to ZWNJ, ligature-forming pairs excluded, and a last-resort tier
accepting any font-valid position. The scheme distinguishes right-joining
letters (whose final form can occur mid-word) from dual-joining letters.
**Syriac gets a separate algorithm**: user tatweel first, else outside-in
— from word end toward the midpoint, then from word start toward the
midpoint — still one point per word.

**Budgeting.** Each chosen point counts as one glue unit alongside spaces;
the segment's extra width divides equally. If the per-unit share is below
the widest required minimum kashida width (the font's tatweel advance),
points are dropped *from line start forward* — a positional heuristic,
not priority-ordered — until it fits; zero survivors → redo as plain
space justification. Chosen positions are stored per line and
re-aggregated into one paragraph-level list; a word broken across lines
can have its best position on another line (that word then gets no
kashida — out-of-range positions are discarded).

**Application phase.** Measurement, paint, and hit-test all run the same
helpers over the per-character advance array: kashida widths land at the
chosen positions (marked in a parallel boolean array) and the remaining
expansion goes to blanks. In Arabic contexts the usual centered-widened-
space trick is disabled; the full extra advance lands on one side, with
compensation at portion-final blanks. The kashida array rides the draw
call into the device layer, where **actual tatweel glyphs are inserted**
RTL as cluster members with zero advance — repeat count = extra width /
tatweel advance, with a slight overlap of copies to absorb any shortfall.
The same array is recorded into metafiles, so print/PDF carry identical
elongation. Because hit-testing uses the same adjusted advances, clicking
an elongation resolves to the adjacent cluster boundary — consistency by
construction, not via a validity API.

**Tabs and fields.** Tabs bound justification segments. Field-expansion
text has no script cache, so kashida is never inserted inside fields;
field spaces still count for space justification.

**Recent evolution (2024–25 cycle, LibreOffice 24.x–25.x).** The
glyph-inserting kashida rework, HarfBuzz-flag validation (a HarfBuzz
capability LibreOffice itself drove upstream; AAT-shaped fonts are
excluded from validation), the extraction of the chooser and the script
classifier into unit-tested i18n modules, Syriac justification (25.2),
and a proportional word-spacing model (minimum/desired/maximum
percentages) integrated with kashida. OOXML's `lowKashida` /
`mediumKashida` / `highKashida` `w:jc` values map to escalating
word-spacing caps — an approximation. The secondary edit engine
(comments, text boxes) now shares the same chooser, closing a decades-old
parity gap.

### 4A.4 CTL font resolution and the triplet model

Every formatting context holds three complete sub-fonts — Western, CJK,
CTL — each with independent family/style/size/weight/posture/*language*,
plus a selector for the active script slot. The script classifier
(recently rewritten into a clean, unit-tested i18n module) is **not
purely per-codepoint — it is bidi-aware**: every character in an RTL bidi
run is forced to the CTL slot (Asian-script characters excepted), and
embedded LTR runs containing no strong-LTR character (numbers inside
Arabic) are also forced to CTL. Weak characters otherwise inherit the
preceding run's script; leading weak characters take the first non-weak
script seen in the paragraph; combining marks join their base's run. An
ODF character attribute can pin the classification per span. Language
attributes are per-slot, so spell/hyphenation/kashida word scanning for
Arabic consults the CTL language.

Writer resolves bidi itself: ICU UBA per paragraph at the paragraph's
base level, stored as level runs in the script cache (portion-terminal
bidi controls folded into the run they terminate). Within a line,
direction changes become *nested* sub-line portions of arbitrary depth;
when painting/measuring, a "strong direction" mode is set on the output
device per directional run so the toolkit does not re-run UBA. Digit
substitution is separate: a user CTL option (Western / Arabic-Indic /
follow-system / follow-text-language) sets a digit language on the output
device, and ASCII digits are substituted to native digits just before
shaping.

### 4A.5 Hyphenation and spellcheck vs. Arabic

Hyphenation: Arabic never hyphenates *by design* — a language-database
predicate excludes Arabic-script languages (Arabic, Persian, Urdu,
Pashto, Sindhi, Kurdish-in-Arabic-script, and others), CJK, and
Vietnamese from hyphenation as a concept. The hyphenation language is
taken from the active script slot's language, so a mixed Arabic/English
paragraph hyphenates only the English; kashida absorbs the raggedness on
the Arabic side.

Spellcheck: provider-based (Hunspell), fed words from the same
locale-aware scanner used by kashida. No editor-side stripping of harakat
or tatweel — tolerance for vowelized/elongated spellings is delegated to
the dictionary's own affix options. Background checking stores
per-paragraph sorted wrong-ranges repainted by the line builder;
decorations follow portion geometry so squiggles segment correctly across
bidi runs. Search exposes two CTL transliteration options:
ignore-diacritics (decompose, strip marks, recompose) and ignore-kashida
(skip tatweel when matching).

### 4A.6 Arabic/bidi cursoring and selection

- **Two cursor-travel modes**, user-selectable (Word parity): logical
  (arrows walk memory order) and visual (arrows walk screen order).
  Visual mode for an insert cursor recursively walks the line's nested
  bidi portion tree, flipping the movement sense inside odd embedding
  levels; for an overwrite cursor it maps logical→visual via ICU, steps
  by one, and maps back.
- **Caret affinity by bidi level.** The cursor carries a bidi level. At a
  direction boundary one logical index has two visual positions;
  caret-rect computation compares the stored level against the boundary
  portion's embedding level to choose which edge blinks. Up/down travel
  sets the level to the smaller of the adjacent levels. One caret,
  affinity-resolved — no split caret.
- **Navigate by cluster, delete by codepoint.** Arrow movement uses
  grapheme-cluster iteration (locale-aware); backspace/delete use
  codepoint iteration — backspace on letter+harakat removes only the
  harakat first. This asymmetry is exactly Word's Arabic behavior and
  easy to miss.
- **Caret inside ligatures** at font-declared ligature-caret positions;
  kashida elongation is hit-testable as part of its cluster.
- **Selection rects** are computed per portion in visual order, with
  explicit handling for nested counter-directional portions — a logical
  range yields discontinuous visual rects. RTL frames mirror x
  coordinates at defined boundaries (layout is LTR-internal).

### 4A.7 Honest judgment — what 35 years delivers, and the weak spots

What maturity buys: (1) end-to-end consistency — kashida decided once,
then measurement, painting, caret, hit-test, selection, metafile print,
and PDF all consume the same arrays; (2) font-validated kashida via a
HarfBuzz flag they drove upstream; (3) the justification-mode matrix
(kashida × spaces × letter-spacing × glyph-scaling × tabs × fields ×
per-language CJK/Thai/Korean modes × Word-compat flags) — the interaction
surface where naive implementations die; (4) bidi-aware font-slot
resolution including numbers-inside-Arabic; (5) the caret affinity +
visual/logical dual mode + cluster/codepoint asymmetry package;
(6) cluster-preserving fallback and ligature-caret carets; (7) a distinct
Syriac algorithm; (8) digit-substitution modes. The deep Arabic machinery
was re-extracted in 2024–25 into unit-tested modules — active investment,
not frozen legacy.

Visible weak spots: letter-spacing has no joining-awareness (tracking
visually disconnects joined Arabic text; Word suppresses this); the
double-shaping workaround and the admitted reordered-cluster
approximation; no kashida validation for AAT-only fonts; the
insufficient-width kashida drop is positional, not priority-ordered;
OOXML kashida intensity approximated via word-spacing caps; no kashida in
field text; Arabic justification parity in the secondary edit engine
arrived only ~2024; bug-workaround density concentrates exactly in the
wrap×text and caret×bidi zones; and the mirror-the-LTR-layout RTL model
demands explicit coordinate switching at every boundary, with one
hit-test path admitting approximate mixed-direction handling.

---

## 4B. Deep dive: incremental relayout

### 4B.1 The validity model

Per-frame state: outer frame rectangle + inner content rectangle held
*relative* to the outer, and exactly three validity booleans — position,
size, inner area. "Fully valid" is their conjunction. Geometry and flags
are isolated in a dedicated base type, and rectangle writes go only
through scoped write-guards so every mutation point is auditable (a
deliberate hardening retrofit). Each frame carries a unique numeric id
keying loop-control bookkeeping. Separate from geometry validity: a
complete-repaint flag, a "retouche" flag (the last frame in a container
owns repainting the vacated gap below itself when content shrank), a
line-number-validity flag, and lazily recomputed cached context flags
(inside table / footnote / fly / section; writing direction).

Invalidation is a conditional no-op when the flag is already invalid.
Two tiers: quiet (clear the flag) and notify (also register with the
owning page). Virtual veto/react hooks let special frame kinds (notably
anchored-object frames) refuse or respond to invalidation — used to break
notification cycles during object positioning.

Attribute changes batch into an effect mask (own inner area / size /
position; own repaint; successor position; successor repaint) applied
once: margins/borders/padding → inner area + size + repaint;
background-only → repaint of self and successor, no geometry;
keep-with-next → position; explicit frame size → size + inner area +
successor position; wholesale style reassignment → everything.

Page-level dirty summaries: five layout bits (body content; layout
structure; fly layout; fly content; flies anchored as characters inside
content) plus a supplementary at-page-object bit letting the repair loop
detect that formatting page-anchored objects re-dirtied the page. The
identical pattern extends to background text services (per-page bits for
spell-check, smart tags, word-completion harvesting, word count),
cleared only when the whole page came out clean. A notify-tier
invalidation sets the bit matching what the frame is; an object anchored
in content additionally invalidates the page hosting the *anchor* (which
may differ from the page hosting the object). Every registration also
arms the root-level idle-needed flag.

Root-level fast path ("turbo"): if since the last action exactly one
content frame registered dirty and nothing structural did, the next
interactive action formats just that frame and skips the page walk; the
shortcut is verified afterwards (if it dirtied page layout or objects,
the full walk runs anyway).

Whole-tree invalidation helpers serve document-global changes
(compatibility settings, reference-device/font-metric changes,
line-numbering configuration, view-option toggles, writing-direction
changes), with skip-ahead bookkeeping so a table or section is
invalidated once rather than per contained paragraph.

### 4B.2 The repair walk

Two complementary mechanisms.

**Pull ("prepare"):** formatting one frame on demand first recursively
formats its parent, then walks the parent's children from the first up to
the target, formatting each invalid one (a frame's position depends on
its predecessor's extent), re-formats the parent, then the target. Guard
objects protect frames from destruction mid-walk; a cursor-specific
variant brings exactly the caret's context up to date.

**Push (the stack-scoped repair action):** one at a time per view,
configured by flags — paint-as-you-go vs. format-only; whole document
vs. visible area; idle-triggered; which input classes may interrupt.
Sequence:

1. Start page: whole-document mode starts at page one; interactive mode
   at the first visible page, corrected *backward* when the first visible
   body content is a continuation — its master's page becomes the start
   (a continuation cannot be formatted correctly without its master).
2. Advance to the first page with any summary bit set; clean pages are
   skipped in O(1).
3. Per dirty page, nested convergence loops: outer — while the page is
   dirty: format page-anchored objects, then repeatedly repair the page's
   structural subtree while the layout bit is set (each layout frame runs
   its own three-flag loop; hardened because formatting a child can
   delete or move siblings); then optimistically clear the
   content-related bits and format content frames in flow order, each
   formatting itself plus its anchored objects; if the content pass
   reports failure the bits are re-set and the loop repeats. Each nested
   loop has a small hard cap with a warning; the document watchdog is fed
   once per round.
4. Motion after a page settles: if any earlier page at/after the start
   went dirty again, back up to the earliest such page (content flows
   backward); a restart-cycle flag (content moved back by more than one
   page) sends the walk back to a re-derived start; a paragraph that
   moved forward by more than one page records the destination so the
   walk can catch up; otherwise advance to the next dirty page.
5. Off-screen shortcut (interactive, non-idle, non-complete only): a page
   entirely beyond the visible area is a skip candidate — but the walker
   first formats the first body content of the *following* page once, to
   stabilize the page break; if that content changed page, the skip is
   aborted and the walk backs up. Skipping is refused when a neighboring
   page holds objects anchored to that content. Browse mode applies a
   finer wholly-below-visible test.
6. Interrupt: between formatting units the action polls the input queue
   for the configured input classes. On interrupt it does *not* simply
   return: it runs a bounded cleanup pass over the pages intersecting the
   visible area (structural repair plus, by policy, one complete content
   pass per visible page) so the user never sees a torn visible page;
   everything else stays flagged for idle.
7. Restart: page creation/destruction mid-walk sets an "again" flag; the
   action unwinds safely and the internal pass re-runs; it also re-runs
   once if the moved-forward registry grew during the pass (after
   clearing it with re-invalidation). Between passes, empty trailing
   pages are removed and pages required by page-anchored objects
   appended.
8. Painting: paint mode accumulates rectangles — deltas between old and
   new frame rects, complete-paint areas, retouche gaps — minus areas
   covered by opaque floating objects. Format-only actions leave the
   complete-paint flags set for the next paint pass.

Per-frame repair loop (all frame kinds): while any of the three flags is
false — fix position (requires predecessor and parent valid), then size
and inner area (frame-kind-specific); for content frames run the text
formatter over the recorded dirty character range; interleave flow
decisions (move forward on no-fit; move backward when space opened; keep
/ widow / orphan constraints). Each move re-invalidates and the loop
re-runs until all three flags hold. A "must fit" escape hatch — engaged
only after a run of consecutive formats with no geometric change —
forcibly places a paragraph oscillating between fitting and not fitting.

### 4B.3 Idle-resumed formatting

Scheduler layer: a document-level idle task at the lowest scheduler
priority. Its readiness probe (evaluated between tasks, not on firing)
reports ready only when a per-view option allows background jobs, no view
holds an open action bracket, and the next job would not be "busy";
otherwise not-ready with an infinite timeout — a busy document never even
wakes the scheduler. An explicit block/unblock count suspends idling
across critical phases (mass frame creation on load; table formatting
inside the repair action). In the tiled-rendering server the first idle
run is delayed so first-tile rendering wins.

Job selection: each firing runs at most one job, then re-arms. Priority:
grammar-check kickoff > idle layout (skipped during drag interactions) >
field updates; "busy" postpones without consuming.

The idle layout job: (i) run the cheap per-page background text jobs over
*visible* pages first (smart tags, spelling, word-completion harvesting)
— if any did work, stop there; (ii) otherwise run a full repair action
flagged idle + complete + paint-collecting, interruptible by any input
class except timer ticks; if paint rectangles accrued, the whole window
is invalidated rather than patched (explicitly trading one big repaint
for correctness); (iii) if not interrupted, run whole-document background
jobs (word count → smart tags → spelling → word completion); (iv) scan
every page's summary bits — layout bits plus each background bit gated by
whether its service is enabled — and only if all pages are clean reset
the root's idle flag and broadcast a "layout finished" document event
(consumed by scripting/accessibility). Anything still dirty leaves the
flag set and the scheduler fires again. Debug builds paint a tiny colored
corner square while idle layout runs — a live "is the idle loop hot"
indicator.

### 4B.4 Pagination watchdog

Cooperating defenses:
- **Oscillation watchdog** (one per repair action, fed once per page-loop
  round): maintains a sliding window of physical page numbers. Progress
  to a clearly later page slides the window and resets both the round
  counter and the escalation stage; regression to an earlier page also
  resets everything (backtracking is normal). Only rounds inside the
  small window count. Past a threshold on the order of a few hundred
  rounds it escalates one stage and force-validates the current page's
  entire subtree (plus the previous page's when churn is backward, the
  next page's when forward) — force-validating = setting all three flags
  true *without* computing geometry. Stages widen scope: ordinary frames
  first (anchored objects get one more chance), then floating objects and
  their contents, finally everything including drawing positions. The
  final stage reached is exposed for tests. Explicit policy:
  stale-but-terminating beats correct-but-nonterminating.
- **Pre-trigger "light" stage:** near the threshold, a text frame
  observed moving both forward and backward within one round (outside
  balanced columns, where that is legitimate) is registered in the
  moved-forward registry, pinning it to its page — often defusing the
  loop before the drastic stage.
- **Moved-forward-by-object registry:** paragraphs pushed to a later page
  by object positioning are recorded (paragraph identity → destination
  page); consulted to refuse a backward move that would re-trigger the
  push; table rows ask whether any registered paragraph sits inside them.
  Cleared at action end; if it *grew* during an action, cleared with
  re-invalidation and the action re-runs once.
- **Backward-move suppression:** each attempt to move a flow frame
  backward into a candidate parent increments a counter keyed by the
  frame id plus a fingerprint of the candidate parent's geometry
  (position, size, remaining free space). Past a couple dozen *identical*
  attempts the move is refused — an unchanged fingerprint proves no
  progress. Cleared per action.
- **Object-grows-cell counter:** an anchored object repeatedly growing
  its containing table cell (a monotonic spiral) is counted per object;
  growth is refused past a cap.
- **Local hard caps** on every nested loop with warning logs, plus a
  large absolute cap on the outermost page loop that forces the interrupt
  flag as a last-resort net. A test-only switch converts these silent
  recoveries into hard failures so CI catches new loops.

### 4B.5 Persisted layout cache

Content: for every page except the first, one break record describing the
first body-content element on that page — its content-node index stored
relative to the body-region start (so hidden regions don't shift it), a
paragraph-vs-table tag, and, when the element continues something split
from the previous page, the split offset (character offset for
paragraphs; cumulative row index for tables, counted across the whole
master/follow chain with repeated header rows factored in). Plus per-page
floating-object records: page number, z-order id, position relative to
the page origin, size. Only body text participates; header/footer-
anchored objects are excluded.

Encoding: a small versioned binary stream stored as its own named stream
inside the ODF package (beside the XML, not in it), with self-delimiting
tagged records — unknown record types are skippable (forward
compatibility). A major-version gate refuses newer caches; a
minor-version quirk flag records that object *sizes* written by a buggy
old version must be ignored while positions are still trusted — versioned
trust in cached data.

Consumption: on load, during mass frame creation, a page-maker helper
walks content in document order. At each node matching the next break
record it first — when a split offset is present — creates the follow
frame directly at the recorded offset *before any formatting* (cloning
repeated header rows into the follow), then starts a new page. Explicit
break attributes insert pages regardless of the cache. When a page is
finished, floating objects still unpositioned get the cached position
(and size, if trusted) pre-assigned — matched by sorting both sets by
z-order, applied only when cardinalities agree, silently skipped
otherwise. Frames built this way still carry invalid flags where
appropriate: pagination is reproduced instantly, then validated lazily.

Robustness: a sanity pass validates every break record (node index inside
the body range; node type matches the tag); any violation discards the
entire cache. A lock count implements the consume discipline — mutations
clear the cache only when unlocked. Without a cache, the same page-maker
still pre-inserts pages using an estimate — the stored document-statistics
page count when plausible, otherwise an assumed paragraphs-per-page
figure weighted by node counts — for progress reporting and to avoid the
pathological start where all content lands on page one. On save the cache
is written from the live layout. A debug-only comparator checks a
computed layout against the cache to detect pagination instability across
load/save cycles. **ODF-only**: the DOC/DOCX/RTF import paths neither
write nor synthesize this stream; foreign formats feed only the
statistics-based estimate.

### 4B.6 Notes for our incremental relayout

The transferable skeleton: (a) three orthogonal validity bits per box + a
relative inner rect; (b) page-level dirty disjunctions for O(1) clean-page
skipping; (c) a root-level one-dirty-frame fast path for typing; (d) a
single stack-scoped repair action with front-to-back page order,
backtracking on predecessor re-invalidation, off-screen skip with a
one-frame lookahead to stabilize the break, and
interrupt-then-clean-up-visible semantics; (e) an idle scheduler that runs
one prioritized job per wake and a completion scan ending in an explicit
"layout finished" signal; (f) watchdog = sliding page window + round
counter + staged force-validation, backed by fingerprint-keyed
move-suppression registries; (g) a persisted break-hint stream validated
then consumed under lock, with split-before-format follow creation. Our
`LazyLayoutState`/`ExpandLayout` maps to their visible-first +
idle-completion pair; the pieces we lack that they prove necessary at
scale are the page dirty summaries, the backtracking rule, the
interrupt-cleanup of visible pages, and the watchdog registries keyed by
(frame identity × target-geometry fingerprint).

---

## 4C. Protocol: footnote/endnote space negotiation

Vocabulary here ("note host", "note area", …) is descriptive, not the
reference's naming.

### 4C.1 Entities and ownership

- A **note host** is the layout unit owning a footnote area: the page on
  single-column pages, or an individual column on columned pages and
  inside columned sections. Every host carries one mutable budget: the
  maximum height its footnote area may currently occupy. The page
  initializes it from the page style's footnote-area setting (zero =
  unlimited) and distributes it to its columns.
- Each host owns at most one **note area** frame — a container placed
  directly after the body region in the host's child list. Its top inset
  is the separator zone (separator-line thickness + configured distances;
  under a Word-compat mode for continuous endnotes it derives from the
  default paragraph font's line height).
- Inside sit individual **note frames**, one per footnote per host,
  ordered by document order of their reference anchors — order resolved
  via the document's global, position-sorted footnote registry, never by
  scanning layout. Each note frame records the content frame containing
  its reference mark and the model attribute identifying the note. Note
  content is ordinary paragraph/table content (bodies live in a hidden
  model region), so the note frame is just another flowable ancestor.
- A note that doesn't fit one host becomes a **chain**: master + follow
  note frames on subsequent hosts, doubly linked. Endnotes share all of
  this, distinguished by a flag; in the global ordering all endnotes sort
  after all footnotes.

### 4C.2 Space reservation during line formatting (the deadline protocol)

- Reservation happens *while the line builder runs*, not as a separate
  pass. Reaching a footnote reference in the text, the formatter computes
  a **deadline**: the absolute bottom edge of the line that will hold the
  reference. Invariant: the note area may never grow above the deadline —
  the reference line always fits above its own note (the note may butt
  directly against the line's bottom).
- Deadline adjustments: add the paragraph's bottom border/margin; if the
  reference sits inside a table that may not split, push the deadline to
  the table's bottom; if only the row may not split, to the row's bottom
  (that block moves as a unit anyway, so the note must not steal space
  above it); bottoms of floating objects anchored at earlier paragraphs
  of the same host also push it down.
- The formatter then "connects" the note: locates or creates the note
  frame under the correct host (a scoped guard temporarily caps the
  host's budget at the deadline, restoring the previous value afterwards
  unless someone changed it meanwhile), then triggers a rearrangement
  (§4C.7) bounded by the deadline. Connecting is idempotent.
- Pathological check afterwards: adding the note may have consumed so
  much space that the reference line itself no longer fits, or the note
  landed on a later host than its reference. If the current line is not
  the first line of the host, the formatter aborts the line and forces a
  break before it, so reference and note move forward together. On the
  first possible line the rule is waived (otherwise nothing would ever
  fit) — mirrored by disabling widow/orphan/keep for content at the very
  start of a host.

### 4C.3 Growth negotiation

- The note area is variable-height. Growth requests propagate note
  content → note frame → note area. The area clamps twice: against the
  host's budget, and against the **body reserve** — the body must retain
  a fixed minimum fraction of its height for text (an explicit policy
  divergence from Word, where notes may fill the page; the fraction is
  smaller under one Word-compat setting). Partial grants carry a reason
  code ("content must flow to a follow") consumed by flow logic.
- How granted space is obtained depends on context: on a plain page, pure
  redistribution — the body shrinks by exactly what the area gains.
  Inside a floating frame, the parent is asked to grow. Columned sections
  mix modes (grow-section-first vs. redistribute-first depending on
  configuration; redistribution preferred when the last note is an
  endnote); in a section still expanding toward its maximum, area growth
  is deferred — the section is invalidated to grow and the area retries.
- On dedicated footnote/endnote pages the area may take as much as it
  wants, bounded only by the body height.
- Inverse: cutting a note shrinks the area; the last note removed takes
  the area frame with it, and an emptied page becomes a removal
  candidate.

### 4C.4 Insertion and ordering invariants

- Insert: find the first note on the host; walk forward (across hosts if
  needed) comparing registry positions to find the tightest sibling pair;
  create the area if missing (budget permitting). Guards: endnotes never
  mix into footnote-page areas and vice versa; inside sections,
  candidates from a different section's collect-at-end scope are
  rejected. Stale incarnations of the same note attribute elsewhere in
  the layout are destroyed first.
- Self-healing chains: a note frame inserted directly next to its own
  master or follow (same host) is merged immediately — content moves into
  the survivor, the redundant frame dies. No separate cleanup pass.
- A columned (non-collect-at-end) section must never have note areas both
  inside its columns and on the page: an append that would create the
  second kind is refused so the situation resolves at page level.

### 4C.5 Splitting a note across hosts; continuation notices

- Note content moves forward like body content: a paragraph that cannot
  fit asks for the "next note leaf", preferring in order the next column
  of the same host group, the section follow's first column, then the
  next page (skipping empty pages); an existing follow just beyond is
  reused, else a follow note frame is created and chained. Symmetrically
  a "previous note leaf" search supports flowing back, with an absolute
  stop at the host rendering the reference (note content may never sit
  before its reference's host).
- **Continuation notices** are line-builder features, active only when
  master and follow are on different pages/columns: the master's last
  content line gets an appended right-aligned notice (configurable text,
  counterpart page number substituted) which narrows the line's usable
  width and re-runs its break; the continuation's first line gets a
  prefixed notice. Notices use the paragraph's font; the two page numbers
  are exchanged between the frames at format time.
- Anti-oscillation: notes being (re)arranged or freshly moved carry a
  **backward-move lock** — paragraphs inside may not flow backward. Locks
  are scoped, released after the pass; an unlocked empty note frame is
  destroyed. A **dummy filler line** device handles wrap interplay: when
  a footnote-carrying line is displaced by object wrap, an artificial
  filler line reaching the page margin removes the space that content
  could flow back into, breaking wrap/footnote oscillation cycles.

### 4C.6 Migration when the reference moves

When a content frame carrying references moves to another host:
1. **Collect** — from the old host's first note, gather every note frame
   whose reference is the moved content (optionally only those on hosts
   before the destination); while collecting, fold each note's follows
   back into the master (content pulled in, empty follows destroyed) and
   cut the master from the layout.
2. **Condense** — squash every collected note and its content tree to
   zero height before re-insertion, so an over-tall note cannot blow past
   the destination page before re-measurement ("grow only after
   formatting proves it fits").
3. **Re-insert** at the destination via the normal insertion protocol;
   reformat each note's content under backward-move lock; destroy notes
   that come out empty.
4. Also reformat the first note *after* the inserted ones (its position
   shifted); update per-page numbering on both hosts if pages changed.

When a reference paragraph splits into master and follow text frames,
notes anchored beyond the split are re-bound: if no host change is
needed, only the back-pointer is rewritten on the whole chain; otherwise
the full move protocol runs. Re-merging rolls back the same way. Policy:
notes are *never destroyed* by such rebinding, only moved — destruction
happens solely when the model attribute is removed or the note ends up
empty.

Fit logic exploits mobility: when deciding whether a line fits, space
occupied by notes anchored in *later* frames of the same host may be
counted as available (they leave if this frame splits); as a last resort
before splitting, notes anchored beyond the last line are tentatively
expelled and the fit re-measured.

### 4C.7 Rearrangement pass

A host-level pass takes a deadline and optionally a starting note: caps
the budget via the scoped guard, walks the host's notes in order,
reformats each note frame and its content, letting content move to follow
hosts as the cap dictates; empty note frames are destroyed on the fly.
Two moods: locked (during line formatting — notes only move forward) and
unlocked (afterwards — back-moves permitted so under-filled hosts pull
content back). Editing-stability rule: when the user is editing inside
the first note of a host, that note is measured so it may not push its
own reference line off the host.

### 4C.8 Endnotes — three placement modes

1. **Dedicated endnote pages** (native default): endnotes collect on
   pages appended after the last content page (page style from the
   endnote settings), flagged as endnote pages. Footnotes-at-document-end
   is a separate position mode producing dedicated footnote pages
   inserted *before* the endnote pages. Appending searches forward from
   the reference page to the correct special page, creating it if
   missing, fast-forwarding page by page via registry-position
   comparison when notes are numerous.
2. **Section-end collection**: a section can declare collect-footnotes
   and/or collect-endnotes at section end (with optional own numbering
   and restart values); notes live inside the section's own columns and
   the host search never leaves the section. The **collector pass**: when
   a columned section with collected notes reformats, a document-global
   collector (owned by a per-document layout-helper singleton that also
   hosts the loop watchdogs) is armed for that section; every endnote
   frame in the section chain's columns is cut out into the collector
   (follows folded into masters; duplicates discarded); the columns are
   refreshed and reformatted without them; the collected notes are
   re-inserted after the section's last content via the standard move-in
   protocol. While armed, note-connection requests for that section
   short-circuit — new note frames are handed to the collector instead of
   placed. This cut-out / reflow / put-back-at-the-end cycle keeps
   end-collected notes after *all* section content even as the section
   splits across pages.
3. **Word-compat continuous endnotes**: a synthetic single endnote
   section appended at the end of the body on the last page; endnotes
   flow into it like ordinary content.

Removal helpers strip all note frames (not references) from a host or
the whole layout, and drop dedicated pages when settings change; changing
footnote settings invalidates areas wholesale and rebuilds.

### 4C.9 Columns and sections interplay

- Host iteration order: columns left-to-right within a page, then the
  next page's first column; a section's last column continues in the
  section follow's first column. Notes on a page keep one global order
  across its columns.
- Available-space computation inside a section subtracts the section's
  position and adds its growth potential; with section-end endnote
  collection, the bottom of the last content frame forms an additional
  deadline for footnote-area growth.
- Anchors inside repeated table header rows on follow tables may not
  carry footnotes (the same model row appears on many pages; only the
  first real occurrence anchors notes). Notes are allowed only for body
  content — plus content in page-splittable floating frames (modern
  exception).

### 4C.10 Keep/widow/orphan interaction

- Widow/orphan/keep evaluation is suppressed in two footnote situations:
  (1) for the first content of a note that is the first note of its host
  while its master chain sits on a different host than the reference (the
  continuation must be free to shrink to any size); (2) inside splittable
  table rows (compat-driven). Suppression = keep off, widow zero, orphan
  zero for that decision.
- The fit test driving paragraph splitting integrates notes twice: the
  available height is taken after the note area has shrunk the body, and
  heights of notes that would depart with a split are credited back.

### 4C.11 Termination defenses specific to notes

Backward-move locks, scoped budget guards, the condense-before-move rule,
dummy filler lines — plus the global staged watchdog and the
fingerprint-keyed backward-move suppression registry (§4B.4).

### 4C.12 Numbering upkeep

Modes: per page, per chapter, per document, with offset, prefix/suffix,
distinct number formats for footnotes vs. endnotes, per-section own
numbering. In per-page mode a page-level pass walks the page's body
content in order, renumbering referenced notes (skipping user-fixed
number strings and follow parts), triggered from every operation that
moves notes across pages. With tracked changes displayed in hide-changes
mode, two parallel number series exist (shown/hidden); the active layout
keeps only its own series current.

### 4C.13 Model side

One global index of notes sorted by anchor position; note bodies are node
ranges in a hidden region; document-level settings objects (one for
footnotes, one for endnotes) hold number format, offset, prefix/suffix,
styles, and — footnotes only — the two continuation-notice strings, the
position mode (per-page vs. end-of-document), and the numbering-restart
mode.

---

## 4D. Protocol: table row splitting across pages

Vocabulary ("continuation row", "span-continuation row") is descriptive,
not the reference's naming.

### 4D.1 Entities

- A table renders as a chain of table frames (master + follows) all
  referencing one model table; rows reference model rows, cells reference
  model boxes. Two **layout-only** row kinds reference an existing model
  row without touching the model: the **continuation row** (first
  non-header row of a follow, holding the overflow of a split row's cells
  — the master keeps a partial row) and the **span-continuation row** (a
  stub first row in a follow representing the continuation of vertically
  merged cells whose master row stayed behind). A table remembers with
  one flag whether its follow currently starts with a continuation row.
  Repeated header rows on follows are likewise layout-only clones,
  individually flagged.
- Vertical merges: each model box carries a row-span count — positive N
  on the merge's master box, one on normal boxes, negative on covered
  boxes. Layout can resolve master↔covered in both directions; a merged
  cell's displayed height is the sum over the spanned rows, re-derived
  after every structural change.

### 4D.2 When splitting is attempted

- During the table frame's self-layout loop, after formatting and after
  all join/backflow opportunities are exhausted, the table checks its
  bottom against the parent's usable bottom (the deadline). Growable
  uppers (sections, cells of outer tables, splittable floating frames)
  are probed for growth first and the deadline extended by what they
  grant. Lower rows are reformatted against the deadline; if the table
  then fits, no split. One special maneuver also forces a split: "cut off
  the last row" (§4D.10).
- Preconditions: at least one non-header row; the split-retry budget not
  exhausted; the table is allowed to split *or* has no preceding sibling
  (a table at the very top of its space must split — moving forward would
  change nothing); row-keep constellations can veto. Additional vetoes:
  inside a zero-height columned section; when the table's own upper is a
  cell whose row forbids splitting. A minimum-content check guards
  pointless work: header rows plus all leading keep-together rows (plus
  one more row when whole-row moving) must fit above the deadline, unless
  the table has no predecessor.

### 4D.3 Split-point selection

Available height = deadline − content top − bottom inset; nonpositive →
fail (whole table moves). Walk rows front to back accumulating heights
(summed per row rather than derived from coordinates, because positions
may be stale); the first row overrunning the budget is the split row; the
leftover budget is what a partial row may keep. Zero-height
span-continuation rows are not counted in header bookkeeping.

### 4D.4 Two-attempt strategy and retry accounting

- **Attempt 1 (row split):** keep a partial split row on the master,
  overflow into a continuation row. **Attempt 2 (row move):** if attempt
  1 fails verification (§4D.7) — or is disallowed from the start — re-run
  the whole split with in-row splitting disabled, so the split row moves
  entirely to the follow. The disallow-retry flag resets when the table
  lands under a new parent. Local brakes below the global watchdog: a
  bounded counter on successful attempt-2 splits per parent; a bounded
  counter around discarding valid layout when removing an existing
  continuation row; an oscillation detector on the table's own
  position/size history.
- Exception: if attempt 1 failed *because footnotes grew during it*
  (§4D.12), retry attempt 1 instead of degrading to attempt 2.

### 4D.5 Row-split eligibility

A row may be split inside only if all hold: positive leftover budget; not
fixed-height; not a repeated header row; the model row's split-allowed
attribute is set — with one override: a row taller than an entire page
body (print height minus header/footer) is force-marked splittable
regardless, since it could otherwise hide content forever; no section
frames inside (a single-column section without a nested table is
tolerated); under row-keep, the row does not demand keeping with its
successor; in splittable floating frames, a minimum row height exceeding
the leftover budget disables in-row splitting; nested rows of the split
row that forbid splitting and don't fit force keeping the row. If the
*first* non-header row is the split row and in-row splitting is
disallowed, moving it forward would empty the master: reported as failure
(whole table moves) after consulting the footnote-rescue oracle
(§4D.12).

### 4D.6 Building the continuation

- **Follow creation:** if none exists, create one after the master under
  the same parent with the master's width, cloning header rows into it
  (clones flagged repeated; floating objects anchored in header content
  re-registered on the new page). Existing follows are reused.
- **Header omission:** before cloning, check whether the incoming row
  would even fit under the headers on a fresh page (row height ≤ page
  body height − header height). If not, the follow is built (or stripped)
  headerless — the row gets one header-free page, headers resume after.
  Harsher fallback: when a *header row itself* no longer fits the page,
  repeated headers are disabled by writing zero into the model's repeat
  count — a deliberate, interop-motivated model mutation from layout —
  skipped inside sections (which can grow instead) and splittable
  floating frames; a follow-up guard suppresses "start table on new page"
  after this fallback.
- **Row transfer:** rows after the split point move to the follow
  (inserted after headers/continuation row). Into a brand-new follow they
  are re-parented wholesale (footnotes left to the follow's own
  forward-move); into an existing follow, footnotes anchored in each
  moved row are migrated explicitly (§4C.6 protocol).
- **In-row split:** the split row stays as the master's last row; a
  continuation row referencing the same model row becomes the follow's
  first non-header row; the table's continuation flag is set. In the
  no-in-row-split case, if the departing row contains covered cells of a
  span, the previous row becomes an artificial last row and a
  span-continuation stub is created instead (the span needs a landing
  point in the follow), its cell heights adjusted from the span sums.

### 4D.7 The split-row rebuild protocol (attempt 1's core)

Once the continuation row exists, the master's last row is rebuilt under
the height budget:
1. Nested-table preprocessing (§4D.8).
2. All floating objects in the row are invalidated and temporarily
   banished off-page so they cannot influence measurement.
3. Every cell's content in the row is shrunk to zero height (recursively;
   spanned masters resolved).
4. The table is pre-shrunk by the departing rows' heights plus the
   expected loss of the split row.
5. With master and follow locked against join, and a mode flag telling
   height calculation to ignore the row's minimum-height attribute (the
   minimum constrains the *total* row, not the first fragment) and to
   ignore floating objects for minimum heights, the row is reformatted;
   content that does not fit flows through normal next-leaf logic into
   the matching cells of the continuation row (cell correspondence by
   position; covered cells route to their span master).
6. Nested-table postprocessing (§4D.8).
7. **Verification:** the table (including footnote-area changes, §4D.12)
   must now fit its parent *and* above the page body's bottom; every cell
   of the rebuilt row must contain content (unless covered by a span, or
   the row is itself a continuation row); the rebuilt row must contain
   content; the continuation row must contain content (unless a span
   stub); floating objects must fit. Any violation → revert-style
   invalidation of the row and failure (triggering attempt 2). On success
   the continuation row is invalidated for a fresh format and special
   vertical alignments re-checked.

After a successful split the follow is immediately formatted (recursion
depth capped), its rows reformatted against its page, and the frame after
the table calculated — making the follow valid at once to prevent
join/split ping-pong.

### 4D.8 Nested tables in a split row

- Preprocessing: for each cell of the split row containing nested rows,
  walk the nested rows accumulating minimum heights against the cell's
  share of the budget; the first nested row that overruns is either split
  itself — a nested continuation row is created inside the corresponding
  cell of the outer continuation row, linked master↔follow — or, if it
  cannot split (fixed height, structure too complex, or a minimum height
  that doesn't fit), it and all following nested rows move wholesale into
  the outer continuation row's cell (footnotes migrating along). Even a
  fully fitting nested row gets a follow shell, to keep cell painting
  sane.
- Postprocessing: a nested continuation row whose master ended up empty
  (all content flowed on) is dissolved — the master nested row relocates
  into the follow cell in its place, absorbing the continuation's
  content, so no empty husk remains.
- Recursion guard: an outer table whose own upper is an unsplittable cell
  never enters the split path (§4D.2) — this is what terminates nested
  splitting.

### 4D.9 Vertical merges across the split

Covered cells' minimum heights are computed at the master cell and
apportioned: a span's last row is charged the master-cell minimum minus
the earlier spanned rows' heights. When rows move between master and
follow, spans move as a *group*: the number of consecutive rows
containing covered cells is computed and join/backflow always moves that
many rows together; span heights are recomputed afterwards. A span broken
by a split gets the span-continuation stub (§4D.6); on rejoin, absorbing
a span-continuation row pulls the whole spanned block back.

### 4D.10 Row-keep protocols

- Native: a table-level split-allowed attribute (off = the table never
  splits; its follow chain is joined greedily) plus per-row split-allowed
  and fixed/minimum height attributes.
- Word-compat row-keep: under a document compatibility setting, a row
  keeps with the next row iff the first paragraph in its first cell
  carries the keep attribute (top-level rows only, never nested).
  Effects: the split point slides *backwards* over any chain of
  keep-with-next rows (bounded so at least header + one row remain, with
  an escape when the keep row allows splitting and owns a span master —
  breaking the span by moving the next row is then tolerated); the master
  greedily joins its follow whenever its own last row wants to keep; the
  would-it-fit-forward estimate includes the whole keep chain plus
  headers.
- Escape valves (both needed to avoid unbreakable tables): if *all* rows
  keep with next and the table cannot move forward, splitting is
  permitted anyway; an "emulate table keep" mode (all rows keep, table
  itself has no keep) treats the whole table as keep-with-next relative
  to the following paragraph while still permitting internal splits when
  moving is impossible — skipped for very large tables where the
  emulation is meaningless and expensive.
- **"Cut off the last row":** when the table's last row keeps with the
  paragraph *after* the table, and that paragraph sits on the next page,
  a one-shot maneuver forces a split sending the last row to the follow
  so it can bond with its keep partner. One attempt only, flagged off
  immediately, abandoned if the table is not movable.

### 4D.11 Rejoining (backward flow)

Whenever the master has leftover space it tries to shrink the split: if a
continuation row exists, remove it — its content merges back into the
master's last row (per-cell append; nested continuation rows resolved via
their master↔follow links; span groups pulled in full), the last row is
rebuilt under the same mode flags as §4D.7, and a follow left with
nothing but headers is destroyed. Otherwise, move the follow's first row
group (span-aware) back, migrating its footnotes. Guards: the follow must
pass a backward-move admission test — same fixed width, enough free space
measured to include a first-content-line estimate (header heights +
keep-chain heights + the first row's first-line height, where an
unsplittable first row contributes its full height, computed recursively
through nested cells); rows containing frames force-moved forward by
object positioning refuse to come back (loop prevention); a back-move
lock lets the master run the test on the follow's behalf and record
intent instead of acting. A follow that becomes empty is always joined.
A "prefer join over split" heuristic: when the master is its parent's
last child, has no continuation row, and its follow would fit below it,
the split flag is cleared so the next iteration joins instead of
re-splitting.

### 4D.12 Footnote interplay (the subtle part)

- **Measurement:** before the row rebuild, the heights of all footnotes
  on the host anchored inside this table are summed; after the rebuild
  the sum is recomputed. Growth (e.g. the split moved a reference down
  and its note in) turns an attempt-1 failure into an attempt-1 retry.
  Verification counts footnotes anchored in the *follow* table as credit
  (they leave with the follow anyway), both against the parent's bottom
  and the page body's bottom.
- **The rescue oracle:** when the first usable row can neither split nor
  move (it would be cut off at the page bottom), examine the page's
  footnote area. If any note on the page is anchored on an *earlier* page
  (it was pushed here), verdict: move the whole table forward — legal
  even without a preceding sibling because the page will not be left
  blank (the note stays), and it cannot recur page after page. Else, if
  the space-eating notes are anchored inside the row itself, verdict:
  rearrange the page's notes with the deadline set to the *table's*
  bottom, pushing them below/off so the row gets its space — at most once
  per layout pass. Otherwise no rescue: the row is cut off (degradation
  accepted).
- **Regained-space retry:** after a split, departing rows took their
  footnotes along, so the page bottom may have dropped below the deadline
  the split measured against. If so, the split verdict is annulled once
  (single-shot flag) and the fit re-evaluated — only once, because a row
  pulled back can bring its note back and re-shrink the page (classic
  ping-pong).
- The taller-than-a-page forced-split cap deliberately ignores footnote
  heights (notes return the space when the row leaves; the row's own
  notes travel with it).
- Related brake: an anchored object repeatedly growing its cell during
  positioning is counted per object; growth refused past a bounded count.

### 4D.13 Height rules summary

Fixed-height rows never split and never shrink below the set height; a
Word-compat setting folds border/padding into the "exact" and "at least"
interpretations. Minimum-height rows: the minimum applies to the whole
logical row; fragments after a split are charged minimum-minus-previous-
fragments; during split computation the minimum is ignored entirely (only
content minima bind), except in splittable floating frames where each
fragment enforces it. A row's content minimum = max over its cells
(skipping rotated cells) of cell minima; a cell minimum = sum of
nested-row minima or content heights plus borders, with floating-object
heights included or excluded per the §4D.7 mode flag; span cells
apportioned per §4D.9. Row shrink is clamped to the largest cell's
minimum; the last row of a parent absorbs shrink the parent refuses
(endless-loop guard). During continuation-row filling, growth is
redirected: a growing last-row cell first consumes slack inside the
parent before letting the table grow, and while a continuation row exists
the table's growth is flagged restricted so growth beyond the parent
flows into the continuation row instead of enlarging the master.

### 4D.14 Termination defenses (table-specific recap)

Bounded attempt-2 counter per parent; valid-layout-discard counter;
position/size oscillation detector; one-shot flags for last-row cut-off,
regained-space retry, and own-footnote rescue; span-group atomic moves;
the fingerprint-keyed back-move suppression registry; the global staged
watchdog; a hard recursion cap on nested self-formatting; and a rule that
a follow whose upper changed resets the local counters (new context,
fresh chances).

---

## 5. Stability mechanisms (concept level)

- **Timed autosave / autorecovery journal.** A framework-level service
  snapshots every modified open document into a recovery directory on a
  timer and maintains a small journal of document states. On crash, the
  next start detects the journal and offers recovery of the snapshot
  copies. Emergency-save also triggers from the crash handler. Pointers:
  `framework/source/services/autorecovery.cxx`, `sfx2/source/doc`.
- **In-file versions.** Optional named snapshots stored inside the
  document container; independent of autosave.
- **Backup-on-save.** Optional copy of the previous file version to a
  backup directory before overwriting.
- **Filter tolerance as "repair".** Import filters are written to accept
  malformed input where possible (the ZIP/package layer can also detect
  and partially repair broken containers); there is a user-visible
  "repair" pathway when a document fails to open cleanly.
- **Layout self-defense.** The staged pagination watchdog with
  progress-resetting round counter (§4B.4), the fingerprint-keyed
  backward-move suppressor, the moved-forward registry, per-object
  grow-spiral caps, hard iteration caps convertible to test failures, the
  wrap-move caps (method 21), and the interruptible layout action with
  visible-page cleanup all convert potential hangs into
  degraded-but-terminating layout.
- **Layout cache trust discipline.** The persisted pagination hints are
  validated record by record (whole cache discarded on any violation),
  consumed under a lock/consume discipline, version-gated, and carry a
  quirk flag distrusting data written by known-buggy old versions.

---

## 6. Notes for our project specifically

- Writer validates our core split (immutable model / derived layout /
  renderer as pure traversal) at 35-year scale; their per-view layout
  supports the future "N views over one engine document" case.
- Their idle/interruptible repair loop is the mature form of our
  viewport-culled lazy pagination. The pieces we have not formalized and
  they prove necessary at scale: page-level dirty summaries (O(1)
  clean-page skipping), the backtracking rule when earlier pages
  re-dirty, interrupt-then-clean-up-visible semantics, the one-dirty-
  frame typing fast path, and the watchdog registries keyed by frame
  identity × target-geometry fingerprint (§4B.6).
- The merged-view redline display (method 13) is the strongest available
  design precedent for "hide tracked deletions without re-modeling" and
  aligns with our story/geometry-swap architecture.
- Footnote/endnote negotiation (§4C) and table continuation rows (§4D)
  are the two highest-risk pagination features on our roadmap; both now
  have full sanitized protocol descriptions here, including their
  cross-mechanism rescue protocol (§4D.12) that neither feature can be
  designed without.
- Their DOCX fidelity strategy is semantic + grab bags, *not* byte
  preservation — our sibling-byte-identity is a genuine differentiator;
  adopt grab-bag thinking only inside parts we regenerate, and note their
  bags are selective, not a catch-all: unmodeled elements absent from the
  import grammar are dropped.
- Kashida: their current model is justification-time, font-validated,
  glyph-inserting (§4A.3). Our engine's priority-band chooser matches
  their per-word priority idea; our current "width is an x_advance bump"
  approach corresponds to their *pre-rework* state — the bar they now set
  is real tatweel glyph insertion sized from the font's tatweel advance,
  shaping-level validation of insertion points, and one shared width
  array feeding measure/paint/hit-test so caret geometry can never drift
  from rendering. Also worth copying: no kashida inside fields, tabs
  bounding justification segments, and per-word validity from a trial
  shape of the word.
- RTL geometry: plan for *two* abstractions, not one — an axis-orientation
  accessor for horizontal vs. vertical, and a mirroring flag +
  coordinate-swap boundary for horizontal RTL (§4, method 20). If we can
  make horizontal RTL a first-class accessor variant instead, we avoid
  their every-feature-must-remember-to-mirror tax.

---

## 7. Revision notes (v2)

Each entry: what v1 said → what v2 says, and why.

1. **Kashida validity exposed to cursor logic (v1 §4 method 12, §6).**
   Refuted for current master. The legacy validity-marking APIs no longer
   exist (zero hits in the Writer/edit-engine trees); validity now comes
   from HarfBuzz's safe-to-insert-tatweel glyph flag read via a trial
   word layout (disabled for AAT-shaped fonts). Cursor/selection
   consistency is achieved by the draw, measure, and hit-test paths
   sharing the same widened advance arrays over the cached chosen
   positions — a by-product of shared measurement, not a validity table.
   Method 12 rewritten; §4A.3 added.

2. **"Kashida candidates computed once per paragraph revision" (v1 §3.3,
   method 12).** Stale. Script runs and bidi levels still are; kashida
   positions are now a per-line, per-justification, *font-dependent*
   product (the font's minimum tatweel width gates them) cached back into
   the paragraph structure after adjustment. §3.3 and method 12
   corrected.

3. **"User marks can force or suppress points" (v1 method 12, §6).**
   Clarified: force/suppress are in-text characters, not a marks
   subsystem — a literal typed tatweel is the top-priority forced point;
   a ZWNJ excludes adjacent candidate positions. There is no per-position
   user-invalidation API.

4. **Kashida additions absent from v1:** a separate Syriac outside-in
   algorithm; rendering as actual repeated tatweel glyphs at the device
   layer (not advance widening); the chooser extracted to a shared
   unit-tested i18n utility consumed by both Writer and the edit engine;
   the drop-from-line-start budgeting rule; no kashida inside fields;
   OOXML `lowKashida`/`mediumKashida`/`highKashida` approximated via
   word-spacing caps. Dated by public bug references to the 2024–25 cycle
   (glyph-inserting rework in 24.x; Syriac in 25.2).

5. **"Writing-direction-agnostic geometry accessor covers LTR, RTL, and
   vertical" (v1 method 20).** Half right. The function-pointer accessor
   table has exactly four variants — one horizontal, three vertical — and
   selection is keyed only on vertical-mode flags; horizontal RTL is
   *not* a variant. RTL is carried by a separate lazily-derived per-frame
   flag + explicit mirroring at sibling-positioning/editing sites + LTR↔
   RTL coordinate-swap helpers at the paragraph-frame boundary. Method 20
   rewritten as two composable abstractions; a caveat added that the
   mirroring model is invasive.

6. **RTL tables (v1 implied, unverified).** Now positively confirmed end
   to end — model attribute, `<w:bidiVisual>` DOCX round-trip with unit
   tests, RTF/binary-DOC symmetry, mirrored cell positioning with
   recursive invalidation on direction change, and direction-aware
   editing. Added as method 20a and noted in the §2 Tables row.

7. **DOCX import/export/grab-bag claims (v1 §3.4, methods 15–17).**
   Confirmed by verification, with three nuances folded in: only the DOCX
   tokenizer is grammar-generated (the RTF tokenizer is hand-written but
   emits the same token stream); grab bags exist at nine levels
   (document, style, paragraph, run, shape, frame, table, row, cell) but
   population is selective and re-emission is almost exclusively
   DOCX-export-side; unmodeled elements absent from the grammar are
   dropped — not a catch-all.

8. **"The same hint mechanism seeds layout from foreign-format page-break
   information" (v1 method 10).** Not supported by current master. The
   persisted layout-cache stream is read/written only by the ODF filter;
   DOC/DOCX/RTF neither write nor synthesize it — foreign formats feed
   only a statistics-based page-count estimate for page pre-allocation.
   Sentence replaced; §4B.5 details the cache.

9. **§3.2 pipeline description.** Expanded from the deep dive: the
   root-level one-dirty-frame "turbo" fast path; the five page layout
   dirty bits + at-page-object bit + background-service bits; the actual
   per-page repair nesting (page-anchored objects → structural subtree →
   optimistic-clear content pass) replacing v1's "position first, then
   size, then content"; interrupt-then-clean-up-visible semantics
   replacing v1's "abandons work beyond the visible area"; the off-screen
   skip's one-frame lookahead; the one-job-per-wake idle scheduler with
   completion scan and "layout finished" event.

10. **Method 9 (watchdog).** Refined: the round counter and escalation
    stage reset on progress in either direction (only churn within a
    small page window counts); staged force-validation differs in whether
    anchored objects are included; four additional independent brakes
    documented (moved-forward registry with action re-run, fingerprint-
    keyed backward-move suppression, per-object grow-in-cell counter,
    hard caps with a test-mode hard-failure switch).

11. **Methods 7 and 8.** Expanded into full protocol sections (§4C
    footnotes, §4D table splitting), including material v1 lacked
    entirely: the deadline protocol computed during line formatting; the
    two host budgets (page-style max + reserved body fraction, a stated
    policy divergence from Word); note migration as collect → condense →
    re-insert; continuation notices as line-builder features; the three
    endnote placement modes; the two-attempt split strategy with rebuild
    verification; header omission and the repeat-count-zero interop
    fallback; span-continuation stub rows and atomic span-group moves;
    row-keep escape valves; and the footnote↔table rescue oracle with the
    single-shot regained-space retry.

12. **§2 inventory touch-ups.** RTL/CTL row rewritten (bidi-aware script
    classification, current kashida model, cursor modes, digit
    substitution); Tables row notes RTL column order; Paragraph row notes
    the by-design hyphenation exclusion of Arabic-script languages, CJK,
    and Vietnamese; Filters row notes the kashida-`w:jc` approximation;
    Find & replace row notes the CTL ignore-diacritics / ignore-kashida
    options; Lists row notes Arabic/abjad/Hebrew numbering types.

13. **New deep-dive sections.** §4A (CTL/Arabic end to end: shaping
    layer, mark/cluster discipline, justification, font triplets,
    hyphenation/spellcheck gating, bidi cursoring, honest strengths and
    weak spots) and §4B (incremental relayout: validity model, repair
    walk, idle machinery, watchdog, persisted cache) added from the
    verification round's deep dives. Provenance unchanged (same commit);
    all statements re-verified against the tree on 2026-08-22.
