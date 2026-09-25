//! Structure-aware `bridge::Command` sequence generator for `rpc_command`
//! (D5.5, issue #90).
//!
//! `bridge::Command` derives `arbitrary::Arbitrary` behind bridge's
//! optional `arbitrary` feature (off by default; enabled here — see
//! `fuzz/Cargo.toml`), so blind `Command::arbitrary(u)` already gives full,
//! structurally-valid coverage of the whole ~80-variant wire enum,
//! including the recursive `Recover { log_tail: Vec<Command>, .. }` shape.
//! That alone is "structure-aware" in the sense the issue asks for, but on
//! its own it mostly produces commands addressing wildly out-of-bounds
//! positions (a `BlockPath` built from raw bytes almost never happens to
//! land inside a one-paragraph seed document), so most of a blind-only
//! sequence would just exercise the same "reject out of range" edges.
//! `gen_command_sequence` mixes it roughly 50/50 with a curated generator
//! (`gen_targeted_command`) that builds commands addressing small, usually
//! in-bounds positions across the issue's named categories — insert /
//! delete / format / table / section / story — so the fuzzer spends real
//! time past the first bounds check, while the blind half keeps hammering
//! the reject paths and the wire schema's full breadth.

use arbitrary::{Arbitrary, Unstructured};
use bridge::{
    Alignment, BlockPath, BridgeCellBorders, Command, Direction, FieldKind, HeaderFooterArea,
    ImageBlob, ImageFit, ImageWrapMode, InsertSide, ListKind, LogicalPos, LogicalRange, MoveDirection, SectionBreakKind,
    SelectionModifier, TextAttrsPatch, UnderlineStyle,
};

// No `sanitize` pass any more. The #90 sweep originally needed one for two
// classes of workaround, both of which are now enforced by the engine
// itself and therefore MUST reach `Engine::apply_sync` unfiltered:
//
// - `Command::InsertTable { rows, cols }` was clamped to 40 × 40 because
//   `DocumentTree::insert_table` allocated `rows × cols` cells from the raw
//   wire `u32`s and a single command could abort the process (issue #114).
//   The engine now rejects anything past Word's 63-column / 32 767-row
//   limits (and a cell-count cap) with a typed `Event::Error` BEFORE
//   allocating, so the blind `Arbitrary` half is free to hammer the full
//   `u32` range — that is exactly the regression this target must catch.
// - `InsertComment` / `ReplyToComment` / `Recover` were replaced by `Ping`
//   (and an empty `SetReviewIdentity.date` patched) because they reached
//   `js_sys::Date` directly, which panics off-wasm (issue #118). Every
//   timestamp now goes through one native-safe clock, so the comment and
//   recovery paths are in the sweep.

/// A short, bounded seed string for `DocumentTree::from_text` — plain text
/// only (no XML), so this stays independent of `docx_gen`'s schema-shaped
/// generator. Draws from a small pool covering LTR, RTL (Arabic), a
/// combining mark, and an empty string, so the seed document itself
/// already varies script/bidi/length before any `Command` runs.
pub fn gen_seed_text(u: &mut Unstructured) -> String {
    const POOL: &[&str] = &[
        "",
        "hello world",
        "a",
        "السلام عليكم ورحمة الله",
        "line one\u{2028}line two",
        "e\u{0301}\u{0301}\u{0301}", // combining marks stacked on one base
        "0123456789",
        "🙂🙂 emoji run",
    ];
    match u.choose(POOL) {
        Ok(s) => (*s).to_string(),
        Err(_) => "seed".to_string(),
    }
}

fn small(u: &mut Unstructured, max: u32) -> u32 {
    u.int_in_range(0..=max).unwrap_or(0)
}

fn small_f32(u: &mut Unstructured, max: i32) -> f32 {
    u.int_in_range(-max..=max).unwrap_or(0) as f32
}

fn pos(u: &mut Unstructured) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(small(u, 3)),
        offset: small(u, 40),
    }
}

fn range(u: &mut Unstructured) -> LogicalRange {
    let a = pos(u);
    let b = pos(u);
    LogicalRange { start: a, end: b }
}

fn text(u: &mut Unstructured) -> String {
    const POOL: &[char] = &['a', 'b', ' ', '\n', 'س', 'ل', '1', '\u{0301}'];
    let len = small(u, 12) as usize;
    let mut s = String::with_capacity(len);
    for _ in 0..len {
        let Ok(c) = u.choose(POOL) else { break };
        s.push(*c);
    }
    s
}

/// One curated, small-bounded command spanning the issue's named
/// categories: insert / delete / format / table / section / story.
fn gen_targeted_command(u: &mut Unstructured) -> Option<Command> {
    let variant = small(u, 20);
    Some(match variant {
        // ---- insert / delete -------------------------------------------------
        0 => Command::InsertText {
            at: if u.ratio(1, 2).unwrap_or(true) {
                None
            } else {
                Some(pos(u))
            },
            text: text(u),
        },
        1 => Command::DeleteRange { range: range(u) },
        2 => Command::DeleteAtCaret {
            forward: u.ratio(1, 2).unwrap_or(false),
            by_word: u.ratio(1, 2).unwrap_or(false),
        },
        3 => Command::ReplaceRange {
            range: range(u),
            text: text(u),
        },
        4 => Command::SplitParagraph { at: pos(u) },
        // ---- format ------------------------------------------------------------
        5 => Command::ApplyFormatting {
            range: if u.ratio(1, 2).unwrap_or(true) {
                None
            } else {
                Some(range(u))
            },
            attrs: TextAttrsPatch {
                bold: Some(u.ratio(1, 2).unwrap_or(false)),
                italic: Some(u.ratio(1, 2).unwrap_or(false)),
                underline: Some(UnderlineStyle::Single),
                strike: None,
                font_family: None,
                font_size: Some(small(u, 96) as f32 + 1.0),
                color: None,
                bg_color: None,
                script: None,
                language: None,
                caps: None,
                small_caps: None,
            },
        },
        6 => Command::SetParagraphAlign {
            range: range(u),
            align: *u
                .choose(&[
                    Alignment::Start,
                    Alignment::End,
                    Alignment::Center,
                    Alignment::Justify,
                ])
                .ok()?,
        },
        7 => Command::SetParagraphDirection {
            range: range(u),
            direction: *u.choose(&[Direction::Ltr, Direction::Rtl]).ok()?,
        },
        8 => Command::ToggleList {
            range: range(u),
            kind: *u
                .choose(&[ListKind::Off, ListKind::Bullet, ListKind::Number])
                .ok()?,
        },
        9 => Command::SetLineSpacing {
            range: range(u),
            multiplier: (small(u, 4) as f32) * 0.5,
        },
        10 => Command::SetParagraphIndent {
            range: range(u),
            start_pt: small_f32(u, 200),
            end_pt: small_f32(u, 200),
            first_line_pt: small_f32(u, 200),
        },
        // ---- table --------------------------------------------------------------
        11 => Command::InsertTable {
            at: BlockPath::top(small(u, 3)),
            rows: small(u, 5) + 1,
            cols: small(u, 5) + 1,
        },
        12 => Command::InsertRow {
            table_path: BlockPath::top(small(u, 3)),
            row: small(u, 5),
            side: if u.ratio(1, 2).unwrap_or(true) {
                InsertSide::Before
            } else {
                InsertSide::After
            },
        },
        13 => Command::DeleteRow {
            table_path: BlockPath::top(small(u, 3)),
            row: small(u, 5),
        },
        14 => Command::MergeCells {
            table_path: BlockPath::top(small(u, 3)),
            from_row: small(u, 4),
            from_col: small(u, 4),
            to_row: small(u, 4),
            to_col: small(u, 4),
        },
        15 => Command::SetCellShading {
            table_path: BlockPath::top(small(u, 3)),
            row: small(u, 4),
            col: small(u, 4),
            color: None,
        },
        16 => Command::SetCellBorders {
            table_path: BlockPath::top(small(u, 3)),
            row: small(u, 4),
            col: small(u, 4),
            borders: BridgeCellBorders::default(),
        },
        // ---- section ------------------------------------------------------------
        17 => Command::InsertSectionBreak {
            at: pos(u),
            kind: *u
                .choose(&[
                    SectionBreakKind::NextPage,
                    SectionBreakKind::Continuous,
                    SectionBreakKind::EvenPage,
                    SectionBreakKind::OddPage,
                ])
                .ok()?,
        },
        18 => Command::SetColumns {
            at: pos(u),
            count: (small(u, 3) as u8) + 1,
            gutter_pt: small_f32(u, 40),
        },
        // ---- story (header/footer) -----------------------------------------------
        19 => Command::EnterHeaderFooter {
            page: small(u, 3),
            area: if u.ratio(1, 2).unwrap_or(true) {
                HeaderFooterArea::Header
            } else {
                HeaderFooterArea::Footer
            },
        },
        _ => Command::ExitHeaderFooter,
    })
}

/// Selection / undo / caret motions — thrown into the mix unweighted
/// against the category list above since they interact with every
/// category (undo after a table edit, caret motion through a story, …).
fn gen_selection_command(u: &mut Unstructured) -> Option<Command> {
    Some(match small(u, 5) {
        0 => Command::SetSelection {
            range: range(u),
            caret: pos(u),
        },
        1 => Command::ExtendSelection {
            to: pos(u),
            modifier: *u
                .choose(&[
                    SelectionModifier::None,
                    SelectionModifier::Shift,
                    SelectionModifier::Alt,
                    SelectionModifier::ShiftAlt,
                ])
                .ok()?,
        },
        2 => Command::SelectAll,
        3 => Command::MoveCaret {
            direction: *u
                .choose(&[
                    MoveDirection::Left,
                    MoveDirection::Right,
                    MoveDirection::Up,
                    MoveDirection::Down,
                    MoveDirection::WordLeft,
                    MoveDirection::WordRight,
                    MoveDirection::LineHome,
                    MoveDirection::LineEnd,
                    MoveDirection::DocHome,
                    MoveDirection::DocEnd,
                ])
                .ok()?,
            extend: u.ratio(1, 2).unwrap_or(false),
        },
        4 => Command::Undo,
        _ => Command::Redo,
    })
}

/// Field authoring — small, separate category (not "format" or "table")
/// that still exercises real document mutation + layout re-resolution.
fn gen_field_command(u: &mut Unstructured) -> Option<Command> {
    Some(match u.int_in_range(0..=5u8).ok()? {
        /* Issue #81 — TOC insertion + F9 regeneration (the page-number
        post-pass runs a bounded fixed point over full paginations). */
        0 => Command::InsertToc {
            at: pos(u),
            switches: bridge::TocSwitches {
                outline_min: u.int_in_range(0..=9).ok()?,
                outline_max: u.int_in_range(0..=9).ok()?,
                hyperlinks: u.ratio(1, 2).unwrap_or(true),
                hide_in_web: true,
                use_outline_levels: u.ratio(1, 2).unwrap_or(true),
                page_numbers: u.ratio(3, 4).unwrap_or(true),
            },
        },
        1 => Command::UpdateFields,
        _ => Command::InsertField {
            at: pos(u),
            kind: *u
                .choose(&[FieldKind::Page, FieldKind::NumPages, FieldKind::Date])
                .ok()?,
        },
    })
}

/// Issue #80 notes — insert a footnote / endnote at a curated position
/// (which ENTERS the new note story), or leave the story again. Paired so
/// the story-aware selection / edit paths see real note stories, not just
/// the blind half's rarely-well-formed positions.
fn gen_note_command(u: &mut Unstructured) -> Option<Command> {
    Some(match small(u, 2) {
        0 => Command::InsertFootnote { at: pos(u) },
        1 => Command::InsertEndnote { at: pos(u) },
        _ => Command::ExitHeaderFooter,
    })
}

/// Inline images + issue #82 text wrap. `InsertImage` lands a real
/// image (tiny placeholder bytes — the native harness never decodes
/// them; dimensions from tiny to `u32`-scale to stress the EMU math and
/// the wrap geometry), then `SetImageWrap` addresses it by paragraph
/// path + byte offset — curated so it actually hits an image often,
/// with every wrap mode, instead of only the blind half's reject path.
fn gen_image_command(u: &mut Unstructured) -> Option<Command> {
    Some(match small(u, 2) {
        0 => Command::InsertImage {
            at: pos(u),
            image: ImageBlob {
                bytes: vec![0x89, b'P', b'N', b'G'],
                mime: "image/png".to_string(),
                width: if u.ratio(1, 8).unwrap_or(false) {
                    u32::arbitrary(u).ok()?
                } else {
                    small(u, 2000)
                },
                height: if u.ratio(1, 8).unwrap_or(false) {
                    u32::arbitrary(u).ok()?
                } else {
                    small(u, 2000)
                },
            },
            fit: *u
                .choose(&[ImageFit::Original, ImageFit::FitWidth, ImageFit::FitPage])
                .ok()?,
        },
        _ => {
            let at = pos(u);
            Command::SetImageWrap {
                path: at.path,
                at: at.offset,
                wrap: *u
                    .choose(&[
                        ImageWrapMode::Square,
                        ImageWrapMode::Tight,
                        ImageWrapMode::Through,
                        ImageWrapMode::TopAndBottom,
                        ImageWrapMode::BehindText,
                        ImageWrapMode::InFrontOfText,
                    ])
                    .ok()?,
            }
        }
    })
}

/// Build a sequence of up to `max_len` commands, mixing:
/// - ~40% curated, small-bounded commands (`gen_targeted_command`) —
///   insert / delete / format / table / section / story, per issue #90.
/// - ~15% selection / undo / caret motion (`gen_selection_command`).
/// - ~3% field authoring (`gen_field_command`).
/// - ~4% footnotes / endnotes (`gen_note_command`, issue #80).
/// - ~5% images + wrap (`gen_image_command`, issue #82).
/// - ~33% blind `Command::arbitrary` — full wire-schema breadth, including
///   variants the curated generator never touches (`LoadDocx`, `Recover`,
///   `SaveDocument`, viewport / zoom / IME commands, …) and out-of-range
///   addresses that stress the reject paths.
///
/// Nothing is filtered or clamped on the way out (see the note above where
/// `sanitize` used to live): every generated command reaches
/// `Engine::apply_sync` exactly as the wire would deliver it.
pub fn gen_command_sequence(u: &mut Unstructured, max_len: usize) -> Vec<Command> {
    let mut out = Vec::new();
    for _ in 0..max_len {
        if u.is_empty() {
            break;
        }
        let bucket = small(u, 99);
        let cmd = match bucket {
            0..=39 => gen_targeted_command(u),
            40..=54 => gen_selection_command(u),
            55..=57 => gen_field_command(u),
            58..=61 => gen_note_command(u),
            62..=66 => gen_image_command(u),
            // Blind, full-schema coverage — `Arbitrary::arbitrary` only
            // consumes what it needs from `u`, so the byte stream still has
            // entropy left for further loop iterations afterward.
            _ => Command::arbitrary(u).ok(),
        };
        let Some(cmd) = cmd else { break };
        out.push(cmd);
        if u.is_empty() {
            break;
        }
    }
    out
}
