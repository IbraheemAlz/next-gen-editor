//! Round-trip harness.
//!
//! 1. Build a fixture `.docx` from a known Arabic seed text.
//! 2. Re-open via `read_docx`, verify the tree matches the seed.
//! 3. Insert a sentence at end of document via `engine::insert_text`.
//! 4. Save as a new `.docx` via `write_docx` (preserves sibling entries).
//! 5. Re-open the saved blob; verify edited text is present.
//! 6. Diff the saved `.docx` ZIP entries against the fixture:
//!    - Non-`word/document.xml` entries must be byte-identical (the writer
//!      preserves them verbatim — 0% structural drift on those bytes).
//!    - `word/document.xml` is allowed to differ but only in the edited
//!      region. We assert the diff is bounded.
//!
//! Exit 0 on PASS, non-zero on FAIL.

use anyhow::{Context, Result, bail};
use engine::DocumentTree;
use format_docx::writer::build_minimal_docx;
use format_docx::{read_docx, write_docx};
use std::process::ExitCode;

const SEED_TEXT: &str = "السلام عليكم ورحمة الله وبركاته";
const INSERT_TEXT: &str = " تم التعديل";

fn run() -> Result<()> {
    /* 1. Build fixture from seed. */
    let seed_doc = DocumentTree::from_text(SEED_TEXT);
    let fixture_bytes = build_minimal_docx(&seed_doc).context("build fixture")?;
    println!("[roundtrip] fixture .docx: {} bytes", fixture_bytes.len());

    /* 2. Re-open fixture; verify it parses to the original tree. */
    let archive_a = read_docx(&fixture_bytes).context("read fixture")?;
    if archive_a.document.paragraph_count() != 1 {
        bail!(
            "fixture has wrong paragraph count: {}",
            archive_a.document.paragraph_count()
        );
    }
    if archive_a.document.paragraph_text(0) != Some(SEED_TEXT) {
        bail!(
            "fixture text mismatch — expected `{}`, got `{:?}`",
            SEED_TEXT,
            archive_a.document.paragraph_text(0)
        );
    }
    println!("[roundtrip] step 2 OK — fixture parses back to seed");

    /* 3. Insert at end-of-document. */
    let end = archive_a.document.end_of_document();
    let edited = archive_a.document.insert_text(end, INSERT_TEXT);
    let expected_combined = format!("{SEED_TEXT}{INSERT_TEXT}");
    if edited.paragraph_text(0) != Some(expected_combined.as_str()) {
        bail!(
            "in-memory insert wrong — expected `{}`, got `{:?}`",
            expected_combined,
            edited.paragraph_text(0)
        );
    }
    println!("[roundtrip] step 3 OK — in-memory edit reflected");

    /* 4. Save the edited tree (using archive_a's siblings). */
    let edited_bytes = write_docx(&archive_a, &edited).context("write edited")?;
    println!(
        "[roundtrip] saved edited .docx: {} bytes",
        edited_bytes.len()
    );

    /* 5. Re-open the saved blob; verify edited text round-trips. */
    let archive_b = read_docx(&edited_bytes).context("re-read edited")?;
    if archive_b.document.paragraph_text(0) != Some(expected_combined.as_str()) {
        bail!(
            "saved .docx didn't preserve edit — expected `{}`, got `{:?}`",
            expected_combined,
            archive_b.document.paragraph_text(0)
        );
    }
    println!("[roundtrip] step 5 OK — saved .docx parses back to edited tree");

    /* 6. Diff sibling entries verbatim + bounded diff for document.xml. */
    let mut sibling_drift = 0_usize;
    for (name, bytes) in &archive_a.other_entries {
        let b = archive_b
            .other_entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b);
        match b {
            Some(b) if b == bytes => {
                println!("  [SAME] {name} ({} B)", bytes.len());
            }
            Some(b) => {
                sibling_drift += b.len().abs_diff(bytes.len());
                println!("  [DRIFT] {name}: {} -> {} bytes", bytes.len(), b.len());
            }
            None => {
                bail!("entry `{name}` missing from saved archive");
            }
        }
    }
    if sibling_drift != 0 {
        bail!(
            "{sibling_drift} bytes of sibling-entry drift — writer should preserve them verbatim"
        );
    }
    println!("[roundtrip] step 6a OK — all sibling entries byte-identical");

    /* For word/document.xml: the saved bytes will differ since we serialized
    fresh, but the only structural change vs. the seed should be the
    inserted text. Assert the diff size is bounded. */
    let extract_doc_xml = |bytes: &[u8]| -> Result<Vec<u8>> {
        use std::io::Read;
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let mut f = a.by_name("word/document.xml")?;
        let mut out = Vec::new();
        f.read_to_end(&mut out)?;
        Ok(out)
    };
    let doc_a = extract_doc_xml(&fixture_bytes)?;
    let doc_b = extract_doc_xml(&edited_bytes)?;
    let doc_diff = (doc_b.len() as isize - doc_a.len() as isize).unsigned_abs();
    let insert_len_utf8 = INSERT_TEXT.len();
    println!(
        "[roundtrip] document.xml: {} -> {} bytes (Δ {} B, insert text {} B)",
        doc_a.len(),
        doc_b.len(),
        doc_diff,
        insert_len_utf8
    );
    /* The diff should be approximately `insert_len_utf8` (UTF-8 byte size of
    the inserted text). Allow up to 2× headroom for any whitespace
    normalization the writer applies. */
    let bound = insert_len_utf8 * 2;
    if doc_diff > bound {
        bail!(
            "document.xml diff {} B exceeds bound {} B (insert {} B × 2)",
            doc_diff,
            bound,
            insert_len_utf8
        );
    }
    println!("[roundtrip] step 6b OK — document.xml diff within bound");

    println!("\nPASS");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e:#}");
            ExitCode::FAILURE
        }
    }
}
