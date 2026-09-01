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
- `<w:t xml:space="preserve">` on **every** text element. Without `preserve`, leading/trailing whitespace gets collapsed; matters for Arabic diacritics + RTL trailing space.
- XML escapes: `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`. Quotes don't matter inside character data.
- Header: `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` + `<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">`.
- Footer: `<w:sectPr/></w:body></w:document>`.

## Parser (`quick-xml`)
- `Reader::config_mut().trim_text(false)` — preserve whitespace inside `<w:t>`.
- Match on `e.name().as_ref() == b"w:t"` (byte comparison, namespace prefix included).
- Track `in_text_elt` flag; `Event::Text(t)` only counts inside a `<w:t>` element.
- Close paragraph on `</w:p>`; emit `<w:p>` boundaries into the `DocumentTree`.

## Round-trip diff bounds
The `tools/roundtrip/` harness asserts:
1. **Sibling entries byte-identical** — zero drift on non-`document.xml` entries.
2. **`document.xml` delta ≤ 2 × UTF-8 byte size of the inserted text.** Tighter is suspicious (probably overwrote unrelated regions). Looser means whitespace creep.

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
- Harness: `tools/roundtrip` default mode step 9 edits
  `grab_bag_exotic.docx` and asserts the regenerated `document.xml` is
  byte-identical to the source plus the inserted text.

## Don't add scope you can't preserve
- Phase 1 doesn't preserve formatting runs. Adding partial run support without proper preservation will fail the round-trip diff bound on existing fixtures.
- Phase 2+ will introduce `Run` model with bold/italic/font/size. Add corresponding XML emission only when the parser reads them too.
