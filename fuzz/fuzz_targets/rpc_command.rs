#![no_main]
//! Fuzz `bridge::Command` JSON deserialization — a malicious or buggy TS
//! caller must not be able to panic the engine with a crafted RPC payload
//! (PHASE_5 §8, §9 "all RPC Command payloads bounds-checked").

use bridge::Command;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<Command>(data);
});
