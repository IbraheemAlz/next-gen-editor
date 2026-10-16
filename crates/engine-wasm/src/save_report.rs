//! Issue #485 - the writer's save report on the wire.
//!
//! `format_docx::save_docx_with_notes` returns the writer's best-effort
//! decisions ([`WriteNote`]): characters XML cannot carry written as
//! U+FFFD (#467), stale markup written at a clamped position, a content
//! control widened around a regenerated wrapper (#245). Before #485 the
//! engine dropped them after the save (`save_docx`), so a file that was
//! repaired looked exactly like a faithful one. [`bridge_save_notes`]
//! lowers them onto `Event::DocumentSaved::notes`: one bridge [`SaveNote`]
//! per kind (exhaustive - a new `WriteNote` variant does not compile
//! until it is classified here), repeats summed, first-occurrence order
//! kept.

use bridge::SaveNote;
use format_docx::WriteNote;

/// Lower a save's write notes onto the wire, one entry per kind.
pub(crate) fn bridge_save_notes(notes: &[WriteNote]) -> Vec<SaveNote> {
    let mut out: Vec<SaveNote> = Vec::new();
    for n in notes {
        let lowered = match n {
            WriteNote::ForbiddenCharactersReplaced { count } => {
                SaveNote::ForbiddenCharactersReplaced { count: *count }
            }
            WriteNote::StaleMarkupClamped { markers } => {
                SaveNote::StaleMarkupClamped { markers: *markers }
            }
            WriteNote::InlineWrapperWidened { .. } => SaveNote::InlineWrapperWidened { count: 1 },
        };
        match out.iter_mut().find(|e| e.kind() == lowered.kind()) {
            Some(SaveNote::ForbiddenCharactersReplaced { count })
            | Some(SaveNote::InlineWrapperWidened { count }) => {
                *count = count.saturating_add(lowered.count());
            }
            Some(SaveNote::StaleMarkupClamped { markers }) => {
                *markers = markers.saturating_add(lowered.count());
            }
            None => out.push(lowered),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_of_a_kind_sum_into_one_entry_in_first_occurrence_order() {
        let out = bridge_save_notes(&[
            WriteNote::InlineWrapperWidened { id: 7 },
            WriteNote::ForbiddenCharactersReplaced { count: 2 },
            WriteNote::StaleMarkupClamped { markers: 3 },
            WriteNote::InlineWrapperWidened { id: 9 },
            WriteNote::ForbiddenCharactersReplaced { count: 5 },
            WriteNote::StaleMarkupClamped { markers: 1 },
        ]);
        assert_eq!(
            out,
            vec![
                SaveNote::InlineWrapperWidened { count: 2 },
                SaveNote::ForbiddenCharactersReplaced { count: 7 },
                SaveNote::StaleMarkupClamped { markers: 4 },
            ]
        );
    }

    #[test]
    fn a_clean_save_has_no_notes() {
        assert!(bridge_save_notes(&[]).is_empty());
    }
}
