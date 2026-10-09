//! Issue #345 — the document-protection firewall.
//!
//! When the open document enforces `<w:documentProtection>`
//! (`DocumentTree::protection_mode`), every command's
//! [`bridge::ProtectionClass`] (`crates/bridge/src/meta.rs`, the single
//! source of truth) decides, before dispatch:
//!
//! - `Exempt` — queries, view / selection moves, document replacements
//!   (load / close / recovery), undo / redo: handled normally.
//! - a class the mode does not admit ([`bridge::ProtectionClass::
//!   admitted_by`]) — refused with `Event::Error { kind: Protected }`
//!   (plus an assertive `aria-live` announcement); nothing changes.
//! - admitted classes are refined:
//!   - `trackedChanges` forces review mode on: `ToggleTrackChanges
//!     { enabled: false }` is refused, and only text edits the engine
//!     records as revisions pass (typing, deletion, Enter, plain and rich
//!     paste and IME commits — issue #366 — and run formatting);
//!   - `forms` admits a text edit only inside form-field content
//!     (`DocumentTree::form_region_for_edit`): a block- or run-level
//!     content control, a legacy text form field, an unprotected
//!     section. A text-form-field edit is PERFORMED here
//!     (`fill_form_text_field` keeps the field around what is typed —
//!     the generic path would type outside the caret-atomic field), and
//!     so is a replacement that starts on a run-level control's opener
//!     (inserted before the old text is removed, so it stays inside).
//!   - while a header / footer / note / text-box story is active every
//!     non-exempt command is refused (story content is never form
//!     content, and story edits are not recorded as revisions).
//!
//! The gate is the engine-side truth; the shell badges the mode
//! (`SelectionChanged.protection`) and toasts the refusal.

use super::*;
use bridge::{ProtectionClass, ProtectionMode};
use engine::{FormEdit, FormRegion};

/// What the forms check needs to know about a text command.
struct TextTarget {
    start: BridgeLogicalPos,
    end: BridgeLogicalPos,
    edit: FormEdit,
    /// The text inserted at `start` (`""` for a pure deletion).
    text: String,
    /// For a collapsed-caret deletion: the range BEFORE the atomic-field
    /// widening (`delete_target_raw`) — a text form field's result is
    /// edited a character at a time, never deleted whole.
    raw_delete: Option<(BridgeLogicalPos, BridgeLogicalPos)>,
}

/// What a text command amounts to before dispatch.
enum TextIntent {
    /// An edit of this shape.
    Edit(TextTarget),
    /// Changes nothing (no composition to commit, nothing to delete at a
    /// document edge, an unresolvable position the handler will refuse
    /// on its own): let the handler answer.
    Inert,
    /// A shape the forms check cannot vouch for (the Phase-1 append path
    /// with no selection at all).
    Unknown,
}

fn protection_label(mode: ProtectionMode) -> &'static str {
    match mode {
        ProtectionMode::ReadOnly => "read-only",
        ProtectionMode::Comments => "comments only",
        ProtectionMode::TrackedChanges => "tracked changes only",
        ProtectionMode::Forms => "filling in forms",
    }
}

impl Engine {
    /// The refusal: a typed error (and an assertive announcement for
    /// screen readers). `why` completes "This document is protected
    /// (<mode>): …".
    fn protection_refusal(&mut self, mode: ProtectionMode, cmd: &Command, why: &str) -> Event {
        let message = format!(
            "This document is protected ({}): {why}",
            protection_label(mode)
        );
        self.announce(AnnouncementPriority::Assertive, message.clone());
        /* A refused IME commit must not leave its preview painted. */
        if matches!(cmd, Command::EndComposition { .. }) && self.composition.take().is_some() {
            self.invalidate_layout_snapshot();
            let _ = self.render_document(None);
        }
        Event::Error {
            message: format!("{}: {message}", cmd.kind().name()),
            kind: Some(bridge::ErrorKind::Protected),
        }
    }

    /// The firewall (see the module docs). `Some(event)` = handled here
    /// (refused, or a form-field edit performed); `None` = dispatch
    /// normally.
    pub(crate) fn protection_gate(&mut self, cmd: &Command) -> Option<Event> {
        let class = cmd.meta().protection;
        if class == ProtectionClass::Exempt {
            return None;
        }
        let mode = bridge_protection_mode(self.protection_mode()?);
        if !class.admitted_by(mode) {
            let why = match mode {
                ProtectionMode::ReadOnly => "it can be read but not edited.",
                ProtectionMode::Comments => "only comments can be added.",
                ProtectionMode::TrackedChanges => {
                    "this change cannot be recorded as a tracked change."
                }
                ProtectionMode::Forms => "only form fields and content controls can be edited.",
            };
            return Some(self.protection_refusal(mode, cmd, why));
        }
        if self.story_active() {
            return Some(self.protection_refusal(
                mode,
                cmd,
                "headers, footers, notes and text boxes cannot be edited.",
            ));
        }
        match (mode, class) {
            (ProtectionMode::TrackedChanges, _) => {
                /* Every admitted edit must be recorded: review mode is
                forced on (it already is from the load; re-assert). */
                self.tracking_changes = true;
                match cmd {
                    Command::ToggleTrackChanges { enabled: false } => Some(
                        self.protection_refusal(mode, cmd, "Track Changes cannot be turned off."),
                    ),
                    Command::InsertText { at: None, .. } if self.selection.is_none() => {
                        Some(self.protection_refusal(
                            mode,
                            cmd,
                            "this change cannot be recorded as a tracked change.",
                        ))
                    }
                    _ => None,
                }
            }
            (ProtectionMode::Forms, ProtectionClass::Text) => self.forms_gate(mode, cmd),
            (ProtectionMode::Comments, ProtectionClass::Comment) => None,
            _ => Some(self.protection_refusal(mode, cmd, "this change is not allowed.")),
        }
    }

    /// `forms` protection for a `Text`-class command.
    fn forms_gate(&mut self, mode: ProtectionMode, cmd: &Command) -> Option<Event> {
        let refuse_why = "only form fields and content controls can be edited.";
        let target = match self.text_intent(cmd) {
            TextIntent::Inert => return None,
            TextIntent::Unknown => return Some(self.protection_refusal(mode, cmd, refuse_why)),
            TextIntent::Edit(t) => t,
        };
        let doc = self.undo.current().clone();
        /* A collapsed Backspace / Delete inside a text form field edits
        one character of its result (the generic path widens to the
        whole atomic field). */
        if let Some((rs, re)) = &target.raw_delete
            && let Some(FormRegion::TextField { field }) = doc.form_region_for_edit(
                &to_engine_pos(rs.clone()),
                &to_engine_pos(re.clone()),
                FormEdit::Text { inserts: false },
            )
        {
            return Some(self.fill_text_field(&doc, rs, re, field, ""));
        }
        let region = doc.form_region_for_edit(
            &to_engine_pos(target.start.clone()),
            &to_engine_pos(target.end.clone()),
            target.edit,
        );
        match region {
            None => Some(self.protection_refusal(mode, cmd, refuse_why)),
            Some(FormRegion::UnprotectedSection | FormRegion::BlockSdt) => None,
            Some(FormRegion::TextField { field }) => {
                Some(self.fill_text_field(&doc, &target.start, &target.end, field, &target.text))
            }
            Some(FormRegion::RunSdt { open, .. }) => {
                let replaces_from_opener = !target.text.is_empty()
                    && target.start.offset == open
                    && target.start.offset < target.end.offset;
                if replaces_from_opener {
                    /* Text inserted AT the opener lands before the control
                    (issue #245 travel rule): insert at the end of the
                    replaced range — inside — then remove the old text. */
                    let at_end = doc.insert_text(to_engine_pos(target.end.clone()), &target.text);
                    let new_doc = at_end.delete_range(
                        to_engine_pos(target.start.clone()),
                        to_engine_pos(target.end.clone()),
                    );
                    let caret = BridgeLogicalPos {
                        path: target.start.path.clone(),
                        offset: target.start.offset + target.text.len() as u32,
                    };
                    Some(self.commit_edit(new_doc, caret))
                } else {
                    None
                }
            }
        }
    }

    /// Fill in the text form field `field` of the paragraph `start` sits in
    /// (`[start, end)` → `text`) and commit it as one undo step.
    fn fill_text_field(
        &mut self,
        doc: &DocumentTree,
        start: &BridgeLogicalPos,
        end: &BridgeLogicalPos,
        field: usize,
        text: &str,
    ) -> Event {
        let path = bridge_to_engine_path(start.path.clone());
        match doc.fill_form_text_field(&path, field, start.offset, end.offset, text) {
            Some((new_doc, caret)) => {
                let caret = BridgeLogicalPos {
                    path: start.path.clone(),
                    offset: caret,
                };
                self.commit_edit(new_doc, caret)
            }
            None => Event::error("form field edit: the field no longer exists"),
        }
    }

    /// The edit a text-class command will make, resolved the way its
    /// handler resolves it.
    fn text_intent(&self, cmd: &Command) -> TextIntent {
        let selection_or = |at: BridgeLogicalPos| -> (BridgeLogicalPos, BridgeLogicalPos) {
            match &self.selection {
                Some(s) => ordered(s.anchor.clone(), s.caret.clone()),
                None => {
                    let at = self.with_selection_doc(|d| clamp_pos(d, at));
                    (at.clone(), at)
                }
            }
        };
        let typing = |at: BridgeLogicalPos, text: &str| {
            let (start, end) = selection_or(at);
            TextIntent::Edit(TextTarget {
                start,
                end,
                edit: FormEdit::Text {
                    inserts: !text.is_empty(),
                },
                text: text.to_string(),
                raw_delete: None,
            })
        };
        match cmd {
            Command::InsertText { at, text } => {
                match self.resolve_interactive_insert_at(at.clone()) {
                    Some(p) => typing(p, text),
                    None => TextIntent::Unknown,
                }
            }
            Command::EndComposition { commit } => match &self.composition {
                Some(c) if *commit && !c.text.is_empty() => typing(c.at.clone(), &c.text),
                _ => TextIntent::Inert,
            },
            Command::PastePlain { text } => {
                let caret = self
                    .selection
                    .as_ref()
                    .map_or_else(|| bpos_top(0, 0), |s| s.caret.clone());
                if text.contains(['\n', '\r']) {
                    let (start, end) = selection_or(caret);
                    TextIntent::Edit(TextTarget {
                        start,
                        end,
                        edit: FormEdit::Break,
                        text: String::new(),
                        raw_delete: None,
                    })
                } else {
                    typing(caret, text)
                }
            }
            /* Rich paste may insert paragraphs and tables: only a
            block-level content control can take it. */
            Command::PasteHtml { .. } => {
                let caret = self
                    .selection
                    .as_ref()
                    .map_or_else(|| bpos_top(0, 0), |s| s.caret.clone());
                let (start, end) = selection_or(caret);
                TextIntent::Edit(TextTarget {
                    start,
                    end,
                    edit: FormEdit::Break,
                    text: String::new(),
                    raw_delete: None,
                })
            }
            Command::DeleteRange { range } => {
                match self.resolve_edit_range("DeleteRange", range.clone()) {
                    Ok((start, end)) => TextIntent::Edit(TextTarget {
                        start,
                        end,
                        edit: FormEdit::Text { inserts: false },
                        text: String::new(),
                        raw_delete: None,
                    }),
                    Err(_) => TextIntent::Inert,
                }
            }
            Command::ReplaceRange { range, text } => {
                match self.resolve_edit_range("ReplaceRange", range.clone()) {
                    Ok((start, end)) => TextIntent::Edit(TextTarget {
                        start,
                        end,
                        edit: FormEdit::Text {
                            inserts: !text.is_empty(),
                        },
                        text: text.clone(),
                        raw_delete: None,
                    }),
                    Err(_) => TextIntent::Inert,
                }
            }
            Command::SplitParagraph { at } => {
                let (start, end) = match (&self.selection, at) {
                    (Some(s), _) => ordered(s.anchor.clone(), s.caret.clone()),
                    (None, Some(p)) => match self.resolve_edit_pos("SplitParagraph", p.clone()) {
                        Ok(p) => (p.clone(), p),
                        Err(_) => return TextIntent::Inert,
                    },
                    (None, None) => return TextIntent::Inert,
                };
                TextIntent::Edit(TextTarget {
                    start,
                    end,
                    edit: FormEdit::Break,
                    text: String::new(),
                    raw_delete: None,
                })
            }
            Command::DeleteAtCaret { forward, by_word } => {
                let Some(sel) = &self.selection else {
                    return TextIntent::Inert;
                };
                let (start, end) = ordered(sel.anchor.clone(), sel.caret.clone());
                if start != end {
                    return TextIntent::Edit(TextTarget {
                        start,
                        end,
                        edit: FormEdit::Text { inserts: false },
                        text: String::new(),
                        raw_delete: None,
                    });
                }
                let raw = self.delete_target_raw(sel.caret.clone(), *forward, *by_word);
                match self.delete_target(sel.caret.clone(), *forward, *by_word) {
                    Some((start, end)) => TextIntent::Edit(TextTarget {
                        start,
                        end,
                        edit: FormEdit::Text { inserts: false },
                        text: String::new(),
                        raw_delete: raw,
                    }),
                    None => TextIntent::Inert,
                }
            }
            _ => TextIntent::Unknown,
        }
    }
}
