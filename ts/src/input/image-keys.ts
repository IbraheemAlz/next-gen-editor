/* Issues #554 / #506 — the keyboard with a picture OBJECT-selected.
 *
 * A click on a picture selects it shell-side (`selectImageByPoint`, resize
 * handles, no caret — the engine's caret stays wherever it was). Object
 * selection REPLACES the caret, so until now every key fell through to that
 * stale caret: Delete / Backspace did nothing to the picture, and an arrow
 * walked from wherever the caret happened to be (the line above the picture
 * landed in the table above it).
 *
 * The rule (clean-room memo `onlyoffice-cursor-memo.md` §9, plus the
 * "collapse" rule of §3):
 *
 *   - Delete / Backspace remove the picture (ONE undoable edit) and leave
 *     the caret where it stood.
 *   - Left / Right collapse the object selection to a caret at the
 *     picture's visual left / right edge and stop; Up / Down put the caret
 *     before / after the picture and then apply the motion, so the walk
 *     starts in the picture's own line. Shift extends from there.
 *   - Escape, Enter, Tab, Home / End and every typing key leave the object
 *     selection: the caret goes just BEFORE the picture's anchor (in the
 *     story that owns it) and the key then does its normal job.
 *   - Chords the picture does not own (Ctrl+C, Ctrl+Z, F-keys, Alt+…) just
 *     drop the selection, as before.
 *
 * Pure planning lives here (`planImageKey`, unit-tested without a DOM); the
 * dispatching is `runImageKey`. Positions: a picture is the 3-byte `U+FFFC`
 * sentinel at byte offset `at` of its paragraph; "before" is `at`, "after"
 * is `at + 3`. In an RTL paragraph the byte order is the reverse of the
 * visual one, so "visual left" is `after` there (`ImageRect.rtl`). */
import type {
    BlockPath,
    Command,
    Event,
    LogicalPos,
    MoveDirection,
    TextBoxHop,
} from '../engine/types';

/** UTF-8 length of the `U+FFFC` object-replacement sentinel. */
export const SENTINEL_LEN = 3;

/** The slice of a key event the planner reads. */
export interface KeyInfo {
    key: string;
    shiftKey: boolean;
    ctrlKey: boolean;
    metaKey: boolean;
    altKey: boolean;
}

/** The selected picture, as `GET_IMAGE_RECTS` addresses it. */
export interface KeyedImage {
    path: BlockPath;
    at: number;
    /** Text-box chain the picture lives in (empty = body). */
    story: TextBoxHop[];
    /** `ImageRect.storyRid` (`''` for a body picture). */
    storyRid: string;
    /** The picture's paragraph runs right-to-left (`ImageRect.rtl`). */
    rtl: boolean;
}

/** Which story the engine's caret is in right now. */
export interface ActiveStory {
    /** `BridgeStoryRef.area` (`'TextBox'`, a header / footer area, …) —
     *  absent for the body. */
    area: string;
    rid: string;
}

export type ImageEdge = 'before' | 'after';

export type ImageKeyPlan =
    /** A bare modifier press: nothing happens, the selection stays. */
    | { kind: 'ignore' }
    /** Not the picture's key: drop the selection, nothing else. */
    | { kind: 'keep' }
    /** Remove the picture. */
    | { kind: 'delete' }
    /** Put the caret at `edge` of the picture, optionally apply a motion,
     *  and either consume the key (`consume`) or let it go on. */
    | {
          kind: 'caret';
          edge: ImageEdge;
          motion?: { direction: MoveDirection; extend: boolean };
          consume: boolean;
      };

const MODIFIER_KEYS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'AltGraph', 'CapsLock']);

/** `true` for a key that is only a modifier (it must not drop the
 *  selection: Shift-in-preparation of Shift+Click, Ctrl before Ctrl+C). */
export const isModifierOnly = (key: string): boolean => MODIFIER_KEYS.has(key);

/** What a key does while `img` is object-selected. */
export function planImageKey(k: KeyInfo, img: Pick<KeyedImage, 'rtl'>): ImageKeyPlan {
    if (isModifierOnly(k.key)) return { kind: 'ignore' };
    /* AltGr is Ctrl+Alt on Windows and types a character. */
    const chord = (k.ctrlKey || k.metaKey) && !k.altKey;
    const alt = k.altKey && !k.ctrlKey;

    if (k.key === 'Delete' || k.key === 'Backspace') {
        return alt ? { kind: 'keep' } : { kind: 'delete' };
    }

    const horizontal = k.key === 'ArrowLeft' || k.key === 'ArrowRight';
    const vertical = k.key === 'ArrowUp' || k.key === 'ArrowDown';
    if (horizontal || vertical) {
        if (alt) return { kind: 'keep' };
        const extend = k.shiftKey;
        if (vertical) {
            const up = k.key === 'ArrowUp';
            return {
                kind: 'caret',
                edge: up ? 'before' : 'after',
                motion: { direction: up ? 'Up' : 'Down', extend },
                consume: true,
            };
        }
        const left = k.key === 'ArrowLeft';
        /* The picture's visual left / right edge as a byte side. */
        const leftEdge: ImageEdge = img.rtl ? 'after' : 'before';
        const rightEdge: ImageEdge = img.rtl ? 'before' : 'after';
        const word = chord;
        if (!extend && !word) {
            /* Collapse to the edge in the direction of travel; no more. */
            return { kind: 'caret', edge: left ? leftEdge : rightEdge, consume: true };
        }
        /* Extend (or jump a word) from the edge BEHIND the direction of
           travel, so the motion crosses the picture / leaves it. */
        const direction: MoveDirection = word
            ? left
                ? 'WordLeft'
                : 'WordRight'
            : left
              ? 'Left'
              : 'Right';
        return {
            kind: 'caret',
            edge: word ? (left ? leftEdge : rightEdge) : left ? rightEdge : leftEdge,
            motion: { direction, extend },
            consume: true,
        };
    }

    if (k.key === 'Escape') return { kind: 'caret', edge: 'before', consume: true };

    /* Ctrl/Cmd chords (copy, undo, bold, …) and function keys act on the
       document, not on the object. */
    if (chord || alt || /^F\d{1,2}$/.test(k.key)) return { kind: 'keep' };

    /* Typing, Enter, Tab, Home / End, PageUp / PageDown, IME ("Process"),
       dead keys: leave the object selection, caret before the anchor, and
       let the key do its normal job from there. */
    return { kind: 'caret', edge: 'before', consume: false };
}

/** The `SET_SELECTION` that collapses the engine's selection to `edge` of
 *  the picture. */
export function caretAtEdge(img: Pick<KeyedImage, 'path' | 'at'>, edge: ImageEdge): Command {
    const pos: LogicalPos = {
        path: img.path,
        offset: img.at + (edge === 'after' ? SENTINEL_LEN : 0),
    };
    return { type: 'SET_SELECTION', range: { start: pos, end: pos }, caret: pos };
}

/** Whether the engine's active story is the one the picture's paths are
 *  rooted in — only then may a story-rooted `SET_SELECTION` address it (in
 *  the wrong story the position would resolve against another document). */
export function storyMatches(img: Pick<KeyedImage, 'story' | 'storyRid'>, active?: ActiveStory): boolean {
    if (img.story.length === 0) return active === undefined;
    return active !== undefined && active.area === 'TextBox' && active.rid === img.storyRid;
}

/** What the caller does with the key event. */
export type ImageKeyOutcome = 'consumed' | 'continue';

/** The slice of `EngineClient` the runner needs. */
export interface ImageKeyClient {
    dispatch(cmd: Command): Promise<Event>;
}

/** Carry out `planImageKey`'s plan. Synchronous up to the commands it
 *  posts (they enter the worker queue in key order, ahead of any later
 *  `beforeinput`); resolves `'consumed'` when the caller should
 *  `preventDefault` and stop, `'continue'` when the key goes on. The
 *  caller clears the object selection either way. */
export function runImageKey(
    plan: ImageKeyPlan,
    img: KeyedImage,
    client: ImageKeyClient,
    active: ActiveStory | undefined,
    onError: (what: string, e: unknown) => void = () => {},
): ImageKeyOutcome {
    const fail = (what: string) => (e: unknown) => onError(what, e);
    switch (plan.kind) {
        case 'ignore':
        case 'keep':
            return 'continue';
        case 'delete': {
            /* A text-box picture selected from outside its box cannot be
               addressed (positions are story-rooted). The key is swallowed
               rather than continued: a continued Delete / Backspace would
               delete a character at the engine's stale caret. */
            if (!storyMatches(img, active)) return 'consumed';
            if (img.story.length === 0) {
                /* ONE command: one undo entry, caret left at the anchor. */
                client
                    .dispatch({
                        type: 'DELETE_RANGE',
                        range: {
                            start: { path: img.path, offset: img.at },
                            end: { path: img.path, offset: img.at + SENTINEL_LEN },
                        },
                    })
                    .catch(fail('delete image'));
                return 'consumed';
            }
            /* `DeleteRange` is body-only: select the sentinel in the story
               and delete the selection, but only once the engine confirms it
               selected exactly the picture (a stale address must never turn
               into a delete at the old caret). */
            void (async () => {
                const sel = await client.dispatch(selectPicture(img));
                if (
                    sel.type === 'SELECTION_CHANGED' &&
                    sel.range.start.offset === img.at &&
                    sel.range.end.offset === img.at + SENTINEL_LEN
                ) {
                    await client.dispatch({ type: 'DELETE_AT_CARET', forward: false, by_word: false });
                }
            })().catch(fail('delete story image'));
            return 'consumed';
        }
        case 'caret': {
            if (!storyMatches(img, active)) return 'continue';
            client.dispatch(caretAtEdge(img, plan.edge)).catch(fail('place caret at image'));
            if (plan.motion) {
                client
                    .dispatch({
                        type: 'MOVE_CARET',
                        direction: plan.motion.direction,
                        extend: plan.motion.extend,
                    })
                    .catch(fail('move caret from image'));
            }
            return plan.consume ? 'consumed' : 'continue';
        }
    }
}

function selectPicture(img: Pick<KeyedImage, 'path' | 'at'>): Command {
    const start: LogicalPos = { path: img.path, offset: img.at };
    const end: LogicalPos = { path: img.path, offset: img.at + SENTINEL_LEN };
    return { type: 'SET_SELECTION', range: { start, end }, caret: end };
}
