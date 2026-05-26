# UI_SURFACE_MAPPING.md — Engine ↔ UI Coverage Audit

> **Status:** Engine is feature-complete through Consolidated Sprints 1–8 and Phase-5 Backlog Sprints 1–12 (`v0.5.0-beta.3`). The TypeScript shell exposes **~48 of 57** bridge commands; many high-value engine capabilities have **zero** discoverable UI affordance, blocking manual QA. This document inventories every engine feature, the bridge surface that drives it, the UI component required to exercise it manually, and the current QA status.
>
> **Authoritative sources:** `crates/bridge/src/{command,event,common,telemetry}.rs`, `crates/engine-wasm/src/lib.rs`, `ts/src/components/`, `ts/src/input/`, `BACKLOG.md`, and the git history for Sprints 1–8.
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
| **Underline (Double / Dotted / Dashed / Wavy)** | `Command::ApplyFormatting` w/ `UnderlineStyle::{Double,Dotted,Dashed,Wavy}` | Toolbar Underline **dropdown** (chevron beside U) → style picker | 🛑 Blocked — only `Single` exposed; engine enum supports five styles (`common.rs:174`) |
| **Strikethrough** | `Command::ApplyFormatting { patch.strike }` | Toolbar Strikethrough button + `Alt+Shift+5` | ✅ Wired |
| **Superscript / Subscript** | `Command::ApplyFormatting` w/ `VerticalScript::{Superscript,Subscript}` | Toolbar `X²` / `X₂` buttons + `Ctrl+Shift+=` / `Ctrl+=` shortcuts | 🛑 Blocked — engine ships render (Sprint 1/5), no UI; `TextAttrsPatch` needs `vertical_script` field added or routed via existing patch shape |
| **All-Caps / Small-Caps transform** | Engine renders caps from `SpanStyle` flag (Sprint 1) | Toolbar `Aa` toggle + dropdown (All / Small) | 🛑 Blocked — render path lives; no patch field on `TextAttrsPatch` (`command.rs:434`) — needs bridge extension |
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
| **Indentation** (Left / Right / FirstLine / Hanging) | No `Command::SetParagraphIndent` yet | Toolbar Indent +/− buttons + ruler drag handles | 🛑 Blocked — model preserves on round-trip; no command |
| **Line height** (single / 1.5 / double / custom multiple / exact pt) | Engine renders from style cascade (Sprint 5) | Toolbar Line-spacing dropdown | 🛑 Blocked — no command |
| **Space before / after paragraph** | Engine renders from style | Paragraph properties dialog (right rail) | 🛑 Blocked |
| **Keep with next / Keep together / Page break before** | `cantSplit` plumbed Sprint 6; no `keepNext` command yet | Paragraph properties dialog | 🛑 Blocked |
| **Outline level / heading style** | Style cascade applies; no `Command::ApplyStyle` | Toolbar Style dropdown (Heading 1–6, Normal, …) | 🛑 Blocked — high-value gap |
| **Pending formatting** (sticky style on collapsed caret) | `SpanStyle` cache armed on toolbar click before typing (Sprint 1) | Toolbar buttons visually reflect armed state | ⚠️ Partial — wired but no visible toolbar "armed" feedback |

---

## 3. Lists, Numbering & Outline Cascade

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Bulleted list** | No `Command::ToggleList { kind: Bullet }` yet | Toolbar Bullet List button + `Ctrl+Shift+8` | 🛑 Blocked — `numPr` cascade resolves (Sprint 7); no edit command |
| **Numbered list** | No `Command::ToggleList { kind: Number }` | Toolbar Numbered List button + `Ctrl+Shift+7` | 🛑 Blocked |
| **Multilevel / outline list** | numPr cascade resolves nested levels (Sprint 7) | Toolbar Multilevel dropdown + `Tab`/`Shift+Tab` to demote/promote inside list | 🛑 Blocked |
| **List restart / continue** | No command | Context menu "Restart numbering at 1" / "Continue previous list" | 🛑 Blocked |
| **Custom bullet character / number format** | No command | List properties dialog | 🛑 Blocked |
| **Style-driven numbering** (`<w:style>/<w:numPr>`) | Cascade resolver Sprint 7 | None — drives automatically when style applied | 🕳 Latent — needs Heading/Style UI (see §2) to test |

---

## 4. Tabs & Indentation

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Tab character round-trip** | Sprint 1 preserved `\t` in model | None — typing `Tab` inserts via `beforeinput` | ⚠️ Partial — `Tab` key currently navigates table cells only; outside tables it should insert `\t` |
| **Geometric tab stops** (Left / Center / Right / Decimal / Clear) | Engine model carries kinds (Sprint 5); render only `Left` ships | Ruler with draggable tab markers + tab-kind selector chip; `Tabs…` dialog | 🛑 Blocked — render of Center/Right/Decimal **deferred**; rendering UI also missing |
| **First-line indent ruler handle** | No command | Ruler with first-line ▽ marker | 🛑 Blocked |
| **Hanging indent ruler handle** | No command | Ruler with hanging △ marker | 🛑 Blocked |

---

## 5. Pagination, Sections & Page Layout

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Page break (hard)** | No `Command::InsertPageBreak` yet | Toolbar Insert → Page Break + `Ctrl+Enter` shortcut | 🛑 Blocked — high-value gap; preserved on `.docx` round-trip but cannot author |
| **Section break (next page / continuous)** | Continuous `<w:type>` ships Sprint 7; no insertion command | Toolbar Layout → Breaks dropdown | 🛑 Blocked |
| **Multi-column layout (snake flow)** | Per-section geometry resolved (Sprint 2); no `Command::SetColumns` | Toolbar Layout → Columns dropdown (1/2/3/Custom) | 🛑 Blocked — engine renders perfectly; cannot author |
| **Column gutter / equal-width toggle** | Resolved from section properties | Columns dialog (right rail) | 🛑 Blocked |
| **Page margins / orientation / size** | Renders from section model | Toolbar Layout → Margins / Orientation / Size dropdowns | 🛑 Blocked |
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
| **Insert image from file** | `Command::InsertImage { blob: ImageBlob, fit }` | Toolbar Insert → Image button → file picker | 🛑 Blocked — engine command lives; no UI button (drag-drop loads whole DOCX, not images) |
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
| **Revisions snapshot** (read-only) | `Engine::revisions_snapshot()` → `Vec<Revision>` (`lib.rs:450`) | "Review" sidebar listing each `<w:ins>` / `<w:del>` / `<w:rPrChange>` with author + timestamp | 🛑 Blocked — engine exposes full list; **zero UI** — high-value gap for QA |
| **Show/hide markup** | No engine command | Toolbar Review → "Display for Review" dropdown (Final / All Markup / No Markup / Original) | 🛑 Blocked |
| **Accept / Reject revision** | No engine command yet | Per-revision Accept ✓ / Reject ✗ buttons in sidebar | 🛑 Blocked — needs bridge addition |
| **Accept All / Reject All** | No engine command | Toolbar Review buttons | 🛑 Blocked |
| **Track-changes toggle** | No engine command | Toolbar Review → Track Changes toggle (records new edits as `<w:ins>`) | 🛑 Blocked |
| **Author / colour assignment** | Engine preserves on round-trip | Settings → user identity (already in `.docx` model) | 🕳 Latent |

---

## 13. Comments & Annotations

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Comments snapshot** (read-only) | `Engine::comments_snapshot()` → `Vec<Comment>` (`lib.rs:478`) | Comments rail on right with anchored callouts | 🛑 Blocked — engine exposes full list; **zero UI** — high-value gap |
| **Insert comment** | No engine command yet | Right-click selection → "New comment" + toolbar Review → Comment | 🛑 Blocked — needs bridge addition |
| **Reply to comment** | No command | Reply field on each comment thread | 🛑 Blocked |
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
| **Open `.docx` (file picker)** | `Command::OpenDocument { bytes, format: Docx }` | Toolbar File → Open button | 🛑 Blocked — only drag-and-drop entrypoint exists (`input/dnd.ts:10`) |
| **Open `.docx` (drag-and-drop)** | Same | `input/dnd.ts:10` | ✅ Wired |
| **Save `.docx`** | `Command::SaveDocument { format: Docx }` / legacy `Command::SaveDocx` | Toolbar File → Save + `Ctrl/Cmd+S` shortcut | ⚠️ Partial — toolbar wired (`Toolbar.tsx:90`); **no keyboard shortcut** |
| **Export plain text** | `DocFormat::PlainText` (`common.rs:156`) | Toolbar File → Export → Plain Text | ✅ Wired (Sprint 9) — `engine::DocumentTree::to_plain_text` (`#9` closed) |
| **Export HTML** | `DocFormat::Html` | Toolbar File → Export → HTML | ✅ Wired (Sprint 9) — `crates/format-html::to_html` (`#9` closed) |
| **Export PDF (`PdfProfile::A1b`)** | `Command::ExportPdf { conformance }` | Toolbar Export PDF button | ✅ Wired (`Toolbar.tsx:70`) — only `A1b` selected; user can't choose conformance |
| **Export PDF (`A2u`, `X3`)** | `PdfConformance` enum supports both (`command.rs:425`) | Export dialog with conformance dropdown | 🛑 Blocked — enum values unused |
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
| **Set zoom** | `Command::SetZoom { scale }` | Status-bar Zoom dropdown (50/75/100/125/150/200/Fit) + `Ctrl+/-/0` shortcuts + pinch gesture | 🛑 Blocked — engine command exists; **no UI** — high-value low-hanging fruit |
| **Set viewport** (record visible band) | `Command::SetViewport { rect }` | Scroll handler emits on scroll/resize | 🛑 Blocked — comment `TODO` at `EditorCanvas.tsx:61`; needed for culling |
| **Expand layout (lazy)** | `Command::ExpandLayout { target_y }` | Scroll handler + `Ctrl+End` | ✅ Wired |
| **Request paint (clipped)** | `Command::RequestPaint { rect }` | Auto from `DirtyTracker`; manual debug button useful | ✅ Wired (auto-path) |
| **Request stats** | `Command::RequestStats` → `Event::Stats { EngineStats }` | Dev HUD overlay (WASM heap, undo depth, glyph cache, paint ms) | ⚠️ Partial — polled `App.tsx:98` but rendered nowhere visible |
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
| **ARIA live announcements** | `Announcements.tsx` | Visible in `Announcements.tsx` | ⚠️ Partial — present, but no event source currently emits human-readable announcements |

---

## 18. Engine Lifecycle, Telemetry & Diagnostics

| Engine Feature | WASM Command / API | Required UI Component | QA Status |
|---|---|---|---|
| **Init** | `Command::Init` | Auto in worker boot | ✅ Wired |
| **Dispose** | `Command::Dispose` | Auto on page unload | ✅ Wired |
| **Recover (after trap)** | `Command::Recover` | Crash overlay → "Reload" button | ⚠️ Partial — worker traps reconnect, but recovery is **stub** (event-log snapshots empty placeholders) |
| **Ping / Pong (liveness)** | `Command::Ping` / `Event::Pong` | Test harness only | ✅ Wired (harness path) |
| **Backend detect** (`vello` vs `canvas2d`) | `detect_backend()` (`lib.rs:573`) | Settings → "Renderer: Canvas2D \| Vello (experimental)" toggle | 🛑 Blocked — engine detects; no UI choice (Vello reachable only via fresh INIT) |
| **Engine capabilities** | `EngineCapabilities` struct (`event.rs:254`) — `simd`, `shared_array_buffer`, formats list | About dialog | 🛑 Blocked |
| **Engine stats** | `EngineStats` (`event.rs:263`) — heap, undo depth, glyph cache, paint ms | Dev HUD overlay | 🛑 Blocked — polled but unrendered |
| **Telemetry batch** | `TelemetryEvent` / `TelemetryBatch` (`telemetry.rs`) — D5.7 mock transport | None (mock `console.log` transport for MVP) | 🕳 Mock — collector exists, no live UI |
| **Error toast** | `Event::Error { message }` | Toast component on error events | 🛑 Blocked — events fire, no visible toast surface |
| **Trap overlay** | `Event::Trap` | Full-screen modal with stack + Reload | ⚠️ Partial — `EngineClient.onTrap` callback exists; `App.tsx` only remounts canvas (no user-visible overlay) |

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

Bridge commands whose UI dispatch count is **zero** (from grep of `ts/src/`):

| Command | Status |
|---|---|
| `Command::SetZoom` | 🛑 No UI |
| `Command::SetViewport` | 🛑 `TODO` at `EditorCanvas.tsx:61` |
| `Command::Tick` | ⚠️ Internal only |
| `Command::UnloadFont` | 🛑 No UI |
| `Command::CloseDocument` | 🛑 No UI |

Commands with **partial** UI (engine surface broader than UI exposes):

| Command | Gap |
|---|---|
| `Command::ApplyFormatting` | `UnderlineStyle::{Double,Dotted,Dashed,Wavy}` unreachable; no `vertical_script` field; no `caps`/`smallCaps`/`clear` |
| `Command::ExportPdf` | Only `A1b` selectable; `A2u`/`X3` enum variants unused |
| `Command::OpenDocument` | Only via drag-drop; no file-picker entry |
| `Command::InsertImage` | Engine ready; no UI trigger at all |
| `Command::SaveDocument` / `Command::SaveDocx` | Toolbar button only; no `Ctrl/Cmd+S` shortcut |
| `Command::SetCellBorders` | Sets all 4 edges identically; no per-edge picker |
| `Command::PasteHtml` / `Command::PastePlain` | Wired; rich-`.docx` fragment paste path not yet exercised in `.docx → .docx` clipboard tests |

Engine APIs without **any** UI consumer:

| API (lib.rs) | Status |
|---|---|
| `Engine::revisions_snapshot()` | 🛑 No UI |
| `Engine::comments_snapshot()` | 🛑 No UI |
| `Engine::media_entries()` | ⚠️ Internal only (used during `LoadDocx`) |
| `Engine::with_vello()` / `detect_backend()` | 🛑 No user-facing renderer choice |

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

**Coverage summary:** **48 / 57** bridge commands have at least one UI dispatch site; **9** are fully blocked. **8 / 25** events have no UI consumer at all. Two read-only engine snapshots (`revisions_snapshot`, `comments_snapshot`) representing significant `.docx` semantic content have **zero** UI rendering. These gaps are the primary obstacle to systematic manual QA against the current engine.

**Sprint 9 (closed `#9`, `#15`).** Three rows flipped 🛑 Blocked → ✅ Wired:
section 13 *Resolve / delete comment* (`Command::ResolveComment` /
`Command::DeleteComment` now round-trip `resolved` through
`word/commentsExtended.xml`), section 15 *Export plain text*
(`engine::DocumentTree::to_plain_text`), and section 15 *Export HTML*
(new `crates/format-html`). Engine-minted comment paraId synthesis +
OPC plumbing for fresh-document commentsExtended.xml remain open as
`#18` (tech-debt).
