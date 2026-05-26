/**
 * InsertImageButton — toolbar Insert → Image entry point.
 *
 * Opens a hidden <input type="file" accept="image/*">, reads the chosen
 * blob into a Uint8Array, probes the image's intrinsic dimensions via an
 * off-DOM Image, then dispatches Command::InsertImage with the ImageBlob
 * + an ImageFit mode.
 *
 * Before this component, the only way to land an image in the document
 * was via the drag-drop handler in `ts/src/input/dnd.ts` — which only
 * accepts `.docx` files. This unblocks the entire INSERT_IMAGE +
 * ImageFit matrix for manual QA.
 */
import { createSignal, type Component } from 'solid-js';
import { createEditorCommands, type ImageFit, type ImageBlob } from '@nge/core';
import './InsertImageButton.css';

const ACCEPT = 'image/png,image/jpeg,image/gif,image/webp';

async function probeImageDimensions(
    bytes: Uint8Array,
    mime: string,
): Promise<{ width: number; height: number }> {
    /* createImageBitmap fails on some browsers for SVG / animated; fall
     * back to <img>. */
    const blob = new Blob([bytes as BlobPart], { type: mime });
    if (typeof createImageBitmap === 'function') {
        try {
            const bmp = await createImageBitmap(blob);
            const dims = { width: bmp.width, height: bmp.height };
            bmp.close?.();
            return dims;
        } catch {
            /* fall through */
        }
    }
    const url = URL.createObjectURL(blob);
    try {
        const img = await new Promise<HTMLImageElement>((resolve, reject) => {
            const el = new Image();
            el.onload = () => resolve(el);
            el.onerror = () => reject(new Error('image decode failed'));
            el.src = url;
        });
        return { width: img.naturalWidth, height: img.naturalHeight };
    } finally {
        URL.revokeObjectURL(url);
    }
}

export interface InsertImageButtonProps {
    /** Default fit mode applied at insert. */
    defaultFit?: ImageFit;
    /** Override label. Default: "Image". */
    label?: string;
}

export const InsertImageButton: Component<InsertImageButtonProps> = (props) => {
    const cmd = createEditorCommands();
    const [busy, setBusy] = createSignal(false);
    let inputEl: HTMLInputElement | undefined;

    const onPick = async (file: File) => {
        setBusy(true);
        try {
            const bytes = new Uint8Array(await file.arrayBuffer());
            const { width, height } = await probeImageDimensions(bytes, file.type);
            const blob: ImageBlob = {
                bytes,
                mime: file.type,
                width,
                height,
            };
            await cmd.insertImageAtCaret(blob, props.defaultFit ?? 'FitWidth');
        } finally {
            setBusy(false);
            if (inputEl) inputEl.value = '';
        }
    };

    return (
        <>
            <button
                class="nge-btn nge-insert-image"
                type="button"
                aria-label="Insert image"
                title="Insert image"
                disabled={busy()}
                onClick={() => inputEl?.click()}
            >
                <span class="nge-insert-image__icon" aria-hidden="true">🖼</span>
                <span>{props.label ?? 'Image'}</span>
            </button>
            <input
                ref={(el) => (inputEl = el)}
                type="file"
                accept={ACCEPT}
                style={{ display: 'none' }}
                onChange={(e) => {
                    const file = e.currentTarget.files?.[0];
                    if (file) void onPick(file);
                }}
            />
        </>
    );
};
