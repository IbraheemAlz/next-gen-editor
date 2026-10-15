//! Issue #328 — the page-level decorations a `<w:sectPr>` declares and
//! the paragraph frame a drop cap uses, typed so layout can paint them.
//!
//! - [`PageBorders`] — `<w:pgBorders>` (ECMA-376 §17.6.10).
//! - [`LineNumbering`] — `<w:lnNumType>` (§17.6.8).
//! - [`DropCap`] — `<w:framePr w:dropCap w:lines>` (§17.3.1.11), read from
//!   the paragraph's `<w:pPr>` grab bag (the bytes stay the truth: the
//!   writer replays them, nothing is re-serialized).
//!
//! The section fields are parsed from the same `<w:sectPr>` bytes the
//! writer's verified passthrough replays, so a zero-edit save stays
//! byte-identical; a regenerated `<w:sectPr>` re-emits them from these
//! typed values.

use serde::{Deserialize, Serialize};

use crate::{BorderStroke, GrabBag};

/// `<w:pgBorders w:offsetFrom>` — what the `w:space` of each edge is
/// measured from.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageBorderOffset {
    /// `text` (the default): `w:space` points OUTWARD from the text
    /// margins.
    #[default]
    Text,
    /// `page`: `w:space` points INWARD from the page edge.
    Page,
}

/// `<w:pgBorders w:display>` — which pages of the section show the
/// borders.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageBorderDisplay {
    #[default]
    AllPages,
    FirstPage,
    NotFirstPage,
}

/// `<w:pgBorders>` — one section's page border box.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct PageBorders {
    pub top: Option<BorderStroke>,
    pub left: Option<BorderStroke>,
    pub bottom: Option<BorderStroke>,
    pub right: Option<BorderStroke>,
    pub offset_from: PageBorderOffset,
    pub display: PageBorderDisplay,
    /// `w:zOrder="back"` — painted beneath the text (the default `front`
    /// paints over it).
    pub z_back: bool,
    /// An edge carried a `w:art` preset (artwork borders are painted as
    /// a plain single line and reported).
    pub has_art: bool,
}

impl PageBorders {
    /// `true` when at least one edge paints something.
    pub fn is_visible(&self) -> bool {
        [&self.top, &self.left, &self.bottom, &self.right]
            .into_iter()
            .flatten()
            .any(|s| !matches!(s.style, crate::BorderStyle::None))
    }

    /// Whether the section's `page_in_section`-th page (0-based) shows
    /// the borders under `w:display`.
    pub fn shows_on(&self, page_in_section: usize) -> bool {
        match self.display {
            PageBorderDisplay::AllPages => true,
            PageBorderDisplay::FirstPage => page_in_section == 0,
            PageBorderDisplay::NotFirstPage => page_in_section > 0,
        }
    }
}

/// `<w:lnNumType w:restart>`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineNumberRestart {
    /// `newPage` (the schema default): numbering restarts on every page.
    #[default]
    NewPage,
    /// `newSection`: restarts at the section's start.
    NewSection,
    /// `continuous`: carries on from the previous section.
    Continuous,
}

/// `<w:lnNumType>` — one section's line numbering. Present only when
/// the element is and `w:countBy` ≥ 1 (Word shows no numbers without
/// it).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(default)]
pub struct LineNumbering {
    /// `w:countBy` — only multiples of this are shown (every line is
    /// still counted).
    pub count_by: u32,
    /// `w:start` as written. Word's attribute is zero-based: its "Start
    /// at: N" dialog writes `N - 1`, so a (re)started count shows
    /// `start + 1` first ([`Self::first_number`]).
    pub start: u32,
    /// `w:distance` in points (`None` = automatic, 0.25 in).
    pub distance_pt: Option<f32>,
    pub restart: LineNumberRestart,
}

impl LineNumbering {
    /// The automatic `w:distance` Word uses when the attribute is absent
    /// (0.25 in).
    pub const AUTO_DISTANCE_PT: f32 = 18.0;

    pub fn distance(&self) -> f32 {
        self.distance_pt.unwrap_or(Self::AUTO_DISTANCE_PT)
    }

    /// The first number a (re)started count shows.
    pub fn first_number(&self) -> u32 {
        self.start.saturating_add(1)
    }
}

/// `<w:framePr w:dropCap>`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropCapKind {
    /// `drop` — inside the text column, the following lines indented
    /// around it.
    Drop,
    /// `margin` — hangs in the margin left of the text column.
    Margin,
}

/// A drop-cap frame: the paragraph holding it is laid out as a float
/// `lines` line pitches tall, anchored to the next paragraph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DropCap {
    pub kind: DropCapKind,
    /// `w:lines` (default 3, clamped 1..=10 like Word's dialog).
    pub lines: u8,
    /// `w:hSpace` in points: the gap between the cap and the text.
    pub h_space_pt: f32,
}

/// Value of attribute `name` in the start tag that opens `xml`
/// (`name="…"` or `name='…'`) — the plain ASCII values these attributes
/// take need no unescaping.
fn attr_in_tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let end = xml.find('>')?;
    let tag = &xml[..end];
    let mut from = 0;
    while let Some(i) = tag[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at > 0 && tag.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = tag[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let body = &rest[1..];
        let close = body.find(quote)?;
        return Some(&body[..close]);
    }
    None
}

fn on_off(v: Option<&str>) -> bool {
    !matches!(v.map(str::trim), Some("0" | "false" | "off"))
}

/// `true` when `frag` is an element named `qname` (start tag or empty).
fn is_element(frag: &str, qname: &str) -> bool {
    frag.strip_prefix('<')
        .and_then(|s| s.strip_prefix(qname))
        .and_then(|s| s.chars().next())
        .is_some_and(|c| c == '/' || c == '>' || c.is_ascii_whitespace())
}

/// The `<w:framePr>` drop cap a paragraph's `<w:pPr>` grab bag carries,
/// if any (`w:dropCap="drop" | "margin"`; `none` and absent mean no
/// drop cap).
pub fn drop_cap_from_bag(bag: &Option<Box<GrabBag>>) -> Option<DropCap> {
    for frag in GrabBag::fragments_of(bag) {
        let Ok(s) = std::str::from_utf8(frag) else {
            continue;
        };
        let s = s.trim_start();
        if !is_element(s, "w:framePr") {
            continue;
        }
        let kind = match attr_in_tag(s, "w:dropCap").map(str::trim) {
            Some("drop") => DropCapKind::Drop,
            Some("margin") => DropCapKind::Margin,
            _ => return None,
        };
        let lines = attr_in_tag(s, "w:lines")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(3)
            .clamp(1, 10) as u8;
        let h_space_pt = attr_in_tag(s, "w:hSpace")
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map_or(0.0, |tw| (tw / 20.0).clamp(0.0, 1584.0));
        return Some(DropCap {
            kind,
            lines,
            h_space_pt,
        });
    }
    None
}

/// `true` when a paragraph's `<w:pPr>` grab bag carries an ON
/// `<w:suppressLineNumbers>`.
pub fn suppresses_line_numbers(bag: &Option<Box<GrabBag>>) -> bool {
    GrabBag::fragments_of(bag).iter().any(|frag| {
        let Ok(s) = std::str::from_utf8(frag) else {
            return false;
        };
        let s = s.trim_start();
        is_element(s, "w:suppressLineNumbers") && on_off(attr_in_tag(s, "w:val"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bag(frags: &[&str]) -> Option<Box<GrabBag>> {
        let mut b = None;
        for f in frags {
            GrabBag::push_into(&mut b, f.as_bytes().to_vec());
        }
        b
    }

    #[test]
    fn drop_cap_reads_kind_lines_and_hspace() {
        let b = bag(&[
            r#"<w:framePr w:dropCap="drop" w:lines="2" w:wrap="around" w:vAnchor="text" w:hAnchor="text" w:hSpace="144"/>"#,
        ]);
        assert_eq!(
            drop_cap_from_bag(&b),
            Some(DropCap {
                kind: DropCapKind::Drop,
                lines: 2,
                h_space_pt: 7.2
            })
        );
        let m = bag(&[r#"<w:framePr w:dropCap="margin"/>"#]);
        assert_eq!(
            drop_cap_from_bag(&m).map(|d| (d.kind, d.lines)),
            Some((DropCapKind::Margin, 3))
        );
        assert_eq!(
            drop_cap_from_bag(&bag(&[r#"<w:framePr w:w="2000"/>"#])),
            None
        );
        assert_eq!(
            drop_cap_from_bag(&bag(&[r#"<w:framePr w:dropCap="none"/>"#])),
            None
        );
    }

    #[test]
    fn suppress_line_numbers_honours_val() {
        assert!(suppresses_line_numbers(&bag(&["<w:suppressLineNumbers/>"])));
        assert!(!suppresses_line_numbers(&bag(&[
            r#"<w:suppressLineNumbers w:val="0"/>"#
        ])));
        assert!(!suppresses_line_numbers(&bag(&["<w:keepNext/>"])));
    }

    #[test]
    fn page_border_display_rules() {
        let mut b = PageBorders::default();
        assert!(b.shows_on(0) && b.shows_on(3));
        b.display = PageBorderDisplay::FirstPage;
        assert!(b.shows_on(0) && !b.shows_on(1));
        b.display = PageBorderDisplay::NotFirstPage;
        assert!(!b.shows_on(0) && b.shows_on(1));
    }
}
