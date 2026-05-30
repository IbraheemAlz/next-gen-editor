# Engine UX Specification & Roadmap

Authoritative description of how the editor MUST behave. Replaces ad-hoc judgement when a question comes up like "what should arrow-left do here?".

Rooted in:
- **UAX #9** — Unicode Bidirectional Algorithm.
- **UAX #29** — Unicode Text Segmentation (grapheme, word, sentence).
- **W3C `contenteditable`** conventions (no spec is normative; cite Chromium / Gecko behaviour where they agree).
- **MS Word**, **Google Docs**, **LibreOffice Writer**, **OnlyOffice** observed behaviour.

When two references disagree, **MS Word wins** for arrow / selection semantics — every user in the target market built their muscle memory there. Where Word's behaviour is itself ambiguous or platform-specific (e.g., macOS option-arrow vs Windows ctrl-arrow), we pick the most common cross-platform variant and pin it.

Conventions:
- `[STATUS]` tag on each rule: `[DONE]` = current engine matches, `[PARTIAL]` = partially implemented, `[TODO]` = not yet wired, `[INVARIANT]` = must never regress.
- "Logical order" = byte order in the UTF-8 source string. "Visual order" = left-to-right pixel order on screen after BiDi reorder.
- "Caret offset" = byte offset inside the host paragraph's `text` field, always at a Unicode `char_boundary`.

---

## I. Caret Navigation (Linear & Spatial)

The caret is the engine's primary navigational state. Every motion command mutates `SelectionState { anchor, caret, ideal_x, kind }` and emits `SelectionChanged`. The UI is a passive renderer.

### I.1 — Left / Right (logical step)

| Modifier | Behaviour |
| --- | --- |
| none | One **grapheme cluster** in logical order (UAX #29 grapheme break). At paragraph start, wrap to end of previous paragraph in the doc-flat walk. At paragraph end, wrap to start of next. At document boundary, pin. |
| Shift | Same motion; anchor stays put. Selection becomes (anchor, new caret). |

**Discount** — the current implementation steps by `char` (Unicode scalar), not by grapheme cluster. For combining marks (e.g., Arabic harakat, Devanagari conjuncts, emoji ZWJ sequences) this can land between visually-fused glyphs. `[TODO]`: switch to `unicode-segmentation::UnicodeSegmentation::grapheme_indices` or an `icu_segmenter::GraphemeClusterSegmenter` so the caret never visibly bisects a "user-perceived character".

**Cross-paragraph wrap** `[DONE]` — `step_left` / `step_right` already descend into table cells via `doc_paragraph_neighbor`. Cell-paragraph at offset 0 + Left → end of preceding cell-paragraph (or out of the cell to the body paragraph above, if at row 0 col 0). Symmetric for Right.

**Pin at document boundary** `[INVARIANT]` — never wrap from end-of-doc to start.

### I.2 — Up / Down (spatial step)

Walks the **line geometry** (`document_geometry()`'s flat `Vec<LineGeom>`), not the document tree.

| Modifier | Behaviour |
| --- | --- |
| none | Find the geometrically adjacent line above (Up) / below (Down) the current line's `y_top`. Snap caret to the slot nearest `ideal_x`. |
| Shift | Same motion; anchor stays put. |

**`ideal_x` — sticky column** `[DONE]` — On the first vertical motion of a gesture, lock `ideal_x` to the caret's current `x`. Subsequent verticals re-use it. Cleared by any Left/Right/Click/Type. Lets the caret snap back to its original column after wandering through short lines.

**Spatial scan, not doc-order** `[DONE]` — Tables interleave cells in document order in a way that doesn't match visual order: `cell(0,1)` follows `cell(0,0)` in the array but sits to the right on the same row, not below. Up/Down must scan by `y_top` and tie-break by horizontal distance to `ideal_x`.

**Boundary escape (table trap)** `[DONE]` — When the spatial scan finds no candidate AND the caret is inside a cell, fall back to the closest body line outside the containing table. `insert_table` auto-appends a trailing paragraph so the boundary always exists.

**Slotless target lines** `[DONE]` — An empty paragraph's placeholder line has zero caret slots. When the target line is slotless, land on `line.start_byte` rather than bouncing back to the previous caret position.

**Page-boundary continuity** `[INVARIANT]` — The Y proximity scan must work across page breaks. `LineGeom.y_top` is absolute document-Y; pages are stacked vertically with `PAGE_GAP_PT × scale` between them. Pressing Down at the bottom line of page N lands on the top line of page N+1 (or the body line that paginated to N+1).

**Home / End / PageUp / PageDown** `[TODO]`:
- **Home** — caret to start of current line. **Shift+Home** — extend. **Ctrl+Home** — caret to document start.
- **End** — caret to end of current line. **Shift+End** — extend. **Ctrl+End** — caret to document end.
- **PageUp / PageDown** — caret moves by one viewport-height in line geometry; `ideal_x` preserved. **Ctrl+PageUp / PageDown** — page-card-boundary jump (start of next/previous paginated page).

### I.3 — Mouse pointer

| Gesture | Behaviour |
| --- | --- |
| Single click | Hit-test → place caret at nearest slot. Anchor + caret collapse to same position. Reset `ideal_x`. `[DONE]` |
| Drag | `pointermove` during drag → extend selection (anchor pinned at down position, caret follows). Cross-page drags continue to resolve via document-absolute coords. `[DONE]` |
| Double-click | `SELECT_WORD_AT` — UAX #29 word boundary expanding around the hit position. `[PARTIAL — current impl uses whitespace-classifier, not UAX #29 WordSegmenter]` |
| Triple-click | `SELECT_PARAGRAPH_AT` — whole paragraph. `[DONE]` |
| Quadruple-click | `[TODO]` — Whole block (cell content if inside a cell; whole table if `SELECT_TABLE`; whole document on the body). |
| Shift+click | Extend selection from anchor to hit position. `[TODO]` (currently a fresh click resets anchor). |

---

## II. Word Boundaries (UAX #29)

The target word boundary algorithm is UAX #29's `WB1`–`WB14`. The current implementation is a whitespace-only classifier — adequate for Latin and Arabic at the seeded text level, but it misses:

- ASCII punctuation as part of a word (URLs, "isn't" → currently treated as two segments)
- Mid-word numbers (`3.14` → split at `.` currently)
- CJK ideographs (no inter-character whitespace; each ideograph is its own word per UAX #29)
- Emoji + ZWJ sequences (`👨‍👩‍👧` should be one word; current treats whitespace as the only break)

`[TODO]` — Migrate `step_word_left` / `step_word_right` to `icu_segmenter::WordSegmenter` (already a workspace dep). Keep the public motion API (`WordLeft` / `WordRight`) stable; swap only the inner classifier. Same for `SELECT_WORD_AT` and `DELETE_AT_CARET { by_word: true }`.

### II.1 — Ctrl/Cmd + ArrowLeft / ArrowRight

| Motion | Target rule |
| --- | --- |
| `WordLeft` | Caret jumps to the start of the previous **word break** (UAX #29 WB boundary preceding the caret, after skipping any whitespace-only segment). Word's "go to start of previous word" semantics. |
| `WordRight` | Caret jumps to the start of the **next word** — i.e., the WB boundary AFTER the run of whitespace following the current word. NOT the end of the current word. Word's "go to start of next word" semantics. |

**Cross-paragraph** `[DONE]` — At paragraph start, WordLeft hops to the end of the previous paragraph in the doc-flat walk (descends into cells). At paragraph end, WordRight hops to offset 0 of the next paragraph.

**Shift extends** `[DONE]` — `extend: e.shiftKey` carries through to the engine's anchor/focus model.

**Alt+Arrow** `[RESERVED]` — Treat as a no-op for now. macOS Word uses Option+Arrow for word-jump; this overlap is intentional — the TS wiring is `(ctrlKey || metaKey) && !altKey`, leaving Alt available for future motions (subword? camelCase split?).

### II.2 — Ctrl/Cmd + Backspace / Delete

| Motion | Target rule |
| --- | --- |
| Ctrl+Backspace | Delete from caret backward to the previous word boundary. `[PARTIAL]` — engine has `DELETE_AT_CARET { forward: false, by_word: true }`, not yet wired in the TS shell. |
| Ctrl+Delete | Delete from caret forward to the next word boundary. `[PARTIAL]` — symmetric. |

### II.3 — Word segment classification

Current rule: `char::is_whitespace` is the only segment boundary.

Target rule (UAX #29 with carve-outs for ergonomic Word/Google Docs behaviour):

1. **Letter run** — any consecutive `Letter` or `Mark` chars (UAX #29 ALetter / Hebrew_Letter / Katakana / etc.).
2. **Number run** — `Numeric` chars, with `MidNumLet` / `MidNum` (`.`, `,`) joining adjacent number runs (`3.14`, `1,000` are single words).
3. **Whitespace run** — `White_Space`-property chars.
4. **Other run** — punctuation, symbols, control chars (each emit-individual per WB14).

`WordLeft` / `WordRight` jump from the start of one Letter/Number run to the start of the next, skipping intermediate whitespace and treating "other" runs as their own one-char segments.

**Apostrophe inside words** — `"isn't"` is ONE word (WB6 / WB7). Don't break at the apostrophe.

**URLs and emails** — out of scope for caret motion. They're a tokenizer concern; treat them as ordinary word runs (motion may split them at `.` and `/`).

---

## III. BiDi (RTL/LTR) Navigation — UAX #9

The hardest part of the spec. The current engine treats ArrowLeft as logical-Left (lower byte offset) and ArrowRight as logical-Right (higher byte offset) regardless of paragraph direction. This is WRONG by Word's convention and confuses bilingual users.

### III.1 — Target rule (Word-compatible visual navigation)

ArrowLeft and ArrowRight are **visual** motions, not logical. They follow the screen direction the user sees.

| Paragraph base direction | ArrowLeft maps to | ArrowRight maps to |
| --- | --- | --- |
| LTR (English-led) | logical Backward (offset−) | logical Forward (offset+) |
| RTL (Arabic-led) | logical Forward (offset+) | logical Backward (offset−) |

`[TODO]` — Implement BiDi-aware arrow mapping. The engine already knows the paragraph's resolved direction (`ParagraphBox.direction` from layout); plumb it into `do_move_caret` via the caret's host paragraph.

**Inside a BiDi-mixed line** — at the seam between a logical-LTR run and a logical-RTL run, ArrowLeft / ArrowRight follow **visual continuity**: the caret moves one visual position left/right, even if that means jumping discontinuously in logical byte order.

Concrete example. Text `"abc ABC"` (lowercase = LTR; uppercase = logical RTL letters with strong-RTL char property). Logical bytes `[a, b, c, _, A, B, C]` at offsets 0..7. After UAX #9 reorder, visual order is `a b c C B A` (the RTL run "ABC" reorders to "CBA"). With caret between `c` and `C` (logical offset 4, between the space and `A`):

- ArrowRight (visual) → caret moves visually right, landing logically AFTER `A` (offset 5), because in the visual order `C B A` the next visual position right of `C` (which sits at logical 6) is `B` at logical 5.
- ArrowLeft (visual) → caret moves visually left, landing logically back at offset 4 (between space and `A`).

This is the "caret follows the cursor key the user pressed, in screen direction" guarantee. UAX #9 / W3C call it the **visual** caret model; Word and Google Docs both implement it.

### III.2 — Word jump in BiDi

Same visual-mapping rule applies to Ctrl+Arrow:

| Paragraph base direction | Ctrl+ArrowLeft | Ctrl+ArrowRight |
| --- | --- | --- |
| LTR | `step_word_left` (logical backward) | `step_word_right` (logical forward) |
| RTL | `step_word_right` (logical forward) | `step_word_left` (logical backward) |

`[TODO]` — Same plumbing as III.1.

### III.3 — Cell-paragraph direction inheritance

Each table cell can carry its own paragraph direction. When the caret sits in a cell, BiDi resolution uses the **cell paragraph's** resolved direction, not the body's. Switching ArrowLeft / ArrowRight orientation per cell.

`[TODO]` — Cell paragraph direction is already stored; just route the lookup.

### III.4 — Direction-aware Home / End

| Paragraph base direction | Home lands on | End lands on |
| --- | --- | --- |
| LTR | Logical offset 0 (visual leftmost slot) | text.len (visual rightmost slot) |
| RTL | Logical offset 0 (visual RIGHTMOST slot) | text.len (visual LEFTMOST slot) |

Both Home and End are logical — they refer to the **start** and **end** of the paragraph's text in source order, which is the visual leading and trailing edge respectively. Word's behaviour.

### III.5 — Caret rect orientation at the BiDi seam

A caret at the boundary between an LTR run and an RTL run has TWO valid visual positions — one at the trailing edge of the LTR run, one at the trailing edge of the RTL run (which visually point in opposite directions). The current engine picks one; the target rule is to pick based on the **direction of the most recent motion**:

- If the caret arrived via ArrowRight in an LTR-base paragraph → render at the LTR-trailing position (the visual right edge of the LTR run).
- If the caret arrived via ArrowLeft in an LTR-base paragraph → render at the RTL-trailing position.

`[TODO]` — Track the last motion's "directional intent" on `SelectionState` (or derive from `ideal_x` carrying a small direction flag).

### III.6 — Selection across BiDi seams

Already handled by `selection_rects_geom` emitting one rect per `RunGeom` (Backlog #7 fix landed pre-beta.3). A selection that crosses a directional run boundary renders as two disjoint rects, not one box that over-covers the gap. `[DONE]`

---

## IV. Cross-Container Selection

A "container" is any sub-tree of the document that can host paragraphs: the body, a cell, a footer (Phase 6b), a footnote band (Phase 8a). Selections that start in one container and end in another raise three problems: rect generation, logical range correctness, and edit-command semantics.

### IV.1 — Linear range across paragraphs (same container)

Anchor + caret may sit in different paragraphs within the same container. `cmp_doc_order(anchor, caret)` puts them in canonical order; `selection_rects_geom` emits rects for every line in the bracketed paragraphs. `[DONE]`

### IV.2 — Selection that includes a whole table

When the selection range strictly brackets a table (start.path precedes `[Block(t_idx)]`, end.path follows it, AND end.path is not a descendant of the table), the entire table renders as **filled cells** — every line inside every cell gets a `hit_left .. hit_left + hit_width` rect instead of a per-text-span clip. `[DONE]` (commit `b4f6823`).

**Single big rect vs filled cells** — current implementation emits filled cells. The alternative (one solid rect covering the table's outer bbox) over-covers the inter-cell gutters and looks ugly with non-rectangular tables (rowspan/colspan). Filled cells is the chosen target.

### IV.3 — Selection that ends INSIDE a table

E.g., anchor in body paragraph 0, caret in cell(1, 2). The bracketed-table detector skips this table (end.path is a descendant). Cells visited up to the cell containing the caret render as filled; the partial cell containing the caret renders as a text-span clip up to the caret offset. `[DONE]`

### IV.4 — Cell-rectangular selection (drag inside a table)

A drag whose start AND end both fall inside the same table renders as a **rectangular block of cells** (`SelectionKind::TableCells`), not a linear text range. The selection covers every cell whose row ∈ [from_row, to_row] AND col ∈ [from_col, to_col]. `[DONE]` (Phase 5 PR 4.4).

**Linear vs cell selection switchover** — currently determined by `derive_selection_kind` on every selection mutation. The kind is sticky once set; a drag that starts cell-rectangular stays cell-rectangular even if the cursor briefly exits the table. `[DONE]`

### IV.5 — Selection across page boundaries

Pages are paginated layout fragments, not document containers. A selection from page 1 to page 3 is logically a linear range that happens to render rects on each intervening page's overlay. The per-page `PageSelectionOverlay` filters by Y range. `[DONE]` (Phase 6c).

### IV.6 — Selection across containers (body ↔ footer ↔ footnote)

`[TODO]` — Currently illegal: anchor in body, caret in footer is not addressable via the linear `BlockPath` model. The target rule is to permit it (BlockPath augmented with a container tag), but defer rendering and edit semantics to a follow-up sprint. Word handles this with "discontinuous" selections (Ctrl+drag). Out of scope until the body↔footer cross-container case has a concrete user request.

### IV.7 — Shift+Click

Sets caret to the click position WITHOUT resetting anchor. `[TODO]` — currently every click resets anchor. Fix in `pointer.ts::onPointerDown` by checking `e.shiftKey` and routing to `EXTEND_SELECTION` instead of `SET_SELECTION`.

### IV.8 — Shift+ArrowDown / Shift+ArrowUp across paragraphs and tables

Already supported via the existing `extend: e.shiftKey` pass-through plus the spatial scan and boundary escape. `[DONE]`

### IV.9 — Shift+End / Shift+Home

`[TODO]` — Bound to Home/End wiring (I.2).

---

## V. Clipboard Operations

Three flavours: plain text, rich text (HTML), and the engine's own internal format. Web clipboard exposes `text/plain` and `text/html` types; `text/rtf` is supported on macOS Safari only.

### V.1 — Copy

Output formats:
1. **`text/plain`** — UTF-8 plain text. Paragraphs joined with `\n`. Table cells joined with `\t` within a row, rows joined with `\n`. `[DONE]`
2. **`text/html`** — A minimal `<div>` containing `<p>`, `<table><tr><td>`, `<strong>`, `<em>`, `<u>`, `<a>` elements that round-trip into the engine on paste. Out-of-scope tags (script, style, headers/footers) stripped. `[PARTIAL]` — engine emits plain `<p>` runs; bold/italic/underline preserved; tables emitted but cell attributes not yet round-tripped.

**Cut** = Copy + `DELETE_RANGE`. `[DONE]`

### V.2 — Paste

Input formats, in priority order:
1. `text/html` from a known-good source (Word, Google Docs, this app's own copy). Parsed by the HTML paste path (`PASTE_HTML`). `[PARTIAL]` — handles `<p>`, `<table>`, basic spans; ignores CSS classes.
2. `text/plain` — raw UTF-8; inserted as a single text run at the caret. `[DONE]`
3. **`text/rtf`**, `image/png`, drag-dropped files — out of scope at the spec tier; routed to specific commands (`INSERT_IMAGE`, `PASTE_PLAIN` fallback). `[PARTIAL]`

**Paste-as-plain shortcut** — Ctrl+Shift+V should force the `text/plain` path even when `text/html` is present. `[TODO]` — not yet wired.

### V.3 — Clipboard data sanitization

`[INVARIANT]` — Pasted HTML is parsed by the engine's HTML paste parser, NOT inserted into the live DOM. Never `innerHTML` paste data; the parser must whitelist tags, ignore inline JavaScript, and reject `<style>` blocks.

### V.4 — Cross-document copy/paste (round-trip)

Copying a styled selection from this editor and pasting it back into the SAME editor must round-trip without style loss. Bold + italic + a `<a>` hyperlink survive the HTML serialize → clipboard → HTML parse cycle. `[PARTIAL]` — Phase 5b sprint shipped the basic round-trip; sub-properties (cell borders, image sizing) lose fidelity.

### V.5 — Selection state after paste

Caret lands AFTER the pasted content. Selection collapses (anchor = caret). The pasted range remains undoable as a single `UNDO` step. `[DONE]`

---

## VI. Roadmap (consume in this order)

Each section is its own commit / sprint. Sections III and IV cross-cut — III should land first so cross-container selections in BiDi paragraphs are correct.

1. **Grapheme-cluster Left/Right** (I.1) — switch char-step to grapheme-step. Foundational; everything else assumes this.
2. **Home / End / Ctrl+Home / Ctrl+End / PageUp / PageDown** (I.2, III.4) — caret motion primitives.
3. **UAX #29 word boundaries** (II) — swap the inner classifier to `icu_segmenter::WordSegmenter`.
4. **BiDi-aware arrow mapping** (III.1, III.2, III.3) — track paragraph direction at the caret, swap ArrowLeft/Right semantics in RTL paragraphs.
5. **Caret rect orientation at BiDi seam** (III.5) — directional intent on `SelectionState`.
6. **Shift+Click and Ctrl+Backspace/Delete word** (II.2, IV.7) — minor wiring on existing engine commands.
7. **Quadruple-click block select** (I.3) — extension of triple-click.
8. **HTML clipboard fidelity** (V.1, V.2) — flesh out the engine's HTML serializer and parser.
9. **Discontinuous and cross-container selection** (IV.6) — only when a concrete user case appears.

---

## VII. Invariants (never regress)

The bullet items tagged `[INVARIANT]` above, restated in one place:

1. **Pin at document boundary** — Left at offset 0 of the first paragraph never wraps. Right at the last paragraph's `text.len` never wraps.
2. **Page-boundary continuity** — Up/Down via the spatial scan must work across pagination breaks; tables that span pages must still be navigable cell-to-cell.
3. **Clipboard sanitization** — Pasted HTML is parsed, not `innerHTML`-injected. No code execution from paste.
4. **Engine owns selection** — The TS shell never holds caret state; every motion round-trips through the engine. UI is a passive renderer of `SelectionState`. (CLAUDE.md core invariant.)
5. **Anchor + caret + ideal_x are the complete navigational state** — no out-of-band "intended position" tracking in the TS shell or in scratch state.

---

## VIII. Out of scope (deliberately)

Don't implement these without an explicit user request:

- **Drag and drop of selected text** within the document. Word does this; we don't yet.
- **Smart quotes / autocorrect** during typing.
- **Track-changes redline view** during editing (the data model exists; the rendering / accept-reject UI does not).
- **Caret prediction / IME composition window styling** beyond the existing inline underline (Backlog #8).
- **Multi-caret editing** (Sublime / VS Code style).
- **Bookmark / anchor jumps** (Word's `<w:bookmarkStart>` is read but not navigable).

Document them here so they aren't lost; pick them up when a user requests them.
