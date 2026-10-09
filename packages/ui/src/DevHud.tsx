/**
 * DevHud — corner overlay that surfaces engine perf + memory counters.
 *
 * Toggle: Ctrl+Shift+D (configurable via `hotkey` prop; pass `null` to
 * disable). Polls Command::RequestStats at the configured cadence and
 * mirrors the EngineStats payload + the latest PAINTED.paint_ms.
 *
 * QA value: every formatting / typing / open-doc operation is now
 * observable for heap growth + paint regression without devtools.
 */
import {
    createEffect,
    createSignal,
    onCleanup,
    Show,
    type Component,
} from 'solid-js';
import { createEditorCommands, createEditorState, type RecoveryReport } from '@nge/core';
import './DevHud.css';

export interface DevHudProps {
    /** Poll cadence in ms. Default 1000ms. Set high in prod, low in QA. */
    pollMs?: number;
    /** Keyboard chord (`Ctrl+Shift+D` by default). `null` disables hotkey. */
    hotkey?: string | null;
    /** Initial visibility. */
    defaultVisible?: boolean;
}

function fmtBytes(n: number | undefined): string {
    if (n === undefined) return '–';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
}

/** Issue #315 — the base the last recovery came back from. */
function recoveryBase(r: RecoveryReport): string {
    const cause = r.cause === 'renderer-retry' ? 'renderer retry · ' : '';
    const base = r.restored ? (r.pinnedBase ? 'pinned snapshot' : 'snapshot') : 'command log';
    const at =
        r.baseSnapshotAt !== undefined
            ? ` @ ${new Date(r.baseSnapshotAt).toLocaleTimeString()}`
            : '';
    return `${cause}${base}${at} +${r.appliedCommands} cmds`;
}

/** Issue #315 — every way the last recovery degraded (`[]` = none). */
function recoveryLosses(r: RecoveryReport): string[] {
    const losses: string[] = [];
    if (r.logTruncated) losses.push('log truncated');
    if (r.tailDropped) losses.push('tail dropped');
    if (r.packageLost) losses.push('package lost');
    if (r.journalGap !== undefined && r.journalGap > 0) {
        losses.push(`journal gap (${r.journalGap} cmds)`);
    }
    if (r.rendererDowngraded) losses.push('renderer downgraded');
    return losses;
}

export const DevHud: Component<DevHudProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [visible, setVisible] = createSignal(props.defaultVisible ?? false);

    /* Hotkey wiring. */
    createEffect(() => {
        const chord = props.hotkey ?? 'Ctrl+Shift+D';
        if (chord === null) return;
        const handler = (e: KeyboardEvent) => {
            const ctrl = e.ctrlKey || e.metaKey;
            if (ctrl && e.shiftKey && e.key.toLowerCase() === 'd') {
                e.preventDefault();
                setVisible((v) => !v);
            }
        };
        window.addEventListener('keydown', handler);
        /* Sprint 8 (UI Edition) — SettingsMenu dispatches a custom
         * event so the HUD can be toggled from the gear popover too. */
        const onMenuToggle = () => setVisible((v) => !v);
        window.addEventListener('nge-toggle-hud', onMenuToggle);
        onCleanup(() => {
            window.removeEventListener('keydown', handler);
            window.removeEventListener('nge-toggle-hud', onMenuToggle);
        });
    });

    /* Stats polling. Only runs while visible. */
    createEffect(() => {
        if (!visible()) return;
        const ms = props.pollMs ?? 1000;
        let cancelled = false;
        const tick = async () => {
            if (cancelled) return;
            try {
                await cmd.requestStats();
            } catch {
                /* worker may be trapped — next poll will retry */
            }
        };
        void tick();
        const id = window.setInterval(tick, ms);
        onCleanup(() => {
            cancelled = true;
            window.clearInterval(id);
        });
    });

    return (
        <Show when={visible()}>
            <div class="nge-hud" role="status" aria-live="off">
                <div class="nge-hud__title">
                    Engine HUD
                    <button
                        class="nge-hud__close"
                        type="button"
                        aria-label="Hide HUD"
                        onClick={() => setVisible(false)}
                    >
                        ×
                    </button>
                </div>
                <dl class="nge-hud__grid">
                    <dt>Renderer</dt>
                    <dd>{state.renderer()}</dd>

                    {/* Issue #99 — why this session is no longer on Vello.
                        Issue #240 — the downgrade is sticky across reloads
                        (persisted, 24 h decay); "Retry" forgets it and
                        probes the GPU again. Issue #270 — in place: the
                        engine restarts from the event log, the document
                        is kept (no reload). */}
                    <Show when={state.rendererDowngrade()}>
                        {(d) => (
                            <>
                                <dt>Fallback</dt>
                                <dd
                                    class="nge-hud__warn"
                                    title={`Forced ${d().to} after ${d().consecutive_traps} consecutive failures on ${d().from}; sticky across reloads for 24 h`}
                                >
                                    {d().from} → {d().to} ({d().consecutive_traps} traps)
                                </dd>
                                <Show when={cmd.canRetryGpuRenderer}>
                                    <dt />
                                    <dd>
                                        <button
                                            class="nge-hud__retry"
                                            type="button"
                                            title={`Forget the crash-loop record and restart the engine on ${d().from}; the document is kept`}
                                            onClick={() => void cmd.retryGpuRenderer()}
                                        >
                                            Retry {d().from}
                                        </button>
                                    </dd>
                                </Show>
                            </>
                        )}
                    </Show>

                    {/* Issue #315 — the most recent crash recovery: the base
                        it restored, the fallbacks it took, and every loss
                        (the same flags the recovery banner and the
                        telemetry CRASH sample carry). */}
                    <Show when={state.lastRecovery()}>
                        {(r) => (
                            <>
                                <dt>Recovery</dt>
                                <dd class="nge-hud__recovery-base">{recoveryBase(r())}</dd>
                                <dt>Fallbacks</dt>
                                <dd class="nge-hud__recovery-fallbacks">
                                    {r().snapshotFallbacks} snapshot · {r().packageFallbacks} package
                                </dd>
                                <dt>Losses</dt>
                                <dd
                                    class="nge-hud__recovery-losses"
                                    classList={{
                                        'nge-hud__recovery-losses--warn':
                                            recoveryLosses(r()).length > 0,
                                    }}
                                >
                                    {recoveryLosses(r()).join(', ') || 'none'}
                                </dd>
                            </>
                        )}
                    </Show>

                    {/* Issue #390 - the event log's health (the typed
                        `CheckpointState` event): checkpoints + the command
                        journal. */}
                    <dt>Event log</dt>
                    <dd
                        class="nge-hud__eventlog"
                        classList={{ 'nge-hud__warn': !state.checkpointState().ok }}
                        title={state.checkpointState().lastError ?? ''}
                    >
                        {state.checkpointState().ok
                            ? state.checkpointState().failures > 0
                                ? `retrying (${state.checkpointState().failures})`
                                : 'ok'
                            : state.checkpointState().journalFailing
                              ? 'journal failing'
                              : 'checkpoints failing'}
                    </dd>

                    <dt>WASM heap</dt>
                    <dd>{fmtBytes(state.stats()?.wasm_heap_bytes)}</dd>

                    <dt>Doc tree</dt>
                    <dd>{fmtBytes(state.stats()?.document_tree_bytes)}</dd>

                    <dt>Glyph cache</dt>
                    <dd>{state.stats()?.glyph_cache_entries ?? '–'}</dd>

                    <dt>Undo depth</dt>
                    <dd>{state.stats()?.undo_stack_depth ?? '–'}</dd>

                    <dt>Fonts</dt>
                    <dd>{state.stats()?.fonts_resident ?? '–'}</dd>

                    <dt>Last paint</dt>
                    <dd>{state.lastPaintMs().toFixed(2)} ms</dd>

                    <dt>Can undo</dt>
                    <dd>{state.canUndo() ? 'yes' : 'no'}</dd>

                    <dt>Can redo</dt>
                    <dd>{state.canRedo() ? 'yes' : 'no'}</dd>
                </dl>
            </div>
        </Show>
    );
};
