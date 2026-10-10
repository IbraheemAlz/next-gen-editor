//! `Event` — messages the engine emits back to the TypeScript client.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::command::{
    BridgeCellBorders, BridgeTabStop, FontSlot, HeaderFooterArea, PageOrientation,
};
use crate::common::{
    Alignment, Color, Direction, DocFormat, ImageRect, LogicalPos, LogicalRange, Rect,
    RendererDowngrade, Script, SelectionKind, TextAttrs,
};

/// `serde(default)` for the user-zoom fields: an absent value means the
/// engine's cold default (100 %), never `0.0`.
fn default_zoom() -> f32 {
    1.0
}

/// `serde(default)` for `SelectionChanged.caret_font_slot`: a producer that
/// predates issue #423 reported the Latin read-back.
fn default_caret_font_slot() -> FontSlot {
    FontSlot::Latin
}

/// An event emitted by the engine. Serialized internally-tagged
/// (`{ "type": "PAINTED", ... }`).
///
/// `SelectionChanged` carries the whole toolbar read-back surface and
/// dwarfs the other variants; events are transient, one-at-a-time RPC
/// payloads, so the size skew is irrelevant (same stance as
/// `engine::Block`).
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    // ===================================================================
    // Phase 1 PoC events.
    // TODO: Deprecate in Phase 3 — superseded by the §5 schema below. Kept
    // now so the visual-diff goldens and test harnesses stay 100% green.
    // ===================================================================
    Pong,
    Log {
        message: String,
    },
    FontLoaded {
        id: String,
        metrics: FontMetrics,
    },
    GlyphPainted {
        font_id: String,
        ch: String,
        advance_width: f32,
        ascent: f32,
        glyph_width: u32,
        glyph_height: u32,
    },
    ShapedAndPainted {
        font_id: String,
        text: String,
        direction: String,
        glyph_count: u32,
        total_advance: f32,
        ascent: f32,
        glyph_ids: Vec<u32>,
    },
    PageRendered {
        page_width: f32,
        page_height: f32,
        line_count: u32,
        glyph_count: u32,
    },
    TextInserted {
        paragraph_count: u32,
        can_undo: bool,
        can_redo: bool,
        undo_depth: u32,
    },
    UndoStateChanged {
        can_undo: bool,
        can_redo: bool,
        undo_depth: u32,
    },
    DocumentLoaded {
        paragraph_count: u32,
        /// Issue #406 — the reader's non-fatal diagnostics for the package
        /// just opened (`format_docx::DocxWarning`, coalesced: identical
        /// warnings ride one entry with a `count`). Non-empty means the
        /// file opened DEGRADED — a clamped page margin, a measure that
        /// was ignored, a part read through namespace normalisation (which
        /// costs that part's byte preservation), a field closed early, … —
        /// and the shell must say so (`@nge/core` `openWarnings()`).
        /// Additive: skipped on the wire when empty (every clean open, the
        /// `.txt` / `.html` opens and the Phase-1 `LoadDocx` harness keep
        /// the pre-#406 `{ type, paragraph_count }` shape), and a pre-#406
        /// payload decodes with no warnings.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[tsify(optional)]
        warnings: Vec<ReadWarning>,
    },
    DocumentSaved {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        size: u32,
    },
    Error {
        message: String,
        /// Issue #348 — a machine-readable class for errors the shell
        /// presents specifically (a refused document open); `None` for
        /// every other error. Additive: skipped on the wire when `None`,
        /// so the pre-#348 `{ type: "ERROR", message }` shape is unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        kind: Option<ErrorKind>,
    },

    // ===================================================================
    // Phase 2 schema — PHASE_2_BRIDGE_MEMORY.md §5.
    // ===================================================================
    /* Lifecycle */
    Ready {
        version: String,
        capabilities: EngineCapabilities,
    },
    /// Reply to `Command::Recover` (issue #85).
    Recovered {
        /// Replay-log commands applied on top of the base snapshot.
        applied_commands: u32,
        /// `true` when a base snapshot was decoded and restored; `false`
        /// when none was supplied or it was unreadable (the engine then
        /// recovered onto a fresh document from the tail alone).
        snapshot_restored: bool,
        /// Issue #66 — the renderer this engine instance actually paints
        /// with (`"vello"` / `"canvas2d"`), reported by the engine itself
        /// so the shell's `__renderer` never drifts from the truth after a
        /// post-trap respawn.
        renderer: String,
        /// Issue #97 — the user zoom fraction the recovered engine renders
        /// at (restored config + any replayed `SetZoom`; `1.0` when the
        /// engine came back cold). The shell re-syncs its zoom controls
        /// from this instead of trusting its pre-trap UI state.
        #[serde(default = "default_zoom")]
        zoom: f32,
        /// Issue #97 — the recovered boot device scale
        /// (`devicePixelRatio × 4/3`, before zoom); `None` when the engine
        /// came back cold with no layout config (the shell re-seeds it).
        #[serde(default)]
        #[tsify(optional)]
        device_scale: Option<f32>,
        /// Issue #99 — set when this generation was forced off its probed
        /// GPU backend after a crash loop (echo of
        /// `Command::Recover.renderer_downgrade`). `None` on an ordinary
        /// recovery.
        #[serde(default)]
        #[tsify(optional)]
        renderer_downgrade: Option<RendererDowngrade>,
        /// Issue #268 — the restored base snapshot named a retained source
        /// package (#134 / #212) that could not be re-attached (not
        /// supplied, mismatched or unreadable), and the replayed tail did
        /// not replace the document: the recovered session saves through
        /// the minimal-package writer, dropping the sibling parts. The
        /// shell uses it to prefer an older base that still has its
        /// package, and reports it when none does. `false` otherwise.
        #[serde(default)]
        package_lost: bool,
    },
    /// Reply to `Command::Snapshot` (issue #85): the versioned snapshot
    /// envelope (`engine::snapshot`, magic + format version + payload).
    Snapshot {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        /// Echo of `Command::Snapshot.seq` (0 when the caller passed none).
        seq: u64,
        /// `engine::snapshot::FORMAT_VERSION` the bytes were written with.
        format_version: u8,
        /// Issue #212 — set when the snapshot was taken with
        /// `Command::Snapshot.detach_package` and the session holds a
        /// retained source package: the content hash `bytes` records in
        /// its place (the key the caller stores the package under).
        #[serde(default)]
        #[tsify(optional)]
        package_hash: Option<String>,
        /// Issue #212 — the detached package itself (an `engine::snapshot`
        /// envelope of `engine::SourcePackage`), shipped only when the
        /// caller's `known_package_hash` did not match `package_hash`.
        #[serde(default, with = "serde_bytes")]
        #[tsify(type = "Uint8Array", optional)]
        package: Option<Vec<u8>>,
    },

    /* Document */
    DocumentClosed,
    PdfExported {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        pages: u32,
    },

    /* Rendering */
    Painted {
        dirty: Rect,
        version: u64,
        paint_ms: f32,
        /// Phase 6b — total height in layout pixels of the pages **already
        /// laid out**, including inter-page gaps. The TS shell sizes its
        /// canvas backing store to `estimated_document_height` (≥ this),
        /// but uses this value for `last_emitted_y` so a hit-test below
        /// it triggers a layout-extend request. `0.0` for harness paths
        /// that bypass pagination (Phase-1 `?test=` cases).
        document_height: f32,
        /// Phase 6b — number of paginated pages the renderer drew so far.
        page_count: u32,
        /// Audit gap C.H1 — pagination state on this paint. `true` when
        /// every body block was consumed; `false` when the paginator
        /// stopped early under a `viewport_budget` so the scrollbar
        /// reflects the *estimated* document height, not the final one.
        /// The TS shell uses this to know whether more pages may
        /// materialize on scroll / `ExpandLayout`.
        is_full_layout: bool,
        /// Audit gap C.H1 — best-guess total document height in layout
        /// pixels at the current scale, including the inter-page gaps
        /// and an `AVG_PARAGRAPH_HEIGHT_PT` × `remaining_blocks` reserve
        /// for blocks that have not yet been laid out. `== document_height`
        /// once `is_full_layout` is `true`. Drives the scrollbar so it
        /// never jumps as background layout fills in the tail.
        estimated_document_height: f32,
        /// Issue #26 — absolute top offset of every laid-out page in
        /// document device px (page 0 sits at `0.0`; each next top adds
        /// the previous page's real height + the inter-page gap).
        /// Overlays and pointer math consume these instead of assuming
        /// uniform A4-portrait pages, so mixed-orientation sections
        /// stop drifting. Empty for harness paths that bypass
        /// pagination.
        page_tops: Vec<f32>,
        /// Issue #26 — per-page heights in device px, index-aligned
        /// with `page_tops`.
        page_heights: Vec<f32>,
        /// Issue #280 — per-page widths in device px, index-aligned
        /// with `page_tops`. The shell sizes every page card's CSS box
        /// from `page_widths` / `page_heights` (÷ its device-px-per-CSS-px
        /// ratio), so a zoom or a landscape section visibly resizes the
        /// page instead of only densifying a fixed A4 box. Additive;
        /// empty from pre-#280 producers (consumers fall back to A4).
        #[serde(default)]
        page_widths: Vec<f32>,
        /// Issue #44 — count of inline images in the whole document. Lets
        /// the shell skip the `GetImageRects` refresh entirely for the
        /// (common) image-free document instead of paying an extra worker
        /// round-trip + geometry walk on every paint.
        image_count: u32,
        /// Phase 3 (#39) — per-page TOP margin in device px, index-aligned
        /// with `page_tops`. The shell's pointer pipeline gates the
        /// double-click-to-edit-header zone with
        /// `page_top ≤ y < page_top + margin`, exact for mixed-geometry
        /// sections (a caret-section read-back can't answer for an
        /// arbitrary page, and a mid-gesture worker round-trip would
        /// arrive after the click).
        page_margin_tops: Vec<f32>,
        /// Phase 3 (#39) — per-page BOTTOM margin in device px,
        /// index-aligned with `page_tops`; the footer-zone mirror of
        /// `page_margin_tops`.
        page_margin_bottoms: Vec<f32>,
        /// Issue #71 — per-page EFFECTIVE body-content top in device
        /// px, page-local, index-aligned with `page_tops`. Equals the
        /// top margin until a header band intrudes past it (band
        /// overflow pushes body content down); the shell's band-zone
        /// gate uses THESE instead of the raw margins so a tall header
        /// keeps its whole painted band double-clickable.
        page_content_tops: Vec<f32>,
        /// Issue #71 — per-page effective body-content BOTTOM (as a
        /// page-local Y, i.e. `height - effective_bottom_inset`),
        /// index-aligned with `page_tops`; the footer mirror of
        /// `page_content_tops`.
        page_content_bottoms: Vec<f32>,
        /// Issue #87 — every degradation the layout self-defense applied
        /// while producing this paint (empty on a nominal paint). The
        /// pixels are on screen either way; this is how telemetry and QA
        /// learn that the paginator watchdog released a constraint, pinned
        /// a churning block, hit the page cap, or that an incremental band
        /// was demoted to a full reflow. Additive: consumers that ignore
        /// it see the same paint they always did.
        layout_degraded: Vec<LayoutDegraded>,
        /// Issue #194 — the engine's monotonic document-mutation counter
        /// at this paint: bumped once per command that changed the
        /// document (edits, undo/redo, loads), untouched by queries,
        /// selection and view commands. A consumer that sees it move
        /// knows the content changed (the worker broadcasts the
        /// accessibility delta off the same counter). Additive; `0` from
        /// a fresh engine.
        #[serde(default)]
        mutation_seq: u64,
    },

    /* Selection */
    SelectionChanged {
        range: LogicalRange,
        caret: Rect,
        direction: Direction,
        rects: Vec<Rect>,
        attrs_at_caret: TextAttrs,
        /// Effective alignment of the caret's paragraph — its own override, or
        /// the document default. With a pending (sticky) style armed this is
        /// unaffected; `attrs_at_caret` carries the pending overlay instead.
        /// Drives the toolbar's alignment picker (Backlog #9, #11).
        paragraph_alignment: Alignment,
        /// Issue #29 — the caret paragraph's `<w:pStyle>` id, `None`
        /// when detached. Drives the StylesDropdown active indicator so
        /// it reads back truthfully instead of trusting its last click.
        paragraph_style_id: Option<String>,
        /// Undo/redo availability — every interactive edit emits this event,
        /// so the toolbar stays reactive without polling (Phase 4 §11).
        can_undo: bool,
        can_redo: bool,
        /// Phase 5 PR 4 — selection flavour. `Linear` for classic text
        /// spans, `TableCells` when the caret is inside a table and the
        /// drag covered multiple cells. UI overlays inspect this to
        /// switch between text-span and cell-rect highlights.
        selection_kind: SelectionKind,
        /// Phase 9b — per-flag "mixed across the selection" bitmap. A flag
        /// is `true` when the underlying spans in `[range.start, range.end)`
        /// don't all agree (some bold + some not). The toolbar renders an
        /// **indeterminate** state for those buttons so a user can't
        /// mis-read the start position's value as the whole selection's.
        /// Always `false` on a collapsed caret (nothing to disagree over).
        attrs_mixed: AttrsMixed,
        /// Phase 9c — paragraph base direction (`<w:bidi>`) reflected as
        /// a tri-state for the toolbar LTR/RTL buttons. `Some(Ltr)` /
        /// `Some(Rtl)` when every paragraph in the range agrees on the
        /// EFFECTIVE direction (explicit `props.direction` if set, else
        /// first-strong inference, else the layout-config default).
        /// `None` when paragraphs disagree → toolbar renders both
        /// direction buttons as INDETERMINATE.
        paragraph_direction: Option<Direction>,
        /// Sprint 10 — page geometry of the section the caret sits in.
        /// Drives `PageSetupDialog` prefill (margin / orientation /
        /// columns). `None` when no section data is available (e.g. a
        /// fresh empty document still on the implicit-default A4).
        section_geometry: Option<BridgeSectionGeometry>,
        /// Sprint 10 — cell shading + per-edge borders for the active
        /// cell when the caret is inside a table. `None` outside any
        /// table — `CellPropertiesDialog` falls back to engine defaults.
        cell_properties: Option<BridgeCellProperties>,
        /// Sprint 11 (#13) — `<w:pPr><w:tabs>` custom tab stops of the
        /// paragraph under the caret. Empty when the paragraph
        /// inherits the default 0.5-inch grid. Drives the Ruler's
        /// tab-stop markers + their drag-edit handles.
        tab_stops: Vec<BridgeTabStop>,
        /// Sprint 15 (#13) — `<w:pPr><w:ind>` indentation of the
        /// paragraph under the caret, in layout pt. Lets the Ruler's
        /// indent handles READ BACK the active paragraph's values
        /// (so the markers jump to a 20 pt indent when the caret lands
        /// there) instead of always sitting at the leading edge, and
        /// lets a drag of one handle PRESERVE the others rather than
        /// zeroing them. `Default` (all-zero) when the caret has no
        /// addressable paragraph (defensive — same contract as
        /// `tab_stops` being empty).
        paragraph_indent: BridgeIndent,
        /// Sprint 14 (#14) — current engine track-changes recording
        /// state. The UI binds `ReviewControls`'s Track-Changes
        /// toggle to this field instead of carrying local Solid
        /// state, so a `ToggleTrackChanges` issued by another tab,
        /// macro, or undo path stays in sync.
        is_tracking_changes: bool,
        /// Issue #38 — total snapshots ever pushed onto the `UndoStack`
        /// (`UndoStack::depth`). Every interactive edit — typed insert,
        /// tracked delete, paste, formatting — pushes exactly one new
        /// snapshot, so a change in this count (and ONLY a change in
        /// this count) means "a new edit landed," distinct from a plain
        /// caret move / click, which never pushes. Undo/Redo replay
        /// existing snapshots and do not change the count. Lets
        /// `TrackChangesSidebar` re-fetch `revisions_snapshot()` on
        /// every real edit without a dedicated event or a noisy
        /// refetch-on-every-caret-move.
        undo_depth: u32,
        /// Issue #42 — `<w:numPr><w:ilvl>` of the paragraph under the
        /// caret. `None` when the paragraph is not a list item. Lets the
        /// shell's Tab-key handler decide, without a round-trip, whether
        /// to demote/promote a list level or fall back to its existing
        /// tab-char/cell-navigation behavior.
        list_ilvl: Option<u8>,
        /// Issue #41 — the caret paragraph's `<w:pPr><w:pBdr>` per-edge
        /// borders, or `None` when the paragraph has no border set. Lets
        /// the paragraph border picker prefill each edge's style / width /
        /// colour from the live document (mirrors how `cell_properties`
        /// feeds the cell border editor) instead of always opening blank.
        paragraph_borders: Option<BridgeCellBorders>,
        /// Phase 3 (#39) — the active header/footer story, or `None` in
        /// body mode. While `Some`, `range`/`caret`/`rects` (and every
        /// caret-relative command) are expressed in STORY space — paths
        /// root at the story's paragraph list — while the geometry
        /// values stay absolute page device px, so the caret/selection
        /// overlays render unchanged. The shell dims the body, outlines
        /// the band on the anchor page, and gates non-story controls.
        editing_story: Option<BridgeStoryRef>,
        /// Issue #77 — `true` while the Alt+F9 field-code view is on.
        /// Fields then PAINT `{ INSTRUCTION }` codes instead of results;
        /// `caret` / `rects` are the pixel geometry of that code text,
        /// while `range` (like every `LogicalPos` on the wire) stays a
        /// SOURCE position.
        field_code_view: bool,
        /// Issue #77 — the field the selection addresses: the one the
        /// selection covers EXACTLY (a click inside a field selects it
        /// whole), else — for a collapsed caret — the field ending
        /// at the caret, else the one starting there. `None` otherwise.
        /// Drives the field-code editor + the "Update field" affordance.
        field_at_caret: Option<BridgeFieldRef>,
        /// Issue #52 — the engine's current user zoom fraction
        /// (`SetZoom`, clamped to `[0.25, 4.0]`; `1.0` before the first
        /// `RenderPage`). `SetZoom` / `SetDeviceScale` answer with this
        /// event, so every zoom control mirrors the ENGINE's value — one
        /// source of truth instead of one local signal per widget.
        /// Issue #239 — that's true once a `RenderPage` has run; a
        /// `SetZoom` / `SetDeviceScale` sent BEFORE the first one answers
        /// with `Event::ZoomPending` instead (there is no selection yet
        /// to build this event around).
        #[serde(default = "default_zoom")]
        zoom: f32,
        /// Issue #260 — the engine's document revision (the issue-#194
        /// `mutation_seq`, the same counter `Event::Painted.mutation_seq`
        /// carries) AFTER the command this event answers: bumped once per
        /// command that changed the document, never on a pure selection
        /// move. Together with `range` + `editing_story` it keys the
        /// shell's synchronous clipboard cache, replacing the hand-kept
        /// "which events invalidate" list. Additive — `0` from producers
        /// that predate it.
        #[serde(default)]
        document_revision: u64,
        /// Issue #423 — the family the caret run's LATIN slot (`w:ascii`,
        /// else `w:hAnsi`) resolves to after the full cascade: the run's
        /// own name → its theme binding (through `theme1.xml`) → the
        /// style chain → docDefaults → the layout's default face. A
        /// display name (`"Calibri"`), never the raw theme token
        /// (`minorHAnsi`). `attrs_at_caret.font_family` keeps reporting
        /// the run's own id (or the layout default) for older consumers.
        /// Additive — empty from producers that predate it.
        #[serde(default)]
        resolved_font_latin: String,
        /// Issue #423 — `resolved_font_latin`
        /// for the COMPLEX-SCRIPT slot (`w:cs` / `w:cstheme`, the theme's
        /// per-script entry for the caret's script): what Arabic / Hebrew
        /// text at the caret is shaped with.
        #[serde(default)]
        resolved_font_cs: String,
        /// Issue #423 — where each slot's resolved family came from, so a
        /// picker can mark a theme font ("Calibri (theme)").
        #[serde(default)]
        font_source: BridgeFontSources,
        /// Issue #420 — the caret run's resolved size / weight / slant /
        /// family id PER SCRIPT SLOT (Word's Font dialog: "Latin text" and
        /// "Complex scripts"). `attrs_at_caret` reports only the slot the
        /// caret's own text reads (`caret_font_slot`).
        #[serde(default)]
        slot_formats: BridgeSlotFormats,
        /// Issue #423 — the slot `attrs_at_caret` reports: `ComplexScript`
        /// when the caret's text is Arabic / Hebrew / … (or the run is
        /// `<w:rtl/>` / `<w:cs/>`), else `Latin`. Never `Both`.
        #[serde(default = "default_caret_font_slot")]
        caret_font_slot: FontSlot,
        /// Issue #345 — the editing restriction the document enforces
        /// (`<w:documentProtection w:enforcement="1">` with a restricting
        /// `w:edit`), so the shell can badge it; `None` for an
        /// unrestricted document. Additive: skipped on the wire when
        /// `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        protection: Option<ProtectionMode>,
    },
    /// Issue #239 — reply to `SetZoom` / `SetDeviceScale` when no
    /// `RenderPage` has run yet: there is no layout config to fold the
    /// value into, and no selection either (`render_page` always resets
    /// it), so answering with `SelectionChanged` would mean fabricating
    /// a selection over a document that doesn't exist yet. The engine
    /// stashes the value and composes it into the config the first
    /// `RenderPage` builds; this reply reports the (clamped) value
    /// directly instead of a misleading `Event::Error` for what is
    /// actually a successful, queued command.
    ZoomPending {
        /// The pending user-zoom fraction — the value just set by
        /// `SetZoom`, or the previously-queued one when this reply
        /// answers a `SetDeviceScale`.
        zoom: f32,
        /// The pending device scale, when one has been queued (by a
        /// `SetDeviceScale`, this one or an earlier one); `None` if only
        /// zoom has been set so far.
        #[serde(default)]
        #[tsify(optional)]
        device_scale: Option<f32>,
    },

    /* IME */
    CompositionUpdated {
        at: LogicalPos,
        text: String,
        target_range: Option<LogicalRange>,
    },

    /* Editing feedback */
    FormattingChanged {
        range: LogicalRange,
        attrs: TextAttrs,
    },

    /* Accessibility */
    /// Fine-grained accessibility patches — broadcast after every document
    /// mutation (PHASE_4_HEADLESS_UI.md §10, Backlog #10). The first delta from
    /// any engine instance (boot or post-recovery) is a single `Replace`; every
    /// later delta carries only the paragraphs that actually changed.
    AccessibilityTreeDelta {
        patches: Vec<A11yPatch>,
    },

    /* Telemetry */
    Stats(EngineStats),

    /* Resource events */
    FontMissing {
        script: Script,
        requested: String,
    },

    /* Errors */
    /// Fatal — the worker is about to die; the TS client should recover.
    Trap {
        stack: String,
    },

    // ===================================================================
    // Phase 4 schema — PHASE_4_HEADLESS_UI.md §7.
    // ===================================================================
    /// Reply to [`crate::Command::HitTest`] — the logical position under
    /// the hit-tested pixel.
    HitResult {
        pos: LogicalPos,
    },

    /// Issue #44 — reply to [`crate::Command::GetImageRects`]. Every
    /// inline image's absolute device-px rectangle plus its resize
    /// address. Empty when the document holds no images.
    ImageRects {
        images: Vec<ImageRect>,
    },

    /// Reply to `Command::GetSelectionAsClipboard` — the selection as
    /// clipboard MIME payloads (`html` / `docx_fragment` populated since
    /// Phase 5 sprint 7's rich-clipboard work).
    ClipboardPayload {
        plain: String,
        html: String,
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        docx_fragment: Vec<u8>,
    },

    /// Sprint 10 — broadcast a short, human-readable message describing
    /// a user-visible mutation ("Aligned center", "Page break inserted",
    /// "Comment added"). The TS shell pipes these into an `aria-live`
    /// region so screen readers narrate engine actions; the engine
    /// owns the wording + priority so the UI never has to compute
    /// ARIA semantics from event payloads.
    Announcement {
        priority: AnnouncementPriority,
        message: String,
    },

    /// Issue #390 - health of the worker's durable event log, broadcast
    /// unsolicited on `subscribe()` (like the accessibility deltas; never
    /// a reply, never produced by `Engine::apply`) whenever it changes.
    /// It replaces the ad-hoc untyped `CHECKPOINT` worker message: a
    /// failed engine-side `SNAPSHOT`, a failed snapshot write and a
    /// failed command-row append each count, are retried on a bounded
    /// 2/4/8 s clock, and flip `ok` to `false` once the retries are
    /// exhausted (the shell then tells the user their changes are not
    /// being protected). Additive: no `Command` carries it.
    CheckpointState {
        /// `true` while checkpoints (snapshots) and the command journal
        /// are landing; `false` once a failure's bounded retries ran out.
        ok: bool,
        /// Consecutive failed attempts in the current failure run
        /// (snapshot dispatch + snapshot write + journal append);
        /// `0` when healthy.
        failures: u32,
        /// The most recent failure's message, when one occurred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        last_error: Option<String>,
        /// The command journal specifically is not being written: a
        /// recovery now would silently miss the commands in the gap.
        #[serde(default)]
        journal_failing: bool,
    },
}

/// Defines [`ErrorKind`], `ErrorKind::ALL` and `ErrorKind::name()` from ONE
/// variant list (issue #454, in the style of `command_meta!`): a variant
/// cannot exist without being listed in `ALL` and named, so the hand-kept
/// list the parity cross-check used to guard is gone. Attributes (docs) ride
/// on each variant; the declaration order is the wire order of `ALL`.
macro_rules! error_kinds {
    ( $( #[$enum_meta:meta] )* ; $( $( #[$meta:meta] )* $variant:ident ),+ $(,)? ) => {
        $( #[$enum_meta] )*
        pub enum ErrorKind {
            $( $( #[$meta] )* $variant ),+
        }

        impl ErrorKind {
            /// Every kind, in declaration order (issue #427). `tools/parity`
            /// joins it with the shell's `ERROR_TOAST_COPY`.
            pub const ALL: &'static [ErrorKind] = &[ $( ErrorKind::$variant ),+ ];

            /// The wire name (the variant name, as `Event::Error.kind` carries it).
            pub const fn name(self) -> &'static str {
                match self {
                    $( ErrorKind::$variant => stringify!($variant) ),+
                }
            }
        }
    };
}

error_kinds! {
/// Issue #348 — the class of an [`Event::Error`] the shell can present
/// specifically.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
;
    /// The document package exceeded the reader's resource limits (a part
    /// or the whole package inflating past its byte budget, too many ZIP
    /// entries, XML nested too deep or holding too many elements): the
    /// open was refused before anything was allocated from the package's
    /// own size claims. The previous document stays open.
    PackageTooLarge,
    /// Issue #364 - a tracked (review-mode) deletion the engine refuses:
    /// nothing changed. The shell shows a visible, non-modal refusal
    /// instead of letting the key press appear to do nothing. Since issue
    /// #365 a range across table cells or over a table is RECORDED (rows
    /// marked deleted, `<w:trPr><w:del/>`), so only a range whose end
    /// addresses no paragraph is refused.
    TrackedDeletionRefused,
    /// Issue #345 — the file is an encrypted (password-protected) Office
    /// document: an OLE compound file (`D0 CF 11 E0 A1 B1 1A E1`) carrying
    /// an MS-OFFCRYPTO / ECMA-376 Part 2 `EncryptedPackage`, not a ZIP. The
    /// open was refused; the previous document stays open.
    EncryptedDocument,
    /// Issue #345 — the document's enforced `w:documentProtection` refused
    /// the command (a read-only document, comments-only, tracked-changes
    /// only, or an edit outside form-field content). Nothing changed.
    Protected,
    /// Issue #345 — `OpenDocument.password` does not open the encrypted
    /// package. The previous document stays open.
    WrongPassword,
    /* Issue #427 - the remaining refusals, typed. Closed and additive: a
    new refusal either reuses one of these or adds a variant AND its
    `ERROR_TOAST_COPY` entry (`tools/parity` fails on a kind with neither
    copy nor a declared own presentation). */
    /// The command needs a selection / caret and the engine has none.
    NoSelection,
    /// The position does not address a paragraph (a stale or out-of-range
    /// address).
    NotInParagraph,
    /// The command is body-only (or text-box-only) and a note, text box,
    /// header or footer is being edited.
    InStory,
    /// The command is not supported inside a table cell (fields, notes,
    /// text boxes, page / section breaks).
    InTableCell,
    /// `SetFieldInstruction` found no field at the caret.
    NoFieldAtCaret,
    /// The command's target (a comment, style, picture, field, table, text
    /// box chain) does not exist (any more).
    NoSuchTarget,
    /// `SetHeaderFooterLink` while no header or footer is being edited.
    NotInHeaderFooter,
    /// A well-formed command the engine does not support at this place or
    /// for this input (PDF as an open format, a TOC outside the body, ...).
    UnsupportedHere,
    /// A numeric argument is outside its allowed range (table dimensions,
    /// text-box extent, render date). A `NaN` / infinite one is
    /// [`ErrorKind::InvalidArgument`] (issue #407).
    OutOfRange,
    /// A required text argument is empty (a field code, a glyph string).
    EmptyInput,
    /// Text boxes nest deeper than the layout allows.
    NestingTooDeep,
    /// The engine is not ready for the command yet (no layout config, a
    /// font not loaded).
    NotReady,
    /// The command is accepted by the schema but not implemented.
    Unimplemented,
    /// An internal failure (paint, rasterize, serialize, export); nothing
    /// the user can fix by changing their input.
    Internal,
    /// The file is not a readable document (a corrupt package that is not
    /// one of the typed open refusals above).
    InvalidDocument,
    /// Issue #407 — a numeric argument of the command was `NaN` or
    /// infinite (`Command::first_non_finite`; the message names the
    /// field). Nothing was applied. A host / shell bug rather than a user
    /// mistake, but like every #427 refusal it is shown (toast copy), never
    /// silent.
    InvalidArgument,
}

/// Issue #345 — an enforced editing restriction (`w:documentProtection`
/// `w:edit`, spelled as in OOXML on the wire).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProtectionMode {
    /// No edits at all.
    ReadOnly,
    /// Comments only.
    Comments,
    /// Every edit is tracked; review mode is forced on.
    TrackedChanges,
    /// Only form-field content (content controls, text form fields).
    Forms,
}

impl ProtectionMode {
    /// Every mode, in declaration order.
    pub const ALL: &'static [ProtectionMode] = &[
        ProtectionMode::ReadOnly,
        ProtectionMode::Comments,
        ProtectionMode::TrackedChanges,
        ProtectionMode::Forms,
    ];

    /// The serde wire spelling (`w:edit`), e.g. `"trackedChanges"`.
    pub const fn wire_name(self) -> &'static str {
        match self {
            ProtectionMode::ReadOnly => "readOnly",
            ProtectionMode::Comments => "comments",
            ProtectionMode::TrackedChanges => "trackedChanges",
            ProtectionMode::Forms => "forms",
        }
    }
}

/// Issue #406 — one coalesced reader diagnostic on
/// [`Event::DocumentLoaded::warnings`].
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct ReadWarning {
    /// The stable class (also the telemetry code, see
    /// [`crate::ReadWarningCount`]).
    pub kind: ReadWarningKind,
    /// The archive entry the warning concerns (`word/styles.xml`,
    /// `_rels/.rels`), when the reader knows it; `None` for diagnostics
    /// raised deep inside a part walk (measures, fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub part: Option<String>,
    /// The specifics, for the details list and the Dev HUD: the attribute
    /// and its raw value (`w:pgMar/@w:top = "99999" → 31680 twips`), the
    /// limit that was hit, the relationship target. Never document text;
    /// never sent to telemetry (which carries `kind` counts only).
    pub detail: String,
    /// How many identical warnings (same kind, part and detail) this entry
    /// stands for; at least 1.
    #[serde(default = "one_warning")]
    pub count: u32,
}

/// `serde(default)` for [`ReadWarning::count`].
fn one_warning() -> u32 {
    1
}

/// Issue #406 — the class of a [`ReadWarning`]: one variant per
/// `format_docx::DocxWarning` class, PascalCase on the wire like
/// [`ErrorKind`]. The spelling is a stable telemetry code — never rename a
/// variant, only add.
#[derive(
    Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub enum ReadWarningKind {
    /// Issue #111 — a table nested past the reader's depth cap was kept as
    /// an opaque block (its bytes survive a save; its inner structure is
    /// not editable).
    TableNestingTooDeep,
    /// Issue #349 / #407 — a measure attribute held an unusable value (not
    /// a number, `NaN`, infinite, a unit its type does not allow, negative
    /// where only non-negative values are legal); the default applies.
    InvalidMeasure,
    /// Issue #349 / #407 — a measure attribute (twips, or a DrawingML /
    /// VML EMU coordinate) held a finite value outside its range; it is
    /// used clamped.
    MeasureClamped,
    /// Issue #350 — complex fields still open in their instruction part
    /// when their paragraph ended were closed there.
    UnclosedField,
    /// Issue #350 — a field `separate` / `end` with no open field was
    /// ignored.
    StrayFieldChar,
    /// Issue #350 — complex fields nested past the reader's cap; the extra
    /// levels stay hidden code.
    FieldNestingTooDeep,
    /// Issues #325 / #394 — a WordprocessingML part binds the namespace
    /// under a non-canonical prefix; it was normalised and read, and is
    /// regenerate-only on save (its bytes are no longer reused verbatim).
    NonCanonicalNamespaces,
    /// Issue #325 — the main part is not WordprocessingML; it reads as an
    /// empty document.
    NotWordprocessingMl,
    /// Issue #353 — the package's `officeDocument` relationship names a
    /// part the archive lacks; the conventional `word/document.xml` was
    /// used.
    MainPartFallback,
    /// Issue #353 — a relationship target escapes the package and was
    /// ignored.
    UnsafeRelationshipTarget,
    /// Issues #439 / #434 / #435 — a WordprocessingML part is not
    /// well-formed XML (bytes that are not UTF-8, a raw `&`, an excluded
    /// character, an undeclared namespace prefix, a truncated part, …).
    /// Repaired up front where a faithful repair exists — the part is then
    /// regenerate-only on save — else read as it is (the detail says
    /// which).
    MalformedPart,
}

impl Event {
    /// An [`Event::Error`] carrying its [`ErrorKind`] (issue #427).
    pub fn error_kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        Event::Error {
            message: message.into(),
            kind: Some(kind),
        }
    }

    /// A plain [`Event::Error`] (no [`ErrorKind`]).
    pub fn error(message: impl Into<String>) -> Self {
        Event::Error {
            message: message.into(),
            kind: None,
        }
    }
}

/// Sprint 10 — `aria-live` priority for an `Event::Announcement`. The
/// TS shell routes `Polite` into an `aria-live="polite"` region (the
/// reader waits for the user to stop typing) and `Assertive` into an
/// `aria-live="assertive"` region (the reader interrupts immediately).
///
/// Engine-side mapping: every user-visible mutation emits `Polite`;
/// only error / blocked-action notifications emit `Assertive`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnouncementPriority {
    Polite,
    Assertive,
}

/// Sprint 10 — lightweight wire shape for the page geometry of the
/// section under the caret. Units are layout pt (1/72 in) at scale=1,
/// matching `engine::PageGeometry` exactly; the UI converts to its
/// preferred unit (in / mm / pt) for display.
///
/// "Lightweight" matters: `SelectionChanged` fires on every keystroke
/// and pointer move. Eight `f32`s + one `u32` + one enum is a single
/// 40-ish byte struct, allocation-free across the wasm bridge.
/// Sprint 15 (#13) — wire shape for the paragraph-under-caret's
/// `<w:pPr><w:ind>` indentation, in layout pt (1/72 in) at scale=1.
/// Drives the Ruler's indent-handle read-back + clobber-free commits.
///
/// `first_line_pt` is **signed** to match `Command::SetParagraphIndent`'s
/// convention: a positive value is a first-line indent (`<w:firstLine>`),
/// a negative value is a hanging indent (`<w:hanging>`). The engine stores
/// the two as mutually-exclusive non-negative twip fields and folds them
/// into this single signed pt here so the UI round-trips a drag without
/// re-deriving the sign.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq)]
pub struct BridgeIndent {
    /// Leading-edge indent (`<w:start>` / `<w:left>`).
    pub start_pt: f32,
    /// Trailing-edge indent (`<w:end>` / `<w:right>`).
    pub end_pt: f32,
    /// Signed first-line offset: `> 0` → `<w:firstLine>`, `< 0` → `<w:hanging>`.
    pub first_line_pt: f32,
}

/// Issue #70 — which band SLOT a story edits, mirroring the page role
/// that was double-clicked (`engine::HeaderFooterRole` wire twin).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BridgeHfRole {
    #[default]
    Default,
    First,
    Even,
}

/// Phase 3 (#39) — wire shape for the active header/footer story riding
/// `Event::SelectionChanged.editing_story`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct BridgeStoryRef {
    /// Which band is being edited.
    pub area: HeaderFooterArea,
    /// Relationship id of the story's part — the key into the engine's
    /// header/footer maps. Diagnostic + dedup value for the shell (two
    /// pages showing the same part share a rid).
    pub rid: String,
    /// The ANCHOR page (0-based): where the user entered the story and
    /// where the caret/selection geometry is projected. Edits reflect on
    /// every page of the owning section at the next paint.
    pub page: u32,
    /// Issue #70 — which slot (Default / First / Even) the anchor
    /// page's role selected; names the story in the UI chip.
    pub role: BridgeHfRole,
    /// Issue #70 — `true` when the edited part is INHERITED from an
    /// earlier section (Word's "Same as Previous"); `false` when the
    /// story's section owns the slot. Drives the Link-to-Previous
    /// toggle state.
    pub linked: bool,
    /// Issue #70 — 0-based index of the story's owning section; the
    /// UI disables Link-to-Previous at index 0 (nothing precedes it).
    pub section_index: u32,
}

/// Issue #77 — wire shape for the field under the selection
/// (`Event::SelectionChanged.field_at_caret`).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq)]
pub struct BridgeFieldRef {
    /// Verbatim field code (`PAGE \* MERGEFORMAT`).
    pub instruction: String,
    /// Upper-cased leading keyword (`PAGE`, `DATE`, `TOC`, …).
    pub keyword: String,
    /// Start of the field's cached-result range — a SOURCE position,
    /// the same space as `SelectionChanged.range` (unchanged by the
    /// code view). `SetSelection { range: start..end }` selects the
    /// field; `SetFieldInstruction { at: end }` addresses it.
    pub start: LogicalPos,
    /// End of that range (one past the last result byte).
    pub end: LogicalPos,
    /// `true` when the selection covers the field exactly.
    pub selected: bool,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct BridgeSectionGeometry {
    pub width_pt: f32,
    pub height_pt: f32,
    pub margin_top_pt: f32,
    pub margin_right_pt: f32,
    pub margin_bottom_pt: f32,
    pub margin_left_pt: f32,
    pub orientation: PageOrientation,
    /// `<w:cols w:num>` — 1 for the default single-column body.
    pub columns: u32,
    /// `<w:cols w:space>` — inter-column gutter in pt.
    pub column_gutter_pt: f32,
    /// Phase 3 (#40) — 0-based index of the caret's section in document
    /// order. Drives the StatusBar "Sec i/n" indicator + QA assertions.
    pub section_index: u32,
    /// Phase 3 (#40) — total section count in the document.
    pub section_count: u32,
    /// Issue #74 — the covering section's `<w:titlePg/>` state; drives
    /// the "Different first page" checkbox.
    pub title_pg: bool,
    /// Issue #74 — the DOCUMENT-wide `<w:evenAndOddHeaders/>` state
    /// (settings.xml; rides section geometry for checkbox convenience).
    pub even_odd_headers: bool,
}

/// Sprint 10 — wire shape for the active cell's shading + per-edge
/// borders. Mirror of `engine::CellProperties`'s
/// CellPropertiesDialog-relevant subset; `grid_span`, `v_merge`,
/// `cell_margins` are deliberately omitted (the dialog does not edit
/// them).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default)]
pub struct BridgeCellProperties {
    pub shading: Option<Color>,
    pub borders: BridgeCellBorders,
    /// Issue #79 — `<w:bidiVisual>` of the top-level table the caret is
    /// in (the table `Command::SetTableProperties` / the table context
    /// menu address): `true` ⇒ right-to-left visual column order.
    #[serde(default)]
    pub table_bidi_visual: bool,
}

/// Per-flag "this attribute is mixed across the selection" bitmap that
/// rides on `Event::SelectionChanged`. The toolbar reads it to switch
/// each button between OFF / ON / INDETERMINATE — without it, a
/// selection that crosses a bold/non-bold boundary would mis-render
/// the start position's bold state as the whole selection's.
///
/// Each field is `true` iff the underlying spans in the selection
/// don't unanimously agree on that flag. A collapsed caret reports all
/// `false` (there's no range to disagree over).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default)]
pub struct AttrsMixed {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

/// Issue #423 — where a script slot's resolved family came from (the
/// cascade level that named it).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontSource {
    /// The run's own formatting names the family: its direct
    /// `<w:rFonts>` name, a toolbar / Font-dialog pick, an armed pending
    /// (sticky) style.
    Explicit,
    /// A theme binding (`w:asciiTheme` / `w:hAnsiTheme` / `w:cstheme`, at
    /// any cascade level) resolved through the document theme, including
    /// its per-script entries (`<a:font script="Arab">`).
    Theme,
    /// The style cascade names it: the character / paragraph style chain
    /// or docDefaults (`<w:rPrDefault>`).
    Style,
    /// Nothing in the document names the slot; the family is the layout's
    /// default face for the script (the font stack's fallback).
    #[default]
    Default,
}

/// Issue #423 — [`FontSource`] per script slot.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgeFontSources {
    pub latin: FontSource,
    pub complex_script: FontSource,
}

/// Issue #420 — one script slot of the formatting at the caret, fully
/// resolved (cascade + theme + pending style), as the Font dialog seeds
/// its "Latin text" / "Complex scripts" sections. Fonts are keyed by the
/// toolbar / font-registry id (`TextAttrs.font_family`'s spelling);
/// the display name is `SelectionChanged.resolved_font_*`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default, PartialEq)]
pub struct BridgeSlotFormat {
    /// Resolution id of the slot's family (`"amiri"`, `"calibri"`, …).
    pub font_family: String,
    /// Points.
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
}

/// Issue #420 — [`BridgeSlotFormat`] per script slot.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default, PartialEq)]
pub struct BridgeSlotFormats {
    /// `<w:sz>`, `<w:b>`, `<w:i>`, `w:ascii` / `w:hAnsi`.
    pub latin: BridgeSlotFormat,
    /// `<w:szCs>`, `<w:bCs>`, `<w:iCs>`, `w:cs`.
    pub complex_script: BridgeSlotFormat,
}

/// Font vertical metrics, scaled to a requested pixel size.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct FontMetrics {
    pub units_per_em: u16,
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    pub cap_height: f32,
    pub x_height: f32,
}

/// Engine-side capabilities reported in `Event::Ready`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct EngineCapabilities {
    pub simd: bool,
    pub shared_array_buffer: bool,
    pub max_document_pages: u32,
    pub formats: Vec<DocFormat>,
}

/// Memory + performance counters, re-emitted on `Command::RequestStats`.
/// Issue #87 — why a paint was laid out degraded. Mirrors
/// `layout::DegradeReason` one-to-one (the engine maps at the bridge
/// boundary; the layout crate has no serde).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LayoutDegradeReason {
    /// A single line (or an atomic table) taller than the page budget was
    /// placed on a fresh page and clips past the bottom margin.
    OversizeLine,
    /// A keep-with-next chain could not move with its follower; the
    /// constraint was released (watchdog stage a).
    KeepChainDropped,
    /// Issue #180 — a `<w:keepLines>` paragraph could not move whole;
    /// the constraint was released and it split where it stood (watchdog
    /// stage a). Distinct from `KeepChainDropped` (a keep-*next* chain).
    KeepLinesDropped,
    /// Issue #180 — a widow/orphan-controlled paragraph's split could not
    /// be adjusted to avoid a single line on either side; the constraint
    /// was released (watchdog stage a). Distinct from `KeepChainDropped`
    /// / `KeepLinesDropped`.
    WidowControlDropped,
    /// Repeated table header rows left no room for a body row on a
    /// continuation page; the repeat was suppressed there (stage a).
    HeaderRepeatDropped,
    /// A footnote band left no body budget on a fresh page; the block was
    /// placed over the band instead of being bounced forever.
    FootnoteOverflow,
    /// Watchdog stage (b): a churning block was pinned at the cursor.
    FrozenPlacement,
    /// Watchdog stage (c): the page cap was hit; the remaining flow was
    /// appended without further page breaks.
    PageCap,
    /// An incremental (viewport-culled) band failed its prefix invariant
    /// against the previous band; the engine demoted to a full reflow.
    FastPathMismatch,
    /// A paragraph layout-cache entry failed its post-conditions and was
    /// re-laid from scratch.
    CacheMismatch,
    /// The table autofit shrink solver hit its iteration cap; column
    /// floors were used as-is.
    AutofitCap,
    /// Issue #82 — a floating object kept pushing its own anchor forward
    /// through the text-wrap loop's per-object cap; its cutouts were
    /// dropped and it paints over the text where it landed.
    WrapObjectFrozen,
    /// Issue #82 — the anchor → position → wrap → reflow loop oscillated
    /// or hit its pass cap; the moving objects were frozen / the current
    /// pages accepted.
    WrapOscillation,
    /// Issue #82 — a tight / through object had no usable wrap polygon;
    /// its bounding box was used (square wrap).
    WrapPolygonFallback,
    /// Issue #81 — the TOC page-number post-pass hit its re-run cap
    /// without a fixed point; the last observed numbers were stamped.
    PageRefCap,
    /// Issue #129 — the per-page footnote renumbering pass
    /// (`<w:numRestart w:val="eachPage"/>`) hit its one re-run without a
    /// fixed point (the restarted labels' widths kept moving references
    /// across pages); the last pass's labels were kept.
    NoteRestartCap,
    /// Issue #141 — an in-front body float reached into the page's
    /// footnote band and was too tall to lift clear of it; it was pinned
    /// at the body top and paints over the band.
    FloatClampedByNotes,
    /// Issue #318 — a table nested past the layout's nesting cap was
    /// flattened to its paragraphs (stacked, in document order, in the
    /// cell holding it) instead of laid out as a grid.
    NestingCapped,
    /// Issue #379 — a TABLE layout-cache entry (the nested-table memo or
    /// the content-keyed table cache that survives between paints) failed
    /// its post-conditions and the table was re-laid from scratch.
    /// Distinct from `CacheMismatch` (the paragraph cache).
    TableCacheMismatch,
}

/// Issue #87 — one degradation note on `Event::Painted`. `page` is the
/// 0-based page being filled when the paginator applied it; `None` for
/// document-level or sub-paginator notes (cache / autofit / fast path).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct LayoutDegraded {
    pub reason: LayoutDegradeReason,
    pub page: Option<u32>,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct EngineStats {
    pub wasm_heap_bytes: u32,
    pub document_tree_bytes: u32,
    pub glyph_cache_entries: u32,
    pub undo_stack_depth: u32,
    pub fonts_resident: u32,
    pub last_paint_ms: f32,
    pub last_command_ms: f32,
    /// Sprint 8 (UI Edition) — total paragraph count across the
    /// active document, including paragraphs nested in tables.
    pub paragraph_count: u32,
    /// Sprint 8 (UI Edition) — whitespace-split word count. Cheap
    /// and Latin-script-leaning; UAX-#29 segmentation is queued
    /// as a follow-up.
    pub word_count: u32,
    /// Sprint 8 (UI Edition) — Unicode scalar character count
    /// (matches Word's "Characters (with spaces)").
    pub character_count: u32,
}

/// A full accessibility snapshot of the document — the structure mirrored into
/// the screen-reader DOM (PHASE_4_HEADLESS_UI.md §10). Carried by the
/// `A11yPatch::Replace` reset patch.
///
/// Phase 5 PR 3b: widened from a flat `paragraphs` list to a node list so a
/// table can appear inline with paragraphs. Cells nest further `nodes` —
/// recursive in shape; PR 3b only emits paragraphs inside cells.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yTree {
    pub nodes: Vec<A11yNode>,
}

/// A top-level (or nested) accessibility node — a paragraph, a table,
/// (issue #73) a header/footer story container, (issue #165) a text
/// box story region, or (issue #203) a footnote / endnote story region.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum A11yNode {
    Paragraph(A11yParagraph),
    Table(A11yTable),
    Story(A11yStory),
    TextBox(A11yTextBox),
    Note(A11yNote),
}

/// Issue #203 — which note family an [`A11yNote`] region or an
/// [`A11yNoteRef`] reference mark belongs to.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum A11yNoteKind {
    #[default]
    Footnote,
    Endnote,
}

/// Issue #203 — one footnote / endnote story mirrored into the
/// screen-reader DOM. A FOOTNOTE (`role="doc-footnote"`) sits right after
/// the paragraph holding its first reference (after that paragraph's
/// text-box regions), in the same node list — top level for a body
/// paragraph, the cell's `nodes` for a cell paragraph. ENDNOTES are
/// appended at the very end of the top-level list, in first-reference
/// order; the mirror wraps that contiguous suffix in one
/// `role="doc-endnotes"` section. A note is its own node, so an edit
/// inside it patches only that region (`A11yPatch::Update`).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default, PartialEq, Eq)]
pub struct A11yNote {
    /// Footnote or endnote. (Not `kind`: that is the node's tag.)
    pub note_kind: A11yNoteKind,
    /// Stable region id — `"footnote-<w:id>"` / `"endnote-<w:id>"`. The
    /// reference marks' [`A11yNoteRef::id`] names it; the shell derives
    /// the DOM `id` from it and matches it against the active note story
    /// (`editing_story` area `Footnote` / `Endnote`, `rid` = the w:id).
    pub id: String,
    /// The note's `w:id` (what `editing_story.rid` carries).
    pub note_id: u32,
    /// Display marker in document order (`"1"`, `"iv"`, …); empty for a
    /// custom-marked reference (the author's own mark follows in text).
    /// Issue #129 — a footnote of an `eachPage` section carries its
    /// per-page label as painted by the current layout.
    pub marker: String,
    pub nodes: Vec<A11yNode>,
}

/// Issue #203 — the note a reference-mark run points at: the mirror
/// renders the run as `role="doc-noteref"` linking to the region whose
/// [`A11yNote::id`] equals `id`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default, PartialEq, Eq)]
pub struct A11yNoteRef {
    pub kind: A11yNoteKind,
    pub id: String,
}

/// Issue #215 — which inline object family an [`A11yObjectRef`] names.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum A11yObjectKind {
    Image,
    TextBox,
}

/// Issue #215 — the inline image / text box a run's `U+FFFC` placeholder
/// stood for: the mirror renders an [`A11yObjectKind::Image`] run as
/// `<img role="img" alt="…">` (from `alt`), and an
/// [`A11yObjectKind::TextBox`] run as a reference to the
/// [`A11yTextBox`] region whose `id` equals this one's `id`. `text` on
/// the carrying [`A11yRun`] is empty — the object IS the run's content,
/// not a marker like a note reference's display number.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yObjectRef {
    pub kind: A11yObjectKind,
    /// [`A11yObjectKind::Image`] — the engine's `DocumentTree::media` key
    /// (issue #188) the picture paints from. [`A11yObjectKind::TextBox`]
    /// — the region's [`A11yTextBox::id`].
    pub id: String,
    /// The picture's `<wp:docPr descr>` / `name` (issue #44), or the text
    /// box's `<wp:docPr descr>` / `name` (issue #165); `None` when the
    /// source object carries neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub alt: Option<String>,
}

/// Issue #165 — one text box story mirrored into the screen-reader DOM as
/// a `role="group"` region, placed right AFTER the paragraph its anchor
/// lives in (in the same node list: top level for a body paragraph, the
/// cell's `nodes` for a cell paragraph, the parent box's `nodes` for a
/// box nested in a box). `nodes` use the exact body paragraph / table
/// shapes. A box is its own node, so an edit inside it patches only
/// that region (`A11yPatch::Update`), never the whole tree.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yTextBox {
    /// Stable address of the box — the same `host path @ anchor byte`
    /// string `BridgeStoryRef.rid` carries while the box's story is
    /// being edited (`"0@5"`, `"2.1x0.0@3"`); a nested box appends its
    /// story-relative address after a `/` (`"0@5/0@12"`). The shell
    /// matches it against `editing_story.rid` to mark the active region.
    pub id: String,
    /// `<wp:docPr name>` — the region's accessible name, when present.
    pub name: Option<String>,
    /// `<wp:docPr descr>` (alt text) — the region's accessible
    /// description, when present.
    pub description: Option<String>,
    pub nodes: Vec<A11yNode>,
}

/// Issue #73 — one REFERENCED header/footer part mirrored into the
/// screen-reader DOM (`role="banner"`-analog for headers,
/// `role="contentinfo"` for footers). Band text was invisible to
/// assistive tech before this. Ordered after the body nodes: headers
/// rid-sorted, then footers.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yStory {
    /// `true` = header part, `false` = footer part.
    pub header: bool,
    /// Relationship id — stable across edits of the same part.
    pub rid: String,
    pub nodes: Vec<A11yNode>,
}

/// One paragraph in the accessibility tree — a `<p>` in the mirror DOM.
///
/// Carries no id: the engine has no stable paragraph identity, so `diff_a11y`
/// matches paragraphs by content and the patches address them by position.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yParagraph {
    /// The DOCUMENT base direction (the layout config's), identical on
    /// every paragraph. Kept for existing consumers; the mirror's per-`<p>`
    /// `dir` reads [`Self::resolved_direction`].
    pub direction: Direction,
    /// Issue #195 — THIS paragraph's resolved base direction, by the same
    /// precedence layout uses: explicit paragraph direction (`<w:bidi>`),
    /// then UAX #9 first-strong auto-direction, then the document base.
    /// Region paragraphs (text box, header / footer) resolve on their own
    /// and inherit nothing from their anchor. Additive.
    pub resolved_direction: Direction,
    pub runs: Vec<A11yRun>,
}

/// One styled text run within an accessibility paragraph — a `<span>` in the
/// mirror DOM, so screen readers can announce formatting boundaries.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Issue #203 — set on a footnote / endnote REFERENCE mark (whose
    /// `text` is then the note's display marker): the note region it
    /// links to. Absent on every other run — and off the wire, so a
    /// document without notes serializes exactly as before. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub note_ref: Option<A11yNoteRef>,
    /// Issue #215 — set on the run that stands in for an inline image's
    /// or inline text box's `U+FFFC` placeholder (`text` is then empty):
    /// the object it names. Absent on every other run — and off the
    /// wire, so a document without inline objects serializes exactly as
    /// before. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub object: Option<A11yObjectRef>,
}

/// Table node — mirrored as `<table role="table">` in the DOM. PR 3b
/// keeps the block-level position (top-level index) as the addressing
/// scheme; nested-table support lands with the `BlockPath` migration.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yTable {
    /// Top-level block index of this table — pinned to its position in
    /// the document `Vec<Block>` so the TS shell can target it by
    /// `BlockPath::top(block_index)` for table mutations.
    pub block_index: u32,
    pub rows: Vec<A11yRow>,
}

/// Row in a table — `<tr role="row">`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yRow {
    pub cells: Vec<A11yCell>,
}

/// Cell in a row — `<td role="gridcell">`. `row_span` / `col_span` are
/// resolved at build time from `vMerge` / `gridSpan` so the DOM can stamp
/// `aria-rowspan` / `aria-colspan` directly. PR 3b emits paragraphs
/// only; the recursive `nodes` slot allows nested tables (Phase 5b).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yCell {
    pub row: u32,
    pub col: u32,
    pub row_span: u32,
    pub col_span: u32,
    pub nodes: Vec<A11yNode>,
}

/// One incremental patch to the mirrored accessibility tree (Backlog #10).
///
/// Patches in a delta apply in order against the current node list:
/// every `Update` first, then either the `Insert`s (ascending index) or the
/// `Remove`s — a prefix/suffix diff only ever grows or shrinks one contiguous
/// region, so a single delta never mixes inserts and removes.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum A11yPatch {
    /// Discard the whole mirror and rebuild it — the first delta from any
    /// engine instance (boot or post-recovery), where there is no prior tree
    /// to diff against.
    Replace { tree: A11yTree },
    /// Replace the node at `index` in place — its content or structure
    /// changed (the common case: a keystroke lands in one paragraph).
    Update { index: u32, node: A11yNode },
    /// Insert a new node at `index`, shifting later nodes down.
    Insert { index: u32, node: A11yNode },
    /// Remove the node at `index`, shifting later nodes up.
    Remove { index: u32 },
}

#[cfg(test)]
mod a11y_note_wire_tests {
    use super::*;

    /// Issue #348 — an untyped error keeps its pre-#348 wire shape (no
    /// `kind` key) and an old payload decodes; a typed one carries `kind`.
    #[test]
    fn error_kind_is_additive_on_the_wire() {
        let plain = serde_json::to_value(Event::error("x")).unwrap();
        assert_eq!(
            plain,
            serde_json::json!({ "type": "ERROR", "message": "x" })
        );
        let back: Event = serde_json::from_value(plain).unwrap();
        assert!(matches!(back, Event::Error { kind: None, .. }));
        let typed = serde_json::to_value(Event::Error {
            message: "too big".into(),
            kind: Some(ErrorKind::PackageTooLarge),
        })
        .unwrap();
        assert_eq!(
            typed,
            serde_json::json!({ "type": "ERROR", "message": "too big", "kind": "PackageTooLarge" })
        );
        /* Issue #427 - `ErrorKind::ALL` / `name()` agree with the wire. */
        let mut seen = std::collections::BTreeSet::new();
        for k in ErrorKind::ALL {
            let wire = serde_json::to_value(k).unwrap();
            assert_eq!(wire, serde_json::json!(k.name()), "{k:?}");
            assert!(seen.insert(k.name()), "duplicate {k:?}");
            let e = Event::error_kind(*k, "x");
            assert!(matches!(e, Event::Error { kind: Some(got), .. } if got == *k));
        }
        /* Issue #345 — the encrypted-package refusal. */
        let encrypted = serde_json::to_value(Event::Error {
            message: "locked".into(),
            kind: Some(ErrorKind::EncryptedDocument),
        })
        .unwrap();
        assert_eq!(encrypted["kind"], "EncryptedDocument");
        /* Issue #407 — the non-finite argument refusal. */
        let invalid = serde_json::to_value(Event::Error {
            message: "SetZoom: scale is NaN".into(),
            kind: Some(ErrorKind::InvalidArgument),
        })
        .unwrap();
        assert_eq!(invalid["kind"], "InvalidArgument");
    }

    /// Issue #345 — protection modes spell `w:edit`; the typed refusal.
    #[test]
    fn protection_wire_shapes() {
        for (mode, wire) in [
            (ProtectionMode::ReadOnly, "readOnly"),
            (ProtectionMode::Comments, "comments"),
            (ProtectionMode::TrackedChanges, "trackedChanges"),
            (ProtectionMode::Forms, "forms"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), wire);
            assert_eq!(mode.wire_name(), wire);
        }
        assert_eq!(ProtectionMode::ALL.len(), 4);
        let refused = serde_json::to_value(Event::Error {
            message: "protected".into(),
            kind: Some(ErrorKind::Protected),
        })
        .unwrap();
        assert_eq!(refused["kind"], "Protected");
    }

    /// Issue #406 — `DocumentLoaded.warnings` is additive: a clean open
    /// keeps the pre-#406 shape, an old payload decodes, and a degraded
    /// open carries PascalCase kinds with an optional `part` and a `count`
    /// that defaults to 1.
    #[test]
    fn document_loaded_warnings_are_additive_on_the_wire() {
        let clean = serde_json::to_value(Event::DocumentLoaded {
            paragraph_count: 3,
            warnings: vec![],
        })
        .unwrap();
        assert_eq!(
            clean,
            serde_json::json!({ "type": "DOCUMENT_LOADED", "paragraph_count": 3 })
        );
        let back: Event = serde_json::from_value(clean).unwrap();
        assert!(
            matches!(back, Event::DocumentLoaded { paragraph_count: 3, warnings } if warnings.is_empty())
        );
        let degraded = serde_json::to_value(Event::DocumentLoaded {
            paragraph_count: 1,
            warnings: vec![
                ReadWarning {
                    kind: ReadWarningKind::MeasureClamped,
                    part: None,
                    detail: "w:pgMar/@w:top = \"99999\" → 31680 twips".into(),
                    count: 2,
                },
                ReadWarning {
                    kind: ReadWarningKind::NonCanonicalNamespaces,
                    part: Some("word/styles.xml".into()),
                    detail: "x".into(),
                    count: 1,
                },
            ],
        })
        .unwrap();
        assert_eq!(
            degraded,
            serde_json::json!({
                "type": "DOCUMENT_LOADED",
                "paragraph_count": 1,
                "warnings": [
                    { "kind": "MeasureClamped", "detail": "w:pgMar/@w:top = \"99999\" → 31680 twips", "count": 2 },
                    { "kind": "NonCanonicalNamespaces", "part": "word/styles.xml", "detail": "x", "count": 1 },
                ],
            })
        );
        let no_count: ReadWarning = serde_json::from_value(serde_json::json!({
            "kind": "InvalidMeasure",
            "detail": "d",
        }))
        .unwrap();
        assert_eq!(no_count.count, 1);
        assert_eq!(no_count.part, None);
    }

    /// Issue #364 - the tracked-deletion refusal is a typed error kind.
    #[test]
    fn tracked_deletion_refused_kind_is_on_the_wire() {
        let typed = serde_json::to_value(Event::Error {
            message: "DeleteRange: table".into(),
            kind: Some(ErrorKind::TrackedDeletionRefused),
        })
        .unwrap();
        assert_eq!(
            typed,
            serde_json::json!({
                "type": "ERROR",
                "message": "DeleteRange: table",
                "kind": "TrackedDeletionRefused"
            })
        );
    }

    /// Issue #390 - `CheckpointState` is an unsolicited, additive event:
    /// a healthy state carries no `last_error` key, a failing one does.
    #[test]
    fn checkpoint_state_wire_shape() {
        let ok = serde_json::to_value(Event::CheckpointState {
            ok: true,
            failures: 0,
            last_error: None,
            journal_failing: false,
        })
        .unwrap();
        assert_eq!(
            ok,
            serde_json::json!({
                "type": "CHECKPOINT_STATE",
                "ok": true,
                "failures": 0,
                "journal_failing": false
            })
        );
        let failing = serde_json::to_value(Event::CheckpointState {
            ok: false,
            failures: 4,
            last_error: Some("QuotaExceededError".into()),
            journal_failing: true,
        })
        .unwrap();
        assert_eq!(
            failing,
            serde_json::json!({
                "type": "CHECKPOINT_STATE",
                "ok": false,
                "failures": 4,
                "last_error": "QuotaExceededError",
                "journal_failing": true
            })
        );
        // An older payload without `journal_failing` still decodes.
        let back: Event = serde_json::from_value(
            serde_json::json!({ "type": "CHECKPOINT_STATE", "ok": true, "failures": 0 }),
        )
        .unwrap();
        assert!(matches!(
            back,
            Event::CheckpointState {
                ok: true,
                journal_failing: false,
                ..
            }
        ));
    }

    fn run(text: &str, note_ref: Option<A11yNoteRef>) -> A11yRun {
        A11yRun {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            note_ref,
            object: None,
        }
    }

    /// Issue #203 — a run without a reference mark serializes exactly as
    /// before (no `note_ref` key), and an old payload deserializes.
    /// Issue #215 — same for `object`.
    #[test]
    fn plain_runs_keep_their_wire_shape() {
        let json = serde_json::to_string(&run("a", None)).unwrap();
        assert_eq!(
            json,
            r#"{"text":"a","bold":false,"italic":false,"underline":false}"#
        );
        let back: A11yRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back, run("a", None));
    }

    /// Issue #215 — an image object reference rides the wire only when
    /// present, and `alt` is omitted (not `null`) when the picture has
    /// no `descr`/`name`.
    #[test]
    fn object_ref_rides_the_wire_only_when_present() {
        let mut r = run("", None);
        r.object = Some(A11yObjectRef {
            kind: A11yObjectKind::Image,
            id: "word/media/image2.png".into(),
            alt: Some("A flow diagram".into()),
        });
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains(
                r#""object":{"kind":"IMAGE","id":"word/media/image2.png","alt":"A flow diagram"}"#
            ),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<A11yRun>(&json).unwrap(), r);

        let mut no_alt = run("", None);
        no_alt.object = Some(A11yObjectRef {
            kind: A11yObjectKind::TextBox,
            id: "0@5".into(),
            alt: None,
        });
        let json = serde_json::to_string(&no_alt).unwrap();
        assert!(
            json.contains(r#""object":{"kind":"TEXT_BOX","id":"0@5"}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<A11yRun>(&json).unwrap(), no_alt);
    }

    #[test]
    fn note_nodes_round_trip() {
        let node = A11yNode::Note(A11yNote {
            note_kind: A11yNoteKind::Endnote,
            id: "endnote-2".into(),
            note_id: 2,
            marker: "i".into(),
            nodes: vec![],
        });
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.starts_with(r#"{"kind":"NOTE""#), "{json}");
        assert_eq!(serde_json::from_str::<A11yNode>(&json).unwrap(), node);
        let r = run(
            "1",
            Some(A11yNoteRef {
                kind: A11yNoteKind::Footnote,
                id: "footnote-1".into(),
            }),
        );
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains(r#""note_ref":{"kind":"Footnote","id":"footnote-1"}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<A11yRun>(&json).unwrap(), r);
    }
}
