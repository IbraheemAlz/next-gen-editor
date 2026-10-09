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
 *
 * Issue #340 - the SDK exposes NO globals: nothing here (or in @nge/ui)
 * assigns to `window`, reads debug URL parameters, or picks a telemetry
 * sink from the page URL. The engine handle comes from the host via
 * `<EngineProvider client={...}>` and the telemetry endpoint via its
 * `telemetryEndpoint` prop. The `window.__dispatch` / `__engineClient`
 * test hooks belong to the reference shell (`ts/src/dev-hooks.ts`) and
 * are installed only in dev / `?test=` / `VITE_NGE_DEV_HOOKS=1` builds.
 */
export { EditorSurface } from './EditorSurface';
export type { EditorSurfaceProps, EditorSurfaceHandle } from './EditorSurface';

export {
    EngineProvider,
    useEngine,
    useDocumentDefaults,
    useTelemetryEndpoint,
    useDebugSurfaces,
} from './EngineProvider';
export type { EngineProviderProps, EngineHandle } from './EngineProvider';

export {
    createEditorCommands,
    docFormatForFileName,
    DEFAULT_TOC_SWITCHES,
    STYLE_PRESETS,
    emptyPatch,
    emptyDocumentDefaults,
} from './createEditorCommands';
export type { EditorCommands, ParagraphStyleId, SlotFormatPatch } from './createEditorCommands';

export { createEditorState } from './createEditorState';
export type { EditorState, CheckpointHealth, EditorError } from './createEditorState';

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

export { createTelemetryConfig } from './createTelemetryConfig';
export type { TelemetryConfig } from './createTelemetryConfig';

export { TelemetryProvider, useTelemetryConfig } from './TelemetryProvider';
export type { TelemetryProviderProps } from './TelemetryProvider';

export type {
    EngineClientLike,
    EngineClientSnapshots,
    RevisionSnapshot,
    CommentSnapshot,
    RecoveryReport,
    CheckpointStatus,
    PreviousSessionInfo,
} from './types';

export {
    recoveryNotices,
    recoveryDegraded,
    checkpointNotices,
    previousSessionNotice,
} from './recovery';

/* Issue #342 — per-command metadata generated from `bridge::meta` (also
 * importable on its own as `@nge/core/command-meta`, which the worker and
 * `EngineClient` use so they never pull Solid into the worker bundle). */
export { COMMAND_META, commandMeta } from './commandMeta.generated';
export { COMMAND_FACADE, FACADE_METHOD_COMMANDS } from './facadeMap';
export type {
    FacadeTarget,
    FacadeMemberTarget,
    LiveCommand,
    LiveCommandType,
} from './facadeMap';
export type {
    CommandMeta,
    CommandStatus,
    CommandType,
    StoryPolicy,
    StubCommandType,
    PartialCommandType,
} from './commandMeta.generated';
export type { RecoveryNotice, RecoveryNoticeKind, RecoveryNoticeOptions } from './recovery';

export type {
    Command,
    Event,
    DocFormat,
    DocumentDefaults,
    DefaultPageSize,
    ErrorKind,
    PackageLimitsOverride,
    TextAttrsPatch,
    UnderlineStyle,
    VerticalScript,
    FormattingToggle,
    FontSlot,
    Alignment,
    Direction,
    PdfConformance,
    ImageFit,
    ImageBlob,
    ImageRect,
    ImageWrapMode,
    TextBoxHop,
    SelectionModifier,
    MoveDirection,
    BridgeBorderStyle,
    BridgeCellBorders,
    BridgeBorderStroke,
    TablePropertiesPatch,
    InsertSide,
    PageOrientation,
    SectionBreakKind,
    HeaderFooterArea,
    BridgeStoryRef,
    BridgeFieldRef,
    BridgeHfRole,
    FieldKind,
    TocSwitches,
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
    FontSource,
    BridgeFontSources,
    BridgeSlotFormat,
    BridgeSlotFormats,
    EngineStats,
    EngineCapabilities,
    A11yTree,
    A11yPatch,
    A11yTextBox,
    A11yNote,
    A11yNoteKind,
    A11yNoteRef,
    A11yObjectKind,
    A11yObjectRef,
    AnnouncementPriority,
    BridgeSectionGeometry,
    BridgeCellProperties,
    BridgeTabStop,
    BridgeTabKind,
    BridgeTabLeader,
    BridgeIndent,
    BridgeStyleProperties,
    BridgeSpanStylePatch,
    BridgeParaPropertiesPatch,
    RendererDowngrade,
    RendererDowngradeReason,
} from './types';
