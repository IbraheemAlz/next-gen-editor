# LibreOffice Writer — sanitized product & architecture reference

**Clean-room status.** Produced by a Reader agent per `plans/cleanroom/PROTOCOL.md`.
Contains no source code, no internal identifiers, no constant tables, no
special-case orderings. Directory/file paths are pointers for future Reader
dives only — Implementers must never open them. Provenance: LibreOffice core,
`https://github.com/LibreOffice/core.git`, commit
`3b30dd71049edf71a5c649c9aefa31538542c294` (master, 2026-08-21), MPL-2.0,
studied 2026-08-22.

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
segmentation (ICU), and shaping (HarfBuzz on all platforms).

---

## 2. Feature inventory

Maturity: **full** = mature, decades of fixes; **partial** = works with
known gaps; **absent** = not present.

| Category | Summary | Maturity |
|---|---|---|
| Text/run formatting | Full character formatting: fonts, sizes, weight/posture, underline/strikethrough/overline variants, color + highlight, sub/superscript, letter spacing, kerning, case effects, hidden text, character borders/shading, OpenType feature selection via font-name syntax, character rotation, ruby text. | full |
| Paragraph formatting | Alignment (incl. block-justify with last-line options), indents, spacing, line spacing modes, tab stops with fill characters, borders/shading, hyphenation controls, widow/orphan, keep-with-next, break-before/after with page-style switch, drop caps, outline level, paragraph grid alignment (CJK). | full |
| Styles & inheritance | Character, paragraph, frame, page, list, and table styles; user + built-in; inheritance chains; conditional paragraph styles (style varies by context, e.g. in table vs. body); "autostyles" for direct formatting in ODF. Built-in styles carry a stable programmatic name distinct from the translated UI name. | full |
| Lists & numbering | Ten-level list styles; per-level number format, start value, prefix/suffix, alignment, position modes; list identity separate from list style (multiple lists can share a style); restart/continue controls; legal numbering; outline numbering tied to heading styles; per-node "is counted" toggle. Counting is maintained in a hierarchical numbering tree per list. | full |
| Tables | Nested tables, row split across pages with continuation rows, repeated header rows, vertical/horizontal cell merge, min/max two-pass auto-layout (AutoFit-like), fixed/relative widths, table styles (autoformat), per-cell formulas with a small spreadsheet-like engine, row-keep policies, tables in floating frames that split across pages, change tracking inside tables. | full |
| Sections & page layout | Text sections: multi-column, protected, hidden/conditionally hidden, footnote/endnote collection at section end, file-linked and DDE-linked sections. Page styles (not sections) own page geometry: size, margins, columns, background, footnote area rules; page-style sequencing (first/left/right/next-style chains). OOXML section properties are mapped onto page-style + section machinery on import. | full |
| Headers & footers | Per page style; shared or distinct left/right; distinct first page; independent margins/height with dynamic spacing negotiation against body text. | full |
| Fields | Very large field inventory: page number/count, date/time, document info, references/cross-references, conditional text, hidden text/paragraph, variables (set/get/formula), input fields, database fields, user fields, script fields, drop-downs, chapter fields, authority (bibliography) entries. Expansion values are cached because correct evaluation may need an up-to-date layout. | full |
| TOC & indexes | TOC, alphabetical index, illustration/table/object indexes, user-defined indexes, bibliography; entries gathered from headings, index marks, captions; generated content is a read-only section with tab-stop-driven layout; hyperlinked entries. | full |
| Footnotes & endnotes | Footnotes at page bottom or beneath text; endnotes at document or section end; footnote area growth negotiation against body; footnotes split across pages with continuation notices; per-page or continuous numbering; separator line configuration. | full |
| Comments (annotations) | Anchored at a point or over a text range; threaded replies; resolve state; author/date; shown in a sidebar with connector lines; printable in margin modes; content is rich text held in a small embedded edit engine, not in the page flow. | full |
| Redlining (track changes) | Insert/delete/format/paragraph-format/table-row/move detection; stored as position-sorted range records with author/date/comment stacks (nested rejections); show-changes vs. hide-changes display both supported per view; accept/reject individually or all; compare-documents and merge-documents generate redlines. | full |
| Images, drawing, wrap | Images and drawing shapes anchored to page/paragraph/character/as-character/frame; wrap modes: none, parallel, left/right-only, through, transparent-through, contour wrap with editable wrap polygon; positioning rules relative to many reference frames (margins, page, paragraph area, character cell); captions; image cropping/rotation. | full |
| Text frames | Floating text frames with the full anchoring/wrap system; frames chainable (text overflows frame A into frame B); "text box" mode attaching a text frame to a drawing shape. | full |
| Charts & OLE | Embedded charts (native chart module) can pull data from Writer tables; generic OLE embedding with replacement images for unavailable components. | full |
| Math | Native formula editor (separate module) embedded as OLE; OOXML Math (OMML) import/export mapping. | full |
| Content controls & forms | Modern content controls: rich/plain text, checkbox, dropdown/combo, date picker, picture — mapped to `<w:sdt>` on import/export with in-document interactive widgets. Legacy form fields (fieldmarks) and full form-controls layer also present. | full |
| Mail merge | Database-driven merge (registered data sources), field insertion, condition evaluation, output to printer/file/e-mail; wizard UI. | full |
| RTL/CTL & i18n | Full CTL support: per-script font/size/weight (Western/CJK/CTL triplets on every run), paragraph direction, bidi rendering per UAX #9, HarfBuzz shaping, kashida justification with per-word insertion-point selection and user kashida marks, Hebrew/Arabic numbering options, vertical writing modes for CJK, text grid pages, phonetic guides (ruby), locale-driven break iteration and calendars. | full |
| Clipboard | Rich multi-format transfer: native document fragment, ODF, RTF, HTML, plain text, images; paste-special; drag-and-drop with internal move optimization. | full |
| Find & replace | Plain, regex, and similarity (fuzzy) search; search by formatting attributes and styles; CJK transliteration-aware options. | full |
| Spellcheck & autocorrect | Provider-based linguistics (spell, hyphenation, thesaurus, grammar) with async background checking painting error decorations; autocorrect: replacement tables, capitalization rules, smart quotes, word completion; grammar checking via pluggable providers. | full |
| Master documents | A container document aggregating sub-documents by reference, with shared styles/numbering and cross-document indexes/references; sections implement the aggregation. | full |
| Accessibility | Parallel accessibility tree mirroring the layout frame tree with events on layout change; screen-reader and platform bridges; document accessibility checker tool. | full |
| Print & PDF | Print via metafile record/replay; PDF export with tagged PDF (structure derived from layout + model), PDF/A profiles, PDF/UA, links, outlines, embedded fonts, form export. | full |
| Filters | ODF (reference format, via shared XML framework), DOCX (dedicated import module + dedicated export), legacy DOC binary (import/export), RTF (import/export), HTML (dated), plain text, Markdown (recent addition), EPUB (separate module), WordPerfect/Lotus via import libraries. | full (HTML/Markdown partial) |
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
| Shared edit engine | `editeng` | Lightweight standalone rich-text engine used by comments, drawing text, and other apps |
| Toolkit: fonts/shaping/output | `vcl/source/text`, `vcl/source/font`, `vcl/source/gdi/CommonSalLayout.cxx`, `vcl/source/pdf` | Run segmentation, bidi (ICU), shaping (HarfBuzz), glyph fallback, output devices, PDF writer |
| Drawing model/renderer | `svx`, `drawinglayer`, `basegfx` | Shapes, primitive-based rendering pipeline |
| i18n services | `i18npool` (breakiterator, collator, calendars, locale data), `i18nlangtag`, `i18nutil` | Break iteration (grapheme/word/line/sentence), locale behavior |
| Linguistics | `linguistic`, `lingucomponent` | Provider registry for spell/hyphenation/thesaurus/grammar |
| Tiled rendering / online | `libreofficekit`, `desktop/source/lib` | Paint-tile API, view isolation, callback-based invalidation to remote clients |
| Autosave/recovery | `framework/source/services/autorecovery.cxx`, `sfx2` | Timed snapshots, crash journal, recovery UI |

### 3.2 Data flow: edit → invalidate → layout → paint

1. **Edit.** UI shells call editing facades which mutate the document model
   inside an undo bracket. The model broadcasts change notifications to
   registered listeners — layout frames listen to their nodes.
2. **Invalidate.** Frames receiving notifications set validity flags false
   (position / size / inner "print area" are tracked separately) and
   propagate targeted invalidations (e.g. "my size may change → parent's
   inner area invalid"). Nothing is recomputed at this point; invalidation
   is cheap and idempotent. Pages remember whether they contain any
   invalid content so the repair pass can skip clean pages.
3. **Repair (layout action).** A short-lived "layout action" object walks
   pages front to back, fixing invalid frames: position first, then size,
   then content, re-running as needed because fixing one can re-invalidate
   another. It is interruptible — it polls for pending user input and
   abandons work beyond the visible area, leaving the rest flagged
   invalid. If pages were created/destroyed it can restart. It optionally
   paints as it goes.
4. **Idle continuation.** When the application goes idle, a timer-driven
   idle job resumes full-document formatting in the background (also:
   field updates, background spell/grammar), so the document eventually
   reaches a fully formatted state without ever blocking typing. A busy
   document postpones idle jobs.
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
Justification distributes extra width into glue portions — or, for Arabic
script, into kashida insertion points chosen per word (see §4 method 12).
The paragraph caches derived per-character information (script runs, bidi
levels, kashida opportunities, hidden ranges) used by formatting, cursor
travel, and painting alike. A paragraph also records the character range
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
- **DOCX import** is three-stage: a tokenizer *generated from a
  declarative grammar file* (`model.xml`) turns OOXML parts into a uniform
  stream of typed tokens; a "domain mapper" consumes tokens, maintains
  property-map stacks (run/paragraph/table/section contexts), and issues
  document-API calls; the RTF tokenizer emits the same token stream, so
  RTF and DOCX share the entire mapping layer. Import order quirks of
  OOXML (properties arriving before/after content) are absorbed by the
  mapper's context stacks.
- **DOCX/RTF export** live in the legacy DOC filter area: one tree walker
  traverses the model emitting semantic events to an abstract
  "attribute output" interface; DOCX, RTF, and binary DOC each implement
  that interface. Export logic (what a property *means*) is written once.
- **Fidelity strategy: semantic mapping + grab bags.** LibreOffice does
  *not* preserve OOXML bytes. It maps what it understands into its model
  and stows what it does not understand into "interop grab bags" — opaque
  key/value bundles attached to the document, styles, paragraphs, runs, or
  shapes — which the exporter re-serializes. This recovers round-trip
  fidelity for unknown attributes but not byte identity; ordering and
  formatting of the XML are regenerated. (Our sibling-byte-identical
  approach is strictly stronger for untouched parts; the grab-bag idea is
  still relevant for attributes *within* parts we re-serialize.)

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

3. **Three-flag invalidation with a page-order repair loop.** Frames track
   validity of position, size, and inner area separately; mutations only
   flip flags. A single repair walker fixes pages front to back (a page's
   geometry depends only on earlier pages), re-iterating locally until
   stable. Invalidation is idempotent and cheap enough to over-invalidate
   safely.

4. **Interruptible + idle-resumed layout.** The repair walker polls for
   user input and quits early beyond the visible area; an idle timer
   resumes it later. Prioritized idle jobs (layout > field update >
   grammar) give "type now, paginate later" responsiveness on huge
   documents. Direct analog of our lazy pagination + `ExpandLayout`.

5. **Master/follow flow chains for everything that flows.** Paragraphs,
   tables, and sections all split across pages/columns as chains of
   frames sharing one model object. Flow logic (can I move forward? must
   I move back? do widow/orphan/keep rules bind me?) is factored into a
   shared "flowable" behavior, written once for all flowing kinds. Moving
   *backward* (content returning to a previous page after a deletion) is
   an explicit, separately-handled operation.

6. **Grow/shrink space negotiation.** A child needing space asks its
   parent to grow; the parent may grow itself (asking *its* parent),
   refuse (fixed-height page body), or grant partially. Footnote areas,
   section frames, table cells, and headers/footers all size themselves
   through this one protocol, which is what makes "footnotes squeeze the
   body text" and "cells grow rows grow tables" fall out uniformly.

7. **Table split with continuation rows.** When a table hits a page
   bottom: try to split inside a row — the split row leaves a
   continuation row carrying the overflow of each cell —
   else move the whole row, else the whole table; repeated header rows
   are cloned onto the follow table; row-keep settings and cells spanning
   multiple rows constrain where the cut may fall. The continuation row
   is *layout-only* — the model row is untouched, so editing during a
   split is safe.

8. **Footnote space negotiation.** Footnotes attach to the page (or
   column) that renders their reference line; the page's footnote
   container grows bottom-up, shrinking the body, but never past the
   reference line ("the reference must fit above its note"). If a note
   cannot fit, it splits and continues on the next page with a
   continuation hint; moving a reference to another page migrates the
   note and re-triggers negotiation on both pages. Endnotes are collected
   and emitted at section/document end by a dedicated collector pass.

9. **Pagination loop watchdog with escalating response.** Pagination can
   oscillate (object wrap moves text, text moves the object…). A watchdog
   counts repair rounds within a page window; past thresholds it enters
   staged degradation, ultimately force-validating the oscillating frames
   to guarantee termination — a deliberate "imperfect layout beats a hang"
   policy. Related: per-object "moved forward by wrap" bookkeeping caps
   repeated wrap-driven page pushes.

10. **Persisted layout cache.** On save, page-break positions (and
    paragraph split offsets) are written into the file; on load, the
    layout engine uses them as pagination hints, reproducing the previous
    page breaks instantly instead of reflowing the whole document — and
    validating lazily afterwards. The same hint mechanism seeds layout
    from foreign-format page-break information.

11. **Typed portion chains per line + reformat-range early-out.** (§3.3.)
    The two key wins: every inline special case (field, footnote ref,
    tab fill, bidi segment, ruby) is a first-class portion with its own
    measurement/paint behavior rather than an if-ladder in one function;
    and incremental reformat = start at first dirty line, stop at
    resynchronization.

12. **Per-paragraph script/direction cache and kashida model.** Script
    runs, bidi embedding levels, and kashida candidate positions are
    computed once per paragraph revision and shared by layout, cursor,
    and paint. Kashida justification picks one insertion point per word
    by script-typographic priority classes, expands there, and lets users
    force/suppress points manually; the same cache marks positions where
    kashida is invalid so cursoring and selection stay consistent with
    rendering.

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
    means a new section") are encoded once.

16. **Interop grab bags for unknown OOXML.** Unrecognized attributes/
    elements are preserved as opaque bundles hung on document/style/
    paragraph/run/shape properties and re-emitted on export. Bounded
    engineering cost for long-tail fidelity. For us: relevant inside
    re-serialized parts (`document.xml`), complementing byte-preservation
    of untouched siblings.

17. **Grammar-generated tokenizer + shared domain mapper.** The DOCX
    reader's tokenizer is generated from a declarative grammar of OOXML
    namespaces/elements; DOCX and RTF tokenizers emit one uniform token
    stream consumed by a single mapping layer that maintains context
    stacks (run/paragraph/section/table) and calls the document API.
    Adding an element = grammar entry + one mapper case; the two formats
    share all semantics.

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

20. **Writing-direction-agnostic geometry.** Layout algorithms access
    frame rectangles through a direction-aware accessor layer, so one
    body of pagination code serves LTR, RTL pages, and vertical CJK
    modes. Choosing the abstraction at the *geometry accessor* level
    (rather than if-branching per algorithm) is the transferable idea.

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
- **Layout self-defense.** The pagination watchdog (method 9), wrap-move
  caps (method 21), and the interruptible layout action all convert
  potential hangs into degraded-but-terminating layout.
- **Layout cache lock.** The persisted pagination hints are consumed under
  a lock/consume discipline so a half-read cache never corrupts layout.

---

## 6. Notes for our project specifically

- Writer validates our core split (immutable model / derived layout /
  renderer as pure traversal) at 35-year scale; their per-view layout
  supports the future "N views over one engine document" case.
- Their idle/interruptible repair loop is the mature form of our
  viewport-culled lazy pagination; the front-to-back page walk with
  page-level dirty summaries is the piece we have not formalized.
- The merged-view redline display (method 13) is the strongest available
  design precedent for "hide tracked deletions without re-modeling" and
  aligns with our story/geometry-swap architecture.
- Footnote/endnote negotiation (methods 6, 8) and table continuation rows
  (method 7) are the two highest-risk pagination features on our roadmap;
  both have crisp, transferable protocols here.
- Their DOCX fidelity strategy is semantic + grab bags, *not* byte
  preservation — our sibling-byte-identity is a genuine differentiator;
  adopt grab-bag thinking only inside parts we regenerate.
- Kashida: one insertion point per word by typographic priority matches
  our existing priority-band design; their extra ideas are user-forced/
  suppressed kashida marks and exposing kashida validity to cursor logic.
