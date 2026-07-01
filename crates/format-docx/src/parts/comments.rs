//! `word/comments.xml` — minimal read-only model (Phase 8a).
//!
//! Each `<w:comment w:id="N" w:author="..." w:date="...">` wraps one or
//! more `<w:p>` paragraphs. Phase 8a only surfaces the comment's plain
//! text + author + date metadata for the sidebar UI — rich body
//! formatting and threaded replies ship with the Phase 8c track-changes
//! sprint.

use crate::error::DocxError;
use engine::CommentDef;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;

/// Map from comment `w:id` to its metadata + body.
#[derive(Debug, Clone, Default)]
pub struct CommentDefinitions {
    pub comments: HashMap<u32, CommentDef>,
}

pub fn parse_comments_xml(xml: &[u8]) -> Result<CommentDefinitions, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut out = CommentDefinitions::default();
    let mut buf = Vec::new();

    let mut cur_id: Option<u32> = None;
    let mut cur_author = String::new();
    let mut cur_date = String::new();
    let mut cur_paras: Vec<String> = Vec::new();
    let mut cur_text = String::new();
    let mut first_para_id: Option<String> = None;
    let mut in_text_elt = false;
    let mut in_p = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match e.name().as_ref() {
                b"w:comment" => {
                    cur_id = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"w:id")
                        .and_then(|a| a.unescape_value().ok())
                        .and_then(|v| v.parse().ok());
                    cur_author = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"w:author")
                        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
                        .unwrap_or_default();
                    cur_date = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"w:date")
                        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
                        .unwrap_or_default();
                    cur_paras.clear();
                    first_para_id = None;
                }
                b"w:p" => {
                    in_p = true;
                    cur_text.clear();
                    /* Sprint 9 — capture the first paragraph's `w14:paraId`
                    for commentsExtended.xml resolution. Word emits one
                    `w14:paraId` per `<w:p>` inside `<w:comment>`; we only
                    need the first to map back to `<w15:commentEx>`. */
                    if first_para_id.is_none() {
                        first_para_id = e
                            .attributes()
                            .flatten()
                            .find(|a| a.key.as_ref() == b"w14:paraId")
                            .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()));
                    }
                }
                b"w:t" => in_text_elt = true,
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"w:comment" => {
                    if let Some(id) = cur_id.take() {
                        out.comments.insert(
                            id,
                            CommentDef {
                                author: std::mem::take(&mut cur_author),
                                date: std::mem::take(&mut cur_date),
                                paragraphs: std::mem::take(&mut cur_paras),
                                /* `resolved` + `parent_id` are filled in
                                 * from a second pass over
                                 * `word/commentsExtended.xml`
                                 * (see `parse_comments_extended_xml`). */
                                resolved: false,
                                first_para_id: first_para_id.take(),
                                parent_id: None,
                            },
                        );
                    }
                    cur_paras.clear();
                }
                b"w:p" => {
                    if in_p {
                        cur_paras.push(std::mem::take(&mut cur_text));
                    }
                    in_p = false;
                }
                b"w:t" => in_text_elt = false,
                _ => {}
            },
            Event::Text(t) if in_text_elt => {
                cur_text.push_str(&t.unescape()?);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// One `<w15:commentEx>` row from `word/commentsExtended.xml`.
///
/// Sprint 9 modelled `(paraId, done)`; issue #27 adds the optional
/// `w15:paraIdParent` that threads a reply under its parent comment
/// (the attribute names the parent comment's first-paragraph
/// `w14:paraId`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentExEntry {
    /// `w15:paraId` — first-paragraph id of the comment this row
    /// describes; matched against `CommentDef.first_para_id`.
    pub para_id: String,
    /// `w15:done` — the resolved flag.
    pub done: bool,
    /// Issue #27 — `w15:paraIdParent`, present only on threaded
    /// replies. Names the PARENT comment's first-paragraph paraId.
    pub parent_para_id: Option<String>,
}

/// Sprint 9 — parse `word/commentsExtended.xml` (`w15`-namespace
/// extension). One `<w15:commentEx w15:paraId="…" w15:done="…"/>` per
/// extended-comment entry; we surface [`CommentExEntry`] rows so the
/// archive reader can match each entry's paraId against the
/// `CommentDef.first_para_id` captured from `word/comments.xml`, flip
/// `CommentDef.resolved`, and (issue #27) attach `parent_id` from
/// `w15:paraIdParent`.
///
/// Lenient: missing / unknown attributes are skipped without erroring
/// (Word silently ignores entries it cannot parse, and we follow). The
/// parser is `Result`-typed only because `quick_xml::Reader` returns a
/// `Result` per event; no `unwrap()` in the body.
pub fn parse_comments_extended_xml(xml: &[u8]) -> Result<Vec<CommentExEntry>, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<CommentExEntry> = Vec::new();

    loop {
        let evt = reader.read_event_into(&mut buf)?;
        match evt {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w15:commentEx" => {
                let mut para_id: Option<String> = None;
                let mut done = false;
                let mut parent_para_id: Option<String> = None;
                for a in e.attributes().flatten() {
                    match a.key.as_ref() {
                        b"w15:paraId" => {
                            if let Ok(v) = a.unescape_value() {
                                para_id = Some(v.into_owned());
                            }
                        }
                        b"w15:paraIdParent" => {
                            if let Ok(v) = a.unescape_value() {
                                parent_para_id = Some(v.into_owned());
                            }
                        }
                        b"w15:done" => {
                            if let Ok(v) = a.unescape_value() {
                                /* OOXML boolean: `1` / `true` / `on` => true;
                                everything else (including absent) => false. */
                                done = matches!(
                                    v.as_ref(),
                                    "1" | "true" | "True" | "TRUE" | "on" | "On"
                                );
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(pid) = para_id {
                    out.push(CommentExEntry {
                        para_id: pid,
                        done,
                        parent_para_id,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Sprint 9 — serialize `word/commentsExtended.xml` from the resolved
/// comments in `defs`. Only comments with a captured `first_para_id`
/// produce an entry; engine-minted comments (no paraId yet) round-trip
/// their `resolved` state in-memory only — a known limitation tracked
/// as Core Engine tech-debt.
///
/// Issue #27 — entries whose `CommentDef.parent_id` names another
/// comment additionally carry `w15:paraIdParent` (the parent's
/// paraId, resolved through `first_para_id` or the minted-override
/// map), which is how Word threads replies.
///
/// Returns `Some(bytes)` when at least one entry carries information
/// (a resolved bit or a parent link); `None` when there is nothing to
/// write (the caller should then preserve the original passthrough
/// entry, or omit the part entirely on a fresh document).
pub fn build_comments_extended_xml(
    defs: &std::collections::HashMap<u32, CommentDef>,
) -> Option<Vec<u8>> {
    build_comments_extended_xml_with_overrides(defs, &std::collections::HashMap::new())
}

/// L1.2 (#18) — same as [`build_comments_extended_xml`], but also
/// consults `overrides` (a map of `comment_id → minted_paraId`) when
/// the underlying `CommentDef.first_para_id` is `None`. The override
/// map is populated by [`build_comments_xml`] when it mints fresh
/// paraIds for engine-minted comments, so the resolved bit on those
/// comments now survives a round-trip.
pub fn build_comments_extended_xml_with_overrides(
    defs: &std::collections::HashMap<u32, CommentDef>,
    overrides: &std::collections::HashMap<u32, String>,
) -> Option<Vec<u8>> {
    /* Resolve a comment id → its paraId, consulting the captured
    `first_para_id` first, then the minted-override map. */
    let para_id_of = |id: &u32| -> Option<String> {
        match defs.get(id).and_then(|c| c.first_para_id.as_deref()) {
            Some(s) => Some(s.to_string()),
            None => overrides.get(id).cloned(),
        }
    };
    let mut entries: Vec<(String, bool, Option<String>)> = defs
        .iter()
        .filter_map(|(id, c)| {
            let pid = para_id_of(id)?;
            /* Issue #27 — a reply also names its parent's paraId. A
            parent whose paraId cannot be resolved degrades leniently
            to an un-threaded entry (Word tolerates the omission). */
            let parent_pid = c.parent_id.as_ref().and_then(para_id_of);
            Some((pid, c.resolved, parent_pid))
        })
        .collect();
    if entries
        .iter()
        .all(|(_, done, parent)| !*done && parent.is_none())
    {
        return None;
    }
    /* Stable output — `defs` is a HashMap, so order otherwise depends
    on hash seed; sort by paraId so the writer's output is determinate
    across runs (round-trip-friendly). */
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut s = String::with_capacity(256 + entries.len() * 80);
    s.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w15:commentsEx xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\">",
    );
    for (pid, done, parent_pid) in entries {
        s.push_str("<w15:commentEx w15:paraId=\"");
        escape_attr(&mut s, &pid);
        if let Some(pp) = parent_pid {
            s.push_str("\" w15:paraIdParent=\"");
            escape_attr(&mut s, &pp);
        }
        s.push_str("\" w15:done=\"");
        s.push_str(if done { "1" } else { "0" });
        s.push_str("\"/>");
    }
    s.push_str("</w15:commentsEx>");
    Some(s.into_bytes())
}

/// L1.2 (#18) — synthesize `word/comments.xml` from in-memory
/// `CommentDef`s when the writer needs to regenerate it (engine-minted
/// comments on a fresh document). Mints a unique `w14:paraId` and
/// `w14:textId` for every `<w:p>` inside each `<w:comment>` so the
/// matching `<w15:commentEx>` rows in `commentsExtended.xml` can refer
/// to them.
///
/// Returns the XML bytes plus a `comment_id → first_para_id_minted` map
/// the caller hands to [`build_comments_extended_xml_with_overrides`].
///
/// **Scope.** The Phase-8a reader captures only plain text per
/// paragraph; this writer emits a minimal `<w:r><w:t>` body per
/// paragraph. Existing Word-authored rich comment bodies are NOT
/// regenerated by this path — the writer's gate ensures it only runs
/// when the archive has no `word/comments.xml`, which is the fresh-
/// document case (every existing comment then is engine-minted and has
/// no rich body to preserve).
pub fn build_comments_xml(
    defs: &std::collections::HashMap<u32, CommentDef>,
) -> (Vec<u8>, std::collections::HashMap<u32, String>) {
    let mut entries: Vec<(&u32, &CommentDef)> = defs.iter().collect();
    /* Stable output — sort by comment id. Counter-driven paraIds are
    then deterministic across runs. */
    entries.sort_by_key(|(id, _)| *id);

    let mut out = String::with_capacity(512 + defs.len() * 192);
    out.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
                     xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">",
    );

    let mut minted: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    /* Counter-based mint: 8 uppercase hex per the canonical OOXML
    form. Starts at 1 so the value is never `00000000` (some Word
    readers treat that as "absent"). */
    let mut counter: u32 = 1;

    for (id, def) in entries {
        out.push_str("<w:comment w:id=\"");
        out.push_str(&id.to_string());
        out.push_str("\" w:author=\"");
        escape_attr(&mut out, &def.author);
        out.push_str("\" w:date=\"");
        escape_attr(&mut out, &def.date);
        out.push_str("\">");

        /* Carry an existing first_para_id through if the def already
        has one (defensive — the writer's gate already excludes this
        case, but it keeps the helper safe to call from tests). Word
        rejects a comment with zero `<w:p>`, so when `paragraphs` is
        empty we still emit a single empty body. */
        let n_paras = def.paragraphs.len().max(1);
        let mut first_for_this_comment: Option<String> = None;
        for pi in 0..n_paras {
            let para_id = if pi == 0 {
                def.first_para_id.clone().unwrap_or_else(|| {
                    let id = format!("{counter:08X}");
                    counter = counter.saturating_add(1);
                    id
                })
            } else {
                let id = format!("{counter:08X}");
                counter = counter.saturating_add(1);
                id
            };
            let text_id = format!("{counter:08X}");
            counter = counter.saturating_add(1);
            if pi == 0 {
                first_for_this_comment = Some(para_id.clone());
            }

            let body = def.paragraphs.get(pi).map(|s| s.as_str()).unwrap_or("");

            out.push_str("<w:p w14:paraId=\"");
            out.push_str(&para_id);
            out.push_str("\" w14:textId=\"");
            out.push_str(&text_id);
            out.push_str("\"><w:r><w:t xml:space=\"preserve\">");
            escape_text(&mut out, body);
            out.push_str("</w:t></w:r></w:p>");
        }
        out.push_str("</w:comment>");

        if let Some(fid) = first_for_this_comment {
            minted.insert(*id, fid);
        }
    }
    out.push_str("</w:comments>");
    (out.into_bytes(), minted)
}

fn escape_attr(out: &mut String, src: &str) {
    for ch in src.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
}

fn escape_text(out: &mut String, src: &str) {
    for ch in src.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_parser_picks_paraid_and_done() {
        let xml = b"<?xml version=\"1.0\"?>\
            <w15:commentsEx xmlns:w15=\"x\">\
              <w15:commentEx w15:paraId=\"00000001\" w15:done=\"1\"/>\
              <w15:commentEx w15:paraId=\"00000002\" w15:done=\"0\"/>\
              <w15:commentEx w15:paraId=\"00000003\"/>\
            </w15:commentsEx>";
        let entries = parse_comments_extended_xml(xml).expect("parses");
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            CommentExEntry {
                para_id: "00000001".to_string(),
                done: true,
                parent_para_id: None,
            }
        );
        assert_eq!(
            entries[1],
            CommentExEntry {
                para_id: "00000002".to_string(),
                done: false,
                parent_para_id: None,
            }
        );
        assert_eq!(
            entries[2],
            CommentExEntry {
                para_id: "00000003".to_string(),
                done: false,
                parent_para_id: None,
            }
        );
    }

    /// Issue #27 — `w15:paraIdParent` threads a reply under its parent.
    #[test]
    fn extended_parser_picks_paraid_parent() {
        let xml = b"<?xml version=\"1.0\"?>\
            <w15:commentsEx xmlns:w15=\"x\">\
              <w15:commentEx w15:paraId=\"AAAA0001\" w15:done=\"0\"/>\
              <w15:commentEx w15:paraId=\"BBBB0002\" w15:paraIdParent=\"AAAA0001\" w15:done=\"0\"/>\
            </w15:commentsEx>";
        let entries = parse_comments_extended_xml(xml).expect("parses");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].parent_para_id, None);
        assert_eq!(
            entries[1].parent_para_id.as_deref(),
            Some("AAAA0001"),
            "reply row carries the parent paraId"
        );
    }

    #[test]
    fn extended_writer_skips_when_no_resolved() {
        let mut defs: std::collections::HashMap<u32, CommentDef> = Default::default();
        defs.insert(
            1,
            CommentDef {
                first_para_id: Some("aaaa1111".into()),
                resolved: false,
                ..Default::default()
            },
        );
        assert!(build_comments_extended_xml(&defs).is_none());
    }

    #[test]
    fn extended_writer_emits_done_for_resolved() {
        let mut defs: std::collections::HashMap<u32, CommentDef> = Default::default();
        defs.insert(
            1,
            CommentDef {
                first_para_id: Some("aaaa1111".into()),
                resolved: true,
                ..Default::default()
            },
        );
        defs.insert(
            2,
            CommentDef {
                first_para_id: Some("bbbb2222".into()),
                resolved: false,
                ..Default::default()
            },
        );
        let bytes = build_comments_extended_xml(&defs).expect("some bytes");
        let s = std::str::from_utf8(&bytes).expect("utf-8");
        assert!(s.contains("w15:paraId=\"aaaa1111\""));
        assert!(s.contains("w15:done=\"1\""));
        assert!(s.contains("w15:paraId=\"bbbb2222\""));
        assert!(s.contains("w15:done=\"0\""));
        /* Round-trip: feed our own output back into the parser. */
        let back = parse_comments_extended_xml(&bytes).expect("re-parses");
        let by_pid: std::collections::HashMap<_, _> =
            back.into_iter().map(|e| (e.para_id, e.done)).collect();
        assert_eq!(by_pid.get("aaaa1111"), Some(&true));
        assert_eq!(by_pid.get("bbbb2222"), Some(&false));
    }

    /// Issue #27 — an UN-resolved reply must still force the extended
    /// part out (the parent link is information worth writing), and
    /// its row must carry `w15:paraIdParent`. Round-trips through our
    /// own parser.
    #[test]
    fn extended_writer_emits_paraid_parent_for_reply() {
        let mut defs: std::collections::HashMap<u32, CommentDef> = Default::default();
        defs.insert(
            1,
            CommentDef {
                first_para_id: Some("AAAA0001".into()),
                resolved: false,
                ..Default::default()
            },
        );
        defs.insert(
            2,
            CommentDef {
                first_para_id: Some("BBBB0002".into()),
                resolved: false,
                parent_id: Some(1),
                ..Default::default()
            },
        );
        let bytes = build_comments_extended_xml(&defs)
            .expect("a thread exists — the part must be written even with no resolved bit");
        let s = std::str::from_utf8(&bytes).expect("utf-8");
        assert!(
            s.contains("w15:paraId=\"BBBB0002\" w15:paraIdParent=\"AAAA0001\""),
            "reply row threads under the parent: {s}"
        );
        assert!(
            !s.contains("w15:paraId=\"AAAA0001\" w15:paraIdParent"),
            "top-level row carries no paraIdParent: {s}"
        );
        let back = parse_comments_extended_xml(&bytes).expect("re-parses");
        let reply = back
            .iter()
            .find(|e| e.para_id == "BBBB0002")
            .expect("reply row");
        assert_eq!(reply.parent_para_id.as_deref(), Some("AAAA0001"));
    }

    /// Issue #27 — a reply whose paraId comes from the minted-override
    /// map (engine-minted thread on a fresh document) resolves both its
    /// own and its parent's paraId through the overrides.
    #[test]
    fn extended_writer_resolves_parent_via_override_map() {
        let mut defs: std::collections::HashMap<u32, CommentDef> = Default::default();
        defs.insert(1, CommentDef::default());
        defs.insert(
            2,
            CommentDef {
                parent_id: Some(1),
                ..Default::default()
            },
        );
        let mut overrides: std::collections::HashMap<u32, String> = Default::default();
        overrides.insert(1, "00000001".into());
        overrides.insert(2, "00000003".into());
        let bytes = build_comments_extended_xml_with_overrides(&defs, &overrides).expect("emitted");
        let s = std::str::from_utf8(&bytes).expect("utf-8");
        assert!(
            s.contains("w15:paraId=\"00000003\" w15:paraIdParent=\"00000001\""),
            "override-minted paraIds thread correctly: {s}"
        );
    }
}
