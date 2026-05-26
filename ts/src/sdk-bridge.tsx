/**
 * sdk-bridge — adapter that wires the existing concrete EngineClient
 * (still living in `ts/src/engine/engine-client.ts`) into the new
 * @nge/core context, and assembles the Sprint 1 (UI Edition) shelf of
 * components from @nge/ui.
 *
 * This file is the migration anchor: App.tsx can mount <SdkShelf
 * client={engineClient}/> alongside its existing Solid tree to gain the
 * new Dev HUD, zoom widget, underline dropdown, super/sub buttons,
 * insert-image button, track-changes sidebar, and comments rail —
 * without rewriting any of the existing Toolbar / EditorCanvas /
 * HiddenInput plumbing.
 *
 * Once the existing Toolbar is fully migrated onto createEditorCommands,
 * the legacy Toolbar.tsx can be deleted and this adapter collapses into
 * App.tsx directly.
 */
import type { Component } from 'solid-js';
import {
    EngineProvider,
    type EngineHandle,
} from '@nge/core';
import {
    DevHud,
    ZoomControls,
    UnderlineStyleDropdown,
    SuperSubButtons,
    InsertImageButton,
    TrackChangesSidebar,
    CommentsRail,
    TableContextMenu,
    LayoutControls,
    FileMenu,
    StylesDropdown,
    ListButtons,
    ParagraphControls,
    ReviewControls,
    StatusBar,
    TrapOverlay,
} from '@nge/ui';
import type { EngineClient } from './engine/engine-client';

import '@nge/ui/theme.css';

export interface SdkShelfProps {
    client: EngineClient;
}

/**
 * Convenience composite that drops the entire Sprint 1 (UI Edition)
 * shelf into an app in one mount. Useful for the QA playground; in
 * production each component can be placed individually.
 */
export const SdkShelf: Component<SdkShelfProps> = (props) => {
    /* The concrete EngineClient already implements the EngineHandle
     * contract (dispatch + subscribe + init + recover + crossOriginIsolated
     * + renderer + revisionsSnapshot + commentsSnapshot). Cast is safe. */
    const handle = props.client as unknown as EngineHandle;

    return (
        <EngineProvider client={handle}>
            <div class="nge-root nge-sprint1-shelf">
                <div class="nge-sprint1-shelf__toolbar">
                    <FileMenu />
                </div>
                <div class="nge-sprint1-shelf__toolbar">
                    <StylesDropdown />
                    <UnderlineStyleDropdown />
                    <SuperSubButtons />
                    <ListButtons />
                    <InsertImageButton />
                </div>
                <div class="nge-sprint1-shelf__toolbar">
                    <ParagraphControls />
                    <LayoutControls />
                    <ReviewControls />
                </div>
                <div class="nge-sprint1-shelf__rails">
                    <TrackChangesSidebar />
                    <CommentsRail />
                </div>
                <TableContextMenu />
                <DevHud pollMs={1000} />
                <TrapOverlay />
                <StatusBar />
            </div>
        </EngineProvider>
    );
};
