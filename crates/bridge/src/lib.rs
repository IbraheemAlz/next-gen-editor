//! `bridge` — RPC command/event types shared between the WASM engine and TS.
//!
//! The Phase 2 schema (PHASE_2_BRIDGE_MEMORY.md §4–§5) is layered **additively**
//! on top of the Phase 1 PoC subset: the Phase 1 commands/events are retained
//! and marked `// TODO: Deprecate in Phase 3`, so the visual-diff goldens and
//! test harnesses keep passing until the §4 `RequestPaint` pipeline exists.
//!
//! Module layout:
//! - [`common`] — types shared by both `Command` and `Event`.
//! - [`command`] — the `Command` enum + command-only payload types.
//! - [`event`] — the `Event` enum + event-only payload types.
//!
//! `tsify-next` notes: `Option<T>` renders as `T | undefined` in the generated
//! `.d.ts` — TS callers MUST pass `undefined`, not `null`. Binary `Vec<u8>`
//! fields carry `#[serde(with = "serde_bytes")]` + `#[tsify(type = "Uint8Array")]`
//! so they cross the wasm boundary as a `Uint8Array` instead of a number array.

mod command;
mod common;
mod event;

pub use command::*;
pub use common::*;
pub use event::*;
