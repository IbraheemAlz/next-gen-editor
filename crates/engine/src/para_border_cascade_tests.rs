//! Issue #395 — paragraph borders cascade per edge, and a logically
//! spelled edge (`<w:start>` / `<w:end>`, issue #352) lands on the side
//! the paragraph's FINAL direction names, whichever level spelled it.

use super::*;

fn stroke(color: u8) -> Option<BorderStroke> {
    Some(BorderStroke {
        style: BorderStyle::Single,
        size_eighth_pt: 8,
        color: Some([color, 0, 0, 255]),
        ..Default::default()
    })
}

fn nil() -> Option<BorderStroke> {
    Some(BorderStroke {
        style: BorderStyle::None,
        size_eighth_pt: 0,
        color: None,
        ..Default::default()
    })
}

fn borders(top: Option<BorderStroke>, left: Option<BorderStroke>) -> Option<CellBorders> {
    Some(CellBorders {
        top,
        left,
        ..Default::default()
    })
}

fn rtl() -> Option<TextDirection> {
    Some(TextDirection::Rtl)
}

#[test]
fn merged_with_overlays_borders_per_edge() {
    let base = ParaProperties {
        borders: Some(CellBorders {
            top: stroke(1),
            bottom: stroke(2),
            ..Default::default()
        }),
        ..Default::default()
    };
    let patch = ParaProperties {
        borders: borders(nil(), stroke(3)),
        ..Default::default()
    };
    let b = base.clone().merged_with(patch).borders.unwrap();
    assert_eq!(b.top, nil(), "an explicit nil overrides the inherited edge");
    assert_eq!(b.bottom, stroke(2), "an unset edge inherits");
    assert_eq!(b.left, stroke(3));
    /* No borders on either side stay `None`; one side wins whole. */
    assert_eq!(
        ParaProperties::default()
            .merged_with(ParaProperties::default())
            .borders,
        None
    );
    assert_eq!(
        base.clone().merged_with(ParaProperties::default()).borders,
        base.borders
    );
}

#[test]
fn oriented_borders_turns_logical_edges_only() {
    /* An LTR style: start (logical) in the left slot, a physical top. */
    let style = ParaProperties {
        borders: borders(stroke(1), stroke(2)),
        border_spelling: BorderSpelling {
            start: true,
            end: false,
        },
        ..Default::default()
    };
    let same = style.clone().oriented_borders(false);
    assert_eq!(same, style);
    let turned = style.clone().oriented_borders(true).borders.unwrap();
    assert_eq!((turned.left, turned.right), (None, stroke(2)));
    assert_eq!(turned.top, stroke(1), "physical edges never move");
    /* Physical left / right never move either. */
    let physical = ParaProperties {
        borders: borders(None, stroke(4)),
        ..Default::default()
    };
    assert_eq!(physical.clone().oriented_borders(true), physical);
    /* Both logical: a swap. */
    let both = ParaProperties {
        direction: rtl(),
        borders: Some(CellBorders {
            left: stroke(5),
            right: stroke(6),
            ..Default::default()
        }),
        border_spelling: BorderSpelling {
            start: true,
            end: true,
        },
        ..Default::default()
    };
    let b = both.oriented_borders(false).borders.unwrap();
    assert_eq!((b.left, b.right), (stroke(6), stroke(5)));
}

#[test]
fn cascade_resolves_a_styles_start_edge_by_the_final_direction() {
    let defaults = ParaProperties::default();
    /* The style spells `<w:start>` (no bidi of its own → left slot). */
    let style = ParaProperties {
        borders: borders(None, stroke(1)),
        border_spelling: BorderSpelling {
            start: true,
            end: false,
        },
        ..Default::default()
    };
    let ltr = ParaProperties::cascade([&defaults, &style, &ParaProperties::default()]);
    let b = ltr.borders.unwrap();
    assert_eq!((b.left, b.right), (stroke(1), None));
    assert!(ltr.border_spelling.start);
    /* A direct `<w:bidi/>` turns it to the right. */
    let direct = ParaProperties {
        direction: rtl(),
        ..Default::default()
    };
    let rtl_props = ParaProperties::cascade([&defaults, &style, &direct]);
    let b = rtl_props.borders.clone().unwrap();
    assert_eq!((b.left, b.right), (None, stroke(1)));
    assert!(rtl_props.border_spelling.start);
    /* A direct physical left adds to it (per edge) without the spelling. */
    let direct_left = ParaProperties {
        direction: rtl(),
        borders: borders(None, stroke(9)),
        ..Default::default()
    };
    let both = ParaProperties::cascade([&defaults, &style, &direct_left]);
    let b = both.borders.unwrap();
    assert_eq!((b.left, b.right), (stroke(9), stroke(1)));
    assert!(both.border_spelling.start, "the start edge is the style's");
    /* Every non-border field folds exactly like `merged_with`. */
    let a = ParaProperties {
        alignment: Some(Alignment::Center),
        keep_next: Some(true),
        ..Default::default()
    };
    let b = ParaProperties {
        keep_next: Some(false),
        ..Default::default()
    };
    assert_eq!(
        ParaProperties::cascade([&a, &b]),
        a.clone().merged_with(b.clone())
    );
}
