//! Issue #282 — splice comment anchors into a *clean* paragraph's source
//! bytes.
//!
//! An untouched paragraph is written from its source bytes, so a comment
//! added to it (or reaching into it) has nowhere to go: its
//! `<w:commentRangeStart/>` / `<w:commentRangeEnd/>` and reference run
//! must be inserted INTO those bytes — at the right text offsets, splitting
//! a run where the comment starts mid-run — without respelling anything
//! else. The regenerate path already knows how to place anchors (the #243
//! planner, [`super::comment_anchors`]); this module turns its output into
//! a pure insertion on the source:
//!
//! - `r0` — the paragraph regenerated with no anchor work (what the
//!   regenerate path writes for the source as it stands),
//! - `r1` — the same with the missing tree endpoints synthesized,
//! - `r1` is `r0` plus insertions; [`transplant`] re-applies exactly
//!   those insertions to the source, mapping each insertion point
//!   through an alignment of `r0` against the source (they differ only
//!   where regeneration is not byte-faithful: a dropped `<w:smartTag>`,
//!   pretty-print whitespace, …).
//!
//! Both diffs run over XML *tokens* ([`tokenize`]): a tag, comment,
//! processing instruction, CDATA section or entity reference is one
//! token, character data is one token per character. A tag can only
//! match an identical tag, so an insertion point always falls between two
//! tags or two characters — never inside markup — and coincidental byte
//! matches (`<w:` of one tag against another) cannot scatter an insertion.
//!
//! The caller verifies the result by re-reading it (text, formatting and
//! every anchor offset must come back as intended) and falls back to the
//! regenerated `r1` otherwise — anchors are never lost, only (rarely)
//! respelled.

/// One run of an edit script between token sequences `a` and `b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `n` tokens common to both.
    Equal(usize),
    /// `n` tokens of `a` absent from `b`.
    Delete(usize),
    /// `n` tokens of `b` absent from `a`.
    Insert(usize),
}

/// Edit budget (tokens) for aligning a regeneration against its source
/// bytes. Beyond it the alignment is abandoned (the caller regenerates):
/// the diff is O((N + M) · D) time and O(D²) memory.
pub const MAX_ALIGN_EDITS: usize = 1024;

/// Split XML `s` into token byte ranges: `<!--…-->`, `<![CDATA[…]]>`,
/// `<?…?>`, an end tag and an entity reference `&…;` are one token each;
/// a start / empty-element tag is its name (`<w:t`), then one token per
/// attribute (with the whitespace before it; quoted values may hold `>`)
/// and its close (`>` / `/>`, with the whitespace before it) — so a
/// regeneration that only ADDS an attribute (`xml:space="preserve"` on a
/// split `<w:t>`) is still an insertion; character data is one token per
/// character. An unterminated construct runs to the end.
pub fn tokenize(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let find = |from: usize, pat: &[u8]| {
        b[from..]
            .windows(pat.len())
            .position(|w| w == pat)
            .map_or(b.len(), |p| from + p + pat.len())
    };
    let mut out = Vec::with_capacity(b.len() / 4);
    let mut i = 0;
    while i < b.len() {
        let end = match b[i] {
            b'<' if b[i..].starts_with(b"<!--") => find(i + 4, b"-->"),
            b'<' if b[i..].starts_with(b"<![CDATA[") => find(i + 9, b"]]>"),
            b'<' if b[i..].starts_with(b"<?") => find(i + 2, b"?>"),
            b'<' if b[i..].starts_with(b"</") || b[i..].starts_with(b"<!") => find(i + 1, b">"),
            b'<' => {
                /* Start tag: the name, each attribute, the close. */
                let mut j = i + 1;
                while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
                    j += 1;
                }
                out.push((i, j));
                loop {
                    let lead = j;
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j >= b.len() {
                        if j > lead {
                            out.push((lead, j));
                        }
                        break;
                    }
                    if b[j] == b'>' || b[j] == b'/' {
                        let close = if b[j] == b'/' { find(j, b">") } else { j + 1 };
                        out.push((lead, close));
                        j = close;
                        break;
                    }
                    /* name = "value" */
                    let mut quote: Option<u8> = None;
                    let mut seen_value = false;
                    while j < b.len() {
                        match (quote, b[j]) {
                            (None, q @ (b'"' | b'\'')) => quote = Some(q),
                            (Some(q), c) if c == q => {
                                quote = None;
                                seen_value = true;
                            }
                            (None, b'>' | b'/') => break,
                            (None, c) if c.is_ascii_whitespace() && seen_value => break,
                            _ => {}
                        }
                        j += 1;
                    }
                    out.push((lead, j));
                }
                i = j;
                continue;
            }
            b'&' => b[i..]
                .iter()
                .position(|&c| c == b';' || c == b'<')
                .map_or(
                    b.len(),
                    |p| if b[i + p] == b';' { i + p + 1 } else { i + p },
                ),
            _ => i + s[i..].chars().next().map_or(1, char::len_utf8),
        };
        out.push((i, end.max(i + 1)));
        i = end.max(i + 1);
    }
    out
}

/// A minimal edit script turning `n` tokens into `m` tokens (`eq(i, j)`:
/// token `i` of the first equals token `j` of the second) — Myers' O(ND)
/// algorithm, common prefix and suffix trimmed first — or `None` when it
/// needs more than `max_d` single-token edits.
pub fn diff_by(
    n: usize,
    m: usize,
    eq: impl Fn(usize, usize) -> bool,
    max_d: usize,
) -> Option<Vec<Op>> {
    let pre = (0..n.min(m)).take_while(|&i| eq(i, i)).count();
    let room = n.min(m) - pre;
    let suf = (0..room).take_while(|&k| eq(n - 1 - k, m - 1 - k)).count();
    let mid = myers(
        n - pre - suf,
        m - pre - suf,
        |i, j| eq(pre + i, pre + j),
        max_d,
    )?;
    let mut ops = Vec::with_capacity(mid.len() + 2);
    push(&mut ops, Op::Equal(pre));
    for op in mid {
        push(&mut ops, op);
    }
    push(&mut ops, Op::Equal(suf));
    Some(ops)
}

/// Append `op`, merging it into a trailing op of the same kind.
fn push(ops: &mut Vec<Op>, op: Op) {
    let n = match op {
        Op::Equal(n) | Op::Delete(n) | Op::Insert(n) => n,
    };
    if n == 0 {
        return;
    }
    match (ops.last_mut(), op) {
        (Some(Op::Equal(m)), Op::Equal(n))
        | (Some(Op::Delete(m)), Op::Delete(n))
        | (Some(Op::Insert(m)), Op::Insert(n)) => *m += n,
        _ => ops.push(op),
    }
}

/// Myers' greedy forward pass with a per-step trace, then the backtrack.
fn myers(n: usize, m: usize, eq: impl Fn(usize, usize) -> bool, max_d: usize) -> Option<Vec<Op>> {
    let (n, m) = (n as i64, m as i64);
    if n == 0 && m == 0 {
        return Some(Vec::new());
    }
    let limit = (max_d as i64).min(n + m);
    let off = limit + 1;
    let mut v = vec![0i64; (2 * limit + 3) as usize];
    /* `trace[d]` = the furthest-reaching x of every diagonal k in
    `-(d+1) ..= d+1` BEFORE step d ran (what the backtrack of step d
    reads). */
    let mut trace: Vec<Vec<i64>> = Vec::new();
    let mut end = None;
    'search: for d in 0..=limit {
        trace.push(v[(off - d - 1) as usize..=(off + d + 1) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let i = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
                v[i + 1]
            } else {
                v[i - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && eq(x as usize, y as usize) {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                end = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let dmax = end?;
    let (mut x, mut y) = (n, m);
    let mut rev = Vec::new();
    for d in (0..=dmax).rev() {
        let snap = &trace[d as usize];
        let at = |k: i64| snap[(k + d + 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = at(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            rev.push(Op::Equal(1));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            rev.push(if x == prev_x {
                Op::Insert(1)
            } else {
                Op::Delete(1)
            });
        }
        x = prev_x;
        y = prev_y;
    }
    let mut ops = Vec::new();
    for op in rev.into_iter().rev() {
        push(&mut ops, op);
    }
    Some(ops)
}

/// The index in `b` of the gap before token `p` of `a`, through the
/// alignment `ops` (`a` → `b`). The gap follows the token before it when
/// that token is common to both (left attachment), else precedes the
/// token after it; `None` when neither neighbour is common.
pub fn map_gap(ops: &[Op], p: usize) -> Option<usize> {
    if p == 0 {
        return Some(0);
    }
    let (mut ia, mut ib) = (0usize, 0usize);
    let mut right = None;
    for op in ops {
        match *op {
            Op::Equal(n) => {
                if ia < p && p <= ia + n {
                    return Some(ib + (p - ia));
                }
                if right.is_none() && ia <= p && p < ia + n {
                    right = Some(ib + (p - ia));
                }
                ia += n;
                ib += n;
            }
            Op::Delete(n) => ia += n,
            Op::Insert(n) => ib += n,
        }
        if ia > p && right.is_some() {
            break;
        }
    }
    right
}

/// `src` with the insertions that turn `r0` into `r1` applied at the
/// matching places (see the module docs). `None` when `r1` is not `r0`
/// plus insertions, when `r0` cannot be aligned against `src` within
/// [`MAX_ALIGN_EDITS`], or when an insertion point has no common
/// neighbour.
pub fn transplant(src: &str, r0: &str, r1: &str) -> Option<String> {
    let (ts, t0, t1) = (tokenize(src), tokenize(r0), tokenize(r1));
    fn tok<'a>(s: &'a str, t: &[(usize, usize)], i: usize) -> &'a [u8] {
        &s.as_bytes()[t[i].0..t[i].1]
    }
    let grown = t1.len().checked_sub(t0.len())?;
    let script = diff_by(
        t0.len(),
        t1.len(),
        |i, j| tok(r0, &t0, i) == tok(r1, &t1, j),
        grown,
    )?;
    /* (gap in r0, inserted byte range of r1) */
    let mut inserts: Vec<(usize, (usize, usize))> = Vec::new();
    let (mut i0, mut i1) = (0usize, 0usize);
    for op in script {
        match op {
            Op::Equal(n) => {
                i0 += n;
                i1 += n;
            }
            Op::Insert(n) => {
                inserts.push((i0, (t1[i1].0, t1[i1 + n - 1].1)));
                i1 += n;
            }
            Op::Delete(_) => return None,
        }
    }
    let align = if r0 == src {
        vec![Op::Equal(ts.len())]
    } else {
        diff_by(
            t0.len(),
            ts.len(),
            |i, j| tok(r0, &t0, i) == tok(src, &ts, j),
            MAX_ALIGN_EDITS,
        )?
    };
    let byte_at = |gap: usize| ts.get(gap).map_or(src.len(), |t| t.0);
    let mut placed: Vec<(usize, &str)> = Vec::with_capacity(inserts.len());
    for (gap, (lo, hi)) in inserts {
        placed.push((byte_at(map_gap(&align, gap)?), &r1[lo..hi]));
    }
    /* Insertion points are monotone in r0; keep r0 order at one point. */
    placed.sort_by_key(|(at, _)| *at);
    let mut out = String::with_capacity(r1.len().max(src.len()) + 64);
    let mut cursor = 0usize;
    for (at, piece) in placed {
        if at < cursor {
            return None;
        }
        out.push_str(&src[cursor..at]);
        out.push_str(piece);
        cursor = at;
    }
    out.push_str(&src[cursor..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_diff(a: &str, b: &str, max_d: usize) -> Option<Vec<Op>> {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        diff_by(a.len(), b.len(), |i, j| a[i] == b[j], max_d)
    }

    fn apply(a: &[u8], b: &[u8], ops: &[Op]) -> Vec<u8> {
        let (mut ia, mut ib) = (0, 0);
        let mut out = Vec::new();
        for op in ops {
            match *op {
                Op::Equal(n) => {
                    assert_eq!(a[ia..ia + n], b[ib..ib + n]);
                    out.extend_from_slice(&a[ia..ia + n]);
                    ia += n;
                    ib += n;
                }
                Op::Delete(n) => ia += n,
                Op::Insert(n) => {
                    out.extend_from_slice(&b[ib..ib + n]);
                    ib += n;
                }
            }
        }
        assert_eq!(ia, a.len());
        out
    }

    #[test]
    fn diff_is_a_minimal_valid_script() {
        for (a, b, d) in [
            ("abcabba", "cbabac", 5),
            ("", "xyz", 3),
            ("xyz", "", 3),
            ("same", "same", 0),
        ] {
            let ops = bytes_diff(a, b, 64).expect("within budget");
            assert_eq!(
                apply(a.as_bytes(), b.as_bytes(), &ops),
                b.as_bytes(),
                "{a} -> {b}"
            );
            let edits: usize = ops
                .iter()
                .map(|o| match o {
                    Op::Equal(_) => 0,
                    Op::Delete(n) | Op::Insert(n) => *n,
                })
                .sum();
            assert_eq!(edits, d, "{a} -> {b}");
        }
        assert!(
            bytes_diff("aaaa", "bbbb", 7).is_none(),
            "8 edits over budget"
        );
    }

    #[test]
    fn tokenize_splits_start_tags_into_name_attributes_and_close() {
        let s = r#"<w:t a="x>y" b='z'>a&amp;é</w:t><!-- c > d --><?pi x?><w:br/><w:p >"#;
        let toks: Vec<&str> = tokenize(s).iter().map(|&(lo, hi)| &s[lo..hi]).collect();
        assert_eq!(
            toks,
            vec![
                "<w:t",
                r#" a="x>y""#,
                " b='z'",
                ">",
                "a",
                "&amp;",
                "é",
                "</w:t>",
                "<!-- c > d -->",
                "<?pi x?>",
                "<w:br",
                "/>",
                "<w:p",
                " >",
            ]
        );
        /* The tokens tile the input. */
        let mut cursor = 0;
        for (lo, hi) in tokenize(s) {
            assert_eq!(lo, cursor);
            cursor = hi;
        }
        assert_eq!(cursor, s.len());
    }

    #[test]
    fn transplant_carries_an_added_attribute() {
        /* Splitting "alpha beta" before "beta" needs `xml:space` on the
        first piece; the drifted source (a proofErr the regeneration
        dropped) still receives exactly the insertions. */
        let src = r#"<w:p><w:proofErr w:type="spellStart"/><w:r><w:t>alpha beta</w:t></w:r></w:p>"#;
        let r0 = r#"<w:p><w:r><w:t>alpha beta</w:t></w:r></w:p>"#;
        let r1 = r#"<w:p><w:r><w:t xml:space="preserve">alpha </w:t></w:r><w:commentRangeStart w:id="2"/><w:r><w:t>beta</w:t></w:r></w:p>"#;
        assert_eq!(
            transplant(src, r0, r1).as_deref(),
            Some(
                r#"<w:p><w:proofErr w:type="spellStart"/><w:r><w:t xml:space="preserve">alpha </w:t></w:r><w:commentRangeStart w:id="2"/><w:r><w:t>beta</w:t></w:r></w:p>"#
            )
        );
    }

    #[test]
    fn transplant_reapplies_insertions_on_drifted_source() {
        /* The regeneration lost a smartTag wrapper the source has; the
        anchors still land around "13 km" — the start ahead of the tag,
        the end inside it (each after its left neighbour). */
        let src = r#"<w:p><w:r><w:t xml:space="preserve">by </w:t></w:r><w:smartTag w:element="m"><w:r><w:t>13 km</w:t></w:r></w:smartTag></w:p>"#;
        let r0 = r#"<w:p><w:r><w:t xml:space="preserve">by </w:t></w:r><w:r><w:t>13 km</w:t></w:r></w:p>"#;
        let r1 = r#"<w:p><w:r><w:t xml:space="preserve">by </w:t></w:r><w:commentRangeStart w:id="1"/><w:r><w:t>13 km</w:t></w:r><w:commentRangeEnd w:id="1"/></w:p>"#;
        let out = transplant(src, r0, r1).expect("transplanted");
        assert_eq!(
            out,
            r#"<w:p><w:r><w:t xml:space="preserve">by </w:t></w:r><w:commentRangeStart w:id="1"/><w:smartTag w:element="m"><w:r><w:t>13 km</w:t></w:r><w:commentRangeEnd w:id="1"/></w:smartTag></w:p>"#
        );
        /* Not an insertion-only change: refused. */
        assert!(transplant(src, r1, r0).is_none());
        /* Identical regeneration: r1 itself. */
        assert_eq!(transplant(r0, r0, r1).as_deref(), Some(r1));
    }

    #[test]
    fn transplant_splits_a_run_mid_text() {
        /* Pretty-printed source; the anchor splits the run after "ab". */
        let src = "<w:p>\n  <w:r>\n    <w:t>abcd</w:t>\n  </w:r>\n</w:p>";
        let r0 = "<w:p><w:r><w:t>abcd</w:t></w:r></w:p>";
        let r1 = r#"<w:p><w:r><w:t>ab</w:t></w:r><w:commentRangeStart w:id="7"/><w:r><w:t>cd</w:t></w:r></w:p>"#;
        let out = transplant(src, r0, r1).expect("transplanted");
        assert_eq!(
            out,
            "<w:p>\n  <w:r>\n    <w:t>ab</w:t></w:r><w:commentRangeStart w:id=\"7\"/><w:r><w:t>cd</w:t>\n  </w:r>\n</w:p>"
        );
    }

    #[test]
    fn map_gap_prefers_the_left_neighbour() {
        /* a = "XY", b = "XZY": the gap between X and Y follows X. */
        let ops = bytes_diff("XY", "XZY", 4).unwrap();
        assert_eq!(map_gap(&ops, 1), Some(1));
        assert_eq!(map_gap(&ops, 2), Some(3));
        assert_eq!(map_gap(&ops, 0), Some(0));
        /* a = "XQY", b = "XY": the gap after the deleted Q precedes Y. */
        let ops = bytes_diff("XQY", "XY", 4).unwrap();
        assert_eq!(map_gap(&ops, 2), Some(1));
    }
}
