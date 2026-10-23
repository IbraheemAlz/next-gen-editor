/* Issue #506 - the worker's picture-decode decisions, kept out of
 * `engine.worker.ts` (which imports the wasm engine and cannot be loaded by
 * vitest) so the "which command needs a decode" rule and the decode loop are
 * unit-tested.
 *
 * Background. The Canvas2D backend paints a picture from a decoded
 * `ImageBitmap` the worker installed with `Engine.register_image`, and a grey
 * placeholder when it has none. The worker decoded only after
 * `OPEN_DOCUMENT` and a recovery, so a picture the engine minted itself
 * (`INSERT_IMAGE`, the toolbar's Insert -> Image) stayed the placeholder for
 * the rest of the session. The engine now answers
 * `pending_media_entries()` - the blobs with no (or a stale) bitmap, empty on
 * the common path - and the worker asks after every command that can change
 * the document, decodes what it is given and repaints. */

/** The two `CommandMeta` flags the decision reads (`commandMeta(type)`). */
export interface MediaDecodeMeta {
    mutates_doc: boolean;
    new_document: boolean;
}

/** What the worker does about pictures after one answered command. */
export interface MediaDecodePlan {
    /** Ask the engine for pending blobs and decode them. */
    decode: boolean;
    /** Repaint when something was decoded: the command's own auto-repaint
     *  already ran with the placeholder, and `register_image` only forgets
     *  what each canvas shows - it does not paint. A NEW document skips it:
     *  its page canvases are (re)registered next and paint then (the
     *  pre-#506 behaviour of an open). */
    repaint: boolean;
}

/** Whether the command can have put a picture blob the engine has no bitmap
 *  for into the document: any command that may change the document
 *  (`INSERT_IMAGE`, a paste, an undo / redo that restores a picture, ...) or
 *  starts a new one (`OPEN_DOCUMENT`). A refused command (`ERROR`) changed
 *  nothing. Read-only commands - the per-pointermove `HIT_TEST` among them -
 *  never reach the engine's media map, so they cost nothing here. */
export function mediaDecodePlan(meta: MediaDecodeMeta, replyType: string): MediaDecodePlan {
    const decode = (meta.mutates_doc || meta.new_document) && replyType !== 'ERROR';
    return { decode, repaint: decode && !meta.new_document };
}

/** One entry of `Engine.pending_media_entries()` / `media_entries()`. */
export interface MediaEntry {
    rel_id: string;
    mime: string;
    bytes: Uint8Array;
}

/** The side effects the decoder needs (the worker wires the real ones). */
export interface MediaDecoderIo<Bitmap> {
    decode(blob: Blob): Promise<Bitmap>;
    register(relId: string, bitmap: Bitmap): void;
    warn(message: string, error: unknown): void;
}

/** Decodes pending media entries one by one. A blob the browser cannot
 *  decode is reported ONCE and then left alone (the engine keeps listing it
 *  as pending - it has no bitmap - so without the memo every later command
 *  would retry the decode and repeat the warning); the renderer keeps the
 *  placeholder for it. */
export class MediaDecoder<Bitmap> {
    private readonly failed = new Set<string>();

    constructor(private readonly io: MediaDecoderIo<Bitmap>) {}

    /** Forget past failures (a new document has different blobs). */
    reset(): void {
        this.failed.clear();
    }

    /** Decode and register `entries`; resolves with how many were
     *  registered. Never throws: a failing blob is skipped. */
    async run(entries: readonly MediaEntry[]): Promise<number> {
        let registered = 0;
        for (const entry of entries) {
            const memo = `${entry.rel_id}:${entry.bytes.byteLength}`;
            if (this.failed.has(memo)) continue;
            try {
                const blob = new Blob([entry.bytes as BlobPart], { type: entry.mime });
                const bitmap = await this.io.decode(blob);
                this.io.register(entry.rel_id, bitmap);
                registered++;
            } catch (e: unknown) {
                this.failed.add(memo);
                this.io.warn(`[worker] image decode failed (${entry.rel_id}):`, e);
            }
        }
        return registered;
    }
}
