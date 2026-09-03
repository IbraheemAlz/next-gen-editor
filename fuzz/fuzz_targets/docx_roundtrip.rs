#![no_main]
//! New target (D5.5, issue #90): read -> write -> read must be stable and
//! never panic. Uses the same structure-aware `.docx` generator as
//! `docx_reader`; see `engine_fuzz::run_docx_roundtrip`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    engine_fuzz::run_docx_roundtrip(data);
});
