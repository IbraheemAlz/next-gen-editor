/**
 * createFontRegistry — headless, data-driven font manager (Solid primitive).
 *
 * Single source of truth is a JSON manifest (`public/fonts.json`) mapping a
 * font id to its display label and `.ttf` URL. Nothing about the font set is
 * hard-coded in TS — adding a face is "drop the `.ttf` + append a manifest
 * row", no rebuild of this module.
 *
 * Lifecycle:
 *   - `loadDefaults()` (boot) pushes only the manifest's minimal `defaults`
 *     set into the engine so the editor is interactive immediately; every
 *     other font is loaded just-in-time.
 *   - `ensureFont(id)` (JIT) fetches the `.ttf` bytes over the network and
 *     pushes them to the engine via the `LOAD_FONT` bridge command, exactly
 *     once per session. Concurrent calls for the same id share one in-flight
 *     Promise; a font already resident short-circuits.
 *   - `reset()` clears the resident set — call it on crash recovery, since
 *     `Command::Recover` wipes the engine's font map and the boot `loadDefaults`
 *     must re-push.
 *
 * Reactivity: the resident-state map is a `createStore` so `stateOf(id)` /
 * `isLoaded(id)` are fine-grained reactive (a dropdown row re-renders only
 * when *its* font flips state). The manifest is fetched once and memoized.
 *
 * URLs in the manifest are resolved against `document.baseURI`, so a deploy
 * under a subpath (`/editor/`) works without absolute-path breakage — the
 * same discipline the worker's `?url` font imports follow.
 *
 * NOTE — engine apply-layer constraint: the bridge's `LOAD_FONT` is
 * string-keyed and accepts any id, but the engine's `FontFamily` enum
 * currently resolves only `amiri` / `liberation` / `noto-naskh` when a font
 * is *applied* to text. Loading a brand-new id succeeds (bytes land in the
 * WASM font map) but paint falls back until the enum is widened — tracked in
 * the core-engine "Widen engine::FontFamily" issue. This module is the full
 * data-driven half; the engine half is deferred.
 */
import { createSignal, type Accessor } from 'solid-js';
import { createStore } from 'solid-js/store';
import type { Command, Event } from './types';

/** One declared font in the manifest. */
export interface FontDescriptor {
    /** Stable id; the value passed to `cmd.setFontFamily` + `LOAD_FONT`. */
    id: string;
    /** Human-readable name shown in the toolbar dropdown. */
    label: string;
    /** `.ttf` URL, resolved relative to `document.baseURI`. */
    url: string;
    /** Optional script-coverage hint (purely informational for the UI). */
    scripts?: string[];
}

/** The parsed `fonts.json`. */
export interface FontManifest {
    /** Font ids loaded at boot (kept minimal — one Latin + one Arabic). */
    defaults: string[];
    /** Every declared font. */
    fonts: FontDescriptor[];
}

/** Per-id residency state in the engine. */
export type FontLoadState = 'idle' | 'loading' | 'loaded' | 'error';

/** The minimal engine surface the registry needs: dispatch a `LOAD_FONT`. */
export interface FontEngine {
    dispatch(cmd: Command, transfer?: Transferable[]): Promise<Event>;
}

export interface FontRegistryConfig {
    /** Manifest URL. Default: `fonts.json` resolved against `document.baseURI`. */
    manifestUrl?: string;
}

export interface FontRegistry {
    /** Reactive declared-font list (empty until the manifest resolves). */
    fonts: Accessor<FontDescriptor[]>;
    /** Reactive boot-default ids. */
    defaults: Accessor<string[]>;
    /** True once `fonts.json` has been fetched + parsed. */
    manifestReady: Accessor<boolean>;
    /** Reactive residency state of one font. */
    stateOf: (id: string) => FontLoadState;
    /** True when the font's bytes are resident in the engine this session. */
    isLoaded: (id: string) => boolean;
    /** Fetch + parse the manifest (memoized; safe to call repeatedly). */
    loadManifest: () => Promise<FontManifest>;
    /** JIT load: fetch + push `id` to the engine if not already resident. */
    ensureFont: (id: string) => Promise<void>;
    /** Boot: load the manifest's `defaults` set into the engine. */
    loadDefaults: () => Promise<void>;
    /** Forget every resident font — call on crash recovery (engine wiped them). */
    reset: () => void;
}

/** Resolve a manifest-relative URL against the document base. */
function resolveUrl(url: string): string {
    const base =
        typeof document !== 'undefined' && document.baseURI ? document.baseURI : '/';
    return new URL(url, base).href;
}

export function createFontRegistry(
    engine: FontEngine,
    config: FontRegistryConfig = {},
): FontRegistry {
    const manifestUrl = config.manifestUrl ?? resolveUrl('fonts.json');

    const [fonts, setFonts] = createSignal<FontDescriptor[]>([]);
    const [defaults, setDefaults] = createSignal<string[]>([]);
    const [manifestReady, setManifestReady] = createSignal(false);

    /* Per-id residency, fine-grained reactive so a dropdown row re-renders
     * only when its own font flips state. */
    const [states, setStates] = createStore<Record<string, FontLoadState>>({});

    /* Dedup: one in-flight Promise per id (plain Map — not reactive). */
    const inflight = new Map<string, Promise<void>>();

    /* Memoized manifest fetch — kicked off eagerly so the dropdown can
     * populate before the user opens it. */
    let manifestPromise: Promise<FontManifest> | null = null;
    const loadManifest = (): Promise<FontManifest> => {
        if (!manifestPromise) {
            manifestPromise = fetch(manifestUrl)
                .then((r) => {
                    if (!r.ok) {
                        throw new Error(
                            `[@nge/core] font manifest ${manifestUrl}: HTTP ${r.status}`,
                        );
                    }
                    return r.json() as Promise<FontManifest>;
                })
                .then((m) => {
                    setFonts(m.fonts ?? []);
                    setDefaults(m.defaults ?? []);
                    setManifestReady(true);
                    return m;
                });
        }
        return manifestPromise;
    };
    /* Eager kick-off; ignore rejection here — callers that await surface it. */
    void loadManifest().catch(() => undefined);

    const ensureFont = (id: string): Promise<void> => {
        if (states[id] === 'loaded') return Promise.resolve();
        const existing = inflight.get(id);
        if (existing) return existing;

        const p = (async () => {
            const manifest = await loadManifest();
            const desc = manifest.fonts.find((f) => f.id === id);
            if (!desc) {
                throw new Error(`[@nge/core] font '${id}' not in manifest`);
            }
            setStates(id, 'loading');
            try {
                const res = await fetch(resolveUrl(desc.url));
                if (!res.ok) {
                    throw new Error(
                        `[@nge/core] font '${id}' fetch ${desc.url}: HTTP ${res.status}`,
                    );
                }
                const bytes = new Uint8Array(await res.arrayBuffer());
                /* Zero-copy: the ArrayBuffer is transferred to the worker.
                 * `bytes` is detached afterward — we never reuse it. */
                const evt = await engine.dispatch(
                    { type: 'LOAD_FONT', id, bytes },
                    [bytes.buffer as ArrayBuffer],
                );
                if (evt.type === 'ERROR') {
                    throw new Error(`[@nge/core] engine LOAD_FONT '${id}': ${evt.message}`);
                }
                setStates(id, 'loaded');
            } catch (e) {
                setStates(id, 'error');
                throw e;
            } finally {
                inflight.delete(id);
            }
        })();
        inflight.set(id, p);
        return p;
    };

    const loadDefaults = async (): Promise<void> => {
        const manifest = await loadManifest();
        await Promise.all(manifest.defaults.map((id) => ensureFont(id)));
    };

    const reset = (): void => {
        inflight.clear();
        /* Drop every residency flag back to idle. The store has no `clear`,
         * so blank each known key. */
        for (const id of Object.keys(states)) {
            setStates(id, 'idle');
        }
    };

    return {
        fonts,
        defaults,
        manifestReady,
        stateOf: (id) => states[id] ?? 'idle',
        isLoaded: (id) => states[id] === 'loaded',
        loadManifest,
        ensureFont,
        loadDefaults,
        reset,
    };
}
