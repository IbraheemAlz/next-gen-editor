//! Issue #556 — the DEFAULT paragraph style (`w:default="1"`, usually
//! `Normal`) formats every paragraph that names no style, or one the
//! document does not define, through the one [`StyleCascade`]: the
//! paragraph half ([`DocumentTree::resolve_style_cascade`],
//! [`recompute_paragraph_props`]) and the run half
//! ([`resolve_run_cascade`]) alike, and a `ModifyStyle` of it re-cascades
//! those paragraphs.

use super::*;

fn style(id: &str, based_on: Option<&str>, para: ParaProperties, run: SpanStyle) -> ParagraphStyle {
    ParagraphStyle {
        id: id.into(),
        name: id.into(),
        based_on: based_on.map(String::from),
        para,
        run,
        ..Default::default()
    }
}

/// `Normal` (default): RTL, 12 pt after, 14 pt bold-less serif run size;
/// `Heading` basedOn `Normal` (centred); `Plain` based on nothing.
fn doc() -> DocumentTree {
    let mut d = DocumentTree::from_text("");
    d.style_defaults.spacing.after_twips = 160;
    d.style_run_defaults.font_size = Some(11.0);
    let mut normal = style(
        "Normal",
        None,
        ParaProperties {
            direction: Some(TextDirection::Rtl),
            spacing: Spacing {
                before_twips: 120,
                after_twips: 240,
            },
            ..Default::default()
        },
        SpanStyle {
            font_size: Some(14.0),
            ..Default::default()
        },
    );
    normal.is_default = true;
    d.styles.insert("Normal".into(), normal);
    d.styles.insert(
        "Heading".into(),
        style(
            "Heading",
            Some("Normal"),
            ParaProperties {
                alignment: Some(Alignment::Center),
                ..Default::default()
            },
            SpanStyle::default(),
        ),
    );
    d.styles.insert(
        "Plain".into(),
        style(
            "Plain",
            None,
            ParaProperties {
                alignment: Some(Alignment::Start),
                ..Default::default()
            },
            SpanStyle::default(),
        ),
    );
    d
}

#[test]
fn the_applied_style_is_the_named_one_else_the_default() {
    let d = doc();
    let c = d.style_cascade();
    assert_eq!(c.default_style().map(|s| s.id.as_str()), Some("Normal"));
    let id = |s: Option<&str>| c.applied_style(s).map(|s| s.id.clone());
    assert_eq!(id(None).as_deref(), Some("Normal"), "no style → Normal");
    assert_eq!(
        id(Some("Missing")).as_deref(),
        Some("Normal"),
        "a style the document does not define → Normal"
    );
    assert_eq!(id(Some("Plain")).as_deref(), Some("Plain"));
    assert_eq!(id(Some("Heading")).as_deref(), Some("Heading"));
    let chain: Vec<&str> = c
        .chain(Some("Heading"))
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(chain, ["Heading", "Normal"], "leaf first");
}

#[test]
fn an_unstyled_paragraph_resolves_through_the_default_style() {
    let d = doc();
    let p = d.resolve_style_cascade(None);
    assert_eq!(p.direction, Some(TextDirection::Rtl));
    assert_eq!(
        p.spacing,
        Spacing {
            before_twips: 120,
            after_twips: 240
        },
        "Normal's spacing over the docDefaults'"
    );
    assert_eq!(d.resolve_style_cascade(Some("Missing")), p);
    assert_eq!(d.resolve_style_run_cascade(None).font_size, Some(14.0));
    assert_eq!(
        resolve_run_cascade(&d.styles, &d.style_run_defaults, Some("Missing")).font_size,
        Some(14.0)
    );
    /* `Normal` stays under a style based on it ... */
    let h = d.resolve_style_cascade(Some("Heading"));
    assert_eq!(h.direction, Some(TextDirection::Rtl));
    assert_eq!(h.alignment, Some(Alignment::Center));
    /* ... and never under one that is not. */
    let plain = d.resolve_style_cascade(Some("Plain"));
    assert_eq!(plain.direction, None);
    assert_eq!(plain.spacing.after_twips, 160, "the docDefaults alone");
    assert_eq!(
        d.resolve_style_run_cascade(Some("Plain")).font_size,
        Some(11.0)
    );
}

#[test]
fn without_a_default_style_the_cascade_is_the_doc_defaults() {
    let mut d = doc();
    if let Some(n) = d.styles.get_mut("Normal") {
        n.is_default = false;
    }
    let c = d.style_cascade();
    assert!(c.default_style().is_none());
    assert!(c.applied_style(None).is_none());
    assert!(c.applied_style(Some("Missing")).is_none());
    let p = d.resolve_style_cascade(None);
    assert_eq!(p.direction, None);
    assert_eq!(p.spacing.after_twips, 160);
    assert_eq!(d.resolve_style_run_cascade(None).font_size, Some(11.0));
}

/// A hand-made snapshot may flag several styles: the smallest id wins,
/// whatever the map order.
#[test]
fn several_flagged_styles_resolve_to_the_smallest_id() {
    let mut d = doc();
    for id in ["Plain", "Heading"] {
        if let Some(s) = d.styles.get_mut(id) {
            s.is_default = true;
        }
    }
    for _ in 0..8 {
        let styles = d.styles.clone();
        assert_eq!(
            StyleCascade::new(&styles)
                .default_style()
                .map(|s| s.id.as_str()),
            Some("Heading")
        );
    }
}

/// A `basedOn` cycle through the default style terminates (the visited
/// set), the chain still applying each style once.
#[test]
fn a_cycle_through_the_default_style_terminates() {
    let mut d = doc();
    if let Some(n) = d.styles.get_mut("Normal") {
        n.based_on = Some("Heading".into());
    }
    let chain: Vec<&str> = d
        .style_cascade()
        .chain(None)
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(chain, ["Normal", "Heading"]);
}

#[test]
fn recompute_folds_the_default_style_under_direct_formatting() {
    let d = doc();
    let mut p = Paragraph {
        direct_overrides: ParaProperties {
            direction: Some(TextDirection::Ltr),
            ..Default::default()
        },
        ..Default::default()
    };
    recompute_paragraph_props(&mut p, &d.styles, &d.style_defaults);
    assert_eq!(p.props.direction, Some(TextDirection::Ltr), "direct wins");
    assert_eq!(p.props.spacing.after_twips, 240, "Normal's spacing");
    p.style_id = Some("Plain".into());
    d.style_cascade().recompute(&mut p, &d.style_defaults);
    assert_eq!(p.props.spacing.after_twips, 160, "no Normal under Plain");
}

/// `ModifyStyle` on the default style re-cascades the paragraphs that
/// name no style (and leaves a paragraph in an unrelated style alone).
#[test]
fn modifying_the_default_style_recascades_unstyled_paragraphs() {
    let mut d = doc();
    let unstyled = Paragraph {
        text: "unstyled".into(),
        props: d.resolve_style_cascade(None),
        ..Default::default()
    };
    let plain = Paragraph {
        text: "plain".into(),
        style_id: Some("Plain".into()),
        props: d.resolve_style_cascade(Some("Plain")),
        ..Default::default()
    };
    d.blocks = vec![Block::Paragraph(unstyled), Block::Paragraph(plain)]
        .into_iter()
        .collect();
    let edited = d.modify_style(
        "Normal",
        Some(ParaProperties {
            alignment: Some(Alignment::Justify),
            ..Default::default()
        }),
        None,
        None,
        None,
    );
    assert_eq!(
        edited.nth_paragraph(0).unwrap().props.alignment,
        Some(Alignment::Justify)
    );
    assert_eq!(
        edited.nth_paragraph(1).unwrap().props.alignment,
        Some(Alignment::Start)
    );
    assert!(
        edited.styles["Normal"].is_default,
        "ModifyStyle never moves the default"
    );
}

/// A pre-#556 snapshot (no `is_default` key) reads every style as
/// non-default, and a non-default style encodes without the key.
#[test]
fn the_default_flag_is_invisible_to_snapshots_when_off() {
    let plain = ParagraphStyle {
        id: "Plain".into(),
        ..Default::default()
    };
    let bytes = rmp_serde::to_vec_named(&plain).expect("encode");
    let decoded: ParagraphStyle = rmp_serde::from_slice(&bytes).expect("decode");
    assert!(!decoded.is_default);
    assert!(
        !bytes
            .windows(b"is_default".len())
            .any(|w| w == b"is_default"),
        "skipped when false"
    );
    let flagged = ParagraphStyle {
        is_default: true,
        ..plain
    };
    let back: ParagraphStyle =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&flagged).expect("encode")).expect("decode");
    assert!(back.is_default);
}
