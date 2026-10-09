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

/// Issue #282 — what [`patch_comments_xml`] did to a source
/// `word/comments.xml`.
#[derive(Debug, Clone, Default)]
pub struct CommentsPatch {
    /// The part, rewritten.
    pub bytes: Vec<u8>,
    /// `comment_id → w14:paraId` minted for every appended comment (the
    /// `commentsExtended.xml` builder keys its rows on it).
    pub minted: HashMap<u32, String>,
    /// Every `w14:paraId` of the removed comments' paragraphs — their rows
    /// in `commentsExtended.xml` / `commentsIds.xml` go too.
    pub removed_para_ids: std::collections::HashSet<String>,
}

/// One `<w:comment>` element of a source part.
struct SourceComment {
    id: Option<u32>,
    /// Byte span of the whole element.
    start: usize,
    end: usize,
    para_ids: Vec<String>,
}

/// The root start tag's `>` (or `/>`) offset, whether the root is
/// self-closing, where `</w:comments>` starts, the comments, every
/// `w14:paraId` / `w14:textId` value, and whether the root binds `w14`.
struct CommentsScan {
    root_open_end: usize,
    root_empty: bool,
    root_close: Option<usize>,
    comments: Vec<SourceComment>,
    ids_in_use: std::collections::HashSet<String>,
    binds_w14: bool,
}

fn scan_comments_part(xml: &[u8]) -> Option<CommentsScan> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut scan = CommentsScan {
        root_open_end: 0,
        root_empty: false,
        root_close: None,
        comments: Vec::new(),
        ids_in_use: std::collections::HashSet::new(),
        binds_w14: false,
    };
    let mut depth = 0usize;
    let mut open: Option<SourceComment> = None;
    let mut prev = 0usize;
    loop {
        let event = reader.read_event_into(&mut buf).ok()?;
        let pos = reader.buffer_position() as usize;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(event, Event::Empty(_));
                if depth == 0 {
                    if e.name().as_ref() != b"w:comments" {
                        return None;
                    }
                    scan.root_open_end = pos - if empty { 2 } else { 1 };
                    scan.root_empty = empty;
                    scan.binds_w14 = e
                        .attributes()
                        .flatten()
                        .any(|a| a.key.as_ref() == b"xmlns:w14");
                } else if depth == 1 && e.name().as_ref() == b"w:comment" {
                    let c = SourceComment {
                        id: e
                            .attributes()
                            .flatten()
                            .find(|a| a.key.as_ref() == b"w:id")
                            .and_then(|a| std::str::from_utf8(&a.value).ok()?.trim().parse().ok()),
                        start: prev,
                        end: pos,
                        para_ids: Vec::new(),
                    };
                    if empty {
                        scan.comments.push(c);
                    } else {
                        open = Some(c);
                    }
                }
                for a in e.attributes().flatten() {
                    if matches!(a.key.as_ref(), b"w14:paraId" | b"w14:textId")
                        && let Ok(v) = std::str::from_utf8(&a.value)
                    {
                        scan.ids_in_use.insert(v.to_string());
                        if a.key.as_ref() == b"w14:paraId"
                            && let Some(c) = open.as_mut()
                        {
                            c.para_ids.push(v.to_string());
                        }
                    }
                }
                if !empty {
                    depth += 1;
                }
            }
            Event::End(e) => {
                depth = depth.checked_sub(1)?;
                if depth == 1 && e.name().as_ref() == b"w:comment" {
                    if let Some(mut c) = open.take() {
                        c.end = pos;
                        scan.comments.push(c);
                    }
                } else if depth == 0 {
                    scan.root_close = Some(prev);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        prev = pos;
        buf.clear();
    }
    (scan.root_open_end > 0).then_some(scan)
}

/// Issue #282 — bring a SOURCE `word/comments.xml` in line with the tree,
/// as an edit of its bytes (every untouched comment keeps its rich body
/// and attributes byte for byte): the `<w:comment>` of every `deleted`
/// (tombstoned, see `engine::DocumentTree::deleted_comments`) id is
/// removed, and every comment in `defs` the part does not carry — added
/// in the editor — is appended before `</w:comments>` with a minted
/// `w14:paraId` (declaring `xmlns:w14` on the root when the part never
/// did). `None` when nothing changes (or the part does not parse; it then
/// rides the passthrough untouched).
pub fn patch_comments_xml(
    src: &[u8],
    defs: &HashMap<u32, CommentDef>,
    deleted: &[u32],
) -> Option<CommentsPatch> {
    let scan = scan_comments_part(src)?;
    let present: std::collections::HashSet<u32> =
        scan.comments.iter().filter_map(|c| c.id).collect();
    let doomed: Vec<&SourceComment> = scan
        .comments
        .iter()
        .filter(|c| {
            c.id.is_some_and(|id| deleted.contains(&id) && !defs.contains_key(&id))
        })
        .collect();
    let mut added: Vec<(&u32, &CommentDef)> = defs
        .iter()
        .filter(|(id, _)| !present.contains(id))
        .collect();
    if doomed.is_empty() && added.is_empty() {
        return None;
    }
    added.sort_by_key(|(id, _)| **id);

    /* Mint paraIds / textIds no element of the part uses (8 hex digits,
    below 0x80000000 as Word requires, never 0). */
    let mut in_use = scan.ids_in_use.clone();
    let mut counter: u32 = 1;
    let mut mint = || loop {
        let v = format!("{counter:08X}");
        counter = counter.saturating_add(1);
        if in_use.insert(v.clone()) {
            return v;
        }
    };
    let mut minted = HashMap::new();
    let mut appended = String::new();
    for (id, def) in &added {
        appended.push_str("<w:comment w:id=\"");
        appended.push_str(&id.to_string());
        appended.push_str("\" w:author=\"");
        escape_attr(&mut appended, &def.author);
        if !def.date.is_empty() {
            appended.push_str("\" w:date=\"");
            escape_attr(&mut appended, &def.date);
        }
        appended.push_str("\">");
        for (pi, body) in def
            .paragraphs
            .iter()
            .map(String::as_str)
            .chain(def.paragraphs.is_empty().then_some(""))
            .enumerate()
        {
            let para_id = mint();
            let text_id = mint();
            if pi == 0 {
                minted.insert(**id, para_id.clone());
            }
            appended.push_str("<w:p w14:paraId=\"");
            appended.push_str(&para_id);
            appended.push_str("\" w14:textId=\"");
            appended.push_str(&text_id);
            appended.push_str("\"><w:r><w:t xml:space=\"preserve\">");
            escape_text(&mut appended, body);
            appended.push_str("</w:t></w:r></w:p>");
        }
        appended.push_str("</w:comment>");
    }

    /* Splice: root `xmlns:w14` (when minting), removals, the appendix. */
    let mut cuts: Vec<(usize, usize, String)> = Vec::new();
    if !added.is_empty() && !scan.binds_w14 {
        cuts.push((
            scan.root_open_end,
            scan.root_open_end,
            " xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\"".to_string(),
        ));
    }
    let mut removed_para_ids = std::collections::HashSet::new();
    for c in &doomed {
        cuts.push((c.start, c.end, String::new()));
        removed_para_ids.extend(c.para_ids.iter().cloned());
    }
    if !added.is_empty() {
        if scan.root_empty {
            /* `<w:comments …/>` → `<w:comments …>…</w:comments>`. */
            cuts.push((
                scan.root_open_end,
                scan.root_open_end + 1,
                format!(">{appended}</w:comments"),
            ));
        } else {
            let at = scan.root_close?;
            cuts.push((at, at, appended));
        }
    }
    cuts.sort_by_key(|(lo, hi, _)| (*lo, *hi));
    let mut out = Vec::with_capacity(src.len() + 256);
    let mut cursor = 0usize;
    for (lo, hi, with) in cuts {
        if lo < cursor {
            return None;
        }
        out.extend_from_slice(&src[cursor..lo]);
        out.extend_from_slice(with.as_bytes());
        cursor = hi;
    }
    out.extend_from_slice(&src[cursor..]);
    Some(CommentsPatch {
        bytes: out,
        minted,
        removed_para_ids,
    })
}

/// Issue #282 — remove from a comments side part every row element named
/// `row` (`w15:commentEx`, `w16cid:commentId`, `w16cex:commentExtensible`)
/// whose `key` attribute is in `values`; also returns the `collect`
/// attribute of every removed row (a `commentsIds.xml` row's
/// `w16cid:durableId` keys `commentsExtensible.xml`). `None` when no row
/// matches or the part does not parse.
pub fn remove_comment_rows(
    xml: &[u8],
    row: &[u8],
    key: &[u8],
    values: &std::collections::HashSet<String>,
    collect: Option<&[u8]>,
) -> Option<(Vec<u8>, std::collections::HashSet<String>)> {
    if values.is_empty() {
        return None;
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut collected = std::collections::HashSet::new();
    let mut open: Option<(usize, u32)> = None;
    let mut prev = 0usize;
    loop {
        let event = reader.read_event_into(&mut buf).ok()?;
        let pos = reader.buffer_position() as usize;
        if let Some((start, depth)) = open.as_mut() {
            match &event {
                Event::Start(_) => *depth += 1,
                Event::End(_) if *depth == 0 => {
                    cuts.push((*start, pos));
                    open = None;
                }
                Event::End(_) => *depth -= 1,
                Event::Eof => return None,
                _ => {}
            }
        } else if let Event::Start(e) | Event::Empty(e) = &event
            && e.name().as_ref() == row
        {
            let attr = |name: &[u8]| {
                e.attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == name)
                    .and_then(|a| std::str::from_utf8(&a.value).ok().map(str::to_string))
            };
            if attr(key).is_some_and(|v| values.contains(&v)) {
                if let Some(c) = collect.and_then(attr) {
                    collected.insert(c);
                }
                if matches!(event, Event::Empty(_)) {
                    cuts.push((prev, pos));
                } else {
                    open = Some((prev, 0));
                }
            }
        } else if let Event::Eof = event {
            break;
        }
        prev = pos;
        buf.clear();
    }
    if cuts.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(xml.len());
    let mut cursor = 0usize;
    for (lo, hi) in cuts {
        out.extend_from_slice(&xml[cursor..lo]);
        cursor = hi;
    }
    out.extend_from_slice(&xml[cursor..]);
    Some((out, collected))
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

    fn def(author: &str, text: &str) -> CommentDef {
        CommentDef {
            author: author.into(),
            date: "2026-10-09T00:00:00Z".into(),
            paragraphs: vec![text.into()],
            ..CommentDef::default()
        }
    }

    /// Issue #282 — a comment added in the editor is appended to the
    /// source part (rich bodies of the others untouched), a deleted one's
    /// element is removed; nothing else moves.
    #[test]
    fn patch_comments_xml_appends_and_removes_elements() {
        let src = concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            r#"<w:comment w:id="0" w:author="A" w:initials="A"><w:p><w:r><w:t>keep</w:t></w:r></w:p></w:comment>"#,
            r#"<w:comment w:id="1" w:author="B"><w:p w14:paraId="00000001" xmlns:w14="x"><w:r><w:t>gone</w:t></w:r></w:p></w:comment>"#,
            r#"</w:comments>"#,
        );
        let parsed = parse_comments_xml(src.as_bytes()).unwrap();
        let mut defs = parsed.comments.clone();
        defs.remove(&1);
        defs.insert(2, def("Me", "new & <shiny>"));
        let p = patch_comments_xml(src.as_bytes(), &defs, &[1]).expect("changed");
        let out = String::from_utf8(p.bytes.clone()).unwrap();
        assert_eq!(
            out,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                "\n",
                r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">"#,
                r#"<w:comment w:id="0" w:author="A" w:initials="A"><w:p><w:r><w:t>keep</w:t></w:r></w:p></w:comment>"#,
                r#"<w:comment w:id="2" w:author="Me" w:date="2026-10-09T00:00:00Z"><w:p w14:paraId="00000002" w14:textId="00000003"><w:r><w:t xml:space="preserve">new &amp; &lt;shiny&gt;</w:t></w:r></w:p></w:comment>"#,
                r#"</w:comments>"#,
            )
        );
        assert_eq!(p.minted.get(&2).map(String::as_str), Some("00000002"));
        assert!(p.removed_para_ids.contains("00000001"));
        let back = parse_comments_xml(&p.bytes).unwrap();
        assert_eq!(back.comments.len(), 2);
        assert_eq!(
            back.comments[&2].paragraphs,
            vec!["new & <shiny>".to_string()]
        );
        /* Nothing to do: None. */
        assert!(patch_comments_xml(src.as_bytes(), &parsed.comments, &[]).is_none());
        /* A deleted id the part never had changes nothing either. */
        assert!(patch_comments_xml(src.as_bytes(), &parsed.comments, &[9]).is_none());
    }

    #[test]
    fn remove_comment_rows_drops_matching_rows_only() {
        let xml = concat!(
            r#"<w16cid:commentsIds xmlns:w16cid="x">"#,
            r#"<w16cid:commentId w16cid:paraId="00000001" w16cid:durableId="1A"/>"#,
            r#"<w16cid:commentId w16cid:paraId="00000002" w16cid:durableId="2B"/>"#,
            r#"</w16cid:commentsIds>"#,
        );
        let values: std::collections::HashSet<String> = ["00000002".to_string()].into();
        let (out, durable) = remove_comment_rows(
            xml.as_bytes(),
            b"w16cid:commentId",
            b"w16cid:paraId",
            &values,
            Some(b"w16cid:durableId"),
        )
        .expect("removed");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            concat!(
                r#"<w16cid:commentsIds xmlns:w16cid="x">"#,
                r#"<w16cid:commentId w16cid:paraId="00000001" w16cid:durableId="1A"/>"#,
                r#"</w16cid:commentsIds>"#,
            )
        );
        assert!(durable.contains("2B"));
        let none: std::collections::HashSet<String> = ["FFFFFFFF".to_string()].into();
        assert!(
            remove_comment_rows(
                xml.as_bytes(),
                b"w16cid:commentId",
                b"w16cid:paraId",
                &none,
                None
            )
            .is_none()
        );
    }

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
