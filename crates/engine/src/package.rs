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
//! here and once in `DocumentTree::media` (the same bytes, keyed by
//! relationship id). The snapshot writes a media entry as a
//! [`MediaRef`] — the `media` key plus the length and an FNV-1a 64 content
//! hash — instead of its bytes ([`SourcePackage::deduplicated_against`]),
//! and restore re-hydrates it from the restored tree's `media`
//! ([`SourcePackage::rehydrated_from`]). **Fallback:** a reference that no
//! longer resolves to bytes of the recorded length and hash drops the whole
//! package — the recovered session then saves through the minimal-package
//! writer, exactly the pre-#134 behavior, rather than writing a package
//! with a wrong or missing part. Non-media binaries (embedded fonts,
//! OLE objects) have no second copy and ride the snapshot verbatim.

use crate::ImageBlob;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
    /// FNV-1a 64 of the referenced bytes.
    pub hash: u64,
}

/// FNV-1a 64 — a content check for [`MediaRef`], not a security hash.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
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
    pub fn deduplicated_against(&self, media: &HashMap<String, ImageBlob>) -> SourcePackage {
        if media.is_empty() {
            return self.clone();
        }
        let mut by_content: HashMap<(usize, u64), &str> = HashMap::new();
        let mut keys: Vec<&String> = media.keys().collect();
        keys.sort_unstable();
        for key in keys.into_iter().rev() {
            let data = &media[key].data;
            by_content.insert((data.len(), fnv1a64(data)), key.as_str());
        }
        SourcePackage {
            entries: self
                .entries
                .iter()
                .map(|e| {
                    if e.media_ref.is_none() && !e.data.is_empty() {
                        let hash = fnv1a64(&e.data);
                        if let Some(key) = by_content.get(&(e.data.len(), hash))
                            && media[*key].data == e.data
                        {
                            return PackageEntry {
                                name: e.name.clone(),
                                data: Vec::new(),
                                media_ref: Some(MediaRef {
                                    key: (*key).to_string(),
                                    len: e.data.len() as u64,
                                    hash,
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
    /// does not resolve to bytes of the recorded length and hash — the
    /// caller drops the package (see the module docs' fallback).
    pub fn rehydrated_from(&self, media: &HashMap<String, ImageBlob>) -> Option<SourcePackage> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            match &e.media_ref {
                None => entries.push(e.clone()),
                Some(r) => {
                    let data = &media.get(&r.key)?.data;
                    if data.len() as u64 != r.len || fnv1a64(data) != r.hash {
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
