//! Telemetry schema — Phase 5 D5.7 (PHASE_5_HARDENING_RELEASE.md §11).
//!
//! The shape of the sampled, PII-free telemetry the UI batches and posts to a
//! collector. The MVP transport is a **mock**: the UI `console.log`s each
//! batch instead of POSTing to Grafana — see `ts/src/state/telemetry.ts`,
//! which mirrors this schema. Defining the types here keeps the schema
//! canonical and `tsify`-exported, ready for a real collector later.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::common::Script;
use crate::event::EngineStats;

/// One telemetry sample. `doc_id` is anonymized — never a document title or
/// path, only an opaque per-session identifier.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct TelemetryEvent {
    pub doc_id: String,
    pub kind: TelemetryKind,
    pub timestamp_ms: f64,
}

/// The payload a [`TelemetryEvent`] carries.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TelemetryKind {
    /// Paint-latency percentiles over a sampling window.
    PaintTiming { p50: f32, p95: f32, p99: f32 },
    /// RPC command-latency percentiles for one command `kind`.
    CommandTiming { kind: String, p50: f32, p95: f32 },
    /// A memory / performance counter snapshot.
    EngineStats(EngineStats),
    /// A recoverable or fatal engine error, classified coarsely.
    Error { code: ErrorCode, recoverable: bool },
    /// A font fallback — signals a missing font package for `script`.
    FontFallback {
        script: Script,
        requested: String,
        fallback: String,
    },
}

/// Coarse error classification for telemetry — carries no PII.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    EngineTrap,
    DocumentParse,
    FontLoad,
    Rpc,
    Unknown,
}

/// A batch of telemetry events — the unit the UI posts to the collector
/// (every 60 s, §11).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct TelemetryBatch {
    pub events: Vec<TelemetryEvent>,
    pub sent_at_ms: f64,
}
