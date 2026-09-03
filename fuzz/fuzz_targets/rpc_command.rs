#![no_main]
//! Fuzz `bridge::Command` sequences (D5.5, issue #90 — scaled up from
//! single-command JSON deserialization) — structure-aware sequences driven
//! end to end through the real `Engine::apply` dispatcher, incl. the
//! auto-repaint -> layout pipeline and the native glyph rasterizer, with
//! invariant assertions after every command. See
//! `engine_fuzz::run_rpc_command` / `engine_fuzz::command_gen`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    engine_fuzz::run_rpc_command(data);
});
