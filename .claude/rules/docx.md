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
- The paragraph-mark `<w:pPr>/<w:rPr>` rides the pPr bag whole; its
  modeled children still seed the run baseline (`fold_rpr_fragment`).
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
  grow a second, cell-only run parser.
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
- Positioned verbatim markers (`<w:proofErr/>`, non-TOC bookmarks,
  permission / move ranges, an empty `<w:fldSimple/>`, text-less runs with
  only unmodeled content, pretty-print whitespace) re-emit at their
  (remapped) text offset between runs.
- **Comment anchors (issue #243)** are *verified* markers
  (`SourceMarker::comment`, `schema::comment_anchors`): a
  `<w:commentRangeStart/End/>` replays verbatim only where the tree-level
  `comment_ranges` puts that end of that comment (a comment with no tree
  range — cell anchors, unpaired ends — only while it exists), the
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
- **Paragraph-mark revisions (issue #262).** `<w:pPr><w:rPr><w:ins/>`
  (`<w:del/>`, `<w:moveFrom/>`, `<w:moveTo/>`) is lifted out of the
  mark-rPr grab-bag fragment into `Paragraph::mark_revision`
  (`parts::document::split_mark_revision`) and recorded on
  `SourcePPr::mark_revision`; the verified pPr passthrough requires it
  unchanged, a regenerated pPr re-injects it as the rPr's first child
  (`writer::with_mark_revision`). The mark travels with the paragraph
  END (split → right half, concat → tail's). `DocumentTree::
  resolve_all_revisions` (`Command::AcceptAllRevisions` /
  `RejectAllRevisions`, one undo step) resolves text revisions per
  paragraph, then merges paragraphs for resolved marks per container
  from the end — through `splice_text` + `remap_text_edit_record` /
  `remap_paragraph_merge` / `remap_block_splice`, never around them.
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

## Don't add scope you can't preserve
- Phase 1 doesn't preserve formatting runs. Adding partial run support without proper preservation will fail the round-trip diff bound on existing fixtures.
- Phase 2+ will introduce `Run` model with bold/italic/font/size. Add corresponding XML emission only when the parser reads them too.
