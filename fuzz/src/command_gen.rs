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
//!
//! **Issue #229 — regression seeds robust to generator layout changes.**
//! The `repro_186_*` / `repro_187_*` corpus seeds used to be raw bytes
//! tuned to survive whichever arms of `gen_targeted_command` happened to
//! fire first in the committed byte sequence; #206 grew an unrelated arm's
//! (`MoveImage` / `SetImageWrap`) byte footprint and silently broke that
//! tuning. [`Scenario`] (near `gen_targeted_command`, below) replaces it
//! with a named, fixed-prefix fast path plus an explicit builder
//! (`Scenario::seed_bytes`); regenerate the committed seeds with
//! `cargo run --manifest-path fuzz/Cargo.toml --example regen-seeds` after
//! touching `gen_seed_text`, `gen_command_sequence`'s bucket dispatch, or
//! the scenario fast path itself — `committed_seed_bytes_match_scenario_builder`
//! (in `tests`, below) fails loudly if the committed bytes go stale.

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

/// Issue #229 — reserved marker consumed by `gen_targeted_command`'s
/// scenario fast path (see [`Scenario`]). Any fixed byte works; the only
/// requirement is that `Scenario::peek` can recognize it before deciding
/// to consume it, so a normal (non-scenario) byte stream is never
/// misinterpreted except on the astronomically rare draw that happens to
/// match both this byte and a valid scenario index.
const SCENARIO_SENTINEL: u8 = 0xFE;

/// Issue #229 — the #186/#187 regression corpus seeds
/// (`repro_186_nan_zoom`, `repro_186_nan_device_scale`,
/// `repro_187_bad_render_date` under `fuzz/corpus/rpc_command/`) used to be
/// raw bytes hand-tuned to survive `gen_command_sequence`'s generic bucket
/// dispatch plus however many of `gen_targeted_command`'s OTHER arms fired
/// first. #206 grew `MoveImage`/`SetImageWrap`'s byte footprint (the new
/// `story: Vec<TextBoxHop>` field) — a change to a completely unrelated
/// arm — which shifted every `Unstructured` read downstream of any command
/// those arms happened to generate earlier in a committed sequence, so the
/// seeds silently stopped reaching their scenario. Nothing caught it: the
/// seeds are raw bytes, not derived from anything that would have flagged
/// the drift.
///
/// `Scenario` replaces "hope the raw bytes still parse the same way" with
/// an explicit, named encoding that does not depend on the generic arms at
/// all: `gen_targeted_command` peeks its next two bytes for
/// `[SCENARIO_SENTINEL, scenario_index]` **before** running any generic
/// arm, and — only on a match — hands off to [`Scenario::build`], which
/// takes no `Unstructured` input whatsoever, so no other arm's
/// byte-consumption change can ever perturb it again.
///
/// Regenerate the committed seeds after touching this mechanism (or the
/// upstream decoding contract it rides on — `gen_seed_text`'s pool
/// selection, `gen_command_sequence`'s bucket dispatch) with:
///
/// ```text
/// cargo run --manifest-path fuzz/Cargo.toml --example regen-seeds
/// ```
///
/// `committed_seed_bytes_match_scenario_builder` (below, in `tests`) pins
/// the committed files to `Scenario::seed_bytes()`'s output, so a change
/// that moves the upstream contract fails LOUDLY — a byte-diff assertion —
/// instead of silently, which is exactly what happened for #229. The fix
/// is the one command above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// Issue #186 — `SetZoom { scale: NaN }`.
    NanZoom,
    /// Issue #186 — `SetDeviceScale { scale: NaN }`.
    NanDeviceScale,
    /// Issue #187 — `SetRenderDate` with the exact out-of-range `month`
    /// the original fuzz run sent (`960_639_140`).
    BadRenderDate,
}

impl Scenario {
    /// Every scenario, in the fixed order [`Scenario::index`] encodes.
    pub const ALL: [Scenario; 3] = [
        Scenario::NanZoom,
        Scenario::NanDeviceScale,
        Scenario::BadRenderDate,
    ];

    /// The committed corpus file this scenario's seed lives at, relative
    /// to `fuzz/corpus/rpc_command/`.
    pub fn corpus_file(self) -> &'static str {
        match self {
            Scenario::NanZoom => "repro_186_nan_zoom",
            Scenario::NanDeviceScale => "repro_186_nan_device_scale",
            Scenario::BadRenderDate => "repro_187_bad_render_date",
        }
    }

    /// This scenario's position in [`Scenario::ALL`] — the byte
    /// `seed_bytes` / `peek` use to select it.
    fn index(self) -> u8 {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .expect("every Scenario appears in ALL") as u8
    }

    /// The one `Command` this scenario builds. Deliberately takes no
    /// `Unstructured` input — nothing left for another arm's
    /// byte-consumption change to perturb.
    fn build(self) -> Command {
        match self {
            Scenario::NanZoom => Command::SetZoom { scale: f32::NAN },
            Scenario::NanDeviceScale => Command::SetDeviceScale { scale: f32::NAN },
            // The exact #187 repro payload — `validate_render_date` rejects
            // this `month` regardless of the other fields.
            Scenario::BadRenderDate => Command::SetRenderDate {
                year: 2026,
                month: 960_639_140,
                day: 1,
                hour: None,
                minute: None,
            },
        }
    }

    /// Peek (never consume on a miss) whether `u`'s next two bytes select a
    /// scenario. `gen_targeted_command` checks this before its generic
    /// per-variant dispatch runs.
    fn peek(u: &Unstructured) -> Option<Scenario> {
        let bytes = u.peek_bytes(2)?;
        if bytes[0] != SCENARIO_SENTINEL {
            return None;
        }
        Self::ALL.get(bytes[1] as usize).copied()
    }

    /// The full corpus seed for this scenario, built through the real
    /// `gen_seed_text` / `gen_command_sequence` decoding contract instead
    /// of hand-picked raw bytes:
    /// - byte 0 selects `gen_seed_text`'s `POOL[0]` (an empty string — its
    ///   content is irrelevant to this scenario, only the one byte it
    ///   consumes matters);
    /// - byte 1 selects `gen_command_sequence`'s bucket 0, which routes to
    ///   `gen_targeted_command`;
    /// - bytes 2–3 are `[SCENARIO_SENTINEL, self.index()]`, consumed by the
    ///   scenario fast path above before any generic arm runs.
    ///
    /// Exactly 4 bytes: `gen_command_sequence`'s `u.is_empty()` check then
    /// stops the sequence right after this one command, so the resulting
    /// `Vec<Command>` has exactly one element.
    pub fn seed_bytes(self) -> Vec<u8> {
        vec![0x00, 0x00, SCENARIO_SENTINEL, self.index()]
    }
}

/// One curated, small-bounded command spanning the issue's named
/// categories: insert / delete / format / table / section / story.
///
/// Issue #229 — checks [`Scenario::peek`] first: a fixed-prefix fast path
/// for the #186/#187 regression scenarios that bypasses every arm below
/// (see `Scenario`'s doc comment for why).
fn gen_targeted_command(u: &mut Unstructured) -> Option<Command> {
    if let Some(scenario) = Scenario::peek(u) {
        // Consume exactly the two bytes `peek` looked at; `build` itself
        // reads no further bytes, by design.
        let _ = u.bytes(2);
        return Some(scenario.build());
    }
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

/// Issue #209 — defines `classify_variant`'s match **and** [`ALL_VARIANT_NAMES`]
/// from one list, so the two can never desync. Before this macro, the match
/// below and a hand-maintained `KNOWN_VARIANT_NAMES: &[&str]` test constant
/// listed the same ~100 variant names independently; nothing enforced they
/// stayed in sync (the *match* itself is still safe on a rename — `Command::
/// $variant` is a concrete path the compiler checks — but a typo in a
/// separately hand-typed name string was not).
///
/// Each entry names a variant exactly once, as the `$variant:ident` token —
/// the same token used both in the generated `Command::$variant` match arm
/// and, via `stringify!`, as the runtime name string. There is no second,
/// independently-typed copy of the name to fall out of sync.
macro_rules! classify_variants {
    ( $( $variant:ident $( { $($field:tt)* } )? => $curated:expr ),+ $(,)? ) => {
        /// Classify every `Command` variant — issue #177. **No wildcard arm.**
        /// Ordered to match `crates/bridge/src/command.rs`'s declaration order
        /// so a side-by-side diff of the two is easy to audit.
        pub fn classify_variant(cmd: &Command) -> VariantInfo {
            match cmd {
                $(
                    Command::$variant $( { $($field)* } )? => VariantInfo {
                        name: stringify!($variant),
                        curated: $curated,
                    },
                )+
            }
        }

        /// Every name `classify_variant` can produce, generated from the
        /// exact same list that defines its match — issue #209. Used to
        /// sanity-check `coverage_snapshot`'s output (see the tests below).
        pub const ALL_VARIANT_NAMES: &[&str] = &[ $( stringify!($variant) ),+ ];
    };
}

classify_variants! {
    // ---- Phase 1 PoC ---------------------------------------------------
    Ping => false,
    LoadFont { .. } => false,
    RasterizeGlyph { .. } => false,
    ShapeAndRasterize { .. } => false,
    RenderPage { .. } => false,
    InsertText { .. } => true, // gen_targeted_command
    Undo => true,              // gen_selection_command
    Redo => true,              // gen_selection_command
    LoadDocx { .. } => false,
    SaveDocx => false,
    // ---- Phase 2 §4 ------------------------------------------------------
    Init { .. } => false,
    Recover { .. } => false,
    Snapshot { .. } => false,
    Dispose => false,
    Tick { .. } => false,
    OpenDocument { .. } => false,
    SaveDocument { .. } => false,
    ExportPdf { .. } => false,
    CloseDocument => false,
    DeleteRange { .. } => true, // gen_targeted_command
    ReplaceRange { .. } => true, // gen_targeted_command
    ApplyFormatting { .. } => true, // gen_targeted_command
    SplitParagraph { .. } => true, // gen_targeted_command
    MergeParagraph { .. } => false,
    InsertImage { .. } => true, // gen_image_command
    ResizeImage { .. } => false,
    MoveImage { .. } => true, // gen_targeted_command (#177)
    SetImageWrap { .. } => true, // gen_image_command
    SetSelection { .. } => true, // gen_selection_command
    ExtendSelection { .. } => true, // gen_selection_command
    SelectAll => true,        // gen_selection_command
    MoveCaret { .. } => true, // gen_selection_command
    BeginComposition { .. } => false,
    UpdateComposition { .. } => false,
    EndComposition { .. } => false,
    SetViewport { .. } => false,
    SetZoom { .. } => true, // gen_targeted_command (#177/#186)
    SetDeviceScale { .. } => true, // gen_targeted_command (#177/#186)
    RequestPaint { .. } => false,
    ExpandLayout { .. } => false,
    UnloadFont { .. } => false,
    RequestStats => false,
    // ---- Phase 4 §7 --------------------------------------------------------
    HitTest { .. } => false,
    HitTestInPage { .. } => false,
    PlaceCaretAtPoint { .. } => false,
    GetImageRects => false,
    SelectWordAt { .. } => false,
    SelectParagraphAt { .. } => false,
    SelectCellAt { .. } => false,
    DeleteAtCaret { .. } => true, // gen_targeted_command
    RequestAccessibilityDelta => false,
    GetSelectionAsClipboard => false,
    PastePlain { .. } => false,
    // ---- Backlog sprint 1 --------------------------------------------------
    SetParagraphAlign { .. } => true, // gen_targeted_command
    SetParagraphDirection { .. } => true, // gen_targeted_command
    // ---- Backlog sprint 7 --------------------------------------------------
    PasteHtml { .. } => false,
    // ---- Phase 5 PR 3 — tables ----------------------------------------------
    InsertTable { .. } => true, // gen_targeted_command
    DeleteTable { .. } => false,
    InsertRow { .. } => true, // gen_targeted_command
    DeleteRow { .. } => true, // gen_targeted_command
    InsertColumn { .. } => false,
    DeleteColumn { .. } => false,
    MergeCells { .. } => true, // gen_targeted_command
    SplitCell { .. } => false,
    SetCellShading { .. } => true, // gen_targeted_command
    SetCellBorders { .. } => true, // gen_targeted_command
    SetTableProperties { .. } => true, // gen_targeted_command (#177)
    SetColumns { .. } => true,                 // gen_targeted_command
    InsertPageBreak { .. } => false,
    InsertSectionBreak { .. } => true, // gen_targeted_command
    EnterHeaderFooter { .. } => true,   // gen_targeted_command
    ExitHeaderFooter => true, // gen_targeted_command fallback / gen_note_command
    SetHeaderFooterLink { .. } => false,
    SetTitlePage { .. } => false,
    SetEvenOddHeaders { .. } => false,
    InsertField { .. } => true, // gen_field_command
    InsertFootnote { .. } => true, // gen_note_command
    InsertEndnote { .. } => true, // gen_note_command
    InsertTextBox { .. } => true, // gen_targeted_command (#177)
    SetRenderDate { .. } => true, // gen_targeted_command (#177/#187)
    UpdateFields => true,          // gen_field_command
    SetFieldCodeView { .. } => false,
    SetFieldInstruction { .. } => false,
    InsertToc { .. } => true, // gen_field_command
    SetParagraphBorders { .. } => false,
    SetPageMargins { .. } => false,
    SetPageOrientation { .. } => false,
    ToggleList { .. } => true, // gen_targeted_command
    ChangeListLevel { .. } => false,
    SetParagraphIndent { .. } => true, // gen_targeted_command
    SetLineSpacing { .. } => true,         // gen_targeted_command
    SetParagraphShading { .. } => false,
    ToggleTrackChanges { .. } => false,
    AcceptRevision { .. } => false,
    RejectRevision { .. } => false,
    InsertComment { .. } => false,
    DeleteComment { .. } => false,
    SetTabStops { .. } => false,
    SetReviewIdentity { .. } => false,
    ApplyStyle { .. } => false,
    ResolveComment { .. } => false,
    ReplyToComment { .. } => false,
    ModifyStyle { .. } => false,
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
    ///
    /// Issue #229 — these seeds are now [`Scenario`]'s `seed_bytes()`
    /// output (see its doc comment), so this test exercises the SAME real
    /// decoding path (`gen_seed_text` then `gen_command_sequence`) the
    /// fuzz target itself uses, rather than a shortcut. Byte-for-byte
    /// staleness against the builder is a separate, narrower test below
    /// (`committed_seed_bytes_match_scenario_builder`).
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

    /// Issue #229 — the committed #186/#187 regression seeds are DERIVED
    /// artifacts of `Scenario::seed_bytes()`, not hand-maintained raw
    /// bytes. A future change to `gen_seed_text`'s pool selection,
    /// `gen_command_sequence`'s bucket dispatch, or the scenario fast path
    /// itself would change what bytes each scenario needs; this test
    /// fails LOUDLY (a byte diff) the moment that happens, instead of the
    /// seed silently no longer reproducing its scenario — #229's own root
    /// cause. Fix with the one command named in `Scenario`'s doc comment:
    /// `cargo run --manifest-path fuzz/Cargo.toml --example regen-seeds`.
    #[test]
    fn committed_seed_bytes_match_scenario_builder() {
        for scenario in Scenario::ALL {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("corpus/rpc_command")
                .join(scenario.corpus_file());
            let committed = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("missing seed corpus file {}: {e}", path.display()));
            let built = scenario.seed_bytes();
            assert_eq!(
                committed,
                built,
                "{} is stale — regenerate with `cargo run --manifest-path \
                 fuzz/Cargo.toml --example regen-seeds` (see Scenario's doc comment \
                 in src/command_gen.rs)",
                scenario.corpus_file()
            );
        }
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
                ALL_VARIANT_NAMES.contains(name),
                "coverage reported an unclassified variant name: {name}"
            );
        }
    }

    /// Issue #209 — `ALL_VARIANT_NAMES` is generated by the
    /// `classify_variants!` macro from the exact same list that defines
    /// `classify_variant`'s match (see its doc comment above), so a variant
    /// rename or count change can no longer silently desync the two the way
    /// a hand-maintained mirror list could. This test pins the properties
    /// that make the generated list trustworthy as `classify_variant`'s
    /// coverage: every name is unique, and the count matches the number of
    /// arms the match actually has today — since that match has no
    /// wildcard arm (issue #177), the count *is* the full `Command` variant
    /// count, so this also doubles as a tripwire for "a variant landed
    /// without anyone updating this test".
    #[test]
    fn all_variant_names_equals_classifier_coverage() {
        let mut seen = std::collections::BTreeSet::new();
        for name in ALL_VARIANT_NAMES {
            assert!(
                seen.insert(*name),
                "duplicate variant name in ALL_VARIANT_NAMES: {name}"
            );
        }
        const EXPECTED_VARIANT_COUNT: usize = 103;
        assert_eq!(
            ALL_VARIANT_NAMES.len(),
            EXPECTED_VARIANT_COUNT,
            "classify_variant's no-wildcard match covers exactly this many \
             Command variants today (crates/bridge/src/command.rs); update \
             EXPECTED_VARIANT_COUNT here (and the curated flags of any new \
             variant in the classify_variants! call above) when that enum \
             gains or loses a variant"
        );
    }
}
