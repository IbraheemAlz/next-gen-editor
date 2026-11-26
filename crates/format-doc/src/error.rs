//! [`DocError`] — why a `.doc` could not be imported.
//!
//! Every message is a fixed phrase plus numbers and the name of the
//! stream / structure that failed (`1Table: Clx`): never bytes or text from
//! the document itself. The engine shows it verbatim in the File menu's
//! open banner (`engine-wasm` `open_error`).

use std::fmt;

/// Why a Word 97-2003 binary document could not be imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocError {
    /// The OLE compound-file container itself is damaged (header, FAT,
    /// directory, or the sector chain of the named stream).
    Container {
        /// The stream being read when the container failed, if any.
        stream: Option<&'static str>,
        /// The container reader's own defect class (a fixed phrase).
        detail: &'static str,
    },
    /// The compound file is readable but has no `WordDocument` stream: it
    /// is not a Word document (an `.xls`, a `.ppt`, another OLE file).
    NotWordDocument,
    /// A Word 6.0 / 95 (or older) binary document: only the Word 97-2003
    /// format ([MS-DOC], `nFib` 0x00C1 and later) is read.
    UnsupportedVersion {
        /// `FibBase.wIdent` as found.
        w_ident: u16,
        /// `FibBase.nFib` as found.
        n_fib: u16,
    },
    /// `FibBase.fEncrypted` is set: the document is password-protected
    /// (XOR obfuscation or RC4, [MS-DOC] 2.2.6). Decryption is not
    /// implemented.
    Encrypted,
    /// A Word structure is malformed beyond recovery. `part` names the
    /// stream and the structure (`WordDocument: FIB`, `1Table: Clx`).
    Malformed { part: String, detail: String },
    /// The document exceeds one of the reader's bounds (`what`, its
    /// maximum `max`).
    TooLarge { what: &'static str, max: u64 },
}

impl fmt::Display for DocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocError::Container {
                stream: Some(s),
                detail,
            } => write!(f, "{s}: the compound-file container is damaged ({detail})"),
            DocError::Container {
                stream: None,
                detail,
            } => write!(f, "the compound-file container is damaged ({detail})"),
            DocError::NotWordDocument => write!(
                f,
                "not a Word document: the compound file has no WordDocument stream (an .xls, \
                 .ppt or other OLE file)"
            ),
            DocError::UnsupportedVersion { w_ident, n_fib } => write!(
                f,
                "a Word 6.0/95 or older binary document (wIdent 0x{w_ident:04X}, nFib \
                 0x{n_fib:04X}); only Word 97-2003 .doc files can be opened"
            ),
            DocError::Encrypted => write!(
                f,
                "the document is encrypted (password protected); opening encrypted Word \
                 97-2003 .doc files is not supported yet"
            ),
            DocError::Malformed { part, detail } => write!(f, "{part}: {detail}"),
            DocError::TooLarge { what, max } => {
                write!(f, "{what} exceeds the reader's limit of {max}")
            }
        }
    }
}

impl std::error::Error for DocError {}

impl DocError {
    /// Name the real table stream (`0Table` / `1Table`) in a structure
    /// error the table-stream parsers raised as `Table: …`.
    pub(crate) fn in_table_stream(self, table: &str) -> DocError {
        match self {
            DocError::Malformed { part, detail } => DocError::Malformed {
                part: match part.strip_prefix("Table") {
                    Some(rest) => format!("{table}{rest}"),
                    None => part,
                },
                detail,
            },
            other => other,
        }
    }
}

/// Shorthand for a [`DocError::Malformed`].
pub(crate) fn malformed(part: &str, detail: impl Into<String>) -> DocError {
    DocError::Malformed {
        part: part.to_string(),
        detail: detail.into(),
    }
}
