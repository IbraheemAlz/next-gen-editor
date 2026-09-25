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

/// Issue #229 — reserved marker consumed by [`gen_command_sequence`]'s
/// scenario fast path (see [`Scenario`]). Any fixed byte works; the only
/// requirement is that `Scenario::peek` can recognize it before deciding
/// to consume it, so a normal (non-scenario) byte stream is never
/// misinterpreted except on the astronomically rare draw that happens to
/// match both this byte and a valid scenario index.
const SCENARIO_SENTINEL: u8 = 0xFE;

/// Issue #229 (extended by #261) — every committed regression seed under
/// `fuzz/corpus/rpc_command/` is a [`Scenario`], never hand-tuned raw
/// bytes. The original #186/#187 seeds (`repro_186_nan_zoom`,
/// `repro_186_nan_device_scale`, `repro_187_bad_render_date`) used to be
/// raw bytes hand-tuned to survive `gen_command_sequence`'s generic bucket
/// dispatch plus however many of `gen_targeted_command`'s OTHER arms fired
/// first. #206 grew `MoveImage`/`SetImageWrap`'s byte footprint (the new
/// `story: Vec<TextBoxHop>` field) — a change to a completely unrelated
/// arm — which shifted every `Unstructured` read downstream of any command
/// those arms happened to generate earlier in a committed sequence, so the
/// seeds silently stopped reaching their scenario. Nothing caught it: the
/// seeds were raw bytes, not derived from anything that would have flagged
/// the drift. #261 found the same fragility in the older #115/#116/#117
/// seeds (`repro_115_*`, `repro_116_1`, `repro_117_*`) and converted them
/// the same way, extending `Scenario` from one `Command` to an ordered
/// `Vec<Command>` (several of those regressions only reproduce as a short
/// sequence — insert multi-byte text then address it, create a table then
/// merge past its shape, select then undo the very edit selected).
///
/// `Scenario` replaces "hope the raw bytes still parse the same way" with
/// an explicit, named encoding that does not depend on any generic arm at
/// all: [`gen_command_sequence`] peeks its next two bytes for
/// `[SCENARIO_SENTINEL, scenario_index]` **first, every iteration**,
/// before its percentage-bucket dispatch runs, and — only on a match —
/// splices in [`Scenario::build`]'s commands, which take no `Unstructured`
/// input whatsoever, so no other arm's byte-consumption change (nor the
/// seed text pool's) can ever perturb them again.
///
/// **New regression seeds must be [`Scenario`] variants, never hand-tuned
/// raw bytes** — add the variant, regenerate, extend
/// `committed_seed_bytes_match_scenario_builder`. Regenerate the committed
/// seeds after touching this mechanism (or the upstream decoding contract
/// it rides on — `gen_seed_text`'s pool selection, `gen_command_sequence`'s
/// bucket dispatch) with:
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
    /// Issue #115 / #261 — `DeleteRange` whose ends land inside a
    /// multi-byte UTF-8 scalar (Arabic BEH, ×3). Was
    /// `repro_115_1`'s raw-byte reproducer.
    TextRangeCharBoundaryDeleteRange,
    /// Issue #115 / #261 — `ReplaceRange` landing inside a combining
    /// mark. Was `repro_115_2`.
    TextRangeCharBoundaryReplaceRange,
    /// Issue #115 / #261 — `SplitParagraph` landing inside an emoji
    /// scalar. Was `repro_115_3`.
    TextRangeCharBoundarySplitParagraph,
    /// Issue #115 / #261 — `ApplyFormatting { range: Some(_) }` landing
    /// inside a multi-byte scalar. Was `repro_115_4`.
    TextRangeCharBoundaryApplyFormatting,
    /// Issue #115 / #261 — the INTERACTIVE `InsertText { at: Some(_) }`
    /// path (`clamp_pos`, not `resolve_edit_pos`) landing inside a
    /// multi-byte scalar. Was `repro_115_5`.
    TextRangeCharBoundaryInsertText,
    /// Issue #115 / #261 — an IME composition anchored mid-scalar
    /// (`BeginComposition` stores `at` verbatim; the snap only happens
    /// at commit time). Was `repro_115_composition_overlay`.
    TextRangeCharBoundaryCompositionCommit,
    /// Issue #116 / #261 — `MergeCells` addressing a row/column rectangle
    /// past the table's actual shape. Was `repro_116_1`.
    TableMergeCellsOutOfRange,
    /// Issue #117 / #261 — `SetSelection` with a wildly out-of-range
    /// range/caret (including a path naming a block that doesn't exist).
    /// Was `repro_117_1`.
    SelectionClampOutOfRange,
    /// Issue #117 / #261 — a valid selection at the end of the document,
    /// then `Undo` reverts the very edit it addressed — the selection
    /// must be re-clamped before the post-undo repaint, not left
    /// dangling for a later command to "self-heal". Was
    /// `repro_117_undo_repaint_error`.
    SelectionClampAfterUndo,
}

impl Scenario {
    /// Every scenario, in the fixed order [`Scenario::index`] encodes.
    pub const ALL: [Scenario; 12] = [
        Scenario::NanZoom,
        Scenario::NanDeviceScale,
        Scenario::BadRenderDate,
        Scenario::TextRangeCharBoundaryDeleteRange,
        Scenario::TextRangeCharBoundaryReplaceRange,
        Scenario::TextRangeCharBoundarySplitParagraph,
        Scenario::TextRangeCharBoundaryApplyFormatting,
        Scenario::TextRangeCharBoundaryInsertText,
        Scenario::TextRangeCharBoundaryCompositionCommit,
        Scenario::TableMergeCellsOutOfRange,
        Scenario::SelectionClampOutOfRange,
        Scenario::SelectionClampAfterUndo,
    ];

    /// The committed corpus file this scenario's seed lives at, relative
    /// to `fuzz/corpus/rpc_command/`.
    pub fn corpus_file(self) -> &'static str {
        match self {
            Scenario::NanZoom => "repro_186_nan_zoom",
            Scenario::NanDeviceScale => "repro_186_nan_device_scale",
            Scenario::BadRenderDate => "repro_187_bad_render_date",
            Scenario::TextRangeCharBoundaryDeleteRange => "repro_115_1",
            Scenario::TextRangeCharBoundaryReplaceRange => "repro_115_2",
            Scenario::TextRangeCharBoundarySplitParagraph => "repro_115_3",
            Scenario::TextRangeCharBoundaryApplyFormatting => "repro_115_4",
            Scenario::TextRangeCharBoundaryInsertText => "repro_115_5",
            Scenario::TextRangeCharBoundaryCompositionCommit => {
                "repro_115_composition_overlay"
            }
            Scenario::TableMergeCellsOutOfRange => "repro_116_1",
            Scenario::SelectionClampOutOfRange => "repro_117_1",
            Scenario::SelectionClampAfterUndo => "repro_117_undo_repaint_error",
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

    /// Issue #115/#261 scenario setup — reset the document to a single
    /// paragraph of `text` AND clear the live selection to `None` (the
    /// `Command::RenderPage` / Phase-1 contract — see `render_page`'s doc
    /// comment). Several commands below (`SplitParagraph`, the
    /// interactive `InsertText { at: Some(_) }`, `BeginComposition` /
    /// `EndComposition`) consult their own explicit position argument
    /// ONLY when no selection exists; `Engine::new_headless` otherwise
    /// always seeds one (fresh-document caret at the top), which would
    /// silently redirect the edit to the LIVE caret instead of the
    /// mid-scalar offset each scenario means to exercise. A plain
    /// `Command::InsertText { at: None, .. }` does NOT clear the
    /// selection this way — it just moves it — so it cannot substitute
    /// here (verified: it was this file's first draft, and every
    /// selection-dependent scenario below failed until switched to this).
    /// `font_id` must match the bundled font `Engine::new_headless` loads
    /// ("fuzz-latin") so the render this triggers has something to shape
    /// with.
    fn seed_text_no_selection(text: &str) -> Command {
        Command::RenderPage {
            text: text.to_string(),
            font_id: "fuzz-latin".to_string(),
            base_direction: "ltr".to_string(),
            px_size: 16.0,
            line_height: 26.0,
            align: "start".to_string(),
            device_pixel_ratio: None,
        }
    }

    /// The ordered `Command` sequence this scenario builds. Deliberately
    /// takes no `Unstructured` input — nothing left for another arm's
    /// byte-consumption change to perturb. Issue #261 widened this from a
    /// single `Command` to a `Vec<Command>`: several of the #115/#116/#117
    /// regressions only reproduce as a short SEQUENCE (insert the
    /// multi-byte text, THEN address it; create a table, THEN merge past
    /// its shape; select, THEN undo the edit the selection addressed) —
    /// see [`gen_command_sequence`]'s scenario fast path, which splices in
    /// every command this returns before resuming the generic dispatch.
    ///
    /// The #115 scenarios below insert their own seed text via an
    /// explicit `Command::InsertText` rather than steering
    /// `gen_seed_text`'s pool — a scenario must stay immune to EVERY
    /// upstream generator's byte layout, seed text included, not just the
    /// command dispatch #229 originally fixed.
    fn build(self) -> Vec<Command> {
        match self {
            Scenario::NanZoom => vec![Command::SetZoom { scale: f32::NAN }],
            Scenario::NanDeviceScale => vec![Command::SetDeviceScale { scale: f32::NAN }],
            // The exact #187 repro payload — `validate_render_date` rejects
            // this `month` regardless of the other fields.
            Scenario::BadRenderDate => vec![Command::SetRenderDate {
                year: 2026,
                month: 960_639_140,
                day: 1,
                hour: None,
                minute: None,
            }],
            Scenario::TextRangeCharBoundaryDeleteRange => vec![
                // "ببب" — 3 Arabic BEH letters, 2 bytes each (char
                // boundaries at 0/2/4/6). Offsets 1 and 5 both land
                // strictly inside a scalar — the exact `is_char_boundary`
                // panic shape #115 fixed.
                Command::InsertText {
                    at: None,
                    text: "ببب".to_string(),
                },
                Command::DeleteRange {
                    range: LogicalRange {
                        start: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 1,
                        },
                        end: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 5,
                        },
                    },
                },
            ],
            Scenario::TextRangeCharBoundaryReplaceRange => vec![
                // A base letter plus 3 stacked combining marks (the exact
                // shape `gen_seed_text`'s own pool uses for this bug
                // class). Boundaries 0/1/3/5/7; offsets 2 and 6 land
                // inside a mark.
                Command::InsertText {
                    at: None,
                    text: "e\u{0301}\u{0301}\u{0301}".to_string(),
                },
                Command::ReplaceRange {
                    range: LogicalRange {
                        start: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 2,
                        },
                        end: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 6,
                        },
                    },
                    text: "Z".to_string(),
                },
            ],
            Scenario::TextRangeCharBoundarySplitParagraph => vec![
                // Two emoji, 4 bytes each (boundaries 0/4/8); offset 2
                // lands inside the first scalar. `do_split_paragraph`
                // only consults its `at` argument when there is no live
                // selection — see `seed_text_no_selection`.
                Self::seed_text_no_selection("\u{1F642}\u{1F642}"),
                Command::SplitParagraph {
                    at: Some(LogicalPos {
                        path: BlockPath::top(0),
                        offset: 2,
                    }),
                },
            ],
            Scenario::TextRangeCharBoundaryApplyFormatting => vec![
                // No live selection ⇒ `apply_formatting`'s tail reports
                // `FormattingChanged { range, .. }` with the FLOORED
                // range visible in the event (with a selection it
                // reports the unrelated `SelectionChanged` instead —
                // see `seed_text_no_selection`).
                Self::seed_text_no_selection("بببب"),
                Command::ApplyFormatting {
                    range: Some(LogicalRange {
                        start: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 1,
                        },
                        end: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 7,
                        },
                    }),
                    attrs: TextAttrsPatch {
                        bold: Some(true),
                        italic: None,
                        underline: None,
                        strike: None,
                        font_family: None,
                        font_size: None,
                        color: None,
                        bg_color: None,
                        script: None,
                        language: None,
                        caps: None,
                        small_caps: None,
                    },
                },
            ],
            Scenario::TextRangeCharBoundaryInsertText => vec![
                // No live selection ⇒ the interactive `InsertText`'s
                // `at` argument actually seeds the edit position (with a
                // selection it is discarded in favor of the live caret —
                // see `seed_text_no_selection`). This exercises
                // `clamp_pos`, not `resolve_edit_pos` — a distinct #115
                // code path from the explicit-range commands above.
                Self::seed_text_no_selection("بببب"),
                Command::InsertText {
                    at: Some(LogicalPos {
                        path: BlockPath::top(0),
                        offset: 3,
                    }),
                    text: "X".to_string(),
                },
            ],
            Scenario::TextRangeCharBoundaryCompositionCommit => vec![
                // No live selection ⇒ the eventual commit's
                // `do_insert_text_interactive` call actually seeds from
                // the composition's OWN anchor instead of discarding it
                // for a live caret — see `seed_text_no_selection`.
                // `BeginComposition` stores `at` VERBATIM (issue #64) —
                // the char-boundary snap only happens at commit time,
                // through `do_insert_text_interactive`'s `clamp_pos`.
                // Composing at a mid-scalar anchor and committing is
                // `repro_115_composition_overlay`'s shape: the IME
                // preview used to cut Arabic mid-scalar.
                Self::seed_text_no_selection("بببب"),
                Command::BeginComposition {
                    at: Some(LogicalPos {
                        path: BlockPath::top(0),
                        offset: 3,
                    }),
                },
                Command::UpdateComposition {
                    text: "Y".to_string(),
                    target_range: Some(LogicalRange {
                        start: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 3,
                        },
                        end: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 5,
                        },
                    }),
                },
                Command::EndComposition { commit: true },
            ],
            Scenario::TableMergeCellsOutOfRange => vec![
                Command::InsertTable {
                    at: BlockPath::top(0),
                    rows: 1,
                    cols: 1,
                },
                // Every corner of a `MergeCells` rectangle must exist;
                // this table only has one. `resolve_table_target` must
                // reject it with a typed error, never index the table's
                // rows/cells out of bounds.
                Command::MergeCells {
                    table_path: BlockPath::top(0),
                    from_row: 5,
                    from_col: 5,
                    to_row: 9,
                    to_col: 9,
                },
            ],
            Scenario::SelectionClampOutOfRange => vec![
                Command::InsertText {
                    at: None,
                    text: "hello".to_string(),
                },
                // Wildly out-of-range offsets AND a path (`top(5)`)
                // naming a block that doesn't exist — `SetSelection`
                // must clamp both ends through `clamp_pos`, the same as
                // every other selection-mutating path.
                Command::SetSelection {
                    range: LogicalRange {
                        start: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 999_999,
                        },
                        end: LogicalPos {
                            path: BlockPath::top(5),
                            offset: 999_999,
                        },
                    },
                    caret: LogicalPos {
                        path: BlockPath::top(0),
                        offset: 999_999,
                    },
                },
            ],
            Scenario::SelectionClampAfterUndo => vec![
                Command::InsertText {
                    at: None,
                    text: "hello world".to_string(),
                },
                // A VALID selection at the end of the current document...
                Command::SetSelection {
                    range: LogicalRange {
                        start: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 11,
                        },
                        end: LogicalPos {
                            path: BlockPath::top(0),
                            offset: 11,
                        },
                    },
                    caret: LogicalPos {
                        path: BlockPath::top(0),
                        offset: 11,
                    },
                },
                // ...that `Undo` then invalidates by reverting the very
                // text it addressed — `repro_117_undo_repaint_error`'s
                // shape (a stale selection surviving a tree swap).
                Command::Undo,
            ],
        }
    }

    /// Peek (never consume on a miss) whether `u`'s next two bytes select a
    /// scenario. [`gen_command_sequence`] checks this FIRST every
    /// iteration, before its generic percentage-bucket dispatch runs.
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
    /// - byte 0 selects `gen_seed_text`'s `POOL[0]` (an empty string —
    ///   every scenario either doesn't need seed text or supplies its own
    ///   via an explicit `Command::InsertText`, so the seed document's
    ///   initial text is irrelevant here);
    /// - bytes 1–2 are `[SCENARIO_SENTINEL, self.index()]`, checked by
    ///   `gen_command_sequence` before ANY generic arm runs (issue #261 —
    ///   previously this went through the bucket dispatch first, which
    ///   needed a 3rd prefix byte; checking it first removes even that
    ///   dependency).
    ///
    /// Exactly 3 bytes: `gen_command_sequence`'s `u.is_empty()` check then
    /// stops the sequence right after this scenario's commands are
    /// spliced in.
    pub fn seed_bytes(self) -> Vec<u8> {
        vec![0x00, SCENARIO_SENTINEL, self.index()]
    }
}

/// One curated, small-bounded command spanning the issue's named
/// categories: insert / delete / format / table / section / story.
///
/// Issue #229/#261 — [`gen_command_sequence`] checks [`Scenario::peek`]
/// before EVERY arm (this one included) gets a chance to run, so no
/// scenario fast path lives here any more (see `Scenario`'s doc comment).
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
        // Issue #64 — `at` is optional now; the curated arm keeps its
        // explicit position (and its exact byte consumption, so committed
        // corpus seeds still decode to the same scenarios). `None` — split
        // at the live caret — is reached via the blind `arbitrary()` half.
        4 => Command::SplitParagraph { at: Some(pos(u)) },
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
/// - A [`Scenario`] tag (issue #229/#261), checked FIRST every iteration —
///   `[SCENARIO_SENTINEL, index]` bypasses every percentage bucket below
///   entirely and splices in that scenario's fixed command list. See
///   `Scenario`'s doc comment for why this has to run before anything
///   else gets a chance to consume a byte.
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
        if let Some(scenario) = Scenario::peek(u) {
            // Issue #229/#261 — consume exactly the two bytes `peek`
            // looked at; `build` itself reads no further bytes, by
            // design, so no other arm's byte-consumption change (nor a
            // multi-command scenario's own length) can ever perturb this
            // again.
            let _ = u.bytes(2);
            for cmd in scenario.build() {
                record_coverage(&cmd);
                out.push(cmd);
                if out.len() >= max_len {
                    return out;
                }
            }
            if u.is_empty() {
                break;
            }
            continue;
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
    ExtendSelectionToPoint { .. } => false,
    GetImageRects => false,
    SelectWordAt { .. } => false,
    SelectParagraphAt { .. } => false,
    SelectCellAt { .. } => false,
    DeleteAtCaret { .. } => true, // gen_targeted_command
    RequestAccessibilityDelta => false,
    GetSelectionAsClipboard { .. } => false,
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
    AcceptAllRevisions => false,
    RejectAllRevisions => false,
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
    use bridge::Event;

    /// Issue #261 — apply a [`Scenario`]'s seed bytes through the REAL
    /// `gen_seed_text` / `gen_command_sequence` decode path (proving the
    /// committed corpus file, once regenerated, still decodes to this
    /// exact scenario) and then through a real headless
    /// `engine_wasm::Engine` (proving the ENGINE survives it, not just the
    /// generator) — a decode alone isn't enough for the #115/#116/#117
    /// regressions, which are all about what the engine does when it
    /// actually receives the sequence. Returns every event, in order, so
    /// a test can inspect any command's result — usually the last one.
    fn apply_scenario(scenario: Scenario) -> (engine_wasm::Engine, Vec<Event>) {
        let bytes = scenario.seed_bytes();
        let mut u = Unstructured::new(&bytes);
        let seed_text = gen_seed_text(&mut u);
        let cmds = gen_command_sequence(&mut u, 64);
        let mut engine =
            engine_wasm::Engine::new_headless(engine::DocumentTree::from_text(&seed_text));
        let mut events = Vec::with_capacity(cmds.len());
        for cmd in cmds {
            events.push(engine.apply_sync(cmd));
        }
        (engine, events)
    }

    /// Issue #115/#261 (`repro_115_1`) — `DeleteRange` whose ends land
    /// inside a multi-byte scalar used to panic
    /// (`assertion failed: self.is_char_boundary(idx)`). The fix floors
    /// both ends to the nearest char boundary at or before the wire
    /// offset (offsets 1 and 5 both floor into the boundary set
    /// {0, 2, 4, 6} of "ببب") instead of panicking or rejecting, so the
    /// command must SUCCEED with the floored range, never error.
    #[test]
    fn text_range_char_boundary_delete_range_snaps_instead_of_panicking() {
        let (engine, events) = apply_scenario(Scenario::TextRangeCharBoundaryDeleteRange);
        assert!(engine.selection_is_valid());
        assert!(
            matches!(
                events.last(),
                Some(Event::SelectionChanged { range, .. })
                    if range.start.offset == 0 && range.end.offset == 0
            ),
            "DeleteRange across a mid-scalar range must snap-and-succeed \
             at the floored boundary (0), not error or panic: {:?}",
            events.last()
        );
    }

    /// Issue #115/#261 (`repro_115_2`) — `ReplaceRange` landing inside a
    /// combining mark (boundary set {0, 1, 3, 5, 7}; offsets 2 and 6 both
    /// floor to 1 and 5).
    #[test]
    fn text_range_char_boundary_replace_range_snaps_instead_of_panicking() {
        let (engine, events) = apply_scenario(Scenario::TextRangeCharBoundaryReplaceRange);
        assert!(engine.selection_is_valid());
        assert!(
            matches!(
                events.last(),
                Some(Event::SelectionChanged { range, .. })
                    if range.start.offset == 2 && range.end.offset == 2
            ),
            "ReplaceRange across a mid-mark range must snap-and-succeed, \
             caret landing right after the inserted text: {:?}",
            events.last()
        );
    }

    /// Issue #115/#261 (`repro_115_3`) — `SplitParagraph` at an explicit
    /// `at` landing inside an emoji scalar (boundary set {0, 4, 8};
    /// offset 2 floors to 0).
    #[test]
    fn text_range_char_boundary_split_paragraph_snaps_instead_of_panicking() {
        let (engine, events) = apply_scenario(Scenario::TextRangeCharBoundarySplitParagraph);
        assert!(engine.selection_is_valid());
        assert!(
            matches!(
                events.last(),
                Some(Event::SelectionChanged { range, .. })
                    if range.start.offset == 0 && range.end.offset == 0
            ),
            "SplitParagraph at a mid-scalar position must snap-and-succeed, \
             landing the caret at the start of the new paragraph: {:?}",
            events.last()
        );
    }

    /// Issue #115/#261 (`repro_115_4`) — `ApplyFormatting { range:
    /// Some(_) }` whose ends land inside a multi-byte scalar (boundary set
    /// {0, 2, 4, 6, 8}; offsets 1 and 7 floor to 0 and 6). With no active
    /// selection this returns `FormattingChanged` (not `SelectionChanged`
    /// — see `apply_formatting`'s Phase-1 harness branch), carrying the
    /// FLOORED range back.
    #[test]
    fn text_range_char_boundary_apply_formatting_snaps_instead_of_panicking() {
        let (engine, events) = apply_scenario(Scenario::TextRangeCharBoundaryApplyFormatting);
        assert!(engine.selection_is_valid());
        assert!(
            matches!(
                events.last(),
                Some(Event::FormattingChanged { range, .. })
                    if range.start.offset == 0 && range.end.offset == 6
            ),
            "ApplyFormatting across a mid-scalar range must snap-and-succeed \
             at the floored range (0, 6), not error or panic: {:?}",
            events.last()
        );
    }

    /// Issue #115/#261 (`repro_115_5`) — the INTERACTIVE `InsertText {
    /// at: Some(_) }` path, which validates through `clamp_pos` rather
    /// than `resolve_edit_pos` (a distinct code path from the explicit-
    /// range commands above). Offset 3 floors into "بببب"'s boundary set
    /// {0, 2, 4, 6, 8} to 2; the inserted "X" then lands the caret at 3.
    #[test]
    fn text_range_char_boundary_insert_text_snaps_instead_of_panicking() {
        let (engine, events) = apply_scenario(Scenario::TextRangeCharBoundaryInsertText);
        assert!(engine.selection_is_valid());
        assert!(
            matches!(
                events.last(),
                Some(Event::SelectionChanged { range, .. })
                    if range.start.offset == 3 && range.end.offset == 3
            ),
            "interactive InsertText at a mid-scalar position must snap via \
             clamp_pos and succeed, not error or panic: {:?}",
            events.last()
        );
    }

    /// Issue #115/#261 (`repro_115_composition_overlay`) — `BeginComposition`
    /// stores its `at` VERBATIM (issue #64), so a mid-scalar composition
    /// anchor only gets caught at commit time, through
    /// `do_insert_text_interactive`'s `clamp_pos` — previously the IME
    /// preview cut Arabic mid-scalar here. Same expected landing offset
    /// (3) as the plain interactive-insert scenario above, reached via
    /// `EndComposition { commit: true }` instead.
    #[test]
    fn text_range_char_boundary_composition_commit_snaps_instead_of_panicking() {
        let (engine, events) =
            apply_scenario(Scenario::TextRangeCharBoundaryCompositionCommit);
        assert!(engine.selection_is_valid());
        assert!(
            matches!(
                events.last(),
                Some(Event::SelectionChanged { range, .. })
                    if range.start.offset == 3 && range.end.offset == 3
            ),
            "committing a composition anchored mid-scalar must snap via \
             clamp_pos and succeed, not error or panic: {:?}",
            events.last()
        );
    }

    /// Issue #116/#261 (`repro_116_1`) — `MergeCells` addressing a
    /// row/column rectangle past a 1×1 table's actual shape used to index
    /// an `im::Vector` out of bounds. `resolve_table_target` must reject
    /// it with a typed `Event::Error`, never panic.
    #[test]
    fn table_merge_cells_out_of_range_errors_instead_of_panicking() {
        let (engine, events) = apply_scenario(Scenario::TableMergeCellsOutOfRange);
        assert!(engine.selection_is_valid());
        assert!(
            matches!(
                events.last(),
                Some(Event::Error { message }) if message.contains("MergeCells")
            ),
            "MergeCells past the table's shape must return a typed Error \
             naming the command, not panic: {:?}",
            events.last()
        );
    }

    /// Issue #117/#261 (`repro_117_1`) — `SetSelection` with a wildly
    /// out-of-range range/caret, including a path (`top(5)`) naming a
    /// block that doesn't exist in a one-paragraph document. Every field
    /// must clamp through `clamp_pos` — same as every other selection
    /// path — landing the whole selection at the document end (offset 5,
    /// `"hello".len()`).
    #[test]
    fn selection_clamp_out_of_range_snaps_instead_of_leaving_garbage() {
        let (engine, events) = apply_scenario(Scenario::SelectionClampOutOfRange);
        assert!(
            engine.selection_is_valid(),
            "issue #117 — SetSelection must never leave an out-of-bounds \
             selection, even momentarily"
        );
        assert!(
            matches!(
                events.last(),
                Some(Event::SelectionChanged { range, .. })
                    if range.start.offset == 5 && range.end.offset == 5
            ),
            "SetSelection with wildly out-of-range fields must clamp to \
             the document end, not error or panic: {:?}",
            events.last()
        );
    }

    /// Issue #117/#261 (`repro_117_undo_repaint_error`) — a VALID
    /// selection at the end of the document, then `Undo` reverts the very
    /// edit that selection addressed. `do_undo` must re-clamp the
    /// selection into the RESTORED (now-empty) document before the
    /// repaint runs, landing at offset 0 — not leave a stale selection
    /// dangling for a later command to "self-heal".
    #[test]
    fn selection_reclamps_after_undo_reverts_its_own_edit() {
        let (engine, events) = apply_scenario(Scenario::SelectionClampAfterUndo);
        assert!(
            engine.selection_is_valid(),
            "issue #117 — Undo must re-clamp the selection into the \
             restored document before repainting, not leave it stale"
        );
        assert!(
            matches!(
                events.last(),
                Some(Event::SelectionChanged { range, .. })
                    if range.start.offset == 0 && range.end.offset == 0
            ),
            "Undo reverting the edit a selection addressed must re-clamp \
             that selection into the restored (now-empty) paragraph: {:?}",
            events.last()
        );
    }

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
        const EXPECTED_VARIANT_COUNT: usize = 106;
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
