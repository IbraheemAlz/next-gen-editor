# Supported features

Internal checklist of what the NGE editor can do right now. High-level
only; no implementation details.

## Text formatting

- Bold
- Italic
- Underline (Single / Double / Dotted / Dashed / Wave / None)
- Strikethrough
- Font family (resolved + raw)
- Font size
- Text color
- Highlight / background color
- Superscript / subscript
- Small caps + all caps
- Sticky / pending format on collapsed caret

## Paragraph & layout

- Left / Center / Right / Justify alignment
- Left-to-right and right-to-left direction (with mixed BiDi)
- First-line indent, hanging indent, left / right indent
- Line spacing
- Paragraph shading
- Paragraph borders
- Tab stops (Left / Center / Right / Decimal kinds) via Interactive Ruler
- Paragraph styles (Normal, Title, Heading 1–3, …) — assign existing style
- Style cascade with `direct_overrides` preservation

## Page & section layout

- Page margins
- Page orientation (Portrait / Landscape)
- Multi-column sections (with gutter)
- Continuous section breaks with column balancing
- Page breaks (manual + paginator-flushed)
- Multi-page rendering
- Section-aware Page Setup Dialog

## Tables

- Insert table (N rows × M columns)
- Insert / delete row
- Insert / delete column
- Delete entire table
- Merge cells / split cell
- Cell shading
- Cell borders (per-edge)
- vMerge (rowspan) preservation
- gridSpan (colspan) preservation
- Autofit honouring `min-content` (long URLs / tokens never clip
  mid-character)
- Table Context Menu + Cell Properties Dialog

## Lists & numbering

- Bullet lists
- Numbered lists
- Toggle Off / Bullet / Number
- 9-level nesting with restart-on-parent-change
- Multiple list formats (Decimal, LowerLetter, UpperRoman, Bullet,
  Hebrew, Aiueo, IroIro, DecimalEnclosedCircle, …)
- AbstractNum reuse across repeated toggles (no list inflation)

## Document I/O

- Open `.docx`
- Save `.docx` (sibling entries byte-identical on round-trip)
- Export HTML (standalone HTML5 with inline images as data URIs)
- Export Plain Text
- Export PDF (incl. PDF/A-1b conformance)

## Comments

- Insert comment over a range
- Resolve / reopen comment
- Delete comment
- Comments rail listing with author + timestamp
- `commentsExtended.xml` round-trip preserving `resolved` state

## Track changes

- Toggle Track Changes on / off
- Insertions wrapped as `<w:ins>` with author + date
- Deletions wrapped as `<w:del>` with author + date
- Format changes tracked
- Accept revision / Reject revision
- Track Changes sidebar listing
- Review identity (author name / date) configurable

## Editing & selection

- Insert / delete / replace text
- Undo / Redo (bounded depth 100)
- Cut / Copy / Paste (plain, HTML)
- Select All
- Hit test (pixel → caret position)
- Click / drag selection (linear + table-cell)
- Word, paragraph, cell select-at
- Keyboard navigation (arrow keys with ideal-x, Shift-extend, Home /
  End, Ctrl-Home / Ctrl-End)
- Triple-click paragraph selection
- IME composition with inline underlined preview (CJK)
- Drag-and-drop image insert
- Insert page break

## Rendering & viewport

- Canvas2D rendering (primary)
- Vello / WebGPU rendering (opt-in)
- Zoom (25 % → 400 %)
- Multi-page DOM canvases (per-page OffscreenCanvas)
- Predictive lazy pagination (fast-flick scroll runway)
- Dirty-rect repaint
- Smooth scrollbar (over-estimating, never yanks)
- Embedded images
- BiDi-aware caret + selection rects

## Document tools

- Word count (UAX-#29; meaningful for CJK / Thai / Khmer)
- Page count
- Engine stats (paint time, layout time, paragraph count)
- Style picker dropdown
- Footnotes
- Headers / footers (default / first / even slots)
- Page numbering (PAGE + NUMPAGES fields, per-section restart)

## Accessibility

- Hidden DOM accessibility tree mirroring the engine
- Per-paragraph BiDi via `<p dir>`; UAX-#9 handled by the browser
- `aria-live` announcements on every user-visible mutation
- Polite + assertive announcement priorities
- Real OS IME for screen reader narration of composition
- Hidden textarea as the OS text-input citizen
- Per-cell + per-section state read-back for Properties Dialogs

## Crash recovery

- Worker crash trap → fresh canvas + engine respawn
- IndexedDB event log replay
- Latest-snapshot prune (newest 3)
- Boot overlay on cold load

## Telemetry

- Engine stats over RPC
- Mock telemetry collector (console batching)

## Settings & status

- Status bar (page count, word count, zoom, …)
- Dev HUD (engine internals)
- Settings menu (zoom, renderer pick)
- Trap overlay on fatal engine error

## File formats

- `.docx` (read + write, OPC passthrough on untouched parts)
- `.html` (write — file export)
- `.txt` (write — file export)
- `.pdf` (write — file export)

## Known limitations / deferred

- Discontinuous + cross-container selection
- Multi-line IME composition with CJK candidate popup (manual QA)
- RTL tab anchoring (LTR-style today)
- Modifying a style's definition (assigning existing styles only)
- Creating brand-new user-defined styles
- Character-only styles (`<w:rStyle>`)
