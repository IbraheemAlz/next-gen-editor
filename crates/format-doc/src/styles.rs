//! The style sheet onto the engine's style model (issue #575): the same
//! structures the `.docx` reader fills, so the ONE cascade
//! (`engine::StyleCascade`, issue #556) resolves an imported `.doc` like
//! any other document — `paragraph_base_direction`, the font-derived line
//! pitch (#475), the toolbar read-backs and `ModifyStyle` all work
//! unchanged.
//!
//! - **Document defaults** (`style_defaults` / `style_run_defaults`, the
//!   `.docx` `<w:docDefaults>`): the default PAP is empty; the default CHP
//!   is 10 pt (`hps` 20, the [MS-DOC] 2.6.1 default of `sprmCHps` /
//!   `sprmCHpsBi`) in the style sheet's default fonts (`Stshif.ftcAsci`,
//!   `STSHI.ftcBi`, 2.9.271 STSH). A style whose `istdBase` is `istdNil`
//!   folds onto them.
//! - **Paragraph styles** become `ParagraphStyle`s holding their OWN
//!   properties (`UpxPapx` / `UpxChpx`, deltas over the base style, as a
//!   `.docx` style's `<w:pPr>` / `<w:rPr>` are), `based_on` /
//!   `next` by id; the Normal style (`istd` 0, `sti` 0 — `istdNormal`) is
//!   the default paragraph style (`is_default`). A paragraph in Normal
//!   keeps `style_id: None`, as Word writes a `.docx` (no `<w:pStyle>`).
//! - **Character styles** (`sprmCIstd`) are folded into the run's span
//!   with their id in `char_style`, exactly what the `.docx` reader does
//!   with `<w:rStyle>`; "Default Paragraph Font" is no character style.
//! - **Toggle operands** 0x80 / 0x81 (2.9.327) resolve against the chain
//!   under the level that carries them ([`crate::props::CharProps::resolve`]).
//!
//! Style ids: a built-in style (`StdfBase.sti`) takes Word's own `.docx`
//! id and name (`Heading1` / `heading 1`) — always for Normal and the nine
//! headings (whatever the file's localized name), for the others when the
//! file names them by their English name; every other style's id is its
//! name without spaces or punctuation, each word capitalised, made unique.

use std::collections::{HashMap, HashSet};

use engine::{
    MAX_STYLE_CHAIN, ParaProperties, ParagraphStyle, SpanStyle, StyleCascade, TextDirection,
};

use crate::ffn::FontTable;
use crate::props::{CharProps, ParaProps};
use crate::sprm::{SGC_CHARACTER, SGC_PARAGRAPH, prls};
use crate::stsh::{ISTD_NIL, Std, Stk, Stsh};

/// `sti` of Normal (`stiNormal`).
const STI_NORMAL: u16 = 0;
/// `sti` of the last heading (`stiLev9`).
const STI_HEADING_9: u16 = 9;
/// `sti` of "Default Paragraph Font", the default character style.
const STI_DEFAULT_PARAGRAPH_FONT: u16 = 65;
/// The default character size: `hps` 20 (10 pt).
const DEFAULT_SIZE_PT: f32 = 10.0;

/// Word's `.docx` style id and name of a built-in style (`sti`), for the
/// styles the engine and the toolbar key on (Normal, the headings, the
/// common body and list styles).
fn builtin(sti: u16) -> Option<(String, String)> {
    let numbered = |id: &str, name: &str, n: u16| (format!("{id}{n}"), format!("{name} {n}"));
    Some(match sti {
        0 => ("Normal".into(), "Normal".into()),
        1..=9 => numbered("Heading", "heading", sti),
        10..=18 => numbered("Index", "index", sti - 9),
        19..=27 => numbered("TOC", "toc", sti - 18),
        28 => ("NormalIndent".into(), "Normal Indent".into()),
        29 => ("FootnoteText".into(), "footnote text".into()),
        30 => ("CommentText".into(), "annotation text".into()),
        31 => ("Header".into(), "header".into()),
        32 => ("Footer".into(), "footer".into()),
        33 => ("IndexHeading".into(), "index heading".into()),
        34 => ("Caption".into(), "caption".into()),
        35 => ("TableofFigures".into(), "table of figures".into()),
        38 => ("FootnoteReference".into(), "footnote reference".into()),
        39 => ("CommentReference".into(), "annotation reference".into()),
        40 => ("LineNumber".into(), "line number".into()),
        41 => ("PageNumber".into(), "page number".into()),
        42 => ("EndnoteReference".into(), "endnote reference".into()),
        43 => ("EndnoteText".into(), "endnote text".into()),
        47 => ("List".into(), "List".into()),
        48 => ("ListBullet".into(), "List Bullet".into()),
        49 => ("ListNumber".into(), "List Number".into()),
        62 => ("Title".into(), "Title".into()),
        65 => (
            "DefaultParagraphFont".into(),
            "Default Paragraph Font".into(),
        ),
        66 => ("BodyText".into(), "Body Text".into()),
        67 => ("BodyTextIndent".into(), "Body Text Indent".into()),
        74 => ("Subtitle".into(), "Subtitle".into()),
        85 => ("Hyperlink".into(), "Hyperlink".into()),
        87 => ("Strong".into(), "Strong".into()),
        88 => ("Emphasis".into(), "Emphasis".into()),
        _ => return None,
    })
}

/// An id from a style name: its letters and digits, each word
/// capitalised (`my quote` -> `MyQuote`).
fn id_from_name(name: &str) -> String {
    let mut out = String::new();
    for word in name.split(|c: char| !c.is_alphanumeric()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// What an `istd` names once resolved.
#[derive(Debug, Clone, Default)]
enum Slot {
    /// Nothing the model represents (no style, a table / numbering style).
    #[default]
    None,
    Paragraph {
        id: String,
        /// The chain's list membership (`sprmPIlfo`), for the report.
        list: Option<bool>,
    },
    Character {
        id: String,
        /// The character-style chain folded root first.
        run: Box<SpanStyle>,
    },
}

/// The resolved style sheet and font table.
#[derive(Debug, Default)]
pub(crate) struct StyleSheet {
    /// `DocumentTree::styles`.
    pub(crate) paragraph_styles: HashMap<String, ParagraphStyle>,
    /// `DocumentTree::style_defaults`.
    pub(crate) para_defaults: ParaProperties,
    /// `DocumentTree::style_run_defaults`.
    pub(crate) run_defaults: SpanStyle,
    pub(crate) fonts: FontTable,
    slots: Vec<Slot>,
    /// The `istd` of the default paragraph style (Normal).
    default_istd: Option<usize>,
}

/// One level's own paragraph properties (`grpprl` of paragraph sprms).
fn own_para(grpprl: &[u8], inherited_rtl: bool) -> ParaProps {
    let mut pp = ParaProps::default();
    for p in prls(grpprl).filter(|p| p.sgc() == SGC_PARAGRAPH) {
        pp.apply(&p);
    }
    pp.finish(inherited_rtl)
}

/// One style's own character properties over the chain under it, as a
/// style definition holds them (no character style, no grab bag).
fn own_run(grpprl: &[u8], under: &SpanStyle, fonts: &FontTable) -> SpanStyle {
    let mut cp = CharProps::default();
    for p in prls(grpprl).filter(|p| p.sgc() == SGC_CHARACTER) {
        cp.apply(&p);
    }
    let mut run = cp.resolve(&SpanStyle::default(), under, fonts);
    run.grab_bag = None;
    run.char_style = None;
    run
}

impl StyleSheet {
    /// Resolve `stsh` (absent when the file has no style sheet: then no
    /// styles and the engine's own defaults) with `fonts`.
    pub(crate) fn new(stsh: Option<&Stsh>, fonts: FontTable) -> StyleSheet {
        let Some(stsh) = stsh else {
            return StyleSheet {
                fonts,
                ..StyleSheet::default()
            };
        };
        let run_defaults = SpanStyle {
            font_size: Some(DEFAULT_SIZE_PT),
            font_size_cs: Some(DEFAULT_SIZE_PT),
            font_family: stsh
                .ftc_ascii
                .or(stsh.ftc_other)
                .and_then(|f| fonts.family(f)),
            font_family_cs: stsh.ftc_bi.and_then(|f| fonts.family(f)),
            ..SpanStyle::default()
        };
        let styles = &stsh.styles;
        let kind_at = |istd: u16| {
            styles
                .get(usize::from(istd))
                .and_then(Option::as_ref)
                .map(|s| s.stk)
        };

        let ids = assign_ids(styles);
        let default_istd = match styles.first() {
            Some(Some(s)) if s.stk == Stk::Paragraph => Some(0),
            _ => styles.iter().position(|s| {
                s.as_ref()
                    .is_some_and(|s| s.stk == Stk::Paragraph && s.sti == STI_NORMAL)
            }),
        };

        /* The chain of `istd` through parents of the same kind, LEAF
        first: cycle-safe and capped like the engine's own walk. */
        let chain = |istd: usize, kind: Stk| -> Vec<&Std> {
            let mut out: Vec<&Std> = Vec::new();
            let mut seen = HashSet::new();
            let mut cur = Some(istd);
            while let Some(i) = cur {
                let Some(s) = styles.get(i).and_then(Option::as_ref) else {
                    break;
                };
                if s.stk != kind || out.len() >= MAX_STYLE_CHAIN || !seen.insert(i) {
                    break;
                }
                out.push(s);
                cur = (s.istd_base != ISTD_NIL).then_some(usize::from(s.istd_base));
            }
            out
        };

        let mut slots = vec![Slot::None; styles.len()];
        let mut paragraph_styles = HashMap::new();
        let para_defaults = ParaProperties::default();
        for (istd, std) in styles.iter().enumerate() {
            let (Some(std), Some(id)) = (std, ids.get(istd).and_then(Option::as_ref)) else {
                continue;
            };
            match std.stk {
                Stk::Paragraph => {
                    /* Fold the chain root first: each level's own sprms
                    read against what the levels under it give. */
                    let mut under_para = para_defaults.clone();
                    let mut under_run = run_defaults.clone();
                    let mut list = None;
                    let mut own = (ParaProperties::default(), SpanStyle::default());
                    for s in chain(istd, Stk::Paragraph).iter().rev() {
                        let rtl = under_para.direction == Some(TextDirection::Rtl);
                        let pp = own_para(&s.papx, rtl);
                        list = pp.list.or(list);
                        let run = own_run(&s.chpx, &under_run, &fonts);
                        under_para = ParaProperties::cascade([&under_para, &pp.props]);
                        under_run = under_run.merged_with(run.clone());
                        own = (pp.props, run);
                    }
                    let para_id = |i: u16| match (kind_at(i), ids.get(usize::from(i))) {
                        (Some(Stk::Paragraph), Some(Some(id))) => Some(id.clone()),
                        _ => None,
                    };
                    let next = para_id(std.istd_next).filter(|n| n != id);
                    paragraph_styles.insert(
                        id.clone(),
                        ParagraphStyle {
                            id: id.clone(),
                            name: std
                                .name
                                .clone()
                                .filter(|_| builtin(std.sti).is_none())
                                .or_else(|| builtin_name(std))
                                .unwrap_or_default(),
                            /* A style based on itself has no parent. */
                            based_on: para_id(std.istd_base).filter(|b| b != id),
                            para: own.0,
                            run: own.1,
                            next,
                            is_default: default_istd == Some(istd),
                        },
                    );
                    slots[istd] = Slot::Paragraph {
                        id: id.clone(),
                        list,
                    };
                }
                Stk::Character if std.sti != STI_DEFAULT_PARAGRAPH_FONT => {
                    let mut run = SpanStyle::default();
                    for s in chain(istd, Stk::Character).iter().rev() {
                        let own = own_run(&s.chpx, &run, &fonts);
                        run = run.merged_with(own);
                    }
                    slots[istd] = Slot::Character {
                        id: id.clone(),
                        run: Box::new(run),
                    };
                }
                _ => {}
            }
        }
        StyleSheet {
            paragraph_styles,
            para_defaults,
            run_defaults,
            fonts,
            slots,
            default_istd,
        }
    }

    /// The engine's cascade over the paragraph styles (build one per walk).
    pub(crate) fn cascade(&self) -> StyleCascade<'_> {
        StyleCascade::new(&self.paragraph_styles)
    }

    fn slot(&self, istd: Option<u16>) -> &Slot {
        istd.and_then(|i| self.slots.get(usize::from(i)))
            .unwrap_or(&Slot::None)
    }

    /// The `style_id` of a paragraph whose `istd` is `istd`: the style's
    /// id, `None` for the default paragraph style (Normal) and for an
    /// `istd` that names no paragraph style — both format the paragraph
    /// with Normal through the engine's cascade.
    pub(crate) fn paragraph_style_id(&self, istd: Option<u16>) -> Option<String> {
        match self.slot(istd) {
            Slot::Paragraph { id, .. } if istd.map(usize::from) != self.default_istd => {
                Some(id.clone())
            }
            _ => None,
        }
    }

    /// The list membership a paragraph's style chain gives it.
    pub(crate) fn paragraph_list(&self, istd: Option<u16>) -> Option<bool> {
        let slot = match self.slot(istd) {
            Slot::Paragraph { .. } => self.slot(istd),
            _ => self.slot(self.default_istd.map(|i| i as u16)),
        };
        match slot {
            Slot::Paragraph { list, .. } => *list,
            _ => None,
        }
    }

    /// The span a run in character style `istd` starts from: the style's
    /// folded properties and its id (`char_style`); empty for none.
    pub(crate) fn character_base(&self, istd: Option<u16>) -> SpanStyle {
        match self.slot(istd) {
            Slot::Character { id, run } => SpanStyle {
                char_style: Some(id.clone()),
                ..(**run).clone()
            },
            _ => SpanStyle::default(),
        }
    }
}

/// The display name of a built-in style: Word's English name when the file
/// either spells it or spells nothing (or the style is Normal or a
/// heading), else the file's own name.
fn builtin_name(std: &Std) -> Option<String> {
    let (_, canon) = builtin(std.sti)?;
    match &std.name {
        Some(n) if std.sti > STI_HEADING_9 && !n.eq_ignore_ascii_case(&canon) => Some(n.clone()),
        _ => Some(canon),
    }
}

/// Every modelled style's id by `istd` (paragraph and character styles;
/// `None` for the rest), unique across both kinds. Built-in styles claim
/// theirs first, so a user style named "Heading 1" cannot take Word's id.
fn assign_ids(styles: &[Option<Std>]) -> Vec<Option<String>> {
    let mut ids: Vec<Option<String>> = vec![None; styles.len()];
    let mut taken: HashSet<String> = HashSet::new();
    /* The next suffix to try per base id: thousands of styles sharing one
    name stay linear. */
    let mut suffix: HashMap<String, u32> = HashMap::new();
    let modelled = |s: &Std| matches!(s.stk, Stk::Paragraph | Stk::Character);
    for builtin_pass in [true, false] {
        for (istd, std) in styles.iter().enumerate() {
            let Some(std) = std.as_ref().filter(|s| modelled(s)) else {
                continue;
            };
            let canon = builtin(std.sti);
            if canon.is_some() != builtin_pass {
                continue;
            }
            let wanted = match (&canon, &std.name) {
                (Some((id, name)), file)
                    if std.sti <= STI_HEADING_9
                        || file.as_deref().is_none_or(|f| f.eq_ignore_ascii_case(name)) =>
                {
                    id.clone()
                }
                (_, Some(file)) => id_from_name(file),
                _ => String::new(),
            };
            let base = if wanted.is_empty() {
                format!("Style{istd}")
            } else {
                wanted
            };
            let mut id = base.clone();
            let n = suffix.entry(base.clone()).or_insert(2);
            while taken.contains(&id) {
                id = format!("{base}{n}");
                *n += 1;
            }
            taken.insert(id.clone());
            ids[istd] = Some(id);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std(sti: u16, stk: Stk, name: &str) -> Option<Std> {
        Some(Std {
            sti,
            stk,
            istd_base: ISTD_NIL,
            istd_next: ISTD_NIL,
            name: (!name.is_empty()).then(|| name.to_string()),
            papx: Vec::new(),
            chpx: Vec::new(),
        })
    }

    #[test]
    fn built_in_styles_take_words_ids_and_user_styles_are_made_unique() {
        let ids = assign_ids(&[
            std(0, Stk::Paragraph, "Standard"),
            std(1, Stk::Paragraph, "Überschrift 1"),
            std(31, Stk::Paragraph, "Kopfzeile"),
            std(32, Stk::Paragraph, "footer"),
            None,
            std(0x0FFE, Stk::Paragraph, "Heading 1"),
            std(0x0FFE, Stk::Character, "my quote"),
            std(0x0FFE, Stk::Paragraph, "--"),
            std(0x0FFE, Stk::Other, "Table Grid"),
            std(0x0FFE, Stk::Paragraph, "My Quote"),
            std(65, Stk::Character, ""),
        ]);
        assert_eq!(
            ids,
            vec![
                Some("Normal".to_string()),
                Some("Heading1".to_string()),
                Some("Kopfzeile".to_string()),
                Some("Footer".to_string()),
                None,
                Some("Heading12".to_string()),
                Some("MyQuote".to_string()),
                Some("Style7".to_string()),
                None,
                Some("MyQuote2".to_string()),
                Some("DefaultParagraphFont".to_string()),
            ]
        );
    }
}
