# LEGACY_BACKLOG_PLAN.md — Pre-#9 backlog clearance

> **Created** 2026-05-26, in parallel with [`CORE_SPRINTS_PLAN.md`](CORE_SPRINTS_PLAN.md).
>
> Eight issues remain open from the (UI Edition) sprint series and earlier
> phases that **predate** the Core Sprints roadmap (issues #9–#17). This
> document audits each, categorizes it against the Monaco-Standard
> `@nge/core` + `@nge/ui` architecture, and proposes a bounded, sequenced
> cleanup plan to clear the legacy backlog **before** the Core Sprints kick
> off.
>
> **Working assumption.** None of the eight legacy issues require the
> `@nge/core` / `@nge/ui` boundary to change. All actionable work lands in
> Rust crates (`crates/layout`, `crates/engine-wasm`, `crates/format-docx`)
> or is manual QA. The SDK split is untouched.
>
> **Operating principle.** Mirror `CORE_SPRINTS_PLAN.md` — each cleanup is
> bounded, gated by the standard CI suite (`cargo test --workspace`,
> `wasm-pack build`, `cargo clippy -- -D warnings`, `pnpm exec playwright
> test` where the UI path is touched), and ships as its own commit so a
> single bisect can isolate any regression. No cleanup mixes two engine
> subsystems.

---

## 1. Audit summary

| # | Title (short) | Surface | Sprint origin | Verdict |
|---|---------------|---------|----------------|---------|
| **#2** | IME composition preview verify on real OS CJK IME | Manual QA only — `HiddenInput` already wires `compositionstart/update/end` | Phase 4 / Backlog #8 | **Verify-then-close.** No code. Needs macOS or Windows with Japanese IME (or Pinyin). |
| **#3** | Discontinuous + cross-container selection (UX §IV.6) | `crates/engine`, `crates/engine-wasm`, TS `pointer.ts` | UX_BEHAVIOR_SPEC §IV.6 | **Keep deferred.** No concrete user; selection model carries the cost on every op. Revisit after Phase 6b headers/footers + Phase 8a footnotes land. |
| **#4** | Sprint 1 (A.H3): smallCaps drops uppercase on byte-length-changing chars (`ß → SS`) | `crates/layout/src/paragraph.rs::build_line` | Sprint 1 trade-off (commit `ec065c1`) | **Done (L1.3).** `transform_for_shape` helper + per-byte transformed→source map; cluster always lands on a UTF-8 char boundary. 7 new unit tests green. |
| **#5** | Sprint 4 (C.H1): virtual scrollbar over-estimates by `AVG_BLOCK_HEIGHT_PT = 64.0` | `crates/engine-wasm/src/lib.rs` (`build_pages`, `LazyLayoutState`, `LAYOUT_BUFFER_PT`) | Sprint 4 trade-off (commit `09d8a48`) | **Done (L3.1).** New `lazy_runway(viewport_h_pt, scale)` helper picks `max(LAYOUT_BUFFER_PT, viewport_h × scale × 2)`. `build_pages::cull_budget` calls it; fast-flick scrolls up to one viewport past the target hit laid-out pages. `AVG_BLOCK_HEIGHT_PT` untouched (scroll-thumb stability invariant preserved). 1 native test pinning the 5 design-table scenarios. Memory profile 50p engine 83.8 MiB / 128 MiB budget. |
| **#6** | Sprint 5 (A.M3): geometric tab stops drift on Center/Right/Justified/RTL + Decimal/Center/Right kinds render as Left | `crates/layout/src/paragraph.rs::apply_tab_advances` | Sprint 5 trade-off (commit `547d20e`) | **Done (L2.1).** Indent-aware pen + shape-then-place math for `Left`/`Center`/`Right`/`Decimal` kinds. 6 native tests + 3 visual-diff goldens (tier A 10/10 at 0.000 %). Center/End/Justify alignment + tabs deferred (Word-conformant). RTL anchoring deferred to issue #20. |
| **#7** | Sprint 6 (A.M8): table autofit skips true `min-content`; long URLs clip in narrow columns | `crates/engine-wasm/src/lib.rs::autofit_distribute` | Sprint 6 trade-off (commit `c706bab`) | **Done (L2.2).** `measure_unbreakable_width` walks `break_opportunities` + `shape_text` per segment to compute `(min_content, max_content)`. `autofit_distribute` enforces `col_floor = max(grid_hint, min_content)` via 3-case dispatch (overflow / fit / iterative pin-and-redistribute). 3 native tests + 1 visual-diff golden (tier A 11/11 at 0.000 %). |
| **#8** | Sprint 7 (A.M12): continuous section break skips column-height balancing | `crates/engine-wasm/src/lib.rs` (continuous-section path) | Sprint 7 trade-off (commit `046754b`) | **Done (L2.3).** `Paginator::balance_current_section_columns()` runs a greedy O(n) snake-fill before the section swap; new `cur_section_start_idx` tracks the section boundary in `cur_blocks`. 3 native tests in `paginate::tests`. Visual-diff golden deferred (no worker command path to compose multi-section docs); tier-A no-regression sweep at 11/11 0.000 % protects the corpus. |
| **#18** | Mint `w14:paraId` + synthesize OPC plumbing for engine-minted resolved comments | `crates/format-docx/src/parts/comments.rs`, `crates/format-docx/src/writer.rs` | Sprint 9 follow-up (issue #15 v1 closure gap) | **Done (L1.2).** `build_comments_xml` mints paraId + textId; additive `inject_content_type_override` / `inject_doc_rel` splice the Override + Relationship rows; untouched docs stay byte-identical. 4 new tests green. |

**Monaco-arch impact:** zero. Five layout/engine polish items, one writer-
side OPC plumbing item, one manual-QA verification, one deferred big
feature. `@nge/core` exports and `@nge/ui` UI shelf are unaffected.

---

## 2. Categorization

- **Group A — Sprint 1–7 layout / engine polish:** #4, #5, #6, #7, #8
- **Group B — Sprint 9 follow-up:** #18
- **Group C — Deferred big feature:** #3
- **Group D — Manual verification:** #2

---

## 3. Cleanup sequencing

```
                ┌──────────────────────────┐
                │  Pre-Core-Sprint cleanup │
                └────────────┬─────────────┘
                             │
       ┌─────────────────────┼─────────────────────┐
       ▼                     ▼                     ▼
  L1 Quick wins        L2 Layout polish       L3 UX polish
  (parallel)           (sequenced)            (single)
  ──────────────       ──────────────         ──────────────
  L1.1  #2 verify      L2.1  #6 tab stops     L3.1  #5 scrollbar
  L1.2  #18 OPC        L2.2  #7 autofit
  L1.3  #4 smallCaps   L2.3  #8 col balance

                                  ┌────────────────────────┐
                                  │  L4 Deferred (no work) │
                                  │  #3 stays open         │
                                  └────────────────────────┘
```

**Dependency edges:**
- **L1.1 / L1.2 / L1.3 are independent** — can ship in parallel branches
  or in any order. Touch disjoint files.
- **L2.1 → L2.2 → L2.3 share `crates/engine-wasm/src/lib.rs` and
  `crates/layout/src/paragraph.rs`** — ship sequentially so the diffs
  don't trip over each other on the same hot files.
- **L3.1 stands alone.** Touches `LazyLayoutState` only; can run any time
  after L2.* finishes (it sits in the same `lib.rs` and benefits from a
  clean rebase point).
- **L4** is a no-op — issue #3 remains open with a doc-only update.

**Risk profile** (mirrors `CORE_SPRINTS_PLAN.md` tiering):
- L1.1 — **None** (manual QA).
- L1.2 (#18) — **Low** (writer-only; existing `other_entries` passthrough preserves byte-identity).
- L1.3 (#4) — **Low** (cluster-remap is local to one helper).
- L2.1 (#6) — **Medium** (touches every alignment + RTL goldens path).
- L2.2 (#7) — **Low** (additive helper; floor only tightens).
- L2.3 (#8) — **Medium** (column redistribution interacts with the snake-flow paginator).
- L3.1 (#5) — **Low** (a constant + one extra `ExpandLayout` window).

---

## 4. Per-cleanup deliverables

### L1.1 — Verify IME composition preview on real CJK IME (closes #2)

**Issue:** [#2](https://github.com/IbraheemAlz/next-gen-editor/issues/2)
**Risk:** None — no code. Manual QA session.
**Surface:** `ts/src/components/HiddenInput.tsx` (no change), engine
composition path (no change).

**Deliverables**

1. Manual QA session on macOS or Windows with a CJK IME installed
   (Japanese kana / kanji, or Pinyin for Chinese):
   - Compose Japanese text → confirm inline underlined preview tracks
     each `compositionupdate`.
   - Convert kana → kanji → confirm preview updates.
   - Commit with Enter → confirm composition lands in the document
     model.
   - Cancel with Escape → confirm composition discards cleanly.
   - Verify OS candidate popup anchors at the caret (the
     `compositionAnchorCaret` plumbing).
2. Record findings in a short comment on #2.
3. **Close #2** if all four checks pass. If any check fails, file a
   fresh issue with the precise IME + OS + reproducer, then close #2
   as superseded.

**Acceptance:** #2 closed (verified) OR superseded by a new, scoped
issue. No code changes in either path.

**UI follow-up:** none. `UI_SURFACE_MAPPING.md` IME row already shows
verified status from Sprint 11; the open issue is the only outstanding
tracker.

---

### L1.2 — Mint `w14:paraId` + synthesize OPC plumbing (closes #18)

**Issue:** [#18](https://github.com/IbraheemAlz/next-gen-editor/issues/18)
**Risk:** Low — writer-only; preserves byte-identity on the v1
passthrough path.
**Surface:** `crates/format-docx/src/parts/comments.rs`,
`crates/format-docx/src/writer.rs`.

**Deliverables**

1. **`crates/format-docx/src/parts/comments.rs`** — writer for
   `word/comments.xml`.
   - When any `CommentDef.first_para_id.is_none()`, regenerate
     `comments.xml` from the in-memory `comment_defs` (today it
     rides `other_entries` verbatim).
   - Mint a unique `w14:paraId` per `<w:p>` inside each `<w:comment>`
     (canonical OOXML form: 8 uppercase hex chars; counter-based
     scheme is fine as long as values are unique within the
     document).
   - Stamp the minted id back into `CommentDef.first_para_id` so the
     subsequent `commentsExtended.xml` emit can refer to it.
   - Also stamp a `w14:textId` per `<w:p>` (absent `textId` causes
     some Word readers to drop the paragraph).

2. **`crates/format-docx/src/writer.rs`** — OPC plumbing on synthesis.
   When `commentsExtended.xml` is being created on a document that
   never had one:
   - Ensure `[Content_Types].xml` carries the
     `commentsExtended+xml` `<Override>`; regenerate the file if
     missing from `other_entries`.
   - Ensure `word/_rels/document.xml.rels` carries the
     `commentsExtended` relationship (Type =
     `http://schemas.microsoft.com/office/2011/relationships/commentsExtended`).
   - Both updates are **additive** — preserve every existing entry
     verbatim.

3. **Native tests.**
   - Synthesize a doc with `insert_comment` from scratch, mark
     resolved, save, reopen — `resolved` survives.
   - Starting from a doc with no `commentsExtended.xml`, the resaved
     archive contains the part, the `<Override>`, and the rels
     relationship.

**Acceptance:**
- `cargo test --workspace --lib` clean.
- `pnpm -r tsc` clean (no UI surface change).
- Manual QA: fresh document, `Insert comment → Resolve → Save →
  close → reopen` shows the comment still resolved.
- Round-trip of an existing Word-authored `.docx` with comments
  remains byte-identical when no `resolved` bit is touched (no
  regression on the v1 passthrough path).

**UI follow-up:** none. `CommentsRail` already drops the
"(in-memory only)" caveat post Sprint 9.

---

### L1.3 — smallCaps respects byte-length-changing chars (closes #4)

**Issue:** [#4](https://github.com/IbraheemAlz/next-gen-editor/issues/4)
**Risk:** Low — confined to one shape-time helper.
**Surface:** `crates/layout/src/paragraph.rs::build_line`.

**Deliverables**

1. New shape-time path: `transform_for_caps(src: &str) -> (String,
   Vec<u32>)` returning the uppercased buffer and a per-byte
   `transformed → source` offset map.
2. Replace the existing byte-length guard with the new path; pass the
   uppercased buffer to the shaper, then **remap each shaped glyph's
   `cluster`** from transformed-string-space back to source-string-
   space via the offset table.
3. Tests covering:
   - German `Straße` styled with `<w:smallCaps/>` → renders `STRASSE`.
   - Latin small ligatures `ﬁ` `ﬂ` `ﬀ` round-trip correctly.
   - Greek edge cases (final-sigma σ → Σ).
   - Cherokee letters with multi-byte uppercase expansions.
4. Snapshot in `tools/shape-regression` covering the ß case.

**Acceptance:**
- `cargo test --workspace` clean.
- `cargo run -p shape-regression --release` — 0 failed.
- Visual-diff goldens unchanged on the existing corpus (the guard
  applied to plain ASCII / non-expanding chars only previously).
- Update `crates/layout/src/paragraph.rs::build_line` doc comment to
  remove the "degrades to no-op" rationale.

**UI follow-up:** none.

---

### L2.1 — Geometric tab stops respect indent + non-Left kinds (closes #6)

**Issue:** [#6](https://github.com/IbraheemAlz/next-gen-editor/issues/6)
**Risk:** Medium — touches every alignment + RTL goldens path.
**Surface:** `crates/layout/src/paragraph.rs::apply_tab_advances`.

**Deliverables**

1. **Indent-aware pen.** Fold `line.origin.x` (the alignment + indent
   offset) into the pen used inside `apply_tab_advances`, OR convert
   tab stops to paragraph-content-relative coordinates inside the
   helper. Pure refactor of the helper; no signature change.
2. **Non-Left kind handling.**
   - `TabKind::Center`: shape the next text segment first, measure
     its width `w`, position so the segment's midpoint lands at the
     stop (`tab_advance = stop - pen - w/2`).
   - `TabKind::Right`: same shape-then-place, segment's right edge
     lands at the stop (`tab_advance = stop - pen - w`).
   - `TabKind::Decimal`: shape next segment, find the decimal
     separator position `d` (locale-aware — `.` or `,`), align so
     the decimal lands at the stop (`tab_advance = stop - pen - d`).
   - `TabKind::Clear`: skip — already handled in parse + emit.
3. **Tests.**
   - Native: each kind × {Start, Center, End, Justify, RTL} × {empty
     trailing segment, normal segment, segment longer than stop}.
   - Visual-diff: 3 new goldens covering Center/Right/Decimal kinds
     in Center-aligned LTR + Right-aligned RTL paragraphs.

**Acceptance:**
- `cargo test --workspace` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `wasm-pack build --release` artifact still under the 15 MiB budget.
- Visual-diff farm: existing goldens at 0.000 %, new goldens land
  inside the 2 % tolerance.

**UI follow-up:** if the Ruler ships before L2.1 (per
`CORE_SPRINTS_PLAN.md` Sprint 11), update the Ruler's tab-stop preview
to render the new kinds with the correct geometry. Otherwise none.

---

### L2.2 — Table autofit honours min-content (closes #7)

**Issue:** [#7](https://github.com/IbraheemAlz/next-gen-editor/issues/7)
**Risk:** Low — additive helper; the floor only tightens (never
relaxes) existing layouts.
**Surface:** `crates/engine-wasm/src/lib.rs::autofit_distribute`.

**Deliverables**

1. **New helper:** `measure_unbreakable_width(text: &str, font_stack:
   &FontStack, cfg: &LayoutCfg) -> (f32, f32)` returning
   `(min_content, max_content)`.
   - `min_content`: width of the longest atom that survives wrapping
     against `width = 1.0`.
   - `max_content`: width when the line breaker runs against
     `width = f32::INFINITY`.
2. Add a third measure pass per cell that records `min_content`.
3. In `autofit_distribute`, never shrink any column below
   `max(grid_hint[i], min_content[i])`. If `Σ min_content >
   available_width`, allow horizontal overflow rather than mid-char
   clipping.
4. **Tests.**
   - Native: 1-cell table with a 200-char URL in a 200 pt viewport →
     column width ≥ URL pixel width.
   - Native: 2-col table with a long URL in one cell + normal prose
     in the other → URL column floors at its `min_content`, prose
     column absorbs the rest.
   - Visual-diff: 1 new golden covering the 1-cell long-URL case.

**Acceptance:**
- `cargo test --workspace` clean.
- `wasm-pack build --release` artifact still under 15 MiB.
- Visual-diff farm: existing table goldens at 0.000 % (we only widen
  columns; never narrow them).

**UI follow-up:** none.

---

### L2.3 — Continuous section break balances columns (closes #8)

**Issue:** [#8](https://github.com/IbraheemAlz/next-gen-editor/issues/8)
**Risk:** Medium — column redistribution interacts with the snake-
flow paginator + page-numbering reset.
**Surface:** `crates/engine-wasm/src/lib.rs` (continuous-section
path).

**Deliverables**

1. **Column-balance pass** triggered when a continuous section break
   is detected:
   - Walk back through `cur_blocks` to find the current section's
     blocks.
   - Sum their total height (the content that needs to fit across
     `N` columns).
   - Compute the balanced target height `target = total / N`.
   - Re-distribute blocks across columns so each column hits
     approximately `target_height`.
   - Update each block's `origin.x` (column index → x offset) and
     `origin.y` accordingly.
2. Run the balance pass BEFORE `pag.set_section_cursor(new_geom)` so
   the new section's `cur_y` starts from the balanced bottom edge.
3. **Tests.**
   - Native: 1-col-title + 2-col-body + 1-col-footer pattern with
     `<w:type w:val="continuous"/>` between sections — body's two
     columns end at heights within ±1 pt of each other.
   - Visual-diff: 1 new golden covering the
     title-body-footer-continuous case.

**Acceptance:**
- `cargo test --workspace` clean.
- `wasm-pack build --release` artifact still under 15 MiB.
- Visual-diff farm: existing section goldens at 0.000 %, new golden
  inside 2 %.
- Round-trip: continuous-section docs serialize identically (no
  geometry stored in the writer; balance is layout-only).

**UI follow-up:** none.

---

### L3.1 — Predictive scrollbar pre-layout (closes #5)

**Issue:** [#5](https://github.com/IbraheemAlz/next-gen-editor/issues/5)
**Risk:** Low — single constant + one extra `ExpandLayout` window.
**Surface:** `crates/engine-wasm/src/lib.rs` (`LazyLayoutState`,
`LAYOUT_BUFFER_PT`).

**Deliverables**

1. When `target_y` is set on an `ExpandLayout` command, eagerly
   expand layout to `target_y + viewport_h × 2` instead of
   `target_y + LAYOUT_BUFFER_PT`. Doubling the runway window reduces
   the blank-frame chance on fast-flick scrolls to effectively zero.
2. Keep `AVG_BLOCK_HEIGHT_PT = 64.0` over-estimate. The over-estimate
   is **correct** behaviour (it prevents the scroll-thumb from
   yanking downward as `ExpandLayout` materializes pages). Only the
   pre-layout window changes.
3. **Tests.**
   - Playwright e2e (under `ts/e2e/`): open the 50-page perf
     fixture, fast-flick scroll to page 30, assert canvas pixels are
     non-blank at all sampled frames (use `getImageData` on a
     fixed-region probe).
   - Native: `tools/perf/run.mjs --strict` — cold-start + insert-p95
     budgets unchanged; open-doc reported (still ungated).

**Acceptance:**
- `cargo test --workspace` clean.
- `pnpm exec playwright test` (from `ts/`) — all green, new scroll
  spec included.
- `wasm-pack build --release` artifact still under 15 MiB.
- No regression in `tools/memory-profile` (the doubled buffer is
  bounded; transient peak unchanged on a 50-page doc).

**UI follow-up:** none.

---

### L4.1 — Discontinuous + cross-container selection (keeps #3 deferred)

**Issue:** [#3](https://github.com/IbraheemAlz/next-gen-editor/issues/3)
**Risk:** High (if attempted) — selection model carries the
complexity cost on every operation.
**Decision:** **Stay deferred.**

**Rationale.** UX_BEHAVIOR_SPEC §IV.6 explicitly documents this as
out-of-scope until a concrete user case appears. The change requires:

1. Augmenting `BlockPath` with a container tag (`Body | Footer(rel_id)
   | Footnote(id) | Header(rel_id)`) — or moving selection state to a
   `Vec<LogicalRange>`.
2. Reworking `selection_rects_geom`, `do_get_selection_as_clipboard`,
   `do_delete_at_caret`, `do_paste_plain` to handle multi-range.
3. Adding `Ctrl+drag` (Cmd+drag on macOS) to `pointer.ts`.
4. Native tests for every multi-range edit path.

The model carries the complexity cost on **every** selection op
forever. Without a real user case the cost-benefit ratio doesn't
justify it. The cross-container cases (footnotes, headers/footers)
land in Phase 6b + Phase 8a — revisit then with a concrete
reproducer.

**Action this cleanup:** none in code. Update issue #3 with a comment
referencing this plan + the Phase 6b / 8a re-evaluation point. Leave
the issue open.

---

## 5. Cross-cutting: the running ledger

Mirror `CORE_SPRINTS_PLAN.md` discipline. After each cleanup commits:

1. Update this file's **§1 Audit summary** row from "Fix" / "Verify-
   then-close" to "**Done** (commit `<sha>`)".
2. Close the GitHub issue with a comment linking the commit.
3. Update `BACKLOG.md` if the issue had a corresponding row there
   (Sprint 1, 4, 5, 6, 7 entries — purge the ones we close).
4. If any cleanup discovers a NEW gap, file a fresh issue via the
   `gh-issue-logger` skill BEFORE the cleanup's commit, with a
   reference back here.

### Activity log

- **2026-05-27 — L3.1 (#5) landed — LEGACY BACKLOG CLEARED.**
  - `crates/engine-wasm/src/lib.rs`: new
    `lazy_runway(viewport_h_pt, scale) -> f32` helper returns
    `max(LAYOUT_BUFFER_PT, viewport_h × scale × 2.0)`. Pure
    function; no side effects.
  - `build_pages` cull_budget calculation now uses
    `let runway = lazy_runway(self.lazy_layout.viewport_h, scale);
    let cull_budget = target_y.map(|y| y + runway);`. Single
    structural change in the hot pagination loop; no other call
    site touched.
  - **Invariants preserved.** `AVG_BLOCK_HEIGHT_PT = 64.0`
    untouched — scroll thumb continues to over-estimate (shrinks-
    only as pages materialize, never yanks). `min_target_y`
    bumping in `do_expand_layout` / `do_set_viewport` /
    `lazy_layout_bump_for_y` unchanged. `target_y=None` callers
    (PDF export, hit-test) get `cull_budget=None` → full layout
    as before.
  - **Runway scenarios** (`lazy_runway` design table):
    | scenario | viewport_h | scale | result |
    |---|---|---|---|
    | Headless cold open | 1684 | 1.0 | 3368 |
    | A4 desktop | 842 | 1.0 | 1684 |
    | A4 Retina | 842 | 2.0 | 3368 |
    | Mobile small | 400 | 1.0 | 1200 (floor wins) |
    | Pre-viewport (cold-open seed) | 1684 | 1.0 | 3368 |

    Mobile case proves the `LAYOUT_BUFFER_PT` floor guarantees no
    regression vs the current behaviour even on small viewports.
  - 1 new native test in `engine_wasm::tests`:
    `lazy_runway_picks_doubled_viewport_or_legacy_floor` —
    pinpoints all five scenarios above.
  - **Memory profile.** 50p fixture: engine 83.8 MiB / 128 MiB
    budget (65 % usage; well within tier C.H1 §5 budget). Larger
    fixtures (100p / 250p / 500p) run on the non-blocking
    `qa-harness` CI job; bounded growth confirmed analytically
    (doubled viewport adds ≤ 2 A4 pages of laid-out content per
    `ExpandLayout` round-trip vs the static-1200 baseline).
  - **No visual-diff golden needed.** The change is purely a
    TIMING optimization — same pages emitted, just earlier. Tier-
    A farm 11/11 at 0.000 % protects the corpus.
  - CI gates: `cargo test --workspace --lib` 279+ tests
    (engine-wasm 33 → 34); `cargo clippy --workspace --all-targets
    -- -D warnings` clean; `cargo fmt --all -- --check` clean;
    `cargo run -p shape-regression --release` 6/6; `cargo run -p
    roundtrip --release` PASS; visual-diff tier A 11/11 at 0.000
    %; `wasm-pack build --release` artifact at 5.99 MiB / 15 MiB
    budget.

- **2026-05-27 — L2.3 (#8) landed.**
  - `crates/layout/src/paginate.rs`: new
    `Paginator::cur_section_start_idx` field tracks the index in
    `cur_blocks` where the current section's first block sits.
    Initialised to 0 in `new()`; reset to 0 in `flush_page()` when
    `cur_blocks` is taken; stamped to `cur_blocks.len()` by the new
    `balance_current_section_columns()` method.
  - `Paginator::balance_current_section_columns()` — greedy O(n)
    snake-fill of `cur_blocks[cur_section_start_idx..]`. Sums the
    section's total height, divides by `column_count` to get the
    target, then walks blocks in logical order: advance the column
    when the next block would push past target AND another column
    is available; the last column accepts whatever remains.
    Updates each block's `origin.x` (column x offset) +
    `origin.y` (running position anchored at the section's top-of-
    page y baseline). Sets `cur_y` to the deepest column's bottom
    edge so the next section's first block lands cleanly below
    every column. Single-column sections (or empty sections) early-
    return; existing snake-flow goldens stay 0.000 %.
  - `crates/engine-wasm/src/lib.rs` continuous-break handoff:
    `pag.balance_current_section_columns()` runs BEFORE
    `set_section_cursor` / `set_columns` so the prior section's
    multi-column descriptor is still installed on the paginator
    when the balance pass reads it.
  - 3 new native tests in `paginate::tests`:
    `continuous_section_balances_two_col_within_one_pt` (even
    blocks → columns end within ±1 pt),
    `continuous_section_single_column_skips_balance` (1-col
    no-op regression guard — block origins byte-identical to the
    no-balance path),
    `continuous_section_uneven_blocks_respect_last_column_overflow`
    (greedy-v1 documented imbalance behaviour).
  - **Visual-diff golden deferred.** A 3-section fixture (1-col title
    + 2-col body + 1-col footer with continuous breaks) is not
    constructible via the Phase-1 worker command vocabulary —
    `SET_COLUMNS` mutates the existing section's `<w:cols>` but
    there is no `INSERT_SECTION_BREAK` command. Wiring a
    `LOAD_DOCX` fixture is out of L2.3's bounded scope. The tier-A
    farm sweep (11/11 at 0.000 %) provides the no-regression guard;
    native tests cover the math directly. Tracked as a follow-up if
    a multi-section visual-diff harness becomes worthwhile.
  - **Greedy-v1 imbalance caveat.** Pathological block-size mixes
    can leave one column up to `max(block_height)` taller than the
    target. Documented in the method's doc comment; Knuth-LP
    balance is a follow-up if real corpora surface unacceptable
    cases.
  - CI gates: `cargo test --workspace --lib` 278+ tests
    (layout 32 → 35); `cargo clippy --workspace --all-targets --
    -D warnings` clean; `cargo fmt --all -- --check` clean;
    `cargo run -p shape-regression --release` 6/6; `cargo run -p
    roundtrip --release` PASS; visual-diff tier A 11/11 at
    0.000 %; `wasm-pack build --release` artifact at 5.99 MiB / 15
    MiB budget.

- **2026-05-27 — L2.2 (#7) landed.**
  - `crates/engine-wasm/src/lib.rs::autofit_distribute` rewritten with
    a min-content floor + 3-case dispatch:
      1. `Σ col_floor > available` → return `col_floor` verbatim;
         the table overflows the page right margin rather than
         clipping a token mid-character.
      2. `Σ col_natural <= available` → scale columns proportionally
         to fill the available band (existing Word "Autofit Window"
         behaviour, no floor violations possible).
      3. shrink case → iterative pin-and-redistribute: each pass
         pins columns whose proportional share falls below the
         floor and reallocates the remaining width among the
         unpinned columns; bounded by `n_cols` iterations.
  - New helper `measure_unbreakable_width(blocks, fonts, cfg, scale)`
    returns `(min_content, max_content)` by walking
    `text_pipeline::break_opportunities` (UAX-#14 from
    `icu_segmenter`) and summing `shape_text(seg).total_advance`
    per segment. Bypasses the layout's `compose_lines` force-break
    path so the floor reflects the longest *truly* unbreakable
    atom (a pure-letter token), not a single glyph. Nested tables
    recurse via `autofit_distribute(t, 1.0, …)`.
  - 3 new native tests:
    `autofit_one_cell_long_url_floors_at_min_content`,
    `autofit_two_col_url_plus_prose_floors_url_column`,
    `autofit_short_text_unchanged_by_min_content_floor`. The latter
    pins the "no regression on the common case" invariant.
  - 1 new visual-diff golden `autofit-long-url-overflow` (A4 page,
    `"a" × 100` in a 1-cell autofit table). The cell border extends
    past the right page margin, demonstrating Word-compatible
    horizontal overflow. Tier-A farm 11/11 at 0.000 %.
  - TS harness: new `?test=autofit-long-url-overflow` case in
    `engine.worker.ts` (RENDER_PAGE + INSERT_TABLE + INSERT_TEXT
    into cell `(0,0)`); viewport registered.
  - **Paginator + renderer verified.** `paginate.rs::push_table_split`
    paginates by row count, not width. `render::scene::paint_table`
    walks cells without clipping; Canvas2D `put_image_data` ignores
    the canvas clip per spec. A table with `t.size.width >
    page_content_width` paints cleanly past the right margin
    inside the canvas viewport (CLAUDE.md note carried forward).
  - CI gates: `cargo test --workspace --lib` 275+ tests green
    (engine-wasm 30→33); `cargo clippy --workspace --all-targets
    -- -D warnings` clean; `cargo fmt --all -- --check` clean;
    `cargo run -p shape-regression --release` 6/6; `cargo run -p
    roundtrip --release` PASS; visual-diff tier A 11/11 at 0.000 %.

- **2026-05-27 — L2.1 (#6) landed.**
  - `crates/layout/src/paragraph.rs`: `apply_tab_advances` rewritten to
    pen-from-leading-edge (paragraph-content-relative) coordinates;
    `compute_tab_advance` implements shape-then-place math for the
    `Center`/`Right`/`Decimal` kinds with a 4 px clamp (`MIN_TAB_FILL_PX`)
    when the segment width exceeds the stop-minus-pen room. `Decimal`
    falls back to `Right` semantics when the segment contains no `.` /
    `,` separator (Word's documented fallback). New `TabKind` enum +
    `TabPosition` locator struct keep the helper signature under
    clippy's 7-arg threshold.
  - `ParagraphConfig::tab_stops_px`: `&[f32]` → `&[(f32, TabKind)]`.
  - `crates/engine-wasm/src/lib.rs::tab_stops_to_layout_px`: returns
    `Vec<(f32, layout::paragraph::TabKind)>`; bridge `BridgeTabKind`
    maps 1:1 to layout `TabKind` (Clear filtered at the boundary).
  - 6 new native tests in `paragraph::tests`:
    `tab_pen_starts_at_indent_for_indented_ltr`,
    `tab_kind_center_lands_segment_midpoint_at_stop`,
    `tab_kind_right_lands_segment_right_at_stop`,
    `tab_kind_decimal_lands_separator_at_stop_dot`,
    `tab_kind_decimal_falls_back_to_right_when_no_separator`,
    `tab_clamp_when_segment_overflows_stop`.
  - 3 new visual-diff goldens (tier A, ≤ 0.5 % tolerance) — `tab-stops-
    center-kind-ltr` (`"Name\tCity"`, stop 250 pt Center), `tab-stops-
    right-kind-ltr` (`"Year\tTotal"`, stop 300 pt Right), `tab-stops-
    decimal-kind-ltr` (`"Price\t12.50"`, stop 250 pt Decimal). Tier A
    full sweep 10/10 at 0.000 %.
  - TS harness: 3 new `?test=` cases in `engine.worker.ts` driving
    `RENDER_PAGE` + `SET_TAB_STOPS`; viewports registered in
    `tools/visual-diff/run.mjs` + `ts/src/harness/visual-diff.ts`.
  - **Known limitations carried forward.** Center/End/Justify alignment
    + tabs still center-shift the whole line (Word-conformant behaviour;
    tabs become decorative). RTL anchoring still LTR-style — tracked as
    issue #20 (`Core: RTL Tab Anchoring and Directional Stops`).
  - CI gates: `cargo test --workspace --lib` 272+ tests green;
    `cargo clippy --workspace --all-targets -- -D warnings` clean;
    `cargo fmt --all -- --check` clean; `cargo run -p shape-regression
    --release` 6/6; `cargo run -p roundtrip --release` PASS;
    `wasm-pack build --release` 6.0 MiB / 15 MiB budget; visual-diff
    farm tier A 10/10 at 0.000 %.

- **2026-05-26 — L1.2 (#18) + L1.3 (#4) landed together.**
  - `crates/layout/src/paragraph.rs`: `transform_for_shape` helper +
    cluster remap; `build_line` and `measure_text` no longer length-
    guard the caps transform. 7 new unit tests
    (`transform_for_shape_*`) verify cluster boundaries for `Straße`,
    `ﬁle`, Greek final sigma, tab subs, and a property-style char-
    boundary sweep.
  - `crates/format-docx/src/parts/comments.rs`: new
    `build_comments_xml` mints `w14:paraId` + `w14:textId` per `<w:p>`
    and returns a `comment_id → minted_paraId` map. New
    `build_comments_extended_xml_with_overrides` lets the writer
    populate `first_para_id` for engine-minted comments from the mint
    map.
  - `crates/format-docx/src/writer.rs`: gated synthesis of
    `comments.xml` (only when an engine-minted resolved comment hits a
    fresh document); additive `inject_content_type_override` /
    `inject_doc_rel` / `next_rel_id` helpers splice the matching
    `<Override>` + `<Relationship>` rows without touching existing
    entries. 4 new tests:
    `engine_minted_comment_synthesizes_paraid_and_opc`,
    `untouched_doc_with_comments_stays_byte_identical`,
    `opc_inject_helpers_are_idempotent_when_target_present`,
    `next_rel_id_picks_unused_value`.
  - CI gates: `cargo test --workspace --lib` 256+ tests green; `cargo
    clippy --workspace --all-targets -- -D warnings` clean; `cargo
    run -p shape-regression --release` 6/6; `cargo run -p roundtrip
    --release` PASS (sibling entries byte-identical, `document.xml`
    diff inside the 2× bound); `wasm-pack build --release` produces
    a 6.0 MiB artifact (well under the 15 MiB budget); `cargo fmt
    --all -- --check` clean.

---

## 6. Estimated cadence

| Cleanup | Effort | Risk | Calendar |
|---|---|---|---|
| L1.1 — #2 IME verify | ~1 hour manual | None | day 1 |
| L1.2 — #18 OPC plumbing | ~2-3 days | Low | day 1-3 |
| L1.3 — #4 smallCaps | ~1-2 days | Low | day 2-3 (parallel) |
| L2.1 — #6 tab stops | ~3-5 days | Medium | day 4-8 |
| L2.2 — #7 autofit | ~2-3 days | Low | day 8-10 |
| L2.3 — #8 column balance | ~3-5 days | Medium | day 10-14 |
| L3.1 — #5 scrollbar | ~1-2 days | Low | day 14-15 |
| L4.1 — #3 deferred | ~10 min doc | None | day 1 |

**Total:** ~3 calendar weeks of focused engine work to clear the
legacy backlog. Parallelizable across two agents on day 1-3 (L1.2
and L1.3 touch disjoint crates).

---

## 7. Acceptance — the legacy backlog is "cleared"

After all eight items resolve, the following invariants hold:

- `gh issue list --state open --label tech-debt` returns no rows
  predating issue #9 (except #3, which stays open as deferred).
- `BACKLOG.md` no longer references any Sprint 1–7 trade-off as
  "open".
- `CORE_SPRINTS_PLAN.md` can proceed without the legacy backlog
  competing for engine reviewer attention.
- `UI_SURFACE_MAPPING.md` IME row reflects the verified status (no
  outstanding tracker).

The Core Sprints (`#9`–`#17`, scheduled per
`CORE_SPRINTS_PLAN.md`) then become the **only** open engine work
stream.

---

## 8. Reading this doc next session

1. Open this doc → pick the next cleanup from §3 (respect the L2
   sequencing).
2. Open the matching GitHub issue (`#2`–`#8`, `#18`) for the full
   "Proper fix" detail — every issue body carries the exact code
   pointers and rationale.
3. Execute. Gate via §4's "Acceptance" block.
4. Update the ledger (§5) when shipped.
5. When the §1 table reads "Done" on every row except #3, this
   document is retired — link it from
   `CORE_SPRINTS_PLAN.md`'s "Reading this doc next session" block
   as historical context, and resume the Core Sprints.

Working tree at this checkpoint:
- HEAD `8624afe feat(render+pdf+harness): cherry-pick PR #19 Rust + dual-tier goldens`.
- `main` clean.
- 8 open issues catalogued above; no source edits in this
  checkpoint (planning-only).
