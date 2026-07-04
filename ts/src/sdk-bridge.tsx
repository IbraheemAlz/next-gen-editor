/**
 * sdk-bridge — adapter that wires the existing concrete EngineClient
 * (still living in `ts/src/engine/engine-client.ts`) into the new
 * @nge/core context, and assembles the default `@nge/ui` shell around
 * the editor canvas.
 *
 * The shell is a CSS-Grid frame (see `@nge/ui/Shell.css`): stacked
 * toolbar rows on top, ruler beneath, the editor canvas in the main
 * grid track, the review rails (Track Changes + Comments) pinned to
 * the right, and the StatusBar pinned to the bottom. The host app
 * (`ts/src/App.tsx`) passes the canvas + overlays in as `children`.
 *
 * `engineReady` gates the right-rail snapshot calls so they don't fire
 * before the worker finishes `Command::Init`; otherwise the engine
 * rejects with "engine not initialized" and the rails render a red
 * error banner.
 */
import { Show, type JSX, type Component } from 'solid-js';
import {
    EngineProvider,
    FontRegistryProvider,
    type EngineHandle,
    type FontRegistry,
} from '@nge/core';
import {
    DevHud,
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
    Ruler,
    ZoomControls,
    TextFormatButtons,
    FontPickers,
    ColorPickers,
    AlignmentButtons,
    InsertTableButton,
    HeaderFooterButtons,
    HistoryButtons,
    CapsButtons,
} from '@nge/ui';
import type { EngineClient } from './engine/engine-client';

import '@nge/ui/theme.css';

export interface SdkShelfProps {
    client: EngineClient;
    /** Data-driven font registry — supplies the FontPickers dropdown and
     *  the JIT loader. Shared with App's boot sequence so the resident-font
     *  cache is one instance across boot + toolbar. */
    fontRegistry: FontRegistry;
    /** Editor canvas + overlays mount here, inside the main grid track. */
    children: JSX.Element;
    /** True once the worker has finished INIT. Defaults to true if omitted
     *  so callers without a boot signal still mount the rails. */
    engineReady?: () => boolean;
}

export const SdkShelf: Component<SdkShelfProps> = (props) => {
    /* The concrete EngineClient already implements the EngineHandle
     * contract (dispatch + subscribe + init + recover + crossOriginIsolated
     * + renderer + revisionsSnapshot + commentsSnapshot). Cast is safe. */
    const handle = props.client as unknown as EngineHandle;
    const ready = () => (props.engineReady ? props.engineReady() : true);

    return (
        <EngineProvider client={handle}>
          <FontRegistryProvider registry={props.fontRegistry}>
            <div class="nge-root nge-shell">
                <header class="nge-shell__topbar">
                    <div class="nge-shell__toolbar-row">
                        <FileMenu />
                        <StylesDropdown />
                        <HistoryButtons />
                    </div>
                    <div class="nge-shell__toolbar-row">
                        <FontPickers />
                        <TextFormatButtons />
                        <UnderlineStyleDropdown />
                        <SuperSubButtons />
                        <CapsButtons />
                        <ColorPickers />
                    </div>
                    <div class="nge-shell__toolbar-row">
                        <AlignmentButtons />
                        <ListButtons />
                        <ParagraphControls />
                        <InsertImageButton />
                        <InsertTableButton />
                        <HeaderFooterButtons />
                        <LayoutControls />
                        <ReviewControls />
                    </div>
                </header>
                <div class="nge-shell__ruler">
                    <Ruler />
                </div>
                <main class="nge-shell__main">
                    <div class="nge-shell__canvas">{props.children}</div>
                </main>
                <aside class="nge-shell__rails">
                    <Show when={ready()}>
                        <TrackChangesSidebar />
                        <CommentsRail />
                    </Show>
                </aside>
                <footer class="nge-shell__statusbar">
                    <StatusBar />
                    <ZoomControls />
                </footer>
                <TableContextMenu />
                <DevHud pollMs={1000} />
                <TrapOverlay />
            </div>
          </FontRegistryProvider>
        </EngineProvider>
    );
};
