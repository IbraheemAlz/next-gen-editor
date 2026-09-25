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
    InsertSide, ListKind, LogicalPos, LogicalRange, MoveDirection, SectionBreakKind,
    SelectionModifier, TextAttrsPatch, UnderlineStyle,
};

/// `engine::DocumentTree::insert_table` (`crates/engine/src/lib.rs`) does
/// `Vec::with_capacity(rows.max(1))` of rows each doing
/// `Vec::with_capacity(cols)` of cells, with NO upper bound on the
/// wire-supplied `rows` / `cols` (`u32`) before either allocation (D5.5,
/// issue #90 finding). Two large-but-plausible `u32`s (the derived
/// `Arbitrary` impl happily produces values near `u32::MAX`) multiply into
/// a multi-gigabyte-to-terabyte allocation request that ABORTS THE PROCESS
/// via Rust's default OOM handler — not a panic, so
/// `std::panic::catch_unwind` cannot save a caller from it (confirmed:
/// this crashed `examples/smoke.rs` outright on iteration 1 before this
/// clamp was added). This is a real, reproducible, unbounded-allocation
/// DoS reachable directly from the untrusted RPC `Command` surface — see
/// the PR description for the full writeup and a minimal repro. Clamped
/// in `sanitize` (below) to keep a fuzzing SESSION alive long enough to
/// find other bugs too, not to hide this one: `rows` / `cols` still cross
/// realistic small-table boundaries (0, 1, a few dozen), just not far
/// enough to OOM the harness on every single run.
const MAX_TABLE_DIM: u32 = 40;

/// Three commands are known, reproducible native-only dead ends (D5.5,
/// issue #90 finding) — all three eventually reach a raw `js_sys::Date`
/// call, which panics with "cannot call wasm-bindgen imported functions on
/// non-wasm targets" outside a browser:
///
/// - `Command::InsertComment` / `ReplyToComment` — `do_insert_comment` /
///   `do_reply_to_comment` call `js_sys::Date::new_0()` directly, unlike
///   every OTHER timestamp site, which goes through
///   `Engine::current_review_date`'s `review_date` override. Filtered to
///   `Ping` — there is no field on either command that routes around it.
/// - `Command::Recover` — `do_recover` (the cold-reset stub;
///   `Command::Recover` is documented as still a stub in `CLAUDE.md`)
///   unconditionally resets `review_date` to `""`, re-arming
///   `current_review_date`'s `js_sys::Date` fallback for the next
///   tracked-mutation command. `Recover`'s own fields (`snapshot`,
///   `log_tail`) don't control `review_date`, so — unlike
///   `SetReviewIdentity` below — there's no field-level fix; filtered to
///   `Ping` as well.
/// - `Command::SetReviewIdentity { date: "" }` — same `review_date` reset,
///   but THIS command's own `date` field is exactly what's empty, so it's
///   patched to a placeholder instead of filtered outright: `author` and a
///   non-empty `date` are still real, exercised values.
///
/// None of this is a real product bug — a real browser session always has
/// `Date` available — it's this harness's no-browser constraint. See the
/// PR description for the full writeup.
fn sanitize(cmd: Command) -> Command {
    match cmd {
        Command::InsertComment { .. }
        | Command::ReplyToComment { .. }
        | Command::Recover { .. } => Command::Ping,
        Command::InsertTable { at, rows, cols } => Command::InsertTable {
            at,
            rows: rows.min(MAX_TABLE_DIM),
            cols: cols.min(MAX_TABLE_DIM),
        },
        Command::SetReviewIdentity { author, date } => Command::SetReviewIdentity {
            author,
            date: if date.is_empty() {
                "2026-01-01T00:00:00Z".to_string()
            } else {
                date
            },
        },
        other => other,
    }
}

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

/// Build a sequence of up to `max_len` commands, mixing:
/// - ~40% curated, small-bounded commands (`gen_targeted_command`) —
///   insert / delete / format / table / section / story, per issue #90.
/// - ~15% selection / undo / caret motion (`gen_selection_command`).
/// - ~5% field authoring (`gen_field_command`).
/// - ~40% blind `Command::arbitrary` — full wire-schema breadth, including
///   variants the curated generator never touches (`LoadDocx`, `Recover`,
///   `SaveDocument`, viewport / zoom / IME commands, …) and out-of-range
///   addresses that stress the reject paths.
///
/// Every command passes through `sanitize` before being pushed, so the two
/// native-only-panicking comment variants never reach `Engine::apply_sync`
/// (see `sanitize`'s doc comment).
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
            55..=59 => gen_field_command(u),
            // Blind, full-schema coverage — `Arbitrary::arbitrary` only
            // consumes what it needs from `u`, so the byte stream still has
            // entropy left for further loop iterations afterward.
            _ => Command::arbitrary(u).ok(),
        };
        let Some(cmd) = cmd else { break };
        out.push(sanitize(cmd));
        if u.is_empty() {
            break;
        }
    }
    out
}
