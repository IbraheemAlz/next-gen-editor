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
 */
import { createEffect, createMemo, createSignal, For, Show, type Component } from 'solid-js';
import {
    checkpointNotices,
    createEditorState,
    recoveryNotices,
    type RecoveryNoticeOptions,
    type RecoveryReport,
} from '@nge/core';
import './RecoveryBanner.css';

export interface RecoveryBannerProps {
    /** Wall-clock formatter for "changes after HH:MM" (default: the
     *  locale's hour + minute). */
    formatTime?: (ms: number) => string;
}

export const RecoveryBanner: Component<RecoveryBannerProps> = (props) => {
    const state = createEditorState();
    /* The report the user dismissed; a newer report shows again. */
    const [dismissed, setDismissed] = createSignal<RecoveryReport | undefined>(undefined);
    /* Issue #333 - the checkpoint warning is dismissed per failure run:
       a recovery to "ok" and a later failure shows it again. */
    const [checkpointDismissed, setCheckpointDismissed] = createSignal(false);
    createEffect(() => {
        if (!state.checkpointFailing()) setCheckpointDismissed(false);
    });
    const notices = createMemo(() => {
        const options: RecoveryNoticeOptions = {};
        if (props.formatTime) options.formatTime = props.formatTime;
        const recovery =
            dismissed() === state.lastRecovery() && state.lastRecovery() !== undefined
                ? []
                : recoveryNotices(state.lastRecovery(), options);
        const checkpoint = checkpointDismissed() ? [] : checkpointNotices(state.checkpointFailing());
        return [...recovery, ...checkpoint];
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
                                <p class="nge-recovery-banner__action">{notice.action}</p>
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
                    }}
                >
                    Dismiss
                </button>
            </div>
        </Show>
    );
};
