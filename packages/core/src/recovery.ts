/**
 * Issue #315 — turn a crash-recovery report into user-facing notices.
 *
 * #241 / #268 made every degraded recovery observable on the recovery
 * report, but only the console said so: a document restored from an
 * earlier snapshot, a source package that could not be re-attached, or a
 * log too truncated to rebuild anything all looked like a normal
 * recovery. Honest UX: a loss the user must act on is shown, with what
 * was lost and what to do. A recovery that lost nothing yields no notice
 * — the banner stays silent (a fallback to an older snapshot that still
 * replayed its full tail lost nothing; the Dev HUD shows it instead).
 *
 * Headless and pure: `@nge/ui`'s `RecoveryBanner` renders the list, and a
 * host with its own chrome can render it however it likes.
 */
import type { PreviousSessionInfo, RecoveryReport } from './types';

export type RecoveryNoticeKind =
    | 'log-truncated'
    | 'tail-dropped'
    | 'package-lost'
    | 'renderer-downgrade'
    | 'checkpoint-failing'
    | 'previous-session';

export interface RecoveryNotice {
    kind: RecoveryNoticeKind;
    /** One-line statement of what happened. */
    title: string;
    /** What was lost (or changed), concretely. */
    detail: string;
    /** What the user should do about it. */
    action: string;
}

export interface RecoveryNoticeOptions {
    /** Wall-clock formatter for the snapshot time (default: the locale's
     *  `HH:MM`). */
    formatTime?: (ms: number) => string;
}

function defaultTime(ms: number): string {
    return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/**
 * The notices a recovery report warrants, most severe first; `[]` for a
 * recovery that lost nothing (and for `undefined` — no recovery yet).
 */
export function recoveryNotices(
    report: RecoveryReport | undefined,
    options: RecoveryNoticeOptions = {},
): RecoveryNotice[] {
    if (!report) return [];
    const formatTime = options.formatTime ?? defaultTime;
    const notices: RecoveryNotice[] = [];
    if (report.logTruncated) {
        notices.push({
            kind: 'log-truncated',
            title: 'Your document could not be recovered',
            detail:
                'The editor crashed and none of its recovery points could be read, so it ' +
                'started over with a new document. Changes since you last saved are lost.',
            action: 'Open your last saved copy to continue working.',
        });
    }
    if (report.tailDropped) {
        const when =
            report.baseSnapshotAt !== undefined
                ? `as it was at ${formatTime(report.baseSnapshotAt)}`
                : 'as it was at an earlier recovery point';
        notices.push({
            kind: 'tail-dropped',
            title: 'Recovered an earlier version of your document',
            detail:
                `The editor crashed and could only restore the document ${when}; ` +
                'changes made after that were lost.',
            action: 'Review the document and redo your most recent edits before saving.',
        });
    }
    if (report.packageLost) {
        notices.push({
            kind: 'package-lost',
            title: 'Parts of the original file could not be restored',
            detail:
                'After the crash, the original file’s styles, headers, settings and embedded ' +
                'files could not be re-attached. Saving now writes a minimal document ' +
                'without them.',
            action:
                'Use Save As to keep this version as a new file instead of overwriting the ' +
                'original, which still has them.',
        });
    }
    if (report.rendererDowngraded && report.rendererDowngrade) {
        const d = report.rendererDowngrade;
        notices.push({
            kind: 'renderer-downgrade',
            title: 'Switched to the compatibility renderer',
            detail:
                `The ${d.from} renderer crashed ${d.consecutive_traps} times in a row, so the ` +
                `editor now draws with ${d.to}. Your document is intact.`,
            action:
                'Drawing may be slower. To try the GPU renderer again, use Retry in the ' +
                'Dev HUD (Ctrl+Shift+D).',
        });
    }
    return notices;
}

/** Issue #315 — whether a recovery report describes a degraded recovery
 *  (anything `recoveryNotices` would show). */
export function recoveryDegraded(report: RecoveryReport | undefined): boolean {
    return recoveryNotices(report).length > 0;
}

/**
 * Issue #333 - the warning for event-log checkpoints that stopped
 * landing (the worker's bounded snapshot-write retries are exhausted):
 * nothing is lost yet, but a crash now would replay a long tail or lose
 * work, so the user is told to save. `[]` while checkpoints are fine.
 */
export function checkpointNotices(failing: boolean): RecoveryNotice[] {
    if (!failing) return [];
    return [
        {
            kind: 'checkpoint-failing',
            title: 'Changes are not being checkpointed',
            detail:
                'The editor could not write its automatic recovery points (the browser ' +
                'storage may be full or blocked). If the editor crashes now, recent edits ' +
                'may be lost.',
            action: 'Save your work now.',
        },
    ];
}

/**
 * Issue #388 - the offer to bring back a previous page generation's
 * unsaved session (a plain reload starts a new session, which would
 * otherwise have dropped it). Non-destructive until the user decides:
 * Recover swaps it in, Discard throws it away; ignoring the banner keeps
 * it for the next boot.
 */
export function previousSessionNotice(
    info: PreviousSessionInfo | undefined,
    options: RecoveryNoticeOptions = {},
): RecoveryNotice[] {
    if (!info) return [];
    const formatTime = options.formatTime ?? defaultTime;
    const when = formatTime(info.lastEditAt ?? info.archivedAt);
    return [
        {
            kind: 'previous-session',
            title: 'Recover previous document?',
            detail:
                `The page was closed or reloaded with unsaved changes (last edit around ${when}). ` +
                'They have been kept aside; this new session starts from a blank document.',
            action: 'Recover them to carry on where you left off, or discard them.',
        },
    ];
}
