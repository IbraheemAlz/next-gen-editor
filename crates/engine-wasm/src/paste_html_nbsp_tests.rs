//! Issue #599 - `&nbsp;` in pasted / opened HTML is U+00A0, kept through
//! the real doors: `Command::PasteHtml`, `Command::OpenDocument` (`.html`),
//! and a `.docx` save + reopen.
//!
//! `engine::html::decode_entities` decoded `&nbsp;` to an ASCII space and
//! `collapse_ws` then merged a run of them into one, so Word's clipboard
//! HTML (an indentation run, `<p><o:p>&nbsp;</o:p></p>` for every blank
//! line) and a browser's `a&nbsp;&nbsp;&nbsp;b` lost their gaps. The unit
//! tests are `engine::html`'s; layout (`layout::nbsp_tests`: no break at
//! it, a space-wide glyph in every shipped face) and the writer
//! (`format_docx::writer_nbsp_tests`) pin the rest of the chain, and this
//! file drives it end to end through `Engine::apply_sync`, exactly as the
//! `rpc_command` fuzz target does.
//!
//! Gated on `fuzz-native` like `paste_html_tests`.

use super::*;

const NBSP: char = '\u{a0}';

fn engine_over(text: &str) -> Engine {
    Engine::new_headless(DocumentTree::from_text(text))
}

fn paste(e: &mut Engine, html: &str) -> Event {
    e.apply_sync(Command::PasteHtml {
        html: html.to_string(),
    })
}

fn text(e: &Engine) -> String {
    e.undo.current().to_plain_text()
}

fn nbsps(n: usize) -> String {
    NBSP.to_string().repeat(n)
}

/// A run of `&nbsp;` lands as a run of U+00A0 (not one ASCII space), in the
/// shapes browsers and Word's clipboard HTML write.
#[test]
fn pasting_a_run_of_nbsp_keeps_every_one() {
    let cases: [(String, String); 6] = [
        (
            "<p>a&nbsp;&nbsp;&nbsp;b</p>".into(),
            format!("a{}b", nbsps(3)),
        ),
        /* The bare-text clipboard flavour of a browser selection. */
        ("a&nbsp;&nbsp;b".into(), format!("a{}b", nbsps(2))),
        /* Word: an indentation run, with an unknown `<o:p>` wrapper. */
        (
            "<p class=MsoNormal><o:p>&nbsp;&nbsp;&nbsp;&nbsp;</o:p>indented</p>".into(),
            format!("{}indented", nbsps(4)),
        ),
        /* The numeric spellings decode to the same character. */
        (
            "<p>a&#160;&#xA0;&#XA0;b</p>".into(),
            format!("a{}b", nbsps(3)),
        ),
        /* Beside formatting whitespace: that collapses, these do not. */
        ("<p>a\n   &nbsp;\n   b</p>".into(), format!("a {NBSP} b")),
        /* Inside an inline style. */
        (
            "<p><b>x&nbsp;&nbsp;y</b></p>".into(),
            format!("x{}y", nbsps(2)),
        ),
    ];
    for (html, expected) in cases {
        let mut e = engine_over("");
        let evt = paste(&mut e, &html);
        assert!(
            !matches!(evt, Event::Error { .. }),
            "{html:?} answered {evt:?}"
        );
        assert_eq!(text(&e), expected, "{html:?}");
        assert!(e.selection_is_valid(), "{html:?}");
        assert_eq!(e.check_invariants_for_fuzzing(), Ok(()), "{html:?}");
    }
}

/// A blank line spelled `<p>&nbsp;</p>` is a paragraph, kept in order; an
/// empty table cell spelled `<td>&nbsp;</td>` holds the character. ASCII
/// whitespace alone is still formatting, never a paragraph.
#[test]
fn a_blank_line_made_of_nbsp_is_kept() {
    let mut e = engine_over("");
    paste(&mut e, "<p>one</p><p>&nbsp;</p><p>two</p><p> </p>");
    assert_eq!(text(&e), format!("one\n{NBSP}\ntwo"));

    let mut e = engine_over("");
    paste(&mut e, "<table><tr><td>&nbsp;</td><td>x</td></tr></table>");
    assert!(text(&e).contains(NBSP), "{:?}", text(&e));
    assert_eq!(e.check_invariants_for_fuzzing(), Ok(()));
}

/// A pasted run stays one undo step, and a tracked-changes paste records
/// the no-break spaces like any inserted text.
#[test]
fn a_pasted_nbsp_run_undoes_in_one_step() {
    let mut e = engine_over("");
    e.apply_sync(Command::InsertText {
        at: None,
        text: "seed ".into(),
    });
    paste(&mut e, "<p>a&nbsp;&nbsp;b</p>");
    assert_eq!(text(&e), format!("seed a{}b", nbsps(2)));
    e.apply_sync(Command::Undo);
    assert_eq!(text(&e), "seed ");
}

/// A `.html` file opened as a document keeps them.
#[test]
fn opening_an_html_file_keeps_its_nbsp() {
    let mut e = engine_over("");
    let html = "<!DOCTYPE html><html><head><title>t&nbsp;t</title></head><body>\
                <p>a&nbsp;&nbsp;&nbsp;b</p><p>&nbsp;</p></body></html>";
    let loaded = e.apply_sync(Command::OpenDocument {
        bytes: html.as_bytes().to_vec(),
        format: bridge::DocFormat::Html,
        name: Some("page.html".into()),
        defaults: None,
        limits: None,
        password: None,
    });
    assert!(matches!(loaded, Event::DocumentLoaded { .. }), "{loaded:?}");
    assert_eq!(text(&e), format!("a{}b\n{NBSP}", nbsps(3)));
    assert_eq!(e.check_invariants_for_fuzzing(), Ok(()));
}

/// The whole chain: paste, save as `.docx`, reopen - the same characters,
/// written as the literal U+00A0 inside `<w:t>` (no ASCII space, no
/// character reference).
#[test]
fn a_pasted_nbsp_run_survives_a_docx_save_and_reopen() {
    let mut e = engine_over("");
    paste(
        &mut e,
        "<p>a&nbsp;&nbsp;&nbsp;b</p><p>&nbsp;</p><p>tail</p>",
    );
    let expected = format!("a{}b\n{NBSP}\ntail", nbsps(3));
    assert_eq!(text(&e), expected);

    let Event::DocumentSaved { bytes, notes, .. } = e.apply_sync(Command::SaveDocx) else {
        panic!("SaveDocx did not answer DocumentSaved");
    };
    assert!(notes.is_empty(), "{notes:?}");
    let archive = format_docx::read_docx(&bytes).expect("the save re-reads");
    assert_eq!(archive.document.to_plain_text(), expected);

    let mut reopened = engine_over("");
    let loaded = reopened.apply_sync(Command::OpenDocument {
        bytes: bytes.clone(),
        format: bridge::DocFormat::Docx,
        name: Some("nbsp.docx".into()),
        defaults: None,
        limits: None,
        password: None,
    });
    assert!(matches!(loaded, Event::DocumentLoaded { .. }), "{loaded:?}");
    assert_eq!(text(&reopened), expected);
    assert_eq!(reopened.check_invariants_for_fuzzing(), Ok(()));
}
