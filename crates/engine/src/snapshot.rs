//! Issue #85 — versioned crash-recovery snapshot envelope.
//!
//! Recovery doctrine (plans/PRD.md §4 track 4): recovery = **base snapshot +
//! replayed change log, persisted continuously**. This module owns the base
//! snapshot's *wire format*; what goes into the payload is the engine
//! shell's business (`engine-wasm` assembles its `EngineSnapshotV1` from the
//! document tree, the undo window, the selection and the story state and
//! hands it to [`encode`]).
//!
//! ## Envelope
//!
//! ```text
//! offset 0..4   MAGIC  b"NGES"
//! offset 4      FORMAT_VERSION (u8)
//! offset 5..    MessagePack payload, structs as NAMED maps (self-describing)
//! ```
//!
//! ## Versioning discipline — per-version defaults, no migration tool
//!
//! * The payload is MessagePack with **named** struct fields, so a reader
//!   can skip fields it does not know and fill fields it does not find.
//!   Every model struct carries `#[serde(default)]`; *adding* a field is
//!   therefore backwards compatible within a format version — an older
//!   snapshot simply reads the new field as its `Default`.
//! * When a field's historical *implicit* value differs from its `Default`,
//!   or a field changes meaning, bump [`FORMAT_VERSION`] and teach the
//!   reader's per-version defaults hook (`engine-wasm`
//!   `EngineSnapshotV1::apply_version_defaults`) what the older version
//!   implied. [`decode`] hands the version back next to the payload for
//!   exactly that purpose.
//! * There is no migration tool: a snapshot outside
//!   `MIN_SUPPORTED_VERSION..=FORMAT_VERSION` is refused
//!   ([`SnapshotError::UnsupportedVersion`]) and recovery falls back to the
//!   replay log alone.
//! * Never rename or repurpose a field; retire one by leaving it unread.
//!
//! Map-typed model fields serialize **sorted by key** ([`ser_sorted_map`]) so
//! two snapshots of the same state are byte-identical regardless of
//! `HashMap` iteration order — the recovery e2e gate compares the pre-trap
//! and post-recovery snapshots byte for byte.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Serialize, Serializer};

/// Leading bytes of every snapshot — a cheap "is this ours?" check that
/// keeps a corrupted or foreign IndexedDB row from being fed to the
/// MessagePack decoder as if it were a document.
pub const MAGIC: [u8; 4] = *b"NGES";
/// Format version this build writes.
pub const FORMAT_VERSION: u8 = 1;
/// Oldest format version this build can still read.
pub const MIN_SUPPORTED_VERSION: u8 = 1;
/// Bytes preceding the payload: `MAGIC` + the version byte.
pub const HEADER_LEN: usize = MAGIC.len() + 1;

/// Why a snapshot could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// Zero bytes — "no snapshot persisted yet", never a corruption.
    Empty,
    /// Fewer than [`HEADER_LEN`] bytes.
    TooShort(usize),
    /// The first four bytes are not [`MAGIC`].
    BadMagic,
    /// A version outside `MIN_SUPPORTED_VERSION..=FORMAT_VERSION`.
    UnsupportedVersion(u8),
    /// The payload failed to serialize.
    Encode(String),
    /// The payload failed to deserialize.
    Decode(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Empty => write!(f, "snapshot: empty buffer"),
            SnapshotError::TooShort(n) => {
                write!(
                    f,
                    "snapshot: {n} bytes is shorter than the {HEADER_LEN}-byte header"
                )
            }
            SnapshotError::BadMagic => write!(f, "snapshot: bad magic (not an NGES snapshot)"),
            SnapshotError::UnsupportedVersion(v) => write!(
                f,
                "snapshot: format version {v} unsupported (this build reads \
                 {MIN_SUPPORTED_VERSION}..={FORMAT_VERSION})"
            ),
            SnapshotError::Encode(e) => write!(f, "snapshot: encode failed: {e}"),
            SnapshotError::Decode(e) => write!(f, "snapshot: decode failed: {e}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// A decoded payload plus the format version it was written with, so the
/// caller can apply that version's defaults.
#[derive(Debug, Clone)]
pub struct Decoded<T> {
    pub version: u8,
    pub payload: T,
}

/// Serialize `payload` behind the versioned header.
pub fn encode<T: Serialize + ?Sized>(payload: &T) -> Result<Vec<u8>, SnapshotError> {
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(&MAGIC);
    out.push(FORMAT_VERSION);
    /* `with_struct_map` = named fields. Positional (array) structs would be
    ~30 % smaller but only tolerate APPENDED fields; named maps tolerate
    any addition, which is the whole point of `#[serde(default)]`. */
    let mut ser = rmp_serde::Serializer::new(&mut out).with_struct_map();
    payload
        .serialize(&mut ser)
        .map_err(|e| SnapshotError::Encode(e.to_string()))?;
    Ok(out)
}

/// Validate the header and return the version byte without decoding.
pub fn peek_version(bytes: &[u8]) -> Result<u8, SnapshotError> {
    if bytes.is_empty() {
        return Err(SnapshotError::Empty);
    }
    if bytes.len() < HEADER_LEN {
        return Err(SnapshotError::TooShort(bytes.len()));
    }
    if bytes[..MAGIC.len()] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let version = bytes[MAGIC.len()];
    if !(MIN_SUPPORTED_VERSION..=FORMAT_VERSION).contains(&version) {
        return Err(SnapshotError::UnsupportedVersion(version));
    }
    Ok(version)
}

/// Validate the header and decode the payload as `T`.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<Decoded<T>, SnapshotError> {
    let version = peek_version(bytes)?;
    let payload = rmp_serde::from_slice::<T>(&bytes[HEADER_LEN..])
        .map_err(|e| SnapshotError::Decode(e.to_string()))?;
    Ok(Decoded { version, payload })
}

/// `#[serde(serialize_with)]` helper: emit a `HashMap` sorted by key so the
/// encoding is deterministic across processes and insertion orders.
/// Deserialization is the plain `HashMap` impl — the reader does not care
/// about order.
pub fn ser_sorted_map<K, V, S>(map: &HashMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    K: Ord + Serialize,
    V: Serialize,
    S: Serializer,
{
    let sorted: BTreeMap<&K, &V> = map.iter().collect();
    sorted.serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numbering::{AbstractNum, LvlDef, NumInstance, NumberingDefinitions};
    use crate::{
        Block, CommentDef, CommentRange, DocumentTree, ImageBlob, ListItem, LogicalPos, NoteKind,
        NoteStory, NoteType, Paragraph, ParagraphStyle, SpanStyle, StyleRun,
    };
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq, Default)]
    #[serde(default)]
    struct V1 {
        a: u32,
        name: String,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq, Default)]
    #[serde(default)]
    struct V2 {
        a: u32,
        name: String,
        /// Added after V1 shipped — an older payload must read it as the
        /// default, never fail.
        added_later: Option<f32>,
        flag: bool,
    }

    #[test]
    fn envelope_round_trips_and_carries_the_version() {
        let v = V1 {
            a: 7,
            name: "seven".into(),
        };
        let bytes = encode(&v).unwrap();
        assert_eq!(&bytes[..4], b"NGES");
        assert_eq!(bytes[4], FORMAT_VERSION);
        let back: Decoded<V1> = decode(&bytes).unwrap();
        assert_eq!(back.version, FORMAT_VERSION);
        assert_eq!(back.payload, v);
    }

    #[test]
    fn decode_rejects_empty_short_and_foreign_buffers() {
        assert_eq!(decode::<V1>(&[]).unwrap_err(), SnapshotError::Empty);
        assert_eq!(
            decode::<V1>(b"NGE").unwrap_err(),
            SnapshotError::TooShort(3)
        );
        assert_eq!(
            decode::<V1>(b"XXXX\x01\x80").unwrap_err(),
            SnapshotError::BadMagic
        );
    }

    #[test]
    fn decode_refuses_versions_outside_the_supported_range() {
        let mut bytes = encode(&V1::default()).unwrap();
        bytes[4] = FORMAT_VERSION + 1;
        assert_eq!(
            decode::<V1>(&bytes).unwrap_err(),
            SnapshotError::UnsupportedVersion(FORMAT_VERSION + 1)
        );
        bytes[4] = 0;
        assert_eq!(
            decode::<V1>(&bytes).unwrap_err(),
            SnapshotError::UnsupportedVersion(0)
        );
    }

    #[test]
    fn missing_fields_read_as_defaults_and_unknown_fields_are_skipped() {
        /* Forward: an old (V1) payload read by a newer struct. */
        let old = V1 {
            a: 3,
            name: "n".into(),
        };
        let back: Decoded<V2> = decode(&encode(&old).unwrap()).unwrap();
        assert_eq!(
            back.payload,
            V2 {
                a: 3,
                name: "n".into(),
                added_later: None,
                flag: false,
            }
        );
        /* Backward: a newer payload read by an older struct ignores the
        fields it does not model. */
        let new = V2 {
            a: 9,
            name: "x".into(),
            added_later: Some(1.5),
            flag: true,
        };
        let back: Decoded<V1> = decode(&encode(&new).unwrap()).unwrap();
        assert_eq!(
            back.payload,
            V1 {
                a: 9,
                name: "x".into()
            }
        );
    }

    #[derive(Serialize, Deserialize, Default)]
    struct MapHolder {
        #[serde(serialize_with = "ser_sorted_map")]
        m: HashMap<String, u32>,
    }

    #[test]
    fn sorted_map_encoding_is_insertion_order_independent() {
        let mut a = MapHolder::default();
        let mut b = MapHolder::default();
        for k in ["zeta", "alpha", "mid", "beta"] {
            a.m.insert(k.into(), k.len() as u32);
        }
        for k in ["beta", "mid", "alpha", "zeta"] {
            b.m.insert(k.into(), k.len() as u32);
        }
        assert_eq!(encode(&a).unwrap(), encode(&b).unwrap());
        let back: Decoded<MapHolder> = decode(&encode(&a).unwrap()).unwrap();
        assert_eq!(back.payload.m, a.m);
    }

    /// A document exercising every map + bytes field on the tree.
    fn rich_document() -> DocumentTree {
        let mut p0 = Paragraph {
            text: "Hello, snapshot world".into(),
            ..Default::default()
        };
        p0.spans.push(StyleRun {
            start: 0,
            end: 5,
            style: SpanStyle {
                bold: Some(true),
                font_size: Some(14.0),
                ..Default::default()
            },
        });
        p0.source_xml = Some(b"<w:p><w:r><w:t>Hello, snapshot world</w:t></w:r></w:p>".to_vec());
        let p1 = Paragraph {
            text: "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}".into(),
            list_item: Some(ListItem { num_id: 1, ilvl: 0 }),
            resolved_marker: Some("1.".into()),
            dirty: true,
            ..Default::default()
        };
        let mut doc = DocumentTree::from_blocks([Block::Paragraph(p0), Block::Paragraph(p1)]);
        doc.headers.insert(
            "rId7".into(),
            vec![Block::Paragraph(Paragraph {
                text: "Header text".into(),
                ..Default::default()
            })],
        );
        doc.footers.insert(
            "rId8".into(),
            vec![Block::Paragraph(Paragraph {
                text: "Footer text".into(),
                ..Default::default()
            })],
        );
        doc.media.insert(
            "rId9".into(),
            ImageBlob {
                content_type: "image/png".into(),
                data: vec![0x89, b'P', b'N', b'G', 0, 1, 2, 255],
            },
        );
        doc.footnote_stories.insert(
            1,
            NoteStory {
                id: 1,
                kind: NoteKind::Footnote,
                note_type: NoteType::Normal,
                body: vec![Block::Paragraph(Paragraph {
                    text: "a footnote".into(),
                    ..Default::default()
                })],
                source_xml: Some(b"<w:footnote w:id=\"1\"/>".to_vec()),
                dirty: false,
            },
        );
        doc.notes_dirty.endnotes = true;
        doc.comment_defs.insert(
            0,
            CommentDef {
                author: "Reviewer".into(),
                date: "2026-09-01T00:00:00Z".into(),
                paragraphs: vec!["Looks good".into()],
                ..Default::default()
            },
        );
        doc.comment_ranges.push(CommentRange {
            id: 0,
            start: LogicalPos::at_top_paragraph(&doc, 0, 0).unwrap(),
            end: LogicalPos::at_top_paragraph(&doc, 0, 5).unwrap(),
        });
        doc.styles.insert(
            "Heading1".into(),
            ParagraphStyle {
                id: "Heading1".into(),
                name: "heading 1".into(),
                ..Default::default()
            },
        );
        doc.styles.insert(
            "Normal".into(),
            ParagraphStyle {
                id: "Normal".into(),
                name: "Normal".into(),
                ..Default::default()
            },
        );
        doc.numbering = NumberingDefinitions {
            abstract_nums: HashMap::from([(
                0,
                AbstractNum {
                    id: 0,
                    levels: vec![LvlDef::default()],
                },
            )]),
            num_instances: HashMap::from([(
                1,
                NumInstance {
                    num_id: 1,
                    abstract_num_id: 0,
                    overrides: Vec::new(),
                },
            )]),
            dirty: false,
        };
        doc.settings.even_and_odd_headers = true;
        doc.styles_dirty = true;
        doc
    }

    #[test]
    fn document_tree_round_trips_every_story_and_table() {
        let doc = rich_document();
        let bytes = encode(&doc).unwrap();
        let back: Decoded<DocumentTree> = decode(&bytes).unwrap();
        let d = back.payload;
        assert_eq!(d.paragraph_text(0), Some("Hello, snapshot world"));
        assert_eq!(d.paragraph_text(1), doc.paragraph_text(1));
        let p0 = d.nth_paragraph(0).unwrap();
        assert_eq!(p0.spans.len(), 1);
        assert_eq!(p0.spans[0].style.bold, Some(true));
        assert_eq!(
            p0.source_xml.as_deref(),
            Some(&b"<w:p><w:r><w:t>Hello, snapshot world</w:t></w:r></w:p>"[..])
        );
        let p1 = d.nth_paragraph(1).unwrap();
        assert_eq!(p1.list_item, Some(ListItem { num_id: 1, ilvl: 0 }));
        assert_eq!(p1.resolved_marker.as_deref(), Some("1."));
        assert!(p1.dirty);
        assert_eq!(
            d.headers["rId7"][0].as_paragraph().unwrap().text,
            "Header text"
        );
        assert_eq!(
            d.footers["rId8"][0].as_paragraph().unwrap().text,
            "Footer text"
        );
        assert_eq!(d.media["rId9"].content_type, "image/png");
        assert_eq!(
            d.media["rId9"].data,
            vec![0x89, b'P', b'N', b'G', 0, 1, 2, 255]
        );
        let note = &d.footnote_stories[&1];
        assert_eq!(note.kind, NoteKind::Footnote);
        assert_eq!(note.body[0].as_paragraph().unwrap().text, "a footnote");
        assert_eq!(
            note.source_xml.as_deref(),
            Some(b"<w:footnote w:id=\"1\"/>".as_slice())
        );
        assert!(d.notes_dirty.endnotes && !d.notes_dirty.footnotes);
        assert_eq!(d.comment_defs[&0].author, "Reviewer");
        assert_eq!(d.comment_ranges.len(), 1);
        assert_eq!(d.comment_ranges[0].end.offset, 5);
        assert_eq!(d.styles.len(), 2);
        assert_eq!(d.styles["Heading1"].name, "heading 1");
        assert_eq!(d.numbering.abstract_nums.len(), 1);
        assert_eq!(d.numbering.num_instances[&1].abstract_num_id, 0);
        assert!(d.settings.even_and_odd_headers);
        assert!(d.styles_dirty);
        /* Byte-stable: re-encoding the decoded tree reproduces the bytes. */
        assert_eq!(encode(&d).unwrap(), bytes);
    }

    #[test]
    fn document_encoding_is_deterministic_across_map_insertion_orders() {
        let a = rich_document();
        let mut b = rich_document();
        /* Same content, different HashMap insertion history. */
        let styles: Vec<_> = b.styles.drain().collect();
        for (k, v) in styles.into_iter().rev() {
            b.styles.insert(k, v);
        }
        let headers: Vec<_> = b.headers.drain().collect();
        for (k, v) in headers.into_iter().rev() {
            b.headers.insert(k, v);
        }
        assert_eq!(encode(&a).unwrap(), encode(&b).unwrap());
    }

    /// Informational — sizes and timings for a 50-page-shaped document
    /// (`tools/perf-fixtures`: 20 paragraphs per page). The browser-side
    /// budget check lives in the recovery e2e; this keeps a native
    /// reference number next to the codec.
    #[test]
    fn fifty_page_document_snapshot_cost_reference() {
        let filler = "The quick brown fox jumps over the lazy dog while the engine \
                      lays out justified Arabic and Latin text on an A4 page. ";
        let mut paras = Vec::new();
        for i in 0..1000u32 {
            let mut p = Paragraph {
                text: format!("{i}: {filler}{filler}{filler}"),
                ..Default::default()
            };
            p.source_xml =
                Some(format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", p.text).into_bytes());
            paras.push(Block::Paragraph(p));
        }
        let doc = DocumentTree::from_blocks(paras);
        let t0 = std::time::Instant::now();
        let bytes = encode(&doc).unwrap();
        let enc = t0.elapsed();
        let t1 = std::time::Instant::now();
        let back: Decoded<DocumentTree> = decode(&bytes).unwrap();
        let dec = t1.elapsed();
        assert_eq!(back.payload.paragraph_count(), 1000);
        eprintln!(
            "[snapshot] 50p-shaped doc: {} bytes, encode {:?}, decode {:?}",
            bytes.len(),
            enc,
            dec
        );
    }
}
