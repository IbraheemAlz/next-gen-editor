/**
 * @nge/ui — public barrel.
 *
 * Consumers must also import the theme stylesheet exactly once at the
 * app root, e.g.
 *
 *   import '@nge/ui/theme.css';
 *
 * Every component below applies its own per-component stylesheet via a
 * sibling .css import — no consumer wiring needed beyond the theme.
 */
export { focusEditorInput } from './focus';

export { DevHud } from './DevHud';
export type { DevHudProps } from './DevHud';

export { ZoomControls } from './ZoomControls';
export type { ZoomControlsProps } from './ZoomControls';

export { UnderlineStyleDropdown } from './UnderlineStyleDropdown';
export type { UnderlineStyleDropdownProps } from './UnderlineStyleDropdown';

export { SuperSubButtons } from './SuperSubButtons';

export { InsertImageButton } from './InsertImageButton';
export type { InsertImageButtonProps } from './InsertImageButton';

export { ImageWrapPicker } from './ImageWrapPicker';
export type { ImageWrapPickerProps, ImageWrapTarget } from './ImageWrapPicker';

export { TrackChangesSidebar } from './TrackChangesSidebar';
export type { TrackChangesSidebarProps } from './TrackChangesSidebar';

export { CommentsRail } from './CommentsRail';
export type { CommentsRailProps } from './CommentsRail';

export { CommentHighlights } from './CommentHighlights';
export type { CommentHighlightsProps } from './CommentHighlights';
export { CommentConnectors } from './CommentConnectors';
export { commentFocusFor } from './commentFocus';
export type { CommentFocus } from './commentFocus';

export { TableContextMenu } from './TableContextMenu';
export type { TableContextMenuProps } from './TableContextMenu';

export { LayoutControls } from './LayoutControls';
export type { LayoutControlsProps } from './LayoutControls';

export { FileMenu } from './FileMenu';
export type { FileMenuProps } from './FileMenu';

export { Dialog } from './Dialog';
export type { DialogProps } from './Dialog';

export { CellPropertiesDialog } from './CellPropertiesDialog';
export type { CellPropertiesDialogProps } from './CellPropertiesDialog';

export { ParagraphBordersDialog } from './ParagraphBordersDialog';
export type { ParagraphBordersDialogProps } from './ParagraphBordersDialog';

export { BorderEditor } from './BorderEditor';
export type { BorderEditorProps } from './BorderEditor';

export { PageSetupDialog } from './PageSetupDialog';
export type { PageSetupDialogProps } from './PageSetupDialog';

export { StylesDropdown } from './StylesDropdown';
export { ListButtons } from './ListButtons';
export { ParagraphControls } from './ParagraphControls';
export { ReviewControls } from './ReviewControls';
export type { ReviewControlsProps } from './ReviewControls';

export { StatusBar } from './StatusBar';
export type { StatusBarProps } from './StatusBar';

export { ProtectionBadge, PROTECTION_LABELS } from './ProtectionBadge';

export { SettingsMenu } from './SettingsMenu';

export { ErrorToast, ERROR_TOAST_COPY, toastMessageFor } from './ErrorToast';
export type { ErrorToastProps } from './ErrorToast';

export { TrapOverlay } from './TrapOverlay';
export type { TrapOverlayProps } from './TrapOverlay';

export { RecoveryBanner } from './RecoveryBanner';
export type { RecoveryBannerProps } from './RecoveryBanner';

export { OpenWarningsBanner } from './OpenWarningsBanner';

export { Ruler } from './Ruler';
export type { RulerProps } from './Ruler';

export { TextFormatButtons } from './TextFormatButtons';
export { FontPickers } from './FontPickers';
export { FontDialog } from './FontDialog';
export type { FontDialogProps } from './FontDialog';
export { ColorPickers } from './ColorPickers';
export { AlignmentButtons } from './AlignmentButtons';
export { InsertTableButton } from './InsertTableButton';
export type { InsertTableButtonProps } from './InsertTableButton';
export { HeaderFooterButtons } from './HeaderFooterButtons';
export { FieldButtons } from './FieldButtons';
export { NoteButtons } from './NoteButtons';
export { TextBoxButton } from './TextBoxButton';
export { HistoryButtons } from './HistoryButtons';
export { CapsButtons } from './CapsButtons';
