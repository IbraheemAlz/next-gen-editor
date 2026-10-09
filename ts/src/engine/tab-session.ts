/* Issue #426 - per-tab session identity for the event log.
 *
 * Before this module every tab opened the SAME `engine-log` database, so a
 * second tab's boot treated the first tab's live, unsaved log as "a previous
 * generation's" (archived it, then `INIT` cleared it under the first tab's
 * feet). Now every page generation claims a session at boot:
 *
 *   - a TAB TOKEN lives in `sessionStorage` (per tab, survives reloads, and
 *     is NOT shared with other tabs). A duplicated tab copies it, so a
 *     heartbeat `instance` tells "my own previous page" from "a live twin":
 *     a twin gets a fresh token;
 *   - a HEARTBEAT per token in `localStorage` (`nge.tab-hb.<token>`: the
 *     page instance, the last beat, and `left` once `pagehide` ran) is the
 *     same trick as the #240 `live` token - a tab that died leaves a stale
 *     beat. `isTabGone` is the single liveness rule;
 *   - the PRIMARY database (`engine-log`) belongs to ONE tab at a time
 *     (`nge.log-owner`); any other live tab opens its own database
 *     `engine-log-<token>`. A boot therefore only ever archives (and then
 *     clears) a log whose owning tab is gone - its own previous page, or a
 *     tab that has since closed. A closed secondary tab's database is swept
 *     by the next boot: unsaved content joins the archive ring, then the
 *     database is deleted.
 *
 * The claim runs under a Web Lock so two tabs booting together cannot both
 * take the primary. Everything here is best effort: with storage blocked
 * the page falls back to the primary database, i.e. the pre-#426 behaviour. */
import {
    PRIMARY_DB,
    SECONDARY_DB_PREFIX,
    archiveLog,
    deleteLogDb,
    inspectActiveLog,
    listSecondaryDbs,
} from './event-log';

const TAB_KEY = 'nge.tab';
const BEAT_PREFIX = 'nge.tab-hb.';
const OWNER_KEY = 'nge.log-owner';
const LOCK_NAME = 'nge-tab-claim';

/** Heartbeat period of a live page. */
export const TAB_BEAT_MS = 2000;
/** A beat older than this means the page is no longer running (it missed
 *  three beats), whatever it last wrote. */
export const TAB_STALE_MS = 6000;
/** A page that said `pagehide` may be mid-reload: its own successor is
 *  recognised by token, but OTHER tabs wait this long before treating the
 *  tab as closed and taking its log. */
export const TAB_LEAVE_GRACE_MS = 4000;

export interface TabSession {
    /** This tab's identity (stable across reloads of the tab). */
    token: string;
    /** The database this page generation logs into. */
    db: string;
    /** Archive entries created at boot from tabs that are gone. */
    sweptArchives: number;
}

interface Beat {
    at: number;
    instance: string;
    left?: boolean;
}

function newId(): string {
    return globalThis.crypto?.randomUUID?.() ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function readBeat(token: string): Beat | undefined {
    try {
        const raw = globalThis.localStorage?.getItem(BEAT_PREFIX + token);
        if (!raw) return undefined;
        const b = JSON.parse(raw) as Partial<Beat>;
        return typeof b.at === 'number' && typeof b.instance === 'string'
            ? { at: b.at, instance: b.instance, ...(b.left === true ? { left: true } : {}) }
            : undefined;
    } catch {
        return undefined;
    }
}

function writeBeat(token: string, beat: Beat): void {
    try {
        globalThis.localStorage?.setItem(BEAT_PREFIX + token, JSON.stringify(beat));
    } catch {
        /* storage blocked: peers see this tab as gone */
    }
}

/** The one liveness rule: a tab with no beat, a stale beat, or a `left`
 *  beat past the reload grace is gone. */
export function isTabGone(beat: Beat | undefined, now: number): boolean {
    if (!beat) return true;
    return now - beat.at > (beat.left ? TAB_LEAVE_GRACE_MS : TAB_STALE_MS);
}

function readStored(): { token: string; db: string | undefined } | undefined {
    try {
        const raw = globalThis.sessionStorage?.getItem(TAB_KEY);
        if (!raw) return undefined;
        const t = JSON.parse(raw) as { token?: unknown; db?: unknown };
        if (typeof t.token !== 'string') return undefined;
        return { token: t.token, db: typeof t.db === 'string' ? t.db : undefined };
    } catch {
        return undefined;
    }
}

function secondaryName(token: string): string {
    return SECONDARY_DB_PREFIX + token;
}

function tokenOf(dbName: string): string {
    return dbName.slice(SECONDARY_DB_PREFIX.length);
}

function withLock<T>(fn: () => Promise<T>): Promise<T> {
    const locks = (globalThis.navigator as Navigator | undefined)?.locks;
    return locks ? (locks.request(LOCK_NAME, fn) as Promise<T>) : fn();
}

/** Claim this page generation's session: pick the database to log into,
 *  start the heartbeat and sweep the tabs that are gone. */
export async function claimTabSession(): Promise<TabSession> {
    const instance = newId();
    return withLock(async () => {
        const now = Date.now();
        const stored = readStored();
        let token = stored?.token;
        /* A live page already beats under this token: a duplicated tab
           (sessionStorage is copied). It must not share the log. */
        if (token !== undefined) {
            const beat = readBeat(token);
            /* A `left` beat is this tab's own previous page (a reload) - the
               grace only protects it from OTHER tabs. */
            if (beat && beat.instance !== instance && !beat.left && !isTabGone(beat, now)) {
                token = undefined;
            }
        }
        const fresh = token === undefined || token !== stored?.token;
        token ??= newId();
        const owner = globalThis.localStorage?.getItem(OWNER_KEY) ?? undefined;
        const ownerLive =
            owner !== undefined && owner !== token && !isTabGone(readBeat(owner), now);
        let db: string;
        if (!fresh && stored?.db === PRIMARY_DB) {
            /* Mine before the reload - unless a live tab has since taken it. */
            db = ownerLive ? secondaryName(token) : PRIMARY_DB;
        } else if (!fresh && stored?.db !== undefined) {
            db = stored.db;
        } else {
            db = ownerLive ? secondaryName(token) : PRIMARY_DB;
        }
        if (db === PRIMARY_DB) {
            try {
                globalThis.localStorage?.setItem(OWNER_KEY, token);
            } catch {
                /* storage blocked */
            }
        }
        try {
            globalThis.sessionStorage?.setItem(TAB_KEY, JSON.stringify({ token, db }));
        } catch {
            /* storage blocked: the next reload claims afresh */
        }
        writeBeat(token, { at: now, instance });
        startHeartbeat(token, instance);
        const sweptArchives = await sweepGoneTabs(token).catch((e: unknown) => {
            console.warn('[event-log] sweeping closed tabs failed', e);
            return 0;
        });
        return { token, db, sweptArchives };
    });
}

function startHeartbeat(token: string, instance: string): void {
    const g = globalThis;
    const beat = (left = false): void =>
        writeBeat(token, { at: Date.now(), instance, ...(left ? { left: true } : {}) });
    const timer = setInterval(() => beat(), TAB_BEAT_MS);
    g.addEventListener?.('pagehide', () => {
        clearInterval(timer);
        beat(true);
    });
    /* Back from the bfcache: the page is running again. */
    g.addEventListener?.('pageshow', (e) => {
        if ((e as PageTransitionEvent).persisted) {
            beat();
            startHeartbeat(token, instance);
        }
    });
}

/** Archive (when unsaved) and delete the databases of tabs that are gone.
 *  Returns how many archive entries were created. */
async function sweepGoneTabs(selfToken: string): Promise<number> {
    const now = Date.now();
    let archived = 0;
    for (const name of await listSecondaryDbs()) {
        const token = tokenOf(name);
        if (token === selfToken || !isTabGone(readBeat(token), now)) continue;
        try {
            const status = await inspectActiveLog(name);
            if (status.clean === false && status.hasContent) {
                await archiveLog(name);
                archived += 1;
            }
            await deleteLogDb(name);
            globalThis.localStorage?.removeItem(BEAT_PREFIX + token);
        } catch (e: unknown) {
            console.warn(`[event-log] could not sweep ${name}`, e);
        }
    }
    return archived;
}
