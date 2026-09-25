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

use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
}

impl SourcePackage {
    /// Build from `(entry name, bytes)` pairs in archive order.
    pub fn from_entries<I: IntoIterator<Item = (String, Vec<u8>)>>(entries: I) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(name, data)| PackageEntry { name, data })
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
