/**
 * @nge/core — public barrel.
 *
 * Two halves:
 *   1. The Locked Surface — <EditorSurface>, EngineProvider — encapsulates
 *      the canvas, hidden input, hit-test math, and worker bootstrap.
 *   2. The Headless API — createEditorCommands, createEditorState — Solid
 *      primitives that expose engine commands + state to downstream UI.
 *
 * Downstream UI never imports from `crates/engine-wasm/pkg` directly.
 * @nge/core is the entire engine surface.
 */
export { EditorSurface } from './EditorSurface';
export type { EditorSurfaceProps, EditorSurfaceHandle } from './EditorSurface';

export { EngineProvider, useEngine } from './EngineProvider';
export type { EngineProviderProps, EngineHandle } from './EngineProvider';

export { createEditorCommands, STYLE_PRESETS, emptyPatch } from './createEditorCommands';
export type { EditorCommands, ParagraphStyleId } from './createEditorCommands';

export { createEditorState } from './createEditorState';
export type { EditorState } from './createEditorState';

export { createFontRegistry } from './createFontRegistry';
export type {
    FontRegistry,
    FontDescriptor,
    FontManifest,
    FontLoadState,
    FontEngine,
    FontRegistryConfig,
} from './createFontRegistry';

export { FontRegistryProvider, useFontRegistry } from './FontRegistryProvider';
export type { FontRegistryProviderProps } from './FontRegistryProvider';

export type {
    EngineClientLike,
    EngineClientSnapshots,
    RevisionSnapshot,
    CommentSnapshot,
} from './types';

export type {
    Command,
    Event,
    DocFormat,
    TextAttrsPatch,
    UnderlineStyle,
    VerticalScript,
    Alignment,
    Direction,
    PdfConformance,
    ImageFit,
    ImageBlob,
    SelectionModifier,
    MoveDirection,
    BridgeBorderStyle,
    BridgeCellBorders,
    BridgeBorderStroke,
    InsertSide,
    PageOrientation,
    SectionBreakKind,
    HeaderFooterArea,
    BridgeStoryRef,
    ListKind,
    LogicalPos,
    LogicalRange,
    BlockPath,
    PathStep,
    SelectionKind,
    Rect,
    Point,
    Color,
    Script,
    TextAttrs,
    AttrsMixed,
    EngineStats,
    EngineCapabilities,
    A11yTree,
    A11yPatch,
    AnnouncementPriority,
    BridgeSectionGeometry,
    BridgeCellProperties,
    BridgeTabStop,
    BridgeTabKind,
    BridgeIndent,
    BridgeStyleProperties,
    BridgeSpanStylePatch,
    BridgeParaPropertiesPatch,
} from './types';
