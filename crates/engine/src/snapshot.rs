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
//! ## Version history / migration notes
//!
//! * **v1** (issue #85) — the original envelope. Content keys were
//!   length + FNV-1a 64: the detached-package key `pkg-<len>-<fnv>`
//!   (`EngineSnapshotV1::package_hash`, #212) and `MediaRef::hash` (#134).
//! * **v2** (issue #269) — content keys are SHA-256: the package key is
//!   `sha256-<64 hex>` ([`crate::package::package_key`]) and a
//!   [`crate::MediaRef`] carries `sha256` (its `hash` field is retired —
//!   written as absent, read as `0`). No field changed meaning, so there
//!   is no per-version default: the v1 forms are recognised by SHAPE (the
//!   `pkg-` prefix, a `MediaRef` without `sha256`) and verified with the
//!   legacy FNV check, which is what keeps a snapshot persisted by the
//!   previous build recoverable. **One release only:** the next bump
//!   raises [`MIN_SUPPORTED_VERSION`] to 2 and deletes the FNV path
//!   (`package::legacy_*`, `MediaRef::hash`).
//!
//! ## Untrusted input — declared lengths are checked first (issue #422)
//!
//! A snapshot comes back from IndexedDB, so its bytes are untrusted.
//! MessagePack prefixes every array / map / string / binary with a declared
//! length, and some visitors preallocate from it uncapped (`im`'s `Vector`
//! and `HashMap` call `Vec::with_capacity(size_hint)`; serde's own
//! collections cap at 1 MiB): one flipped bit in a `blocks` header would
//! ask a 2 GiB wasm heap for `4 G × size_of::<Block>()` and trap the
//! worker instead of returning an error. [`decode`] therefore runs
//! [`validate_payload`] first — an allocation-free walk of the whole value
//! that refuses any declared length the remaining bytes cannot hold
//! ([`SnapshotError::DeclaredLength`]), nesting past
//! [`MAX_PAYLOAD_DEPTH`] ([`SnapshotError::TooDeep`]) and any malformation
//! ([`SnapshotError::Malformed`]). After it passes, every declared count is
//! backed by real values, so a preallocation can never exceed what the
//! decoded payload itself occupies.
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
pub const FORMAT_VERSION: u8 = 2;
/// Oldest format version this build can still read.
pub const MIN_SUPPORTED_VERSION: u8 = 1;
/// Bytes preceding the payload: `MAGIC` + the version byte.
pub const HEADER_LEN: usize = MAGIC.len() + 1;
/// Issue #422 — the deepest container nesting a payload may declare
/// (rmp-serde's own default limit, so [`validate_payload`] never accepts a
/// shape the decoder would refuse for depth). The deepest real document —
/// tables nested to the reader's 256-level XML cap — stays well under it.
pub const MAX_PAYLOAD_DEPTH: usize = 1024;

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
    /// Issue #422 — a container / string / binary at payload byte `offset`
    /// declares a length (`declared` values or bytes) larger than the
    /// `remaining` bytes after its header could hold. Refused before the
    /// decoder could preallocate from it.
    DeclaredLength {
        offset: usize,
        declared: u64,
        remaining: u64,
    },
    /// Issue #422 — containers nest deeper than [`MAX_PAYLOAD_DEPTH`] at
    /// payload byte `offset`.
    TooDeep { offset: usize },
    /// Issue #422 — the payload is not a well-formed MessagePack value
    /// (truncated header or value, reserved marker) at payload byte
    /// `offset`.
    Malformed { offset: usize, what: &'static str },
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
            SnapshotError::DeclaredLength {
                offset,
                declared,
                remaining,
            } => write!(
                f,
                "snapshot: payload byte {offset} declares a length of {declared} but only \
                 {remaining} bytes follow"
            ),
            SnapshotError::TooDeep { offset } => write!(
                f,
                "snapshot: payload nests deeper than {MAX_PAYLOAD_DEPTH} levels at byte {offset}"
            ),
            SnapshotError::Malformed { offset, what } => {
                write!(f, "snapshot: malformed payload at byte {offset}: {what}")
            }
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

/// Validate the header and decode the payload as `T`. The payload is
/// structurally validated first ([`validate_payload`], issue #422), so no
/// declared length reaches the decoder unbacked.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<Decoded<T>, SnapshotError> {
    let version = peek_version(bytes)?;
    let payload = &bytes[HEADER_LEN..];
    validate_payload(payload)?;
    let payload =
        rmp_serde::from_slice::<T>(payload).map_err(|e| SnapshotError::Decode(e.to_string()))?;
    Ok(Decoded { version, payload })
}

/// Issue #422 — walk the MessagePack value at the start of `payload`
/// without decoding (or allocating per value) and refuse it unless every
/// declared length fits the bytes that follow its header: a string /
/// binary / extension needs its bytes, an array its `n` values and a map
/// its `2n` (each value is at least one byte). Containers may nest at most
/// [`MAX_PAYLOAD_DEPTH`] deep. Returns the length of the value; bytes after
/// it are ignored, exactly as `rmp_serde::from_slice` ignores them.
///
/// Iterative (one `u64` per open container), so a hostile nesting depth
/// costs neither stack nor more than `8 × MAX_PAYLOAD_DEPTH` bytes of heap.
pub fn validate_payload(payload: &[u8]) -> Result<usize, SnapshotError> {
    fn be(payload: &[u8], pos: &mut usize, n: usize, at: usize) -> Result<u64, SnapshotError> {
        let end = pos.checked_add(n).filter(|&e| e <= payload.len());
        let Some(end) = end else {
            return Err(SnapshotError::Malformed {
                offset: at,
                what: "truncated length / scalar header",
            });
        };
        let v = payload[*pos..end]
            .iter()
            .fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        *pos = end;
        Ok(v)
    }
    let mut pos = 0usize;
    // Values still owed by each open container, innermost last.
    let mut open: Vec<u64> = Vec::new();
    loop {
        let at = pos;
        let Some(&marker) = payload.get(pos) else {
            return Err(SnapshotError::Malformed {
                offset: at,
                what: "truncated: a declared value is missing",
            });
        };
        pos += 1;
        /* `(nested values, bytes to skip)` this header declares. */
        let (children, skip): (u64, u64) = match marker {
            0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => (0, 0),
            0x80..=0x8f => (2 * u64::from(marker & 0x0f), 0),
            0x90..=0x9f => (u64::from(marker & 0x0f), 0),
            0xa0..=0xbf => (0, u64::from(marker & 0x1f)),
            0xc1 => {
                return Err(SnapshotError::Malformed {
                    offset: at,
                    what: "reserved marker 0xc1",
                });
            }
            // bin / str 8, 16, 32: a length, then that many bytes.
            0xc4 | 0xd9 => (0, be(payload, &mut pos, 1, at)?),
            0xc5 | 0xda => (0, be(payload, &mut pos, 2, at)?),
            0xc6 | 0xdb => (0, be(payload, &mut pos, 4, at)?),
            // ext 8, 16, 32: a length, a type byte, the data.
            0xc7 => (0, be(payload, &mut pos, 1, at)? + 1),
            0xc8 => (0, be(payload, &mut pos, 2, at)? + 1),
            0xc9 => (0, be(payload, &mut pos, 4, at)? + 1),
            // float 32 / 64, (u)int 8..64: fixed widths.
            0xca => (0, 4),
            0xcb => (0, 8),
            0xcc | 0xd0 => (0, 1),
            0xcd | 0xd1 => (0, 2),
            0xce | 0xd2 => (0, 4),
            0xcf | 0xd3 => (0, 8),
            // fixext 1, 2, 4, 8, 16: a type byte plus the data.
            0xd4 => (0, 2),
            0xd5 => (0, 3),
            0xd6 => (0, 5),
            0xd7 => (0, 9),
            0xd8 => (0, 17),
            // array / map 16, 32.
            0xdc => (be(payload, &mut pos, 2, at)?, 0),
            0xdd => (be(payload, &mut pos, 4, at)?, 0),
            0xde => (2 * be(payload, &mut pos, 2, at)?, 0),
            0xdf => (2 * be(payload, &mut pos, 4, at)?, 0),
        };
        let remaining = (payload.len() - pos) as u64;
        let declared = skip.max(children);
        if declared > remaining {
            return Err(SnapshotError::DeclaredLength {
                offset: at,
                declared,
                remaining,
            });
        }
        pos += skip as usize;
        if children > 0 {
            if open.len() >= MAX_PAYLOAD_DEPTH {
                return Err(SnapshotError::TooDeep { offset: at });
            }
            open.push(children);
            continue;
        }
        /* A complete value: it may complete its container, and that one
        its own, and so on up. */
        loop {
            match open.last_mut() {
                None => return Ok(pos),
                Some(owed) => {
                    *owed -= 1;
                    if *owed > 0 {
                        break;
                    }
                    open.pop();
                }
            }
        }
    }
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

/// Issue #422 — the most bytes [`de_bounded_vector`] reserves up front
/// (serde's own collections use the same 1 MiB "cautious" bound).
pub const MAX_PREALLOC_BYTES: usize = 1024 * 1024;

/// Issue #422 — `#[serde(deserialize_with)]` helper for an `im::Vector`.
/// `im`'s own visitor calls `Vec::with_capacity(size_hint)` uncapped, so
/// even after [`validate_payload`] (which only guarantees one payload byte
/// per declared element) a crafted `blocks` array of N one-byte values
/// would reserve `N × size_of::<Block>()` before the first element failed
/// to decode. This visitor reserves at most [`MAX_PREALLOC_BYTES`] and
/// grows from there, exactly like serde's `Vec` impl. Output is unchanged:
/// the same elements in the same order.
pub fn de_bounded_vector<'de, D, T>(deserializer: D) -> Result<im::Vector<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Clone,
{
    struct BoundedVector<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de> + Clone> serde::de::Visitor<'de> for BoundedVector<T> {
        type Value = im::Vector<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a sequence")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let cap = seq
                .size_hint()
                .unwrap_or(0)
                .min(MAX_PREALLOC_BYTES / std::mem::size_of::<T>().max(1));
            let mut out = Vec::with_capacity(cap);
            while let Some(v) = seq.next_element()? {
                out.push(v);
            }
            Ok(out.into_iter().collect())
        }
    }
    deserializer.deserialize_seq(BoundedVector(std::marker::PhantomData))
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

    /// A payload struct with an `im::Vector` — the collection whose serde
    /// visitor preallocates the DECLARED length uncapped.
    #[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
    #[serde(default)]
    struct VecHolder {
        v: im::Vector<u64>,
        name: String,
    }

    fn envelope(payload: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.push(FORMAT_VERSION);
        out.extend_from_slice(payload);
        out
    }

    /// Issue #422 — a container that declares more values than the bytes
    /// after it can hold is refused BEFORE rmp-serde runs: an `array32`
    /// of 4 G `u64`s in a 20-byte buffer must not become a 32 GiB
    /// `Vec::with_capacity` (it would abort a wasm worker).
    #[test]
    fn a_lying_container_length_is_refused_before_decoding() {
        // {"v": array32(0xFFFF_FFFF)} and nothing after it.
        let mut p = vec![0x81, 0xa1, b'v', 0xdd];
        p.extend_from_slice(&u32::MAX.to_be_bytes());
        p.extend_from_slice(&[0x01, 0x02]);
        assert_eq!(
            decode::<VecHolder>(&envelope(&p)).unwrap_err(),
            SnapshotError::DeclaredLength {
                offset: 3,
                declared: u64::from(u32::MAX),
                remaining: 2
            }
        );
        // The same for a map32 (2n values), a str32 and a bin32.
        for (marker, children_per_entry) in [(0xdfu8, 2u64), (0xdb, 1), (0xc6, 1)] {
            let mut p = vec![marker];
            p.extend_from_slice(&0x7fff_ffffu32.to_be_bytes());
            p.push(0xc0);
            let err = validate_payload(&p).unwrap_err();
            assert_eq!(
                err,
                SnapshotError::DeclaredLength {
                    offset: 0,
                    declared: 0x7fff_ffff * children_per_entry,
                    remaining: 1
                },
                "{marker:#x}"
            );
        }
        // An honest count of the same shape decodes.
        let ok = VecHolder {
            v: (0..300u64).collect(),
            name: "n".into(),
        };
        let back: Decoded<VecHolder> = decode(&encode(&ok).unwrap()).unwrap();
        assert_eq!(back.payload, ok);
    }

    #[test]
    fn nesting_and_malformations_are_typed_errors() {
        // MAX_PAYLOAD_DEPTH nested one-element arrays around a nil: fine.
        let mut deep = vec![0x91; MAX_PAYLOAD_DEPTH];
        deep.push(0xc0);
        assert_eq!(validate_payload(&deep), Ok(deep.len()));
        // One more level is refused, without recursion.
        let mut deeper = vec![0x91; MAX_PAYLOAD_DEPTH + 1];
        deeper.push(0xc0);
        assert_eq!(
            validate_payload(&deeper).unwrap_err(),
            SnapshotError::TooDeep {
                offset: MAX_PAYLOAD_DEPTH
            }
        );
        assert!(matches!(
            validate_payload(&[0xc1]).unwrap_err(),
            SnapshotError::Malformed { offset: 0, .. }
        ));
        assert!(matches!(
            validate_payload(&[0xdd, 0x00]).unwrap_err(),
            SnapshotError::Malformed { offset: 0, .. }
        ));
        // Two values declared, one byte left: refused at the header.
        assert_eq!(
            validate_payload(&[0x92, 0xc0]).unwrap_err(),
            SnapshotError::DeclaredLength {
                offset: 0,
                declared: 2,
                remaining: 1
            }
        );
        // Plausible counts, but the second value never comes.
        assert!(matches!(
            validate_payload(&[0x92, 0x91, 0xc0]).unwrap_err(),
            SnapshotError::Malformed { offset: 3, .. }
        ));
        assert!(matches!(
            validate_payload(&[]).unwrap_err(),
            SnapshotError::Malformed { offset: 0, .. }
        ));
        // Every scalar width, and bytes after the value are ignored (as
        // `rmp_serde::from_slice` ignores them).
        let scalars: &[&[u8]] = &[
            &[0xca, 0, 0, 0, 0],
            &[0xcb, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0xcf, 0, 0, 0, 0, 0, 0, 0, 1],
            &[0xd3, 0, 0, 0, 0, 0, 0, 0, 1],
            &[0xd4, 1, 2],
            &[0xd8, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0xc7, 2, 9, 1, 2],
            &[0xa3, b'a', b'b', b'c'],
        ];
        for s in scalars {
            assert_eq!(validate_payload(s), Ok(s.len()), "{s:x?}");
            let mut trailing = s.to_vec();
            trailing.push(0xc1);
            assert_eq!(validate_payload(&trailing), Ok(s.len()), "{s:x?}");
        }
    }

    /// Issue #422 — the walk agrees with the encoder: every snapshot this
    /// module writes validates to its full length.
    #[test]
    fn every_encoded_payload_validates_to_its_full_length() {
        let doc = rich_document();
        let bytes = encode(&doc).unwrap();
        assert_eq!(
            validate_payload(&bytes[HEADER_LEN..]),
            Ok(bytes.len() - HEADER_LEN)
        );
        let small = encode(&V2::default()).unwrap();
        assert_eq!(
            validate_payload(&small[HEADER_LEN..]),
            Ok(small.len() - HEADER_LEN)
        );
    }

    /// Issue #422 — the second line of defence: even straight through
    /// `rmp_serde` (no [`validate_payload`]), a `blocks` array declaring
    /// 2^31 entries reserves at most [`MAX_PREALLOC_BYTES`] and fails on
    /// the missing elements. `im`'s own visitor would have asked for
    /// `2^31 × size_of::<Block>()` and aborted the process.
    #[test]
    fn a_lying_blocks_count_reserves_a_bounded_prefix() {
        let mut p = vec![0x81, 0xa6];
        p.extend_from_slice(b"blocks");
        p.push(0xdd);
        p.extend_from_slice(&0x7fff_ffffu32.to_be_bytes());
        p.push(0xc0);
        assert!(rmp_serde::from_slice::<DocumentTree>(&p).is_err());
        // An honest array decodes to the same document as before.
        let doc = rich_document();
        let back: Decoded<DocumentTree> = decode(&encode(&doc).unwrap()).unwrap();
        assert_eq!(encode(&back.payload).unwrap(), encode(&doc).unwrap());
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

    /// Issues #199 / #106 — a paragraph's attribute-level grab bag and
    /// source markup survive crash recovery byte-stably, and a paragraph
    /// without one (every engine-authored paragraph) encodes exactly as
    /// before (the field is skipped, so older snapshots read it as `None`).
    #[test]
    fn paragraph_source_markup_round_trips() {
        use crate::{SourceAttr, SourceMarker, SourceMarkup, SourcePPr, SourceRun, SpanStyle};
        let attr = |n: &str, v: &str| SourceAttr {
            name: n.into(),
            value: v.into(),
            ws: None,
        };
        let mut doc = DocumentTree::from_text("Hello wrold");
        let plain = encode(&doc).unwrap();
        let markup = SourceMarkup {
            text_len: 11,
            attrs: vec![attr("w14:paraId", "1A2B3C4D"), attr("w:rsidR", "00A1B2C3")],
            ppr: Some(SourcePPr {
                xml: br#"<w:pPr><w:ind w:left="720"/></w:pPr>"#.to_vec(),
                style_id: Some("Body".into()),
                ..SourcePPr::default()
            }),
            runs: vec![SourceRun {
                start: 6,
                end: 11,
                attrs: vec![attr("w:rsidR", "00445566")],
                rpr: Some(br#"<w:rPr><w:u w:val="single" w:color="FF0000"/></w:rPr>"#.to_vec()),
                style: SpanStyle {
                    bold: Some(true),
                    ..SpanStyle::default()
                },
                lead: b"<w:lastRenderedPageBreak/>".to_vec(),
                t_attrs: Some(Vec::new()),
                /* Issue #245 — pretty-print whitespace inside the run. */
                pad: Some(Box::new(crate::RunPad {
                    open: b"\n  ".to_vec(),
                    after_rpr: b"\n  ".to_vec(),
                    close: b"\n".to_vec(),
                })),
                bare_edge_ws: true,
            }],
            markers: vec![
                SourceMarker {
                    at: 6,
                    xml: br#"<w:proofErr w:type="spellStart"/>"#.to_vec(),
                    ..SourceMarker::default()
                },
                SourceMarker {
                    /* Issue #244 — a content span keeps its role. */
                    at: 11,
                    xml: br#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#.to_vec(),
                    role: crate::MarkerRole::Content,
                    comment: None,
                },
                SourceMarker {
                    /* Issue #245 — a content control's two ends. */
                    at: 0,
                    xml: b"<w:sdt><w:sdtContent>".to_vec(),
                    role: crate::MarkerRole::Open {
                        id: 7,
                        close_xml: b"</w:sdtContent></w:sdt>".to_vec(),
                    },
                    comment: None,
                },
                SourceMarker {
                    at: 5,
                    xml: b"</w:sdtContent></w:sdt>".to_vec(),
                    role: crate::MarkerRole::Close { id: 7 },
                    comment: None,
                },
                SourceMarker {
                    /* Issue #243 — a comment anchor keeps its identity. */
                    at: 5,
                    xml: br#"<w:commentRangeEnd w:id="3"/>"#.to_vec(),
                    comment: Some(crate::CommentAnchor {
                        kind: crate::CommentAnchorKind::RangeEnd,
                        id: 3,
                    }),
                    ..SourceMarker::default()
                },
            ],
        };
        let Some(crate::Block::Paragraph(p)) = doc.blocks.get(0).cloned() else {
            panic!("paragraph");
        };
        doc.blocks.set(
            0,
            crate::Block::Paragraph(crate::Paragraph {
                source_markup: Some(Box::new(markup.clone())),
                /* Issue #246 — a field's source form travels too. */
                fields: vec![crate::Field {
                    start: 0,
                    end: 5,
                    instruction: "FILENAME".into(),
                    span: None,
                    source: Some(Box::new(crate::FieldSource {
                        instruction: "FILENAME".into(),
                        open: br#"<w:fldSimple w:instr=" FILENAME ">"#.to_vec(),
                        close: b"</w:fldSimple>".to_vec(),
                    })),
                }],
                ..p
            }),
        );
        let bytes = encode(&doc).unwrap();
        assert_ne!(bytes, plain);
        let back: Decoded<DocumentTree> = decode(&bytes).unwrap();
        let p0 = back.payload.nth_paragraph(0).unwrap();
        assert_eq!(p0.source_markup.as_deref(), Some(&markup));
        assert!(p0.fields[0].source.is_some(), "field source form");
        assert_eq!(encode(&back.payload).unwrap(), bytes, "byte-stable");
        /* No markup: the pre-#199 encoding, and it decodes to `None`. */
        let back: Decoded<DocumentTree> = decode(&plain).unwrap();
        assert!(
            back.payload
                .nth_paragraph(0)
                .unwrap()
                .source_markup
                .is_none()
        );
    }

    /// Issues #359 / #104 / #249 — the complex-script twins survive crash
    /// recovery byte-stably, and a style without them encodes exactly as
    /// before they existed (the unset twins are absent keys).
    #[test]
    fn complex_script_twins_round_trip_and_stay_absent_when_unset() {
        let latin = SpanStyle {
            font_size: Some(11.0),
            ..SpanStyle::default()
        };
        let plain = encode(&latin).unwrap();
        let has_key = |bytes: &[u8], key: &[u8]| bytes.windows(key.len()).any(|w| w == key);
        for key in [
            &b"font_size_cs"[..],
            b"bold_cs",
            b"italic_cs",
            b"char_style",
            b"font_family_cs",
        ] {
            assert!(
                !has_key(&plain, key),
                "unset twins are absent from the encoding"
            );
        }
        let twins = SpanStyle {
            font_size_cs: Some(14.0),
            bold_cs: Some(true),
            italic_cs: Some(false),
            char_style: Some("Emph".into()),
            font_family_cs: Some(crate::FontFamily::Amiri),
            ..latin
        };
        let bytes = encode(&twins).unwrap();
        let back: Decoded<SpanStyle> = decode(&bytes).unwrap();
        assert_eq!(back.payload, twins);
        assert_eq!(encode(&back.payload).unwrap(), bytes, "byte-stable");
        let old: Decoded<SpanStyle> = decode(&plain).unwrap();
        assert_eq!(old.payload.font_size_cs, None);
    }

    /// Issue #79 — the `<w:bidiVisual>` flag survives crash recovery.
    #[test]
    fn table_bidi_visual_round_trips() {
        let doc = DocumentTree::from_text("x")
            .insert_table(crate::BlockPath::top(1), 1, 3)
            .set_table_bidi_visual(crate::BlockPath::top(1), true);
        let bytes = encode(&doc).unwrap();
        let back: Decoded<DocumentTree> = decode(&bytes).unwrap();
        let t = back.payload.blocks[1].as_table().unwrap();
        assert!(t.props.bidi_visual);
        assert_eq!(encode(&back.payload).unwrap(), bytes);
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
