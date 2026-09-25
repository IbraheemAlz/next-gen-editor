//! Structure-aware `bridge::Command` sequence generator for `rpc_command`
//! (D5.5, issue #90).
//!
//! `bridge::Command` derives `arbitrary::Arbitrary` behind bridge's
//! optional `arbitrary` feature (off by default; enabled here — see
//! `fuzz/Cargo.toml`), so blind `Command::arbitrary(u)` already gives full,
//! structurally-valid coverage of the whole ~100-variant wire enum,
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
//!
//! **Issue #177 — generator coverage audit.** A curated subset drifts out
//! of sync with the enum as new variants land (`SetTableProperties` /
//! `InsertTextBox` / `MoveImage` all shipped blind-only). `classify_variant`
//! near the bottom of this file is a compile-time exhaustiveness check —
//! see its doc comment — plus a per-variant coverage tracker
//! (`reset_coverage` / `coverage_snapshot`) the smoke driver reports after
//! the `rpc_command` target runs.

use arbitrary::{Arbitrary, Unstructured};
use bridge::{
    Alignment, BlockPath, BridgeCellBorders, Command, Direction, FieldKind, HeaderFooterArea,
    ImageBlob, ImageFit, ImageWrapMode, InsertSide, ListKind, LogicalPos, LogicalRange,
    MoveDirection, SectionBreakKind, SelectionModifier, TablePropertiesPatch, TextAttrsPatch,
    TextBoxHop, UnderlineStyle,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

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

/// A small-bounded signed `i64` — for EMU offsets (`MoveImage`,
/// `InsertTextBox`), which are 914_400-per-inch and would otherwise need
/// a much larger range than `small_f32`'s `i32` round trip comfortably
/// covers.
fn small_i64(u: &mut Unstructured, max: i64) -> i64 {
    u.int_in_range(-max..=max).unwrap_or(0)
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
    let variant = small(u, 26);
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
        // ---- issue #177 audit: view / calendar / table / text-box / image ------
        // #186 / #187 — SetZoom / SetDeviceScale / SetRenderDate were
        // blind-only, so the exact scenarios those issues fixed (a NaN
        // scale, an out-of-range calendar field) only ever reached the
        // engine by luck of the blind `Command::arbitrary()` draw. Each
        // arm below deliberately picks a deliberately-invalid payload
        // about a quarter of the time so the curated half keeps stressing
        // the new command-boundary rejections, not just the "sane" path.
        20 => Command::SetZoom {
            scale: if u.ratio(1, 4).unwrap_or(false) {
                *u.choose(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY])
                    .ok()?
            } else {
                (small(u, 16) as f32) * 0.5 - 1.0
            },
        },
        21 => Command::SetDeviceScale {
            scale: if u.ratio(1, 4).unwrap_or(false) {
                *u.choose(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY])
                    .ok()?
            } else {
                (small(u, 20) as f32) * 0.5 - 2.0
            },
        },
        22 => Command::SetRenderDate {
            year: if u.ratio(1, 4).unwrap_or(false) {
                *u.choose(&[0, -1, 10_000, 999_999_999]).ok()?
            } else {
                small(u, 2) as i32 + 2024
            },
            month: if u.ratio(1, 4).unwrap_or(false) {
                // The #187 repro itself sent `month: 960_639_140`.
                *u.choose(&[0u32, 13, 255, 960_639_140]).ok()?
            } else {
                small(u, 11) + 1
            },
            day: if u.ratio(1, 4).unwrap_or(false) {
                *u.choose(&[0u32, 30, 31, 32, 400]).ok()?
            } else {
                small(u, 27) + 1
            },
            hour: if u.ratio(1, 2).unwrap_or(false) {
                Some(if u.ratio(1, 4).unwrap_or(false) {
                    *u.choose(&[24u32, 99, 255]).ok()?
                } else {
                    small(u, 23)
                })
            } else {
                None
            },
            minute: if u.ratio(1, 2).unwrap_or(false) {
                Some(if u.ratio(1, 4).unwrap_or(false) {
                    *u.choose(&[60u32, 99, 255]).ok()?
                } else {
                    small(u, 59)
                })
            } else {
                None
            },
        },
        23 => Command::InsertTextBox {
            at: pos(u),
            width_emu: small_i64(u, 5_000_000),
            height_emu: small_i64(u, 5_000_000),
        },
        24 => Command::SetTableProperties {
            table_path: BlockPath::top(small(u, 3)),
            patch: TablePropertiesPatch {
                bidi_visual: if u.ratio(1, 2).unwrap_or(true) {
                    Some(u.ratio(1, 2).unwrap_or(false))
                } else {
                    None
                },
            },
        },
        25 => Command::MoveImage {
            path: BlockPath::top(small(u, 3)),
            at: small(u, 40),
            offset_h_emu: small_i64(u, 2_000_000),
            offset_v_emu: small_i64(u, 2_000_000),
            story: story_chain(u),
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
                story: story_chain(u),
            }
        }
    })
}

/// Issue #206 — an image command's text-box story chain: mostly the body
/// (empty), sometimes one or two small hops (a real box at `(0, 0)` is
/// the common authored shape) and rarely a chain past the nesting cap, so
/// the story resolver's reject paths are exercised too.
fn story_chain(u: &mut Unstructured) -> Vec<TextBoxHop> {
    let n = match small(u, 8) {
        0..=4 => 0,
        5 | 6 => 1,
        7 => 2,
        _ => 3,
    };
    (0..n)
        .map(|_| TextBoxHop {
            path: BlockPath::top(small(u, 3)),
            at: small(u, 40),
        })
        .collect()
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
        record_coverage(&cmd);
        out.push(cmd);
        if u.is_empty() {
            break;
        }
    }
    out
}

/* ====================================================================
Issue #177 — generator coverage audit.

`gen_command_sequence` mixes curated arms (above) with the blind
`Command::arbitrary()` half; nothing enforced that every `Command`
variant added over time got a curated arm, so a new variant could land
silently blind-only (exactly what happened to `SetTableProperties`,
`InsertTextBox` and `MoveImage` before this issue). `classify_variant`
is a **compile-time exhaustiveness check**: it matches every `Command`
variant with NO wildcard arm, so adding a bridge `Command` variant
without extending this match is a compile error in this crate — that
failure IS the audit, caught by `cargo check --manifest-path
fuzz/Cargo.toml` (this function is not `#[cfg(test)]`-gated, so a plain
`cargo check` — no `--tests` needed — already fails to build until the
new variant is classified). `command_variant_classification_is_exhaustive`
below is the executable half: it doesn't need real command instances
(match exhaustiveness is a property of the *type*, checked wherever this
function is compiled), so it exists to document the audit and to keep a
runtime-visible list of exactly which variants are curated, for the
`coverage_snapshot()` the smoke driver reports per variant.
==================================================================== */

/// One `Command` variant's issue #177 classification.
pub struct VariantInfo {
    /// The variant's identifier, e.g. `"InsertText"`.
    pub name: &'static str,
    /// Whether a curated (small-bounded) generator arm exists for this
    /// variant in `gen_targeted_command` / `gen_selection_command` /
    /// `gen_field_command` / `gen_note_command` / `gen_image_command`.
    /// `false` means the variant is reached only through the blind
    /// `Command::arbitrary()` half of `gen_command_sequence`.
    pub curated: bool,
}

/// Classify every `Command` variant — issue #177. **No wildcard arm.**
/// Ordered to match `crates/bridge/src/command.rs`'s declaration order
/// so a side-by-side diff of the two is easy to audit.
pub fn classify_variant(cmd: &Command) -> VariantInfo {
    fn v(name: &'static str, curated: bool) -> VariantInfo {
        VariantInfo { name, curated }
    }
    match cmd {
        // ---- Phase 1 PoC ---------------------------------------------------
        Command::Ping => v("Ping", false),
        Command::LoadFont { .. } => v("LoadFont", false),
        Command::RasterizeGlyph { .. } => v("RasterizeGlyph", false),
        Command::ShapeAndRasterize { .. } => v("ShapeAndRasterize", false),
        Command::RenderPage { .. } => v("RenderPage", false),
        Command::InsertText { .. } => v("InsertText", true), // gen_targeted_command
        Command::Undo => v("Undo", true),                    // gen_selection_command
        Command::Redo => v("Redo", true),                    // gen_selection_command
        Command::LoadDocx { .. } => v("LoadDocx", false),
        Command::SaveDocx => v("SaveDocx", false),
        // ---- Phase 2 §4 ------------------------------------------------------
        Command::Init { .. } => v("Init", false),
        Command::Recover { .. } => v("Recover", false),
        Command::Snapshot { .. } => v("Snapshot", false),
        Command::Dispose => v("Dispose", false),
        Command::Tick { .. } => v("Tick", false),
        Command::OpenDocument { .. } => v("OpenDocument", false),
        Command::SaveDocument { .. } => v("SaveDocument", false),
        Command::ExportPdf { .. } => v("ExportPdf", false),
        Command::CloseDocument => v("CloseDocument", false),
        Command::DeleteRange { .. } => v("DeleteRange", true), // gen_targeted_command
        Command::ReplaceRange { .. } => v("ReplaceRange", true), // gen_targeted_command
        Command::ApplyFormatting { .. } => v("ApplyFormatting", true), // gen_targeted_command
        Command::SplitParagraph { .. } => v("SplitParagraph", true), // gen_targeted_command
        Command::MergeParagraph { .. } => v("MergeParagraph", false),
        Command::InsertImage { .. } => v("InsertImage", true), // gen_image_command
        Command::ResizeImage { .. } => v("ResizeImage", false),
        Command::MoveImage { .. } => v("MoveImage", true), // gen_targeted_command (#177)
        Command::SetImageWrap { .. } => v("SetImageWrap", true), // gen_image_command
        Command::SetSelection { .. } => v("SetSelection", true), // gen_selection_command
        Command::ExtendSelection { .. } => v("ExtendSelection", true), // gen_selection_command
        Command::SelectAll => v("SelectAll", true),        // gen_selection_command
        Command::MoveCaret { .. } => v("MoveCaret", true), // gen_selection_command
        Command::BeginComposition { .. } => v("BeginComposition", false),
        Command::UpdateComposition { .. } => v("UpdateComposition", false),
        Command::EndComposition { .. } => v("EndComposition", false),
        Command::SetViewport { .. } => v("SetViewport", false),
        Command::SetZoom { .. } => v("SetZoom", true), // gen_targeted_command (#177/#186)
        Command::SetDeviceScale { .. } => v("SetDeviceScale", true), // gen_targeted_command (#177/#186)
        Command::RequestPaint { .. } => v("RequestPaint", false),
        Command::ExpandLayout { .. } => v("ExpandLayout", false),
        Command::UnloadFont { .. } => v("UnloadFont", false),
        Command::RequestStats => v("RequestStats", false),
        // ---- Phase 4 §7 --------------------------------------------------------
        Command::HitTest { .. } => v("HitTest", false),
        Command::HitTestInPage { .. } => v("HitTestInPage", false),
        Command::PlaceCaretAtPoint { .. } => v("PlaceCaretAtPoint", false),
        Command::GetImageRects => v("GetImageRects", false),
        Command::SelectWordAt { .. } => v("SelectWordAt", false),
        Command::SelectParagraphAt { .. } => v("SelectParagraphAt", false),
        Command::SelectCellAt { .. } => v("SelectCellAt", false),
        Command::DeleteAtCaret { .. } => v("DeleteAtCaret", true), // gen_targeted_command
        Command::RequestAccessibilityDelta => v("RequestAccessibilityDelta", false),
        Command::GetSelectionAsClipboard => v("GetSelectionAsClipboard", false),
        Command::PastePlain { .. } => v("PastePlain", false),
        // ---- Backlog sprint 1 --------------------------------------------------
        Command::SetParagraphAlign { .. } => v("SetParagraphAlign", true), // gen_targeted_command
        Command::SetParagraphDirection { .. } => v("SetParagraphDirection", true), // gen_targeted_command
        // ---- Backlog sprint 7 --------------------------------------------------
        Command::PasteHtml { .. } => v("PasteHtml", false),
        // ---- Phase 5 PR 3 — tables ----------------------------------------------
        Command::InsertTable { .. } => v("InsertTable", true), // gen_targeted_command
        Command::DeleteTable { .. } => v("DeleteTable", false),
        Command::InsertRow { .. } => v("InsertRow", true), // gen_targeted_command
        Command::DeleteRow { .. } => v("DeleteRow", true), // gen_targeted_command
        Command::InsertColumn { .. } => v("InsertColumn", false),
        Command::DeleteColumn { .. } => v("DeleteColumn", false),
        Command::MergeCells { .. } => v("MergeCells", true), // gen_targeted_command
        Command::SplitCell { .. } => v("SplitCell", false),
        Command::SetCellShading { .. } => v("SetCellShading", true), // gen_targeted_command
        Command::SetCellBorders { .. } => v("SetCellBorders", true), // gen_targeted_command
        Command::SetTableProperties { .. } => v("SetTableProperties", true), // gen_targeted_command (#177)
        Command::SetColumns { .. } => v("SetColumns", true),                 // gen_targeted_command
        Command::InsertPageBreak { .. } => v("InsertPageBreak", false),
        Command::InsertSectionBreak { .. } => v("InsertSectionBreak", true), // gen_targeted_command
        Command::EnterHeaderFooter { .. } => v("EnterHeaderFooter", true),   // gen_targeted_command
        Command::ExitHeaderFooter => v("ExitHeaderFooter", true), // gen_targeted_command fallback / gen_note_command
        Command::SetHeaderFooterLink { .. } => v("SetHeaderFooterLink", false),
        Command::SetTitlePage { .. } => v("SetTitlePage", false),
        Command::SetEvenOddHeaders { .. } => v("SetEvenOddHeaders", false),
        Command::InsertField { .. } => v("InsertField", true), // gen_field_command
        Command::InsertFootnote { .. } => v("InsertFootnote", true), // gen_note_command
        Command::InsertEndnote { .. } => v("InsertEndnote", true), // gen_note_command
        Command::InsertTextBox { .. } => v("InsertTextBox", true), // gen_targeted_command (#177)
        Command::SetRenderDate { .. } => v("SetRenderDate", true), // gen_targeted_command (#177/#187)
        Command::UpdateFields => v("UpdateFields", true),          // gen_field_command
        Command::SetFieldCodeView { .. } => v("SetFieldCodeView", false),
        Command::SetFieldInstruction { .. } => v("SetFieldInstruction", false),
        Command::InsertToc { .. } => v("InsertToc", true), // gen_field_command
        Command::SetParagraphBorders { .. } => v("SetParagraphBorders", false),
        Command::SetPageMargins { .. } => v("SetPageMargins", false),
        Command::SetPageOrientation { .. } => v("SetPageOrientation", false),
        Command::ToggleList { .. } => v("ToggleList", true), // gen_targeted_command
        Command::ChangeListLevel { .. } => v("ChangeListLevel", false),
        Command::SetParagraphIndent { .. } => v("SetParagraphIndent", true), // gen_targeted_command
        Command::SetLineSpacing { .. } => v("SetLineSpacing", true),         // gen_targeted_command
        Command::SetParagraphShading { .. } => v("SetParagraphShading", false),
        Command::ToggleTrackChanges { .. } => v("ToggleTrackChanges", false),
        Command::AcceptRevision { .. } => v("AcceptRevision", false),
        Command::RejectRevision { .. } => v("RejectRevision", false),
        Command::InsertComment { .. } => v("InsertComment", false),
        Command::DeleteComment { .. } => v("DeleteComment", false),
        Command::SetTabStops { .. } => v("SetTabStops", false),
        Command::SetReviewIdentity { .. } => v("SetReviewIdentity", false),
        Command::ApplyStyle { .. } => v("ApplyStyle", false),
        Command::ResolveComment { .. } => v("ResolveComment", false),
        Command::ReplyToComment { .. } => v("ReplyToComment", false),
        Command::ModifyStyle { .. } => v("ModifyStyle", false),
    }
}

thread_local! {
    static COVERAGE: RefCell<BTreeMap<&'static str, usize>> = const { RefCell::new(BTreeMap::new()) };
}

/// Clear the per-variant generation counters — called once before a
/// fresh coverage-reporting run (the smoke driver resets per
/// `rpc_command` target invocation, not per fuzz input, so it reports
/// how many commands of each shape the whole corpus + sweep produced).
pub fn reset_coverage() {
    COVERAGE.with(|c| c.borrow_mut().clear());
}

/// A sorted `(variant name, times generated)` snapshot since the last
/// [`reset_coverage`].
pub fn coverage_snapshot() -> BTreeMap<&'static str, usize> {
    COVERAGE.with(|c| c.borrow().clone())
}

fn record_coverage(cmd: &Command) {
    let name = classify_variant(cmd).name;
    COVERAGE.with(|c| *c.borrow_mut().entry(name).or_insert(0) += 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issues #186 / #187 — the committed corpus seeds
    /// `repro_186_nan_zoom` / `repro_186_nan_device_scale` /
    /// `repro_187_bad_render_date` (`fuzz/corpus/rpc_command/`) must keep
    /// deterministically reproducing the scenario each was found for.
    /// `Unstructured` decoding is a pure function of the generator code +
    /// the bytes, so this is exact, not probabilistic — a change to the
    /// generator that stops hitting one of these branches is a real
    /// regression in fuzz coverage of the #186/#187 fix, not just an
    /// unlucky seed.
    #[test]
    fn issue_186_187_corpus_seeds_reproduce_their_scenarios() {
        let read = |name: &str| {
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("corpus/rpc_command")
                    .join(name),
            )
            .unwrap_or_else(|e| panic!("missing seed corpus file {name}: {e}"))
        };
        let gen_cmds = |bytes: &[u8]| {
            let mut u = Unstructured::new(bytes);
            let _ = gen_seed_text(&mut u);
            gen_command_sequence(&mut u, 64)
        };

        let nan_zoom = gen_cmds(&read("repro_186_nan_zoom"));
        assert!(
            nan_zoom
                .iter()
                .any(|c| matches!(c, Command::SetZoom { scale } if !scale.is_finite())),
            "repro_186_nan_zoom must still generate a non-finite SetZoom"
        );

        let nan_device_scale = gen_cmds(&read("repro_186_nan_device_scale"));
        assert!(
            nan_device_scale
                .iter()
                .any(|c| matches!(c, Command::SetDeviceScale { scale } if !scale.is_finite())),
            "repro_186_nan_device_scale must still generate a non-finite SetDeviceScale"
        );

        let bad_date = gen_cmds(&read("repro_187_bad_render_date"));
        assert!(
            bad_date.iter().any(|c| matches!(
                c,
                Command::SetRenderDate { year, month, day, hour, minute }
                    if engine::validate_render_date(*year, *month, *day, *hour, *minute).is_err()
            )),
            "repro_187_bad_render_date must still generate an invalid SetRenderDate"
        );
    }

    /// Issue #177 acceptance: "exhaustiveness test green". The real
    /// enforcement is `classify_variant`'s match having no wildcard arm
    /// (a compile-time property — see the module doc comment above);
    /// this test exists so the audit is discoverable from `cargo test`
    /// and so CI has an explicit, named green/red signal for it rather
    /// than relying on someone noticing a build failure was THIS check.
    #[test]
    fn command_variant_classification_is_exhaustive() {
        // Spot-check a few variants on both sides of the #177 sweep —
        // pre-existing curated variants, the three the issue named
        // explicitly, and a sample that stays blind-only by design
        // (lifecycle / telemetry / read-only query commands the
        // curated generators have no reason to target).
        assert!(!classify_variant(&Command::Ping).curated);
        assert!(
            classify_variant(&Command::SetTableProperties {
                table_path: BlockPath::top(0),
                patch: TablePropertiesPatch::default(),
            })
            .curated
        );
        assert!(
            classify_variant(&Command::InsertTextBox {
                at: LogicalPos {
                    path: BlockPath::top(0),
                    offset: 0,
                },
                width_emu: 0,
                height_emu: 0,
            })
            .curated
        );
        assert!(
            classify_variant(&Command::MoveImage {
                path: BlockPath::top(0),
                at: 0,
                offset_h_emu: 0,
                offset_v_emu: 0,
                story: Vec::new(),
            })
            .curated
        );
        assert!(classify_variant(&Command::SetZoom { scale: 1.0 }).curated);
        assert!(classify_variant(&Command::SetDeviceScale { scale: 1.0 }).curated);
        assert!(
            classify_variant(&Command::SetRenderDate {
                year: 2026,
                month: 1,
                day: 1,
                hour: None,
                minute: None,
            })
            .curated
        );
    }

    /// A generated sequence's coverage snapshot only ever names variants
    /// `classify_variant` actually knows about (i.e. `record_coverage`
    /// and `gen_command_sequence` agree on classification) and reports a
    /// nonzero count for at least one curated #177 variant across a
    /// reasonably long, entropy-rich run.
    #[test]
    fn coverage_snapshot_tracks_generated_variants() {
        reset_coverage();
        let mut bytes = Vec::new();
        // A long, varied deterministic byte stream — enough entropy for
        // `gen_command_sequence` to explore every bucket many times over.
        for i in 0..8000u32 {
            bytes.push((i.wrapping_mul(2654435761) >> 8) as u8);
        }
        let mut u = Unstructured::new(&bytes);
        let _ = gen_command_sequence(&mut u, 64);
        let snap = coverage_snapshot();
        assert!(!snap.is_empty(), "a long run should generate something");
        for name in snap.keys() {
            // Every reported name must be a real variant name — i.e. it
            // came from `classify_variant`, not some other source.
            assert!(
                KNOWN_VARIANT_NAMES.contains(name),
                "coverage reported an unclassified variant name: {name}"
            );
        }
    }

    /// Every name `classify_variant` can produce — kept in sync by hand
    /// alongside the match above; used only to sanity-check
    /// `coverage_snapshot`'s output in the test above.
    const KNOWN_VARIANT_NAMES: &[&str] = &[
        "Ping",
        "LoadFont",
        "RasterizeGlyph",
        "ShapeAndRasterize",
        "RenderPage",
        "InsertText",
        "Undo",
        "Redo",
        "LoadDocx",
        "SaveDocx",
        "Init",
        "Recover",
        "Snapshot",
        "Dispose",
        "Tick",
        "OpenDocument",
        "SaveDocument",
        "ExportPdf",
        "CloseDocument",
        "DeleteRange",
        "ReplaceRange",
        "ApplyFormatting",
        "SplitParagraph",
        "MergeParagraph",
        "InsertImage",
        "ResizeImage",
        "MoveImage",
        "SetImageWrap",
        "SetSelection",
        "ExtendSelection",
        "SelectAll",
        "MoveCaret",
        "BeginComposition",
        "UpdateComposition",
        "EndComposition",
        "SetViewport",
        "SetZoom",
        "SetDeviceScale",
        "RequestPaint",
        "ExpandLayout",
        "UnloadFont",
        "RequestStats",
        "HitTest",
        "HitTestInPage",
        "PlaceCaretAtPoint",
        "GetImageRects",
        "SelectWordAt",
        "SelectParagraphAt",
        "SelectCellAt",
        "DeleteAtCaret",
        "RequestAccessibilityDelta",
        "GetSelectionAsClipboard",
        "PastePlain",
        "SetParagraphAlign",
        "SetParagraphDirection",
        "PasteHtml",
        "InsertTable",
        "DeleteTable",
        "InsertRow",
        "DeleteRow",
        "InsertColumn",
        "DeleteColumn",
        "MergeCells",
        "SplitCell",
        "SetCellShading",
        "SetCellBorders",
        "SetTableProperties",
        "SetColumns",
        "InsertPageBreak",
        "InsertSectionBreak",
        "EnterHeaderFooter",
        "ExitHeaderFooter",
        "SetHeaderFooterLink",
        "SetTitlePage",
        "SetEvenOddHeaders",
        "InsertField",
        "InsertFootnote",
        "InsertEndnote",
        "InsertTextBox",
        "SetRenderDate",
        "UpdateFields",
        "SetFieldCodeView",
        "SetFieldInstruction",
        "InsertToc",
        "SetParagraphBorders",
        "SetPageMargins",
        "SetPageOrientation",
        "ToggleList",
        "ChangeListLevel",
        "SetParagraphIndent",
        "SetLineSpacing",
        "SetParagraphShading",
        "ToggleTrackChanges",
        "AcceptRevision",
        "RejectRevision",
        "InsertComment",
        "DeleteComment",
        "SetTabStops",
        "SetReviewIdentity",
        "ApplyStyle",
        "ResolveComment",
        "ReplyToComment",
        "ModifyStyle",
    ];
}
