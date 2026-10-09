//! Telemetry schema — Phase 5 D5.7 (PHASE_5_HARDENING_RELEASE.md §11).
//!
//! The shape of the sampled, PII-free telemetry the UI batches and posts to a
//! collector. The MVP transport is a **mock**: the UI `console.log`s each
//! batch instead of POSTing to Grafana — see `ts/src/state/telemetry.ts`,
//! which mirrors this schema. Defining the types here keeps the schema
//! canonical and `tsify`-exported, ready for a real collector later.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::common::{RendererDowngrade, Script};
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
        /// Issue #99 — present when this trap tipped the session into a
        /// crash-loop renderer downgrade (the recovery booted Canvas2D
        /// instead of re-probing Vello). Backend names + a count only —
        /// no document content. Omitted from the wire when `None`, so an
        /// ordinary crash sample keeps its exact #86 shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        renderer_downgrade: Option<RendererDowngrade>,
        /// Issue #315 — what the recovery this trap triggered had to give
        /// up (booleans + counts only, no document content): present on a
        /// `Recovered` sample, omitted while `Pending` / on `Failed`, so a
        /// `Pending` sample keeps its exact #86 shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        recovery: Option<RecoveryFlags>,
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

/// Issue #315 — the shape of one completed crash recovery, carried on
/// [`TelemetryKind::Crash::recovery`]. Mirrors the shell's `RecoveryInfo`
/// (#241 / #268): which base the session came back from and every way it
/// degraded. Booleans and counts only — no PII, no document content.
/// Every field defaults, so a collector tolerates a sender that predates
/// any one of them.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct RecoveryFlags {
    /// A persisted base snapshot was restored (`false`: the command log
    /// alone was replayed, or nothing could be).
    pub snapshot_restored: bool,
    /// The restored base was the document's pinned snapshot (#268).
    pub pinned_base: bool,
    /// The pinned base was restored WITHOUT its pruned tail: edits made
    /// after it were lost (#268).
    pub tail_dropped: bool,
    /// The recovered document lost its retained source package: saving
    /// goes through the minimal writer (#268).
    pub package_lost: bool,
    /// No snapshot restored and the log no longer reached its first
    /// command: the document could not be rebuilt at all (#241).
    pub log_truncated: bool,
    /// Newer snapshots skipped because they would not restore (#241).
    pub snapshot_fallbacks: u32,
    /// Readable snapshots passed over for an older base that still had
    /// its package (#268).
    pub package_fallbacks: u32,
    /// Issue #390 / #427 - logged commands after the restored base whose
    /// journal row was never written, so the recovery could not replay
    /// them (a count, `0` = none). Additive: a sender that predates it
    /// decodes as `0`.
    pub journal_gap: u32,
}

/// Coarse error classification for telemetry — carries no PII.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    EngineTrap,
    DocumentParse,
    FontLoad,
    Rpc,
    /// Issue #333 - a persisted event-log snapshot write failed (the
    /// worker retries with backoff; each failure is one sample).
    CheckpointFailed,
    /// Issue #390 - the worker's command journal (`appendCommand`) or an
    /// engine-side `SNAPSHOT` dispatch kept failing after its bounded
    /// retries: a recovery now would miss commands.
    JournalFailed,
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
            renderer_downgrade: None,
            recovery: None,
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
            renderer_downgrade: None,
            recovery: None,
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
    fn crash_kind_carries_the_renderer_downgrade_when_one_happened() {
        let kind = TelemetryKind::Crash {
            trap_message: "RuntimeError: unreachable".to_string(),
            recent_commands: vec!["PING".to_string()],
            recovery_outcome: RecoveryOutcome::Recovered,
            renderer_downgrade: Some(RendererDowngrade {
                from: "vello".to_string(),
                to: "canvas2d".to_string(),
                reason: crate::common::RendererDowngradeReason::CrashLoop,
                consecutive_traps: 2,
            }),
            recovery: None,
        };
        let json = roundtrip(&kind);
        assert_eq!(
            json["renderer_downgrade"],
            serde_json::json!({
                "from": "vello",
                "to": "canvas2d",
                "reason": "CRASH_LOOP",
                "consecutive_traps": 2,
            })
        );
        /* A #86-era sample without the key still decodes. */
        let legacy: TelemetryKind = serde_json::from_value(serde_json::json!({
            "type": "CRASH",
            "trap_message": "x",
            "recent_commands": [],
            "recovery_outcome": "FAILED",
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            TelemetryKind::Crash {
                renderer_downgrade: None,
                ..
            }
        ));
    }

    /// Issue #315 — a recovered sample carries the recovery's flags as
    /// plain booleans + counts; a sample without the key (every #86-era
    /// sender, and every `Pending` sample) still decodes, and a partial
    /// flags object fills the missing fields with defaults.
    #[test]
    fn crash_kind_carries_the_recovery_flags() {
        let kind = TelemetryKind::Crash {
            trap_message: "RuntimeError: unreachable".to_string(),
            recent_commands: vec!["INSERT_TEXT".to_string()],
            recovery_outcome: RecoveryOutcome::Recovered,
            renderer_downgrade: None,
            recovery: Some(RecoveryFlags {
                snapshot_restored: true,
                pinned_base: true,
                tail_dropped: true,
                package_lost: false,
                log_truncated: false,
                snapshot_fallbacks: 3,
                package_fallbacks: 0,
                journal_gap: 2,
            }),
        };
        let json = roundtrip(&kind);
        assert_eq!(
            json["recovery"],
            serde_json::json!({
                "snapshot_restored": true,
                "pinned_base": true,
                "tail_dropped": true,
                "package_lost": false,
                "log_truncated": false,
                "snapshot_fallbacks": 3,
                "package_fallbacks": 0,
                "journal_gap": 2,
            })
        );
        assert!(json.get("renderer_downgrade").is_none());

        let legacy: TelemetryKind = serde_json::from_value(serde_json::json!({
            "type": "CRASH",
            "trap_message": "x",
            "recent_commands": [],
            "recovery_outcome": "PENDING",
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            TelemetryKind::Crash { recovery: None, .. }
        ));

        let partial: TelemetryKind = serde_json::from_value(serde_json::json!({
            "type": "CRASH",
            "trap_message": "x",
            "recent_commands": [],
            "recovery_outcome": "RECOVERED",
            "recovery": { "package_lost": true },
        }))
        .unwrap();
        let TelemetryKind::Crash {
            recovery: Some(flags),
            ..
        } = partial
        else {
            panic!("recovery flags decoded");
        };
        assert_eq!(
            flags,
            RecoveryFlags {
                package_lost: true,
                ..RecoveryFlags::default()
            }
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
    fn checkpoint_failed_error_code_is_screaming_snake() {
        // Issue #333 - the TS collector mirrors this exact wire spelling.
        let kind = TelemetryKind::Error {
            code: ErrorCode::CheckpointFailed,
            recoverable: true,
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "CHECKPOINT_FAILED", "recoverable": true })
        );
    }

    #[test]
    fn journal_failed_error_code_is_screaming_snake() {
        // Issue #390 - the TS collector mirrors this exact wire spelling.
        let kind = TelemetryKind::Error {
            code: ErrorCode::JournalFailed,
            recoverable: true,
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "JOURNAL_FAILED", "recoverable": true })
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
                    renderer_downgrade: None,
                    recovery: None,
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
