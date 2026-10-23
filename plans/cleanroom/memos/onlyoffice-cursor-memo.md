# OnlyOffice caret and selection: sanitized methods memo

**Provenance.** `https://github.com/ONLYOFFICE/sdkjs.git`, local `/data/code/reference/onlyoffice-sdkjs`, commit `75a9683e0f34472d646642f7a9b7f81dfb91aa23` (develop, shallow). Studied 2026-10-23 by a clean-room Reader under `plans/cleanroom/PROTOCOL.md`. AGPL-3.0, ideas only. This is from reading the code, not from running the product. Word's behaviour is never asserted here; verify it black-box.

**Shape.** The document is a set of *stories*: the body, each header or footer object, footnotes, endnotes and the drawing layer. Exactly one story is active, and its own controller receives every caret, selection and edit call. Caret state is *distributed*: every container (document, table, cell, paragraph, inline container, run) keeps a "current child" pointer plus selection start and end pointers. The caret is the chain of these pointers, and an inactive story keeps its own chain. One bidi reorderer feeds painting, caret x, hit-testing and selection rectangles.

## 1. Caret model
- **Position is logical**: a path of indices from the paragraph through inline containers and runs down to an offset.
- **Line hint.** The paragraph may also store which line, and which segment of it, the caret sits on (a segment is a piece of a line split by float wrap). With no hint, a wrap point displays at the *start of the next line*. Only geometry sets the hint: a click, Up/Down, or Home/End. Then the same position can display at the end of the upper line. Logical moves and edits clear it.
- **Selection.** At every level, the start pointer is the anchor and the end pointer is the focus. Each end has its own hint: the earlier end prefers the following line and the later end the preceding one. A selection that ends at a wrap point is therefore drawn on the upper line.
- **Sticky column** (page x, plus a y): held at the top document and by each paragraph of an inner story. Horizontal moves, Home/End, clicks, edits and IME refresh it; Up/Down and PageUp/PageDown refresh only y. A block that a vertical move leaves hands its sticky x to the block it enters.
- **Direction affinity is not stored.** The caret is drawn at the *leading edge*, in its resolved direction, of the element after it. Typing exception: if the element before the caret was just typed, the caret is drawn at that element's trailing edge, so typing at a direction boundary never makes it jump.
- **Normalisation after every logical move or click:**
  - The caret never lands inside a base + combining-mark cluster.
  - At a hyperlink's edge, the caret moves outside the link (fields and content controls keep their edge positions inside).
  - At a run's start that is not a line start, the caret moves to the end of the previous run, so formatting is inherited from the left.
  - Empty runs are left behind.
- **Undo snapshot**: story, pointer chain, sticky x/y, page, and the header page instance.

## 2. Click placement
- **Page.** A point inside a page belongs to it, with x clamped near the page edges. The gap below a page belongs to the page above, and points above the first or below the last page belong to that page. Page-local y may fall outside the page; later clamping absorbs it.
- **Bands.** The *body area* is the section margins, pushed inward wherever the laid-out header or footer overflows its margin. Above it is the header band; below it the footer band. A point inside a header or footer drawing counts as that band.
- **Editing the body:** a single click anywhere, bands included, resolves to the nearest body line. A **double-click in a band** enters that page's header or footer, unless the point is over a body floating object or floating table. Note areas are entered with a single click.
- **Editing a header or footer:** a single click in either band of any page switches straight to that part. A single click in the body is *ignored*. A double-click in the body exits and places the body caret at the click. Clicking a band whose part does not exist creates it, as an undoable action.
- **Drawings are layer-scoped.** Only the active story's drawing layer is hit-tested, so header objects are inert while the body is edited, and vice versa.
- **Objects against text.** Objects in front of text and inline objects beat text: the object is selected and the caret hidden. Behind-text objects win only off text. So a single click on an inline picture places no caret.
- **Body block.** The section is chosen by y, the column by x split at the midpoint of the gap (an empty trailing column falls back to the last used one), and floating tables take precedence. The block is the one whose next sibling starts below y, clamped to the first or last block, and its fragment on that page and column is used.
- **Paragraph:**
  - The line is the first whose bottom (baseline + descent + line gap) lies below y; otherwise the last line.
  - The segment is chosen by x midpoints, mirrored for RTL.
  - Elements are walked in visual order, comparing x with both edges of each glyph (the edges swap for RTL glyphs); the nearest edge wins.
  - An x outside the segment clamps to its logical start or end, mirrored for RTL.
  - A line break offers no position after itself, and the paragraph mark only offers one to selections.
  - Floating anchors have zero width; hidden field code is skipped.
  - A separate *on-text* test (x inside a glyph, y inside the line's ascent–descent band) gates link following and drag-and-drop.
- **Below the text** resolves to the last line. A double-click there is click-and-type, which appends paragraphs.
- **Tables.** Hit zones: borders (resize), left of a row, above a column, a cell's inner left edge (all selection zones), otherwise text. The cell is the one in the grid column under x (clamped), in the first row of this page's fragment whose bottom, across its merges, is below y. A merge continuation resolves to its merge start. Cell padding belongs to the cell's text hit-test.

## 3. Horizontal arrows
- **Logical, never visual.** The key mapping flips when the caret's paragraph is RTL. Mixed text inside a paragraph is walked logically, so the caret can jump visually.
- **Steps.** One grapheme cluster per press; an inline picture is one step. Floating anchors and hidden field code are skipped. The paragraph mark is passed only while extending a selection.
- **Fields.** The caret enters the field result. After each press, a selection whose ends sit in different fields grows in the direction of travel to cover them whole: fields are atomic for selection, not for the caret.
- **No end-of-line stop.** Right moves to the next logical position, shown at the start of the next line.
- **Collapsing.** With a selection and no Shift, Left collapses to the logical start and Right to the logical end, with no further move.
- **Edges.** At a paragraph edge the move continues into the neighbouring block; at a story edge nothing happens.
- **Neutral characters (the period case).** A run of neutrals takes the opposite of the paragraph direction only when strong characters of that opposite direction enclose it on both sides. Otherwise it takes the paragraph direction, with one exception: at a line end in an LTR paragraph, neutrals that follow RTL text stay RTL and attach to it visually. Digits form LTR islands, and separators between digits join them.
  - **Vertical position never depends on direction.** A line has one baseline, and a run's only vertical offsets are its explicit raise and sub/superscript.
  - The face comes from the code-point class plus the run's `<w:cs/>` / `<w:rtl/>` flags. A flagged run puts its neutrals on the complex-script face, the same face as the Arabic beside them.

## 4. Vertical arrows
- **Body: geometric probe.** Start from the sticky x and the caret's baseline. Step y in small increments, re-running the §2 hit-test, until the resolved baseline changes. Past the page bottom, continue from the top of the next page, skipping pages with nothing to hit. Tables, merges and sections need no special code: the probe enters the cell under x. Inference: in multi-column sections the probe reaches the same column on the next page, not the next column.
- **Inner stories (header, footer, text box, note): structural.** A paragraph moves one line index at the sticky x (and sets the hint). At its first or last line, the move goes to the neighbouring block's last or first line. A table moves to the adjacent row, taking the cell whose grid end is at or past the sticky x. At the story edge nothing happens. **Arrows never cross between a header and the body.**
- **Collapsing.** With a selection and no Shift, the selection collapses first; if that changed the paragraph, the keypress ends there.
- **Shift.** Shift past the document's edge extends to its start or end. Shift across a cell boundary switches to cell selection.
- **Column resets** come only from horizontal moves, clicks and edits. Ctrl+Up/Down is not implemented.

## 5. Home/End, word moves, paging
- **Home/End** go to the logical start or end of the current line segment (not mirrored for RTL). They set the hint, so after End the caret is drawn at the end of the same line. A trailing manual break and the paragraph mark are excluded.
- **Ctrl+Home/End** stay within the active story: the body even from a cell, the part when inside a header.
- **Ctrl+Left/Right** are logical and flip in RTL paragraphs. Classes: text, punctuation (a class of its own), separators (space, tab, no-break space). Any non-text element is a word by itself. Ctrl+Left skips separators, then a run of one class. Ctrl+Right skips a run of one class, then the separators after it. There are no Arabic-specific rules.
- **PageDown/Up** move by the viewport height at the sticky x, falling back to one line if the caret does not advance. In header editing they walk the part instances page by page.

## 6. Selection
- **Shift+click** extends from the anchor. **Shift+arrows** move only the focus; reaching the anchor again collapses the selection.
- **Click count.** A double-click selects words and drag extends by words; a triple-click does the same with paragraphs. A double-clicked word includes its trailing spaces.
- **Tables.** A drag inside one cell is a text selection. Once the focus enters another cell it becomes a *cell rectangle*: the union of the start and end cells' grid spans (ignoring near-zero sliver columns), covering every intersecting cell in the rows between. If the selection also spans blocks outside the table, the table contributes whole rows.
- **Select All follows the active story:** the whole body (even from a cell), the part only (header or footer), the box only (text box). With an object selected, the object selection is dropped and the whole body selected.

## 7. Tables
- **Left/Right** follow logical cell order, skipping merge continuations; leaving the first or last cell exits the table. **There is no RTL-table logic** (`<w:bidiVisual>` is ignored), so OnlyOffice is not a model for RTL tables.
- **Tab / Shift+Tab** move to the next or previous cell and select its content (or put the caret at its start if it is empty). Tab in the last cell appends a row. The innermost table handles the key.
- **Delete** at the end of a paragraph that precedes a table merges the paragraph into the first cell. **Backspace** after a table does not merge. Deletion never crosses a cell boundary.

## 8. Headers, footers, notes, text boxes
- **Menu entry** simulates a header-mode click at the page's top or bottom corner.
- **Caret on double-click entry:** the *start of the part*; the click point is not used. The part is hit-tested in its own frame, so the caret always lands on one of its lines.
- **Page instance.** One part can serve many pages. It keeps a laid-out snapshot per page and loads the clicked page's snapshot, so page fields show that page's number. Changing part or page clears the old selection.
- **Exit.** Esc exits, but if an object in the part is selected, the first Esc only drops it. After Esc the body caret goes to the top of the current page (from a header) or its bottom (from a footer), not back to where it was before entry. A double-click in the body also exits.
- **Text boxes.** Esc or Delete returns the caret to just before the shape's anchor.

## 9. Pictures and inline objects
- **Object selection replaces the caret.**
- **Leaving object selection** (Esc, Delete, typing) puts the caret just before the object's anchor and activates the story that owns it. With layer-scoped hits, that is the story that was already active.
- **Arrows** nudge a selected floating object; **Tab** cycles the selection.
- **After inserting a picture**, the picture is object-selected.

## 10. Caret after edits
- **Enter** puts the caret at the start of the second half; at a paragraph's start it stays with the original text. An empty numbered paragraph loses or outdents its numbering instead of splitting.
- **Backspace/Delete** across a paragraph boundary merge with the caret at the join (an empty neighbour is simply removed).
- **Paste** leaves the caret after the inserted content.
- **Undo** restores the pre-action snapshot. **Redo** restores the snapshot taken before the next action.
- **Formatting a collapsed caret** inserts an empty run carrying the new properties and moves the caret into it. Moving away discards that run.
- **IME.** Composition replaces the selection and works inside the run at the caret. An empty commit undoes back to the snapshot from before the composition.

## 11. Caret rendering
- **The caret is an overlay**, not painted into the page.
- **Geometry.** The top is the line baseline minus the caret run's font ascent (adjusted for sub/superscript and raise). The height is that font's ascent + descent, **not the line height**. An empty paragraph uses the paragraph mark's font. Outside the story's clip the caret is hidden.
- **Colour** is the text colour at the caret. The width is fixed in CSS pixels; the height scales with zoom.
- **Blink** restarts on every arrow press. Caret updates are coalesced to the next paint; vertical moves force an immediate update.
- **Scroll into view** only when the caret moved, by the overflow plus a small margin.

## Reader's mapping to the observed bugs
1. **Caret outside the band:** the part is hit-tested in its own frame, and entry puts the caret at the part's start.
2. **Footer double-click opens the header:** the band comes from y against the body-area edges, never "the first part found".
3. **Reversed arrows in cells:** horizontal moves are logical, flipped by paragraph direction; cell crossing follows logical order; vertical moves are geometric.
4. **Image click leads to the header:** drawings are hit-tested only in the active story's layer, and leaving object selection activates the object's own story.
5. **Period off the baseline:** one baseline per line, and a neutral's face comes from its run's flags. Horizontally, a line-final period after RTL text in an LTR paragraph attaches to that RTL text.

## Confidence / not found
- **RTL tables:** unsupported in this tree.
- **Ctrl+Up/Down:** not implemented.
- **Arrows with an inline object selected:** behaviour not determined.
- **Typed character with a picture selected:** whether it is inserted is not confirmed.
- **Backspace after a table:** other effects not traced.
- **Column-to-column vertical moves:** inferred, not observed.
- **Arrows at footnote and text-box edges:** only partly traced.
- **Arabic-specific word boundaries:** not determined.
- **IME sub-range styling:** not studied.
- **Tuned values** (blink rate, probe step, tolerances, caret width, nudge distance) are omitted on purpose.
