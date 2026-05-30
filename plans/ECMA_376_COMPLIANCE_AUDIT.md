# ECMA-376 Part 1 — WordprocessingML Compliance Audit

**Audit scope.** Foundational WML primitives defined in the first ~5,000 pages of
ECMA-376 5th Edition Part 1 (`CT_PPr`, `CT_RPr`, `CT_Tbl*`, `CT_SectPr`,
`CT_SimpleField`, `CT_FldChar`, `CT_TrackChange`, plus the toggle / cascade
machinery they depend on).

**Crates reviewed.**
`crates/engine/` (document model),
`crates/layout/` (paragraph + paginator),
`crates/format-docx/` (we have no `format-ooxml` — the OOXML
reader/writer lives here: `schema/ct_ppr.rs`, `schema/ct_rpr.rs`,
`parts/document.rs`, `parts/table.rs`, `parts/styles.rs`,
`parts/settings.rs`, `parts/header.rs`, `writer.rs`).

**Method.** Static read of every WML handler against the spec's element
inventory. Round-trip semantics judged against the harness invariant in
`.claude/rules/docx.md`: unmutated paragraphs ride `source_xml` verbatim
(passthrough — byte-stable); **mutated paragraphs regenerate from the
typed model**, so any field the reader silently drops is silently lost on
the first edit.

**Status legend.**
- `[100% COMPLIANT]` — read, modelled, written, layout/render consumes it.
- `[PARTIAL - MISSED ATTRIBUTES]` — element handled, but spec-mandatory child
  attributes or variants are dropped.
- `[HIDDEN GAP - UNHANDLED]` — element silently ignored by the parser; only
  survives via the passthrough optimisation, lost on first edit.
- `[MODEL-ONLY]` — parsed + round-tripped through writer, but no layout /
  render consumer — semantically a no-op at paint time.

Severity: `High` (breaks rendering or destroys user data on save),
`Medium` (cosmetic drift, wrong-but-readable output),
`Low` (rarely-used, niche, or only matters for collaboration metadata).

---

## A. Core paragraph & run properties (`<w:pPr>` / `<w:rPr>`)

### A.1 — `<w:rPr><w:sz>` and `<w:szCs>` (run font size)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | **High** |

`crates/format-docx/src/schema/ct_rpr.rs::apply_rpr` has no arm for `b"w:sz"`
or `b"w:szCs"`. `engine::SpanStyle::font_size: Option<f32>` exists and the
engine consumes it at layout time, but neither the direct `<w:rPr>` reader
nor the stylesheet `apply_rpr` path ever populates it. Symmetrically,
`format-docx/src/writer.rs::emit_rpr` emits `<w:rFonts>`, `<w:b>`, `<w:i>`,
`<w:strike>`, `<w:color>`, `<w:u>`, `<w:shd>` — but **no `<w:sz>`**. Any
paragraph the engine touches (`dirty == true`) loses its font size on save;
the clean passthrough path masks the bug on untouched paragraphs.

### A.2 — `<w:rPr><w:u w:val="…">` underline variants
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | **High** |

`bridge::common::UnderlineStyle` enumerates `None`, `Single`, `Double`,
`Dotted`, `Dashed`, `Wavy` — the API surface advertises full coverage.
`engine::SpanStyle::underline: Option<bool>` collapses every spec value
(`single`, `double`, `thick`, `dotted`, `dotDash`, `dashDotDotHeavy`,
`wave`, `wavyHeavy`, `dottedHeavy`, `dashLong`, `dashedHeavy`, …) into one
bit. `ct_rpr.rs::apply_rpr` reads `<w:u>` as `Some(true)` for anything that
isn't `"none"`; `writer.rs::emit_rpr` hard-codes `<w:u w:val="single"/>`.
Result: every `double`, `dotted`, `dashDotDotHeavy`, `wave` underline
round-trips as a plain single underline.

### A.3 — `<w:rPr><w:highlight>` named palette
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

`ct_rpr.rs::highlight_color` maps the 16 named values to RGBA, then collapses
into the same `SpanStyle.bg_color` slot as `<w:shd w:fill>`. The writer always
emits `<w:shd>`, never `<w:highlight>`. A document carrying
`<w:highlight w:val="yellow"/>` round-trips as
`<w:shd w:val="clear" w:color="auto" w:fill="FFFF00"/>` — visually identical
in Word, but the original element identity (matters for `<w:highlight>`-aware
processors and the Word "Text Highlight Color" toggle in the ribbon) is lost.
Also `"none"` is not recognized — it falls through to `None` instead of
clearing inheritance.

### A.4 — `<w:rPr><w:shd>` (run-level shading)
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

`ct_rpr.rs::apply_rpr` reads only the `w:fill` attribute. The spec defines
`w:val` (shading pattern: `clear`, `solid`, `pct10`, `horzStripe`,
`diagCross`, 23 patterns total) and `w:color` (foreground for patterned
fills). Both are dropped. Result: any patterned background degrades to a
solid fill.

### A.5 — `<w:pPr><w:spacing w:lineRule>` rules
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` |
| **Severity** | — |

`auto` (240-ths multiple), `atLeast` (minimum, grows for tall glyphs),
`exact` (fixed twips, clips on overflow) are all parsed in
`ct_ppr.rs::apply_spacing` into the `engine::LineHeight` enum, written
back in `writer.rs::emit_ppr`. `layout::paragraph::apply_line_height_rule`
consumes the rule.

### A.6 — `<w:pPr><w:ind w:hanging>` and `<w:firstLine>`
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` |
| **Severity** | — |

Mutual exclusion is enforced (`apply_ind` zeroes the loser). Twips land
verbatim in `engine::Indent`. `layout::paragraph` honours `start_twips +
first_line_twips` for line 0 and clamps `hanging_twips` negatively. Tested.

### A.7 — `<w:pPr><w:ind w:start|left|end|right>`
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` |
| **Severity** | — |

Modern (`start`/`end`) and legacy (`left`/`right`) attribute names both read
into the same writing-direction-relative slots.

### A.8 — `<w:rPr>` text-effect toggles
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Medium (High for `<w:caps>` / `<w:smallCaps>` — visual mismatch) |

The following toggle elements have **no handler** in `apply_rpr`:
`<w:caps>`, `<w:smallCaps>`, `<w:dstrike>` (double-strike),
`<w:outline>`, `<w:shadow>`, `<w:emboss>`, `<w:imprint>`, `<w:vanish>`,
`<w:specVanish>`, `<w:vertAlign>` (superscript/subscript — note: writer
emits this for footnote refs but reader never parses it), `<w:position>`
(baseline shift), `<w:em>` (East-Asian emphasis mark), `<w:effect>`
(animation), `<w:kern>` (kerning threshold), `<w:w>` (character scale %),
`<w:spacing>` at run level (character spacing twips — collides with the
paragraph element of the same name), `<w:lang>` (language for spell-check
+ shaping), `<w:noProof>`, `<w:rtl>` (run-level RTL force), `<w:bCs>` /
`<w:iCs>` (complex-script bold/italic — different toggle from `w:b`/`w:i`),
`<w:cs>` (complex-script toggle).

All are silently dropped from the typed model; survive only on clean
passthrough.

### A.9 — `<w:rPr><w:rFonts>` theme + East-Asian + complex-script attrs
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

`ct_rpr.rs::apply_rpr` reads `w:ascii`, `w:hAnsi`, `w:cs` (string).
**Drops**: `w:eastAsia`, `w:asciiTheme`, `w:hAnsiTheme`, `w:csTheme`,
`w:eastAsiaTheme`, `w:hint`. The four `*Theme` attributes are the
mechanism by which Word documents bind to the active theme part
(`word/theme/theme1.xml`); without them, theme-driven font changes never
land. The whitelist `family_from_docx` recognises three families (Amiri,
Liberation Sans, Noto Naskh Arabic). Anything outside that whitelist —
including `Calibri`, `Times New Roman`, `Cambria` — is silently dropped
even on the recognised attribute names.

### A.10 — `<w:rPr><w:color>` advanced attrs
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Low |

`w:val` hex parses. `w:themeColor`, `w:themeTint`, `w:themeShade` are
dropped — same theme-binding problem as `<w:rFonts>`.

### A.11 — `<w:pPr><w:tabs>`, `<w:pBdr>`, `<w:textAlignment>`, `<w:contextualSpacing>`, `<w:widowControl>`, `<w:suppressAutoHyphens>`, `<w:adjustRightInd>`, `<w:autoSpaceDE>`, `<w:autoSpaceDN>`, `<w:snapToGrid>`
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Medium for `<w:tabs>` and `<w:pBdr>`; Low for the rest |

`apply_ppr` covers `w:jc`, `w:ind`, `w:spacing`, `w:bidi`, `w:keepNext`,
`w:keepLines`, `w:pageBreakBefore`, `w:pStyle`, `w:numPr`. Every other
element falls through the `_ => {}` arm. Custom tab stops (`<w:tabs>`) are
the highest-impact omission — the layout engine has no tab-stop table at
all, so `<w:tab/>` runs (themselves also unhandled, see C.5) cannot
position correctly. `<w:pBdr>` (paragraph borders) silently degrades to no
border on any dirty paragraph.

### A.12 — Run children `<w:br>`, `<w:tab>`, `<w:sym>`, `<w:noBreakHyphen>`, `<w:softHyphen>`, `<w:cr>`, `<w:pgNum>`, `<w:lastRenderedPageBreak>`, `<w:annotationRef>`
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | **High** for `<w:br>` and `<w:tab>`; Medium for `<w:sym>` |

`parts/document.rs` matches `b"w:t"` and `b"w:delText"` inside `<w:r>` and
nothing else. A `<w:br/>` (Shift+Enter line break) inside a run is dropped
— the paragraph reads with no soft break where the source has one. Same
for `<w:br w:type="page"/>` (mid-paragraph page break — even more critical
than `<w:pageBreakBefore>` because it can land anywhere). `<w:tab/>`
becomes invisible; `<w:sym>` symbol characters become invisible; the
"non-breaking hyphen" + "soft hyphen" U+2010/U+00AD glyphs are dropped.

### A.13 — Doc defaults + `basedOn` cascade
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` (within the subset of properties A.1–A.12 cover) |
| **Severity** | — |

`style_resolver::StyleResolver::resolve_paragraph` / `resolve_run` folds
`<w:docDefaults>` → `<w:basedOn>` chain → direct rPr/pPr → run rPr. The
cascade itself is correct; it only carries what `apply_ppr` / `apply_rpr`
parse, so every gap above propagates through it.

---

## B. Tables (`<w:tblPr>`, `<w:trPr>`, `<w:tcPr>`)

### B.1 — `<w:tcPr><w:tcMar>` per-cell margins
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | **High** |

`engine::CellProperties` has **no `cell_margins` field**. Only
`TableProperties::cell_margins: CellMargins` exists (table-level default).
`parts/table.rs::handle_property_inner` has no arm for `b"w:tcMar"` under
either `<w:tcPr>` or `<w:tblPr>`'s `<w:tblCellMar>`. Per-cell margin
overrides are silently dropped; the layout engine never reads even the
table-level `cell_margins` field (see B.2). Cells render without padding.

### B.2 — `<w:tblPr><w:tblCellMar>` table default cell margins
| | |
|---|---|
| **Status** | `[MODEL-ONLY]` (field exists, **never parsed**, never consumed) |
| **Severity** | **High** |

`engine::TableProperties.cell_margins: CellMargins` is declared but
**no XML handler ever writes to it** (grep: zero hits other than the
struct field declaration and the writer's read of the empty default).
Layout / render also do not consume it (grep across `crates/layout`,
`crates/render`: zero references). All cell content renders flush to the
cell border.

### B.3 — `<w:tcPr><w:vAlign>` vertical alignment
| | |
|---|---|
| **Status** | `[MODEL-ONLY]` (parsed, written, **layout ignores**) |
| **Severity** | **High** |

`parts/table.rs` parses `top` / `center` / `bottom` into
`CellProperties.v_align`; `writer.rs` round-trips it. Grep across
`crates/layout` and `crates/render` for `v_align` / `VerticalAlign`:
**zero hits**. Every cell renders top-aligned regardless of the parsed
value.

### B.4 — `<w:tcPr><w:tcW>` cell width + `<w:tblPr><w:tblInd>`
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` |
| **Severity** | — |

`CellWidth::Dxa`, `Pct`, `Auto`, `Nil` all parsed; `tblInd` (table indent)
is read into `TableProperties.indent_twips` and round-tripped. Layout
honours the grid template widths.

### B.5 — `<w:tblGridChange>` and `<w:gridCol w:w>` column geometry
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

`<w:gridCol w:w>` parses; `<w:tblGridChange>` (revision-mark for grid
edits) is silently dropped. Round-trip of a tracked-change document with
grid resizes will lose the diff metadata when the table is dirtied.

### B.6 — `<w:tblPr><w:tblLayout w:type="autofit|fixed">`
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Medium |

No handler for `<w:tblLayout>`. The engine always treats grid widths as
fixed (per the comment at `engine/src/lib.rs:2078`, "Phase 5c will switch
to `<w:tblLayout w:type=\"autofit\"/>`"). Word's default is autofit; our
fixed-layout assumption gives different visual results on every table the
author let Word size automatically.

### B.7 — `<w:trPr><w:tblHeader>` repeated header rows
| | |
|---|---|
| **Status** | `[MODEL-ONLY]` (parsed into `RowProperties.header`, **paginator ignores**) |
| **Severity** | Medium |

Comment at `engine/src/lib.rs:953`: "Phase 5a captures but does not
honour." The paginator does not repeat header rows on page breaks.
Multi-page tables lose their header on every page after the first.

### B.8 — `<w:trPr><w:cantSplit>` (no mid-row pagination)
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` (treated as implicit-on for every row) |
| **Severity** | Low |

Same comment block: "Phase 5a treats this as implicit-on for every row
(no mid-row pagination yet)." Conservative — no data loss — but a tall
cell that should split across pages instead pushes the entire row to the
next page.

### B.9 — `<w:trPr><w:trHeight w:hRule="auto">`
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Low |

Reader maps the bare `<w:trHeight w:val>` without `w:hRule` to `AtLeast`.
The spec default is `auto`. The engine's `RowHeight::Auto` variant exists
but is unreachable through the parser.

### B.10 — Row-level `<w:tblPrEx>` (row property exceptions)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Low |

Per-row overrides to table-wide properties (borders, shading, look) —
silently dropped. Rare in practice.

### B.11 — `<w:tcPr><w:tcBorders>` / `<w:tblPr><w:tblBorders>`
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` (model + round-trip) / `[MODEL-ONLY]` (render side) |
| **Severity** | Medium |

Parse + write all six edges (`top` / `left|start` / `bottom` / `right|end`
/ `insideH` / `insideV`) with `BorderStyle::Single|Double|Dotted|Dashed|None`.
Other styles fall through to `BorderStyle::Other(String)` and round-trip
verbatim, but the renderer only paints `Single`. Other variants paint
as the default stroke or are skipped.

---

## C. Section geometry (`<w:sectPr>`)

### C.1 — `<w:sectPr><w:titlePg>` (different first-page header/footer)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | **High** |

`parts/document.rs::SectPrAccum` has no field for `title_pg` and the
`apply` match has no arm for `b"w:titlePg"`. `engine::Section` carries
exactly one `header_ref` and one `footer_ref`. The paginator
(`crates/layout/src/paginate.rs`) attaches the same header/footer to
every page. First-page differentiation impossible.

### C.2 — `<w:settings><w:evenAndOddHeaders>` (different odd/even pages)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | **High** |

`parts/settings.rs::SettingsPart` only models `default_tab_stop_twips`.
No `even_and_odd_headers` field. The element is silently dropped from
the typed model. Same single-`header_ref` constraint on `Section` would
prevent honouring it anyway.

### C.3 — `<w:headerReference w:type="default|first|even">` attribute
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | **High** (blocks C.1 and C.2 from ever being fixable without schema growth) |

`SectPrAccum::apply` reads `r:id` and overwrites a single slot. The
spec-mandated `w:type` discriminator (`default` / `first` / `even`) is
discarded. The last `<w:headerReference>` in document order wins; all
others lost. Same for `<w:footerReference>`.

### C.4 — `<w:sectPr><w:cols>` (multi-column layout)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | **High** |

No `cols` field on `SectPrAccum` or `engine::Section`; no handler.
Single-column rendering only. A two-column newsletter renders as one
continuous full-width column.

### C.5 — `<w:sectPr><w:pgSz w:orient="landscape">` page orientation
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

`w:w` + `w:h` parse → `PageGeometry.width/height`. The `w:orient`
attribute is dropped, but because `w:w` and `w:h` are already swapped by
Word when the author selects landscape, visual layout is usually correct.
Loss is metadata-only — Word's "Page Setup → Orientation" dropdown reads
this attribute, so a round-trip will leave the dropdown showing
"Portrait" on a clearly-landscape page.

### C.6 — `<w:sectPr><w:pgNumType>` (page numbering reset / format)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Medium (becomes High when D.1 ships — PAGE fields need a numbering origin) |

No handler. Cannot restart numbering at a section boundary; cannot
switch number format (`decimal`, `lowerLetter`, `upperRoman`, ...).

### C.7 — `<w:sectPr><w:type w:val="continuous|nextPage|oddPage|evenPage">`
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Medium |

The section-break *kind* is dropped. The paginator always flushes the
current page at a section boundary (`Paginator::start_new_page` is
unconditional). `continuous` sections (which should flow without a page
break) get a hard page break instead.

### C.8 — `<w:sectPr><w:lnNumType>` (line numbers in the margin)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Low |

Legal-style line numbering — silently dropped.

### C.9 — `<w:sectPr><w:pgBorders>` (page borders)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Low |

Decorative page-edge border. Not parsed.

### C.10 — `<w:sectPr><w:vAlign>` (vertical alignment of page content)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Low |

`top` / `center` / `both` / `bottom` for the body content of the page.
Always renders top.

### C.11 — `<w:sectPr><w:pgMar w:gutter>` / `w:mirrorMargins`
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Low |

`w:top`, `w:bottom`, `w:left`, `w:right`, `w:header`, `w:footer` parse.
`w:gutter` (bind margin) and `w:mirrorMargins` (book-style layout)
dropped.

---

## D. Fields & dynamic elements (`<w:fldSimple>`, `<w:fldChar>`, `<w:instrText>`)

### D.1 — `<w:fldSimple w:instr="PAGE">` and the full simple-field family
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | **High** |

Repository-wide grep for `fldSimple`, `fldChar`, `instrText`, `w:fld`:
**zero hits**. No simple-field reader, no complex-field state machine, no
instruction parser. `PAGE`, `NUMPAGES`, `DATE`, `TIME`, `AUTHOR`,
`TITLE`, `TOC`, `REF`, `HYPERLINK`, `PAGEREF`, `STYLEREF`, `IF`,
`MERGEFIELD` — every field type is invisible to the engine.

**Round-trip behaviour.** A `<w:fldSimple>` *with* a cached
`<w:r><w:t>…</w:t></w:r>` child (Word's typical persistence — the cached
result text rendered last time) **does** appear in the parsed text
because the reader's `Event::Text(t) if in_text_elt` arm matches the
cached display string. But the field instruction is dropped: subsequent
mutations cannot refresh the value (e.g., insert a paragraph above a
PAGE field — the cached "1" stays "1" even though the field now sits on
page 2). A `<w:fldSimple>` *without* cached children (rare but legal)
becomes empty text. Complex fields (`<w:fldChar fldCharType="begin">` →
`<w:instrText>PAGE</w:instrText>` → `<w:fldChar fldCharType="separate">`
→ cached result → `<w:fldChar fldCharType="end">`) work the same way —
cached text survives, instructions and structure are lost. The
passthrough writer salvages clean paragraphs; **the first edit to any
paragraph containing a field permanently strips its dynamic behaviour**.

### D.2 — `<w:hyperlink>` (modelled as a structural element, not a field)
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` |
| **Severity** | — |

`engine::Hyperlink` overlays a byte range with a resolved URL.
`<w:hyperlink w:anchor>` (intra-document anchors) is dropped — the comment
at `engine/src/lib.rs:404` flags this — but external links round-trip.

---

## E. Revisions & collaboration (`<w:ins>`, `<w:del>`, `<w:rsid*>`)

### E.1 — `<w:ins>` / `<w:del>` wrapper round-trip on dirty paragraphs
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` (read-side complete, **write-side absent**) |
| **Severity** | **High** |

`parts/document.rs` parses both wrappers into a `revision_stack`, captures
`w:author` + `w:date`, supports nesting (insertion inside a deletion),
and folds `<w:delText>` into the run text alongside live content.
`engine::Revision` carries the overlay.

Writer side: grep `crates/format-docx/src/writer.rs` for `w:ins` / `w:del`
/ `RevisionKind` / `revisions`: **zero matches in any emit path**. The
writer's regeneration path (`serialize_paragraph` →
`emit_styled_runs_with_objects`) never emits `<w:ins>` or `<w:del>`. Every
revision survives only via the clean-paragraph passthrough; the first
mutation to a paragraph containing tracked changes silently accepts every
insertion and discards every deletion.

### E.2 — `<w:ins w:id>` revision identifier
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

`engine::Revision` lacks an `id` field. The `w:id` attribute that Word's
accept/reject UI uses to address an individual change is dropped. Once
E.1 is fixed, the regenerated wrappers will have fresh ids that don't
match the originals — collaboration servers diffing on `w:id` will see
spurious churn.

### E.3 — `<w:p w:rsidR>` / `<w:rsidP>` / `<w:rsidRDefault>` / `<w:rsidRPr>` / `<w:rsidTr>`
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Medium (becomes High in collaborative-edit scenarios) |

Revision Save IDs — Word's mechanism for telling which save session
produced which paragraph/run/row/table. Not captured anywhere in the
typed model. The writer's `<w:p>` open tag never carries them.
Passthrough preserves them on clean paragraphs only; on mutation, the
regenerated `<w:p>` has no rsid attributes, so Word's merge algorithm
loses the granular provenance it uses for three-way diffs. The doc-level
`<w:rsids>` table in `settings.xml` also rides verbatim, but its
referenced ids will become stale.

### E.4 — `<w:moveFrom>` / `<w:moveTo>` (tracked move)
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Medium |

Tracked-move wrappers — the "moved from X to Y" pair Word uses when the
author drags text. No handler. Drops to invisible on dirty paragraphs.

### E.5 — `<w:pPrChange>` / `<w:rPrChange>` / `<w:sectPrChange>` / `<w:tblPrChange>` / `<w:trPrChange>` / `<w:tcPrChange>`
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Low |

Property-change tracked revisions ("border was X, became Y"). All
silently dropped except via passthrough.

### E.6 — `<w:commentRangeStart>` / `<w:commentRangeEnd>` / `<w:commentReference>`
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` (read + round-trip via passthrough) |
| **Severity** | — |

Range start/end captured into `engine::CommentRange`. The reference
marker is intentionally dropped from the typed model (canvas paints it
invisible) and round-trips byte-identical through passthrough.

---

## F. Supporting machinery

### F.1 — `<w:p>` / `<w:r>` attribute preservation on dirty regeneration
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

The writer always emits bare `<w:p>` / `<w:r>` open tags. Spec-allowed
attributes (`w:rsidR`, `w:rsidRDefault`, `w:rsidP`, `w:rsidRPr`,
`w:paraId`, `w:textId`) are universally dropped on regeneration.

### F.2 — `<w:numPr>` inheritance from `<w:pStyle>`
| | |
|---|---|
| **Status** | `[PARTIAL - MISSED ATTRIBUTES]` |
| **Severity** | Medium |

Comment at `parts/document.rs:183-184`: "We don't inherit either field
from a paragraph style here; that's a separate cascade source Phase 4
ships without modelling." A list paragraph whose `<w:numPr>` lives on the
paragraph style (e.g., the built-in "List Paragraph" style) instead of
the direct `<w:pPr>` will not be recognised as a list item.

### F.3 — `<w:proofErr>`, `<w:permStart>`, `<w:permEnd>`, `<w:bookmarkStart>`, `<w:bookmarkEnd>`
| | |
|---|---|
| **Status** | `[HIDDEN GAP - UNHANDLED]` |
| **Severity** | Low — passthrough rescues these on clean paragraphs |

Spell-check ranges, permission ranges, bookmarks — none modelled. The
spec considers bookmarks load-bearing for `<w:hyperlink w:anchor>` and
cross-references; once D.1 (fields) ships, bookmark IDs will need
modelling for PAGEREF / REF to resolve.

### F.4 — `<w:pPr><w:pStyle>` / `<w:rPr><w:rStyle>` cascade
| | |
|---|---|
| **Status** | `[100% COMPLIANT]` |
| **Severity** | — |

Parsed, resolved through `style_resolver`, flattened into the engine
model. Style identity is lost (no `pStyle` slot on the engine paragraph),
which is intentional per the architecture — but means style-driven
ribbon UI in a hypothetical "Style" picker cannot round-trip the
selection.

---

## G. Aggregate metrics

| Domain | Compliant | Partial | Hidden gap | Model-only |
|---|---|---|---|---|
| A. pPr / rPr | 3 | 4 | 3 | 0 |
| B. Tables | 1 | 3 | 2 | 3 |
| C. Sections | 0 | 2 | 8 | 0 |
| D. Fields | 1 | 0 | 1 | 0 |
| E. Revisions | 1 | 2 | 3 | 0 |
| F. Supporting | 1 | 2 | 1 | 0 |
| **Total** | **7** | **13** | **18** | **3** |

41 spec areas surveyed; **24 carry visible defects** (`Partial` +
`Hidden gap` + `Model-only`).

---

## H. Top-3 critical gaps blocking 100% completion

The following three are the highest-impact entries because each one
**silently corrupts user data on the first save**, and each one is
load-bearing for downstream features already shipped (formatting toolbar,
multi-page layout, collaboration UI).

### 1. `<w:rPr><w:sz>` font size — never parsed, never written (A.1)
The reader does not handle `<w:sz>` and the writer does not emit it. Every
edited paragraph loses its run-level font sizes on save. The bug is
masked by the clean-paragraph passthrough on round-trip-only tests but
will surface the moment a font-size edit goes through `Command::ApplyFormatting`.
**Fix surface:** one arm in `ct_rpr.rs::apply_rpr`, one block in
`writer.rs::emit_rpr`. Also `<w:szCs>` for symmetry.

### 2. Tracked-change writer is empty — `<w:ins>` / `<w:del>` dropped on dirty save (E.1)
Read side captures every revision; write side has no emission path.
Any document with tracked changes that the user *edits* silently has
every insertion accepted and every deletion discarded — irreversible
data loss in collaborative review. Compounds with E.2 (`w:id` missing)
and E.3 (rsids) — once the writer ships, the generated wrappers won't
have stable ids, breaking the receiving Word client's accept/reject UI.
**Fix surface:** `emit_styled_runs_with_objects` must consult
`Paragraph.revisions` and wrap the matching byte ranges; `engine::Revision`
needs a `Option<u32> id` field; the parser needs to capture `w:id`.

### 3. Section/header model is one-slot — `<w:titlePg>`, `<w:evenAndOddHeaders>`, `w:type` discriminator (C.1/C.2/C.3)
`engine::Section.header_ref: Option<String>` admits exactly one header
reference. The spec requires three (default / first / even) keyed by the
`w:type` attribute on `<w:headerReference>`, plus the `<w:titlePg>` and
`<w:evenAndOddHeaders>` toggles to decide which to render where. Every
real-world business document with a distinct cover page or a book-style
spread renders with the wrong header on every page after the first.
**Fix surface:** widen `engine::Section.header_ref` (and footer) to a
3-slot struct keyed by an enum, add `title_pg: bool` to `Section`, add
`even_and_odd_headers: bool` to `engine::DocumentSettings` (which itself
needs to be created in `engine` and populated from `settings.xml`), then
teach `Paginator` to pick the right band based on the page number's
position relative to the section.

---

## I. What to do next

A pragmatic ordering that respects the "additive, never break CI" rule
from [`MEMORY.md`](.claude/projects/-home-ibrahim-Desktop-code-next-gen-editor/memory/feedback_ci_safety.md):

1. **A.1 (`<w:sz>`)** — single-day fix, lifts every dirty-save round-trip
   to font-stable. Goldens unaffected (every existing fixture rides
   passthrough).
2. **E.1 + E.2 (revision writer + `w:id`)** — extends the model and
   writer; covered by a new round-trip fixture with tracked changes
   under `tools/roundtrip/`.
3. **A.2 (underline variants)** — `SpanStyle.underline: Option<bool>` →
   `Option<UnderlineStyle>`; reader, writer, and the renderer's
   underline stroke all touch. Bridge already has the right enum.
4. **C.1–C.3 (titlePg + evenAndOdd + `w:type`)** — schema growth on
   `engine::Section`, paginator extension. Largest gap, biggest
   user-visible payoff.
5. **D.1 (fields)** — full simple+complex field state machine. The
   biggest chunk of work; budget a whole sprint. Until then, `PAGE`
   numbers in cached form silently lie after the first edit.
6. Sweep through A.8/A.11/A.12 (`<w:tab>`, `<w:br>`, `<w:caps>`, …) in
   priority order. Most are leaf-shaped one-line additions to the
   existing fold functions.
