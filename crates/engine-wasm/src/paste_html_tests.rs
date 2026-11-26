//! Issue #592 - pasted / opened HTML with an `&` in front of non-ASCII text
//! is a document, never an engine trap.
//!
//! `engine::html::decode_entities` looked for an entity's terminating `;`
//! in `rest[..rest.len().min(12)]`, a BYTE-index slice. When byte 12 fell
//! inside a multi-byte character (an `&` followed by Arabic text, say) the
//! slice panicked - and with `panic = "abort"` in the wasm build that is a
//! trap and a crash recovery. The nightly `rpc_command` target found it as
//! `PasteHtml { html: "ӈhttp&/ZZZZZseaAē" }` (33 raw bytes,
//! `tlZId3eJ04hodHRwJi9aWlpaWnNlYUHEk7p3bHd8d3e6`); the realistic shape is
//! `<p>R&amp;شركة العرب</p>`.
//!
//! These tests drive the real doors - `Command::PasteHtml` and
//! `Command::OpenDocument` (`.html`) - through `Engine::apply_sync`, so the
//! whole chain `apply_command` -> `do_paste_html` ->
//! `engine::html::from_html_blocks` -> `from_html` -> `decode_entities`
//! runs natively with overflow / bounds checks on, exactly as the fuzz
//! target does. The sibling the audit found (`parse_css_color` slicing a hex
//! colour by byte index) rides the same doors.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s through `Engine::apply_sync`.

use super::*;

const ARABIC: &str = "\u{634}\u{631}\u{643}\u{629} \u{627}\u{644}\u{639}\u{631}\u{628}";

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

/// The reproducer from the issue body and the realistic shapes, pasted into
/// an empty document: the text lands, and `&amp;` is the one thing decoded.
#[test]
fn pasting_an_ampersand_in_front_of_multibyte_text_lands_the_text() {
    let reproducer = "\u{4c8}http&/ZZZZZseaA\u{113}";
    let cases: [(String, String); 6] = [
        /* The nightly input, as the fuzz target pasted it. */
        (reproducer.to_string(), reproducer.to_string()),
        (format!("<p>R&amp;{ARABIC}</p>"), format!("R&{ARABIC}")),
        (format!("<p>AT&T{ARABIC}</p>"), format!("AT&T{ARABIC}")),
        /* The triage's survivor: byte 12 happens to be a boundary. */
        (
            format!("<p>Tom &amp; {ARABIC}</p>"),
            format!("Tom & {ARABIC}"),
        ),
        /* An unterminated entity at the very end, after Arabic. */
        (format!("<p>{ARABIC} &amp</p>"), format!("{ARABIC} &amp")),
        /* A real entity whose `;` is the last byte of the window. */
        (format!("<p>&#x00000041;{ARABIC}</p>"), format!("A{ARABIC}")),
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

/// The alignment of the `&` against its 12-byte window decides the bug, so
/// sweep it: every pad, every scalar width, in a paragraph, in a table cell
/// and behind an inline style. No paste may panic or answer an error, and
/// the visible text of a paragraph paste is the literal text (no entity in
/// these inputs decodes).
#[test]
fn every_alignment_of_the_entity_window_pastes_without_a_trap() {
    for ch in ['\u{634}', '\u{20ac}', '\u{1f600}', '\u{113}'] {
        for pad in 0..=14 {
            let run = format!(
                "{}&{}{}",
                "a".repeat(pad),
                "b".repeat(pad),
                ch.to_string().repeat(7)
            );
            let shapes = [
                format!("<p>{run}</p>"),
                format!("<p><b>{run}</b></p>"),
                format!("<p><span style=\"color:#ff0000\">{run}</span></p>"),
                format!("<table><tr><td>{run}</td><td>x</td></tr></table>"),
                run.clone(),
            ];
            for html in shapes {
                let mut e = engine_over("seed");
                let evt = paste(&mut e, &html);
                assert!(
                    !matches!(evt, Event::Error { .. }),
                    "{ch:?} pad {pad}: {html:?} answered {evt:?}"
                );
                assert!(
                    text(&e).contains(&run),
                    "{ch:?} pad {pad}: {html:?} lost its text: {:?}",
                    text(&e)
                );
                assert!(e.selection_is_valid(), "{ch:?} pad {pad}: {html:?}");
                assert_eq!(
                    e.check_invariants_for_fuzzing(),
                    Ok(()),
                    "{ch:?} pad {pad}: {html:?}"
                );
            }
        }
    }
}

/// The audit's sibling: `parse_css_color` sliced `#` + 3 or 6 BYTES by byte
/// index, so `#` + e-acute + `a` (three bytes) panicked. It is not a colour;
/// the text is pasted unstyled.
#[test]
fn a_non_ascii_hex_colour_is_ignored_not_a_trap() {
    for color in [
        "#\u{e9}a",
        "#a\u{e9}a\u{e9}a",
        "#\u{634}\u{631}\u{643}",
        "#\u{1f600}\u{1f600}",
    ] {
        for html in [
            format!("<p><span style=\"color:{color}\">x</span></p>"),
            format!("<p><span style=\"background-color:{color}\">x</span></p>"),
            format!("<p><font color=\"{color}\">x</font></p>"),
            /* `parse_border_shorthand` / `parse_cell_style` feed every
            token / value to the same colour parser. */
            format!(
                "<table><tr><td style=\"background-color:{color};border-top:1px solid {color}\">x</td></tr></table>"
            ),
        ] {
            let mut e = engine_over("");
            let evt = paste(&mut e, &html);
            assert!(
                !matches!(evt, Event::Error { .. }),
                "{html:?} answered {evt:?}"
            );
            assert!(text(&e).contains('x'), "{html:?}: {:?}", text(&e));
            assert!(!text(&e).contains('#'), "{html:?}: {:?}", text(&e));
            assert_eq!(e.check_invariants_for_fuzzing(), Ok(()), "{html:?}");
        }
    }
}

/// A real `.html` file reaches the same parser through
/// `OpenDocument` (issue #339): an `&amp;` before Arabic text opens.
#[test]
fn opening_an_html_file_with_an_entity_before_arabic_text_loads_it() {
    let mut e = engine_over("");
    let html = format!(
        "<!DOCTYPE html><html><head><title>t</title></head><body>\
         <p>R&amp;{ARABIC}</p><p>AT&T{ARABIC}</p></body></html>"
    );
    let loaded = e.apply_sync(Command::OpenDocument {
        bytes: html.into_bytes(),
        format: bridge::DocFormat::Html,
        name: Some("page.html".into()),
        defaults: None,
        limits: None,
        password: None,
    });
    assert!(matches!(loaded, Event::DocumentLoaded { .. }), "{loaded:?}");
    assert_eq!(text(&e), format!("R&{ARABIC}\nAT&T{ARABIC}"));
    assert_eq!(e.check_invariants_for_fuzzing(), Ok(()));
}
