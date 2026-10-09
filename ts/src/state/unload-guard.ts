/* Issue #388 - `beforeunload` guard for unsaved edits.
 *
 * A plain reload / tab close starts a new session, and a new session
 * clears the event log. While the document differs from the last save
 * (`EngineClient.hasUnsavedChanges`, folded from the same `clean` rule
 * the worker persists) the browser's "unsaved changes" prompt is raised.
 * The next boot additionally offers the session back (`RecoveryBanner`),
 * so a user who ignores the prompt still loses nothing.
 *
 * Host-configurable: a host that autosaves turns it off with
 * `attachUnloadGuard(client, { enabled: false })`, or a build with
 * `VITE_NGE_UNLOAD_GUARD=0`. A reload that carries the document
 * (`prepareCarryOver`, the crash overlay's "Reload page") is not
 * guarded - the client reports no unsaved changes while one is prepared. */
export interface UnloadGuardClient {
    readonly hasUnsavedChanges: boolean;
}

export interface UnloadGuardOptions {
    /** Default: on, unless the build set `VITE_NGE_UNLOAD_GUARD=0`. */
    enabled?: boolean;
}

/** Install the guard; returns the uninstall function. */
export function attachUnloadGuard(
    client: UnloadGuardClient,
    options: UnloadGuardOptions = {},
): () => void {
    const enabled = options.enabled ?? import.meta.env.VITE_NGE_UNLOAD_GUARD !== '0';
    if (!enabled) return () => undefined;
    const onBeforeUnload = (e: BeforeUnloadEvent): void => {
        if (!client.hasUnsavedChanges) return;
        /* The legacy `returnValue` assignment is what makes Chrome prompt;
           the text itself is never shown. */
        e.preventDefault();
        e.returnValue = '';
    };
    window.addEventListener('beforeunload', onBeforeUnload);
    return () => window.removeEventListener('beforeunload', onBeforeUnload);
}
