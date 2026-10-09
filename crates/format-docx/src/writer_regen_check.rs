//! Issue #384 — the paragraph regenerator measured on its own.
//!
//! A clean paragraph is written from its source bytes, so a zero-edit save
//! says nothing about how faithfully the writer REGENERATES it — yet every
//! ordinary edit regenerates the paragraph it touches, and whatever the
//! regenerator cannot reproduce becomes `source_bytes_rewritten` (#199 /
//! #251). [`regen_check`] runs one ordinary [`super::write_docx`] with a
//! probe switched on: every clean paragraph that would be replayed from
//! its bytes (body and table cells, at any depth) is ALSO serialized
//! through the regenerate path, in the exact write context — trusted
//! source package, published comment plan and inherited directions,
//! hyperlink ids resolved for every paragraph — and compared with its
//! source bytes. The save itself is discarded.
//!
//! The probe has no effect on an ordinary save: nothing is collected
//! unless [`regen_check`] is on the stack, and the regeneration it adds
//! rolls back what it synthesized (comment reference runs).

use super::{Paragraph, comment_anchors, revision_ids, serialize_paragraph};
use crate::error::DocxError;
use crate::opc::archive::DocxArchive;
use engine::DocumentTree;
use std::collections::HashMap;

/// One clean paragraph whose regeneration differs from its source bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegenMismatch {
    /// The paragraph's source bytes (`<w:p …>…</w:p>`).
    pub source: String,
    /// What the regenerate path writes for it (annotation-id tokens
    /// resolved to the ids they keep).
    pub regenerated: String,
    /// The paragraph has text.
    pub nonempty: bool,
}

/// What [`regen_check`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegenReport {
    /// Clean paragraphs regenerated and compared.
    pub checked: u32,
    /// Of those, the ones with text.
    pub nonempty_checked: u32,
    /// Every paragraph that did not come back byte-identical.
    pub mismatches: Vec<RegenMismatch>,
}

thread_local! {
    /// The report of the running [`regen_check`]; `None` outside it.
    static REGEN_CHECK: std::cell::RefCell<Option<RegenReport>> =
        const { std::cell::RefCell::new(None) };
}

/// Issue #384 — regenerate every clean paragraph of `doc` (written
/// against `archive`) and compare it with its source bytes. See the
/// module docs.
pub fn regen_check(archive: &DocxArchive, doc: &DocumentTree) -> Result<RegenReport, DocxError> {
    let outer = REGEN_CHECK.with(|c| c.replace(Some(RegenReport::default())));
    let res = super::write_docx(archive, doc);
    let report = REGEN_CHECK.with(|c| std::mem::replace(&mut *c.borrow_mut(), outer));
    res.map(|_| report.unwrap_or_default())
}

/// `true` while a [`regen_check`] runs (and is not already inside a
/// probe's own regeneration).
pub(super) fn active() -> bool {
    REGEN_CHECK.with(|c| c.borrow().is_some())
}

/// Probe one clean paragraph written from `src`.
pub(super) fn check_paragraph(
    para: &Paragraph,
    src: &str,
    hyperlink_rel_map: &HashMap<String, String>,
) {
    /* Taken out for the probe's own regeneration: a text-box story it
    writes must not be probed again from inside. */
    let Some(mut report) = REGEN_CHECK.with(|c| c.borrow_mut().take()) else {
        return;
    };
    let checkpoint = comment_anchors::checkpoint();
    let mut regen = String::new();
    serialize_paragraph(para, &mut regen, hyperlink_rel_map);
    comment_anchors::rollback(checkpoint);
    let regen = revision_ids::detokenize(&regen);
    let nonempty = !para.text.is_empty();
    report.checked += 1;
    report.nonempty_checked += u32::from(nonempty);
    if regen != src {
        report.mismatches.push(RegenMismatch {
            source: src.to_string(),
            regenerated: regen,
            nonempty,
        });
    }
    REGEN_CHECK.with(|c| *c.borrow_mut() = Some(report));
}
