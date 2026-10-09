//! Issue #342 — `CommandMeta`: the single source of truth for every
//! [`Command`] variant's dispatch metadata.
//!
//! Before this module the same facts lived in three hand-maintained lists
//! that had already drifted apart (issue #260): the worker's
//! `shouldLogCommand` switch, `EngineClient`'s `READ_ONLY_COMMANDS` set (which
//! listed `GET_IMAGE_RECTS` while the worker logged it) and the engine's
//! `story_gate` match. Nothing at all recorded which variants are real,
//! partial, or `phase3_stub`s.
//!
//! [`command_meta!`] below is a **single list, no wildcard**: it generates
//! [`CommandKind`] (a fieldless mirror of `Command`), the exhaustive
//! `Command::kind()` match, and [`COMMAND_META`] from the same tokens, so a
//! new `Command` variant fails to compile until it is classified here, and
//! a classification can never name a variant that does not exist. The
//! pattern is `fuzz/src/command_gen.rs`'s `classify_variants!` (issue #209).
//!
//! Consumers:
//! - the engine's `story_gate` reads [`CommandMeta::story`];
//! - the TS worker (`shouldLogCommand`, the #268 pinning set) and
//!   `EngineClient.writesInFlight` read the generated
//!   `packages/core/src/commandMeta.generated.ts` ([`render_ts_module`];
//!   `cargo test -p bridge` fails when the committed file is stale —
//!   regenerate with `NGE_UPDATE_COMMAND_META=1 cargo test -p bridge`);
//! - `tools/parity` joins it with the `@nge/core` facade map, the UI's
//!   `data-nge-command` / `data-nge-pending-issue` attributes, the e2e
//!   suite and the fuzz generator into the parity matrix (issue #342).

use crate::Command;

/// The sentinel issue number of a [`CommandStatus::Stub`] / `Partial` gap
/// that has no GitHub issue yet. `tools/parity` reports every such row as
/// UNFILED, and its floor test caps how many may exist (the cap may only
/// shrink) — file the issue, then replace the sentinel with its number.
pub const UNFILED: u32 = 0;

/// How far a command's engine behaviour is real.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    /// Every input the wire type admits is handled (a rejected input is an
    /// honest, specific `Event::Error`, never a placeholder).
    Implemented,
    /// Real for part of its input space; the rest is tracked by `issue`
    /// and the UI that exposes the gap carries an "Engine pending" badge
    /// (`data-nge-pending-issue`).
    Partial { issue: u32 },
    /// Answers a placeholder error (`phase3_stub`) for every input. Never
    /// exposed on the `@nge/core` facade, never counted as live.
    Stub { issue: u32 },
}

impl CommandStatus {
    /// The tracking issue of a gap; `None` for [`CommandStatus::Implemented`].
    pub const fn issue(self) -> Option<u32> {
        match self {
            CommandStatus::Implemented => None,
            CommandStatus::Partial { issue } | CommandStatus::Stub { issue } => Some(issue),
        }
    }

    /// Whether the command does real work for at least some inputs.
    pub const fn is_live(self) -> bool {
        !matches!(self, CommandStatus::Stub { .. })
    }
}

/// What the engine does with a command while a header/footer, note or
/// text-box story is active (Phase 3 #39 — `Engine::story_gate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryPolicy {
    /// Handled normally: the handler is story-aware (routes through the
    /// story adapters) or the command is global (paint, view, I/O, …).
    Allowed,
    /// Rejected with an `Event::Error` while any story is active — the UI
    /// disables the control in story mode; the gate is the backstop.
    BodyOnly,
    /// Allowed only while the active story is a text box (picture edits
    /// carry an explicit body-rooted address, issue #206); rejected in
    /// header/footer and note stories.
    TextBoxOnly,
    /// Tears the story's ground away (a document load / close): the
    /// engine exits to the body first, then handles the command.
    ExitsStory,
}

/// One `Command` variant's dispatch metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandMeta {
    /// May change the document model (text, structure, formatting,
    /// styles, sections, field results, review state).
    pub mutates_doc: bool,
    /// May move the engine-owned selection / caret / composition or the
    /// active story.
    pub moves_selection: bool,
    /// Belongs in the worker's durable event log: recovery replays the
    /// tail through `apply`, so anything a later logged command depends
    /// on must be kept. Pure read-backs are skipped (`PING` is the one
    /// read-only command that IS logged — the D2.6 exit gate drives the
    /// log with it).
    pub logged: bool,
    /// Can never change the document, the selection or any engine state
    /// a later command observes (pure read-backs and probes). Everything
    /// else counts toward `EngineClient.writesInFlight` (issue #57).
    pub read_only: bool,
    /// Starts a new document: the snapshot taken after it becomes the
    /// document's pinned event-log base (issue #268).
    pub new_document: bool,
    /// Story-mode behaviour (`Engine::story_gate`).
    pub story: StoryPolicy,
    /// How real the engine behaviour is.
    pub status: CommandStatus,
}

impl CommandMeta {
    /// Whether the command is handled normally while a story is active.
    pub const fn story_safe(&self) -> bool {
        matches!(self.story, StoryPolicy::Allowed)
    }

    /// Pure read-back: no state change, not logged.
    const QUERY: CommandMeta = CommandMeta {
        mutates_doc: false,
        moves_selection: false,
        logged: false,
        read_only: true,
        new_document: false,
        story: StoryPolicy::Allowed,
        status: CommandStatus::Implemented,
    };

    /// View / session state (fonts, zoom, viewport, render clock):
    /// neither the document nor the selection, but replay needs it.
    const VIEW: CommandMeta = CommandMeta {
        read_only: false,
        logged: true,
        ..Self::QUERY
    };

    /// Selection / caret / composition / active-story moves.
    const SELECT: CommandMeta = CommandMeta {
        moves_selection: true,
        ..Self::VIEW
    };

    /// Property edits: change the document, leave the selection alone.
    const FORMAT: CommandMeta = CommandMeta {
        mutates_doc: true,
        ..Self::VIEW
    };

    /// Content edits: change the document and move the caret.
    const EDIT: CommandMeta = CommandMeta {
        mutates_doc: true,
        moves_selection: true,
        ..Self::VIEW
    };

    const fn body_only(self) -> Self {
        CommandMeta {
            story: StoryPolicy::BodyOnly,
            ..self
        }
    }

    const fn text_box_only(self) -> Self {
        CommandMeta {
            story: StoryPolicy::TextBoxOnly,
            ..self
        }
    }

    const fn exits_story(self) -> Self {
        CommandMeta {
            story: StoryPolicy::ExitsStory,
            ..self
        }
    }

    const fn new_document(self) -> Self {
        CommandMeta {
            new_document: true,
            ..self
        }
    }

    const fn logged(self) -> Self {
        CommandMeta {
            logged: true,
            ..self
        }
    }

    const fn unlogged(self) -> Self {
        CommandMeta {
            logged: false,
            ..self
        }
    }

    const fn partial(self, issue: u32) -> Self {
        CommandMeta {
            status: CommandStatus::Partial { issue },
            ..self
        }
    }

    const fn stub(self, issue: u32) -> Self {
        CommandMeta {
            status: CommandStatus::Stub { issue },
            ..self
        }
    }
}

/// Defines [`CommandKind`], `Command::kind()` and [`COMMAND_META`] from one
/// list. Each entry names a variant exactly once (`$variant`), with the
/// same `{ .. }` pattern shape the `Command` match needs, and a `const`
/// [`CommandMeta`] expression built from the presets above.
macro_rules! command_meta {
    ( $( $variant:ident $( { $($field:tt)* } )? => $meta:expr ),+ $(,)? ) => {
        /// A fieldless mirror of [`Command`] — one value per variant, in
        /// declaration order (`CommandKind::X as usize` indexes
        /// [`COMMAND_META`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum CommandKind {
            $( $variant ),+
        }

        impl CommandKind {
            /// Every kind, in `Command` declaration order.
            pub const ALL: &'static [CommandKind] = &[ $( CommandKind::$variant ),+ ];

            /// The Rust variant identifier, e.g. `"InsertText"`.
            pub const fn name(self) -> &'static str {
                match self {
                    $( CommandKind::$variant => stringify!($variant) ),+
                }
            }
        }

        impl Command {
            /// This command's [`CommandKind`]. **No wildcard arm** — a new
            /// variant does not compile until it is classified in
            /// `crates/bridge/src/meta.rs`.
            pub const fn kind(&self) -> CommandKind {
                match self {
                    $( Command::$variant $( { $($field)* } )? => CommandKind::$variant ),+
                }
            }
        }

        /// Every variant's metadata, indexed by `CommandKind as usize`.
        pub const COMMAND_META: &[CommandMeta] = &[ $( $meta ),+ ];
    };
}

use CommandMeta as M;

command_meta! {
    // ---- Phase 1 PoC -------------------------------------------------------
    Ping => M::QUERY.logged(), // D2.6: the event-log exit gate replays PING
    LoadFont { .. } => M::VIEW,
    RasterizeGlyph { .. } => M::VIEW.body_only(),
    ShapeAndRasterize { .. } => M::VIEW.body_only(),
    RenderPage { .. } => M::EDIT.new_document().body_only(),
    InsertText { .. } => M::EDIT,
    Undo => M::EDIT,
    Redo => M::EDIT,
    LoadDocx { .. } => M::EDIT.new_document().exits_story(),
    SaveDocx => M::QUERY,
    // ---- Phase 2 §4 ----------------------------------------------------------
    // Lifecycle slots the worker protocol owns (INIT message, worker
    // termination); no client dispatches them.
    Init { .. } => M::VIEW.body_only().stub(UNFILED),
    // Issue #85 — recovery primitives are never part of the history they
    // persist / restore.
    Recover { .. } => M::EDIT.unlogged(),
    Snapshot { .. } => M::QUERY,
    Dispose => M::VIEW.body_only().stub(UNFILED),
    Tick { .. } => M::VIEW.body_only().stub(UNFILED),
    OpenDocument { .. } => M::EDIT.new_document().exits_story().partial(339),
    SaveDocument { .. } => M::QUERY,
    ExportPdf { .. } => M::QUERY,
    CloseDocument => M::EDIT.new_document().body_only().stub(338),
    DeleteRange { .. } => M::EDIT.body_only(),
    ReplaceRange { .. } => M::EDIT.body_only(),
    ApplyFormatting { .. } => M::FORMAT,
    ToggleFormatting { .. } => M::FORMAT,
    SplitParagraph { .. } => M::EDIT,
    // Stable paragraph ids do not exist yet; Backspace/Delete across a
    // boundary (`DeleteAtCaret`) is the interactive merge.
    MergeParagraph { .. } => M::EDIT.body_only().stub(UNFILED),
    InsertImage { .. } => M::EDIT.body_only(),
    ResizeImage { .. } => M::FORMAT.text_box_only(),
    MoveImage { .. } => M::FORMAT.text_box_only(),
    // Issue #137 — an inline picture has no wrap mode until inline →
    // floating conversion ships (ImageWrapPicker's "Engine pending" badge).
    SetImageWrap { .. } => M::FORMAT.text_box_only().partial(137),
    SetSelection { .. } => M::SELECT,
    ExtendSelection { .. } => M::SELECT,
    SelectAll => M::SELECT,
    MoveCaret { .. } => M::SELECT,
    BeginComposition { .. } => M::SELECT,
    UpdateComposition { .. } => M::SELECT,
    EndComposition { .. } => M::EDIT,
    SetViewport { .. } => M::VIEW,
    SetZoom { .. } => M::VIEW,
    SetDeviceScale { .. } => M::VIEW,
    RequestPaint { .. } => M::QUERY,
    ExpandLayout { .. } => M::VIEW,
    // Issue #376 — fonts' bytes are leaked for a `'static` face; unloading
    // needs owned font storage first.
    UnloadFont { .. } => M::VIEW.body_only().stub(376),
    RequestStats => M::QUERY,
    // ---- Phase 4 §7 ------------------------------------------------------------
    HitTest { .. } => M::QUERY,
    HitTestInPage { .. } => M::QUERY,
    PlaceCaretAtPoint { .. } => M::SELECT,
    ExtendSelectionToPoint { .. } => M::SELECT,
    // Issue #342 — was read-only in EngineClient but logged by the worker.
    GetImageRects => M::QUERY,
    SelectWordAt { .. } => M::SELECT,
    SelectParagraphAt { .. } => M::SELECT,
    SelectCellAt { .. } => M::SELECT,
    DeleteAtCaret { .. } => M::EDIT,
    RequestAccessibilityDelta => M::QUERY,
    GetSelectionAsClipboard { .. } => M::QUERY,
    PastePlain { .. } => M::EDIT,
    // ---- Backlog sprint 1 ------------------------------------------------------
    SetParagraphAlign { .. } => M::FORMAT,
    SetParagraphDirection { .. } => M::FORMAT,
    // ---- Backlog sprint 7 ------------------------------------------------------
    PasteHtml { .. } => M::EDIT.body_only(),
    // ---- Phase 5 PR 3 — tables ------------------------------------------------
    InsertTable { .. } => M::EDIT,
    DeleteTable { .. } => M::EDIT,
    InsertRow { .. } => M::EDIT,
    DeleteRow { .. } => M::EDIT,
    InsertColumn { .. } => M::EDIT,
    DeleteColumn { .. } => M::EDIT,
    MergeCells { .. } => M::EDIT,
    SplitCell { .. } => M::EDIT,
    SetCellShading { .. } => M::FORMAT,
    SetCellBorders { .. } => M::FORMAT,
    SetTableProperties { .. } => M::FORMAT,
    SetColumns { .. } => M::FORMAT.body_only(),
    InsertPageBreak { .. } => M::EDIT.body_only(),
    InsertSectionBreak { .. } => M::EDIT.body_only(),
    EnterHeaderFooter { .. } => M::SELECT,
    ExitHeaderFooter => M::SELECT,
    SetHeaderFooterLink { .. } => M::FORMAT,
    SetTitlePage { .. } => M::FORMAT,
    SetEvenOddHeaders { .. } => M::FORMAT,
    InsertField { .. } => M::EDIT,
    InsertFootnote { .. } => M::EDIT.body_only(),
    InsertEndnote { .. } => M::EDIT.body_only(),
    InsertTextBox { .. } => M::EDIT.body_only(),
    SetRenderDate { .. } => M::VIEW,
    UpdateFields => M::FORMAT,
    SetFieldCodeView { .. } => M::SELECT,
    SetFieldInstruction { .. } => M::EDIT,
    InsertToc { .. } => M::EDIT.body_only(),
    SetParagraphBorders { .. } => M::FORMAT,
    SetPageMargins { .. } => M::FORMAT.body_only(),
    SetPageOrientation { .. } => M::FORMAT.body_only(),
    ToggleList { .. } => M::FORMAT,
    ChangeListLevel { .. } => M::FORMAT,
    SetParagraphIndent { .. } => M::FORMAT,
    SetLineSpacing { .. } => M::FORMAT,
    SetParagraphShading { .. } => M::FORMAT,
    ToggleTrackChanges { .. } => M::FORMAT.body_only(),
    AcceptRevision { .. } => M::EDIT.body_only(),
    RejectRevision { .. } => M::EDIT.body_only(),
    AcceptAllRevisions => M::EDIT.body_only(),
    RejectAllRevisions => M::EDIT.body_only(),
    InsertComment { .. } => M::FORMAT.body_only(),
    DeleteComment { .. } => M::FORMAT.body_only(),
    SetTabStops { .. } => M::FORMAT,
    SetReviewIdentity { .. } => M::VIEW.body_only(),
    ApplyStyle { .. } => M::FORMAT,
    ResolveComment { .. } => M::FORMAT.body_only(),
    ReplyToComment { .. } => M::FORMAT.body_only(),
    ModifyStyle { .. } => M::FORMAT,
}

impl CommandKind {
    /// This kind's metadata.
    pub const fn meta(self) -> &'static CommandMeta {
        &COMMAND_META[self as usize]
    }

    /// The serde wire tag (`#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`
    /// on `Command`), e.g. `"INSERT_TEXT"` — the TS `Command['type']`.
    pub fn wire_name(self) -> String {
        screaming_snake(self.name())
    }

    /// Look a kind up by its wire tag.
    pub fn from_wire_name(wire: &str) -> Option<CommandKind> {
        CommandKind::ALL
            .iter()
            .copied()
            .find(|k| k.wire_name() == wire)
    }
}

impl Command {
    /// This command's [`CommandMeta`].
    pub const fn meta(&self) -> &'static CommandMeta {
        self.kind().meta()
    }
}

/// serde's `SCREAMING_SNAKE_CASE` for a `PascalCase` variant identifier:
/// an underscore before every interior uppercase letter, then uppercase.
fn screaming_snake(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 8);
    for (i, ch) in ident.char_indices() {
        if i > 0 && ch.is_uppercase() {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

/// Repository path (from the workspace root) of the generated TS module.
pub const TS_MODULE_PATH: &str = "packages/core/src/commandMeta.generated.ts";

fn ts_status(status: CommandStatus) -> String {
    match status {
        CommandStatus::Implemented => "{ kind: 'implemented' }".to_string(),
        CommandStatus::Partial { issue } => format!("{{ kind: 'partial', issue: {issue} }}"),
        CommandStatus::Stub { issue } => format!("{{ kind: 'stub', issue: {issue} }}"),
    }
}

fn ts_story(story: StoryPolicy) -> &'static str {
    match story {
        StoryPolicy::Allowed => "allowed",
        StoryPolicy::BodyOnly => "body_only",
        StoryPolicy::TextBoxOnly => "text_box_only",
        StoryPolicy::ExitsStory => "exits_story",
    }
}

fn ts_union(kinds: impl Iterator<Item = CommandKind>) -> String {
    let names: Vec<String> = kinds.map(|k| format!("'{}'", k.wire_name())).collect();
    if names.is_empty() {
        "never".to_string()
    } else {
        names.join(" | ")
    }
}

/// Render `packages/core/src/commandMeta.generated.ts` — the TS view of
/// [`COMMAND_META`] the worker, `EngineClient` and the `@nge/core` facade
/// map consume. A plain module with a type-only import, so the worker can
/// import it without loading the wasm package or Solid.
pub fn render_ts_module() -> String {
    let mut out = String::new();
    out.push_str(
        "/* GENERATED by crates/bridge/src/meta.rs (`render_ts_module`) — DO NOT EDIT.\n\
         \x20* Regenerate: NGE_UPDATE_COMMAND_META=1 cargo test -p bridge\n\
         \x20* `cargo test -p bridge` fails while this file is stale (issue #342). */\n\
         import type { Command } from './types';\n\
         \n\
         export type CommandType = Command['type'];\n\
         \n\
         /** How far a command's engine behaviour is real (`bridge::CommandStatus`). */\n\
         export type CommandStatus =\n\
         \x20   | { readonly kind: 'implemented' }\n\
         \x20   | { readonly kind: 'partial'; readonly issue: number }\n\
         \x20   | { readonly kind: 'stub'; readonly issue: number };\n\
         \n\
         /** Story-mode behaviour (`bridge::StoryPolicy`, `Engine::story_gate`). */\n\
         export type StoryPolicy = 'allowed' | 'body_only' | 'text_box_only' | 'exits_story';\n\
         \n\
         /** One command's dispatch metadata (`bridge::CommandMeta`). */\n\
         export interface CommandMeta {\n\
         \x20   /** The Rust `Command` variant identifier. */\n\
         \x20   readonly variant: string;\n\
         \x20   readonly mutates_doc: boolean;\n\
         \x20   readonly moves_selection: boolean;\n\
         \x20   /** Kept in the worker's durable event log (replayed on recovery). */\n\
         \x20   readonly logged: boolean;\n\
         \x20   /** Never changes engine state; excluded from `writesInFlight`. */\n\
         \x20   readonly read_only: boolean;\n\
         \x20   /** The next snapshot is the new document's pinned base (#268). */\n\
         \x20   readonly new_document: boolean;\n\
         \x20   readonly story: StoryPolicy;\n\
         \x20   readonly status: CommandStatus;\n\
         }\n\n",
    );
    let stubs = CommandKind::ALL
        .iter()
        .copied()
        .filter(|k| matches!(k.meta().status, CommandStatus::Stub { .. }));
    let partials = CommandKind::ALL
        .iter()
        .copied()
        .filter(|k| matches!(k.meta().status, CommandStatus::Partial { .. }));
    out.push_str(&format!(
        "/** Commands that answer a placeholder error for every input. The\n\
         \x20*  `@nge/core` facade map types these as `'stub'` only, so no facade\n\
         \x20*  method can dispatch one (issue #338). */\n\
         export type StubCommandType = {};\n\n",
        ts_union(stubs)
    ));
    out.push_str(&format!(
        "/** Commands real for only part of their input space. */\n\
         export type PartialCommandType = {};\n\n",
        ts_union(partials)
    ));
    out.push_str("export const COMMAND_META: { readonly [K in CommandType]: CommandMeta } = {\n");
    for kind in CommandKind::ALL {
        let m = kind.meta();
        out.push_str(&format!(
            "    {}: {{ variant: '{}', mutates_doc: {}, moves_selection: {}, logged: {}, \
             read_only: {}, new_document: {}, story: '{}', status: {} }},\n",
            kind.wire_name(),
            kind.name(),
            m.mutates_doc,
            m.moves_selection,
            m.logged,
            m.read_only,
            m.new_document,
            ts_story(m.story),
            ts_status(m.status),
        ));
    }
    out.push_str("};\n\n");
    out.push_str(
        "/** A conservative stand-in for a type the table does not know (a\n\
         \x20*  newer/older wire message): logged, counted as a write. */\n\
         const UNKNOWN_COMMAND_META: CommandMeta = {\n\
         \x20   variant: 'Unknown',\n\
         \x20   mutates_doc: true,\n\
         \x20   moves_selection: true,\n\
         \x20   logged: true,\n\
         \x20   read_only: false,\n\
         \x20   new_document: false,\n\
         \x20   story: 'body_only',\n\
         \x20   status: { kind: 'implemented' },\n\
         };\n\
         \n\
         /** Metadata for a runtime command type — `UNKNOWN_COMMAND_META` when\n\
         \x20*  the string is not a known `Command['type']`. */\n\
         export function commandMeta(type: string): CommandMeta {\n\
         \x20   return Object.prototype.hasOwnProperty.call(COMMAND_META, type)\n\
         \x20       ? COMMAND_META[type as CommandType]\n\
         \x20       : UNKNOWN_COMMAND_META;\n\
         }\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Issue #342 — the same pin as `fuzz/src/command_gen.rs`'s
    /// `all_variant_names_equals_classifier_coverage`: `Command::kind()`'s
    /// match has no wildcard, so `CommandKind::ALL` is the full variant
    /// list; update this when `Command` gains or loses a variant (and
    /// classify the new variant above).
    const EXPECTED_VARIANT_COUNT: usize = 107;

    #[test]
    fn command_meta_is_exhaustive() {
        assert_eq!(
            CommandKind::ALL.len(),
            EXPECTED_VARIANT_COUNT,
            "CommandKind mirrors every Command variant (crates/bridge/src/command.rs)"
        );
        assert_eq!(COMMAND_META.len(), CommandKind::ALL.len());
        let mut seen = BTreeSet::new();
        for (i, kind) in CommandKind::ALL.iter().enumerate() {
            assert_eq!(*kind as usize, i, "ALL is in declaration order");
            assert!(
                seen.insert(kind.wire_name()),
                "duplicate wire name {kind:?}"
            );
            assert_eq!(CommandKind::from_wire_name(&kind.wire_name()), Some(*kind));
        }
    }

    /// The wire names match serde's own rename for every unit variant we
    /// can construct without payload (struct variants share the rule; the
    /// generated TS table is additionally checked against the tsify union
    /// by `tsc`, whose mapped type rejects a misspelt key).
    #[test]
    fn wire_names_match_serde() {
        for (cmd, kind) in [
            (Command::Ping, CommandKind::Ping),
            (Command::SaveDocx, CommandKind::SaveDocx),
            (Command::GetImageRects, CommandKind::GetImageRects),
            (
                Command::RequestAccessibilityDelta,
                CommandKind::RequestAccessibilityDelta,
            ),
            (Command::AcceptAllRevisions, CommandKind::AcceptAllRevisions),
            (Command::ExitHeaderFooter, CommandKind::ExitHeaderFooter),
        ] {
            assert_eq!(cmd.kind(), kind);
            let json = serde_json::to_value(&cmd).unwrap();
            assert_eq!(json["type"], kind.wire_name());
        }
        assert_eq!(CommandKind::HitTestInPage.wire_name(), "HIT_TEST_IN_PAGE");
        assert_eq!(CommandKind::ExportPdf.wire_name(), "EXPORT_PDF");
        assert_eq!(CommandKind::InsertToc.wire_name(), "INSERT_TOC");
    }

    /// Internal consistency of the flags.
    #[test]
    fn command_meta_flags_are_consistent() {
        for kind in CommandKind::ALL {
            let m = kind.meta();
            if m.read_only {
                assert!(
                    !m.mutates_doc && !m.moves_selection && !m.new_document,
                    "{kind:?}: read-only commands change nothing"
                );
            }
            /* Recovery replays the log tail: anything that changes state a
            later command depends on must be logged — except the recovery
            primitive itself (issue #85). */
            if (m.mutates_doc || m.moves_selection) && *kind != CommandKind::Recover {
                assert!(
                    m.logged,
                    "{kind:?}: a state-changing command must be logged"
                );
            }
            if m.logged && m.read_only {
                assert_eq!(
                    *kind,
                    CommandKind::Ping,
                    "only PING is logged while read-only"
                );
            }
            if m.new_document {
                assert!(m.mutates_doc && m.moves_selection && m.logged, "{kind:?}");
            }
        }
    }

    /// Floor (issue #342): every gap names its tracking issue, or the
    /// UNFILED sentinel — and the number of UNFILED gaps may only shrink.
    #[test]
    fn every_gap_cites_an_issue() {
        const MAX_UNFILED: usize = 4;
        let mut unfiled = Vec::new();
        for kind in CommandKind::ALL {
            if let Some(issue) = kind.meta().status.issue()
                && issue == UNFILED
            {
                unfiled.push(kind.name());
            }
        }
        assert!(
            unfiled.len() <= MAX_UNFILED,
            "new UNFILED gaps {unfiled:?} — file a GitHub issue and cite its number"
        );
    }

    /// The committed TS module is the rendering of this table. Set
    /// `NGE_UPDATE_COMMAND_META=1` to rewrite it.
    #[test]
    fn generated_ts_module_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(TS_MODULE_PATH);
        let want = render_ts_module();
        if std::env::var_os("NGE_UPDATE_COMMAND_META").is_some() {
            std::fs::write(&path, &want).expect("write the generated TS module");
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "{TS_MODULE_PATH} is stale — regenerate with \
             `NGE_UPDATE_COMMAND_META=1 cargo test -p bridge`"
        );
    }
}
