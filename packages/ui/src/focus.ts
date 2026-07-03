/**
 * focus — shared focus hand-back helper (issues #35/#49).
 *
 * The editor's hidden textarea is the app's single OS text-input
 * surface. It re-grabs focus on canvas pointerups, but any component
 * that transiently owns focus (a dialog, the comment draft popover,
 * a ruler drag) must hand focus back explicitly on teardown so typing
 * continues into the document.
 *
 * Two host shapes expose the textarea:
 *   - the app shell's `HiddenInput` marks it `data-nge-hidden-input`;
 *   - the SDK's `EditorSurface` defaults its id to `nge-hidden-input`.
 */
export function focusEditorInput(): void {
    document
        .querySelector<HTMLTextAreaElement>(
            '#nge-hidden-input, textarea[data-nge-hidden-input]',
        )
        ?.focus();
}
