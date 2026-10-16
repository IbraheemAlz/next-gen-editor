# Render / layout rules

Phase 3 invariants for the box model, the Canvas2D backend, and PDF export.

## Coordinate spaces

- The box tree is `PageBox → ParagraphBox → LineBox → VisualRun →
  PositionedGlyph`. Every box's `origin` is **parent-relative**. Absolute
  positions are reached by accumulating down the tree:
  `page.margins + para.origin + line.origin + run pen + glyph.x_offset`.
- `layout_paragraph` owns **all** geometry. Line stacking (`origin.y`) and the
  alignment offset (`origin.x`) are baked into `LineBox.origin` — the renderer
  is a pure traversal and never re-derives alignment.
- `PositionedGlyph` stores advances/offsets only, no absolute `x`; position is
  the run pen plus the cumulative `x_advance`. The pen advances for glyph id 0
  (`.notdef`) too, even though it is not drawn.
- Issues #359 / #104 / #249 — a `StyleSpan` carries an optional
  complex-script set (`StyleSpan::cs`: size, shift, weight, slant,
  family; `None` when equal to the Latin set). `build_line` and the
  width probe segment by script AND complex-script class
  (`segment_by_script_class`) and shape each piece with
  `StyleSpan::face_for(complex)` — keep the two in step, and never read
  `span.px_size` / `span.bold` / `span.font_family` directly for a piece.

## Font substitution and line pitch (issue #329)

- `FontStack::resolve_family` matches a document's family before the
  per-script fallback: exact id → name (id key or the face's `name`-table
  family, `family_key`-folded) → `text_pipeline::SUBSTITUTIONS`, keyed by
  (family, `ScriptClass`) — Arial Latin is Liberation Sans, Arial Arabic
  is Noto Naskh Arabic. A substitute must cover the class. Add rows to
  the table (and the face to `fonts.json` `substitutes`), never a special
  case in layout; `every_shipped_substitute_resolves_through_its_name_table`
  pins the shipped faces against it.
- **The per-script default is declared, never inferred from face ids
  (issue #472).** A run that names no family (or one nothing resolves)
  takes `FontStack::resolve`'s per-script default: the stack's explicit
  **priority** for the script (`FontStack::with_priority(script, &[ids])`,
  loaded faces that cover the script, in declared order), then the
  id-sorted covering faces, then the fallback chain (the primary face,
  then every face by id). Without a priority a dual-script face wins by
  name: Amiri (`amiri` < `liberation`) used to shape the interactive
  editor's Latin text with Amiri's glyphs. Every consumer declares the same
  table: the shell sends `ts/public/fonts.json` `defaults_by_script`
  (`latin: [liberation]`, `arabic: [amiri, noto-naskh]`, `common:
  [liberation]`) as `Command::SetFontPriority` after the boot fonts load (and again after a
  recovery, which wipes the engine's font setup with its font map);
  `tools/harness-fonts` declares it as `DEFAULTS_BY_SCRIPT` (a unit test
  pins the two together) and registers its faces under the manifest ids,
  so no id has to sort a particular way. Build every engine-side stack
  through `Engine::font_stack` (it applies the declared priority) — never
  a bare `FontStack::from_faces(self.fonts.clone(), …)`. The script's
  priority also orders faces that answer to ONE family name in
  `resolve_family`; a substitution row's own family order stays the
  document's mapping and wins over it. The shell's Latin default is also
  its seed face (`cfg.font_id`, the fallback root), so scripts the stack
  does not specialise take it.
- **ONE walk order for "which face?" (issue #490).** `FontStack::walk_order`
  (the script's explicit priority, then the id-sorted covering faces, then
  the fallback chain) is the only place that order lives: `resolve_detailed`
  takes the first face of it that matches the weight / slant,
  `resolve_covering(ch)` (the list-marker / `<w:sym>` glyph fallback) the
  first one whose cmap has the glyph, walked for `script_of(ch)` — a bullet
  or a digit is `Script::Common`, so the **`common`** default
  (`defaults_by_script.common`, declared explicitly since #490, also in
  `harness_fonts::DEFAULTS_BY_SCRIPT` and in `corpus-native`'s
  `register_editor_fonts`) decides it, a letter its own script's default.
  A glyph the declared defaults lack still comes from the first covering
  face of the id-sorted tail (primary face first); a glyph only one face
  has comes from that face whatever the order. Never reintroduce a second
  hand-rolled walk over `fallback_chain` / `by_script`.
- **Line pitch has ONE model (issue #475).** Every document — read from
  a Word package or built in the editor (the seeded page, `.txt` /
  `.html` opens, the `RenderPage` harness) — lays out with Word's
  font-derived pitch (`layout::FontLinePitch`, engine-wasm
  `paragraph_line_rule`): a line is the largest `ascent + line gap` plus
  the largest `descent` of its runs' faces under
  `LoadedFont::line_metrics` (Windows Word's rule: win extent + GDI
  external leading, typo metrics when `USE_TYPO_METRICS`), × the `auto`
  multiple / floored by `atLeast`, the extra space ABOVE the text; a
  runless line takes the paragraph mark's face. A substituted run whose
  row names `metrics_from` measures that face
  (`VisualRun::metrics_font`). Only `exact` is absolute. The configured
  `RenderConfig::line_height` sets no line's pitch any more (it is the
  nominal fallback when no face can be measured), so a document typed
  here lays out identically after `SaveDocx` + reopen (pinned by the
  fuzz-native `a_typed_document_reopens_from_its_docx_with_identical_geometry`).
  Never re-introduce a per-document pitch switch.
- The rule is the Windows one on purpose (Arabic documents are authored
  there); Mac Word uses `hhea`. Whether Windows Word honours
  `USE_TYPO_METRICS` is unverified (Amiri: 1.758 em typo vs 2.760 em win)
  — `text_pipeline::fonts::word_line_metrics` is the one place to flip it.

## Canvas2D backend

- **`put_image_data` ignores the canvas clip path** (and the transform), per
  the Canvas2D spec. Glyphs are blitted with `put_image_data`, so `ctx.clip()`
  does **not** skip off-region glyphs — cull glyph runs by a bounding-box test
  in the Rust loop instead. The clip only bounds `fill_rect` / `stroke_rect`.
- **A lost 2D context repaints on restore (issue #502).** A page canvas
  whose context is lost and restored comes back blank, and the #497
  per-canvas display-list memo would skip an unchanged page forever. The
  worker (`ts/src/engine/canvas-context.ts`, `CanvasContextWatcher`)
  listens for `contextlost` / `contextrestored` on every `OffscreenCanvas`
  it holds (an `isContextLost()` poll only where a surface lacks the
  events) and, on a restore, calls `Engine::forget_canvas_memo(idx)` (that
  one canvas; the other pages keep skipping) and repaints. Every change
  broadcasts the additive `Event::CanvasContext` (Dev HUD "Canvas context"
  row, telemetry `ERROR / CANVAS_CONTEXT_LOST`). Not a `Command`: it is a
  worker-side canvas fact, never document state. A new path that draws on
  a page canvas outside `render_document` must clear its memo the same way.
- A clip covering the whole page is a no-op: every run intersects it, nothing
  is culled, so a full repaint is byte-identical to the unclipped path. Pure
  render refactors must keep the visual-diff goldens at 0.000 %.

## Layout self-defense (issue #87)

**An imperfect layout that terminates beats a perfect one that hangs.**
Every convergence loop in `crates/layout` (and the layout helpers in
`engine-wasm`) is bounded twice: by construction — each re-push consumes
content (a paragraph tail has fewer lines, a table continuation fewer
rows) or the oversize guard clips — and by the `layout::watchdog`
backstop, which counts *churn rounds* (the same content re-placed on a
fresh page without consuming any of it; a fresh page is the most room
the paginator can ever offer, so a repeat is a proof of non-progress)
and escalates: **(a) drop optional constraints** (keep-with-next chains,
repeated table header rows), **(b) freeze** — pin the block atomically
at the cursor, clipping, **(c) force-validate** — the page cap: append
the rest without page breaks and accept the state. Progress (a smaller
re-push, a new top-level block) resets counter and stage. A strict
switch (`Paginator::with_strict_watchdog`) turns every recovery into a
hard failure so CI catches a new loop instead of a silent recovery.

- Never add a retry / re-push / negotiation loop without a local
  termination rule *and* a `DegradeReason` for the escape hatch. The
  escape hatch must paint something; it must never drop content.
- Every degradation is reported: the paginator's notes ride
  `Event::Painted.layout_degraded` (and the synthetic side-channel).
  A degraded paint is telemetry, never a blocking error.
- **Verified fast paths.** An incremental relayout (`LazyLayoutState` /
  `ExpandLayout` bands, the paragraph layout LRU, any future
  single-paragraph tier) is a *prediction*. Run the real layout, check
  its invariants against the previous result (`layout::verify_prefix`:
  page count, page geometry, per-block origin + size, end position;
  `cached_paragraph_is_consistent` at the paragraph tier) and demote to
  a full reflow on mismatch — never trust the prediction. A stale-layout
  bug becomes a perf blip plus a `FastPathMismatch` / `CacheMismatch`
  note, not corruption.
- **The cull counts a line at most one page (issue #501).** The viewport
  cull compares "finished pages + the paginator's in-progress cursor" with
  its budget; a line taller than its page (clipped by the oversize guard)
  leaves the cursor at the line's full height, so a size no page can hold
  put the running height past EVERY finite target and the band stopped
  behind that block for ever (#499). The comparison uses
  `Paginator::cursor_y_within_page` (the cursor bounded by the page), so
  each such block adds at most one page and a finite target keeps working;
  a cursor inside the page is unchanged, so no normal band moves. Command
  sizes are now bounded at the door (`engine::MAX_FONT_SIZE_PT`, see
  `docx.md`), but a restored snapshot or an old logged command can still
  hold any size: the clamp is the structural defence, the progress-bounded
  expansion below the backstop.
- **One expansion routine (issues #499 / #501).** Every growth of the lazy
  band that must reach a depth - a caret move, a click on a page past the
  band (`lazy_nav.rs`), and the scroll-driven paint (`RequestPaint`,
  `ExpandLayout`, the paint after `SetViewport`: `do_request_paint` ->
  `Engine::lazy_expand_to_viewport`, a cache hit while the band reaches
  `min_target_y`) - goes through `Engine::lazy_nav_expand_until` (the #499
  progress bound and `NavExpansionStalled` fallback, below). Never add a
  second grow-until loop beside it. The test hook `CULL_CLAMP_OFF`
  (`cfg(test)`) switches the #501 clamp off so `oversize_line_tests` can
  still prove the fallback now that the clamp makes the stall unreachable
  from a line.
- A band's geometry must be self-consistent where it stops: never cull
  right behind a keep-with-next paragraph (issue #95), and mark pages
  that a pending pass will still rewrite — a continuous section's column
  balance — as provisional (`LazyLayoutInfo::open_from_page`, verified
  with `layout::verify_prefix_open`, issue #93) instead of letting every
  expand demote.
- The nominal path stays output-identical: `geometry_fingerprint` pins
  the pre-watchdog geometry for every paginator and engine fixture. A
  self-defense change that moves a pinned fingerprint moves the goldens.
- **A deeper cull target is not a full layout (issue #499).** The
  viewport cull compares the paginator's RUNNING height (an oversize
  line's cursor can run far past its page), not the band's page sizes,
  so a band can stay put under any finite target. A loop that grows the
  band (`lazy_nav_expand_until`) is bounded by band PROGRESS — more pages
  or fewer unlaid blocks per round, else ONE target-less
  `build_pages(None)` noted `NavExpansionStalled` — never by "the target
  will get big enough".
- **Bound slow progress, not only churn (issue #318).** The watchdog
  catches non-progress; a recursion that re-does a subtree per ancestor
  pass makes progress and still never finishes (nested-table autofit
  was `F(2·depth)` grid layouts). Any per-level re-measure of a subtree
  goes through a memo scoped to ONE top-level layout (engine-wasm
  `TableLayout`: inner table per width, column solve per width,
  intrinsic widths per cell — keyed by the subtree's address in the
  immutably borrowed tree, verified on every hit), and recursion over
  document nesting is capped (`MAX_TABLE_LAYOUT_DEPTH` = 32: deeper
  tables flatten to their paragraphs, `NestingCapped`). A probe pass
  that only needs a size takes it from the size's own computation
  (a nested table's width is its column sum) — never from a full
  layout at a width the final pass will not use.
- **Caches that outlive a paint are keyed by CONTENT (issue #379).**
  Beneath the per-call `TableLayout` memo sits the content-keyed table
  cache (`engine-wasm/src/table_cache.rs`, inside `LayoutCache` with the
  paragraph LRU; `LayoutCache::clear` drops both). Its key is a
  fingerprint of every layout input — cell paragraphs by their
  `paragraph_layout_key`, the table / row / cell / `RenderConfig`
  structs by **exhaustive destructuring** (no `..`), so a new model
  field fails to compile there until it is hashed or shown
  layout-inert. The hit check is structural, not a re-layout: a field
  missing from the key is a stale paint the check can miss, so the key
  is the real guard. A failed hit notes `TableCacheMismatch` (the
  paragraph tier keeps `CacheMismatch`); notes a layout emitted are
  stored with the entry and replayed on a hit, so a cached paint reports
  what a fresh one would. `tools/corpus-native` re-lays every document
  warm (`engine_repaint_consistent`) to prove warm == cold.
- **The paragraph cache is weighted in BYTES, not entries (issue #457).**
  `engine-wasm/src/paragraph_cache.rs` (`ParagraphCache`, inside
  `LayoutCache`): an estimated-bytes budget over the cached
  `ParagraphBox`es (`paragraph_box_bytes` — inline `size_of`s, every
  `Vec` / `String` / `Box` behind a glyph, line or field, a fixed
  per-allocation allowance; the table cache's `table_box_bytes` shares
  it), `PARAGRAPH_CACHE_BUDGET_BYTES` = 192 MiB, an entry ceiling of
  65 536 as a sanity bound only, and a box over an eighth of the budget
  is never cached. The old 4096-ENTRY LRU was smaller than the
  paragraphs of a long document (the `250p` / `500p` perf fixtures had
  5 000 / 10 000 before issue #476 re-calibrated them to 1 500 / 3 000),
  and a document-order sweep over an LRU smaller than its working set
  hits NOTHING — a warm full re-layout (PDF export, `ExpandLayout` to
  the end, the save restamp) cost the same as a cold one. 10 000
  paragraphs are ~113 MiB of boxes at native pointer width (~87 MiB on
  wasm32), so the budget fits a 500-page document with headroom;
  re-measure (the `dense-5000` / `dense-10000` harness documents keep
  the old fixtures' shape) with
  `cargo test -p engine-wasm --release profile_paragraph_cache -- --ignored --nocapture`
  (sizing + warm/cold) before moving the budget, and re-run
  `node tools/memory-profile/run.mjs --budgets`. A scene larger than
  the budget thrashes exactly like the old cap did — the budget is the
  trade, not a guarantee. A new owned field on `ParagraphBox` /
  `LineBox` / `VisualRun` / `PositionedGlyph` that holds heap memory
  must be added to `paragraph_box_bytes`, or the budget under-counts it.

## PDF export

- PDF user space has its origin **bottom-left, y-up**; the layout engine is
  **top-left, y-down**. Invert every glyph: `pdf_y = page_height - layout_y`.
  X needs no inversion.
- Fonts embed as `Type0` / `CIDFontType2` with `Identity-H` encoding and
  `CIDToGIDMap /Identity`, **subset** to the glyphs the document shows (issue
  #327, the `subsetter` crate — `format-pdf/src/font_program.rs`). The
  subset renumbers glyph ids, so the 2-byte content-stream codes are the
  SUBSET's ids, never the shaped ones: always emit a shown glyph through
  `FontObj::show_code` (it assigns the id on first use — that is how the
  subset learns which glyphs it needs), and key `/W`, `/ToUnicode` and the
  PDF/A-1b `/CIDSet` by code. Subset names carry a deterministic six-letter
  `ABCDEF+` tag. A face the subsetter rejects falls back to full embedding
  with codes == shaped glyph ids (the pre-#327 output).
- A CFF-outline face (`OTTO` `.otf`, `CFF ` table, no `glyf`) embeds as
  `CIDFontType0` + `FontFile3 /Subtype /CIDFontType0C` (the bare CFF program
  the subsetter rewrote CID-keyed with an identity charset) and **no**
  `CIDToGIDMap` (only `CIDFontType2` may carry one) — issue #361,
  `font_program::Outlines`. TrueType output must stay byte-identical when
  touching the CFF branch.
- CFF test fonts are synthesized at test time from the shipped OFL `.ttf`s
  (`format-pdf/src/cff_test_font.rs`) — never commit a font binary. Keep a
  `Notice`/`Copyright` in any synthesized Top DICT: veraPDF 1.30 mis-scales
  CFF widths when the subset's `FontMatrix` directly follows `ROS`.
- Tests that inspect content-stream text decode codes through the font's
  own `/ToUnicode` (`format_pdf::test_support::{to_unicode_cmaps,
  decode_codes}`) — comparing them against `LoadedFont::glyph_id` is wrong
  for a subset font.
- Position every glyph with an explicit text matrix, not the PDF font's
  advances: our `x_advance` carries justification + Kashida adjustments the
  font's intrinsic widths do not.
- **Tagging is opt-in and byte-neutral when off (issue #360).** Only
  `PdfExportOptions::tagged` (always on for `PdfProfile::Ua1`, the tagged
  PDF/A-2u) writes marked content, the structure tree and the PDF/UA
  identification; every other profile must stay byte-identical — sha256
  the tier-a exports before/after (`write_tier_a_exports` in
  `engine-wasm/src/pdf_semantics_tests.rs`). In a tagged export every
  painting operator is inside either a `BDC` with an `/MCID` (real content,
  recorded through `SemCtx` — `tagging.rs`) or an `/Artifact` sequence;
  a new paint pass must pick one (`artifact_begin` / `decoration_pass`),
  or veraPDF's PDF/UA-1 §7.1 check fails. A tagged PDF 1.5+ file is
  re-laid out by `objstm::pack` (object stream + xref stream), so tests
  that search a tagged file for dictionary text go through
  `test_support::searchable_text`.
