//! Issue #135 — images inserted into an OPENED document.
//!
//! `write_docx` re-emits the source package's sibling entries verbatim, so
//! a picture the user inserted after open (`DocumentTree::media` gains an
//! engine-minted key such as `nge_img_1`) has no `word/media/*` part, no
//! `<Relationship>` and possibly no `[Content_Types].xml` `<Default>` for
//! its extension. This module plans the additive OPC plumbing:
//!
//! * a **new** image is a `doc.media` key that some story references
//!   (`InlineKind::Image::rel_id` of a picture WITHOUT a reader-resolved
//!   `media_key` — issue #188) but that is NOT a relationship id of
//!   `word/_rels/document.xml.rels` — imported pictures keep their ids and
//!   their parts byte-identical;
//! * every new image gets a Word-shaped relationship id `rIdN` above the
//!   highest `rIdN` of EVERY `.rels` part of the package (one id that is
//!   free in the document, header, footer and note scopes at once, so the
//!   same id can be declared in whichever parts reference the picture),
//!   and a media part name `word/media/imageK.<ext>` above the highest
//!   existing `imageK` (never colliding with any entry);
//! * the writer renames the model's id to the package id on a copy of the
//!   tree ([`rename_image_rel_ids`]) so every emitter writes `r:embed`
//!   consistently, then splices one `<Relationship>` row per referencing
//!   part into that part's rels (existing rows untouched) and one
//!   `<Default Extension>` per extension the package does not type yet.
//!
//! Unreferenced media (a picture inserted then deleted) is not written.

use crate::opc::archive::RELS_XML;
use crate::opc::relationships::parse_relationships;
use engine::{Block, DocumentTree, InlineKind};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub(crate) const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

/// The part an image reference lives in — each owns its own rels scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StoryPart {
    Body,
    /// Header part by its `word/_rels/document.xml.rels` id.
    Header(String),
    Footer(String),
    Footnotes,
    Endnotes,
}

/// One image the package does not carry yet.
#[derive(Debug, Clone)]
pub(crate) struct NewMedia {
    /// Relationship id declared in every referencing part's rels.
    pub rel_id: String,
    /// Archive entry name, `word/media/imageK.<ext>`.
    pub entry_name: String,
    /// Rels target, relative to `word/` (`media/imageK.<ext>`).
    pub target: String,
    pub extension: &'static str,
    /// Content type for the extension's `<Default>` row.
    pub default_content_type: &'static str,
    pub data: Vec<u8>,
    pub parts: BTreeSet<StoryPart>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MediaPlan {
    pub new_media: Vec<NewMedia>,
    /// Model id → package id, for [`rename_image_rel_ids`].
    pub renames: BTreeMap<String, String>,
    /// First `rIdN` number above every id this plan minted and every
    /// `rIdN` of every rels part: the document-rels minting passes that
    /// run after the plan start at or above it, so they never collide.
    pub next_free_rid: u32,
}

impl MediaPlan {
    pub fn is_empty(&self) -> bool {
        self.new_media.is_empty()
    }
}

/// Derive the extension a `word/media/*` part uses from its MIME type, and
/// the content type its `<Default>` row declares. Unrecognised types fall
/// back to `bin` / `application/octet-stream`.
pub(crate) fn media_extension_and_type(content_type: &str) -> (&'static str, &'static str) {
    match content_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => ("png", "image/png"),
        "image/jpeg" | "image/jpg" => ("jpeg", "image/jpeg"),
        "image/gif" => ("gif", "image/gif"),
        "image/bmp" => ("bmp", "image/bmp"),
        "image/tiff" => ("tiff", "image/tiff"),
        "image/x-emf" | "image/emf" => ("emf", "image/x-emf"),
        "image/x-wmf" | "image/wmf" => ("wmf", "image/x-wmf"),
        "image/svg+xml" => ("svg", "image/svg+xml"),
        _ => ("bin", "application/octet-stream"),
    }
}

/// Every engine-keyed image `rel_id` referenced by `blocks`, recursing
/// into table cells (any depth) and text-box stories. Issue #188 — a
/// picture carrying a `media_key` was resolved by the reader against its
/// own part's rels, so its relationship and media part already exist in
/// the package: only pictures whose `rel_id` IS their media key (engine-
/// inserted, or a pre-#188 snapshot) are candidates.
fn collect_image_refs(blocks: &[Block], out: &mut BTreeSet<String>) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                for io in &p.inline_objects {
                    match &io.kind {
                        InlineKind::Image {
                            rel_id,
                            media_key: None,
                            ..
                        } if !rel_id.is_empty() => {
                            out.insert(rel_id.clone());
                        }
                        InlineKind::TextBox { story, .. } => collect_image_refs(&story.body, out),
                        _ => {}
                    }
                }
            }
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_image_refs(&cell.blocks, out);
                    }
                }
            }
        }
    }
}

/// `(story part, image ids it references)` for every part the writer can
/// emit: the body, each referenced header / footer, both note parts.
fn image_refs_by_part(doc: &DocumentTree) -> Vec<(StoryPart, BTreeSet<String>)> {
    let mut out = Vec::new();
    let body: Vec<Block> = doc.blocks.iter().cloned().collect();
    let mut ids = BTreeSet::new();
    collect_image_refs(&body, &mut ids);
    out.push((StoryPart::Body, ids));
    doc.for_each_referenced_story(&mut |header, rid, blocks| {
        let mut ids = BTreeSet::new();
        collect_image_refs(blocks, &mut ids);
        let part = if header {
            StoryPart::Header(rid.to_string())
        } else {
            StoryPart::Footer(rid.to_string())
        };
        out.push((part, ids));
    });
    for (part, stories) in [
        (StoryPart::Footnotes, &doc.footnote_stories),
        (StoryPart::Endnotes, &doc.endnote_stories),
    ] {
        let mut ids = BTreeSet::new();
        for story in stories.values() {
            collect_image_refs(&story.body, &mut ids);
        }
        out.push((part, ids));
    }
    out
}

/// Numeric suffix of an `rIdN` id.
fn rid_number(id: &str) -> Option<u32> {
    id.strip_prefix("rId")?.parse().ok()
}

/// Numeric suffix of a `word/media/imageK.<ext>` entry name.
fn media_index(entry: &str) -> Option<u32> {
    let base = entry.strip_prefix("word/media/image")?;
    let (digits, _) = base.split_once('.')?;
    digits.parse().ok()
}

/// Plan the OPC additions for every image in `doc` the package `entries`
/// (the source archive's non-`document.xml` entries) does not carry.
pub(crate) fn plan_new_media(entries: &[(String, Vec<u8>)], doc: &DocumentTree) -> MediaPlan {
    let doc_rel_ids: HashSet<String> = entries
        .iter()
        .find(|(n, _)| n == RELS_XML)
        .and_then(|(_, b)| parse_relationships(b).ok())
        .map(|r| r.items.into_iter().map(|i| i.id).collect())
        .unwrap_or_default();

    /* model id → every part referencing it. */
    let mut referencing: BTreeMap<String, BTreeSet<StoryPart>> = BTreeMap::new();
    for (part, ids) in image_refs_by_part(doc) {
        for id in ids {
            if doc.media.contains_key(&id) && !doc_rel_ids.contains(&id) {
                referencing.entry(id).or_default().insert(part.clone());
            }
        }
    }

    /* Every id already declared by ANY rels part, and the highest rIdN. */
    let mut taken_ids: HashSet<String> = HashSet::new();
    for (name, bytes) in entries {
        if name.ends_with(".rels")
            && let Ok(rels) = parse_relationships(bytes)
        {
            taken_ids.extend(rels.items.into_iter().map(|i| i.id));
        }
    }
    let mut next_rid = taken_ids
        .iter()
        .filter_map(|id| rid_number(id))
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let entry_names: HashSet<String> = entries
        .iter()
        .map(|(n, _)| n.to_ascii_lowercase())
        .collect();
    let mut next_media = entries
        .iter()
        .filter_map(|(n, _)| media_index(n))
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    let mut plan = MediaPlan::default();
    for (model_id, parts) in referencing {
        let Some(blob) = doc.media.get(&model_id) else {
            continue;
        };
        let rel_id = loop {
            let candidate = format!("rId{next_rid}");
            next_rid = next_rid.saturating_add(1);
            if !taken_ids.contains(&candidate) {
                break candidate;
            }
        };
        let (extension, default_content_type) = media_extension_and_type(&blob.content_type);
        let entry_name = loop {
            let candidate = format!("word/media/image{next_media}.{extension}");
            next_media = next_media.saturating_add(1);
            if !entry_names.contains(&candidate.to_ascii_lowercase()) {
                break candidate;
            }
        };
        let target = entry_name
            .strip_prefix("word/")
            .unwrap_or(&entry_name)
            .to_string();
        if rel_id != model_id {
            plan.renames.insert(model_id.clone(), rel_id.clone());
        }
        plan.new_media.push(NewMedia {
            rel_id,
            entry_name,
            target,
            extension,
            default_content_type,
            data: blob.data.clone(),
            parts,
        });
    }
    plan.next_free_rid = next_rid;
    plan
}

fn rename_in_blocks(blocks: &mut [Block], renames: &BTreeMap<String, String>) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                for io in &mut p.inline_objects {
                    match &mut io.kind {
                        InlineKind::Image { rel_id, .. } => {
                            if let Some(new) = renames.get(rel_id.as_str()) {
                                *rel_id = new.clone();
                            }
                        }
                        InlineKind::TextBox { story, .. } => {
                            rename_in_blocks(&mut story.body, renames)
                        }
                        _ => {}
                    }
                }
            }
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        rename_in_blocks(&mut cell.blocks, renames);
                    }
                }
            }
        }
    }
}

/// A copy of `doc` whose image references (and `media` keys) use the
/// package ids of `renames`. Only the write path sees the copy; the live
/// document keeps its engine ids.
pub(crate) fn rename_image_rel_ids(
    doc: &DocumentTree,
    renames: &BTreeMap<String, String>,
) -> DocumentTree {
    let mut out = doc.clone();
    let mut body: Vec<Block> = out.blocks.iter().cloned().collect();
    rename_in_blocks(&mut body, renames);
    out.blocks = body.into_iter().collect();
    for blocks in out.headers.values_mut().chain(out.footers.values_mut()) {
        rename_in_blocks(blocks, renames);
    }
    for story in out
        .footnote_stories
        .values_mut()
        .chain(out.endnote_stories.values_mut())
    {
        rename_in_blocks(&mut story.body, renames);
    }
    for (old, new) in renames {
        if let Some(blob) = out.media.remove(old) {
            out.media.insert(new.clone(), blob);
        }
    }
    out
}

/// Additive-or-noop splice of a `<Default Extension>` row into an existing
/// `[Content_Types].xml`: skipped when the extension already has a default
/// (compared case-insensitively, as OPC requires). The source rows stay
/// byte-identical.
pub(crate) fn inject_content_type_default(
    xml: &str,
    extension: &str,
    content_type: &str,
) -> Option<String> {
    let declared = crate::opc::content_types::parse_content_types(xml.as_bytes())
        .map(|ct| {
            ct.defaults
                .iter()
                .any(|d| d.extension.eq_ignore_ascii_case(extension))
        })
        .unwrap_or(false);
    if declared {
        return None;
    }
    let close_idx = xml.find("</Types>")?;
    let mut out = String::with_capacity(xml.len() + 96);
    out.push_str(&xml[..close_idx]);
    out.push_str(&format!(
        "<Default Extension=\"{extension}\" ContentType=\"{content_type}\"/>"
    ));
    out.push_str(&xml[close_idx..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ImageBlob;

    const RELS_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">";

    fn rels(rows: &[(&str, &str)]) -> Vec<u8> {
        let mut s = RELS_HEAD.to_string();
        for (id, target) in rows {
            s.push_str(&format!(
                "<Relationship Id=\"{id}\" Type=\"{IMAGE_REL_TYPE}\" Target=\"{target}\"/>"
            ));
        }
        s.push_str("</Relationships>");
        s.into_bytes()
    }

    fn doc_with_images(ids: &[&str]) -> DocumentTree {
        let mut doc = DocumentTree::from_paragraphs(["pic".to_string()]);
        for id in ids {
            doc.media.insert(
                (*id).to_string(),
                ImageBlob {
                    content_type: "image/png".into(),
                    data: vec![1, 2, 3],
                },
            );
            doc = with_image(&doc, id);
        }
        doc
    }

    fn with_image(doc: &DocumentTree, id: &str) -> DocumentTree {
        let mut d = doc.clone();
        let mut blocks: Vec<Block> = d.blocks.iter().cloned().collect();
        if let Some(Block::Paragraph(p)) = blocks.first_mut() {
            p.inline_objects.push(engine::InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: id.to_string(),
                    width_emu: 10,
                    height_emu: 10,
                    media_key: None,
                },
                anchor: None,
                source_xml: None,
            });
        }
        d.blocks = blocks.into_iter().collect();
        d
    }

    #[test]
    fn plans_ids_above_every_rels_part_and_media_names_above_existing() {
        let entries = vec![
            (
                RELS_XML.to_string(),
                rels(&[("rId3", "media/image1.png"), ("rId7", "media/image2.png")]),
            ),
            (
                "word/_rels/header1.xml.rels".to_string(),
                rels(&[("rId12", "media/image9.png")]),
            ),
            ("word/media/image1.png".to_string(), vec![0]),
            ("word/media/image2.png".to_string(), vec![0]),
            ("word/media/image9.png".to_string(), vec![0]),
        ];
        /* `rId3` is imported (a document rel) — never planned. */
        let doc = doc_with_images(&["rId3", "nge_img_1"]);
        let plan = plan_new_media(&entries, &doc);
        assert_eq!(plan.new_media.len(), 1);
        let m = &plan.new_media[0];
        assert_eq!(m.rel_id, "rId13");
        assert_eq!(m.entry_name, "word/media/image10.png");
        assert_eq!(m.target, "media/image10.png");
        assert_eq!(m.parts, BTreeSet::from([StoryPart::Body]));
        assert_eq!(
            plan.renames.get("nge_img_1").map(String::as_str),
            Some("rId13")
        );
        assert_eq!(plan.next_free_rid, 14);

        let renamed = rename_image_rel_ids(&doc, &plan.renames);
        assert!(renamed.media.contains_key("rId13"));
        assert!(!renamed.media.contains_key("nge_img_1"));
        let mut ids = BTreeSet::new();
        collect_image_refs(
            &renamed.blocks.iter().cloned().collect::<Vec<_>>(),
            &mut ids,
        );
        assert_eq!(
            ids,
            BTreeSet::from(["rId3".to_string(), "rId13".to_string()])
        );
    }

    /// Issue #188 — a picture the reader resolved against its own part's
    /// rels (`media_key` set, media keyed by target path) already has its
    /// relationship and part: never planned, even when its part-local
    /// `rel_id` is absent from `document.xml.rels`.
    #[test]
    fn reader_resolved_pictures_are_not_planned() {
        let entries = vec![
            (RELS_XML.to_string(), rels(&[("rId3", "media/image1.png")])),
            (
                "word/_rels/header1.xml.rels".to_string(),
                rels(&[("rId12", "media/image2.png")]),
            ),
            ("word/media/image2.png".to_string(), vec![0]),
        ];
        let mut doc = DocumentTree::from_paragraphs(["pic".to_string()]);
        doc.media.insert(
            "word/media/image2.png".into(),
            ImageBlob {
                content_type: "image/png".into(),
                data: vec![0],
            },
        );
        let mut blocks: Vec<Block> = doc.blocks.iter().cloned().collect();
        if let Some(Block::Paragraph(p)) = blocks.first_mut() {
            p.inline_objects.push(engine::InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: "rId12".into(),
                    width_emu: 10,
                    height_emu: 10,
                    media_key: Some("word/media/image2.png".into()),
                },
                anchor: None,
                source_xml: None,
            });
        }
        doc.blocks = blocks.into_iter().collect();
        assert!(plan_new_media(&entries, &doc).is_empty());
    }

    #[test]
    fn unreferenced_media_is_not_planned() {
        let mut doc = DocumentTree::from_paragraphs(["x".to_string()]);
        doc.media.insert(
            "nge_img_1".into(),
            ImageBlob {
                content_type: "image/gif".into(),
                data: vec![0],
            },
        );
        assert!(plan_new_media(&[], &doc).is_empty());
    }

    #[test]
    fn content_type_default_is_added_once_case_insensitively() {
        let ct =
            "<Types xmlns=\"x\"><Default Extension=\"PNG\" ContentType=\"image/png\"/></Types>";
        assert_eq!(inject_content_type_default(ct, "png", "image/png"), None);
        let out = inject_content_type_default(ct, "jpeg", "image/jpeg").expect("added");
        assert!(out.starts_with("<Types xmlns=\"x\"><Default Extension=\"PNG\""));
        assert!(out.ends_with("<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/></Types>"));
    }
}
