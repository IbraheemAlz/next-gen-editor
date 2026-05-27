# FEATURES.md — Next-Gen Editor Capability Catalog

> **Cut:** `v0.6.0-beta.2` (commit `b8d3a7a`).
> **Status:** All Core Sprints (`#9`–`#17`) + Legacy Backlog (pre-`#9`)
> closed. Engineering-complete; v0.1.0 MVP pending external D5.6
> (security audit) + D5.9 (operator runbook) + D5.10 (Arabic
> typography sign-off).

This document catalogues every capability that is **actually
implemented and functional today**. Deferred or planned items are
listed at the bottom under "Tracked follow-ups" — they are NOT
working features.

---

## 1. Core architecture

| Capability | Where |
|---|---|
| **WASM-first Rust engine** — pure-Rust document model + layout + rendering compiled to `wasm32-unknown-unknown`; no main-thread WASM | `crates/engine-wasm/` |
| **Single dedicated Web Worker** — WASM loaded exactly once; `OffscreenCanvas` transferred at INIT and never re-transferred | `ts/src/engine/engine.worker.ts` |
| **Cross-origin isolated** — Vite serves COOP `same-origin` + COEP `require-corp` + CORP `same-origin`; `crossOriginIsolated === true` invariant | `ts/vite.config.ts` |
| **SharedArrayBuffer support** — gated on `crossOriginIsolated` for the 50 MB zero-copy round-trip (`sab-transfer` e2e proves < 50 ms) | `ts/e2e/sab-transfer.spec.ts` |
| **Headless UI shell** — Solid.js mounts the canvas + DOM chrome; engine never touches the DOM | `ts/src/index.tsx` |
| **Crash recovery** — WASM trap → worker `self.close()` → `EngineClient.onTrap` → UI bumps `canvasGen` signal → fresh `<canvas>` remounts → respawn + `Command::Recover` | `ts/src/engine/engine-client.ts`, `e2e/crash-recovery.spec.ts` |
| **IndexedDB event log** — commands + snapshots in one `engine-log` DB; logged OFF the critical path so RPC reply fires first; sustains 1000+ cmds/s | `ts/src/event-log.ts`, `e2e/rpc-throughput.spec.ts` |
| **Cold boot < 500 ms** — engine spawn + WASM compile + first paint; e2e gate enforced | `e2e/boot.spec.ts` |
| **WASM artifact ≤ 15 MiB** — CI gate; current size 6.28 MiB (≈ 40 % of budget) | `.github/workflows/release.yml` |
| **Memory budget ≤ 256 MiB / worker on 50-page doc** — e2e gate | `e2e/memory-50p.spec.ts` |
| **Monaco-Standard SDK split** — `@nge/core` (Locked Surface + Headless API, pure Solid primitives) and `@nge/ui` (CSS-namespaced default UI shelf) ship as separate libraries; downstream UI imports ONLY from `@nge/core` | `packages/core/`, `packages/ui/` |
| **Pure CSS namespace** — every UI selector prefixed `.nge-`; zero Tailwind / Shadcn / MUI / external UI libraries | `packages/ui/src/*.css` |
| **Typed bridge** — `Command` / `Event` enums tagged `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]`; `tsify-next` generates the matching `.d.ts` | `crates/bridge/` |
| **Zero-copy binary bridge** — `Vec<u8>` fields with `serde_bytes` + `#[tsify(type = "Uint8Array")]` cross as native `Uint8Array`; `String::into_bytes()` reuses the underlying allocation | `crates/bridge/src/{command,event}.rs` |
| **Telemetry pipeline (mock)** — `crates/bridge/src/telemetry.rs` schema; UI collector batches + `console.log`s every 60 s (mock transport for the MVP) | `ts/src/state/telemetry.ts` |
| **Tag-triggered release pipeline** — `v*` tag builds WASM + SBOM + static site → GitHub Release | `.github/workflows/release.yml` |

---

## 2. Text & typography

| Capability | Where |
|---|---|
| **Per-line BiDi** — UAX #9 runs per line via `unicode-bidi`; visual order never flattened across line breaks | `crates/text-pipeline/src/bidi.rs`, `crates/layout/src/paragraph.rs` |
| **Per-script font fallback** — `FontStack` resolves Latin / Arabic / other scripts to the best-covering face; falls through `fallback_chain` | `crates/text-pipeline/src/fonts.rs` |
| **Native RTL** — base direction propagates to the paragraph; `dir="rtl"` mirrors selection rects + caret movement | `crates/layout/`, `packages/core/` |
| **HarfBuzz-equivalent shaping** — `rustybuzz`-driven glyph runs with `cluster` byte offsets back into the source text | `crates/text-pipeline/src/shape.rs` |
| **Bold / italic** — real face when loaded; otherwise faux synthesis (Canvas2D `shadow-blur` for bold; CSS `transform: skewX` for italic) | `crates/layout/src/paragraph.rs::build_line` |
| **Underline + strikethrough** — every UAX-#14 underline variant (Single / Double / Thick / Dotted / Dashed / DashDotted / Wave / DoubleWave / DottedHeavy / DashedHeavy / DashLongHeavy / DashDottedHeavy / DashDotDottedHeavy / DashLongHeavy / WavyHeavy / WavyDouble); strike rendered as a horizontal stroke | `crates/render/src/scene.rs` |
| **Foreground + background colour spans** — `SpanStyle.color` + `bg_color` round-trip via `<w:color>` + `<w:highlight>` / `<w:shd>` | `crates/engine/`, `crates/format-docx/` |
| **Font-size spans** — `<w:sz>` / `<w:szCs>` (CJK + complex-script) round-trip | `crates/format-docx/src/parts/document.rs` |
| **Font-family spans** — typed `FontFamily` enum + raw `<w:rFonts w:ascii>` passthrough for unloaded faces | `crates/engine/src/lib.rs` |
| **Super- / subscript** — `<w:vertAlign>` round-trip; pen Y offset baked into `SpanStyle.baseline_shift_px`; px-size shrunk for proper small-cap height | `crates/layout/src/paragraph.rs` |
| **smallCaps with cluster-safe byte-length expansion** — `transform_for_shape` builds a per-byte transformed→source map so `ß → SS`, ligature `ﬁ → FI`, `ŉ → ʼN`, etc. never panic on a mid-char slice | `crates/layout/src/paragraph.rs::transform_for_shape` |
| **`<w:caps>` / `<w:smallCaps>`** — shape-time uppercase transform with cluster remap | (same) |
| **UAX-#29 accurate word count** — `icu_segmenter::WordSegmenter::new_auto` in a `thread_local!`; CJK / Thai / Khmer (whitespace-less scripts) report meaningful counts | `crates/engine/src/lib.rs::count_uax_words` |
| **Justified text** — Kashida priority-band stretching (Microsoft P1–P5) for Arabic; word-space stretching for Latin; weighted mixed-script split | `crates/text-pipeline/src/justify*.rs` |
| **Kashida ink** — real tatweel `U+0640` glyph injection (not just `x_advance` bump); per-priority-band stroke selection | `crates/text-pipeline/src/justify_kashida.rs` |
| **ICU line-break opportunities** — `LineSegmenter::new_auto` in a `thread_local!`; greedy fit at break opp boundaries | `crates/text-pipeline/src/line_break.rs` |
| **CSS `overflow-wrap: anywhere` fallback** — `char_break_fit` force-breaks unbreakable tokens at character boundaries when no break opp fits | `crates/layout/src/paragraph.rs::char_break_fit` |
| **`.docx` `<w:rPr>` round-trip** — full SpanStyle round-trip for every emitted run | `crates/format-docx/src/{reader,writer}.rs` |

---

## 3. Paragraph & layout

| Capability | Where |
|---|---|
| **Hierarchical box model** — `PageBox → ParagraphBox → LineBox → VisualRun → PositionedGlyph`; parent-relative origins; renderer is a pure traversal | `crates/layout/src/boxes.rs` |
| **Paragraph alignment** — Start / End / Center / Justify; respects base direction (Start RTL = right; End RTL = left) | `crates/layout/src/paragraph.rs::alignment_origin_x` |
| **Indentation** — `<w:ind w:start w:end w:firstLine w:hanging>` round-trip; first-line vs hanging applied per-line | `crates/engine/`, `crates/layout/` |
| **Line spacing** — `<w:spacing w:line w:lineRule="auto">`; per-line dynamic height grows to host larger glyphs / inline images | `crates/layout/src/paragraph.rs::line_extents` |
| **Paragraph borders + shading** — per-edge `<w:pBdr>` + `<w:shd>` round-trip | `crates/engine/src/lib.rs::Paragraph` |
| **Geometric tab stops with kinds** — `Left` (default grid) / `Center` (segment midpoint at stop) / `Right` (segment right edge at stop) / `Decimal` (first `.` or `,` at stop; falls back to Right when no separator); shape-then-place math; 4 px floor when segment overflows stop | `crates/layout/src/paragraph.rs::apply_tab_advances` |
| **Indent-aware tab pen** — pen starts at `leading_off` so tab stops compare directly in paragraph-content-relative coordinates | (same) |
| **Default tab grid** — half-inch (`DEFAULT_TAB_GRID_PT = 36.0`) fallback when no custom stop fits | (same) |
| **Multi-column sections** — `<w:cols w:num w:space>` snake flow; `cur_blocks` flush at page boundary | `crates/layout/src/paginate.rs` |
| **Continuous section column balancing** — greedy O(n) snake-fill before continuous-break section swap; columns end within ±1 pt of average on the title + 2-col body + footer pattern | `crates/layout/src/paginate.rs::balance_current_section_columns` |
| **Section breaks** — Continuous / NextPage / EvenPage / OddPage; in-place geometry / column / page-numbering swap on Continuous | `crates/engine-wasm/src/lib.rs` |
| **Page numbering** — `<w:pgNumType>` start + format (Decimal / LowerLetter / UpperRoman / …); section-relative formatting for `PAGE` field | `crates/engine/src/lib.rs::PageNumType` |
| **PAGE / NUMPAGES fields** — paginator evaluates per-page, stamps the resolved text into the paragraph for emit | `crates/layout/src/paginate.rs` |
| **Headers + footers (3 slots)** — Default / First (via `<w:titlePg>`) / Even (via `<w:evenAndOddHeaders>`) | `crates/engine/`, `crates/layout/` |
| **Footnotes** — body band with per-document marker resolution + footnote bodies pre-laid out; reference glyph rendered as superscript | `crates/engine/`, `crates/layout/`, `crates/render/` |
| **Pagination with row-boundary splits** — tables split at row boundaries; single oversize line accepts page-clip atomically | `crates/layout/src/paginate.rs::push_table_split` |
| **Hard page breaks** — `<w:br w:type="page"/>` honoured | `crates/engine/src/lib.rs::Paragraph::page_break_before` |
| **Dynamic line height** — line grows beyond `line_height` when an inline image / large glyph exceeds the box | `crates/layout/src/paragraph.rs::line_extents` |
| **Multi-level numbered lists** — 9-level abstract numbering definitions; per-instance `NumInstance` counters; `resolve_markers_in_place` resets deeper levels when shallower levels advance; format templates (`%1.`, `%1.%2.`, …) | `crates/engine/src/numbering.rs` |
| **Bullet + numbered list synthesis** — engine-toggled lists mint fresh `AbstractNum` + `NumInstance` pairs; subsequent toggles reuse the same templates (idempotent across 20+ toggles) | `crates/engine/src/numbering.rs::synth_list_definition` |
| **`<w:numFmt>` glyph sets** — Decimal / LowerLetter / UpperLetter / LowerRoman / UpperRoman / Bullet / Hebrew / Aiueo / IroIro / DecimalEnclosedCircle | `crates/engine/src/numbering.rs::NumFmt` |
| **Interactive Ruler** — start / first-line / right indent + tab-stop handles; pointer drag dispatches `cmd.setParagraphIndent` / `cmd.setTabStops` per gesture release (one drag = one undo entry); drag-off-strip removes a tab stop (Word-conformant) | `packages/ui/src/Ruler.tsx` |
| **Per-paragraph borders + shading** — round-trip + UI dispatch | `packages/ui/src/ParagraphControls.tsx` |

---

## 4. Tables

| Capability | Where |
|---|---|
| **Insert / delete table** — `Command::InsertTable { at, rows, cols }` + `DeleteTable { path }`; default Word borders applied | `crates/engine/src/lib.rs::insert_table` |
| **Insert / delete rows + columns** — `Before` / `After` sides resolved at the bridge boundary | `crates/engine-wasm/src/lib.rs::do_insert_row`, … |
| **Merge / split cells** — `Command::MergeCells` / `SplitCell` with bridge-side `(from_row, from_col, to_row, to_col)` | `crates/engine/src/lib.rs::merge_cells` |
| **Cell shading** — `<w:shd>` round-trip; UI via `CellPropertiesDialog`; `Command::SetCellShading` | `crates/format-docx/`, `packages/ui/src/CellPropertiesDialog.tsx` |
| **Per-edge cell borders** — `<w:tcBorders>` with per-edge stroke + colour + style (Single / Double / Dotted / Dashed / None / Other); `Command::SetCellBorders` | (same) |
| **Vertical-merge spans** — `VMergeRole::{Restart, Continue, None}` round-trip; `Restart` cell visually owns the merged region | `crates/engine/src/lib.rs::VMergeRole` |
| **Horizontal-merge (grid_span)** — `<w:gridSpan>` round-trip; emits proper `colspan` in HTML export | `crates/engine/`, `crates/format-html/` |
| **Cell margins** — per-cell `<w:tcMar>` + table-level `<w:tblCellMar>` resolution; subtracts from content width before paragraph layout | `crates/engine/src/lib.rs::CellMargins` |
| **Table autofit (`<w:tblLayout w:type="autofit"/>`) with `min-content` floor** — `measure_unbreakable_width` walks `break_opportunities` + sums `shape_text.total_advance` per segment; 3-case dispatch (overflow / fit / iterative pin-and-redistribute); column never shrinks below its longest unbreakable atom; long URLs / hashes overflow horizontally rather than mid-character clip | `crates/engine-wasm/src/lib.rs::autofit_distribute` |
| **`<w:tblGrid>` honoured** — explicit grid widths from the source `.docx` survive as a soft floor across autofit | (same) |
| **vMerge height accumulation** — `Restart` cells absorb subsequent `Continue` row heights; v-align reapplied to the merged region | `crates/engine-wasm/src/lib.rs::accumulate_vmerge_heights` |
| **Nested tables** — recursive `autofit_distribute` + per-cell layout; renderer + paginator handle nested geometry | (same) |

---

## 5. Advanced editing

| Capability | Where |
|---|---|
| **Bounded undo + redo** — `UndoStack` depth 100; pushing a new snapshot truncates the redo branch | `crates/engine/src/undo.rs` |
| **Persistent-vector document model** — `im::Vector<Paragraph>`; O(1) clone for snapshots; structural sharing across the undo stack | `crates/engine/src/lib.rs::DocumentTree` |
| **Insert / delete / replace text** — engine-side primitives + bridge commands; tracked-changes-aware on the interactive path | `crates/engine/src/lib.rs` |
| **Range selections** — `LogicalRange { start, end }` with linear + per-cell `TableCells` kinds | `crates/bridge/src/common.rs::LogicalRange` |
| **Selection rects per line** — `selection_rects_geom` projects geometry from the box tree; multi-line + table-cell rects | `crates/engine-wasm/src/lib.rs` |
| **Hit-testing** — pixel → logical position via `CaretSlot` table; per-line absolute-x → source-byte map | `crates/engine-wasm/src/lib.rs::document_geometry` |
| **Caret movement** — arrow keys with ideal-x preservation, Home / End, Page Up / Down, Ctrl+Home / Ctrl+End | `ts/src/components/HiddenInput.tsx`, `crates/engine-wasm/` |
| **Word + paragraph selection** — double-click word, triple-click paragraph, `Ctrl/Cmd+A` select-all | (same) |
| **Shift-extend selection** — Shift+arrow / Shift+click / Shift+End | (same) |
| **Live style table — assignment** — `Command::ApplyStyle { range, style_id }` re-cascades resolved `props` through `defaults → style chain → direct_overrides`; `direct_overrides` shadow preserves user-typed bold across a style swap | `crates/engine/src/lib.rs::set_paragraph_style` |
| **Style cascade resolver** — `<w:basedOn>` chain capped at `MAX_STYLE_CHAIN = 10` per ECMA-376 §17.7.4.5 | `crates/engine/src/lib.rs::resolve_style_cascade` |
| **`<w:pStyle>` writer** — emit `<w:pStyle w:val>` when `Paragraph.style_id.is_some()`; `styles.xml` rides OPC passthrough byte-identical otherwise | `crates/format-docx/src/writer.rs` |
| **Track Changes recording** — parallel-API + bridge-side gate: `tracked_insert_text` / `tracked_delete_range` / `tracked_format_change` sit alongside the original mutators; `Revision` lives INSIDE the document tree (`Paragraph.revisions`); undoing a tracked Insert pops both text and revision metadata in one snapshot | `crates/engine/src/lib.rs::tracked_*` |
| **Tracked-Insert merge by same author** — typing inside an own-author Insert extends it (no per-keystroke fragmentation); adjacent same-author Inserts merge into one revision | `crates/engine/src/lib.rs::tracked_insert_text` |
| **Tracked-Insert inside a Delete** — splits the Delete around the insertion point + stamps a fresh Insert in the gap (typing inside `<w:del>` logically replaces deleted text) | (same) |
| **Track-Changes toggle binding** — `Event::SelectionChanged.is_tracking_changes` reactive signal; UI toggle stays in sync across undo / macro / other-tab dispatches | `packages/ui/src/ReviewControls.tsx` |
| **Accept / reject revisions** — Sprint 7 `AcceptRevision` / `RejectRevision` commands; `TrackChangesSidebar` lists revisions with author + date | `packages/ui/src/TrackChangesSidebar.tsx` |
| **Comments** — anchored to logical ranges; engine-minted comments mint `w14:paraId` + `w14:textId`; resolved bit round-trips via `commentsExtended.xml` with `w15:done` | `crates/engine/src/lib.rs::CommentDef`, `crates/format-docx/src/parts/comments.rs` |
| **Pending (sticky) formatting** — bold / italic toggled before typing arms a pending overlay; the next typed run inherits it; cleared on caret move | `crates/engine-wasm/src/lib.rs::pending_format` |
| **Rich clipboard** — copy / cut / paste with plain text + HTML + `.docx` fragment payloads | `crates/engine-wasm/src/lib.rs::do_get_selection_as_clipboard` |
| **HTML paste with sanitisation** — `Command::PasteHtml` parses + folds into `DocumentTree` | `crates/engine/src/html.rs` |
| **Inline images** — `<a:blip>` round-trip via `media[rel_id]`; engine resolves image dimensions to a glyph-equivalent advance | `crates/engine/src/lib.rs::InlineObject`, `crates/format-docx/` |
| **Drag-and-drop image insertion** — file drop on the editor → `Command::InsertImage` | `ts/src/input/dnd.ts` |
| **IME composition preview** — inline underlined on-canvas preview during `compositionupdate`; commit on `compositionend`; native popup anchors at caret | `ts/src/components/HiddenInput.tsx`, `crates/engine-wasm/src/lib.rs::do_*_composition` |

---

## 6. I/O & serialization

| Capability | Where |
|---|---|
| **`.docx` open (read)** — `quick-xml`-driven parser; every non-`word/document.xml` archive entry stashed verbatim in `DocxArchive.other_entries` for byte-identical passthrough | `crates/format-docx/src/reader.rs` |
| **`.docx` save (write)** — regenerated `word/document.xml` + verbatim passthrough on every sibling entry; round-trip diff ≤ 2 × inserted-text bytes | `crates/format-docx/src/writer.rs` |
| **HTML export** — standalone HTML5 document with `<p dir="ltr\|rtl">` per paragraph, full `<span style>` per `StyleRun`, `<table>/<tr>/<td>` with `colspan` (`<w:gridSpan>`) + `rowspan` (`<w:vMerge>`), inline images as `data:` URI base64; `cid:` fallback for missing blobs | `crates/format-html/src/lib.rs::to_html` |
| **HTML fragment export** — `to_html_fragment` emits just the block sequence (no `<html>` envelope) for clipboard / embed use | (same) |
| **Plain-text export** — `DocumentTree::to_plain_text` concatenates paragraph text with `\n`; tables → tab-separated rows; inline images → `[image]` marker; footnote refs → `[footnote N]` (no silent drops) | `crates/engine/src/lib.rs::to_plain_text` |
| **PDF export** — `pdf-writer`-driven; full `Type0` / `CIDFontType2` font embedding; `/ToUnicode` map; `FlateDecode` content streams | `crates/format-pdf/` |
| **PDF/A-1b conformance** — `format-pdf` emits PDF/A-1b when `PdfProfile::A1b`; synthesised sRGB ICC profile (built from `crates/format-pdf/build.rs`, no binary blob in tree); validated by `tools/pdf-validate` (veraPDF harness) | `crates/format-pdf/` |
| **`commentsExtended.xml` round-trip** — `w15:done` parse + emit; engine-minted comments mint fresh `w14:paraId` + `w14:textId` | `crates/format-docx/src/parts/comments.rs` |
| **Additive OPC plumbing** — `inject_content_type_override` + `inject_doc_rel` splice `<Override>` + `<Relationship>` rows into `[Content_Types].xml` + `word/_rels/document.xml.rels` without touching existing entries; untouched docs round-trip byte-identical | `crates/format-docx/src/writer.rs` |
| **`numbering.xml` regen** — `DocumentTree.numbering.dirty` flag drives writer regen; otherwise passthrough byte-identical | `crates/format-docx/src/parts/numbering.rs` |
| **`commentsExtended.xml` regen** — same dirty-flag discipline | `crates/format-docx/src/parts/comments.rs` |
| **`<w:pStyle>` writer** — `crates/format-docx/src/writer.rs`'s `<w:pPr>` emitter | (same) |
| **`styles.xml` reader** — parses `<w:styles>/<w:style>` into `DocumentTree.styles` for the cascade resolver | `crates/format-docx/src/parts/styles.rs` |
| **`<w:rFonts>` raw passthrough** — unrecognised font families round-trip via `SpanStyle.raw_font_family` so the writer emits a faithful XML | `crates/engine/src/lib.rs::SpanStyle` |
| **Inline image MIME mapping** — `image/png` / `image/jpeg` / `image/gif` / `image/tiff` / `image/x-emf` / `image/x-wmf` / `image/svg+xml`; falls back to `application/octet-stream` | `crates/format-docx/src/writer.rs::media_filename` |
| **Round-trip harness** — `tools/roundtrip` asserts sibling-byte-identity on every non-`document.xml` entry + bounded `document.xml` diff + HTML emit + plain-text emit (8 assertion steps) | `tools/roundtrip/src/main.rs` |

---

## 7. Accessibility & UX

| Capability | Where |
|---|---|
| **Visually-hidden accessibility tree** — `role="document"` mirror DOM; one `<p dir>` per paragraph + `<span>` per StyleRun; browser's UAX-#9 handles BiDi for the screen reader | `ts/src/components/AccessibilityTree.tsx` |
| **Fine-grained a11y deltas** — `Event::AccessibilityTreeDelta` carries Replace / Update / Insert / Remove patches per paragraph after every doc mutation; UI reconciles | `crates/bridge/src/event.rs::A11yPatch` |
| **`aria-live` announcement region** — `Event::Announcement { priority, message }` routed into polite + assertive `aria-live` DOM regions | `ts/src/components/Announcements.tsx` |
| **Announcement coverage** — table insert / delete / row / column / merge / split / shading / borders; revision accept / reject; comment add / delete; page break inserted; tab stops updated; style apply; format-change label-driven (bold / italic / align) | `crates/engine-wasm/src/lib.rs::self.announce` |
| **Selection read-back** — `Event::SelectionChanged` carries paragraph alignment, paragraph direction, undo / redo availability, selection kind (linear vs table-cells), per-flag `attrs_mixed` bitmap, paragraph style id, section geometry, cell properties, tab stops, tracking-changes state | `crates/bridge/src/event.rs::SelectionChanged` |
| **`Properties` dialog prefill** — `PageSetupDialog` reads section geometry; `CellPropertiesDialog` reads cell properties; UI never overwrites from defaults | `packages/ui/src/PageSetupDialog.tsx`, `CellPropertiesDialog.tsx` |
| **Mixed-attribute indicators** — toolbar buttons render INDETERMINATE when the selected range carries disagreeing bold / italic / underline / strike values | `packages/ui/src/*Controls.tsx` |
| **Predictive scrollbar lazy layout** — `lazy_runway(viewport_h, scale)` = `max(LAYOUT_BUFFER_PT, viewport_h × scale × 2)`; fast-flick scrolls up to one viewport past the current band hit pages that are already laid out; `AVG_BLOCK_HEIGHT_PT = 64.0` virtual height over-estimate ensures the scrollbar shrinks only, never yanks downward | `crates/engine-wasm/src/lib.rs::lazy_runway` |
| **Cold-open viewport-cull pagination** — `INITIAL_COLD_OPEN_BUDGET_PT = 1684` ≈ 2 A4 pages; opens a 50-page doc in bounded time | `crates/engine-wasm/src/lib.rs::LazyLayoutState` |
| **DPR-aware scaling** — `device_pixel_ratio` propagates through layout config; HiDPI canvases stay crisp | `crates/engine-wasm/src/lib.rs::Command::SetZoom` |
| **DirtyTracker partial repaints** — `Command::RequestPaint` paints only the dirty region; `render_canvas2d` clips fills / strokes + culls off-region glyph runs | `crates/render/src/dirty.rs` |
| **Crash overlay** — `TrapOverlay` covers the canvas + offers recovery actions when the worker traps | `packages/ui/src/TrapOverlay.tsx` |
| **Status bar** — page count + word count + revision count + zoom indicator pinned to the bottom strip | `packages/ui/src/StatusBar.tsx` |
| **Dev HUD** — toggle-able overlay with `EngineStats` (last paint ms, command ms, page count, document height) every 1 s | `packages/ui/src/DevHud.tsx` |
| **Zoom controls** — `Command::SetZoom` clamped to `[0.25, 4.0]`; UI buttons + ratio display | `packages/ui/src/ZoomControls.tsx` |
| **File menu** — open `.docx`, save `.docx`, export HTML, export Plain Text, export PDF | `packages/ui/src/FileMenu.tsx` |
| **Honest UX discipline** — engine-pending UI surfaces render `disabled` + amber "Engine pending" badge until the engine path lands; every such badge has a matching GitHub issue | `.claude/rules/sdk-architecture.md` |
| **Vello / WebGPU renderer (opt-in)** — `init_vello` plumbed; `?renderer=vello` switches the active backend (Canvas2D stays the default for CI determinism) | `crates/render/src/vello_backend.rs` |

---

## Tracked follow-ups (NOT yet shipped)

The following sit on the issue tracker but are **not functional
today**:

| # | Title | Status |
|---|---|---|
| `#2` | IME composition preview verify on real CJK IME | Manual QA only — needs macOS / Windows + Japanese / Pinyin IME |
| `#3` | Discontinuous + cross-container selection (UX §IV.6) | Deferred until a concrete user case appears (selection model carries the cost on every op) |
| `#20` | RTL Tab Anchoring + Directional Stops | Filed during L2.1 — LTR tab kinds shipped, RTL still uses LTR positions; mirror pass scoped for a follow-up sprint |
| `#21` | `Command::ModifyStyle` + Cascade Re-application | Filed during Sprint 12 closure — `ApplyStyle` (style assignment) shipped; mutating the style DEFINITION itself (`Normal` font-size 12 → 14) ships separately with its own `StyleEditDialog` |

---

## Quality discipline

Every commit in `main` passes:

- `cargo test --workspace --lib` — 279+ tests (engine 85, engine-wasm 34, format-docx 81, layout 35, …)
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `cargo run -p shape-regression --release` — 6/6 shape goldens
- `cargo run -p roundtrip --release` — 8/8 assertions (sibling byte-identity + bounded `document.xml` diff + HTML emit + plain-text emit)
- `wasm-pack build --release crates/engine-wasm` — artifact < 15 MiB (current 6.28 MiB)
- `tools/visual-diff` tier A — 11/11 cases at 0.000 % diff
- `pnpm exec playwright test` — 7/7 e2e specs (boot, crash-recovery, event-log-replay, isolation, memory-50p, rpc-throughput, sab-transfer)
- `pnpm -r tsc` — TypeScript strict + `noUncheckedIndexedAccess` + `exactOptionalPropertyTypes`

Zero `unwrap()` traps in production code across every audited
surface: `format-html`, `DocumentTree::to_plain_text`,
`selection_changed`, `tracked_insert_text` / `tracked_delete_range`,
`count_uax_words`, `numbering`, `apply_tab_advances`,
`autofit_distribute`, `lazy_runway`,
`balance_current_section_columns`, `transform_for_shape`.
