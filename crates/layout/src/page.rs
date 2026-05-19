//! A4 page model. All units are PostScript points (72 pt = 1 in).

#[derive(Debug, Clone, Copy)]
pub struct Margins {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Margins {
    pub const fn uniform(v: f32) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct A4Page {
    pub width: f32,
    pub height: f32,
    pub margin: Margins,
}

impl A4Page {
    /// ISO 216 A4 at 1 pt/unit (595 × 842 pt). Margins default to 72 pt (1 in).
    pub const fn a4() -> Self {
        Self {
            width: 595.0,
            height: 842.0,
            margin: Margins::uniform(72.0),
        }
    }

    pub fn content_width(&self) -> f32 {
        self.width - self.margin.left - self.margin.right
    }

    pub fn content_height(&self) -> f32 {
        self.height - self.margin.top - self.margin.bottom
    }
}
