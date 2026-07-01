# UI_SURFACE_MAPPING.md — Engine ↔ UI Coverage Audit

> **Status:** Engine is feature-complete through Consolidated Sprints 1–8, Phase-5 Backlog Sprints 1–12, and the UI Edition sprints (`v0.6.0-beta.2` lineage). The original audit's "many high-value engine capabilities have zero discoverable UI affordance" verdict is resolved: the Sprints 1–8 (UI Edition) shelf wave landed the `packages/ui/` components (see the 2026-07-02 appendix note), leaving only genuinely engine-less rows blocked. This document inventories every engine feature, the bridge surface that drives it, the UI component required to exercise it manually, and the current QA status.
>
> **Authoritative sources:** `crates/bridge/src/{command,event,common,telemetry}.rs`, `crates/engine-wasm/src/lib.rs`, `packages/ui/src/`, `packages/core/src/`, `ts/src/components/`, `ts/src/input/`, `BACKLOG.md`, and the git history for Sprints 1–8.
>
> **Legend — QA Status:**
> - ✅ **Wired** — UI affordance exists; manual QA possible end-to-end.
> - ⚠️ **Partial** — Some paths reachable; significant interactions blocked.
> - 🛑 **Blocked — Missing UI** — Engine ships the capability; no manual entry point.
> - 🕳 **Latent** — Engine API surfaced but only data-preserved on round-trip (no render/edit path yet).

---

## Table of Contents

1. [Typography & Character Formatting](#1-typography--character-formatting)
2. [Paragraph Formatting](#2-paragraph-formatting)
3. [Lists, Numbering & Outline Cascade](#3-lists-numbering--outline-cascade)
4. [Tabs & Indentation](#4-tabs--indentation)
5. [Pagination, Sections & Page Layout](#5-pagination-sections--page-layout)
6. [Tables](#6-tables)
7. [Images & Inline Objects](#7-images--inline-objects)
8. [Selection, Caret & Navigation (BiDi-aware)](#8-selection-caret--navigation-bidi-aware)
9. [Clipboard & Drag-and-Drop](#9-clipboard--drag-and-drop)
10. [Undo / Redo](#10-undo--redo)
11. [IME / Composition](#11-ime--composition)
12. [Track Changes & Revisions](#12-track-changes--revisions)
13. [Comments & Annotations](#13-comments--annotations)
14. [Fields & Dynamic Content](#14-fields--dynamic-content)
15. [Document I/O — Open / Save / Export](#15-document-io--open--save--export)
16. [Viewport, Zoom & Performance Controls](#16-viewport-zoom--performance-controls)
17. [Accessibility Tree](#17-accessibility-tree)
18. [Engine Lifecycle, Telemetry & Diagnostics](#18-engine-lifecycle-telemetry--diagnostics)
19. [Sprint 1 (UI Edition) — Low-Hanging Fruit](#sprint-1-ui-edition--low-hanging-fruit)
20. [Cross-Reference: Commands Without UI Consumers](#cross-reference-commands-without-ui-consumers)

---

## 1. Typography & Character Formatting

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Bold** | `Command::ApplyFormatting { patch.bold }` | Toolbar Bold button + `Ctrl/Cmd+B` shortcut | ✅ Wired (`Toolbar.tsx:152`) |
| **Italic** | `Command::ApplyFormatting { patch.italic }` | Toolbar Italic button + `Ctrl/Cmd+I` shortcut | ✅ Wired (`Toolbar.tsx:152`) |
| **Underline (single)** | `Command::ApplyFormatting { patch.underline: Single }` | Toolbar Underline button + `Ctrl/Cmd+U` shortcut | ✅ Wired |
| **Underline (Double / Dotted / Dashed / Wavy)** | `Command::ApplyFormatting` w/ `UnderlineStyle::{Double,Dotted,Dashed,Wavy}` | Toolbar Underline **dropdown** (chevron beside U) → style picker | ✅ Wired (Sprints 1–8 UI Edition) — `UnderlineStyleDropdown.tsx` |
| **Strikethrough** | `Command::ApplyFormatting { patch.strike }` | Toolbar Strikethrough button + `Alt+Shift+5` | ✅ Wired |
| **Superscript / Subscript** | `Command::ApplyFormatting` w/ `VerticalScript::{Superscript,Subscript}` | Toolbar `X²` / `X₂` buttons + `Ctrl+Shift+=` / `Ctrl+=` shortcuts | ✅ Wired (Sprints 1–8 UI Edition) — `SuperSubButtons.tsx` via `cmd.setVerticalScript` |
| **All-Caps / Small-Caps transform** | `Command::ApplyFormatting { patch.caps / patch.small_caps }` | Toolbar `Aa` toggle + dropdown (All / Small) | ✅ Wired — `CapsButtons.tsx`; `TextAttrsPatch` grew `caps` + `small_caps` fields (`command.rs:665`) |
| **Font family (per-run)** | `Command::ApplyFormatting { patch.font_family }` | Toolbar Font Family picker (combobox over loaded faces) | ✅ Wired (FontFamilyPicker, Sprint 6) |
| **Font size (pt)** | `Command::ApplyFormatting { patch.font_size }` | Toolbar Font Size combobox (8–72 pt + free input) | ✅ Wired |
| **Foreground colour** | `Command::ApplyFormatting { patch.color }` | Toolbar Colour swatch + colour-picker popover | ✅ Wired |
| **Background highlight** | `Command::ApplyFormatting { patch.bg_color }` | Toolbar Highlight swatch (yellow default + picker) | ✅ Wired |
| **Script tag** (`Script::Arabic`/`Hebrew`/`Han`/…) | `Command::ApplyFormatting { patch.script }` | Hidden — driven by ICU script detection on input | ⚠️ Partial — engine auto-tags; no manual override UI (rarely needed) |
| **Language tag** (BCP-47) | `Command::ApplyFormatting { patch.language }` | Status-bar Language picker (en-US, ar-EG, …) — drives hyphenation & spell-check downstream | 🛑 Blocked — round-trips through `<w:rPr><w:lang>` but no UI |
| **Faux bold / italic synthesis** | Automatic when face lacks weight/style (Sprint 12) | None — diagnostic toggle in Dev HUD ("show faux indicator") would help QA | 🕳 Latent — auto-applies, no inspector |
| **Clear character formatting** | `Command::ApplyFormatting` w/ all-`None` patch + reset to paragraph default | Toolbar `Clear formatting` button + `Ctrl+\` shortcut | 🛑 Blocked — no UI |

---

## 2. Paragraph Formatting

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Alignment** (Start / End / Center / Justify) | `Command::SetParagraphAlign { alignment }` | Toolbar `AlignmentPicker` (4 icon buttons) + `Ctrl+L/E/R/J` shortcuts | ✅ Wired (`Toolbar.tsx:169`) — keyboard shortcuts **missing** |
| **Direction override** (LTR / RTL) | `Command::SetParagraphDirection { direction }` | Toolbar Direction buttons (Phase 4) | ✅ Wired (`Toolbar.tsx:194`) |
| **Auto-direction (first-strong, UAX-#9 P2/P3)** | Engine auto-detects on insert (Sprint 5) | Dev HUD indicator showing detected direction; no user control | 🕳 Latent |
| **Paragraph borders** (`<w:pPr><w:pBdr>`) | Rendered from style; no `Command::SetParagraphBorders` yet | Toolbar Borders dropdown (Top/Bottom/Left/Right/All/None + style/colour/width) | 🛑 Blocked — render shipped Sprint 5; no edit command on bridge |
| **Paragraph shading / background** | No `Command::SetParagraphShading` yet | Toolbar Paragraph background swatch | 🛑 Blocked — engine has no command |
| **Indentation** (Left / Right / FirstLine / Hanging) | `Command::SetParagraphIndent` (Sprint 11) | Toolbar Indent +/− buttons + ruler drag handles | ✅ Wired (Sprint 11) — `Ruler.tsx` drag handles (§4); toolbar +/− buttons still absent |
| **Line height** (single / 1.5 / double / custom multiple / exact pt) | Engine renders from style cascade (Sprint 5) | Toolbar Line-spacing dropdown | 🛑 Blocked — no command |
| **Space before / after paragraph** | Engine renders from style | Paragraph properties dialog (right rail) | 🛑 Blocked |
| **Keep with next / Keep together / Page break before** | `cantSplit` plumbed Sprint 6; no `keepNext` command yet | Paragraph properties dialog | 🛑 Blocked |
| **Outline level / heading style** | `Command::ApplyStyle` + `DocumentTree.styles` cascade (Sprint 12) | `StylesDropdown.tsx` — Normal / Title / Heading 1-3 | ✅ Wired (Sprint 12) — shadow direct_overrides preserves user edits across style swaps (`#11` closed) |
| **Pending formatting** (sticky style on collapsed caret) | `SpanStyle` cache armed on toolbar click before typing (Sprint 1) | Toolbar buttons visually reflect armed state | ⚠️ Partial — wired but no visible toolbar "armed" feedback |

---

## 3. Lists, Numbering & Outline Cascade

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Bulleted list** | `Command::ToggleList { kind: Bullet }` + idempotent `synth_list_definition` (Sprint 13) | `ListButtons.tsx` Bullet button | ✅ Wired (Sprint 13) — reuses existing matching templates; never inflates `numbering.xml` (`#12` closed) |
| **Numbered list** | `Command::ToggleList { kind: Number }` + idempotent synth (Sprint 13) | `ListButtons.tsx` Number button | ✅ Wired (Sprint 13, `#12` closed) |
| **Multilevel / outline list** | numPr cascade resolves nested levels (Sprint 7) | Toolbar Multilevel dropdown + `Tab`/`Shift+Tab` to demote/promote inside list | 🛑 Blocked |
| **List restart / continue** | No command | Context menu "Restart numbering at 1" / "Continue previous list" | 🛑 Blocked |
| **Custom bullet character / number format** | No command | List properties dialog | 🛑 Blocked |
| **Style-driven numbering** (`<w:style>/<w:numPr>`) | Cascade resolver Sprint 7 + `Command::ApplyStyle` (Sprint 12) + numbering synthesis (Sprint 13) | `StylesDropdown.tsx` + `ListButtons.tsx` | ✅ Wired |

---

## 4. Tabs & Indentation

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Tab character round-trip** | Sprint 1 preserved `\t` in model | None — typing `Tab` inserts via key handler | ✅ Wired — `Tab` inserts `\t` in body text; inside tables it navigates cells (`HiddenInput.tsx:164`) |
| **Geometric tab stops** (Left / Center / Right / Decimal / Clear) | `Command::SetTabStops` + `Event::SelectionChanged.tab_stops` (Sprint 11) | `Ruler.tsx` — L/C/R/D markers, click cycles kind, drag moves, drag-off removes | ✅ Wired (Sprint 11, `#13` closed) — authoring **and** rendering: Center/Right/Decimal render, incl. BiDi interior-anchor mirroring (`#20` closed) |
| **First-line indent ruler handle** | `Command::SetParagraphIndent` + Ruler (Sprint 11) | `Ruler.tsx` first-line ▽ marker at leading edge | ✅ Wired (Sprint 11, `#13` closed) |
| **Hanging indent ruler handle** | `Command::SetParagraphIndent` (negative `first_line_pt`) + Ruler (Sprint 11) | `Ruler.tsx` left-indent △ marker | ✅ Wired (Sprint 11, `#13` closed) |

---

## 5. Pagination, Sections & Page Layout

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Page break (hard)** | `Command::InsertPageBreak` (`command.rs:402`) | Toolbar Insert → Page Break + `Ctrl+Enter` shortcut | ✅ Wired — `LayoutControls.tsx:75` via `cmd.insertPageBreak` |
| **Section break (next page / continuous)** | Continuous `<w:type>` ships Sprint 7; no insertion command | Toolbar Layout → Breaks dropdown | 🛑 Blocked |
| **Multi-column layout (snake flow)** | Per-section geometry resolved (Sprint 2); no `Command::SetColumns` | Toolbar Layout → Columns dropdown (1/2/3/Custom) | 🛑 Blocked — engine renders perfectly; cannot author |
| **Column gutter / equal-width toggle** | Resolved from section properties | Columns dialog (right rail) | 🛑 Blocked |
| **Page margins / orientation / size** | `Command::SetPageMargins` / `Command::SetPageOrientation` + `Event::SelectionChanged.section_geometry` (Sprint 10) | `PageSetupDialog.tsx` (Layout → Page Setup) | ✅ Wired (Sprint 10) — dialog prefills from active section (`#10` closed) |
| **Headers & footers** | No model surface yet | Header/footer edit zones above/below page | 🛑 Blocked |
| **Page number type / format** (Decimal, Roman lower/upper, Letter lower/upper) | Sprint 7 `<w:pgNumType>` + PAGE field evaluator | Header/Footer dialog → "Format Page Numbers…" | 🛑 Blocked — engine renders correctly via field; cannot author |
| **Page number start value** | Sprint 7 start-at honoured | Same dialog as above | 🛑 Blocked |
| **`Expand layout` on scroll / `Ctrl+End`** | `Command::ExpandLayout { target_y }` (Sprint 4) | Scroll handler + `Ctrl+End` keybind | ✅ Wired |
| **Lazy / viewport-culled pagination** | Auto; eager 2-page initial cap (Sprint 4) | Dev HUD: "Pages laid out: X / Y" | ⚠️ Partial — runs invisibly; QA cannot easily verify cull boundary |

---

## 6. Tables

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Insert table (rows × cols picker)** | `Command::InsertTable { rows, cols, at }` | Toolbar Insert → Table grid picker | ✅ Wired (`Toolbar.tsx:215`) |
| **Delete entire table** | `Command::DeleteTable { at }` | Table context menu → Delete Table; `TablePanel` button | ✅ Wired (`TablePanel.tsx:193`) |
| **Insert row above / below** | `Command::InsertRow { table_path, at, side }` | Table context menu; `TablePanel` buttons | ✅ Wired (`TablePanel.tsx:107,113`) |
| **Delete row** | `Command::DeleteRow { table_path, row }` | Same as above | ✅ Wired |
| **Insert column left / right** | `Command::InsertColumn { table_path, at, side }` | Same | ✅ Wired (`TablePanel.tsx:125,131`) |
| **Delete column** | `Command::DeleteColumn { table_path, col }` | Same | ✅ Wired |
| **Merge cells** (rectangle) | `Command::MergeCells { table_path, from, to }` | `TablePanel` Merge button (enabled when ≥2 cells selected) | ✅ Wired (`TablePanel.tsx:144`) |
| **Split cell** | `Command::SplitCell { table_path, at, into_rows, into_cols }` | `TablePanel` Split button → dialog (rows × cols input) | ✅ Wired (`TablePanel.tsx:157`) — dialog has hard-coded inputs; manual split into arbitrary R×C TBD |
| **Cell shading / background colour** | `Command::SetCellShading { table_path, cell, color }` | Table context menu → Shading colour picker | ✅ Wired (`TablePanel.tsx:164`) |
| **Cell borders (per-edge)** | `Command::SetCellBorders { table_path, cell, borders: BridgeCellBorders }` | Cell properties dialog: 4-edge toggle + style/width/colour | ⚠️ Partial — sets all four edges uniformly; no per-edge UI |
| **Border style picker** (`Single / Double / Dotted / Dashed / None`) | `BridgeBorderStyle` enum (`command.rs:405`) | Style dropdown inside Borders dialog | 🛑 Blocked — engine enum unused by UI |
| **Vertical merge (`vMerge`) height accumulation** | Engine renders correctly (Sprint 6) | Toolbar Table → "Merge cells vertically" | 🕳 Latent — works on imported docs; no author UI |
| **Repeating header rows** (`<w:tblHeader>`) | Engine renders across page breaks (Sprint 6) | Row properties dialog → "Repeat as header at top of each page" checkbox | 🛑 Blocked |
| **Cell-cannot-split (`cantSplit`)** | Plumbed Sprint 6; mid-row split deferred | Row properties dialog → "Allow row to break across pages" checkbox | 🛑 Blocked |
| **Auto-fit table layout** (two-pass measure + distribute) | Engine renders (Sprint 6) | Toolbar Table → AutoFit dropdown (Contents / Window / Fixed) | 🛑 Blocked |
| **Cell selection** (rectangular `TableCells` kind) | `Command::SelectCellAt`; `SelectionKind::TableCells` (`common.rs:124`) | Click-and-drag across cells; visible cell-rect overlay | ✅ Wired (pointer + overlay) |
| **Tab / Shift+Tab between cells** | `Command::MoveCaret { dir: NextCell/PrevCell }` | `HiddenInput.tsx:156` | ✅ Wired |

---

## 7. Images & Inline Objects

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Insert image from file** | `Command::InsertImage { blob: ImageBlob, fit }` | Toolbar Insert → Image button → file picker | ✅ Wired (Sprints 1–8 UI Edition) — `InsertImageButton.tsx` via `cmd.insertImageAtCaret` |
| **Image fit modes** (`Original` / `FitWidth` / `FitPage`) | `ImageFit` enum (`command.rs:455`) | Right-click image → Fit submenu | 🛑 Blocked |
| **Image resize / reposition** | No commands yet | Selection handles around image | 🛑 Blocked — model surface missing |
| **Inline-image registration** (`register_image`) | `Engine::register_image(rel_id, ImageBitmap)` | Auto on `LoadDocx` — driven by `media_entries()` enumeration | ✅ Wired (worker bridge) |

---

## 8. Selection, Caret & Navigation (BiDi-aware)

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Pointer click → caret** | `Command::HitTest` → `Command::SetSelection` | `input/pointer.ts:66,69` | ✅ Wired |
| **Pointer drag → range** | `Command::ExtendSelection` | `input/pointer.ts:78` | ✅ Wired |
| **Shift+Click extend** | `Command::ExtendSelection` w/ anchor fixed | `input/pointer.ts:78` | ✅ Wired |
| **Double-click word select** | `Command::SelectWordAt` | `input/pointer.ts:111` | ✅ Wired |
| **Triple-click paragraph select** | `Command::SelectParagraphAt` | `input/pointer.ts:125` | ✅ Wired |
| **Quadruple-click cell / document** | `Command::SelectCellAt` | `input/pointer.ts:128` | ✅ Wired |
| **Select All** | `Command::SelectAll` + `Ctrl/Cmd+A` | `HiddenInput.tsx:169` + toolbar | ✅ Wired |
| **Arrow keys** (logical Left/Right) | `Command::MoveCaret { dir: Left/Right }` | `HiddenInput.tsx:120` | ✅ Wired (BiDi affinity preserved Sprint 8) |
| **Up / Down (ideal-x preservation)** | `Command::MoveCaret { dir: Up/Down }` | `HiddenInput.tsx:120` | ✅ Wired (Sprint 12) |
| **Word-by-word** (`Ctrl/Alt+Arrow`) | `Command::MoveCaret { dir: WordLeft/WordRight }` | `HiddenInput.tsx:120` | ✅ Wired (UAX-#29 Sprint 5/8) |
| **Home / End** (direction-aware) | `Command::MoveCaret { dir: LineHome/LineEnd }` | `HiddenInput.tsx:143` | ✅ Wired (Sprint 3) |
| **Ctrl+Home / Ctrl+End** | `Command::MoveCaret { dir: DocHome/DocEnd }` | `HiddenInput.tsx:143` | ✅ Wired |
| **Shift+Arrow extend** | `Command::MoveCaret { modifier: Shift }` | `HiddenInput.tsx:120` | ✅ Wired (Sprint 12) |
| **`Alt+Arrow` (block move)** | `MoveDirection` enum supports `Alt`/`ShiftAlt` modifiers; no handler | Move paragraph up/down feature | 🛑 Blocked |
| **Page Up / Page Down** | No direct command; needs `ExpandLayout` + `MoveCaret` orchestration | `HiddenInput` key handler | 🛑 Blocked |
| **Discontinuous BiDi selection rects** | Per-run rects emitted (Sprint 2) | `SelectionOverlay.tsx`, `PageSelectionOverlay.tsx` | ✅ Wired |
| **BiDi caret affinity** (LTR/RTL seam) | Engine preserves on movement; resets on non-arrow (Sprint 8) | Visible caret with direction tick (chevron) | ⚠️ Partial — affinity tracked; caret glyph doesn't visually indicate direction |
| **Multi-page hit-test** | `Command::HitTestInPage { page_idx }` | `input/pointer.ts` walks per-page canvases | ✅ Wired |

---

## 9. Clipboard & Drag-and-Drop

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Copy → HTML + `.docx` fragment** | `Command::GetSelectionAsClipboard` → `Event::ClipboardPayload` | `copy` event handler (`input/clipboard.ts:33`) | ✅ Wired (Sprint 7) |
| **Cut** | `GetSelectionAsClipboard` + `DeleteAtCaret` | `cut` event handler (`input/clipboard.ts:40,43`) | ✅ Wired |
| **Paste plain text** | `Command::PastePlain { text }` | `paste` event handler; `Ctrl/Cmd+Shift+V` shortcut | ✅ Wired (`HiddenInput.tsx:183`, `clipboard.ts:70`) |
| **Paste rich HTML** | `Command::PasteHtml { html }` | `paste` event handler when clipboard has `text/html` | ✅ Wired (`clipboard.ts:61`) |
| **Paste `.docx` fragment** | Round-trip via HTML path | Same | ⚠️ Partial — `.docx` fragments are emitted on copy but paste path uses HTML route |
| **Drag-and-drop `.docx` file** | `Command::LoadDocx` w/ Transferable bytes | `input/dnd.ts:10` | ✅ Wired |
| **Drag-and-drop image** | `Command::InsertImage` | `input/dnd.ts` | 🛑 Blocked — dnd path only handles `.docx` |
| **Drag-text within document** | No engine command | Pointer drag of selection | 🛑 Blocked |

---

## 10. Undo / Redo

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Undo (depth 100, bounded)** | `Command::Undo` | Toolbar Undo button + `Ctrl/Cmd+Z` | ✅ Wired (`Toolbar.tsx:253`, `HiddenInput.tsx:172`) |
| **Redo** | `Command::Redo` | Toolbar Redo button + `Ctrl/Cmd+Y` / `Ctrl+Shift+Z` | ✅ Wired |
| **Undo / Redo availability state** | `Event::UndoStateChanged { can_undo, can_redo }` | Toolbar button enable/disable | ✅ Wired |
| **Undo history panel** | No engine API for replayable history list | Side panel listing recent operations | 🛑 Blocked — out of scope for beta |

---

## 11. IME / Composition

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Begin composition** | `Command::BeginComposition` | `compositionstart` on `<textarea>` | ✅ Wired (`HiddenInput.tsx:68`) |
| **Update composition (preview + caret-relative range)** | `Command::UpdateComposition { text, caret_offset }` | `compositionupdate` | ✅ Wired |
| **End composition (commit / cancel)** | `Command::EndComposition { commit }` | `compositionend` | ✅ Wired |
| **Inline underlined preview on canvas** | Render path splices composition into paragraph (Sprint 11) | Auto on `UpdateComposition` | ✅ Wired |
| **IME `target_range` sub-segment styling** | Engine supports uniform underline only | Target-clause emphasis (darker underline) | 🕳 Deferred — BACKLOG.md #8 |
| **IME popup anchoring** | UI tracks caret rect for anchor | `HiddenInput` repositions textarea each `SelectionChanged` | ✅ Wired |

---

## 12. Track Changes & Revisions

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Revisions snapshot** (read-only) | `Engine::revisions_snapshot()` → `Vec<Revision>` (`lib.rs:450`) | "Review" sidebar listing each `<w:ins>` / `<w:del>` / `<w:rPrChange>` with author + timestamp | ✅ Wired (Sprints 1–8 UI Edition) — `TrackChangesSidebar.tsx` |
| **Show/hide markup** | No engine command | Toolbar Review → "Display for Review" dropdown (Final / All Markup / No Markup / Original) | 🛑 Blocked |
| **Accept / Reject revision** | `Command::AcceptRevision` / `Command::RejectRevision` (`command.rs:480,487`) | Per-revision Accept ✓ / Reject ✗ buttons in sidebar | ✅ Wired — `TrackChangesSidebar.tsx:53,57` via `cmd.acceptRevision` / `cmd.rejectRevision` |
| **Accept All / Reject All** | Same commands, iterated over the snapshot | Toolbar Review buttons | ✅ Wired — `ReviewControls.tsx:87,98` |
| **Track-changes toggle** | `Command::ToggleTrackChanges` + `Event::SelectionChanged.is_tracking_changes` (Sprint 14) | `ReviewControls.tsx` Track toggle (binds to engine state, no local UI signal) | ✅ Wired (Sprint 14) — every InsertText / DeleteRange / DeleteAtCaret / ApplyFormatting gates into `<w:ins>` / `<w:del>` / `<w:rPrChange>` revisions; boundary math preserves invariants (`#14` closed) |
| **Author / colour assignment** | Engine preserves on round-trip | Settings → user identity (already in `.docx` model) | 🕳 Latent |

---

## 13. Comments & Annotations

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Comments snapshot** (read-only) | `Engine::comments_snapshot()` → `Vec<Comment>` (`lib.rs:478`) | Comments rail on right with anchored callouts | ✅ Wired (Sprints 1–8 UI Edition) — `CommentsRail.tsx` via `engine.commentsSnapshot()` |
| **Insert comment** | No engine command yet | Right-click selection → "New comment" + toolbar Review → Comment | 🛑 Blocked — needs bridge addition |
| **Reply to comment** | `Command::ReplyToComment` (`command.rs`, after `ResolveComment`) | Reply field on each comment thread | ✅ Wired (2026-07-02, issue #27) — `CommentsRail.tsx` groups the snapshot into threads on `parent_id` and posts replies via `cmd.replyToComment`; the engine anchors the reply on the parent's range, delete cascades over the thread, and threading round-trips through `word/commentsExtended.xml` `<w15:commentEx w15:paraIdParent>` |
| **Resolve / delete comment** | `Command::ResolveComment` / `Command::DeleteComment` | `CommentsRail.tsx` per-comment row | ✅ Wired (Sprint 9) — `resolved` round-trips through `word/commentsExtended.xml` (`#15` closed) |
| **Comment-anchor highlight in body** | Engine has range info | Coloured underline at comment range | 🛑 Blocked |

---

## 14. Fields & Dynamic Content

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **PAGE field (current page #)** | Evaluated during render w/ section pgNumType (Sprint 7) | Toolbar Insert → Field → PAGE | 🛑 Blocked — engine renders correctly; cannot author |
| **NUMPAGES field (total pages)** | If implemented, same path | Insert → Field → NUMPAGES | 🛑 Blocked |
| **DATE / TIME / FILENAME / AUTHOR fields** | Round-trip preserved (parser path) | Insert → Field menu | 🕳 Latent — preserved, not evaluated; needs render path + UI |
| **Field code toggle** (`Alt+F9`) | No command | Toolbar Review → Show field codes | 🛑 Blocked |
| **Update fields** (`F9`) | No command | Toolbar Review → Update fields | 🛑 Blocked |

---

## 15. Document I/O — Open / Save / Export

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Open `.docx` (file picker)** | `Command::OpenDocument { bytes, format: Docx }` | Toolbar File → Open button | ✅ Wired (Sprints 1–8 UI Edition) — `FileMenu.tsx` Open → file picker |
| **Open `.docx` (drag-and-drop)** | Same | `input/dnd.ts:10` | ✅ Wired |
| **Save `.docx`** | `Command::SaveDocument { format: Docx }` / legacy `Command::SaveDocx` | Toolbar File → Save + `Ctrl/Cmd+S` shortcut | ✅ Wired — `FileMenu.tsx` Save entry + `Ctrl/Cmd+S` binding |
| **Export plain text** | `DocFormat::PlainText` (`common.rs:156`) | Toolbar File → Export → Plain Text | ✅ Wired (Sprint 9) — `engine::DocumentTree::to_plain_text` (`#9` closed) |
| **Export HTML** | `DocFormat::Html` | Toolbar File → Export → HTML | ✅ Wired (Sprint 9) — `crates/format-html::to_html` (`#9` closed) |
| **Export PDF (`PdfProfile::A1b`)** | `Command::ExportPdf { conformance }` | Toolbar Export PDF button | ✅ Wired — `FileMenu.tsx` conformance picker (A1b engine-real) |
| **Export PDF (`A2u`, `X3`)** | `PdfConformance` enum supports both (`command.rs:425`) | Export dialog with conformance dropdown | 🛑 Blocked (engine) — `FileMenu.tsx` picker lists both, gated "Engine pending"; `do_export_pdf` falls back to `PdfProfile::Plain` |
| **Close document** | `Command::CloseDocument` | Toolbar File → Close + `Ctrl/Cmd+W` | 🛑 Blocked — no UI; one-doc-per-tab assumed |
| **Recent files** | None | Toolbar File → Recent list (IndexedDB-backed) | 🛑 Blocked |
| **New empty document** | Auto on first mount (`App.tsx:55`) | Toolbar File → New + `Ctrl/Cmd+N` | 🛑 Blocked — no explicit affordance |
| **Load font** | `Command::LoadFont { font_id, bytes }` | Settings → "Add Font…" file picker (TTF/OTF) | ⚠️ Partial — wired internally (`engine-client.ts:135`); no end-user UI to inspect or add fonts |
| **Unload font** | `Command::UnloadFont` | Font manager → Remove button | 🛑 Blocked |
| **`Event::FontMissing` recovery prompt** | Worker emits when font requested but absent | Toast: "Font 'X' not loaded — install or substitute?" | 🛑 Blocked — event currently silently dropped |

---

## 16. Viewport, Zoom & Performance Controls

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Set zoom** | `Command::SetZoom { scale }` | Status-bar Zoom dropdown (50/75/100/125/150/200/Fit) + `Ctrl+/-/0` shortcuts + pinch gesture | ✅ Wired (Sprints 1–8 UI Edition) — `ZoomControls.tsx:45` via `cmd.setZoom` |
| **Set viewport** (record visible band) | `Command::SetViewport { rect }` | Scroll handler emits on scroll/resize | ✅ Wired — `EditorCanvas.tsx:80` dispatches rAF-coalesced `SET_VIEWPORT` on scroll/resize (drives lazy pagination) |
| **Expand layout (lazy)** | `Command::ExpandLayout { target_y }` | Scroll handler + `Ctrl+End` | ✅ Wired |
| **Request paint (clipped)** | `Command::RequestPaint { rect }` | Auto from `DirtyTracker`; manual debug button useful | ✅ Wired (auto-path) |
| **Request stats** | `Command::RequestStats` → `Event::Stats { EngineStats }` | Dev HUD overlay (WASM heap, undo depth, glyph cache, paint ms) | ✅ Wired (Sprints 1–8 UI Edition) — `DevHud.tsx` polls while visible + renders the payload |
| **Animation tick** | `Command::Tick` | rAF loop for caret blink / overlay animation | ⚠️ Partial — wired internally; no user-tunable cadence |

---

## 17. Accessibility Tree

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Request accessibility tree (full)** | `Command::RequestAccessibilityDelta` (first call → full tree) | Auto on mount | ✅ Wired (`engine.worker.ts:526`) |
| **A11y deltas** (Replace / Update / Insert / Remove patches) | `Event::AccessibilityTreeDelta` (Sprint 9) | `AccessibilityTree.tsx` reconciler | ✅ Wired |
| **Screen-reader DOM mirror** | One `<p dir>` per paragraph, `<span>` per run | `AccessibilityTree.tsx` (visually hidden) | ✅ Wired |
| **Table a11y** (`role="table"` + cells) | `A11yTable` / `A11yRow` / `A11yCell` shipped | Reconciler emits matching DOM | ✅ Wired |
| **Stable a11y IDs across edits** | Engine diffs by content (Sprint 9); position-identity deferred | None (assistive tech tracks by structural path) | 🕳 Deferred — BACKLOG #10 |
| **ARIA live announcements** | `Event::Announcement { priority, message }` (Sprint 10) | `Announcements.tsx` — polite + assertive `aria-live` regions | ✅ Wired (Sprint 10) — engine emits from every user-visible mutation handler (`#16` closed) |

---

## 18. Engine Lifecycle, Telemetry & Diagnostics

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Init** | `Command::Init` | Auto in worker boot | ✅ Wired |
| **Dispose** | `Command::Dispose` | Auto on page unload | ✅ Wired |
| **Recover (after trap)** | `Command::Recover` | Crash overlay → "Reload" button | ⚠️ Partial — worker traps reconnect, but recovery is **stub** (event-log snapshots empty placeholders) |
| **Ping / Pong (liveness)** | `Command::Ping` / `Event::Pong` | Test harness only | ✅ Wired (harness path) |
| **Backend detect** (`vello` vs `canvas2d`) | `detect_backend()` (`lib.rs:573`) | Settings → "Renderer: Canvas2D \| Vello (experimental)" toggle | ✅ Wired — `SettingsMenu.tsx` renderer switch (reloads with `?renderer=`; choice is baked at INIT) |
| **Engine capabilities** | `EngineCapabilities` struct (`event.rs:254`) — `simd`, `shared_array_buffer`, formats list | About dialog | 🛑 Blocked |
| **Engine stats** | `EngineStats` (`event.rs:263`) — heap, undo depth, glyph cache, paint ms | Dev HUD overlay | ✅ Wired (Sprints 1–8 UI Edition) — `DevHud.tsx` |
| **Telemetry batch** | `TelemetryEvent` / `TelemetryBatch` (`telemetry.rs`) — D5.7 mock transport | None (mock `console.log` transport for MVP) | 🕳 Mock — collector exists, no live UI |
| **Error toast** | `Event::Error { message }` | Toast component on error events | 🛑 Blocked — events fire, no visible toast surface |
| **Trap overlay** | `Event::Trap` | Full-screen modal with stack + Reload | ✅ Wired (Sprints 1–8 UI Edition) — `TrapOverlay.tsx` (Portal modal) |

---

# Sprint 1 (UI Edition) — Low-Hanging Fruit

> **Goal:** Unblock manual QA for the largest swath of engine surface area in **one short sprint** by wiring components whose engine half is already shipped and whose UI cost is purely "drop a button on the toolbar + handle one event".
>
> **Selection criteria:** (a) engine command exists and is tested, (b) UI work is < 100 LOC of TS, (c) unblocks an entire feature area for manual exploration.

| # | UI Component | Engine Command(s) | Why It's Cheap | What It Unblocks |
|---|---|---|---|---|
| **1** | **Zoom controls** (status-bar dropdown + `Ctrl+/-/0`) | `Command::SetZoom { scale }` | Engine command exists, untouched by UI. ~30 LOC: a `<select>` + 3 keyboard branches. | Manual verification of glyph hinting, hit-test scaling, overlay alignment under DPR change — touches **every** rendered feature. |
| **2** | **Underline style dropdown** (chevron beside U) | `Command::ApplyFormatting` w/ `UnderlineStyle::{Double,Dotted,Dashed,Wavy}` | Toolbar button already exists; need a 5-item popover. ~50 LOC. | Validates the four non-`Single` underline render paths, BiDi underline continuity, decoration baseline (Sprint 6). |
| **3** | **Super / Subscript buttons** (`X²` / `X₂`) | `Command::ApplyFormatting` w/ `VerticalScript` field added to `TextAttrsPatch` | Engine renders (Sprint 1/5); patch field add is a 1-line bridge change + 2 buttons. ~40 LOC. | Validates super/sub pen-shift, size scaling, line-height interaction. Whole `VerticalScript` enum becomes testable. |
| **4** | **Insert image button** (file picker) | `Command::InsertImage { blob: ImageBlob, fit }` | Reuses existing `dnd.ts` decoder path; just `<input type="file" accept="image/*">` and the same `register_image` flow. ~60 LOC. | Validates inline image insertion, three `ImageFit` modes, image round-trip on `.docx` save, image hit-test. |
| **5** | **Track Changes review sidebar** (read-only list view) | `Engine::revisions_snapshot()` (already on engine) | Engine returns a `Vec<Revision>`; one `<aside>` mapping each entry to a card. ~80 LOC + minimal styling. | Validates entire ins/del/rPrChange parser path — currently zero coverage outside of `.docx` round-trip byte diff. |
| **6** | **Comments rail** (read-only callouts) | `Engine::comments_snapshot()` (already on engine) | Same shape as #5. ~80 LOC. | Validates comments parser path and per-anchor range resolution — currently zero coverage. |
| **7** | **Dev HUD overlay** (toggle with `Ctrl+Shift+D`) | `Command::RequestStats` → `Event::Stats { EngineStats }` (already polled at `App.tsx:98`) | Already polled; just render four `<div>`s in a corner. ~40 LOC. | Surfaces `wasm_heap_bytes`, `undo_stack_depth`, `glyph_cache_entries`, `last_paint_ms` — turns every other QA session into a perf-regression sniff test. |

**Total estimated effort:** ~1 engineer-week. **Unlocks:** zoom × DPR matrix, full underline style coverage, super/sub matrix, image lifecycle, **entire Track Changes data path**, **entire Comments data path**, and continuous perf observability.

**Recommended order:**
1. Dev HUD (#7) — instrument **first** so the rest of the sprint is observable.
2. Zoom (#1) — broadest blast radius for the buck.
3. Track Changes sidebar (#5) + Comments rail (#6) — pure read paths, parallelizable.
4. Underline dropdown (#2) + Super/Sub (#3) — tiny formatting wins.
5. Insert image (#4) — closes the image manual-QA gap.

---

# Cross-Reference: Commands Without UI Consumers

Bridge commands whose UI dispatch count is **zero** (from grep of
`packages/ui/src/`, `packages/core/src/` and `ts/src/`; updated 2026-07-02):

| Command | Status |
|---|---|
| `Command::SetZoom` | ✅ Wired — `ZoomControls.tsx:45` |
| `Command::SetViewport` | ✅ Wired — `EditorCanvas.tsx:80` scroll/resize handler |
| `Command::Tick` | ⚠️ Internal only |
| `Command::UnloadFont` | 🛑 No UI |
| `Command::CloseDocument` | 🛑 No UI |

Commands with **partial** UI (engine surface broader than UI exposes):

| Command | Gap |
|---|---|
| `Command::ApplyFormatting` | ✅ Closed — underline styles, `vertical_script`, `caps`/`small_caps` all reachable (`UnderlineStyleDropdown`, `SuperSubButtons`, `CapsButtons`); a `clear`-formatting affordance is still absent |
| `Command::ExportPdf` | `FileMenu.tsx` conformance picker ships; `A2u`/`X3` gated "Engine pending" (`do_export_pdf` falls back to Plain) |
| `Command::OpenDocument` | ✅ Closed — `FileMenu.tsx` file picker + drag-drop |
| `Command::InsertImage` | ✅ Closed — `InsertImageButton.tsx` |
| `Command::SaveDocument` / `Command::SaveDocx` | ✅ Closed — `FileMenu.tsx` + `Ctrl/Cmd+S` |
| `Command::SetCellBorders` | Sets all 4 edges identically; no per-edge picker |
| `Command::PasteHtml` / `Command::PastePlain` | Wired; rich-`.docx` fragment paste path not yet exercised in `.docx → .docx` clipboard tests |

Engine APIs without **any** UI consumer:

| API (lib.rs) | Status |
|---|---|
| `Engine::revisions_snapshot()` | ✅ Wired — `TrackChangesSidebar.tsx` / `ReviewControls.tsx` |
| `Engine::comments_snapshot()` | ✅ Wired — `CommentsRail.tsx` |
| `Engine::media_entries()` | ⚠️ Internal only (used during `LoadDocx`) |
| `Engine::with_vello()` / `detect_backend()` | ✅ Wired — `SettingsMenu.tsx` renderer switch |

Bridge **events** without UI handlers:

| Event | Gap |
|---|---|
| `Event::FontMissing` | 🛑 No toast or substitution prompt |
| `Event::Error` | ⚠️ Logged only; no toast surface |
| `Event::Stats` | ⚠️ Polled, not displayed |
| `Event::Painted { paint_ms }` | 🕳 Always `0.0` (D5.7 telemetry pipeline pending real numbers) |

---

## Appendix: Authoritative Bridge Surface

- **Commands:** 57 variants in `crates/bridge/src/command.rs` (lines 19–365).
- **Events:** 25 variants in `crates/bridge/src/event.rs` (lines 16–215).
- **Shared types:** `LogicalPos`, `BlockPath`, `LogicalRange`, `SelectionKind`, `Rect`, `Point`, `DocFormat`, `Color`, `UnderlineStyle`, `VerticalScript`, `Direction`, `Alignment`, `Script`, `TextAttrs` (`common.rs:17–248`).
- **Telemetry:** `TelemetryEvent`, `TelemetryKind`, `ErrorCode`, `TelemetryBatch` (`telemetry.rs:18–62`).
- **Engine surface (`#[wasm_bindgen]`):** `new`, `with_vello`, `dispatch`, `media_entries`, `register_image`, `set_page_canvas`, `paint_dims`, `revisions_snapshot`, `comments_snapshot`, plus free `detect_backend` (`engine-wasm/src/lib.rs:235–573`).
- **UI dispatchers (current):** `Toolbar.tsx`, `HiddenInput.tsx`, `TablePanel.tsx`, `input/{pointer,clipboard,dnd}.ts`, `engine/{engine-client,engine.worker}.ts`, `App.tsx`.

**Coverage summary (original audit — superseded, see the 2026-07-02 note):** the **48 / 57** command coverage and the "zero UI rendering" verdict on `revisions_snapshot` / `comments_snapshot` described the pre-SDK-split shell. After the Sprints 1–8 (UI Edition) wave and its follow-ups, both snapshots render (`TrackChangesSidebar.tsx`, `CommentsRail.tsx`) and the remaining 🛑 rows are those whose **engine** half is genuinely missing (markup display modes, headers / footers, field authoring, multi-column authoring, list outline tooling) — systematic manual QA is no longer UI-blocked.

**Sprint 9 (closed `#9`, `#15`).** Three rows flipped 🛑 Blocked → ✅ Wired:
section 13 *Resolve / delete comment* (`Command::ResolveComment` /
`Command::DeleteComment` now round-trip `resolved` through
`word/commentsExtended.xml`), section 15 *Export plain text*
(`engine::DocumentTree::to_plain_text`), and section 15 *Export HTML*
(new `crates/format-html`). Engine-minted comment paraId synthesis +
OPC plumbing for fresh-document commentsExtended.xml remain open as
`#18` (tech-debt).

**Sprint 10 (closed `#10`, `#16`).** Two rows flipped to ✅ Wired:
section 5 *Page margins / orientation / size* (`PageSetupDialog`
prefills from `Event::SelectionChanged.section_geometry`) and section
17 *ARIA live announcements* (`Event::Announcement { priority,
message }` from engine mutation handlers + polite + assertive
`aria-live` regions in `Announcements.tsx`). `CellPropertiesDialog`
also prefills from `Event::SelectionChanged.cell_properties` (per-
edge edit remains a future enhancement — section 7 *Cell borders*
stays ⚠️ Partial).

**Sprint 11 (closed `#13`, `#17`).** Three rows flipped to ✅ Wired:
section 4 *Geometric tab stops*, *First-line indent ruler handle*,
*Hanging indent ruler handle* — all served by the new `Ruler.tsx` +
`Command::SetTabStops` / `Command::SetParagraphIndent`. `Ruler`
dispatches once on `pointerup`, never on `pointermove`, so one drag
gesture is one undo entry. Word-count now uses
`icu_segmenter::WordSegmenter::new_auto` (UAX-#29) so CJK reports
> 1; wasm 4.18 → 5.93 MiB (well within the 15 MiB budget). Center /
Right / Decimal tab-stop **rendering** still degrades to Left
(out-of-scope per plan).

**Sprint 12 (closed `#11`, HIGH risk).** One row flipped to ✅ Wired:
section 2 *Outline level / heading style* — `StylesDropdown` now
dispatches `Command::ApplyStyle` against the real engine cascade
(`DocumentTree.styles` + shadow `direct_overrides`). Style-driven
numbering (§4) flipped from 🕳 Latent to ⚠️ Partial — the cascade
drives, but numbering synthesis remains a Sprint 13 deliverable.
Custom-style creation UI + character styles (`<w:rStyle>`) stay
out of scope per plan. WASM unchanged at 5.94 MiB (Sprint 12 added
no new deps).

**Sprint 13 (closed `#12`, HIGH risk).** Two rows flipped to ✅
Wired in §4: *Bulleted list* and *Numbered list* — both routed
through `Command::ToggleList` + engine `synth_list_definition`
(idempotent: repeated toggles reuse existing matching templates,
never inflate `numbering.xml`). §4 *Style-driven numbering*
upgraded ⚠️ Partial → ✅ Wired now that the synthesis half ships.
WASM 5.94 → 5.96 MiB (+20 KiB; no new deps, only added engine
mirror types). Tab/Shift-Tab demote/promote, restart-numbering UI,
custom bullet pickers stay out of scope per plan.

**Sprint 14 (closed `#14`, HIGHEST risk — FINAL Core sprint).** One
row flipped to ✅ Wired in §12: *Track-changes toggle* — every
text mutation (InsertText / DeleteRange / DeleteAtCaret /
ApplyFormatting) gates through three new engine helpers
(`tracked_insert_text`, `tracked_delete_range`,
`tracked_format_change`). Boundary math:
typing-in-same-author-Insert grows it (no fragmentation);
typing-in-Delete splits the Delete and stamps a new Insert in the
gap (no nested `<w:ins>` in `<w:del>`); adjacent same-author
revisions merge. Undo restores prior tree snapshots — no counter-
revisions. `ReviewControls` Track toggle binds to
`state.isTrackingChanges()` (broadcast on every SelectionChanged)
instead of carrying local Solid state. WASM 5.96 → 5.97 MiB (+12
KiB). Cross-paragraph tracked-delete, FormatChange-reject restoring
prev_attrs, and mixed-overlap deletes stay v1 limitations.

**Sprints 1–8 (UI Edition) + follow-ups — retroactive doc sync
(2026-07-02).** The shelf wave (commit `747796b`, cut
`v0.6.0-beta.2`) shipped §19's entire low-hanging-fruit list and
more, but the rows above were never flipped; this note records the
sync. Rows flipped 🛑/⚠️ → ✅ Wired: §1 *Underline dropdown*
(`UnderlineStyleDropdown.tsx`), *Super/Subscript*
(`SuperSubButtons.tsx`), *All-/Small-Caps* (`CapsButtons.tsx` —
`TextAttrsPatch` grew `caps` + `small_caps`); §2 *Indentation*
(resolving the self-contradiction with §4 —
`Command::SetParagraphIndent` shipped in Sprint 11); §4 *Tab
character* (`Tab` inserts `\t` in body text, `HiddenInput.tsx:164`);
§5 *Page break* (`LayoutControls.tsx:75`); §7 *Insert image*
(`InsertImageButton.tsx`); §12 *Revisions snapshot* +
*Accept / Reject* + *Accept All / Reject All*
(`TrackChangesSidebar.tsx`, `ReviewControls.tsx` —
`Command::AcceptRevision` / `RejectRevision` landed on the bridge);
§13 *Comments snapshot* (`CommentsRail.tsx`); §15 *Open (file
picker)*, *Save + `Ctrl/Cmd+S`*, *PDF conformance picker*
(`FileMenu.tsx` — `A2u`/`X3` stay engine-gated behind the "Engine
pending" badge); §16 *Set zoom* (`ZoomControls.tsx`), *Set viewport*
(`EditorCanvas.tsx:80`), *Request stats* (`DevHud.tsx`); §18
*Backend detect* (`SettingsMenu.tsx`), *Engine stats* (`DevHud.tsx`),
*Trap overlay* (`TrapOverlay.tsx`). The Sprint 11 caveat is also
resolved: Center / Right / Decimal tab stops now **render**,
including BiDi interior-anchor mirroring (closes `#20`). The
cross-reference tables and coverage summary above were updated in
place; the per-sprint notes above are left as historical record.
