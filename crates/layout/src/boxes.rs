//! Hierarchical layout box model (PHASE_3_RENDER_RTL.md §5).
//!
//! `PageBox` → `ParagraphBox` → `LineBox` → `VisualRun` → `PositionedGlyph`.
//! Every box's `origin` is **relative to its parent container**: a `LineBox`
//! origin is relative to its `ParagraphBox`, a `ParagraphBox` origin relative
//! to the `PageBox` content area (the page rect inset by `margins`). The
//! renderer accumulates origins down the tree to reach absolute positions.
//!
//! Pragmatic subset of §5: the boxes carry only data backed by the current
//! pipeline. §5's `Script`, `ParagraphStyleId`, `JustifyInfo`, and
//! `HeaderFooterBox` wait for the styling / script / header-footer subsystems.

use crate::page::Margins;
use std::ops::Range;
use text_pipeline::{Alignment, ShapingDirection};

/// Font identifier — a key into the engine's font map.
pub type FontId = String;

/// A point in a parent-relative coordinate space, in PostScript points.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A 2D extent, in PostScript points.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// Per-run text attributes the renderer needs — the data the temporary
/// `render::scene::PaintConfig` used to carry out-of-band.
#[derive(Debug, Clone, Copy)]
pub struct TextAttrs {
    pub px_size: f32,
    /// Straight-alpha RGBA fill colour.
    pub color: [u8; 4],
    /// Render this run with synthetic (faux) bold — set when no real bold
    /// face was available (Backlog #1).
    pub faux_bold: bool,
    /// Render this run with synthetic (faux) italic via a shear transform.
    pub faux_italic: bool,
    /// Draw an underline stroke below the run (Backlog #1).
    pub underline: bool,
    /// Draw a strikethrough stroke through the run (Backlog #1).
    pub strike: bool,
    /// Highlight colour painted behind the run's glyphs (Backlog #1).
    pub bg_color: Option<[u8; 4]>,
}

/// A resolved rich-text style span — a paragraph byte range `[start, end)` with
/// its fully-resolved style (paragraph defaults already applied).
/// `layout_paragraph` splits shaping runs at these boundaries.
#[derive(Debug, Clone)]
pub struct StyleSpan {
    pub start: u32,
    pub end: u32,
    pub px_size: f32,
    pub color: [u8; 4],
    /// Requested bold — resolved to a real face or faux synthesis at layout.
    pub bold: bool,
    /// Requested italic — resolved to a real face or faux synthesis.
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub bg_color: Option<[u8; 4]>,
    /// Resolved font id for an explicit family request; `None` keeps the
    /// per-script default face (Backlog #9).
    pub font_family: Option<String>,
}

/// One shaped glyph, positioned by advance/offset relative to the pen. There is
/// no stored absolute x: it is the run pen plus the cumulative `x_advance` of
/// the prior glyphs in the run.
#[derive(Debug, Clone, Copy)]
pub struct PositionedGlyph {
    pub id: u16,
    /// Byte offset into the owning [`VisualRun::source_range`].
    pub cluster: u32,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    /// A synthetic glyph layout inserted — a Kashida Tatweel (U+0640). Drawn
    /// like any glyph, but skipped by caret / hit-test slot emission so the
    /// byte<->glyph map is not corrupted (Backlog #2).
    pub synthetic: bool,
}

/// A maximal run of glyphs sharing one font, direction, and style — the unit
/// produced by BiDi reordering. Glyphs are in visual (left-to-right) order.
#[derive(Debug, Clone)]
pub struct VisualRun {
    pub glyphs: Vec<PositionedGlyph>,
    pub font: FontId,
    pub direction: ShapingDirection,
    /// Byte range in the source paragraph text this run was shaped from.
    pub source_range: Range<u32>,
    pub attrs: TextAttrs,
}

/// One laid-out line. `origin` is relative to the parent [`ParagraphBox`];
/// `origin.x` already carries the alignment offset, so the renderer never
/// recomputes alignment.
#[derive(Debug, Clone)]
pub struct LineBox {
    pub origin: Point,
    /// Baseline offset below `origin.y`.
    pub baseline: f32,
    pub height: f32,
    /// Content width — natural, or the justified target.
    pub width: f32,
    pub runs: Vec<VisualRun>,
    pub alignment: Alignment,
}

/// One laid-out paragraph. `origin` is relative to the [`PageBox`] content area.
#[derive(Debug, Clone)]
pub struct ParagraphBox {
    pub origin: Point,
    pub size: Size,
    pub lines: Vec<LineBox>,
    pub direction: ShapingDirection,
}

/// A laid-out page — the root of the box tree and the renderer's input.
#[derive(Debug, Clone)]
pub struct PageBox {
    pub size: Size,
    pub margins: Margins,
    pub paragraphs: Vec<ParagraphBox>,
}
