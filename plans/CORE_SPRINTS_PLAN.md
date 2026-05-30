# CORE_SPRINTS_PLAN.md — Post-MVP Core Engine roadmap

> **Created** 2026-05-26 at the end of the (UI Edition) sprint series.
> Picks up from commit `747796b feat(sdk): MVP UI shell (Sprints 1-8 UI Edition)`.
>
> The UI shell is feature-complete per [`UI_SURFACE_MAPPING.md`](UI_SURFACE_MAPPING.md).
> Every "Engine pending" badge in `@nge/ui` corresponds to one of the 9
> backlog issues this document organizes into the next phase of work.
>
> **Operating principle.** Each sprint is **bounded** — one focused
> engine deliverable, gated by `cargo test --workspace`, `wasm-pack
> build`, and the matching `pnpm -r tsc` after the UI flip-the-badge
> follow-up. No sprint mixes "I/O writer" with "cross-cutting
> mutation gating" — those have different blast radii.

## Sprint sequencing

```
                   ┌──────────────────────┐
                   │ MVP UI shell (DONE)  │
                   │ commit 747796b       │
                   └──────────┬───────────┘
                              │
       ┌──────────────────────┼──────────────────────┐
       │                      │                      │
       ▼                      ▼                      ▼
 Sprint 9 (Core)        Sprint 10 (Core)      Sprint 14 (Core)
 I/O + Resolved         State Read-back       Track Changes
 Comments               + Announcements       Recording
 (#9, #15)              (#10, #16)            (#14)
       │                      │
       │                      ▼
       │                Sprint 11 (UI+Core)
       │                Word count + Ruler
       │                (#17, #13)
       │
       ├──────────────────────┐
       ▼                      ▼
 Sprint 12 (Core)        Sprint 13 (Core)
 Live Style Table        Numbering Synthesis
 (#11)                   (#12)
```

**Dependency edges:**
- **Sprint 11 needs Sprint 10's SectionGeometry read-back** for the Ruler to
  prefill against the active section. Ship Sprint 10 before Sprint 11.
- **Sprints 9, 12, 13, 14 are independent** of each other — can ship in
  any order, even in parallel branches if multiple agents collaborate.
- **Sprint 14 is the highest-risk** (touches every mutation handler) —
  defer until other Core sprints stabilize so its blast radius is
  isolated.

---

## Sprint 9 — Core I/O Serializers + Resolved Comments ✅ DONE

**Status:** Closed 2026-05-27. Issues #9 + #15 both resolved.

  * `crates/format-html/` — 793-line crate, panic-free, 10 native
    tests covering the 5 mandatory acceptance cases + bonuses (HTML
    escapes, explicit LTR, missing-blob cid fallback, vMerge rowspan,
    colspan, font-family resolved + raw).
  * `engine::DocumentTree::to_plain_text` — folded into the engine
    crate per the "<80 LOC = method" guidance. 3 tests cover join,
    inline-image marker, and tab-separated table flatten.
  * `engine-wasm::save_html_bytes` + `save_plain_text_bytes` route
    `Command::SaveDocument { Html | PlainText }` through the new
    crates and return `Event::DocumentSaved { bytes, size }`. Zero
    extra copies on the WASM side via `String::into_bytes()` reuse
    of the underlying `Vec<u8>` allocation; the bridge crosses as a
    single `Uint8Array` via `serde_bytes` + `tsify`.
  * `tools/roundtrip` extended with step 7 (HTML emit) + step 8
    (plain-text emit) covering the seed-plus-edit DocumentTree.
  * `format-docx/src/parts/comments.rs` reader + writer round-trip
    `w15:done` through `commentsExtended.xml` (Sprint 9 v1 + L1.2 #18
    OPC plumbing finishes the engine-minted-comment case).
  * UI activation: `packages/ui/src/FileMenu.tsx` HTML + Plain Text
    entries enabled with no "Engine pending" badge;
    `packages/ui/src/CommentsRail.tsx` Resolve button title carries
    no "(in-memory only)" caveat.
  * `UI_SURFACE_MAPPING.md` §13 (Resolve / delete comment) + §15
    (Export Plain Text + Export HTML) flipped to ✅ Wired.

---

## Sprint 9 — Original plan (archived)

**Issues:** [#9](https://github.com/IbraheemAlz/next-gen-editor/issues/9), [#15](https://github.com/IbraheemAlz/next-gen-editor/issues/15)

**Risk:** Low — pure writer work, no engine model mutations, no cross-cutting handler changes.

**Deliverables**

1. **`crates/format-html/`** (new crate)
   - Walks `DocumentTree.blocks` → emits `<p dir="…">` per paragraph
     with `<span style="…">` per `StyleRun` (bold/italic/underline/strike/
     color/bg/size/family).
   - Tables → `<table>/<tr>/<td>` honouring `vMerge` (rowspan) +
     merged cells (colspan) + shading + borders.
   - Inline images → `<img>` with `src="data:image/…;base64,…"` from
     `media[rel_id]`.
   - Preserves BiDi via `dir="ltr|rtl"` on paragraph; never flattens
     visual order across line breaks.
   - Native tests: empty doc, mixed-style paragraph, 2×2 table, BiDi
     paragraph (Arabic + English), paragraph with one inline image.

2. **`crates/format-text/`** (new crate; could fold into a
   `DocumentTree::to_plain_text` method if scope stays under ~80 LOC)
   - Concatenates paragraph text with `\n`; tables emit tab-separated
     rows.
   - Inline objects render as `[image]` markers (or just drop).

3. **`crates/engine-wasm/src/lib.rs`** — `Command::SaveDocument {
   format }` routes the `Html` / `PlainText` variants through these
   new crates and returns `Event::DocumentSaved { bytes, size }`.

4. **`crates/format-docx/src/parts/comments.rs`** — extend reader to
   parse `word/commentsExtended.xml`, populate `CommentDef.resolved`
   from `w15:done`.

5. **`crates/format-docx/src/writer.rs`** — when
   `comment_defs.values().any(|c| c.resolved)`, regenerate
   `commentsExtended.xml`; otherwise the existing `other_entries`
   passthrough preserves byte-identity.

**UI follow-up** (under 20 LOC total):
- `packages/ui/src/FileMenu.tsx`: drop `disabled` + "Engine pending"
  badge from HTML and Plain Text export entries (4 lines).
- `packages/ui/src/CommentsRail.tsx`: drop the "(in-memory only)"
  caveat in the Resolve button's title tooltip (1 line).

**Acceptance**
- `cargo test --workspace --lib` clean.
- `tools/roundtrip` adds two cases: HTML emit + commentsExtended.xml
  round-trip; both green.
- Manual QA: `Save → HTML / Plain Text` downloads a working file;
  `Resolve a comment → Save → reopen → comment still resolved`.

---

## Sprint 10 — State Read-back + ARIA Announcements ✅ DONE

**Status:** Closed 2026-05-27. Issues #10 + #16 both resolved.

  * `crates/bridge/src/event.rs` carries the additive
    `SelectionChanged { section_geometry: Option<BridgeSectionGeometry>,
    cell_properties: Option<BridgeCellProperties>, … }` extension +
    the new `Event::Announcement { priority, message }` variant.
    `BridgeSectionGeometry` is a 40-byte `Copy` struct (8 `f32`s + 1
    `u32` + enum); `BridgeCellProperties` is a `Default` mirror of
    the dialog-relevant subset of `CellProperties`.
  * `engine-wasm::selection_changed` populates both fields via
    `section_geometry_for_caret` + `cell_properties_for_caret`. Zero
    `unwrap()` / `expect()` in the resolution path — all `Option<T>`
    propagation via `?`. `section_for_block` falls back to a
    synthesized A4 default when the document carries no `<w:sectPr>`;
    `innermost_cell_props_at` walks the engine tree with `?` early-
    return at every step. Lookups are O(sections) / O(table-depth)
    — no full-tree traversal per keystroke.
  * `Engine::announce()` queues messages; the worker drains them
    into `Event::Announcement` events after every command. Sprint
    10 closure pass added missing announce sites for table
    mutations:
      - `Table inserted, N rows by M columns`
      - `Table deleted`
      - `Row inserted` / `Row deleted`
      - `Column inserted` / `Column deleted`
      - `Cells merged` / `Cell split`
      - `Cell shading updated` / `Cell shading cleared`
      - `Cell borders updated`
    Joins the existing coverage (revision accepted / rejected,
    comment added / deleted, tab stops updated, page break
    inserted, formatting label-driven for bold / italic / align).
  * UI consumption:
    `@nge/core::createEditorState` exposes `sectionGeometry()` +
    `cellProperties()` Solid signals;
    `@nge/ui::PageSetupDialog.tsx` + `CellPropertiesDialog.tsx` +
    `Ruler.tsx` prefill from them;
    `ts/src/components/Announcements.tsx` routes the announcement
    stream into a polite + assertive `aria-live` DOM region.

---

## Sprint 10 — Original plan (archived)

**Issues:** [#10](https://github.com/IbraheemAlz/next-gen-editor/issues/10), [#16](https://github.com/IbraheemAlz/next-gen-editor/issues/16)

**Risk:** Low — both are additive bridge-event extensions emitted from
existing handlers. No model changes, no new mutation paths.

**Deliverables**

1. **`Event::SelectionChanged`** extension (`crates/bridge/src/event.rs`):
   ```rust
   section_geometry: Option<BridgeSectionGeometry>,
   cell_properties:  Option<BridgeCellProperties>,
   ```
   Where `BridgeSectionGeometry` carries the active section's
   width/height/margins/orientation/columns/gutter; `BridgeCellProperties`
   carries the active cell's shading + per-edge borders. Both `None`
   when not applicable (e.g., `cell_properties` is `None` outside any
   table).

2. **`crates/engine-wasm/src/lib.rs`** — `selection_changed()` resolves
   these from `DocumentTree.sections[section_idx_for(caret)]` and the
   resolved cell at `Table.rows[row].cells[col]`.

3. **`Event::Announcement`** variant (new):
   ```rust
   Announcement { priority: AnnouncementPriority, message: String }
   ```
   Priority enum: `Polite` | `Assertive`. Engine emits from every
   user-visible mutation handler ("Aligned center", "Page break
   inserted", "Comment added", "Revision accepted", …).

4. **`@nge/core` `createEditorState`** — add `sectionGeometry`,
   `cellProperties` accessors; expose announcement stream.

5. **`@nge/ui`** — `PageSetupDialog` + `CellPropertiesDialog`
   prefill from the new state on open; the existing
   `ts/src/a11y/Announcements.tsx` consumes the announcement
   stream into its live region.

**UI follow-up**: Properties dialogs become useful for in-place
tweaking (today they overwrite from defaults).

**Acceptance**
- Dialog prefill works on a real `.docx` with non-default margins.
- Screen reader (NVDA / VoiceOver) speaks the key mutations.
- Errors fire as `Assertive`; everything else `Polite`.

---

## Sprint 11 — Accurate Word Count + Interactive Ruler ✅ DONE

**Status:** Closed 2026-05-27. Issues #13 + #17 both resolved.

  * `engine::DocumentTree::word_count` now segments via
    UAX-#29 `icu_segmenter::WordSegmenter::new_auto` (filtering to
    `WordType::Word`) so CJK / Thai / Khmer text reports a
    meaningful count instead of "one word per whitespace run".
    `walk_paragraphs` + `saturating_add` for overflow safety.
  * **Performance discipline**: `count_uax_words` parks the
    segmenter in a `thread_local!` so the icu data tables compile
    in exactly once per worker; subsequent calls are pure boundary
    walks. Shares data with `text-pipeline::break_opportunities`'s
    `LineSegmenter::new_auto` so the wasm artifact does not grow
    beyond what was already linked.
  * Native tests in `engine::tests`:
    `word_count_latin_matches_word_like_split`,
    `word_count_cjk_segments_chars`.
  * `packages/ui/src/Ruler.tsx` exposes 4 marker handles (start
    indent / first-line indent / right indent / tab stops).
    Pointer drag dispatches `cmd.setParagraphIndent` /
    `cmd.setTabStops` at gesture release (one drag = one undo
    entry); local Solid state mirrors the marker during drag for
    instant visual feedback. Markers read `sectionGeometry()` +
    `tab_stops()` from `@nge/core::createEditorState`. Zero
    layout math in Solid — every committed position becomes a
    bridge command.
  * Drag-off-strip removal is wired: dropping a tab marker below
    the ruler strip dispatches `setTabStops` with that stop
    omitted (matches Word).

---

## Sprint 11 — Original plan (archived)

**Issues:** [#17](https://github.com/IbraheemAlz/next-gen-editor/issues/17), [#13](https://github.com/IbraheemAlz/next-gen-editor/issues/13)

**Dependency:** Sprint 10 must ship first (Ruler reads
`state.sectionGeometry()` to align its 0-mark; without it the Ruler
falls back to A4 defaults — acceptable but degraded).

**Risk:** Medium — Ruler is a substantial new UI component with
pointer-drag math + rAF throttling. `#17` is trivial (drop-in icu
swap).

**Deliverables**

1. **`crates/engine/src/lib.rs`** — `DocumentTree::word_count`
   replaces `text.split_whitespace().count()` with
   `icu_segmenter::WordSegmenter` (`new_auto`) filtered by
   `WordType::Word`. Regression test on a CJK fixture (count > 1).

2. **`packages/ui/src/Ruler.tsx` + `.css`** (new):
   - Horizontal bar above `EditorSurface`, aligned to active
     section's content area (read from `state.sectionGeometry()`,
     fall back to A4 + 1-inch margins).
   - Tick marks every 0.125 inch, major labels per inch.
   - First-line (▽) + hanging (△) handles at the leading edge,
     drag-to-edit live updating `Paragraph.props.indent`. Throttle
     dispatches to `requestAnimationFrame`.
   - Tab-stop markers keyed to `TabKind` (`L`/`C`/`R`/`D`/`–`);
     click empty spot to add, click existing to cycle kind, drag
     to move, drag off to remove.
   - Selection follow: re-pull on `SELECTION_CHANGED`.

3. **Bridge** — new `Event::SelectionChanged.tab_stops:
   Vec<BridgeTabStop>` field; new `Command::SetTabStops { range,
   stops: Vec<BridgeTabStop> }`. Engine model already carries
   `ParaProperties.tab_stops`.

4. **Mirror horizontally** when section direction is RTL (Word's
   ruler flips edge).

**Out of scope (per #13)**: vertical ruler, margin-band drag,
decimal-tab rendering itself (`A.M3` partial; separate ticket).

**Acceptance**
- Ruler aligns 0-mark to content-area leading edge on A4 with
  1-inch margins AND on a custom-margin loaded doc.
- Drag first-line handle live-updates indent; release commits one
  undo entry (not one per frame).
- Tab stops add / move / remove and round-trip through `.docx`
  save / load.
- CJK fixture's word count reports a meaningful value.

---

## Sprint 12 — Live Style Table + Cascade Re-application ✅ DONE

**Status:** Closed 2026-05-27. Issue #11 resolved per its explicit
scope (ApplyStyle + cascade + writer + StylesDropdown rewire).

  * `engine::DocumentTree.styles: HashMap<String, ParagraphStyle>`
    populated by `crates/format-docx`'s reader from
    `word/styles.xml`. `MAX_STYLE_CHAIN = 10` caps `basedOn`
    traversal per ECMA-376 §17.7.4.5.
  * `engine::Paragraph.style_id: Option<String>` +
    `direct_overrides: ParaProperties` — the "cascade-source
    shadow" v1 approach picked in the original plan. Resolver
    folds `defaults → style chain → direct_overrides` into the
    paragraph's resolved `props`.
  * `Command::ApplyStyle { range, style_id }` routes to
    `DocumentTree::set_paragraph_style`;
    `do_apply_style` clears `layout_cache` + invalidates the dirty
    rect + announces (`"Applied style {id}"` /
    `"Cleared paragraph style"`). Zero `unwrap()` in the handler.
  * Writer emits `<w:pStyle w:val>` when `style_id.is_some()`;
    `styles.xml` rides the OPC passthrough byte-identical on the
    additive-only case. `paragraph_style_id_round_trips_via_p_style`
    test (writer.rs line 3430) pins the invariant.
  * UI follow-up shipped: `packages/ui/src/StylesDropdown.tsx` line
    46 — `await cmd.applyStyle(id)` against the real engine path.

**Follow-up filed:** [#21](https://github.com/IbraheemAlz/next-gen-editor/issues/21)
— Core: Implement `Command::ModifyStyle` and Cascade Re-application.
Out-of-#11-scope (style **definition** mutation vs the **assignment**
this sprint shipped). Lands with its own UI dialog so the bridge
doesn't ship as a Phantom command.

---

## Sprint 12 — Original plan (archived)

**Issue:** [#11](https://github.com/IbraheemAlz/next-gen-editor/issues/11)

**Risk:** **High** — the cascade re-application step is genuinely
tricky (today the engine can't distinguish "user typed Bold" from
"style Heading1 inherited Bold"). Likely multi-week.

**Deliverables**

1. **Style Table model** — `DocumentTree.styles:
   HashMap<String, ParagraphStyle>` mirroring `<w:styles>/<w:style>`
   entries with `basedOn`, `<w:pPr>`, `<w:rPr>`. Reader populates
   from `styles.xml`.

2. **`Paragraph.style_id: Option<String>`** field. Reader sets from
   `<w:pStyle w:val="…"/>`. Cascade resolver consults
   `styles[style_id]`.

3. **Cascade-source tagging** — invasive. Two viable approaches:
   - **Shadow direct-overrides**: each paragraph carries a
     `direct_overrides: ParaProperties` shadow holding only fields
     the user explicitly set. The resolved `props` is `style
     cascade ∪ direct_overrides`.
   - **Per-field cascade-source tag**: each `Option<T>` becomes
     `Option<(T, CascadeSource)>` where `CascadeSource ∈ { Direct,
     Cascade }`. Strip on style change keeps `Direct`-tagged
     fields, drops `Cascade`-tagged ones.

   The shadow approach is simpler to implement, harder to keep
   coherent across merges (split_paragraph + paste). The tagged
   approach is more invasive but bulletproof. **Pick the shadow
   approach** for v1; pay the tagged-approach price only if
   shadow-merge bugs surface.

4. **`Command::ApplyStyle { range, style_id }`** (new bridge variant)
   routed to `DocumentTree::set_paragraph_style`.

5. **Writer** — emit `<w:pStyle w:val>` when `style_id.is_some()`;
   preserve `styles.xml` byte-identity on the additive-only case.

**UI follow-up**: `packages/ui/src/StylesDropdown.tsx` flips from
faux preset dispatch to `cmd.applyStyle(id)` against the real
engine path (~4 lines).

**Acceptance**
- `Paragraph.style_id` round-trips through open → edit → save.
- A `.docx` authored in Word.exe with `Heading1` paragraphs renders
  correctly AND lets the user change the style.
- `tools/roundtrip` adds a styles-touched case; green.

**Out of scope**: user-defined custom styles (no UI for creating a
new style entry); character styles (`<w:rStyle>`).

---

## Sprint 13 — Numbering Synthesis ✅ DONE

**Status:** Closed 2026-05-27. Issue #12 resolved.

  * `crates/engine/src/numbering.rs` (546 lines) — dedicated module:
    `NumFmt` enum (Decimal / LowerLetter / UpperRoman / Bullet /
    Hebrew / Aiueo / IroIro / DecimalEnclosedCircle / …);
    `LvlDef` (one `<w:lvl>` row with format, restart rules, indent,
    marker template); `AbstractNum` (9-level `LvlDef` array, one
    `<w:abstractNum>`); `LvlOverride` (per-instance start delta);
    `NumInstance` (`<w:num>` ↔ `abstract_num_id`); `ListSynthesisKind`
    (Bullet / Number toggle target); `NumberingDefinitions`
    (HashMap<abstract_id, AbstractNum> + HashMap<num_id, NumInstance>
    + `dirty` flag).
  * `render_marker` projects `(num_id, ilvl, sibling_counter)` into
    a marker string per the resolved `LvlDef.fmt_text` template
    (`%1.`, `%1.%2.`, `(%3)`, etc.).
  * `resolve_markers_in_place` walks the document in order, maintains
    per-level counters, resets deeper levels when shallower levels
    advance (Word's default restart-on-parent-change semantics).
  * `DocumentTree.numbering.dirty` flag controls writer regeneration
    of `word/numbering.xml`; passthrough byte-identical otherwise.
    L1.2 (#18) discipline applied for OPC plumbing synthesis on fresh
    docs (`<Override>` for `numbering.xml` MIME type +
    `<Relationship>` Id in `word/_rels/document.xml.rels`).
  * `toggle_list_on_range` mints fresh AbstractNums on first toggle
    and reuses them on subsequent toggles. The
    `list_toggle_idempotency_via_round_trip` test (writer.rs)
    pins exactly **2** AbstractNums + **2** NumInstances after 20
    Bullet ↔ Number toggles, not 40.
  * Panic surface in production code: 0. (Two `.unwrap()` in
    `numbering.rs` are inside `#[cfg(test)]` and target
    test-fixture-constructed counters.)
  * UI wired: `packages/ui/src/ListButtons.tsx` dispatches
    `cmd.toggleList('Off' | 'Bullet' | 'Number')` against the
    real engine path.

---

## Sprint 13 — Original plan (archived)

**Issue:** [#12](https://github.com/IbraheemAlz/next-gen-editor/issues/12)

**Risk:** **High** — numbering is one of OOXML's most complex
sub-specs. Multi-week.

**Deliverables**

1. **In-memory numbering** — `DocumentTree.numbering:
   NumberingDefinitions { abstract_nums: HashMap<u32, AbstractNum>,
   nums: HashMap<u32, Num> }`. Each `AbstractNum` carries up to 9
   `<w:lvl>` entries with `numFmt` (`bullet`/`decimal`/`lowerLetter`/
   `lowerRoman`/…), `lvlText` template, indent, tab stops.
   Reader populates from `numbering.xml`.

2. **Resolver** — existing `Paragraph::resolved_marker` resolution
   consults this in-memory store rather than chasing through
   `other_entries`.

3. **Synthesis** — `DocumentTree::synth_list_definition(kind: ListKind) -> u32`:
   - `Bullet` → spawn an `AbstractNum` with Word's stock 9-level
     bullet hierarchy (•, ◦, ▪, …).
   - `Number` → spawn an `AbstractNum` with the decimal hierarchy
     (1., a., i., …).
   - Spawn a `Num` referencing the new abstract; return its
     `num_id`.
   - **Idempotent**: detects existing matching definitions and
     reuses them — no `numbering.xml` bloat from repeated clicks.

4. **`Command::ToggleList { Bullet | Number }`** wires through to
   `synth_list_definition` + sets `Paragraph.list_item` on every
   spanned paragraph. Replaces the current `Event::Error` stub.

5. **Writer** — emit `numbering.xml` when `DocumentTree.numbering`
   touched; passthrough byte-identical when untouched.

**UI follow-up**: `packages/ui/src/ListButtons.tsx` flips from
disabled+badge to live buttons (~10 lines).

**Acceptance**
- `cmd.toggleList('Bullet')` on a blank document produces a bulleted
  paragraph that survives `.docx` save/reload AND opens correctly in
  Word.exe with the bullet marker.
- Repeated Bullet→Number→Bullet→Number on the same paragraph does
  NOT inflate `numbering.xml` with duplicates.

**Out of scope**: multilevel list demote/promote (Tab/Shift-Tab
inside a list); restart numbering UI; custom bullet picker;
`<w:lvlOverride>` per-instance customization.

---

## Sprint 14 — Track Changes Recording (cross-cutting) ✅ DONE

**Status:** Closed 2026-05-27 (originally shipped in commit
`eb1ca60 feat(core+ui): Sprint 14 — Track Changes Recording (#14)`).
Issue #14 resolved.

  * **Engine parallel API**: `DocumentTree::tracked_insert_text`
    (line 2159) + `tracked_delete_range` (line 2300) sit alongside
    the original `insert_text` + `delete_range`. Same `Self`
    return type, same persistent-vector cloning. The lowest-level
    mutators were NOT modified — keeps Phase-1 PoC commands +
    visual-diff harness path untouched.
  * **Bridge-side gate**: `do_insert_text_interactive` (line 5978)
    reads `Engine.tracking_changes` and routes to the tracked
    variant. Selection-replace branch splits into
    `tracked_delete_range` + `tracked_insert_text`.
  * **Format-change tracking**: `tracked_format_change` (line 4090+
    in engine-wasm) captures pre-state attrs and stamps a Format
    revision when tracking is on.
  * **Undo/Redo discipline**: `Revision` lives INSIDE
    `Paragraph.revisions` (the document tree), not in a side
    table. `UndoStack::push(new_doc)` treats tracked + untracked
    snapshots identically — undoing a tracked Insert pops both
    the text and the revision metadata in one step.
  * **Boundary math** (per `tracked_insert_text` doc comment):
    typing inside an own-author Insert extends it (no per-
    keystroke fragmentation); typing inside a Delete splits the
    Delete + stamps fresh Insert; adjacent same-author Inserts
    merge; otherwise fresh `Insert(author, date, range)`.
  * **UI binding**: `packages/ui/src/ReviewControls.tsx` Track-
    Changes toggle binds to `state.isTrackingChanges()` reactive
    to every `Event::SelectionChanged.is_tracking_changes`.
    `TrackChangesSidebar.tsx` lists revisions with author + date;
    Accept / Reject buttons dispatch the existing Sprint 7
    `AcceptRevision` / `RejectRevision` commands.
  * Panic surface in the tracked mutators: 0.

---

## Sprint 14 — Original plan (archived)

**Issue:** [#14](https://github.com/IbraheemAlz/next-gen-editor/issues/14)

**Risk:** **Highest** — touches every text-mutation handler.
Ship LAST so other sprints' regressions can stabilize first.

**Deliverables**

1. **`Engine.tracking_changes: bool`** field (and a
   `Command::ToggleTrackChanges { enabled }` real handler).

2. **Gate every mutation** through a "record as revision" branch
   when tracking is on:
   - `InsertText` → wrap in `Revision { kind: Insert }`
   - `DeleteRange` / `DeleteAtCaret` → mark as `Revision { kind:
     Delete }` instead of slicing
   - `ReplaceRange` → combine: mark original as Delete, insert new
     as Insert
   - `ApplyFormatting` → new `RevisionKind::FormatChange {
     prev_attrs }` variant (additive bridge enum extension)
   - `PastePlain` / `PasteHtml` → funnel through InsertText path

3. **`Command::SetReviewIdentity { author, date }`** (new bridge
   variant) — UI calls when the user enters their identity. Engine
   stamps every new revision with these values. Default author
   `"You"` when unset.

4. **Adjacent revisions by same author within a short timewindow
   should merge** — Word's behaviour. Don't fragment runs.

5. **Edits inside an existing revision range need special
   handling**:
   - Typing inside a Delete should split the Delete or convert it.
   - Typing inside an Insert should grow the Insert.

6. **Undo + tracking interaction** — undoing a tracked edit removes
   the revision, does NOT stack a counter-revision. The undo stack
   already supports this via the immutable-tree snapshot model;
   verify the integration.

**UI follow-up**: `packages/ui/src/ReviewControls.tsx` drops the
"Engine pending" badge on the Track Changes toggle; the toggle's
data-active state now matches the real engine state (via
`SELECTION_CHANGED` extension — see Sprint 10's read-back pattern).

**Acceptance**
- With tracking on, typing produces `<w:ins>` overlays visible in
  the `TrackChangesSidebar` after each keystroke.
- Deleting wraps in `<w:del>`; text persists until Accept.
- Sprint 7's Accept/Reject path continues to work end-to-end.
- Tracking off: no regression to current Phase 4 behaviour.
- Round-trip: a tracked edit serializes as `<w:ins>` / `<w:del>` and
  re-loads identically.

---

## Cross-cutting: the running ledger

After each sprint commits + the UI follow-up removes the matching
"Engine pending" badge, update `UI_SURFACE_MAPPING.md`:

1. Flip the QA Status emoji from 🛑 Blocked → ✅ Wired in the
   affected row.
2. Drop the row from "Cross-Reference: Commands Without UI
   Consumers" + "Partial UI" sections.
3. Update the "Coverage summary" tally at the bottom.

The `gh-issue-logger` skill discipline (`.claude/skills/
gh-issue-logger/SKILL.md`) stays in force throughout: any NEW gap
discovered during a Core sprint gets a fresh issue filed before the
sprint commits.

## Estimated cadence

| Sprint | Effort | Calendar |
|---|---|---|
| 9 — I/O + Resolved Comments | ~1 week | week 1 |
| 10 — Read-back + Announcements | ~1 week | week 1-2 |
| 11 — Word count + Ruler | ~1-2 weeks | week 2-3 |
| 12 — Style Table | ~2-3 weeks | week 4-6 |
| 13 — Numbering Synthesis | ~2-3 weeks | week 4-6 (parallel) |
| 14 — Track Changes Recording | ~2-3 weeks | week 7-9 |

**Total**: ~6-9 calendar weeks of focused Core Engine work to close
every "Engine pending" badge currently in the UI shell. Sprints 12
and 13 can run in parallel branches if multiple agents are
collaborating, since they touch disjoint engine surfaces (style
cascade vs numbering store).

## Reading this doc next session

1. Skim `UI_SURFACE_MAPPING.md` for the authoritative status map.
2. Open this doc — pick the next sprint based on the dependency
   graph above + which UI Engine-pending badge you want to retire.
3. Open the matching GitHub issue (`#9`–`#17`) for the full
   acceptance criteria + scope + out-of-scope list.
4. Execute. Update the ledger when shipped.

Working tree at this checkpoint:
- HEAD `747796b feat(sdk): MVP UI shell (Sprints 1-8 UI Edition)`
- All 8 (UI Edition) sprints + meta-docs committed.
- Local branch `main` ahead of `origin/main` by 2 commits; no push
  performed (per checkpoint discipline).
- `pnpm-lock.yaml` untracked — promote to a tracked file in the
  next sprint's first commit so the workspace pin is reproducible.
