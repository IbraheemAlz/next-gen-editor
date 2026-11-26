//! Issue #354 — the settings toggles patched into the SOURCE part.

use super::{build_settings_xml, patch_settings_xml};
use crate::parts::settings::parse_settings_xml;
use engine::DocumentSettings;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// A Word-shaped `settings.xml` (the children Word 2016 writes, in its
/// order, extensions last) with `extra` spliced right after `<w:zoom>`.
fn word_settings(extra: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <w:settings xmlns:w=\"{W}\" xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">\
         <w:zoom w:percent=\"100\"/>{extra}<w:proofState w:spelling=\"clean\" w:grammar=\"clean\"/>\
         <w:defaultTabStop w:val=\"720\"/><w:characterSpacingControl w:val=\"doNotCompress\"/>\
         <w:compat><w:compatSetting w:name=\"compatibilityMode\" w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"15\"/></w:compat>\
         <w:rsids><w:rsidRoot w:val=\"00A1B2C3\"/></w:rsids>\
         <w:themeFontLang w:val=\"en-US\"/><w:decimalSymbol w:val=\".\"/><w:listSeparator w:val=\",\"/>\
         <w14:docId w14:val=\"1A2B3C4D\"/></w:settings>"
    )
}

fn settings(track: bool, even_odd: bool, mirror: bool) -> DocumentSettings {
    DocumentSettings {
        track_revisions: track,
        even_and_odd_headers: even_odd,
        mirror_margins: mirror,
        ..DocumentSettings::default()
    }
}

fn patch(xml: &str, s: &DocumentSettings) -> Option<String> {
    patch_settings_xml(xml.as_bytes(), s).map(|b| String::from_utf8(b).expect("utf8"))
}

#[test]
fn an_unchanged_toggle_keeps_the_part() {
    for (extra, live) in [
        ("", settings(false, false, false)),
        ("<w:trackRevisions/>", settings(true, false, false)),
        /* A spelling the writer would not use is still "on". */
        (
            "<w:trackRevisions w:val=\"1\"/>",
            settings(true, false, false),
        ),
        (
            "<w:trackRevisions></w:trackRevisions>",
            settings(true, false, false),
        ),
        (
            "<w:trackRevisions w:val=\"false\"/>",
            settings(false, false, false),
        ),
        (
            "<w:mirrorMargins/><w:trackRevisions/>",
            settings(true, false, true),
        ),
    ] {
        let xml = word_settings(extra);
        assert_eq!(patch(&xml, &live), None, "{extra}");
    }
}

#[test]
fn turning_track_revisions_on_inserts_it_at_its_schema_rank() {
    let xml = word_settings("");
    let out = patch(&xml, &settings(true, false, false)).expect("patched");
    /* After `<w:proofState>` (rank below), before `<w:defaultTabStop>`. */
    let want = xml.replacen(
        "w:grammar=\"clean\"/>",
        "w:grammar=\"clean\"/><w:trackRevisions/>",
        1,
    );
    assert_eq!(out, want);
    let back = parse_settings_xml(out.as_bytes()).expect("reparse");
    assert!(back.track_revisions && !back.even_and_odd_headers);
    /* The extension element stays last. */
    assert!(out.ends_with("<w14:docId w14:val=\"1A2B3C4D\"/></w:settings>"));
}

#[test]
fn turning_a_toggle_off_removes_every_spelling() {
    for extra in [
        "<w:trackRevisions/>",
        "<w:trackRevisions w:val=\"true\"/>",
        "<w:trackRevisions>\n</w:trackRevisions>",
        "<w:trackRevisions/><w:trackRevisions w:val=\"1\"/>",
    ] {
        let xml = word_settings(extra);
        let out = patch(&xml, &settings(false, false, false)).expect("patched");
        assert_eq!(out, word_settings(""), "{extra}");
    }
}

#[test]
fn an_off_spelling_turned_on_becomes_the_bare_toggle() {
    let xml = word_settings("<w:trackRevisions w:val=\"0\"/>");
    let out = patch(&xml, &settings(true, false, false)).expect("patched");
    assert_eq!(out.matches("trackRevisions").count(), 1);
    assert!(parse_settings_xml(out.as_bytes()).unwrap().track_revisions);
    assert_eq!(
        out,
        word_settings("").replacen(
            "w:grammar=\"clean\"/>",
            "w:grammar=\"clean\"/><w:trackRevisions/>",
            1
        )
    );
}

#[test]
fn several_toggles_land_in_schema_order() {
    let xml = word_settings("");
    let out = patch(&xml, &settings(true, true, true)).expect("patched");
    let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("{needle}"));
    /* mirrorMargins after zoom and before proofState; trackRevisions
    before defaultTabStop; evenAndOddHeaders after defaultTabStop and
    before characterSpacingControl. */
    assert!(at("<w:zoom") < at("<w:mirrorMargins/>"));
    assert!(at("<w:mirrorMargins/>") < at("<w:proofState"));
    assert!(at("<w:proofState") < at("<w:trackRevisions/>"));
    assert!(at("<w:trackRevisions/>") < at("<w:defaultTabStop"));
    assert!(at("<w:defaultTabStop") < at("<w:evenAndOddHeaders/>"));
    assert!(at("<w:evenAndOddHeaders/>") < at("<w:characterSpacingControl"));
    let back = parse_settings_xml(out.as_bytes()).unwrap();
    assert!(back.track_revisions && back.even_and_odd_headers && back.mirror_margins);
    /* Turning them all off again restores the source bytes. */
    let back_off = patch(&out, &settings(false, false, false)).expect("patched");
    assert_eq!(back_off, xml);
}

#[test]
fn two_toggles_sharing_an_anchor_keep_their_rank_order() {
    /* Only `<w:zoom>` ranks before either: both go right after it. */
    let xml = format!(r#"<w:settings xmlns:w="{W}"><w:zoom/><w:compat/></w:settings>"#);
    let out = patch(&xml, &settings(true, true, false)).expect("patched");
    assert_eq!(
        out,
        format!(
            r#"<w:settings xmlns:w="{W}"><w:zoom/><w:trackRevisions/><w:evenAndOddHeaders/><w:compat/></w:settings>"#
        )
    );
    /* No ranked child before it: right after the root start tag. */
    let xml = format!(r#"<w:settings xmlns:w="{W}"><w:compat/></w:settings>"#);
    let out = patch(&xml, &settings(true, false, false)).expect("patched");
    assert_eq!(
        out,
        format!(r#"<w:settings xmlns:w="{W}"><w:trackRevisions/><w:compat/></w:settings>"#)
    );
}

#[test]
fn a_removed_neighbour_does_not_disturb_an_insertion() {
    /* mirrorMargins goes, trackRevisions lands where it ended. */
    let xml =
        format!(r#"<w:settings xmlns:w="{W}"><w:zoom/><w:mirrorMargins/><w:compat/></w:settings>"#);
    let out = patch(&xml, &settings(true, false, false)).expect("patched");
    assert_eq!(
        out,
        format!(
            r#"<w:settings xmlns:w="{W}"><w:zoom/><w:trackRevisions/><w:compat/></w:settings>"#
        )
    );
}

#[test]
fn a_bom_and_a_self_closing_root_are_patched_in_place() {
    let xml = format!("\u{FEFF}<w:settings xmlns:w=\"{W}\"><w:zoom/></w:settings>");
    let out = patch(&xml, &settings(true, false, false)).expect("patched");
    assert_eq!(
        out,
        format!("\u{FEFF}<w:settings xmlns:w=\"{W}\"><w:zoom/><w:trackRevisions/></w:settings>")
    );
    let xml = format!("<w:settings xmlns:w=\"{W}\" />");
    let out = patch(&xml, &settings(true, false, false)).expect("patched");
    assert_eq!(
        out,
        format!("<w:settings xmlns:w=\"{W}\"><w:trackRevisions/></w:settings>")
    );
    assert!(parse_settings_xml(out.as_bytes()).unwrap().track_revisions);
}

#[test]
fn an_unreadable_part_is_left_alone() {
    assert_eq!(
        patch(
            "<w:settings><w:zoom></w:settings>",
            &settings(true, false, false)
        ),
        None
    );
}

#[test]
fn a_new_part_carries_only_the_toggles_that_are_on() {
    assert_eq!(build_settings_xml(&settings(false, false, false)), None);
    let xml = build_settings_xml(&settings(true, true, false)).expect("minted");
    assert!(xml.ends_with("<w:trackRevisions/><w:evenAndOddHeaders/></w:settings>"));
    let back = parse_settings_xml(xml.as_bytes()).unwrap();
    assert!(back.track_revisions && back.even_and_odd_headers && !back.mirror_margins);
}
