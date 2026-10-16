import { describe, expect, it } from 'vitest';
import type { BridgeFontSources } from '@nge/core';
import { slotFromTheme, substituteMarker, substituteMarkerTitle } from './fontSourceMarkers';

const sources = (extra: Partial<BridgeFontSources>): BridgeFontSources => ({
    latin: 'Default',
    complex_script: 'Default',
    ...extra,
});

describe('slotFromTheme (issue #474)', () => {
    it('is false without a read-back', () => {
        expect(slotFromTheme(undefined, 'latin')).toBe(false);
    });

    it('reads a plain Theme source as a theme font', () => {
        const s = sources({ latin: 'Theme' });
        expect(slotFromTheme(s, 'latin')).toBe(true);
        expect(slotFromTheme(s, 'complex_script')).toBe(false);
    });

    it('keeps the theme origin of a substituted family (the flag, not the source)', () => {
        const s = sources({
            latin: 'Substituted',
            latin_from_theme: true,
            complex_script: 'Substituted',
        });
        expect(slotFromTheme(s, 'latin')).toBe(true);
        expect(slotFromTheme(s, 'complex_script')).toBe(false);
    });

    it('reads the complex-script slot through its own flag', () => {
        const s = sources({ complex_script: 'Substituted', complex_script_from_theme: true });
        expect(slotFromTheme(s, 'complex_script')).toBe(true);
        expect(slotFromTheme(s, 'latin')).toBe(false);
    });

    it('does not call an explicit / style / substituted-style family a theme font', () => {
        expect(slotFromTheme(sources({ latin: 'Explicit' }), 'latin')).toBe(false);
        expect(slotFromTheme(sources({ latin: 'Style' }), 'latin')).toBe(false);
        expect(slotFromTheme(sources({ latin: 'Substituted' }), 'latin')).toBe(false);
    });
});

describe('substituteMarker (issue #474)', () => {
    it('shows the substitute alone for a family the document names itself', () => {
        expect(substituteMarker('Carlito', false)).toBe('(Carlito)');
    });

    it('shows the theme origin for a substituted theme font', () => {
        expect(substituteMarker('Carlito', true)).toBe('(theme → Carlito)');
        expect(substituteMarker('Noto Naskh Arabic', true)).toBe('(theme → Noto Naskh Arabic)');
    });

    it('titles the marker with the origin', () => {
        expect(substituteMarkerTitle('Calibri', 'Carlito', false)).toBe(
            'Calibri is not installed; the document is laid out with Carlito',
        );
        expect(substituteMarkerTitle('Calibri', 'Carlito', true)).toContain(
            'comes from the document theme',
        );
    });
});
