//! Shared bridge types referenced by both `Command` and `Event`.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

/// Caret/anchor position in the document model: paragraph index + offset.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalPos {
    pub para: u32,
    pub offset: u32,
}

/// Half-open span between two logical positions.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalRange {
    pub start: LogicalPos,
    pub end: LogicalPos,
}

/// Axis-aligned rectangle in CSS pixels.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A point in canvas device pixels — a pointer hit-test coordinate
/// (PHASE_4_HEADLESS_UI.md §7).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Document container format.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DocFormat {
    Docx,
    Pdf,
    PlainText,
    Html,
}

/// 8-bit-per-channel RGBA color.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Underline decoration style.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum UnderlineStyle {
    None,
    Single,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// Sub-/super-script positioning.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum VerticalScript {
    Normal,
    Superscript,
    Subscript,
}

/// Directionality of a selection or a resolved text run.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

/// Paragraph text alignment (Backlog #9). `Start` / `End` are
/// writing-direction-relative — they resolve against the paragraph's base
/// direction at layout time; `Center` and `Justify` are absolute. Serializes
/// as the bare variant string (`"Start"`, `"Center"`, …).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Start,
    End,
    Center,
    Justify,
}

/// Unicode script, reported when a glyph needs a font the engine lacks.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum Script {
    Common,
    Latin,
    Greek,
    Cyrillic,
    Arabic,
    Hebrew,
    Han,
    Hiragana,
    Katakana,
    Hangul,
    Devanagari,
    Thai,
    Unknown,
}

/// Resolved (fully-specified) inline text attributes at a position or range.
/// The sparse-patch counterpart is [`crate::TextAttrsPatch`].
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct TextAttrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: UnderlineStyle,
    pub strike: bool,
    pub font_family: String,
    pub font_size: f32,
    pub color: Color,
    pub bg_color: Option<Color>,
    pub script: VerticalScript,
    pub language: String,
}
