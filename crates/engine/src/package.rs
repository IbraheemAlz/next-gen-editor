//! Issue #134 — the source OPC package an opened `.docx` came from.
//!
//! The live editor saves from the tree alone (engine-wasm `SaveDocx` /
//! `SaveDocument`). Before this module it synthesized a brand-new package
//! (`format_docx::build_minimal_docx`), dropping every part the model does
//! not regenerate — headers/footers (leaving dangling `headerReference`
//! ids, which Word reports as a corrupt file), `styles.xml`,
//! `numbering.xml`, `settings.xml`, theme, fontTable, comments, custom
//! XML. The `.docx` reader now records every archive entry except
//! `word/document.xml` here, verbatim and in archive order, and the save
//! path hands them to `format_docx::write_docx`, which re-emits them
//! byte-identical (additive splices only: content types, rels, new media).
//!
//! The package is immutable after open and shared by every undo state
//! through an [`Arc`] — an edit clones the pointer, never the bytes. The
//! crash-recovery snapshot (`engine-wasm` `EngineSnapshotV1`) persists it
//! ONCE per snapshot, not once per undo entry.
//!
//! **Snapshot size.** The package is the file minus `document.xml`, so for
//! a picture-heavy document its media parts would be persisted twice: once
//! here and once in `DocumentTree::media` (the same bytes, keyed by the
//! resolved target path since #188). The snapshot writes a media entry as a
//! [`MediaRef`] — the `media` key plus the length and a SHA-256 content
//! digest (issue #269; FNV-1a 64 in format-v1 snapshots) — instead of its
//! bytes ([`SourcePackage::deduplicated_against`]),
//! and restore re-hydrates it from the restored tree's `media`
//! ([`SourcePackage::rehydrated_from`]). **Fallback:** a reference that no
//! longer resolves to bytes of the recorded length and hash drops the whole
//! package — the recovered session then saves through the minimal-package
//! writer, exactly the pre-#134 behavior, rather than writing a package
//! with a wrong or missing part. Non-media binaries (embedded fonts,
//! OLE objects) have no second copy and ride the snapshot verbatim.
//!
//! **Content keys (issue #269).** The detached-package key
//! ([`package_key`], #212) and the media-reference check are SHA-256.
//! They were length + FNV-1a 64 before (snapshot format v1): fine as a
//! cache key within one session, but not collision-resistant — two
//! packages of equal length and equal FNV (constructible in seconds, see
//! the tests) would let a restore attach the wrong package and write its
//! sibling parts into another document. The old forms are still ACCEPTED
//! on restore for one release ([`LEGACY_PACKAGE_KEY_PREFIX`], a
//! [`MediaRef`] without `sha256`) so a snapshot persisted by the previous
//! build recovers; nothing writes them any more.

use crate::ImageBlob;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

/// Every entry of the source package except `word/document.xml`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct SourcePackage {
    /// Archive entries in their source order.
    pub entries: Vec<PackageEntry>,
}

/// One archive entry, verbatim.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct PackageEntry {
    /// Archive entry name (`word/styles.xml`, `word/media/image1.png`).
    pub name: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    /// Snapshot-only: `data` is empty and the bytes are the tree's
    /// `media` blob this names (see the module docs). Always `None` on a
    /// live package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_ref: Option<MediaRef>,
}

/// A package entry persisted by reference to `DocumentTree::media`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct MediaRef {
    /// The `DocumentTree::media` key holding the bytes.
    pub key: String,
    /// Byte length of the referenced bytes.
    pub len: u64,
    /// Format-v1 check: FNV-1a 64 of the referenced bytes. Issue #269 —
    /// retired: `0` and not written by this build; still honoured on a
    /// v1 reference (one with no [`Self::sha256`]) for one release.
    #[serde(skip_serializing_if = "is_zero")]
    pub hash: u64,
    /// Issue #269 — SHA-256 of the referenced bytes (32 bytes). `None`
    /// only on a reference persisted by a format-v1 snapshot.
    #[serde(with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Vec<u8>>,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl MediaRef {
    /// Whether `data` are the bytes this reference was taken from:
    /// length plus the SHA-256 digest, or — a format-v1 reference — the
    /// legacy FNV-1a 64.
    pub fn matches(&self, data: &[u8]) -> bool {
        if data.len() as u64 != self.len {
            return false;
        }
        match &self.sha256 {
            Some(digest) => sha256(data).as_slice() == digest.as_slice(),
            None => legacy_fnv1a64(data) == self.hash,
        }
    }
}

/// Issue #269 — SHA-256 of `bytes`, the content digest of the
/// crash-recovery package key and of a [`MediaRef`].
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// FNV-1a 64 — the format-v1 content check, kept only to read snapshots
/// persisted by the previous build (issue #269). Not collision-resistant.
pub fn legacy_fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Issue #269 — prefix of a current package key (`sha256-<64 hex>`).
pub const PACKAGE_KEY_PREFIX: &str = "sha256-";
/// Issue #269 — prefix of a format-v1 package key
/// (`pkg-<len hex>-<fnv1a64 hex>`), accepted on restore for one release.
pub const LEGACY_PACKAGE_KEY_PREFIX: &str = "pkg-";

/// Issue #212 — content key of a detached, encoded package (the
/// `Event::Snapshot.package_hash` the crash-recovery event log stores the
/// package under, once per document). Issue #269 — SHA-256 of the encoded
/// bytes, so two different packages never share a key: a restore can
/// only ever attach the package the snapshot was taken with.
pub fn package_key(encoded: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut key = String::with_capacity(PACKAGE_KEY_PREFIX.len() + 64);
    key.push_str(PACKAGE_KEY_PREFIX);
    for b in sha256(encoded) {
        let _ = write!(key, "{b:02x}");
    }
    key
}

/// The format-v1 package key (length + FNV-1a 64) — only to recognise a
/// package persisted by the previous build (issue #269).
pub fn legacy_package_key(encoded: &[u8]) -> String {
    format!(
        "{LEGACY_PACKAGE_KEY_PREFIX}{:x}-{:016x}",
        encoded.len(),
        legacy_fnv1a64(encoded)
    )
}

/// Issue #269 — whether `encoded` is the package `key` names, for either
/// key format (see the module docs). An unknown prefix never matches.
pub fn package_key_matches(key: &str, encoded: &[u8]) -> bool {
    if key.starts_with(PACKAGE_KEY_PREFIX) {
        package_key(encoded) == key
    } else if key.starts_with(LEGACY_PACKAGE_KEY_PREFIX) {
        legacy_package_key(encoded) == key
    } else {
        false
    }
}

/// Issue #269 — whether `key` is in the format-v1 (FNV) form.
pub fn is_legacy_package_key(key: &str) -> bool {
    key.starts_with(LEGACY_PACKAGE_KEY_PREFIX)
}

impl SourcePackage {
    /// Build from `(entry name, bytes)` pairs in archive order.
    pub fn from_entries<I: IntoIterator<Item = (String, Vec<u8>)>>(entries: I) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(name, data)| PackageEntry {
                    name,
                    data,
                    media_ref: None,
                })
                .collect(),
        }
    }

    /// The entry named `name`, if the package carries it.
    pub fn entry(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.data.as_slice())
    }

    /// Total payload bytes (entry data only).
    pub fn byte_len(&self) -> usize {
        self.entries.iter().map(|e| e.data.len()).sum()
    }

    /// The snapshot form: every entry whose bytes equal a `media` blob is
    /// replaced by a [`MediaRef`] to it (the smallest key wins, so the
    /// output is deterministic). Everything else stays verbatim.
    ///
    /// Candidates are bucketed by length and confirmed byte for byte, so
    /// the SHA-256 digest is computed only for a blob an entry actually
    /// references (once per blob), never for every part of the package.
    pub fn deduplicated_against(&self, media: &HashMap<String, ImageBlob>) -> SourcePackage {
        if media.is_empty() {
            return self.clone();
        }
        let mut by_len: HashMap<usize, Vec<&str>> = HashMap::new();
        let mut keys: Vec<&String> = media.keys().collect();
        keys.sort_unstable();
        for key in keys {
            by_len
                .entry(media[key].data.len())
                .or_default()
                .push(key.as_str());
        }
        let mut digests: HashMap<&str, [u8; 32]> = HashMap::new();
        SourcePackage {
            entries: self
                .entries
                .iter()
                .map(|e| {
                    if e.media_ref.is_none() && !e.data.is_empty() {
                        let hit = by_len.get(&e.data.len()).and_then(|keys| {
                            keys.iter().copied().find(|k| media[*k].data == e.data)
                        });
                        if let Some(key) = hit {
                            let digest = *digests.entry(key).or_insert_with(|| sha256(&e.data));
                            return PackageEntry {
                                name: e.name.clone(),
                                data: Vec::new(),
                                media_ref: Some(MediaRef {
                                    key: key.to_string(),
                                    len: e.data.len() as u64,
                                    hash: 0,
                                    sha256: Some(digest.to_vec()),
                                }),
                            };
                        }
                    }
                    e.clone()
                })
                .collect(),
        }
    }

    /// Inverse of [`Self::deduplicated_against`]. `None` when a reference
    /// does not resolve to bytes of the recorded length and digest — the
    /// caller drops the package (see the module docs' fallback).
    pub fn rehydrated_from(&self, media: &HashMap<String, ImageBlob>) -> Option<SourcePackage> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            match &e.media_ref {
                None => entries.push(e.clone()),
                Some(r) => {
                    let data = &media.get(&r.key)?.data;
                    if !r.matches(data) {
                        return None;
                    }
                    entries.push(PackageEntry {
                        name: e.name.clone(),
                        data: data.clone(),
                        media_ref: None,
                    });
                }
            }
        }
        Some(SourcePackage { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(data: &[u8]) -> ImageBlob {
        ImageBlob {
            content_type: "image/png".into(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn media_entries_persist_by_reference_and_rehydrate() {
        let pkg = SourcePackage::from_entries([
            ("word/styles.xml".to_string(), b"<w:styles/>".to_vec()),
            ("word/media/image1.png".to_string(), b"PNG-ONE".to_vec()),
            ("word/fonts/font1.odttf".to_string(), b"FONT".to_vec()),
        ]);
        let media = HashMap::from([
            ("rId9".to_string(), blob(b"PNG-ONE")),
            ("rId4".to_string(), blob(b"PNG-ONE")),
        ]);
        let snap = pkg.deduplicated_against(&media);
        assert_eq!(snap.entries[0], pkg.entries[0]);
        assert_eq!(snap.entries[2], pkg.entries[2]);
        assert!(snap.entries[1].data.is_empty());
        assert_eq!(snap.entries[1].media_ref.as_ref().unwrap().key, "rId4");
        assert_eq!(snap.rehydrated_from(&media).as_ref(), Some(&pkg));
        /* No media: nothing to reference. */
        assert_eq!(pkg.deduplicated_against(&HashMap::new()), pkg);
    }

    #[test]
    fn an_unresolvable_reference_drops_the_package() {
        let pkg = SourcePackage::from_entries([(
            "word/media/image1.png".to_string(),
            b"PNG-ONE".to_vec(),
        )]);
        let media = HashMap::from([("rId4".to_string(), blob(b"PNG-ONE"))]);
        let snap = pkg.deduplicated_against(&media);
        assert_eq!(snap.rehydrated_from(&HashMap::new()), None);
        let changed = HashMap::from([("rId4".to_string(), blob(b"PNG-TWO"))]);
        assert_eq!(snap.rehydrated_from(&changed), None);
    }

    /// Issue #269 — two 16-byte messages with the same FNV-1a 64 (found by
    /// a distinguished-point rho search over hex strings, ~2^32 steps on
    /// 32 cores in about a second).
    const FNV_COLLISION: (&[u8], &[u8]) = (b"38b0726403d33c1d", b"7128c048dccab265");

    #[test]
    fn fnv_colliding_packages_get_distinct_keys() {
        let (a, b) = FNV_COLLISION;
        assert_ne!(a, b);
        assert_eq!(a.len(), b.len());
        /* The collision is real: the format-v1 key cannot tell them apart. */
        assert_eq!(legacy_fnv1a64(a), legacy_fnv1a64(b));
        assert_eq!(legacy_package_key(a), legacy_package_key(b));
        /* The current key can, and each key only matches its own bytes. */
        assert_ne!(package_key(a), package_key(b));
        assert!(package_key(a).starts_with(PACKAGE_KEY_PREFIX));
        assert_eq!(package_key(a).len(), PACKAGE_KEY_PREFIX.len() + 64);
        assert!(package_key_matches(&package_key(a), a));
        assert!(!package_key_matches(&package_key(a), b));
        /* A format-v1 key still matches (one-release migration) — and is
        exactly as weak as it always was. */
        assert!(package_key_matches(&legacy_package_key(a), a));
        assert!(package_key_matches(&legacy_package_key(a), b));
        assert!(is_legacy_package_key(&legacy_package_key(a)));
        assert!(!package_key_matches("md5-whatever", a));
    }

    #[test]
    fn fnv_colliding_media_no_longer_rehydrates_the_wrong_bytes() {
        let (a, b) = FNV_COLLISION;
        let pkg = SourcePackage::from_entries([("word/media/image1.png".to_string(), a.to_vec())]);
        let media = HashMap::from([("rId4".to_string(), blob(a))]);
        let snap = pkg.deduplicated_against(&media);
        let r = snap.entries[0].media_ref.clone().unwrap();
        assert_eq!(r.hash, 0, "the FNV field is retired");
        assert_eq!(r.sha256.as_deref(), Some(sha256(a).as_slice()));
        /* The blob under the key was swapped for the colliding bytes. */
        let swapped = HashMap::from([("rId4".to_string(), blob(b))]);
        assert_eq!(snap.rehydrated_from(&swapped), None);
        assert_eq!(snap.rehydrated_from(&media).as_ref(), Some(&pkg));
        /* A format-v1 reference (FNV only) would have accepted them. */
        let mut v1 = snap.clone();
        v1.entries[0].media_ref = Some(MediaRef {
            sha256: None,
            hash: legacy_fnv1a64(a),
            ..r
        });
        assert_eq!(v1.rehydrated_from(&media).as_ref(), Some(&pkg));
        assert!(v1.rehydrated_from(&swapped).is_some());
    }

    /// Issue #269 — a format-v1 `MediaRef` (no `sha256` field on the
    /// wire) still decodes and verifies; a v2 one omits the FNV field.
    #[test]
    fn media_ref_wire_forms_round_trip() {
        #[derive(Serialize)]
        struct V1Ref<'a> {
            key: &'a str,
            len: u64,
            hash: u64,
        }
        let data = b"PNG-ONE";
        let v1 = V1Ref {
            key: "rId4",
            len: data.len() as u64,
            hash: legacy_fnv1a64(data),
        };
        let bytes = crate::snapshot::encode(&v1).unwrap();
        let back: MediaRef = crate::snapshot::decode(&bytes).unwrap().payload;
        assert_eq!(back.sha256, None);
        assert!(back.matches(data));
        assert!(!back.matches(b"PNG-TWO"));
        let v2 = MediaRef {
            key: "rId4".into(),
            len: data.len() as u64,
            hash: 0,
            sha256: Some(sha256(data).to_vec()),
        };
        let bytes = crate::snapshot::encode(&v2).unwrap();
        assert!(
            !bytes.windows(4).any(|w| w == b"hash"),
            "FNV field not written"
        );
        let back: MediaRef = crate::snapshot::decode(&bytes).unwrap().payload;
        assert_eq!(back, v2);
        assert!(back.matches(data));
    }
}

/// `serde(with)` for `Option<Arc<SourcePackage>>` — serde's `Arc` impls
/// sit behind its `rc` feature, which the workspace does not enable.
pub mod arc_option {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<Arc<SourcePackage>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_deref().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<SourcePackage>>, D::Error> {
        Ok(Option::<SourcePackage>::deserialize(deserializer)?.map(Arc::new))
    }
}
