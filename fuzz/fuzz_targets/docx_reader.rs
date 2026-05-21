#![no_main]
//! Fuzz the `.docx` reader with arbitrary bytes — the parser must never panic,
//! only return `Err` on malformed input (PHASE_5 §8, §9 zip-slip / zip-bomb).
//!
//! PHASE_5 §8 sketches a `Reader::new(data).read_document()` API; the real
//! `format-docx` surface is the free function `read_docx`, used here.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    /* Skip trivially-tiny and oversized inputs — a `.docx` is a ZIP (needs a
    header) and the §9 per-archive cap keeps the CI corpus run bounded. */
    if data.len() < 4 || data.len() > 50 * 1024 * 1024 {
        return;
    }
    let _ = format_docx::read_docx(data);
});
