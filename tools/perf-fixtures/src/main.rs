//! Generates the synthetic `.docx` load files the memory-profile harness
//! (D5.2) profiles against — `tests/perf/{50,100,250,500}p.docx`.
//!
//! Each file holds `pages * PARAS_PER_PAGE` filler paragraphs. The documents
//! are built with the engine's own `build_minimal_docx` writer and verified to
//! round-trip through `read_docx`, so the harness's `LOAD_DOCX` command can
//! always parse them.

use engine::DocumentTree;
use format_docx::read_docx;
use format_docx::writer::build_minimal_docx;
use std::fs;
use std::path::Path;

/// Filler paragraphs per nominal page — roughly a page of body copy on A4.
const PARAS_PER_PAGE: usize = 20;

/// Mixed Latin + Arabic filler so a loaded document exercises the BiDi and
/// shaping paths a real corpus document would.
const FILLER: &str = "The quick brown fox jumps over the lazy dog. \
    مرحبا بالعالم، هذا نص حشو لقياس استهلاك الذاكرة.";

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/perf");
    fs::create_dir_all(&out_dir).expect("create tests/perf");

    for pages in [50usize, 100, 250, 500] {
        let count = pages * PARAS_PER_PAGE;
        let texts = (0..count).map(|i| format!("Paragraph {i:05}. {FILLER}"));
        let doc = DocumentTree::from_paragraphs(texts);

        let bytes = build_minimal_docx(&doc).expect("build .docx");

        /* Sanity: the file must parse back, or the harness LOAD_DOCX fails. */
        let parsed = read_docx(&bytes).expect("re-read generated .docx");
        assert_eq!(
            parsed.document.paragraph_count() as usize,
            count,
            "round-trip paragraph count mismatch for {pages}p",
        );

        let path = out_dir.join(format!("{pages}p.docx"));
        fs::write(&path, &bytes).expect("write .docx");
        println!(
            "[perf-fixtures] {pages}p -> {count} paragraphs, {} bytes",
            bytes.len()
        );
    }
}
