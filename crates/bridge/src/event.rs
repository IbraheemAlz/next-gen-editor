//! `Event` — messages the engine emits back to the TypeScript client.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::command::{BridgeCellBorders, BridgeTabStop, HeaderFooterArea, PageOrientation};
use crate::common::{
    Alignment, Color, Direction, DocFormat, ImageRect, LogicalPos, LogicalRange, Rect,
    RendererDowngrade, Script, SelectionKind, TextAttrs,
};

/// `serde(default)` for the user-zoom fields: an absent value means the
/// engine's cold default (100 %), never `0.0`.
fn default_zoom() -> f32 {
    1.0
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
    },
    DocumentSaved {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        size: u32,
    },
    Error {
        message: String,
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
#[serde(default)]
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
#[serde(default)]
pub struct A11yNoteRef {
    pub kind: A11yNoteKind,
    pub id: String,
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

    fn run(text: &str, note_ref: Option<A11yNoteRef>) -> A11yRun {
        A11yRun {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            note_ref,
        }
    }

    /// Issue #203 — a run without a reference mark serializes exactly as
    /// before (no `note_ref` key), and an old payload deserializes.
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
