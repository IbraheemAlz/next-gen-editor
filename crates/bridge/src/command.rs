//! `Command` — messages the TypeScript client sends to the engine.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::common::{
    Alignment, BlockPath, Color, Direction, DocFormat, ImageWrapMode, LogicalPos, LogicalRange,
    Point, Rect, RendererDowngrade, TextBoxHop, UnderlineStyle, VerticalScript,
};

/// A command issued to the engine. Serialized internally-tagged
/// (`{ "type": "INSERT_TEXT", ... }`).
///
/// `tsify-next` renders `Option<T>` as `T | undefined`; TS callers must pass
/// `undefined`, never `null`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Command {
    // ===================================================================
    // Phase 1 PoC commands.
    // TODO: Deprecate in Phase 3 — superseded by the §4 schema below once the
    // RequestPaint render pipeline lands. Kept now so the visual-diff goldens
    // and test harnesses stay 100% green.
    // ===================================================================
    /// Liveness probe; engine replies with `Event::Pong`.
    Ping,

    /// Parse and register a TTF/OTF font buffer under `id`.
    LoadFont {
        id: String,
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
    },

    /// Rasterize and paint a single glyph by character (no shaping).
    RasterizeGlyph {
        font_id: String,
        ch: String,
        px_size: f32,
    },

    /// Shape `text` with `font_id` via rustybuzz, then rasterize each glyph.
    ShapeAndRasterize {
        text: String,
        font_id: String,
        direction: String,
        px_size: f32,
    },

    /// Layout + paint a paragraph onto a synthesized A4 page, caching the
    /// layout config so later edits auto-repaint.
    RenderPage {
        text: String,
        font_id: String,
        base_direction: String,
        px_size: f32,
        line_height: f32,
        align: String,
        /// Device-pixel ratio. The engine lays out + paints the page scaled by
        /// this so HiDPI canvases stay crisp; `None` ⇒ 1.0 (the golden suite).
        device_pixel_ratio: Option<f32>,
    },

    /// Insert `text` into the in-engine document. `at == None` inserts
    /// at the engine's LIVE caret (issue #53 — interactive typing
    /// passes `None` so a keystroke racing a click's hit-test can
    /// never land at a stale UI-side position); when no selection
    /// exists (the Phase-1 harness), `None` keeps the historical
    /// append-at-end contract and emits `TextInserted`.
    InsertText {
        at: Option<LogicalPos>,
        text: String,
    },

    /// Pop one snapshot off the undo stack; repaints.
    Undo,
    /// Re-apply one snapshot from the redo branch; repaints.
    Redo,

    /// Replace the in-engine document with one parsed from a `.docx` blob.
    LoadDocx {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
    },

    /// Serialize the in-engine document to a freshly-packed `.docx` blob.
    SaveDocx,

    // ===================================================================
    // Phase 2 schema — PHASE_2_BRIDGE_MEMORY.md §4.
    // ===================================================================
    /* Lifecycle */
    /// Initialize the engine surface for a canvas.
    Init {
        canvas_id: u32,
        dpi: f32,
        locale: String,
        capabilities: ClientCapabilities,
    },
    /// Restore engine state from a snapshot plus a tail of replay commands
    /// (issue #85). `snapshot` is an `engine::snapshot` envelope produced by
    /// [`Command::Snapshot`] — empty means "no base snapshot, replay the
    /// tail onto a fresh document". The engine restores the snapshot, then
    /// replays `log_tail` in order through the normal command path with
    /// painting suppressed, and answers `Event::Recovered`.
    Recover {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        snapshot: Vec<u8>,
        log_tail: Vec<Command>,
        /// Issue #99 — the shell forced this generation off its probed
        /// GPU backend after a crash loop. The engine does not act on it
        /// (the worker already constructed the Canvas2D engine); it echoes
        /// it on `Event::Recovered.renderer_downgrade` so the downgrade is
        /// reported by the same event that reports the renderer.
        #[serde(default)]
        #[tsify(optional)]
        renderer_downgrade: Option<RendererDowngrade>,
        /// Issue #212 — the detached source package for a `snapshot`
        /// taken with `Command::Snapshot.detach_package`: the
        /// `Event::Snapshot.package` bytes whose `package_hash` the
        /// snapshot records. Re-attached when its hash matches; absent or
        /// mismatched, the recovered session saves through the minimal-
        /// package writer (the pre-#134 fallback). Ignored for a snapshot
        /// that carries its package inline.
        #[serde(default, with = "serde_bytes")]
        #[tsify(type = "Uint8Array", optional)]
        package: Option<Vec<u8>>,
    },
    /// Issue #85 — serialize the whole engine session (document tree +
    /// styles + stories + undo window + selection + layout config) into a
    /// versioned `engine::snapshot` envelope; replies `Event::Snapshot`.
    /// Read-only: never logged, never replayed. `seq` is echoed back so the
    /// worker can stamp the persisted row with the event-log position the
    /// snapshot was taken at (the engine itself has no notion of log
    /// sequence).
    Snapshot {
        seq: Option<u64>,
        /// Issue #212 — leave the opened `.docx`'s retained source package
        /// (`DocumentTree::source_package`, #134) OUT of `bytes`: the
        /// snapshot records only its content hash
        /// (`Event::Snapshot.package_hash`) and the caller stores the
        /// package once per document, handing it back on
        /// `Command::Recover.package`. `false`/absent keeps the package
        /// inline — a self-contained snapshot, as before.
        #[serde(default)]
        #[tsify(optional)]
        detach_package: Option<bool>,
        /// Issue #212 — with `detach_package`: the package hash the caller
        /// already stores. When it matches, `Event::Snapshot.package` is
        /// omitted (the bytes are only shipped when the package changed).
        #[serde(default)]
        #[tsify(optional)]
        known_package_hash: Option<String>,
    },
    /// Tear down the engine and release resources.
    Dispose,
    /// Animation/heartbeat tick.
    Tick {
        now_ms: f64,
    },

    /* Document I/O */
    OpenDocument {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        format: DocFormat,
        name: Option<String>,
    },
    SaveDocument {
        format: DocFormat,
    },
    ExportPdf {
        conformance: PdfConformance,
    },
    CloseDocument,

    /* Editing */
    DeleteRange {
        range: LogicalRange,
    },
    ReplaceRange {
        range: LogicalRange,
        text: String,
    },
    /// Apply a character-formatting patch. `range: None` resolves to
    /// the engine-owned live selection (the authoritative one — mirrors
    /// `InsertText`'s caret handling), so a stale UI-side selection
    /// mirror can never misbind formatting. Pass an explicit range only
    /// for a genuine override.
    ApplyFormatting {
        range: Option<LogicalRange>,
        attrs: TextAttrsPatch,
    },
    /// Break the paragraph at the caret (replacing any non-empty
    /// selection). Issue #64 — `at == None` splits at the engine's LIVE
    /// caret (interactive Enter passes `None`, so a keystroke racing a
    /// click can never carry the UI mirror's stale position). An explicit
    /// `at` is consulted only when no selection exists (API / harness
    /// callers); `None` with no selection at all replies `Event::Error`.
    SplitParagraph {
        at: Option<LogicalPos>,
    },
    MergeParagraph {
        left: ParagraphId,
        right: ParagraphId,
    },
    InsertImage {
        at: LogicalPos,
        image: ImageBlob,
        fit: ImageFit,
    },
    /// Issue #44 — overwrite the display extent (`<wp:extent>`) of the
    /// inline image anchored at `(path, at)`. `at` is the `U+FFFC`
    /// sentinel byte offset the image occupies in its paragraph (its
    /// `InlineObject.at`). Dimensions are EMU (914400/inch) — the model's
    /// native unit, threaded straight to the writer. Drives the canvas
    /// resize handles and the numeric size control.
    ResizeImage {
        path: BlockPath,
        at: u32,
        width_emu: i64,
        height_emu: i64,
        /// Issue #206 — the text-box story `path` is rooted in (the
        /// `ImageRect.story` of the picture); empty / absent ⇒ the body.
        #[serde(default)]
        story: Vec<TextBoxHop>,
    },
    /// Issue #69 — reposition the FLOATING (`<wp:anchor>`) image anchored
    /// at `(path, at)`: both positioning axes become fixed EMU offsets
    /// (`<wp:posOffset>`) inside their CURRENT reference frames
    /// (`relativeFrom` is preserved; an `<wp:align>` / percentage
    /// placement is replaced, and `simplePos` is switched off — exactly
    /// what Word does the moment an aligned object is dragged). Drives the
    /// body-drag of the image overlay. Replies `Event::Error` when the
    /// address holds no floating image — inline images flow with the text
    /// and have no free position.
    MoveImage {
        path: BlockPath,
        at: u32,
        offset_h_emu: i64,
        offset_v_emu: i64,
        /// Issue #206 — the text-box story `path` is rooted in (the
        /// `ImageRect.story` of the picture); empty / absent ⇒ the body.
        #[serde(default)]
        story: Vec<TextBoxHop>,
    },
    /// Issue #82 — set the text-wrap mode of the FLOATING image anchored
    /// at `(path, at)` (Word's "Wrap Text" menu): the body text is cut
    /// around it per the mode on the next layout. Side rule, distances
    /// and an existing wrap polygon are kept. Replies `Event::Error` when
    /// the address holds no floating image (inline ↔ floating conversion
    /// is not a wrap mode).
    SetImageWrap {
        path: BlockPath,
        at: u32,
        wrap: ImageWrapMode,
        /// Issue #206 — the text-box story `path` is rooted in (the
        /// `ImageRect.story` of the picture); empty / absent ⇒ the body.
        #[serde(default)]
        story: Vec<TextBoxHop>,
    },

    /* Selection */
    SetSelection {
        range: LogicalRange,
        caret: LogicalPos,
    },
    ExtendSelection {
        to: LogicalPos,
        modifier: SelectionModifier,
    },
    SelectAll,
    /// Move the caret one step (Backlog #14). With `extend: true` the anchor
    /// stays put — the gesture extends the selection. The engine tracks an
    /// ideal-x on `SelectionState` so vertical motion through short lines
    /// keeps its column; horizontal motion resets it.
    MoveCaret {
        direction: MoveDirection,
        extend: bool,
    },

    /* IME */
    /// Start an IME composition. Issue #64 — `at == None` anchors the
    /// composition at the engine's LIVE caret (what `HiddenInput` sends);
    /// an explicit `at` is honoured verbatim for API callers.
    BeginComposition {
        at: Option<LogicalPos>,
    },
    UpdateComposition {
        text: String,
        target_range: Option<LogicalRange>,
    },
    EndComposition {
        commit: bool,
    },

    /* View */
    SetViewport {
        rect: Rect,
    },
    /// Issue #239 — sent before the engine's first `RenderPage`, this is
    /// queued (`Event::ZoomPending`) rather than dropped; `RenderPage`
    /// composes it into the fresh layout config it builds.
    SetZoom {
        scale: f32,
    },
    /// Post-boot devicePixelRatio change (monitor move / browser zoom).
    /// Replaces the boot device scale (`devicePixelRatio × 4/3`) and
    /// recomposes the effective scale with the user zoom untouched —
    /// unlike `SetZoom`, which owns the user-zoom factor. Issue #239 —
    /// same pre-`RenderPage` queuing as `SetZoom`.
    SetDeviceScale {
        scale: f32,
    },
    RequestPaint {
        viewport: Rect,
        dirty: Option<Rect>,
    },
    /// Audit gap C.H1 — viewport-culled lazy pagination. The TS shell
    /// asks the engine to extend its lazy layout down to `target_y` (in
    /// layout pixels at the current device scale, measured from the
    /// document top). Issued on scroll-near-bottom + on click /
    /// PageDown that would land beyond the laid-out region. The reply
    /// is a fresh `Painted` event whose `is_full_layout` flips `true`
    /// when the extend exhausted every remaining block, or stays
    /// `false` when the budget was hit again.
    ExpandLayout {
        target_y: f32,
    },

    /* Fonts / resources */
    UnloadFont {
        id: String,
    },

    /* Telemetry */
    RequestStats,

    // ===================================================================
    // Phase 4 schema — PHASE_4_HEADLESS_UI.md §7.
    // Additive: the frozen §4 schema carries selection commands keyed by
    // `LogicalPos` but no pixel→logical path. These supply it.
    // ===================================================================
    /// Hit-test a canvas pixel to a logical document position. The engine
    /// replies with [`crate::Event::HitResult`]; selection is not mutated.
    HitTest {
        at: Point,
    },

    /// Phase 6c — hit-test for the multi-canvas DOM architecture. `at` is
    /// in the clicked page's LOCAL device-pixel coords (origin at page
    /// top-left). The engine adds the page's accumulated top offset in
    /// document space, so the TS shell routes pointer events from N
    /// independent `<canvas>` elements without any offset math of its
    /// own. Replies with `Event::HitResult` like `HitTest`.
    HitTestInPage {
        page: u32,
        at: Point,
    },

    /// Issue #53 — single-hop caret placement: hit-test the page-local
    /// pixel AND set the collapsed selection in ONE serialized dispatch,
    /// replying with `Event::SelectionChanged`. Replaces the shell's
    /// two-hop `HitTestInPage` → `SetSelection` for plain clicks: the
    /// awaited round-trip between those two opened a window where a
    /// fast keystroke's `InsertText` entered the worker queue ahead of
    /// the `SetSelection` and executed against the stale caret.
    PlaceCaretAtPoint {
        page: u32,
        at: Point,
    },

    /// Issue #64 — single-hop selection EXTENSION: hit-test the
    /// page-local pixel (same coordinate contract as
    /// [`Command::PlaceCaretAtPoint`]) and move the caret there keeping
    /// the anchor, in ONE serialized dispatch; replies
    /// `Event::SelectionChanged`. Replaces the shell's two-hop
    /// `HitTestInPage` → `ExtendSelection` for drag and shift-click, so a
    /// keystroke posted right after a shift-click executes against the
    /// extended selection.
    ExtendSelectionToPoint {
        page: u32,
        at: Point,
    },

    /// Issue #44 — query every inline image's on-canvas rectangle +
    /// resize address. A pure read; replies with `Event::ImageRects`.
    /// The shell issues it after paints to position the resize-handle
    /// overlay and to hit-test clicks against images.
    GetImageRects,

    /// Select the word under a canvas pixel (double-click). The engine
    /// updates the selection and replies with `Event::SelectionChanged`.
    SelectWordAt {
        at: Point,
    },

    /// Select the whole paragraph under a canvas pixel (triple-click).
    /// Selection runs from byte 0 to the paragraph's length.
    SelectParagraphAt {
        at: Point,
    },

    /// Select the whole cell content under a canvas pixel
    /// (quadruple-click inside a table). The engine resolves the hit
    /// position's owning cell; if found, selection runs from offset 0
    /// of the cell's first paragraph to `text.len()` of its last.
    /// When the hit lands outside any cell, the command falls back to
    /// `SelectAll` so quadruple-click in the body still has a useful
    /// meaning (UX_BEHAVIOR_SPEC §I.3).
    SelectCellAt {
        at: Point,
    },

    /// Delete relative to the caret (Backspace / Delete). If the selection
    /// is non-empty it is deleted; otherwise one grapheme — or one word when
    /// `by_word` — is removed in the `forward` direction. The frozen
    /// `DeleteRange` needs explicit positions a document-blind UI cannot
    /// compute for a collapsed caret; this supplies the caret-relative path.
    DeleteAtCaret {
        forward: bool,
        by_word: bool,
    },

    /// Request an incremental accessibility delta — the engine diffs the
    /// document against its last broadcast and replies with
    /// `Event::AccessibilityTreeDelta` (§10, Backlog #10). The worker issues
    /// this after every document mutation to keep the mirror DOM synced.
    RequestAccessibilityDelta,

    /// Snapshot the current selection for the clipboard — the engine replies
    /// with `Event::ClipboardPayload` (PHASE_4_HEADLESS_UI.md §12).
    /// Issue #57 — `include_docx` (absent ⇒ `true`) gates the `.docx`
    /// fragment ZIP build: the shell's debounced clipboard prefetch passes
    /// `false` (it only needs `plain` + `html` for the synchronous
    /// `setData` path) and receives an empty `docx_fragment`.
    GetSelectionAsClipboard {
        #[serde(default)]
        #[tsify(optional)]
        include_docx: Option<bool>,
    },

    /// Paste plain text at the caret, replacing any non-empty selection.
    PastePlain {
        text: String,
    },

    // ===================================================================
    // Backlog sprint 1 — editor UX. Additive: paragraph alignment (Backlog
    // #9). Sticky/pending formatting (Backlog #11) reuses `ApplyFormatting`
    // over a collapsed caret and needs no new command.
    // ===================================================================
    /// Set the paragraph alignment of every paragraph the `range` spans. A
    /// real edit — pushes undo and reflows — but the selection is preserved.
    SetParagraphAlign {
        range: LogicalRange,
        align: Alignment,
    },

    /// Phase 9c — set the paragraph base direction (`<w:bidi>`) of
    /// every paragraph the `range` spans. Direction defines logical
    /// text flow and punctuation placement; alignment is a separate
    /// concern (visual anchoring). Word ties them implicitly via the
    /// direction-relative `Start` / `End` alignment tokens — flipping
    /// direction automatically swaps which visual edge those resolve
    /// to. A real edit (undo + reflow); selection preserved.
    SetParagraphDirection {
        range: LogicalRange,
        direction: Direction,
    },

    // ===================================================================
    // Backlog sprint 7 — interoperability (Backlog #12). Additive: rich
    // HTML paste. Copy/cut keep using `GetSelectionAsClipboard`, whose
    // `ClipboardPayload` reply now also carries the `html` + `docx_fragment`.
    // ===================================================================
    /// Paste HTML at the caret, replacing any non-empty selection. The engine
    /// parses the markup into styled paragraphs (`engine::html`) and splices
    /// them in — the rich counterpart of [`Command::PastePlain`].
    PasteHtml {
        html: String,
    },

    // ===================================================================
    // Phase 5 PR 3 — table mutation commands. `at` / `table_path` are
    // `BlockPath`s; the engine resolves them through `Vector<Block>`
    // (PR 3 supports top-level blocks only; nested-cell paths land in
    // Phase 5b). Every command flips `Table.dirty = true` so the writer
    // regenerates the table on save.
    // ===================================================================
    InsertTable {
        at: BlockPath,
        rows: u32,
        cols: u32,
    },
    DeleteTable {
        path: BlockPath,
    },
    /// Insert a row at the given `row` index, on the named `side`.
    /// Sprint 2 (UI Edition) hotfix — replaces the previous
    /// `after_row: u32` shape that could not express "insert before
    /// row 0" without `0u32 - 1` underflow on the wire.
    InsertRow {
        table_path: BlockPath,
        row: u32,
        side: InsertSide,
    },
    DeleteRow {
        table_path: BlockPath,
        row: u32,
    },
    /// Insert a column at the given `col` index, on the named `side`.
    /// See [`Command::InsertRow`] for the same hotfix rationale.
    InsertColumn {
        table_path: BlockPath,
        col: u32,
        side: InsertSide,
    },
    DeleteColumn {
        table_path: BlockPath,
        col: u32,
    },
    MergeCells {
        table_path: BlockPath,
        from_row: u32,
        from_col: u32,
        to_row: u32,
        to_col: u32,
    },
    SplitCell {
        table_path: BlockPath,
        row: u32,
        col: u32,
    },
    SetCellShading {
        table_path: BlockPath,
        row: u32,
        col: u32,
        color: Option<Color>,
    },
    /// Phase 5 PR 3b — set the per-edge border strokes of one cell. Any
    /// unset edge clears that edge; the cell-level enum has no
    /// `inside_h` / `inside_v` (those apply only at table level).
    SetCellBorders {
        table_path: BlockPath,
        row: u32,
        col: u32,
        borders: BridgeCellBorders,
    },
    /// Issue #79 — patch table-level properties of the table at
    /// `table_path` (a top-level table, like every table command). Every
    /// `None` field of the patch leaves that property untouched. Flips
    /// the table dirty (one undo step) and replies
    /// `Event::SelectionChanged` — its `cell_properties.table_bidi_visual`
    /// reflects the new state.
    SetTableProperties {
        table_path: BlockPath,
        patch: TablePropertiesPatch,
    },
    /// Sprint 2 (UI Edition) — set the multi-column layout of the
    /// section containing `at`. `count == 1` collapses to single
    /// column (gutter ignored); `count >= 2` enables snake-flow
    /// pagination at the given `gutter_pt`. The engine writes
    /// `Section.columns` and the paginator picks the change up on
    /// the next reflow.
    SetColumns {
        at: LogicalPos,
        count: u8,
        gutter_pt: f32,
    },
    /// Word's `Ctrl+Enter` — insert a manual page break (U+000C FORM
    /// FEED, saved as `<w:br w:type="page"/>`) at the caret, replacing
    /// a non-empty selection like typed text; with a collapsed (or no)
    /// selection the break lands at `at`. Issue #75: it no longer flips
    /// `ParaProperties.page_break_before` (the paragraph-format
    /// property, which the paginator honours on its own). Rejected with
    /// `Event::Error` inside a table cell and in header/footer stories.
    InsertPageBreak {
        at: LogicalPos,
    },
    /// Phase 3 (#40) — insert a REAL section break at `at`. The
    /// paragraph splits at the caret; the text before it becomes the
    /// tail of a new section carrying a copy of the covering section's
    /// full `<w:sectPr>` payload, and the following section begins per
    /// `kind` (Word semantics: `<w:type>` describes how the section it
    /// opens starts relative to the previous one). Distinct from
    /// `InsertPageBreak`, which inserts a manual page break (FORM FEED)
    /// inside the current section. Rejected with `Event::Error` when `at`
    /// sits inside a table cell (Word-parity there is deferred).
    InsertSectionBreak {
        at: LogicalPos,
        kind: SectionBreakKind,
    },
    /// Phase 3 (#39) / issue #70 — enter header/footer editing for the
    /// band the double-clicked page `page` (0-based) DISPLAYS: the
    /// page's role (Default / First / Even) picks which slot is
    /// edited. The engine stashes the body selection and resolves the
    /// (section, area, role) reference through §17.10.3 inheritance:
    /// a resolved part (own or inherited) is edited IN PLACE — edits
    /// appear on every page that resolves to it, Word's linked-editing
    /// semantics; only when NO section in the chain has a part does
    /// the engine materialize an empty one on the edited section.
    /// [`Command::SetHeaderFooterLink`] forks/relinks explicitly.
    /// `SelectionChanged.editing_story` reports the active story
    /// (rid + role + linked state).
    EnterHeaderFooter {
        page: u32,
        area: HeaderFooterArea,
    },
    /// Phase 3 (#39) — leave header/footer editing and restore the
    /// stashed body selection (clamped if the body changed under it).
    ExitHeaderFooter,
    /// Issue #70 — Word's "Link to Previous" toggle for the ACTIVE
    /// story's (section, area, role) slot. `linked: false` (unlink)
    /// copies the currently-resolved part into a fresh private part
    /// owned by the story's section; `linked: true` (relink) clears
    /// the section's own slot so it inherits again (Event::Error on
    /// the first section — nothing precedes it). Story-scoped:
    /// rejected outside header/footer editing.
    SetHeaderFooterLink {
        linked: bool,
    },
    /// Issue #74 — toggle `<w:titlePg/>` ("different first page") on
    /// the covering section: the active story's section when a story
    /// is open, else the caret's. The first page of that section then
    /// displays the First-role band (blank until one is authored).
    SetTitlePage {
        enabled: bool,
    },
    /// Issue #74 — document-wide `<w:evenAndOddHeaders/>` toggle
    /// (settings.xml). Even-NUMBERED pages then display Even-role
    /// bands (blank until authored).
    SetEvenOddHeaders {
        enabled: bool,
    },
    /// Issue #43 — author a dynamic field at `at` (body paragraph or
    /// header/footer story; rejected with `Event::Error` inside table
    /// cells — the cell reader cannot round-trip fields yet). The
    /// engine splices placeholder text + the field overlay; layout
    /// resolves the live value per page.
    InsertField {
        at: LogicalPos,
        kind: FieldKind,
    },
    /// Issue #80 — author a footnote referenced at `at` (a body
    /// paragraph; rejected with `Event::Error` inside table cells and
    /// while a story is being edited). The engine splices the reference
    /// anchor, creates the note story (one paragraph opening with the
    /// self-mark), renumbers every later marker, and ENTERS the new
    /// note so typing lands in it — `SelectionChanged.editing_story`
    /// reports `area: Footnote`. `ExitHeaderFooter` returns to the body
    /// at the reference.
    InsertFootnote {
        at: LogicalPos,
    },
    /// Issue #80 — endnote twin of [`Command::InsertFootnote`]; the note
    /// collects at section / document end per `<w:endnotePr><w:pos>`.
    InsertEndnote {
        at: LogicalPos,
    },
    /// Issue #83 — insert a floating text box anchored at `at` (a body
    /// paragraph; rejected with `Event::Error` inside table cells and
    /// while a story is being edited). `width_emu` × `height_emu` is the
    /// shape extent (914400 EMU per inch). The box gets Word's "Draw Text
    /// Box" defaults — white fill, 0.75 pt black outline, square wrap,
    /// column / paragraph-relative at zero offset — and the engine ENTERS
    /// its story so typing lands in it: `SelectionChanged.editing_story`
    /// reports `area: TextBox`. A click outside the box (or
    /// `ExitHeaderFooter`) returns to the body; a click inside any text
    /// box enters its story.
    InsertTextBox {
        at: LogicalPos,
        width_emu: i64,
        height_emu: i64,
    },
    /// Issue #43 — install the render-time date DATE fields resolve
    /// against. The worker injects today's date right after INIT (the
    /// engine core never reads a wall clock — determinism for tests
    /// and byte-stable exports). Month/day are 1-based. Issue #77 —
    /// the optional clock component (`hour` 0–23 + `minute`) is what
    /// TIME fields resolve against; both absent keeps TIME cached.
    SetRenderDate {
        year: i32,
        month: u32,
        day: u32,
        #[serde(default)]
        #[tsify(optional)]
        hour: Option<u32>,
        #[serde(default)]
        #[tsify(optional)]
        minute: Option<u32>,
    },
    /// Issue #77 — F9: re-resolve every field in the document (body +
    /// every header/footer part) and stamp the live values into the
    /// model as ONE undo step. Page-dependent kinds read the current
    /// full pagination; the rest read the render environment.
    UpdateFields,
    /// Issue #77 — Alt+F9: toggle the field-code view. While enabled
    /// every field PAINTS its `{ INSTRUCTION }` code in place of the
    /// result (body + stories). Logical positions (`LogicalPos` in
    /// every command and in `SelectionChanged.range`) stay SOURCE
    /// positions — the engine maps them onto the displayed code text
    /// for pixel geometry only, which the atomic-field invariant makes
    /// exact (a caret is never strictly inside a field in either view).
    /// A pure display state — never persisted, never saved.
    SetFieldCodeView {
        enabled: bool,
    },
    /// Issue #77 — replace the instruction (field code) of the field
    /// the caret at `at` addresses (strictly inside, ending at, or
    /// starting at `at`; see `SelectionChanged.field_at_caret`). The
    /// cached result stays until the next update (Word parity); the
    /// selection is preserved. Rejected with `Event::Error` when `at`
    /// addresses no field or `instruction` is blank. Body or story.
    SetFieldInstruction {
        at: LogicalPos,
        instruction: String,
    },
    /// Issue #81 — insert a Table of Contents at `at` (a top-level body
    /// paragraph outside tables and other TOCs): before the paragraph
    /// when the caret is at its start, after it at its end, else the
    /// paragraph splits. The result is generated immediately from the
    /// document's headings with live page numbers; `UpdateFields` (F9)
    /// regenerates it. Rejected with `Event::Error` inside a table, a
    /// header/footer/note story, or an existing TOC.
    InsertToc {
        at: LogicalPos,
        #[serde(default)]
        switches: TocSwitches,
    },
    /// Sprint 2 (UI Edition) — set `<w:pPr><w:pBdr>` on every
    /// paragraph the range spans. Mirrors `SetCellBorders` over the
    /// paragraph-border model that shipped in Sprint 5. Pass an
    /// all-edges-`None` `BridgeCellBorders` to clear the borders.
    SetParagraphBorders {
        range: LogicalRange,
        borders: BridgeCellBorders,
    },
    /// Sprint 4 (UI Edition) — set `<w:pgMar>` on the section
    /// containing `at`. All four edges in points (1 pt = 1/72 inch).
    SetPageMargins {
        at: LogicalPos,
        top_pt: f32,
        right_pt: f32,
        bottom_pt: f32,
        left_pt: f32,
    },
    /// Sprint 4 (UI Edition) — flip page orientation on the section
    /// containing `at`. Swaps `<w:pgSz>` width/height; margins are
    /// preserved per-edge (no rotation — Word's behavior).
    SetPageOrientation {
        at: LogicalPos,
        orientation: PageOrientation,
    },
    /// Sprint 5 (UI Edition) — toggle list membership on every
    /// paragraph the range spans. `Off` clears `list_item`;
    /// `Bullet` / `Number` run the idempotent `numbering.xml`
    /// synthesis (`DocumentTree::toggle_list_on_range`, Sprint 13
    /// #12) so repeated toggles reuse the existing AbstractNum /
    /// Num templates.
    ToggleList {
        range: LogicalRange,
        kind: ListKind,
    },
    /// Issue #42 — demote (`delta: 1`) or promote (`delta: -1`) the
    /// outline level (`<w:numPr><w:ilvl>`) of every list paragraph the
    /// range spans. No-op on paragraphs with no `list_item`. Clamped to
    /// Word's nine stock outline levels (`0..=8`) engine-side.
    ChangeListLevel {
        range: LogicalRange,
        delta: i8,
    },
    /// Sprint 6 (UI Edition) — set `<w:pPr><w:ind>` on every
    /// paragraph the range spans. All values in points. Negative
    /// `first_line_pt` populates `<w:hanging>`; non-negative
    /// populates `<w:firstLine>` (Word's mutually-exclusive
    /// semantics — engine handles the swap).
    SetParagraphIndent {
        range: LogicalRange,
        start_pt: f32,
        end_pt: f32,
        first_line_pt: f32,
    },
    /// Sprint 6 (UI Edition) — set line spacing as a multiplier of
    /// single-line height (1.0 = single, 1.5 = one-and-a-half,
    /// 2.0 = double). Multiplier `<= 0.0` clears the line-height
    /// override (paragraph inherits the renderer default).
    SetLineSpacing {
        range: LogicalRange,
        multiplier: f32,
    },
    /// Sprint 6 (UI Edition) — set `<w:pPr><w:shd w:fill>` paragraph
    /// background colour. `None` clears the shading.
    SetParagraphShading {
        range: LogicalRange,
        color: Option<Color>,
    },
    /// Sprint 7 (UI Edition) — toggle the engine's tracked-changes
    /// RECORDING flag. Implemented in Sprint 14 (#14): while enabled,
    /// every text-mutation handler gates its work into a tracked
    /// revision (`<w:ins>`/`<w:del>`), and the live flag rides back
    /// on `Event::SelectionChanged.is_tracking_changes`.
    ToggleTrackChanges {
        enabled: bool,
    },
    /// Sprint 7 (UI Edition) — accept a tracked-change revision
    /// addressed by top-level `block` index + byte `start` + byte
    /// `end`. Insert+Accept keeps text; Delete+Accept removes it.
    AcceptRevision {
        block: u32,
        start: u32,
        end: u32,
    },
    /// Sprint 7 (UI Edition) — reject a tracked-change revision.
    /// Insert+Reject removes text; Delete+Reject keeps it.
    RejectRevision {
        block: u32,
        start: u32,
        end: u32,
    },
    /// Sprint 7 (UI Edition) — insert a new `<w:comment>` anchored
    /// to `range`. Engine assigns a fresh sequential `w:id`.
    InsertComment {
        range: LogicalRange,
        text: String,
        author: String,
    },
    /// Sprint 7 (UI Edition) — delete a comment by id.
    DeleteComment {
        id: u32,
    },
    /// Sprint 11 (#13) — replace `<w:pPr><w:tabs>` on every paragraph
    /// the range spans. Empty `stops` clears the paragraph's custom
    /// tab grid (falls back to the default 0.5-inch grid). The Ruler
    /// dispatches this ONCE on `pointerup` after a drag — never
    /// continuously on `pointermove` — so one user action produces
    /// exactly one undo entry.
    SetTabStops {
        range: LogicalRange,
        stops: Vec<BridgeTabStop>,
    },
    /// Sprint 14 (#14) — UI calls this when the user enters their
    /// review identity; the engine stamps every new tracked revision
    /// with these values. Default `author = "You"` + ISO 8601
    /// `date = ""` (engine fills `Date.now()` at stamp time) when
    /// unset. `author` is the OOXML `w:author` attribute.
    SetReviewIdentity {
        author: String,
        date: String,
    },
    /// Sprint 12 (#11) — apply (or detach when `None`) a paragraph
    /// style on every paragraph the range spans. The engine sets
    /// `Paragraph.style_id` and recomputes the resolved `props` view
    /// (`style_cascade(style_id) ∪ direct_overrides`); pre-existing
    /// `direct_overrides` are preserved verbatim. Character styles
    /// (`<w:rStyle>`) are out of scope.
    ApplyStyle {
        range: LogicalRange,
        style_id: Option<String>,
    },
    /// Sprint 7 (UI Edition) — toggle the `resolved` flag on a
    /// comment. Round-trips through `word/commentsExtended.xml`
    /// (Sprint 9 — `<w15:commentEx w15:done>`).
    ResolveComment {
        id: u32,
        resolved: bool,
    },
    /// Issue #27 — append a threaded reply to the comment with
    /// `parent_id`. The reply is anchored to the parent's range (Word
    /// anchors replies on the same span) and the engine mints the new
    /// sequential `w:id`. Unknown `parent_id` → `Event::Error`.
    /// Threading round-trips through `word/commentsExtended.xml`
    /// (`<w15:commentEx w15:paraIdParent>`).
    ReplyToComment {
        parent_id: u32,
        text: String,
        author: String,
    },

    /// Issue #21 — mutate an existing style DEFINITION and re-cascade
    /// every dependent paragraph (assignment stays `ApplyStyle`).
    /// Unknown `style_id` replies `Event::Error`.
    ModifyStyle {
        style_id: String,
        properties: BridgeStyleProperties,
    },
}

/// Issue #79 — additive patch for [`Command::SetTableProperties`]. Each
/// field is optional; `None` = leave as is. Grows one optional field per
/// newly-authorable `<w:tblPr>` property.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[serde(default)]
pub struct TablePropertiesPatch {
    /// `<w:bidiVisual>` — right-to-left visual column order (grid
    /// column 1 rightmost). Purely visual: logical cell order is kept.
    pub bidi_visual: Option<bool>,
}

/// Wire shape for `engine::CellBorders` — per-edge strokes for one
/// cell. Phase 5 PR 3b: cell-level only; the `inside_*` edges apply
/// only at table level and have no wire path yet.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct BridgeCellBorders {
    pub top: Option<BridgeBorderStroke>,
    pub left: Option<BridgeBorderStroke>,
    pub bottom: Option<BridgeBorderStroke>,
    pub right: Option<BridgeBorderStroke>,
}

/// Wire shape for `engine::BorderStroke` — one edge. `size_eighth_pt`
/// is `<w:sz>` (eighths of a point, OOXML unit). `color: None` →
/// inherit from style.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct BridgeBorderStroke {
    pub style: BridgeBorderStyle,
    pub size_eighth_pt: u16,
    pub color: Option<Color>,
}

/// Sprint 5 (UI Edition) — list kind for [`Command::ToggleList`].
/// `Off` clears the paragraph's `list_item`; `Bullet` / `Number` run
/// the numbering synthesis path (`DocumentTree::toggle_list_on_range`,
/// Sprint 13 #12) that mints/reuses an idempotent `<w:abstractNum>` /
/// `<w:num>` template.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum ListKind {
    #[default]
    Off,
    Bullet,
    Number,
}

/// Sprint 4 (UI Edition) — page orientation enum for
/// [`Command::SetPageOrientation`]. Portrait keeps the section's
/// page dimensions as-is when width ≤ height; Landscape forces
/// width > height. The handler swaps when needed and is a no-op
/// when the page already matches.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum PageOrientation {
    #[default]
    Portrait,
    Landscape,
}

/// Phase 3 (#40) / issue #74 — the authorable section-break kinds for
/// [`Command::InsertSectionBreak`], the full wire mirror of engine
/// `SectionType`. `EvenPage` / `OddPage` begin the new section on the
/// next even/odd-NUMBERED page — the paginator emits one blank filler
/// page (which PAGE fields count) when the incoming page's parity
/// mismatches.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum SectionBreakKind {
    #[default]
    NextPage,
    Continuous,
    EvenPage,
    OddPage,
}

/// Phase 3 (#39) — which margin band a header/footer command targets.
/// Issue #80 widened it to every editable STORY: `Footnote` / `Endnote`
/// name a note story on `SelectionChanged.editing_story` (entered by
/// clicking into a note band or by `InsertFootnote` / `InsertEndnote`;
/// left with `ExitHeaderFooter`). `EnterHeaderFooter` rejects the two
/// note areas with `Event::Error` — notes are entered by content, not
/// by page zone. Issue #83 — `TextBox` names a text-box story (entered
/// by clicking into the box or by `InsertTextBox`); `EnterHeaderFooter`
/// rejects it the same way.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum HeaderFooterArea {
    #[default]
    Header,
    Footer,
    Footnote,
    Endnote,
    TextBox,
}

/// Issue #43 — the field kinds [`Command::InsertField`] authors.
/// `Page` renders the page's formatted number, `NumPages` the
/// document's total page count (forces full pagination on documents
/// that carry one), `Date` today's date per the shell-injected render
/// date (`M/d/yyyy`, Word's en default). Issue #77 — `Time` the
/// shell-injected clock (`h:mm am/pm`), `FileName` the document name
/// the shell opened / will save (`OpenDocument.name`), `Author` the
/// `docProps/core.xml` creator.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum FieldKind {
    #[default]
    Page,
    NumPages,
    Date,
    Time,
    FileName,
    Author,
}

/// Issue #81 — the `TOC` field switches [`Command::InsertToc`] authors.
/// Defaults are Word's Insert › Table of Contents: `TOC \o "1-3" \h \z \u`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[serde(default)]
pub struct TocSwitches {
    /// `\o "min-max"` — heading outline levels collected (1-based,
    /// clamped to 1..=9). `outline_max == 0` omits `\o`.
    pub outline_min: u8,
    pub outline_max: u8,
    /// `\h` — entries hyperlink to their headings.
    pub hyperlinks: bool,
    /// `\z` — hide tab leaders / numbers in Web layout.
    pub hide_in_web: bool,
    /// `\u` — also collect paragraphs by their direct outline level.
    pub use_outline_levels: bool,
    /// `false` emits `\n` (no page numbers).
    pub page_numbers: bool,
}

impl Default for TocSwitches {
    fn default() -> Self {
        Self {
            outline_min: 1,
            outline_max: 3,
            hyperlinks: true,
            hide_in_web: true,
            use_outline_levels: true,
            page_numbers: true,
        }
    }
}

/// Sprint 11 (#13) — wire shape for one `<w:pPr><w:tabs><w:tab>`
/// entry. `position_pt` is layout pt (1/72 in) at scale=1 — the
/// engine model unit. Kind mirrors `engine::TabKind`.
///
/// Issue #145 — `leader` is additive: `None` (omitted / `undefined`
/// from TS — see the `tsify-next` `Option<T>` convention) means
/// "leave this stop's leader as it is," so a caller that only edits
/// position (the Ruler drag path) can never silently clear a TOC
/// entry's dot leader. An explicit `Some(BridgeTabLeader::None)` is
/// the deliberate clear. The read-back (`EditorState.tab_stops`)
/// always sends a concrete `Some` — this optionality only matters on
/// the way in, through `Command::SetTabStops`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct BridgeTabStop {
    pub position_pt: f32,
    pub kind: BridgeTabKind,
    pub leader: Option<BridgeTabLeader>,
}

/// Sprint 11 (#13) — wire shape for `engine::TabKind`. `Decimal` is
/// round-tripped but renders as `Left` today (proper measure-then-
/// place pass deferred — see BACKLOG / out-of-scope §11).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum BridgeTabKind {
    #[default]
    Left,
    Center,
    Right,
    Decimal,
    /// `<w:clear>` — explicit "no tab at this position", used to
    /// defeat an inherited tab stop from the style cascade.
    Clear,
}

/// Issue #145 — wire shape for `engine::TabLeader` (`<w:tab w:leader>`
/// / `ST_TabTlc`). Mirrors the engine enum one-for-one so the Ruler can
/// round-trip a TOC entry's dot / hyphen / underscore / heavy /
/// middle-dot leader without lossy translation.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum BridgeTabLeader {
    #[default]
    None,
    Dot,
    Hyphen,
    Underscore,
    Heavy,
    MiddleDot,
}

/// Sprint 2 (UI Edition) hotfix — anchor side for `InsertRow` /
/// `InsertColumn`. The previous `after_row: u32` / `after_col: u32`
/// shape could not represent "insert before row/column 0" because the
/// wire type is unsigned; callers were forced into `0 - 1` math that
/// would either underflow in Rust or be rejected by serde when
/// arriving as a negative `Number`. Tsify renders this enum as
/// `"Before" | "After"`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum InsertSide {
    #[default]
    After,
    Before,
}

/// Wire shape for `engine::BorderStyle`. PR 3b ships the common
/// subset; round-tripped exotic styles stay engine-side.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum BridgeBorderStyle {
    #[default]
    Single,
    Double,
    Dotted,
    Dashed,
    None,
}

/// Browser/runtime capabilities advertised to the engine at `Init`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct ClientCapabilities {
    pub shared_array_buffer: bool,
    pub offscreen_canvas: bool,
    pub simd: bool,
    pub device_pixel_ratio: f32,
}

/// Target PDF/A (or PDF/X) conformance level for `ExportPdf`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum PdfConformance {
    A1b,
    A2u,
    X3,
}

/// A sparse patch of inline text attributes — `None` fields are left
/// Issue #21 — patch for a style's `<w:pPr>` half. A pragmatic subset
/// of the paragraph surface (alignment / direction / line spacing /
/// indents / shading); borders, tab stops and numbering stay
/// per-paragraph concerns. `None` preserves the current value.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub struct BridgeParaPropertiesPatch {
    pub alignment: Option<Alignment>,
    pub direction: Option<Direction>,
    /// Word "Multiple" line spacing; `1.0` = single.
    pub line_spacing_multiplier: Option<f32>,
    pub indent_start_pt: Option<f32>,
    pub indent_end_pt: Option<f32>,
    /// Positive = first-line indent, negative = hanging.
    pub first_line_pt: Option<f32>,
    pub shading: Option<Color>,
}

/// Issue #21 — patch for a style's `<w:rPr>` half (engine `SpanStyle`
/// mirror). `None` preserves.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub struct BridgeSpanStylePatch {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<UnderlineStyle>,
    pub strike: Option<bool>,
    pub font_size: Option<f32>,
    pub color: Option<Color>,
    pub bg_color: Option<Color>,
    pub font_family: Option<String>,
    pub caps: Option<bool>,
    pub small_caps: Option<bool>,
}

/// Issue #21 — the `ModifyStyle` payload. `based_on` re-parents when
/// `Some`; `clear_based_on: Some(true)` detaches the parent (two fields
/// because three states matter and `Option<Option<T>>` has no stable
/// JSON encoding through tsify); both absent leaves the parent alone.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub struct BridgeStyleProperties {
    pub para_props: Option<BridgeParaPropertiesPatch>,
    pub run_props: Option<BridgeSpanStylePatch>,
    pub based_on: Option<String>,
    pub clear_based_on: Option<bool>,
    pub display_name: Option<String>,
}

/// unchanged. The resolved counterpart is [`crate::TextAttrs`].
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct TextAttrsPatch {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<UnderlineStyle>,
    pub strike: Option<bool>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub color: Option<Color>,
    pub bg_color: Option<Color>,
    pub script: Option<VerticalScript>,
    pub language: Option<String>,
    /// `<w:caps/>` — display every character as uppercase. Closes the
    /// audit gap A.H3 noted in `engine-wasm::apply_formatting` — the
    /// engine `SpanStyle.caps` field has existed since Phase 3 but the
    /// interactive bridge surface only just routes a toggle.
    pub caps: Option<bool>,
    /// `<w:smallCaps/>` — same provenance as [`Self::caps`].
    pub small_caps: Option<bool>,
}

/// Stable identifier for a paragraph in the document model.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct ParagraphId {
    pub id: u32,
}

/// How an inserted image is sized relative to the content area.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum ImageFit {
    Original,
    FitWidth,
    FitPage,
}

/// An encoded image payload plus its intrinsic dimensions.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct ImageBlob {
    #[serde(with = "serde_bytes")]
    #[tsify(type = "Uint8Array")]
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// Keyboard modifier accompanying a selection-extend gesture.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum SelectionModifier {
    None,
    Shift,
    Alt,
    ShiftAlt,
}

/// Cardinal direction for `Command::MoveCaret` (Backlog #14 — core nav).
/// `Left`/`Right` step one Unicode char in logical order (paragraph-local,
/// hopping across paragraph boundaries at the ends). `Up`/`Down` walk to the
/// adjacent line and snap to the slot nearest the caret's stored ideal-x.
/// `NextCell` / `PrevCell` are the Phase 5 PR 4 cell-traversal motions
/// — Tab / Shift+Tab inside a table. Outside a table they no-op.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum MoveDirection {
    /// Visual arrow keys. The engine maps these to logical byte motion
    /// using the caret's host paragraph base direction (UAX #9, see
    /// `UX_BEHAVIOR_SPEC.md` §III):
    /// * **LTR paragraph** — `Left` decrements offset, `Right` increments.
    /// * **RTL paragraph** — `Left` increments offset, `Right` decrements.
    ///
    /// The motion unit is a UAX #29 grapheme cluster, not a Unicode
    /// scalar — combining marks (Arabic harakat, Devanagari conjuncts,
    /// emoji ZWJ sequences) traverse as one user-perceived character.
    Up,
    Down,
    Left,
    Right,
    NextCell,
    PrevCell,
    /// Ctrl/Cmd + ArrowLeft (visual). Jump caret to the start of the
    /// previous **word-like** UAX #29 segment in the caret's host
    /// paragraph, after applying the same RTL flip as `Left`. Crosses
    /// paragraph boundaries by jumping to the end of the previous
    /// paragraph.
    WordLeft,
    /// Ctrl/Cmd + ArrowRight (visual). Jump caret to the start of the
    /// next word-like UAX #29 segment, after the RTL flip. Crosses
    /// paragraph boundaries by jumping to offset 0 of the next.
    WordRight,
    /// `Home` — caret to the start of the current visual line. Snaps to
    /// the LineGeom whose `start_byte..end_byte` brackets the caret and
    /// lands on `start_byte`.
    LineHome,
    /// `End` — caret to the end of the current visual line.
    LineEnd,
    /// `Ctrl/Cmd + Home` — caret to the very first paragraph at offset 0.
    DocHome,
    /// `Ctrl/Cmd + End` — caret to the last paragraph at `text.len`.
    DocEnd,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #57 — `GetSelectionAsClipboard` grew a struct body; the
    /// pre-#57 wire shape (tag only) must still decode, as `None`
    /// (⇒ `.docx` fragment included).
    #[test]
    fn get_selection_as_clipboard_accepts_the_legacy_tag_only_shape() {
        let legacy: Command =
            serde_json::from_value(serde_json::json!({ "type": "GET_SELECTION_AS_CLIPBOARD" }))
                .expect("legacy shape decodes");
        assert!(matches!(
            legacy,
            Command::GetSelectionAsClipboard { include_docx: None }
        ));
        let prefetch: Command = serde_json::from_value(serde_json::json!({
            "type": "GET_SELECTION_AS_CLIPBOARD",
            "include_docx": false,
        }))
        .expect("prefetch shape decodes");
        assert!(matches!(
            prefetch,
            Command::GetSelectionAsClipboard {
                include_docx: Some(false)
            }
        ));
    }
}
