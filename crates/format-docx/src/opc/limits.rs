//! Issue #348 — package resource limits.
//!
//! A `.docx` is attacker-shaped input: the ZIP central directory declares
//! each entry's uncompressed size, and nothing stops it from lying. The
//! reader used to `Vec::with_capacity(declared)` and `read_to_end` without a
//! cap, so an entry claiming ~4 GiB trapped the wasm worker ("capacity
//! overflow" → crash-recovery cycle) and a highly compressible part was
//! inflated in full against the 256 MiB worker budget.
//!
//! [`PackageLimits`] bounds what one read may materialize:
//!
//! - **bytes** — every entry is read through `take(limit + 1)`, so at most
//!   one byte past the per-part limit (or what remains of the per-package
//!   total) is ever inflated; the declared size is never used to allocate;
//! - **entries** — the number of ZIP entries, checked before any is read;
//! - **XML shape** — the nesting depth and element count of every XML part
//!   the reader walks ([`check_xml_part`]), so a 5000-deep content-control
//!   chain or a multi-million-element part is refused before the typed
//!   walk starts.
//!
//! Every overflow is the typed [`DocxError::PackageTooLarge`], which the
//! engine answers as `Event::Error { kind: PackageTooLarge }` — a refused
//! open, never a trap. The defaults are sized to the worker budget: the
//! decompressed package lives in memory twice (the archive's entries and
//! the tree's retained source package) next to the model, so 128 MiB of
//! decompressed parts is the most one open may hold.

use crate::error::DocxError;
use crate::parts::table::MAX_TABLE_NESTING_DEPTH;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::io::{Cursor, Read};
use zip::ZipArchive;

const MIB: u64 = 1024 * 1024;

/// Issue #348 — the resource bounds of one package read. See the module
/// docs; [`PackageLimits::default`] is sized to the 256 MiB worker budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageLimits {
    /// Largest decompressed size of any one entry, in bytes.
    pub max_part_bytes: u64,
    /// Largest decompressed size of the whole package, in bytes.
    pub max_total_bytes: u64,
    /// Most ZIP entries a package may hold.
    pub max_entries: usize,
    /// Deepest element nesting of an XML part the reader walks. Content
    /// inside a table nested past `parts::table::MAX_TABLE_NESTING_DEPTH`
    /// is kept opaque (issue #111) and is not counted.
    pub max_xml_depth: usize,
    /// Most elements one XML part may hold.
    pub max_xml_elements: u64,
}

impl PackageLimits {
    /// 64 MiB per part, 128 MiB per package, 10 000 entries, XML depth
    /// 256, 4 000 000 elements per part.
    pub const DEFAULT: Self = Self {
        max_part_bytes: 64 * MIB,
        max_total_bytes: 128 * MIB,
        max_entries: 10_000,
        max_xml_depth: 256,
        max_xml_elements: 4_000_000,
    };
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Which [`PackageLimits`] bound a package exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageLimit {
    /// One entry inflates past `max_part_bytes`.
    PartBytes,
    /// The package inflates past `max_total_bytes`.
    TotalBytes,
    /// More than `max_entries` ZIP entries.
    Entries,
    /// An XML part nests deeper than `max_xml_depth`.
    XmlDepth,
    /// An XML part holds more than `max_xml_elements` elements.
    XmlElements,
}

impl std::fmt::Display for PackageLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PackageLimit::PartBytes => "part size",
            PackageLimit::TotalBytes => "package size",
            PackageLimit::Entries => "entry count",
            PackageLimit::XmlDepth => "XML nesting depth",
            PackageLimit::XmlElements => "XML element count",
        })
    }
}

fn too_large(limit: PackageLimit, max: u64, part: Option<&str>) -> DocxError {
    DocxError::PackageTooLarge {
        limit,
        max,
        part: part.map(str::to_owned),
    }
}

/// Read one entry's bytes through `take(budget + 1)`: at most one byte past
/// the smaller of the per-part limit and what is left of the package total
/// is ever inflated, and the declared size is never trusted for the
/// allocation. `total` is the running decompressed size of the package.
pub(crate) fn read_entry_bounded<R: Read>(
    entry: R,
    name: &str,
    limits: &PackageLimits,
    total: &mut u64,
) -> Result<Vec<u8>, DocxError> {
    let left = limits.max_total_bytes.saturating_sub(*total);
    let budget = limits.max_part_bytes.min(left);
    let mut buf = Vec::new();
    entry.take(budget.saturating_add(1)).read_to_end(&mut buf)?;
    let len = buf.len() as u64;
    if len > budget {
        return Err(if budget == limits.max_part_bytes {
            too_large(PackageLimit::PartBytes, limits.max_part_bytes, Some(name))
        } else {
            too_large(PackageLimit::TotalBytes, limits.max_total_bytes, Some(name))
        });
    }
    *total += len;
    /* `read_to_end` grows geometrically; give a large slack back rather
    than holding up to twice the part for the life of the document. */
    if buf.capacity() - buf.len() > buf.len() / 4 {
        buf.shrink_to_fit();
    }
    Ok(buf)
}

/// Every entry of the ZIP package `bytes`, `(name, decompressed bytes)` in
/// archive order, read under `limits` (entry count first, then each entry
/// through [`read_entry_bounded`]).
pub(crate) fn read_package_entries(
    bytes: &[u8],
    limits: &PackageLimits,
) -> Result<Vec<(String, Vec<u8>)>, DocxError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() > limits.max_entries {
        return Err(too_large(
            PackageLimit::Entries,
            limits.max_entries as u64,
            None,
        ));
    }
    let mut total = 0u64;
    let mut out = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let name = file.name().to_owned();
        let data = read_entry_bounded(file, &name, limits, &mut total)?;
        out.push((name, data));
    }
    Ok(out)
}

/// `true` for an entry the reader may parse as XML (and therefore walk):
/// every `.xml` / `.rels` part — the main part and its siblings are found
/// through the relationships (issue #353), so they can live anywhere —
/// except the data parts the reader never parses (custom XML data, the
/// extended / custom document properties), which ride the passthrough as
/// bytes.
pub(crate) fn is_walked_xml_part(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let xml = lower.ends_with(".xml") || lower.ends_with(".rels");
    let data_only = lower.starts_with("customxml/")
        || (lower.starts_with("docprops/") && lower != "docprops/core.xml");
    xml && !data_only
}

/// Issue #348 — the XML shape bounds of one part: element nesting depth
/// and element count. A table nested past `MAX_TABLE_NESTING_DEPTH` is kept
/// as opaque bytes (issue #111) and never walked structurally, so the
/// depth inside it does not count (its elements still do). Malformed XML
/// is not this check's business — it stops scanning and leaves the verdict
/// to the part's parser.
pub(crate) fn check_xml_part(
    name: &str,
    xml: &[u8],
    limits: &PackageLimits,
) -> Result<(), DocxError> {
    let mut reader = Reader::from_reader(xml);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = false;
    let mut buf = Vec::new();
    /* `(is a w:tbl)` per open element: the table depth decides whether
    the walk is still structural. */
    let mut open: Vec<bool> = Vec::new();
    let mut tables: u32 = 0;
    /* Structural depth: elements opened while fewer than the cap's tables
    enclose them. */
    let mut depth: usize = 0;
    let mut elements: u64 = 0;
    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(ev) => ev,
        };
        match event {
            Event::Start(e) => {
                elements += 1;
                let is_tbl = e.name().as_ref() == b"w:tbl";
                let structural = tables <= MAX_TABLE_NESTING_DEPTH;
                if structural {
                    depth += 1;
                    if depth > limits.max_xml_depth {
                        return Err(too_large(
                            PackageLimit::XmlDepth,
                            limits.max_xml_depth as u64,
                            Some(name),
                        ));
                    }
                }
                if is_tbl {
                    tables += 1;
                }
                open.push(is_tbl);
            }
            Event::Empty(_) => {
                elements += 1;
                if tables <= MAX_TABLE_NESTING_DEPTH && depth + 1 > limits.max_xml_depth {
                    return Err(too_large(
                        PackageLimit::XmlDepth,
                        limits.max_xml_depth as u64,
                        Some(name),
                    ));
                }
            }
            Event::End(_) => {
                if let Some(is_tbl) = open.pop() {
                    if is_tbl {
                        tables = tables.saturating_sub(1);
                    }
                    if tables <= MAX_TABLE_NESTING_DEPTH {
                        depth = depth.saturating_sub(1);
                    }
                }
            }
            _ => {}
        }
        if elements > limits.max_xml_elements {
            return Err(too_large(
                PackageLimit::XmlElements,
                limits.max_xml_elements,
                Some(name),
            ));
        }
        buf.clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested(open: &str, close: &str, n: usize, inner: &str) -> String {
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(open);
        }
        s.push_str(inner);
        for _ in 0..n {
            s.push_str(close);
        }
        s
    }

    #[test]
    fn depth_cap_refuses_a_deep_chain() {
        let xml = nested("<a>", "</a>", 300, "");
        let err = check_xml_part("word/document.xml", xml.as_bytes(), &PackageLimits::DEFAULT)
            .expect_err("300 levels exceed 256");
        assert!(
            matches!(
                err,
                DocxError::PackageTooLarge {
                    limit: PackageLimit::XmlDepth,
                    max: 256,
                    ..
                }
            ),
            "{err:?}"
        );
        let ok = nested("<a>", "</a>", 255, "<b/>");
        check_xml_part("word/document.xml", ok.as_bytes(), &PackageLimits::DEFAULT)
            .expect("256 levels are allowed");
    }

    #[test]
    fn opaque_deep_tables_do_not_count_toward_depth() {
        /* Past the #111 table cap the subtree is bytes, not structure. */
        let xml = nested(
            "<w:tbl><w:tr><w:tc>",
            "</w:tc></w:tr></w:tbl>",
            500,
            "<w:p/>",
        );
        check_xml_part("word/document.xml", xml.as_bytes(), &PackageLimits::DEFAULT)
            .expect("a deep table chain past the nesting cap stays readable");
    }

    #[test]
    fn element_cap_counts_every_element() {
        let limits = PackageLimits {
            max_xml_elements: 10,
            ..PackageLimits::DEFAULT
        };
        let xml = format!("<r>{}</r>", "<e/>".repeat(10));
        let err = check_xml_part("p.xml", xml.as_bytes(), &limits).expect_err("11 elements");
        assert!(matches!(
            err,
            DocxError::PackageTooLarge {
                limit: PackageLimit::XmlElements,
                ..
            }
        ));
    }

    #[test]
    fn malformed_xml_is_left_to_the_parser() {
        check_xml_part("p.xml", b"<a><b></a", &PackageLimits::DEFAULT).expect("not ours");
    }

    #[test]
    fn bounded_read_never_inflates_past_the_budget() {
        let limits = PackageLimits {
            max_part_bytes: 8,
            max_total_bytes: 12,
            ..PackageLimits::DEFAULT
        };
        let mut total = 0;
        let err = read_entry_bounded(&b"123456789"[..], "b", &limits, &mut total)
            .expect_err("9 > part limit");
        assert_eq!(total, 0, "a refused part adds nothing");
        assert!(matches!(
            err,
            DocxError::PackageTooLarge {
                limit: PackageLimit::PartBytes,
                ..
            }
        ));
        let a = read_entry_bounded(&b"12345678"[..], "a", &limits, &mut total).expect("fits");
        assert_eq!(a.len(), 8);
        let err =
            read_entry_bounded(&b"12345"[..], "c", &limits, &mut total).expect_err("8 + 5 > total");
        assert!(matches!(
            err,
            DocxError::PackageTooLarge {
                limit: PackageLimit::TotalBytes,
                ..
            }
        ));
    }
}
