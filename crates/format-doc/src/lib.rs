//! `format-doc` — a read-only importer for legacy Word 97-2003 binary
//! documents (`.doc`, the "Word Binary File Format", [MS-DOC] in the
//! Microsoft Open Specifications) into the engine's [`engine::DocumentTree`].
//!
//! There is no writer: a document opened from `.doc` saves as `.docx`
//! (through `format-docx`'s minimal-package writer, like any document the
//! engine built itself). The reader follows the public specification —
//! every structure cites its [MS-DOC] section — and parts of it are
//! modified from WordCraft (MIT OR Apache-2.0; each such file says so in
//! its header, and `THIRD-PARTY-NOTICES.md` at the repository root lists
//! them).
//!
//! What is imported (this crate's scope, issue-tracked beyond it):
//!
//! - the main document's text through the piece table ([`piece`]): 8-bit
//!   Windows-1252 and 16-bit UTF-16 pieces, in any physical order, so a
//!   fast-saved document reads like a fully saved one;
//! - paragraph boundaries and their direct formatting from the PAPX FKPs
//!   (alignment, indents, spacing, line spacing, right-to-left);
//! - character runs and their direct formatting from the CHPX FKPs (bold,
//!   italic, underline, strike, size, colour, the right-to-left flag and
//!   the complex-script bold / italic / size twins), with the piece's
//!   `Prm` applied on top;
//! - page size, margins and section starts from every section's SEPX.
//!
//! What is not (yet) imported is COUNTED and reported
//! ([`DocImport::not_imported`]): tables (their cell text becomes plain
//! paragraphs), list numbering, pictures and drawings, fields (each kept
//! as the text it last displayed), headers / footers, notes, comments,
//! text boxes, styles and fonts. Honest UX: the engine turns the report
//! into open warnings, so a partial import never looks like a clean one.
//!
//! The input is attacker-shaped and the wasm worker aborts on a panic: the
//! container goes through `format-docx`'s bounded MS-CFB reader, every
//! offset is range-checked ([`le`]), the text is held to the
//! `WordDocument` stream's size and to [`reader::MAX_TEXT_CHARS`], and the
//! paragraph count to [`reader::MAX_PARAGRAPHS`]. Hostile input yields a
//! typed [`DocError`] or a best-effort document, never a panic or an
//! unbounded allocation.

mod cp1252;
mod error;
mod fib;
mod fkp;
mod le;
mod piece;
mod props;
mod reader;
mod sections;
mod sprm;
#[doc(hidden)]
pub mod test_builder;
#[cfg(test)]
mod tests;

pub use error::DocError;
pub use reader::{MAX_PARAGRAPHS, MAX_TEXT_CHARS};

use engine::{DocumentTree, SectionProps};
use format_docx::opc::cfb::{CfbError, CompoundFile};

use crate::fkp::{BinKind, Bins};
use crate::sections::Section;

/// A class of `.doc` content the importer does not model yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LegacyContent {
    /// Table cells and rows: their text was imported as plain paragraphs.
    Tables,
    /// List numbering / bullets: the paragraphs came in without markers.
    Lists,
    /// Pictures and drawn objects (`0x01` / `0x08` anchors).
    Pictures,
    /// Fields: each was kept as the text it last displayed.
    Fields,
    /// The header / footer subdocument.
    HeadersFooters,
    /// The footnote and endnote subdocuments.
    Notes,
    /// The comment subdocument.
    Comments,
    /// The text-box subdocuments.
    TextBoxes,
    /// The style sheet and the font table: only direct formatting was
    /// imported.
    Styles,
}

impl LegacyContent {
    /// One line for the open-warning details list (never document text).
    pub fn describe(self) -> &'static str {
        match self {
            LegacyContent::Tables => "tables: their cell text was imported as plain paragraphs",
            LegacyContent::Lists => {
                "list numbering and bullets: the list paragraphs were imported without their \
                 markers"
            }
            LegacyContent::Pictures => "pictures and drawn objects",
            LegacyContent::Fields => "fields: each was kept as the text it last displayed",
            LegacyContent::HeadersFooters => "headers and footers",
            LegacyContent::Notes => "footnotes and endnotes",
            LegacyContent::Comments => "comments",
            LegacyContent::TextBoxes => "text boxes",
            LegacyContent::Styles => {
                "paragraph and character styles and fonts: only direct formatting was imported"
            }
        }
    }
}

/// One entry of the import report: `count` occurrences of `content` (a
/// subdocument that exists counts once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotImported {
    pub content: LegacyContent,
    pub count: u32,
}

/// A successful import.
#[derive(Debug, Clone)]
pub struct DocImport {
    /// The document: paragraphs with their direct formatting, every
    /// section's page setup (interior sections on their closing
    /// paragraphs, the last one as `body_section`). No source package —
    /// a save writes a fresh `.docx`.
    pub document: DocumentTree,
    /// `false` when the file has no section table: the page setup is then
    /// the host's default, not something the file said.
    pub has_page_setup: bool,
    /// What the importer did not model, in [`LegacyContent`] order.
    pub not_imported: Vec<NotImported>,
}

/// `true` when `bytes` are an OLE compound file that is NOT an
/// MS-OFFCRYPTO encrypted OOXML package — the files [`read_doc`] is for
/// (a `.doc`, or another legacy binary format it will refuse by name).
pub fn is_legacy_compound_file(bytes: &[u8]) -> bool {
    matches!(
        format_docx::sniff_compound_file(bytes),
        Some(format_docx::CompoundFileKind::Other)
    )
}

fn container(stream: Option<&'static str>, e: CfbError) -> DocError {
    DocError::Container {
        stream,
        detail: e.0,
    }
}

/// Read the top-level stream `name`: `Ok(None)` when it is absent.
fn stream(cf: &CompoundFile<'_>, name: &'static str) -> Result<Option<Vec<u8>>, DocError> {
    match cf.stream(name) {
        None => Ok(None),
        Some(Ok(bytes)) => Ok(Some(bytes)),
        Some(Err(e)) => Err(container(Some(name), e)),
    }
}

/// Import a Word 97-2003 binary document.
pub fn read_doc(bytes: &[u8]) -> Result<DocImport, DocError> {
    let cf = CompoundFile::parse(bytes).map_err(|e| container(None, e))?;
    let word = stream(&cf, "WordDocument")?.ok_or(DocError::NotWordDocument)?;
    let fib = fib::Fib::parse(&word)?;

    /* The FIB names the table stream; a writer that set the wrong bit is
    tolerated by falling back to the other one. */
    let (named, other) = if fib.which_tbl_stm {
        ("1Table", "0Table")
    } else {
        ("0Table", "1Table")
    };
    let (table_name, table) = match stream(&cf, named)? {
        Some(t) => (named, t),
        None => match stream(&cf, other)? {
            Some(t) => (other, t),
            None => {
                return Err(error::malformed(
                    "WordDocument: FIB",
                    "the document has neither a 0Table nor a 1Table stream",
                ));
            }
        },
    };
    let pair = |i: usize| fib.pair(i).unwrap_or((0, 0));

    let (fc, lcb) = pair(fib::pair::CLX);
    let pieces =
        piece::PieceTable::parse(&table, fc, lcb).map_err(|e| e.in_table_stream(table_name))?;
    let (fc, lcb) = pair(fib::pair::PLCF_BTE_CHPX);
    let chpx = Bins::parse(BinKind::Chpx, "Table: PlcfBteChpx", &table, fc, lcb)
        .map_err(|e| e.in_table_stream(table_name))?;
    let (fc, lcb) = pair(fib::pair::PLCF_BTE_PAPX);
    let papx = Bins::parse(BinKind::Papx, "Table: PlcfBtePapx", &table, fc, lcb)
        .map_err(|e| e.in_table_stream(table_name))?;
    let (fc, lcb) = pair(fib::pair::PLCF_SED);
    let sections =
        sections::parse(&word, &table, fc, lcb).map_err(|e| e.in_table_stream(table_name))?;
    let sect_ends: Vec<u32> = sections.iter().map(|s| s.end_cp).collect();

    let walk = reader::Walk {
        word: &word,
        pieces: &pieces,
        chpx: &chpx,
        papx: &papx,
        sect_ends: &sect_ends,
    };
    let (mut paragraphs, skipped) = walk.run(fib.ccp.text)?;

    /* Sections (the [MS-DOC] 2.4 algorithm "Determining Section
    Boundaries"): section i ends with the character before its `aCp[i +
    1]`. Every section but the last
    rides the paragraph its section mark closes (`Paragraph::section_end`,
    the engine's marker model); the last one is the body section. */
    let section_props = |s: &Section| SectionProps {
        geometry: s.props.geometry,
        section_type: s.props.section_type,
        ..SectionProps::default()
    };
    if let Some((_, interior)) = sections.split_last() {
        let last_end = paragraphs.last().map(|p| p.end_cp);
        for s in interior {
            if Some(s.end_cp) == last_end {
                continue;
            }
            if let Ok(i) = paragraphs.binary_search_by_key(&s.end_cp, |p| p.end_cp) {
                paragraphs[i].paragraph.section_end = Some(Box::new(section_props(s)));
            }
        }
    }
    let mut document = if paragraphs.is_empty() {
        DocumentTree::from_text("")
    } else {
        DocumentTree::from_rich_paragraphs(paragraphs.into_iter().map(|p| p.paragraph))
    };
    if let Some(last) = sections.last() {
        document.body_section = section_props(last);
    }

    let mut not_imported = Vec::new();
    let mut note = |content: LegacyContent, count: u32| {
        if count > 0 {
            not_imported.push(NotImported { content, count });
        }
    };
    let present = |ccp: u32| u32::from(ccp > 0);
    note(LegacyContent::Tables, skipped.table_paragraphs);
    note(LegacyContent::Lists, skipped.list_paragraphs);
    note(LegacyContent::Pictures, skipped.objects);
    note(LegacyContent::Fields, skipped.fields);
    note(LegacyContent::HeadersFooters, present(fib.ccp.hdd));
    note(
        LegacyContent::Notes,
        present(fib.ccp.ftn.saturating_add(fib.ccp.edn)),
    );
    note(LegacyContent::Comments, present(fib.ccp.atn));
    note(
        LegacyContent::TextBoxes,
        present(fib.ccp.txbx.saturating_add(fib.ccp.hdr_txbx)),
    );
    note(LegacyContent::Styles, present(pair(fib::pair::STSHF).1));

    Ok(DocImport {
        document,
        has_page_setup: !sections.is_empty(),
        not_imported,
    })
}
