//! `engine` — document model + undo stack.
//!
//! Phase 1 weeks 15–18: plain-text paragraphs, in-place text insertion,
//! cheap snapshots via `im::Vector` for undo/redo.
//!
//! # Wire-value validation policy (issues #114–#117)
//!
//! Every byte offset, block path and table dimension that reaches this
//! crate ultimately comes off the RPC wire (or from a replayed event log),
//! so nothing here may trust one. The rules, applied at the model
//! boundary so no caller has to remember them:
//!
//! - **Byte offsets snap down** ([`snap_offset`] / [`Paragraph::snap_offset`]).
//!   An offset is first capped at `text.len()`, then floored to the nearest
//!   UTF-8 char boundary at or before it — *an offset strictly inside a
//!   scalar denotes the boundary before that scalar*. Floor (not ceil,
//!   not reject) because it extends the long-standing `min(len)` clamp
//!   the same way for both range ends, is idempotent (so "clamping is a
//!   no-op" is exactly the validity test), and turns a stale-by-a-byte
//!   caret from an async shell into the nearest sensible position
//!   instead of a worker trap. Every `Paragraph` primitive that slices or
//!   stores an offset (`delete_text`, `split_at`, `apply_style`,
//!   `with_spliced_range`, `word_bounds`, …) and every `DocumentTree`
//!   mutation that stores one (`insert_text`, the tracked-change family,
//!   fields, comments, inline objects) goes through it.
//! - **Block paths never index unchecked.** The `*_in_top` / `*_in_vec`
//!   helpers resolve every step with `get` and return `None` for a path
//!   that does not address what the caller expects; an `im::Vector::set`
//!   is always preceded by a bounds check.
//! - **Table dimensions are capped before allocation**
//!   ([`MAX_TABLE_ROWS`], [`MAX_TABLE_COLS`], [`MAX_TABLE_CELLS`]) and
//!   every row / column / cell coordinate is resolved through
//!   [`DocumentTree::resolve_table_target`], which returns a typed
//!   [`TableError`] instead of panicking. The infallible mutation
//!   helpers stay lenient (out-of-range → unchanged tree) for internal
//!   callers; the `engine-wasm` command handlers use the checked
//!   resolver so the shell gets an `Event::Error` it can show.
//! - **Scales are rejected, not silently clamped, when non-finite**
//!   ([`validate_finite_scale`], issue #186). `Command::SetZoom` /
//!   `SetDeviceScale` carry an `f32` that lands in `RenderConfig.scale`
//!   and, from there, every layout + DPR computation. `f32::clamp`
//!   leaves a NaN `self` untouched (`NaN < min` and `NaN > max` are
//!   both `false`), so a NaN wire value used to sail straight through
//!   the existing `.clamp(MIN, MAX)` call. `±∞` already clamped
//!   correctly (an out-of-range comparison against infinity is not
//!   `false`), but is rejected too so the boundary has one rule:
//!   finite in, or a typed `Event::Error`, never NaN/∞ out.
//! - **Calendar fields are range-checked before they're cached**
//!   ([`validate_render_date`], issue #187). `Command::SetRenderDate`
//!   feeds `year`/`month`/`day`/`hour`/`minute` straight into
//!   `render_date_time_picture`'s `format!` calls; nothing there
//!   panics on a garbage value (issue #187's own repro was a fuzzer-
//!   sent `month: 960_639_140`), but a nonsense date silently lands in
//!   DATE/TIME field text on the next F9 or save. Rejected instead:
//!   year `1..=9999`, month `1..=12`, day valid for that month (leap
//!   years included), hour `0..=23`, minute `0..=59`.

use im::Vector;
use serde::{Deserialize, Serialize};

mod block_remap;
pub use block_remap::CellMove;
pub mod fields;
#[cfg(test)]
mod revision_tests;
mod revisions;
mod text_remap;
pub use text_remap::TextEdit;
pub mod html;
pub mod numbering;
pub mod package;
pub mod snapshot;

pub mod toc;
pub use fields::{
    FieldEnv, FieldInstruction, FieldSite, FieldStory, FieldSwitch, PageContext, TocSwitches,
    TypedField, render_date_time_picture,
};
pub use package::{MediaRef, PackageEntry, SourcePackage};
pub use toc::{TocEntry, TocHeading};

/// Top-level document block (Phase 5 PR 1). Tables sit alongside
/// paragraphs in the body; future block variants (Phase 7 floating
/// images, Phase 8 footnotes) extend this enum.
///
/// Sprint 12 (#11) — `Paragraph` grew past clippy's
/// `large_enum_variant` threshold once the shadow direct_overrides
/// field landed (Paragraph now carries a full ParaProperties + a
/// shadow ParaProperties + style_id + everything from prior phases).
/// Boxing `Paragraph` here would touch every `Block::Paragraph(p)`
/// match site across nine crates; the trade-off is not worth the
/// memory savings for the typical 50-page document the engine
/// targets (the persistent `im::Vector` shares structurally between
/// snapshots anyway). Allowing the lint here is the documented
/// pragmatic choice.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
}

impl Block {
    pub fn as_paragraph(&self) -> Option<&Paragraph> {
        match self {
            Block::Paragraph(p) => Some(p),
            Block::Table(_) => None,
        }
    }
    pub fn as_paragraph_mut(&mut self) -> Option<&mut Paragraph> {
        match self {
            Block::Paragraph(p) => Some(p),
            Block::Table(_) => None,
        }
    }
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Block::Table(t) => Some(t),
            Block::Paragraph(_) => None,
        }
    }
    pub fn as_table_mut(&mut self) -> Option<&mut Table> {
        match self {
            Block::Table(t) => Some(t),
            Block::Paragraph(_) => None,
        }
    }

    /// Issue #120 — the block-level passthrough markup around this block.
    pub fn body_xml(&self) -> Option<&BodyPassthrough> {
        match self {
            Block::Paragraph(p) => p.body_xml.as_deref(),
            Block::Table(t) => t.body_xml.as_deref(),
        }
    }

    /// Issue #120 — mutable slot for the block-level passthrough markup.
    pub fn body_xml_mut(&mut self) -> &mut Option<Box<BodyPassthrough>> {
        match self {
            Block::Paragraph(p) => &mut p.body_xml,
            Block::Table(t) => &mut t.body_xml,
        }
    }
}

/// Normalize a wire-supplied byte `offset` into `text` (issue #115) —
/// the crate's single offset policy, see the module docs: cap at
/// `text.len()`, then floor to the nearest UTF-8 char boundary at or
/// before it. Idempotent; at most three steps back (a scalar is ≤ 4
/// bytes). `text.len()` itself is always a boundary.
pub fn snap_offset(text: &str, offset: u32) -> u32 {
    let mut o = (offset as usize).min(text.len());
    while o > 0 && !text.is_char_boundary(o) {
        o -= 1;
    }
    o as u32
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct DocumentTree {
    /// Top-level block sequence. Previously a flat `Vector<Paragraph>`;
    /// Phase 5 PR 1 widened it to `Vector<Block>` so tables can appear at
    /// any document position. Still `im::Vector` so undo snapshots clone
    /// in O(1) — table cells use plain `Vec<Block>` instead.
    pub blocks: Vector<Block>,
    /// Phase 3 (#40) — the body-level trailing `<w:sectPr>` governing the
    /// FINAL section. Interior section boundaries live on their closing
    /// paragraphs ([`Paragraph::section_end`]); the derived per-range view
    /// is [`Self::effective_sections`]. Default = a single implicit A4
    /// section over the whole document — a fresh document therefore always
    /// has a mutable trailing section (the pre-Phase-3 empty-`Vec` model
    /// made `set_section_*` silent no-ops on new documents).
    pub body_section: SectionProps,
    /// Phase 6b — parsed header parts keyed by the OOXML relationship id
    /// (`r:id` from `<w:headerReference>`). Issue #72 widened the value
    /// from `Vec<Paragraph>` to `Vec<Block>` so `<w:tbl>` inside a
    /// header part survives — the same body/cell block model, so the
    /// story adapter can run every body mutation against a part. The
    /// paginator looks each `Section`'s refs up here and renders the
    /// blocks in the top margin band. `section_end` markers are
    /// meaningless inside a part and are stripped at every write path.
    #[serde(serialize_with = "crate::snapshot::ser_sorted_map")]
    pub headers: std::collections::HashMap<String, Vec<Block>>,
    /// Mirror of `headers` for `<w:footerReference>`. Carries the full
    /// block model: style spans, inline objects, hyperlinks, revisions,
    /// `Field` overlays and tables. The paginator's per-page field
    /// evaluator stamps PAGE/NUMPAGES on the laid-out copies these
    /// blocks produce.
    #[serde(serialize_with = "crate::snapshot::ser_sorted_map")]
    pub footers: std::collections::HashMap<String, Vec<Block>>,
    /// Phase 7 — image blobs keyed by MEDIA KEY. Issue #188 — the archive
    /// reader keys them by the resolved target entry name
    /// (`word/media/image2.png`), registering the picture rels of every
    /// part it parses (body, headers, footers, notes) — never by the bare
    /// relationship id, which is scoped per part. Engine-inserted blobs
    /// (and pre-#188 snapshots) are keyed by their `rel_id`. Pictures
    /// look up through [`InlineKind::image_media_key`].
    #[serde(serialize_with = "crate::snapshot::ser_sorted_map")]
    pub media: std::collections::HashMap<String, ImageBlob>,
    /// Issue #80 — `word/footnotes.xml` note stories keyed by the OOXML
    /// `w:id` (an `i32`: Word's separator sentinels are `-1` / `0`). Every
    /// entry the part carries lands here — the special separator /
    /// continuation notes included — so a regenerated part re-emits them
    /// and the passthrough writer keeps clean notes byte-identical. Note
    /// bodies are the body's own [`Block`] model, so the story adapter
    /// runs every body mutation against a note unchanged.
    ///
    /// Snapshot discipline (issue #85): this REPLACES the Phase-8a
    /// `footnotes: HashMap<u32, Vec<String>>` field. The old key is simply
    /// unread by this build (serde ignores unknown map keys), and a fresh
    /// default here is the correct reading of an older snapshot — the old
    /// plain-text table was a render cache, never authoritative content.
    #[serde(serialize_with = "crate::snapshot::ser_sorted_map")]
    pub footnote_stories: std::collections::HashMap<i32, NoteStory>,
    /// Issue #80 — `word/endnotes.xml` twin of [`Self::footnote_stories`].
    #[serde(serialize_with = "crate::snapshot::ser_sorted_map")]
    pub endnote_stories: std::collections::HashMap<i32, NoteStory>,
    /// Issue #80 — document-level `<w:settings><w:footnotePr>`: numbering
    /// format / start / restart rule + position. Section-level
    /// `<w:sectPr><w:footnotePr>` overrides ride [`SectionProps`].
    pub footnote_props: NoteProps,
    /// Issue #80 — document-level `<w:settings><w:endnotePr>`.
    pub endnote_props: NoteProps,
    /// Issue #80 — which note PARTS the writer must regenerate. Rides the
    /// tree (like `hf_dirty`) so undo reverts it with the content; the
    /// per-note [`NoteStory::dirty`] flag decides passthrough per entry.
    pub notes_dirty: NotesDirty,
    /// Phase 8a — parsed `word/comments.xml` entries keyed by `w:id`.
    /// Plain text + author / date metadata for the sidebar UI.
    #[serde(serialize_with = "crate::snapshot::ser_sorted_map")]
    pub comment_defs: std::collections::HashMap<u32, CommentDef>,
    /// Phase 8a — comment range overlays. Each entry is the byte-range
    /// span of one `<w:commentRangeStart>` / `<w:commentRangeEnd>` pair
    /// expressed in `LogicalPos` so a comment can span across paragraph
    /// (and table-cell) boundaries.
    pub comment_ranges: Vec<CommentRange>,
    /// Phase 2 audit — typed `word/settings.xml` flags. Currently only
    /// `even_and_odd_headers`; grows as more settings get modelled.
    pub settings: DocumentSettings,
    /// Sprint 12 (#11) — parsed `word/styles.xml` entries keyed by
    /// `w:styleId`. Sprint 12 ships paragraph styles only; character
    /// styles (`<w:rStyle>`) are deliberately out of scope. The
    /// reader populates this from `<w:style w:type="paragraph">`
    /// entries; the cascade walker
    /// (`DocumentTree::resolve_style_cascade`) folds a `style_id`
    /// chain through `based_on` into a flat `ParaProperties`.
    #[serde(serialize_with = "crate::snapshot::ser_sorted_map")]
    pub styles: std::collections::HashMap<String, ParagraphStyle>,
    /// Sprint 12 (#11) — document-wide `<w:docDefaults>`. Sits at
    /// the bottom of every paragraph's resolved cascade. The
    /// resolver merges `defaults → style chain → direct_overrides`
    /// in document order.
    pub style_defaults: ParaProperties,
    /// Issue #29 — `<w:docDefaults><w:rPrDefault>` run properties. The
    /// base of the run cascade: `style_run_defaults → pStyle-chain
    /// <w:rPr> → direct span formatting`, folded at span-materialize
    /// time (engine-wasm `build_style_spans`), never baked into spans.
    pub style_run_defaults: SpanStyle,
    /// Issue #21 — flips when `modify_style` mutates the style table so
    /// the `.docx` writer regenerates `word/styles.xml` (mirror of
    /// `NumberingDefinitions.dirty`). Never set by reads.
    pub styles_dirty: bool,
    /// Sprint 13 (#12) — in-memory mirror of `word/numbering.xml`.
    /// Drives marker resolution + the synthesis path the
    /// `Command::ToggleList { Bullet | Number }` handler invokes.
    /// `.dirty` flips to `true` only when synth_list_definition
    /// actually appends new entries; the writer then regenerates
    /// the part, otherwise the OPC passthrough byte-identical.
    pub numbering: numbering::NumberingDefinitions,
    /// Phase 3 (#39) — header/footer parts whose content the engine
    /// mutated (or created) since load; the writer regenerates exactly
    /// these and passthroughs the rest byte-identical.
    pub hf_dirty: HfDirty,
    /// Issues #74/#43 — flips when the engine mutates `settings`
    /// (currently only `even_and_odd_headers`); the writer then patches
    /// `word/settings.xml` in place. Mirror of `styles_dirty` /
    /// `NumberingDefinitions.dirty`. Never set by reads.
    pub settings_dirty: bool,
    /// Issue #100 — every attribute of the source part root
    /// (`<w:document>`), `(name, escaped value)` in document order: the
    /// `xmlns:*` bindings (`w14`, `w15`, `mc`, … — Word declares ~30) plus
    /// `mc:Ignorable`. The `.docx` reader fills it; it is empty for an
    /// engine-authored document. Every `.docx` writer synthesizes its own
    /// part roots (`word/document.xml`, regenerated header/footer parts)
    /// and re-declares these on them, so passthrough paragraphs carrying
    /// `w14:paraId` and root-bound grab-bag fragments stay
    /// namespace-well-formed — including on the live editor's save path,
    /// which has only this tree, not the source `DocxArchive`.
    pub document_root_attrs: Vec<(String, String)>,
    /// Issue #100 — root attributes of the OTHER parts the writer may
    /// regenerate from the tree alone, keyed by archive entry name
    /// (`word/footnotes.xml`, `word/endnotes.xml`). A note part's root can
    /// bind prefixes the document root does not (`w14` for a note
    /// paragraph's `w14:paraId`); the UI save path has no archive to read
    /// them from, so the reader records them here. Empty for an
    /// engine-authored document.
    pub part_root_attrs: std::collections::BTreeMap<String, Vec<(String, String)>>,
    /// Issue #112 — the source `word/document.xml`'s prolog, root start
    /// tag, `<w:body>` tag and tail, verbatim (see [`DocumentEnvelope`]).
    /// Every writer path re-emits them so a zero-edit resave is
    /// byte-identical; empty (synthesized header) for an engine-authored
    /// document. Rides the tree — like [`Self::document_root_attrs`] —
    /// because the live editor saves without the source archive.
    pub document_envelope: DocumentEnvelope,
    /// Issue #134 — every entry of the source `.docx` package except
    /// `word/document.xml`, verbatim (see [`package`]). `Some` for a
    /// document opened from `.docx`: the live editor's save path hands it
    /// to `format_docx::write_docx`, so headers/footers, styles,
    /// numbering, settings, theme, comments and custom XML survive a UI
    /// save byte-identical. `None` for an engine-authored document (saved
    /// through the minimal-package writer). Shared by every undo state via
    /// the `Arc`; never mutated after open.
    #[serde(with = "package::arc_option", skip_serializing_if = "Option::is_none")]
    pub source_package: Option<std::sync::Arc<SourcePackage>>,
}

/// Sprint 12 (#11) — one `<w:style w:type="paragraph">` entry,
/// modelled in the engine so the live editor can apply / re-resolve
/// styles without the format-docx crate's `StyleTable`. Character
/// styles + table styles are deliberately out of scope.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ParagraphStyle {
    pub id: String,
    /// Human-readable name from `<w:name w:val>`. Drives the styles
    /// dropdown label; falls back to `id` when absent.
    pub name: String,
    /// `<w:basedOn w:val>` — parent style id. The cascade walker
    /// folds the chain root-first.
    pub based_on: Option<String>,
    /// `<w:pPr>` overrides this style contributes (folded onto the
    /// root-most ancestor's already-folded baseline).
    pub para: ParaProperties,
    /// `<w:rPr>` overrides this style contributes — applied to spans
    /// during cascade resolution since issue #29 (closed).
    pub run: SpanStyle,
    /// Issue #277 — `<w:next w:val>`: the style Word gives the NEW
    /// paragraph when Enter is pressed at the very end of a paragraph
    /// in this style (Heading 1 → Normal). `None` ⇒ the same style.
    /// Skipped when `None`, so a pre-#277 snapshot encodes unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// Phase 8a — author + date + body for one entry of `word/comments.xml`.
/// Body is currently the joined plain text of every `<w:p>` inside the
/// comment (rich formatting + reply threading deferred to Phase 8c).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CommentDef {
    pub author: String,
    pub date: String,
    pub paragraphs: Vec<String>,
    /// Sprint 9 — round-tripped through `word/commentsExtended.xml`.
    /// The reader populates from `<w15:commentEx w15:done="1"/>`; the
    /// writer regenerates `commentsExtended.xml` when ANY comment
    /// carries `resolved = true`, otherwise the OPC passthrough keeps
    /// the original part byte-identical.
    pub resolved: bool,
    /// Sprint 9 — `w14:paraId` of this comment's first paragraph as
    /// captured by the comments.xml reader. `<w15:commentEx>` keys its
    /// entries by this id; without one, a synthesized comment cannot
    /// round-trip its resolved bit. Engine-minted comments leave this
    /// `None` until the comments.xml writer learns to mint paraIds —
    /// tracked as Core Engine tech-debt.
    pub first_para_id: Option<String>,
    /// Issue #27 — threaded replies. `Some(id)` marks this comment as
    /// a reply to the comment with that `w:id`; `None` marks a
    /// top-level comment. Round-trips through
    /// `word/commentsExtended.xml` `<w15:commentEx w15:paraIdParent>`
    /// (the reader maps the parent paraId back to its comment id via
    /// `first_para_id`).
    pub parent_id: Option<u32>,
}

/// Phase 8a — one `<w:commentRangeStart>` / `<w:commentRangeEnd>` overlay
/// on a logical position range. `id` matches a key in
/// [`DocumentTree::comment_defs`].
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommentRange {
    pub id: u32,
    pub start: LogicalPos,
    pub end: LogicalPos,
}

/// Page geometry for a [`Section`]. Dimensions are layout pixels at 1 pt/unit
/// (matching `layout::A4Page`). The reader converts twips → pt (× 1/20) and
/// the renderer / paginator consume these values directly.
///
/// **OOXML reference values.** A page size landed verbatim from a Word
/// `<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>` is ISO 216 A4
/// (210 × 297 mm). The canonical twips → pt math:
///
/// - `11906 twips / 20 = 595.3 pt`  ← page width
/// - `16838 twips / 20 = 841.9 pt`  ← page height
/// - `1440 twips / 20 = 72.0 pt`    ← Word default 1-inch margins
///
/// Aspect ratio 841.9 / 595.3 = 1.4143, matching ISO 216's `1 : √2`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct PageGeometry {
    pub width: f32,
    pub height: f32,
    pub margin_top: f32,
    pub margin_right: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    /// Distance from the top edge of the page to the top edge of the header
    /// content area. Optional in OOXML; defaults to half the top margin.
    pub header_offset: f32,
    /// Distance from the bottom edge of the page to the bottom edge of the
    /// footer content area.
    pub footer_offset: f32,
}

impl PageGeometry {
    /// ISO 216 A4 with 1-inch (72 pt) margins.
    ///
    /// Dimensions land exactly on the OOXML canonical twips:
    /// `<w:pgSz w:w="11906" w:h="16838"/>` and
    /// `<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
    /// w:header="720" w:footer="720"/>` — what `Word.exe` itself stamps
    /// on a freshly-created `Document1.docx`. Header / footer offsets
    /// default to 0.5 inch (720 twips / 36 pt), Word's stock value.
    pub const fn a4() -> Self {
        Self::from_twips(11906, 16838, 1440, 1440, 1440, 1440, 720, 720)
    }

    /// US Letter (8.5 × 11 in) with the same 1-inch margins / 0.5-inch
    /// header-footer offsets `a4()` uses — `Word.exe` stamps identical
    /// margins regardless of `pgSz`. Issue #109 — the second preset a host
    /// can select via [`DefaultPageSize::Letter`] for the `<w:sectPr>`
    /// fallback.
    ///
    /// - `12240 twips / 20 = 612.0 pt` ← page width
    /// - `15840 twips / 20 = 792.0 pt` ← page height
    pub const fn letter() -> Self {
        Self::from_twips(12240, 15840, 1440, 1440, 1440, 1440, 720, 720)
    }

    /// Build a `PageGeometry` from OOXML twips directly. 1 twip = 1/20 pt.
    /// Used by the `<w:pgSz>` / `<w:pgMar>` parser to preserve exact
    /// integer round-trip; in-code default constructors call this with
    /// canonical Word values so the model never drifts off-spec by
    /// floating-point rounding.
    #[allow(clippy::too_many_arguments)]
    pub const fn from_twips(
        w_twips: i32,
        h_twips: i32,
        top_twips: i32,
        right_twips: i32,
        bottom_twips: i32,
        left_twips: i32,
        header_twips: i32,
        footer_twips: i32,
    ) -> Self {
        Self {
            width: (w_twips as f32) / 20.0,
            height: (h_twips as f32) / 20.0,
            margin_top: (top_twips as f32) / 20.0,
            margin_right: (right_twips as f32) / 20.0,
            margin_bottom: (bottom_twips as f32) / 20.0,
            margin_left: (left_twips as f32) / 20.0,
            header_offset: (header_twips as f32) / 20.0,
            footer_offset: (footer_twips as f32) / 20.0,
        }
    }

    pub fn content_width(&self) -> f32 {
        self.width - self.margin_left - self.margin_right
    }

    pub fn content_height(&self) -> f32 {
        self.height - self.margin_top - self.margin_bottom
    }
}

impl Default for PageGeometry {
    fn default() -> Self {
        Self::a4()
    }
}

/// Issue #109 — the fallback page-size preset a `.docx` reader falls back
/// to when a `<w:sectPr>` omits `<w:pgSz>` (ECMA-376 requires it, but the
/// Apache POI / docx4j "wild document" corpus ships files that skip it).
/// This used to be a hard-coded `PageGeometry::a4()` inside
/// `format-docx`'s `SectPrAccum::into_geometry`; it now rides
/// [`DocumentSettings::default_page_size`] so an embedding host can
/// request `Letter` (via `format_docx::read_docx_with_settings`) instead
/// of patching the parser. `#[default]` stays `A4` — every pinned
/// `layout::geometry_fingerprint` fixture assumes it, and `read_docx`
/// (the A4 convenience wrapper) is unchanged.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultPageSize {
    #[default]
    A4,
    Letter,
}

impl DefaultPageSize {
    pub const fn geometry(self) -> PageGeometry {
        match self {
            DefaultPageSize::A4 => PageGeometry::a4(),
            DefaultPageSize::Letter => PageGeometry::letter(),
        }
    }
}

/// `<w:headerReference>` / `<w:footerReference>` discriminator — the
/// `w:type` attribute. `Default` is what every page uses unless a more
/// specific variant is requested and selected by the paginator.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HeaderFooterRole {
    #[default]
    Default,
    /// `w:type="first"` — only used when `Section.title_pg` is `true`.
    First,
    /// `w:type="even"` — only used when
    /// `DocumentSettings.even_and_odd_headers` is `true` and the page
    /// number is even.
    Even,
}

/// Per-role header / footer references. The reader fills the slots based
/// on each `<w:headerReference w:type="…" r:id="…"/>` in a section; a
/// `None` slot means "inherit from the previous section" (§17.10.3) —
/// resolved by [`resolve_hf_inheritance`], NOT by a same-section
/// default-fallback.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct HeaderFooterRefs {
    pub default: Option<String>,
    pub first: Option<String>,
    pub even: Option<String>,
}

impl HeaderFooterRefs {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.first.is_none() && self.even.is_none()
    }

    /// Set the slot for `role`; replaces any existing value.
    pub fn set(&mut self, role: HeaderFooterRole, rid: String) {
        match role {
            HeaderFooterRole::Default => self.default = Some(rid),
            HeaderFooterRole::First => self.first = Some(rid),
            HeaderFooterRole::Even => self.even = Some(rid),
        }
    }

    /// The slot for `role`, exactly. Issue #70 REMOVED the old
    /// role→Default same-section fallback: per §17.10.3 each role
    /// inherits independently ACROSS sections ([`resolve_hf_inheritance`])
    /// and an exhausted chain means a BLANK band — Word observably shows
    /// an empty first-page header when titlePg is on with no first ref,
    /// not the Default content bleeding through.
    pub fn resolve(&self, role: HeaderFooterRole) -> Option<&str> {
        match role {
            HeaderFooterRole::Default => self.default.as_deref(),
            HeaderFooterRole::First => self.first.as_deref(),
            HeaderFooterRole::Even => self.even.as_deref(),
        }
    }

    /// Backfill every `None` slot from `from` — the §17.10.3 forward
    /// fold's single step.
    fn inherit_missing_from(&mut self, from: &HeaderFooterRefs) {
        if self.default.is_none() {
            self.default = from.default.clone();
        }
        if self.first.is_none() {
            self.first = from.first.clone();
        }
        if self.even.is_none() {
            self.even = from.even.clone();
        }
    }
}

/// Issue #70 — §17.10.3 Link-to-Previous inheritance, derived at
/// consumption time (storage stays absence-based so the writer
/// round-trips Word's linked sections faithfully). Returns one
/// fully-resolved `(header_refs, footer_refs)` pair per section: a
/// section's own slot wins; a `None` slot carries the nearest earlier
/// section's resolved slot; still-`None` after section 0 = blank band.
///
/// Deliberately NOT stored on [`Section`]/[`SectionProps`]:
/// `insert_section_break_at` copies `SectionProps::from(&Section)` into
/// storage, and resolved refs riding that copy would silently
/// denormalize the linked state.
pub fn resolve_hf_inheritance(sections: &[Section]) -> Vec<(HeaderFooterRefs, HeaderFooterRefs)> {
    let mut out = Vec::with_capacity(sections.len());
    let mut carried_h = HeaderFooterRefs::default();
    let mut carried_f = HeaderFooterRefs::default();
    for section in sections {
        let mut h = section.header_refs.clone();
        let mut f = section.footer_refs.clone();
        h.inherit_missing_from(&carried_h);
        f.inherit_missing_from(&carried_f);
        carried_h = h.clone();
        carried_f = f.clone();
        out.push((h, f));
    }
    out
}

/// Audit gap A.H2 — `<w:sectPr><w:cols/>` descriptor. Holds the column
/// count and inter-column gutter for a section; equal-width snake flow
/// is the only supported layout this sprint (uneven `<w:col>` child
/// widths fall back to equal partitioning). Gutter is layout pixels
/// converted from twips at parse time.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct ColumnSpec {
    pub count: u8,
    pub gutter_pt: f32,
}

impl ColumnSpec {
    /// Word's stock single-column body — `<w:cols>` absent or
    /// `w:num="1"`; gutter is irrelevant when `count == 1`.
    pub const fn single() -> Self {
        Self {
            count: 1,
            gutter_pt: 0.0,
        }
    }

    /// Build from raw twips. `<w:cols w:space>` defaults to 720 twips
    /// (½ inch / 36 pt) per OOXML when absent; callers pass the parsed
    /// value through unchanged. `num == 0` collapses to single column
    /// — defensive against malformed files.
    pub fn from_twips(num: u8, space_twips: i32) -> Self {
        Self {
            count: num.max(1),
            gutter_pt: (space_twips as f32) / 20.0,
        }
    }

    pub fn is_multi(self) -> bool {
        self.count > 1
    }
}

impl Default for ColumnSpec {
    fn default() -> Self {
        Self::single()
    }
}

/// One OOXML `<w:sectPr>` worth of state. A section spans a contiguous
/// half-open block range `[start, end)`; the page geometry is applied to
/// every page the paginator emits while flowing those blocks. The
/// reference structs carry the relationship ids the reader captured —
/// the header / footer XML parts live in the archive's `other_entries`
/// for the passthrough writer.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Section {
    pub geometry: PageGeometry,
    /// First top-level block (inclusive) covered by this section.
    pub start_block: u32,
    /// One past the last top-level block (exclusive).
    pub end_block: u32,
    /// `<w:headerReference>` table, keyed by `w:type`.
    pub header_refs: HeaderFooterRefs,
    /// `<w:footerReference>` table, keyed by `w:type`.
    pub footer_refs: HeaderFooterRefs,
    /// `<w:titlePg/>` — when `true`, the first page of this section uses
    /// the `First` header / footer slot instead of `Default`.
    pub title_pg: bool,
    /// Audit gap A.H2 — `<w:cols>` descriptor. `Default` is the implicit
    /// single-column body; multi-column sections snake-flow inside the
    /// section's page geometry.
    pub columns: ColumnSpec,
    /// Audit gap A.M11 — `<w:pgNumType>` descriptor. Controls section-
    /// relative `PAGE` field rendering (start value + number format).
    /// `Default` keeps the doc-wide absolute page count.
    pub page_num: PageNumType,
    /// Audit gap A.M12 — `<w:sectPr><w:type w:val>`. Default `NextPage`
    /// forces a page break at section start; `Continuous` flows the
    /// new section directly below the previous one on the SAME page.
    /// `EvenPage` / `OddPage` round-trip but degrade to `NextPage`
    /// (parity routing is paginator work deferred to a later sprint).
    pub section_type: SectionType,
    /// Issue #80 — `<w:sectPr><w:footnotePr>` overrides for this section
    /// (unset fields inherit the document-level `settings.xml` props).
    pub footnote_props: NoteProps,
    /// Issue #80 — `<w:sectPr><w:endnotePr>` overrides.
    pub endnote_props: NoteProps,
    /// Issue #112 — the raw `<w:sectPr>…</w:sectPr>` bytes this section
    /// was read from (see [`SectionProps::source_xml`]).
    #[serde(default, with = "serde_bytes")]
    pub source_xml: Option<Vec<u8>>,
}

/// Audit gap A.M11 — `<w:pgNumType>` descriptor.
///
/// `start: Some(n)` restarts the section's page numbering at `n`; the
/// paginator's PAGE-field evaluator uses `(current_doc_page -
/// section_first_page + n)` instead of the absolute count. `None`
/// keeps doc-wide numbering. `format` picks the glyph set: decimal,
/// lower/upper roman, lower/upper letter — anything else falls back
/// to decimal so an unrecognised `w:fmt` doesn't crash the paginator.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(default)]
pub struct PageNumType {
    pub start: Option<u32>,
    pub format: PageNumFormat,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageNumFormat {
    #[default]
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerLetter,
    UpperLetter,
}

impl PageNumFormat {
    /// Audit gap A.M11 — render a 1-based page number under this
    /// format. Roman conversion clamps to the 1..=3999 range (above
    /// that the classical Roman system has no glyphs — beyond Word's
    /// supported range too). Letter format cycles A-Z / AA-ZZ / ...
    pub fn render(self, n: u32) -> String {
        match self {
            PageNumFormat::Decimal => n.to_string(),
            PageNumFormat::LowerRoman => to_roman(n).to_lowercase(),
            PageNumFormat::UpperRoman => to_roman(n),
            PageNumFormat::LowerLetter => to_letter(n, false),
            PageNumFormat::UpperLetter => to_letter(n, true),
        }
    }
}

fn to_roman(mut n: u32) -> String {
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    let table: &[(u32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for &(v, s) in table {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

fn to_letter(n: u32, upper: bool) -> String {
    if n == 0 {
        return n.to_string();
    }
    let base = if upper { b'A' } else { b'a' };
    /* Word's `lowerLetter` / `upperLetter`: 1..=26 → A..Z; 27..=52 →
    AA..ZZ (NOT base-26 — letters REPEAT). Match that quirk. */
    let count = ((n - 1) / 26) + 1;
    let letter = base + ((n - 1) % 26) as u8;
    let mut out = String::with_capacity(count as usize);
    for _ in 0..count {
        out.push(letter as char);
    }
    out
}

/// Audit gap A.M12 — `<w:sectPr><w:type>` discriminator.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SectionType {
    /// Section starts on a fresh page (the default when `<w:type>` is
    /// absent).
    #[default]
    NextPage,
    /// New section flows in-line on the same page — the paginator does
    /// NOT flush before swapping geometry. Used for layouts like
    /// "1-column title, then 2-column body on the same page".
    Continuous,
    /// Round-trips through reader / writer but degrades to `NextPage`
    /// in the paginator until parity-aware page routing lands.
    EvenPage,
    OddPage,
}

impl Section {
    /// Width of one column in this section's page geometry, in layout
    /// pixels. For a single-column section that's just `content_width`.
    pub fn column_width_pt(&self) -> f32 {
        let cw = self.geometry.content_width();
        let n = self.columns.count.max(1) as f32;
        if n <= 1.0 {
            return cw;
        }
        let gutters = (n - 1.0) * self.columns.gutter_pt;
        ((cw - gutters) / n).max(0.0)
    }

    /// Distance (in layout pixels) from the section's content-area
    /// leading edge to the leading edge of column `idx`.
    pub fn column_x_offset_pt(&self, idx: u8) -> f32 {
        let cw = self.column_width_pt();
        (idx.min(self.columns.count.saturating_sub(1)) as f32) * (cw + self.columns.gutter_pt)
    }
}

/// Phase 3 (#40) — the range-free payload of one `<w:sectPr>`: everything
/// a [`Section`] carries except the derived `[start_block, end_block)`
/// coverage. Two homes:
///
/// - [`Paragraph::section_end`] — a paragraph whose mark terminates a
///   mid-document section stores that section's properties here
///   (OOXML-faithful: an interior `<w:sectPr>` IS a `<w:pPr>` child of
///   the section's last paragraph).
/// - [`DocumentTree::body_section`] — the body-level trailing
///   `<w:sectPr>` governing the final section.
///
/// Block ranges are NEVER stored — [`DocumentTree::effective_sections`]
/// derives them by walking the block list, so section boundaries ride
/// their paragraphs through every insert/delete/split/merge and cannot
/// desync (the pre-Phase-3 `Vec<Section>` range-stamping never
/// re-indexed on block-count changes, corrupting multi-section docs on
/// the first edit).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct SectionProps {
    pub geometry: PageGeometry,
    /// `<w:headerReference>` table, keyed by `w:type`.
    pub header_refs: HeaderFooterRefs,
    /// `<w:footerReference>` table, keyed by `w:type`.
    pub footer_refs: HeaderFooterRefs,
    /// `<w:titlePg/>`.
    pub title_pg: bool,
    /// `<w:cols>` descriptor.
    pub columns: ColumnSpec,
    /// `<w:pgNumType>` descriptor.
    pub page_num: PageNumType,
    /// `<w:type w:val>` — how this section BEGINS relative to the
    /// previous one (§17.6.22: the kind of break that precedes this
    /// section's content).
    pub section_type: SectionType,
    /// Issue #80 — `<w:footnotePr>` overrides.
    pub footnote_props: NoteProps,
    /// Issue #80 — `<w:endnotePr>` overrides.
    pub endnote_props: NoteProps,
    /// Issue #112 — the raw `<w:sectPr>…</w:sectPr>` bytes these
    /// properties were read from. The `.docx` writer re-emits them
    /// verbatim as long as a re-parse of the bytes still yields these
    /// exact properties (a *verified* passthrough: `<w:docGrid>`,
    /// `w:rsidSect`, `w:gutter`, `<w:cols w:space>` and every other
    /// unmodeled child survive a zero-edit resave), and regenerates from
    /// the typed fields the moment page setup, a header reference or the
    /// section type was changed in the editor. `None` for an
    /// engine-authored section. Ignored by equality-of-properties checks.
    #[serde(default, with = "serde_bytes")]
    pub source_xml: Option<Vec<u8>>,
}

impl SectionProps {
    /// Materialize a derived [`Section`] covering `[start_block, end_block)`.
    pub fn into_section(self, start_block: u32, end_block: u32) -> Section {
        Section {
            geometry: self.geometry,
            start_block,
            end_block,
            header_refs: self.header_refs,
            footer_refs: self.footer_refs,
            title_pg: self.title_pg,
            columns: self.columns,
            page_num: self.page_num,
            section_type: self.section_type,
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            source_xml: self.source_xml,
        }
    }

    /// Issue #112 — the typed properties only, `source_xml` cleared: what
    /// the writer compares a re-parse of the source bytes against.
    pub fn without_source(&self) -> Self {
        Self {
            source_xml: None,
            ..self.clone()
        }
    }
}

impl From<&Section> for SectionProps {
    fn from(s: &Section) -> Self {
        Self {
            geometry: s.geometry,
            header_refs: s.header_refs.clone(),
            footer_refs: s.footer_refs.clone(),
            title_pg: s.title_pg,
            columns: s.columns,
            page_num: s.page_num,
            section_type: s.section_type,
            footnote_props: s.footnote_props,
            endnote_props: s.endnote_props,
            source_xml: s.source_xml.clone(),
        }
    }
}

/// Phase 3 (#39) — per-part dirty tracking for header/footer stories,
/// keyed by relationship id. Lives IN the tree (mirroring `styles_dirty`
/// and `numbering.dirty`) so undo reverts the flag together with the
/// content: an edit-then-undo leaves the writer on the byte-identical
/// passthrough path for that part.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct HfDirty {
    pub headers: std::collections::BTreeSet<String>,
    pub footers: std::collections::BTreeSet<String>,
}

impl HfDirty {
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty() && self.footers.is_empty()
    }
}

/// Document-wide flags pulled from `word/settings.xml`. Phase 2 — only
/// the header/footer parity toggle is modelled; later phases grow the
/// struct as more setting elements get typed support.
///
/// `Default` is hand-written (not derived) because [`Self::
/// widow_control_default`] must default to `true` — a derived
/// `#[serde(default)]` struct-level attribute fills missing fields from
/// `DocumentSettings::default()`, so the manual impl IS what `read_docx`
/// (unversioned) and any `#[serde(default)]` deserialize of a partial
/// settings blob fall back to.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct DocumentSettings {
    /// `<w:evenAndOddHeaders/>` — when `true`, even-numbered pages render
    /// the `Even` header / footer instead of the `Default` slot.
    pub even_and_odd_headers: bool,
    /// Issue #77 — `docProps/core.xml` `<dc:creator>`, the value the
    /// `AUTHOR` field resolves to. Read-only ingest: the writer never
    /// regenerates the core-properties part (it rides the OPC
    /// passthrough), so `None` on documents without one.
    pub author: Option<String>,
    /// Issue #109 — the [`DefaultPageSize`] a `.docx` reader was asked to
    /// fall back to for any `<w:sectPr>` that omits `<w:pgSz>`. This is
    /// never read FROM the archive (OOXML has no such setting — Word
    /// always stamps `pgSz` explicitly); it records what the *host*
    /// requested via `format_docx::read_docx_with_settings` so the value
    /// stays inspectable after parsing. `read_docx` (unchanged) always
    /// leaves this at the `#[default]` `A4`.
    pub default_page_size: DefaultPageSize,
    /// Issue #179 — the effective `<w:widowControl>` when a paragraph's
    /// resolved [`ParaProperties::widow_control`] is `None` (never
    /// specified anywhere in the cascade). ECMA-376 says an absent
    /// element means the constraint is NOT applied; #95 chose `true`
    /// instead — Word's actual application default, and what every
    /// pinned fingerprint assumes. This is a per-host override of that
    /// choice (like [`Self::default_page_size`]), never read FROM the
    /// archive: `read_docx` always leaves it at the `#[default]` `true`,
    /// and only a host calling `format_docx::read_docx_with_settings`
    /// with the strict ECMA-376 reading sets it `false`.
    pub widow_control_default: bool,
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self {
            even_and_odd_headers: false,
            author: None,
            default_page_size: DefaultPageSize::default(),
            widow_control_default: true,
        }
    }
}

/* ============================================================
Issue #80 — footnotes & endnotes model.
============================================================ */

/// Issue #80 — which note family a story or a reference belongs to.
/// Footnotes negotiate page-bottom space with the body flow; endnotes
/// collect as a trailing story at section / document end.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NoteKind {
    #[default]
    Footnote,
    Endnote,
}

/// Issue #80 — `w:type` of a `<w:footnote>` / `<w:endnote>` entry
/// (ECMA-376 §17.11.17 / §17.11.9). The three special kinds are the
/// document's separator stories: Word always ships a
/// `continuationSeparator` (`w:id="-1"`) and a `separator` (`w:id="0"`);
/// a `continuationNotice` is authored on demand. None of them is ever
/// referenced from body text.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoteType {
    #[default]
    Normal,
    Separator,
    ContinuationSeparator,
    ContinuationNotice,
}

/// Issue #80 — one note story: the body of a `<w:footnote>` /
/// `<w:endnote>` entry. `body` is the body's own block model (paragraphs
/// carry the `<w:footnoteRef/>` self-mark as an
/// [`InlineKind::NoteSelfRef`] anchor), so the story adapter, the
/// paginator and the writer reuse every body code path. `source_xml`
/// is the raw `<w:footnote …>…</w:footnote>` element for the passthrough
/// writer; `dirty` flips on the first engine mutation and forces a
/// regenerate of THIS entry only.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct NoteStory {
    pub id: i32,
    pub kind: NoteKind,
    pub note_type: NoteType,
    pub body: Vec<Block>,
    #[serde(with = "serde_bytes")]
    pub source_xml: Option<Vec<u8>>,
    pub dirty: bool,
}

/// Issue #80 — `<w:footnotePr><w:pos>` / `<w:endnotePr><w:pos>`
/// (§17.11.21 / §17.11.13). Footnotes: `pageBottom` (default) or
/// `beneathText`; the section / document-end values are meaningless for
/// footnotes in Word and behave as `beneathText`. Endnotes: `sectEnd` or
/// `docEnd` (default).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotePosition {
    #[default]
    PageBottom,
    BeneathText,
    SectEnd,
    DocEnd,
}

/// Issue #80 — `<w:numRestart w:val>` (§17.11.19). `EachPage` is
/// footnote-only per the schema. The document-order derivation
/// ([`DocumentTree::note_markers`]) numbers it as `Continuous` — the page
/// a reference lands on is only known after pagination — and the layout
/// post-pass (issue #129) relabels the footnotes of every
/// [`DocumentTree::each_page_note_numbering`] section per page.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoteNumRestart {
    #[default]
    Continuous,
    EachSect,
    EachPage,
}

/// Issue #80 — the payload of one `<w:footnotePr>` / `<w:endnotePr>`.
/// Every field is optional so a section-level element can override a
/// single property and inherit the rest from the document level
/// (`settings.xml`); [`DocumentTree::resolved_note_props`] folds the two
/// with the schema defaults.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(default)]
pub struct NoteProps {
    pub position: Option<NotePosition>,
    /// `<w:numFmt w:val>` — the same `ST_NumberFormat` subset page
    /// numbering models; unknown formats (chicago, …) read as decimal.
    pub num_format: Option<PageNumFormat>,
    /// `<w:numStart w:val>` — first number of the sequence.
    pub num_start: Option<u32>,
    pub num_restart: Option<NoteNumRestart>,
}

impl NoteProps {
    /// `true` when nothing is set — the writer omits the element.
    pub fn is_empty(&self) -> bool {
        self.position.is_none()
            && self.num_format.is_none()
            && self.num_start.is_none()
            && self.num_restart.is_none()
    }

    /// `self` with every unset field taken from `base`.
    pub fn inherit_from(self, base: &NoteProps) -> NoteProps {
        NoteProps {
            position: self.position.or(base.position),
            num_format: self.num_format.or(base.num_format),
            num_start: self.num_start.or(base.num_start),
            num_restart: self.num_restart.or(base.num_restart),
        }
    }
}

/// Issue #80 — fully-resolved note properties for one kind in one
/// section (schema defaults applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedNoteProps {
    pub position: NotePosition,
    pub num_format: PageNumFormat,
    pub num_start: u32,
    pub num_restart: NoteNumRestart,
}

/// Issue #80 — which note parts the writer regenerates. Flips when a
/// story is added, edited or removed; mirrors [`HfDirty`].
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct NotesDirty {
    pub footnotes: bool,
    pub endnotes: bool,
}

/// Issue #80 — the key every numbering / layout table uses for one
/// referenced note: `(kind, w:id)`. Shared with the layout crate so a
/// glyph anchor and a laid-out note body agree by construction.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoteAnchor {
    pub kind: NoteKind,
    pub id: u32,
}

/// Issue #278 — the story container a note reference sits in. The
/// numbering walk visits every container that paints; consumers that
/// care (the a11y mirror, tests) key on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NoteContainer {
    /// A top-level body paragraph.
    #[default]
    Body,
    /// A paragraph inside a body table cell (any nesting depth).
    TableCell,
    /// A paragraph inside a text-box story (any nesting depth, cells of
    /// a table inside the story included) anchored in the body.
    TextBox,
    /// A header part (its cells and text boxes included).
    Header,
    /// A footer part (its cells and text boxes included).
    Footer,
}

/// Issue #80 — one document-order note reference: the top-level block
/// that carries it, the anchor, and whether the author supplied a
/// custom mark (`w:customMarkFollows` — the sequence skips it).
///
/// Issue #278 — `container` says where the reference sits. For a
/// reference in a table cell or a text box, `top_block` is the
/// top-level block hosting the cell / the box's anchor paragraph; for a
/// header / footer reference it is the `start_block` of the first
/// section whose pages paint that part (the band opens that section's
/// first page, so it numbers ahead of the section's body references).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteReference {
    pub top_block: u32,
    pub anchor: NoteAnchor,
    pub custom_mark: bool,
    pub container: NoteContainer,
}

/// Address of a `Block` inside a `DocumentTree`. Walks from the root
/// `blocks: Vector<Block>` down through table cells. Phase 5 PR 3.
///
/// Examples:
/// - `BlockPath { steps: vec![PathStep::Block(2)] }` — the 3rd top-level
///   block.
/// - `BlockPath { steps: vec![PathStep::Block(2), PathStep::Cell{row:1,
///   col:0}, PathStep::Block(0)] }` — the first paragraph in the
///   top-left cell of row 1 of the 3rd top-level block (which must be
///   `Block::Table`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct BlockPath {
    pub steps: Vec<PathStep>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathStep {
    /// Index into the current `&[Block]` / `&Vector<Block>`.
    Block(u32),
    /// Step from a `Block::Table` into one of its cells.
    Cell { row: u32, col: u32 },
}

impl BlockPath {
    /// Empty path — addresses the root `blocks` container itself.
    pub fn root() -> Self {
        Self::default()
    }
    /// Path to the Nth top-level block.
    pub fn top(idx: u32) -> Self {
        Self {
            steps: vec![PathStep::Block(idx)],
        }
    }
    pub fn push(mut self, step: PathStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Parent container path (every step except the last).
    pub fn parent(&self) -> Self {
        let mut steps = self.steps.clone();
        steps.pop();
        Self { steps }
    }

    /// The path's final `Block`-step index, when one terminates the
    /// path. `None` when the path is empty or its last step is `Cell`.
    pub fn last_block_index(&self) -> Option<u32> {
        match self.steps.last()? {
            PathStep::Block(n) => Some(*n),
            PathStep::Cell { .. } => None,
        }
    }

    /// `true` when this path is a prefix of `descendant` (or equal).
    pub fn is_ancestor_of(&self, descendant: &Self) -> bool {
        if self.steps.len() > descendant.steps.len() {
            return false;
        }
        self.steps
            .iter()
            .zip(descendant.steps.iter())
            .all(|(a, b)| a == b)
    }

    /// Compare two paths in document order (depth-first walk). Used to
    /// canonicalize selection endpoints before edit/range operations.
    pub fn cmp_doc_order(&self, other: &Self) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        let n = self.steps.len().min(other.steps.len());
        for i in 0..n {
            let ord = match (&self.steps[i], &other.steps[i]) {
                (PathStep::Block(a), PathStep::Block(b)) => a.cmp(b),
                (PathStep::Cell { row: r1, col: c1 }, PathStep::Cell { row: r2, col: c2 }) => {
                    r1.cmp(r2).then_with(|| c1.cmp(c2))
                }
                /* Shape mismatch in well-formed paths is unreachable
                (a Cell step always follows a Block step that descends
                into a table). Compare by surface index when it
                happens — keeps doc-order stable. */
                (PathStep::Block(a), PathStep::Cell { row: b, .. }) => a.cmp(b),
                (PathStep::Cell { row: a, .. }, PathStep::Block(b)) => a.cmp(b),
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        self.steps.len().cmp(&other.steps.len())
    }
}

/// A selectable font family (Backlog #9 / core-engine issue #23). `engine-wasm`
/// resolves it to a loaded font face when building layout style spans; the pure
/// document model just stores the choice.
///
/// The three named variants are the engine's seed faces and keep their
/// canonical asymmetric id ↔ display mappings ("liberation" ↔ "Liberation
/// Sans"). [`Custom`](FontFamily::Custom) is the dynamic, string-backed slot:
/// `id` is the FontStack resolution id + toolbar id (e.g. `"cairo"`), `display`
/// is the verbatim `.docx`/CSS family name. Carrying both makes a document
/// round-trip byte-identically while still letting the layout engine resolve
/// the loaded face. `Custom` holds owned `String`s, so the enum is `Clone` but
/// **not** `Copy` — pass it by reference.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Amiri,
    LiberationSans,
    NotoNaskhArabic,
    Custom { id: String, display: String },
}

impl FontFamily {
    /// The FontStack resolution id + toolbar id. Named faces keep their
    /// canonical lowercase-hyphenated id; a [`Custom`](FontFamily::Custom)
    /// face returns its stored id verbatim.
    pub fn id(&self) -> &str {
        match self {
            FontFamily::Amiri => "amiri",
            FontFamily::LiberationSans => "liberation",
            FontFamily::NotoNaskhArabic => "noto-naskh",
            FontFamily::Custom { id, .. } => id,
        }
    }

    /// Human-facing family name for `.docx` `<w:rFonts>` and CSS
    /// `font-family`. A custom face returns the verbatim display string so a
    /// document round-trips byte-identically.
    pub fn display_name(&self) -> &str {
        match self {
            FontFamily::Amiri => "Amiri",
            FontFamily::LiberationSans => "Liberation Sans",
            FontFamily::NotoNaskhArabic => "Noto Naskh Arabic",
            FontFamily::Custom { display, .. } => display,
        }
    }

    /// Parse a toolbar / resolution id (e.g. `"amiri"`, `"cairo"`) into a
    /// family. Unknown ids become a [`Custom`](FontFamily::Custom) face whose
    /// display name is humanized from the id. Empty input yields `None`.
    pub fn from_id(id: &str) -> Option<FontFamily> {
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        Some(match id.to_ascii_lowercase().as_str() {
            "amiri" => FontFamily::Amiri,
            "liberation" => FontFamily::LiberationSans,
            "noto-naskh" => FontFamily::NotoNaskhArabic,
            _ => FontFamily::Custom {
                id: id.to_string(),
                display: humanize_font_id(id),
            },
        })
    }

    /// Parse a display / `.docx` / CSS family name (e.g. `"Amiri"`,
    /// `"Liberation Sans"`, `"Cairo"`) into a family. Unknown names become a
    /// [`Custom`](FontFamily::Custom) face that preserves the **verbatim**
    /// display string and derives a resolution id by slugifying it. Empty
    /// input yields `None`.
    ///
    /// The trimmed, unquoted form is used **only** to match a seed face and to
    /// derive the Custom id — the stored `display` is the caller's untouched
    /// input. This is load-bearing for `.docx` byte-identity: the docx reader
    /// (`format-docx`'s `family_from_docx`) passes the raw `<w:rFonts>`
    /// attribute value, which must round-trip byte-for-byte (surrounding
    /// whitespace and any literal quote characters included). The CSS parser,
    /// where quoting/padding are syntax rather than data, pre-cleans the token
    /// before calling (see `engine::html`'s `family_from_name`).
    pub fn from_display_name(name: &str) -> Option<FontFamily> {
        let key = name.trim().trim_matches(['"', '\'']).trim();
        if key.is_empty() {
            return None;
        }
        Some(match key.to_ascii_lowercase().as_str() {
            "amiri" => FontFamily::Amiri,
            "liberation sans" | "liberation" => FontFamily::LiberationSans,
            "noto naskh arabic" | "noto-naskh" => FontFamily::NotoNaskhArabic,
            _ => FontFamily::Custom {
                id: slugify_font_name(key),
                display: name.to_string(),
            },
        })
    }
}

/// Humanize a font id into a display name: split on `-`, upper-case the first
/// letter of each word. `"cairo"` → `"Cairo"`, `"noto-naskh-arabic"` →
/// `"Noto Naskh Arabic"`.
fn humanize_font_id(id: &str) -> String {
    id.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Slugify a display name into a resolution id: lowercase, collapse whitespace
/// runs to a single `-`. `"Cairo"` → `"cairo"`, `"Times New Roman"` →
/// `"times-new-roman"`.
fn slugify_font_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// Underline decoration style — OOXML `<w:u w:val="…"/>` variants the
/// engine carries through layout + render. `Single` matches the legacy
/// boolean-true behaviour; `None` matches boolean-false. The renderer
/// approximates `Dotted` / `Dashed` / `Wavy` with patterned fill rects
/// (Canvas2D backend has no native dash array on the underline path).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

impl UnderlineStyle {
    /// `true` when the variant should paint any stroke at all. `None`
    /// is the only variant that suppresses painting.
    pub fn is_visible(self) -> bool {
        !matches!(self, UnderlineStyle::None)
    }
}

/// Audit gap A.M1 — `<w:vertAlign>` super/subscript positioning. The
/// renderer shrinks the run's font and shifts the baseline up
/// (`Superscript`) or down (`Subscript`); `Baseline` is the implicit
/// default and a no-op.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VertAlign {
    #[default]
    Baseline,
    Superscript,
    Subscript,
}

impl VertAlign {
    /// `true` when the variant alters baseline / font size at paint time.
    pub fn is_shifted(self) -> bool {
        !matches!(self, VertAlign::Baseline)
    }
}

/// Issue #84 — an in-part OOXML **grab bag**: the raw XML fragments of
/// every child element the `.docx` reader saw inside a property container
/// (`<w:rPr>`, `<w:pPr>`, `<w:tblPr>`, `<w:trPr>`, `<w:tcPr>`) but does
/// not model. A dirty paragraph / table regenerates from the typed model,
/// so anything the model cannot express would otherwise vanish on the
/// first edit (the `[HIDDEN GAP - UNHANDLED]` class of the ECMA-376
/// audit). The bag converts that whole long tail to "preserved verbatim":
/// the writer re-emits each fragment byte-for-byte, interleaved with the
/// modeled children in schema order.
///
/// Semantics — the bag is an opaque attachment, **not** a formatting
/// property:
///
/// - fragments are complete elements (`<w:framePr …/>`,
///   `<w:rPrChange>…</w:rPrChange>`), namespace prefixes intact, in source
///   document order;
/// - layout / render never read it;
/// - on span split (`Paragraph::split_at`, `apply_style` re-derivation)
///   both halves clone it; adjacent spans coalesce only when their whole
///   `SpanStyle` — bag included — is byte-equal;
/// - the cascade never inherits it: a merge keeps the *patch's* bag when
///   the patch carries one, else the receiver's, so a style definition's
///   bag can never leak into a run as direct formatting (style sources
///   simply never carry one).
///
/// Boxed behind an `Option` on every host struct so the common no-bag case
/// costs one pointer-width and `SpanStyle::default()` stays cheap to
/// compare.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct GrabBag {
    /// Raw child-element fragments (UTF-8 XML bytes), document order.
    pub fragments: Vec<Vec<u8>>,
}

impl GrabBag {
    /// Append `fragment` to the bag behind `slot`, allocating the box on
    /// first use. The reader's one-liner for every unmodeled child.
    pub fn push_into(slot: &mut Option<Box<GrabBag>>, fragment: Vec<u8>) {
        slot.get_or_insert_with(Default::default)
            .fragments
            .push(fragment);
    }

    /// Fragments of `slot`, or an empty slice when there is no bag.
    pub fn fragments_of(slot: &Option<Box<GrabBag>>) -> &[Vec<u8>] {
        slot.as_deref().map_or(&[], |b| b.fragments.as_slice())
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

/// Issues #199 / #106 — one raw XML attribute captured from a source
/// element the model reads only partially (`<w:p w:rsidR="…">`,
/// `<w:r w:rsidRPr="…">`, `<w:t xml:space="preserve">`). `name` is the
/// qualified name as written; `value` is the **escaped** source form, pasted
/// back verbatim into a double-quoted attribute position by the writer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SourceAttr {
    pub name: String,
    pub value: String,
    /// Issue #248 — the whitespace written before the attribute when it
    /// is not a single space (a pretty-printed start tag that breaks its
    /// attributes over several lines). `None` = one space. Skipped when
    /// `None`, so a pre-#248 snapshot encodes unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws: Option<String>,
}

/// Issues #199 / #106 — the paragraph's own `<w:pPr>` as read, plus the
/// model state it produced. The writer re-emits `xml` verbatim only while
/// the paragraph's `props` / `style_id` / `list_item` still equal the
/// recorded ones and it carries no section marker (a *verified*
/// passthrough — any formatting edit falls back to regeneration). `xml`
/// also carries the whitespace between the `<w:p>` start tag and the
/// `<w:pPr>` of a pretty-printed part; it is empty for a source paragraph
/// without a `<w:pPr>`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct SourcePPr {
    #[serde(with = "serde_bytes")]
    pub xml: Vec<u8>,
    pub props: ParaProperties,
    pub style_id: Option<String>,
    pub list_item: Option<ListItem>,
    /// Issue #262 — the paragraph-mark revision `xml` spells (its
    /// `<w:rPr><w:ins/>`): the bytes are re-emitted only while the
    /// paragraph still carries exactly this one. Skipped when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark_revision: Option<Revision>,
}

/// Issues #199 / #106 — one source `<w:r>` covering the text bytes
/// `[start, end)` of its paragraph.
///
/// - `attrs` — the `<w:r>` attributes (`w:rsidR`, `w:rsidRPr`, `w:rsidDel`,
///   …), re-emitted on every regenerated run inside the range.
/// - `rpr` — the raw `<w:rPr>…</w:rPr>`, re-emitted verbatim while the
///   paragraph's style at the run still equals `style` (verified).
/// - `lead` — unmodeled leading run content (`<w:lastRenderedPageBreak/>`),
///   re-emitted in the range's first regenerated run.
/// - `t_attrs` — the first `<w:t>`'s attributes (`None`: the run had no
///   `<w:t>`), so a run Word wrote as a bare `<w:t>` does not grow an
///   `xml:space="preserve"` unless its text now needs one.
///
/// Travel rule: the range follows the text like a style span, except that
/// an insertion exactly at the run's END also extends it (typing at the end
/// of a run continues that run, as in Word). A run split by an insertion or
/// a style change therefore keeps its attributes on *every* regenerated
/// piece — the inserted text included; the engine does not mint rsids.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct SourceRun {
    pub start: u32,
    pub end: u32,
    pub attrs: Vec<SourceAttr>,
    #[serde(with = "serde_bytes")]
    pub rpr: Option<Vec<u8>>,
    pub style: SpanStyle,
    #[serde(with = "serde_bytes")]
    pub lead: Vec<u8>,
    pub t_attrs: Option<Vec<SourceAttr>>,
    /// Issue #245 — the pretty-print whitespace inside the source `<w:r>`
    /// (`None` for a compact part). Skipped when `None`, so a pre-#245
    /// snapshot encodes unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pad: Option<Box<RunPad>>,
    /// Issue #245 — the source wrote this run's text with edge whitespace
    /// in a bare `<w:t>` (no `xml:space`); the reader kept the whitespace,
    /// so the writer keeps the source spelling instead of adding
    /// `xml:space="preserve"` to text it did not change the meaning of.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bare_edge_ws: bool,
}

/// Issue #245 — whitespace between the children of a pretty-printed
/// source `<w:r>`: after the start tag (`open`), after the `<w:rPr>`
/// (`after_rpr`) and before the end tag (`close`). Re-emitted on every
/// regenerated piece of the run, so an edit inside a pretty-printed part
/// rewrites only the bytes it changed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct RunPad {
    #[serde(with = "serde_bytes")]
    pub open: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub after_rpr: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub close: Vec<u8>,
}

/// Issues #199 / #106 — unmodeled in-paragraph markup at text offset `at`:
/// `<w:proofErr/>`, a non-TOC `<w:bookmarkStart/>` / `<w:bookmarkEnd/>`,
/// `<w:permStart/>`, a text-less run holding only unmodeled content, the
/// whitespace of a pretty-printed part. Re-emitted verbatim between the
/// regenerated runs at its offset.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SourceMarker {
    pub at: u32,
    #[serde(with = "serde_bytes")]
    pub xml: Vec<u8>,
    /// What the bytes are to the writer (issue #244). Skipped when
    /// [`MarkerRole::Verbatim`], so a pre-#244 snapshot encodes unchanged.
    #[serde(skip_serializing_if = "MarkerRole::is_verbatim")]
    pub role: MarkerRole,
    /// Issue #243 — `Some` when the marker is a comment anchor: a
    /// `<w:commentRangeStart/>` / `<w:commentRangeEnd/>` or the run holding
    /// a `<w:commentReference/>`. Unlike every other marker it is NOT
    /// replayed blindly: the writer checks it against the tree-level
    /// [`DocumentTree::comment_ranges`] / [`DocumentTree::comment_defs`]
    /// (a deleted comment's anchor is dropped, a moved one is re-emitted
    /// where the tree says).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<CommentAnchor>,
}

impl SourceMarker {
    /// A [`MarkerRole::Verbatim`] marker.
    pub fn verbatim(at: u32, xml: Vec<u8>) -> Self {
        Self {
            at,
            xml,
            role: MarkerRole::Verbatim,
            comment: None,
        }
    }
}

/// Issue #244 — how the writer treats a [`SourceMarker`].
///
/// - [`Self::Verbatim`]: positioned formatting-neutral markup (`proofErr`,
///   bookmarks, pretty-print whitespace). Written only while the
///   paragraph's offsets are in sync — a stale marker is dropped rather
///   than misplaced.
/// - [`Self::Content`]: unmodeled paragraph *content* kept whole, e.g. a
///   zero-result legacy form field (`FORMCHECKBOX` / `FORMDROPDOWN` — the
///   `fldChar begin … end` byte range, `<w:ffData>` included). PRD Tier 3:
///   never dropped — when the offsets go stale it is still written, at its
///   offset clamped to the text (a best-effort position beats losing the
///   control).
/// - [`Self::Open`] / [`Self::Close`] (issue #245): the two ends of an
///   unmodeled run-level WRAPPER around a text range — a `<w:sdt>` content
///   control. `xml` of the opener is `<w:sdt>…<w:sdtPr>…</w:sdtPr>
///   <w:sdtContent>`, of the closer `</w:sdtContent></w:sdt>`; `id` pairs
///   them. They travel with the text like any marker (an insertion at the
///   closer's offset lands INSIDE the control, as typing at the end of a
///   run continues it). The writer pairs them with a stack and keeps the
///   part well-formed whatever an edit did: an opener that lost its
///   closer (a split) closes with `close_xml` at the paragraph end, a
///   closer without an opener is skipped, and a range that would cross a
///   regenerated wrapper (hyperlink, revision, field) is widened to
///   enclose it. Tier 3 like [`Self::Content`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub enum MarkerRole {
    #[default]
    Verbatim,
    Content,
    Open {
        id: u32,
        #[serde(with = "serde_bytes")]
        close_xml: Vec<u8>,
    },
    Close {
        id: u32,
    },
}

impl MarkerRole {
    pub fn is_verbatim(&self) -> bool {
        matches!(self, Self::Verbatim)
    }

    /// `true` for markup that must survive stale offsets.
    pub fn must_survive(&self) -> bool {
        !self.is_verbatim()
    }
}

/// Issue #243 — what a comment-anchor [`SourceMarker`] is.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommentAnchor {
    pub kind: CommentAnchorKind,
    /// The comment's `w:id`.
    pub id: u32,
}

/// Issue #243 — the three in-paragraph pieces of a comment's anchoring.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommentAnchorKind {
    /// `<w:commentRangeStart/>`.
    RangeStart,
    /// `<w:commentRangeEnd/>`.
    RangeEnd,
    /// The run holding `<w:commentReference/>`.
    Reference,
}

/// Issues #199 / #106 — attribute-level grab bag + in-paragraph source
/// markup of a paragraph read from a `.docx`. The element-level grab bags
/// of #84 ([`GrabBag`]) keep unmodeled `<w:pPr>` / `<w:rPr>` *children*;
/// this keeps what they cannot: the attributes of `<w:p>` / `<w:r>` /
/// `<w:t>`, the source `<w:pPr>` / `<w:rPr>` bytes (verified against the
/// model before reuse, which also keeps the unread attributes of modeled
/// children), the source run boundaries, and the in-paragraph markers.
/// A clean paragraph never consults it (its `source_xml` passthrough
/// wins); a regenerated one uses it to stay close to the source bytes.
///
/// `runs` / `markers` are byte-offset anchored and are remapped by the
/// paragraph edit primitives (`insert_text`, `delete_text`, `split_at`,
/// `concat`, inline-object splices). `text_len` is the paragraph text
/// length they were last synced to: an edit path that does not remap them
/// leaves it stale and the writer then ignores both (never misplaces them).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct SourceMarkup {
    pub text_len: u32,
    /// `<w:p>` attributes, source order (`w14:paraId`, `w:rsidR`, …).
    pub attrs: Vec<SourceAttr>,
    pub ppr: Option<SourcePPr>,
    pub runs: Vec<SourceRun>,
    pub markers: Vec<SourceMarker>,
}

/// `<w:p>` attributes that identify ONE paragraph (unique per part). The
/// second half of a split never inherits them.
const PARAGRAPH_IDENTITY_ATTRS: [&str; 2] = ["w14:paraId", "w14:textId"];

/// `text_len` of a [`SourceMarkup`] whose offsets can no longer be trusted.
const STALE_TEXT_LEN: u32 = u32::MAX;

impl SourceMarkup {
    /// `true` while `runs` / `markers` are in sync with a paragraph text of
    /// `len` bytes.
    pub fn offsets_valid(&self, len: usize) -> bool {
        self.text_len as usize == len
    }

    /// `len` bytes were inserted at `off` into a text that was `old_len`
    /// bytes long. See [`SourceRun`] for the travel rule; markers at or
    /// after `off` slide right.
    pub fn note_insert(slot: &mut Option<Box<Self>>, old_len: u32, off: u32, len: u32) {
        let Some(m) = slot.as_deref_mut() else {
            return;
        };
        if len == 0 {
            return;
        }
        text_remap::debug_assert_in_step(m, old_len);
        if m.text_len != old_len {
            m.text_len = STALE_TEXT_LEN;
            return;
        }
        /* The run the insertion extends: the one ending at (or strictly
        containing) `off`; at the paragraph start, the first run. */
        let grow = m
            .runs
            .iter()
            .position(|r| r.start < off && off <= r.end)
            .or_else(|| {
                (off == 0)
                    .then(|| m.runs.iter().position(|r| r.start == 0))
                    .flatten()
            });
        for (i, r) in m.runs.iter_mut().enumerate() {
            if Some(i) == grow {
                r.end += len;
            } else if r.start >= off {
                r.start += len;
                r.end += len;
            }
        }
        for mk in &mut m.markers {
            if mk.at >= off {
                mk.at += len;
            }
        }
        m.text_len += len;
    }

    /// Bytes `[s, e)` were removed from a text that was `old_len` bytes
    /// long. Runs clip (a fully deleted one disappears); markers inside the
    /// gap collapse to `s`.
    pub fn note_delete(slot: &mut Option<Box<Self>>, old_len: u32, s: u32, e: u32) {
        let Some(m) = slot.as_deref_mut() else {
            return;
        };
        if s >= e {
            return;
        }
        text_remap::debug_assert_in_step(m, old_len);
        if m.text_len != old_len {
            m.text_len = STALE_TEXT_LEN;
            return;
        }
        let gap = e - s;
        let map = |p: u32| -> u32 {
            if p <= s {
                p
            } else if p >= e {
                p - gap
            } else {
                s
            }
        };
        for r in &mut m.runs {
            r.start = map(r.start);
            r.end = map(r.end);
        }
        m.runs.retain(|r| r.start < r.end);
        for mk in &mut m.markers {
            mk.at = map(mk.at);
        }
        m.text_len -= gap;
    }

    /// Split for [`Paragraph::split_at`] at byte `at` of a text `old_len`
    /// bytes long. The left half keeps the paragraph identity
    /// (`w14:paraId` / `w14:textId`); the right half gets the remaining
    /// attributes (rsids) only. Markers at the split point stay left.
    pub fn split_at(
        slot: &Option<Box<Self>>,
        old_len: u32,
        at: u32,
    ) -> (Option<Box<Self>>, Option<Box<Self>>) {
        let Some(m) = slot.as_deref() else {
            return (None, None);
        };
        text_remap::debug_assert_in_step(m, old_len);
        let valid = m.text_len == old_len;
        let mut left = Self {
            text_len: at,
            attrs: m.attrs.clone(),
            ppr: m.ppr.clone(),
            runs: Vec::new(),
            markers: Vec::new(),
        };
        let mut right = Self {
            text_len: old_len - at,
            attrs: m
                .attrs
                .iter()
                .filter(|a| !PARAGRAPH_IDENTITY_ATTRS.contains(&a.name.as_str()))
                .cloned()
                .collect(),
            ppr: m.ppr.clone(),
            runs: Vec::new(),
            markers: Vec::new(),
        };
        if valid {
            for r in &m.runs {
                if r.start < at {
                    left.runs.push(SourceRun {
                        end: r.end.min(at),
                        ..r.clone()
                    });
                }
                if r.end > at {
                    right.runs.push(SourceRun {
                        start: r.start.max(at) - at,
                        end: r.end - at,
                        ..r.clone()
                    });
                }
            }
            for mk in &m.markers {
                if mk.at <= at {
                    left.markers.push(mk.clone());
                } else {
                    right.markers.push(SourceMarker {
                        at: mk.at - at,
                        ..mk.clone()
                    });
                }
            }
        } else {
            /* Stale offsets: nothing offset-anchored can travel. */
            left.text_len = STALE_TEXT_LEN;
            right.text_len = STALE_TEXT_LEN;
        }
        (Some(Box::new(left)), Some(Box::new(right)))
    }

    /// Merge for [`Paragraph::concat`]: the head's attributes and `<w:pPr>`
    /// record survive (the head keeps its paragraph identity); the tail's
    /// runs and markers shift right by `head_len`.
    pub fn concat(
        head: &Option<Box<Self>>,
        head_len: u32,
        tail: &Option<Box<Self>>,
        tail_len: u32,
    ) -> Option<Box<Self>> {
        if head.is_none() && tail.is_none() {
            return None;
        }
        let empty = Self::default();
        let h = head.as_deref().unwrap_or(&empty);
        let t = tail.as_deref().unwrap_or(&empty);
        if head.is_some() {
            text_remap::debug_assert_in_step(h, head_len);
        }
        if tail.is_some() {
            text_remap::debug_assert_in_step(t, tail_len);
        }
        let h_ok = head.is_none() || h.text_len == head_len;
        let t_ok = tail.is_none() || t.text_len == tail_len;
        let mut out = Self {
            text_len: head_len + tail_len,
            attrs: h.attrs.clone(),
            ppr: h.ppr.clone(),
            runs: Vec::new(),
            markers: Vec::new(),
        };
        if !(h_ok && t_ok) {
            out.text_len = STALE_TEXT_LEN;
            return Some(Box::new(out));
        }
        out.runs.extend(h.runs.iter().cloned());
        out.markers.extend(h.markers.iter().cloned());
        for r in &t.runs {
            out.runs.push(SourceRun {
                start: r.start + head_len,
                end: r.end + head_len,
                ..r.clone()
            });
        }
        for mk in &t.markers {
            out.markers.push(SourceMarker {
                at: mk.at + head_len,
                ..mk.clone()
            });
        }
        Some(Box::new(out))
    }
}

/// Issue #120 — one piece of block-level (`<w:body>` / `<w:tc>` child)
/// markup that is not a paragraph or a table and that the typed model does
/// not represent: a `<w:bookmarkStart/>` between two paragraphs, a
/// `<w:sdt>` content-control envelope around a run of blocks, the
/// whitespace of a pretty-printed part. The reader attaches these to the
/// neighbouring block ([`BodyPassthrough`]) and the `.docx` writer
/// re-emits them verbatim around that block, clean or regenerated.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum BodyFragment {
    /// Self-contained markup emitted as-is (`<w:bookmarkStart …/>`, a
    /// `<w:sdt>` whose content holds no block, inter-block whitespace).
    Verbatim {
        #[serde(with = "serde_bytes")]
        xml: Vec<u8>,
    },
    /// Opens an envelope around this block and the ones that follow it
    /// up to the matching [`Self::Close`]: `open_xml` is everything from
    /// the container's start tag through the last byte before its first
    /// inner block (`<w:sdt><w:sdtPr>…</w:sdtPr><w:sdtContent>`),
    /// `close_xml` everything after its last inner block through its end
    /// tag (`</w:sdtContent></w:sdt>`). The writer keeps a stack, so an
    /// envelope whose other end was lost to an edit still closes
    /// (well-formedness is never at the mercy of an edit) and a closer
    /// without an opener is skipped.
    Open {
        id: u32,
        #[serde(with = "serde_bytes")]
        open_xml: Vec<u8>,
        #[serde(with = "serde_bytes")]
        close_xml: Vec<u8>,
    },
    /// Closes envelope `id` after this block.
    Close { id: u32 },
}

/// Issue #120 — the block-level passthrough markup that surrounds one
/// block: `before` is emitted ahead of the block's own XML, `after`
/// behind it, both in source order. Boxed behind an `Option` on
/// [`Paragraph`] / [`Table`] so the common case costs a pointer.
///
/// Travel rules mirror the paragraph mark: a split keeps `before` on the
/// left half and `after` on the right (the envelope keeps wrapping both),
/// a merge keeps the head's `before` and the tail's `after`, and
/// clipboard fragments carry none (a paste never transplants a content
/// control's envelope).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct BodyPassthrough {
    pub before: Vec<BodyFragment>,
    pub after: Vec<BodyFragment>,
}

impl BodyPassthrough {
    pub fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }

    /// The `before` half only (for the left side of a split).
    pub fn before_only(this: &Option<Box<Self>>) -> Option<Box<Self>> {
        this.as_deref().filter(|b| !b.before.is_empty()).map(|b| {
            Box::new(Self {
                before: b.before.clone(),
                after: Vec::new(),
            })
        })
    }

    /// The `after` half only (for the right side of a split).
    pub fn after_only(this: &Option<Box<Self>>) -> Option<Box<Self>> {
        this.as_deref().filter(|b| !b.after.is_empty()).map(|b| {
            Box::new(Self {
                before: Vec::new(),
                after: b.after.clone(),
            })
        })
    }

    /// Both blocks' markup, for a merge: `before` = head's then tail's,
    /// `after` = head's then tail's. Markup that sat BETWEEN the two
    /// (a bookmark, an empty content control, a closer / opener pair)
    /// has no boundary to sit on any more and moves to the merged
    /// block's edges instead of being dropped — an envelope grows to
    /// cover the merge, it never loses its content control.
    pub fn merged(head: &Option<Box<Self>>, tail: &Option<Box<Self>>) -> Option<Box<Self>> {
        let mut before = head
            .as_deref()
            .map(|b| b.before.clone())
            .unwrap_or_default();
        before.extend(
            tail.as_deref()
                .map(|b| b.before.clone())
                .unwrap_or_default(),
        );
        let mut after = head.as_deref().map(|b| b.after.clone()).unwrap_or_default();
        after.extend(tail.as_deref().map(|b| b.after.clone()).unwrap_or_default());
        (!before.is_empty() || !after.is_empty()).then(|| Box::new(Self { before, after }))
    }
}

/// Issue #112 — the bytes of a source `word/document.xml` that surround
/// the block list: everything the writer used to synthesize and that
/// therefore drifted on every zero-edit resave (Word ends its XML
/// declaration with `\r\n`, declares `xmlns:wpc` before `xmlns:w`, …).
/// Captured by the `.docx` reader, re-emitted verbatim by every writer
/// path (the live editor saves from the tree alone); empty for an
/// engine-authored document, in which case the writer synthesizes the
/// stock header and footer exactly as before.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct DocumentEnvelope {
    /// Everything before the root start tag: BOM, XML declaration, the
    /// newline after it, comments.
    #[serde(with = "serde_bytes")]
    pub prolog: Vec<u8>,
    /// The root start tag itself (`<w:document …>`), attribute order and
    /// spelling intact. The writer adds any binding a regenerated element
    /// needs that the source root lacks (a picture pasted into a document
    /// whose root never declared `wp:`).
    #[serde(with = "serde_bytes")]
    pub root_tag: Vec<u8>,
    /// Bytes between the root start tag and the `<w:body>` start tag
    /// (whitespace in a pretty-printed part).
    #[serde(with = "serde_bytes")]
    pub root_to_body: Vec<u8>,
    /// The `<w:body …>` start tag.
    #[serde(with = "serde_bytes")]
    pub body_tag: Vec<u8>,
    /// Everything after the trailing body-level `<w:sectPr>` (or the last
    /// block when there is none): `</w:body>`, `</w:document>` and any
    /// whitespace around them, to EOF.
    #[serde(with = "serde_bytes")]
    pub tail: Vec<u8>,
}

impl DocumentEnvelope {
    /// `true` when the reader captured a usable envelope (root + body
    /// tags + tail); an engine-authored document has none.
    pub fn is_captured(&self) -> bool {
        !self.root_tag.is_empty() && !self.body_tag.is_empty() && !self.tail.is_empty()
    }
}

/// Inline style for a run of characters: font size, colour, the
/// bold / italic / underline / strikethrough flags, a background (highlight)
/// colour, and a font family. All are carried through layout and render.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct SpanStyle {
    pub font_size: Option<f32>,
    pub color: Option<[u8; 4]>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<UnderlineStyle>,
    pub strike: Option<bool>,
    pub bg_color: Option<[u8; 4]>,
    pub font_family: Option<FontFamily>,
    /// `<w:caps/>` — display every character of the run as its uppercase
    /// equivalent. Applied as a `to_uppercase` transform at shape time so
    /// glyph metrics + BiDi + line breaking all see the visible string.
    /// `caps` wins over `small_caps` when both are set (OOXML §17.3.2.7).
    pub caps: Option<bool>,
    /// `<w:smallCaps/>` — display lowercase characters as reduced-height
    /// uppercase glyphs while leaving originally-uppercase characters at
    /// full size. The engine's best-effort approximation uppercases the
    /// originally-lowercase substrings and shrinks their font_size to
    /// ~80% of the run's nominal size.
    pub small_caps: Option<bool>,
    /// Audit gap A.M1 — `<w:vertAlign w:val="superscript|subscript"/>`.
    /// `None` ⇒ baseline (the no-op default); explicit `Some(Baseline)`
    /// is preserved so a run-style override can defeat an inherited
    /// super/subscript from the style cascade.
    pub vert_align: Option<VertAlign>,
    /// Audit gap A.M2 — verbatim font family name from `<w:rFonts w:ascii>`
    /// when the engine cannot resolve it to a loaded face. Round-tripped
    /// back into the writer so a save preserves the author's
    /// "Cambria" / "Times New Roman" / etc. even though we render with
    /// the fallback. `None` when the engine successfully resolves the
    /// name into [`font_family`].
    pub raw_font_family: Option<String>,
    /// Audit gap A.M2 — `<w:rFonts w:asciiTheme="…"/>` (and the
    /// per-script `hAnsiTheme` / `cstheme`). Round-tripped verbatim. Lost
    /// theme bindings break Word's "Update Style" — preserve at all
    /// costs even though our font picker ignores them.
    pub font_theme: Option<String>,
    /// Issue #84 — unmodeled `<w:rPr>` children captured verbatim by the
    /// `.docx` reader (see [`GrabBag`]). `None` for every engine-authored
    /// style and for runs whose `<w:rPr>` the model fully expresses.
    pub grab_bag: Option<Box<GrabBag>>,
}

impl SpanStyle {
    /// Issue #276 — this run's formatting as continued by text typed
    /// next to it: everything except revision records. A grab-bag
    /// `<w:rPrChange>` (a tracked formatting change) belongs to the text
    /// it was recorded on; copying it onto new text would forge a
    /// revision and duplicate its `w:id`.
    pub fn for_typing(&self) -> SpanStyle {
        let Some(bag) = self.grab_bag.as_deref() else {
            return self.clone();
        };
        let kept: Vec<Vec<u8>> = bag
            .fragments
            .iter()
            .filter(|f| !f.starts_with(b"<w:rPrChange"))
            .cloned()
            .collect();
        if kept.len() == bag.fragments.len() {
            return self.clone();
        }
        SpanStyle {
            grab_bag: (!kept.is_empty()).then(|| Box::new(GrabBag { fragments: kept })),
            ..self.clone()
        }
    }

    /// Overlay `patch`'s set fields onto `self`.
    pub fn merged_with(self, patch: SpanStyle) -> SpanStyle {
        SpanStyle {
            font_size: patch.font_size.or(self.font_size),
            color: patch.color.or(self.color),
            bold: patch.bold.or(self.bold),
            italic: patch.italic.or(self.italic),
            underline: patch.underline.or(self.underline),
            strike: patch.strike.or(self.strike),
            bg_color: patch.bg_color.or(self.bg_color),
            font_family: patch.font_family.or(self.font_family),
            caps: patch.caps.or(self.caps),
            small_caps: patch.small_caps.or(self.small_caps),
            vert_align: patch.vert_align.or(self.vert_align),
            raw_font_family: patch.raw_font_family.or(self.raw_font_family),
            font_theme: patch.font_theme.or(self.font_theme),
            /* Issue #84 — same "set field wins" rule as every slot above:
            a formatting patch (no bag) keeps the run's bag; a direct
            `<w:rPr>` folded onto a cascade baseline (which never carries
            one) contributes its own. */
            grab_bag: patch.grab_bag.or(self.grab_bag),
        }
    }
}

/// A styled byte range `[start, end)` within a paragraph.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StyleRun {
    pub start: u32,
    pub end: u32,
    pub style: SpanStyle,
}

/// Phase 7 — one EMU is **1/914400 of an inch**. 914400 EMU = 1 in = 72 pt;
/// dividing by 12700 converts straight to PostScript points, which is the
/// layout unit at scale=1. The paginator then multiplies by `scale` for the
/// device-pixel canvas.
pub const EMU_PER_PT: i64 = 12700;

/// Convert EMUs to layout points (the engine's 1 pt/unit space).
pub fn emu_to_pt(emu: i64) -> f32 {
    (emu as f32) / (EMU_PER_PT as f32)
}

/// Kind of inline object anchored in a paragraph's text. Phase 7 ships
/// inline images; Phase 8a adds footnote references.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum InlineKind {
    /// `<w:drawing><wp:inline><a:graphic><pic:pic>` — a DrawingML picture.
    /// `rel_id` is the OOXML relationship id from the `<a:blip r:embed=...>`
    /// pointing to the `word/media/*` archive entry. `width_emu` /
    /// `height_emu` come from `<wp:extent cx="..." cy="..."/>`.
    ///
    /// Issue #188 — `rel_id` is scoped to the OPC part the picture lives
    /// in (`word/_rels/header1.xml.rels` and `document.xml.rels` may both
    /// declare `rId5` for different targets), so it is kept only as the
    /// part-local serialization id the writer re-emits as `r:embed`.
    /// `media_key` is the [`DocumentTree::media`] key the picture paints
    /// from: the reader resolves the rel against its own part's rels and
    /// stores the target part name (`word/media/image2.png`), so equal
    /// targets share one blob. `None` ⇒ `rel_id` doubles as the media key
    /// (engine-inserted pictures, pre-#188 snapshots). Read it through
    /// [`InlineKind::image_media_key`].
    Image {
        rel_id: String,
        width_emu: i64,
        height_emu: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_key: Option<String>,
    },
    /// `<w:footnoteReference w:id="N"/>` — Phase 8a / issue #80. `id` is
    /// the OOXML footnote id (a key into
    /// [`DocumentTree::footnote_stories`]). The displayed number is NOT
    /// stored: [`DocumentTree::note_markers`] derives it in document
    /// order at layout time, so an inserted or deleted note renumbers
    /// every later reference for free. `custom_mark_follows` mirrors
    /// `w:customMarkFollows` — the author's own mark text follows in the
    /// next run and the auto-sequence skips this reference.
    FootnoteRef {
        id: u32,
        #[serde(default)]
        custom_mark_follows: bool,
    },
    /// `<w:endnoteReference w:id="N"/>` — issue #80 twin of
    /// [`Self::FootnoteRef`] keyed into [`DocumentTree::endnote_stories`].
    EndnoteRef {
        id: u32,
        #[serde(default)]
        custom_mark_follows: bool,
    },
    /// `<w:footnoteRef/>` / `<w:endnoteRef/>` — issue #80. The self-mark
    /// at the head of a note body: paints the note's own number and
    /// round-trips the element on a regenerated body. Only meaningful
    /// inside a [`NoteStory`]; body paragraphs never carry one.
    NoteSelfRef { kind: NoteKind },
    /// Issue #83 — a text box: a `<wps:wsp>` shape carrying a
    /// `<wps:txbx><w:txbxContent>` story (or its VML twin, a `<v:shape>`
    /// with a `<v:textbox>`). The shape extent is `width_emu` ×
    /// `height_emu` (`<wp:extent>`); placement rides the owning
    /// [`InlineObject::anchor`] exactly like a floating picture (`None` ⇒
    /// an in-line `<wp:inline>` box). The story is the body's own block
    /// model, so the story adapter, layout and the writer reuse every body
    /// code path — see [`TextBoxStory`].
    TextBox {
        width_emu: i64,
        height_emu: i64,
        story: Box<TextBoxStory>,
    },
}

impl InlineKind {
    /// Issue #188 — the [`DocumentTree::media`] key a picture paints
    /// from: its resolved `media_key`, else its `rel_id`. `None` for every
    /// non-picture kind.
    pub fn image_media_key(&self) -> Option<&str> {
        match self {
            InlineKind::Image {
                rel_id, media_key, ..
            } => Some(media_key.as_deref().unwrap_or(rel_id)),
            _ => None,
        }
    }
}

/// Issue #188 — visit every picture ([`InlineKind::Image`]) in `blocks`,
/// recursing into table cells (any depth) and text-box stories. The
/// callback gets the whole kind so it can read or rewrite `rel_id` /
/// `media_key` together.
pub fn for_each_image_mut(blocks: &mut [Block], f: &mut dyn FnMut(&mut InlineKind)) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                for io in &mut p.inline_objects {
                    match &mut io.kind {
                        k @ InlineKind::Image { .. } => f(k),
                        InlineKind::TextBox { story, .. } => for_each_image_mut(&mut story.body, f),
                        _ => {}
                    }
                }
            }
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        for_each_image_mut(&mut cell.blocks, f);
                    }
                }
            }
        }
    }
}

/// Issue #83 — vertical anchoring of a text box story inside the shape's
/// inset rect (`<wps:bodyPr anchor="t|ctr|b">`, VML `v-text-anchor`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextBoxVAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

/// Issue #83 — a shape outline (`<a:ln w="…"><a:solidFill>`): RGBA colour
/// plus stroke width in EMU.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct ShapeOutline {
    pub color: [u8; 4],
    pub width_emu: i64,
}

/// Issue #83 — the story + shape of one text box.
///
/// **Round-trip contract.** `source_xml` is the verbatim container the
/// shape was read from — `<mc:AlternateContent>` (DrawingML choice + VML
/// fallback), `<w:drawing>` or a bare VML `<w:pict>` — so everything this
/// model does not type (geometry presets, effects, `docPr`, the VML
/// duplicate) survives. `story_ranges` locate every `<w:txbxContent>`
/// element inside it (the choice AND the fallback: both carry the same
/// story). A clean box re-emits `source_xml` as-is; a `dirty` one splices
/// the regenerated story into each range. `host_range` locates the
/// container inside the HOST paragraph's `source_xml`, so an edit that
/// only touched the story keeps the host paragraph on its passthrough
/// bytes with just the container swapped (bounded drift). An
/// engine-authored box (`source_xml == None`) is synthesized from the
/// typed fields.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct TextBoxStory {
    /// The story blocks (`<w:txbxContent>` children).
    pub body: Vec<Block>,
    /// `<wps:bodyPr lIns tIns rIns bIns>` (EMU). Word's defaults are
    /// 0.1" left/right and 0.05" top/bottom.
    pub inset_left_emu: i64,
    pub inset_top_emu: i64,
    pub inset_right_emu: i64,
    pub inset_bottom_emu: i64,
    pub v_align: TextBoxVAlign,
    /// `<wps:spPr><a:solidFill>` — `None` ⇒ no fill (transparent).
    pub fill: Option<[u8; 4]>,
    /// `<wps:spPr><a:ln>` — `None` ⇒ no outline.
    pub outline: Option<ShapeOutline>,
    /// `<a:spAutoFit/>` — the shape grows to fit its text. Word stores
    /// the fitted extent on save, so layout uses the extent; the flag
    /// only rides the round-trip and the synthesized XML.
    pub auto_fit: bool,
    /// Verbatim source container (see the type docs).
    pub source_xml: Option<String>,
    /// Byte ranges `[start, end)` of every `<w:txbxContent>` element
    /// inside `source_xml`, in document order.
    pub story_ranges: Vec<(u32, u32)>,
    /// Byte range of the container inside the host paragraph's
    /// `source_xml`, when the host was read from a `.docx`.
    pub host_range: Option<(u32, u32)>,
    /// `true` once the engine mutated the story: the writer regenerates
    /// the `<w:txbxContent>` elements.
    pub dirty: bool,
}

impl Default for TextBoxStory {
    fn default() -> Self {
        Self {
            body: vec![Block::Paragraph(Paragraph::default())],
            inset_left_emu: 91_440,
            inset_top_emu: 45_720,
            inset_right_emu: 91_440,
            inset_bottom_emu: 45_720,
            v_align: TextBoxVAlign::Top,
            fill: None,
            outline: None,
            auto_fit: false,
            source_xml: None,
            story_ranges: Vec::new(),
            host_range: None,
            dirty: false,
        }
    }
}

/// Structural equality through the snapshot encoding: the block model
/// carries `f32` geometry and no `PartialEq` derive of its own, so two
/// stories compare equal exactly when they serialize identically (the
/// snapshot codec sorts maps, so equal states are byte-identical).
impl PartialEq for TextBoxStory {
    fn eq(&self, other: &Self) -> bool {
        match (rmp_serde::to_vec(self), rmp_serde::to_vec(other)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for TextBoxStory {}

impl TextBoxStory {
    /// A content hash over the snapshot encoding (deterministic: maps
    /// serialize sorted) — the layout cache key for the story.
    pub fn content_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        rmp_serde::to_vec(self).unwrap_or_default().hash(&mut h);
        h.finish()
    }
}

/// A non-text inline node anchored at a single byte offset in a paragraph.
/// The paragraph text carries one U+FFFC (OBJECT REPLACEMENT CHARACTER) at
/// `at`; layout looks the object up here when it sees the sentinel and
/// reserves the right physical size in the line.
///
/// Issue #69 — `anchor` distinguishes the two DrawingML placements: `None`
/// is `<wp:inline>` (the object flows with the text and reserves its own
/// width in the line); `Some` is `<wp:anchor>` (the object *floats*: it is
/// positioned on the page relative to a reference frame and reserves NO
/// width — the sentinel byte only records where in the run stream the
/// anchor lives, exactly as OOXML does). Keeping the anchor on the same
/// byte-anchored object means every offset-shifting edit path (insert,
/// delete, split, concat, clipboard) already carries floats correctly.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InlineObject {
    pub at: u32,
    pub kind: InlineKind,
    /// Issue #69 — `Some` ⇒ the object is a floating (`<wp:anchor>`)
    /// object; `None` ⇒ in-line (`<wp:inline>`). Boxed — floats are rare
    /// and `Paragraph` clones constantly. `#[serde(default)]` keeps the
    /// pre-#69 snapshot envelope (format version 1) readable.
    #[serde(default)]
    pub anchor: Option<Box<FloatAnchor>>,
    /// Issue #119 — the run-level source element this object was read
    /// from, verbatim: the whole `<w:drawing>`, `<mc:AlternateContent>`
    /// (DrawingML choice + VML fallback), `<w:pict>` or `<w:object>`.
    /// The `.docx` writer re-emits it byte-for-byte while it still
    /// describes the object (same picture, extent and anchor — verified
    /// against a re-scan at write time) and regenerates from the typed
    /// fields only once the object was resized or moved. It is the ONLY
    /// representation of a text box, shape, chart, SmartArt or OLE object
    /// (`InlineKind::Image` with an empty `rel_id`): the extent is
    /// modeled so layout reserves the box, the content is not (text
    /// boxes are issue #83), and such an object is always written from
    /// these bytes. `None` for engine-authored objects.
    #[serde(default, with = "serde_bytes")]
    pub source_xml: Option<Vec<u8>>,
}

impl InlineObject {
    /// `true` for a `<wp:anchor>`-placed (floating) object.
    pub fn is_floating(&self) -> bool {
        self.anchor.is_some()
    }

    /// Issue #165 — the accessible `(name, description)` of a text box:
    /// its `<wp:docPr name descr>` (read from the anchor's verbatim
    /// `doc_pr_xml`, else from the first `<wp:docPr>` in the box's
    /// verbatim source container — an in-line box keeps it there), or
    /// the VML `<v:shape alt>` as the description for a bare VML box.
    /// Blank values are `None`; `None` for anything but a text box.
    pub fn text_box_label(&self) -> Option<(Option<String>, Option<String>)> {
        let InlineKind::TextBox { story, .. } = &self.kind else {
            return None;
        };
        let from_anchor = self
            .anchor
            .as_deref()
            .and_then(|a| a.doc_pr_xml.as_deref())
            .and_then(|x| start_tag(x, "<wp:docPr"));
        let from_source = || {
            story
                .source_xml
                .as_deref()
                .and_then(|x| start_tag(x, "<wp:docPr"))
        };
        if let Some(tag) = from_anchor.or_else(from_source) {
            return Some((xml_attr(tag, "name"), xml_attr(tag, "descr")));
        }
        let alt = story
            .source_xml
            .as_deref()
            .and_then(|x| start_tag(x, "<v:shape"))
            .and_then(|tag| xml_attr(tag, "alt"));
        Some((None, alt))
    }

    /// Issue #215 — the accessible `(name, description)` of a picture,
    /// mirroring [`Self::text_box_label`]: its `<wp:docPr name descr>`
    /// (read from the anchor's verbatim `doc_pr_xml` for a float, else
    /// from the object's OWN verbatim `source_xml` — an inline picture
    /// keeps it there), or the VML `<v:shape alt>` as the description for
    /// a bare VML picture. Blank values are `None`; `None` for anything
    /// but an image.
    pub fn image_label(&self) -> Option<(Option<String>, Option<String>)> {
        if !matches!(self.kind, InlineKind::Image { .. }) {
            return None;
        }
        let source = || {
            self.source_xml
                .as_deref()
                .and_then(|b| core::str::from_utf8(b).ok())
        };
        let from_anchor = self
            .anchor
            .as_deref()
            .and_then(|a| a.doc_pr_xml.as_deref())
            .and_then(|x| start_tag(x, "<wp:docPr"));
        let from_source = || source().and_then(|x| start_tag(x, "<wp:docPr"));
        if let Some(tag) = from_anchor.or_else(from_source) {
            return Some((xml_attr(tag, "name"), xml_attr(tag, "descr")));
        }
        let alt = source()
            .and_then(|x| start_tag(x, "<v:shape"))
            .and_then(|tag| xml_attr(tag, "alt"));
        Some((None, alt))
    }
}

/// Issue #165 — the first start tag in `xml` opening with `open` (e.g.
/// `"<wp:docPr"`), up to its `>`: the element name must end right after
/// `open` (so `<v:shape` never matches `<v:shapetype`).
fn start_tag<'a>(xml: &'a str, open: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(rel) = xml[from..].find(open) {
        let start = from + rel;
        let after = start + open.len();
        let next = xml[after..].chars().next()?;
        if next.is_whitespace() || next == '>' || next == '/' {
            let end = after + xml[after..].find('>')?;
            return Some(&xml[start..end]);
        }
        from = after;
    }
    None
}

/// Issue #165 — the unescaped value of attribute `key` in a start tag
/// (`"` or `'` quoted); `None` when absent or blank.
fn xml_attr(tag: &str, key: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(rel) = tag[from..].find(key) {
        let at = from + rel;
        from = at + key.len();
        let preceded = at > 0 && bytes[at - 1].is_ascii_whitespace();
        let rest = tag[from..].trim_start();
        if !preceded || !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let body = &rest[1..];
        let value = &body[..body.find(quote)?];
        let value = xml_unescape(value);
        let value = value.trim();
        return (!value.is_empty()).then(|| value.to_string());
    }
    None
}

/// The five predefined XML entities plus numeric character references.
fn xml_unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let ent = &tail[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Issue #69 — horizontal reference frame of a floating object
/// (`<wp:positionH relativeFrom="…">`, ECMA-376 §20.4.3.4 `ST_RelFromH`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HRelativeFrom {
    /// The anchor character's left edge.
    Character,
    /// The text column the anchor paragraph flows in (the whole content
    /// area in single-column sections). Word's default.
    #[default]
    Column,
    /// The inside margin (left on odd pages, right on even).
    InsideMargin,
    LeftMargin,
    /// The content area between the left and right margins.
    Margin,
    /// The outside margin (right on odd pages, left on even).
    OutsideMargin,
    /// The physical page edge.
    Page,
    RightMargin,
}

/// Issue #69 — vertical reference frame of a floating object
/// (`<wp:positionV relativeFrom="…">`, ECMA-376 §20.4.3.5 `ST_RelFromV`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VRelativeFrom {
    BottomMargin,
    InsideMargin,
    /// The line the anchor character sits on.
    Line,
    /// The content area between the top and bottom margins.
    Margin,
    OutsideMargin,
    Page,
    /// The top of the anchor paragraph. Word's default.
    #[default]
    Paragraph,
    TopMargin,
}

/// Issue #69 — `<wp:align>` values. `Left` / `Right` are horizontal-only,
/// `Top` / `Bottom` vertical-only; `Center` / `Inside` / `Outside` apply
/// to both axes (ECMA-376 `ST_AlignH` / `ST_AlignV`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatAlign {
    Left,
    Right,
    Center,
    Inside,
    Outside,
    Top,
    Bottom,
}

/// Issue #69 — how a floating object is placed along one axis inside its
/// reference frame: a fixed EMU offset (`<wp:posOffset>`), an alignment
/// keyword (`<wp:align>`), or a percentage of the frame's extent
/// (`<wp14:pctPosHOffset>` / `<wp14:pctPosVOffset>`, thousandths of a
/// percent — `50000` ⇒ 50 %).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatOffset {
    Emu(i64),
    Align(FloatAlign),
    PercentMilli(i32),
}

impl Default for FloatOffset {
    fn default() -> Self {
        FloatOffset::Emu(0)
    }
}

/// Issue #69 — one positioning axis (`<wp:positionH>` / `<wp:positionV>`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct HPosition {
    pub relative_from: HRelativeFrom,
    pub offset: FloatOffset,
}

/// Issue #69 — the vertical twin of [`HPosition`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct VPosition {
    pub relative_from: VRelativeFrom,
    pub offset: FloatOffset,
}

/// Issue #69 / #82 — the text-wrap mode a floating object declares
/// (`<wp:wrapNone>`, `<wp:wrapSquare>`, `<wp:wrapTight>`,
/// `<wp:wrapThrough>`, `<wp:wrapTopAndBottom>`). `None` is "behind text"
/// or "in front of text" depending on [`FloatAnchor::behind_doc`]; every
/// other mode cuts the lines it overlaps (layout `crate::wrap`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WrapKind {
    #[default]
    None,
    Square,
    Tight,
    Through,
    TopAndBottom,
}

/// Issue #82 — which side(s) of a square / tight / through object text
/// may flow on (`wrapText`, ECMA-376 §20.4.3.7 `ST_WrapText`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WrapText {
    #[default]
    BothSides,
    Left,
    Right,
    Largest,
}

/// Issue #69 — the `<wp:anchor>` placement of a floating object. Mirrors
/// ECMA-376 §20.4.2.3 `CT_Anchor`: the two positioning axes, the
/// `simplePos` escape hatch, the z-order + layering flags, the wrap
/// distances, and the declared wrap mode. Unmodeled children that the
/// writer cannot regenerate from the typed fields (`<wp:docPr>` and the
/// wrap element, which may carry a `<wp:wrapPolygon>`) ride verbatim so a
/// regenerated paragraph stays byte-faithful to the source.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct FloatAnchor {
    pub position_h: HPosition,
    pub position_v: VPosition,
    /// `simplePos="1"` — ignore both axes and place the object at
    /// `simple_pos_{x,y}_emu` from the page's top-left corner.
    pub simple_pos: bool,
    pub simple_pos_x_emu: i64,
    pub simple_pos_y_emu: i64,
    /// `relativeHeight` — z-order among floating objects (higher paints
    /// later, i.e. on top).
    pub relative_height: u32,
    /// `behindDoc="1"` — paint under the text instead of over it.
    pub behind_doc: bool,
    /// `locked="1"` — the anchor may not move to another paragraph.
    pub locked: bool,
    /// `layoutInCell="1"` — inside a table cell, position relative to the
    /// cell rather than the page.
    pub layout_in_cell: bool,
    /// `allowOverlap="1"` — may overlap other floating objects.
    pub allow_overlap: bool,
    /// `hidden="1"` — not painted.
    pub hidden: bool,
    /// `distT` / `distB` / `distL` / `distR` — wrap distances (EMU).
    pub dist_top_emu: i64,
    pub dist_bottom_emu: i64,
    pub dist_left_emu: i64,
    pub dist_right_emu: i64,
    /// Declared wrap mode (issue #82 — layout cuts text around it).
    pub wrap: WrapKind,
    /// Issue #82 — `wrapText` of a square / tight / through wrap element.
    pub wrap_text: WrapText,
    /// Issue #82 — the `<wp:wrapPolygon>` of a tight / through wrap, in
    /// Word's 21600-unit shape space (`(21600, 21600)` is the object's
    /// bottom-right corner), `<wp:start>` first. `None` when the element
    /// carried no polygon (layout falls back to the bounding box).
    pub wrap_polygon: Option<Vec<(i64, i64)>>,
    /// Verbatim source bytes of the wrap element (`<wp:wrapSquare …/>`,
    /// `<wp:wrapTight>…<wp:wrapPolygon>…</wp:wrapTight>`) for
    /// byte-faithful regeneration. `None` for engine-authored anchors —
    /// the writer synthesizes the element from `wrap`.
    pub wrap_xml: Option<String>,
    /// Verbatim `<wp:docPr …/>` (id / name / descr / hyperlink children).
    /// `None` ⇒ the writer synthesizes a stock one.
    pub doc_pr_xml: Option<String>,
}

impl Default for FloatAnchor {
    /// Word's stock "In Front of Text" anchor: column-relative X, paragraph-
    /// relative Y, zero offsets, topmost z-order, overlap allowed.
    fn default() -> Self {
        Self {
            position_h: HPosition::default(),
            position_v: VPosition::default(),
            simple_pos: false,
            simple_pos_x_emu: 0,
            simple_pos_y_emu: 0,
            relative_height: 251_658_240,
            behind_doc: false,
            locked: false,
            layout_in_cell: true,
            allow_overlap: true,
            hidden: false,
            dist_top_emu: 0,
            dist_bottom_emu: 0,
            dist_left_emu: 0,
            dist_right_emu: 0,
            wrap: WrapKind::None,
            wrap_text: WrapText::BothSides,
            wrap_polygon: None,
            wrap_xml: None,
            doc_pr_xml: None,
        }
    }
}

/// A hyperlink overlay on a contiguous byte range of a paragraph. Display
/// styling (blue + underline if no explicit `<w:rPr>`) is applied at layout
/// time; clicks are out of scope for Phase 7 (the model is read-only).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Hyperlink {
    pub start: u32,
    pub end: u32,
    /// External URL (`Target` from the `r:id`'s rel entry), or `#name`
    /// for an internal bookmark anchor (`<w:hyperlink w:anchor>`, issue
    /// #81).
    pub target: String,
    /// Issue #242 — the source `<w:hyperlink>` attributes, source order
    /// (`r:id`, `w:history`, `w:tooltip`, `w:anchor`, `w:tgtFrame`, …),
    /// re-emitted when the paragraph regenerates. The writer keeps the
    /// source `r:id` only while the package's rels part still maps it to
    /// `target` (a *verified* id — two links to one URL keep their own
    /// rows) and re-resolves it otherwise; an internal `target` re-derives
    /// `w:anchor`. Empty for an engine-authored link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<SourceAttr>,
}

/// Phase 2 audit (gap D.1) — complex field overlay on a paragraph byte
/// range. The cached display text lives in the paragraph's `text` field
/// at `[start, end)`; `instruction` is the OOXML field code (`PAGE`,
/// `NUMPAGES`, `DATE`, `TIME`, etc.) lifted verbatim from the
/// `<w:instrText>` element(s) between the field's `begin` and
/// `separate` fldChars.
///
/// Evaluation lives at paginate / paint time, not parse time: a `PAGE`
/// field's actual page number is not knowable until the containing
/// paragraph has been placed on a page. The reader preserves whatever
/// cached value the source `.docx` shipped (Word stamps the
/// last-rendered value as the cached text); the paginator overrides it
/// with the live value before flushing each page via [`Field::evaluate`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Byte offset where the cached display text starts (inclusive).
    pub start: u32,
    /// One past the end of the cached display text.
    pub end: u32,
    /// Field code — the unparsed `<w:instrText>` content. Trimmed of
    /// surrounding whitespace; switches like `\* MERGEFORMAT` are
    /// preserved (the evaluator parses the leading keyword).
    pub instruction: String,
    /// Issue #81 — `Some` when this overlay is one END of a field whose
    /// result runs across several paragraphs (a TOC). See [`FieldSpan`].
    /// `None` = the ordinary paragraph-local field (#43 / #77).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<FieldSpan>,
    /// Issue #246 — the field's source markup, for a field read from
    /// `.docx`; `None` for an engine-authored one. Skipped when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Box<FieldSource>>,
}

/// Issue #246 — how a field read from `.docx` was spelled, so a
/// regenerated paragraph writes it back in the SAME form: a
/// `<w:fldSimple>` stays simple (instead of growing into a
/// `fldChar begin / instrText / separate … end` complex field), and a
/// complex field keeps its source prologue — the begin run with its
/// `<w:ffData>` (a `FORMTEXT` with a result), rsids, the instruction runs
/// with their spacing — and its end run.
///
/// Verified: the writer uses the bytes only while the field's live
/// `instruction` still equals the one they produced; an edited
/// instruction regenerates the standard complex form. The result runs in
/// between always regenerate from the text.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct FieldSource {
    /// The (trimmed) instruction the bytes produced.
    pub instruction: String,
    /// `<w:fldSimple …>` start tag, or the complex field's runs from the
    /// begin `fldChar` through the `separate` one.
    #[serde(with = "serde_bytes")]
    pub open: Vec<u8>,
    /// `</w:fldSimple>`, or the complex field's end-`fldChar` run.
    #[serde(with = "serde_bytes")]
    pub close: Vec<u8>,
}

impl FieldSource {
    /// The source bytes still spell `instruction`.
    pub fn is_current(&self, instruction: &str) -> bool {
        self.instruction == instruction
    }
}

/// Issue #81 — the multi-paragraph field representation. OOXML lets a
/// complex field's result run across paragraphs (Word writes a TOC as
/// `begin` + instruction + `separate` in the FIRST entry paragraph and
/// `end` in the LAST); the engine models it as a matched pair of
/// overlays on sibling top-level body paragraphs:
///
/// - [`FieldSpan::Head`] on the first paragraph: `start` is the `begin`
///   offset, `end` is that paragraph's text length, `instruction` is
///   the field code.
/// - [`FieldSpan::Tail`] on the last paragraph: `start` is 0, `end` is
///   the `end` offset (may be 0 — Word often closes a TOC in an empty
///   paragraph), `instruction` is empty.
///
/// Every paragraph between them belongs to the result. The pair is
/// matched by document order (a Head claims the NEXT Tail among the
/// following siblings), so structural edits inside the result (Enter,
/// merges) keep the region intact without any index side table; an
/// orphan end degrades to a one-paragraph region, never to data loss.
/// Span overlays are NOT caret-atomic (Word lets you edit TOC text).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldSpan {
    Head,
    Tail,
}

impl Field {
    /// `true` for a paragraph-local field (the caret-atomic kind).
    pub fn is_local(&self) -> bool {
        self.span.is_none()
    }

    /// Extract the leading keyword from `instruction` — the part Word
    /// uses to dispatch field types. `"PAGE \* MERGEFORMAT"` → `"PAGE"`;
    /// `"DATE"` → `"DATE"`. Returns an uppercase owned `String` so
    /// callers can `match` on it without re-allocating.
    pub fn keyword(&self) -> String {
        self.instruction
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase()
    }

    /// Compute the live display string for this field given the page
    /// context. Returns `None` for instructions the engine does not
    /// evaluate (the renderer keeps the cached text in that case).
    /// `current_page` and `total_pages` are 1-based.
    pub fn evaluate(&self, current_page: u32, total_pages: u32) -> Option<String> {
        /* Issue #77 — page-only shim over the environment evaluator in
        `fields.rs`; every kind resolves through `evaluate_in`. */
        let env = FieldEnv::default().with_page(Some(current_page.to_string()), Some(total_pages));
        match self.typed() {
            TypedField::Page | TypedField::NumPages => self.evaluate_in(&env),
            _ => None,
        }
    }

    /// Issue #43 — the `\@ "…"` date-picture switch, or `None` when the
    /// instruction carries none (the evaluator then uses the Word
    /// en-default `M/d/yyyy`).
    pub fn date_picture(&self) -> Option<String> {
        let ins = &self.instruction;
        let at = ins.find("\\@")?;
        let rest = ins[at + 2..].trim_start();
        if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"')?;
            Some(stripped[..end].to_string())
        } else {
            /* Unquoted picture — runs to the next switch or the end. */
            let tok = rest.split_whitespace().next()?;
            (!tok.starts_with('\\')).then(|| tok.to_string())
        }
    }
}

/// Issue #43 — render `(year, month, day)` through Word's date-picture
/// language: `yyyy`, `yy`, `MM`, `M`, `dd`, `d` (longest-match,
/// case-sensitive per Word: `M` = month, `d` = day). Unrecognized
/// characters (and time tokens — no clock here) pass through verbatim.
pub fn render_date_picture(picture: &str, year: i32, month: u32, day: u32) -> String {
    /* Issue #77 — date-only shim over the shared date/time renderer
    (`fields::render_date_time_picture`); time tokens pass through. */
    render_date_time_picture(picture, Some((year, month, day)), None)
}

/// Phase 8b — kind of tracked-change revision.
///
/// - `Insert` — `<w:ins>` wraps text that a reviewer added.
/// - `Delete` — `<w:del>` wraps text that the original document carried
///   but a reviewer marked for removal. The deleted text rides in the
///   paragraph's `text` field alongside live content; the renderer
///   applies markup styling.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionKind {
    Insert,
    Delete,
    /// Sprint 14 (#14) — `<w:rPrChange>` tracked formatting change.
    /// The original `SpanStyle` (pre-mutation) lives on
    /// [`Revision::prev_attrs`] so accept/reject can restore it.
    /// Payload-free here to keep `RevisionKind: Copy`, which a dozen
    /// existing match sites rely on.
    FormatChange,
    /// Issue #247 — `<w:moveFrom>`: the SOURCE side of a tracked move.
    /// Text semantics are a deletion's (accept drops it, reject keeps
    /// it); the reviewer sees it as moved-away text. The source spells
    /// it with `<w:t>` (not `<w:delText>`). The pairing with its
    /// destination rides [`Revision::move_name`].
    MoveFrom,
    /// Issue #247 — `<w:moveTo>`: the DESTINATION side of a tracked
    /// move. Text semantics are an insertion's (accept keeps it, reject
    /// drops it).
    MoveTo,
}

impl RevisionKind {
    /// Issue #247 — `true` when ACCEPTING this revision removes its
    /// text (`Delete`, `MoveFrom`).
    pub fn removes_on_accept(self) -> bool {
        matches!(self, Self::Delete | Self::MoveFrom)
    }

    /// Issue #247 — `true` when REJECTING this revision removes its
    /// text (`Insert`, `MoveTo`).
    pub fn removes_on_reject(self) -> bool {
        matches!(self, Self::Insert | Self::MoveTo)
    }

    /// Issue #247 — `true` for the kinds that wrap runs in the source
    /// (`<w:ins>` / `<w:del>` / `<w:moveFrom>` / `<w:moveTo>`);
    /// `FormatChange` rides the run's `<w:rPr>` instead.
    pub fn wraps_text(self) -> bool {
        !matches!(self, Self::FormatChange)
    }

    /// Issue #247 — the text is removed by exactly one of accept /
    /// reject: `accept == true` asks for the accept outcome.
    pub fn removes_text(self, accept: bool) -> bool {
        if accept {
            self.removes_on_accept()
        } else {
            self.removes_on_reject()
        }
    }
}

/// Phase 8b — one `<w:ins>` / `<w:del>` / `<w:rPrChange>` overlay on a
/// paragraph's byte range. `author` + `date` carry the OOXML
/// `w:author` / `w:date` attributes so the TS shell can surface them
/// on hover. `id` carries the `w:id` attribute Word's accept/reject
/// UI uses to address an individual change; `None` for revisions the
/// engine synthesised (writer assigns a fresh sequential id at
/// emission time) or for source files that omit the attribute.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Revision {
    pub start: u32,
    pub end: u32,
    pub kind: RevisionKind,
    pub author: String,
    pub date: String,
    pub id: Option<u32>,
    /// Sprint 14 (#14) — pre-mutation `SpanStyle` snapshot for a
    /// `RevisionKind::FormatChange` so reject can restore the
    /// original look. `None` for `Insert` / `Delete` revisions where
    /// the attribute is irrelevant.
    pub prev_attrs: Option<SpanStyle>,
    /// Issue #247 — for a `MoveFrom` / `MoveTo` revision, the `w:name`
    /// of the enclosing `<w:moveFromRangeStart>` / `<w:moveToRangeStart>`
    /// (the pair's shared name links a move's two halves). `None` for
    /// every other kind and for a move read outside a named range. The
    /// range markers themselves ride the paragraph's source markup as
    /// positioned verbatim markers. Skipped when `None`, so a pre-#247
    /// snapshot encodes unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_name: Option<String>,
}

/// Phase 7 — a media blob stashed for the renderer to decode.
///
/// `content_type` is the MIME type the OOXML rels claimed (`image/png`,
/// `image/jpeg`, ...). The bytes are the raw archive entry contents — no
/// re-encoding, so format round-trips byte-identical through the writer
/// (writer-side media emission is a follow-up sprint).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageBlob {
    pub content_type: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Paragraph text alignment (Backlog #9). `Start` / `End` are
/// writing-direction-relative — they resolve against the base direction at
/// layout time; `Center` and `Justify` are absolute. Mirrors
/// `text_pipeline::Alignment`; kept here so the pure document model carries no
/// dependency on the text-shaping crate.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Start,
    End,
    Center,
    Justify,
}

/// Paragraph indentation. OOXML carries these as twips (1/1440 inch); the
/// engine stores them in the same unit and converts to layout pixels at
/// `engine-wasm` boundary so the pure document model has no float-DPI
/// dependency. `first_line` and `hanging` are mutually exclusive in OOXML;
/// the reader sets the matching field and zeroes the other.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Indent {
    pub start_twips: i32,
    pub end_twips: i32,
    pub first_line_twips: i32,
    pub hanging_twips: i32,
}

/// Per-paragraph vertical spacing. Twips, matching `<w:spacing>`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Spacing {
    pub before_twips: i32,
    pub after_twips: i32,
}

/// Explicit paragraph base direction (`<w:bidi/>` for RTL). `None` lets
/// `text_pipeline::first_strong_direction` infer from the first strong
/// character — the current document-wide default.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

/// Per-paragraph line-height override (`<w:spacing w:line>` /
/// `w:lineRule>`). `None` inherits the renderer's default line height.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// `w:lineRule="auto"` — `w:line` is a 240-ths multiple of single line
    /// height; we store the integer twips for round-trip, layout converts.
    Auto { twips: i32 },
    /// `w:lineRule="exact"` — fixed twip height; overflow clips.
    Exact { twips: i32 },
    /// `w:lineRule="atLeast"` — minimum; grows for tall glyphs.
    AtLeast { twips: i32 },
}

/// Paragraph-level properties parsed from `<w:pPr>`. Holds every field the
/// engine needs to round-trip a Word paragraph; layout consumes the
/// Audit gap A.M3 — one `<w:pPr><w:tabs><w:tab/>` entry. `position`
/// is layout pt at scale=1 (twip → pt at parse); `kind` controls the
/// alignment of content at the stop. Phase-5 line builder honours
/// `Left` precisely; `Center` / `Right` / `Decimal` round-trip
/// faithfully on the writer but render as `Left` for now (proper
/// alignment requires a measure-then-place pass deferred to a later
/// sprint).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(default)]
pub struct TabStop {
    pub position_pt: f32,
    pub kind: TabKind,
    /// Issue #81 — `<w:tab w:leader>` fill character drawn across the
    /// tab's advance (TOC dot leaders). `None` for the default.
    #[serde(skip_serializing_if = "TabLeader::is_none")]
    pub leader: TabLeader,
}

/// Issue #81 — `<w:tab w:leader>` (ST_TabTlc). The layout records the
/// advance a leadered tab covers; the renderer fills it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum TabLeader {
    #[default]
    None,
    Dot,
    Hyphen,
    Underscore,
    Heavy,
    MiddleDot,
}

impl TabLeader {
    pub fn is_none(&self) -> bool {
        matches!(self, TabLeader::None)
    }

    /// OOXML `w:leader` token (`None` → `"none"`).
    pub fn as_ooxml(self) -> &'static str {
        match self {
            TabLeader::None => "none",
            TabLeader::Dot => "dot",
            TabLeader::Hyphen => "hyphen",
            TabLeader::Underscore => "underscore",
            TabLeader::Heavy => "heavy",
            TabLeader::MiddleDot => "middleDot",
        }
    }

    /// Parse an OOXML `w:leader` token; unknown → `None`.
    pub fn from_ooxml(v: &str) -> Self {
        match v.trim() {
            "dot" => TabLeader::Dot,
            "hyphen" => TabLeader::Hyphen,
            "underscore" => TabLeader::Underscore,
            "heavy" => TabLeader::Heavy,
            "middleDot" => TabLeader::MiddleDot,
            _ => TabLeader::None,
        }
    }
}

/// Issue #145 — one incoming `<w:pPr><w:tabs><w:tab>` entry for
/// [`DocumentTree::set_tab_stops`]. `leader: None` means "keep this
/// stop's existing leader" — the Ruler (and any other caller) that
/// does not itself track leaders must not silently clear one every
/// time it writes a position; `Some(TabLeader::None)` is the explicit
/// clear. Resolution is positional: patch entry `i` inherits from the
/// paragraph's *current* `tab_stops[i]` when present, else `TabLeader::
/// None` (a brand-new stop has nothing to inherit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabStopPatch {
    pub position_pt: f32,
    pub kind: TabKind,
    pub leader: Option<TabLeader>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabKind {
    /// Tab cursor jumps to `position_pt`; content lands right of it.
    /// Word's default and what the line builder honours.
    #[default]
    Left,
    Center,
    Right,
    Decimal,
    /// `<w:clear>` — explicit "no tab at this position", used to defeat
    /// an inherited tab stop from the style cascade.
    Clear,
}

/// alignment / indent / spacing / direction / line-height subset.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct ParaProperties {
    pub alignment: Option<Alignment>,
    pub indent: Indent,
    pub spacing: Spacing,
    pub direction: Option<TextDirection>,
    pub line_height: Option<LineHeight>,
    /// Issue #178 — `<w:keepNext>` resolved through the style cascade.
    /// `Option`, like [`Self::widow_control`], so a paragraph's explicit
    /// `w:val="0"` can switch an inherited style's ON back off (a plain
    /// bool merged with OR could never do that). `None` means never
    /// specified (OOXML default: off). Unlike `widow_control`, the
    /// direct element is fully modeled (not grab-bagged) — it round-trips
    /// through this field on both styles and direct paragraphs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_next: Option<bool>,
    /// Issue #178 — `<w:keepLines>`, same contract as [`Self::keep_next`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_lines: Option<bool>,
    pub page_break_before: bool,
    /// Audit gap A.M4 — `<w:pPr><w:pBdr>` border strokes painted around
    /// the paragraph bounding rectangle. Mirror of the table-cell
    /// border model (top/left/bottom/right edges plus the unused
    /// inside_h/inside_v slots `CellBorders` ships with). `None` ⇒ no
    /// border (the implicit default). Renderer reuses the cell-border
    /// drawing primitive at paragraph-rect bounds.
    pub borders: Option<CellBorders>,
    /// Audit gap A.M3 — `<w:pPr><w:tabs>` custom tab stops in
    /// document order. Empty list ⇒ fall back to the 0.5-inch default
    /// grid the line builder uses. Position is layout pt at scale=1
    /// (1 twip = 1/20 pt; reader converts at parse time).
    pub tab_stops: Vec<TabStop>,
    /// Audit gap A.M17 — `<w:pPr><w:numPr>` numbering binding inherited
    /// via the pStyle chain. Carries the resolved (num_id, ilvl) when
    /// the paragraph's style cascade specifies a list binding. The
    /// document parser folds this into `Paragraph.list_item` when no
    /// direct `<w:pPr><w:numPr>` appears on the paragraph itself.
    pub list_item: Option<ListItem>,
    /// Sprint 6 (UI Edition) — `<w:pPr><w:shd w:fill>` paragraph
    /// background fill. `None` ⇒ transparent (the implicit default).
    /// Mirror of the cell-shading model; the renderer paints a filled
    /// rect at the paragraph's bounding rectangle before drawing the
    /// `<w:pBdr>` strokes.
    pub shading: Option<[u8; 4]>,
    /// Issue #84 — unmodeled direct `<w:pPr>` children (and the whole
    /// paragraph-mark `<w:pPr>/<w:rPr>`, which the writer never
    /// regenerates) captured verbatim by the `.docx` reader. See
    /// [`GrabBag`]. Rides `Paragraph::direct_overrides` as well as the
    /// resolved `props` so a style re-cascade keeps it.
    pub grab_bag: Option<Box<GrabBag>>,
    /// Issue #81 — `<w:outlineLvl w:val>` (0-based; 9 = body text),
    /// read from styles.xml and from the direct `<w:pPr>`. READ-ONLY on
    /// the model: the direct element still rides the grab bag verbatim
    /// (the writer never regenerates it), so modelling it cannot drift
    /// a round-trip. The TOC heading collector reads the resolved value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_level: Option<u8>,
    /// Issue #95 — `<w:widowControl>` resolved through the style
    /// cascade: `Some(false)` is an explicit `w:val="0"` (which must be
    /// able to switch an inherited ON off, hence `Option`), `None` means
    /// never specified. Layout reads `None` through
    /// [`Self::widow_control_on`] against the host-configurable
    /// [`DocumentSettings::widow_control_default`] (issue #179) — Word's
    /// application default is ON, which diverges from the spec's "not
    /// applied"; a host that wants the strict ECMA-376 reading sets the
    /// document setting off instead of patching every paragraph.
    /// READ-ONLY on the paragraph model like [`Self::outline_level`]: the
    /// direct element rides the grab bag verbatim; style definitions
    /// emit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub widow_control: Option<bool>,
}

impl ParaProperties {
    /// Issue #95 / #179 — the effective widow / orphan control. `default_on`
    /// is [`DocumentSettings::widow_control_default`] (Word's own default:
    /// on) — read from the *document* the paragraph belongs to, since
    /// OOXML has no such element to read from the paragraph itself.
    pub fn widow_control_on(&self, default_on: bool) -> bool {
        self.widow_control.unwrap_or(default_on)
    }

    /// Issue #178 — the effective `<w:keepNext>` (OOXML default: off).
    pub fn keep_next_on(&self) -> bool {
        self.keep_next.unwrap_or(false)
    }

    /// Issue #178 — the effective `<w:keepLines>` (OOXML default: off).
    pub fn keep_lines_on(&self) -> bool {
        self.keep_lines.unwrap_or(false)
    }

    /// Overlay `patch` onto `self` using OOXML cascade semantics: a child
    /// style with a *set* (non-default) field overrides the parent. Used by
    /// the Phase 3 `format_docx::style_resolver` to fold a basedOn chain
    /// root → leaf and then drop direct `<w:pPr>` on top.
    ///
    /// **Known limitation.** Engine fields are flat (`Indent`, `Spacing` are
    /// non-`Option` structs), so we cannot distinguish "child specified 0"
    /// from "child inherited". A child whose `<w:ind w:start="0"/>` is
    /// intentional will lose to a parent's non-zero start. Real-world
    /// stylesheets virtually never set 0 explicitly, so the trade-off is
    /// acceptable for Phase 3; Phase 4+ may widen to `Option`.
    pub fn merged_with(self, patch: ParaProperties) -> ParaProperties {
        ParaProperties {
            shading: patch.shading.or(self.shading),
            alignment: patch.alignment.or(self.alignment),
            indent: if patch.indent == Indent::default() {
                self.indent
            } else {
                patch.indent
            },
            spacing: if patch.spacing == Spacing::default() {
                self.spacing
            } else {
                patch.spacing
            },
            direction: patch.direction.or(self.direction),
            line_height: patch.line_height.or(self.line_height),
            /* Issue #178 — last explicit wins: the direct override
            (`patch`) always beats the inherited style when it set the
            field at all, `Some(false)` included. */
            keep_next: patch.keep_next.or(self.keep_next),
            keep_lines: patch.keep_lines.or(self.keep_lines),
            page_break_before: patch.page_break_before || self.page_break_before,
            /* Audit gap A.M4 — `<w:pBdr>` overlay: patch's borders win
            when set; otherwise inherit. */
            borders: patch.borders.or(self.borders),
            /* Audit gap A.M3 — `<w:tabs>` overlay: patch's stops
            REPLACE the parent's (Word's documented behaviour — child
            `<w:tabs>` is not additive, it shadows the cascade).
            Empty patch inherits. */
            tab_stops: if patch.tab_stops.is_empty() {
                self.tab_stops
            } else {
                patch.tab_stops
            },
            /* Audit gap A.M17 — list binding cascades: patch wins
            when set, otherwise inherit. */
            list_item: patch.list_item.or(self.list_item),
            /* Issue #84 — the bag is an attachment, not a cascading
            property: the direct `<w:pPr>` (patch) contributes its own;
            style sources never carry one, so nothing leaks downward. */
            grab_bag: patch.grab_bag.or(self.grab_bag),
            outline_level: patch.outline_level.or(self.outline_level),
            widow_control: patch.widow_control.or(self.widow_control),
        }
    }
}

/// `<w:numPr>` reference — a paragraph's binding to a numbering definition.
/// `num_id` keys into `word/numbering.xml`'s `<w:num>` entries; `ilvl`
/// (0-indexed) selects the level inside the bound `<w:abstractNum>`. The
/// resolved marker text lives in [`Paragraph::resolved_marker`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListItem {
    pub num_id: u32,
    pub ilvl: u8,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Paragraph {
    pub text: String,
    /// Non-overlapping styled ranges, sorted by `start`; default-styled ranges
    /// are omitted. An empty list is plain text.
    pub spans: Vec<StyleRun>,
    /// Paragraph-level properties (`<w:pPr>`). Default = inherit everything
    /// from the render config / document defaults.
    pub props: ParaProperties,
    /// Phase 4 list membership. `Some` when the paragraph carries a
    /// `<w:numPr>` reference; resolved marker is in [`Self::resolved_marker`].
    pub list_item: Option<ListItem>,
    /// Phase 4 cached list marker (`"1."`, `"a)"`, `"•"`, `"1.1.2."`).
    /// Populated by the numbering resolver after `parse_document_xml` returns,
    /// once the full paragraph sequence is known. `None` for non-list
    /// paragraphs and for list paragraphs whose `num_id` resolves to no
    /// definition (defensive — Word tolerates dangling numIds).
    pub resolved_marker: Option<String>,
    /// Issue #50 — the numbering level's `<w:ind>` (`LvlDef.indent`),
    /// stamped by the marker resolver alongside [`Self::resolved_marker`].
    /// Transient render geometry: the layout boundary falls back to it when
    /// the paragraph carries no direct indent, so interactively-toggled
    /// lists indent without mutating `props`. The `.docx` writer never
    /// serializes it — level indents already live in `numbering.xml`.
    pub resolved_list_indent: Option<Indent>,
    /// Phase 3 passthrough optimisation. `false` on load; flips to `true` the
    /// first time any engine mutation produces a derived paragraph. The writer
    /// emits `source_xml` verbatim when this is `false` and ignores it
    /// otherwise — so unmutated stylesheet-driven paragraphs round-trip
    /// byte-identical.
    pub dirty: bool,
    /// Raw `<w:p>...</w:p>` source bytes captured by the reader (Phase 3).
    /// `None` for paragraphs the engine synthesised (`from_text`, splits,
    /// pastes); `Some` for any paragraph parsed from a real `.docx`.
    #[serde(with = "serde_bytes")]
    pub source_xml: Option<Vec<u8>>,
    /// Phase 7 — non-text inline objects anchored in the paragraph's text.
    /// Each one corresponds to a U+FFFC OBJECT REPLACEMENT CHARACTER in
    /// `text` at `inline_objects[i].at`. Sorted by `at`.
    pub inline_objects: Vec<InlineObject>,
    /// Phase 7 — hyperlink overlays on the paragraph's text. Multiple
    /// hyperlinks may exist; they do not overlap.
    pub hyperlinks: Vec<Hyperlink>,
    /// Phase 8b — tracked-change overlays. Each `<w:ins>` / `<w:del>`
    /// in the source paragraph produces one entry. The paragraph's
    /// `text` retains deleted text alongside live content so the
    /// renderer can show markup; the passthrough writer round-trips
    /// the wrappers byte-identical from `source_xml`.
    pub revisions: Vec<Revision>,
    /// Phase 2 audit (gap D.1) — complex-field overlays. Each
    /// `<w:fldChar fldCharType="begin">`/`separate`/`end` triplet the
    /// reader sees produces one entry covering the cached display
    /// text's byte range. The paginator overrides the rendered string
    /// at flush time for `PAGE` / `NUMPAGES`; other instructions
    /// (`DATE`, `TIME`, …) render their cached value.
    pub fields: Vec<Field>,
    /// Sprint 12 (#11) — `<w:pPr><w:pStyle w:val>` paragraph-style id.
    /// `Some` when the paragraph references an entry in
    /// `DocumentTree.styles`; the cascade walker
    /// (`DocumentTree::resolve_style_cascade`) folds the style's
    /// properties into the bottom of the resolved `props`, with
    /// [`Self::direct_overrides`] layered on top.
    pub style_id: Option<String>,
    /// Sprint 12 (#11) — shadow holding ONLY fields the user
    /// explicitly set on this paragraph (or a `<w:pPr>` that the
    /// reader saw directly on the `<w:p>` element). Resolved `props`
    /// = `style_cascade(style_id) ∪ direct_overrides`. On a style
    /// change, `direct_overrides` is preserved verbatim — that is the
    /// whole point of the shadow approach (a user's manual bold
    /// survives a style switch).
    pub direct_overrides: ParaProperties,
    /// Phase 3 (#40) — `Some` when this paragraph's MARK terminates a
    /// mid-document section: the terminated section's `<w:sectPr>`
    /// payload (OOXML: an interior sectPr is a `<w:pPr>` child of the
    /// section's last paragraph). Boxed — markers are rare and
    /// `Paragraph` clones constantly.
    ///
    /// Travel rules (each has a dedicated regression test):
    /// - `split_at`: the ORIGINAL paragraph mark ends the RIGHT half,
    ///   so the marker moves right; the left half gets a fresh mark
    ///   (`None`).
    /// - `concat`: the TAIL's marker survives — the head's mark (and
    ///   any marker on it) is the one being deleted. This is the
    ///   OPPOSITE precedence from every other field in `concat`
    ///   (head-wins) and matches Word: deleting a section break makes
    ///   the preceding text adopt the FOLLOWING section's properties.
    /// - clipboard fragments (`slice` / `slice_blocks`) always CLEAR
    ///   it — pasting must never transplant a section break.
    pub section_end: Option<Box<SectionProps>>,
    /// Issue #81 — paragraph-scoped bookmarks (`<w:bookmarkStart>` …
    /// `<w:bookmarkEnd>` around the paragraph content). Modelled only
    /// for the TOC's `_Toc*` heading anchors (the target of a `\h`
    /// entry's `<w:hyperlink w:anchor>`); other bookmarks keep riding
    /// `source_xml` untouched.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<Bookmark>,
    /// Issue #120 — block-level passthrough markup surrounding this
    /// paragraph in its source part (a `<w:sdt>` content-control envelope,
    /// `<w:bookmarkStart/>` between paragraphs, inter-block whitespace).
    /// See [`BodyPassthrough`] for the travel rules; the `.docx` writer
    /// emits it around the paragraph whether the paragraph is clean or
    /// regenerated.
    pub body_xml: Option<Box<BodyPassthrough>>,
    /// Issues #199 / #106 — attribute-level grab bag and in-paragraph
    /// source markup (see [`SourceMarkup`]); `None` for engine-synthesized
    /// paragraphs. Boxed: `Paragraph` clones constantly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_markup: Option<Box<SourceMarkup>>,
    /// Issue #262 — a tracked change on the paragraph MARK
    /// (`<w:pPr><w:rPr><w:ins/>` / `<w:del/>` / `<w:moveFrom/>` /
    /// `<w:moveTo/>`): an inserted mark is a tracked paragraph SPLIT, a
    /// deleted one a tracked MERGE with the following paragraph. Only
    /// `kind` / `author` / `date` / `id` / `move_name` are meaningful —
    /// `start` / `end` are unused (0). Accepting a deleted (or moved-
    /// away) mark, or rejecting an inserted (or moved-in) one, merges
    /// this paragraph with the next ([`DocumentTree::resolve_all_revisions`]).
    ///
    /// Travel rules: the mark belongs to the paragraph END, so
    /// `split_at` gives it to the RIGHT half (the left half gets a fresh
    /// mark) and `concat` keeps the TAIL's (like `section_end`);
    /// clipboard fragments clear it. Skipped when `None`, so a pre-#262
    /// snapshot encodes unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark_revision: Option<Revision>,
}

/// Issue #81 — one paragraph-scoped bookmark. `id` is the source
/// `w:id` when the bookmark was read from a file (kept so a dirty
/// re-serialization does not renumber it); engine-stamped bookmarks
/// carry `None` and the writer derives a stable id from the name.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Bookmark {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
}

impl Paragraph {
    /// Resolved style at byte offset `at` (default if no span covers it).
    pub fn style_at(&self, at: u32) -> SpanStyle {
        self.spans
            .iter()
            .find(|s| at >= s.start && at < s.end)
            .map_or(SpanStyle::default(), |s| s.style.clone())
    }

    /// Return a copy with `patch` overlaid on the byte range `[start, end)`.
    /// Existing spans are split at the boundaries; every covered sub-range
    /// merges the patch's set fields. Adjacent equal spans are coalesced and
    /// default-only spans dropped, so the representation stays minimal.
    pub fn apply_style(&self, start: u32, end: u32, patch: SpanStyle) -> Paragraph {
        self.restyle_with(start, end, |style| style.merged_with(patch.clone()))
    }

    /// Issue #276 — the style typing at byte `at` produces (before any
    /// sticky formatting): the character before `at`, or at the
    /// paragraph start the character after it, never an inline-object anchor.
    pub fn typing_style_at(&self, at: u32) -> SpanStyle {
        self.inheriting_span(self.snap_offset(at))
            .map_or_else(SpanStyle::default, |i| self.spans[i].style.for_typing())
    }

    /// Issue #276 — index of the style span an insertion at (snapped)
    /// byte `off` continues: the one holding the character BEFORE `off`
    /// (`start < off <= end`), or at the paragraph start the one holding
    /// the character after it (`start == 0`). `None` ⇒ that character is
    /// unstyled, so the inserted text is too. Mirrors
    /// `SourceMarkup::note_insert`'s run choice.
    ///
    /// An inline-object anchor (image, note reference, text box — its
    /// U+FFFC sentinel) never passes its run formatting on: typing after
    /// a footnote reference must not come out superscript, nor text after
    /// a picture inherit its `noProof` / language tagging.
    fn inheriting_span(&self, off: u32) -> Option<usize> {
        let donor = if off == 0 {
            0
        } else {
            let before = self.text[..off as usize].chars().next_back()?;
            off - before.len_utf8() as u32
        };
        if self.inline_objects.iter().any(|o| o.at == donor) {
            return None;
        }
        if off == 0 {
            self.spans.iter().position(|s| s.start == 0 && s.end > 0)
        } else {
            self.spans
                .iter()
                .position(|s| s.start < off && off <= s.end)
        }
    }

    /// Issue #276 — return a copy whose bytes `[start, end)` carry
    /// exactly `style` (replacing, not merging, whatever they had).
    /// Typing over a selection gives the new text the formatting of the
    /// first replaced character, as Word does.
    pub fn set_style(&self, start: u32, end: u32, style: SpanStyle) -> Paragraph {
        self.restyle_with(start, end, |_| style.clone())
    }

    /// Shared body of [`Self::apply_style`] / [`Self::set_style`]: every
    /// sub-range of `[start, end)` gets `f(current style)`.
    fn restyle_with(&self, start: u32, end: u32, f: impl Fn(SpanStyle) -> SpanStyle) -> Paragraph {
        let text_len = self.text.len() as u32;
        let start = self.snap_offset(start);
        let end = self.snap_offset(end);
        if start >= end {
            return self.clone();
        }

        /* Every boundary: text extent, the patch range, existing span edges. */
        let mut bounds: Vec<u32> = vec![0, text_len, start, end];
        for s in &self.spans {
            bounds.push(s.start);
            bounds.push(s.end);
        }
        bounds.retain(|&b| b <= text_len);
        bounds.sort_unstable();
        bounds.dedup();

        /* Re-derive each interval's style, merging the patch where covered. */
        let mut spans: Vec<StyleRun> = Vec::new();
        for win in bounds.windows(2) {
            let (a, b) = (win[0], win[1]);
            let mut style = self.style_at(a);
            if a >= start && b <= end {
                style = f(style);
            }
            if style == SpanStyle::default() {
                continue;
            }
            match spans.last_mut() {
                Some(prev) if prev.end == a && prev.style == style => prev.end = b,
                _ => spans.push(StyleRun {
                    start: a,
                    end: b,
                    style,
                }),
            }
        }

        Paragraph {
            text: self.text.clone(),
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            resolved_list_indent: self.resolved_list_indent,
            dirty: true,
            source_xml: None,
            /* Issue #56 — `apply_style` never touches `text`, so every byte
            offset these overlays anchor to (hyperlink spans, tracked-change
            revisions, field ranges, inline objects) stays valid, and
            `style_id` / `direct_overrides` aren't offset-anchored at all.
            Clearing them here silently stripped a paragraph's hyperlinks,
            revisions, and `<w:pStyle>` binding the moment ANY character
            formatting was applied. Contrast `delete_text` / `split_at` /
            `concat` below, which clear because the offsets genuinely shift. */
            inline_objects: self.inline_objects.clone(),
            hyperlinks: self.hyperlinks.clone(),
            revisions: self.revisions.clone(),
            fields: self.fields.clone(),
            style_id: self.style_id.clone(),
            direct_overrides: self.direct_overrides.clone(),
            /* Phase 3 (#40) — not offset-anchored; formatting a marker
            paragraph must never dissolve its section break. */
            section_end: self.section_end.clone(),
            bookmarks: self.bookmarks.clone(),
            body_xml: self.body_xml.clone(),
            /* Issues #199 / #106 — no offset moves; the writer verifies
            each run's recorded `<w:rPr>` against the new style. */
            source_markup: self.source_markup.clone(),
            mark_revision: self.mark_revision.clone(),
        }
    }

    /// Normalize a wire byte offset into this paragraph's text — the
    /// crate-wide snap-down policy ([`snap_offset`], module docs).
    pub fn snap_offset(&self, offset: u32) -> u32 {
        snap_offset(&self.text, offset)
    }

    /// Byte range `[start, end)` of the word containing caret position
    /// `offset` — a whitespace-delimited span (PHASE_4_HEADLESS_UI.md §7,
    /// double-click select). When `offset` sits on whitespace, the run of
    /// whitespace is returned. `offset` is clamped to a char boundary.
    pub fn word_bounds(&self, offset: u32) -> (u32, u32) {
        let text = self.text.as_str();
        let len = text.len();
        if len == 0 {
            return (0, 0);
        }
        let off = self.snap_offset(offset) as usize;
        /* Classify by the char to the right; at end-of-text, the char left. */
        let ws = text[off..]
            .chars()
            .next()
            .or_else(|| text[..off].chars().next_back())
            .is_some_and(char::is_whitespace);

        let mut start = off;
        for (i, c) in text[..off].char_indices().rev() {
            if c.is_whitespace() == ws {
                start = i;
            } else {
                break;
            }
        }
        let mut end = off;
        for (i, c) in text[off..].char_indices() {
            if c.is_whitespace() == ws {
                end = off + i + c.len_utf8();
            } else {
                break;
            }
        }
        (start as u32, end as u32)
    }

    /// Return a copy with bytes `[s, e)` removed. Style spans are clipped and
    /// shifted across the deletion.
    pub fn delete_text(&self, s: u32, e: u32) -> Paragraph {
        let s = self.snap_offset(s);
        let e = self.snap_offset(e);
        if s >= e {
            return self.clone();
        }
        let mut text = self.text.clone();
        text.replace_range(s as usize..e as usize, "");
        let gap = e - s;
        let mut markup = self.source_markup.clone();
        SourceMarkup::note_delete(&mut markup, self.text.len() as u32, s, e);
        /* Map a pre-delete offset to its post-delete position. */
        let map = |p: u32| -> u32 {
            if p <= s {
                p
            } else if p >= e {
                p - gap
            } else {
                s
            }
        };
        let mut spans = Vec::new();
        for run in &self.spans {
            let (ns, ne) = (map(run.start), map(run.end));
            if ns < ne {
                spans.push(StyleRun {
                    start: ns,
                    end: ne,
                    style: run.style.clone(),
                });
            }
        }
        /* Issue #43 (field engine) — FIELD overlays are remapped, not
        dropped: a PAGE field in a footer must survive the user editing
        around it. Rules: strictly before the gap → unchanged; strictly
        after → shift left; fully CONTAINING the gap → shrink (Word
        keeps a field whose cached result you edit); anything crossing
        a gap boundary → drop (the atom is broken). Hyperlink/revision/
        inline-object remapping stays out of scope (issue #56). */
        let mut fields = Vec::new();
        for f in &self.fields {
            if !f.is_local() {
                /* Issue #81 — a multi-paragraph field end is a POINT
                (Head: its `begin` at `start`; Tail: its `end` at `end`),
                not an atom: it survives every edit, clamped into the
                gap's start when the gap swallowed it. */
                let map = |o: u32| {
                    if o >= e {
                        o - gap
                    } else if o > s {
                        s
                    } else {
                        o
                    }
                };
                let (ns, ne) = (map(f.start), map(f.end));
                fields.push(Field {
                    start: ns,
                    end: ne.max(ns),
                    ..f.clone()
                });
            } else if f.end <= s {
                fields.push(f.clone());
            } else if f.start >= e {
                fields.push(Field {
                    start: f.start - gap,
                    end: f.end - gap,
                    ..f.clone()
                });
            } else if f.start <= s && f.end >= e {
                let nf = Field {
                    start: f.start,
                    end: f.end - gap,
                    ..f.clone()
                };
                if nf.start < nf.end {
                    fields.push(nf);
                }
            }
        }
        /* Issue #69 — INLINE OBJECTS are remapped like fields: an anchor
        strictly before the gap is unchanged, one at/after the gap's end
        shifts left, and one whose sentinel byte lies inside the gap is
        dropped together with its sentinel (deleting the anchor deletes
        the object — Word semantics; for a floating image the object
        leaves the page with it). Before #69 this rebuild cleared the
        whole list, orphaning the surviving U+FFFC sentinels as tofu. */
        let inline_objects: Vec<InlineObject> = self
            .inline_objects
            .iter()
            .filter_map(|o| {
                if o.at < s {
                    Some(o.clone())
                } else if o.at >= e {
                    Some(InlineObject {
                        at: o.at - gap,
                        kind: o.kind.clone(),
                        anchor: o.anchor.clone(),
                        source_xml: o.source_xml.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            resolved_list_indent: self.resolved_list_indent,
            dirty: true,
            source_xml: None,
            /* Unlike `apply_style` (issue #56), THIS rebuild changes `text` —
            every stashed byte offset (hyperlink spans, revision ranges)
            would dangle across the deleted range. Clearing these overlays
            is a known, deliberately out-of-scope limitation (offset
            remapping is a separate, larger task), not an oversight.
            Inline objects are remapped above (issue #69) — an anchor is a
            single sentinel byte, so the remap is exact (issue #80: a
            footnote reference survives editing the words around it). */
            inline_objects,
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields,
            /* Issue #277 — the paragraph style binding and the direct
            paragraph formatting are not offset-anchored (same class as
            `apply_style`, issue #56): deleting characters inside a
            Heading must not demote it to an unstyled paragraph. */
            style_id: self.style_id.clone(),
            direct_overrides: self.direct_overrides.clone(),
            /* Phase 3 (#40) — NOT cleared with the overlays above: the
            marker has no byte offsets and the paragraph mark survives
            an in-paragraph character deletion. */
            section_end: self.section_end.clone(),
            bookmarks: self.bookmarks.clone(),
            body_xml: self.body_xml.clone(),
            source_markup: markup,
            /* Issue #262 — the paragraph mark is untouched. */
            mark_revision: self.mark_revision.clone(),
        }
    }

    /// Split into `[0, at)` and `[at, len)`. Spans straddling `at` are split.
    pub fn split_at(&self, at: u32) -> (Paragraph, Paragraph) {
        let at = self.snap_offset(at);
        let (markup_left, markup_right) =
            SourceMarkup::split_at(&self.source_markup, self.text.len() as u32, at);
        let mut left = Vec::new();
        let mut right = Vec::new();
        for run in &self.spans {
            if run.start < at {
                left.push(StyleRun {
                    start: run.start,
                    end: run.end.min(at),
                    style: run.style.clone(),
                });
            }
            if run.end > at {
                right.push(StyleRun {
                    start: run.start.max(at) - at,
                    end: run.end - at,
                    style: run.style.clone(),
                });
            }
        }
        /* Splitting shifts every offset in the right half to be relative to
        `at` — the same "offsets genuinely shift" class as `delete_text`
        (see its comment). Hyperlink/revision dropping is the same
        deliberately out-of-scope limitation (issue #56); FIELDS remap
        (issue #43): whole-side fields survive, a field straddling the
        split point is dropped (the atom is broken). */
        let mut fields_left = Vec::new();
        let mut fields_right = Vec::new();
        for f in &self.fields {
            match f.span {
                /* Issue #81 — a Head's `begin` stays left when it sits
                before the split (its region now continues through the
                new right paragraph), otherwise it moves right. A Tail's
                `end` stays left when it sits at/before the split. */
                Some(FieldSpan::Head) if f.start < at => fields_left.push(Field {
                    end: at,
                    ..f.clone()
                }),
                Some(FieldSpan::Head) => fields_right.push(Field {
                    start: f.start - at,
                    end: (f.end.max(f.start)) - at,
                    ..f.clone()
                }),
                Some(FieldSpan::Tail) if f.end <= at => fields_left.push(Field {
                    start: 0,
                    ..f.clone()
                }),
                Some(FieldSpan::Tail) => fields_right.push(Field {
                    start: 0,
                    end: f.end - at,
                    ..f.clone()
                }),
                None if f.end <= at => fields_left.push(f.clone()),
                None if f.start >= at => fields_right.push(Field {
                    start: f.start - at,
                    end: f.end - at,
                    ..f.clone()
                }),
                None => {}
            }
        }
        /* Issue #80 — inline objects travel with the half that holds
        their sentinel (an anchor is one byte, so nothing straddles). */
        let mut objects_left = Vec::new();
        let mut objects_right = Vec::new();
        for o in &self.inline_objects {
            if o.at < at {
                objects_left.push(o.clone());
            } else {
                objects_right.push(InlineObject {
                    at: o.at - at,
                    kind: o.kind.clone(),
                    anchor: o.anchor.clone(),
                    source_xml: o.source_xml.clone(),
                });
            }
        }
        (
            Paragraph {
                text: self.text[..at as usize].to_owned(),
                spans: left,
                props: self.props.clone(),
                list_item: self.list_item,
                resolved_marker: self.resolved_marker.clone(),
                resolved_list_indent: self.resolved_list_indent,
                dirty: true,
                source_xml: None,
                inline_objects: objects_left,
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: fields_left,
                /* Issue #277 — both halves keep the paragraph style and
                the direct paragraph formatting (Word: a mid-paragraph
                split leaves two paragraphs in the same style). The
                next-style rule for Enter at the paragraph END is
                `DocumentTree::split_paragraph`'s business, not this
                primitive's (a clipboard slice must keep the style). */
                style_id: self.style_id.clone(),
                direct_overrides: self.direct_overrides.clone(),
                /* Phase 3 (#40) — the LEFT half receives a brand-new
                paragraph mark; the original mark (and any section
                marker riding it) belongs to the right half. */
                section_end: None,
                /* Issue #81 — paragraph-scoped bookmarks anchor at the
                paragraph START, which the left half keeps. */
                bookmarks: self.bookmarks.clone(),
                /* Issue #120 — the leading envelope markup stays with the
                left half, the trailing markup moves right with the mark:
                a content control wrapping the paragraph wraps both halves. */
                body_xml: BodyPassthrough::before_only(&self.body_xml),
                source_markup: markup_left,
                /* Issue #262 — a fresh mark for the left half. */
                mark_revision: None,
            },
            Paragraph {
                text: self.text[at as usize..].to_owned(),
                spans: right,
                props: self.props.clone(),
                list_item: self.list_item,
                resolved_marker: self.resolved_marker.clone(),
                resolved_list_indent: self.resolved_list_indent,
                dirty: true,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: fields_right,
                style_id: self.style_id.clone(),
                direct_overrides: self.direct_overrides.clone(),
                /* Phase 3 (#40) — the ORIGINAL paragraph mark terminates
                the right half, so a section marker travels with it. */
                section_end: self.section_end.clone(),
                bookmarks: Vec::new(),
                body_xml: BodyPassthrough::after_only(&self.body_xml),
                source_markup: markup_right,
                /* Issue #262 — the original mark ends the right half. */
                mark_revision: self.mark_revision.clone(),
            },
        )
    }

    /// Append `other` to a copy of `self`, shifting `other`'s spans right.
    /// The merged paragraph keeps `self`'s alignment — the surviving
    /// paragraph mark wins when a paragraph break is deleted.
    pub fn concat(&self, other: &Paragraph) -> Paragraph {
        let shift = self.text.len() as u32;
        let mut text = self.text.clone();
        text.push_str(&other.text);
        let mut spans = self.spans.clone();
        for run in &other.spans {
            spans.push(StyleRun {
                start: run.start + shift,
                end: run.end + shift,
                style: run.style.clone(),
            });
        }
        /* Concatenation shifts `other`'s offsets right by `self`'s length —
        the same "offsets genuinely shift" class as `delete_text` (see its
        comment). Hyperlink/revision dropping is the same deliberately
        out-of-scope limitation, not an oversight (issue #56); the merged
        paragraph also has two candidate `style_id`s to reconcile, which
        offset remapping would need to resolve anyway. FIELDS remap
        (issue #43): both sides' fields survive, tail's shifted right. */
        let mut fields = self.fields.clone();
        /* Issue #81 — a multi-paragraph Head runs to the paragraph end,
        which the merge just moved. */
        for f in fields.iter_mut() {
            if f.span == Some(FieldSpan::Head) {
                f.end = text.len() as u32;
            }
        }
        for f in &other.fields {
            fields.push(Field {
                start: f.start + shift,
                end: f.end + shift,
                ..f.clone()
            });
        }
        let mut bookmarks = self.bookmarks.clone();
        bookmarks.extend(other.bookmarks.iter().cloned());
        /* Issue #80 — both sides' inline objects survive; the tail's
        anchors shift right with its text. */
        let mut inline_objects = self.inline_objects.clone();
        for o in &other.inline_objects {
            inline_objects.push(InlineObject {
                at: o.at + shift,
                kind: o.kind.clone(),
                anchor: o.anchor.clone(),
                source_xml: o.source_xml.clone(),
            });
        }
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            resolved_list_indent: self.resolved_list_indent,
            dirty: true,
            source_xml: None,
            inline_objects,
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields,
            style_id: None,
            direct_overrides: ParaProperties::default(),
            /* Phase 3 (#40) — DELIBERATELY INVERTED from the head-wins
            convention every other field above follows: the surviving
            paragraph mark for `section_end` purposes is the TAIL's.
            A merge deletes the HEAD's mark, and with it any section
            break riding that mark — Word-exact (deleting a section
            break makes the preceding text adopt the FOLLOWING
            section's properties). Do not "fix" this to self.*. */
            section_end: other.section_end.clone(),
            bookmarks,
            /* Issue #120 — the merged block sits where both did: it keeps
            the HEAD's leading envelope markup and the TAIL's trailing
            markup, so a content control wrapping both still wraps the
            merge. */
            body_xml: BodyPassthrough::merged(&self.body_xml, &other.body_xml),
            source_markup: SourceMarkup::concat(
                &self.source_markup,
                self.text.len() as u32,
                &other.source_markup,
                other.text.len() as u32,
            ),
            /* Issue #262 — the head's mark is the one deleted: the
            surviving mark (and its tracked change) is the tail's. */
            mark_revision: other.mark_revision.clone(),
        }
    }

    /// Issue #43 (design review M5) — replace byte range `[start, end)`
    /// with `replacement`, remapping EVERY overlay with the full
    /// clamp-and-drop-degenerate discipline:
    ///
    /// - offsets ≤ `start` are unchanged; offsets ≥ `end` shift by the
    ///   length delta;
    /// - a START boundary strictly inside the range clamps to `start`,
    ///   an END boundary strictly inside clamps to `start + rep_len`
    ///   (a span reaching into the replaced text stretches over the
    ///   whole replacement — visually closest for bold-over-a-field);
    /// - degenerate results (`start >= end`) are dropped;
    /// - an inline-object anchor strictly inside the range is dropped
    ///   (its sentinel byte no longer exists).
    ///
    /// This is the field-resolution splice primitive: the per-page
    /// reshape and the body substitution pass both run it on CLONES
    /// destined for layout — it deliberately does NOT set `dirty` or
    /// clear `source_xml` (the model text is not being edited).
    pub fn with_spliced_range(&self, start: u32, end: u32, replacement: &str) -> Paragraph {
        let start = self.snap_offset(start);
        let end = self.snap_offset(end).max(start);
        let rep_len = replacement.len() as u32;
        let old_len = end - start;
        let mut text = self.text.clone();
        text.replace_range(start as usize..end as usize, replacement);
        let map_start = |o: u32| -> u32 {
            if o <= start {
                o
            } else if o >= end {
                o - old_len + rep_len
            } else {
                start
            }
        };
        let map_end = |o: u32| -> u32 {
            if o <= start {
                o
            } else if o >= end {
                o - old_len + rep_len
            } else {
                start + rep_len
            }
        };
        let mut out = self.clone();
        out.text = text;
        out.spans = self
            .spans
            .iter()
            .filter_map(|s| {
                let (ns, ne) = (map_start(s.start), map_end(s.end));
                (ns < ne).then(|| StyleRun {
                    start: ns,
                    end: ne,
                    style: s.style.clone(),
                })
            })
            .collect();
        out.hyperlinks = self
            .hyperlinks
            .iter()
            .filter_map(|h| {
                let (ns, ne) = (map_start(h.start), map_end(h.end));
                (ns < ne).then(|| Hyperlink {
                    start: ns,
                    end: ne,
                    ..h.clone()
                })
            })
            .collect();
        out.revisions = self
            .revisions
            .iter()
            .filter_map(|r| {
                let (ns, ne) = (map_start(r.start), map_end(r.end));
                (ns < ne).then(|| {
                    let mut nr = r.clone();
                    nr.start = ns;
                    nr.end = ne;
                    nr
                })
            })
            .collect();
        out.fields = self
            .fields
            .iter()
            .filter_map(|f| {
                let (ns, ne) = (map_start(f.start), map_end(f.end));
                /* Issue #81 — multi-paragraph ends are points: keep. */
                (ns < ne || !f.is_local()).then(|| Field {
                    start: ns,
                    end: ne.max(ns),
                    ..f.clone()
                })
            })
            .collect();
        out.inline_objects = self
            .inline_objects
            .iter()
            .filter(|o| o.at <= start || o.at >= end)
            .map(|o| InlineObject {
                at: map_start(o.at),
                kind: o.kind.clone(),
                anchor: o.anchor.clone(),
                source_xml: o.source_xml.clone(),
            })
            .collect();
        out
    }

    /// Byte offset of the UAX-#29 extended grapheme cluster boundary
    /// immediately before `o` (clamped to 0). Audit gap B.H1 — stepping
    /// by Unicode scalar (`char`) bisects Arabic harakat, Devanagari
    /// conjuncts, emoji ZWJ sequences; grapheme stepping keeps each
    /// user-perceived character atomic so Backspace removes a whole
    /// cluster instead of leaving an orphaned combining mark.
    pub fn prev_offset(&self, o: u32) -> u32 {
        use unicode_segmentation::UnicodeSegmentation;
        let o = self.snap_offset(o) as usize;
        self.text[..o]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i as u32)
    }

    /// Byte offset of the UAX-#29 extended grapheme cluster boundary
    /// immediately after `o` (clamped to len). See [`Self::prev_offset`]
    /// for the symmetric rationale.
    pub fn next_offset(&self, o: u32) -> u32 {
        use unicode_segmentation::UnicodeSegmentation;
        let o = self.snap_offset(o) as usize;
        self.text[o..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.text.len() as u32, |(rel_i, _)| (o + rel_i) as u32)
    }
}

/* ===================================================================
Phase 5 PR 1 — Table model
==================================================================== */

/// Border line style (`<w:val>` on `<w:left>` / `<w:top>` / …).
/// Phase 5 PR 1 ships the common subset; `Other` preserves the
/// original token for round-trip.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub enum BorderStyle {
    #[default]
    Single,
    Double,
    Dotted,
    Dashed,
    None,
    Other(String),
}

/// One border edge stroke. `size_eighth_pt` is `<w:sz>` (eighths of a
/// point — the OOXML unit); divide by 8 to get points, by 6 to get px
/// at 96 DPI.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct BorderStroke {
    pub style: BorderStyle,
    pub size_eighth_pt: u16,
    pub color: Option<[u8; 4]>,
}

/// Per-edge border strokes for a `<w:tcBorders>` or `<w:tblBorders>`.
/// `inside_h` / `inside_v` only apply when carried at the table level
/// (`<w:tblBorders>`); cell-level borders ignore them.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct CellBorders {
    pub top: Option<BorderStroke>,
    pub left: Option<BorderStroke>,
    pub bottom: Option<BorderStroke>,
    pub right: Option<BorderStroke>,
    pub inside_h: Option<BorderStroke>,
    pub inside_v: Option<BorderStroke>,
}

/// `<w:tblCellMar>` (table default) or `<w:tcMar>` (per-cell override)
/// cell padding. Per-edge `Option<i32>` because OOXML lets each edge
/// override independently — a `<w:tcMar>` carrying only `<w:left>` and
/// `<w:right>` inherits top/bottom from the table default, which itself
/// can also leave edges unset. The layout solver collapses the
/// inherit chain via [`CellMargins::resolve_edges`] →
/// [`ResolvedCellMargins`] (every edge populated with Word stock as
/// the final fallback).
///
/// `default()` is all-`None` — meaning every edge inherits.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(default)]
pub struct CellMargins {
    pub top_twips: Option<i32>,
    pub left_twips: Option<i32>,
    pub bottom_twips: Option<i32>,
    pub right_twips: Option<i32>,
}

/// Fully-resolved per-edge padding the layout solver consumes. Every
/// edge is populated; the [`CellMargins::resolve_edges`] resolver
/// walks cell override → table default → Word stock for each edge
/// independently.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCellMargins {
    pub top_twips: i32,
    pub left_twips: i32,
    pub bottom_twips: i32,
    pub right_twips: i32,
}

impl CellMargins {
    /// Word's stock cell padding (0 / 108 / 0 / 108 twips). Used by the
    /// layout solver when neither `<w:tcMar>` nor `<w:tblCellMar>`
    /// specify an explicit value — matches what `winword.exe` emits on
    /// a freshly-inserted table.
    pub const fn word_default() -> ResolvedCellMargins {
        ResolvedCellMargins {
            top_twips: 0,
            left_twips: 108,
            bottom_twips: 0,
            right_twips: 108,
        }
    }

    /// Per-edge resolver — for each edge, return the cell override if
    /// set, otherwise the table default, otherwise Word's stock value.
    /// Crucially, each edge is resolved INDEPENDENTLY: a cell
    /// `<w:tcMar>` setting only `<w:left>` and `<w:right>` correctly
    /// inherits top/bottom from the table default (or Word stock if
    /// the table also leaves them unset).
    pub fn resolve_edges(cell: Option<&Self>, table: &Self) -> ResolvedCellMargins {
        let stock = Self::word_default();
        let pick = |c: Option<i32>, t: Option<i32>, s: i32| -> i32 { c.or(t).unwrap_or(s) };
        ResolvedCellMargins {
            top_twips: pick(
                cell.and_then(|c| c.top_twips),
                table.top_twips,
                stock.top_twips,
            ),
            left_twips: pick(
                cell.and_then(|c| c.left_twips),
                table.left_twips,
                stock.left_twips,
            ),
            bottom_twips: pick(
                cell.and_then(|c| c.bottom_twips),
                table.bottom_twips,
                stock.bottom_twips,
            ),
            right_twips: pick(
                cell.and_then(|c| c.right_twips),
                table.right_twips,
                stock.right_twips,
            ),
        }
    }
}

/// `<w:tcW>` / `<w:tblW>` width — twips, percent (50-thousandths per
/// OOXML), auto (content-driven), or nil (no width).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidth {
    Dxa(i32),
    Pct(u16),
    Auto,
    Nil,
}

/// `<w:vMerge>` — vertical merge role.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VMergeRole {
    /// Independent cell.
    #[default]
    None,
    /// Top of a vertical span; renders its content, spans down through
    /// every `Continue` cell directly below.
    Restart,
    /// Placeholder; content is ignored at render time (the `Restart`
    /// cell visually owns the merged block).
    Continue,
}

/// `<w:vAlign>` — vertical alignment of a cell's blocks within the
/// cell bounding box.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

/// `<w:trHeight>` row height. `hRule` decides whether the value is a
/// minimum, exact, or auto-fit.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowHeight {
    Auto,
    AtLeast { twips: i32 },
    Exact { twips: i32 },
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct RowProperties {
    pub height: Option<RowHeight>,
    /// `<w:cantSplit/>` — row cannot break across pages. Phase 5a
    /// treats this as implicit-on for every row (no mid-row pagination
    /// yet). Carried verbatim for round-trip.
    pub cant_split: bool,
    /// `<w:tblHeader/>` — row repeats at the top of every page after
    /// a break. Phase 5a captures but does not honour.
    pub header: bool,
    /// Issue #84 — unmodeled `<w:trPr>` children, verbatim. See
    /// [`GrabBag`].
    pub grab_bag: Option<Box<GrabBag>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct CellProperties {
    pub grid_span: u8,
    pub v_merge: VMergeRole,
    pub width: Option<CellWidth>,
    pub borders: Option<CellBorders>,
    pub shading: Option<[u8; 4]>,
    pub v_align: VerticalAlign,
    /// Phase 2 audit (gap B.1) — `<w:tcMar>` per-cell padding override.
    /// `None` ⇒ inherit from the table's `<w:tblCellMar>`; an explicit
    /// `Some` value wins per-edge as resolved by
    /// [`CellMargins::resolve_edges`].
    pub cell_margins: Option<CellMargins>,
    /// Issue #84 — unmodeled `<w:tcPr>` children, verbatim. See
    /// [`GrabBag`].
    pub grab_bag: Option<Box<GrabBag>>,
}

/// Audit gap A.M8 — `<w:tblLayout w:type>`. `Autofit` (Word's
/// default) measures cell content and distributes column widths to
/// fit the available band; `Fixed` honours `<w:tblGrid>` verbatim
/// regardless of content.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableLayout {
    #[default]
    Autofit,
    Fixed,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct TableProperties {
    pub width: Option<CellWidth>,
    pub alignment: Option<Alignment>,
    pub indent_twips: i32,
    pub borders: Option<CellBorders>,
    pub cell_margins: CellMargins,
    pub table_style_id: Option<String>,
    /// Audit gap A.M8 — `<w:tblLayout w:type="autofit|fixed"/>`.
    /// Default `Autofit` matches Word's behaviour when the element
    /// is absent.
    pub layout: TableLayout,
    /// Issue #79 — `<w:tblPr><w:bidiVisual/>` (ECMA-376 §17.4.1): the
    /// table is presented right-to-left — grid column 1 is the visually
    /// rightmost column, and the start/end (`left`/`right`) cell edges
    /// (borders, margins) resolve to the right/left visual edges. Purely
    /// visual: the grid, spans, the logical cell order in `rows` (and so
    /// Tab order) are unchanged. `false` when the element is absent or
    /// explicitly off.
    pub bidi_visual: bool,
    /// Issue #84 — unmodeled `<w:tblPr>` children (`<w:tblLook>`,
    /// `<w:tblpPr>`, …), verbatim. See [`GrabBag`].
    pub grab_bag: Option<Box<GrabBag>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct TableCell {
    pub props: CellProperties,
    /// Nested block sequence. `Vec`, not `im::Vector`: cells average
    /// 1-2 paragraphs, so persistent-vector overhead is not worth the
    /// structural-sharing win at that size (RFC §1.4).
    pub blocks: Vec<Block>,
    /// Issue #248 — the source `<w:tc>` markup (see
    /// [`CellSourceMarkup`]). Rides the cell object, so it follows the
    /// cell through every table restructuring. Skipped when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_markup: Option<Box<CellSourceMarkup>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct TableRow {
    pub props: RowProperties,
    pub cells: Vec<TableCell>,
    /// Issue #248 — the source `<w:tr>` markup (see [`RowSourceMarkup`]).
    /// Skipped when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_markup: Option<Box<RowSourceMarkup>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Table {
    /// `<w:tblGrid>` — column template widths in twips. Length is the
    /// logical column count; cells with `grid_span > 1` consume
    /// multiple template columns.
    pub grid: Vec<i32>,
    pub props: TableProperties,
    pub rows: Vec<TableRow>,
    /// Phase 3 passthrough mirror: `false` on load, `true` after any
    /// mutation. Writer emits `source_xml` verbatim when clean.
    pub dirty: bool,
    /// Raw `<w:tbl>...</w:tbl>` source bytes captured by the reader.
    /// `None` for engine-synthesised tables.
    #[serde(with = "serde_bytes")]
    pub source_xml: Option<Vec<u8>>,
    /// Issue #120 — block-level passthrough markup surrounding this table
    /// (see [`Paragraph::body_xml`]).
    pub body_xml: Option<Box<BodyPassthrough>>,
    /// Issue #248 — the source `<w:tbl>` markup (see
    /// [`TableSourceMarkup`]). Skipped when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_markup: Option<Box<TableSourceMarkup>>,
}

/// Issue #248 — one source property element of a table (`<w:tblPr>`,
/// `<w:tblGrid>`, `<w:tblPrEx>`, `<w:trPr>`, `<w:tcPr>`) as read, with
/// the model state it produced. `lead` is what the source wrote between
/// the previous sibling (or the parent's start tag) and the element — the
/// whitespace of a pretty-printed part — and is re-emitted whenever the
/// element is written. `xml` is re-emitted verbatim only while the
/// owner's live model still equals `model` (a *verified* passthrough, the
/// `<w:pPr>` rule of #199); otherwise the element regenerates, adopting
/// the source spelling of every unchanged empty child.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct SourceElement<T> {
    #[serde(with = "serde_bytes")]
    pub lead: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub xml: Vec<u8>,
    pub model: T,
}

/// Issue #248 — attribute-level + whitespace source markup of a
/// `<w:tbl>` read from a `.docx`, the table counterpart of
/// [`SourceMarkup`]. A clean table never consults it (its `source_xml`
/// passthrough wins); a regenerated one (any cell edit or table command)
/// uses it to stay byte-close to the source: the `<w:tbl>` attributes,
/// the verified `<w:tblPr>` bytes and the verified `<w:tblGrid>` bytes
/// (`<w:tblGridChange>` included). What sits between rows (whitespace,
/// bookmarks, a row-level `<w:sdt>` wrapper) rides each row's
/// [`RowSourceMarkup::body_xml`].
///
/// Nothing here is offset-anchored: the row / cell markup lives ON the
/// row / cell objects, so a row or column insert / delete, a merge or a
/// split carries it with the content it describes (a fresh row or cell
/// has none and is written plainly), and the property bytes are
/// re-verified against the model at every write — so the markup can
/// never land on the wrong element.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct TableSourceMarkup {
    /// `<w:tbl>` attributes, source order.
    pub attrs: Vec<SourceAttr>,
    pub tbl_pr: Option<SourceElement<TableProperties>>,
    pub grid: Option<SourceElement<Vec<i32>>>,
}

/// Issue #248 — source markup of one `<w:tr>` (see [`TableSourceMarkup`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct RowSourceMarkup {
    /// `<w:tr>` attributes (`w:rsidR`, `w14:paraId`, …), source order.
    pub attrs: Vec<SourceAttr>,
    /// Row-level passthrough between the rows of the table: whitespace
    /// and range markers before the `<w:tr>` (`before`), a `<w:sdt>` /
    /// `<w:customXml>` wrapper around one or more rows as an
    /// `Open` / `Close` pair (issue #245's `Bug66263-table.docx`), the
    /// whitespace before `</w:tbl>` (`after` of the last row). Same
    /// fragments and writer stack as the block level (issue #120).
    pub body_xml: Option<Box<BodyPassthrough>>,
    /// Issue #103 — the row's `<w:tblPrEx>` (table property exceptions),
    /// unmodeled: always re-emitted verbatim (`model` unused).
    pub tbl_pr_ex: Option<SourceElement<()>>,
    pub tr_pr: Option<SourceElement<RowProperties>>,
}

/// Issue #248 — source markup of one `<w:tc>` (see [`TableSourceMarkup`]).
/// The whitespace inside the cell around its blocks rides the blocks'
/// own `body_xml` (issue #120).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct CellSourceMarkup {
    /// `<w:tc>` attributes, source order.
    pub attrs: Vec<SourceAttr>,
    /// Cell-level passthrough between the cells of a row (whitespace,
    /// markers, a cell-level `<w:sdt>` / `<w:customXml>` wrapper; the
    /// whitespace before `</w:tr>` is the last cell's `after`).
    pub body_xml: Option<Box<BodyPassthrough>>,
    pub tc_pr: Option<SourceElement<CellProperties>>,
}

/* ===================================================================
Issues #114 / #116 — table bounds + typed table errors.
Every table dimension and coordinate off the wire is validated here,
BEFORE any allocation or index. See the module docs.
==================================================================== */

/// Word's hard limit on table columns (`Insert Table` dialog, OOXML
/// interoperability ceiling).
pub const MAX_TABLE_COLS: u32 = 63;
/// Word's hard limit on table rows.
pub const MAX_TABLE_ROWS: u32 = 32_767;
/// Engine-side cap on `rows × cols` for one table. Cells are eager
/// (`TableCell` carries a real `Paragraph`), so a Word-legal 32 767 × 63
/// request would still be ~2 M paragraphs — far past the 256 MiB
/// per-worker soft budget. 65 535 cells ≈ a 1 000-row × 63-column grid,
/// or a 32 767-row × 2-column one.
pub const MAX_TABLE_CELLS: u64 = 65_535;

/// Why a table command was rejected (issue #116). Every variant maps to
/// a typed `Event::Error` in `engine-wasm`; none of them is ever a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableError {
    /// The path does not address a `Block::Table`.
    NotATable {
        path: BlockPath,
    },
    /// The path addresses a table nested inside a cell; mutation of
    /// nested tables is not supported yet (PR 3b).
    NestedUnsupported {
        path: BlockPath,
    },
    RowOutOfRange {
        row: u32,
        rows: usize,
    },
    ColOutOfRange {
        col: u32,
        cols: usize,
    },
    /// A zero row or column count.
    ZeroDimension,
    TooManyRows {
        requested: u64,
        max: u32,
    },
    TooManyCols {
        requested: u64,
        max: u32,
    },
    TooManyCells {
        requested: u64,
        max: u64,
    },
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableError::NotATable { path } => {
                write!(f, "path {:?} does not address a table", path.steps)
            }
            TableError::NestedUnsupported { path } => write!(
                f,
                "path {:?} addresses a nested table; nested-table editing is not supported yet",
                path.steps
            ),
            TableError::RowOutOfRange { row, rows } => {
                write!(f, "row {row} is out of range (table has {rows} rows)")
            }
            TableError::ColOutOfRange { col, cols } => {
                write!(f, "column {col} is out of range (row has {cols} cells)")
            }
            TableError::ZeroDimension => write!(f, "a table needs at least one row and one column"),
            TableError::TooManyRows { requested, max } => {
                write!(f, "{requested} rows exceeds the {max}-row limit")
            }
            TableError::TooManyCols { requested, max } => {
                write!(f, "{requested} columns exceeds the {max}-column limit")
            }
            TableError::TooManyCells { requested, max } => {
                write!(f, "{requested} cells exceeds the {max}-cell limit")
            }
        }
    }
}

impl std::error::Error for TableError {}

/// Validate a `rows × cols` request against the caps — pure arithmetic,
/// no allocation. Issue #114.
pub fn check_table_dims(rows: u32, cols: u32) -> Result<(), TableError> {
    if rows == 0 || cols == 0 {
        return Err(TableError::ZeroDimension);
    }
    if rows > MAX_TABLE_ROWS {
        return Err(TableError::TooManyRows {
            requested: rows as u64,
            max: MAX_TABLE_ROWS,
        });
    }
    if cols > MAX_TABLE_COLS {
        return Err(TableError::TooManyCols {
            requested: cols as u64,
            max: MAX_TABLE_COLS,
        });
    }
    let cells = rows as u64 * cols as u64;
    if cells > MAX_TABLE_CELLS {
        return Err(TableError::TooManyCells {
            requested: cells,
            max: MAX_TABLE_CELLS,
        });
    }
    Ok(())
}

/// Why a `SetZoom` / `SetDeviceScale` scale was rejected (issue #186).
/// Maps to a typed `Event::Error` in `engine-wasm`; never a panic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScaleError {
    /// NaN or ±infinity — `f32::clamp` would otherwise pass a NaN
    /// straight through untouched (both clamp comparisons are `false`
    /// for NaN), landing an unusable scale in the layout config.
    NotFinite { value: f32 },
}

impl std::fmt::Display for ScaleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScaleError::NotFinite { value } => {
                write!(f, "scale {value} is not finite (NaN or ±infinity)")
            }
        }
    }
}

impl std::error::Error for ScaleError {}

/// Reject a non-finite `SetZoom` / `SetDeviceScale` scale before it
/// reaches `.clamp()`. A finite value (including one outside the
/// documented `[0.25, 4.0]` / `[0.5, 8.0]` bounds) is `Ok` — the
/// existing `.clamp()` call in `do_set_zoom` / `do_set_device_scale`
/// still handles range clamping; this only guards finiteness. Issue #186.
pub fn validate_finite_scale(value: f32) -> Result<(), ScaleError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(ScaleError::NotFinite { value })
    }
}

/// Why a `SetRenderDate` payload was rejected (issue #187). Maps to a
/// typed `Event::Error` in `engine-wasm`; never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateError {
    YearOutOfRange {
        year: i32,
    },
    MonthOutOfRange {
        month: u32,
    },
    /// `day` is out of range for `(year, month)` — leap years widen
    /// February to 29 days.
    DayOutOfRange {
        year: i32,
        month: u32,
        day: u32,
    },
    HourOutOfRange {
        hour: u32,
    },
    MinuteOutOfRange {
        minute: u32,
    },
}

impl std::fmt::Display for DateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DateError::YearOutOfRange { year } => {
                write!(f, "year {year} is out of range (must be 1..=9999)")
            }
            DateError::MonthOutOfRange { month } => {
                write!(f, "month {month} is out of range (must be 1..=12)")
            }
            DateError::DayOutOfRange { year, month, day } => write!(
                f,
                "day {day} is out of range for {year}-{month:02} \
                 (has {} days)",
                days_in_month(*year, *month)
            ),
            DateError::HourOutOfRange { hour } => {
                write!(f, "hour {hour} is out of range (must be 0..=23)")
            }
            DateError::MinuteOutOfRange { minute } => {
                write!(f, "minute {minute} is out of range (must be 0..=59)")
            }
        }
    }
}

impl std::error::Error for DateError {}

/// Proleptic Gregorian leap-year rule — divisible by 4, except
/// centuries, except every 4th century.
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days in `month` of `year` (1-based month); `0` for an out-of-range
/// month — callers validate `month` first, so this is only reached with
/// `1..=12` in practice, but stays total rather than panicking.
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Validate a `Command::SetRenderDate` payload — issue #187. `hour` /
/// `minute` are the optional TIME half (`SetRenderDate`'s "both or
/// neither" contract is enforced by the caller, not here); when
/// present each is range-checked independently. Order matches the
/// field order a fuzzer / UI is most likely to get wrong first (year,
/// then month, since `day` validity depends on both).
pub fn validate_render_date(
    year: i32,
    month: u32,
    day: u32,
    hour: Option<u32>,
    minute: Option<u32>,
) -> Result<(), DateError> {
    if !(1..=9999).contains(&year) {
        return Err(DateError::YearOutOfRange { year });
    }
    if !(1..=12).contains(&month) {
        return Err(DateError::MonthOutOfRange { month });
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(DateError::DayOutOfRange { year, month, day });
    }
    if let Some(h) = hour
        && h > 23
    {
        return Err(DateError::HourOutOfRange { hour: h });
    }
    if let Some(m) = minute
        && m > 59
    {
        return Err(DateError::MinuteOutOfRange { minute: m });
    }
    Ok(())
}

impl Table {
    /// Logical column count — the grid width, falling back to the first
    /// row's cell count for a grid-less (reader-synthesised) table.
    pub fn column_count(&self) -> usize {
        if self.grid.is_empty() {
            self.rows.first().map_or(0, |r| r.cells.len())
        } else {
            self.grid.len()
        }
    }

    /// Would adding `add_rows` rows and `add_cols` columns keep this table
    /// inside the caps? Checked by `InsertRow` / `InsertColumn` before
    /// they allocate (issue #114 sibling audit).
    pub fn check_growth(&self, add_rows: u32, add_cols: u32) -> Result<(), TableError> {
        let rows = self.rows.len() as u64 + add_rows as u64;
        let cols = self.column_count() as u64 + add_cols as u64;
        if rows > MAX_TABLE_ROWS as u64 {
            return Err(TableError::TooManyRows {
                requested: rows,
                max: MAX_TABLE_ROWS,
            });
        }
        if cols > MAX_TABLE_COLS as u64 {
            return Err(TableError::TooManyCols {
                requested: cols,
                max: MAX_TABLE_COLS,
            });
        }
        if rows * cols > MAX_TABLE_CELLS {
            return Err(TableError::TooManyCells {
                requested: rows * cols,
                max: MAX_TABLE_CELLS,
            });
        }
        Ok(())
    }
}

/* ===================================================================
LogicalPos — BlockPath addressing (Phase 5 PR 4).
`path` walks the block tree to a `Block::Paragraph`; `offset` is the
caret's byte offset inside that paragraph's UTF-8 text. Cross-cell
ranges work as long as the endpoints share a parent container; full
cross-container linear semantics ship with Phase 5c (the engine
currently clamps to the deeper endpoint's container).
==================================================================== */

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogicalPos {
    pub path: BlockPath,
    /// Byte offset within the paragraph (UTF-8).
    pub offset: u32,
}

impl LogicalPos {
    pub fn new(path: BlockPath, offset: u32) -> Self {
        Self { path, offset }
    }

    /// Path to the Nth top-level paragraph, skipping tables — the
    /// canonical compat shim for callers that still address the doc
    /// paragraph-flat (RFC §4: `BlockPath::root_paragraph(n)`).
    pub fn at_top_paragraph(doc: &DocumentTree, n: u32, offset: u32) -> Option<Self> {
        let path = doc.path_to_top_paragraph(n)?;
        Some(Self { path, offset })
    }
}

impl DocumentTree {
    pub fn new() -> Self {
        Self {
            blocks: Vector::new(),
            body_section: SectionProps::default(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnote_stories: std::collections::HashMap::new(),
            endnote_stories: std::collections::HashMap::new(),
            footnote_props: NoteProps::default(),
            endnote_props: NoteProps::default(),
            notes_dirty: NotesDirty::default(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
            hf_dirty: HfDirty::default(),
            settings_dirty: false,
            document_root_attrs: Vec::new(),
            part_root_attrs: Default::default(),
            document_envelope: Default::default(),
            source_package: None,
        }
    }

    /// Build a single-paragraph document from a plain string.
    pub fn from_text(text: &str) -> Self {
        let mut blocks = Vector::new();
        blocks.push_back(Block::Paragraph(Paragraph {
            text: text.to_owned(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        }));
        Self {
            blocks,
            body_section: SectionProps::default(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnote_stories: std::collections::HashMap::new(),
            endnote_stories: std::collections::HashMap::new(),
            footnote_props: NoteProps::default(),
            endnote_props: NoteProps::default(),
            notes_dirty: NotesDirty::default(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
            hf_dirty: HfDirty::default(),
            settings_dirty: false,
            document_root_attrs: Vec::new(),
            part_root_attrs: Default::default(),
            document_envelope: Default::default(),
            source_package: None,
        }
    }

    /// Build a document from a list of paragraph plain-text bodies.
    pub fn from_paragraphs<I: IntoIterator<Item = String>>(texts: I) -> Self {
        let mut blocks = Vector::new();
        for t in texts {
            blocks.push_back(Block::Paragraph(Paragraph {
                text: t,
                spans: Vec::new(),
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                resolved_list_indent: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
                section_end: None,
                bookmarks: Vec::new(),
                body_xml: None,
                source_markup: None,
                mark_revision: None,
            }));
        }
        Self {
            blocks,
            body_section: SectionProps::default(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnote_stories: std::collections::HashMap::new(),
            endnote_stories: std::collections::HashMap::new(),
            footnote_props: NoteProps::default(),
            endnote_props: NoteProps::default(),
            notes_dirty: NotesDirty::default(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
            hf_dirty: HfDirty::default(),
            settings_dirty: false,
            document_root_attrs: Vec::new(),
            part_root_attrs: Default::default(),
            document_envelope: Default::default(),
            source_package: None,
        }
    }

    /// Build a document from pre-styled paragraphs — the `.docx` reader (run
    /// properties → spans) and the HTML paste path both produce these.
    pub fn from_rich_paragraphs<I: IntoIterator<Item = Paragraph>>(paras: I) -> Self {
        let mut blocks = Vector::new();
        for p in paras {
            blocks.push_back(Block::Paragraph(p));
        }
        Self {
            blocks,
            body_section: SectionProps::default(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnote_stories: std::collections::HashMap::new(),
            endnote_stories: std::collections::HashMap::new(),
            footnote_props: NoteProps::default(),
            endnote_props: NoteProps::default(),
            notes_dirty: NotesDirty::default(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
            hf_dirty: HfDirty::default(),
            settings_dirty: false,
            document_root_attrs: Vec::new(),
            part_root_attrs: Default::default(),
            document_envelope: Default::default(),
            source_package: None,
        }
    }

    /// Build a document from a pre-mixed block sequence — the `.docx` reader
    /// (with tables) produces these. Phase 5 PR 1 entry point.
    pub fn from_blocks<I: IntoIterator<Item = Block>>(blocks_in: I) -> Self {
        let mut blocks = Vector::new();
        for b in blocks_in {
            blocks.push_back(b);
        }
        Self {
            blocks,
            body_section: SectionProps::default(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnote_stories: std::collections::HashMap::new(),
            endnote_stories: std::collections::HashMap::new(),
            footnote_props: NoteProps::default(),
            endnote_props: NoteProps::default(),
            notes_dirty: NotesDirty::default(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
            hf_dirty: HfDirty::default(),
            settings_dirty: false,
            document_root_attrs: Vec::new(),
            part_root_attrs: Default::default(),
            document_envelope: Default::default(),
            source_package: None,
        }
    }

    /// Phase 6 — build a document from a pre-mixed block sequence plus the
    /// section table the `.docx` reader collected from `<w:sectPr>` elements.
    /// Trims sections that fall outside the block range so the paginator
    /// never indexes off the end.
    ///
    /// Phase 3 (#40) — ranges are converted to the paragraph-anchored
    /// marker model at this boundary: every non-final section stamps its
    /// props onto the paragraph closing its range (`end_block - 1` — by
    /// reader construction always a `Block::Paragraph`, since an interior
    /// `<w:sectPr>` is itself a paragraph property); the final section
    /// becomes [`Self::body_section`]. Stamping does NOT dirty the
    /// paragraph — its `source_xml` already carries the sectPr verbatim,
    /// so the writer's passthrough stays byte-stable.
    pub fn from_blocks_with_sections<I: IntoIterator<Item = Block>>(
        blocks_in: I,
        sections_in: Vec<Section>,
    ) -> Self {
        let mut blocks = Vector::new();
        for b in blocks_in {
            blocks.push_back(b);
        }
        let len = blocks.len() as u32;
        let sections: Vec<Section> = sections_in
            .into_iter()
            .filter_map(|mut s| {
                s.start_block = s.start_block.min(len);
                s.end_block = s.end_block.min(len);
                if s.end_block <= s.start_block {
                    return None;
                }
                Some(s)
            })
            .collect();
        let mut body_section = SectionProps::default();
        if let Some((last, interior)) = sections.split_last() {
            body_section = SectionProps::from(last);
            for s in interior {
                let marker_idx = s.end_block.saturating_sub(1) as usize;
                /* Defensive: a range whose closing block is a table has
                no legal marker home (cannot happen via the reader —
                interior sectPr rides a paragraph). Walk back to the
                nearest paragraph inside the range; if none exists the
                boundary dissolves into the following section. */
                let start = s.start_block as usize;
                let mut target: Option<usize> = None;
                for idx in (start..=marker_idx.min(blocks.len().saturating_sub(1))).rev() {
                    if matches!(blocks.get(idx), Some(Block::Paragraph(_))) {
                        target = Some(idx);
                        break;
                    }
                }
                if let Some(idx) = target
                    && let Some(Block::Paragraph(p)) = blocks.get(idx)
                {
                    let mut p = p.clone();
                    p.section_end = Some(Box::new(SectionProps::from(s)));
                    blocks.set(idx, Block::Paragraph(p));
                }
            }
        }
        Self {
            blocks,
            body_section,
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnote_stories: std::collections::HashMap::new(),
            endnote_stories: std::collections::HashMap::new(),
            footnote_props: NoteProps::default(),
            endnote_props: NoteProps::default(),
            notes_dirty: NotesDirty::default(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
            hf_dirty: HfDirty::default(),
            settings_dirty: false,
            document_root_attrs: Vec::new(),
            part_root_attrs: Default::default(),
            document_envelope: Default::default(),
            source_package: None,
        }
    }

    /// Phase 6b — attach the parsed header / footer parts collected from
    /// `word/header*.xml` / `word/footer*.xml`, keyed by their relationship
    /// id (`r:id`). Consumed by the paginator when a section's
    /// `header_ref` / `footer_ref` resolves.
    pub fn with_header_footer_parts(
        mut self,
        headers: std::collections::HashMap<String, Vec<Block>>,
        footers: std::collections::HashMap<String, Vec<Block>>,
    ) -> Self {
        self.headers = headers;
        self.footers = footers;
        self
    }

    /// Phase 3 (#39) — replace (or create) the header part keyed by
    /// `rid` and mark it dirty for the writer. The only mutation path
    /// into [`Self::headers`] — story editing routes every content
    /// change through here so `hf_dirty` can never desync from the map.
    /// `section_end` markers are stripped: a part is not the body, and a
    /// stray marker would corrupt `effective_sections` if the blocks ever
    /// round-tripped through a body-shaped story tree.
    pub fn with_updated_header_part(&self, rid: &str, blocks: Vec<Block>) -> Self {
        let mut next = self.clone();
        next.headers
            .insert(rid.to_string(), strip_section_markers(blocks));
        next.hf_dirty.headers.insert(rid.to_string());
        next
    }

    /// Phase 3 (#39) — [`Self::with_updated_header_part`]'s footer twin.
    pub fn with_updated_footer_part(&self, rid: &str, blocks: Vec<Block>) -> Self {
        let mut next = self.clone();
        next.footers
            .insert(rid.to_string(), strip_section_markers(blocks));
        next.hf_dirty.footers.insert(rid.to_string());
        next
    }

    /// Phase 3 (#39) — mint a relationship id for an engine-created
    /// header/footer part. The `ngeHf` prefix is collision-proof against
    /// Word's `rIdN` ids by construction; the counter walks past any
    /// engine-minted key already present in EITHER map (parts and their
    /// ids may be shared or forked over the document's lifetime).
    pub fn fresh_hf_rid(&self) -> String {
        let mut counter = 1u32;
        loop {
            let candidate = format!("ngeHf{counter}");
            if !self.headers.contains_key(&candidate) && !self.footers.contains_key(&candidate) {
                return candidate;
            }
            counter = counter.saturating_add(1);
        }
    }

    /* ============================================================
    Issue #80 — note stories (footnotes / endnotes).
    ============================================================ */

    /// The story map for `kind`.
    pub fn note_stories(&self, kind: NoteKind) -> &std::collections::HashMap<i32, NoteStory> {
        match kind {
            NoteKind::Footnote => &self.footnote_stories,
            NoteKind::Endnote => &self.endnote_stories,
        }
    }

    fn note_stories_mut(
        &mut self,
        kind: NoteKind,
    ) -> &mut std::collections::HashMap<i32, NoteStory> {
        match kind {
            NoteKind::Footnote => &mut self.footnote_stories,
            NoteKind::Endnote => &mut self.endnote_stories,
        }
    }

    /// The story a body reference addresses (`None` for a dangling id —
    /// Word tolerates those; the paginator simply paints no note).
    pub fn note_story(&self, anchor: NoteAnchor) -> Option<&NoteStory> {
        self.note_stories(anchor.kind).get(&(anchor.id as i32))
    }

    /// The document's special story of `note_type` for `kind` (separator,
    /// continuation separator, continuation notice), if the part ships one.
    pub fn special_note(&self, kind: NoteKind, note_type: NoteType) -> Option<&NoteStory> {
        let mut found: Option<&NoteStory> = None;
        for s in self.note_stories(kind).values() {
            if s.note_type == note_type && found.is_none_or(|f| s.id < f.id) {
                found = Some(s);
            }
        }
        found
    }

    /// Every note reference in document order — the single walk the
    /// numbering, the paginator's note-body table, the a11y mirror and
    /// the HTML export all key on.
    ///
    /// Issue #278 — every story container that paints is walked, not
    /// just top-level body paragraphs: top-level blocks in sequence,
    /// table cells depth-first (nested tables included), each text-box
    /// story at its anchor's position in the host paragraph (nested
    /// boxes recursively), and the header / footer parts the sections
    /// paint. A part is walked ONCE, ahead of the body of the first
    /// section that shows it, in the order its pages show the roles
    /// (`first` under `titlePg`, then `default` / `even` by page parity;
    /// the header before the footer of the same role) — Word numbers a
    /// header note with the first page it appears on. A role the section
    /// never paints (`first` without `titlePg`, `even` without
    /// `evenAndOddHeaders`) and a part no section references contribute
    /// nothing here; the writer's keep-list is
    /// [`Self::all_note_reference_anchors`].
    pub fn note_references(&self) -> Vec<NoteReference> {
        let mut out = Vec::new();
        if self.headers.is_empty() && self.footers.is_empty() {
            for (idx, b) in self.blocks.iter().enumerate() {
                walk_block_note_refs(b, idx as u32, NoteContainer::Body, &mut out);
            }
            return out;
        }
        let sections = self.effective_sections();
        let resolved = resolve_hf_inheritance(&sections);
        let even_and_odd = self.settings.even_and_odd_headers;
        let mut seen_headers: Vec<&str> = Vec::new();
        let mut seen_footers: Vec<&str> = Vec::new();
        let mut next_section = 0usize;
        let n_blocks = self.blocks.len() as u32;
        let mut emit_sections_up_to = |block: u32, out: &mut Vec<NoteReference>| {
            while let Some(s) = sections.get(next_section) {
                if s.start_block > block {
                    break;
                }
                let (h, f) = &resolved[next_section];
                let at = s.start_block.min(n_blocks);
                for role in painted_hf_roles(s.title_pg, even_and_odd) {
                    for (refs, parts, seen, container) in [
                        (h, &self.headers, &mut seen_headers, NoteContainer::Header),
                        (f, &self.footers, &mut seen_footers, NoteContainer::Footer),
                    ] {
                        let Some(rid) = refs.resolve(role) else {
                            continue;
                        };
                        let Some((key, blocks)) = parts.get_key_value(rid) else {
                            continue;
                        };
                        if seen.contains(&key.as_str()) {
                            continue;
                        }
                        seen.push(key.as_str());
                        for b in blocks {
                            walk_block_note_refs(b, at, container, out);
                        }
                    }
                }
                next_section += 1;
            }
        };
        for (idx, b) in self.blocks.iter().enumerate() {
            emit_sections_up_to(idx as u32, &mut out);
            walk_block_note_refs(b, idx as u32, NoteContainer::Body, &mut out);
        }
        emit_sections_up_to(u32::MAX, &mut out);
        out
    }

    /// Issue #278 — every note any story of the document references,
    /// painted or not: [`Self::note_references`] plus the references in
    /// header / footer parts no painted role shows (an unreferenced part,
    /// a `first` slot without `titlePg`). The writer's "referenced notes
    /// only" filter keeps exactly these — a part that survives the save
    /// must never lose the note it points at.
    pub fn all_note_reference_anchors(&self) -> std::collections::HashSet<NoteAnchor> {
        let mut out: Vec<NoteReference> = Vec::new();
        for (idx, b) in self.blocks.iter().enumerate() {
            walk_block_note_refs(b, idx as u32, NoteContainer::Body, &mut out);
        }
        for (parts, container) in [
            (&self.headers, NoteContainer::Header),
            (&self.footers, NoteContainer::Footer),
        ] {
            for blocks in parts.values() {
                for b in blocks {
                    walk_block_note_refs(b, 0, container, &mut out);
                }
            }
        }
        out.into_iter().map(|r| r.anchor).collect()
    }

    /// Resolved `<w:footnotePr>` / `<w:endnotePr>` for `kind`: the
    /// section's own overrides over the document-level `settings.xml`
    /// props over the schema defaults (footnotes: page bottom; endnotes:
    /// document end; decimal, start 1, continuous).
    pub fn resolved_note_props(
        &self,
        kind: NoteKind,
        section: Option<&Section>,
    ) -> ResolvedNoteProps {
        let doc_level = match kind {
            NoteKind::Footnote => &self.footnote_props,
            NoteKind::Endnote => &self.endnote_props,
        };
        let merged = match section {
            Some(s) => match kind {
                NoteKind::Footnote => s.footnote_props.inherit_from(doc_level),
                NoteKind::Endnote => s.endnote_props.inherit_from(doc_level),
            },
            None => *doc_level,
        };
        let default_pos = match kind {
            NoteKind::Footnote => NotePosition::PageBottom,
            NoteKind::Endnote => NotePosition::DocEnd,
        };
        ResolvedNoteProps {
            position: merged.position.unwrap_or(default_pos),
            num_format: merged.num_format.unwrap_or_default(),
            num_start: merged.num_start.unwrap_or(1).max(1),
            num_restart: merged.num_restart.unwrap_or_default(),
        }
    }

    /// Display marker per referenced note, derived in document order:
    /// each kind runs its own sequence from `numStart`, restarting at
    /// every section boundary under `eachSect`, formatted per `numFmt`.
    /// A custom-marked reference contributes no number and maps to an
    /// empty string (its mark is the author's following run). `eachPage`
    /// numbers as `continuous` HERE (see [`NoteNumRestart`]); the layout
    /// relabels those footnotes per page after pagination (issue #129,
    /// driven by [`Self::each_page_note_numbering`]).
    pub fn note_markers(&self) -> std::collections::HashMap<NoteAnchor, String> {
        let sections = self.effective_sections();
        let refs = self.note_references();
        let mut out = std::collections::HashMap::with_capacity(refs.len());
        for kind in [NoteKind::Footnote, NoteKind::Endnote] {
            let mut counter: Option<u32> = None;
            let mut section_idx: Option<usize> = None;
            for r in refs.iter().filter(|r| r.anchor.kind == kind) {
                let si = sections
                    .iter()
                    .position(|s| r.top_block >= s.start_block && r.top_block < s.end_block)
                    .unwrap_or(sections.len().saturating_sub(1));
                let props = self.resolved_note_props(kind, sections.get(si));
                let restart_here = counter.is_none()
                    || (props.num_restart == NoteNumRestart::EachSect && section_idx != Some(si));
                if restart_here {
                    counter = Some(props.num_start);
                }
                section_idx = Some(si);
                if r.custom_mark {
                    out.entry(r.anchor).or_insert_with(String::new);
                    continue;
                }
                let n = counter.unwrap_or(1);
                out.entry(r.anchor)
                    .or_insert_with(|| props.num_format.render(n));
                counter = Some(n.saturating_add(1));
            }
        }
        out
    }

    /// Issue #129 — the restart rule layout needs for per-page footnote
    /// numbering: every numbered (not custom-marked) footnote reference
    /// whose section resolves `<w:numRestart w:val="eachPage"/>`, with
    /// its section's `numStart` and `numFmt`. Empty — the default, and
    /// every continuous / per-section document — means layout runs no
    /// relabelling pass at all.
    pub fn each_page_note_numbering(
        &self,
    ) -> std::collections::HashMap<NoteAnchor, (u32, PageNumFormat)> {
        let mut out = std::collections::HashMap::new();
        if self.footnote_stories.is_empty() {
            return out;
        }
        let sections = self.effective_sections();
        let each_page: Vec<Option<(u32, PageNumFormat)>> = sections
            .iter()
            .map(|s| {
                let props = self.resolved_note_props(NoteKind::Footnote, Some(s));
                (props.num_restart == NoteNumRestart::EachPage)
                    .then_some((props.num_start, props.num_format))
            })
            .collect();
        if each_page.iter().all(Option::is_none) {
            return out;
        }
        for r in self.note_references() {
            if r.anchor.kind != NoteKind::Footnote || r.custom_mark {
                continue;
            }
            let si = sections
                .iter()
                .position(|s| r.top_block >= s.start_block && r.top_block < s.end_block)
                .unwrap_or(sections.len().saturating_sub(1));
            if let Some(Some(rule)) = each_page.get(si) {
                out.entry(r.anchor).or_insert(*rule);
            }
        }
        out
    }

    /// Replace (or create) the body of note `id` of `kind` and mark the
    /// entry + its part dirty for the writer — the only mutation path into
    /// the story maps, so `notes_dirty` can never desync from the content.
    /// Section markers are stripped: a note body is not the body.
    pub fn with_updated_note_story(&self, kind: NoteKind, id: i32, body: Vec<Block>) -> Self {
        let mut next = self.clone();
        let body = strip_section_markers(body);
        let map = next.note_stories_mut(kind);
        match map.get_mut(&id) {
            Some(story) => {
                story.body = body;
                story.dirty = true;
                story.source_xml = None;
            }
            None => {
                map.insert(
                    id,
                    NoteStory {
                        id,
                        kind,
                        note_type: NoteType::Normal,
                        body,
                        source_xml: None,
                        dirty: true,
                    },
                );
            }
        }
        match kind {
            NoteKind::Footnote => next.notes_dirty.footnotes = true,
            NoteKind::Endnote => next.notes_dirty.endnotes = true,
        }
        next
    }

    /// Smallest positive id not used by any story of `kind` — Word's own
    /// ids are small positive integers, so a fresh note simply extends
    /// the sequence.
    pub fn fresh_note_id(&self, kind: NoteKind) -> u32 {
        let max = self
            .note_stories(kind)
            .keys()
            .copied()
            .filter(|id| *id > 0)
            .max()
            .unwrap_or(0);
        (max as u32).saturating_add(1)
    }

    /// Author a new note of `kind` referenced at `pos`: a reference anchor
    /// (U+FFFC + [`InlineKind::FootnoteRef`] / [`InlineKind::EndnoteRef`])
    /// is spliced into the paragraph at `pos` and a fresh story — one
    /// paragraph opening with the self-mark and a space, Word's empty-note
    /// shape — is created. Returns the new tree and the minted id. `pos`
    /// must address a paragraph (the caller gates table cells).
    pub fn insert_note_at(&self, pos: LogicalPos, kind: NoteKind) -> (Self, u32) {
        let id = self.fresh_note_id(kind);
        let mut blocks = self.blocks.clone();
        let target = if self.paragraph_at_path(&pos.path).is_some() {
            pos.path.clone()
        } else {
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let reference = match kind {
            NoteKind::Footnote => InlineKind::FootnoteRef {
                id,
                custom_mark_follows: false,
            },
            NoteKind::Endnote => InlineKind::EndnoteRef {
                id,
                custom_mark_follows: false,
            },
        };
        let mut edit = None;
        let _ = mutate_paragraph_in_top(&mut blocks, &target, |para| {
            edit = Some(splice_inline_object(para, pos.offset, reference));
        });
        let mut next = self.clone();
        next.blocks = blocks;
        if let Some(e) = edit {
            next.remap_text_edit_record(&target, e);
        }
        let mut body_para = Paragraph {
            text: format!("{}{}", '\u{FFFC}', ' '),
            dirty: true,
            ..Default::default()
        };
        body_para.inline_objects.push(InlineObject {
            at: 0,
            kind: InlineKind::NoteSelfRef { kind },
            anchor: None,
            source_xml: None,
        });
        /* Word styles note bodies `FootnoteText` / `EndnoteText`; adopt
        the style when the document defines it so the body picks up the
        smaller size the template intends. */
        let style_id = match kind {
            NoteKind::Footnote => "FootnoteText",
            NoteKind::Endnote => "EndnoteText",
        };
        if self.styles.contains_key(style_id) {
            body_para.style_id = Some(style_id.to_string());
        }
        let next = next.with_updated_note_story(kind, id as i32, vec![Block::Paragraph(body_para)]);
        (next, id)
    }

    /// Resolved section coverage, DERIVED by walking the top-level block
    /// list: every paragraph carrying a [`Paragraph::section_end`] marker
    /// closes a section at its own index; [`Self::body_section`] closes
    /// the final one. Ranges are therefore correct by construction under
    /// any block insertion/deletion — nothing to re-index (Phase 3, #40).
    ///
    /// Always returns at least one section. A marker on the very last
    /// block suppresses the would-be-empty trailing body section (an
    /// edit can strand a marker there; the writer + paginator both want
    /// non-empty ranges).
    ///
    /// O(top-level blocks) per call — hot callers (the per-keystroke
    /// `SelectionChanged` builder in engine-wasm) go through a
    /// revision-keyed memo rather than calling this directly.
    pub fn effective_sections(&self) -> Vec<Section> {
        let len = self.blocks.len() as u32;
        let mut out: Vec<Section> = Vec::new();
        let mut start = 0u32;
        for (i, b) in self.blocks.iter().enumerate() {
            if let Block::Paragraph(p) = b
                && let Some(props) = &p.section_end
            {
                out.push(props.as_ref().clone().into_section(start, i as u32 + 1));
                start = i as u32 + 1;
            }
        }
        if start < len || out.is_empty() {
            out.push(self.body_section.clone().into_section(start, len));
        }
        out
    }

    /* ============================================================
    Phase 5 PR 4 — `BlockPath` walk helpers
    The paragraph-flat shim (`nth_paragraph` / `paragraph_count` /
    `paragraph_text`) is kept as a compatibility surface for tests
    and round-trip callers; the canonical position type is now
    `LogicalPos { path, offset }`, addressed through the helpers
    below.
    ============================================================ */

    /// Resolve a `BlockPath` to its terminal `Block`. The path's first
    /// step is a top-level `Block(n)`; subsequent `Cell` / `Block`
    /// pairs descend into table cells.
    pub fn block_at(&self, path: &BlockPath) -> Option<&Block> {
        let first = path.steps.first()?;
        let PathStep::Block(n) = first else {
            return None;
        };
        let block = self.blocks.get(*n as usize)?;
        block_at_descend(block, &path.steps[1..])
    }

    /// Resolve a `BlockPath` to its terminal `Paragraph`; `None` when
    /// the path is empty or terminates at a `Table`.
    pub fn paragraph_at_path(&self, path: &BlockPath) -> Option<&Paragraph> {
        self.block_at(path)?.as_paragraph()
    }

    /// Normalize a wire position's offset against the paragraph its path
    /// addresses (issue #115, crate offset policy). The path is left
    /// untouched; a path that does not resolve to a paragraph returns the
    /// position unchanged — path fallback is the caller's decision.
    pub fn snap_pos(&self, pos: LogicalPos) -> LogicalPos {
        match self.paragraph_at_path(&pos.path) {
            Some(p) => LogicalPos {
                offset: p.snap_offset(pos.offset),
                path: pos.path,
            },
            None => pos,
        }
    }

    /// Resolve a `BlockPath` to a borrowed reference to its terminal
    /// `Table`; `None` when the path does not terminate at one.
    pub fn table_at_path(&self, path: &BlockPath) -> Option<&Table> {
        self.block_at(path)?.as_table()
    }

    /// Issue #116 — resolve a wire `table_path` to the top-level table
    /// every table mutation operates on. Typed errors, never a panic:
    /// a path that addresses a paragraph, nothing, or a nested table.
    pub fn resolve_table(&self, path: &BlockPath) -> Result<&Table, TableError> {
        match path.steps.as_slice() {
            [PathStep::Block(n)] => match self.blocks.get(*n as usize) {
                Some(Block::Table(t)) => Ok(t),
                _ => Err(TableError::NotATable { path: path.clone() }),
            },
            [PathStep::Block(_), _, ..] if self.table_at_path(path).is_some() => {
                Err(TableError::NestedUnsupported { path: path.clone() })
            }
            _ => Err(TableError::NotATable { path: path.clone() }),
        }
    }

    /// Issue #116 — the single validated boundary for every table
    /// command: `table_path` must resolve ([`Self::resolve_table`]),
    /// `row` (when given) must index an existing row, and `col` (when
    /// given) must index an existing cell of that row — or, for a
    /// column-only command, an existing logical column. Returns the
    /// table so callers can run further shape checks without a second
    /// lookup.
    pub fn resolve_table_target(
        &self,
        path: &BlockPath,
        row: Option<u32>,
        col: Option<u32>,
    ) -> Result<&Table, TableError> {
        let t = self.resolve_table(path)?;
        match (row, col) {
            (Some(r), c) => {
                let Some(row_box) = t.rows.get(r as usize) else {
                    return Err(TableError::RowOutOfRange {
                        row: r,
                        rows: t.rows.len(),
                    });
                };
                if let Some(c) = c
                    && c as usize >= row_box.cells.len()
                {
                    return Err(TableError::ColOutOfRange {
                        col: c,
                        cols: row_box.cells.len(),
                    });
                }
            }
            (None, Some(c)) => {
                let cols = t.column_count();
                if c as usize >= cols {
                    return Err(TableError::ColOutOfRange { col: c, cols });
                }
            }
            (None, None) => {}
        }
        Ok(t)
    }

    /// Sprint 10 — walk `path` and return a borrowed reference to the
    /// **innermost** `CellProperties` the caret is sitting inside, or
    /// `None` when the path never enters a table cell. Backs the
    /// `Event::SelectionChanged.cell_properties` field that drives
    /// `CellPropertiesDialog` prefill.
    pub fn innermost_cell_props_at(&self, path: &BlockPath) -> Option<&CellProperties> {
        let mut last: Option<&CellProperties> = None;
        let mut i = 0usize;
        let first = path.steps.first()?;
        let PathStep::Block(n) = first else {
            return None;
        };
        let mut current: &Block = self.blocks.get(*n as usize)?;
        i += 1;
        while i < path.steps.len() {
            let Block::Table(t) = current else {
                return last;
            };
            let PathStep::Cell { row, col } = path.steps[i] else {
                return last;
            };
            let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
            last = Some(&cell.props);
            i += 1;
            let Some(PathStep::Block(b_idx)) = path.steps.get(i) else {
                return last;
            };
            current = cell.blocks.get(*b_idx as usize)?;
            i += 1;
        }
        last
    }

    /// Sprint 10 — locate the `Section` covering top-level block
    /// `block_idx`. Phase 3 (#40): derived from the paragraph markers —
    /// the covering section is closed by the FIRST marker at index
    /// `>= block_idx`, or by [`Self::body_section`] when no such marker
    /// exists. The caller can read the returned section's geometry
    /// directly into the `SelectionChanged` event's `section_geometry`
    /// field.
    pub fn section_for_block(&self, block_idx: u32) -> Section {
        let mut start = 0u32;
        for (i, b) in self.blocks.iter().enumerate() {
            let i = i as u32;
            if let Block::Paragraph(p) = b
                && let Some(props) = &p.section_end
            {
                if block_idx <= i {
                    return props.as_ref().clone().into_section(start, i + 1);
                }
                start = i + 1;
            }
        }
        self.body_section
            .clone()
            .into_section(start, self.blocks.len() as u32)
    }

    /// Path to the Nth top-level paragraph (skipping tables). Compat
    /// shim for callers that still index paragraph-flat — RFC §4
    /// `BlockPath::root_paragraph(n)`.
    pub fn path_to_top_paragraph(&self, n: u32) -> Option<BlockPath> {
        let mut seen = 0u32;
        for (i, b) in self.blocks.iter().enumerate() {
            if matches!(b, Block::Paragraph(_)) {
                if seen == n {
                    return Some(BlockPath::top(i as u32));
                }
                seen += 1;
            }
        }
        None
    }

    /// Path to the document's last top-level paragraph (skipping
    /// tables). `None` for empty / tables-only documents.
    pub fn path_to_last_top_paragraph(&self) -> Option<BlockPath> {
        let mut last: Option<u32> = None;
        for (i, b) in self.blocks.iter().enumerate() {
            if matches!(b, Block::Paragraph(_)) {
                last = Some(i as u32);
            }
        }
        last.map(BlockPath::top)
    }

    /// Issue #72 (design review B6) — block-tree-aware FIRST caret
    /// home: the first top-level paragraph's path, or — when the
    /// document/story is table-led — a descent into the first table's
    /// first cell's first paragraph. A table-only header part must
    /// never park the caret on a bare `Block(table)` path (no text
    /// consumer can resolve it).
    pub fn path_to_first_paragraph_deep(&self) -> Option<BlockPath> {
        for (i, b) in self.blocks.iter().enumerate() {
            match b {
                Block::Paragraph(_) => return Some(BlockPath::top(i as u32)),
                Block::Table(t) => {
                    if let Some(path) = first_cell_paragraph_path(t, BlockPath::top(i as u32)) {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /// Issue #72 (design review B6) — block-tree-aware LAST caret
    /// home; [`Self::path_to_first_paragraph_deep`]'s tail twin, used
    /// by clamp fallbacks after structural edits.
    pub fn path_to_last_paragraph_deep(&self) -> Option<BlockPath> {
        for (i, b) in self.blocks.iter().enumerate().rev() {
            match b {
                Block::Paragraph(_) => return Some(BlockPath::top(i as u32)),
                Block::Table(t) => {
                    if let Some(path) = last_cell_paragraph_path(t, BlockPath::top(i as u32)) {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /* ============================================================
    Phase 5 PR 1 — paragraph-flat shim
    Treats `Block::Table` as inert. Kept as a compatibility helper;
    every interactive path now uses the `BlockPath` helpers above.
    ============================================================ */

    /// Number of `Block::Paragraph`s in the doc, skipping tables.
    pub fn paragraph_count(&self) -> u32 {
        self.blocks
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(_)))
            .count() as u32
    }

    /// Total block count (paragraphs + tables).
    pub fn block_count(&self) -> u32 {
        self.blocks.len() as u32
    }

    /// Issue #44 — count of inline images across the whole document
    /// (body + table cells). Lets the shell skip its `GetImageRects`
    /// refresh for image-free documents. Cheap O(paragraphs) walk.
    ///
    /// Issue #206 — pictures inside text-box stories (and boxes nested in
    /// them) count too: the image-geometry query lists them, so a document
    /// whose only pictures live in a box must still trigger the refresh.
    /// The walk is bounded by the tree's own (finite) story nesting.
    pub fn count_inline_images(&self) -> u32 {
        fn count(p: &Paragraph, n: &mut u32) {
            for io in &p.inline_objects {
                match &io.kind {
                    InlineKind::Image { .. } => *n = n.saturating_add(1),
                    InlineKind::TextBox { story, .. } => {
                        for b in &story.body {
                            walk_block(b, &mut |sp: &Paragraph| count(sp, n));
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut n = 0u32;
        walk_paragraphs(&self.blocks, &mut |p| count(p, &mut n));
        n
    }

    /// Sprint 8 (UI Edition) — total character count across every
    /// paragraph in the document, including paragraphs nested in
    /// table cells. Counts Unicode scalars (`char`s), not bytes —
    /// matches Word's "Characters (no spaces)" minus the no-space
    /// filter. Cheap O(n) walk.
    ///
    /// Issue #73 — includes REFERENCED header/footer stories (typing
    /// in a band raises the StatusBar count; shared parts count once).
    pub fn character_count(&self) -> u32 {
        let mut n = 0u32;
        let mut count = |p: &Paragraph| {
            n = n.saturating_add(p.text.chars().count() as u32);
        };
        walk_paragraphs(&self.blocks, &mut count);
        self.for_each_referenced_story(&mut |_, _, blocks| {
            for b in blocks {
                walk_block(b, &mut count);
            }
        });
        n
    }

    /// Sprint 11 (#17) — total word count via UAX-#29 word
    /// segmentation. Replaces the Sprint 8 whitespace-split
    /// fallback so CJK / Thai / Khmer (scripts without inter-word
    /// whitespace) report a meaningful count.
    ///
    /// `icu_segmenter::WordSegmenter::new_auto` shares its data
    /// tables with `text-pipeline`'s `LineSegmenter::new_auto`, so
    /// the wasm artifact does not grow beyond the icu data already
    /// linked for line breaking. We filter `WordType::Word` so
    /// punctuation and inter-word whitespace runs don't count as
    /// words.
    ///
    /// Issue #73 — includes REFERENCED header/footer stories, like
    /// [`Self::character_count`]. `count_inline_images` deliberately
    /// stays body-only (body + text-box stories, issue #206): the shell's
    /// image-rect/selection pipeline is body-scoped, and the count gates
    /// exactly that pipeline.
    pub fn word_count(&self) -> u32 {
        let mut n = 0u32;
        let mut count = |p: &Paragraph| {
            n = n.saturating_add(count_uax_words(&p.text) as u32);
        };
        walk_paragraphs(&self.blocks, &mut count);
        self.for_each_referenced_story(&mut |_, _, blocks| {
            for b in blocks {
                walk_block(b, &mut count);
            }
        });
        n
    }

    /// Issue #73 — every header/footer part REFERENCED by some
    /// section's own ref slots, visited once per rid (shared parts
    /// dedup), headers then footers, rid-sorted for determinism.
    /// Orphaned parts (relinked-away, imported-but-unreferenced) are
    /// NOT visited — they don't render, so they don't count.
    pub fn for_each_referenced_story<'a>(&'a self, f: &mut impl FnMut(bool, &str, &'a [Block])) {
        let sections = self.effective_sections();
        let mut header_rids: Vec<String> = Vec::new();
        let mut footer_rids: Vec<String> = Vec::new();
        for s in &sections {
            for rid in [
                &s.header_refs.default,
                &s.header_refs.first,
                &s.header_refs.even,
            ]
            .into_iter()
            .flatten()
            {
                if !header_rids.contains(rid) {
                    header_rids.push(rid.clone());
                }
            }
            for rid in [
                &s.footer_refs.default,
                &s.footer_refs.first,
                &s.footer_refs.even,
            ]
            .into_iter()
            .flatten()
            {
                if !footer_rids.contains(rid) {
                    footer_rids.push(rid.clone());
                }
            }
        }
        header_rids.sort_unstable();
        footer_rids.sort_unstable();
        for rid in &header_rids {
            if let Some(blocks) = self.headers.get(rid) {
                f(true, rid, blocks);
            }
        }
        for rid in &footer_rids {
            if let Some(blocks) = self.footers.get(rid) {
                f(false, rid, blocks);
            }
        }
    }

    /// The Nth `Block::Paragraph`, skipping tables. Phase 5 PR 1 shim
    /// that keeps Phase 1-4 callers working unchanged. Phase 5 PR 3
    /// widens callers to `BlockPath`.
    pub fn nth_paragraph(&self, n: u32) -> Option<&Paragraph> {
        self.blocks
            .iter()
            .filter_map(Block::as_paragraph)
            .nth(n as usize)
    }

    pub fn paragraph_text(&self, idx: u32) -> Option<&str> {
        self.nth_paragraph(idx).map(|p| p.text.as_str())
    }

    /// Sprint 9 — flatten the whole document to plain text.
    ///
    /// Paragraphs join with `\n`. Tables emit one tab-separated row per
    /// `TableRow` (cells joined with `\t`, in their visual order); the
    /// table itself sits on its own line, with a blank-line separator
    /// before and after. Inline objects (images, footnote refs) render
    /// as the placeholder marker `[image]` / `[footnote N]` so the
    /// caller never silently drops them.
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for block in self.blocks.iter() {
            match block {
                Block::Paragraph(p) => {
                    push_paragraph_plain(p, &mut out);
                    out.push('\n');
                }
                Block::Table(t) => push_table_plain(t, &mut out),
            }
        }
        /* Drop the trailing newline so single-paragraph docs are not
        terminated by an empty line. */
        if out.ends_with('\n') {
            out.pop();
        }
        out
    }

    pub fn end_of_document(&self) -> LogicalPos {
        let Some(path) = self.path_to_last_top_paragraph() else {
            return LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            };
        };
        let offset = self
            .paragraph_at_path(&path)
            .map(|p| p.text.len() as u32)
            .unwrap_or(0);
        LogicalPos { path, offset }
    }

    /// Insert `text` at `at`. Out-of-range positions are clamped to end of
    /// document. Returns the new tree (the old one is structurally shared via
    /// `im::Vector`).
    /// Sprint 14 (#14) — track-changes-aware text insertion.
    ///
    /// Boundary math:
    /// - **Inside an existing Insert by same author** → existing
    ///   Insert grows via the offset shift; NO new revision added
    ///   (prevents per-keystroke fragmentation).
    /// - **Inside an existing Delete** → split the Delete around the
    ///   insertion point and stamp a fresh Insert in the gap (typing
    ///   inside a `<w:del>` logically replaces deleted text).
    /// - **Adjacent to an Insert by same author** (cursor at its
    ///   right edge) → extend the existing Insert end (merge
    ///   keystrokes).
    /// - Otherwise → add a fresh `Insert` revision over `[at,
    ///   at+len)`.
    pub fn tracked_insert_text(
        &self,
        at: LogicalPos,
        text: &str,
        author: String,
        date: String,
    ) -> Self {
        if text.is_empty() {
            return self.clone();
        }
        /* Empty doc: fall through to plain insert_text + stamp the
        Insert revision on paragraph 0. */
        /* Issue #115 — resolve the landing offset against the PRE-insert
        text: `insert_text` snaps a mid-scalar offset down to a char
        boundary, and the same snap against the post-insert text could
        land inside the freshly inserted run instead. */
        let off_input = self
            .paragraph_at_path(&at.path)
            .map_or(at.offset, |p| p.snap_offset(at.offset));
        let mut doc = self.insert_text(at.clone(), text);
        let len = text.len() as u32;
        /* Resolve the path the insert actually landed on (insert_text
        clamps to the document end when the original path is stale). */
        let target_path = if doc.paragraph_at_path(&at.path).is_some() {
            at.path
        } else {
            doc.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let mut blocks = doc.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, &target_path, |para| {
            /* `insert_text` clamps `at.offset` to `para.text.len()`
            BEFORE inserting; mirror that clamp so revision math
            uses the same byte position the insertion actually
            landed at. */
            let pre_text_len = (para.text.len() as u32).saturating_sub(len);
            let off = off_input.min(pre_text_len);

            /* Detect boundary state on the PRE-insert geometry (the
            same paragraph of `self`). `insert_text` already shifted the
            revisions by `len` (issue #247): revisions starting at or
            after `off` slid right; revisions containing `off` grew. */
            let pre_revisions = self
                .paragraph_at_path(&target_path)
                .map_or(&[][..], |p| p.revisions.as_slice());
            let inside_insert_same_author = pre_revisions.iter().any(|r| {
                r.kind == RevisionKind::Insert && r.start < off && off < r.end && r.author == author
            });
            let inside_delete = pre_revisions
                .iter()
                .any(|r| r.kind == RevisionKind::Delete && r.start <= off && off < r.end);

            /* If we landed inside a Delete, the shift above grew the
            Delete to span both halves. Split it back into the two
            halves around the new Insert. */
            if inside_delete {
                let mut split: Vec<Revision> = Vec::with_capacity(para.revisions.len() + 1);
                for r in para.revisions.drain(..) {
                    let was_split =
                        r.kind == RevisionKind::Delete && r.start <= off && off + len < r.end;
                    if !was_split {
                        split.push(r);
                        continue;
                    }
                    /* Left half [r.start, off) keeps Delete kind. */
                    if off > r.start {
                        split.push(Revision {
                            start: r.start,
                            end: off,
                            kind: RevisionKind::Delete,
                            author: r.author.clone(),
                            date: r.date.clone(),
                            id: None,
                            prev_attrs: None,
                            move_name: None,
                        });
                    }
                    /* Right half [off + len, r.end) — note r.end was
                    already shifted by +len above, so it correctly
                    covers the post-insert remainder. */
                    if r.end > off + len {
                        split.push(Revision {
                            start: off + len,
                            end: r.end,
                            kind: RevisionKind::Delete,
                            author: r.author,
                            date: r.date,
                            id: None,
                            prev_attrs: None,
                            move_name: None,
                        });
                    }
                }
                para.revisions = split;
            }

            /* Add (or merge-grow) the new Insert revision unless we're
            already inside an Insert by the same author (the offset-
            shift already extended its end). */
            if !inside_insert_same_author {
                let new_end = off + len;
                let merged = para
                    .revisions
                    .iter_mut()
                    .find(|r| r.kind == RevisionKind::Insert && r.end == off && r.author == author);
                if let Some(left) = merged {
                    left.end = new_end;
                    left.date = date.clone();
                } else {
                    para.revisions.push(Revision {
                        start: off,
                        end: new_end,
                        kind: RevisionKind::Insert,
                        author: author.clone(),
                        date: date.clone(),
                        id: None,
                        prev_attrs: None,
                        move_name: None,
                    });
                }
            }
            para.dirty = true;
        });
        doc.blocks = blocks;
        doc
    }

    /// Sprint 14 (#14) — track-changes-aware delete.
    ///
    /// Boundary math:
    /// - **Range entirely inside a same-author Insert** → shrink the
    ///   Insert AND remove the text. Inserts never originated in the
    ///   source; deleting one's own pending insertion is a no-revision
    ///   undo of that pending edit.
    /// - **Range outside any Insert** → preserve the text, mark a
    ///   fresh `Delete` revision covering the range. Adjacent
    ///   same-author Delete gets merged.
    /// - Mixed cases (range straddles Insert + non-Insert) fall back
    ///   to the marker-only behaviour for v1 (text preserved, Delete
    ///   stamped over the whole range; the overlapped Insert remains).
    pub fn tracked_delete_range(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        author: String,
        date: String,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        if start == end || !same_parent(&start.path, &end.path) {
            return self.clone();
        }
        let Some(s_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(e_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        if s_idx != e_idx {
            /* Cross-paragraph tracked-delete falls back to the
            mark-only flow per-paragraph; v1 limitation. */
            return self.clone();
        }
        let target_path = start.path.clone();
        /* Issue #115 — snap both ends to char boundaries before any
        revision math or `replace_range` sees them. */
        let (s_off, e_off) = match self.paragraph_at_path(&target_path) {
            Some(p) => (p.snap_offset(start.offset), p.snap_offset(end.offset)),
            None => (start.offset, end.offset),
        };
        if s_off >= e_off {
            return self.clone();
        }
        let mut blocks = self.blocks.clone();
        let mut removed_edit = None;
        let _ = mutate_paragraph_in_top(&mut blocks, &target_path, |para| {
            /* Range entirely inside a same-author Insert? If so, undo
            the Insert (remove text + shrink the Insert overlay). */
            let owning_insert = para.revisions.iter().any(|r| {
                r.kind == RevisionKind::Insert
                    && r.author == author
                    && r.start <= s_off
                    && e_off <= r.end
            });
            if owning_insert {
                let s = s_off.min(para.text.len() as u32);
                let e = e_off.min(para.text.len() as u32);
                let removed_len = e.saturating_sub(s);
                if e > s {
                    /* Issues #250 / #252 — one splice drives the source
                    markup and (below) the comment anchors. Issue #265 —
                    the SAME (at, removed) window then drives every other
                    byte-offset table through `shift_paragraph_offsets_after`
                    (spans, hyperlinks, fields, inline objects, and the
                    revisions themselves): the owning Insert satisfies
                    `start <= s && e <= end`, so the shared gap-shift rule
                    shrinks its `end` by `removed_len` and leaves `start`
                    alone — exactly the old bespoke shrink — and drops it
                    outright if that shrinks it to empty, via the same
                    `retain` every other overlay gets. This is the
                    `apply_revision_decision` (accept/reject) bookkeeping,
                    reused so a field, hyperlink or picture inside a
                    reviewer's own removed insertion leaves no stale
                    offsets. */
                    let edit = para.splice_text(s, removed_len, "");
                    shift_paragraph_offsets_after(para, edit.at, edit.removed);
                    removed_edit = Some(edit);
                }
                para.dirty = true;
                return;
            }
            /* Marker-only delete: stamp a fresh Delete over the range
            (text preserved). Merge with adjacent same-author Delete. */
            let new_end = e_off;
            let merged_left = para
                .revisions
                .iter_mut()
                .find(|r| r.kind == RevisionKind::Delete && r.end == s_off && r.author == author);
            if let Some(left) = merged_left {
                left.end = new_end;
                left.date = date.clone();
            } else {
                para.revisions.push(Revision {
                    start: s_off,
                    end: new_end,
                    kind: RevisionKind::Delete,
                    author: author.clone(),
                    date: date.clone(),
                    id: None,
                    prev_attrs: None,
                    move_name: None,
                });
            }
            para.dirty = true;
        });
        let mut out = Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        if let Some(e) = removed_edit {
            out.remap_text_edit_record(&target_path, e);
        }
        out
    }

    /// Sprint 14 (#14) — track-changes-aware format-change stamp.
    /// Records a `FormatChange` revision over the range carrying the
    /// pre-mutation `SpanStyle` snapshot (so reject can restore it).
    /// Caller still applies the formatting via the existing path —
    /// this helper only adds the overlay.
    pub fn tracked_format_change(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        prev_attrs: SpanStyle,
        author: String,
        date: String,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        if start == end || !same_parent(&start.path, &end.path) {
            return self.clone();
        }
        let Some(s_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(e_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        let parent = start.path.parent();
        /* Issue #115 — revision ranges are stored offsets the layout
        slices by; snap them like every other stored offset. */
        let s_off = self
            .paragraph_at_path(&start.path)
            .map_or(start.offset, |p| p.snap_offset(start.offset));
        let e_off = self
            .paragraph_at_path(&end.path)
            .map_or(end.offset, |p| p.snap_offset(end.offset));
        let mut blocks = self.blocks.clone();
        let single_paragraph = s_idx == e_idx;
        for idx in s_idx..=e_idx {
            let child_path = parent.clone().push(PathStep::Block(idx));
            let author_local = author.clone();
            let date_local = date.clone();
            let prev_local = prev_attrs.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &child_path, |para| {
                let r_start = if single_paragraph { s_off } else { 0 };
                let r_end = if single_paragraph {
                    e_off
                } else {
                    para.text.len() as u32
                };
                if r_end <= r_start {
                    return;
                }
                para.revisions.push(Revision {
                    start: r_start,
                    end: r_end,
                    kind: RevisionKind::FormatChange,
                    author: author_local,
                    date: date_local,
                    id: None,
                    prev_attrs: Some(prev_local),
                    move_name: None,
                });
                para.dirty = true;
            });
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    pub fn insert_text(&self, at: LogicalPos, text: &str) -> Self {
        if text.is_empty() {
            return self.clone();
        }
        let mut blocks = self.blocks.clone();
        /* Design review B6 — `paragraph_count()` is a TOP-LEVEL shim:
        a table-only tree (letterhead header story) reports 0 but has
        real cell paragraphs; the append fast-path must not fire for
        it or typing lands in a phantom trailing paragraph. */
        let count = self.paragraph_count();
        if count == 0 && self.path_to_first_paragraph_deep().is_none() {
            blocks.push_back(Block::Paragraph(Paragraph {
                text: text.to_owned(),
                spans: Vec::new(),
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                resolved_list_indent: None,
                dirty: true,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
                section_end: None,
                bookmarks: Vec::new(),
                body_xml: None,
                source_markup: None,
                mark_revision: None,
            }));
            return Self {
                blocks,
                body_section: self.body_section.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnote_stories: self.footnote_stories.clone(),
                endnote_stories: self.endnote_stories.clone(),
                footnote_props: self.footnote_props,
                endnote_props: self.endnote_props,
                notes_dirty: self.notes_dirty.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
                hf_dirty: self.hf_dirty.clone(),
                settings_dirty: self.settings_dirty,
                document_root_attrs: self.document_root_attrs.clone(),
                part_root_attrs: self.part_root_attrs.clone(),
                document_envelope: self.document_envelope.clone(),
                source_package: self.source_package.clone(),
            };
        }
        let target = if self.paragraph_at_path(&at.path).is_some() {
            at.path.clone()
        } else {
            /* Path no longer addresses a paragraph (clamped after a
            structural edit). Fall back to the document end — deep:
            a table-only tree's last paragraph lives inside a cell. */
            self.path_to_last_paragraph_deep()
                .unwrap_or(BlockPath::top(0))
        };
        let off = at.offset;
        let mut edit = None;
        let mutated = mutate_paragraph_in_top(&mut blocks, &target, |para| {
            /* Issue #276 — pick the span to continue on the PRE-edit
            paragraph, at the offset `splice_text` snaps to. */
            let grow = para.inheriting_span(para.snap_offset(off.min(para.text.len() as u32)));
            /* Issues #199 / #106 / #252 — ONE splice drives the source
            markup here and the comment anchors below. */
            let e = para.splice_text(off, 0, text);
            edit = Some(e);
            /* Issue #276 — the inserted text continues the formatting of
            the character BEFORE the insertion point (at the paragraph
            start: the character after it), as in Word: the span holding
            that character grows over the insertion, every span at/after
            the point slides right. This is exactly the source run
            `SourceMarkup::note_insert` extends, so a save continues the
            source `<w:r>` (rsids included, #199) instead of minting a
            fresh unformatted one. Sticky (pending) formatting is layered
            on top by the interactive caller. */
            let off = e.at;
            let len = text.len() as u32;
            let donor = grow.map(|i| para.spans[i].style.clone());
            for (i, s) in para.spans.iter_mut().enumerate() {
                if Some(i) == grow {
                    s.end += len;
                } else if s.start >= off {
                    s.start += len;
                    s.end += len;
                }
            }
            /* ... minus revision records: a donor run's tracked
            formatting change (`<w:rPrChange>`, grab bag) describes an
            edit of THAT text, not of the new text — and re-emitting it
            would duplicate its `w:id`. */
            if let Some(donor) = donor {
                let typed = donor.for_typing();
                if typed != donor {
                    *para = para.set_style(off, off + len, typed);
                }
            }
            /* Issue #43 — FIELD anchors shift too (they render live now;
            a stale range would repaint the wrong bytes). Typing at a
            field's start boundary stays outside (shift); strictly inside
            grows the field (the cached result was hand-edited — the next
            resolution overwrites it wholesale). */
            for f in &mut para.fields {
                if f.start >= off {
                    f.start += len;
                    f.end += len;
                } else if f.end > off {
                    f.end += len;
                }
            }
            /* Issue #242 — hyperlinks follow their text the same way
            (typing at either boundary stays outside the link); a stale
            range re-anchored every link of an edited paragraph onto the
            wrong bytes on save. */
            for h in &mut para.hyperlinks {
                if h.start >= off {
                    h.start += len;
                    h.end += len;
                } else if h.end > off {
                    h.end += len;
                }
            }
            /* Issue #247 — tracked-change overlays (a move, an ins / del
            read from the file) follow their text like a span: typing at
            a revision's start stays outside it (shift), strictly inside
            grows it, at its end stays outside. `tracked_insert_text`
            builds on this shift (a Delete it lands in grows and is split
            back around the new insertion there). */
            for r in &mut para.revisions {
                if r.start >= off {
                    r.start += len;
                }
                if r.end > off {
                    r.end += len;
                }
            }
            /* Issue #69 / #80 — inline-object anchors (images, note
            references) slide right with their sentinel byte. Typing
            exactly AT the anchor inserts before it (the sentinel keeps its
            object; the typed text lands to its left), so `>=`. */
            for io in &mut para.inline_objects {
                if io.at >= off {
                    io.at += len;
                }
            }
        });
        if mutated.is_none() {
            return self.clone();
        }
        let mut out = Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        if let Some(e) = edit {
            out.remap_text_edit_record(&target, e);
        }
        out
    }

    /// Apply a style `patch` over the logical range `[start, end)`. Splits and
    /// merges spans on every covered paragraph; unaffected paragraphs are
    /// structurally shared. PR 4: full range support only when `start` and
    /// `end` share a parent container (`same_parent`); cross-container
    /// ranges clamp to the `start` endpoint's paragraph.
    pub fn apply_style(&self, start: LogicalPos, end: LogicalPos, patch: SpanStyle) -> Self {
        let (start, end) = order_positions(start, end);
        if !same_parent(&start.path, &end.path) {
            return self.apply_style_single(start, end, patch);
        }
        let Some(start_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(end_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return self.clone();
        };
        let mut blocks = self.blocks.clone();
        let parent = start.path.parent();
        for idx in start_idx..=end_idx {
            let Some(Block::Paragraph(p)) = container.get(idx as usize) else {
                continue;
            };
            let lo = if idx == start_idx { start.offset } else { 0 };
            let hi = if idx == end_idx {
                end.offset
            } else {
                p.text.len() as u32
            };
            let styled = p.apply_style(lo, hi, patch.clone());
            let child_path = parent.clone().push(PathStep::Block(idx));
            replace_block_in_top(&mut blocks, &child_path, Block::Paragraph(styled));
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Issue #276 — give bytes `[at.offset, end)` of the paragraph at
    /// `at.path` exactly `style` ([`Paragraph::set_style`]). Used to
    /// restyle text just typed over a selection; a no-op when the path
    /// does not address a paragraph.
    pub fn set_span_style(&self, at: LogicalPos, end: u32, style: SpanStyle) -> Self {
        let mut out = self.clone();
        let _ = mutate_paragraph_in_top(&mut out.blocks, &at.path, |para| {
            *para = para.set_style(at.offset, end, style);
        });
        out
    }

    fn apply_style_single(&self, start: LogicalPos, end: LogicalPos, patch: SpanStyle) -> Self {
        let Some(p) = self.paragraph_at_path(&start.path) else {
            return self.clone();
        };
        let hi = if start.path == end.path {
            end.offset
        } else {
            p.text.len() as u32
        };
        let styled = p.apply_style(start.offset, hi, patch);
        let mut blocks = self.blocks.clone();
        replace_block_in_top(&mut blocks, &start.path, Block::Paragraph(styled));
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Set `align` on every paragraph the logical range `[start, end)` spans
    /// (Backlog #9). Paragraphs outside the range are structurally shared.
    /// `start`/`end` are expected in document order. PR 4: same-parent
    /// ranges spread across siblings; cross-container ranges align only
    /// `start`'s paragraph.
    pub fn set_alignment(&self, start: LogicalPos, end: LogicalPos, align: Alignment) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, |para| {
                    para.props.alignment = Some(align);
                    /* Sprint 12 (#11) — shadow direct_overrides so a
                    subsequent ApplyStyle preserves this user edit. */
                    para.direct_overrides.alignment = Some(align);
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.props.alignment = Some(align);
                para.direct_overrides.alignment = Some(align);
            });
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Set paragraph base direction (`<w:bidi>`) on every paragraph the
    /// range spans. Mirrors [`Self::set_alignment`] but writes
    /// `props.direction` instead. The direction defines logical text
    /// flow + punctuation placement; alignment is a separate concern
    /// (visual anchoring). Word ties them with the
    /// writing-direction-relative `Start` / `End` alignment tokens —
    /// flipping direction automatically swaps which visual edge those
    /// resolve to, no alignment rewrite needed.
    pub fn set_direction(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        direction: TextDirection,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, |para| {
                    para.props.direction = Some(direction);
                    para.direct_overrides.direction = Some(direction);
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.props.direction = Some(direction);
                para.direct_overrides.direction = Some(direction);
            });
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Phase 3 (#40) — shared per-section mutation. Resolves the section
    /// covering the top-level block step of `pos` and applies `f` to its
    /// props IN THEIR STORAGE HOME: the closing marker paragraph for an
    /// interior section (via `mutate_paragraph_in_top`, which dirties the
    /// paragraph so the writer regenerates its `<w:pPr>` with the mutated
    /// sectPr), or [`Self::body_section`] for the final section. A fresh
    /// document always has a mutable trailing section, so this can never
    /// silently no-op — the pre-Phase-3 setters skipped the whole
    /// mutation on any document with an empty `sections` table (every
    /// non-imported document).
    fn update_section_props_at(&self, pos: &LogicalPos, f: impl FnOnce(&mut SectionProps)) -> Self {
        let block_idx = pos
            .path
            .steps
            .iter()
            .find_map(|s| match s {
                PathStep::Block(n) => Some(*n),
                PathStep::Cell { .. } => None,
            })
            .unwrap_or(0);
        let mut blocks = self.blocks.clone();
        let mut body_section = self.body_section.clone();
        /* The covering section is closed by the FIRST marker paragraph at
        index >= block_idx; markers before it close earlier sections. */
        let marker_idx = self
            .blocks
            .iter()
            .enumerate()
            .skip(block_idx as usize)
            .find_map(|(i, b)| match b {
                Block::Paragraph(p) if p.section_end.is_some() => Some(i as u32),
                _ => None,
            });
        if let Some(idx) = marker_idx {
            let _ = mutate_paragraph_in_top(&mut blocks, &BlockPath::top(idx), |para| {
                if let Some(props) = &mut para.section_end {
                    f(props);
                }
            });
        } else {
            f(&mut body_section);
        }
        Self {
            blocks,
            body_section,
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Issue #43 — author a dynamic field at `at`: splice `cached`
    /// (the placeholder display text — resolved live at layout time)
    /// into the paragraph and stamp the `Field` overlay over it.
    /// `insert_text` shifts pre-existing span/field anchors; the new
    /// overlay is inserted in start order (fields stay disjoint —
    /// authoring inside another field's range is the caller's error,
    /// tolerated as overlapping overlays the renderer resolves
    /// last-wins). Callers reject table-cell paths (the cell reader
    /// cannot round-trip fields yet).
    pub fn insert_field_at(&self, at: LogicalPos, instruction: &str, cached: &str) -> Self {
        if cached.is_empty() || self.paragraph_at_path(&at.path).is_none() {
            return self.clone();
        }
        /* Issue #115 — the field anchors where `insert_text` actually
        landed: the offset snapped against the PRE-insert text (a
        post-insert snap could land inside `cached` itself). */
        let start = self
            .paragraph_at_path(&at.path)
            .map_or(0, |p| p.snap_offset(at.offset));
        let doc = self.insert_text(at.clone(), cached);
        let mut blocks = doc.blocks.clone();
        let ok = mutate_paragraph_in_top(&mut blocks, &at.path, |para| {
            para.fields.push(Field {
                start,
                end: start + cached.len() as u32,
                instruction: instruction.to_string(),
                span: None,
                source: None,
            });
            para.fields.sort_by_key(|f| f.start);
        });
        if ok.is_none() {
            return doc;
        }
        let mut next = doc;
        next.blocks = blocks;
        next
    }

    /// Phase 3 (#40) — insert a section break at `at`, Word-exact:
    ///
    /// 1. Split the paragraph at the caret (an existing marker on it
    ///    rides the RIGHT half with the original paragraph mark).
    /// 2. Stamp the LEFT half's fresh mark with a COPY of the covering
    ///    section's props — its `section_type` (how the covering
    ///    section itself begins) travels with the first half, which
    ///    still starts the same way.
    /// 3. Set `section_type := kind` on the covering section's OWN
    ///    terminal storage — after the split that is the first marker
    ///    at an index past the left half (possibly the right half
    ///    itself), or `body_section`. NEVER the structurally-next
    ///    section's storage: OOXML `<w:type>` describes how the
    ///    section it terminates BEGINS, and the inserted break opens
    ///    the covering section's second half.
    ///
    /// Both halves start with identical geometry (Word clones the
    /// sectPr on break insertion). Returns `self` unchanged when `at`
    /// addresses a table cell — the caller surfaces the rejection.
    pub fn insert_section_break_at(&self, at: LogicalPos, kind: SectionType) -> Self {
        /* Table cells cannot host a section boundary (an interior
        sectPr is a body-level paragraph property). */
        if at.path.steps.len() != 1 {
            return self.clone();
        }
        let Some(block_idx) = at.path.last_block_index() else {
            return self.clone();
        };
        if self.paragraph_at_path(&at.path).is_none() {
            return self.clone();
        }
        let covering = SectionProps::from(&self.section_for_block(block_idx));
        let split = self.split_paragraph(at.clone());
        let mut blocks = split.blocks.clone();
        let mut body_section = split.body_section.clone();
        /* Step 2 — the left half closes the new first-half section. */
        let _ = mutate_paragraph_in_top(&mut blocks, &at.path, |para| {
            para.section_end = Some(Box::new(covering.clone()));
        });
        /* Step 3 — the covering section's own terminal now closes the
        second half; it starts per `kind`.

        Issue #70 — the second half is also born LINKED: its hf ref
        slots are CLEARED (absence = inherit, §17.10.3), matching what
        Word writes after a fresh break ("Same as Previous" on). The
        first half's marker owns the covering section's original refs
        via the step-2 copy, so rendering is unchanged — the second
        half now inherits them through the forward fold. Deleting the
        break restores ownership to this terminal via the marker-drop
        backfill in `backfill_hf_refs_from_dropped_markers` (design
        review B1 — without it, insert-then-delete would orphan the
        original parts). */
        let next_marker = blocks
            .iter()
            .enumerate()
            .skip(block_idx as usize + 1)
            .find_map(|(i, b)| match b {
                Block::Paragraph(p) if p.section_end.is_some() => Some(i as u32),
                _ => None,
            });
        if let Some(mi) = next_marker {
            let _ = mutate_paragraph_in_top(&mut blocks, &BlockPath::top(mi), |para| {
                if let Some(props) = &mut para.section_end {
                    props.section_type = kind;
                    props.header_refs = HeaderFooterRefs::default();
                    props.footer_refs = HeaderFooterRefs::default();
                }
            });
        } else {
            body_section.section_type = kind;
            body_section.header_refs = HeaderFooterRefs::default();
            body_section.footer_refs = HeaderFooterRefs::default();
        }
        Self {
            blocks,
            body_section,
            headers: split.headers.clone(),
            footers: split.footers.clone(),
            media: split.media.clone(),
            footnote_stories: split.footnote_stories.clone(),
            endnote_stories: split.endnote_stories.clone(),
            footnote_props: split.footnote_props,
            endnote_props: split.endnote_props,
            notes_dirty: split.notes_dirty.clone(),
            comment_defs: split.comment_defs.clone(),
            comment_ranges: split.comment_ranges.clone(),
            settings: split.settings.clone(),
            styles: split.styles.clone(),
            style_defaults: split.style_defaults.clone(),
            style_run_defaults: split.style_run_defaults.clone(),
            styles_dirty: split.styles_dirty,
            numbering: split.numbering.clone(),
            hf_dirty: split.hf_dirty.clone(),
            settings_dirty: split.settings_dirty,
            document_root_attrs: split.document_root_attrs.clone(),
            part_root_attrs: split.part_root_attrs.clone(),
            document_envelope: split.document_envelope.clone(),
            source_package: split.source_package.clone(),
        }
    }

    /// Phase 3 (#39) — point the covering section's DEFAULT
    /// header/footer reference at `rid`. Backs materialize-on-enter and
    /// fork-on-enter: the caller has already installed the part under
    /// `rid` via [`Self::with_updated_header_part`] /
    /// [`Self::with_updated_footer_part`].
    pub fn set_section_hf_default_ref_at(&self, pos: LogicalPos, header: bool, rid: &str) -> Self {
        self.set_section_hf_ref_at(pos, header, HeaderFooterRole::Default, Some(rid))
    }

    /// Issues #70/#74 — the generalized ref setter: write (or CLEAR,
    /// with `rid: None`) any of the covering section's six
    /// (header/footer × default/first/even) reference slots.
    /// `None` is the Link-to-Previous storage state — inheritance is
    /// the ABSENCE of a slot (§17.10.3), so "relink" is a clear.
    pub fn set_section_hf_ref_at(
        &self,
        pos: LogicalPos,
        header: bool,
        role: HeaderFooterRole,
        rid: Option<&str>,
    ) -> Self {
        let rid = rid.map(str::to_string);
        self.update_section_props_at(&pos, move |props| {
            let refs = if header {
                &mut props.header_refs
            } else {
                &mut props.footer_refs
            };
            match role {
                HeaderFooterRole::Default => refs.default = rid,
                HeaderFooterRole::First => refs.first = rid,
                HeaderFooterRole::Even => refs.even = rid,
            }
        })
    }

    /// Issue #74 — toggle `<w:titlePg/>` ("different first page") on
    /// the covering section.
    pub fn set_section_title_pg_at(&self, pos: LogicalPos, enabled: bool) -> Self {
        self.update_section_props_at(&pos, move |props| {
            props.title_pg = enabled;
        })
    }

    /// Issue #74 — document-wide `<w:evenAndOddHeaders/>` toggle
    /// (settings.xml, NOT sectPr — even/odd is a document setting).
    /// Flips `settings_dirty` so the writer patches settings.xml.
    pub fn with_even_odd_headers(&self, enabled: bool) -> Self {
        let mut next = self.clone();
        if next.settings.even_and_odd_headers != enabled {
            next.settings.even_and_odd_headers = enabled;
            next.settings_dirty = true;
        }
        next
    }

    /// Sprint 2 (UI Edition) — set the covering section's `columns` for
    /// the section containing the top-level block step at `pos`.
    /// `count == 0` collapses to single column (matches
    /// `ColumnSpec::from_twips` defensive clamping). The paginator picks
    /// the new geometry up on the next reflow.
    pub fn set_section_columns_at(&self, pos: LogicalPos, count: u8, gutter_pt: f32) -> Self {
        self.update_section_props_at(&pos, |props| {
            props.columns = ColumnSpec {
                count: count.max(1),
                gutter_pt,
            };
        })
    }

    /// Sprint 2 (UI Edition) — set `ParaProperties.page_break_before`
    /// on the paragraph identified by `pos`. The flag is already
    /// honoured by the paginator (renders from `<w:pageBreakBefore>`
    /// on `.docx` load); this method is what the editor calls when the
    /// user inserts a page break via `Ctrl+Enter`.
    pub fn set_page_break_before(&self, pos: LogicalPos, value: bool) -> Self {
        let mut blocks = self.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, &pos.path, |para| {
            para.props.page_break_before = value;
        });
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /* ===========================================================
    Sprint 7 (UI Edition) — review mutators.
    Track changes RECORDING (gating new edits as `<w:ins>`/`<w:del>`)
    is a separate Core Engine task and is NOT implemented here; the
    bridge `ToggleTrackChanges` surfaces an Error. The mutators
    below operate on revisions and comments already present in the
    `DocumentTree`.
    =========================================================== */

    /// Accept a tracked-change revision identified by (top-level
    /// `block`, byte `start`, byte `end`). Semantics:
    ///   - `Insert + Accept` → keep the inserted text, drop the overlay
    ///   - `Delete + Accept` → drop the deleted text + drop the overlay
    pub fn accept_revision_at(&self, block: u32, start: u32, end: u32) -> Self {
        self.apply_revision_decision(block, start, end, /* accept = */ true)
    }

    /// Reject a tracked-change revision identified by (top-level
    /// `block`, byte `start`, byte `end`). Semantics:
    ///   - `Insert + Reject` → drop the inserted text + drop the overlay
    ///   - `Delete + Reject` → keep the original text, drop the overlay
    pub fn reject_revision_at(&self, block: u32, start: u32, end: u32) -> Self {
        self.apply_revision_decision(block, start, end, /* accept = */ false)
    }

    fn apply_revision_decision(&self, block: u32, start: u32, end: u32, accept: bool) -> Self {
        /* Issue #262 — a paragraph-MARK revision is addressed as the
        empty range at the paragraph end (`revisions_snapshot` lists it
        so); text revisions are never empty. */
        if start == end
            && let Some(p) = self
                .blocks
                .get(block as usize)
                .and_then(Block::as_paragraph)
            && p.mark_revision.is_some()
            && start as usize == p.text.len()
            && !p.revisions.iter().any(|r| r.start == start && r.end == end)
        {
            return self.resolve_mark_revision_at(block, accept);
        }
        let mut blocks = self.blocks.clone();
        let path = BlockPath::top(block);
        let mut removed_edit = None;
        let _ = mutate_paragraph_in_top(&mut blocks, &path, |para| {
            let Some(idx) = para
                .revisions
                .iter()
                .position(|r| r.start == start && r.end == end)
            else {
                return;
            };
            /* Remove the matched revision FIRST so the offset-shift
             * helper does not also `retain`-drop it (which would make
             * any post-shift index lookup brittle). */
            let rev = para.revisions.remove(idx);
            /* Issue #262 — a rejected formatting change restores the
            recorded style. */
            if !accept
                && rev.kind == RevisionKind::FormatChange
                && let Some(prev) = &rev.prev_attrs
            {
                revisions::restyle(para, rev.start, rev.end, prev);
            }
            /* Reject Insert / MoveTo, accept Delete / MoveFrom (issue
            #247): the text goes; otherwise it stays live. */
            let delete_text = rev.kind.removes_text(accept);
            if delete_text {
                let s = para.snap_offset(rev.start);
                let e = para.snap_offset(rev.end);
                if s < e {
                    /* Issues #250 / #252 — one splice drives the source
                    markup and (below) the comment anchors. */
                    let edit = para.splice_text(s, e - s, "");
                    shift_paragraph_offsets_after(para, edit.at, edit.removed);
                    removed_edit = Some(edit);
                }
            }
            para.dirty = true;
        });
        let mut out = Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        if let Some(e) = removed_edit {
            out.remap_text_edit_record(&path, e);
        }
        out
    }

    /// Sprint 7 (UI Edition) — append a new comment anchored to a
    /// logical range. Picks a fresh `id` (max existing + 1) and
    /// installs both a `CommentDef` (with `paragraphs = [text]`)
    /// and a matching `CommentRange`. Returns `(new_doc, id)`.
    pub fn insert_comment(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        text: String,
        author: String,
        date: String,
    ) -> (Self, u32) {
        let (start, end) = order_positions(self.snap_pos(start), self.snap_pos(end));
        let new_id = self
            .comment_defs
            .keys()
            .max()
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let mut comment_defs = self.comment_defs.clone();
        comment_defs.insert(
            new_id,
            CommentDef {
                author,
                date,
                paragraphs: vec![text],
                resolved: false,
                /* Engine-minted comments have no paraId yet — their
                resolved state survives only in-memory until the
                comments.xml writer learns to mint paraIds. */
                first_para_id: None,
                parent_id: None,
            },
        );
        let mut comment_ranges = self.comment_ranges.clone();
        comment_ranges.push(CommentRange {
            id: new_id,
            start,
            end,
        });
        let doc = Self {
            blocks: self.blocks.clone(),
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs,
            comment_ranges,
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        (doc, new_id)
    }

    /// Issue #27 — append a threaded reply to an existing comment.
    /// Mints the next `w:id` (max existing + 1, same discipline as
    /// [`Self::insert_comment`]) and installs a `CommentDef` with
    /// `parent_id = Some(parent_id)`. The reply's `CommentRange` is
    /// CLONED from the parent's range (same `start` / `end`) — Word
    /// anchors replies on the parent's span, and the snapshot loop
    /// (which iterates `comment_ranges`) then surfaces the reply
    /// without any special-casing. A parent that carries no range
    /// (orphan def) still accepts the reply; no range is pushed in
    /// that case, mirroring the parent's own anchor-less state.
    ///
    /// Returns `None` when `parent_id` names no existing comment —
    /// the wasm layer maps that to `Event::Error`.
    pub fn reply_to_comment(
        &self,
        parent_id: u32,
        text: String,
        author: String,
        date: String,
    ) -> Option<(Self, u32)> {
        if !self.comment_defs.contains_key(&parent_id) {
            return None;
        }
        let new_id = self
            .comment_defs
            .keys()
            .max()
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let mut comment_defs = self.comment_defs.clone();
        comment_defs.insert(
            new_id,
            CommentDef {
                author,
                date,
                paragraphs: vec![text],
                resolved: false,
                /* Engine-minted replies have no paraId yet — the
                comments.xml writer mints one at save time. */
                first_para_id: None,
                parent_id: Some(parent_id),
            },
        );
        let mut comment_ranges = self.comment_ranges.clone();
        if let Some(parent_range) = self.comment_ranges.iter().find(|r| r.id == parent_id) {
            comment_ranges.push(CommentRange {
                id: new_id,
                start: parent_range.start.clone(),
                end: parent_range.end.clone(),
            });
        }
        let doc = Self {
            blocks: self.blocks.clone(),
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs,
            comment_ranges,
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        Some((doc, new_id))
    }

    /// Sprint 7 (UI Edition) — remove a comment by id from both
    /// `comment_defs` and `comment_ranges`.
    ///
    /// Issue #27 — deletion CASCADES through the reply thread: every
    /// comment whose `parent_id` chain (walked transitively) reaches
    /// the deleted id is removed too, along with its ranges. Deleting
    /// a reply leaves its parent untouched.
    pub fn delete_comment(&self, id: u32) -> Self {
        /* Transitive closure of the thread rooted at `id`. Fixpoint
        loop — reply chains are short (Word nests one level, but a
        chain of replies-to-replies still terminates because each
        pass only ever adds ids). */
        let mut doomed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        doomed.insert(id);
        loop {
            let before = doomed.len();
            for (cid, def) in &self.comment_defs {
                if let Some(pid) = def.parent_id
                    && doomed.contains(&pid)
                {
                    doomed.insert(*cid);
                }
            }
            if doomed.len() == before {
                break;
            }
        }
        let mut comment_defs = self.comment_defs.clone();
        comment_defs.retain(|cid, _| !doomed.contains(cid));
        let mut comment_ranges = self.comment_ranges.clone();
        comment_ranges.retain(|r| !doomed.contains(&r.id));
        Self {
            blocks: self.blocks.clone(),
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs,
            comment_ranges,
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Sprint 7 (UI Edition) — set the in-memory `resolved` flag on
    /// the comment with the given id. No `commentsExtended.xml`
    /// round-trip yet — see Core Engine backlog.
    pub fn set_comment_resolved(&self, id: u32, resolved: bool) -> Self {
        let mut comment_defs = self.comment_defs.clone();
        if let Some(cd) = comment_defs.get_mut(&id) {
            cd.resolved = resolved;
        }
        Self {
            blocks: self.blocks.clone(),
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs,
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Sprint 6 (UI Edition) — set `<w:pPr><w:ind>` (paragraph
    /// indentation) on every paragraph the range spans. Values in pt
    /// (1 pt = 20 twips). `first_line_pt > 0` populates
    /// `first_line_twips`; `first_line_pt < 0` populates
    /// `hanging_twips` with `|first_line_pt| * 20` (Word's mutually-
    /// exclusive `<w:firstLine>` vs `<w:hanging>` semantics).
    ///
    /// `start_pt` / `end_pt` may be **negative** — a negative `<w:start>` /
    /// `<w:end>` is a Word/Google-Docs *outdent* that pulls the leading /
    /// trailing edge into the page margin. ECMA-376 defines `w:start` /
    /// `w:end` as `ST_SignedTwipsMeasure`, so negative twips round-trip
    /// faithfully through the `.docx` writer. `first_line_pt` stays signed
    /// only through the `firstLine` / `hanging` split — both of those are
    /// `ST_TwipsMeasure` (unsigned), so the magnitude is always stored
    /// non-negative.
    pub fn set_paragraph_indent(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        start_pt: f32,
        end_pt: f32,
        first_line_pt: f32,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        /* No `.max(0.0)` floor: negative start/end are first-class outdents
        (Bug B — "the grey area"). The Ruler clamps the drag to the page
        edge so the model never receives an outdent larger than the margin. */
        let start_twips = (start_pt * 20.0).round() as i32;
        let end_twips = (end_pt * 20.0).round() as i32;
        let (first_line_twips, hanging_twips) = if first_line_pt >= 0.0 {
            ((first_line_pt * 20.0).round() as i32, 0)
        } else {
            (0, (-first_line_pt * 20.0).round() as i32)
        };
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            let new_ind = Indent {
                start_twips,
                end_twips,
                first_line_twips,
                hanging_twips,
            };
            para.props.indent = new_ind;
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.indent = new_ind;
        };
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Issue #277 — the style a paragraph created by Enter at the end
    /// of a `style_id` paragraph takes: the style's `<w:next>` when it
    /// names a DIFFERENT style this document defines, else `None`
    /// (keep the same style).
    pub fn next_style_after(&self, style_id: Option<&str>) -> Option<String> {
        let id = style_id?;
        let next = self.styles.get(id)?.next.as_deref()?;
        (next != id && self.styles.contains_key(next)).then(|| next.to_owned())
    }

    /// Sprint 12 (#11) — resolve the paragraph cascade for `style_id`
    /// into a flat `ParaProperties`. Cycle-safe (visited set) +
    /// depth-capped at [`MAX_STYLE_CHAIN`] entries, matching ECMA-376
    /// §17.7.4.5 implementation guidance. Used both by
    /// [`Self::recompute_paragraph_props`] (on every style mutation)
    /// and by the reader's first-pass cascade.
    pub fn resolve_style_cascade(&self, style_id: Option<&str>) -> ParaProperties {
        let mut out = self.style_defaults.clone();
        let Some(leaf) = style_id else {
            return out;
        };
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut chain: Vec<&ParagraphStyle> = Vec::new();
        let mut current: Option<&str> = Some(leaf);
        while let Some(id) = current {
            if chain.len() >= MAX_STYLE_CHAIN || !visited.insert(id) {
                break;
            }
            let Some(def) = self.styles.get(id) else {
                break;
            };
            chain.push(def);
            current = def.based_on.as_deref();
        }
        for def in chain.iter().rev() {
            out = out.clone().merged_with(def.para.clone());
        }
        out
    }

    /// Issue #29 — the RUN half of the cascade: fold
    /// `style_run_defaults → pStyle chain <w:rPr>` (root → leaf) into
    /// the `SpanStyle` a run inherits before its direct formatting.
    /// Same cycle / depth guards as [`Self::resolve_style_cascade`].
    pub fn resolve_style_run_cascade(&self, style_id: Option<&str>) -> SpanStyle {
        resolve_run_cascade(&self.styles, &self.style_run_defaults, style_id)
    }

    /// Sprint 12 (#11) — apply `style_id` to every paragraph the range
    /// spans. The user's pre-existing `direct_overrides` are
    /// preserved; only `props` (the resolved view) is recomputed so
    /// downstream rendering picks up the cascade. Empty `style_id`
    /// detaches the paragraph from any style (resolved view falls
    /// back to `style_defaults ∪ direct_overrides`).
    pub fn set_paragraph_style(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        style_id: Option<String>,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let styles_for_apply = self.styles.clone();
        let defaults_for_apply = self.style_defaults.clone();
        let apply = |para: &mut Paragraph| {
            para.style_id = style_id.clone();
            recompute_paragraph_props(para, &styles_for_apply, &defaults_for_apply);
        };
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Issue #21 — mutate an existing style definition in-place and
    /// re-cascade every styled paragraph (body + table cells). Patch
    /// semantics: `None` leaves a half untouched; `Some(patch)` folds
    /// over the current definition via `merged_with`. `based_on` is the
    /// three-state knob (leave / clear / re-parent). Direct overrides
    /// survive verbatim — the #11 re-application discipline. Flips
    /// `styles_dirty` so the writer regenerates `word/styles.xml`.
    /// Unknown `style_id` is a no-op clone.
    pub fn modify_style(
        &self,
        style_id: &str,
        para_patch: Option<ParaProperties>,
        run_patch: Option<SpanStyle>,
        based_on: Option<Option<String>>,
        display_name: Option<String>,
    ) -> Self {
        if !self.styles.contains_key(style_id) {
            return self.clone();
        }
        let mut styles = self.styles.clone();
        if let Some(def) = styles.get_mut(style_id) {
            if let Some(pp) = para_patch {
                def.para = def.para.clone().merged_with(pp);
            }
            if let Some(rp) = run_patch {
                def.run = def.run.clone().merged_with(rp);
            }
            if let Some(b) = based_on {
                def.based_on = b;
            }
            if let Some(n) = display_name {
                def.name = n;
            }
        }
        /* Re-resolve every paragraph's cascaded para props against the
        mutated table (the run half folds at span-materialize time, so
        it re-cascades for free). Recomputing unstyled paragraphs too
        is a harmless idempotent fold — cheaper than chain-membership
        bookkeeping and immune to basedOn re-parenting edge cases. */
        fn recompute_block(
            b: &mut Block,
            styles: &std::collections::HashMap<String, ParagraphStyle>,
            defaults: &ParaProperties,
        ) {
            match b {
                Block::Paragraph(p) => recompute_paragraph_props(p, styles, defaults),
                Block::Table(t) => {
                    for row in &mut t.rows {
                        for cell in &mut row.cells {
                            for cb in &mut cell.blocks {
                                recompute_block(cb, styles, defaults);
                            }
                        }
                    }
                }
            }
        }
        let mut blocks = self.blocks.clone();
        for i in 0..blocks.len() {
            let mut b = blocks[i].clone();
            recompute_block(&mut b, &styles, &self.style_defaults);
            blocks.set(i, b);
        }
        /* Design review B5 — header/footer paragraphs carry the SAME
        baked cascade; skipping them left band paragraphs styled with
        the pre-mutation look until an unrelated part edit. Recompute
        every part; dirty tracking is untouched (a pure cascade
        recompute does not change the part's serialized styling —
        `pStyle` stays; the writer re-resolves at emission). */
        let mut headers = self.headers.clone();
        for part in headers.values_mut() {
            for b in part.iter_mut() {
                recompute_block(b, &styles, &self.style_defaults);
            }
        }
        let mut footers = self.footers.clone();
        for part in footers.values_mut() {
            for b in part.iter_mut() {
                recompute_block(b, &styles, &self.style_defaults);
            }
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers,
            footers,
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles,
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: true,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Sprint 11 (#13) — replace `<w:pPr><w:tabs>` on every paragraph
    /// the range spans with `stops`. Empty `stops` clears the
    /// paragraph's custom tab grid (it falls back to the default
    /// 0.5-inch grid the line builder ships). One commit per call,
    /// so the Ruler's drag-end dispatch produces exactly one undo
    /// entry per tab-stop edit (matches Word's "release commits"
    /// behaviour).
    ///
    /// Issue #145 — each `stops[i]` is a [`TabStopPatch`]: a `None`
    /// leader inherits the paragraph's *own current* `tab_stops[i]`
    /// leader (resolved per paragraph, since a multi-paragraph range
    /// can carry different existing leaders); an explicit `Some` sets
    /// or clears it. Without this, replacing the whole `<w:tabs>` list
    /// on every write silently dropped a TOC entry's dot leader the
    /// first time its stop was dragged.
    pub fn set_tab_stops(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        stops: Vec<TabStopPatch>,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            let resolved: Vec<TabStop> = stops
                .iter()
                .enumerate()
                .map(|(i, patch)| TabStop {
                    position_pt: patch.position_pt,
                    kind: patch.kind,
                    leader: patch.leader.unwrap_or_else(|| {
                        para.props
                            .tab_stops
                            .get(i)
                            .map_or(TabLeader::None, |s| s.leader)
                    }),
                })
                .collect();
            para.props.tab_stops = resolved.clone();
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.tab_stops = resolved;
        };
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Sprint 6 (UI Edition) — set `<w:pPr><w:spacing w:line>` as an
    /// `Auto` (multiplier) line height. 240 twips = single (1.0×).
    /// Pass `multiplier <= 0.0` to clear (`line_height: None`).
    pub fn set_line_spacing(&self, start: LogicalPos, end: LogicalPos, multiplier: f32) -> Self {
        let target = if multiplier > 0.0 {
            Some(LineHeight::Auto {
                twips: (multiplier * 240.0).round() as i32,
            })
        } else {
            None
        };
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            para.props.line_height = target;
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.line_height = target;
        };
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Sprint 6 (UI Edition) — set `<w:pPr><w:shd>` (paragraph
    /// shading) on every paragraph the range spans. `None` clears.
    pub fn set_paragraph_shading(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        color: Option<[u8; 4]>,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            para.props.shading = color;
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.shading = color;
        };
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Issue #50 — re-stamp `resolved_marker` / `resolved_list_indent`
    /// across the top-level paragraphs after a structural mutation
    /// (split / merge / splice). `Paragraph::split_at` / `concat` clone
    /// the stamped fields onto both outputs, so without a refresh an
    /// Enter inside a numbered list shows the head's marker twice
    /// instead of renumbering the tail. Cheap early-out when the
    /// document carries no list paragraphs.
    fn with_list_markers_refreshed(mut self) -> Self {
        let has_lists = self
            .blocks
            .iter()
            .any(|b| b.as_paragraph().is_some_and(|p| p.list_item.is_some()));
        if !has_lists {
            return self;
        }
        /* Two passes so `im::Vector` structural sharing survives the hot
        edit path: compute the expected stamps immutably, then path-copy
        ONLY the paragraphs whose stamps actually changed. An Enter inside
        a bullet list changes nothing ("•" stays "•"); a numbered-list
        edit touches only the renumbered tail — a blanket `iter_mut()`
        would thaw every chunk of the shared tree (and un-share every
        UndoStack snapshot) on each split/merge/paste. */
        let (para_indices, items): (Vec<usize>, Vec<Option<ListItem>>) = self
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(idx, b)| b.as_paragraph().map(|p| (idx, p.list_item)))
            .unzip();
        let expected = numbering::compute_markers(&items, &self.numbering);
        for (idx, (marker, indent)) in para_indices.into_iter().zip(expected) {
            let stale = self.blocks[idx]
                .as_paragraph()
                .is_some_and(|p| p.resolved_marker != marker || p.resolved_list_indent != indent);
            if stale && let Some(p) = self.blocks.get_mut(idx).and_then(|b| b.as_paragraph_mut()) {
                p.resolved_marker = marker;
                p.resolved_list_indent = indent;
            }
        }
        self
    }

    /// Sprint 5 (UI Edition) — clear `list_item` on every paragraph
    /// the range spans. The engine has no numbering synthesizer
    /// today, so this is the only list mutation that is safe to
    /// expose: removing list membership cannot introduce a dangling
    /// `num_id`. Adding list membership is filed as a Core Engine
    /// task (see project backlog).
    /// Sprint 13 (#12) — set `Paragraph.list_item = Some(ListItem {
    /// num_id, ilvl: 0 })` on every paragraph the range spans, then
    /// re-resolve markers for the whole document. `num_id` is the
    /// idempotent return from
    /// [`numbering::NumberingDefinitions::synth_list_definition`] —
    /// reuses an existing matching template when one is present so
    /// repeated toggles do not inflate `numbering.xml`.
    ///
    /// The synth runs against a CLONED numbering store; only if it
    /// flips `.dirty` does the new store replace the existing one
    /// (preserves passthrough byte-identity in the no-op reuse
    /// case).
    pub fn toggle_list_on_range(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        kind: numbering::ListSynthesisKind,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut next_numbering = self.numbering.clone();
        let num_id = next_numbering.synth_list_definition(kind);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            para.list_item = Some(ListItem { num_id, ilvl: 0 });
            /* resolved_marker is re-stamped by the document-wide
            resolver below — clearing now keeps it consistent if the
            resolver bails on a malformed cascade. */
            para.resolved_marker = None;
        };
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        /* Document-wide marker refresh — counters reset at the top
        because the toggled range might appear in the middle. */
        let mut paragraph_refs: Vec<&mut Paragraph> = blocks
            .iter_mut()
            .filter_map(|b| b.as_paragraph_mut())
            .collect();
        numbering::resolve_markers_in_place(&mut paragraph_refs, &next_numbering);
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: next_numbering,
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Issue #42 — demote (`delta > 0`) or promote (`delta < 0`) the
    /// outline level of every list paragraph the range spans. Clamped to
    /// `0..=8` — Word's nine stock outline levels, all synthesized up
    /// front by [`numbering::stock_bullet_levels`] /
    /// [`numbering::stock_number_levels`], so a bumped `ilvl` always
    /// resolves against a real level definition. Paragraphs with no
    /// `list_item` (not in a list) are left untouched — this is a no-op
    /// on plain body text, matching the Tab-key contract in the shell.
    pub fn change_list_level_on_range(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        delta: i8,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            if let Some(item) = para.list_item.as_mut() {
                item.ilvl = (i16::from(item.ilvl) + i16::from(delta)).clamp(0, 8) as u8;
                /* resolved_marker is re-stamped by the document-wide
                resolver below — clearing now keeps it consistent if the
                resolver bails on a malformed cascade. */
                para.resolved_marker = None;
            }
        };
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        /* Document-wide marker refresh — counters reset at the top
        because the changed range might appear in the middle. */
        let mut paragraph_refs: Vec<&mut Paragraph> = blocks
            .iter_mut()
            .filter_map(|b| b.as_paragraph_mut())
            .collect();
        numbering::resolve_markers_in_place(&mut paragraph_refs, &self.numbering);
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    pub fn clear_list_item_on_range(&self, start: LogicalPos, end: LogicalPos) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, |para| {
                    para.list_item = None;
                    para.resolved_marker = None;
                    para.resolved_list_indent = None;
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.list_item = None;
                para.resolved_marker = None;
                para.resolved_list_indent = None;
            });
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
        .with_list_markers_refreshed()
    }

    /// Sprint 4 (UI Edition) — set `<w:pgMar>` (top/right/bottom/left
    /// in points) on the section containing the top-level block step
    /// at `pos`. Header/footer offsets are preserved.
    pub fn set_section_margins_at(
        &self,
        pos: LogicalPos,
        top_pt: f32,
        right_pt: f32,
        bottom_pt: f32,
        left_pt: f32,
    ) -> Self {
        self.update_section_props_at(&pos, |props| {
            props.geometry.margin_top = top_pt.max(0.0);
            props.geometry.margin_right = right_pt.max(0.0);
            props.geometry.margin_bottom = bottom_pt.max(0.0);
            props.geometry.margin_left = left_pt.max(0.0);
        })
    }

    /// Sprint 4 (UI Edition) — force the orientation of the section
    /// containing `pos`. `landscape == true` swaps width and height
    /// when width <= height; `landscape == false` swaps the other
    /// way. Margins are NOT rotated — Word treats `<w:pgMar>` as
    /// edge-labelled, not paper-relative.
    pub fn set_section_orientation_at(&self, pos: LogicalPos, landscape: bool) -> Self {
        self.update_section_props_at(&pos, |props| {
            let is_landscape = props.geometry.width > props.geometry.height;
            if landscape != is_landscape {
                let (w, h) = (props.geometry.width, props.geometry.height);
                props.geometry.width = h;
                props.geometry.height = w;
            }
        })
    }

    /// Sprint 3 (UI Edition) — insert a brand-new inline image at
    /// `pos`. Picks a fresh `rel_id` (collision-free against
    /// `media`), inserts a U+FFFC sentinel in the paragraph text at
    /// the byte offset, shifts existing styled spans + inline
    /// objects + hyperlinks + revisions + fields rightward by the
    /// sentinel's UTF-8 length, and appends a new
    /// [`InlineKind::Image`] entry pointing at the registered blob.
    ///
    /// Width/height are passed in EMU (English Metric Units —
    /// 914_400 per inch) so the model unit matches what `.docx`
    /// readers and the renderer already expect.
    pub fn insert_inline_image_at(
        &self,
        pos: LogicalPos,
        blob: ImageBlob,
        width_emu: i64,
        height_emu: i64,
    ) -> Self {
        let mut media = self.media.clone();
        /* Find a rel_id not already in the media map. Walk a counter
         * past any existing `nge_img_*` keys so removal-then-insert
         * cycles never collide. */
        let mut counter = media.len() as u32 + 1;
        let rel_id = loop {
            let candidate = format!("nge_img_{counter}");
            if !media.contains_key(&candidate) {
                break candidate;
            }
            counter = counter.saturating_add(1);
        };
        media.insert(rel_id.clone(), blob);

        let mut blocks = self.blocks.clone();
        let target = if self.paragraph_at_path(&pos.path).is_some() {
            pos.path.clone()
        } else {
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let off = pos.offset;
        let rel_id_for_inline = rel_id.clone();
        let mut edit = None;
        let _ = mutate_paragraph_in_top(&mut blocks, &target, |para| {
            edit = Some(splice_inline_object(
                para,
                off,
                InlineKind::Image {
                    rel_id: rel_id_for_inline.clone(),
                    width_emu,
                    height_emu,
                    media_key: None,
                },
            ));
        });

        let mut out = Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media,
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        if let Some(e) = edit {
            out.remap_text_edit_record(&target, e);
        }
        out
    }

    /// Issue #44 — overwrite the display extent of the inline image
    /// anchored at `(path, at)`. `at` is the `U+FFFC` sentinel byte
    /// offset the image was inserted at (its `InlineObject.at`), which is
    /// the true anchor even when the same blob is inserted more than once.
    /// Dimensions are EMU (the model's native `<wp:extent>` unit), so the
    /// `.docx` writer threads them straight through with no px round-trip.
    /// A no-op (returns a structural clone) when the offset holds no image.
    pub fn resize_inline_image_at(
        &self,
        path: &BlockPath,
        at: u32,
        width_emu: i64,
        height_emu: i64,
    ) -> Self {
        let mut blocks = self.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, path, |para| {
            for io in &mut para.inline_objects {
                if io.at == at
                    && let InlineKind::Image {
                        width_emu: w,
                        height_emu: h,
                        ..
                    } = &mut io.kind
                {
                    *w = width_emu.max(1);
                    *h = height_emu.max(1);
                }
            }
        });
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Issue #69 — reposition the FLOATING image anchored at `(path, at)`
    /// to fixed offsets inside its reference frames: both axes become
    /// `FloatOffset::Emu` (an `<wp:align>` / percentage placement is
    /// replaced, exactly as Word converts an aligned object to an absolute
    /// offset the moment it is dragged) and `simplePos` is switched off so
    /// the axes are what the layout reads. The reference frames
    /// (`relative_from`) are preserved — EXCEPT for a `simplePos` object,
    /// whose only frame was the page corner: the layout reports its frame
    /// origin as the page's top-left, the shell hands back page-relative
    /// offsets, so both axes are re-based onto `Page` to keep the object
    /// where it was dropped. A no-op (structural clone) when the offset
    /// holds no floating image.
    pub fn move_floating_image_at(
        &self,
        path: &BlockPath,
        at: u32,
        offset_h_emu: i64,
        offset_v_emu: i64,
    ) -> Self {
        let mut blocks = self.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, path, |para| {
            for io in &mut para.inline_objects {
                if io.at == at
                    && matches!(io.kind, InlineKind::Image { .. })
                    && let Some(anchor) = io.anchor.as_mut()
                {
                    if anchor.simple_pos {
                        anchor.position_h.relative_from = HRelativeFrom::Page;
                        anchor.position_v.relative_from = VRelativeFrom::Page;
                    }
                    anchor.position_h.offset = FloatOffset::Emu(offset_h_emu);
                    anchor.position_v.offset = FloatOffset::Emu(offset_v_emu);
                    anchor.simple_pos = false;
                }
            }
        });
        Self {
            blocks,
            ..self.clone()
        }
    }

    /// Issue #82 — set the wrap mode of the FLOATING image anchored at
    /// `(path, at)`: `wrap` is the declared mode, `behind_doc` picks
    /// "behind text" vs "in front of text" (only meaningful with
    /// `WrapKind::None`; any other mode paints in front, as Word does).
    /// The verbatim wrap element is dropped when the mode changes — the
    /// writer then synthesizes it from the typed fields (a tight / through
    /// switch gets Word's default full-rectangle polygon unless the
    /// object already carried one). Side rule and distances are kept. A
    /// no-op (structural clone) when the offset holds no floating image.
    pub fn set_floating_image_wrap_at(
        &self,
        path: &BlockPath,
        at: u32,
        wrap: WrapKind,
        behind_doc: bool,
    ) -> Self {
        let mut blocks = self.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, path, |para| {
            for io in &mut para.inline_objects {
                if io.at == at
                    && matches!(io.kind, InlineKind::Image { .. })
                    && let Some(anchor) = io.anchor.as_mut()
                {
                    if anchor.wrap != wrap {
                        anchor.wrap = wrap;
                        anchor.wrap_xml = None;
                    }
                    anchor.behind_doc = matches!(wrap, WrapKind::None) && behind_doc;
                }
            }
        });
        Self {
            blocks,
            ..self.clone()
        }
    }

    /// Issue #83 — the text box anchored at byte `at` of the paragraph
    /// at `host` (body-rooted, cells included).
    pub fn text_box_at(&self, host: &BlockPath, at: u32) -> Option<&TextBoxStory> {
        self.paragraph_at_path(host)?
            .inline_objects
            .iter()
            .find_map(|io| match &io.kind {
                InlineKind::TextBox { story, .. } if io.at == at => Some(story.as_ref()),
                _ => None,
            })
    }

    /// Issue #206 — the story tree a chain of text-box hops addresses:
    /// each `(host, at)` names the box anchored at byte `at` of the
    /// paragraph `host`, rooted in the previous hop's story (the first in
    /// this tree). An empty chain is this tree itself. `None` when a hop
    /// holds no text box. The chain is walked once per hop, so its length
    /// bounds the descent.
    pub fn text_box_story_tree(&self, hops: &[(BlockPath, u32)]) -> Option<Self> {
        match hops.split_first() {
            None => Some(self.clone()),
            Some(((host, at), rest)) => {
                let story = self.text_box_at(host, *at)?;
                DocumentTree::from_blocks(story.body.clone()).text_box_story_tree(rest)
            }
        }
    }

    /// Issue #206 — run the model edit `f` inside the story a chain of
    /// text-box hops addresses ([`Self::text_box_story_tree`]) and write
    /// the edited story back up the chain through
    /// [`Self::with_updated_text_box`] (each box on the chain goes dirty;
    /// a top-level host keeps its passthrough bytes). An empty chain runs
    /// `f` on this tree. `None` when a hop holds no text box.
    pub fn with_text_box_story_edit(
        &self,
        hops: &[(BlockPath, u32)],
        f: impl FnOnce(&DocumentTree) -> DocumentTree,
    ) -> Option<Self> {
        match hops.split_first() {
            None => Some(f(self)),
            Some(((host, at), rest)) => {
                let story = DocumentTree::from_blocks(self.text_box_at(host, *at)?.body.clone());
                let edited = story.with_text_box_story_edit(rest, f)?;
                Some(self.with_updated_text_box(host, *at, edited.blocks.iter().cloned().collect()))
            }
        }
    }

    /// Issue #83 — replace the story of the text box at `(host, at)` and
    /// mark it dirty for the writer. The HOST paragraph keeps its
    /// passthrough bytes when it is a top-level paragraph: the writer
    /// splices the regenerated container in through
    /// [`TextBoxStory::host_range`], so a story edit never regenerates
    /// the surrounding runs. (A host inside a table cell dirties the
    /// table, like every cell mutation.) Section markers are stripped —
    /// a story is not the body. A no-op clone when nothing is there.
    pub fn with_updated_text_box(&self, host: &BlockPath, at: u32, body: Vec<Block>) -> Self {
        let body = strip_section_markers(body);
        let mut blocks = self.blocks.clone();
        let update = |para: &mut Paragraph| {
            for io in &mut para.inline_objects {
                if io.at == at
                    && let InlineKind::TextBox { story, .. } = &mut io.kind
                {
                    story.body = body.clone();
                    story.dirty = true;
                }
            }
        };
        if host.steps.len() == 1 {
            let _ = mutate_paragraph_keep_source(&mut blocks, host, update);
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, host, update);
        }
        Self {
            blocks,
            ..self.clone()
        }
    }

    /// Issue #83 — insert an engine-authored floating text box at `pos`:
    /// a U+FFFC anchor carrying an [`InlineKind::TextBox`] with one empty
    /// paragraph, `width_emu` × `height_emu`, a black 0.75 pt outline,
    /// white fill, square wrap, positioned column-relative at the anchor
    /// paragraph's top (Word's "Draw Text Box" defaults). Returns the new
    /// tree plus the `(host, at)` address of the box.
    pub fn insert_text_box_at(
        &self,
        pos: LogicalPos,
        width_emu: i64,
        height_emu: i64,
    ) -> (Self, BlockPath, u32) {
        let target = if self.paragraph_at_path(&pos.path).is_some() {
            pos.path.clone()
        } else {
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let mut blocks = self.blocks.clone();
        let mut placed_at = 0u32;
        let mut edit = None;
        let _ = mutate_paragraph_in_top(&mut blocks, &target, |para| {
            let mut off = (pos.offset as usize).min(para.text.len());
            while !para.text.is_char_boundary(off) {
                off -= 1;
            }
            placed_at = off as u32;
            edit = Some(splice_inline_object(
                para,
                off as u32,
                InlineKind::TextBox {
                    width_emu: width_emu.max(1),
                    height_emu: height_emu.max(1),
                    story: Box::new(TextBoxStory {
                        fill: Some([255, 255, 255, 255]),
                        outline: Some(ShapeOutline {
                            color: [0, 0, 0, 255],
                            width_emu: 9_525,
                        }),
                        dirty: true,
                        ..TextBoxStory::default()
                    }),
                },
            ));
            if let Some(io) = para
                .inline_objects
                .iter_mut()
                .find(|io| io.at == placed_at && matches!(io.kind, InlineKind::TextBox { .. }))
            {
                io.anchor = Some(Box::new(FloatAnchor {
                    wrap: WrapKind::Square,
                    dist_left_emu: 114_300,
                    dist_right_emu: 114_300,
                    ..FloatAnchor::default()
                }));
            }
        });
        let mut out = Self {
            blocks,
            ..self.clone()
        };
        if let Some(e) = edit {
            out.remap_text_edit_record(&target, e);
        }
        (out, target, placed_at)
    }

    /// Issue #83 — every text box in the body (cells included), as
    /// `(host path, anchor byte)` in document order.
    pub fn text_box_addresses(&self) -> Vec<(BlockPath, u32)> {
        fn walk(blocks: &[Block], prefix: &BlockPath, out: &mut Vec<(BlockPath, u32)>) {
            for (i, b) in blocks.iter().enumerate() {
                let path = prefix.clone().push(PathStep::Block(i as u32));
                match b {
                    Block::Paragraph(p) => {
                        for io in &p.inline_objects {
                            if matches!(io.kind, InlineKind::TextBox { .. }) {
                                out.push((path.clone(), io.at));
                            }
                        }
                    }
                    Block::Table(t) => {
                        for (r, row) in t.rows.iter().enumerate() {
                            for (c, cell) in row.cells.iter().enumerate() {
                                let cp = path.clone().push(PathStep::Cell {
                                    row: r as u32,
                                    col: c as u32,
                                });
                                walk(&cell.blocks, &cp, out);
                            }
                        }
                    }
                }
            }
        }
        let top: Vec<Block> = self.blocks.iter().cloned().collect();
        let mut out = Vec::new();
        walk(&top, &BlockPath::root(), &mut out);
        out
    }

    /// Issue #69 — count the floating (`<wp:anchor>`) images in the body.
    pub fn count_floating_images(&self) -> u32 {
        let mut n = 0u32;
        walk_paragraphs(&self.blocks, &mut |p| {
            for io in &p.inline_objects {
                if io.is_floating() && matches!(io.kind, InlineKind::Image { .. }) {
                    n = n.saturating_add(1);
                }
            }
        });
        n
    }

    /// Sprint 2 (UI Edition) — set `<w:pPr><w:pBdr>` on every
    /// paragraph the range spans. Mirrors [`Self::set_alignment`] but
    /// writes `props.borders`. Pass `None` to clear the borders.
    pub fn set_paragraph_borders(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        borders: Option<CellBorders>,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, |para| {
                    para.props.borders = borders.clone();
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.props.borders = borders.clone();
            });
        }
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }

    /// Delete the logical range `[start, end)`. A range spanning paragraphs
    /// merges the partial first and last paragraphs and drops those between.
    /// PR 4: same-parent cross-paragraph ranges work end-to-end; cross-
    /// container ranges (cell ↔ body) clamp to the `start` paragraph.
    pub fn delete_range(&self, start: LogicalPos, end: LogicalPos) -> Self {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return self.clone();
        }
        if start.path == end.path {
            let mut blocks = self.blocks.clone();
            let mut edit = None;
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                /* Issue #252 — the same snapped gap `delete_text` removes
                (and remaps the source markup over) drives the anchors. */
                let (s, e) = (para.snap_offset(start.offset), para.snap_offset(end.offset));
                if s < e {
                    edit = Some(TextEdit {
                        at: s,
                        removed: e - s,
                        inserted: 0,
                    });
                }
                *para = para.delete_text(start.offset, end.offset);
            });
            let mut out = Self {
                blocks,
                body_section: self.body_section.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnote_stories: self.footnote_stories.clone(),
                endnote_stories: self.endnote_stories.clone(),
                footnote_props: self.footnote_props,
                endnote_props: self.endnote_props,
                notes_dirty: self.notes_dirty.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
                hf_dirty: self.hf_dirty.clone(),
                settings_dirty: self.settings_dirty,
                document_root_attrs: self.document_root_attrs.clone(),
                part_root_attrs: self.part_root_attrs.clone(),
                document_envelope: self.document_envelope.clone(),
                source_package: self.source_package.clone(),
            };
            if let Some(e) = edit {
                out.remap_text_edit_record(&start.path, e);
            }
            return out;
        }
        if !same_parent(&start.path, &end.path) {
            /* Cross-container delete clamps to the start endpoint —
            full cross-container linear semantics land with Phase 5c. */
            let end_in_start_container = LogicalPos {
                path: start.path.clone(),
                offset: self
                    .paragraph_at_path(&start.path)
                    .map(|p| p.text.len() as u32)
                    .unwrap_or(start.offset),
            };
            return self.delete_range(start, end_in_start_container);
        }
        let Some(sp_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(ep_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return self.clone();
        };
        /* Issue #252 — the snapped merge point, for the anchor remap. */
        let snap_in = |idx: u32, off: u32| {
            container
                .get(idx as usize)
                .and_then(|b| b.as_paragraph())
                .map_or(0, |p| p.snap_offset(off))
        };
        let (s_snap, e_snap) = (snap_in(sp_idx, start.offset), snap_in(ep_idx, end.offset));
        let head = container
            .get(sp_idx as usize)
            .and_then(|b| b.as_paragraph())
            .map(|p| p.split_at(start.offset).0)
            .unwrap_or_default();
        let tail = container
            .get(ep_idx as usize)
            .and_then(|b| b.as_paragraph())
            .map(|p| p.split_at(end.offset).1)
            .unwrap_or_default();
        /* Issue #70 (design review B1) — this branch DROPS section
        markers: `sp`'s own marker (its paragraph mark is the one being
        deleted; `tail`-wins concat discards it) and any marker on the
        wholesale-removed middle blocks `sp+1..ep`. `ep`'s marker
        survives on `merged`. A dropped marker may be the ONLY owner of
        header/footer parts that later sections (and the merged section
        itself) resolve through inheritance — losing it would silently
        blank bands document-wide on save. Fold the dropped markers'
        ref slots in document order (later provider wins — exactly what
        the forward fold saw just before the next terminal)… */
        let mut dropped_h = HeaderFooterRefs::default();
        let mut dropped_f = HeaderFooterRefs::default();
        let top_level = start.path.steps.len() == 1;
        if top_level {
            let mut fold = |p: &Paragraph| {
                if let Some(props) = &p.section_end {
                    /* later marker's Some slots override earlier's */
                    let mut h = props.header_refs.clone();
                    let mut f = props.footer_refs.clone();
                    h.inherit_missing_from(&dropped_h);
                    f.inherit_missing_from(&dropped_f);
                    dropped_h = h;
                    dropped_f = f;
                }
            };
            for idx in sp_idx..ep_idx {
                if let Some(p) = container.get(idx as usize).and_then(|b| b.as_paragraph()) {
                    fold(p);
                }
            }
        }
        let merged = head.concat(&tail);
        let mut blocks = self.blocks.clone();
        let parent = start.path.parent();
        /* Drop every block strictly after sp up to and including ep,
        then replace sp with the merged paragraph. */
        for idx in ((sp_idx + 1)..=ep_idx).rev() {
            let child = parent.clone().push(PathStep::Block(idx));
            delete_block_at_path(&mut blocks, &child);
        }
        let sp_path = parent.push(PathStep::Block(sp_idx));
        replace_block_in_top(&mut blocks, &sp_path, Block::Paragraph(merged));
        /* …then backfill them into the covering section's NEW terminal
        (the first marker at/after the merge point — possibly `merged`
        itself — else `body_section`) wherever that terminal's slot is
        None. Downstream resolution is preserved by construction; a
        terminal that OWNS a slot keeps it (Word: the following
        section's own header wins when you delete a break). */
        let mut body_section = self.body_section.clone();
        if top_level && !(dropped_h.is_empty() && dropped_f.is_empty()) {
            let next_terminal =
                blocks
                    .iter()
                    .enumerate()
                    .skip(sp_idx as usize)
                    .find_map(|(i, b)| match b {
                        Block::Paragraph(p) if p.section_end.is_some() => Some(i as u32),
                        _ => None,
                    });
            if let Some(mi) = next_terminal {
                let _ = mutate_paragraph_in_top(&mut blocks, &BlockPath::top(mi), |para| {
                    if let Some(props) = &mut para.section_end {
                        props.header_refs.inherit_missing_from(&dropped_h);
                        props.footer_refs.inherit_missing_from(&dropped_f);
                    }
                });
            } else {
                body_section.header_refs.inherit_missing_from(&dropped_h);
                body_section.footer_refs.inherit_missing_from(&dropped_f);
            }
        }
        let mut merged_doc = Self {
            blocks,
            body_section,
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        /* Issue #252 — the merge removed blocks `sp+1..=ep` and spliced
        `ep`'s tail onto `sp`: anchors follow their text. */
        merged_doc.remap_paragraph_merge(&sp_path, s_snap, ep_idx, e_snap);
        merged_doc.with_list_markers_refreshed()
    }

    /// Split the paragraph at `at`, the break falling between the two halves.
    pub fn split_paragraph(&self, at: LogicalPos) -> Self {
        let count = self.paragraph_count();
        let mut blocks = self.blocks.clone();
        /* Design review B6 — see `insert_text`: a table-only tree has
        cell paragraphs the deep path reaches. */
        if count == 0 && self.path_to_first_paragraph_deep().is_none() {
            blocks.push_back(Block::Paragraph(Paragraph::default()));
            blocks.push_back(Block::Paragraph(Paragraph::default()));
            return Self {
                blocks,
                body_section: self.body_section.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnote_stories: self.footnote_stories.clone(),
                endnote_stories: self.endnote_stories.clone(),
                footnote_props: self.footnote_props,
                endnote_props: self.endnote_props,
                notes_dirty: self.notes_dirty.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
                hf_dirty: self.hf_dirty.clone(),
                settings_dirty: self.settings_dirty,
                document_root_attrs: self.document_root_attrs.clone(),
                part_root_attrs: self.part_root_attrs.clone(),
                document_envelope: self.document_envelope.clone(),
                source_package: self.source_package.clone(),
            };
        }
        let Some(p) = self.paragraph_at_path(&at.path) else {
            return self.clone();
        };
        let (left, mut right) = p.split_at(at.offset);
        /* Issue #277 — Word's "next style" rule: Enter at the very END
        of a paragraph gives the NEW paragraph its style's `<w:next>`
        (Heading 1 → Normal); a split anywhere else keeps the style on
        both halves (`split_at`). An unknown / absent next keeps the
        same style. The direct paragraph formatting and the list binding
        survive the switch, exactly as `set_paragraph_style` keeps them. */
        if p.snap_offset(at.offset) as usize == p.text.len()
            && let Some(next) = self.next_style_after(right.style_id.as_deref())
        {
            right.style_id = Some(next);
            recompute_paragraph_props(&mut right, &self.styles, &self.style_defaults);
        }
        replace_block_in_top(&mut blocks, &at.path, Block::Paragraph(left));
        insert_block_after_path_in_top(&mut blocks, &at.path, Block::Paragraph(right));
        let mut split = Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        /* Issue #152 — the right half is a new block: comment anchors
        behind the split point (and in every later block) follow it. */
        split.remap_paragraph_split(&at.path, p.snap_offset(at.offset));
        split.with_list_markers_refreshed()
    }

    /// Insert `text` at `at`, splitting it into separate paragraphs on every
    /// newline — `\r\n` and bare `\r` are normalized to `\n` first. A `text`
    /// with no newline behaves exactly like [`DocumentTree::insert_text`].
    /// Returns the new tree and the caret position at the end of the last
    /// inserted line (Backlog #12, multi-line paste).
    pub fn insert_multiline(&self, at: LogicalPos, text: &str) -> (Self, LogicalPos) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        let mut doc = self.clone();
        let mut cur = at;
        for (i, line) in lines.iter().enumerate() {
            doc = doc.insert_text(cur.clone(), line);
            let after = LogicalPos {
                path: cur.path.clone(),
                offset: cur.offset + line.len() as u32,
            };
            if i + 1 < lines.len() {
                /* A newline follows this line — break the paragraph so the
                next line lands in a fresh one; the remainder of the original
                paragraph rides along on the tail. */
                doc = doc.split_paragraph(after.clone());
                /* Advance the path to the inserted sibling — its last
                Block step bumps by 1; other steps unchanged. */
                cur = LogicalPos {
                    path: bump_last_block_index(&cur.path),
                    offset: 0,
                };
            } else {
                cur = after;
            }
        }
        (doc, cur)
    }

    /// Extract the logical range `[start, end)` as standalone paragraphs,
    /// style spans clipped and shifted to local offsets. Drives rich
    /// clipboard copy — HTML + `.docx`-fragment generation (Backlog #12).
    /// **Tables in the spanned range are silently dropped** —
    /// clipboard fragments stay paragraph-only. Cross-container ranges
    /// clamp to the start endpoint's container until Phase 5c.
    pub fn slice(&self, start: LogicalPos, end: LogicalPos) -> Vec<Paragraph> {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return Vec::new();
        }
        if start.path == end.path {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            let head = p.split_at(end.offset).0;
            return vec![strip_section_marker(head.split_at(start.offset).1)];
        }
        if !same_parent(&start.path, &end.path) {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            return vec![strip_section_marker(p.split_at(start.offset).1)];
        }
        let Some(sp_idx) = start.path.last_block_index() else {
            return Vec::new();
        };
        let Some(ep_idx) = end.path.last_block_index() else {
            return Vec::new();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return Vec::new();
        };
        let mut out: Vec<Paragraph> = Vec::with_capacity((ep_idx - sp_idx + 1) as usize);
        if let Some(p) = container
            .get(sp_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(strip_section_marker(p.split_at(start.offset).1));
        }
        for idx in (sp_idx + 1)..ep_idx {
            if let Some(p) = container.get(idx as usize).and_then(|b| b.as_paragraph()) {
                out.push(strip_section_marker(p.clone()));
            }
        }
        if let Some(p) = container
            .get(ep_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(p.split_at(end.offset).0);
        }
        out
    }

    /// Insert pre-styled `paras` at `at`; returns the new tree and the caret
    /// at the end of the inserted content. The caller deletes any active
    /// selection first. Drives HTML paste (Backlog #12).
    ///
    /// Phase 3 (#40) — inputs are marker-stripped defensively: no paste
    /// path may ever transplant a section break, regardless of which
    /// producer built the fragment.
    pub fn insert_rich(&self, at: LogicalPos, paras: &[Paragraph]) -> (Self, LogicalPos) {
        let (mut out, caret) = self.insert_rich_unmapped(at.clone(), paras);
        /* Issue #252 — comment anchors follow the text around the paste:
        the target splits at the paste point, the fragment's middle
        paragraphs land between the halves, and its first / last
        paragraphs are spliced onto the head / tail (the source markup
        rode `split_at` / `concat`). */
        let Some((target, at_snap)) = self.rich_paste_target(&at) else {
            return (out, caret);
        };
        match paras {
            [] => {}
            [only] => out.remap_text_edit(&target, at_snap, 0, only.text.len() as u32),
            [first, .., last] => {
                let n = paras.len() as u32;
                out.remap_paragraph_split(&target, at_snap);
                let (container, idx) = split_block_path(&target);
                out.remap_block_splice(&container, idx + 1, 0, n - 2);
                let tail = block_path_in(&container, idx + n - 1);
                out.remap_text_edit(&tail, 0, 0, last.text.len() as u32);
                out.remap_text_edit(&target, at_snap, 0, first.text.len() as u32);
            }
        }
        (out, caret)
    }

    /// Issue #252 — the paragraph a rich paste at `at` lands in and the
    /// snapped paste offset (the same resolution `insert_rich_unmapped`
    /// applies); `None` when no paragraph is addressable.
    fn rich_paste_target(&self, at: &LogicalPos) -> Option<(BlockPath, u32)> {
        let target = if self.paragraph_at_path(&at.path).is_some() {
            at.path.clone()
        } else {
            self.path_to_last_top_paragraph()?
        };
        let p = self.paragraph_at_path(&target)?;
        Some((target.clone(), p.snap_offset(at.offset)))
    }

    fn insert_rich_unmapped(&self, at: LogicalPos, paras: &[Paragraph]) -> (Self, LogicalPos) {
        if paras.is_empty() {
            return (self.clone(), at.clone());
        }
        let paras: Vec<Paragraph> = paras
            .iter()
            .map(|p| strip_section_marker(p.clone()))
            .collect();
        let paras: &[Paragraph] = &paras;
        let mut blocks = self.blocks.clone();
        if self.paragraph_count() == 0 {
            blocks.push_back(Block::Paragraph(Paragraph::default()));
        }
        let target_path = if self.paragraph_at_path(&at.path).is_some() {
            at.path.clone()
        } else {
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let Some(target_para) = self
            .paragraph_at_path(&target_path)
            .cloned()
            .or_else(|| Some(Paragraph::default()))
        else {
            return (self.clone(), at.clone());
        };
        let (head, tail) = target_para.split_at(at.offset);
        if paras.len() == 1 {
            let caret = LogicalPos {
                path: target_path.clone(),
                offset: (head.text.len() + paras[0].text.len()) as u32,
            };
            replace_block_in_top(
                &mut blocks,
                &target_path,
                Block::Paragraph(head.concat(&paras[0]).concat(&tail)),
            );
            return (
                Self {
                    blocks,
                    body_section: self.body_section.clone(),
                    headers: self.headers.clone(),
                    footers: self.footers.clone(),
                    media: self.media.clone(),
                    footnote_stories: self.footnote_stories.clone(),
                    endnote_stories: self.endnote_stories.clone(),
                    footnote_props: self.footnote_props,
                    endnote_props: self.endnote_props,
                    notes_dirty: self.notes_dirty.clone(),
                    comment_defs: self.comment_defs.clone(),
                    comment_ranges: self.comment_ranges.clone(),
                    settings: self.settings.clone(),
                    styles: self.styles.clone(),
                    style_defaults: self.style_defaults.clone(),
                    style_run_defaults: self.style_run_defaults.clone(),
                    styles_dirty: self.styles_dirty,
                    numbering: self.numbering.clone(),
                    hf_dirty: self.hf_dirty.clone(),
                    settings_dirty: self.settings_dirty,
                    document_root_attrs: self.document_root_attrs.clone(),
                    part_root_attrs: self.part_root_attrs.clone(),
                    document_envelope: self.document_envelope.clone(),
                    source_package: self.source_package.clone(),
                }
                .with_list_markers_refreshed(),
                caret,
            );
        }
        let lastp = &paras[paras.len() - 1];
        replace_block_in_top(
            &mut blocks,
            &target_path,
            Block::Paragraph(head.concat(&paras[0])),
        );
        let mut last_path = target_path.clone();
        for p in &paras[1..paras.len() - 1] {
            insert_block_after_path_in_top(&mut blocks, &last_path, Block::Paragraph(p.clone()));
            last_path = bump_last_block_index(&last_path);
        }
        insert_block_after_path_in_top(
            &mut blocks,
            &last_path,
            Block::Paragraph(lastp.concat(&tail)),
        );
        let final_path = bump_last_block_index(&last_path);
        let caret = LogicalPos {
            path: final_path,
            offset: lastp.text.len() as u32,
        };
        (
            Self {
                blocks,
                body_section: self.body_section.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnote_stories: self.footnote_stories.clone(),
                endnote_stories: self.endnote_stories.clone(),
                footnote_props: self.footnote_props,
                endnote_props: self.endnote_props,
                notes_dirty: self.notes_dirty.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
                hf_dirty: self.hf_dirty.clone(),
                settings_dirty: self.settings_dirty,
                document_root_attrs: self.document_root_attrs.clone(),
                part_root_attrs: self.part_root_attrs.clone(),
                document_envelope: self.document_envelope.clone(),
                source_package: self.source_package.clone(),
            }
            .with_list_markers_refreshed(),
            caret,
        )
    }

    /// Like [`Self::slice`] but preserves the top-level block sequence —
    /// tables that fall between the start and end paragraphs survive
    /// the slice as `Block::Table` entries, so the rich clipboard can
    /// round-trip a selection that crosses a table without dropping
    /// the cell content. The first and last paragraphs are clipped to
    /// their respective endpoint offsets; tables between them are
    /// cloned verbatim. Cross-container ranges (cell ↔ body) fall back
    /// to the start paragraph's tail only — full cross-container
    /// slicing lands with §IV.6 of UX_BEHAVIOR_SPEC.
    pub fn slice_blocks(&self, start: LogicalPos, end: LogicalPos) -> Vec<Block> {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return Vec::new();
        }
        if start.path == end.path {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            let head = p.split_at(end.offset).0;
            return vec![Block::Paragraph(strip_section_marker(
                head.split_at(start.offset).1,
            ))];
        }
        if !same_parent(&start.path, &end.path) {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            return vec![Block::Paragraph(strip_section_marker(
                p.split_at(start.offset).1,
            ))];
        }
        let Some(sp_idx) = start.path.last_block_index() else {
            return Vec::new();
        };
        let Some(ep_idx) = end.path.last_block_index() else {
            return Vec::new();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return Vec::new();
        };
        let mut out: Vec<Block> = Vec::with_capacity((ep_idx - sp_idx + 1) as usize);
        if let Some(p) = container
            .get(sp_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(Block::Paragraph(strip_section_marker(
                p.split_at(start.offset).1,
            )));
        }
        for idx in (sp_idx + 1)..ep_idx {
            if let Some(b) = container.get(idx as usize) {
                out.push(match b {
                    Block::Paragraph(p) => Block::Paragraph(strip_section_marker(p.clone())),
                    other => other.clone(),
                });
            }
        }
        if let Some(p) = container
            .get(ep_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(Block::Paragraph(p.split_at(end.offset).0));
        }
        out
    }

    /// Like [`Self::insert_rich`] but accepts pre-styled blocks — the
    /// rich clipboard's table-and-paragraph payload. Splits the target
    /// paragraph at `at`, splices the head + first input block, then
    /// inserts the middle blocks (tables and paragraphs alike), then
    /// the last input block + the target's tail. Returns the new tree
    /// and the caret at the end of the inserted content.
    pub fn insert_rich_blocks(&self, at: LogicalPos, blocks_in: &[Block]) -> (Self, LogicalPos) {
        /* The all-paragraph shape delegates to `insert_rich`, which
        remaps the anchors itself. */
        if blocks_in.iter().all(|b| matches!(b, Block::Paragraph(_))) {
            return self.insert_rich_blocks_unmapped(at, blocks_in);
        }
        let (mut out, caret) = self.insert_rich_blocks_unmapped(at.clone(), blocks_in);
        /* Issue #252 — see `insert_rich`: split at the paste point, the
        blocks between the head and the tail are inserted, a leading /
        trailing fragment paragraph is spliced onto the head / tail. */
        let Some((target, at_snap)) = self.rich_paste_target(&at) else {
            return (out, caret);
        };
        let n = blocks_in.len() as u32;
        let first_para = blocks_in.first().and_then(Block::as_paragraph);
        let last_para = if n >= 2 {
            blocks_in.last().and_then(Block::as_paragraph)
        } else {
            None
        };
        let between = u32::from(first_para.is_none())
            + n.saturating_sub(2)
            + u32::from(n >= 2 && last_para.is_none());
        out.remap_paragraph_split(&target, at_snap);
        let (container, idx) = split_block_path(&target);
        out.remap_block_splice(&container, idx + 1, 0, between);
        if let Some(last) = last_para {
            let tail = block_path_in(&container, idx + 1 + between);
            out.remap_text_edit(&tail, 0, 0, last.text.len() as u32);
        }
        if let Some(first) = first_para {
            out.remap_text_edit(&target, at_snap, 0, first.text.len() as u32);
        }
        (out, caret)
    }

    fn insert_rich_blocks_unmapped(
        &self,
        at: LogicalPos,
        blocks_in: &[Block],
    ) -> (Self, LogicalPos) {
        if blocks_in.is_empty() {
            return (self.clone(), at.clone());
        }
        /* Phase 3 (#40) — same defensive marker strip as `insert_rich`:
        no paste path may transplant a section break. */
        let blocks_in: Vec<Block> = blocks_in
            .iter()
            .map(|b| match b {
                Block::Paragraph(p) => Block::Paragraph(strip_section_marker(p.clone())),
                other => other.clone(),
            })
            .collect();
        let blocks_in: &[Block] = &blocks_in;
        /* When the input is all paragraphs, defer to the existing
        paragraph-only insert — it merges head+first and last+tail
        intra-paragraph so consecutive `\n`-joined paragraphs splice
        with no inter-paragraph break. */
        if blocks_in.iter().all(|b| matches!(b, Block::Paragraph(_))) {
            let paras: Vec<Paragraph> = blocks_in
                .iter()
                .filter_map(|b| b.as_paragraph().cloned())
                .collect();
            return self.insert_rich(at, &paras);
        }
        let mut blocks = self.blocks.clone();
        if self.paragraph_count() == 0 {
            blocks.push_back(Block::Paragraph(Paragraph::default()));
        }
        let target_path = if self.paragraph_at_path(&at.path).is_some() {
            at.path.clone()
        } else {
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let Some(target_para) = self.paragraph_at_path(&target_path).cloned() else {
            return (self.clone(), at.clone());
        };
        let (head, tail) = target_para.split_at(at.offset);
        /* Replace the target paragraph with the head + first input
        block (merged inline if that first block is a paragraph; else
        head stays its own paragraph and the input block follows). */
        let (first_replace, first_is_para) = match &blocks_in[0] {
            Block::Paragraph(p) => (Block::Paragraph(head.concat(p)), true),
            _ => (Block::Paragraph(head), false),
        };
        replace_block_in_top(&mut blocks, &target_path, first_replace);
        let mut after_path = target_path.clone();
        if !first_is_para {
            insert_block_after_path_in_top(&mut blocks, &after_path, blocks_in[0].clone());
            after_path = bump_last_block_index(&after_path);
        }
        /* Middle blocks (everything between the first and last input
        block) splice in verbatim. */
        let last_idx = blocks_in.len() - 1;
        for b in blocks_in.get(1..last_idx).unwrap_or(&[]) {
            insert_block_after_path_in_top(&mut blocks, &after_path, b.clone());
            after_path = bump_last_block_index(&after_path);
        }
        /* Last input block + the target's tail. When both are
        paragraphs, merge them inline (the typical Word paste shape).
        When the last input is a table, the tail becomes its own
        trailing paragraph below the table. */
        let (last_block, caret_offset) = match (blocks_in.get(last_idx), last_idx == 0) {
            (Some(Block::Paragraph(p)), true) => {
                /* Single-input-paragraph case is handled by the
                paragraph fast-path above; this branch is unreachable
                in practice but stays for completeness. */
                (Block::Paragraph(p.concat(&tail)), p.text.len() as u32)
            }
            (Some(Block::Paragraph(p)), false) => {
                let offset = p.text.len() as u32;
                (Block::Paragraph(p.concat(&tail)), offset)
            }
            (Some(Block::Table(t)), false) => {
                /* Splice the table, then append the tail as a fresh
                paragraph BELOW it so the caret has a logical home. */
                insert_block_after_path_in_top(&mut blocks, &after_path, Block::Table(t.clone()));
                after_path = bump_last_block_index(&after_path);
                (Block::Paragraph(tail.clone()), 0)
            }
            _ => (Block::Paragraph(tail.clone()), 0),
        };
        /* Splice the final block. When the last input block was a
        table, `last_block` is the synthesised trailing paragraph the
        match above produced and `after_path` already points to that
        table; otherwise `after_path` still points to the merged
        head+first paragraph (or the last middle block) and we
        append the merged last+tail behind it. */
        insert_block_after_path_in_top(&mut blocks, &after_path, last_block);
        after_path = bump_last_block_index(&after_path);
        let caret = LogicalPos {
            path: after_path,
            offset: caret_offset,
        };
        (
            Self {
                blocks,
                body_section: self.body_section.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnote_stories: self.footnote_stories.clone(),
                endnote_stories: self.endnote_stories.clone(),
                footnote_props: self.footnote_props,
                endnote_props: self.endnote_props,
                notes_dirty: self.notes_dirty.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
                hf_dirty: self.hf_dirty.clone(),
                settings_dirty: self.settings_dirty,
                document_root_attrs: self.document_root_attrs.clone(),
                part_root_attrs: self.part_root_attrs.clone(),
                document_envelope: self.document_envelope.clone(),
                source_package: self.source_package.clone(),
            }
            .with_list_markers_refreshed(),
            caret,
        )
    }

    /// Extract the text of the logical range `[start, end)`. Paragraphs the
    /// range spans are joined by `\n`. Used for clipboard copy.
    pub fn text_range(&self, start: LogicalPos, end: LogicalPos) -> String {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return String::new();
        }
        if start.path == end.path {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return String::new();
            };
            let lo = p.snap_offset(start.offset) as usize;
            let hi = p.snap_offset(end.offset) as usize;
            if lo >= hi {
                return String::new();
            }
            return p.text[lo..hi].to_string();
        }
        if !same_parent(&start.path, &end.path) {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return String::new();
            };
            let lo = p.snap_offset(start.offset) as usize;
            return p.text[lo..].to_string();
        }
        let Some(sp_idx) = start.path.last_block_index() else {
            return String::new();
        };
        let Some(ep_idx) = end.path.last_block_index() else {
            return String::new();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return String::new();
        };
        let mut out = String::new();
        for idx in sp_idx..=ep_idx {
            let Some(para) = container.get(idx as usize).and_then(|b| b.as_paragraph()) else {
                continue;
            };
            let len = para.text.len();
            let lo = if idx == sp_idx {
                para.snap_offset(start.offset) as usize
            } else {
                0
            };
            let hi = if idx == ep_idx {
                para.snap_offset(end.offset) as usize
            } else {
                len
            };
            if idx > sp_idx {
                out.push('\n');
            }
            if lo < hi {
                out.push_str(&para.text[lo..hi]);
            }
        }
        out
    }

    /* ============================================================
    Phase 5 PR 3 — table mutation commands.
    Every command flips `Table.dirty = true` + drops
    `source_xml`, so the writer regenerates the table from rows
    instead of emitting the captured passthrough bytes.
    ============================================================ */

    /// Insert an empty `rows × cols` table at the *block position* given
    /// by `at` (top-level path only — nested-cell insertion in 5b). The
    /// new table sits at top-level block index `at.steps[0]`; the
    /// existing block at that index slides down by one.
    pub fn try_insert_table(
        &self,
        at: BlockPath,
        rows: u32,
        cols: u32,
    ) -> Result<Self, TableError> {
        check_table_dims(rows, cols)?;
        Ok(self.insert_table(at, rows, cols))
    }

    /// Infallible sibling of [`Self::try_insert_table`] for internal
    /// callers (fixtures, writer tests). Issue #114 — the dimensions are
    /// clamped into the caps, so this can never allocate from an
    /// unbounded wire value either; the command boundary uses the
    /// checked variant so the shell sees a typed error instead of a
    /// silently smaller table.
    pub fn insert_table(&self, at: BlockPath, rows: u32, cols: u32) -> Self {
        let idx = top_level_block_index(&at).unwrap_or(self.blocks.len() as u32);
        let cols_u32 = cols.clamp(1, MAX_TABLE_COLS);
        let rows = rows
            .clamp(1, MAX_TABLE_ROWS)
            .min((MAX_TABLE_CELLS / cols_u32 as u64) as u32)
            .max(1);
        let cols = cols_u32 as usize;
        /* Default column width — evenly divide A4 content width
        (9020 twips ≈ 6.26 in) so a fresh table fits the page on
        insert. Phase 5c will switch to `<w:tblLayout w:type="autofit"/>`
        once auto-fit lands; until then a literal grid that matches
        the page is the pragmatic default. */
        let per_col = (DEFAULT_A4_CONTENT_TWIPS / cols as i32).max(720);
        let grid: Vec<i32> = vec![per_col; cols];
        let mut row_vec: Vec<TableRow> = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            let mut cells = Vec::with_capacity(cols);
            for _ in 0..cols {
                cells.push(default_table_cell());
            }
            row_vec.push(TableRow {
                props: RowProperties::default(),
                cells,
                source_markup: None,
            });
        }
        let table = Table {
            grid,
            /* Word-style default outer borders — 0.5 pt single black on
            every edge so a freshly inserted table is visible without
            the user opening the borders panel. */
            props: TableProperties {
                borders: Some(default_word_borders()),
                ..TableProperties::default()
            },
            rows: row_vec,
            /* Engine-synthesised — no source bytes, fully regenerated on
            save. */
            dirty: true,
            source_xml: None,
            body_xml: None,
            source_markup: None,
        };
        let mut blocks = self.blocks.clone();
        let insert_at = (idx as usize).min(blocks.len());
        blocks.insert(insert_at, Block::Table(table));
        /* OOXML mandates a `<w:p>` after every `<w:tbl>` boundary
        (the body's last child must be a paragraph). Beyond spec
        compliance, the trailing paragraph is the caret's escape
        hatch — without one, pressing Down at the bottom row has
        nowhere to go and traps the caret inside the table. Splice
        one in unless the next block is already a Paragraph. */
        let needs_trailing = blocks
            .get(insert_at + 1)
            .is_none_or(|b| !matches!(b, Block::Paragraph(_)));
        if needs_trailing {
            blocks.insert(insert_at + 1, Block::Paragraph(Paragraph::default()));
        }
        let mut out = Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        /* Issue #152 — the table (+ its escape paragraph) slid every
        later block down: keep comment anchors on their paragraphs. */
        out.remap_block_indices(insert_at as u32, 1 + i64::from(needs_trailing));
        out
    }

    /// Delete the table at `at.steps[0]` (top-level only at PR 3).
    pub fn delete_table(&self, at: BlockPath) -> Self {
        let idx = match top_level_block_index(&at) {
            Some(i) => i as usize,
            None => return self.clone(),
        };
        let mut blocks = self.blocks.clone();
        let removed = idx < blocks.len() && matches!(blocks[idx], Block::Table(_));
        if removed {
            blocks.remove(idx);
        }
        let mut out = Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        };
        /* Issue #152 — later blocks slid up by one. */
        if removed {
            out.remap_block_indices(idx as u32, -1);
        }
        out
    }

    /// Insert a fresh row at `at` (`at` is the index the new row will
    /// occupy; existing rows at that index and below shift down). When
    /// `at >= row_count` the row is appended. Cell count matches the
    /// existing rows' cell count.
    ///
    /// Sprint 2 (UI Edition) hotfix: caller chooses Before vs After
    /// at the bridge boundary (see [`bridge::InsertSide`]); the engine
    /// receives a single resolved insert position in `usize` and
    /// performs no signed arithmetic of its own.
    pub fn insert_row(&self, table_path: BlockPath, at: usize) -> Self {
        /* Issue #253 — rows at/after the insert point move down one. */
        let remap = self.mutated_table_shape(&table_path).map(|(tp, t)| {
            let insert_at = at.min(t.rows.len()) as u32;
            (tp, insert_at)
        });
        let mut out = self.insert_row_unmapped(table_path, at);
        if let Some((tp, insert_at)) = remap {
            out.remap_table_cells(&tp, |row, col| {
                if row >= insert_at {
                    CellMove::To { row: row + 1, col }
                } else {
                    CellMove::Keep
                }
            });
        }
        out
    }

    fn insert_row_unmapped(&self, table_path: BlockPath, at: usize) -> Self {
        self.mutate_table(table_path, |t| {
            /* Prefer the grid width: a merged first row has FEWER cells
            than the table has logical columns, and a fresh row must
            always come in unmerged at full width (Word behaviour). */
            let cols = if t.grid.is_empty() {
                t.rows.first().map(|r| r.cells.len()).unwrap_or(1)
            } else {
                t.grid.len()
            };
            let new_row = TableRow {
                props: RowProperties::default(),
                cells: (0..cols).map(|_| default_table_cell()).collect(),
                source_markup: None,
            };
            let insert_at = at.min(t.rows.len());
            t.rows.insert(insert_at, new_row);
        })
    }

    pub fn delete_row(&self, table_path: BlockPath, row: u32) -> Self {
        /* Issue #253 — anchors in the deleted row move to the start of the
        neighbouring row's first cell (the row below, else the one above);
        rows below shift up. */
        let remap = self
            .mutated_table_shape(&table_path)
            .filter(|(_, t)| (row as usize) < t.rows.len())
            .map(|(tp, t)| (tp, t.rows.len() as u32));
        let out = self.mutate_table(table_path, |t| {
            let i = row as usize;
            if i < t.rows.len() {
                t.rows.remove(i);
            }
        });
        let Some((tp, rows_before)) = remap else {
            return out;
        };
        let mut out = out;
        out.remap_table_cells(&tp, |r, col| {
            if r < row {
                CellMove::Keep
            } else if r > row {
                CellMove::To { row: r - 1, col }
            } else if row + 1 < rows_before {
                CellMove::Collapse {
                    row,
                    col: 0,
                    at_end: false,
                }
            } else {
                /* The last row went: the row above (`row - 1`); with no
                row left at all the collapse falls back past the table. */
                CellMove::Collapse {
                    row: row.saturating_sub(1),
                    col: 0,
                    at_end: row == 0,
                }
            }
        });
        out
    }

    /// Issue #253 — the top-level table `mutate_table` would restructure
    /// for `table_path` (it addresses the table by the path's FIRST block
    /// step), with its path. `None` when the command is a no-op.
    fn mutated_table_shape(&self, table_path: &BlockPath) -> Option<(BlockPath, &Table)> {
        let idx = top_level_block_index(table_path)?;
        let tp = BlockPath::top(idx);
        let t = self.blocks.get(idx as usize)?.as_table()?;
        Some((tp, t))
    }

    /// Insert a column at `at` (new column occupies that index; rows
    /// to the right shift). When `at >= column_count` the column is
    /// appended. Sprint 2 (UI Edition) hotfix — same rationale as
    /// [`Self::insert_row`].
    pub fn insert_column(&self, table_path: BlockPath, at: usize) -> Self {
        /* Issue #253 — per row, cells at/after the row's insert index
        (mirrors the clamp below) move right one. */
        let remap = self.mutated_table_shape(&table_path).map(|(tp, t)| {
            let insert_at = at.min(t.grid.len());
            let per_row: Vec<u32> = t
                .rows
                .iter()
                .map(|r| insert_at.min(r.cells.len()) as u32)
                .collect();
            (tp, per_row)
        });
        let mut out = self.insert_column_unmapped(table_path, at);
        if let Some((tp, per_row)) = remap {
            out.remap_table_cells(&tp, |row, col| match per_row.get(row as usize) {
                Some(&cell_at) if col >= cell_at => CellMove::To { row, col: col + 1 },
                _ => CellMove::Keep,
            });
        }
        out
    }

    fn insert_column_unmapped(&self, table_path: BlockPath, at: usize) -> Self {
        self.mutate_table(table_path, |t| {
            let insert_at = at.min(t.grid.len());
            /* Re-divide the A4 content width across the new column
            count so an inserted column shrinks the existing ones
            instead of pushing the table past the right margin. */
            let new_cols = t.grid.len() + 1;
            let per_col = (DEFAULT_A4_CONTENT_TWIPS / new_cols.max(1) as i32).max(720);
            t.grid.insert(insert_at, per_col);
            for w in t.grid.iter_mut() {
                *w = per_col;
            }
            for row in &mut t.rows {
                let cell_at = insert_at.min(row.cells.len());
                row.cells.insert(cell_at, default_table_cell());
            }
        })
    }

    pub fn delete_column(&self, table_path: BlockPath, col: u32) -> Self {
        /* Issue #253 — per row: anchors in the deleted cell move to the
        start of the cell that slides into its place (else the end of the
        cell to its left); cells to the right shift left. */
        let remap = self.mutated_table_shape(&table_path).map(|(tp, t)| {
            let lens: Vec<u32> = t.rows.iter().map(|r| r.cells.len() as u32).collect();
            (tp, lens)
        });
        let mut out = self.delete_column_unmapped(table_path, col);
        if let Some((tp, lens)) = remap {
            out.remap_table_cells(&tp, |row, c| {
                let len = lens.get(row as usize).copied().unwrap_or(0);
                if col >= len || c < col {
                    CellMove::Keep
                } else if c > col {
                    CellMove::To { row, col: c - 1 }
                } else if col + 1 < len {
                    CellMove::Collapse {
                        row,
                        col,
                        at_end: false,
                    }
                } else {
                    CellMove::Collapse {
                        row,
                        col: col.saturating_sub(1),
                        at_end: true,
                    }
                }
            });
        }
        out
    }

    fn delete_column_unmapped(&self, table_path: BlockPath, col: u32) -> Self {
        self.mutate_table(table_path, |t| {
            let c = col as usize;
            if c < t.grid.len() {
                t.grid.remove(c);
            }
            for row in &mut t.rows {
                if c < row.cells.len() {
                    row.cells.remove(c);
                }
            }
        })
    }

    /// Merge the rectangle of cells `(from_row, from_col)..=(to_row,
    /// to_col)` inside the table at `table_path`. The top-left cell
    /// becomes the visual owner: its `grid_span` widens to cover the
    /// column range, every cell directly below in the column range
    /// becomes `VMergeRole::Continue`. Horizontal partners (same row,
    /// columns to the right) are *removed*, their widths summed into the
    /// owner's `grid_span`; a continuation row's own cell is emptied to a
    /// single blank paragraph. Issue #263 — none of that content is
    /// dropped: every merged-away cell's blocks are appended into the
    /// owner, in row-major order (Word's behaviour), by
    /// [`Self::merge_cells_unmapped`].
    pub fn merge_cells(
        &self,
        table_path: BlockPath,
        from_row: u32,
        from_col: u32,
        to_row: u32,
        to_col: u32,
    ) -> Self {
        let (r0, r1) = if from_row <= to_row {
            (from_row, to_row)
        } else {
            (to_row, from_row)
        };
        let (c0, c1) = if from_col <= to_col {
            (from_col, to_col)
        } else {
            (to_col, from_col)
        };
        /* Issue #253 / #263 — mirror the merge on the PRE-mutation shape:
        per affected row, the cells `c0 + 1 ..= c0 + drop` are removed and
        (issue #263) a continuation row's own `c0` cell is also emptied —
        every one of those cells' blocks lands, whole, inside the owner
        `(r0, c0)`. `absorbed` records exactly where: `cursor` walks the
        SAME row-major order `merge_cells_unmapped` appends in (row r0's
        horizontal partners, then each continuation row's own cell
        followed by its horizontal partners), so an anchor at block `k` of
        an absorbed cell lands on block `cursor + k` of the owner. Cells
        past the merged range shift left by `drop`. */
        let remap = self.mutated_table_shape(&table_path).and_then(|(tp, t)| {
            let rcount = t.rows.len() as u32;
            let owner_row = t.rows.get(r0 as usize)?;
            let last = (owner_row.cells.len() as u32).checked_sub(1)?;
            if c0 > last {
                return None;
            }
            let mut cursor = owner_row
                .cells
                .get(c0 as usize)
                .map_or(0, |c| c.blocks.len() as u32);
            let mut absorbed: Vec<(u32, u32, u32)> = Vec::new();
            /* (row, cells dropped) for the owner row and each continuation. */
            let mut drops = Vec::new();
            let c1_r0 = c1.min(last);
            for c in (c0 + 1)..=c1_r0 {
                let len = owner_row
                    .cells
                    .get(c as usize)
                    .map_or(0, |cell| cell.blocks.len() as u32);
                absorbed.push((r0, c, cursor));
                cursor += len;
            }
            drops.push((r0, c1_r0 - c0));
            for r in (r0 + 1)..=r1.min(rcount.saturating_sub(1)) {
                let row = &t.rows[r as usize];
                let len = row.cells.len() as u32;
                if c0 >= len {
                    continue;
                }
                let own_len = row.cells[c0 as usize].blocks.len() as u32;
                absorbed.push((r, c0, cursor));
                cursor += own_len;
                let c1_r = c1.min(len - 1);
                for c in (c0 + 1)..=c1_r {
                    let clen = row.cells[c as usize].blocks.len() as u32;
                    absorbed.push((r, c, cursor));
                    cursor += clen;
                }
                drops.push((r, c1_r - c0));
            }
            Some((tp, drops, absorbed))
        });
        let mut out = self.merge_cells_unmapped(table_path, r0, r1, c0, c1);
        if let Some((tp, drops, absorbed)) = remap {
            out.remap_table_cells(&tp, |row, col| {
                let Some(&(_, drop)) = drops.iter().find(|(r, _)| *r == row) else {
                    return CellMove::Keep;
                };
                let owner = row == r0 && col == c0;
                if col < c0 || owner {
                    CellMove::Keep
                } else if col <= c0 + drop {
                    match absorbed.iter().find(|(r, c, _)| *r == row && *c == col) {
                        Some(&(_, _, block_offset)) => CellMove::Absorbed {
                            row: r0,
                            col: c0,
                            block_offset,
                        },
                        /* Defensive fallback; every merged-away cell the
                        classification reaches this branch for is also in
                        `absorbed` by construction. */
                        None => CellMove::Collapse {
                            row: r0,
                            col: c0,
                            at_end: true,
                        },
                    }
                } else {
                    CellMove::To {
                        row,
                        col: col - drop,
                    }
                }
            });
        }
        out
    }

    /// Issue #263 — physically restructure the table AND carry every
    /// merged-away cell's blocks into the owner `(r0, c0)`, row-major
    /// (owner row's horizontal partners, left to right, then each
    /// continuation row's own cell followed by ITS horizontal partners) —
    /// [`Self::merge_cells`]'s `absorbed` list mirrors this exact order so
    /// a comment anchor lands on the same paragraph its text moved to.
    fn merge_cells_unmapped(
        &self,
        table_path: BlockPath,
        r0: u32,
        r1: u32,
        c0: u32,
        c1: u32,
    ) -> Self {
        self.mutate_table(table_path, |t| {
            let rcount = t.rows.len() as u32;
            if r0 >= rcount {
                return;
            }
            let span = (c1 - c0 + 1).min(u8::MAX as u32) as u8;
            /* Horizontal collapse: top-row cells in the rectangle's
            column range merge into one cell with `grid_span = span`. */
            let Some(top_row) = t.rows.get_mut(r0 as usize) else {
                return;
            };
            /* Issue #116 — a cell-less row (or `c0` past the row's last
            cell) used to underflow `len - 1 - c0`; nothing to merge. */
            let Some(last) = (top_row.cells.len() as u32).checked_sub(1) else {
                return;
            };
            if c0 > last {
                return;
            }
            let drop_count = (c1.min(last) - c0) as usize;
            top_row.cells[c0 as usize].props.grid_span = span;
            top_row.cells[c0 as usize].props.v_merge = if r0 == r1 {
                VMergeRole::None
            } else {
                VMergeRole::Restart
            };
            /* Issue #263 — Word appends every merged-away cell's blocks
            into the owner instead of discarding them. `remove` always
            takes whatever now sits right after the owner, so this loop
            already visits the horizontal partners left to right. */
            let mut appended: Vec<Block> = Vec::new();
            for _ in 0..drop_count {
                if (c0 as usize + 1) < top_row.cells.len() {
                    appended.extend(top_row.cells.remove(c0 as usize + 1).blocks);
                }
            }
            top_row.cells[c0 as usize].blocks.extend(appended);
            /* Vertical: rows r0+1..=r1 collapse to Word's on-disk shape —
            ONE cell per continuation row spanning the merged columns
            (`gridSpan = span`, `vMerge` continue), horizontal partners
            physically removed exactly like the top row. Anything else
            double-counts grid columns in the layout cursor walk and
            diverges from what the .docx reader produces for the same
            merge authored in Word. Issue #263 — the continuation cell's
            OWN blocks move into the owner too (Word never leaves content
            behind a `vMerge="continue"` cell); it is left with a single
            blank paragraph, same as a fresh cell elsewhere in the tree. */
            for r in (r0 + 1)..=r1.min(rcount - 1) {
                if (c0 as usize) >= t.rows[r as usize].cells.len() {
                    continue;
                }
                let row = &mut t.rows[r as usize];
                let mut appended = std::mem::replace(
                    &mut row.cells[c0 as usize].blocks,
                    vec![Block::Paragraph(Paragraph::default())],
                );
                row.cells[c0 as usize].props.v_merge = VMergeRole::Continue;
                row.cells[c0 as usize].props.grid_span = span.max(1);
                let drop_count = (c1.min(row.cells.len() as u32 - 1) - c0) as usize;
                for _ in 0..drop_count {
                    if (c0 as usize + 1) < row.cells.len() {
                        appended.extend(row.cells.remove(c0 as usize + 1).blocks);
                    }
                }
                t.rows[r0 as usize].cells[c0 as usize]
                    .blocks
                    .extend(appended);
            }
        })
    }

    /// Inverse of [`merge_cells`]: reset the owner's spans and physically
    /// restore the horizontally-merged-away partner cells (fresh default
    /// cells — Word keeps the merged content in the first cell), then walk
    /// the vertical continuation run below the owner and restore each of
    /// those rows the same way. Continuation cells are matched by their
    /// starting *grid column* (cursor walk over per-row `grid_span`s), not
    /// by cell index — preceding cells in a row may themselves span.
    pub fn split_cell(&self, table_path: BlockPath, row: u32, col: u32) -> Self {
        fn restore_row(row: &mut TableRow, idx: usize, span: usize) {
            row.cells[idx].props.grid_span = 1;
            row.cells[idx].props.v_merge = VMergeRole::None;
            for k in 0..span.saturating_sub(1) {
                row.cells.insert(idx + 1 + k, default_table_cell());
            }
        }
        /* Cell index in `row` whose starting grid column == `grid_col`. */
        fn cell_at_grid_col(row: &TableRow, grid_col: usize) -> Option<usize> {
            let mut cursor = 0usize;
            for (i, c) in row.cells.iter().enumerate() {
                match cursor.cmp(&grid_col) {
                    std::cmp::Ordering::Equal => return Some(i),
                    std::cmp::Ordering::Greater => return None,
                    std::cmp::Ordering::Less => cursor += c.props.grid_span.max(1) as usize,
                }
            }
            None
        }
        /* Issue #253 — the rows `restore_row` will widen and the cell index
        it widens at, computed on the PRE-mutation shape exactly like the
        walk below (restoring a row never changes a later row's cells):
        anchors in cells past that index shift right by `span - 1`. */
        let remap = self.mutated_table_shape(&table_path).and_then(|(tp, t)| {
            let owner_row = t.rows.get(row as usize)?;
            let owner = owner_row.cells.get(col as usize)?;
            let span = owner.props.grid_span.max(1) as u32;
            let owner_grid_col: usize = owner_row.cells[..col as usize]
                .iter()
                .map(|c| c.props.grid_span.max(1) as usize)
                .sum();
            let mut widened = vec![(row, col)];
            if matches!(owner.props.v_merge, VMergeRole::Restart) {
                for r in (row as usize + 1)..t.rows.len() {
                    let Some(i) = cell_at_grid_col(&t.rows[r], owner_grid_col) else {
                        break;
                    };
                    if !matches!(t.rows[r].cells[i].props.v_merge, VMergeRole::Continue) {
                        break;
                    }
                    widened.push((r as u32, i as u32));
                }
            }
            Some((tp, span, widened))
        });
        let mut out = self.mutate_table(table_path, |t| {
            let Some(owner_row) = t.rows.get(row as usize) else {
                return;
            };
            let Some(owner) = owner_row.cells.get(col as usize) else {
                return;
            };
            let span = owner.props.grid_span.max(1) as usize;
            let was_restart = matches!(owner.props.v_merge, VMergeRole::Restart);
            let owner_grid_col: usize = owner_row.cells[..col as usize]
                .iter()
                .map(|c| c.props.grid_span.max(1) as usize)
                .sum();
            restore_row(&mut t.rows[row as usize], col as usize, span);
            if was_restart {
                for r in (row as usize + 1)..t.rows.len() {
                    let Some(i) = cell_at_grid_col(&t.rows[r], owner_grid_col) else {
                        break;
                    };
                    if !matches!(t.rows[r].cells[i].props.v_merge, VMergeRole::Continue) {
                        break;
                    }
                    restore_row(&mut t.rows[r], i, span);
                }
            }
        });
        if let Some((tp, span, widened)) = remap
            && span > 1
        {
            out.remap_table_cells(&tp, |r, c| match widened.iter().find(|(wr, _)| *wr == r) {
                Some(&(_, idx)) if c > idx => CellMove::To {
                    row: r,
                    col: c + span - 1,
                },
                _ => CellMove::Keep,
            });
        }
        out
    }

    pub fn set_cell_shading(
        &self,
        table_path: BlockPath,
        row: u32,
        col: u32,
        color: Option<[u8; 4]>,
    ) -> Self {
        self.mutate_table(table_path, |t| {
            if let Some(r) = t.rows.get_mut(row as usize)
                && let Some(cell) = r.cells.get_mut(col as usize)
            {
                cell.props.shading = color;
            }
        })
    }

    pub fn set_cell_borders(
        &self,
        table_path: BlockPath,
        row: u32,
        col: u32,
        borders: CellBorders,
    ) -> Self {
        self.mutate_table(table_path, |t| {
            if let Some(r) = t.rows.get_mut(row as usize)
                && let Some(cell) = r.cells.get_mut(col as usize)
            {
                cell.props.borders = Some(borders.clone());
            }
        })
    }

    /// Issue #79 — set the table's `<w:bidiVisual>` flag (visual
    /// right-to-left column order). Flips `dirty` like every table edit
    /// so the writer regenerates `<w:tblPr>` with the flag.
    pub fn set_table_bidi_visual(&self, table_path: BlockPath, bidi_visual: bool) -> Self {
        self.mutate_table(table_path, |t| t.props.bidi_visual = bidi_visual)
    }

    /// Helper — open `table_path`'s `Block::Table`, run `f`, flip dirty,
    /// drop source bytes, write back.
    fn mutate_table<F>(&self, path: BlockPath, f: F) -> Self
    where
        F: FnOnce(&mut Table),
    {
        let idx = match top_level_block_index(&path) {
            Some(i) => i as usize,
            None => return self.clone(),
        };
        let mut blocks = self.blocks.clone();
        if idx >= blocks.len() {
            return self.clone();
        }
        let mut block = blocks[idx].clone();
        let Some(table) = block.as_table_mut() else {
            return self.clone();
        };
        f(table);
        /* Phase 5 PR 3 invariant: every table mutation drops the
        passthrough — the writer must regenerate from rows. */
        table.dirty = true;
        table.source_xml = None;
        blocks.set(idx, block);
        Self {
            blocks,
            body_section: self.body_section.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnote_stories: self.footnote_stories.clone(),
            endnote_stories: self.endnote_stories.clone(),
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            notes_dirty: self.notes_dirty.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
            hf_dirty: self.hf_dirty.clone(),
            settings_dirty: self.settings_dirty,
            document_root_attrs: self.document_root_attrs.clone(),
            part_root_attrs: self.part_root_attrs.clone(),
            document_envelope: self.document_envelope.clone(),
            source_package: self.source_package.clone(),
        }
    }
}

/// Issue #252 — `(container steps, last block index)` of a paragraph path
/// (`(root, 0)` for a path that does not end in a block step).
fn split_block_path(path: &BlockPath) -> (Vec<PathStep>, u32) {
    match path.steps.split_last() {
        Some((PathStep::Block(i), container)) => (container.to_vec(), *i),
        _ => (Vec::new(), 0),
    }
}

/// Issue #252 — the path of block `idx` inside `container`.
fn block_path_in(container: &[PathStep], idx: u32) -> BlockPath {
    let mut steps = container.to_vec();
    steps.push(PathStep::Block(idx));
    BlockPath { steps }
}

/// Extract the top-level block index from a `BlockPath` (first step
/// must be `PathStep::Block(N)`; nested-cell paths return `None`
/// at PR 3 — full nested-table mutation is PR 3b).
fn top_level_block_index(path: &BlockPath) -> Option<u32> {
    match path.steps.first()? {
        PathStep::Block(n) => Some(*n),
        PathStep::Cell { .. } => None,
    }
}

/// Default `<w:sz>` for a synthesised cell border — 4 eighths of a
/// point ≈ 0.5 pt single black line. Word's out-of-the-box border
/// weight; matches what `<w:tblBorders>` emits on `Normal.dotx`.
const DEFAULT_BORDER_SIZE_EIGHTH_PT: u16 = 4;

/// A4 content width in twips, sized to match the engine's layout-px
/// universe (1 layout px ≡ 1 CSS px ≡ 15 twips at 96 DPI — the same
/// conversion factor `twips_to_layout_px` uses). The page is 595 ×
/// 842 layout-px with 72 layout-px margins, leaving 451 layout-px of
/// content → 451 × 15 = 6765 twips. Used to seed a fresh table's
/// grid so the table fits inside the page margins on insert. Phase
/// 5c will switch to `<w:tblLayout w:type="autofit"/>`.
const DEFAULT_A4_CONTENT_TWIPS: i32 = 6765;

/* ---- Sprint 9 plain-text flattening helpers ------------------------- */

/// Flatten one paragraph's text + inline objects into the running plain-
/// text buffer (no trailing newline). U+FFFC anchors render as marker
/// strings so the caller never silently drops an image / footnote ref.
fn push_paragraph_plain(p: &Paragraph, out: &mut String) {
    let mut cursor: usize = 0;
    for obj in &p.inline_objects {
        let at = snap_offset(&p.text, obj.at) as usize;
        if at > cursor {
            out.push_str(&p.text[cursor..at]);
        }
        match &obj.kind {
            InlineKind::Image { .. } => out.push_str("[image]"),
            /* Issue #80 — numbers are derived at layout time
            (`DocumentTree::note_markers`), so the flat placeholder
            names the kind only. */
            InlineKind::FootnoteRef { .. } => out.push_str("[footnote]"),
            InlineKind::EndnoteRef { .. } => out.push_str("[endnote]"),
            InlineKind::NoteSelfRef { .. } => {}
            /* Issue #83 — the story flattens inline, one line per
            paragraph, so a copy never loses text-box content. */
            InlineKind::TextBox { story, .. } => {
                for (i, b) in story.body.iter().enumerate() {
                    if let Block::Paragraph(sp) = b {
                        if i > 0 {
                            out.push('\n');
                        }
                        push_paragraph_plain(sp, out);
                    }
                }
            }
        }
        /* Skip the 3-byte U+FFFC sentinel. Snapped (issue #115): an
        object offset that does not sit on its sentinel must not leave the
        cursor mid-scalar; never step backwards past `cursor` either. */
        cursor = cursor.max(snap_offset(&p.text, (at as u32).saturating_add(3)) as usize);
    }
    if cursor < p.text.len() {
        out.push_str(&p.text[cursor..]);
    }
}

/// Flatten a table — one tab-separated row per `TableRow`, with a
/// leading + trailing blank line so the surrounding paragraphs do not
/// glue against the table. `VMergeRole::Continue` cells emit an empty
/// column so the row width matches the document grid.
fn push_table_plain(t: &Table, out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for row in &t.rows {
        let mut first = true;
        for cell in &row.cells {
            if !first {
                out.push('\t');
            }
            first = false;
            if cell.props.v_merge == VMergeRole::Continue {
                continue;
            }
            let cell_text = flatten_blocks_plain(&cell.blocks);
            /* Within a cell, newlines + tabs would break the row layout —
            collapse them to spaces. */
            for ch in cell_text.chars() {
                match ch {
                    '\n' | '\r' | '\t' => out.push(' '),
                    other => out.push(other),
                }
            }
        }
        out.push('\n');
    }
}

fn flatten_blocks_plain(blocks: &[Block]) -> String {
    let mut s = String::new();
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Paragraph(p) => {
                if i > 0 {
                    s.push('\n');
                }
                push_paragraph_plain(p, &mut s);
            }
            Block::Table(inner) => push_table_plain(inner, &mut s),
        }
    }
    s
}

/// Sprint 12 (#11) — depth cap on `<w:basedOn>` style chains, matching
/// ECMA-376 §17.7.4.5 implementation guidance. Anything beyond is
/// almost certainly a malformed stylesheet; we silently clamp.
pub const MAX_STYLE_CHAIN: usize = 10;

/// Sprint 12 (#11) — recompute the resolved `props` view on a single
/// paragraph from its `style_id` cascade ∪ `direct_overrides`.
/// `style_defaults` is the document's `<w:docDefaults>` snapshot
/// (sits at the bottom of every cascade).
/// Issue #29 — free-fn form of [`DocumentTree::resolve_style_run_cascade`]
/// for callers holding only the style table (span materialization in
/// engine-wasm). Same cycle / depth guards as the para half.
pub fn resolve_run_cascade(
    styles: &std::collections::HashMap<String, ParagraphStyle>,
    run_defaults: &SpanStyle,
    style_id: Option<&str>,
) -> SpanStyle {
    let mut out = run_defaults.clone();
    let Some(leaf) = style_id else {
        return out;
    };
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut chain: Vec<&ParagraphStyle> = Vec::new();
    let mut current: Option<&str> = Some(leaf);
    while let Some(id) = current {
        if chain.len() >= MAX_STYLE_CHAIN || !visited.insert(id) {
            break;
        }
        let Some(def) = styles.get(id) else {
            break;
        };
        chain.push(def);
        current = def.based_on.as_deref();
    }
    for def in chain.iter().rev() {
        out = out.merged_with(def.run.clone());
    }
    out
}

pub fn recompute_paragraph_props(
    para: &mut Paragraph,
    styles: &std::collections::HashMap<String, ParagraphStyle>,
    style_defaults: &ParaProperties,
) {
    let mut resolved = style_defaults.clone();
    /* Walk the style chain leaf → root with the same cycle / depth
    guard as `DocumentTree::resolve_style_cascade` (kept here as a
    free fn so callers without a borrowed `DocumentTree` can still
    invoke it — e.g. the reader's per-paragraph fold loop). */
    if let Some(leaf) = para.style_id.as_deref() {
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut chain: Vec<&ParagraphStyle> = Vec::new();
        let mut current: Option<&str> = Some(leaf);
        while let Some(id) = current {
            if chain.len() >= MAX_STYLE_CHAIN || !visited.insert(id) {
                break;
            }
            let Some(def) = styles.get(id) else {
                break;
            };
            chain.push(def);
            current = def.based_on.as_deref();
        }
        for def in chain.iter().rev() {
            resolved = resolved.merged_with(def.para.clone());
        }
    }
    para.props = resolved.merged_with(para.direct_overrides.clone());
}

/// Sprint 11 — UAX-#29 word count for one paragraph's text. Shares
/// the `WordSegmenter::new_auto` thread-local with the line-break
/// path so the icu data tables compile in exactly once; subsequent
/// calls are pure boundary walks.
fn count_uax_words(text: &str) -> usize {
    use icu_segmenter::options::WordBreakInvariantOptions;
    use icu_segmenter::{WordSegmenter, WordSegmenterBorrowed};
    thread_local! {
        static SEGMENTER: WordSegmenterBorrowed<'static> =
            WordSegmenter::new_auto(WordBreakInvariantOptions::default());
    }
    SEGMENTER.with(|seg| {
        seg.segment_str(text)
            .iter_with_word_type()
            .filter(|(_, ty)| ty.is_word_like())
            .count()
    })
}

/// Word-style default cell-edge stroke — single 0.5 pt black.
pub fn default_word_stroke() -> BorderStroke {
    BorderStroke {
        style: BorderStyle::Single,
        size_eighth_pt: DEFAULT_BORDER_SIZE_EIGHTH_PT,
        color: Some([0, 0, 0, 255]),
    }
}

/// All-edges Word default border set. Used for both cell-level and
/// table-level borders on engine-synthesised tables (`InsertTable` /
/// `InsertRow` / `InsertColumn`) so freshly inserted tables paint
/// without the user opening the borders picker first.
pub fn default_word_borders() -> CellBorders {
    let s = default_word_stroke();
    CellBorders {
        top: Some(s.clone()),
        left: Some(s.clone()),
        bottom: Some(s.clone()),
        right: Some(s.clone()),
        inside_h: Some(s.clone()),
        inside_v: Some(s),
    }
}

/// Construct a default table cell — one empty paragraph (so layout
/// has something to measure) + Word-default 0.5 pt single-line
/// borders on every edge. Without the placeholder paragraph cells
/// collapse to zero height; without the borders the table is
/// invisible until the user dresses it up.
pub fn default_table_cell() -> TableCell {
    TableCell {
        props: CellProperties {
            borders: Some(default_word_borders()),
            ..CellProperties::default()
        },
        blocks: vec![Block::Paragraph(Paragraph::default())],
        source_markup: None,
    }
}

/// Continue a `block_at` walk from one `Block` through any remaining
/// `Cell + Block` step pairs.
fn block_at_descend<'a>(block: &'a Block, steps: &[PathStep]) -> Option<&'a Block> {
    if steps.is_empty() {
        return Some(block);
    }
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[0] else {
        return None;
    };
    let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
    let PathStep::Block(n) = *steps.get(1)? else {
        return None;
    };
    let next = cell.blocks.get(n as usize)?;
    block_at_descend(next, &steps[2..])
}

/// Issue #72 (design review B6) — first cell paragraph inside `t`,
/// path rooted at `base` (the table's own path). One nesting level:
/// a nested table's cells are searched too, depth-first.
fn first_cell_paragraph_path(t: &Table, base: BlockPath) -> Option<BlockPath> {
    for (ri, row) in t.rows.iter().enumerate() {
        for (ci, cell) in row.cells.iter().enumerate() {
            for (bi, b) in cell.blocks.iter().enumerate() {
                let child = base
                    .clone()
                    .push(PathStep::Cell {
                        row: ri as u32,
                        col: ci as u32,
                    })
                    .push(PathStep::Block(bi as u32));
                match b {
                    Block::Paragraph(_) => return Some(child),
                    Block::Table(nested) => {
                        if let Some(p) = first_cell_paragraph_path(nested, child) {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Tail twin of [`first_cell_paragraph_path`].
fn last_cell_paragraph_path(t: &Table, base: BlockPath) -> Option<BlockPath> {
    for (ri, row) in t.rows.iter().enumerate().rev() {
        for (ci, cell) in row.cells.iter().enumerate().rev() {
            for (bi, b) in cell.blocks.iter().enumerate().rev() {
                let child = base
                    .clone()
                    .push(PathStep::Cell {
                        row: ri as u32,
                        col: ci as u32,
                    })
                    .push(PathStep::Block(bi as u32));
                match b {
                    Block::Paragraph(_) => return Some(child),
                    Block::Table(nested) => {
                        if let Some(p) = last_cell_paragraph_path(nested, child) {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Issue #73 — single-block twin of [`walk_paragraphs`] for
/// slice-backed header/footer parts (which are `Vec<Block>`, not
/// `im::Vector`). Same one-cell-level descent.
fn walk_block<F: FnMut(&Paragraph)>(block: &Block, f: &mut F) {
    match block {
        Block::Paragraph(p) => f(p),
        Block::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for nested in &cell.blocks {
                        if let Block::Paragraph(p) = nested {
                            f(p);
                        }
                    }
                }
            }
        }
    }
}

/// Sprint 8 (UI Edition) helper — depth-first walk over every
/// `Block::Paragraph` in the tree, including paragraphs nested
/// inside `Block::Table` cells. Used by the count-style helpers
/// that need to visit every text-bearing node regardless of
/// container.
fn walk_paragraphs<F: FnMut(&Paragraph)>(blocks: &Vector<Block>, f: &mut F) {
    for b in blocks.iter() {
        match b {
            Block::Paragraph(p) => f(p),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for nested in &cell.blocks {
                            match nested {
                                Block::Paragraph(p) => f(p),
                                Block::Table(_) => {
                                    /* Nested tables — uncommon;
                                     * defer until a real corpus
                                     * exercises them. */
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Sprint 7 (UI Edition) helper — shift every byte-offset-bearing
/// field on `para` LEFT by `removed_len`, for every value at or
/// after `from`. Mirrors the rightward shift performed by
/// `insert_inline_image_at` in reverse. Used when a tracked-change
/// revision is rejected (Insert) or accepted (Delete) — and (issue
/// #265) when the reviewer's own pending insertion is removed by
/// `tracked_delete_range` — and the covered text range is sliced out.
fn shift_paragraph_offsets_after(para: &mut Paragraph, from: u32, removed_len: u32) {
    let to = from + removed_len;
    let shift = |v: &mut u32| {
        if *v >= to {
            *v -= removed_len;
        } else if *v > from {
            *v = from;
        }
    };
    for s in &mut para.spans {
        shift(&mut s.start);
        shift(&mut s.end);
    }
    para.spans.retain(|s| s.start < s.end);
    /* Issue #265 — an inline object is a single sentinel byte, not a
    range: one whose sentinel lies inside the removed gap has nothing
    left to clamp onto (unlike a span/field/hyperlink, which can be
    clipped to the gap's edge) and is dropped, exactly like
    `Paragraph::delete_text`'s rule for the same case. */
    para.inline_objects.retain_mut(|io| {
        if io.at >= to {
            io.at -= removed_len;
            true
        } else {
            io.at < from
        }
    });
    for h in &mut para.hyperlinks {
        shift(&mut h.start);
        shift(&mut h.end);
    }
    para.hyperlinks.retain(|h| h.start < h.end);
    for r in &mut para.revisions {
        shift(&mut r.start);
        shift(&mut r.end);
    }
    para.revisions.retain(|r| r.start < r.end);
    for f in &mut para.fields {
        shift(&mut f.start);
        shift(&mut f.end);
    }
    para.fields.retain(|f| f.start < f.end);
    /* Issues #199 / #106 / #250 — the source markup was remapped by the
    caller's `Paragraph::splice_text`, together with the text. */
}

/// Phase 3 (#40) — clipboard fragments are never section-marker
/// carriers: `slice` / `slice_blocks` strip on the way out and
/// `insert_rich` / `insert_rich_blocks` strip on the way in, so no
/// copy/paste path can transplant a section break into an unrelated
/// part of the document.
fn strip_section_marker(mut p: Paragraph) -> Paragraph {
    p.section_end = None;
    /* Issue #120 — a clipboard fragment never transplants the block-level
    envelope markup (a content control's `<w:sdt>`, a bookmark) either. */
    p.body_xml = None;
    /* Issues #199 / #106 — nor the source paragraph's identity
    (`w14:paraId`) and rsids: a pasted copy is a new paragraph. */
    p.source_markup = None;
    /* Issue #262 — nor a tracked change on the source paragraph's mark. */
    p.mark_revision = None;
    p
}

/// Issue #72 — header/footer parts are body-shaped block lists but are
/// NOT the body: a `section_end` marker inside a part would corrupt
/// `effective_sections` the moment the part round-trips through a
/// story tree. Every write into `DocumentTree::headers` / `footers`
/// funnels through this strip.
/// Splice a U+FFFC anchor + its [`InlineObject`] into `para` at byte
/// `offset` (clamped to the text length), shifting every overlay at or
/// past the anchor by the sentinel's 3 bytes. Shared by inline images
/// (Phase 7) and note references (issue #80).
fn splice_inline_object(para: &mut Paragraph, offset: u32, kind: InlineKind) -> TextEdit {
    const SENTINEL: &str = "\u{FFFC}";
    let sentinel_len = SENTINEL.len() as u32;
    /* Issue #115 — the crate's single offset policy (module docs);
    issues #250 / #252 — the splice remaps the source markup and returns
    the record the caller feeds to `remap_text_edit`. */
    let edit = para.splice_text(offset, 0, SENTINEL);
    let off = edit.at;
    for s in &mut para.spans {
        if s.start >= off {
            s.start += sentinel_len;
        }
        if s.end >= off {
            s.end += sentinel_len;
        }
    }
    for io in &mut para.inline_objects {
        if io.at >= off {
            io.at += sentinel_len;
        }
    }
    for h in &mut para.hyperlinks {
        if h.start >= off {
            h.start += sentinel_len;
        }
        if h.end >= off {
            h.end += sentinel_len;
        }
    }
    for r in &mut para.revisions {
        if r.start >= off {
            r.start += sentinel_len;
        }
        if r.end >= off {
            r.end += sentinel_len;
        }
    }
    for f in &mut para.fields {
        if f.start >= off {
            f.start += sentinel_len;
        }
        if f.end >= off {
            f.end += sentinel_len;
        }
    }
    para.inline_objects.push(InlineObject {
        at: off,
        kind,
        anchor: None,
        source_xml: None,
    });
    para.inline_objects.sort_by_key(|i| i.at);
    para.dirty = true;
    edit
}

/// Issue #80 — collect every note reference inside `block` (cells
/// depth-first) in source order, stamped with the TOP-LEVEL block index
/// `top` so numbering can resolve the owning section.
/// Issue #278 — the note references inside a free-standing block list (a
/// text-box story, a note body, a header part), in story order, tagged
/// `container` and hosted by top-level block 0. Layout uses it to carry
/// a text box's references on its sentinel glyph.
pub fn note_references_in_blocks(blocks: &[Block], container: NoteContainer) -> Vec<NoteReference> {
    let mut out = Vec::new();
    for b in blocks {
        walk_block_note_refs(b, 0, container, &mut out);
    }
    out
}

/// Issue #278 — the header / footer roles a section's pages paint, in
/// the order its pages show them: `first` (under `titlePg`) on page 1,
/// then `default` on odd pages and `even` (under `evenAndOddHeaders`) on
/// even ones — `even` comes before `default` exactly when page 1 is the
/// title page.
fn painted_hf_roles(title_pg: bool, even_and_odd: bool) -> Vec<HeaderFooterRole> {
    let mut roles = Vec::with_capacity(3);
    if title_pg {
        roles.push(HeaderFooterRole::First);
        if even_and_odd {
            roles.push(HeaderFooterRole::Even);
        }
        roles.push(HeaderFooterRole::Default);
    } else {
        roles.push(HeaderFooterRole::Default);
        if even_and_odd {
            roles.push(HeaderFooterRole::Even);
        }
    }
    roles
}

/// Issue #80 / #278 — append the note references in `block` (hosted by
/// top-level block `top`, sitting in `container`) in document order:
/// a paragraph's inline objects by anchor offset — a text box's story
/// is walked at its anchor's position — and a table's cells row-major,
/// depth-first. Recursion follows the model's own nesting (cells in
/// cells, boxes in boxes), which the reader bounds.
fn walk_block_note_refs(
    block: &Block,
    top: u32,
    container: NoteContainer,
    out: &mut Vec<NoteReference>,
) {
    match block {
        Block::Paragraph(p) => {
            let mut objs: Vec<&InlineObject> = p.inline_objects.iter().collect();
            objs.sort_by_key(|o| o.at);
            for obj in objs {
                let (anchor, custom_mark) = match &obj.kind {
                    InlineKind::FootnoteRef {
                        id,
                        custom_mark_follows,
                    } => (
                        NoteAnchor {
                            kind: NoteKind::Footnote,
                            id: *id,
                        },
                        *custom_mark_follows,
                    ),
                    InlineKind::EndnoteRef {
                        id,
                        custom_mark_follows,
                    } => (
                        NoteAnchor {
                            kind: NoteKind::Endnote,
                            id: *id,
                        },
                        *custom_mark_follows,
                    ),
                    InlineKind::TextBox { story, .. } => {
                        let inner = match container {
                            NoteContainer::Body | NoteContainer::TableCell => {
                                NoteContainer::TextBox
                            }
                            other => other,
                        };
                        for b in &story.body {
                            walk_block_note_refs(b, top, inner, out);
                        }
                        continue;
                    }
                    InlineKind::Image { .. } | InlineKind::NoteSelfRef { .. } => continue,
                };
                out.push(NoteReference {
                    top_block: top,
                    anchor,
                    custom_mark,
                    container,
                });
            }
        }
        Block::Table(t) => {
            let inner = match container {
                NoteContainer::Body => NoteContainer::TableCell,
                other => other,
            };
            for row in &t.rows {
                for cell in &row.cells {
                    for b in &cell.blocks {
                        walk_block_note_refs(b, top, inner, out);
                    }
                }
            }
        }
    }
}

fn strip_section_markers(blocks: Vec<Block>) -> Vec<Block> {
    blocks
        .into_iter()
        .map(|b| match b {
            Block::Paragraph(p) => Block::Paragraph(strip_section_marker(p)),
            Block::Table(mut t) => {
                t.body_xml = None;
                Block::Table(t)
            }
        })
        .collect()
}

/// Issue #83 — [`mutate_paragraph_in_top`] for a TOP-LEVEL paragraph
/// that keeps its passthrough capture (`dirty` / `source_xml` untouched):
/// the only caller is a text-box story edit, whose writer path splices
/// the regenerated container into the host bytes.
fn mutate_paragraph_keep_source<F>(top: &mut Vector<Block>, path: &BlockPath, f: F) -> Option<()>
where
    F: FnOnce(&mut Paragraph),
{
    let [PathStep::Block(n)] = path.steps.as_slice() else {
        return None;
    };
    let n = *n as usize;
    let mut block = top.get(n)?.clone();
    let Block::Paragraph(ref mut p) = block else {
        return None;
    };
    f(p);
    top.set(n, block);
    Some(())
}

/// Mutate the paragraph addressed by `path` in `top` (the
/// `im::Vector` top-level container). `f` runs against the cloned
/// paragraph in place; the containing block (and any intervening
/// table) is cloned + spliced back so the structural-sharing
/// invariant holds. Returns `Some(())` on success.
fn mutate_paragraph_in_top<F>(top: &mut Vector<Block>, path: &BlockPath, f: F) -> Option<()>
where
    F: FnOnce(&mut Paragraph),
{
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    let mut block = top.get(n)?.clone();
    if path.steps.len() == 1 {
        let Block::Paragraph(ref mut p) = block else {
            return None;
        };
        f(p);
        p.dirty = true;
        p.source_xml = None;
        top.set(n, block);
        return Some(());
    }
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let row_box = t.rows.get_mut(row as usize)?;
    let cell = row_box.cells.get_mut(col as usize)?;
    mutate_paragraph_in_vec(&mut cell.blocks, &path.steps[2..], f)?;
    /* A mutation inside a cell dirties the containing table so the
    writer regenerates it (PR 3 passthrough invariant). */
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

#[allow(clippy::ptr_arg)]
fn mutate_paragraph_in_vec<F>(blocks: &mut Vec<Block>, steps: &[PathStep], f: F) -> Option<()>
where
    F: FnOnce(&mut Paragraph),
{
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    let block = blocks.get_mut(n)?;
    if steps.len() == 1 {
        let Block::Paragraph(p) = block else {
            return None;
        };
        f(p);
        p.dirty = true;
        p.source_xml = None;
        return Some(());
    }
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let row_box = t.rows.get_mut(row as usize)?;
    let cell = row_box.cells.get_mut(col as usize)?;
    mutate_paragraph_in_vec(&mut cell.blocks, &steps[2..], f)?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
}

/// Replace the block at `path` with `replacement` in `top`. Used by
/// `split_paragraph` etc. to splice freshly built paragraphs into the
/// container structurally.
fn replace_block_in_top(
    top: &mut Vector<Block>,
    path: &BlockPath,
    replacement: Block,
) -> Option<()> {
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if path.steps.len() == 1 {
        /* Issue #116 — `im::Vector::set` panics (via `index_mut`) on an
        out-of-range index; a stale wire path must be a no-op instead. */
        if n >= top.len() {
            return None;
        }
        top.set(n, replacement);
        return Some(());
    }
    let mut block = top.get(n)?.clone();
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    replace_block_in_vec(&mut cell.blocks, &path.steps[2..], replacement)?;
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

#[allow(clippy::ptr_arg)]
fn replace_block_in_vec(
    blocks: &mut Vec<Block>,
    steps: &[PathStep],
    replacement: Block,
) -> Option<()> {
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if steps.len() == 1 {
        if n >= blocks.len() {
            return None;
        }
        blocks[n] = replacement;
        return Some(());
    }
    let block = blocks.get_mut(n)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    replace_block_in_vec(&mut cell.blocks, &steps[2..], replacement)?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
}

/// Insert `inserted` immediately after `path` in its parent container.
fn insert_block_after_path_in_top(
    top: &mut Vector<Block>,
    path: &BlockPath,
    inserted: Block,
) -> Option<()> {
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if path.steps.len() == 1 {
        if n >= top.len() {
            top.push_back(inserted);
        } else {
            top.insert(n + 1, inserted);
        }
        return Some(());
    }
    let mut block = top.get(n)?.clone();
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    insert_block_after_path_in_vec(&mut cell.blocks, &path.steps[2..], inserted)?;
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

fn insert_block_after_path_in_vec(
    blocks: &mut Vec<Block>,
    steps: &[PathStep],
    inserted: Block,
) -> Option<()> {
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if steps.len() == 1 {
        let at = (n + 1).min(blocks.len());
        blocks.insert(at, inserted);
        return Some(());
    }
    let block = blocks.get_mut(n)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    insert_block_after_path_in_vec(&mut cell.blocks, &steps[2..], inserted)?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
}

/// Resolve `path`'s parent container into an owned snapshot
/// (`Vec<Block>` cloned out of the document). Used by range methods
/// that need to walk the paragraphs between two same-container
/// endpoints; structural sharing is preserved by mutating through
/// the dedicated splice helpers above (`replace_block_in_*`,
/// `insert_block_after_path_in_*`), not by writing this snapshot
/// back.
pub fn parent_container_snapshot(doc: &DocumentTree, path: &BlockPath) -> Option<Vec<Block>> {
    if path.steps.len() == 1 {
        return Some(doc.blocks.iter().cloned().collect());
    }
    if path.steps.len() < 3 {
        return None;
    }
    let n = path.steps.len();
    let grandparent = BlockPath {
        steps: path.steps[..n - 2].to_vec(),
    };
    let block = doc.block_at(&grandparent)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[n - 2] else {
        return None;
    };
    let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
    Some(cell.blocks.clone())
}

/// Same parent container? Two paragraph paths share a container
/// when every step but the last is identical.
pub fn same_parent(a: &BlockPath, b: &BlockPath) -> bool {
    a.steps.len() == b.steps.len() && a.parent() == b.parent()
}

/// Path with its last `Block` step's index bumped by 1. Used by
/// `split_paragraph` etc. to compute the path of the inserted
/// sibling.
pub fn bump_last_block_index(path: &BlockPath) -> BlockPath {
    let mut steps = path.steps.clone();
    if let Some(PathStep::Block(n)) = steps.last_mut() {
        *n += 1;
    }
    BlockPath { steps }
}

/// Order two positions in document order. The first returned position
/// is always `<=` the second when compared by `(path, offset)`.
pub fn order_positions(a: LogicalPos, b: LogicalPos) -> (LogicalPos, LogicalPos) {
    use core::cmp::Ordering;
    let ord = a.path.cmp_doc_order(&b.path);
    let swap = match ord {
        Ordering::Less => false,
        Ordering::Greater => true,
        Ordering::Equal => a.offset > b.offset,
    };
    if swap { (b, a) } else { (a, b) }
}

/// Delete the block at `path` from its parent container.
fn delete_block_at_path(top: &mut Vector<Block>, path: &BlockPath) -> Option<()> {
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if path.steps.len() == 1 {
        if n >= top.len() {
            return None;
        }
        top.remove(n);
        return Some(());
    }
    let mut block = top.get(n)?.clone();
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    delete_block_in_vec(&mut cell.blocks, &path.steps[2..])?;
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

fn delete_block_in_vec(blocks: &mut Vec<Block>, steps: &[PathStep]) -> Option<()> {
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if steps.len() == 1 {
        if n >= blocks.len() {
            return None;
        }
        blocks.remove(n);
        return Some(());
    }
    let block = blocks.get_mut(n)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    delete_block_in_vec(&mut cell.blocks, &steps[2..])?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
}

/// Bounded undo/redo snapshot stack. Pushing a new snapshot truncates the
/// redo branch (standard editor semantics).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UndoStack {
    /// Each element is a complete document snapshot. `im::Vector` clones in O(1)
    /// so pushing a snapshot is cheap structurally; only the modified
    /// `Paragraph.text` allocates.
    snapshots: Vec<DocumentTree>,
    /// Index of the current document in `snapshots`. Always `< snapshots.len()`.
    cursor: usize,
    /// Maximum snapshots retained (oldest are dropped on overflow).
    cap: usize,
    /// Monotone counter bumped by every operation that changes which
    /// document `current()` returns (`push` / `undo` / `redo` /
    /// `replace_current`). Consumers use it as a cheap cache key for
    /// derived state (the engine's layout snapshot) — two equal
    /// revisions on the SAME stack guarantee an identical `current()`.
    /// A fresh stack restarts at 0, so callers that swap stacks must
    /// invalidate their caches explicitly.
    revision: u64,
}

impl UndoStack {
    pub fn new(initial: DocumentTree, cap: usize) -> Self {
        Self {
            snapshots: vec![initial],
            cursor: 0,
            cap,
            revision: 0,
        }
    }

    pub fn current(&self) -> &DocumentTree {
        &self.snapshots[self.cursor]
    }

    /// See the `revision` field: bumped on every mutation of the
    /// current document; stable across read-only access.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn replace_current(&mut self, doc: DocumentTree) {
        text_remap::debug_assert_tree_in_step(&doc);
        self.snapshots[self.cursor] = doc;
        self.revision += 1;
    }

    pub fn push(&mut self, doc: DocumentTree) {
        /* Issue #250 — test builds: no committed mutation may leave a
        paragraph's source markup out of step with its text. */
        text_remap::debug_assert_tree_in_step(&doc);
        /* Truncate any redo branch. */
        if self.cursor + 1 < self.snapshots.len() {
            self.snapshots.truncate(self.cursor + 1);
        }
        self.snapshots.push(doc);
        self.cursor = self.snapshots.len() - 1;
        /* Cap from the bottom. */
        while self.snapshots.len() > self.cap {
            self.snapshots.remove(0);
            self.cursor = self.cursor.saturating_sub(1);
        }
        self.revision += 1;
    }

    pub fn undo(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.cursor + 1 >= self.snapshots.len() {
            return false;
        }
        self.cursor += 1;
        self.revision += 1;
        true
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.snapshots.len()
    }

    pub fn depth(&self) -> u32 {
        self.snapshots.len() as u32
    }

    /// Maximum snapshots this stack retains.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Issue #85 — the most recent `max_entries` snapshots plus the cursor
    /// remapped into that window, for crash-recovery persistence. The
    /// window always contains the current document (the cursor entry),
    /// even when a long redo branch would otherwise push it out; entries
    /// older than the window are the ones recovery forgets.
    pub fn history_window(&self, max_entries: usize) -> (Vec<DocumentTree>, usize) {
        let max_entries = max_entries.max(1);
        let len = self.snapshots.len();
        let lo = len.saturating_sub(max_entries).min(self.cursor);
        (self.snapshots[lo..].to_vec(), self.cursor - lo)
    }

    /// Issue #85 — rebuild a stack from a persisted window. Sanitizes
    /// hostile input: an empty window yields a fresh empty-document stack,
    /// an out-of-range cursor is clamped to the newest entry, and the
    /// window is trimmed from the bottom to `cap`. The revision counter
    /// restarts at 0, so callers must drop any revision-keyed caches.
    pub fn from_history(snapshots: Vec<DocumentTree>, cursor: usize, cap: usize) -> Self {
        let cap = cap.max(1);
        if snapshots.is_empty() {
            return Self::new(DocumentTree::new(), cap);
        }
        let mut snapshots = snapshots;
        let mut cursor = cursor.min(snapshots.len() - 1);
        if snapshots.len() > cap {
            let drop = snapshots.len() - cap;
            snapshots.drain(..drop);
            cursor = cursor.saturating_sub(drop);
        }
        Self {
            snapshots,
            cursor,
            cap,
            revision: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* ---- issue #23: dynamic, string-backed FontFamily ------------- */

    #[test]
    fn font_family_named_ids_and_display_names() {
        assert_eq!(FontFamily::Amiri.id(), "amiri");
        assert_eq!(FontFamily::LiberationSans.id(), "liberation");
        assert_eq!(FontFamily::NotoNaskhArabic.id(), "noto-naskh");
        assert_eq!(FontFamily::Amiri.display_name(), "Amiri");
        assert_eq!(FontFamily::LiberationSans.display_name(), "Liberation Sans");
        assert_eq!(
            FontFamily::NotoNaskhArabic.display_name(),
            "Noto Naskh Arabic"
        );
    }

    #[test]
    fn font_family_from_id_resolves_named_and_custom() {
        assert_eq!(FontFamily::from_id("amiri"), Some(FontFamily::Amiri));
        assert_eq!(
            FontFamily::from_id("noto-naskh"),
            Some(FontFamily::NotoNaskhArabic)
        );
        // Unknown id → Custom, display humanized from the id.
        assert_eq!(
            FontFamily::from_id("cairo"),
            Some(FontFamily::Custom {
                id: "cairo".into(),
                display: "Cairo".into()
            })
        );
        assert_eq!(
            FontFamily::from_id("times-new-roman"),
            Some(FontFamily::Custom {
                id: "times-new-roman".into(),
                display: "Times New Roman".into()
            })
        );
        assert_eq!(FontFamily::from_id(""), None);
        assert_eq!(FontFamily::from_id("   "), None);
    }

    #[test]
    fn font_family_from_display_name_resolves_named_and_custom() {
        assert_eq!(
            FontFamily::from_display_name("Liberation Sans"),
            Some(FontFamily::LiberationSans)
        );
        assert_eq!(
            FontFamily::from_display_name("\"Amiri\""),
            Some(FontFamily::Amiri)
        );
        // Unknown display name → Custom, verbatim display + slugified id.
        assert_eq!(
            FontFamily::from_display_name("Cairo"),
            Some(FontFamily::Custom {
                id: "cairo".into(),
                display: "Cairo".into()
            })
        );
        // Verbatim display is preserved even when slugify→humanize would not
        // recover the original casing — this is what guards docx byte-identity.
        let dejavu = FontFamily::from_display_name("DejaVu Sans").unwrap();
        assert_eq!(dejavu.display_name(), "DejaVu Sans");
        assert_eq!(dejavu.id(), "dejavu-sans");
        assert_eq!(FontFamily::from_display_name("   "), None);
    }

    #[test]
    fn font_family_from_display_name_preserves_verbatim_display() {
        // The display is stored VERBATIM (surrounding whitespace + quotes
        // intact) while the resolution id is derived from the trimmed/unquoted
        // key. This is what guards .docx `<w:rFonts>` byte-identity — the docx
        // reader passes the raw attribute value and must not see it normalized.
        let f = FontFamily::from_display_name("Calibri ").unwrap();
        assert_eq!(f.display_name(), "Calibri ", "trailing space must survive");
        assert_eq!(f.id(), "calibri", "id is the slugified, trimmed key");

        // A name that literally contains quote characters is preserved too.
        let q = FontFamily::from_display_name("\"Weird\" Font").unwrap();
        assert_eq!(q.display_name(), "\"Weird\" Font");
        // Leading/trailing quote+space are stripped only for the match key.
        let amiri = FontFamily::from_display_name("  \"Amiri\"  ").unwrap();
        assert_eq!(amiri, FontFamily::Amiri);
    }

    #[test]
    fn font_family_custom_id_display_round_trip_via_methods() {
        // A custom face's id and display survive the accessor methods intact —
        // the layout/render boundary reads id(), the docx/CSS writer reads
        // display_name().
        let f = FontFamily::Custom {
            id: "cairo".into(),
            display: "Cairo".into(),
        };
        assert_eq!(FontFamily::from_id(f.id()), Some(f.clone()));
    }

    /* ---- Sprint 9: plain-text flattening -------------------------- */

    #[test]
    fn plain_text_two_paragraphs() {
        let d = DocumentTree::from_text("hello");
        let d = d.split_paragraph(LogicalPos {
            path: BlockPath::top(0),
            offset: 5,
        });
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            "world",
        );
        assert_eq!(d.to_plain_text(), "hello\nworld");
    }

    #[test]
    fn plain_text_image_marker_replaces_sentinel() {
        let mut d = DocumentTree::default();
        d.blocks.push_back(Block::Paragraph(Paragraph {
            text: "a\u{FFFC}b".into(),
            inline_objects: vec![InlineObject {
                at: 1,
                kind: InlineKind::Image {
                    rel_id: "rId1".into(),
                    width_emu: 0,
                    height_emu: 0,
                    media_key: None,
                },
                anchor: None,
                source_xml: None,
            }],
            ..Default::default()
        }));
        assert_eq!(d.to_plain_text(), "a[image]b");
    }

    #[test]
    fn resize_inline_image_overwrites_extent_at_offset() {
        let mut d = DocumentTree::default();
        d.blocks.push_back(Block::Paragraph(Paragraph {
            text: "a\u{FFFC}b".into(),
            inline_objects: vec![InlineObject {
                at: 1,
                kind: InlineKind::Image {
                    rel_id: "rId1".into(),
                    width_emu: 914_400,
                    height_emu: 914_400,
                    media_key: None,
                },
                anchor: None,
                source_xml: None,
            }],
            ..Default::default()
        }));
        let d = d.resize_inline_image_at(&BlockPath::top(0), 1, 457_200, 228_600);
        let InlineKind::Image {
            width_emu,
            height_emu,
            ..
        } = &d.blocks[0].as_paragraph().unwrap().inline_objects[0].kind
        else {
            panic!("expected an image inline object");
        };
        assert_eq!((*width_emu, *height_emu), (457_200, 228_600));
    }

    #[test]
    fn resize_inline_image_clamps_to_at_least_one_emu() {
        let mut d = DocumentTree::default();
        d.blocks.push_back(Block::Paragraph(Paragraph {
            text: "\u{FFFC}".into(),
            inline_objects: vec![InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: "rId1".into(),
                    width_emu: 100,
                    height_emu: 100,
                    media_key: None,
                },
                anchor: None,
                source_xml: None,
            }],
            ..Default::default()
        }));
        let d = d.resize_inline_image_at(&BlockPath::top(0), 0, 0, -5);
        let InlineKind::Image {
            width_emu,
            height_emu,
            ..
        } = &d.blocks[0].as_paragraph().unwrap().inline_objects[0].kind
        else {
            panic!("expected an image inline object");
        };
        assert_eq!(
            (*width_emu, *height_emu),
            (1, 1),
            "a degenerate resize must clamp to 1 EMU, never 0/negative"
        );
    }

    /* ---------------------------------------------------------------
    Issue #69 — floating anchors.
    --------------------------------------------------------------- */

    fn floating_image_doc(anchor: FloatAnchor) -> DocumentTree {
        let mut d = DocumentTree::default();
        d.blocks.push_back(Block::Paragraph(Paragraph {
            text: "ab\u{FFFC}cd".into(),
            inline_objects: vec![InlineObject {
                at: 2,
                kind: InlineKind::Image {
                    rel_id: "rId1".into(),
                    width_emu: 914_400,
                    height_emu: 914_400,
                    media_key: None,
                },
                anchor: Some(Box::new(anchor)),
                source_xml: None,
            }],
            ..Default::default()
        }));
        d
    }

    fn first_object(d: &DocumentTree) -> &InlineObject {
        &d.blocks[0].as_paragraph().unwrap().inline_objects[0]
    }

    #[test]
    fn move_floating_image_sets_fixed_offsets_and_keeps_the_frames() {
        let anchor = FloatAnchor {
            position_h: HPosition {
                relative_from: HRelativeFrom::Margin,
                offset: FloatOffset::Align(FloatAlign::Center),
            },
            position_v: VPosition {
                relative_from: VRelativeFrom::Paragraph,
                offset: FloatOffset::PercentMilli(25_000),
            },
            ..FloatAnchor::default()
        };
        let d = floating_image_doc(anchor).move_floating_image_at(&BlockPath::top(0), 2, 111, 222);
        let obj = first_object(&d);
        let a = obj.anchor.as_deref().expect("still floating");
        assert_eq!(a.position_h.offset, FloatOffset::Emu(111));
        assert_eq!(a.position_v.offset, FloatOffset::Emu(222));
        assert_eq!(
            a.position_h.relative_from,
            HRelativeFrom::Margin,
            "the reference frame is preserved — only the offset moves"
        );
        assert_eq!(a.position_v.relative_from, VRelativeFrom::Paragraph);
        assert!(!a.simple_pos);
        assert!(
            d.blocks[0].as_paragraph().unwrap().dirty,
            "a move dirties the paragraph so the writer regenerates the anchor"
        );
    }

    /// A `simplePos` object was positioned from the page corner; the shell
    /// hands back page-relative offsets, so the move must re-base both axes
    /// onto the `Page` frame (else the object would jump on its first drag).
    #[test]
    fn move_floating_image_rebases_simple_pos_onto_the_page_frame() {
        let anchor = FloatAnchor {
            position_h: HPosition {
                relative_from: HRelativeFrom::Margin,
                offset: FloatOffset::Align(FloatAlign::Center),
            },
            position_v: VPosition {
                relative_from: VRelativeFrom::Paragraph,
                offset: FloatOffset::Emu(5),
            },
            simple_pos: true,
            simple_pos_x_emu: 10,
            simple_pos_y_emu: 20,
            ..FloatAnchor::default()
        };
        let d = floating_image_doc(anchor).move_floating_image_at(&BlockPath::top(0), 2, 111, 222);
        let a = first_object(&d).anchor.as_deref().expect("still floating");
        assert!(!a.simple_pos, "a drag replaces simplePos with the axes");
        assert_eq!(a.position_h.relative_from, HRelativeFrom::Page);
        assert_eq!(a.position_v.relative_from, VRelativeFrom::Page);
        assert_eq!(a.position_h.offset, FloatOffset::Emu(111));
        assert_eq!(a.position_v.offset, FloatOffset::Emu(222));
    }

    #[test]
    fn move_floating_image_is_a_no_op_for_inline_images() {
        let mut d = DocumentTree::default();
        d.blocks.push_back(Block::Paragraph(Paragraph {
            text: "\u{FFFC}".into(),
            inline_objects: vec![InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: "rId1".into(),
                    width_emu: 100,
                    height_emu: 100,
                    media_key: None,
                },
                anchor: None,
                source_xml: None,
            }],
            ..Default::default()
        }));
        let moved = d.move_floating_image_at(&BlockPath::top(0), 0, 5, 5);
        assert!(!first_object(&moved).is_floating());
        assert_eq!(moved.count_floating_images(), 0);
        assert_eq!(moved.count_inline_images(), 1);
    }

    /// Issue #82 — the wrap setter changes the mode, drops the verbatim
    /// element only when the mode changed, keeps side rule + distances,
    /// and ties `behind_doc` to wrap-none.
    #[test]
    fn set_floating_image_wrap_switches_modes_and_layering() {
        let d = floating_image_doc(FloatAnchor {
            wrap: WrapKind::Square,
            wrap_text: WrapText::Left,
            dist_left_emu: 12_700,
            wrap_xml: Some("<wp:wrapSquare wrapText=\"left\"/>".into()),
            ..FloatAnchor::default()
        });
        let same = d.set_floating_image_wrap_at(&BlockPath::top(0), 2, WrapKind::Square, true);
        let a = first_object(&same).anchor.as_deref().unwrap();
        assert!(
            a.wrap_xml.is_some(),
            "unchanged mode keeps the verbatim element"
        );
        assert!(!a.behind_doc, "only wrap-none can sit behind the text");
        let behind = d.set_floating_image_wrap_at(&BlockPath::top(0), 2, WrapKind::None, true);
        let a = first_object(&behind).anchor.as_deref().unwrap();
        assert_eq!(a.wrap, WrapKind::None);
        assert!(a.behind_doc);
        assert!(a.wrap_xml.is_none());
        assert_eq!((a.wrap_text, a.dist_left_emu), (WrapText::Left, 12_700));
        let front = behind.set_floating_image_wrap_at(&BlockPath::top(0), 2, WrapKind::None, false);
        assert!(!first_object(&front).anchor.as_deref().unwrap().behind_doc);
        /* Wrong offset: structural no-op. */
        let none = d.set_floating_image_wrap_at(&BlockPath::top(0), 0, WrapKind::None, false);
        assert_eq!(first_object(&none), first_object(&d));
    }

    #[test]
    fn floating_anchor_survives_offset_shifting_edits() {
        let d = floating_image_doc(FloatAnchor::default());
        assert_eq!(d.count_floating_images(), 1);
        /* Delete the "a" before the anchor: the sentinel shifts left and
        the anchor rides along. */
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 1,
            },
        );
        let obj = first_object(&d);
        assert_eq!(obj.at, 1, "sentinel shifted by the deleted byte");
        assert!(
            obj.is_floating(),
            "the anchor must travel with its sentinel"
        );
        /* Insert text before it: shifts right, still floating. */
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "xyz",
        );
        let obj = first_object(&d);
        assert_eq!(obj.at, 4);
        assert!(obj.is_floating());
    }

    /// The pre-#69 snapshot shape (`{at, kind}` with no `anchor`) must keep
    /// decoding — format version 1 is unchanged, `anchor` defaults.
    #[test]
    fn inline_object_without_anchor_field_decodes_as_inline() {
        #[derive(Serialize)]
        struct Legacy {
            at: u32,
            kind: InlineKind,
        }
        let legacy = Legacy {
            at: 3,
            kind: InlineKind::FootnoteRef {
                id: 1,
                custom_mark_follows: false,
            },
        };
        let bytes = rmp_serde::to_vec_named(&legacy).expect("encode legacy");
        let decoded: InlineObject = rmp_serde::from_slice(&bytes).expect("decode with default");
        assert_eq!(decoded.at, 3);
        assert!(decoded.anchor.is_none());
        /* And a floating object round-trips its whole anchor. */
        let obj = InlineObject {
            at: 0,
            kind: InlineKind::Image {
                rel_id: "r".into(),
                width_emu: 1,
                height_emu: 2,
                media_key: None,
            },
            anchor: Some(Box::new(FloatAnchor {
                wrap: WrapKind::Square,
                wrap_xml: Some("<wp:wrapSquare wrapText=\"bothSides\"/>".into()),
                behind_doc: true,
                ..FloatAnchor::default()
            })),
            source_xml: None,
        };
        let bytes = rmp_serde::to_vec_named(&obj).expect("encode");
        let back: InlineObject = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(back, obj);
    }

    #[test]
    fn count_inline_images_walks_body_and_cells() {
        let mut d = DocumentTree::from_text("plain");
        assert_eq!(d.count_inline_images(), 0);
        d.blocks.push_back(Block::Paragraph(Paragraph {
            text: "\u{FFFC}\u{FFFC}".into(),
            inline_objects: vec![
                InlineObject {
                    at: 0,
                    kind: InlineKind::Image {
                        rel_id: "a".into(),
                        width_emu: 1,
                        height_emu: 1,
                        media_key: None,
                    },
                    anchor: None,
                    source_xml: None,
                },
                InlineObject {
                    at: 3,
                    kind: InlineKind::Image {
                        rel_id: "b".into(),
                        width_emu: 1,
                        height_emu: 1,
                        media_key: None,
                    },
                    anchor: None,
                    source_xml: None,
                },
            ],
            ..Default::default()
        }));
        assert_eq!(d.count_inline_images(), 2);
        /* Issue #206 — a picture inside a text box's story counts too. */
        let (boxed, host, at) = d.insert_text_box_at(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            914_400,
            914_400,
        );
        let mut story = DocumentTree::from_text("\u{FFFC}");
        if let Some(Block::Paragraph(p)) = story.blocks.get_mut(0) {
            p.inline_objects.push(InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: "c".into(),
                    width_emu: 1,
                    height_emu: 1,
                    media_key: None,
                },
                anchor: None,
                source_xml: None,
            });
        }
        let boxed = boxed.with_updated_text_box(&host, at, story.blocks.iter().cloned().collect());
        assert_eq!(boxed.count_inline_images(), 3);
        let tree = boxed.text_box_story_tree(&[(host, at)]).expect("story");
        assert_eq!(tree.count_inline_images(), 1);
    }

    /// Issue #80 — typing before an inline anchor slides it right with
    /// its sentinel byte; typing after leaves it alone.
    #[test]
    fn typing_after_a_note_reference_does_not_inherit_its_superscript() {
        let mut d = DocumentTree::from_text("ab\u{FFFC}");
        let sup = SpanStyle {
            vert_align: Some(VertAlign::Superscript),
            ..SpanStyle::default()
        };
        {
            let p = d.blocks[0].as_paragraph_mut().unwrap();
            p.inline_objects = vec![InlineObject {
                at: 2,
                kind: InlineKind::FootnoteRef {
                    id: 1,
                    custom_mark_follows: false,
                },
                anchor: None,
                source_xml: None,
            }];
            p.spans = vec![StyleRun {
                start: 2,
                end: 5,
                style: sup.clone(),
            }];
        }
        let at = |o| LogicalPos {
            path: BlockPath::top(0),
            offset: o,
        };
        /* Issue #276 — the anchor's run formatting is not continued. */
        assert_eq!(
            d.nth_paragraph(0).unwrap().typing_style_at(5),
            SpanStyle::default()
        );
        let after = d.insert_text(at(5), " more");
        let p = after.nth_paragraph(0).unwrap();
        assert_eq!(p.style_at(2), sup, "the reference keeps its own style");
        assert_eq!(p.style_at(5), SpanStyle::default());
        assert_eq!(p.style_at(9), SpanStyle::default());
    }

    /// Issue #276 — typed text continues a run's formatting but never its
    /// tracked-formatting record (`<w:rPrChange>` in the grab bag).
    #[test]
    fn typing_after_a_run_with_a_format_change_drops_the_revision_record() {
        let mut bag = None;
        GrabBag::push_into(&mut bag, b"<w:lang w:val=\"de-DE\"/>".to_vec());
        GrabBag::push_into(&mut bag, b"<w:rPrChange w:id=\"8\"/>".to_vec());
        let donor = SpanStyle {
            bold: Some(true),
            grab_bag: bag,
            ..SpanStyle::default()
        };
        let d = DocumentTree::from_text("ab cd").apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            donor.clone(),
        );
        let typed = donor.for_typing();
        assert_eq!(typed.bold, Some(true));
        assert_eq!(
            GrabBag::fragments_of(&typed.grab_bag),
            &[b"<w:lang w:val=\"de-DE\"/>".to_vec()]
        );
        for at in [5, 2] {
            let after = d.insert_text(
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: at,
                },
                "XY",
            );
            let p = after.nth_paragraph(0).unwrap();
            assert_eq!(p.style_at(at), typed, "typed text at {at}");
            assert_eq!(p.style_at(at + 1), typed);
            assert_eq!(p.style_at(0), donor, "the source text keeps it");
            /* Mid-run: the tail of the split donor span keeps it too. */
            assert_eq!(
                p.style_at(6),
                if at == 2 {
                    donor.clone()
                } else {
                    typed.clone()
                }
            );
        }
    }

    #[test]
    fn insert_text_shifts_inline_anchors_past_the_insertion_point() {
        let mut d = DocumentTree::from_text("ab\u{FFFC}cd\u{FFFC}");
        d.blocks[0].as_paragraph_mut().unwrap().inline_objects = vec![
            InlineObject {
                at: 2,
                kind: InlineKind::FootnoteRef {
                    id: 1,
                    custom_mark_follows: false,
                },
                anchor: None,
                source_xml: None,
            },
            InlineObject {
                at: 7,
                kind: InlineKind::EndnoteRef {
                    id: 1,
                    custom_mark_follows: false,
                },
                anchor: None,
                source_xml: None,
            },
        ];
        let after = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 1,
            },
            "XYZ",
        );
        let p = after.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.text, "aXYZb\u{FFFC}cd\u{FFFC}");
        assert_eq!(p.inline_objects[0].at, 5);
        assert_eq!(p.inline_objects[1].at, 10);
        assert_eq!(&p.text[5..8], "\u{FFFC}");
        assert_eq!(&p.text[10..13], "\u{FFFC}");
        /* Typing right AT the anchor byte inserts BEFORE the sentinel. */
        let at_anchor = after.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            "!",
        );
        let p = at_anchor.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.text, "aXYZb!\u{FFFC}cd\u{FFFC}");
        assert_eq!(p.inline_objects[0].at, 6);
        /* Typing after every anchor leaves them in place. */
        let tail = after.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 13,
            },
            "end",
        );
        assert_eq!(
            tail.blocks[0].as_paragraph().unwrap().inline_objects[1].at,
            10
        );
    }

    #[test]
    fn resize_inline_image_is_noop_at_a_non_image_offset() {
        let d = DocumentTree::from_text("plain text");
        let before = d.clone();
        let after = d.resize_inline_image_at(&BlockPath::top(0), 3, 500, 500);
        assert_eq!(after.blocks[0].as_paragraph().unwrap().text, "plain text");
        assert_eq!(
            before.blocks[0]
                .as_paragraph()
                .unwrap()
                .inline_objects
                .len(),
            after.blocks[0].as_paragraph().unwrap().inline_objects.len()
        );
    }

    #[test]
    fn plain_text_table_tab_separates_cells() {
        let mut d = DocumentTree::default();
        let cell = |s: &str| TableCell {
            props: CellProperties::default(),
            blocks: vec![Block::Paragraph(Paragraph {
                text: s.into(),
                ..Default::default()
            })],
            source_markup: None,
        };
        d.blocks.push_back(Block::Table(Table {
            grid: vec![6765, 6765],
            props: TableProperties::default(),
            rows: vec![
                TableRow {
                    props: RowProperties::default(),
                    cells: vec![cell("a"), cell("b")],
                    source_markup: None,
                },
                TableRow {
                    props: RowProperties::default(),
                    cells: vec![cell("c"), cell("d")],
                    source_markup: None,
                },
            ],
            dirty: true,
            source_xml: None,
            body_xml: None,
            source_markup: None,
        }));
        assert_eq!(d.to_plain_text(), "a\tb\nc\td");
    }

    /* ---- Sprint 12 (#11): style cascade + shadow direct_overrides --- */

    /// Issue #21 — ModifyStyle mutates the definition and re-cascades
    /// every dependent paragraph, preserving direct overrides; #29 —
    /// the run half resolves through the chain.
    #[test]
    fn modify_style_recascades_and_flips_dirty() {
        let d = doc_with_heading_style();
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 5);
        let d = d.set_paragraph_style(start, end, Some("Heading1".into()));
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().props.alignment,
            Some(Alignment::Center)
        );
        assert!(!d.styles_dirty, "reads never flip the writer gate");
        let d = d.modify_style(
            "Heading1",
            Some(ParaProperties {
                alignment: Some(Alignment::End),
                ..Default::default()
            }),
            Some(SpanStyle {
                bold: Some(true),
                font_size: Some(16.0),
                ..Default::default()
            }),
            None,
            None,
        );
        assert!(d.styles_dirty, "ModifyStyle must arm styles.xml regen");
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(
            p.props.alignment,
            Some(Alignment::End),
            "dependent paragraph re-cascaded"
        );
        let run = d.resolve_style_run_cascade(Some("Heading1"));
        assert_eq!(run.bold, Some(true));
        assert_eq!(run.font_size, Some(16.0));
        /* Unknown ids are a no-op clone. */
        let same = d.modify_style("Nope", None, None, None, None);
        assert_eq!(same.styles.len(), d.styles.len());
    }

    /// Issue #21 — a direct override survives a style mutation (the #11
    /// re-application discipline).
    #[test]
    fn modify_style_preserves_direct_overrides() {
        let d = doc_with_heading_style();
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 5);
        let d = d.set_paragraph_style(start.clone(), end.clone(), Some("Heading1".into()));
        /* User sets an explicit alignment on top of the style. */
        let mut d = d;
        {
            let mut blocks = d.blocks.clone();
            let mut b = blocks[0].clone();
            if let Block::Paragraph(p) = &mut b {
                p.direct_overrides.alignment = Some(Alignment::Justify);
                recompute_paragraph_props(p, &d.styles, &d.style_defaults);
            }
            blocks.set(0, b);
            d.blocks = blocks;
        }
        let d = d.modify_style(
            "Heading1",
            Some(ParaProperties {
                alignment: Some(Alignment::End),
                ..Default::default()
            }),
            None,
            None,
            None,
        );
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().props.alignment,
            Some(Alignment::Justify),
            "direct override outranks the mutated style"
        );
    }

    fn doc_with_heading_style() -> DocumentTree {
        let mut d = DocumentTree::from_text("hello");
        d.styles.insert(
            "Heading1".into(),
            ParagraphStyle {
                id: "Heading1".into(),
                name: "Heading 1".into(),
                based_on: None,
                para: ParaProperties {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                },
                run: SpanStyle::default(),
                next: None,
            },
        );
        d
    }

    #[test]
    fn apply_style_sets_style_id_and_props() {
        let d = doc_with_heading_style();
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_paragraph_style(p0.clone(), p0, Some("Heading1".into()));
        let p = d.nth_paragraph(0).expect("paragraph 0");
        assert_eq!(p.style_id.as_deref(), Some("Heading1"));
        assert_eq!(
            p.props.alignment,
            Some(Alignment::Center),
            "style cascade should fold into resolved props"
        );
    }

    #[test]
    fn direct_override_survives_subsequent_style_change() {
        /* Apply a direct alignment (Right) first; then apply
        Heading1 (which sets Center). The shadow approach must keep
        Right because direct_overrides win over the style cascade. */
        let d = doc_with_heading_style();
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_alignment(p0.clone(), p0.clone(), Alignment::End);
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.direct_overrides.alignment, Some(Alignment::End));
        let d = d.set_paragraph_style(p0.clone(), p0, Some("Heading1".into()));
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.style_id.as_deref(), Some("Heading1"));
        assert_eq!(
            p.props.alignment,
            Some(Alignment::End),
            "direct_overrides must win over style cascade"
        );
    }

    #[test]
    fn detach_style_falls_back_to_direct_overrides_only() {
        let d = doc_with_heading_style();
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_paragraph_style(p0.clone(), p0.clone(), Some("Heading1".into()));
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        let d = d.set_paragraph_style(p0.clone(), p0, None);
        assert_eq!(d.nth_paragraph(0).unwrap().style_id, None);
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            None,
            "detached style + no direct_overrides → default"
        );
    }

    #[test]
    fn style_cascade_walks_based_on_chain() {
        let mut d = DocumentTree::from_text("x");
        d.styles.insert(
            "Base".into(),
            ParagraphStyle {
                id: "Base".into(),
                name: "Base".into(),
                based_on: None,
                para: ParaProperties {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                },
                run: SpanStyle::default(),
                next: None,
            },
        );
        d.styles.insert(
            "Child".into(),
            ParagraphStyle {
                id: "Child".into(),
                name: "Child".into(),
                based_on: Some("Base".into()),
                /* Child contributes direction; alignment inherits from Base. */
                para: ParaProperties {
                    direction: Some(TextDirection::Rtl),
                    ..Default::default()
                },
                run: SpanStyle::default(),
                next: None,
            },
        );
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_paragraph_style(p0.clone(), p0, Some("Child".into()));
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.props.alignment, Some(Alignment::Center));
        assert_eq!(p.props.direction, Some(TextDirection::Rtl));
    }

    /* ---- Sprint 14 (#14): track-changes recording ----------------- */

    fn tracked_doc() -> DocumentTree {
        DocumentTree::from_text("hello")
    }

    fn pos0(off: u32) -> LogicalPos {
        LogicalPos {
            path: BlockPath::top(0),
            offset: off,
        }
    }

    #[test]
    fn tracked_insert_outside_revision_adds_insert_revision() {
        let d = tracked_doc();
        let d = d.tracked_insert_text(pos0(0), "X", "Alice".into(), "2026-01-01".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "Xhello");
        assert_eq!(p.revisions.len(), 1);
        assert_eq!(p.revisions[0].kind, RevisionKind::Insert);
        assert_eq!(p.revisions[0].start, 0);
        assert_eq!(p.revisions[0].end, 1);
        assert_eq!(p.revisions[0].author, "Alice");
    }

    #[test]
    fn tracked_insert_grows_adjacent_same_author_insert() {
        let d = tracked_doc();
        let d = d.tracked_insert_text(pos0(5), "A", "Alice".into(), "t1".into());
        /* Cursor now at offset 6; same author types another char. */
        let d = d.tracked_insert_text(pos0(6), "B", "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "helloAB");
        assert_eq!(
            p.revisions.len(),
            1,
            "adjacent same-author Inserts must merge — got {:?}",
            p.revisions
        );
        assert_eq!(p.revisions[0].start, 5);
        assert_eq!(p.revisions[0].end, 7);
    }

    #[test]
    fn tracked_insert_inside_existing_insert_grows_it_no_new_revision() {
        let d = tracked_doc();
        let d = d.tracked_insert_text(pos0(5), "AAA", "Alice".into(), "t1".into());
        /* Type INSIDE the Insert at offset 6 (between AAA). */
        let d = d.tracked_insert_text(pos0(6), "Z", "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "helloAZAA");
        assert_eq!(
            p.revisions.len(),
            1,
            "inside-Insert keystroke must grow the Insert, not split"
        );
        assert_eq!(p.revisions[0].kind, RevisionKind::Insert);
        assert_eq!(p.revisions[0].start, 5);
        assert_eq!(p.revisions[0].end, 9);
    }

    #[test]
    fn tracked_insert_inside_delete_splits_delete_and_adds_insert() {
        let d = tracked_doc();
        /* First mark "hello" entirely as a tracked Delete. */
        let d = d.tracked_delete_range(pos0(0), pos0(5), "Alice".into(), "t1".into());
        assert_eq!(
            d.nth_paragraph(0).unwrap().revisions.len(),
            1,
            "single Delete after mark"
        );
        /* Now type inside the Delete at offset 2 (between "he" and "llo"). */
        let d = d.tracked_insert_text(pos0(2), "X", "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "heXllo");
        /* Expected: [0, 2) Delete (he) + [2, 3) Insert (X) + [3, 6) Delete (llo). */
        let kinds: Vec<_> = p
            .revisions
            .iter()
            .map(|r| (r.kind, r.start, r.end))
            .collect();
        assert!(
            kinds.contains(&(RevisionKind::Delete, 0, 2)),
            "left Delete half missing: {kinds:?}"
        );
        assert!(
            kinds.contains(&(RevisionKind::Delete, 3, 6)),
            "right Delete half missing: {kinds:?}"
        );
        assert!(
            kinds.contains(&(RevisionKind::Insert, 2, 3)),
            "Insert in gap missing: {kinds:?}"
        );
    }

    #[test]
    fn tracked_delete_marker_only_preserves_text() {
        let d = tracked_doc();
        let d = d.tracked_delete_range(pos0(0), pos0(3), "Alice".into(), "t1".into());
        let p = d.nth_paragraph(0).unwrap();
        /* Marker-only delete: text remains. */
        assert_eq!(p.text, "hello");
        assert_eq!(p.revisions.len(), 1);
        assert_eq!(p.revisions[0].kind, RevisionKind::Delete);
        assert_eq!(p.revisions[0].start, 0);
        assert_eq!(p.revisions[0].end, 3);
    }

    #[test]
    fn tracked_delete_inside_own_insert_shrinks_insert_removes_text() {
        let d = tracked_doc();
        /* Type 3 chars at offset 5 — Insert overlay covers [5, 8). */
        let d = d.tracked_insert_text(pos0(5), "ABC", "Alice".into(), "t1".into());
        assert_eq!(d.nth_paragraph(0).unwrap().text, "helloABC");
        /* Backspace one char (delete [7, 8)). Range is fully inside
        the same-author Insert → uninsert: text shrinks AND Insert
        end shifts left by 1. */
        let d = d.tracked_delete_range(pos0(7), pos0(8), "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "helloAB");
        assert_eq!(p.revisions.len(), 1);
        assert_eq!(p.revisions[0].kind, RevisionKind::Insert);
        assert_eq!(p.revisions[0].start, 5);
        assert_eq!(p.revisions[0].end, 7);
    }

    /// Issue #265 — deleting the reviewer's own pending insertion must
    /// remap EVERY byte-offset table, not just style spans and the
    /// comment anchors (#252): a field, a hyperlink and a picture inside
    /// the removed insertion must leave no stale offsets, and undo must
    /// restore them.
    #[test]
    fn tracked_delete_own_insert_remaps_fields_hyperlinks_and_inline_objects() {
        let base = tracked_doc();
        let mut undo = UndoStack::new(base.clone(), 100);
        let inserted =
            base.tracked_insert_text(pos0(5), "PPPPLL\u{FFFC}", "Alice".into(), "t1".into());
        undo.push(inserted.clone());
        assert_eq!(
            inserted.nth_paragraph(0).unwrap().text,
            "helloPPPPLL\u{FFFC}"
        );
        /* Attach a field over "PPPP" [5, 9), a hyperlink over "LL"
        [9, 11), and a picture at the sentinel [11, 14) — all inside the
        pending Insert revision [5, 14) `tracked_insert_text` just
        recorded. */
        let mut blocks = inserted.blocks.clone();
        if let Block::Paragraph(p) = &mut blocks[0] {
            p.fields.push(Field {
                start: 5,
                end: 9,
                instruction: "PAGE".into(),
                span: None,
                source: None,
            });
            p.hyperlinks.push(Hyperlink {
                start: 9,
                end: 11,
                target: "https://example.com".into(),
                ..Default::default()
            });
            p.inline_objects.push(InlineObject {
                at: 11,
                kind: InlineKind::Image {
                    rel_id: "rId9".into(),
                    width_emu: 100,
                    height_emu: 100,
                    media_key: None,
                },
                anchor: None,
                source_xml: None,
            });
        }
        let mut with_overlays = inserted;
        with_overlays.blocks = blocks;
        undo.push(with_overlays.clone());
        /* Delete the WHOLE pending insertion — the own-insertion
        (owning_insert) path. */
        let deleted =
            with_overlays.tracked_delete_range(pos0(5), pos0(14), "Alice".into(), "t2".into());
        undo.push(deleted.clone());
        let p = deleted.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "hello");
        assert!(p.fields.is_empty(), "stale field: {:?}", p.fields);
        assert!(
            p.hyperlinks.is_empty(),
            "stale hyperlink: {:?}",
            p.hyperlinks
        );
        assert!(
            p.inline_objects.is_empty(),
            "stale inline object: {:?}",
            p.inline_objects
        );
        assert!(
            p.revisions.iter().all(|r| r.kind != RevisionKind::Insert),
            "the fully-removed Insert must not survive: {:?}",
            p.revisions
        );
        /* Undo restores the overlays and their text. */
        assert!(undo.undo());
        let restored = undo.current().nth_paragraph(0).unwrap();
        assert_eq!(restored.text, "helloPPPPLL\u{FFFC}");
        assert_eq!(restored.fields.len(), 1);
        assert_eq!(restored.hyperlinks.len(), 1);
        assert_eq!(restored.inline_objects.len(), 1);
    }

    #[test]
    fn tracked_delete_merges_adjacent_same_author_delete() {
        let d = tracked_doc();
        let d = d.tracked_delete_range(pos0(0), pos0(2), "Alice".into(), "t1".into());
        let d = d.tracked_delete_range(pos0(2), pos0(4), "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(
            p.revisions.len(),
            1,
            "adjacent same-author Deletes must merge"
        );
        assert_eq!(p.revisions[0].start, 0);
        assert_eq!(p.revisions[0].end, 4);
    }

    #[test]
    fn tracked_format_change_carries_prev_attrs() {
        let d = tracked_doc();
        let prev = SpanStyle {
            bold: Some(false),
            ..Default::default()
        };
        let d =
            d.tracked_format_change(pos0(0), pos0(5), prev.clone(), "Alice".into(), "t1".into());
        let p = d.nth_paragraph(0).unwrap();
        let rev = p
            .revisions
            .iter()
            .find(|r| r.kind == RevisionKind::FormatChange);
        let rev = rev.expect("FormatChange revision present");
        assert_eq!(rev.start, 0);
        assert_eq!(rev.end, 5);
        assert_eq!(rev.prev_attrs, Some(prev));
    }

    #[test]
    fn undo_of_tracked_insert_restores_snapshot_no_counter_delete() {
        /* UndoStack is snapshot-based: undo restores the prior tree,
        which had no revisions. No counter-Delete should appear. */
        let initial = tracked_doc();
        let mut stack = UndoStack::new(initial.clone(), 100);
        let after_insert = initial.tracked_insert_text(pos0(0), "X", "Alice".into(), "t1".into());
        stack.push(after_insert);
        assert_eq!(stack.current().nth_paragraph(0).unwrap().text, "Xhello");
        assert_eq!(stack.current().nth_paragraph(0).unwrap().revisions.len(), 1);
        stack.undo();
        let p = stack.current().nth_paragraph(0).unwrap();
        assert_eq!(p.text, "hello");
        assert!(
            p.revisions.is_empty(),
            "undo of tracked insert must NOT leave a counter-Delete revision"
        );
    }

    /* ---- Sprint 11 (#17): UAX-#29 word_count ---------------------- */

    #[test]
    fn word_count_latin_matches_word_like_split() {
        let d = DocumentTree::from_text("Hello, brave new world!");
        assert_eq!(d.word_count(), 4);
    }

    #[test]
    fn word_count_cjk_segments_chars() {
        /* Mandarin "我喜欢编程" (I like programming) — 5 Han chars.
        Whitespace-split would return 1; UAX-#29 with dictionary
        segmentation returns a CJK-meaningful count > 1. The exact
        count varies with the segmenter's dictionary; assert > 1 so
        the test survives dictionary updates without sacrificing
        regression coverage for "CJK reports a meaningful value". */
        let d = DocumentTree::from_text("我喜欢编程");
        assert!(
            d.word_count() > 1,
            "CJK word_count should segment, got {}",
            d.word_count()
        );
    }

    #[test]
    fn word_count_punctuation_excluded() {
        /* Five word-like tokens; commas + period must NOT count. */
        let d = DocumentTree::from_text("one, two, three, four, five.");
        assert_eq!(d.word_count(), 5);
    }

    /* ---- Sprint 10: section + cell read-back helpers --------------- */

    #[test]
    fn section_for_block_returns_default_a4_when_no_sections() {
        let d = DocumentTree::from_text("hello");
        let s = d.section_for_block(0);
        assert!((s.geometry.width - 595.3).abs() < 0.5);
        assert!((s.geometry.height - 841.9).abs() < 0.5);
        assert_eq!(s.columns.count, 1);
    }

    #[test]
    fn section_for_block_picks_matching_section() {
        /* Marker-model rewrite of the old range-stamped test (was
        `d.sections = vec![Section{0..1, narrow}, Section{1..5, a4}]`).
        The sole paragraph (index 0) closes the narrow section via its
        `section_end` marker; the trailing `body_section` (doc-wide
        a4 default) plays the role of the old second range — a query
        past the marker (block 2, which doesn't even exist in this
        1-paragraph doc) must still fall through to it, exactly as the
        old out-of-range `end_block: 5` did. */
        let mut d = DocumentTree::from_text("a");
        let mut narrow = PageGeometry::a4();
        narrow.margin_left = 36.0;
        if let Some(Block::Paragraph(p)) = d.blocks.get(0) {
            let mut p = p.clone();
            p.section_end = Some(Box::new(SectionProps {
                geometry: narrow,
                ..Default::default()
            }));
            d.blocks.set(0, Block::Paragraph(p));
        }
        d.body_section = SectionProps {
            geometry: PageGeometry::a4(),
            ..Default::default()
        };
        assert!((d.section_for_block(0).geometry.margin_left - 36.0).abs() < 0.1);
        assert!((d.section_for_block(2).geometry.margin_left - 72.0).abs() < 0.1);
    }

    /* ================================================================
    Phase 3 (#40) — paragraph-anchored section markers. Every rule in
    `Paragraph::section_end`'s doc comment has a regression test here.
    ================================================================ */

    /// Two-section fixture: paragraphs "a" | "b" "c", the marker on "a"
    /// carrying `margin_left = 30`, the body section stock A4 (72).
    fn two_section_doc() -> DocumentTree {
        let blocks = vec![
            Block::Paragraph(Paragraph {
                text: "a".into(),
                ..Default::default()
            }),
            Block::Paragraph(Paragraph {
                text: "b".into(),
                ..Default::default()
            }),
            Block::Paragraph(Paragraph {
                text: "c".into(),
                ..Default::default()
            }),
        ];
        let mut narrow = PageGeometry::a4();
        narrow.margin_left = 30.0;
        let sections = vec![
            Section {
                geometry: narrow,
                start_block: 0,
                end_block: 1,
                ..Default::default()
            },
            Section {
                start_block: 1,
                end_block: 3,
                ..Default::default()
            },
        ];
        DocumentTree::from_blocks_with_sections(blocks, sections)
    }

    #[test]
    fn from_blocks_with_sections_stamps_markers_and_body_section() {
        let d = two_section_doc();
        let marker = d
            .paragraph_at_path(&BlockPath::top(0))
            .and_then(|p| p.section_end.as_ref())
            .expect("paragraph 0 carries the interior section marker");
        assert!((marker.geometry.margin_left - 30.0).abs() < 0.1);
        /* Load-time stamping must not dirty the paragraph — its
        source_xml (when present) already carries the sectPr, and the
        writer passthrough must stay byte-stable. */
        assert!(!d.paragraph_at_path(&BlockPath::top(0)).unwrap().dirty);
        assert!((d.body_section.geometry.margin_left - 72.0).abs() < 0.1);
        let derived = d.effective_sections();
        assert_eq!(derived.len(), 2);
        assert_eq!((derived[0].start_block, derived[0].end_block), (0, 1));
        assert_eq!((derived[1].start_block, derived[1].end_block), (1, 3));
    }

    #[test]
    fn editing_above_a_boundary_keeps_section_coverage() {
        /* THE desync regression (pre-Phase-3 bug B1): pressing Enter in
        section 1 grew `blocks` without re-indexing later sections, so
        every block after the boundary fell under the wrong geometry.
        With markers the boundary rides its paragraph. */
        let d = two_section_doc();
        let split = d.split_paragraph(LogicalPos::new(BlockPath::top(0), 0));
        let derived = split.effective_sections();
        assert_eq!(derived.len(), 2, "boundary survived the split");
        /* The marker travelled with the RIGHT half (original mark), so
        section 1 now covers blocks [0, 2). */
        assert_eq!((derived[0].start_block, derived[0].end_block), (0, 2));
        assert_eq!((derived[1].start_block, derived[1].end_block), (2, 4));
        assert!((split.section_for_block(3).geometry.margin_left - 72.0).abs() < 0.1);
        assert!((split.section_for_block(1).geometry.margin_left - 30.0).abs() < 0.1);
    }

    #[test]
    fn split_at_moves_marker_to_right_half_only() {
        let mut p = Paragraph {
            text: "xy".into(),
            ..Default::default()
        };
        p.section_end = Some(Box::new(SectionProps::default()));
        let (left, right) = p.split_at(1);
        assert!(left.section_end.is_none(), "left half gets a fresh mark");
        assert!(right.section_end.is_some(), "original mark ends the right");
    }

    #[test]
    fn concat_takes_the_tail_marker_deleting_the_heads() {
        /* Deliberately inverted from concat's head-wins convention:
        merging deletes the HEAD's paragraph mark, so the head's
        section break dies and the TAIL's survives (Word: deleting a
        section break makes preceding text adopt the FOLLOWING
        section's properties). */
        let mut head = Paragraph {
            text: "h".into(),
            ..Default::default()
        };
        let mut head_props = SectionProps::default();
        head_props.geometry.margin_left = 30.0;
        head.section_end = Some(Box::new(head_props));
        let tail = Paragraph {
            text: "t".into(),
            ..Default::default()
        };
        let merged = head.concat(&tail);
        assert!(
            merged.section_end.is_none(),
            "head's break deleted; tail carried no marker"
        );

        let mut tail2 = Paragraph {
            text: "t".into(),
            ..Default::default()
        };
        let mut tail_props = SectionProps::default();
        tail_props.geometry.margin_left = 40.0;
        tail2.section_end = Some(Box::new(tail_props));
        let merged2 = head.concat(&tail2);
        let kept = merged2.section_end.expect("tail marker survives");
        assert!((kept.geometry.margin_left - 40.0).abs() < 0.1);
    }

    #[test]
    fn deleting_across_a_marker_merges_sections_following_props_win() {
        let d = two_section_doc();
        /* Delete from mid-"a" (the marker paragraph) into "b" — the
        marker paragraph's mark is consumed by the merge. */
        let merged = d.delete_range(
            LogicalPos::new(BlockPath::top(0), 0),
            LogicalPos::new(BlockPath::top(1), 0),
        );
        let derived = merged.effective_sections();
        assert_eq!(derived.len(), 1, "sections merged");
        assert!(
            (derived[0].geometry.margin_left - 72.0).abs() < 0.1,
            "following (body) section's geometry governs the merged range"
        );
    }

    #[test]
    fn copy_paste_never_transplants_a_section_break() {
        let d = two_section_doc();
        /* Copy a range covering the marker paragraph... */
        let fragment = d.slice(
            LogicalPos::new(BlockPath::top(0), 0),
            LogicalPos::new(BlockPath::top(1), 1),
        );
        assert!(
            fragment.iter().all(|p| p.section_end.is_none()),
            "clipboard fragments are never marker carriers"
        );
        /* ...and paste it at the document end: section count unchanged. */
        let before = d.effective_sections().len();
        let (pasted, _) = d.insert_rich(d.end_of_document(), &fragment);
        assert_eq!(pasted.effective_sections().len(), before);

        /* Same guarantee for the block-preserving variant. */
        let block_fragment = d.slice_blocks(
            LogicalPos::new(BlockPath::top(0), 0),
            LogicalPos::new(BlockPath::top(1), 1),
        );
        assert!(block_fragment.iter().all(|b| match b {
            Block::Paragraph(p) => p.section_end.is_none(),
            _ => true,
        }));
        let (pasted2, _) = d.insert_rich_blocks(d.end_of_document(), &block_fragment);
        assert_eq!(pasted2.effective_sections().len(), before);
    }

    #[test]
    fn fresh_document_section_setters_hit_body_section() {
        /* Pre-Phase-3 bug B2: on a fresh document (`sections` empty),
        SetPageMargins/SetPageOrientation/SetColumns silently no-opped.
        The always-present `body_section` makes them land. */
        let d = DocumentTree::from_text("hello");
        let pos = LogicalPos::new(BlockPath::top(0), 0);
        let with_margins = d.set_section_margins_at(pos.clone(), 10.0, 20.0, 30.0, 40.0);
        let s = with_margins.section_for_block(0);
        assert!((s.geometry.margin_top - 10.0).abs() < 0.1);
        assert!((s.geometry.margin_left - 40.0).abs() < 0.1);

        let landscape = d.set_section_orientation_at(pos.clone(), true);
        let s = landscape.section_for_block(0);
        assert!(s.geometry.width > s.geometry.height);

        let cols = d.set_section_columns_at(pos, 2, 18.0);
        assert_eq!(cols.section_for_block(0).columns.count, 2);
    }

    #[test]
    fn interior_section_setter_mutates_the_marker_and_dirties_it() {
        let d = two_section_doc();
        let updated =
            d.set_section_margins_at(LogicalPos::new(BlockPath::top(0), 0), 5.0, 6.0, 7.0, 8.0);
        let marker_para = updated.paragraph_at_path(&BlockPath::top(0)).unwrap();
        let props = marker_para.section_end.as_ref().expect("marker survives");
        assert!((props.geometry.margin_top - 5.0).abs() < 0.1);
        assert!(
            marker_para.dirty,
            "writer must regenerate the pPr with the mutated sectPr"
        );
        /* The FOLLOWING section is untouched. */
        assert!((updated.section_for_block(2).geometry.margin_top - 72.0).abs() < 0.1);
    }

    #[test]
    fn marker_on_last_block_suppresses_empty_trailing_section() {
        let d = two_section_doc();
        /* Delete blocks 1..3 ("b", "c") so the marker paragraph "a"
        becomes the final block. */
        let truncated = d.delete_range(
            LogicalPos::new(BlockPath::top(0), 1),
            LogicalPos::new(BlockPath::top(2), 1),
        );
        let derived = truncated.effective_sections();
        assert!(
            !derived.iter().any(|s| s.end_block <= s.start_block),
            "no empty derived section: {derived:?}"
        );
    }

    #[test]
    fn insert_section_break_splits_and_stamps() {
        let d = DocumentTree::from_text("hello world");
        let broken =
            d.insert_section_break_at(LogicalPos::new(BlockPath::top(0), 5), SectionType::NextPage);
        let sections = broken.effective_sections();
        assert_eq!(sections.len(), 2);
        assert_eq!((sections[0].start_block, sections[0].end_block), (0, 1));
        assert_eq!((sections[1].start_block, sections[1].end_block), (1, 2));
        assert_eq!(broken.paragraph_text(0), Some("hello"));
        assert_eq!(broken.paragraph_text(1), Some(" world"));
        /* Both halves share the covering geometry (Word clones on
        insert). */
        assert!((sections[0].geometry.margin_left - 72.0).abs() < 0.1);
        assert!((sections[1].geometry.margin_left - 72.0).abs() < 0.1);
    }

    #[test]
    fn insert_continuous_break_types_the_following_section() {
        let d = DocumentTree::from_text("hello world");
        let broken = d.insert_section_break_at(
            LogicalPos::new(BlockPath::top(0), 5),
            SectionType::Continuous,
        );
        let sections = broken.effective_sections();
        assert_eq!(sections.len(), 2);
        assert_eq!(
            sections[0].section_type,
            SectionType::NextPage,
            "first half keeps the covering section's own start type"
        );
        assert_eq!(
            sections[1].section_type,
            SectionType::Continuous,
            "<w:type> describes how the section it OPENS begins"
        );
    }

    #[test]
    fn insert_break_targets_the_covering_sections_own_terminal() {
        /* A | B | C — critic Q2's off-by-one-section hazard: a break
        inside B must retype B's OWN terminal, never C's storage. */
        let d = two_section_doc(); /* A = [0,1) narrow-marker, B = body [1,3) */
        let broken = d.insert_section_break_at(
            LogicalPos::new(BlockPath::top(1), 1),
            SectionType::Continuous,
        );
        let sections = broken.effective_sections();
        assert_eq!(sections.len(), 3);
        /* A untouched. */
        assert!((sections[0].geometry.margin_left - 30.0).abs() < 0.1);
        assert_eq!(sections[0].section_type, SectionType::NextPage);
        /* B's first half: new marker, B's original start type. */
        assert_eq!(sections[1].section_type, SectionType::NextPage);
        /* B's second half (closed by body_section) starts continuous. */
        assert_eq!(sections[2].section_type, SectionType::Continuous);
        assert_eq!(broken.body_section.section_type, SectionType::Continuous);
    }

    #[test]
    fn insert_break_on_a_marker_paragraph_itself() {
        /* Splitting the marker paragraph: the original marker rides the
        right half; the new marker lands on the left; the right-half
        marker (the covering section's own terminal) takes the kind. */
        let d = two_section_doc();
        let broken = d.insert_section_break_at(
            LogicalPos::new(BlockPath::top(0), 1),
            SectionType::Continuous,
        );
        let sections = broken.effective_sections();
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[1].section_type, SectionType::Continuous);
        /* The old narrow geometry now closes section 2 (the original
        marker, retyped). */
        assert!((sections[1].geometry.margin_left - 30.0).abs() < 0.1);
        /* Body section untouched. */
        assert_eq!(broken.body_section.section_type, SectionType::NextPage);
    }

    #[test]
    fn insert_break_inside_a_table_cell_is_rejected() {
        let mut d = DocumentTree::from_text("body");
        d.blocks.push_back(Block::Table(Table {
            grid: vec![2000],
            props: TableProperties::default(),
            rows: vec![TableRow {
                props: RowProperties::default(),
                cells: vec![TableCell {
                    props: CellProperties::default(),
                    blocks: vec![Block::Paragraph(Paragraph {
                        text: "cell".into(),
                        ..Default::default()
                    })],
                    source_markup: None,
                }],
                source_markup: None,
            }],
            dirty: true,
            source_xml: None,
            body_xml: None,
            source_markup: None,
        }));
        let cell_pos = LogicalPos::new(
            BlockPath::top(1)
                .push(PathStep::Cell { row: 0, col: 0 })
                .push(PathStep::Block(0)),
            0,
        );
        let out = d.insert_section_break_at(cell_pos, SectionType::NextPage);
        assert_eq!(out.effective_sections().len(), 1, "no section created");
        assert_eq!(out.block_count(), d.block_count(), "no split happened");
    }

    #[test]
    fn document_tree_default_body_section_is_a4() {
        /* Guards the indirect Default chain: if PageGeometry ever gains
        a derived (zeroed) Default, a fresh document would silently lay
        out on a 0x0 page. */
        let d = DocumentTree::default();
        assert!((d.body_section.geometry.width - 595.3).abs() < 0.5);
        assert!((d.body_section.geometry.height - 841.9).abs() < 0.5);
        let derived = d.effective_sections();
        assert_eq!(derived.len(), 1, "empty doc still derives one section");
    }

    #[test]
    fn formatting_a_marker_paragraph_keeps_the_break() {
        let d = two_section_doc();
        let marker_para = d.paragraph_at_path(&BlockPath::top(0)).unwrap();
        let styled = marker_para.apply_style(
            0,
            1,
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert!(styled.section_end.is_some(), "apply_style preserves marker");
        let deleted = marker_para.delete_text(0, 1);
        assert!(
            deleted.section_end.is_some(),
            "in-paragraph deletion preserves the mark's marker"
        );
    }

    #[test]
    fn innermost_cell_props_returns_none_outside_table() {
        let d = DocumentTree::from_text("hello");
        let path = BlockPath::top(0);
        assert!(d.innermost_cell_props_at(&path).is_none());
    }

    #[test]
    fn innermost_cell_props_finds_cell_at_top_level_table() {
        let mut d = DocumentTree::default();
        let mut cell = TableCell {
            props: CellProperties::default(),
            blocks: vec![Block::Paragraph(Paragraph {
                text: "x".into(),
                ..Default::default()
            })],
            source_markup: None,
        };
        cell.props.shading = Some([0xff, 0, 0, 0xff]);
        d.blocks.push_back(Block::Table(Table {
            grid: vec![6765],
            props: TableProperties::default(),
            rows: vec![TableRow {
                props: RowProperties::default(),
                cells: vec![cell],
                source_markup: None,
            }],
            dirty: true,
            source_xml: None,
            body_xml: None,
            source_markup: None,
        }));
        let path = BlockPath {
            steps: vec![
                PathStep::Block(0),
                PathStep::Cell { row: 0, col: 0 },
                PathStep::Block(0),
            ],
        };
        let props = d.innermost_cell_props_at(&path).expect("cell resolved");
        assert_eq!(props.shading, Some([0xff, 0, 0, 0xff]));
    }

    #[test]
    fn insert_into_empty() {
        let d = DocumentTree::new();
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "hello",
        );
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn insert_mid_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            ",",
        );
        assert_eq!(d.paragraph_text(0), Some("hello, world"));
    }

    #[test]
    fn apply_style_creates_span() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            SpanStyle {
                font_size: Some(20.0),
                color: None,
                ..Default::default()
            },
        );
        let spans = &doc.nth_paragraph(0).unwrap().spans;
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0],
            StyleRun {
                start: 0,
                end: 5,
                style: SpanStyle {
                    font_size: Some(20.0),
                    color: None,
                    ..Default::default()
                },
            }
        );
    }

    /// Issue #56 — applying character formatting must never drop a
    /// hyperlink overlay AND its tracked-change revisions; `apply_style`
    /// doesn't touch `text`, so both overlays' byte ranges are still valid
    /// after the patch (the epic's manual-QA scenario: a hyperlink +
    /// Track Changes revision surviving a style apply).
    #[test]
    fn apply_style_preserves_hyperlink_overlay() {
        let mut doc = DocumentTree::from_text("hello world");
        let mut para = doc.nth_paragraph(0).unwrap().clone();
        para.hyperlinks.push(Hyperlink {
            start: 0,
            end: 5,
            target: "https://example.com".to_string(),
            ..Default::default()
        });
        para.revisions.push(Revision {
            start: 6,
            end: 11,
            kind: RevisionKind::Insert,
            author: "Reviewer".to_string(),
            date: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            prev_attrs: None,
            move_name: None,
        });
        doc.blocks[0] = Block::Paragraph(para);

        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let revisions = &doc.nth_paragraph(0).unwrap().revisions;
        assert_eq!(
            revisions.len(),
            1,
            "tracked-change revision must survive a style apply"
        );
        assert_eq!(revisions[0].author, "Reviewer");
        let hyperlinks = &doc.nth_paragraph(0).unwrap().hyperlinks;
        assert_eq!(hyperlinks.len(), 1, "hyperlink must survive a style apply");
        assert_eq!(hyperlinks[0].target, "https://example.com");
    }

    /// Issue #56 — a `style_id`-bound paragraph must keep its `<w:pStyle>`
    /// binding after direct formatting, and the cascade must still resolve
    /// through it (pins the #29 cascade path against regressing here).
    #[test]
    fn apply_style_preserves_style_id_and_cascade_still_resolves() {
        let mut doc = DocumentTree::from_text("hello world");
        doc.styles.insert(
            "Heading1".to_string(),
            ParagraphStyle {
                id: "Heading1".to_string(),
                name: "Heading 1".to_string(),
                based_on: None,
                para: ParaProperties {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                },
                run: SpanStyle::default(),
                next: None,
            },
        );
        let mut para = doc.nth_paragraph(0).unwrap().clone();
        para.style_id = Some("Heading1".to_string());
        doc.blocks[0] = Block::Paragraph(para);

        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let style_id = doc.nth_paragraph(0).unwrap().style_id.clone();
        assert_eq!(
            style_id.as_deref(),
            Some("Heading1"),
            "style_id must survive a style apply"
        );
        assert_eq!(
            doc.resolve_style_cascade(style_id.as_deref()).alignment,
            Some(Alignment::Center),
            "the cascade must still resolve through the preserved style_id"
        );
    }

    #[test]
    fn overlapping_styles_split_and_merge() {
        let doc = DocumentTree::from_text("hello world");
        let red = SpanStyle {
            font_size: None,
            color: Some([255, 0, 0, 255]),
            ..Default::default()
        };
        let big = SpanStyle {
            font_size: Some(30.0),
            color: None,
            ..Default::default()
        };
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 8,
            },
            red.clone(),
        );
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 4,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 11,
            },
            big.clone(),
        );
        let spans = &doc.nth_paragraph(0).unwrap().spans;
        /* [0,4) red ; [4,8) red+big ; [8,11) big */
        assert_eq!(spans.len(), 3);
        assert_eq!((spans[0].start, spans[0].end), (0, 4));
        assert_eq!(spans[0].style, red);
        assert_eq!((spans[1].start, spans[1].end), (4, 8));
        assert_eq!(
            spans[1].style,
            SpanStyle {
                font_size: Some(30.0),
                color: Some([255, 0, 0, 255]),
                ..Default::default()
            }
        );
        assert_eq!((spans[2].start, spans[2].end), (8, 11));
        assert_eq!(spans[2].style, big);
    }

    /// Issue #84 — a grab bag is an opaque attachment on the span style:
    /// it rides every clone, never merges across spans that differ only
    /// by bag, and is never inherited by a formatting patch.
    #[test]
    fn grab_bag_survives_split_merge_and_formatting() {
        fn bag(frag: &str) -> Option<Box<GrabBag>> {
            let mut slot = None;
            GrabBag::push_into(&mut slot, frag.as_bytes().to_vec());
            slot
        }
        let bold = |b: Option<Box<GrabBag>>| SpanStyle {
            bold: Some(true),
            grab_bag: b,
            ..Default::default()
        };
        let para = Paragraph {
            text: "abcdef".into(),
            spans: vec![
                StyleRun {
                    start: 0,
                    end: 3,
                    style: bold(bag("<w:lang w:val=\"en-GB\"/>")),
                },
                StyleRun {
                    start: 3,
                    end: 6,
                    style: bold(bag("<w:lang w:val=\"ar-SA\"/>")),
                },
            ],
            ..Default::default()
        };

        /* A patch over both spans re-derives every interval; the two are
        identical in modeled fields but differ by bag, so they must NOT
        coalesce, and each keeps its own bag. */
        let italic = SpanStyle {
            italic: Some(true),
            ..Default::default()
        };
        let p = para.apply_style(0, 6, italic.clone());
        assert_eq!(p.spans.len(), 2, "byte-different bags never merge");
        assert_eq!(p.spans[0].style.italic, Some(true));
        assert_eq!(p.spans[0].style.grab_bag, bag("<w:lang w:val=\"en-GB\"/>"));
        assert_eq!(p.spans[1].style.grab_bag, bag("<w:lang w:val=\"ar-SA\"/>"));

        /* Byte-equal bags DO merge. */
        let mut same = para.clone();
        same.spans[1].style.grab_bag = bag("<w:lang w:val=\"en-GB\"/>");
        let p = same.apply_style(0, 6, italic);
        assert_eq!(p.spans.len(), 1, "byte-equal bags coalesce");
        assert_eq!(p.spans[0].style.grab_bag, bag("<w:lang w:val=\"en-GB\"/>"));

        /* Splitting inside a span clones the bag to both halves. */
        let (l, r) = para.split_at(1);
        assert_eq!(l.spans[0].style.grab_bag, bag("<w:lang w:val=\"en-GB\"/>"));
        assert_eq!(r.spans[0].style.grab_bag, bag("<w:lang w:val=\"en-GB\"/>"));
        assert_eq!(r.spans[1].style.grab_bag, bag("<w:lang w:val=\"ar-SA\"/>"));

        /* Merge precedence: the patch's bag wins when it has one, else the
        receiver keeps its own — a cascade baseline never carries one, so
        a direct `<w:rPr>` bag always comes through unchanged. */
        let base = bold(bag("<w:base/>"));
        assert_eq!(
            base.clone().merged_with(SpanStyle::default()).grab_bag,
            bag("<w:base/>")
        );
        assert_eq!(
            SpanStyle::default().merged_with(base.clone()).grab_bag,
            bag("<w:base/>")
        );
        assert_eq!(
            base.merged_with(bold(bag("<w:direct/>"))).grab_bag,
            bag("<w:direct/>")
        );
        let pp = ParaProperties {
            grab_bag: bag("<w:framePr/>"),
            ..Default::default()
        };
        assert_eq!(
            ParaProperties::default().merged_with(pp.clone()).grab_bag,
            bag("<w:framePr/>")
        );
        assert_eq!(
            pp.merged_with(ParaProperties::default()).grab_bag,
            bag("<w:framePr/>")
        );
    }

    /// Issue #84 — paragraph-level bags ride `props` through split /
    /// concat like every other paragraph property.
    #[test]
    fn paragraph_grab_bag_rides_split_and_concat() {
        let mut slot = None;
        GrabBag::push_into(&mut slot, b"<w:cnfStyle w:val=\"1\"/>".to_vec());
        let para = Paragraph {
            text: "hello".into(),
            props: ParaProperties {
                grab_bag: slot.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        let (l, r) = para.split_at(2);
        assert_eq!(l.props.grab_bag, slot);
        assert_eq!(r.props.grab_bag, slot);
        let joined = l.concat(&r);
        assert_eq!(joined.props.grab_bag, slot);
        assert_eq!(joined.text, "hello");
    }

    #[test]
    fn insert_shifts_spans() {
        let doc = DocumentTree::from_text("abcdef");
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 4,
            },
            SpanStyle {
                font_size: None,
                color: Some([1, 2, 3, 255]),
                ..Default::default()
            },
        );
        let doc = doc.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "XX",
        );
        let span = doc.nth_paragraph(0).unwrap().spans[0].clone();
        assert_eq!((span.start, span.end), (4, 6));
    }

    #[test]
    fn word_bounds_latin() {
        let p = Paragraph {
            text: "hello world".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        };
        assert_eq!(p.word_bounds(2), (0, 5));
        assert_eq!(p.word_bounds(0), (0, 5));
        assert_eq!(p.word_bounds(5), (5, 6)); // on the space
        assert_eq!(p.word_bounds(8), (6, 11));
        assert_eq!(p.word_bounds(11), (6, 11)); // end of text → last word
    }

    #[test]
    fn word_bounds_arabic() {
        /* "مرحبا بالعالم" — 5-char word, space, 7-char word; 2 bytes/char. */
        let p = Paragraph {
            text: "مرحبا بالعالم".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        };
        assert_eq!(p.word_bounds(4), (0, 10));
        assert_eq!(p.word_bounds(0), (0, 10));
        assert_eq!(p.word_bounds(12), (11, 25)); // mid-char offset clamps
    }

    #[test]
    fn word_bounds_empty() {
        let p = Paragraph {
            text: String::new(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        };
        assert_eq!(p.word_bounds(0), (0, 0));
    }

    #[test]
    fn delete_within_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 11,
            },
        );
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn delete_merges_paragraphs() {
        let d = DocumentTree::from_paragraphs(["abc".to_string(), "def".to_string()]);
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abcdef"));
    }

    #[test]
    fn delete_clips_spans() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            SpanStyle {
                font_size: Some(20.0),
                color: None,
                ..Default::default()
            },
        );
        let doc = doc.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
        );
        assert_eq!(doc.paragraph_text(0), Some("hel world"));
        let spans = &doc.nth_paragraph(0).unwrap().spans;
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
    }

    #[test]
    fn split_paragraph_in_two() {
        let d = DocumentTree::from_text("hello world");
        let d = d.split_paragraph(LogicalPos {
            path: BlockPath::top(0),
            offset: 5,
        });
        assert_eq!(d.paragraph_count(), 2);
        assert_eq!(d.paragraph_text(0), Some("hello"));
        assert_eq!(d.paragraph_text(1), Some(" world"));
    }

    #[test]
    fn prev_next_offset_utf8() {
        /* "a"=1 byte, "م"=2 bytes, "b"=1 byte → grapheme boundaries 0,1,3,4. */
        let p = Paragraph {
            text: "aمb".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        };
        assert_eq!(p.next_offset(0), 1);
        assert_eq!(p.next_offset(1), 3);
        assert_eq!(p.prev_offset(4), 3);
        assert_eq!(p.prev_offset(3), 1);
    }

    /// Audit gap B.H1 — `prev_offset` / `next_offset` step by UAX-#29
    /// extended grapheme cluster, not Unicode scalar. The Arabic letter
    /// `ي` plus FATHATAN diacritic `ً` forms one user-perceived
    /// character (two `char`s, four UTF-8 bytes); Backspace must
    /// remove the cluster atomically instead of leaving an orphaned
    /// combining mark behind.
    #[test]
    fn prev_next_offset_step_grapheme_cluster() {
        /* Byte map for "aيًb":
          0: 'a'                              (1 byte)
          1: 'ي'         start of cluster    (2 bytes)
          3: ARABIC FATHATAN U+064B          (2 bytes, combining)
          5: 'b'                              (1 byte)
          6: end
        The يً cluster spans bytes 1..5. */
        let p = Paragraph {
            text: "aيًb".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        };
        /* Forward from 'a' jumps over the whole يً cluster, not just 'ي'. */
        assert_eq!(p.next_offset(1), 5, "forward must skip the FATHATAN");
        /* Backward from 'b' jumps over both letters of the cluster. */
        assert_eq!(p.prev_offset(5), 1, "backward must skip the FATHATAN");
        /* Edges still pin / clamp. */
        assert_eq!(p.prev_offset(0), 0);
        assert_eq!(p.next_offset(6), 6);
    }

    #[test]
    fn text_range_within_and_across() {
        let d = DocumentTree::from_paragraphs(["hello world".to_string(), "second".to_string()]);
        assert_eq!(
            d.text_range(
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 0
                },
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 5
                },
            ),
            "hello"
        );
        assert_eq!(
            d.text_range(
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 6
                },
                LogicalPos {
                    path: BlockPath::top(1),
                    offset: 6
                },
            ),
            "world\nsecond"
        );
        /* reversed args normalize to document order */
        assert_eq!(
            d.text_range(
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 5
                },
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 0
                },
            ),
            "hello"
        );
    }

    #[test]
    fn apply_style_bold_italic_underline() {
        let doc = DocumentTree::from_text("hello world");
        /* Apply bold over [0,5). */
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(doc.nth_paragraph(0).unwrap().spans.len(), 1);
        assert_eq!(
            doc.nth_paragraph(0).unwrap().spans[0].style.bold,
            Some(true)
        );
        /* Overlay italic + underline on the same range — they merge in. */
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            SpanStyle {
                italic: Some(true),
                underline: Some(UnderlineStyle::Single),
                ..Default::default()
            },
        );
        let style = doc.nth_paragraph(0).unwrap().style_at(2);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(UnderlineStyle::Single));
        /* Outside the styled range — unstyled. */
        assert_eq!(
            doc.nth_paragraph(0).unwrap().style_at(8),
            SpanStyle::default()
        );
    }

    #[test]
    fn undo_redo_cycle() {
        let initial = DocumentTree::from_text("abc");
        let mut undo = UndoStack::new(initial.clone(), 16);

        let d2 = initial.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            "def",
        );
        undo.push(d2.clone());
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));

        let d3 = d2.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 6,
            },
            "ghi",
        );
        undo.push(d3.clone());
        assert_eq!(undo.current().paragraph_text(0), Some("abcdefghi"));

        undo.undo();
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));
        undo.undo();
        assert_eq!(undo.current().paragraph_text(0), Some("abc"));
        assert!(!undo.can_undo());

        undo.redo();
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));
    }

    #[test]
    fn set_alignment_marks_spanned_paragraphs() {
        let d = DocumentTree::from_paragraphs(["a".into(), "b".into(), "c".into()]);
        let d = d.set_alignment(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            Alignment::Center,
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        assert_eq!(
            d.nth_paragraph(1).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        /* outside the range — untouched */
        assert_eq!(d.nth_paragraph(2).unwrap().props.alignment, None);
    }

    #[test]
    fn set_direction_marks_spanned_paragraphs() {
        let d = DocumentTree::from_paragraphs(["a".into(), "b".into(), "c".into()]);
        let d = d.set_direction(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            TextDirection::Rtl,
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.direction,
            Some(TextDirection::Rtl)
        );
        assert_eq!(
            d.nth_paragraph(1).unwrap().props.direction,
            Some(TextDirection::Rtl)
        );
        /* outside the range — untouched */
        assert_eq!(d.nth_paragraph(2).unwrap().props.direction, None);
    }

    #[test]
    fn set_paragraph_indent_allows_negative_outdent() {
        /* Bug B — negative `<w:start>` / `<w:end>` are first-class outdents
        (ECMA-376 ST_SignedTwipsMeasure). The `.max(0.0)` floor used to
        clobber them to zero, making the grey-margin drag a no-op. */
        let d = DocumentTree::from_paragraphs(["a".into(), "b".into()]);
        let d = d.set_paragraph_indent(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            -18.0, // start: 18 pt outdent
            -6.0,  // end: 6 pt outdent
            -12.0, // first-line: negative ⇒ hanging
        );
        let ind = d.nth_paragraph(0).unwrap().props.indent;
        assert_eq!(ind.start_twips, -360, "negative start survives (−18 pt)");
        assert_eq!(ind.end_twips, -120, "negative end survives (−6 pt)");
        /* first_line stays signed only through the firstLine/hanging split:
        the magnitude is stored non-negative in `hanging_twips`. */
        assert_eq!(ind.first_line_twips, 0);
        assert_eq!(
            ind.hanging_twips, 240,
            "negative first-line ⇒ hanging (12 pt)"
        );
    }

    #[test]
    fn alignment_survives_text_edits() {
        let d = DocumentTree::from_text("hello world");
        let d = d.set_alignment(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            Alignment::End,
        );
        /* insertion clones the paragraph in place — alignment rides along */
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "X",
        );
        assert_eq!(d.paragraph_text(0), Some("Xhello world"));
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::End)
        );
        /* a style change preserves alignment */
        let d = d.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::End)
        );
        /* and so does a deletion */
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 1,
            },
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::End)
        );
    }

    #[test]
    fn split_paragraph_inherits_alignment() {
        let d = DocumentTree::from_text("hello world");
        let d = d.set_alignment(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            Alignment::Center,
        );
        let d = d.split_paragraph(LogicalPos {
            path: BlockPath::top(0),
            offset: 5,
        });
        assert_eq!(d.paragraph_count(), 2);
        /* both halves carry the original paragraph's alignment */
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        assert_eq!(
            d.nth_paragraph(1).unwrap().props.alignment,
            Some(Alignment::Center)
        );
    }

    #[test]
    fn merge_keeps_first_paragraph_alignment() {
        let d = DocumentTree::from_paragraphs(["abc".into(), "def".into()]);
        let d = d.set_alignment(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            Alignment::Center,
        );
        let d = d.set_alignment(
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            Alignment::End,
        );
        /* deleting the paragraph break merges the two */
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abcdef"));
        /* the surviving paragraph keeps the first paragraph's alignment */
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
    }

    #[test]
    fn insert_multiline_single_line_is_plain_insert() {
        let d = DocumentTree::from_text("abcd");
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            "XY",
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abXYcd"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(0),
                offset: 4
            }
        );
    }

    #[test]
    fn insert_multiline_splits_into_paragraphs() {
        let d = DocumentTree::from_text("abcd");
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            "L0\nL1\nL2",
        );
        assert_eq!(d.paragraph_count(), 3);
        /* the original paragraph splits around the caret; the tail rides the
        last pasted line's paragraph */
        assert_eq!(d.paragraph_text(0), Some("abL0"));
        assert_eq!(d.paragraph_text(1), Some("L1"));
        assert_eq!(d.paragraph_text(2), Some("L2cd"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(2),
                offset: 2
            }
        );
    }

    #[test]
    fn insert_multiline_normalizes_crlf_and_cr() {
        let d = DocumentTree::from_text("");
        let (d, _) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "a\r\nb\rc",
        );
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("a"));
        assert_eq!(d.paragraph_text(1), Some("b"));
        assert_eq!(d.paragraph_text(2), Some("c"));
    }

    #[test]
    fn insert_multiline_trailing_newline_makes_empty_paragraph() {
        let d = DocumentTree::from_text("xy");
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            "Z\n",
        );
        assert_eq!(d.paragraph_count(), 2);
        assert_eq!(d.paragraph_text(0), Some("xyZ"));
        assert_eq!(d.paragraph_text(1), Some(""));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0
            }
        );
    }

    #[test]
    fn insert_multiline_into_second_paragraph() {
        let d = DocumentTree::from_paragraphs(["first".to_string(), "second".to_string()]);
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(1),
                offset: 3,
            },
            "A\nB",
        );
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("first"));
        assert_eq!(d.paragraph_text(1), Some("secA"));
        assert_eq!(d.paragraph_text(2), Some("Bond"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(2),
                offset: 1
            }
        );
    }

    #[test]
    fn slice_single_paragraph_clips_and_shifts_spans() {
        /* "hello world" with bold over "world" (bytes 6-11). */
        let bold = SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        let para = Paragraph {
            text: "hello world".into(),
            spans: vec![StyleRun {
                start: 6,
                end: 11,
                style: bold.clone(),
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        /* Slice "lo wor" (bytes 3-9) — the bold span clips to 3-6, local. */
        let cut = doc.slice(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 9,
            },
        );
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].text, "lo wor");
        assert_eq!(
            cut[0].spans,
            vec![StyleRun {
                start: 3,
                end: 6,
                style: bold,
            }]
        );
    }

    #[test]
    fn insert_rich_single_paragraph_merges_inline() {
        let doc = DocumentTree::from_text("hello world");
        let frag = vec![Paragraph {
            text: "BRAVE ".into(),
            spans: vec![],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        }];
        let (out, caret) = doc.insert_rich(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 6,
            },
            &frag,
        );
        assert_eq!(out.paragraph_count(), 1);
        assert_eq!(out.paragraph_text(0), Some("hello BRAVE world"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(0),
                offset: 12
            }
        );
    }

    #[test]
    fn insert_rich_multi_paragraph_splices_and_keeps_spans() {
        let doc = DocumentTree::from_text("ABCD");
        let bold = SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        let frag = vec![
            Paragraph {
                text: "one".into(),
                spans: vec![],
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                resolved_list_indent: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
                section_end: None,
                bookmarks: Vec::new(),
                body_xml: None,
                source_markup: None,
                mark_revision: None,
            },
            Paragraph {
                text: "two".into(),
                spans: vec![StyleRun {
                    start: 0,
                    end: 3,
                    style: bold,
                }],
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                resolved_list_indent: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
                section_end: None,
                bookmarks: Vec::new(),
                body_xml: None,
                source_markup: None,
                mark_revision: None,
            },
        ];
        let (out, caret) = doc.insert_rich(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            &frag,
        );
        assert_eq!(out.paragraph_count(), 2);
        assert_eq!(out.paragraph_text(0), Some("ABone"));
        assert_eq!(out.paragraph_text(1), Some("twoCD"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(1),
                offset: 3
            }
        );
        assert_eq!(out.nth_paragraph(1).unwrap().style_at(0).bold, Some(true));
        assert_eq!(out.nth_paragraph(1).unwrap().style_at(3).bold, None);
    }

    /* ---- Phase 5 PR 3: table command suite ------------------------- */

    #[test]
    fn insert_table_synthesises_dirty_block_with_no_source() {
        let d = DocumentTree::from_text("hello");
        let d = d.insert_table(BlockPath::top(1), 2, 3);
        /* 3 blocks: "hello" + table + auto-appended trailing empty
        paragraph (the OOXML-mandated escape paragraph). */
        assert_eq!(d.blocks.len(), 3);
        let t = d.blocks[1].as_table().expect("Block::Table");
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 3);
        assert_eq!(t.grid.len(), 3);
        assert!(t.dirty, "synthesised tables must regen on save");
        assert!(t.source_xml.is_none());
        assert!(
            d.blocks[2]
                .as_paragraph()
                .is_some_and(|p| p.text.is_empty()),
            "trailing escape paragraph"
        );
    }

    /// PR 4 visibility fix — a freshly inserted table must paint:
    /// each cell carries an empty paragraph placeholder so layout has
    /// something to measure, and Word-default 0.5pt single black
    /// borders on every cell + the table outer perimeter so the user
    /// sees the table on the canvas immediately.
    #[test]
    fn insert_table_seeds_placeholder_paragraph_and_default_borders() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 3, 3);
        let t = d.blocks[0].as_table().expect("Block::Table");
        for (r, row) in t.rows.iter().enumerate() {
            for (c, cell) in row.cells.iter().enumerate() {
                assert!(
                    !cell.blocks.is_empty(),
                    "cell ({r},{c}) needs a placeholder paragraph for layout"
                );
                let p = cell.blocks[0]
                    .as_paragraph()
                    .expect("cell placeholder is a Paragraph");
                assert!(p.text.is_empty(), "placeholder is the empty paragraph");
                let borders = cell
                    .props
                    .borders
                    .as_ref()
                    .expect("cell needs default Word borders");
                assert!(borders.top.is_some());
                assert!(borders.bottom.is_some());
                assert!(borders.left.is_some());
                assert!(borders.right.is_some());
            }
        }
        let outer = t.props.borders.as_ref().expect("outer borders");
        assert!(outer.top.is_some() && outer.bottom.is_some());
    }

    /// PR 4 follow-up — Bug 4. A fresh table's grid must fit the A4
    /// content area: total grid twips ≤ DEFAULT_A4_CONTENT_TWIPS so
    /// the layout pass produces a table width ≤ page content width.
    /// 451 layout-px content × 15 twips/layout-px = 6765 twips.
    #[test]
    fn insert_table_grid_fits_a4_content_width() {
        for cols in 1u32..=8u32 {
            let d = DocumentTree::new().insert_table(BlockPath::top(0), 1, cols);
            let t = d.blocks[0].as_table().unwrap();
            assert_eq!(t.grid.len(), cols as usize);
            let total: i32 = t.grid.iter().sum();
            assert!(
                total <= DEFAULT_A4_CONTENT_TWIPS,
                "{cols}-col grid totals {total} twips, exceeds A4 content {DEFAULT_A4_CONTENT_TWIPS}"
            );
            assert!(
                total >= DEFAULT_A4_CONTENT_TWIPS - cols as i32,
                "{cols}-col grid totals {total} twips, leaves >1 twip/col slack"
            );
        }
    }

    /// Inserting a column re-divides the grid so total stays under
    /// the A4 content area instead of pushing the table past the
    /// right margin.
    #[test]
    fn insert_column_redivides_grid_within_content_width() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 1, 3);
        let d = d.insert_column(BlockPath::top(0), 0);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.grid.len(), 4);
        let total: i32 = t.grid.iter().sum();
        assert!(total <= DEFAULT_A4_CONTENT_TWIPS);
    }

    #[test]
    fn insert_row_appends_with_matching_column_count() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 2);
        let d = d.insert_row(BlockPath::top(1), 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].cells.len(), 2);
    }

    /// Sprint 2 (UI Edition) hotfix — prepending a row at index 0
    /// must NOT underflow the wire `u32` or be silently coerced to
    /// "insert after row 0". `insert_row(path, 0)` lands the new row
    /// at index 0; the original row shifts to index 1.
    /// Issue #25 — `set_line_spacing` stores Word's Auto multiplier as
    /// 240-ths twips on props AND direct_overrides; `<= 0` clears.
    #[test]
    fn set_line_spacing_stores_auto_multiplier_and_clears() {
        let d = DocumentTree::from_text("hi");
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 2);
        let d = d.set_line_spacing(start.clone(), end.clone(), 1.15);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.props.line_height, Some(LineHeight::Auto { twips: 276 }));
        assert_eq!(
            p.direct_overrides.line_height,
            Some(LineHeight::Auto { twips: 276 })
        );
        let d = d.set_line_spacing(start, end, 0.0);
        assert_eq!(d.blocks[0].as_paragraph().unwrap().props.line_height, None);
    }

    /// Issue #145 — `SetTabStops` must not clobber an existing leader
    /// when the wire omits it (a caller that only edits position, like
    /// the Ruler drag path, and therefore sends `leader: None`). `None`
    /// inherits per-index from the paragraph's *current* stops; an
    /// explicit `Some` sets or clears; a genuinely new index (beyond
    /// the old list) has nothing to inherit and resolves to
    /// `TabLeader::None`.
    #[test]
    fn set_tab_stops_preserves_leader_when_wire_omits_it() {
        let d = DocumentTree::from_text("hi");
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 2);

        /* Seed a dot-leadered right tab — the TOC entry shape. */
        let d = d.set_tab_stops(
            start.clone(),
            end.clone(),
            vec![TabStopPatch {
                position_pt: 400.0,
                kind: TabKind::Right,
                leader: Some(TabLeader::Dot),
            }],
        );
        let stop = d.blocks[0].as_paragraph().unwrap().props.tab_stops[0];
        assert_eq!(stop.leader, TabLeader::Dot);

        /* The Ruler drags the same stop to a new position without
        itself tracking leaders, so it dispatches `leader: None`. The
        dot leader must survive — this is the bug #145 fixes. */
        let d = d.set_tab_stops(
            start.clone(),
            end.clone(),
            vec![TabStopPatch {
                position_pt: 420.0,
                kind: TabKind::Right,
                leader: None,
            }],
        );
        let stop = d.blocks[0].as_paragraph().unwrap().props.tab_stops[0];
        assert_eq!(stop.position_pt, 420.0, "position must still move");
        assert_eq!(
            stop.leader,
            TabLeader::Dot,
            "leader must survive an omitted patch"
        );

        /* An explicit `Some(TabLeader::None)` is the deliberate clear. */
        let d = d.set_tab_stops(
            start.clone(),
            end.clone(),
            vec![TabStopPatch {
                position_pt: 420.0,
                kind: TabKind::Right,
                leader: Some(TabLeader::None),
            }],
        );
        let stop = d.blocks[0].as_paragraph().unwrap().props.tab_stops[0];
        assert_eq!(stop.leader, TabLeader::None);

        /* A brand-new stop at an index beyond the old list has nothing
        to inherit from — `None` must not pick up a stale leader from
        some other index. */
        let d = d.set_tab_stops(
            start,
            end,
            vec![
                TabStopPatch {
                    position_pt: 100.0,
                    kind: TabKind::Left,
                    leader: Some(TabLeader::Hyphen),
                },
                TabStopPatch {
                    position_pt: 420.0,
                    kind: TabKind::Right,
                    leader: None,
                },
            ],
        );
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.props.tab_stops[0].leader, TabLeader::Hyphen);
        assert_eq!(p.props.tab_stops[1].leader, TabLeader::None);
    }

    /// Issue #50 — ToggleList must stamp BOTH the marker text and the
    /// numbering level's indent; the indent is what layout consumes to
    /// park the bullet in the hanging gutter instead of underneath the
    /// first text glyph.
    #[test]
    fn toggle_list_stamps_marker_and_level_indent() {
        let d = DocumentTree::from_text("alpha");
        let pos = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.toggle_list_on_range(pos.clone(), pos, numbering::ListSynthesisKind::Bullet);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.resolved_marker.as_deref(), Some("\u{2022}"));
        assert_eq!(
            p.resolved_list_indent,
            Some(Indent {
                start_twips: 720,
                end_twips: 0,
                first_line_twips: 0,
                hanging_twips: 360,
            }),
            "level-0 stock indent must ride along with the marker"
        );
    }

    /// Issue #42 — Tab (delta +1) demotes a bulleted list item's outline
    /// level and the marker re-resolves against the deeper stock level's
    /// glyph (level 0 "•" → level 1 "◦"), with its indent following.
    #[test]
    fn change_list_level_demotes_bullet_marker_and_indent() {
        let d = DocumentTree::from_text("alpha");
        let pos = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.toggle_list_on_range(
            pos.clone(),
            pos.clone(),
            numbering::ListSynthesisKind::Bullet,
        );
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().list_item.unwrap().ilvl,
            0
        );
        let d = d.change_list_level_on_range(pos.clone(), pos.clone(), 1);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.list_item.unwrap().ilvl, 1);
        assert_eq!(p.resolved_marker.as_deref(), Some("\u{25E6}"));
        assert_eq!(
            p.resolved_list_indent,
            Some(Indent {
                start_twips: 1080,
                end_twips: 0,
                first_line_twips: 0,
                hanging_twips: 360,
            }),
            "level-1 stock indent must follow the demote"
        );
        /* Shift+Tab (delta -1) promotes back to level 0. */
        let d = d.change_list_level_on_range(pos.clone(), pos, -1);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.list_item.unwrap().ilvl, 0);
        assert_eq!(p.resolved_marker.as_deref(), Some("\u{2022}"));
    }

    /// Issue #42 — level clamps at the top (promote past 0 stays 0) and
    /// the bottom (demote past the 9 stock levels stays at 8), matching
    /// Word's outline-level bounds.
    #[test]
    fn change_list_level_clamps_at_bounds() {
        let d = DocumentTree::from_text("alpha");
        let pos = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.toggle_list_on_range(
            pos.clone(),
            pos.clone(),
            numbering::ListSynthesisKind::Bullet,
        );
        let d = d.change_list_level_on_range(pos.clone(), pos.clone(), -5);
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().list_item.unwrap().ilvl,
            0
        );
        let d = d.change_list_level_on_range(pos.clone(), pos.clone(), 20);
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().list_item.unwrap().ilvl,
            8
        );
        let d = d.change_list_level_on_range(pos.clone(), pos, 3);
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().list_item.unwrap().ilvl,
            8,
            "demoting past the deepest stock level stays clamped at 8"
        );
    }

    /// Issue #42 — a non-list paragraph is left untouched (no `list_item`
    /// to demote/promote), matching the Tab key's fallback-to-tab-char
    /// contract in the shell.
    #[test]
    fn change_list_level_is_noop_on_non_list_paragraph() {
        let d = DocumentTree::from_text("alpha");
        let pos = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.change_list_level_on_range(pos.clone(), pos, 1);
        assert!(d.blocks[0].as_paragraph().unwrap().list_item.is_none());
    }

    /// Issue #50 — Enter inside a numbered list renumbers the tail;
    /// `Paragraph::split_at` alone clones "1." onto both halves.
    #[test]
    fn split_inside_numbered_list_renumbers_the_tail() {
        let d = DocumentTree::from_text("onetwo");
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 6);
        let d = d.toggle_list_on_range(start, end, numbering::ListSynthesisKind::Number);
        assert_eq!(
            d.blocks[0]
                .as_paragraph()
                .unwrap()
                .resolved_marker
                .as_deref(),
            Some("1.")
        );
        let d = d.split_paragraph(LogicalPos::new(BlockPath::top(0), 3));
        let markers: Vec<Option<&str>> = d
            .blocks
            .iter()
            .filter_map(|b| b.as_paragraph())
            .map(|p| p.resolved_marker.as_deref())
            .collect();
        assert_eq!(
            markers,
            vec![Some("1."), Some("2.")],
            "split must renumber, not duplicate the head's marker"
        );
    }

    /// Issue #50 — merging across the break (delete_range) and clearing
    /// list membership both re-resolve the remaining markers.
    #[test]
    fn merge_and_clear_renumber_following_list_items() {
        /* three numbered paragraphs: one / two / three */
        let d = DocumentTree::from_text("one");
        let d = d.split_paragraph(LogicalPos::new(BlockPath::top(0), 3));
        let d = d.insert_text(LogicalPos::new(BlockPath::top(1), 0), "two");
        let d = d.split_paragraph(LogicalPos::new(BlockPath::top(1), 3));
        let d = d.insert_text(LogicalPos::new(BlockPath::top(2), 0), "three");
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(2), 5);
        let d = d.toggle_list_on_range(start, end, numbering::ListSynthesisKind::Number);
        /* delete across the first paragraph break — "one" and "two"
        merge; the survivors renumber 1. / 2. */
        let d = d.delete_range(
            LogicalPos::new(BlockPath::top(0), 3),
            LogicalPos::new(BlockPath::top(1), 0),
        );
        let markers: Vec<Option<String>> = d
            .blocks
            .iter()
            .filter_map(|b| b.as_paragraph())
            .map(|p| p.resolved_marker.clone())
            .collect();
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].as_deref(), Some("1."));
        assert_eq!(markers[1].as_deref(), Some("2."));
        /* clearing the first item renumbers the rest from 1. */
        let p0 = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.clear_list_item_on_range(p0.clone(), p0);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert!(p.resolved_marker.is_none() && p.resolved_list_indent.is_none());
        assert_eq!(
            d.blocks[1]
                .as_paragraph()
                .unwrap()
                .resolved_marker
                .as_deref(),
            Some("1."),
            "the surviving list item restarts at 1."
        );
    }

    #[test]
    fn insert_row_at_zero_prepends_no_underflow() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 2, 2);
        let before = d.blocks[0].as_table().unwrap().clone();
        let d = d.insert_row(BlockPath::top(0), 0);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.rows.len(), 3, "row was not inserted");
        assert_eq!(
            t.rows[1].cells.len(),
            before.rows[0].cells.len(),
            "row 1 should be the original row 0 after prepend",
        );
        /* Appending at `at == row_count` lands at the end. */
        let d = d.insert_row(BlockPath::top(0), 3);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.rows.len(), 4);
    }

    /// Sprint 2 (UI Edition) hotfix — prepending a column at index 0
    /// is the mirror invariant for [`insert_row_at_zero_prepends_no_underflow`].
    #[test]
    fn insert_column_at_zero_prepends_no_underflow() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 2, 2);
        let d = d.insert_column(BlockPath::top(0), 0);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.grid.len(), 3);
        assert!(t.rows.iter().all(|r| r.cells.len() == 3));
    }

    #[test]
    fn insert_column_widens_grid_and_every_row() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 2, 2);
        let d = d.insert_column(BlockPath::top(1), 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.grid.len(), 3);
        assert!(t.rows.iter().all(|r| r.cells.len() == 3));
    }

    #[test]
    fn delete_row_and_column() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 3);
        let d = d.delete_row(BlockPath::top(1), 1);
        let d = d.delete_column(BlockPath::top(1), 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.grid.len(), 2);
        assert!(t.rows.iter().all(|r| r.cells.len() == 2));
    }

    #[test]
    fn merge_cells_horizontal_sets_grid_span_and_drops_partners() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 0, 2);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells.len(), 1);
        assert_eq!(t.rows[0].cells[0].props.grid_span, 3);
        assert_eq!(t.rows[0].cells[0].props.v_merge, VMergeRole::None);
    }

    #[test]
    fn merge_cells_vertical_flips_continue_rows() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 2);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 2, 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells[0].props.v_merge, VMergeRole::Restart);
        assert_eq!(t.rows[1].cells[0].props.v_merge, VMergeRole::Continue);
        assert_eq!(t.rows[2].cells[0].props.v_merge, VMergeRole::Continue);
    }

    /// A rectangular merge must produce Word's on-disk shape: every
    /// member row collapses to ONE spanning cell (partners physically
    /// removed), so per-row grid-column accounting stays exact for
    /// cells to the right of the merge.
    #[test]
    fn merge_cells_rectangular_collapses_continuation_rows() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 1, 1);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells.len(), 2, "top row collapses 3 → 2 cells");
        assert_eq!(t.rows[0].cells[0].props.grid_span, 2);
        assert_eq!(t.rows[0].cells[0].props.v_merge, VMergeRole::Restart);
        assert_eq!(t.rows[0].cells[1].props.grid_span.max(1), 1);
        assert_eq!(
            t.rows[1].cells.len(),
            2,
            "continuation row collapses 3 → 2 cells"
        );
        assert_eq!(t.rows[1].cells[0].props.grid_span, 2);
        assert_eq!(t.rows[1].cells[0].props.v_merge, VMergeRole::Continue);
        assert_eq!(
            t.rows[1].cells[1].props.v_merge,
            VMergeRole::None,
            "cell right of the merge is untouched"
        );
        assert_eq!(t.rows[2].cells.len(), 3, "row below the merge untouched");
    }

    /// `split_cell` is the true inverse of `merge_cells`: it restores
    /// the horizontally-removed partners in the owner row AND in every
    /// vertical continuation row, matched by starting grid column.
    #[test]
    fn split_cell_restores_merged_partners() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 1, 1);
        let d = d.split_cell(BlockPath::top(1), 0, 0);
        let t = d.blocks[1].as_table().unwrap();
        for (r, row) in t.rows.iter().enumerate() {
            assert_eq!(row.cells.len(), 3, "row {r} must be back to 3 cells");
            for (c, cell) in row.cells.iter().enumerate() {
                assert_eq!(cell.props.grid_span.max(1), 1, "cell {r},{c} span reset");
                assert_eq!(
                    cell.props.v_merge,
                    VMergeRole::None,
                    "cell {r},{c} vMerge cleared"
                );
            }
        }
    }

    /// Horizontal-only merge + split round-trips the row shape.
    #[test]
    fn split_cell_restores_horizontal_only_merge() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 0, 2);
        assert_eq!(d.blocks[1].as_table().unwrap().rows[0].cells.len(), 1);
        let d = d.split_cell(BlockPath::top(1), 0, 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells.len(), 3);
        assert!(
            t.rows[0]
                .cells
                .iter()
                .all(|c| c.props.grid_span.max(1) == 1 && c.props.v_merge == VMergeRole::None)
        );
    }

    #[test]
    fn set_table_bidi_visual_flips_flag_and_dirty() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 3);
        assert!(!d.blocks[1].as_table().unwrap().props.bidi_visual);
        let d = d.set_table_bidi_visual(BlockPath::top(1), true);
        let t = d.blocks[1].as_table().unwrap();
        assert!(t.props.bidi_visual);
        assert!(t.dirty);
        assert!(t.source_xml.is_none());
        /* Logical cell order is untouched — the flag is purely visual. */
        assert_eq!(t.rows[0].cells.len(), 3);
        let d = d.set_table_bidi_visual(BlockPath::top(1), false);
        assert!(!d.blocks[1].as_table().unwrap().props.bidi_visual);
        /* A non-table path is a no-op. */
        let d2 = d.set_table_bidi_visual(BlockPath::top(0), true);
        assert!(d2.blocks[0].as_table().is_none());
    }

    #[test]
    fn set_cell_shading_and_borders_flip_dirty() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 1);
        /* The previously-inserted table starts dirty (synthesised); force
        a "clean" reset to exercise the dirty-flip invariant on a
        passthrough-eligible table. */
        let mut blocks = d.blocks.clone();
        if let Some(t) = blocks[1].as_table_mut() {
            t.dirty = false;
            t.source_xml = Some(b"<w:tbl/>".to_vec());
        }
        let d = DocumentTree {
            blocks,
            body_section: SectionProps::default(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnote_stories: std::collections::HashMap::new(),
            endnote_stories: std::collections::HashMap::new(),
            footnote_props: NoteProps::default(),
            endnote_props: NoteProps::default(),
            notes_dirty: NotesDirty::default(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
            hf_dirty: HfDirty::default(),
            settings_dirty: false,
            document_root_attrs: Vec::new(),
            part_root_attrs: Default::default(),
            document_envelope: Default::default(),
            source_package: None,
        };
        let d = d.set_cell_shading(BlockPath::top(1), 0, 0, Some([0xFF, 0, 0, 0xFF]));
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells[0].props.shading, Some([0xFF, 0, 0, 0xFF]));
        assert!(t.dirty, "shading edit must flip dirty");
        assert!(t.source_xml.is_none(), "shading edit must drop source");

        let d = d.set_cell_borders(
            BlockPath::top(1),
            0,
            0,
            CellBorders {
                top: Some(BorderStroke {
                    style: BorderStyle::Single,
                    size_eighth_pt: 8,
                    color: Some([0, 0, 0xFF, 0xFF]),
                }),
                ..Default::default()
            },
        );
        let t = d.blocks[1].as_table().unwrap();
        assert!(t.rows[0].cells[0].props.borders.is_some());
    }

    #[test]
    fn delete_table_removes_top_level_block() {
        let d = DocumentTree::from_text("before").insert_table(BlockPath::top(1), 1, 1);
        /* 3 blocks: "before" + table + auto-trailing empty paragraph
        (the OOXML-mandated escape paragraph `insert_table` appends). */
        assert_eq!(d.blocks.len(), 3);
        let d = d.delete_table(BlockPath::top(1));
        /* 2 blocks remain: "before" + the trailing empty paragraph
        (delete_table removes only the Table block at idx 1). */
        assert_eq!(d.blocks.len(), 2);
        assert!(d.blocks[0].as_paragraph().is_some());
        assert!(
            d.blocks[1]
                .as_paragraph()
                .is_some_and(|p| p.text.is_empty())
        );
    }

    /* ---- issue #27: threaded comment replies ----------------------- */

    /// Anchor a top-level comment on a non-trivial range so the reply's
    /// cloned range is distinguishable from a default.
    fn doc_with_comment() -> (DocumentTree, u32) {
        let doc = DocumentTree::from_text("hello world");
        doc.insert_comment(
            LogicalPos::new(BlockPath::top(0), 2),
            LogicalPos::new(BlockPath::top(0), 7),
            "root comment".into(),
            "Alice".into(),
            "2026-07-01T00:00:00Z".into(),
        )
    }

    #[test]
    fn reply_to_comment_mints_id_sets_parent_and_clones_range() {
        let (doc, parent) = doc_with_comment();
        let (doc, reply) = doc
            .reply_to_comment(
                parent,
                "reply body".into(),
                "Bob".into(),
                "2026-07-02T00:00:00Z".into(),
            )
            .expect("parent exists");
        assert_eq!(reply, parent + 1, "next sequential id minted");
        let def = doc.comment_defs.get(&reply).expect("reply def");
        assert_eq!(def.parent_id, Some(parent));
        assert_eq!(def.paragraphs, vec!["reply body".to_string()]);
        assert_eq!(def.author, "Bob");
        assert!(!def.resolved);
        assert!(def.first_para_id.is_none());
        /* The reply's range is a clone of the parent's span. */
        let pr = doc
            .comment_ranges
            .iter()
            .find(|r| r.id == parent)
            .expect("parent range");
        let rr = doc
            .comment_ranges
            .iter()
            .find(|r| r.id == reply)
            .expect("reply range");
        assert_eq!(rr.start, pr.start);
        assert_eq!(rr.end, pr.end);
    }

    #[test]
    fn reply_to_unknown_parent_returns_none() {
        let (doc, _parent) = doc_with_comment();
        assert!(
            doc.reply_to_comment(999, "x".into(), "Bob".into(), "d".into())
                .is_none()
        );
    }

    #[test]
    fn delete_comment_cascades_to_transitive_replies() {
        let (doc, parent) = doc_with_comment();
        let (doc, reply) = doc
            .reply_to_comment(parent, "reply".into(), "Bob".into(), "d".into())
            .expect("parent exists");
        let (doc, nested) = doc
            .reply_to_comment(reply, "reply to reply".into(), "Carol".into(), "d".into())
            .expect("reply exists");
        assert_eq!(doc.comment_defs.len(), 3);
        assert_eq!(doc.comment_ranges.len(), 3);
        let doc = doc.delete_comment(parent);
        assert!(doc.comment_defs.is_empty(), "cascade removed the thread");
        assert!(doc.comment_ranges.is_empty());
        let _ = nested;
    }

    #[test]
    fn deleting_a_reply_leaves_the_parent() {
        let (doc, parent) = doc_with_comment();
        let (doc, reply) = doc
            .reply_to_comment(parent, "reply".into(), "Bob".into(), "d".into())
            .expect("parent exists");
        let doc = doc.delete_comment(reply);
        assert!(doc.comment_defs.contains_key(&parent));
        assert!(!doc.comment_defs.contains_key(&reply));
        assert!(doc.comment_ranges.iter().any(|r| r.id == parent));
        assert!(!doc.comment_ranges.iter().any(|r| r.id == reply));
    }

    /* ================================================================
    Issues #70 / #43 / #73 / #74 — Stage 1 engine core: inheritance
    resolver, marker-drop backfill, field-overlay survival, field
    authoring, story-aware counts, role setters.
    ================================================================ */

    fn hf(default: Option<&str>, first: Option<&str>, even: Option<&str>) -> HeaderFooterRefs {
        HeaderFooterRefs {
            default: default.map(str::to_string),
            first: first.map(str::to_string),
            even: even.map(str::to_string),
        }
    }

    #[test]
    fn hf_inheritance_folds_forward_per_role_slot() {
        let sections = vec![
            Section {
                header_refs: hf(Some("h1"), Some("f1"), None),
                ..Default::default()
            },
            Section {
                header_refs: hf(None, None, Some("e2")),
                ..Default::default()
            },
            Section {
                header_refs: hf(Some("h3"), None, None),
                ..Default::default()
            },
        ];
        let resolved = resolve_hf_inheritance(&sections);
        /* Section 0 — own slots only; unset even stays blank. */
        assert_eq!(resolved[0].0.default.as_deref(), Some("h1"));
        assert_eq!(resolved[0].0.first.as_deref(), Some("f1"));
        assert_eq!(resolved[0].0.even, None);
        /* Section 1 — inherits default+first, owns even. */
        assert_eq!(resolved[1].0.default.as_deref(), Some("h1"));
        assert_eq!(resolved[1].0.first.as_deref(), Some("f1"));
        assert_eq!(resolved[1].0.even.as_deref(), Some("e2"));
        /* Section 2 — own default WINS over the carried h1; first and
        even carry through the whole chain. */
        assert_eq!(resolved[2].0.default.as_deref(), Some("h3"));
        assert_eq!(resolved[2].0.first.as_deref(), Some("f1"));
        assert_eq!(resolved[2].0.even.as_deref(), Some("e2"));
    }

    #[test]
    fn hf_resolve_is_blank_not_default_fallback() {
        /* Issue #70 removed the same-section role→Default fallback:
        titlePg with no first ref shows a BLANK first-page header
        (Word-observed), never the Default content. */
        let refs = hf(Some("d"), None, None);
        assert_eq!(refs.resolve(HeaderFooterRole::Default), Some("d"));
        assert_eq!(refs.resolve(HeaderFooterRole::First), None);
        assert_eq!(refs.resolve(HeaderFooterRole::Even), None);
    }

    #[test]
    fn insert_break_clears_right_terminal_refs_left_keeps_copy() {
        let mut d = DocumentTree::from_text("hello world");
        d.body_section.header_refs.default = Some("rIdA".into());
        let broken =
            d.insert_section_break_at(LogicalPos::new(BlockPath::top(0), 5), SectionType::NextPage);
        let sections = broken.effective_sections();
        assert_eq!(sections.len(), 2);
        assert_eq!(
            sections[0].header_refs.default.as_deref(),
            Some("rIdA"),
            "left half owns the covering section's original refs"
        );
        assert!(
            sections[1].header_refs.is_empty(),
            "second half is born LINKED (absence = inherit, like Word)"
        );
        /* Rendering unchanged: inheritance resolves section 2 to rIdA. */
        let resolved = resolve_hf_inheritance(&sections);
        assert_eq!(resolved[1].0.default.as_deref(), Some("rIdA"));
    }

    #[test]
    fn insert_then_delete_break_restores_refs_via_backfill() {
        /* Design review B1 — the content-loss repro: insert a break
        into a doc whose ONLY ref lives on body_section, then delete
        the break. Without the marker-drop backfill the ref would be
        orphaned (left marker holds the only copy and tail-wins concat
        discards it). */
        let mut d = DocumentTree::from_text("hello world");
        d.body_section.header_refs.default = Some("rIdA".into());
        d.body_section.footer_refs.even = Some("rIdF".into());
        let broken =
            d.insert_section_break_at(LogicalPos::new(BlockPath::top(0), 5), SectionType::NextPage);
        /* Backspace across the boundary — merges the marker paragraph
        into the following one. */
        let merged = broken.delete_range(
            LogicalPos::new(BlockPath::top(0), 5),
            LogicalPos::new(BlockPath::top(1), 0),
        );
        let sections = merged.effective_sections();
        assert_eq!(sections.len(), 1, "back to one section");
        assert_eq!(
            sections[0].header_refs.default.as_deref(),
            Some("rIdA"),
            "insert+delete round trip must not lose the header"
        );
        assert_eq!(sections[0].footer_refs.even.as_deref(), Some("rIdF"));
    }

    #[test]
    fn deleting_a_break_keeps_downstream_linked_sections_rendering() {
        /* Imported shape: section A owns the header on its marker; B
        and the trailing body section are linked (empty refs). Deleting
        A's break must backfill A's refs into the next terminal so B/C
        keep rendering the header. */
        let blocks = vec![
            Block::Paragraph(Paragraph {
                text: "a".into(),
                ..Default::default()
            }),
            Block::Paragraph(Paragraph {
                text: "b".into(),
                ..Default::default()
            }),
            Block::Paragraph(Paragraph {
                text: "c".into(),
                ..Default::default()
            }),
        ];
        let sections = vec![
            Section {
                header_refs: hf(Some("rIdA"), None, None),
                start_block: 0,
                end_block: 1,
                ..Default::default()
            },
            Section {
                start_block: 1,
                end_block: 2,
                ..Default::default()
            },
            Section {
                start_block: 2,
                end_block: 3,
                ..Default::default()
            },
        ];
        let d = DocumentTree::from_blocks_with_sections(blocks, sections);
        let merged = d.delete_range(
            LogicalPos::new(BlockPath::top(0), 1),
            LogicalPos::new(BlockPath::top(1), 0),
        );
        let derived = merged.effective_sections();
        assert_eq!(derived.len(), 2, "A merged into B");
        assert_eq!(
            derived[0].header_refs.default.as_deref(),
            Some("rIdA"),
            "A's ref backfilled into B's terminal"
        );
        let resolved = resolve_hf_inheritance(&derived);
        assert_eq!(
            resolved[1].0.default.as_deref(),
            Some("rIdA"),
            "trailing section still inherits"
        );
    }

    #[test]
    fn backfill_never_overrides_an_owned_slot() {
        /* Word: when the FOLLOWING section owns its header, deleting
        the break keeps the following section's header. */
        let blocks = vec![
            Block::Paragraph(Paragraph {
                text: "a".into(),
                ..Default::default()
            }),
            Block::Paragraph(Paragraph {
                text: "b".into(),
                ..Default::default()
            }),
        ];
        let sections = vec![
            Section {
                header_refs: hf(Some("rIdA"), Some("rIdAF"), None),
                start_block: 0,
                end_block: 1,
                ..Default::default()
            },
            Section {
                header_refs: hf(Some("rIdB"), None, None),
                start_block: 1,
                end_block: 2,
                ..Default::default()
            },
        ];
        let d = DocumentTree::from_blocks_with_sections(blocks, sections);
        let merged = d.delete_range(
            LogicalPos::new(BlockPath::top(0), 1),
            LogicalPos::new(BlockPath::top(1), 0),
        );
        let derived = merged.effective_sections();
        assert_eq!(derived.len(), 1);
        assert_eq!(
            derived[0].header_refs.default.as_deref(),
            Some("rIdB"),
            "owned slot wins over the dropped marker's"
        );
        assert_eq!(
            derived[0].header_refs.first.as_deref(),
            Some("rIdAF"),
            "unowned slot backfills from the dropped marker"
        );
    }

    #[test]
    fn delete_text_remaps_field_overlays() {
        let p = Paragraph {
            text: "abc PAGE xyz".into(),
            fields: vec![Field {
                start: 4,
                end: 8,
                instruction: "PAGE".into(),
                span: None,
                source: None,
            }],
            ..Default::default()
        };
        /* Delete strictly before — field shifts left. */
        let d1 = p.delete_text(0, 2);
        assert_eq!((d1.fields[0].start, d1.fields[0].end), (2, 6));
        /* Delete strictly after — untouched. */
        let d2 = p.delete_text(9, 12);
        assert_eq!((d2.fields[0].start, d2.fields[0].end), (4, 8));
        /* Delete strictly inside — field shrinks. */
        let d3 = p.delete_text(5, 7);
        assert_eq!((d3.fields[0].start, d3.fields[0].end), (4, 6));
        /* Delete crossing a boundary — the atom breaks, field drops. */
        let d4 = p.delete_text(2, 6);
        assert!(d4.fields.is_empty());
    }

    #[test]
    fn split_and_concat_field_rules() {
        let p = Paragraph {
            text: "ab12cd".into(),
            fields: vec![Field {
                start: 2,
                end: 4,
                instruction: "PAGE".into(),
                span: None,
                source: None,
            }],
            ..Default::default()
        };
        /* Split before the field — right keeps it, rebased. */
        let (l, r) = p.split_at(1);
        assert!(l.fields.is_empty());
        assert_eq!((r.fields[0].start, r.fields[0].end), (1, 3));
        /* Split after — left keeps it. */
        let (l2, r2) = p.split_at(5);
        assert_eq!((l2.fields[0].start, l2.fields[0].end), (2, 4));
        assert!(r2.fields.is_empty());
        /* Split inside — straddler drops on both sides. */
        let (l3, r3) = p.split_at(3);
        assert!(l3.fields.is_empty() && r3.fields.is_empty());
        /* Concat — tail's field shifts right by head len. */
        let head = Paragraph {
            text: "head ".into(),
            ..Default::default()
        };
        let merged = head.concat(&p);
        assert_eq!((merged.fields[0].start, merged.fields[0].end), (7, 9));
    }

    #[test]
    fn insert_text_shifts_field_anchors() {
        let mut d = DocumentTree::from_text("Page 1");
        {
            let mut blocks = d.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &BlockPath::top(0), |para| {
                para.fields.push(Field {
                    start: 5,
                    end: 6,
                    instruction: "PAGE".into(),
                    span: None,
                    source: None,
                });
            });
            d.blocks = blocks;
        }
        /* Typing before the field shifts it whole. */
        let d2 = d.insert_text(LogicalPos::new(BlockPath::top(0), 0), "X: ");
        let p2 = d2.paragraph_at_path(&BlockPath::top(0)).unwrap();
        assert_eq!((p2.fields[0].start, p2.fields[0].end), (8, 9));
        /* Typing right AT the field start stays outside (shift). */
        let d3 = d.insert_text(LogicalPos::new(BlockPath::top(0), 5), "~");
        let p3 = d3.paragraph_at_path(&BlockPath::top(0)).unwrap();
        assert_eq!((p3.fields[0].start, p3.fields[0].end), (6, 7));
    }

    #[test]
    fn insert_field_at_authors_text_and_overlay() {
        let d = DocumentTree::from_text("Page  of it");
        let with_field = d.insert_field_at(LogicalPos::new(BlockPath::top(0), 5), "PAGE", "1");
        let p = with_field.paragraph_at_path(&BlockPath::top(0)).unwrap();
        assert_eq!(p.text, "Page 1 of it");
        assert_eq!(p.fields.len(), 1);
        assert_eq!((p.fields[0].start, p.fields[0].end), (5, 6));
        assert_eq!(p.fields[0].instruction, "PAGE");
        assert!(p.dirty, "authoring dirties the paragraph for the writer");
    }

    #[test]
    fn with_spliced_range_full_overlay_discipline() {
        /* Design review M5's mandated case: a style-span boundary
        strictly inside the field's cached range must clamp, never
        dangle. Text "Page 999 end", field [5,9), span [6,7) fully
        inside, span [9,12) after. Splice the field to "12". */
        let p = Paragraph {
            text: "Page 999 end".into(),
            fields: vec![Field {
                start: 5,
                end: 8,
                instruction: "NUMPAGES".into(),
                span: None,
                source: None,
            }],
            spans: vec![
                StyleRun {
                    start: 6,
                    end: 7,
                    style: SpanStyle::default(),
                },
                StyleRun {
                    start: 9,
                    end: 12,
                    style: SpanStyle::default(),
                },
            ],
            ..Default::default()
        };
        let s = p.with_spliced_range(5, 8, "12");
        assert_eq!(s.text, "Page 12 end");
        /* Interior span stretches over the whole replacement. */
        assert_eq!((s.spans[0].start, s.spans[0].end), (5, 7));
        /* Trailing span shifts by the length delta (-1). */
        assert_eq!((s.spans[1].start, s.spans[1].end), (8, 11));
        /* The field itself now covers the replacement exactly. */
        assert_eq!((s.fields[0].start, s.fields[0].end), (5, 7));
        /* Degenerate span (entirely inside, zero-width after clamp
        when replacement is empty) drops. */
        let gone = p.with_spliced_range(6, 7, "");
        assert!(gone.spans.iter().all(|r| r.start < r.end));
    }

    #[test]
    fn date_picture_render_and_switch_parse() {
        assert_eq!(render_date_picture("M/d/yyyy", 2026, 7, 5), "7/5/2026");
        assert_eq!(render_date_picture("dd MM yy", 2026, 7, 5), "05 07 26");
        assert_eq!(
            render_date_picture("yyyy-MM-dd", 2026, 12, 31),
            "2026-12-31"
        );
        let f = Field {
            start: 0,
            end: 1,
            instruction: "DATE \\@ \"dd/MM/yyyy\" \\* MERGEFORMAT".into(),
            span: None,
            source: None,
        };
        assert_eq!(f.date_picture().as_deref(), Some("dd/MM/yyyy"));
        let bare = Field {
            start: 0,
            end: 1,
            instruction: "DATE".into(),
            span: None,
            source: None,
        };
        assert_eq!(bare.date_picture(), None);
    }

    #[test]
    fn counts_include_referenced_stories_once() {
        let mut d = DocumentTree::from_text("one two");
        d.headers.insert(
            "rH".into(),
            vec![Block::Paragraph(Paragraph {
                text: "three four five".into(),
                ..Default::default()
            })],
        );
        d.headers.insert(
            "orphan".into(),
            vec![Block::Paragraph(Paragraph {
                text: "never counted words here".into(),
                ..Default::default()
            })],
        );
        /* Unreferenced parts don't count. */
        assert_eq!(d.word_count(), 2);
        /* Referenced once → counted once. */
        let d = d.set_section_hf_ref_at(
            LogicalPos::new(BlockPath::top(0), 0),
            true,
            HeaderFooterRole::Default,
            Some("rH"),
        );
        assert_eq!(d.word_count(), 5);
        /* A second role slot referencing the SAME rid still counts once. */
        let d = d.set_section_hf_ref_at(
            LogicalPos::new(BlockPath::top(0), 0),
            true,
            HeaderFooterRole::First,
            Some("rH"),
        );
        assert_eq!(d.word_count(), 5);
        assert_eq!(d.character_count(), 7 + 15);
    }

    #[test]
    fn role_setters_and_settings_toggles() {
        let d = DocumentTree::from_text("x");
        let pos = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.set_section_hf_ref_at(pos.clone(), false, HeaderFooterRole::Even, Some("fe"));
        assert_eq!(
            d.effective_sections()[0].footer_refs.even.as_deref(),
            Some("fe")
        );
        /* Clearing = relink (absence-based linked state). */
        let d = d.set_section_hf_ref_at(pos.clone(), false, HeaderFooterRole::Even, None);
        assert!(d.effective_sections()[0].footer_refs.even.is_none());
        let d = d.set_section_title_pg_at(pos, true);
        assert!(d.effective_sections()[0].title_pg);
        assert!(!d.settings_dirty, "sectPr edits never dirty settings.xml");
        let d = d.with_even_odd_headers(true);
        assert!(d.settings.even_and_odd_headers);
        assert!(d.settings_dirty, "settings edit flips the dirty flag");
        let same = d.with_even_odd_headers(true);
        assert!(same.settings_dirty, "no-op toggle keeps existing dirt");
    }

    #[test]
    fn deep_paragraph_paths_descend_into_table_cells() {
        /* Design review B6 — a table-only document/story must resolve
        caret homes INSIDE the table, never a bare table path. */
        let table = Table {
            rows: vec![TableRow {
                cells: vec![default_table_cell(), default_table_cell()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let d = DocumentTree::from_blocks(vec![Block::Table(table)]);
        let first = d.path_to_first_paragraph_deep().expect("first path");
        assert_eq!(
            first.steps,
            vec![
                PathStep::Block(0),
                PathStep::Cell { row: 0, col: 0 },
                PathStep::Block(0)
            ]
        );
        let last = d.path_to_last_paragraph_deep().expect("last path");
        assert_eq!(
            last.steps,
            vec![
                PathStep::Block(0),
                PathStep::Cell { row: 0, col: 1 },
                PathStep::Block(0)
            ]
        );
        assert!(
            d.paragraph_at_path(&first).is_some(),
            "deep path resolves to a real paragraph"
        );
    }
}

/// Issue #165 — accessible labels of text boxes from `<wp:docPr>`.
#[cfg(test)]
mod text_box_label_tests {
    use super::*;

    fn text_box(anchor_doc_pr: Option<&str>, source: Option<&str>) -> InlineObject {
        InlineObject {
            at: 0,
            kind: InlineKind::TextBox {
                width_emu: 914_400,
                height_emu: 457_200,
                story: Box::new(TextBoxStory {
                    source_xml: source.map(str::to_string),
                    ..TextBoxStory::default()
                }),
            },
            anchor: anchor_doc_pr.map(|x| {
                Box::new(FloatAnchor {
                    doc_pr_xml: Some(x.to_string()),
                    ..FloatAnchor::default()
                })
            }),
            source_xml: None,
        }
    }

    #[test]
    fn anchor_doc_pr_names_and_describes_the_box() {
        let io = text_box(
            Some(r#"<wp:docPr id="3" name="Callout &amp; note" descr='Side &#x2014; bar'/>"#),
            None,
        );
        assert_eq!(
            io.text_box_label(),
            Some((
                Some("Callout & note".to_string()),
                Some("Side \u{2014} bar".to_string())
            ))
        );
    }

    #[test]
    fn inline_box_reads_the_outer_doc_pr_from_its_source() {
        let src = r#"<w:drawing><wp:inline><wp:docPrX name="no"/><wp:docPr id="1" name="Outer" descr="  "/><wps:txbx><w:txbxContent><wp:docPr id="2" name="Inner"/></w:txbxContent></wps:txbx></wp:inline></w:drawing>"#;
        let io = text_box(None, Some(src));
        assert_eq!(io.text_box_label(), Some((Some("Outer".to_string()), None)));
    }

    #[test]
    fn vml_alt_is_the_description_and_non_boxes_have_no_label() {
        let src = r#"<w:pict><v:shapetype alt="x"/><v:shape id="s" alt="Pull quote"><v:textbox/></v:shape></w:pict>"#;
        assert_eq!(
            text_box(None, Some(src)).text_box_label(),
            Some((None, Some("Pull quote".to_string())))
        );
        assert_eq!(text_box(None, None).text_box_label(), Some((None, None)));
        let pic = InlineObject {
            at: 0,
            kind: InlineKind::NoteSelfRef {
                kind: NoteKind::Footnote,
            },
            anchor: None,
            source_xml: None,
        };
        assert_eq!(pic.text_box_label(), None);
    }
}

#[cfg(test)]
mod image_label_tests {
    use super::*;

    fn image(anchor_doc_pr: Option<&str>, source: Option<&str>) -> InlineObject {
        InlineObject {
            at: 0,
            kind: InlineKind::Image {
                rel_id: "rId1".to_string(),
                width_emu: 914_400,
                height_emu: 914_400,
                media_key: None,
            },
            anchor: anchor_doc_pr.map(|x| {
                Box::new(FloatAnchor {
                    doc_pr_xml: Some(x.to_string()),
                    ..FloatAnchor::default()
                })
            }),
            source_xml: source.map(|s| s.as_bytes().to_vec()),
        }
    }

    #[test]
    fn anchor_doc_pr_names_and_describes_the_picture() {
        let io = image(
            Some(r#"<wp:docPr id="4" name="Diagram" descr="A flow diagram"/>"#),
            None,
        );
        assert_eq!(
            io.image_label(),
            Some((
                Some("Diagram".to_string()),
                Some("A flow diagram".to_string())
            ))
        );
    }

    #[test]
    fn inline_picture_reads_the_doc_pr_from_its_own_source() {
        let src = r#"<w:drawing><wp:inline><wp:docPr id="2" name="Logo" descr="Company logo"/></wp:inline></w:drawing>"#;
        let io = image(None, Some(src));
        assert_eq!(
            io.image_label(),
            Some((Some("Logo".to_string()), Some("Company logo".to_string())))
        );
    }

    #[test]
    fn vml_alt_is_the_description_and_non_images_have_no_label() {
        let src = r#"<w:pict><v:shape id="s" alt="Scanned page"><v:imagedata/></v:shape></w:pict>"#;
        assert_eq!(
            image(None, Some(src)).image_label(),
            Some((None, Some("Scanned page".to_string())))
        );
        assert_eq!(image(None, None).image_label(), Some((None, None)));
        let tb = InlineObject {
            at: 0,
            kind: InlineKind::NoteSelfRef {
                kind: NoteKind::Footnote,
            },
            anchor: None,
            source_xml: None,
        };
        assert_eq!(tb.image_label(), None);
    }
}

/// Issue #85 — crash-recovery persistence of the undo stack.
#[cfg(test)]
mod undo_history_tests {
    use super::*;

    fn stack_with(n_pushes: usize, cap: usize) -> UndoStack {
        let mut s = UndoStack::new(DocumentTree::from_text("0"), cap);
        for i in 1..=n_pushes {
            s.push(DocumentTree::from_text(&i.to_string()));
        }
        s
    }

    fn text(d: &DocumentTree) -> &str {
        d.paragraph_text(0).unwrap()
    }

    #[test]
    fn history_window_keeps_the_newest_entries_and_remaps_the_cursor() {
        let s = stack_with(9, 100); /* entries "0".."9", cursor 9 */
        let (win, cursor) = s.history_window(4);
        assert_eq!(win.len(), 4);
        assert_eq!(text(&win[0]), "6");
        assert_eq!(text(&win[3]), "9");
        assert_eq!(cursor, 3);
        assert_eq!(text(&win[cursor]), text(s.current()));
    }

    #[test]
    fn history_window_never_drops_the_current_document_behind_a_redo_branch() {
        let mut s = stack_with(9, 100);
        for _ in 0..6 {
            assert!(s.undo());
        }
        assert_eq!(text(s.current()), "3");
        /* A 4-entry window from the top would be "6".."9" — none of them
        current. The window must slide down to include the cursor. */
        let (win, cursor) = s.history_window(4);
        assert_eq!(text(&win[cursor]), "3");
        assert_eq!(cursor, 0);
        assert_eq!(text(win.last().unwrap()), "9", "redo branch survives");
    }

    #[test]
    fn history_window_with_fewer_entries_than_max_returns_everything() {
        let s = stack_with(2, 100);
        let (win, cursor) = s.history_window(16);
        assert_eq!(win.len(), 3);
        assert_eq!(cursor, 2);
    }

    #[test]
    fn from_history_round_trips_a_window_and_keeps_undo_redo_working() {
        let s = stack_with(9, 100);
        let (win, cursor) = s.history_window(4);
        let mut r = UndoStack::from_history(win, cursor, 100);
        assert_eq!(text(r.current()), "9");
        assert_eq!(r.depth(), 4);
        assert!(r.can_undo());
        assert!(!r.can_redo());
        assert!(r.undo());
        assert_eq!(text(r.current()), "8");
        assert!(r.redo());
        assert_eq!(text(r.current()), "9");
        /* Below the window there is nothing to undo into. */
        assert!(r.undo() && r.undo() && r.undo());
        assert!(!r.undo());
        assert_eq!(text(r.current()), "6");
        assert_eq!(r.revision(), 5, "fresh revision counter, bumped per op");
    }

    #[test]
    fn from_history_sanitizes_hostile_input() {
        let fresh = UndoStack::from_history(Vec::new(), 7, 100);
        assert_eq!(fresh.depth(), 1);
        assert!(!fresh.can_undo());

        let docs = vec![DocumentTree::from_text("a"), DocumentTree::from_text("b")];
        let clamped = UndoStack::from_history(docs, 99, 100);
        assert_eq!(text(clamped.current()), "b");

        /* A window larger than the cap is trimmed from the bottom. */
        let big: Vec<_> = (0..10)
            .map(|i| DocumentTree::from_text(&i.to_string()))
            .collect();
        let trimmed = UndoStack::from_history(big, 9, 4);
        assert_eq!(trimmed.depth(), 4);
        assert_eq!(text(trimmed.current()), "9");
        assert_eq!(trimmed.cap(), 4);
    }
}

/// Issues #114–#117 — wire-value validation at the model boundary. Table-
/// driven over the scripts that broke the #90 sweep: Arabic (2-byte
/// scalars), stacked combining marks, emoji (4-byte), ZWJ sequences.
#[cfg(test)]
mod wire_validation_tests {
    use super::*;

    const ARABIC: &str = "السلام"; // six 2-byte letters, 12 bytes

    const SAMPLES: &[&str] = &[
        "السلام عليكم ورحمة الله",
        "e\u{0301}\u{0301}\u{0301}",
        "🙂🙂 emoji run",
        "👨\u{200D}👩\u{200D}👧 family",
        "mixed عربي 🙂 e\u{0301} end",
        "",
        "a",
    ];

    fn every_offset(text: &str) -> impl Iterator<Item = u32> {
        0..=(text.len() as u32 + 3)
    }

    fn pos(block: u32, offset: u32) -> LogicalPos {
        LogicalPos {
            path: BlockPath::top(block),
            offset,
        }
    }

    fn bold() -> SpanStyle {
        SpanStyle {
            bold: Some(true),
            ..Default::default()
        }
    }

    fn para(text: &str) -> Paragraph {
        Paragraph {
            text: text.to_string(),
            ..Default::default()
        }
    }

    /// Every stored offset that later slices `text` sits on a char boundary.
    fn assert_boundaries(p: &Paragraph) {
        let ok = |o: u32| p.text.is_char_boundary(o as usize);
        for s in &p.spans {
            assert!(
                ok(s.start) && ok(s.end),
                "span {:?} in {:?}",
                (s.start, s.end),
                p.text
            );
        }
        for f in &p.fields {
            assert!(
                ok(f.start) && ok(f.end),
                "field {:?} in {:?}",
                (f.start, f.end),
                p.text
            );
        }
        for r in &p.revisions {
            assert!(
                ok(r.start) && ok(r.end),
                "revision {:?} in {:?}",
                (r.start, r.end),
                p.text
            );
        }
    }

    fn all_paragraphs(doc: &DocumentTree) -> Vec<&Paragraph> {
        fn walk<'a>(b: &'a Block, out: &mut Vec<&'a Paragraph>) {
            match b {
                Block::Paragraph(p) => out.push(p),
                Block::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            for cb in &cell.blocks {
                                walk(cb, out);
                            }
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        for b in doc.blocks.iter() {
            walk(b, &mut out);
        }
        out
    }

    #[test]
    fn snap_offset_floors_to_char_boundary_and_is_idempotent() {
        for text in SAMPLES {
            for off in every_offset(text) {
                let s = snap_offset(text, off);
                let cap = (off as usize).min(text.len());
                assert!(text.is_char_boundary(s as usize), "{text:?} @ {off}");
                assert!(s as usize <= cap, "snap never moves forward");
                assert_eq!(snap_offset(text, s), s, "idempotent");
                /* Floor: no boundary strictly between the snap and the cap. */
                for k in (s as usize + 1)..=cap {
                    assert!(
                        !text.is_char_boundary(k),
                        "{text:?}: {k} is a closer boundary"
                    );
                }
            }
        }
        assert_eq!(snap_offset(ARABIC, 1), 0);
        assert_eq!(snap_offset(ARABIC, 3), 2);
        assert_eq!(snap_offset(ARABIC, 12), 12);
        assert_eq!(snap_offset(ARABIC, 13), 12);
        assert_eq!(snap_offset("🙂", 2), 0);
        assert_eq!(
            snap_offset("e\u{0301}", 2),
            1,
            "a combining mark is its own scalar"
        );
    }

    #[test]
    fn paragraph_primitives_never_panic_at_any_offset_pair() {
        for text in SAMPLES {
            let p = para(text);
            for a in every_offset(text) {
                for b in every_offset(text) {
                    let d = p.delete_text(a, b);
                    assert_boundaries(&d);
                    let (l, r) = p.split_at(a);
                    assert_eq!(l.text.len() + r.text.len(), text.len());
                    assert_boundaries(&l);
                    assert_boundaries(&r);
                    let styled = p.apply_style(a, b, bold());
                    assert_boundaries(&styled);
                    let spliced = p.with_spliced_range(a, b, "XY");
                    assert_boundaries(&spliced);
                    let (ws, we) = p.word_bounds(a);
                    assert!(
                        text.is_char_boundary(ws as usize) && text.is_char_boundary(we as usize)
                    );
                    assert!(text.is_char_boundary(p.prev_offset(a) as usize));
                    assert!(text.is_char_boundary(p.next_offset(a) as usize));
                }
            }
        }
    }

    #[test]
    fn document_text_ops_snap_mid_scalar_offsets_and_never_panic() {
        for text in SAMPLES {
            let doc = DocumentTree::from_text(text);
            for a in every_offset(text) {
                for b in every_offset(text) {
                    let results = [
                        doc.delete_range(pos(0, a), pos(0, b)),
                        doc.insert_text(pos(0, a), "x"),
                        doc.split_paragraph(pos(0, a)),
                        doc.apply_style(pos(0, a), pos(0, b), bold()),
                        doc.tracked_delete_range(pos(0, a), pos(0, b), "a".into(), "d".into()),
                        doc.tracked_insert_text(pos(0, a), "yz", "a".into(), "d".into()),
                        doc.tracked_format_change(
                            pos(0, a),
                            pos(0, b),
                            SpanStyle::default(),
                            "a".into(),
                            "d".into(),
                        ),
                        doc.insert_field_at(pos(0, a), "PAGE", "1"),
                        doc.insert_comment(
                            pos(0, a),
                            pos(0, b),
                            "c".into(),
                            "a".into(),
                            "d".into(),
                        )
                        .0,
                    ];
                    for d in &results {
                        for p in all_paragraphs(d) {
                            assert_boundaries(p);
                        }
                    }
                    let _ = doc.text_range(pos(0, a), pos(0, b));
                    let _ = doc.slice(pos(0, a), pos(0, b));
                    let _ = doc.slice_blocks(pos(0, a), pos(0, b));
                }
            }
        }
    }

    #[test]
    fn mid_scalar_offsets_resolve_to_the_boundary_before_the_scalar() {
        let doc = DocumentTree::from_text(ARABIC);
        /* [1, 3) snaps to [0, 2): exactly the first letter goes. */
        let d = doc.delete_range(pos(0, 1), pos(0, 3));
        assert_eq!(d.to_plain_text(), &ARABIC[2..]);
        /* Insert at 3 lands at 2. */
        let d = doc.insert_text(pos(0, 3), "x");
        assert_eq!(
            d.to_plain_text(),
            format!("{}x{}", &ARABIC[..2], &ARABIC[2..])
        );
        /* Split at 3 splits at 2. */
        let d = doc.split_paragraph(pos(0, 3));
        assert_eq!(d.blocks[0].as_paragraph().unwrap().text, &ARABIC[..2]);
        assert_eq!(d.blocks[1].as_paragraph().unwrap().text, &ARABIC[2..]);
        /* A mid-scalar style range snaps to the enclosing boundaries. */
        let d = doc.apply_style(pos(0, 1), pos(0, 5), bold());
        let spans = &d.blocks[0].as_paragraph().unwrap().spans;
        assert_eq!((spans[0].start, spans[0].end), (0, 4));
        /* Reads agree with writes. */
        assert_eq!(doc.text_range(pos(0, 1), pos(0, 5)), &ARABIC[0..4]);
    }

    #[test]
    fn tracked_insert_revision_anchors_on_the_pre_insert_boundary() {
        let doc = DocumentTree::from_text(ARABIC);
        let d = doc.tracked_insert_text(pos(0, 3), "zz", "a".into(), "d".into());
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.text, format!("{}zz{}", &ARABIC[..2], &ARABIC[2..]));
        assert_eq!(p.revisions.len(), 1);
        assert_eq!((p.revisions[0].start, p.revisions[0].end), (2, 4));
    }

    #[test]
    fn field_anchor_snaps_to_the_pre_insert_boundary() {
        let doc = DocumentTree::from_text(ARABIC);
        let d = doc.insert_field_at(pos(0, 3), "PAGE", "1");
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.text, format!("{}1{}", &ARABIC[..2], &ARABIC[2..]));
        assert_eq!((p.fields[0].start, p.fields[0].end), (2, 3));
    }

    #[test]
    fn out_of_range_block_paths_are_no_ops_not_panics() {
        let doc = DocumentTree::from_text("one");
        let far = pos(7, 2);
        let _ = doc.delete_range(far.clone(), pos(9, 3));
        let _ = doc.apply_style(far.clone(), pos(9, 3), bold());
        let _ = doc.split_paragraph(far.clone());
        let _ = doc.insert_text(far, "x");
        let mut blocks = doc.blocks.clone();
        assert!(
            replace_block_in_top(
                &mut blocks,
                &BlockPath::top(5),
                Block::Paragraph(Paragraph::default())
            )
            .is_none()
        );
        assert_eq!(blocks.len(), 1, "an out-of-range replace touched nothing");
    }

    // ---- tables (#114 / #116) -------------------------------------------------

    #[test]
    fn check_table_dims_enforces_the_caps_before_any_allocation() {
        assert_eq!(check_table_dims(0, 3), Err(TableError::ZeroDimension));
        assert_eq!(check_table_dims(3, 0), Err(TableError::ZeroDimension));
        assert!(matches!(
            check_table_dims(u32::MAX, u32::MAX),
            Err(TableError::TooManyRows { .. })
        ));
        assert!(matches!(
            check_table_dims(32_768, 1),
            Err(TableError::TooManyRows { .. })
        ));
        assert!(matches!(
            check_table_dims(1, 64),
            Err(TableError::TooManyCols { .. })
        ));
        assert!(matches!(
            check_table_dims(2_000, 63),
            Err(TableError::TooManyCells { .. })
        ));
        for (r, c) in [(1, 1), (32_767, 2), (1_040, 63), (63, 63)] {
            assert_eq!(check_table_dims(r, c), Ok(()), "{r}x{c} is legal");
        }
        let doc = DocumentTree::from_text("x");
        /* The #114 reproducer: a ~128 GB request must return, not abort. */
        assert!(matches!(
            doc.try_insert_table(BlockPath::top(0), u32::MAX, u32::MAX),
            Err(TableError::TooManyRows { .. })
        ));
        assert_eq!(
            doc.try_insert_table(BlockPath::top(0), 0, 1).err(),
            Some(TableError::ZeroDimension)
        );
        let ok = doc.try_insert_table(BlockPath::top(0), 2, 3).unwrap();
        let t = ok.blocks[0].as_table().unwrap();
        assert_eq!((t.rows.len(), t.rows[0].cells.len()), (2, 3));
    }

    // ---- scale / render-date validation (#186 / #187) ----------------------

    #[test]
    fn validate_finite_scale_rejects_nan_and_infinity_only() {
        assert!(matches!(
            validate_finite_scale(f32::NAN),
            Err(ScaleError::NotFinite { .. })
        ));
        assert!(matches!(
            validate_finite_scale(f32::INFINITY),
            Err(ScaleError::NotFinite { .. })
        ));
        assert!(matches!(
            validate_finite_scale(f32::NEG_INFINITY),
            Err(ScaleError::NotFinite { .. })
        ));
        // Finite is `Ok` regardless of whether it's inside the documented
        // clamp range — `validate_finite_scale` only guards finiteness;
        // `do_set_zoom` / `do_set_device_scale` still clamp the range.
        for v in [0.0_f32, 1.0, 0.25, 4.0, 0.5, 8.0, -1.0, 1_000.0] {
            assert_eq!(validate_finite_scale(v), Ok(()), "{v} is finite");
        }
    }

    #[test]
    fn validate_render_date_enforces_calendar_bounds() {
        // The #187 repro: a fuzzer-sent `month: 960_639_140`.
        assert!(matches!(
            validate_render_date(2026, 960_639_140, 5, None, None),
            Err(DateError::MonthOutOfRange { .. })
        ));
        assert!(matches!(
            validate_render_date(0, 1, 1, None, None),
            Err(DateError::YearOutOfRange { year: 0 })
        ));
        assert!(matches!(
            validate_render_date(10_000, 1, 1, None, None),
            Err(DateError::YearOutOfRange { year: 10_000 })
        ));
        assert!(matches!(
            validate_render_date(2026, 0, 1, None, None),
            Err(DateError::MonthOutOfRange { month: 0 })
        ));
        assert!(matches!(
            validate_render_date(2026, 13, 1, None, None),
            Err(DateError::MonthOutOfRange { month: 13 })
        ));
        // April has 30 days.
        assert!(matches!(
            validate_render_date(2026, 4, 31, None, None),
            Err(DateError::DayOutOfRange { .. })
        ));
        // 2026 is not a leap year; 2024 is.
        assert!(matches!(
            validate_render_date(2026, 2, 29, None, None),
            Err(DateError::DayOutOfRange { .. })
        ));
        assert_eq!(validate_render_date(2024, 2, 29, None, None), Ok(()));
        assert!(matches!(
            validate_render_date(2026, 1, 0, None, None),
            Err(DateError::DayOutOfRange { day: 0, .. })
        ));
        assert!(matches!(
            validate_render_date(2026, 7, 5, Some(24), Some(0)),
            Err(DateError::HourOutOfRange { hour: 24 })
        ));
        assert!(matches!(
            validate_render_date(2026, 7, 5, Some(0), Some(60)),
            Err(DateError::MinuteOutOfRange { minute: 60 })
        ));
        assert_eq!(validate_render_date(2026, 7, 5, Some(23), Some(59)), Ok(()));
        assert_eq!(validate_render_date(2026, 7, 5, None, None), Ok(()));
        assert_eq!(validate_render_date(1, 1, 1, None, None), Ok(()));
        assert_eq!(validate_render_date(9999, 12, 31, None, None), Ok(()));
    }

    #[test]
    fn infallible_insert_table_clamps_into_the_caps() {
        let doc = DocumentTree::from_text("x");
        let t = doc.insert_table(BlockPath::top(0), 5, u32::MAX);
        let t = t.blocks[0].as_table().unwrap();
        assert_eq!(
            (t.rows.len(), t.column_count()),
            (5, MAX_TABLE_COLS as usize)
        );
        let t = doc.insert_table(BlockPath::top(0), u32::MAX, u32::MAX);
        let t = t.blocks[0].as_table().unwrap();
        assert_eq!(t.column_count(), MAX_TABLE_COLS as usize);
        assert_eq!(t.rows.len() as u64, MAX_TABLE_CELLS / MAX_TABLE_COLS as u64);
        let t = doc.insert_table(BlockPath::top(0), 0, 0);
        let t = t.blocks[0].as_table().unwrap();
        assert_eq!((t.rows.len(), t.column_count()), (1, 1));
    }

    #[test]
    fn resolve_table_target_returns_typed_errors() {
        let doc = DocumentTree::from_text("p").insert_table(BlockPath::top(0), 2, 3);
        assert!(matches!(
            doc.resolve_table(&BlockPath::top(1)),
            Err(TableError::NotATable { .. })
        ));
        assert!(matches!(
            doc.resolve_table(&BlockPath::top(9)),
            Err(TableError::NotATable { .. })
        ));
        assert!(matches!(
            doc.resolve_table(&BlockPath::root()),
            Err(TableError::NotATable { .. })
        ));
        assert!(doc.resolve_table(&BlockPath::top(0)).is_ok());
        assert_eq!(
            doc.resolve_table_target(&BlockPath::top(0), Some(2), None)
                .err(),
            Some(TableError::RowOutOfRange { row: 2, rows: 2 })
        );
        assert_eq!(
            doc.resolve_table_target(&BlockPath::top(0), Some(1), Some(3))
                .err(),
            Some(TableError::ColOutOfRange { col: 3, cols: 3 })
        );
        assert_eq!(
            doc.resolve_table_target(&BlockPath::top(0), None, Some(3))
                .err(),
            Some(TableError::ColOutOfRange { col: 3, cols: 3 })
        );
        assert!(
            doc.resolve_table_target(&BlockPath::top(0), Some(1), Some(2))
                .is_ok()
        );
        assert!(
            doc.resolve_table_target(&BlockPath::top(0), None, Some(2))
                .is_ok()
        );
        /* A cell paragraph is not a table … */
        let cell_para = BlockPath::top(0)
            .push(PathStep::Cell { row: 0, col: 0 })
            .push(PathStep::Block(0));
        assert!(matches!(
            doc.resolve_table(&cell_para),
            Err(TableError::NotATable { .. })
        ));
        /* … and a real nested table is recognised but not editable yet. */
        let mut nested = doc.clone();
        let mut blocks = nested.blocks.clone();
        let mut b = blocks[0].clone();
        if let Block::Table(t) = &mut b {
            t.rows[0].cells[0]
                .blocks
                .push(Block::Table(Table::default()));
        }
        blocks.set(0, b);
        nested.blocks = blocks;
        let nested_path = BlockPath::top(0)
            .push(PathStep::Cell { row: 0, col: 0 })
            .push(PathStep::Block(1));
        assert!(matches!(
            nested.resolve_table(&nested_path),
            Err(TableError::NestedUnsupported { .. })
        ));
        /* Every error renders as a human-readable message. */
        for e in [
            TableError::NotATable {
                path: BlockPath::top(1),
            },
            TableError::RowOutOfRange { row: 2, rows: 2 },
            TableError::ZeroDimension,
            TableError::TooManyCells {
                requested: 1,
                max: 1,
            },
        ] {
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn table_growth_is_capped() {
        let wide = Table {
            grid: vec![1; MAX_TABLE_COLS as usize],
            rows: vec![TableRow::default()],
            ..Default::default()
        };
        assert!(matches!(
            wide.check_growth(0, 1),
            Err(TableError::TooManyCols { .. })
        ));
        assert_eq!(wide.check_growth(1, 0), Ok(()));
        let tall = Table {
            grid: vec![1; 2],
            rows: (0..MAX_TABLE_ROWS).map(|_| TableRow::default()).collect(),
            ..Default::default()
        };
        assert!(matches!(
            tall.check_growth(1, 0),
            Err(TableError::TooManyRows { .. })
        ));
        assert!(matches!(
            tall.check_growth(0, 1),
            Err(TableError::TooManyCells { .. })
        ));
    }

    #[test]
    fn table_mutations_with_out_of_range_indices_never_panic() {
        let doc = DocumentTree::from_text("p").insert_table(BlockPath::top(0), 2, 2);
        let paths = [
            BlockPath::top(0),
            BlockPath::top(1),
            BlockPath::top(7),
            BlockPath::root(),
            BlockPath::top(0).push(PathStep::Cell { row: 0, col: 0 }),
        ];
        for path in &paths {
            for r in [0u32, 1, 2, 99] {
                for c in [0u32, 1, 2, 99] {
                    let _ = doc.insert_row(path.clone(), r as usize);
                    let _ = doc.delete_row(path.clone(), r);
                    let _ = doc.insert_column(path.clone(), c as usize);
                    let _ = doc.delete_column(path.clone(), c);
                    let _ = doc.merge_cells(path.clone(), r, c, 99, 99);
                    let _ = doc.merge_cells(path.clone(), 99, 99, r, c);
                    let _ = doc.merge_cells(path.clone(), r, c, 0, 0);
                    let _ = doc.split_cell(path.clone(), r, c);
                    let _ = doc.set_cell_shading(path.clone(), r, c, Some([1, 2, 3, 4]));
                    let _ = doc.set_cell_borders(path.clone(), r, c, CellBorders::default());
                }
            }
            let _ = doc.delete_table(path.clone());
        }
        /* The `len - 1` underflow: merge across a row that has no cells. */
        let mut hollow = doc.clone();
        let mut blocks = hollow.blocks.clone();
        let mut b = blocks[0].clone();
        if let Block::Table(t) = &mut b {
            t.rows[0].cells.clear();
        }
        blocks.set(0, b);
        hollow.blocks = blocks;
        let _ = hollow.merge_cells(BlockPath::top(0), 0, 0, 1, 1);
        let _ = hollow.merge_cells(BlockPath::top(0), 1, 0, 0, 1);
        let _ = hollow.merge_cells(BlockPath::top(0), 1, 5, 1, 9);
    }
}

/// Issues #199 / #106 — the travel rules of [`SourceMarkup`].
#[cfg(test)]
mod source_markup_tests {
    use super::*;

    fn run(start: u32, end: u32, rsid: &str) -> SourceRun {
        SourceRun {
            start,
            end,
            attrs: vec![SourceAttr {
                name: "w:rsidR".into(),
                value: rsid.into(),
                ws: None,
            }],
            ..SourceRun::default()
        }
    }

    fn marker(at: u32) -> SourceMarker {
        SourceMarker {
            at,
            xml: b"<w:proofErr/>".to_vec(),
            ..SourceMarker::default()
        }
    }

    /// "Hello " [0,6) + "world" [6,11), a marker at 6 and one at the end.
    fn para() -> Paragraph {
        Paragraph {
            text: "Hello world".into(),
            source_markup: Some(Box::new(SourceMarkup {
                text_len: 11,
                attrs: vec![
                    SourceAttr {
                        name: "w14:paraId".into(),
                        value: "1A2B3C4D".into(),
                        ws: None,
                    },
                    SourceAttr {
                        name: "w:rsidR".into(),
                        value: "00A1".into(),
                        ws: None,
                    },
                ],
                ppr: None,
                runs: vec![run(0, 6, "01"), run(6, 11, "02")],
                markers: vec![marker(6), marker(11)],
            })),
            ..Paragraph::default()
        }
    }

    fn markup(p: &Paragraph) -> &SourceMarkup {
        p.source_markup.as_deref().unwrap()
    }

    fn ranges(m: &SourceMarkup) -> Vec<(u32, u32)> {
        m.runs.iter().map(|r| (r.start, r.end)).collect()
    }

    #[test]
    fn insert_extends_the_run_it_ends_or_lands_in() {
        let doc = DocumentTree {
            blocks: vec![Block::Paragraph(para())].into(),
            ..DocumentTree::default()
        };
        let pos = |o| LogicalPos {
            path: BlockPath::top(0),
            offset: o,
        };
        /* At a run boundary: the run ending there grows, the next shifts. */
        let d = doc.insert_text(pos(6), "XY");
        let m = markup(d.nth_paragraph(0).unwrap());
        assert_eq!(ranges(m), vec![(0, 8), (8, 13)]);
        assert_eq!(m.markers[0].at, 8);
        assert_eq!(m.text_len, 13);
        /* At the paragraph end: the last run continues. */
        let d = doc.insert_text(pos(11), "!");
        let m = markup(d.nth_paragraph(0).unwrap());
        assert_eq!(ranges(m), vec![(0, 6), (6, 12)]);
        assert_eq!(m.markers[1].at, 12);
        /* At the paragraph start: the first run. */
        let d = doc.insert_text(pos(0), ">");
        assert_eq!(
            ranges(markup(d.nth_paragraph(0).unwrap())),
            vec![(0, 7), (7, 12)]
        );
    }

    /// Issue #276 — style spans follow the SAME run choice as the source
    /// markup: the span holding the character before the insertion grows
    /// over it (at the paragraph start: the span holding the first
    /// character), so typed text continues that run's formatting.
    #[test]
    fn insert_continues_the_style_span_before_the_caret() {
        let bold = SpanStyle {
            bold: Some(true),
            ..SpanStyle::default()
        };
        let mut p = para();
        p.spans = vec![StyleRun {
            start: 0,
            end: 6,
            style: bold.clone(),
        }];
        let doc = DocumentTree {
            blocks: vec![Block::Paragraph(p)].into(),
            ..DocumentTree::default()
        };
        let pos = |o| LogicalPos {
            path: BlockPath::top(0),
            offset: o,
        };
        /* At the bold span's end: it grows, exactly like source run 0. */
        let d = doc.insert_text(pos(6), "XY");
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!((p.spans[0].start, p.spans[0].end), (0, 8));
        assert_eq!(ranges(markup(p)), vec![(0, 8), (8, 13)]);
        assert_eq!(p.typing_style_at(8), bold);
        /* At the paragraph start: the first span. */
        let d = doc.insert_text(pos(0), ">");
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!((p.spans[0].start, p.spans[0].end), (0, 7));
        /* After unstyled text: stays unstyled. */
        let d = doc.insert_text(pos(11), "!");
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.spans.len(), 1);
        assert_eq!(p.style_at(11), SpanStyle::default());
    }

    #[test]
    fn delete_clips_runs_and_collapses_markers() {
        let p = para().delete_text(4, 8);
        let m = markup(&p);
        assert_eq!(ranges(m), vec![(0, 4), (4, 7)]);
        assert_eq!(m.markers[0].at, 4);
        assert_eq!(m.text_len, 7);
        assert!(m.offsets_valid(p.text.len()));
        /* Deleting a whole run drops it. */
        let p = para().delete_text(6, 11);
        assert_eq!(ranges(markup(&p)), vec![(0, 6)]);
    }

    #[test]
    fn split_keeps_identity_left_and_rsids_on_both_halves() {
        let (l, r) = para().split_at(8);
        let (ml, mr) = (markup(&l), markup(&r));
        assert_eq!(ranges(ml), vec![(0, 6), (6, 8)]);
        assert_eq!(ranges(mr), vec![(0, 3)]);
        assert_eq!(ml.runs[1].attrs, mr.runs[0].attrs, "split run: both halves");
        assert!(ml.attrs.iter().any(|a| a.name == "w14:paraId"));
        assert!(!mr.attrs.iter().any(|a| a.name == "w14:paraId"));
        assert!(mr.attrs.iter().any(|a| a.name == "w:rsidR"));
        assert_eq!(ml.markers.len(), 1);
        assert_eq!(mr.markers[0].at, 3);
        assert!(ml.offsets_valid(l.text.len()) && mr.offsets_valid(r.text.len()));
    }

    #[test]
    fn concat_shifts_the_tail_and_keeps_the_head_identity() {
        let (l, r) = para().split_at(8);
        let joined = l.concat(&r);
        let m = markup(&joined);
        assert_eq!(ranges(m), vec![(0, 6), (6, 8), (8, 11)]);
        assert_eq!(
            m.markers.iter().map(|k| k.at).collect::<Vec<_>>(),
            vec![6, 11]
        );
        assert!(m.attrs.iter().any(|a| a.name == "w14:paraId"));
        assert!(m.offsets_valid(joined.text.len()));
    }

    #[test]
    fn an_unaware_text_edit_goes_stale_instead_of_misplacing() {
        let mut p = para();
        p.text.push_str(" more");
        assert!(!markup(&p).offsets_valid(p.text.len()));
        /* A later remap keeps an explicitly stale record stale. (A remap
        over a SILENTLY out-of-step record trips the issue #250 test
        assertion instead — see `text_remap::tests`.) */
        p.source_markup.as_deref_mut().unwrap().text_len = STALE_TEXT_LEN;
        let q = p.delete_text(0, 1);
        assert!(!markup(&q).offsets_valid(q.text.len()));
    }

    #[test]
    fn clipboard_fragments_drop_the_markup() {
        assert!(strip_section_marker(para()).source_markup.is_none());
    }
}
