//! `bridge` — RPC command/event types shared between the WASM engine and TS.
//!
//! Phase 1 PoC subset. Full schema lands in Phase 2 per
//! PHASE_2_BRIDGE_MEMORY.md §4–§5.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Command {
    /// Liveness probe. Engine replies with `Event::Pong`.
    Ping,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    /// Reply to `Command::Ping`.
    Pong,
    /// Recoverable error surface.
    Error { message: String },
}
