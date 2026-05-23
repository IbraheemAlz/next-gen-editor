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
    /// ISO 216 A4 at 1 pt/unit. Dimensions are the exact OOXML twips:
    /// `11906 / 20 = 595.3` pt wide × `16838 / 20 = 841.9` pt tall —
    /// what Word stamps on a fresh document. Margins default to 72 pt
    /// (1 inch / 1440 twips), matching Word's stock template. Aspect
    /// ratio = 1 : √2 = 1 : 1.4143 (ISO 216).
    pub const fn a4() -> Self {
        Self {
            width: 595.3,
            height: 841.9,
            margin: Margins::uniform(72.0),
        }
    }

    pub fn content_width(&self) -> f32 {
        self.width - self.margin.left - self.margin.right
    }

    pub fn content_height(&self) -> f32 {
        self.height - self.margin.top - self.margin.bottom
    }

    /// This page with every dimension multiplied by `s`. Used to lay out and
    /// paint at device-pixel scale on HiDPI canvases while the logical model
    /// stays at 1 pt/unit.
    pub fn scaled(self, s: f32) -> Self {
        Self {
            width: self.width * s,
            height: self.height * s,
            margin: Margins {
                top: self.margin.top * s,
                right: self.margin.right * s,
                bottom: self.margin.bottom * s,
                left: self.margin.left * s,
            },
        }
    }
}
