//! Issue #384 — `--regen-check`: regenerate every clean paragraph with no
//! edit (`format_docx::writer::regen_check`) and bucket the paragraphs
//! that do not come back byte-identical by the normalization that makes
//! them equal.
//!
//! The classes (`main.rs` histograms them over the corpus):
//!
//! - `smartTag` — the source carries an inline `<w:smartTag>` /
//!   `<w:customXml>` wrapper the model flattens (issue #272);
//! - `proofErr-order` — equal once every `<w:proofErr/>` is removed
//!   (a marker re-emitted on the wrong side of a wrapper boundary);
//! - `whitespace` — equal once whitespace-only character data between
//!   elements is removed (pretty-print whitespace lost or moved);
//! - `instrText-space` — equal once every `<w:instrText>`'s content is
//!   trimmed and its attributes dropped;
//! - `mixed` — only the three normalizations together make it equal;
//! - `other` — none does.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Per-document `--regen-check` result.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RegenCheck {
    /// Clean paragraphs regenerated.
    pub checked: u32,
    /// Of those, the ones with text.
    pub nonempty_checked: u32,
    /// Paragraphs whose regeneration differs from the source bytes.
    pub mismatched: u32,
    /// Of those, the ones with text.
    pub nonempty_mismatched: u32,
    /// Mismatching paragraphs per class (see the module docs).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub classes: BTreeMap<String, u32>,
}

impl RegenCheck {
    pub fn of(report: &format_docx::writer::regen_check::RegenReport) -> Self {
        let mut c = RegenCheck {
            checked: report.checked,
            nonempty_checked: report.nonempty_checked,
            ..Default::default()
        };
        for m in &report.mismatches {
            c.mismatched += 1;
            c.nonempty_mismatched += u32::from(m.nonempty);
            *c.classes
                .entry(classify(&m.source, &m.regenerated).to_string())
                .or_insert(0) += 1;
        }
        c
    }
}

/// One normalization of a paragraph's bytes.
type Normalize = fn(&str) -> String;

/// The class of one mismatching paragraph (see the module docs).
pub fn classify(source: &str, regenerated: &str) -> &'static str {
    if source.contains("<w:smartTag") || source.contains("<w:customXml") {
        return "smartTag";
    }
    let norms: [(&str, Normalize); 3] = [
        ("proofErr-order", strip_proof_err),
        ("whitespace", strip_inter_element_ws),
        ("instrText-space", trim_instr_text),
    ];
    for (label, f) in norms {
        if f(source) == f(regenerated) {
            return label;
        }
    }
    let all = |s: &str| trim_instr_text(&strip_inter_element_ws(&strip_proof_err(s)));
    if all(source) == all(regenerated) {
        "mixed"
    } else {
        "other"
    }
}

/// `xml` without its `<w:proofErr …/>` elements.
fn strip_proof_err(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<w:proofErr") {
        out.push_str(&rest[..i]);
        match rest[i..].find("/>") {
            Some(j) => rest = &rest[i + j + 2..],
            None => {
                rest = &rest[i..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `xml` without whitespace-only character data between two tags.
fn strip_inter_element_ws(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find('>') {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        let ws = rest
            .bytes()
            .take_while(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
            .count();
        if ws > 0 && rest[ws..].starts_with('<') {
            rest = &rest[ws..];
        }
    }
    out.push_str(rest);
    out
}

/// `xml` with every `<w:instrText …>content</w:instrText>` spelled
/// `<w:instrText>trimmed content</w:instrText>`.
fn trim_instr_text(xml: &str) -> String {
    const OPEN: &str = "<w:instrText";
    const CLOSE: &str = "</w:instrText>";
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find(OPEN) {
        let after = &rest[i + OPEN.len()..];
        /* `<w:instrTextX` is another element. */
        if !after.starts_with(['>', ' ', '\t', '\r', '\n', '/']) {
            out.push_str(&rest[..i + OPEN.len()]);
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else {
            break;
        };
        out.push_str(&rest[..i]);
        if after[..gt].ends_with('/') {
            out.push_str("<w:instrText/>");
            rest = &after[gt + 1..];
            continue;
        }
        let body = &after[gt + 1..];
        let Some(end) = body.find(CLOSE) else {
            out.push_str(&rest[i..]);
            return out;
        };
        out.push_str("<w:instrText>");
        out.push_str(body[..end].trim());
        out.push_str(CLOSE);
        rest = &body[end + CLOSE.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_each_normalization() {
        let src = r#"<w:p><w:hyperlink r:id="r1"><w:r><w:t>a</w:t></w:r><w:proofErr w:type="spellEnd"/></w:hyperlink></w:p>"#;
        let regen = r#"<w:p><w:hyperlink r:id="r1"><w:r><w:t>a</w:t></w:r></w:hyperlink><w:proofErr w:type="spellEnd"/></w:p>"#;
        assert_eq!(classify(src, regen), "proofErr-order");

        let src = "<w:p>\n  <w:r><w:t>a</w:t></w:r>\n</w:p>";
        let regen = "<w:p><w:r><w:t>a</w:t></w:r></w:p>";
        assert_eq!(classify(src, regen), "whitespace");

        let src = r#"<w:p><w:r><w:instrText> PAGE </w:instrText></w:r></w:p>"#;
        let regen = r#"<w:p><w:r><w:instrText xml:space="preserve">PAGE</w:instrText></w:r></w:p>"#;
        assert_eq!(classify(src, regen), "instrText-space");

        let src = r#"<w:p><w:smartTag w:uri="u" w:element="e"><w:r><w:t>a</w:t></w:r></w:smartTag></w:p>"#;
        let regen = r#"<w:p><w:r><w:t>a</w:t></w:r></w:p>"#;
        assert_eq!(classify(src, regen), "smartTag");

        let src = "<w:p>\n<w:proofErr w:type=\"x\"/><w:r><w:t>a</w:t></w:r></w:p>";
        let regen = "<w:p><w:r><w:t>a</w:t></w:r><w:proofErr w:type=\"x\"/></w:p>";
        assert_eq!(classify(src, regen), "mixed");

        assert_eq!(classify("<w:p><w:r/></w:p>", "<w:p/>"), "other");
    }

    #[test]
    fn whitespace_inside_text_is_kept_by_the_normalizer() {
        assert_eq!(
            strip_inter_element_ws("<w:t>a b</w:t>\n <w:t> c</w:t>"),
            "<w:t>a b</w:t><w:t> c</w:t>"
        );
        assert_eq!(
            trim_instr_text("<w:instrTextX>a</w:instrTextX>"),
            "<w:instrTextX>a</w:instrTextX>"
        );
    }
}
