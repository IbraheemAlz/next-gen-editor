#![no_main]
//! Fuzz the `.docx` reader with a structure-aware, schema-shaped WML
//! document wrapped in a minimal OPC package (D5.5, issue #90) — never
//! raw bytes handed straight to the parser. See `engine_fuzz::docx_gen`
//! for the generator and `engine_fuzz::run_docx_reader` for the body this
//! wraps (also driven by `examples/smoke.rs` on stable Rust).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    engine_fuzz::run_docx_reader(data);
});
