//! `DocxError` — shared error type for every reader / writer / OPC parser.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DocxError {
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("XML attribute error: {0}")]
    XmlAttr(#[from] quick_xml::events::attributes::AttrError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("required entry missing: {0}")]
    MissingEntry(String),
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    /// Issue #110 — `check_document_xml_well_formed` found a part that
    /// quick-xml accepted event-by-event but that is not a single
    /// well-formed document (unclosed elements at EOF, no root element).
    #[error("malformed XML: {0}")]
    MalformedXml(String),
    /// Issue #348 — the package exceeded one of the reader's
    /// [`crate::PackageLimits`] (a part or the whole package inflating past
    /// its byte budget, too many entries, an XML part nested too deep or
    /// holding too many elements). The open is refused before anything is
    /// allocated from the attacker-controlled sizes.
    #[error("package too large: {limit} exceeds the reader's limit of {max}{}", part.as_deref().map(|p| format!(" (in `{p}`)")).unwrap_or_default())]
    PackageTooLarge {
        limit: crate::opc::limits::PackageLimit,
        max: u64,
        part: Option<String>,
    },
    /// Issue #353 — a relationship target (or archive path) resolves
    /// outside the package (`../../x`) or carries a NUL: refused instead
    /// of being clamped into some other part's name.
    #[error("unsafe part name: {0}")]
    UnsafePartName(String),
}

/// Non-fatal reader diagnostics. The document opened, but some subtree was
/// degraded on the way into the typed model; its bytes still ride the
/// passthrough so a resave loses nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocxWarning {
    /// Issue #111 — a `<w:tbl>` nested `limit` or more levels deep was kept
    /// as an opaque passthrough block (`source_xml` preserved, `rows`
    /// empty) instead of recursing further. Apache POI's
    /// `deep-table-cell.docx` nests 5000 tables; unbounded recursion
    /// overflowed the stack.
    TableNestingTooDeep { limit: u32 },
    /// Issue #349 — a measure attribute (`attr`, e.g. `w:pgSz/@w:w`) held
    /// an unusable `value` (not a number, `NaN`, infinite, a unit its type
    /// does not allow, negative where only non-negative values are legal):
    /// it was ignored and the default applies. The source bytes are kept.
    InvalidMeasure { attr: String, value: String },
    /// Issue #349 — a measure attribute held a finite `value` outside its
    /// spec range; the model uses it clamped to `twips`. The source bytes
    /// are kept.
    MeasureClamped {
        attr: String,
        value: String,
        twips: i64,
    },
    /// Issue #407 — a DrawingML / VML EMU coordinate (`wp:extent/@cx`,
    /// `wp:posOffset`, a VML `style` length) held a finite `value` outside
    /// its range (±22 in); the model uses it clamped to `emu`. The source
    /// bytes are kept.
    EmuClamped {
        attr: String,
        value: String,
        emu: i64,
    },
    /// Issue #350 — complex fields still open in their instruction part
    /// when their paragraph ended (`count` of them): closed there, so their
    /// hidden instruction never swallows the following paragraphs.
    UnclosedField { count: u32 },
    /// Issue #350 — a `separate` / `end` field character with no open
    /// field (`kind` is the `w:fldCharType`): ignored.
    StrayFieldChar { kind: String },
    /// Issue #350 — complex fields nested deeper than `limit`: the extra
    /// levels are not modeled (their text stays hidden code).
    FieldNestingTooDeep { limit: u32 },
    /// Issue #325 — a WordprocessingML part binds WordprocessingML under a
    /// prefix (or as the default namespace) the literal-qname reader does
    /// not match. `part` is the archive entry (issue #394: the main part
    /// and every sibling the reader walks — headers, footers, footnotes,
    /// endnotes, comments, styles, numbering, settings). `normalized` =
    /// the part was re-prefixed into the canonical spelling and read; it
    /// is then **regenerate-only** (its source bytes are not reused: a
    /// zero-edit save re-emits the normalised bytes, and every passthrough
    /// / in-place patch of the part starts from them). `false` =
    /// normalisation itself failed and the part read as-is (likely empty).
    NonCanonicalNamespaces {
        part: String,
        detail: String,
        normalized: bool,
    },
    /// Issue #325 — the main part's root is not a WordprocessingML element
    /// in either namespace family; it reads as an empty document.
    NotWordprocessingMl,
    /// Issue #353 — `_rels/.rels` names an `officeDocument` part the
    /// archive does not contain; the fixed `word/document.xml` was used.
    MainPartFallback { target: String },
    /// Issue #353 — a relationship target of a sibling part escapes the
    /// package; it was ignored (the fixed sibling name applies).
    UnsafeRelationshipTarget { target: String },
}

impl DocxWarning {
    /// Issue #406 — the archive entry this warning concerns, when the
    /// reader knows it (`None` for diagnostics raised deep inside a part
    /// walk — measures, fields, table nesting).
    pub fn part(&self) -> Option<&str> {
        match self {
            DocxWarning::NonCanonicalNamespaces { part, .. } => Some(part),
            DocxWarning::MainPartFallback { .. } => Some("_rels/.rels"),
            DocxWarning::TableNestingTooDeep { .. }
            | DocxWarning::InvalidMeasure { .. }
            | DocxWarning::MeasureClamped { .. }
            | DocxWarning::EmuClamped { .. }
            | DocxWarning::UnclosedField { .. }
            | DocxWarning::StrayFieldChar { .. }
            | DocxWarning::FieldNestingTooDeep { .. }
            | DocxWarning::NotWordprocessingMl
            | DocxWarning::UnsafeRelationshipTarget { .. } => None,
        }
    }

    /// Issue #406 — the specifics of this warning in one line, for the
    /// shell's details list (the attribute and its raw value, the limit
    /// that was hit, the relationship target). Never document text.
    pub fn detail(&self) -> String {
        match self {
            DocxWarning::TableNestingTooDeep { limit } => {
                format!("tables nested {limit} or more levels deep kept as-is")
            }
            DocxWarning::InvalidMeasure { attr, value } => format!("{attr} = \"{value}\""),
            DocxWarning::MeasureClamped { attr, value, twips } => {
                format!("{attr} = \"{value}\" → {twips} twips")
            }
            DocxWarning::EmuClamped { attr, value, emu } => {
                format!("{attr} = \"{value}\" → {emu} EMU")
            }
            DocxWarning::UnclosedField { count } => {
                format!("{count} field(s) closed at the end of their paragraph")
            }
            DocxWarning::StrayFieldChar { kind } => format!("fldCharType=\"{kind}\""),
            DocxWarning::FieldNestingTooDeep { limit } => {
                format!("fields nested deeper than {limit} levels")
            }
            DocxWarning::NonCanonicalNamespaces {
                detail, normalized, ..
            } => {
                if *normalized {
                    detail.clone()
                } else {
                    format!("{detail} (normalisation failed; read as-is)")
                }
            }
            DocxWarning::NotWordprocessingMl => {
                "the main part's root is not a WordprocessingML element".to_string()
            }
            DocxWarning::MainPartFallback { target }
            | DocxWarning::UnsafeRelationshipTarget { target } => target.clone(),
        }
    }
}

/// Most reader warnings one read collects; later ones are dropped (a
/// hostile part can repeat the same bad value a million times).
const MAX_READ_WARNINGS: usize = 1000;

thread_local! {
    /// Issues #349 / #350 — the warnings sink of the read in progress
    /// ([`collect_read_warnings`]); `None` outside a read, where [`warn`]
    /// is a no-op (the writer re-parses source bytes for its verified
    /// passthroughs and must not report anything).
    static READ_WARNINGS: std::cell::RefCell<Option<Vec<DocxWarning>>> =
        const { std::cell::RefCell::new(None) };
}

/// Report a non-fatal reader diagnostic to the read in progress. Deep
/// helpers (`schema::measure`, the field machinery) have no warnings
/// vector in hand; this is their channel.
pub(crate) fn warn(w: DocxWarning) {
    READ_WARNINGS.with(|c| {
        if let Some(v) = c.borrow_mut().as_mut()
            && v.len() < MAX_READ_WARNINGS
        {
            v.push(w);
        }
    });
}

/// Run `f` with a fresh warnings sink; whatever [`warn`] collected is
/// appended to `out` afterwards (nested scopes each flush into their own
/// `out`). The previous sink is restored even if `f` panics.
pub(crate) fn collect_read_warnings<T>(
    out: &mut Vec<DocxWarning>,
    f: impl FnOnce(&mut Vec<DocxWarning>) -> T,
) -> T {
    struct Restore(Option<Option<Vec<DocxWarning>>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(prev) = self.0.take() {
                READ_WARNINGS.with(|c| *c.borrow_mut() = prev);
            }
        }
    }
    let prev = READ_WARNINGS.with(|c| c.borrow_mut().replace(Vec::new()));
    let mut guard = Restore(Some(prev));
    let r = f(out);
    let prev = guard.0.take().unwrap_or_default();
    let collected = READ_WARNINGS.with(|c| std::mem::replace(&mut *c.borrow_mut(), prev));
    out.extend(collected.unwrap_or_default());
    r
}

/// Issues #244 / #245 — non-fatal writer diagnostics: a best-effort
/// decision [`crate::writer::write_docx_with_notes`] took to keep
/// unmodeled content instead of dropping it. The file was written and is
/// well-formed; the content may sit at an approximate position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteNote {
    /// A regenerated paragraph's source markup was stale (an edit path
    /// that does not remap its offsets): its must-survive markup — legacy
    /// form fields, content-control boundaries — was written at offsets
    /// clamped to the current text.
    StaleMarkupClamped { markers: u32 },
    /// Issue #245 — the run-level content control `id` (the source byte
    /// offset of its `<w:sdt>`) would have crossed a regenerated wrapper
    /// (hyperlink, revision, field) or another control after an edit; its
    /// range was widened to enclose it so the part stays well-formed.
    InlineWrapperWidened { id: u32 },
}
