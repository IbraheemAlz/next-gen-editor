//! Issue #419 — per-child verified reuse inside a regenerated `<w:pPr>`.
//!
//! A paragraph whose recorded `<w:pPr>` no longer describes it (one
//! property changed) used to be regenerated whole from the model: every
//! other child lost its source spelling and its unmodeled attributes
//! (`<w:tab w:leader>`, `<w:top w:space w:shadow>`, a `pct25` `<w:shd>`),
//! `w:jc="right"` came back as `"end"`, and values the model only holds
//! resolved through the style cascade (`<w:spacing>` from docDefaults) were
//! baked into direct formatting. Now the regenerated pPr is the SOURCE
//! element with only the changed children replaced:
//!
//! - the children the writer would emit for the RECORDED model state
//!   (`SourcePPr::props` / `style_id` / `list_item` / mark revisions) and
//!   for the LIVE one are compared child by child (by element name): an
//!   equal pair means the child's meaning did not change, so the source
//!   bytes stay — or its absence, for a value that only came from the
//!   cascade;
//! - a changed child is re-emitted from the live model; an empty one keeps
//!   the source twin's attributes the model does not own (theme bindings
//!   dropped — a stale one would override the new value), `<w:pBdr>` and
//!   `<w:tabs>` splice edge by edge / stop by stop the same way; a child
//!   the live model no longer has is removed, a new one inserted at its
//!   schema rank;
//! - the section marker (`<w:sectPr>`) is always the live one;
//! - whitespace between source children (a pretty-printed part) stays
//!   with the child it precedes.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// One child the writer generates: schema rank, identity key, bytes.
struct Gen {
    rank: i64,
    key: Vec<u8>,
    xml: String,
}

/// One child of a source property element, with the whitespace before it.
struct SrcChild<'a> {
    ws: &'a str,
    xml: &'a str,
    qname: Vec<u8>,
    key: Vec<u8>,
}

/// A source property element, split.
struct SrcElement<'a> {
    /// Bytes before the start tag (the pretty-print whitespace a recorded
    /// `<w:pPr>` carries).
    lead: &'a str,
    /// The start tag as written (`<w:pPr/>` when self-closing).
    open: &'a str,
    self_closing: bool,
    children: Vec<SrcChild<'a>>,
    /// Whitespace before the end tag, and the end tag.
    tail: &'a str,
    close: &'a str,
}

/// How a child is identified: by element name, or (a tab stop) by name +
/// position.
type KeyFn = fn(&[u8], &BytesStart) -> Vec<u8>;

fn by_name(qname: &[u8], _: &BytesStart) -> Vec<u8> {
    qname.to_vec()
}

fn by_name_and_pos(qname: &[u8], e: &BytesStart) -> Vec<u8> {
    let mut key = qname.to_vec();
    key.push(b'@');
    if let Some(pos) = crate::schema::ct_rpr::attr_val(e, b"w:pos") {
        key.extend_from_slice(pos.trim().as_bytes());
    }
    key
}

/// Split `xml` (optional leading whitespace + one element) into its
/// children. `None` for anything else: text or a comment between children,
/// trailing bytes, a malformed part.
fn parse_element(xml: &str, key: KeyFn) -> Option<SrcElement<'_>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut depth = 0usize;
    let mut pos = 0usize;
    let mut open: Option<(usize, usize, bool)> = None;
    let mut children = Vec::new();
    let mut ws_start = 0usize;
    let mut child: Option<(usize, Vec<u8>, Vec<u8>)> = None;
    let mut close: Option<(usize, usize, usize)> = None;
    loop {
        let ev = reader.read_event().ok()?;
        let end = reader.buffer_position() as usize;
        match ev {
            Event::Eof => break,
            /* Whitespace anywhere; text only inside a child. */
            Event::Text(t) if t.iter().all(u8::is_ascii_whitespace) => {}
            /* Nothing but whitespace may follow the element. */
            _ if close.is_some() => return None,
            Event::Text(_) if depth <= 1 => return None,
            Event::Text(_) => {}
            Event::Start(e) => {
                match depth {
                    0 => {
                        open = Some((pos, end, false));
                        ws_start = end;
                    }
                    1 => {
                        let q = e.name().as_ref().to_vec();
                        child = Some((pos, key(&q, &e), q));
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::Empty(e) => match depth {
                0 => {
                    open = Some((pos, end, true));
                    close = Some((end, end, end));
                }
                1 => {
                    let q = e.name().as_ref().to_vec();
                    children.push(SrcChild {
                        ws: xml.get(ws_start..pos)?,
                        xml: xml.get(pos..end)?,
                        key: key(&q, &e),
                        qname: q,
                    });
                    ws_start = end;
                }
                _ => {}
            },
            Event::End(_) => {
                depth = depth.checked_sub(1)?;
                match depth {
                    0 => close = Some((ws_start, pos, end)),
                    1 => {
                        let (start, key, qname) = child.take()?;
                        children.push(SrcChild {
                            ws: xml.get(ws_start..start)?,
                            xml: xml.get(start..end)?,
                            qname,
                            key,
                        });
                        ws_start = end;
                    }
                    _ => {}
                }
            }
            _ if depth <= 1 => return None,
            _ => {}
        }
        pos = end;
    }
    let (o_start, o_end, self_closing) = open?;
    let (t_start, c_start, c_end) = close?;
    Some(SrcElement {
        lead: xml.get(..o_start)?,
        open: xml.get(o_start..o_end)?,
        self_closing,
        children,
        tail: if self_closing {
            ""
        } else {
            xml.get(t_start..c_start)?
        },
        close: if self_closing {
            ""
        } else {
            xml.get(c_start..c_end)?
        },
    })
}

/// The children of one generated element (`<w:pBdr>…</w:pBdr>`) as
/// [`Gen`]s ranked by `rank`.
fn generated_children(xml: &str, key: KeyFn, rank: fn(&SrcChild) -> i64) -> Option<Vec<Gen>> {
    let el = parse_element(xml, key)?;
    Some(
        el.children
            .iter()
            .map(|c| Gen {
                rank: rank(c),
                key: c.key.clone(),
                xml: c.xml.to_string(),
            })
            .collect(),
    )
}

/// The qualified name of a generated child's element.
fn qname_of(xml: &str) -> Vec<u8> {
    crate::schema::grab_bag::fragment_qname(xml.as_bytes()).to_vec()
}

/// Splice one level: the source children in source order, each kept or
/// replaced, removed children dropped, new children inserted before the
/// first source child of a higher rank.
fn splice_children(
    out: &mut String,
    src: &[SrcChild<'_>],
    rec: &[Gen],
    live: &[Gen],
    src_rank: &dyn Fn(&SrcChild<'_>) -> i64,
    changed: &dyn Fn(&SrcChild<'_>, Option<&Gen>, &Gen) -> String,
) {
    let find = |list: &'_ [Gen], key: &[u8]| list.iter().position(|g| g.key == key);
    let mut new: Vec<&Gen> = live
        .iter()
        .filter(|g| {
            !src.iter().any(|c| c.key == g.key)
                && find(rec, &g.key).map(|i| rec[i].xml.as_str()) != Some(g.xml.as_str())
        })
        .collect();
    new.sort_by_key(|g| g.rank);
    let mut next_new = 0usize;
    let mut seen: Vec<&[u8]> = Vec::new();
    for c in src {
        let rank = src_rank(c);
        while next_new < new.len() && new[next_new].rank < rank {
            out.push_str(&new[next_new].xml);
            next_new += 1;
        }
        /* A repeated source child (which the schema does not allow) is
        kept as written; only the first of a key is compared. */
        if seen.contains(&c.key.as_slice()) {
            out.push_str(c.ws);
            out.push_str(c.xml);
            continue;
        }
        seen.push(&c.key);
        let (r, l) = (
            find(rec, &c.key).map(|i| &rec[i]),
            find(live, &c.key).map(|i| &live[i]),
        );
        /* The section marker moves with edits: always the live one. */
        if c.key == b"w:sectPr" {
            if let Some(l) = l {
                out.push_str(c.ws);
                out.push_str(&l.xml);
            }
            continue;
        }
        match (r, l) {
            (None, None) => {
                out.push_str(c.ws);
                out.push_str(c.xml);
            }
            (Some(r), Some(l)) if r.xml == l.xml => {
                out.push_str(c.ws);
                out.push_str(c.xml);
            }
            (r, Some(l)) => {
                out.push_str(c.ws);
                out.push_str(&changed(c, r, l));
            }
            (Some(_), None) => {}
        }
    }
    for g in &new[next_new..] {
        out.push_str(&g.xml);
    }
}

/// Wrap spliced children back into `el`'s start / end tags (a
/// self-closing source element opens when it gains children).
fn rewrap(el: &SrcElement<'_>, children: &str, out: &mut String) {
    out.push_str(el.lead);
    if el.self_closing {
        if children.is_empty() {
            out.push_str(el.open);
            return;
        }
        let open = el.open.trim_end_matches("/>").trim_end();
        let name_end = open
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(open.len());
        out.push_str(open);
        out.push('>');
        out.push_str(children);
        out.push_str("</");
        out.push_str(&open[1..name_end]);
        out.push('>');
        return;
    }
    out.push_str(el.open);
    out.push_str(children);
    out.push_str(el.tail);
    out.push_str(el.close);
}

/// Issue #419 — the regenerated `<w:pPr>` for `source` (a recorded
/// `SourcePPr::xml`, possibly empty: the source paragraph had none),
/// given the children the writer emits for the recorded (`rec`) and live
/// (`live`) model state. `None` when the source cannot be split (the
/// caller regenerates). An empty string when there is nothing to say.
pub(super) fn splice_ppr(
    source: &[u8],
    rec: &[(u16, String)],
    live: &[(u16, String)],
) -> Option<String> {
    let gens = |items: &[(u16, String)]| -> Vec<Gen> {
        items
            .iter()
            .map(|(rank, xml)| Gen {
                rank: i64::from(*rank),
                key: qname_of(xml),
                xml: xml.clone(),
            })
            .collect()
    };
    let (rec, live) = (gens(rec), gens(live));
    let source = std::str::from_utf8(source).ok()?;
    let src_rank = |c: &SrcChild<'_>| i64::from(crate::schema::ct_ppr::ppr_child_rank(&c.qname));
    if source.trim().is_empty() {
        /* No source pPr: only what changed is written. */
        let mut children = String::new();
        splice_children(&mut children, &[], &rec, &live, &src_rank, &|_, _, l| {
            l.xml.clone()
        });
        let mut out = String::new();
        if !children.is_empty() {
            out.push_str("<w:pPr>");
            out.push_str(&children);
            out.push_str("</w:pPr>");
        }
        return Some(out);
    }
    let el = parse_element(source, by_name)?;
    let mut children = String::new();
    splice_children(
        &mut children,
        &el.children,
        &rec,
        &live,
        &src_rank,
        &changed_child,
    );
    let mut out = String::new();
    rewrap(&el, &children, &mut out);
    Some(out)
}

/// Issue #371 — [`splice_ppr`] for a `<w:rPr>` (`source` non-empty): a
/// child whose meaning did not change keeps its source bytes, an
/// unmodeled one (`<w:kern>`, `<w:lang>`) stays, a changed one is
/// re-emitted adopting its source twin by meaning (unowned attributes
/// kept — `adopt_source_rpr_children`).
pub(super) fn splice_rpr(
    source: &str,
    rec: &[(u16, String)],
    live: &[(u16, String)],
) -> Option<String> {
    let gens = |items: &[(u16, String)]| -> Vec<Gen> {
        items
            .iter()
            .map(|(rank, xml)| Gen {
                rank: i64::from(*rank),
                key: qname_of(xml),
                xml: xml.clone(),
            })
            .collect()
    };
    let (rec, live) = (gens(rec), gens(live));
    let el = parse_element(source, by_name)?;
    let src_rank = |c: &SrcChild<'_>| i64::from(crate::schema::ct_rpr::rpr_child_rank(&c.qname));
    let mut children = String::new();
    splice_children(
        &mut children,
        &el.children,
        &rec,
        &live,
        &src_rank,
        &|c, _, l| {
            let mut one = vec![(0u16, l.xml.clone())];
            crate::schema::source_markup::adopt_source_rpr_children(&mut one, c.xml.as_bytes());
            one.swap_remove(0).1
        },
    );
    let mut out = String::new();
    rewrap(&el, &children, &mut out);
    Some(out)
}

/// A changed `<w:pPr>` child: a container splices its own children, an
/// empty element keeps its source twin's unowned attributes.
fn changed_child(c: &SrcChild<'_>, r: Option<&Gen>, l: &Gen) -> String {
    let container = match c.qname.as_slice() {
        b"w:pBdr" => Some((by_name as KeyFn, edge_rank as fn(&SrcChild) -> i64)),
        b"w:tabs" => Some((by_name_and_pos as KeyFn, tab_rank as fn(&SrcChild) -> i64)),
        _ => None,
    };
    if let Some((key, rank)) = container
        && let Some(spliced) = splice_container(c, r, l, key, rank)
    {
        return spliced;
    }
    carry_attrs(c, &l.xml).unwrap_or_else(|| l.xml.clone())
}

/// CT_PBdr sequence rank of an edge.
fn edge_rank(c: &SrcChild<'_>) -> i64 {
    match c.qname.as_slice() {
        b"w:top" => 0,
        b"w:left" | b"w:start" => 1,
        b"w:bottom" => 2,
        b"w:right" | b"w:end" => 3,
        b"w:between" => 4,
        b"w:bar" => 5,
        _ => 6,
    }
}

/// A tab stop ranks by its position.
fn tab_rank(c: &SrcChild<'_>) -> i64 {
    let key = &c.key;
    let pos = key
        .iter()
        .position(|&b| b == b'@')
        .and_then(|i| std::str::from_utf8(&key[i + 1..]).ok())
        .and_then(|v| v.parse::<i64>().ok());
    pos.unwrap_or(i64::MAX)
}

fn splice_container(
    c: &SrcChild<'_>,
    r: Option<&Gen>,
    l: &Gen,
    key: KeyFn,
    rank: fn(&SrcChild) -> i64,
) -> Option<String> {
    let el = parse_element(c.xml, key)?;
    let live = generated_children(&l.xml, key, rank)?;
    let rec = match r {
        Some(r) => generated_children(&r.xml, key, rank)?,
        None => Vec::new(),
    };
    let mut children = String::new();
    splice_children(
        &mut children,
        &el.children,
        &rec,
        &live,
        &|c| rank(c),
        &|c, _, l| carry_attrs(c, &l.xml).unwrap_or_else(|| l.xml.clone()),
    );
    if children.is_empty() {
        return Some(l.xml.clone());
    }
    let mut out = String::new();
    rewrap(&el, &children, &mut out);
    Some(out)
}

/// `(qualified name, raw value)` of every attribute of one empty element.
type Attrs = Vec<(Vec<u8>, Vec<u8>)>;

fn empty_element(xml: &str) -> Option<(Vec<u8>, Attrs)> {
    let mut reader = Reader::from_str(xml);
    let Event::Empty(e) = reader.read_event().ok()? else {
        return None;
    };
    let attrs = e
        .attributes()
        .with_checks(false)
        .flatten()
        .map(|a| (a.key.as_ref().to_vec(), a.value.to_vec()))
        .collect();
    let name = e.name().as_ref().to_vec();
    matches!(reader.read_event().ok()?, Event::Eof).then_some((name, attrs))
}

/// A changed empty child `live`, keeping the source twin's attributes the
/// model does not own (see [`carried`]); `w:ind` keeps the source's
/// `w:left` / `w:right` spelling. `None` when either is not one empty
/// element of the same name.
fn carry_attrs(c: &SrcChild<'_>, live: &str) -> Option<String> {
    let (name, mut attrs) = empty_element(live)?;
    let (src_name, src_attrs) = empty_element(c.xml)?;
    if name != src_name {
        return None;
    }
    let has = |list: &Attrs, k: &[u8]| list.iter().any(|(n, _)| n == k);
    if name == b"w:ind" {
        for (logical, physical) in [(&b"w:start"[..], &b"w:left"[..]), (b"w:end", b"w:right")] {
            if has(&src_attrs, physical) && !has(&src_attrs, logical) {
                for (k, _) in attrs.iter_mut().filter(|(k, _)| k == logical) {
                    *k = physical.to_vec();
                }
            }
        }
    }
    let live_attrs = attrs.clone();
    for (k, v) in &src_attrs {
        if !has(&live_attrs, k) && carried(&name, k, &src_attrs, &live_attrs) {
            attrs.push((k.clone(), v.clone()));
        }
    }
    let mut out = String::from("<");
    out.push_str(std::str::from_utf8(&name).ok()?);
    for (k, v) in &attrs {
        out.push(' ');
        out.push_str(std::str::from_utf8(k).ok()?);
        out.push_str("=\"");
        out.push_str(&String::from_utf8_lossy(v).replace('"', "&quot;"));
        out.push('"');
    }
    out.push_str("/>");
    Some(out)
}

/// Whether the source attribute `k` of a changed `elem` survives: a theme
/// binding never does (it would override the new value); `<w:shd>` and a
/// border edge keep everything the model does not own; `<w:spacing>`
/// keeps `w:beforeLines` / `w:beforeAutospacing` only while `w:before`
/// is unchanged (likewise after); `<w:ind>` keeps a `*Chars` value only
/// while its twips twin is unchanged; every other element keeps nothing
/// (its attributes are all owned).
fn carried(elem: &[u8], k: &[u8], src: &Attrs, live: &Attrs) -> bool {
    if k.starts_with(b"w:theme") {
        return false;
    }
    let get = |list: &Attrs, k: &[u8]| list.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let same = |k: &[u8]| get(src, k) == get(live, k);
    match elem {
        b"w:shd" => true,
        b"w:top" | b"w:left" | b"w:start" | b"w:bottom" | b"w:right" | b"w:end" | b"w:between"
        | b"w:bar" => true,
        b"w:spacing" => match k {
            b"w:beforeLines" | b"w:beforeAutospacing" => same(b"w:before"),
            b"w:afterLines" | b"w:afterAutospacing" => same(b"w:after"),
            _ => false,
        },
        b"w:ind" => match k {
            b"w:leftChars" => same(b"w:left") && same(b"w:start"),
            b"w:startChars" => same(b"w:start") && same(b"w:left"),
            b"w:rightChars" => same(b"w:right") && same(b"w:end"),
            b"w:endChars" => same(b"w:end") && same(b"w:right"),
            b"w:firstLineChars" => same(b"w:firstLine"),
            b"w:hangingChars" => same(b"w:hanging"),
            _ => false,
        },
        _ => false,
    }
}

/// Issue #371 — what one paragraph style says, for [`splice_style`]: the
/// modeled children as the writer emits them (`ppr` / `rpr` are the whole
/// regenerated `<w:pPr>` / `<w:rPr>`, empty when there is nothing to say;
/// `ppr_children` / `rpr_children` their children for the per-child
/// splice).
pub(super) struct StyleParts<'a> {
    pub name: Option<&'a str>,
    pub based_on: Option<&'a str>,
    pub next: Option<&'a str>,
    pub ppr: String,
    pub ppr_children: Vec<(u16, String)>,
    pub rpr: String,
    pub rpr_children: Vec<(u16, String)>,
}

/// CT_Style child sequence rank.
fn style_child_rank(c: &SrcChild<'_>) -> i64 {
    style_rank_of(&c.qname)
}

fn style_rank_of(qname: &[u8]) -> i64 {
    const ORDER: [&[u8]; 22] = [
        b"w:name",
        b"w:aliases",
        b"w:basedOn",
        b"w:next",
        b"w:link",
        b"w:autoRedefine",
        b"w:hidden",
        b"w:uiPriority",
        b"w:semiHidden",
        b"w:unhideWhenUsed",
        b"w:qFormat",
        b"w:locked",
        b"w:personal",
        b"w:personalCompose",
        b"w:personalReply",
        b"w:rsid",
        b"w:pPr",
        b"w:rPr",
        b"w:tblPr",
        b"w:trPr",
        b"w:tcPr",
        b"w:tblStylePr",
    ];
    ORDER
        .iter()
        .position(|n| *n == qname)
        .map_or(ORDER.len() as i64, |i| i as i64)
}

/// The modeled children of a style as [`Gen`]s.
fn style_gens(p: &StyleParts<'_>) -> Vec<Gen> {
    let mut out = Vec::new();
    for (q, v) in [
        ("w:name", p.name),
        ("w:basedOn", p.based_on),
        ("w:next", p.next),
    ] {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            let mut xml = format!("<{q} w:val=\"");
            super::push_escaped_attr(v, &mut xml);
            xml.push_str("\"/>");
            out.push(Gen {
                rank: style_rank_of(q.as_bytes()),
                key: q.as_bytes().to_vec(),
                xml,
            });
        }
    }
    for (q, xml) in [("w:pPr", &p.ppr), ("w:rPr", &p.rpr)] {
        if !xml.is_empty() {
            out.push(Gen {
                rank: style_rank_of(q.as_bytes()),
                key: q.as_bytes().to_vec(),
                xml: xml.clone(),
            });
        }
    }
    out
}

/// Issue #371 — one `<w:style>` element of a source `styles.xml`
/// (`source`) re-written for an edited paragraph style: every child whose
/// meaning did not change keeps its source bytes (`<w:uiPriority>`,
/// `<w:qFormat>`, `<w:rsid>`, unmodeled pPr / rPr children, the start
/// tag's `w:default` / `w:customStyle`); a changed `<w:name>` /
/// `<w:basedOn>` / `<w:next>` is re-emitted, a changed `<w:pPr>` spliced
/// child by child ([`splice_ppr`]), a changed `<w:rPr>` the same way
/// ([`splice_rpr`]). `None` when the element cannot be split.
pub(super) fn splice_style(
    source: &str,
    rec: &StyleParts<'_>,
    live: &StyleParts<'_>,
) -> Option<String> {
    let el = parse_element(source, by_name)?;
    if el.self_closing {
        return None;
    }
    let (rec_gens, live_gens) = (style_gens(rec), style_gens(live));
    let mut children = String::new();
    splice_children(
        &mut children,
        &el.children,
        &rec_gens,
        &live_gens,
        &style_child_rank,
        &|c, _, l| match c.qname.as_slice() {
            b"w:pPr" => splice_ppr(c.xml.as_bytes(), &rec.ppr_children, &live.ppr_children)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| l.xml.clone()),
            b"w:rPr" => splice_rpr(c.xml, &rec.rpr_children, &live.rpr_children)
                .unwrap_or_else(|| l.xml.clone()),
            _ => l.xml.clone(),
        },
    );
    let mut out = String::new();
    rewrap(&el, &children, &mut out);
    Some(out)
}
