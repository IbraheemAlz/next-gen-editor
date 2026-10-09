---
description: .docx round-trip invariants — preserve everything except what was edited.
paths:
  - "crates/format-docx/**"
  - "tools/roundtrip/**"
---

# `.docx` round-trip rules

## Archive handling
- The reader (`crates/format-docx/src/reader.rs`) stashes **every** non-`word/document.xml` archive entry verbatim in `DocxArchive.other_entries`. The writer (`crates/format-docx/src/writer.rs`) emits those entries byte-identical.
- Don't re-serialize `[Content_Types].xml`, `_rels/.rels`, `word/_rels/document.xml.rels`, or any media/headers/footers. Pass-through only.
- Use `zip = "2"` with `default-features = false, features = ["deflate"]`. Compression method: `Deflated`.

## XML serialization
- `<w:t xml:space="preserve">` on **every** engine-authored text element. Without `preserve`, leading/trailing whitespace gets collapsed; matters for Arabic diacritics + RTL trailing space. Exception (issue #199): a run regenerated inside a *source* run keeps the source `<w:t>`'s attributes, and a source bare `<w:t>` gains `preserve` only when its text now has edge whitespace — the invariant's purpose, without re-spelling every Word run.
- XML escapes: `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`. Quotes don't matter inside character data.
- Header: `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` + `<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">`.
- Footer: `<w:sectPr/></w:body></w:document>`.

## Parser (`quick-xml`)
- `Reader::config_mut().trim_text(false)` — preserve whitespace inside `<w:t>`.
- Match on `e.name().as_ref() == b"w:t"` (byte comparison, namespace prefix included).
- Track `in_text_elt` flag; `Event::Text(t)` only counts inside a `<w:t>` element.
- Close paragraph on `</w:p>`; emit `<w:p>` boundaries into the `DocumentTree`.
- **Byte space (issue #110).** quick-xml strips a leading UTF-8 BOM from
  its input but does **not** count it in `buffer_position()`. Every
  passthrough / grab-bag capture slices the raw part with reader offsets,
  so a part parser must `strip_utf8_bom` first (`parts/document.rs`) —
  never index the un-stripped bytes. Captures go through
  `grab_bag::slice_element`, which refuses a range that does not start
  with the expected start tag and end on `>` (regenerate beats splicing
  a misaligned range). docx4j / Apache POI write BOM-prefixed parts.
- **Nesting cap (issue #111).** `parts/table.rs` recurses one frame per
  nested `<w:tbl>` and re-scans the remaining subtree at each level; the
  walk stops at `MAX_TABLE_NESTING_DEPTH` (64), keeps the deeper subtree
  as an opaque passthrough block, and reports
  `DocxWarning::TableNestingTooDeep` on `DocxArchive::warnings`. Never add
  an unbounded recursion over attacker-shaped input (POI ships a 5000-deep
  17 KB file).
- **Package limits (issue #348).** Every ZIP entry is read through
  `opc::limits::read_entry_bounded` (`take(limit + 1)`) — never
  `Vec::with_capacity(file.size())`: the central directory's declared size
  is attacker-controlled (a 4 GiB claim trapped the wasm worker).
  `PackageLimits` (64 MiB part / 128 MiB package / 10k entries / XML depth
  256 / 4M elements per part; `read_docx_with_limits`, overridable from
  `Command::OpenDocument.limits`) is checked before any typed walk — the
  XML shape caps run over every part the reader may walk (every `.xml` /
  `.rels` entry but custom XML data and `docProps/*` other than
  `core.xml` — the #353 main part can live anywhere), and depth inside a
  table nested past the #111 cap is not counted (it is opaque bytes). Overflow is
  `DocxError::PackageTooLarge` → `Event::Error { kind: PackageTooLarge }`
  (the shell's File-menu banner), never a trap.
- **Measures (issue #349).** Page geometry, `<w:ind>`, `<w:spacing>` and
  table widths go through `schema::measure` (`attr_measure*`): integer or
  decimal twips, ECMA universal-measure units (`in` / `cm` / `mm` / `pt` /
  `pc` / `pi`), never `NaN` / infinite (unusable → the default +
  `DocxWarning::InvalidMeasure`), clamped to ±31 680 twips (Word's 22 in;
  pages ≥ 144) with `DocxWarning::MeasureClamped`. Never parse a measure
  with `f32::from_str` (it accepts `NaN`). Deep helpers report through
  `error::warn` into the read's sink (`collect_read_warnings`; a no-op in
  the writer's re-parses). Verified-reuse equality never uses float `==`:
  `writer::same_section_props` compares geometry by bits.
- **Namespace prefixes (issues #325 / #394).** The parsers match literal
  qnames (`w:p`), so every WordprocessingML part the reader walks — the
  main part, `styles.xml`, `numbering.xml`, `settings.xml`, the note
  parts, `comments.xml` and every header / footer the main part's rels
  name — has its root classified first (`NamespaceScope::classify_root`).
  A part binding WordprocessingML to another prefix (or as the default
  namespace) is rewritten by `schema::ns_normalize::canonicalize_prefixes`
  and reported as `DocxWarning::NonCanonicalNamespaces { part, .. }`; a
  sibling's normalised bytes REPLACE its `other_entries` row, so the
  parsers, the verbatim passthrough, in-place patches (`comments.xml`)
  and the tree's source package all see one spelling. Such a part is
  regenerate-only (its zero-edit save is not byte-identical to the
  source — `tools/corpus-native`'s `normalized_parts`); canonical parts
  and parts the reader never walks stay verbatim.
- **Paragraph borders (issues #352 / #395).** `<w:pBdr>` is read by
  `schema::ct_pbdr` for paragraphs AND styles / docDefaults
  (`parts::styles`). Borders cascade PER EDGE (`ParaProperties::
  merged_with`); an explicit `w:val="nil"` / `"none"` is a set
  `BorderStyle::None` edge (painted by nothing) so it removes an
  inherited one. A logical `<w:start>` / `<w:end>` edge is stored in the
  slot its OWN properties' `direction` names (+ `border_spelling`), and
  every cascade — the reader's `StyleResolver::resolve_paragraph`, the
  engine's `recompute_paragraph_props` / `resolve_style_cascade` — goes
  through `ParaProperties::cascade`, which re-orients each level to the
  paragraph's FINAL direction (`oriented_borders`) before folding, so a
  style's `<w:start>` lands where the paragraph's (cascaded) `<w:bidi>`
  says. Never fold paragraph properties with a bare `merged_with` loop.

## Round-trip diff bounds
The `tools/roundtrip/` harness asserts:
1. **Sibling entries byte-identical** — zero drift on non-`document.xml` entries.
2. **Primary (issue #251): `source_bytes_rewritten == 0`.** An edited save
   must not respell or drop a single ORIGINAL byte; the whole delta must be
   insertion. Superseded the old size-only check, which couldn't tell a
   faithful insertion (which may legitimately mint a new `<w:r>`) from a
   lossy regeneration landing in bounds by coincidence.
3. **Secondary, informational: `document.xml` delta ≤ 2 × UTF-8 byte size
   of the inserted text + a per-new-run allowance** (48 B/run — see
   `tools/corpus-native/src/pipeline.rs::NEW_RUN_ALLOWANCE_BYTES`). The
   bare `≤ 2×N` number (no allowance) is kept as an informational column
   only (`EditCheck::bound_bytes` / `within_bound`).

## In-part grab bags (issue #84)
- A dirty paragraph / table regenerates from the typed model. Every
  `<w:rPr>` / `<w:pPr>` / `<w:tblPr>` / `<w:trPr>` / `<w:tcPr>` child the
  reader does not model is captured **verbatim** into the owner's
  `grab_bag: Option<Box<engine::GrabBag>>` (`SpanStyle`, `ParaProperties`,
  `TableProperties`, `RowProperties`, `CellProperties`) and the writer
  re-emits it, interleaved with the modeled children **in schema order**
  (rank tables in `schema/ct_rpr.rs`, `ct_ppr.rs`, `ct_tbl.rs`).
- The paragraph-mark `<w:pPr>/<w:rPr>` rides the pPr bag whole. Issue
  #369 — Word applies it to the mark (the pilcrow) ONLY: it is never
  folded into the paragraph's runs (a bold mark over plain runs reads
  plain runs). Issue #293 — its modeled children are `Paragraph::mark_style`
  (`schema::ct_rpr::mark_rpr_style`; `None` = not modeled, the bag is the
  truth): typing into an empty paragraph inherits it, `split_at` gives an
  EMPTY half the insertion formatting at the split point, `concat` keeps
  the surviving paragraph's. The writer re-emits the bag fragment while
  `mark_rpr_style(fragment) == mark_style` (also a condition of the
  verified pPr passthrough), else regenerates it from `mark_style` keeping
  the fragment's unmodeled children (`unmodeled_rpr_children`) and the
  source spelling of unchanged ones; the #262 mark revision is re-injected
  after. Harness: `tools/roundtrip` step 40 (`paragraph_format.rs` — with
  the #292 merge and the #297 style names).
- **When you model a new child:** add it to the `*_child_is_modeled`
  predicate *and* emit it through the `PrChildren` sink in `writer.rs`
  at its rank — never both bag it and emit it (duplicate child).
- Bags never cascade (`merged_with` keeps the patch's bag, else the
  receiver's); style definitions carry none. Adjacent spans coalesce only
  when the whole `SpanStyle` — bag included — is equal.
- The writer synthesizes the part root but re-declares the source root's
  attributes (`DocxArchive::document_root_attrs`), so root-bound foreign
  prefixes (`w14:`, `mc:`) stay well-formed on passthrough AND regenerated
  content. A fragment relying on a prefix bound only on an intermediate
  ancestor is dropped at capture (`grab_bag::bound_by_root`) rather than
  written unbound.
- **Two save paths (issues #100 / #134).** The live editor (engine-wasm
  `SaveDocx` / `SaveDocument`) has no `DocxArchive`; it calls
  `format_docx::save_docx(&DocumentTree)`. A tree read from `.docx`
  carries its source package (`DocumentTree::source_package`, every
  non-`document.xml` entry, shared by every undo state via `Arc`, persisted
  once per crash-recovery snapshot with media parts by reference to
  `DocumentTree::media`), so `save_docx` rebuilds the archive and goes
  through `write_docx` — siblings byte-identical, new pictures get media
  parts + rels + content-type defaults (#135, `media_plan.rs`). Only an
  engine-authored tree (no package) falls back to `build_minimal_docx`. So the reader ALSO records the
  roots on the tree: `DocumentTree::document_root_attrs` (document + header/
  footer roots) and `DocumentTree::part_root_attrs` (per-entry, e.g. note
  parts whose root binds prefixes the document root does not). Any part a
  writer regenerates from the tree alone must re-declare the matching
  entry. `check_document_xml_well_formed` / `check_part_xml_well_formed`
  are namespace-aware: an unbound prefix fails the guard.
- **Cell paragraphs (issue #101)** parse through the body run parser
  (`parts::table::parse_cell_paragraph` re-roots the `<w:p>` under the
  part's namespace scope and calls `parse_document_xml`), so cells carry
  the same spans / grab bags / pictures / fields as body paragraphs. Never
  grow a second, cell-only run parser. Issue #284 — the same parse reports
  the paragraph's comment anchor pieces (`parse_document_xml_with_events`),
  and the table walk roots them at the cell (`CommentSink`: a nested
  table's pieces under its `Cell` + `Block` steps, a range marker between
  cell blocks / cells / rows at offset 0 of what follows), so
  `comment_ranges` carries full cell paths; the body parser pairs every
  piece in document order (`pair_comment_events`). The walk recurses once
  per nesting level: keep new per-level work out of line (`#[inline(never)]`
  helpers) — a debug test thread's 2 MiB stack bounds
  `MAX_TABLE_NESTING_DEPTH`.
- Harness: `tools/roundtrip` default mode step 9 edits
  `grab_bag_exotic.docx` and asserts the regenerated `document.xml` is
  byte-identical to the source plus the inserted text.

## Zero-edit resave is byte-identical (issues #112 / #119 / #120)

The real-document corpus (`tools/corpus-native`, `--dump-drift DIR` writes
the original / resaved `document.xml` of every drifting document) is the
gate: a no-edit `read_docx → write_docx` must reproduce `word/document.xml`
byte for byte. The mechanisms, all verbatim bytes the reader captures and
the writer replays:

- **Document envelope** (`DocumentTree::document_envelope`): prolog (BOM,
  declaration, the CRLF after it), the root start tag in its source
  attribute order, the `<w:body>` tag and the tail. The writer never
  re-orders or re-synthesizes them for a document read from `.docx`; it
  only appends a binding the emitted body uses *unbound*
  (`grab_bag::unbound_prefixes` — `a` / `pic` declared inline on
  `<a:graphic>` / `<pic:pic>` do not count). Lives on the tree because the
  live editor saves from the tree alone.
- **Block-level passthrough** (`Paragraph::body_xml` / `Table::body_xml`,
  `schema::block_envelope`): a `<w:sdt>` / `<w:customXml>` envelope becomes
  `BodyFragment::Open` on its first inner block and `Close` on its last —
  the inner blocks stay ordinary body blocks; bookmarks, `proofErr`, range
  markers, comments/PIs and pretty-print whitespace between blocks ride
  `Verbatim` on the following block. The writer's `EnvelopeStack` keeps
  every envelope balanced whatever an edit did to its ends (a closer whose
  opener was deleted is skipped; an unclosed envelope closes at the
  container end). Travel rules: split keeps `before` left and `after`
  right, merge keeps both blocks' markup, clipboard fragments carry none.
  Never re-add an index-keyed side table for these.
- **Self-closing `<w:p …/>`** is one `Empty` event: both walkers
  (`parts::document`, `parts::table`) must handle it, or the paragraph
  vanishes.
- **Verified passthroughs.** A `<w:sectPr>`'s bytes
  (`SectionProps::source_xml`) and a run-level object's bytes
  (`InlineObject::source_xml` — `<w:drawing>`, `<mc:AlternateContent>`,
  `<w:pict>`, `<w:object>`; `schema::drawing::scan_drawing` lowers them)
  are re-emitted only while a re-parse of the bytes still yields the live
  typed fields; a page-setup change or a resize / drag regenerates. An
  object with no picture (shape, chart, OLE) has no regeneration and is
  ALWAYS written from its bytes — never dropped. Text boxes are stories
  (`InlineKind::TextBox`, issue #83) and splice through `parts::textbox`.
- **Markup compatibility (issue #351).** `mc:AlternateContent` reads ONE
  branch at every level — the first `mc:Choice` whose `Requires` prefixes
  the reader understands (`schema::mce`: by URI when declared on the AC /
  choice, else by conventional name — `wps`, `wpg`, `wpc`, `w14`, `w15`,
  `w16*`, `wp14`, `a14`, VML, …), else the `mc:Fallback` — and keeps the
  others as bytes: run level, `scan_drawing` / the text-box lowering scan
  the selected branch of the whole-element capture (the writer's verified
  re-scan decides identically from the same bytes); paragraph level, the
  wrapper is an opener / closer marker pair (`MarkupCapture::
  wrapper_start` — the run-level `<w:sdt>` mechanism, #245); block level
  and between cells / rows, an envelope (`BlockEnvelopes`, like a
  block-level `<w:sdt>`). Inside a cell paragraph the table walker skips
  drawings / AC / text-box stories whole (`parse_cell_paragraph` owns
  them), so a box's own `<w:p>` is never a cell block. An element whose
  prefix the root's `mc:Ignorable` lists and whose namespace the reader
  does not understand (`NamespaceScope::ignores_element`) is never walked
  for content — kept verbatim between blocks / between runs; re-rooted
  parses (cells, text-box stories) re-declare `mc:Ignorable`.
- Known exception: a part with two `<w:body>` elements (POI's
  `MultipleBodyBug.docx`) gets the synthesized header.

## Attribute-level grab bag + in-paragraph source markup (issues #199 / #106)

A *regenerated* (dirty) paragraph stays close to its source bytes through
`Paragraph::source_markup` (`engine::SourceMarkup`, captured by
`schema::source_markup::MarkupCapture` in `parts::document`, replayed by
`writer::serialize_paragraph` / `emit_styled_runs_with_objects`):

- `<w:p>` / `<w:r>` / `<w:t>` attributes (rsids, `w14:paraId` /
  `w14:textId`, `xml:space`) re-emit verbatim; a split gives the paragraph
  identity (`w14:paraId` / `w14:textId`) to the LEFT half only; clipboard
  fragments carry no markup.
- Source run boundaries are writer cut points, so equally formatted
  source runs come back as their own `<w:r>`; consecutive tab / break /
  text segments of one source run share one `<w:r>`. A run's range grows
  with an insertion strictly inside it OR at its end (typing continues the
  run), so a split / extended run keeps its attributes on every piece —
  the inserted text included (the engine mints no rsids).
- **Verified** source bytes: the recorded `<w:pPr>` is re-emitted only
  while `props` / `style_id` / `list_item` equal what it produced and no
  section marker rides the paragraph; a run's `<w:rPr>` only while the
  span style equals the recorded one. Only inside `write_docx` (the source
  package, `styles.xml` included, travels); `build_minimal_docx`
  regenerates. Otherwise the element regenerates and each regenerated
  EMPTY child adopts its source twin when the twin only adds attributes
  other than `w:val` (`schema::source_markup::adopt_source_children`) —
  `<w:u w:color>` survives a bold toggle, a changed font never inherits a
  stale `w:asciiTheme`.
- **Per-child `<w:pPr>` reuse (issue #419).** A recorded pPr that is not
  current as a whole is SPLICED, not regenerated (`writer::ppr_splice`):
  the children `ppr_children` writes for the recorded state
  (`SourcePPr::props` / `style_id` / `list_item` / mark revisions, the
  same read-only clearing as the live side) and for the live one are
  compared by element name — an equal pair keeps the source bytes (or
  the source's ABSENCE: a value only the cascade supplies is never baked
  into direct formatting); a changed child is re-emitted from the live
  model, an empty one keeping the source twin's unowned attributes
  (`w:theme*` dropped; `<w:spacing>` keeps `*Lines` / `*Autospacing`
  only beside an unchanged before / after; `<w:ind>` keeps the `w:left`
  spelling and an unchanged `*Chars`); `<w:pBdr>` splices edge by edge,
  `<w:tabs>` stop by stop (keyed by `w:pos`); a removed child goes, a new
  one is inserted at its rank; whitespace stays with the child it
  precedes. A section-mark paragraph's pPr is recorded too (never
  replayed whole: `source_ppr_is_current` refuses bytes holding a
  `<w:sectPr>`), the `<w:sectPr>` child always the live one — a split's
  left half drops it. Modeled for this: `BorderStroke::space_pt` /
  `shadow` / `frame` (`w:space` / `w:shadow` / `w:frame`, every border),
  `ParaProperties::shading_pattern` (`<w:shd w:val w:color>` beside the
  fill; clearing the shading clears it). Harness: `tools/roundtrip` step
  56 (`ppr_attributes.docx`); `tools/corpus-native`'s `ppr_check` (an
  indent change on the first pPr paragraph respells only `<w:ind>`).
- Positioned verbatim markers (`<w:proofErr/>`, non-TOC bookmarks,
  permission / move ranges, an empty `<w:fldSimple/>`, text-less runs with
  only unmodeled content, pretty-print whitespace) re-emit at their
  (remapped) text offset between runs.
- **Comment anchors (issue #243)** are *verified* markers
  (`SourceMarker::comment`, `schema::comment_anchors`): a
  `<w:commentRangeStart/End/>` replays verbatim only where the tree-level
  `comment_ranges` puts that end of that comment (a comment with no tree
  range — unpaired ends, story anchors — only while it exists), the
  `<w:commentReference>` run only while the comment exists; a deleted
  comment is never resurrected. Every tree endpoint no verbatim byte
  carries (engine-minted comment, stale markup) is synthesized at its
  offset, plus a `CommentReference`-styled reference run after the end
  when the source has none. The plan is published per body write (the
  paragraph serializer has no tree in hand). Anchors are `MarkerRole::
  Verbatim` (dropped when stale — the tree re-synthesizes them) and pass
  through `positioned_markers` with every other marker; anchors inside
  always-kept markup (a #244 content span, a #245 sdt end) count as
  already carried, so nothing is duplicated.
- **Comments on replayed paragraphs (issue #282).** The plan covers clean
  paragraphs too. A tree endpoint no verbatim byte carries (a comment
  added to an untouched paragraph / cell, a reply) is SPLICED into the
  source bytes: the paragraph is regenerated twice
  (`comment_anchors::AnchorMode::Verbatim` = no anchor work, `Patch` =
  plus the missing endpoints) and `schema::anchor_patch::transplant`
  re-applies exactly what `Patch` inserted to the source, aligned over
  XML tokens (a tag never matches part of another tag); the splice is
  verified by re-reading it and falls back to the regenerated paragraph
  (anchors never lost, only respelled). A clean table splices its
  patched cell paragraphs into its own bytes (`patch_clean_table`): each
  is located by searching for its bytes — a prediction, verified by
  re-reading the table (issue #351: an unselected `mc:Choice` the reader
  skips can spell the same paragraph first); a mismatch regenerates the
  table after `comment_anchors::rollback` forgets the reference runs the
  discarded attempt synthesized. Comment markers inside an
  `mc:AlternateContent` are read from the selected branch only (body and
  cell paragraphs alike); a deleted comment's markers leave every branch.
  `delete_comment` tombstones the thread (`DocumentTree::
  deleted_comments`; new ids are minted above every tombstone) and the
  writer strips exactly those ids from the whole written body
  (`comment_anchors::strip_deleted` — replayed paragraphs, block-level
  fragments, always-kept spans; an empty reference run goes whole), so a
  dangling anchor the SOURCE had still round-trips. `comments.xml` is
  patched in place (`parts::comments::patch_comments_xml`: new comments
  appended with minted paraIds, tombstoned ones removed, their
  `commentsExtended` / `commentsIds` / `commentsExtensible` rows too), and
  synthesized for ANY comment on a package without one.
  `tools/corpus-native` reports the `comment_check` (pure insertion /
  re-read anchored / pure deletion).
- `<w:hyperlink>` attributes ride the link itself (`Hyperlink::attrs`,
  issue #242) and re-emit in source order. The source `r:id` is kept only
  while the rels part still maps it to the link's target (*verified* —
  several rows may share one URL, and each link keeps its own); otherwise
  the writer re-resolves by target / mints a row. An internal `#name`
  target re-derives `w:anchor`. Typing at either end of a link stays
  outside it.
- **Content spans (issue #244).** A complex field with no result that
  the model does not represent (legacy form fields: `FORMCHECKBOX`,
  `FORMDROPDOWN`, an empty `FORMTEXT`, `<w:ffData>` in the begin
  `fldChar`) is ONE marker with `MarkerRole::Content`: the whole `begin …
  end` run range (balanced, root-bound), replacing the markers captured
  inside it (name bookmark, text-less runs). Kept out of the field model.
  A field nested in another field's *instruction* is never a span of its
  own (only the enclosing field's span may keep it). Content markers are
  Tier 3: when the offsets go stale they are still written, at the offset
  clamped to the text, and `write_docx_with_notes` reports
  `WriteNote::StaleMarkupClamped` (verbatim markers stay dropped).
- **Run-level wrappers (issue #245).** An in-paragraph `<w:sdt>` keeps
  its runs as paragraph content; its wrapper rides as a marker pair —
  `MarkerRole::Open { id, close_xml }` (`<w:sdt>…<w:sdtContent>`, the
  `sdtPr` subtree skipped whole by the parser) and `MarkerRole::Close
  { id }` (`</w:sdtContent>…</w:sdt>`), `id` = the source byte offset. An
  insertion at the closer's offset lands inside the control. The writer
  (`positioned_markers`) pairs them with a stack (an orphaned closer is
  dropped, an opener that lost its closer — a split — closes with
  `close_xml` at the paragraph end) and checks every pair against the
  wrappers it regenerates (hyperlinks, `ins` / `del`, local fields,
  `nests_with`): a pair that would cross one is widened to a fixpoint and
  the markers are emitted in a constructed order, noted as
  `WriteNote::InlineWrapperWidened`. Row / cell-level `sdt` inside a
  regenerated table ride the table markup (#248, below).
- **Tracked moves (issue #247).** `<w:moveFrom>` / `<w:moveTo>` are
  run-wrapping revisions (`RevisionKind::MoveFrom` / `MoveTo`, text
  semantics of a deletion / insertion; `Revision::move_name` = the
  enclosing range's `w:name`) regenerated like `<w:ins>` / `<w:del>`
  (moveFrom content keeps `<w:t>`); two wrappers over the same range
  nest in source order. The `move*RangeStart/End` markers stay
  positioned verbatim markers. Untracked `insert_text` carries every
  revision with its text (shift at / after the start, grow strictly
  inside).
- **Paragraph-mark revisions (issues #262 / #303).** `<w:pPr><w:rPr><w:ins/>`
  (`<w:del/>`, `<w:moveFrom/>`, `<w:moveTo/>`) — ALL of them, in source
  order (a mark one reviewer inserted and another deleted carries two) —
  is lifted out of the mark-rPr grab-bag fragment into
  `Paragraph::mark_revisions` (`parts::document::split_mark_revisions`;
  `Paragraph::mark_revision()` is the first-change accessor) and recorded
  on `SourcePPr::mark_revisions`; the verified pPr passthrough requires
  them unchanged, a regenerated pPr re-injects them as the rPr's first
  children in schema order (`writer::with_mark_revisions`). Snapshots keep
  the pre-#303 key: one change encodes as the bare revision, several as a
  sequence. The mark travels with the paragraph END (split → right half,
  concat → tail's). `DocumentTree::resolve_all_revisions`
  (`Command::AcceptAllRevisions` / `RejectAllRevisions`, one undo step)
  resolves text revisions per paragraph, then merges paragraphs for
  resolved marks per container from the end — a mark's changes in order,
  any one that removes the mark merges; a single Accept/Reject decides
  the addressed one (by range: the first) — through `splice_text` + `remap_text_edit_record` /
  `remap_paragraph_merge` / `remap_block_splice`, never around them.
  Issue #367 — a removed mark carrying a SECTION BREAK merges too
  (`merge_paragraph_with_next`), by Word's rule (the #70 `delete_range`
  one): the text joins the FOLLOWING section and takes its properties
  (`concat` keeps the tail's `section_end`), and the dropped section's
  header / footer refs backfill the surviving terminal's EMPTY slots
  (owned slots win) — so an own inserted break is removed, not marked.
  Harness: `tools/roundtrip` step 54 (`section_break_revision.docx`).
  Issue #305 — the single `AcceptRevision` / `RejectRevision` is the
  SAME resolver (`DocumentTree::resolve_revisions` with a
  `RevisionPick::Only`, addressed by `engine::RevisionRef`); text leaves
  a paragraph only through `revisions::remove_text` (one overlay-shift
  rule: an inline object whose sentinel was removed goes with it), and
  `markup-assert` checks every inline object still anchors on a U+FFFC.
  Issue #304 — a `revisions_snapshot` row carries a stable
  `revision_id` (`DocumentTree::revision_entries`: a content hash —
  kind, author, date, `w:id`, move name, covered text — probed to be
  unique in document order; nothing stored on the model), which
  `AcceptRevision` / `RejectRevision` take instead of the range, so two
  wrappers over one range — and each change of a mark carrying several
  (`RevisionSlot::Mark(i)`, one row each) — are all reachable; resolving
  either half of a tracked move resolves every move revision sharing its
  `move_name`.
- **Annotation ids (issue #295).** Regenerated content never prints a
  tracked-change annotation `w:id` (`ins` / `del` / `moveFrom` /
  `moveTo` / `rPrChange` / `pPrChange` / …) directly: `writer::
  revision_ids` writes a KEEP-`n` token for an id the model carries
  (`serialize_paragraph` tokenizes its whole output, verbatim run /
  paragraph properties included) and a FRESH token for an engine-made
  revision; `write_docx_inner` resolves them over every regenerated part
  together (body first, then headers / footers, notes) — a KEEP keeps
  its id the first time unless passthrough bytes of its own part spell
  it, the rest get ids above every id in the package (fidelity first: an
  id two source parts already shared is left alone). A run split in two (sub-
  range formatting, a paragraph split, a writer cut) writes its
  `<w:rPrChange>` once with the source id; range markers are never
  rewritten; a save that regenerates nothing is untouched. The reader
  also models a run's `<w:rPrChange>` as a `FormatChange` revision over
  the run (`parts::format_change`: `prev_attrs` = the recorded rPr) —
  the element still rides the grab bag; accepting drops it
  (`SpanStyle::for_typing`), rejecting restores `prev_attrs`.
- **Recording structural tracked changes (issues #301 / #298).** Enter
  with review mode on (`DocumentTree::tracked_split_paragraph`, engine
  `crates/engine/src/tracked.rs`) records the NEW mark — the one ending
  the left half, Word's `<w:ins/>` on the first paragraph — as inserted;
  `split_at` carries the text revisions — and, issue #292, the
  hyperlinks — onto both halves, so `split_paragraph`, tracked Enter, a
  cross-paragraph delete's halves and clipboard slices all do (a
  straddling change is cut once, `tracked::split_revisions`; the right
  piece drops its source id, and `concat` re-joins the two pieces). A
  tracked deletion (`try_tracked_delete_range`; `tracked_delete_range`
  wraps it) works over any range between two paragraphs: per paragraph
  the reviewer's own pending insertions are removed outright (the #265
  path, `revisions::remove_text`), already-deleted bytes are left alone,
  the rest is marked `Delete`; every swallowed mark (one whose merge
  partner on accept — the next paragraph, past any table the range
  deletes whole — is in the range) is marked `Delete` — or, when it is
  the reviewer's own inserted mark, removed (the paragraphs merge through
  `merge_paragraph_with_next`). Issue #365 — tables, Word's way: a range
  crossing a row boundary or entering a table from outside deletes every
  row it touches WHOLE (`RowProperties::revisions` += `Delete` — the
  `<w:trPr><w:del/>` — plus the cells' contents as text; a nested
  table's rows marked too; the reviewer's own inserted row removed
  outright), a range across cells of ONE row deletes each cell's
  sub-range. Only an end that addresses no paragraph is refused
  (`TrackedEditError::NoParagraph`, answered as `Event::Error` — never a
  silent no-op). Tracked Backspace leaves the caret at the START of what
  it marked (Word: it steps over struck text). Issue #366 — a paste with
  review mode on (`tracked_insert_multiline` / `tracked_insert_rich_blocks`)
  records its text as `Insert`s and every mark it creates as inserted
  (a pasted table's rows as inserted rows); replacing a selection marks
  it deleted first. An IME commit goes through the tracked typing path.
- **Table-row revisions (issue #365).** `<w:trPr><w:ins/>` / `<w:del/>`
  (both, in source order) read into `RowProperties::revisions` (modeled
  `CT_TrPr` children — no longer bagged), so the verified `<w:trPr>`
  passthrough covers them; a regenerated `<w:trPr>` emits them at their
  rank (before `<w:trPrChange>`) with #295 id tokens. The resolver
  (`resolve_revisions`) resolves rows in the mark walk, after a table's
  cells and before the paragraph in front of it: an accepted deletion /
  rejected insertion removes the row (`remove_table_rows`, comment anchors
  via `remap_table_cells`; a table left without rows goes via
  `remap_block_splice`). `revision_entries` lists row changes of
  top-level tables as `RevisionSlot::Row { row, index }` (path = the
  table), `revisions_snapshot` rows carry `row`. `<w:tblPrChange>` /
  `<w:trPrChange>` stay verbatim grab-bag bytes (not resolved).
- **Run padding (issue #245).** Pretty-print whitespace inside a source
  `<w:r>` rides `SourceRun::pad` (`open` / `after_rpr` / `close`) and is
  re-emitted on every regenerated piece of the run; a source bare `<w:t>`
  whose text already had edge whitespace (`SourceRun::bare_edge_ws`) keeps
  its bare spelling.
- **Field source form (issue #246).** A field read from `.docx` carries
  `Field::source` (`engine::FieldSource`): a `<w:fldSimple>`'s start tag
  + `</w:fldSimple>`, or a complex field's source prologue (begin run —
  `<w:ffData>` included — through the separate run; the markers captured
  inside it are dropped, the bytes carry them) + its end run. The writer
  (`open_field` / `close_field`) re-emits them while the live instruction
  equals `FieldSource::instruction` and outside a `<w:del>`; a
  `<w:fldSimple>` only while its element nests with every regenerated
  wrapper (`simple_field_nests`), else the complex form.
- **Field phases (issue #350).** Text (`<w:t>`, `<w:delText>`, tabs,
  breaks) never enters the visible paragraph while any open field is in
  its instruction part (`field_code_hidden`) — a nested field's RESULT
  there is code too: `IF { MERGEFIELD x } = …` shows only the IF's
  result, the inner field gets no overlay and joins the outer
  `Field::instruction` as `{ MERGEFIELD x }` (Word's code-view spelling);
  its bytes ride the outer field's source prologue. Nesting is capped at
  32 (`FieldCap`; deeper fields are hidden code), a `separate` / `end`
  with no open field is ignored (its run kept verbatim), and a field
  still in its instruction part at `</w:p>` is closed there
  (`close_open_field_code`) — a stray `begin` can no longer hide every
  later paragraph — with the broken code kept as one content marker
  (`MarkupCapture::close_field_spans`). Result-part fields still span
  paragraphs (TOC). Each case is a `DocxWarning`.
- Offsets are remapped by `delete_text`, `split_at`, `concat` and — for
  every in-place text change — `Paragraph::splice_text` (`engine::
  text_remap`, issues #250 / #252), which returns the `TextEdit` the
  caller also feeds to `DocumentTree::remap_text_edit`, so the source
  markup and the tree-level `comment_ranges` see ONE edit record
  (insert / tracked insert + own-insertion delete / inline objects /
  accept-reject / rich paste / field restamp all route through it — a
  restamp that left the markup stale used to drop the `_GoBack` bookmark
  after a FILENAME field, issue #246).
  `SourceMarkup::text_len` still makes an unaware edit go *stale* in a
  release build (runs / markers ignored, never misplaced); test builds
  (`engine` feature `markup-assert`, on in `cfg(test)` and engine-wasm's
  dev-deps) assert on every `UndoStack::push` that nothing went stale.
- **Start-tag whitespace (issue #248).** `SourceAttr::ws` keeps the
  whitespace before an attribute when it is not one space (a `<w:p>` /
  `<w:tr>` start tag broken over several lines); `attrs_xml` re-emits it.
- `tools/corpus-native` reports `edit_check.source_bytes_rewritten` (bytes
  of the original the edited save rewrote; 0 = pure insertion) — the
  primary bound since issue #251, see "Round-trip diff bounds" above — next
  to the informational size-delta bound, and (when `> 0`) a cheap
  `rewrite_cause` tag (`hyperlink` / `comment anchor` / `form field` /
  `sdt` / `fldSimple` / `move` / `table` / `rPr` / `other`) tracking the
  corpus against issues #242–#249. Two one-byte shapes are tagged first,
  by shape (issue #248): `empty <w:p/>` (#267 — typing into a
  self-closing paragraph rewrites its `/`) and `t preserve` (a bare
  `<w:t>` gaining `xml:space="preserve"` — two insertions the
  single-region metric reports as one rewritten `>`).
- **The regenerator is measured on its own (issue #384).**
  `format_docx::writer::regen_check` runs one ordinary `write_docx` with a
  probe on: every clean paragraph (body and cells, any depth) is also
  regenerated in the exact write context and compared with its source
  bytes; `tools/corpus-native --regen-check` histograms the mismatches by
  class (`proofErr-order` / `whitespace` / `instrText-space` / `smartTag` /
  `mixed` / `other`, the same shapes `classify_rewrite` now tags). The
  classes it found are fixed by construction:
  - **Marker slots.** Every unpaired marker records where it sat among
    the wrapper boundaries at its offset (`SourceMarker::closes_after` /
    `opens_before`, counted by `MarkupCapture` from `wrapper_open` /
    `wrapper_close` / the field `separate` / `end` — a run holding a field
    character is a boundary, any other run ends the stretch). The writer
    builds the ends (field, revision, hyperlink, span tails) and starts
    (span heads, hyperlink, revision, field) at an offset as pieces and
    interleaves the markers by slot (`emit_boundaries`), clamped to what
    is there — a `<w:proofErr/>` before `</w:hyperlink>` stays inside the
    link, an edit that moved a marker can never make it cross a wrapper.
    Content-control openers / closers always sit between.
  - `_Toc*` bookmarks keep their source position as *verified* markers
    (`SourceMarker::toc_bookmark`): replayed while the paragraph still owns
    the bookmark (then it does not wrap the content there), dropped
    otherwise (a split's right half).
  - A TOC's Head / Tail carry `Field::source` too (the verbatim prologue —
    untrimmed instruction, run properties, whitespace — and the end run);
    `emit_span_event` writes them while the instruction is unchanged.
  - In-paragraph `<w:smartTag>` / `<w:customXml>` are opener / closer
    pairs like a run-level `<w:sdt>` (#272); `<w:delInstrText>` is read as
    the instruction of a field inside a deletion (a source prologue is
    reused where its `del`-ness matches); `RunPad::inner` / the lead keep
    whitespace between a run's children; an empty `<w:pict/>` run is kept
    verbatim; a source paragraph with no text mints no empty run and stays
    self-closing when nothing is inside.
  Harness: `tools/roundtrip` step 55 (`regen_classes.docx`).

## `styles.xml` is patched, not regenerated (issue #371)

`ModifyStyle` (`styles_dirty`) no longer rebuilds the part from the
engine's paragraph-style table (which dropped every character / table /
numbering style, `<w:latentStyles>`, unmodeled `<w:docDefaults>` children
and the unmodeled children of the styles it kept). With a source part
(`write_docx`, the UI save path through `source_package` included),
`writer::styles_patch` copies it and re-writes only:

- a paragraph style whose model (`<w:name>`, `basedOn`, `next`, pPr, rPr)
  no longer equals what its source `<w:style>` element produced (the
  part re-parsed — no recorded model needed): spliced child by child
  (`ppr_splice::splice_style` — `<w:pPr>` through the #419 pPr splice, the
  `<w:rPr>` through `splice_rpr`: unchanged children keep their bytes,
  unmodeled ones such as `<w:kern>` / `<w:lang>` / `<w:link>` /
  `<w:uiPriority>` / `<w:rsid>` stay, a changed child adopts its source
  twin by meaning);
- `<w:docDefaults>`, only when the defaults changed (regenerated whole);
- a style the source does not have, appended before `</w:styles>`.

Only the LAST source element of an id is compared (the parsed table's).
`build_styles_xml` remains the path for a package without `styles.xml`
(`build_minimal_docx`, an engine-authored tree). Character / table /
numbering styles are not mirrored into the engine model: the part's own
bytes are the source of truth, and format-docx's `StyleTable` already
models their id / name / type / `basedOn` for the read-time cascade.
Harness: `tools/roundtrip` step 57 (`styles_patch.docx`);
`tools/corpus-native`'s `style_check` (bold toggled on `Heading1` or the
first paragraph style: only that element of `styles.xml` may change).

## Table source markup (issue #248)

A *regenerated* (dirty) table stays byte-close to its source through
`Table::source_markup` / `TableRow::source_markup` /
`TableCell::source_markup` (`engine::TableSourceMarkup`,
`RowSourceMarkup`, `CellSourceMarkup`; captured by `parts::table`,
replayed by `writer::regenerate_table` / `emit_table_row` /
`emit_table_cell`):

- `<w:tbl>` / `<w:tr>` / `<w:tc>` attributes (row rsids,
  `w14:paraId`) re-emit verbatim.
- Each property element (`<w:tblPr>`, `<w:tblGrid>`, `<w:trPr>`,
  `<w:tcPr>`) is an `engine::SourceElement<T>`: `lead` (the whitespace
  before it, always re-emitted with the element), the source bytes and
  the model they produced. The bytes are re-emitted only while the live
  model equals `model` and only inside `write_docx` (the #199 rule);
  otherwise the element regenerates and adopts its unchanged empty
  children's source spelling (`PrChildren::adopt`). `<w:tblGrid>` bytes
  carry `<w:tblGridChange>` (the reader no longer appends its history
  columns to the live grid).
- `<w:tblPrEx>` (#103) is unmodeled: captured whole (its borders no
  longer overwrite the table's) and always re-emitted, first in `<w:tr>`.
- Between rows and between cells, the reader runs one `BlockEnvelopes`
  tracker per level (generic over `schema::block_envelope::
  PassthroughSlot`: blocks, rows, cells): whitespace, range markers and
  `<w:sdt>` / `<w:customXml>` wrappers around rows or cells ride the
  row's / cell's `body_xml` (`before` / `after`), and the writer keeps
  them balanced with an `EnvelopeStack` per level — exactly the #120
  block-level mechanism.
- Nothing is offset- or index-anchored: the markup lives ON the row /
  cell objects, so every table command (row / column insert + delete,
  merge, split, cell typing) carries it with the content it describes; a
  fresh row / cell has none and is written plainly; the property bytes
  re-verify at every write. Never add an index-keyed side table.
- Harness: `tools/roundtrip` step 30 (`table_source_markup.docx`).

## Complex-script run properties (issues #359 / #104 / #249)

- Every Latin run slot has a complex-script twin on `SpanStyle`:
  `font_size` / `font_size_cs` (`<w:sz>` / `<w:szCs>`), `bold` /
  `bold_cs` (`<w:b>` / `<w:bCs>`), `italic` / `italic_cs`, `font_family`
  (`w:ascii`, else `w:hAnsi`) / `font_family_cs` (`w:cs`), `font_theme` /
  `font_theme_cs` (`w:cstheme`). Read apart, cascaded independently (an
  unset twin takes the cascade's twin, never the Latin value — Word's
  rule), written from their own slot. Never fold one into the other.
- Layout picks the set per piece by script class
  (`text_pipeline::is_complex_script`; a grab-bag `<w:rtl/>` / `<w:cs/>`
  forces the twins for the whole run — `SpanStyle::forces_complex_script`).
- Engine-authored formatting sets both (`SpanStyle::with_cs_twins`:
  `ApplyFormatting` with no `font_slot`, ModifyStyle, HTML paste);
  `TextAttrsPatch.font_slot` = `Latin` / `ComplexScript` (the `cs_only`
  flag) narrows it.
- `SpanStyle::char_style` keeps the run's `<w:rStyle>` id next to the
  folded style properties; the writer emits it first. A source
  `<w:rPrChange>`'s recorded `<w:rStyle>` rides `Revision::prev_attrs`
  the same way, so rejecting the change writes it back (#295 × #104).
- The paragraph mark (`Paragraph::mark_style`, #293) goes through the
  same `apply_rpr`, so its twins (`<w:bCs>`, `<w:szCs>`, `w:cs`) are
  modeled there too; `<w:rStyle>` stays verbatim on a mark.
- On/off properties write an explicit OFF (`<w:b w:val="0"/>`).
- A regenerated `<w:rPr>` adopts its source by MEANING
  (`schema::source_markup::adopt_source_rpr_children`): a child whose
  source twin folds to the same model value keeps the source bytes
  (`<w:b w:val="false" />`, `<w:rFonts w:ascii="X" />` without the
  regenerated `w:hAnsi`, `<w:highlight>` for a regenerated `<w:shd>`); a
  changed `<w:rFonts>` / `<w:u>` keeps its unowned attributes
  (`w:eastAsia`, `w:hint`, `w:color`); a source child the model reads as
  nothing (`<w:rFonts w:hint="cs"/>`, `<w:color w:val="auto"/>`) is
  re-emitted. `<w:rStyle>` is never resurrected.
- `raw_font_family` (a family the registry does not ship) follows the
  chosen `font_slot` like `font_family` does (issue #424): a
  complex-script-only edit writes it to `w:cs` and leaves the Latin
  slot alone, a both-slots edit writes it to every slot, and a new name
  at any cascade level replaces both fields — never let a raw name
  rename the other slot or hide behind an inherited family.
- Read-back (issue #423): `SelectionChanged` carries
  `resolved_font_latin` / `resolved_font_cs` + `font_source` (Explicit /
  Theme / Style / Default) and `slot_formats`, so the toolbar and the
  Font dialog show the family the caret's text actually resolves to;
  `attrs_at_caret.font_family` keeps its old meaning.
- Harness: `tools/roundtrip` step 45 (`complex_script_size.docx`, a
  `<w:bCs/>` + `<w:rStyle>` run, a Word-shaped Arabic run), step 47
  (raw family per slot, both storage forms) and step 48 (each Font
  dialog section writes only its own slot).

## Don't add scope you can't preserve
- Phase 1 doesn't preserve formatting runs. Adding partial run support without proper preservation will fail the round-trip diff bound on existing fixtures.
- Phase 2+ will introduce `Run` model with bold/italic/font/size. Add corresponding XML emission only when the parser reads them too.
