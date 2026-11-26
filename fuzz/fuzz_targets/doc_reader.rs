#![no_main]
//! Fuzz the legacy Word 97-2003 `.doc` importer (`format_doc::read_doc`,
//! issue #567): a committed seed or a mutation of one is read as the file
//! itself, every input also as a splice over a built image (see
//! `engine_fuzz::doc_gen`; not structure-aware yet). The body is
//! `engine_fuzz::run_doc_reader`, also driven by `examples/smoke.rs` on
//! stable Rust.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    engine_fuzz::run_doc_reader(data);
});
