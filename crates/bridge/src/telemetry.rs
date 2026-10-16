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
use crate::event::{
    EngineStats, ErrorKind, LayoutDegradeReason, ReadWarningKind, SaveNote, SaveNoteKind,
};

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
    PaintTiming {
        p50: f32,
        p95: f32,
        p99: f32,
        /// Issue #500 (item 3) - the engine's layout caches as of this
        /// batch (`EngineStats.paragraph_cache_*` / `table_cache_*`, read
        /// when the batch flushes): bytes, entry counts and the hit ratio
        /// since the engine started, so a cache regression (#497) shows
        /// up in the field. Numbers only, no document content. Omitted
        /// from the wire when the engine did not answer the stats request,
        /// so a sender that predates it keeps its exact shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        layout_caches: Option<LayoutCacheTelemetry>,
    },
    /// RPC command-latency percentiles for one command `kind`.
    CommandTiming { kind: String, p50: f32, p95: f32 },
    /// A memory / performance counter snapshot.
    EngineStats(EngineStats),
    /// A recoverable or fatal engine error, classified coarsely.
    Error {
        code: ErrorCode,
        recoverable: bool,
        /// Issue #461 - the typed [`ErrorKind`] of an engine `Event::Error`
        /// refusal (#427 / #407), so refusal rates are visible per kind.
        /// `None` (omitted from the wire) for every sample that did not
        /// come from a typed engine reply - an untyped `Event::Error`
        /// stays `code: UNKNOWN` with no kind, the shell-synthesized
        /// codes (`CHECKPOINT_FAILED`, ...) keep their exact prior shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        kind: Option<ErrorKind>,
        /// Issue #469 - the wire name (`"APPLY_FORMATTING"`) of the command
        /// the refusal answered (`Event::Error.command`), so refusals can
        /// be counted per command x kind. A command name, never content.
        /// Omitted when unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        command: Option<String>,
    },
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
        /// Issue #329 — how many (family, script slot) font substitutions
        /// the opened document's layout makes (`Event::DocumentLoaded.
        /// substituted`): a count only, never the family names. Omitted
        /// from the wire when `None`, so a sender that predates it keeps
        /// the exact #86 shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[tsify(optional)]
        font_substitutions: Option<u32>,
        /// Issue #406 — the reader's warnings for this open, as per-kind
        /// counts (`Event::DocumentLoaded::warnings` folded by kind, kinds
        /// in ascending order): how often field documents open degraded,
        /// and how. Codes and counts only — never a warning's detail (an
        /// attribute value is document bytes). Omitted from the wire when
        /// the open was clean, so a clean sample keeps its exact #86 shape.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[tsify(optional)]
        read_warnings: Vec<ReadWarningCount>,
    },
    /// Issue #485 - a save that carried write notes (`Event::DocumentSaved::
    /// notes`): the file was written REPAIRED. Only degraded saves sample
    /// (a clean save emits nothing), so the sample rate is the repair
    /// rate. `size_bytes` is the saved file's byte length, `save_notes` the
    /// per-kind counts (kinds ascending): codes and counts only, no
    /// document content.
    DocSave {
        size_bytes: u32,
        save_notes: Vec<SaveNoteCount>,
    },
}

/// Issue #500 (item 3) - the layout caches on
/// [`TelemetryKind::PaintTiming::layout_caches`]: the paragraph cache
/// (#457) and the content-keyed table box cache (#379). A hit ratio is
/// `hits / (hits + misses)` over the engine's lifetime (`0.0..=1.0`),
/// omitted while the cache has seen no lookup.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default, PartialEq)]
#[serde(default)]
pub struct LayoutCacheTelemetry {
    /// Estimated bytes held by the paragraph cache.
    pub paragraph_bytes: u64,
    /// Paragraph boxes cached.
    pub paragraph_entries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub paragraph_hit_ratio: Option<f32>,
    /// Estimated bytes held by the table box cache.
    pub table_bytes: u64,
    /// Table boxes cached.
    pub table_entries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub table_hit_ratio: Option<f32>,
}

/// Issue #500 - `hits / (hits + misses)`, `None` before the first lookup.
pub fn cache_hit_ratio(hits: u64, misses: u64) -> Option<f32> {
    let total = hits.saturating_add(misses);
    (total > 0).then(|| (hits as f64 / total as f64) as f32)
}

impl LayoutCacheTelemetry {
    /// The sample a stats reply stands for.
    pub fn from_stats(stats: &EngineStats) -> Self {
        Self {
            paragraph_bytes: stats.paragraph_cache_bytes,
            paragraph_entries: stats.paragraph_cache_entries,
            paragraph_hit_ratio: cache_hit_ratio(
                stats.paragraph_cache_hits,
                stats.paragraph_cache_misses,
            ),
            table_bytes: stats.table_cache_bytes,
            table_entries: stats.table_cache_entries,
            table_hit_ratio: cache_hit_ratio(stats.table_cache_hits, stats.table_cache_misses),
        }
    }
}

/// Issue #485 - one note kind's count on [`TelemetryKind::DocSave::
/// save_notes`].
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveNoteCount {
    pub kind: SaveNoteKind,
    pub count: u32,
}

/// Issue #485 - fold a save's notes into per-kind counts, kinds in
/// ascending ([`SaveNoteKind`] declaration) order, each kind once.
pub fn save_note_counts(notes: &[SaveNote]) -> Vec<SaveNoteCount> {
    let mut counts: std::collections::BTreeMap<SaveNoteKind, u32> =
        std::collections::BTreeMap::new();
    for n in notes {
        let c = counts.entry(n.kind()).or_insert(0);
        *c = c.saturating_add(n.count().max(1));
    }
    counts
        .into_iter()
        .map(|(kind, count)| SaveNoteCount { kind, count })
        .collect()
}

/// Issue #406 — one warning kind's count on
/// [`TelemetryKind::DocOpen::read_warnings`].
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadWarningCount {
    pub kind: ReadWarningKind,
    pub count: u32,
}

/// Issue #406 — fold an open's warnings into per-kind counts, kinds in
/// ascending ([`ReadWarningKind`] declaration) order, each kind once.
pub fn read_warning_counts(warnings: &[crate::ReadWarning]) -> Vec<ReadWarningCount> {
    let mut counts: std::collections::BTreeMap<ReadWarningKind, u32> =
        std::collections::BTreeMap::new();
    for w in warnings {
        let c = counts.entry(w.kind).or_insert(0);
        *c = c.saturating_add(w.count.max(1));
    }
    counts
        .into_iter()
        .map(|(kind, count)| ReadWarningCount { kind, count })
        .collect()
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
    /// Issue #502 - a page canvas lost its 2D context (one sample per
    /// loss; the worker repaints the page when the context is restored,
    /// and `Event::CanvasContext` says whether it ever was).
    CanvasContextLost,
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
            font_substitutions: None,
            read_warnings: vec![],
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
        /* Issue #329 — the substitution count rides along when known. */
        let kind = TelemetryKind::DocOpen {
            size_bytes: 1,
            page_count: 1,
            open_ms: 1.0,
            backend: "canvas2d".to_string(),
            font_substitutions: Some(3),
            read_warnings: vec![],
        };
        assert_eq!(roundtrip(&kind)["font_substitutions"], 3);
    }

    /// Issue #406 — a degraded open carries per-kind warning counts (codes
    /// + counts, no detail); a #86-era sample without the key decodes.
    #[test]
    fn doc_open_carries_read_warning_counts() {
        use crate::{ReadWarning, ReadWarningKind as K};
        let w = |kind, detail: &str, count| ReadWarning {
            kind,
            part: None,
            detail: detail.to_string(),
            count,
        };
        let counts = read_warning_counts(&[
            w(K::NonCanonicalNamespaces, "x", 1),
            w(K::MeasureClamped, "w:pgMar/@w:top", 2),
            w(K::MeasureClamped, "w:pgMar/@w:left", 1),
            w(K::InvalidMeasure, "w:tab/@w:pos", 0),
        ]);
        assert_eq!(
            counts,
            vec![
                ReadWarningCount {
                    kind: K::InvalidMeasure,
                    count: 1
                },
                ReadWarningCount {
                    kind: K::MeasureClamped,
                    count: 3
                },
                ReadWarningCount {
                    kind: K::NonCanonicalNamespaces,
                    count: 1
                },
            ]
        );
        let kind = TelemetryKind::DocOpen {
            size_bytes: 1,
            page_count: 1,
            open_ms: 1.0,
            backend: "canvas2d".to_string(),
            font_substitutions: None,
            read_warnings: counts,
        };
        let json = roundtrip(&kind);
        assert_eq!(
            json["read_warnings"],
            serde_json::json!([
                { "kind": "InvalidMeasure", "count": 1 },
                { "kind": "MeasureClamped", "count": 3 },
                { "kind": "NonCanonicalNamespaces", "count": 1 },
            ])
        );
        let legacy: TelemetryKind = serde_json::from_value(serde_json::json!({
            "type": "DOC_OPEN",
            "size_bytes": 1,
            "page_count": 1,
            "open_ms": 1.0,
            "backend": "vello",
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            TelemetryKind::DocOpen { read_warnings, .. } if read_warnings.is_empty()
        ));
    }

    #[test]
    fn paint_timing_carries_the_layout_caches_and_omits_them_when_absent() {
        // Issue #500 - additive `layout_caches` on the existing PAINT_TIMING
        // sample: a sender without it keeps the exact pre-#500 shape.
        let bare = TelemetryKind::PaintTiming {
            p50: 1.0,
            p95: 2.0,
            p99: 3.0,
            layout_caches: None,
        };
        assert_eq!(
            roundtrip(&bare),
            serde_json::json!({ "type": "PAINT_TIMING", "p50": 1.0, "p95": 2.0, "p99": 3.0 })
        );
        let stats = EngineStats {
            wasm_heap_bytes: 0,
            document_tree_bytes: 0,
            glyph_cache_entries: 0,
            undo_stack_depth: 0,
            fonts_resident: 0,
            last_paint_ms: 0.0,
            last_command_ms: 0.0,
            paragraph_count: 0,
            word_count: 0,
            character_count: 0,
            paragraph_cache_bytes: 1024,
            paragraph_cache_entries: 4,
            paragraph_cache_hits: 3,
            paragraph_cache_misses: 1,
            table_cache_bytes: 0,
            table_cache_entries: 0,
            table_cache_hits: 0,
            table_cache_misses: 0,
        };
        let kind = TelemetryKind::PaintTiming {
            p50: 1.0,
            p95: 2.0,
            p99: 3.0,
            layout_caches: Some(LayoutCacheTelemetry::from_stats(&stats)),
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({
                "type": "PAINT_TIMING", "p50": 1.0, "p95": 2.0, "p99": 3.0,
                "layout_caches": {
                    "paragraph_bytes": 1024, "paragraph_entries": 4,
                    "paragraph_hit_ratio": 0.75,
                    "table_bytes": 0, "table_entries": 0
                }
            })
        );
    }

    #[test]
    fn cache_hit_ratio_is_none_before_the_first_lookup() {
        assert_eq!(cache_hit_ratio(0, 0), None);
        assert_eq!(cache_hit_ratio(0, 5), Some(0.0));
        assert_eq!(cache_hit_ratio(5, 0), Some(1.0));
        assert_eq!(cache_hit_ratio(u64::MAX, u64::MAX), Some(0.5));
    }

    #[test]
    fn existing_kinds_are_unaffected_by_the_new_variants() {
        // Regression guard: adding variants to an internally-tagged enum
        // must not perturb the tag/field shape of the pre-existing arms.
        let kind = TelemetryKind::Error {
            code: ErrorCode::EngineTrap,
            recoverable: true,
            kind: None,
            command: None,
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "ENGINE_TRAP", "recoverable": true })
        );
    }

    #[test]
    fn typed_error_sample_carries_its_kind() {
        // Issue #461 - the kind rides the ERROR sample under its wire name.
        let kind = TelemetryKind::Error {
            code: ErrorCode::Unknown,
            recoverable: true,
            kind: Some(ErrorKind::InvalidArgument),
            command: None,
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "UNKNOWN", "recoverable": true, "kind": "InvalidArgument" })
        );
    }

    #[test]
    fn error_sample_carries_the_refused_command() {
        // Issue #469 - additive `command` (wire name), omitted when absent.
        let kind = TelemetryKind::Error {
            code: ErrorCode::Unknown,
            recoverable: true,
            kind: Some(ErrorKind::InvalidArgument),
            command: Some("APPLY_FORMATTING".to_string()),
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "UNKNOWN", "recoverable": true, "kind": "InvalidArgument", "command": "APPLY_FORMATTING" })
        );
    }

    #[test]
    fn checkpoint_failed_error_code_is_screaming_snake() {
        // Issue #333 - the TS collector mirrors this exact wire spelling.
        let kind = TelemetryKind::Error {
            code: ErrorCode::CheckpointFailed,
            recoverable: true,
            kind: None,
            command: None,
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
            kind: None,
            command: None,
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "JOURNAL_FAILED", "recoverable": true })
        );
    }

    #[test]
    fn canvas_context_lost_error_code_is_screaming_snake() {
        // Issue #502 - the TS collector mirrors this exact wire spelling.
        let kind = TelemetryKind::Error {
            code: ErrorCode::CanvasContextLost,
            recoverable: true,
            kind: None,
            command: None,
        };
        assert_eq!(
            roundtrip(&kind),
            serde_json::json!({ "type": "ERROR", "code": "CANVAS_CONTEXT_LOST", "recoverable": true })
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

    /// Issue #485 - a degraded save samples `DOC_SAVE` with per-kind
    /// counts; the kinds fold in declaration order, each once.
    #[test]
    fn doc_save_sample_carries_per_kind_counts() {
        use crate::event::SaveNote;
        let counts = save_note_counts(&[
            SaveNote::StaleMarkupClamped { markers: 2 },
            SaveNote::ForbiddenCharactersReplaced { count: 3 },
            SaveNote::StaleMarkupClamped { markers: 1 },
        ]);
        let kind = TelemetryKind::DocSave {
            size_bytes: 4096,
            save_notes: counts,
        };
        assert_eq!(
            serde_json::to_value(&kind).unwrap(),
            serde_json::json!({
                "type": "DOC_SAVE",
                "size_bytes": 4096,
                "save_notes": [
                    { "kind": "ForbiddenCharactersReplaced", "count": 3 },
                    { "kind": "StaleMarkupClamped", "count": 3 },
                ],
            })
        );
    }
}
