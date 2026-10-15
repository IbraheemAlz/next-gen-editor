/* Issue #467 - the shell's half of the text input boundary.
 *
 * The engine cleans control characters and refuses U+FFFE / U+FFFF
 * (`engine::text_input`, `text_input_guard.rs`), but it cannot see a LONE
 * UTF-16 SURROGATE: a Rust `String` cannot hold one, and the wasm-bindgen
 * string glue (`TextEncoder`) turns it into U+FFFD before the engine reads
 * the command - a silent substitution of text the user typed or pasted,
 * from a half-pasted emoji or a bad clipboard producer. So the worker
 * refuses it BEFORE the command reaches the engine, with the same typed
 * reply the engine gives a noncharacter: `Event::Error { kind:
 * InvalidArgument }` naming the command, the field and the position. The
 * command is not logged (see `handleClientCommand`): a replay would
 * otherwise insert the U+FFFD the live session refused.
 *
 * Pure (no DOM, no wasm) so it unit-tests without the worker. */
import type { Command, Event } from './types';

/** Deepest object nesting walked. Commands are shallow (a patch inside a
 *  command); the cap only keeps a hostile cyclic structure finite. */
const MAX_DEPTH = 12;

/** `isWellFormed` is ES2024; the scan below is the fallback for an older
 *  engine (it checks the pairing by hand). */
function wellFormed(s: string): boolean {
    const native = (s as { isWellFormed?: () => boolean }).isWellFormed;
    if (typeof native === 'function') return native.call(s);
    return loneSurrogateIndex(s) === -1;
}

/** UTF-16 index of the first lone surrogate in `s`, or -1. */
export function loneSurrogateIndex(s: string): number {
    for (let i = 0; i < s.length; i++) {
        const c = s.charCodeAt(i);
        if (c >= 0xd800 && c <= 0xdbff) {
            const next = s.charCodeAt(i + 1);
            if (next >= 0xdc00 && next <= 0xdfff) {
                i++;
                continue;
            }
            return i;
        }
        if (c >= 0xdc00 && c <= 0xdfff) return i;
    }
    return -1;
}

/** Where a lone surrogate sits in a command: the field path (`text`,
 *  `attrs.font_family`, `log_tail[2].text`) and the UTF-16 index. */
export interface IllFormedText {
    field: string;
    index: number;
}

function walk(value: unknown, path: string, depth: number): IllFormedText | undefined {
    if (typeof value === 'string') {
        if (wellFormed(value)) return undefined;
        return { field: path, index: loneSurrogateIndex(value) };
    }
    if (value === null || typeof value !== 'object' || depth > MAX_DEPTH) return undefined;
    /* Byte payloads (fonts, documents, images, snapshots) carry no text. */
    if (ArrayBuffer.isView(value) || value instanceof ArrayBuffer) return undefined;
    if (Array.isArray(value)) {
        for (let i = 0; i < value.length; i++) {
            const hit = walk(value[i], `${path}[${i}]`, depth + 1);
            if (hit) return hit;
        }
        return undefined;
    }
    for (const [key, child] of Object.entries(value)) {
        const hit = walk(child, path === '' ? key : `${path}.${key}`, depth + 1);
        if (hit) return hit;
    }
    return undefined;
}

/** `INSERT_TEXT` -> `InsertText`, the spelling the engine's messages use. */
function commandName(wire: string): string {
    return wire
        .toLowerCase()
        .split('_')
        .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
        .join('');
}

/** The first lone surrogate in any string of `cmd`, if there is one.
 *  `RECOVER` is not walked: its tail replays through the engine's own
 *  dispatcher, and it carries state, not user text. */
export function findIllFormedText(cmd: Command): IllFormedText | undefined {
    if (cmd.type === 'RECOVER') return undefined;
    return walk(cmd, '', 0);
}

/** The typed refusal for a command carrying a lone surrogate, or
 *  `undefined` when every string is well formed (the common case: no
 *  allocation beyond the walk). */
export function refuseIllFormedText(cmd: Command): Event | undefined {
    const bad = findIllFormedText(cmd);
    if (!bad) return undefined;
    return {
        type: 'ERROR',
        message:
            `${commandName(cmd.type)}: ${bad.field}: text holds a lone UTF-16 surrogate ` +
            `at index ${bad.index}, which is not a character a document can hold ` +
            '(nothing was applied)',
        kind: 'InvalidArgument',
        command: cmd.type,
    } as Event;
}
