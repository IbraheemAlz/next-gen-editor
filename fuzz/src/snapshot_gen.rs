//! Structure-aware `engine::snapshot` envelope generator for the
//! `snapshot_decode` target (issue #341).
//!
//! A persisted snapshot comes back from IndexedDB at every boot after a
//! crash, so a planted or corrupted row is an input the engine must
//! survive. The envelope is `NGES` + a version byte + named-field
//! MessagePack of a private engine struct, so a generator cannot name its
//! fields; instead it takes the engine's OWN snapshot of a generated
//! session, parses the MessagePack into a small [`Mp`] tree and mutates
//! that tree (hostile integers, NaN floats, truncated / duplicated
//! containers, swapped types, dangling package keys, a wrong magic /
//! version byte, a truncated payload) before re-encoding.
//!
//! Issue #422 — two more rules. A mutation may LIE about a container's
//! length (an array / map / string / binary header rewritten to claim far
//! more than follows — the shape `engine::snapshot::validate_payload` must
//! refuse before `im`'s uncapped preallocation sees it), and no generated
//! envelope exceeds [`MAX_SNAPSHOT_BYTES`]: an over-budget mutation falls
//! back to the unmutated snapshot.
//!
//! Also the container format of the committed seeds
//! (`corpus/snapshot_decode/`, written by `examples/regen-seeds`):
//! [`SEED_MAGIC`] + `u32` LE snapshot length + snapshot bytes + the
//! detached package bytes (the `Command::Recover.package` side).

use crate::command_gen;
use crate::util::pick;
use arbitrary::Unstructured;

/// A MessagePack value, kept lossless enough that re-encoding an
/// unmutated tree reproduces the bytes `rmp_serde` wrote.
#[derive(Debug, Clone, PartialEq)]
pub enum Mp {
    Nil,
    Bool(bool),
    Int(i128),
    F32(f32),
    F64(f64),
    Str(Vec<u8>),
    Bin(Vec<u8>),
    Arr(Vec<Mp>),
    Map(Vec<(Mp, Mp)>),
    Ext(i8, Vec<u8>),
}

const MAX_DEPTH: usize = 256;

/// Issue #422 — the largest envelope [`mutate_snapshot`] returns (the
/// fuzz driver's per-input memory is a small multiple of it).
pub const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn be(&mut self, n: usize) -> Option<u64> {
        Some(
            self.take(n)?
                .iter()
                .fold(0u64, |a, b| (a << 8) | u64::from(*b)),
        )
    }
    fn value(&mut self, depth: usize) -> Option<Mp> {
        if depth > MAX_DEPTH {
            return None;
        }
        let t = *self.take(1)?.first()?;
        Some(match t {
            0x00..=0x7f => Mp::Int(i128::from(t)),
            0xe0..=0xff => Mp::Int(i128::from(t as i8)),
            0x80..=0x8f => self.map(usize::from(t & 0x0f), depth)?,
            0x90..=0x9f => self.arr(usize::from(t & 0x0f), depth)?,
            0xa0..=0xbf => Mp::Str(self.take(usize::from(t & 0x1f))?.to_vec()),
            0xc0 => Mp::Nil,
            0xc2 => Mp::Bool(false),
            0xc3 => Mp::Bool(true),
            0xc4..=0xc6 => {
                let n = self.be(1 << (t - 0xc4))? as usize;
                Mp::Bin(self.take(n)?.to_vec())
            }
            0xc7..=0xc9 => {
                let n = self.be(1 << (t - 0xc7))? as usize;
                let ty = *self.take(1)?.first()? as i8;
                Mp::Ext(ty, self.take(n)?.to_vec())
            }
            0xca => Mp::F32(f32::from_bits(self.be(4)? as u32)),
            0xcb => Mp::F64(f64::from_bits(self.be(8)?)),
            0xcc..=0xcf => Mp::Int(i128::from(self.be(1 << (t - 0xcc))?)),
            0xd0 => Mp::Int(i128::from(self.be(1)? as u8 as i8)),
            0xd1 => Mp::Int(i128::from(self.be(2)? as u16 as i16)),
            0xd2 => Mp::Int(i128::from(self.be(4)? as u32 as i32)),
            0xd3 => Mp::Int(i128::from(self.be(8)? as i64)),
            0xd4..=0xd8 => {
                let ty = *self.take(1)?.first()? as i8;
                Mp::Ext(ty, self.take(1 << (t - 0xd4))?.to_vec())
            }
            0xd9..=0xdb => {
                let n = self.be(1 << (t - 0xd9))? as usize;
                Mp::Str(self.take(n)?.to_vec())
            }
            0xdc => {
                let n = self.be(2)? as usize;
                self.arr(n, depth)?
            }
            0xdd => {
                let n = self.be(4)? as usize;
                self.arr(n, depth)?
            }
            0xde => {
                let n = self.be(2)? as usize;
                self.map(n, depth)?
            }
            0xdf => {
                let n = self.be(4)? as usize;
                self.map(n, depth)?
            }
            _ => return None,
        })
    }
    fn arr(&mut self, n: usize, depth: usize) -> Option<Mp> {
        // Every element is at least one byte: refuse a count the rest of
        // the buffer cannot hold before reserving anything.
        if n > self.b.len() - self.pos {
            return None;
        }
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.value(depth + 1)?);
        }
        Some(Mp::Arr(v))
    }
    fn map(&mut self, n: usize, depth: usize) -> Option<Mp> {
        if n.saturating_mul(2) > self.b.len() - self.pos {
            return None;
        }
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let k = self.value(depth + 1)?;
            let val = self.value(depth + 1)?;
            v.push((k, val));
        }
        Some(Mp::Map(v))
    }
}

impl Mp {
    /// Parse one whole MessagePack value; `None` on trailing bytes or any
    /// malformation.
    pub fn parse(bytes: &[u8]) -> Option<Mp> {
        let mut r = Reader { b: bytes, pos: 0 };
        let v = r.value(0)?;
        (r.pos == bytes.len()).then_some(v)
    }

    /// Encode with the smallest representation of every integer and
    /// length — what `rmp_serde` writes.
    pub fn encode(&self, out: &mut Vec<u8>) {
        fn len_hdr(out: &mut Vec<u8>, n: usize, fix: (u8, usize), w16: u8, w32: u8) {
            if n < fix.1 {
                out.push(fix.0 | n as u8);
            } else if n <= usize::from(u16::MAX) {
                out.push(w16);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            } else {
                out.push(w32);
                out.extend_from_slice(&(n as u32).to_be_bytes());
            }
        }
        match self {
            Mp::Nil => out.push(0xc0),
            Mp::Bool(b) => out.push(if *b { 0xc3 } else { 0xc2 }),
            Mp::Int(i) => {
                let i = *i;
                if (0..=0x7f).contains(&i) {
                    out.push(i as u8);
                } else if (-32..0).contains(&i) {
                    out.push(i as i8 as u8);
                } else if i >= 0 {
                    let u = i.min(i128::from(u64::MAX)) as u64;
                    if u <= u64::from(u8::MAX) {
                        out.extend_from_slice(&[0xcc, u as u8]);
                    } else if u <= u64::from(u16::MAX) {
                        out.push(0xcd);
                        out.extend_from_slice(&(u as u16).to_be_bytes());
                    } else if u <= u64::from(u32::MAX) {
                        out.push(0xce);
                        out.extend_from_slice(&(u as u32).to_be_bytes());
                    } else {
                        out.push(0xcf);
                        out.extend_from_slice(&u.to_be_bytes());
                    }
                } else {
                    let s = i.max(i128::from(i64::MIN)) as i64;
                    if s >= i64::from(i8::MIN) {
                        out.extend_from_slice(&[0xd0, s as i8 as u8]);
                    } else if s >= i64::from(i16::MIN) {
                        out.push(0xd1);
                        out.extend_from_slice(&(s as i16).to_be_bytes());
                    } else if s >= i64::from(i32::MIN) {
                        out.push(0xd2);
                        out.extend_from_slice(&(s as i32).to_be_bytes());
                    } else {
                        out.push(0xd3);
                        out.extend_from_slice(&s.to_be_bytes());
                    }
                }
            }
            Mp::F32(f) => {
                out.push(0xca);
                out.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            Mp::F64(f) => {
                out.push(0xcb);
                out.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            Mp::Str(s) => {
                if s.len() < 32 {
                    out.push(0xa0 | s.len() as u8);
                } else if s.len() <= 0xff {
                    out.extend_from_slice(&[0xd9, s.len() as u8]);
                } else if s.len() <= usize::from(u16::MAX) {
                    out.push(0xda);
                    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
                } else {
                    out.push(0xdb);
                    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
                }
                out.extend_from_slice(s);
            }
            Mp::Bin(b) => {
                if b.len() <= 0xff {
                    out.extend_from_slice(&[0xc4, b.len() as u8]);
                } else if b.len() <= usize::from(u16::MAX) {
                    out.push(0xc5);
                    out.extend_from_slice(&(b.len() as u16).to_be_bytes());
                } else {
                    out.push(0xc6);
                    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
                }
                out.extend_from_slice(b);
            }
            Mp::Arr(v) => {
                len_hdr(out, v.len(), (0x90, 16), 0xdc, 0xdd);
                for e in v {
                    e.encode(out);
                }
            }
            Mp::Map(v) => {
                len_hdr(out, v.len(), (0x80, 16), 0xde, 0xdf);
                for (k, e) in v {
                    k.encode(out);
                    e.encode(out);
                }
            }
            Mp::Ext(ty, d) => {
                out.extend_from_slice(&[0xc7, d.len().min(255) as u8, *ty as u8]);
                out.extend_from_slice(&d[..d.len().min(255)]);
            }
        }
    }

    /// The value of top-level map key `name`.
    fn field_mut(&mut self, name: &str) -> Option<&mut Mp> {
        let Mp::Map(entries) = self else { return None };
        entries
            .iter_mut()
            .find(|(k, _)| matches!(k, Mp::Str(s) if s == name.as_bytes()))
            .map(|(_, v)| v)
    }

    fn string(s: &str) -> Mp {
        Mp::Str(s.as_bytes().to_vec())
    }
}

/// Integers a hand-planted snapshot might carry in place of a position,
/// an index, a length or a revision id.
const HOSTILE_INTS: &[i128] = &[
    0,
    1,
    -1,
    255,
    256,
    65_535,
    65_536,
    i32::MAX as i128,
    i32::MIN as i128,
    u32::MAX as i128,
    i64::MAX as i128,
    i64::MIN as i128,
    u64::MAX as i128,
];

/// The top-level fields of `EngineSnapshotV1`.
const TOP_FIELDS: &[&str] = &[
    "doc_history",
    "undo_cursor",
    "selection",
    "stashed_body_selection",
    "active_story",
    "pending_format",
    "caret_affinity",
    "tracking_changes",
    "review_author",
    "review_date",
    "layout_cfg",
    "document_name",
    "source_package",
    "package_hash",
];

/// Descend to a random node (stopping early at random), returning it.
fn random_node<'a>(u: &mut Unstructured, root: &'a mut Mp) -> &'a mut Mp {
    // Choose the path first (immutable walk), then follow it mutably.
    let mut path: Vec<usize> = Vec::new();
    let mut view: &Mp = root;
    loop {
        let go = u.ratio(5, 6).unwrap_or(false);
        let next = match view {
            Mp::Arr(v) if go && !v.is_empty() => {
                let i = u.choose_index(v.len()).unwrap_or(0);
                Some((i, &v[i]))
            }
            Mp::Map(v) if go && !v.is_empty() => {
                let i = u.choose_index(v.len()).unwrap_or(0);
                Some((i, &v[i].1))
            }
            _ => None,
        };
        let Some((i, child)) = next else { break };
        path.push(i);
        view = child;
    }
    let mut cur = root;
    for i in path {
        cur = match cur {
            Mp::Arr(v) => &mut v[i],
            Mp::Map(v) => &mut v[i].1,
            _ => unreachable!("the path was walked over containers"),
        };
    }
    cur
}

/// One hostile rewrite of `node`.
fn hostile_rewrite(u: &mut Unstructured, node: &mut Mp) {
    match u.int_in_range(0u8..=9).unwrap_or(0) {
        0 => *node = Mp::Nil,
        1 => *node = Mp::Int(*pick(u, HOSTILE_INTS)),
        2 => {
            *node = Mp::F64(*pick(
                u,
                &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -0.0],
            ))
        }
        3 => *node = Mp::F32(*pick(u, &[f32::NAN, f32::INFINITY, -1e30, 0.0])),
        4 => match node {
            Mp::Arr(v) => {
                let keep = u.choose_index(v.len() + 1).unwrap_or(0);
                v.truncate(keep);
            }
            Mp::Map(v) => {
                let keep = u.choose_index(v.len() + 1).unwrap_or(0);
                v.truncate(keep);
            }
            Mp::Str(s) | Mp::Bin(s) => {
                let keep = u.choose_index(s.len() + 1).unwrap_or(0);
                s.truncate(keep);
            }
            _ => *node = Mp::Nil,
        },
        5 => match node {
            Mp::Arr(v) if !v.is_empty() => {
                let i = u.choose_index(v.len()).unwrap_or(0);
                let copy = v[i].clone();
                v.push(copy.clone());
                v.insert(0, copy);
            }
            Mp::Map(v) if !v.is_empty() => {
                let i = u.choose_index(v.len()).unwrap_or(0);
                let copy = v[i].clone();
                v.push(copy); // duplicate key
            }
            _ => {}
        },
        6 => {
            *node = match std::mem::replace(node, Mp::Nil) {
                Mp::Str(s) => Mp::Bin(s),
                Mp::Bin(b) => Mp::Str(b),
                Mp::Arr(v) => Mp::Map(v.into_iter().map(|e| (Mp::Nil, e)).collect()),
                Mp::Map(v) => Mp::Arr(v.into_iter().map(|(_, e)| e).collect()),
                Mp::Int(i) => Mp::Bool(i != 0),
                other => other,
            }
        }
        7 => {
            *node = Mp::Str(vec![b'a'; *pick(u, &[0usize, 1, 31, 32, 300, 70_000])]);
        }
        8 => {
            if let Mp::Int(i) = node {
                *i = i.saturating_add(*pick(u, &[-2i128, -1, 1, 2]));
            } else {
                *node = Mp::Bool(u.ratio(1, 2).unwrap_or(false));
            }
        }
        _ => *node = Mp::Str(b"\xff\xfe not utf-8 \x00".to_vec()),
    }
}

/// Replace `k` integers somewhere in `node`'s subtree with hostile ones
/// — how a position, a range or an id ends up dangling.
fn saturate_ints(u: &mut Unstructured, node: &mut Mp, k: usize) {
    for _ in 0..k {
        let leaf = random_node(u, node);
        if matches!(leaf, Mp::Int(_)) {
            *leaf = Mp::Int(*pick(u, HOSTILE_INTS));
        }
    }
}

/// Keys a Recover caller could present for `package` (the detached
/// source package bytes), valid and not.
fn package_keys(package: &[u8]) -> Vec<String> {
    let mut keys = vec![
        engine::package::package_key(package),
        engine::package::legacy_package_key(package),
        engine::package::legacy_package_key(b"another package"),
        "pkg-0-0000000000000000".to_string(),
        "pkg-".to_string(),
        "pkg-zz-zz".to_string(),
        format!("sha256-{}", "0".repeat(64)),
        "sha256-".to_string(),
        "sha256-abc".to_string(),
        String::new(),
        "unknown-prefix-1234".to_string(),
    ];
    keys.push(format!("sha256-{}", "f".repeat(65)));
    keys
}

/// Byte offsets of every length-carrying header (array, map, string,
/// binary) in the MessagePack `payload`, in document order, with the
/// header's width. Stops at the first malformation; walks iteratively.
pub fn length_headers(payload: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut owed: Vec<u64> = Vec::new();
    let be = |pos: usize, n: usize| -> Option<u64> {
        let b = payload.get(pos..pos.checked_add(n)?)?;
        Some(b.iter().fold(0u64, |a, x| (a << 8) | u64::from(*x)))
    };
    while let Some(&m) = payload.get(pos) {
        let at = pos;
        let (hdr, children, skip): (usize, u64, u64) = match m {
            0x80..=0x8f => (1, 2 * u64::from(m & 0x0f), 0),
            0x90..=0x9f => (1, u64::from(m & 0x0f), 0),
            0xa0..=0xbf => (1, 0, u64::from(m & 0x1f)),
            0xc4 | 0xd9 => (2, 0, be(pos + 1, 1).unwrap_or(u64::MAX)),
            0xc5 | 0xda => (3, 0, be(pos + 1, 2).unwrap_or(u64::MAX)),
            0xc6 | 0xdb => (5, 0, be(pos + 1, 4).unwrap_or(u64::MAX)),
            0xdc => (3, be(pos + 1, 2).unwrap_or(u64::MAX), 0),
            0xdd => (5, be(pos + 1, 4).unwrap_or(u64::MAX), 0),
            0xde => (3, 2 * be(pos + 1, 2).unwrap_or(u64::MAX / 2), 0),
            0xdf => (5, 2 * be(pos + 1, 4).unwrap_or(u64::MAX / 2), 0),
            0xca => (5, 0, 0),
            0xcb => (9, 0, 0),
            0xcc | 0xd0 => (2, 0, 0),
            0xcd | 0xd1 => (3, 0, 0),
            0xce | 0xd2 => (5, 0, 0),
            0xcf | 0xd3 => (9, 0, 0),
            0xd4..=0xd8 => (2 + (1usize << (m - 0xd4)), 0, 0),
            0xc7..=0xc9 => {
                let n = 1usize << (m - 0xc7);
                (2 + n, 0, be(pos + 1, n).unwrap_or(u64::MAX))
            }
            0xc1 => break,
            _ => (1, 0, 0),
        };
        if matches!(m, 0x80..=0xbf | 0xc4..=0xc6 | 0xd9..=0xdf) {
            out.push((at, hdr));
        }
        let Some(next) = (pos + hdr).checked_add(usize::try_from(skip).unwrap_or(usize::MAX))
        else {
            break;
        };
        if next > payload.len() || owed.len() > MAX_DEPTH {
            break;
        }
        pos = next;
        if children > 0 {
            owed.push(children);
            continue;
        }
        while let Some(top) = owed.last_mut() {
            *top -= 1;
            if *top > 0 {
                break;
            }
            owed.pop();
        }
        if owed.is_empty() {
            break;
        }
    }
    out
}

/// Issue #422 — rewrite one length header of `payload` (chosen by `u`) to
/// claim a hostile count in its widest form: `array32` / `map32` /
/// `str32` / `bin32`. What follows the header is left as it was, so the
/// declared length no longer matches the bytes.
fn lie_about_a_length(u: &mut Unstructured, payload: &[u8]) -> Option<Vec<u8>> {
    let headers = length_headers(payload);
    if headers.is_empty() {
        return None;
    }
    let header = headers[u.choose_index(headers.len()).unwrap_or(0)];
    let remaining = (payload.len() - header.0) as u32;
    let claim = *pick(
        u,
        &[
            u32::MAX,
            0x7fff_ffff,
            0x0100_0000,
            65_536,
            remaining.saturating_add(1),
            remaining.saturating_mul(2),
        ],
    );
    Some(lie_at(payload, header, claim))
}

/// `payload` with the length header at `(offset, width)` replaced by the
/// widest header of its kind claiming `claim`.
pub fn lie_at(payload: &[u8], (at, width): (usize, usize), claim: u32) -> Vec<u8> {
    let wide = match payload[at] {
        0x80..=0x8f | 0xde | 0xdf => 0xdf,
        0x90..=0x9f | 0xdc | 0xdd => 0xdd,
        0xa0..=0xbf | 0xd9..=0xdb => 0xdb,
        _ => 0xc6,
    };
    let mut out = payload[..at].to_vec();
    out.push(wide);
    out.extend_from_slice(&claim.to_be_bytes());
    out.extend_from_slice(&payload[at + width..]);
    out
}

/// Mutate the envelope `bytes` (a valid snapshot) with `ops` operations
/// drawn from `u`. `package` is the detached package that would ride with
/// it, if any (used to forge matching and mismatching keys).
pub fn mutate_snapshot(u: &mut Unstructured, bytes: &[u8], package: Option<&[u8]>) -> Vec<u8> {
    let (head, payload) = bytes.split_at(bytes.len().min(5));
    let mut head = head.to_vec();
    let mut tree = Mp::parse(payload);
    let ops = u.int_in_range(0u8..=4).unwrap_or(1);
    let mut payload_override: Option<Vec<u8>> = None;
    let mut lie = false;
    for _ in 0..ops {
        match u.int_in_range(0u8..=10).unwrap_or(0) {
            // Issue #422 — a declared length the bytes cannot back
            // (applied to the final encoding, below).
            10 => lie = true,
            // Header: magic / version byte.
            0 => {
                if head.len() == 5 {
                    match u.int_in_range(0u8..=3).unwrap_or(0) {
                        0 => head[u.choose_index(4).unwrap_or(0)] ^= 0x20,
                        1 => head[4] = *pick(u, &[0u8, 1, 2, 3, 127, 255]),
                        2 => head[4] = 1,
                        _ => head[4] = 2,
                    }
                }
            }
            // Truncated payload.
            1 => {
                let mut b = Vec::new();
                if let Some(t) = &tree {
                    t.encode(&mut b);
                } else {
                    b.extend_from_slice(payload);
                }
                let keep = u.choose_index(b.len() + 1).unwrap_or(0);
                b.truncate(keep);
                payload_override = Some(b);
            }
            // A random node, rewritten.
            2 | 3 => {
                if let Some(t) = tree.as_mut() {
                    let node = random_node(u, t);
                    hostile_rewrite(u, node);
                }
            }
            // A named top-level field: removed or rewritten.
            4 | 5 => {
                if let Some(t) = tree.as_mut() {
                    let name = *pick(u, TOP_FIELDS);
                    if u.ratio(1, 3).unwrap_or(false) {
                        if let Mp::Map(entries) = t {
                            entries
                                .retain(|(k, _)| !matches!(k, Mp::Str(s) if s == name.as_bytes()));
                        }
                    } else if let Some(f) = t.field_mut(name) {
                        hostile_rewrite(u, f);
                    }
                }
            }
            // Dangling ranges / ids: saturate ints inside one field.
            6 => {
                if let Some(t) = tree.as_mut() {
                    let name = *pick(u, TOP_FIELDS);
                    let k = u.int_in_range(1usize..=6).unwrap_or(1);
                    if let Some(f) = t.field_mut(name) {
                        saturate_ints(u, f, k);
                    }
                }
            }
            // The package key: v1 / v2 / mismatched / malformed.
            7 => {
                if let Some(t) = tree.as_mut() {
                    let keys = package_keys(package.unwrap_or(b""));
                    let key = pick(u, &keys).clone();
                    if let Mp::Map(entries) = t {
                        entries.retain(|(k, _)| !matches!(k, Mp::Str(s) if s == b"package_hash"));
                        entries.push((Mp::string("package_hash"), Mp::string(&key)));
                    }
                }
            }
            // An undo window far past the cap, the cursor anywhere.
            8 => {
                if let Some(t) = tree.as_mut() {
                    let copies = *pick(u, &[2usize, 7, 120, 260]);
                    if let Some(Mp::Arr(h)) = t.field_mut("doc_history")
                        && !h.is_empty()
                    {
                        let i = u.choose_index(h.len()).unwrap_or(0);
                        let one = h[i].clone();
                        h.extend(std::iter::repeat_n(one, copies));
                    }
                    if let Some(c) = t.field_mut("undo_cursor") {
                        *c = Mp::Int(*pick(u, HOSTILE_INTS));
                    }
                }
            }
            // A story that no longer exists.
            _ => {
                if let Some(t) = tree.as_mut() {
                    let story = forged_story(u);
                    if let Some(f) = t.field_mut("active_story") {
                        *f = story;
                    }
                    if u.ratio(1, 2).unwrap_or(false)
                        && let Some(f) = t.field_mut("selection")
                    {
                        saturate_ints(u, f, 4);
                    }
                }
            }
        }
    }
    let mut body = match payload_override {
        Some(p) => p,
        None => match &tree {
            Some(t) => {
                let mut b = Vec::new();
                t.encode(&mut b);
                b
            }
            None => payload.to_vec(),
        },
    };
    if lie && let Some(lied) = lie_about_a_length(u, &body) {
        body = lied;
    }
    /* Issue #422 — never hand the target more than the budget (duplicated
    history entries and 70 KB strings add up); fall back to the input. */
    if head.len() + body.len() > MAX_SNAPSHOT_BYTES {
        return bytes.to_vec();
    }
    let mut out = head;
    out.extend_from_slice(&body);
    out
}

/// An `active_story` naming a note / text box / header that is not there.
fn forged_story(u: &mut Unstructured) -> Mp {
    let n = |u: &mut Unstructured| Mp::Int(*pick(u, HOSTILE_INTS));
    let (name, fields) = match u.int_in_range(0u8..=3).unwrap_or(0) {
        0 => (
            "Note",
            vec![
                ("kind", Mp::string(*pick(u, &["Footnote", "Endnote"]))),
                ("id", n(u)),
                ("page", n(u)),
                ("section_block", n(u)),
            ],
        ),
        1 => (
            "Header",
            vec![
                ("rid", Mp::string("rIdMissing")),
                ("page", n(u)),
                ("section_block", n(u)),
                ("role", Mp::string("Default")),
            ],
        ),
        2 => (
            "Footer",
            vec![
                ("rid", Mp::string("")),
                ("page", n(u)),
                ("section_block", n(u)),
                ("role", Mp::string("First")),
            ],
        ),
        _ => (
            "TextBox",
            vec![
                ("host", Mp::Arr(vec![Mp::Int(*pick(u, HOSTILE_INTS))])),
                ("at", n(u)),
                ("page", n(u)),
                ("section_block", n(u)),
            ],
        ),
    };
    Mp::Map(vec![(
        Mp::string(name),
        Mp::Map(
            fields
                .into_iter()
                .map(|(k, v)| (Mp::string(k), v))
                .collect(),
        ),
    )])
}

/* ------------------------------------------------------------------ */
/* Base sessions                                                       */
/* ------------------------------------------------------------------ */

/// A session worth snapshotting: a seeded document, optionally opened from
/// a generated `.docx` (so a source package, media and comments exist),
/// then a short generated command sequence.
pub fn base_engine(u: &mut Unstructured) -> engine_wasm::Engine {
    let seed_text = command_gen::gen_seed_text(u);
    let mut engine = engine_wasm::Engine::new_headless(engine::DocumentTree::from_text(&seed_text));
    if u.ratio(1, 2).unwrap_or(false) {
        let docx = if u.ratio(1, 2).unwrap_or(false) {
            Some(format_docx::test_fixtures::part_scoped_media_docx(
                b"\x89PNG not really",
                b"\xff\xd8 not really",
            ))
        } else {
            crate::docx_gen::build_docx(u)
        };
        if let Some(bytes) = docx {
            let _ = engine.apply_sync(bridge::Command::OpenDocument {
                bytes,
                format: bridge::DocFormat::Docx,
                name: Some("fuzz.docx".to_string()),
                defaults: None,
                limits: None,
                password: None,
            });
        }
    }
    for cmd in command_gen::gen_command_sequence(u, 24) {
        let _ = engine.apply_sync(cmd);
    }
    engine
}

/// `(snapshot bytes, package key, detached package)` as the engine's own
/// `Command::Snapshot` reports them.
pub fn capture(
    engine: &mut engine_wasm::Engine,
    detach: bool,
) -> Option<(Vec<u8>, Option<String>, Option<Vec<u8>>)> {
    match engine.apply_sync(bridge::Command::Snapshot {
        seq: Some(1),
        detach_package: Some(detach),
        known_package_hash: None,
    }) {
        bridge::Event::Snapshot {
            bytes,
            package_hash,
            package,
            ..
        } => Some((bytes, package_hash, package)),
        _ => None,
    }
}

/* ------------------------------------------------------------------ */
/* Seed container                                                      */
/* ------------------------------------------------------------------ */

/// Leading bytes of a committed `snapshot_decode` seed.
pub const SEED_MAGIC: &[u8; 8] = b"NGESSEED";

/// `snapshot` (+ `package`) in the committed-seed container format.
pub fn seed_bytes(snapshot: &[u8], package: Option<&[u8]>) -> Vec<u8> {
    let mut out = SEED_MAGIC.to_vec();
    out.extend_from_slice(&(snapshot.len() as u32).to_le_bytes());
    out.extend_from_slice(snapshot);
    out.extend_from_slice(package.unwrap_or(&[]));
    out
}

/// Inverse of [`seed_bytes`]; `None` when `data` is not a seed container.
/// An empty package tail reads as "no package supplied".
pub fn parse_seed(data: &[u8]) -> Option<(&[u8], Option<&[u8]>)> {
    let rest = data.strip_prefix(SEED_MAGIC.as_slice())?;
    let (len, rest) = rest.split_at_checked(4)?;
    let len = u32::from_le_bytes(len.try_into().ok()?) as usize;
    let (snapshot, package) = rest.split_at_checked(len)?;
    Some((snapshot, (!package.is_empty()).then_some(package)))
}

/// The committed `corpus/snapshot_decode/` seeds, `(file name, bytes)`:
/// the v1 (`pkg-<len>-<fnv>`) and v2 (`sha256-`) detached-package formats,
/// a self-contained inline package, a media-bearing document, and the
/// hostile envelopes a refusal must handle (bad magic, unsupported
/// version, truncated payload, a package that does not match its key).
pub fn snapshot_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let mut seeds: Vec<(&'static str, Vec<u8>)> = Vec::new();
    let open_engine = |docx: Vec<u8>| {
        let mut e =
            engine_wasm::Engine::new_headless(engine::DocumentTree::from_text("Seed paragraph."));
        let _ = e.apply_sync(bridge::Command::OpenDocument {
            bytes: docx,
            format: bridge::DocFormat::Docx,
            name: Some("seed.docx".to_string()),
            defaults: None,
            limits: None,
            password: None,
        });
        let _ = e.apply_sync(bridge::Command::InsertText {
            text: " edited".to_string(),
            at: None,
        });
        e
    };
    let simple = format_docx::test_fixtures::package_with_document_xml(
        &format_docx::test_fixtures::document_xml_with_body(
            "<w:p><w:r><w:t>Hello snapshot</w:t></w:r></w:p>",
        ),
        &[],
    );
    let media = format_docx::test_fixtures::part_scoped_media_docx(
        b"\x89PNG\r\n\x1a\n body image",
        b"\xff\xd8\xff header image",
    );

    let mut e = open_engine(simple);
    if let Some((bytes, key, pkg)) = capture(&mut e, true) {
        let pkg = pkg.expect("a detached snapshot ships its package");
        seeds.push(("seed_v2_detached_sha256", seed_bytes(&bytes, Some(&pkg))));
        seeds.push(("seed_v2_detached_package_missing", seed_bytes(&bytes, None)));
        seeds.push((
            "seed_v2_detached_package_mismatch",
            seed_bytes(&bytes, Some(b"not the package the key names")),
        ));
        // The format-v1 shape: version byte 1, FNV key.
        let mut tree = Mp::parse(&bytes[5..]).expect("snapshot payload is msgpack");
        if let Some(f) = tree.field_mut("package_hash") {
            *f = Mp::string(&engine::package::legacy_package_key(&pkg));
        }
        let mut v1 = bytes[..4].to_vec();
        v1.push(1);
        tree.encode(&mut v1);
        seeds.push(("seed_v1_detached_pkg_fnv", seed_bytes(&v1, Some(&pkg))));
        let _ = key;
        let mut trunc = bytes.clone();
        trunc.truncate(bytes.len() / 2);
        seeds.push(("seed_truncated_payload", seed_bytes(&trunc, None)));
        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        seeds.push(("seed_bad_magic", seed_bytes(&bad_magic, None)));
        let mut bad_version = bytes.clone();
        bad_version[4] = 9;
        seeds.push(("seed_unsupported_version", seed_bytes(&bad_version, None)));
    }
    if let Some((bytes, _, _)) = capture(&mut e, false) {
        seeds.push(("seed_v2_inline_package", seed_bytes(&bytes, None)));
    }
    let mut e = open_engine(media);
    if let Some((bytes, _, _)) = capture(&mut e, false) {
        seeds.push(("seed_v2_inline_media_refs", seed_bytes(&bytes, None)));
    }
    if let Some((bytes, _, pkg)) = capture(&mut e, true) {
        seeds.push(("seed_v2_detached_media", seed_bytes(&bytes, pkg.as_deref())));
    }
    // An engine-authored document: no package at all.
    let mut e = engine_wasm::Engine::new_headless(engine::DocumentTree::from_text("no package"));
    if let Some((bytes, _, _)) = capture(&mut e, true) {
        seeds.push(("seed_v2_no_package", seed_bytes(&bytes, None)));
    }
    seeds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #422 — EVERY length header of a real engine snapshot (its
    /// document, styles, media, selection…), rewritten to claim 4 G:
    /// restore must refuse each one with a typed error. Before
    /// `engine::snapshot::validate_payload`, the `blocks` header alone
    /// aborted the process (`im::Vector` preallocated 3.9 TB).
    #[test]
    fn every_lying_length_header_is_a_typed_restore_error() {
        let (_, seed) = snapshot_seeds()
            .into_iter()
            .find(|(n, _)| *n == "seed_v2_inline_media_refs")
            .expect("seed");
        let (snap, _) = parse_seed(&seed).expect("container");
        let (head, payload) = snap.split_at(engine::snapshot::HEADER_LEN);
        let headers = length_headers(payload);
        assert!(headers.len() > 50, "walked {} headers", headers.len());
        let mut engine = engine_wasm::Engine::new_headless(engine::DocumentTree::new());
        for h in headers {
            let mut lied = head.to_vec();
            lied.extend_from_slice(&lie_at(payload, h, u32::MAX));
            let got = engine.restore_for_fuzzing(&lied, None);
            assert!(got.is_err(), "header at {}: {got:?}", h.0);
        }
    }

    /// Issue #422 — a mutated envelope never exceeds the budget.
    #[test]
    fn mutations_stay_within_the_snapshot_budget() {
        let (_, seed) = snapshot_seeds()
            .into_iter()
            .find(|(n, _)| *n == "seed_v2_detached_media")
            .expect("seed");
        let (snap, pkg) = parse_seed(&seed).expect("container");
        // Every op, forced: many history copies + long strings.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..300 {
            let mut noise = Vec::new();
            for _ in 0..64 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                noise.push(state as u8);
            }
            let m = mutate_snapshot(&mut Unstructured::new(&noise), snap, pkg);
            assert!(m.len() <= MAX_SNAPSHOT_BYTES);
        }
    }
}
