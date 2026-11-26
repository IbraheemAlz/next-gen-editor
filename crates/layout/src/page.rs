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

    /// Issue #354 — THE page-margins helper: the margins the page
    /// numbered `page_number` lays out with, `self` being its section's
    /// nominal margins. Under `<w:mirrorMargins/>` (`mirror`) a section's
    /// left / right margins are its INSIDE / OUTSIDE margins: an odd page
    /// (a recto, bound on its left) keeps them, an EVEN page (a verso,
    /// bound on its right) swaps them. Parity is the page NUMBER's — the
    /// formatted number a PAGE field shows (`PageBox::page_number`,
    /// `<w:pgNumType w:start>` restart included), the parity the even /
    /// odd header bands and inside / outside float alignment key on — not
    /// the physical sheet index. Every page geometry goes through here
    /// (the paginator's flush, a blank Even/Odd-page filler); downstream
    /// code reads `PageBox::margins` and never swaps on its own.
    pub fn for_page(self, page_number: u32, mirror: bool) -> Self {
        if mirror && page_number % 2 == 0 {
            Self {
                left: self.right,
                right: self.left,
                ..self
            }
        } else {
            self
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
