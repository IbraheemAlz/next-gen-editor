/**
 * RecoveryBanner — issue #315: the visible half of a DEGRADED crash
 * recovery.
 *
 * #241 / #268 report every way a recovery can degrade on the engine's
 * recovery report, but until now only the console said so: a document
 * restored from an earlier snapshot (later edits lost), a source package
 * that could not be re-attached (saving drops the original file's sibling
 * parts), a log too truncated to rebuild anything, or a crash-loop switch
 * to the compatibility renderer all looked like a normal recovery. This
 * dismissible `role="alert"` banner names what was lost and what to do.
 *
 * Normal recoveries stay silent: the notices come from `@nge/core`'s
 * `recoveryNotices()`, which returns nothing for a recovery that lost
 * nothing (an older-snapshot fallback that still replayed its full tail
 * included — the Dev HUD shows those). A newer recovery replaces a
 * dismissed banner's content and shows it again.
 *
 * Issue #485 - the same banner carries the writer's save report: a save
 * that replaced characters (or clamped markup) says so, one line per note
 * kind (`data-kind="save-notes"`), until dismissed or the next save.
 */
import { createEffect, createMemo, createSignal, For, Show, type Component } from 'solid-js';
import {
    checkpointNotices,
    createEditorState,
    previousSessionNotice,
    recoveryNotices,
    saveNotices,
    useEngine,
    type RecoveryNoticeOptions,
    type RecoveryReport,
    type SaveNote,
} from '@nge/core';
import './RecoveryBanner.css';

function localTime(ms: number): string {
    return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export interface RecoveryBannerProps {
    /** Wall-clock formatter for "changes after HH:MM" (default: the
     *  locale's hour + minute). */
    formatTime?: (ms: number) => string;
}

export const RecoveryBanner: Component<RecoveryBannerProps> = (props) => {
    const state = createEditorState();
    const engine = useEngine();
    /* Issue #388 - "Recover previous document?" is dismissed per offer
       (the session stays archived: the next boot offers it again). */
    const [previousDismissed, setPreviousDismissed] = createSignal(false);
    const [busy, setBusy] = createSignal(false);
    createEffect(() => {
        if (state.previousSession() === undefined) setPreviousDismissed(false);
    });
    const decide = async (action: 'recover' | 'discard', id?: string): Promise<void> => {
        if (busy()) return;
        setBusy(true);
        try {
            if (action === 'recover') await engine.recoverPreviousSession?.(id);
            else await engine.discardPreviousSession?.(id);
        } catch (e: unknown) {
            console.error(`[recovery] previous session ${action} failed`, e);
        } finally {
            setBusy(false);
        }
    };
    /* The report the user dismissed; a newer report shows again. */
    const [dismissed, setDismissed] = createSignal<RecoveryReport | undefined>(undefined);
    /* Issue #333 - the checkpoint warning is dismissed per failure run:
       a recovery to "ok" and a later failure shows it again. */
    const [checkpointDismissed, setCheckpointDismissed] = createSignal(false);
    createEffect(() => {
        if (state.checkpointState().ok) setCheckpointDismissed(false);
    });
    /* Issue #485 - a repaired save is dismissed per save: the notes array
       is replaced by every save, so the next repaired save shows again. */
    const [saveDismissed, setSaveDismissed] = createSignal<SaveNote[] | undefined>(undefined);
    const notices = createMemo(() => {
        const options: RecoveryNoticeOptions = {};
        if (props.formatTime) options.formatTime = props.formatTime;
        const recovery =
            dismissed() === state.lastRecovery() && state.lastRecovery() !== undefined
                ? []
                : recoveryNotices(state.lastRecovery(), options);
        /* Issue #390 - the typed `CheckpointState`: a failed checkpoint, or
           (worse) a failed command journal. */
        const health = state.checkpointState();
        const checkpoint = checkpointDismissed()
            ? []
            : checkpointNotices(!health.ok, health.journalFailing);
        const previous = previousDismissed()
            ? []
            : previousSessionNotice(state.previousSessions(), options);
        const saved =
            saveDismissed() === state.lastSaveNotes() ? [] : saveNotices(state.lastSaveNotes());
        return [...previous, ...recovery, ...checkpoint, ...saved];
    });
    const visible = createMemo(() => notices().length > 0);

    return (
        <Show when={visible()}>
            <div
                class="nge-recovery-banner"
                role="alert"
                data-kinds={notices()
                    .map((n) => n.kind)
                    .join(' ')}
            >
                <span class="nge-recovery-banner__icon" aria-hidden="true">
                    !
                </span>
                <div class="nge-recovery-banner__body">
                    <For each={notices()}>
                        {(notice) => (
                            <section
                                class="nge-recovery-banner__notice"
                                data-kind={notice.kind}
                            >
                                <strong class="nge-recovery-banner__title">
                                    {notice.title}
                                </strong>
                                <p class="nge-recovery-banner__detail">{notice.detail}</p>
                                <Show when={notice.lines}>
                                    {(lines) => (
                                        <ul class="nge-recovery-banner__lines">
                                            <For each={lines()}>{(l) => <li>{l}</li>}</For>
                                        </ul>
                                    )}
                                </Show>
                                <p class="nge-recovery-banner__action">{notice.action}</p>
                                <Show when={notice.kind === 'previous-session'}>
                                    <For each={state.previousSessions()}>
                                        {(entry) => (
                                            <div
                                                class="nge-recovery-banner__choices"
                                                data-session-id={entry.id}
                                            >
                                                <Show when={state.previousSessions().length > 1}>
                                                    <span class="nge-recovery-banner__session">
                                                        {`Last edit ${(props.formatTime ?? localTime)(
                                                            entry.lastEditAt ?? entry.archivedAt,
                                                        )} \u00b7 ${entry.commandCount} edits`}
                                                    </span>
                                                </Show>
                                                <button
                                                    class="nge-btn nge-recovery-banner__recover"
                                                    type="button"
                                                    disabled={busy()}
                                                    onClick={() => void decide('recover', entry.id)}
                                                >
                                                    Recover
                                                </button>
                                                <button
                                                    class="nge-btn nge-recovery-banner__discard"
                                                    type="button"
                                                    disabled={busy()}
                                                    onClick={() => void decide('discard', entry.id)}
                                                >
                                                    Discard
                                                </button>
                                            </div>
                                        )}
                                    </For>
                                </Show>
                            </section>
                        )}
                    </For>
                </div>
                <button
                    class="nge-btn nge-recovery-banner__dismiss"
                    type="button"
                    aria-label="Dismiss recovery notice"
                    onClick={() => {
                        setDismissed(() => state.lastRecovery());
                        setCheckpointDismissed(true);
                        setSaveDismissed(() => state.lastSaveNotes());
                        setPreviousDismissed(true);
                        void engine.dismissPreviousSessions?.();
                    }}
                >
                    Dismiss
                </button>
            </div>
        </Show>
    );
};
