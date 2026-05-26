/**
 * FileMenu — Document I/O ribbon entry.
 *
 * Open / Save / Export the active document. Live capabilities track
 * the engine surface:
 *
 *   - **Open** → hidden file input → `Command::OpenDocument { format: Docx }`.
 *   - **Save** → `Command::SaveDocument { format: Docx }`. Bound to
 *     `Ctrl/Cmd+S` globally.
 *   - **Export PDF** → `Command::ExportPdf { conformance }`. Conformance
 *     picker exposes the full `PdfConformance` enum (`A1b` / `A2u` / `X3`).
 *   - **Export HTML / Plain Text** → `Command::SaveDocument { format }`
 *     with `html` / `plain_text`. Sprint 9 wired the engine
 *     serializers (`crates/format-html` + `DocumentTree::to_plain_text`);
 *     bytes flow back through `DOCUMENT_SAVED` and the auto-download
 *     subscriber picks the right MIME + extension.
 *
 * Download trigger: subscribes to `DOCUMENT_SAVED` / `PDF_EXPORTED`
 * events, wraps the returned `bytes` in a `Blob`, and synthesises a
 * download anchor click. The active document name (set by the most
 * recent Open) drives the default filename.
 */
import {
    createEffect,
    createSignal,
    onCleanup,
    Show,
    type Component,
} from 'solid-js';
import {
    createEditorCommands,
    useEngine,
    type PdfConformance,
} from '@nge/core';
import './FileMenu.css';

const PDF_CONFORMANCES: { value: PdfConformance; label: string; hint: string }[] = [
    { value: 'A1b', label: 'PDF/A-1b', hint: 'Archival (default)' },
    { value: 'A2u', label: 'PDF/A-2u', hint: 'Archival, Unicode' },
    { value: 'X3', label: 'PDF/X-3', hint: 'Print-ready' },
];

export interface FileMenuProps {
    /** Default filename stem when the active document has no name. */
    defaultFilename?: string;
    /** Whether to bind `Ctrl/Cmd+S` to Save. Default true. */
    bindShortcuts?: boolean;
}

function triggerDownload(bytes: Uint8Array, mime: string, filename: string): void {
    const blob = new Blob([bytes as BlobPart], { type: mime });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    /* Defer revoke so Safari has time to start the download. */
    setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export const FileMenu: Component<FileMenuProps> = (props) => {
    const cmd = createEditorCommands();
    const engine = useEngine();
    const [open, setOpen] = createSignal(false);
    const [exportOpen, setExportOpen] = createSignal(false);
    const [pdfPickerOpen, setPdfPickerOpen] = createSignal(false);
    const [docName, setDocName] = createSignal<string | undefined>(undefined);
    const [error, setError] = createSignal<string | null>(null);
    let fileInput: HTMLInputElement | undefined;

    const stem = () => {
        const name = docName() ?? props.defaultFilename ?? 'document';
        return name.replace(/\.[^.]+$/, '');
    };

    /* Subscribe to engine events so saves + exports auto-download.
     * `Event::Error` from any DocFormat path bubbles into the error
     * banner. Sprint 9 — DocFormat::Html / PlainText share the same
     * DOCUMENT_SAVED reply; `pendingExportFormat` (set by the export
     * handler) tells us which MIME + extension to pick. */
    createEffect(() => {
        const unsub = engine.subscribe((evt) => {
            switch (evt.type) {
                case 'DOCUMENT_SAVED': {
                    const pending = pendingExportFormat();
                    if (pending === 'html') {
                        triggerDownload(evt.bytes, 'text/html;charset=utf-8', `${stem()}.html`);
                    } else if (pending === 'plain_text') {
                        triggerDownload(evt.bytes, 'text/plain;charset=utf-8', `${stem()}.txt`);
                    } else {
                        triggerDownload(
                            evt.bytes,
                            'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
                            `${stem()}.docx`,
                        );
                    }
                    setPendingExportFormat(null);
                    break;
                }
                case 'PDF_EXPORTED':
                    triggerDownload(evt.bytes, 'application/pdf', `${stem()}.pdf`);
                    break;
                case 'ERROR':
                    setError(evt.message);
                    setPendingExportFormat(null);
                    setTimeout(() => setError(null), 6000);
                    break;
            }
        });
        onCleanup(unsub);
    });

    const closeAll = () => {
        setOpen(false);
        setExportOpen(false);
        setPdfPickerOpen(false);
    };

    /* Click-away dismiss. */
    createEffect(() => {
        if (!open()) return;
        const handler = (e: MouseEvent) => {
            const root = (e.target as HTMLElement | null)?.closest('.nge-fm');
            if (!root) closeAll();
        };
        window.addEventListener('mousedown', handler);
        onCleanup(() => window.removeEventListener('mousedown', handler));
    });

    /* Ctrl/Cmd+S binding. */
    createEffect(() => {
        if (props.bindShortcuts === false) return;
        const handler = (e: KeyboardEvent) => {
            const ctrl = e.ctrlKey || e.metaKey;
            if (ctrl && !e.shiftKey && e.key.toLowerCase() === 's') {
                e.preventDefault();
                void cmd.saveDocument('docx');
            }
        };
        window.addEventListener('keydown', handler);
        onCleanup(() => window.removeEventListener('keydown', handler));
    });

    const onPickFile = async (file: File) => {
        const bytes = new Uint8Array(await file.arrayBuffer());
        setDocName(file.name);
        await cmd.openDocument(bytes, file.name);
        closeAll();
        if (fileInput) fileInput.value = '';
    };

    const onSave = async () => {
        closeAll();
        await cmd.saveDocument('docx');
    };

    const onExportPdf = async (conformance: PdfConformance) => {
        closeAll();
        await cmd.exportPdf(conformance);
    };

    /* Sprint 9 — HTML / Plain Text exports go through the same
     * Command::SaveDocument path; the DOCUMENT_SAVED subscriber above
     * triggers the download. Track which format is in flight so the
     * download wrapper picks the right MIME + extension. */
    const [pendingExportFormat, setPendingExportFormat] = createSignal<
        'html' | 'plain_text' | null
    >(null);

    const onExportHtml = async () => {
        closeAll();
        setPendingExportFormat('html');
        await cmd.saveDocument('html');
    };

    const onExportPlainText = async () => {
        closeAll();
        setPendingExportFormat('plain_text');
        await cmd.saveDocument('plain_text');
    };

    return (
        <>
            <div class="nge-fm">
                <button
                    class="nge-btn nge-fm__trigger"
                    type="button"
                    aria-haspopup="menu"
                    aria-expanded={open()}
                    onClick={() => setOpen((v) => !v)}
                >
                    <span>File</span>
                    <span class="nge-fm__chevron" aria-hidden="true">▾</span>
                </button>

                <Show when={open()}>
                    <ul class="nge-fm__menu" role="menu">
                        <li role="none">
                            <button
                                class="nge-fm__item"
                                type="button"
                                role="menuitem"
                                onClick={() => fileInput?.click()}
                            >
                                <span>Open…</span>
                                <span class="nge-fm__shortcut">.docx</span>
                            </button>
                        </li>
                        <li role="none">
                            <button
                                class="nge-fm__item"
                                type="button"
                                role="menuitem"
                                onClick={() => void onSave()}
                            >
                                <span>Save</span>
                                <span class="nge-fm__shortcut">Ctrl+S</span>
                            </button>
                        </li>
                        <li class="nge-fm__separator" role="separator" />
                        <li role="none" class="nge-fm__submenu-anchor">
                            <button
                                class="nge-fm__item"
                                type="button"
                                role="menuitem"
                                aria-haspopup="menu"
                                aria-expanded={exportOpen()}
                                onClick={() => setExportOpen((v) => !v)}
                            >
                                <span>Export</span>
                                <span class="nge-fm__submenu-arrow" aria-hidden="true">▸</span>
                            </button>
                            <Show when={exportOpen()}>
                                <ul class="nge-fm__menu nge-fm__submenu" role="menu">
                                    <li role="none">
                                        <button
                                            class="nge-fm__item"
                                            type="button"
                                            role="menuitem"
                                            onClick={() => setPdfPickerOpen((v) => !v)}
                                            aria-haspopup="menu"
                                            aria-expanded={pdfPickerOpen()}
                                        >
                                            <span>PDF…</span>
                                            <span class="nge-fm__submenu-arrow" aria-hidden="true">▸</span>
                                        </button>
                                        <Show when={pdfPickerOpen()}>
                                            <ul class="nge-fm__menu nge-fm__submenu" role="menu">
                                                {PDF_CONFORMANCES.map((c) => (
                                                    <li role="none">
                                                        <button
                                                            class="nge-fm__item nge-fm__item--stacked"
                                                            type="button"
                                                            role="menuitem"
                                                            onClick={() => void onExportPdf(c.value)}
                                                        >
                                                            <span class="nge-fm__label">{c.label}</span>
                                                            <span class="nge-fm__hint">{c.hint}</span>
                                                        </button>
                                                    </li>
                                                ))}
                                            </ul>
                                        </Show>
                                    </li>
                                    <li role="none">
                                        <button
                                            class="nge-fm__item"
                                            type="button"
                                            role="menuitem"
                                            onClick={() => void onExportHtml()}
                                        >
                                            <span>HTML</span>
                                            <span class="nge-fm__shortcut">.html</span>
                                        </button>
                                    </li>
                                    <li role="none">
                                        <button
                                            class="nge-fm__item"
                                            type="button"
                                            role="menuitem"
                                            onClick={() => void onExportPlainText()}
                                        >
                                            <span>Plain Text</span>
                                            <span class="nge-fm__shortcut">.txt</span>
                                        </button>
                                    </li>
                                </ul>
                            </Show>
                        </li>
                    </ul>
                </Show>

                <input
                    ref={(el) => (fileInput = el)}
                    type="file"
                    accept=".docx,application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    style={{ display: 'none' }}
                    onChange={(e) => {
                        const file = e.currentTarget.files?.[0];
                        if (file) void onPickFile(file);
                    }}
                />
            </div>

            <Show when={error()}>
                <div class="nge-fm__error" role="alert">{error()}</div>
            </Show>
        </>
    );
};
