//! Issue #354 — the `word/settings.xml` toggles the engine owns, written
//! back by a surgical edit of the source part.
//!
//! The part rides the package passthrough byte-identical (zoom,
//! proofState, compat, rsids, … are never modelled). The writer owns
//! three on/off settings: `<w:evenAndOddHeaders/>` (issue #74, the
//! `SetEvenOddHeaders` command), `<w:trackRevisions/>` (the engine's review
//! mode, stamped onto the saved tree) and `<w:mirrorMargins/>` (read for
//! layout; no command changes it yet). Each is compared with what the ONE
//! settings parse ([`crate::parts::settings::parse_settings_xml`]) reads
//! from the source part:
//!
//! - equal → nothing is touched, whatever spelling the source used
//!   (`w:val="1"`, a paired tag): an unchanged flag keeps the part
//!   byte-identical, so does a toggle turned on and off again;
//! - turned ON → every source occurrence (an off spelling) is removed and
//!   the bare `<w:name/>` is inserted at its CT_Settings rank (ECMA-376
//!   Part 1 §17.15.1.78): after the last top-level child the schema puts
//!   before it, else right after the root start tag;
//! - turned OFF → every occurrence is removed (`w:val="0"` would also do,
//!   Word drops the element).
//!
//! A package without a settings part gets one ([`build_settings_xml`])
//! only when a toggle is on; the writer adds its content-type override and
//! document relationship the way it adds every other part it mints.

use crate::parts::settings::{SettingsPart, parse_settings_xml};
use engine::DocumentSettings;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// CT_Settings' child sequence (ECMA-376 Part 1 §17.15.1.78), qualified
/// with the prefixes the reader canonicalises to (issue #394). A child not
/// listed (the `w14:` / `w15:` / `w16*:` extensions Word appends, an
/// `mc:AlternateContent`) has no rank: it never anchors an insertion.
const SETTINGS_CHILD_ORDER: &[&[u8]] = &[
    b"w:writeProtection",
    b"w:view",
    b"w:zoom",
    b"w:removePersonalInformation",
    b"w:removeDateAndTime",
    b"w:doNotDisplayPageBoundaries",
    b"w:displayBackgroundShape",
    b"w:printPostScriptOverText",
    b"w:printFractionalCharacterWidth",
    b"w:printFormsData",
    b"w:embedTrueTypeFonts",
    b"w:embedSystemFonts",
    b"w:saveSubsetFonts",
    b"w:saveFormsData",
    b"w:mirrorMargins",
    b"w:alignBordersAndEdges",
    b"w:bordersDoNotSurroundHeader",
    b"w:bordersDoNotSurroundFooter",
    b"w:gutterAtTop",
    b"w:hideSpellingErrors",
    b"w:hideGrammaticalErrors",
    b"w:activeWritingStyle",
    b"w:proofState",
    b"w:formsDesign",
    b"w:attachedTemplate",
    b"w:linkStyles",
    b"w:stylePaneFormatFilter",
    b"w:stylePaneSortMethod",
    b"w:documentType",
    b"w:mailMerge",
    b"w:revisionView",
    b"w:trackRevisions",
    b"w:doNotTrackMoves",
    b"w:doNotTrackFormatting",
    b"w:documentProtection",
    b"w:autoFormatOverride",
    b"w:styleLockTheme",
    b"w:styleLockQFSet",
    b"w:defaultTabStop",
    b"w:autoHyphenation",
    b"w:consecutiveHyphenLimit",
    b"w:hyphenationZone",
    b"w:doNotHyphenateCaps",
    b"w:showEnvelope",
    b"w:summaryLength",
    b"w:clickAndTypeStyle",
    b"w:defaultTableStyle",
    b"w:evenAndOddHeaders",
    b"w:bookFoldRevPrinting",
    b"w:bookFoldPrinting",
    b"w:bookFoldPrintingSheets",
    b"w:drawingGridHorizontalSpacing",
    b"w:drawingGridVerticalSpacing",
    b"w:displayHorizontalDrawingGridEvery",
    b"w:displayVerticalDrawingGridEvery",
    b"w:doNotUseMarginsForDrawingGridOrigin",
    b"w:drawingGridHorizontalOrigin",
    b"w:drawingGridVerticalOrigin",
    b"w:doNotShadeFormData",
    b"w:noPunctuationKerning",
    b"w:characterSpacingControl",
    b"w:printTwoOnOne",
    b"w:strictFirstAndLastChars",
    b"w:noLineBreaksAfter",
    b"w:noLineBreaksBefore",
    b"w:savePreviewPicture",
    b"w:doNotValidateAgainstSchema",
    b"w:saveInvalidXml",
    b"w:ignoreMixedContent",
    b"w:alwaysShowPlaceholderText",
    b"w:doNotDemarcateInvalidXml",
    b"w:saveXmlDataOnly",
    b"w:useXSLTWhenSaving",
    b"w:saveThroughXslt",
    b"w:showXMLTags",
    b"w:alwaysMergeEmptyNamespace",
    b"w:updateFields",
    b"w:hdrShapeDefaults",
    b"w:footnotePr",
    b"w:endnotePr",
    b"w:compat",
    b"w:docVars",
    b"w:rsids",
    b"m:mathPr",
    b"w:attachedSchema",
    b"w:themeFontLang",
    b"w:clrSchemeMapping",
    b"w:doNotIncludeSubdocsInStats",
    b"w:doNotAutoCompressPictures",
    b"w:forceUpgrade",
    b"w:captions",
    b"w:readModeInkLockDown",
    b"w:smartTagType",
    b"sl:schemaLibrary",
    b"w:shapeDefaults",
    b"w:doNotEmbedSmartTags",
    b"w:decimalSymbol",
    b"w:listSeparator",
];

/// The schema rank of a top-level settings child, `None` when unlisted.
fn rank(qname: &[u8]) -> Option<usize> {
    SETTINGS_CHILD_ORDER.iter().position(|n| *n == qname)
}

/// One toggle the writer owns: its element, the live value and the value
/// the source part reads as.
struct Toggle {
    qname: &'static str,
    live: fn(&DocumentSettings) -> bool,
    source: fn(&SettingsPart) -> bool,
}

/// Every toggle the writer owns, in schema order.
const TOGGLES: [Toggle; 3] = [
    Toggle {
        qname: "w:mirrorMargins",
        live: |s| s.mirror_margins,
        source: |s| s.mirror_margins,
    },
    Toggle {
        qname: "w:trackRevisions",
        live: |s| s.track_revisions,
        source: |s| s.track_revisions,
    },
    Toggle {
        qname: "w:evenAndOddHeaders",
        live: |s| s.even_and_odd_headers,
        source: |s| s.even_and_odd_headers,
    },
];

/// A top-level child of the root: qualified name and byte range.
struct Child {
    qname: Vec<u8>,
    start: usize,
    end: usize,
}

/// Where things sit in the source part (byte offsets into it).
struct SettingsLayout {
    /// The root start tag's range; `self_closing` for `<w:settings/>`.
    root: (usize, usize),
    self_closing: bool,
    children: Vec<Child>,
}

/// Locate the root and its children. `None` for a part the scan cannot
/// follow (malformed, or a root other than the canonical `w:settings`).
fn scan(xml: &[u8]) -> Option<SettingsLayout> {
    /* quick-xml strips a BOM without counting it in its positions
    (issue #110). */
    let bom = if xml.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    let mut reader = Reader::from_reader(&xml[bom..]);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut pos = 0usize;
    let mut root = None;
    let mut self_closing = false;
    let mut children = Vec::new();
    let mut open: Option<(usize, Vec<u8>)> = None;
    loop {
        let ev = reader.read_event_into(&mut buf).ok()?;
        let end = reader.buffer_position() as usize;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                match depth {
                    0 if e.name().as_ref() != b"w:settings" => return None,
                    0 => root = Some((bom + pos, bom + end)),
                    1 => open = Some((bom + pos, e.name().as_ref().to_vec())),
                    _ => {}
                }
                depth += 1;
            }
            Event::Empty(e) => match depth {
                0 if e.name().as_ref() != b"w:settings" => return None,
                0 => {
                    root = Some((bom + pos, bom + end));
                    self_closing = true;
                }
                1 => children.push(Child {
                    qname: e.name().as_ref().to_vec(),
                    start: bom + pos,
                    end: bom + end,
                }),
                _ => {}
            },
            Event::End(e) => {
                depth = depth.checked_sub(1)?;
                if depth == 1 {
                    let (start, qname) = open.take()?;
                    if qname.as_slice() != e.name().as_ref() {
                        return None;
                    }
                    children.push(Child {
                        qname,
                        start,
                        end: bom + end,
                    });
                }
            }
            _ => {}
        }
        pos = end;
        buf.clear();
    }
    (depth == 0).then_some(())?;
    Some(SettingsLayout {
        root: root?,
        self_closing,
        children,
    })
}

/// One byte edit of the source part: `[start, end)` becomes `text`.
/// `order` breaks ties between insertions at the same offset (schema
/// rank).
struct Edit {
    start: usize,
    end: usize,
    order: usize,
    text: String,
}

/// Issue #354 — the patched `settings.xml` for the live `settings`, or
/// `None` when every owned toggle still reads as the source part says
/// (the passthrough keeps the bytes) or the part cannot be patched (it
/// is then kept as it is — the reader saw the same unreadable part).
pub(super) fn patch_settings_xml(xml: &[u8], settings: &DocumentSettings) -> Option<Vec<u8>> {
    let source = parse_settings_xml(xml).ok()?;
    let changed: Vec<(usize, &Toggle, bool)> = TOGGLES
        .iter()
        .filter_map(|t| {
            let live = (t.live)(settings);
            (live != (t.source)(&source)).then(|| (rank(t.qname.as_bytes()).unwrap_or(0), t, live))
        })
        .collect();
    if changed.is_empty() {
        return None;
    }
    let layout = scan(xml)?;
    let mut edits: Vec<Edit> = Vec::new();
    let mut root_insertions = String::new();
    for &(toggle_rank, toggle, on) in &changed {
        /* Every source occurrence goes (an off spelling when turning on). */
        for c in layout
            .children
            .iter()
            .filter(|c| c.qname == toggle.qname.as_bytes())
        {
            edits.push(Edit {
                start: c.start,
                end: c.end,
                order: toggle_rank,
                text: String::new(),
            });
        }
        if !on {
            continue;
        }
        let element = format!("<{}/>", toggle.qname);
        if layout.self_closing {
            root_insertions.push_str(&element);
            continue;
        }
        /* After the last child the schema ranks before this toggle. */
        let at = layout
            .children
            .iter()
            .filter(|c| rank(&c.qname).is_some_and(|r| r < toggle_rank))
            .map(|c| c.end)
            .max()
            .unwrap_or(layout.root.1);
        edits.push(Edit {
            start: at,
            end: at,
            order: toggle_rank,
            text: element,
        });
    }
    if layout.self_closing && !root_insertions.is_empty() {
        /* `<w:settings …/>` → `<w:settings …>` + toggles + end tag. */
        let (start, end) = layout.root;
        let tag = std::str::from_utf8(xml.get(start..end)?).ok()?;
        let open = tag.strip_suffix("/>")?.trim_end();
        edits.push(Edit {
            start,
            end,
            order: 0,
            text: format!("{open}>{root_insertions}</w:settings>"),
        });
    }
    edits.sort_by_key(|e| (e.start, e.end, e.order));
    let mut out = Vec::with_capacity(xml.len() + 64);
    let mut cursor = 0usize;
    for e in &edits {
        /* Edits never overlap: removals are whole distinct children,
        insertions sit at a child's end or the root tag's end. */
        if e.start < cursor {
            return None;
        }
        out.extend_from_slice(xml.get(cursor..e.start)?);
        out.extend_from_slice(e.text.as_bytes());
        cursor = e.end;
    }
    out.extend_from_slice(xml.get(cursor..)?);
    Some(out)
}

/// Issue #354 — a minimal `settings.xml` for a package that has none,
/// carrying every owned toggle that is on (schema order); `None` when all
/// are off (no part is minted). Transitional spelling: the caller maps it
/// to the write's namespace family.
pub(super) fn build_settings_xml(settings: &DocumentSettings) -> Option<String> {
    let toggles: String = TOGGLES
        .iter()
        .filter(|t| (t.live)(settings))
        .map(|t| format!("<{}/>", t.qname))
        .collect();
    (!toggles.is_empty()).then(|| {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:settings xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             {toggles}</w:settings>"
        )
    })
}

#[cfg(test)]
#[path = "writer_settings_patch_tests.rs"]
mod tests;
