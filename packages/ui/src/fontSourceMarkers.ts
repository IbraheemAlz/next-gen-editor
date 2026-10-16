/**
 * Pure helpers for the font family picker's origin marker (issues #423,
 * #329, #474) — kept out of the Solid components so the wording is
 * unit-testable without a DOM.
 *
 * The engine reports where a slot's family came from (`FontSource`) and,
 * separately, whether a theme binding named it (`*_from_theme`): a family
 * the engine substitutes reads `Substituted` whatever cascade level named
 * it, so the source alone cannot say "this is the document's theme font,
 * laid out with Carlito".
 */
import type { BridgeFontSources } from '@nge/core';

export type FontSlotKey = 'latin' | 'complex_script';

/** Whether a theme binding named the slot's family — `Theme` itself, or
 *  the origin flag next to a `Substituted` source (issue #474). */
export function slotFromTheme(
    sources: BridgeFontSources | undefined,
    slot: FontSlotKey,
): boolean {
    if (!sources) return false;
    if (sources[slot] === 'Theme') return true;
    return (slot === 'latin' ? sources.latin_from_theme : sources.complex_script_from_theme) === true;
}

/** The marker beside the family picker for a substituted family:
 *  "(Carlito)", or "(theme → Carlito)" when a theme binding named it. */
export function substituteMarker(substitute: string, fromTheme: boolean): string {
    return fromTheme ? `(theme → ${substitute})` : `(${substitute})`;
}

/** The marker's tooltip. */
export function substituteMarkerTitle(
    family: string,
    substitute: string,
    fromTheme: boolean,
): string {
    return fromTheme
        ? `${family} comes from the document theme and is not installed; the document is laid out with ${substitute}`
        : `${family} is not installed; the document is laid out with ${substitute}`;
}
