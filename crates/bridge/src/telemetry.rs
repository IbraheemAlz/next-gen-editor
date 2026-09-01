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
use crate::event::{EngineStats, LayoutDegradeReason};

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
    /// Issue #87 — a paint was laid out degraded (one sample per note on
    /// `Event::Painted::layout_degraded`). Counts how often the layout
    /// self-defense fires in the field; carries no document content.
    LayoutDegraded {
        reason: LayoutDegradeReason,
        page: Option<u32>,
    },
    /// Issue #86 — a WASM trap (crash) plus enough breadcrumb context to
    /// reproduce it, without ever carrying document content: the trap
    /// message the worker surfaced, the **types** of the last N dispatched
    /// commands (never their payloads — `InsertText`'s `text` field would
    /// be document content), and whether crash recovery brought the
    /// session back. The worker cannot emit this itself (the WASM module
    /// that would build it is the thing that just crashed) — the TS
    /// collector synthesizes it from the bridge-shaped `Event::Trap`, the
    /// same way `EngineClient.onTrap` synthesizes `Event::Trap` itself.
    Crash {
        trap_message: String,
        recent_commands: Vec<String>,
        recovery_outcome: RecoveryOutcome,
    },
    /// Issue #86 — one document-open latency sample. `size_bytes` is the
    /// `.docx` archive's byte length (not its content); `backend` is the
    /// renderer picked at INIT (`"vello"` / `"canvas2d"`) — both carry no
    /// PII and no document content.
    DocOpen {
        size_bytes: u32,
        page_count: u32,
        open_ms: f32,
        backend: String,
    },
}

/// Outcome of a post-trap recovery attempt (`Command::Recover`), carried on
/// [`TelemetryKind::Crash`]. `Pending` covers the (rare, short) window a
/// telemetry batch is flushed before recovery has resolved either way —
/// e.g. the page unloads mid-recovery on a `visibilitychange` flush.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RecoveryOutcome {
    Recovered,
    Failed,
    Pending,
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

#[cfg(test)]
mod tests {
    //! Issue #86 — serde shape tests for the new sample kinds. These pin
    //! the exact wire JSON `ts/src/state/telemetry.ts` hand-mirrors (this
    //! crate is never built as the wasm-bindgen target, so nothing
    //! codegens the `.d.ts` for TS to import — see the module doc).
    use super::*;

    fn roundtrip(kind: &TelemetryKind) -> serde_json::Value {
        let json = serde_json::to_value(kind).expect("serialize TelemetryKind");
        let back: TelemetryKind =
            serde_json::from_value(json.clone()).expect("deserialize TelemetryKind");
        // Re-serialize the round-tripped value too — catches an enum arm
        // that silently changes shape between ser and de.
        assert_eq!(json, serde_json::to_value(&back).unwrap());
        json
    }

    #[test]
    fn crash_kind_is_screaming_snake_tagged_with_no_extraneous_fields() {
        let kind = TelemetryKind::Crash {
            trap_message: "RuntimeError: unreachable".to_string(),
            recent_commands: vec!["INSERT_TEXT".to_string(), "APPLY_FORMATTING".to_string()],
            recovery_outcome: RecoveryOutcome::Recovered,
        };
        let json = roundtrip(&kind);
        assert_eq!(
            json,
            serde_json::json!({
                "type": "CRASH",
                "trap_message": "RuntimeError: unreachable",
                "recent_commands": ["INSERT_TEXT", "APPLY_FORMATTING"],
                "recovery_outcome": "RECOVERED",
            })
        );
    }

    #[test]
    fn crash_kind_carries_no_pii_shaped_fields() {
        // Structural guard, not a content scan: the variant must expose
        // only the three documented fields — nothing shaped like a
        // document body, a path, or a user identifier could hide behind
        // an added field without this test's `json!{}` comparison above
        // failing first. This test asserts the *keys*, independent of
        // any particular value.
        let kind = TelemetryKind::Crash {
            trap_message: String::new(),
            recent_commands: vec![],
            recovery_outcome: RecoveryOutcome::Pending,
        };
        let json = serde_json::to_value(&kind).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "recent_commands",
                "recovery_outcome",
                "trap_message",
                "type"
            ]
        );
    }

    #[test]
    fn recovery_outcome_variants_are_screaming_snake_case() {
        for (outcome, expected) in [
            (RecoveryOutcome::Recovered, "\"RECOVERED\""),
            (RecoveryOutcome::Failed, "\"FAILED\""),
            (RecoveryOutcome::Pending, "\"PENDING\""),
        ] {
            assert_eq!(serde_json::to_string(&outcome).unwrap(), expected);
        }
    }

    #[test]
    fn doc_open_kind_is_screaming_snake_tagged() {
        let kind = TelemetryKind::DocOpen {
            size_bytes: 123_456,
            page_count: 12,
            open_ms: 87.5,
            backend: "vello".to_string(),
        };
        let json = roundtrip(&kind);
        assert_eq!(
            json,
            serde_json::json!({
                "type": "DOC_OPEN",
                "size_bytes": 123_456,
                "page_count": 12,
                "open_ms": 87.5,
                "backend": "vello",
            })
        );
    }

    #[test]
    fn existing_kinds_are_unaffected_by_the_new_variants() {
        // Regression guard: adding variants to an internally-tagged enum
        // must not perturb the tag/field shape of the pre-existing arms.
        let kind = TelemetryKind::Error {
            code: ErrorCode::EngineTrap,
            recoverable: true,
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "ENGINE_TRAP", "recoverable": true })
        );
    }

    #[test]
    fn telemetry_batch_round_trips_a_crash_event() {
        let batch = TelemetryBatch {
            events: vec![TelemetryEvent {
                doc_id: "anon-abc123".to_string(),
                kind: TelemetryKind::Crash {
                    trap_message: "unreachable".to_string(),
                    recent_commands: vec!["UNDO".to_string()],
                    recovery_outcome: RecoveryOutcome::Failed,
                },
                timestamp_ms: 42.0,
            }],
            sent_at_ms: 100.0,
        };
        let json = serde_json::to_value(&batch).unwrap();
        let back: TelemetryBatch = serde_json::from_value(json).unwrap();
        assert_eq!(back.events.len(), 1);
        assert_eq!(back.sent_at_ms, 100.0);
        assert!(matches!(
            back.events[0].kind,
            TelemetryKind::Crash {
                recovery_outcome: RecoveryOutcome::Failed,
                ..
            }
        ));
    }
}
