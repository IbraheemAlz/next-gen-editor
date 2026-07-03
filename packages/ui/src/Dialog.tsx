/**
 * Dialog — modal primitive built on solid-js/web `<Portal>`.
 *
 * Renders backdrop + chrome at document.body level so the modal is
 * never clipped by the editor surface's z-index stack. Backdrop click
 * + ESC keypress both dismiss; pass a tabbable element in the body
 * with `autofocus` for keyboard-first flows.
 *
 * Composition slots:
 *   - `title` (string) — header text + close button row
 *   - `children` — body content
 *   - `footer` (JSX) — action button row pinned to the bottom
 *
 * Strict CSS namespace: `.nge-dialog-overlay` + `.nge-dialog`. Theme
 * tokens drive every visual constant; no external UI library.
 */
import {
    createEffect,
    onCleanup,
    Show,
    type JSX,
    type ParentComponent,
} from 'solid-js';
import { Portal } from 'solid-js/web';
import { focusEditorInput } from './focus';
import './Dialog.css';

/* Everything a Tab press can land on inside the dialog chrome. */
const FOCUSABLE =
    'a[href], button:not([disabled]), input:not([disabled]), ' +
    'select:not([disabled]), textarea:not([disabled]), ' +
    '[tabindex]:not([tabindex="-1"])';

export interface DialogProps {
    open: boolean;
    title: string;
    onClose: () => void;
    /** Optional footer slot — pinned action row. */
    footer?: JSX.Element | undefined;
    /** Width preset. `sm` = 320, `md` = 480, `lg` = 640. Default `md`. */
    size?: 'sm' | 'md' | 'lg' | undefined;
    /** Whether ESC closes. Default true. */
    dismissOnEsc?: boolean | undefined;
    /** Whether backdrop click closes. Default true. */
    dismissOnBackdrop?: boolean | undefined;
    /** ARIA-labelled body description, optional. */
    description?: string | undefined;
}

export const Dialog: ParentComponent<DialogProps> = (props) => {
    let dialogEl: HTMLDivElement | undefined;

    /* Focus trap (issues #35/#49). While open: seed focus into the body's
       `autofocus` element (or the first focusable), keep Tab cycling
       inside the chrome, and hand focus back on close — to whatever held
       it before opening (normally the editor's hidden textarea), with
       `focusEditorInput()` as the fallback when that element is gone. */
    createEffect(() => {
        if (!props.open) return;
        const prev =
            document.activeElement instanceof HTMLElement
                ? document.activeElement
                : null;
        /* The effect runs post-render, so the Portal DOM exists. Prefer
           the body's first control — seeding on the header's × close
           button is technically valid but useless for keyboard-first
           entry into forms like Page Setup. */
        const seed =
            dialogEl?.querySelector<HTMLElement>('[autofocus]') ??
            dialogEl?.querySelector<HTMLElement>(
                `.nge-dialog__body :is(${FOCUSABLE})`,
            ) ??
            dialogEl?.querySelector<HTMLElement>(FOCUSABLE);
        seed?.focus();

        const onTab = (e: KeyboardEvent) => {
            if (e.key !== 'Tab' || !dialogEl) return;
            const items = Array.from(
                dialogEl.querySelectorAll<HTMLElement>(FOCUSABLE),
            ).filter((el) => el.offsetParent !== null);
            const first = items[0];
            const last = items[items.length - 1];
            if (!first || !last) return;
            const active = document.activeElement;
            const inside = active instanceof Node && dialogEl.contains(active);
            if (e.shiftKey && (active === first || !inside)) {
                e.preventDefault();
                last.focus();
            } else if (!e.shiftKey && (active === last || !inside)) {
                e.preventDefault();
                first.focus();
            }
        };
        window.addEventListener('keydown', onTab);
        onCleanup(() => {
            window.removeEventListener('keydown', onTab);
            if (prev?.isConnected) prev.focus();
            else focusEditorInput();
        });
    });

    /* ESC handler. Bound only while the dialog is open. */
    createEffect(() => {
        if (!props.open) return;
        if (props.dismissOnEsc === false) return;
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                e.preventDefault();
                props.onClose();
            }
        };
        window.addEventListener('keydown', handler);
        onCleanup(() => window.removeEventListener('keydown', handler));
    });

    /* Lock body scroll while open so the page behind the modal
     * doesn't drift if the user wheels. */
    createEffect(() => {
        if (!props.open) return;
        const prev = document.body.style.overflow;
        document.body.style.overflow = 'hidden';
        onCleanup(() => {
            document.body.style.overflow = prev;
        });
    });

    const onBackdropMouseDown = (e: MouseEvent) => {
        if (props.dismissOnBackdrop === false) return;
        /* Only fire when the press starts on the backdrop itself
         * (target === currentTarget) — a click that began inside the
         * chrome but ended outside should NOT close. */
        if (e.target === e.currentTarget) {
            props.onClose();
        }
    };

    const sizeClass = () => `nge-dialog--${props.size ?? 'md'}`;

    return (
        <Show when={props.open}>
            <Portal>
                <div
                    class="nge-dialog-overlay"
                    onMouseDown={onBackdropMouseDown}
                >
                    <div
                        ref={dialogEl}
                        class={`nge-dialog ${sizeClass()}`}
                        role="dialog"
                        aria-modal="true"
                        aria-labelledby="nge-dialog-title"
                        aria-describedby={props.description ? 'nge-dialog-desc' : undefined}
                    >
                        <header class="nge-dialog__head">
                            <h2 class="nge-dialog__title" id="nge-dialog-title">
                                {props.title}
                            </h2>
                            <button
                                class="nge-dialog__close"
                                type="button"
                                aria-label="Close dialog"
                                onClick={props.onClose}
                            >
                                ×
                            </button>
                        </header>
                        <Show when={props.description}>
                            <p class="nge-dialog__desc" id="nge-dialog-desc">
                                {props.description}
                            </p>
                        </Show>
                        <div class="nge-dialog__body">{props.children}</div>
                        <Show when={props.footer}>
                            <footer class="nge-dialog__footer">{props.footer}</footer>
                        </Show>
                    </div>
                </div>
            </Portal>
        </Show>
    );
};
