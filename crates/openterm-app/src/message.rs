//! The single `Message` type the iced runtime delivers to `update`.
//!
//! Some variants are wired ahead of their UI (SFTP write/mkdir/remove/rename,
//! palette actions) so the connection actor and message plumbing are complete
//! before Phase 2/3 surface them.
#![allow(dead_code)]

use iced::{Point, Size};

use crate::connection::Event as ConnEvent;
use crate::session::AuthMode;

#[derive(Debug, Clone)]
pub enum Message {
    // --- Host sidebar ---
    HostSearchChanged(String),
    /// One-click: open a session for the saved host and connect immediately.
    ConnectSavedHost(usize),
    /// Open the host editor for a saved host (pencil).
    EditSavedHost(usize),
    DeleteSavedHost(usize),
    /// Confirm the pending saved-host delete (sidebar ✕ → modal → Delete).
    ConfirmDeleteHost,
    /// Dismiss the saved-host delete confirmation.
    CancelDeleteHost,
    NewHost,
    /// Row hover tracking so edit/delete affordances only show on the hovered row.
    HostHovered(Option<usize>),
    /// Collapse/expand the whole sidebar (VS Code style).
    ToggleSidebar,
    /// Begin / drag / end resizing the sidebar via its right divider.
    SidebarDragStart,
    SidebarDragMove(Point),
    SidebarDragEnd,
    /// Drag the left edge of the resource rail to resize it.
    RailDragStart,
    RailDragMove(Point),
    RailDragEnd,

    // --- Session tabs ---
    SelectTab(usize),
    NewTab,
    CloseTab(usize),
    /// Open a local shell (zsh/bash/etc.) as a new tab.
    NewLocalShell,
    /// Open a new tab with the same SSH config as the active session and connect immediately.
    DuplicateTab,
    /// Begin dragging the whole window (empty tab-bar area acts as the titlebar).
    /// A double press of this within 400ms zooms (maximizes) the window instead.
    StartWindowDrag,
    /// Keyboard modifier state changed (tracked for multi-select clicks).
    ModifiersChanged(iced::keyboard::Modifiers),

    // --- Connection form (active session's config) ---
    NameChanged(String),
    HostChanged(String),
    UserChanged(String),
    PortChanged(String),
    AuthModeChanged(AuthMode),
    PasswordChanged(String),
    KeyPathChanged(String),
    PassphraseChanged(String),
    GroupChanged(String),
    TagsChanged(String),
    BrowseKeyFile,
    KeyFileSelected(Option<String>),
    ToggleJump,
    JumpHostChanged(String),
    Connect,
    Disconnect,
    /// Terminal text selection changed (col1, row1, col2, row2); None clears it.
    SelectionChanged(Option<(usize, usize, usize, usize)>),
    /// Copy the current terminal selection to the system clipboard.
    TerminalCopy,
    SaveHost,

    // --- Host editor modal ---
    CloseEditor,

    // --- Host key confirmation ---
    AcceptHostKey,
    RejectHostKey,

    // --- Terminal ---
    TerminalInput(Vec<u8>),
    TerminalAreaResized(Size),
    TerminalScroll(f32),
    WindowResized(Size),
    PasteRequested,
    PasteReady(Option<String>),
    ClearTerminal,
    FontSizeDelta(i16),
    // Command-history side panel
    ToggleHistory,
    HistoryInsert(String),
    /// Copy a history command to the system clipboard.
    HistoryCopyCmd(String),
    /// Update the keyword/time filter in the history panel.
    HistoryFilterChanged(String),
    /// Clear all persisted history.
    HistoryClearAll,
    HistoryDragStart,
    HistoryDragMove(Point),
    HistoryDragEnd,

    // --- SFTP (Phase 2) ---
    /// Switch the connected session's workspace view: false = terminal, true = files.
    ShowFiles(bool),
    ToggleSftp,
    /// Show/hide the always-on resource rail.
    ToggleMonitor,
    /// Periodic tick (while connected) to sample remote metrics.
    MetricsTick,
    /// Open the rail's process expander on a metric (CPU/Memory sort).
    MonitorSelect(crate::session::MonitorPanel),
    /// Collapse the process expander.
    MonitorCloseDetail,
    SftpRefresh,
    SftpSetSort(crate::session::SftpSide, crate::session::SortField),
    SftpRemotePathChanged(String),
    SftpSelectRemote(usize),
    /// Enter the remote directory at this index (single click on a folder).
    SftpEnterRemote(usize),
    SftpParentDir,
    SftpDownloadSelected,
    SftpDeleteRemoteSelected,
    // Local pane
    SftpLocalPathChanged(String),
    SftpSelectLocal(usize),
    /// Enter the local directory at this index (single click on a folder).
    SftpEnterLocal(usize),
    SftpLocalParentDir,
    SftpUploadSelected,
    // Context menu + quick operations
    SftpOpenMenu(crate::session::SftpSide, usize),
    SftpCloseMenu,
    SftpMenuDownload,
    SftpMenuUpload,
    SftpMenuDelete,
    /// Confirm the pending delete and actually remove the entry.
    SftpConfirmDelete,
    /// Dismiss the delete confirmation without deleting.
    SftpCancelDelete,
    /// Begin a rename for the menu's target row.
    SftpStartRename,
    /// Begin creating a new folder in `side`'s current directory.
    SftpStartNewFolder(crate::session::SftpSide),
    SftpPromptChanged(String),
    SftpPromptConfirm,
    SftpPromptCancel,
    /// Open the chmod modal for the selected remote entry.
    SftpMenuChmod,
    /// Update the octal input in the chmod modal.
    SftpChmodInput(String),
    /// Apply the chmod.
    SftpChmodConfirm,
    /// Dismiss the chmod modal.
    SftpChmodCancel,

    // --- Settings ---
    OpenSettings,
    CloseSettings,
    SettingsPanelChanged(crate::session::SettingsPanel),
    SettingsDefaultUserChanged(String),
    SettingsDefaultPortChanged(String),
    SettingsFontSize(i16),
    SettingsServerAliveInterval(String),
    SettingsOnDisconnect(crate::session::OnDisconnect),
    SettingsColorScheme(crate::theme::ColorScheme),

    // --- Command palette ---
    TogglePalette,
    PaletteQueryChanged(String),
    PaletteMove(i32),
    PaletteRunSelected,
    PaletteRun(Box<Message>),
    ClosePalette,

    // --- File Viewer ---
    /// Open the file viewer for the given remote path (single click in SFTP).
    OpenFileViewer(String),
    /// Close the file viewer panel.
    FileViewerClose,
    /// A chunk of file content arrived from the connection actor.
    FileViewerChunk { offset: u64, data: Vec<u8>, total: u64 },
    /// Switch between Preview and Edit mode.
    FileViewerToggleEdit,
    /// Editor action (multi-line text_editor widget).
    FileViewerAction(iced::widget::text_editor::Action),
    /// Text changed in the editor (legacy single-line path, unused).
    FileViewerTextChanged(String),
    /// Search query changed.
    FileViewerSearchChanged(String),
    /// Replace string changed.
    FileViewerReplaceChanged(String),
    FileViewerSearchNext,
    FileViewerSearchPrev,
    FileViewerReplaceOne,
    FileViewerReplaceAll,
    /// Save the edited file back to the server.
    FileViewerSave,
    /// FileSaved event: Ok or Err message.
    FileViewerSaved(Result<(), String>),
    /// Load the next page (log mode).
    FileViewerNextPage,
    FileViewerPrevPage,
    /// Scroll position updated.
    FileViewerScroll(f32),

    // --- Connection worker events ---
    Conn(ConnEvent),

    // --- Ping ---
    PingTick,
    PingResult { host_id: openterm_core::HostId, latency_ms: Option<u32> },

    // --- Misc ---
    PointerMoved(Point),
    /// Animation frame tick (only delivered while something is animating).
    Tick(std::time::Instant),
    /// Slow heartbeat for the "connecting" status-dot pulse (≈700 ms).
    PulseTick,

    // --- Terminal search (Cmd+F) ---
    TerminalSearchOpen,
    TerminalSearchQuery(String),
    TerminalSearchNext,
    TerminalSearchPrev,
    TerminalSearchClose,

    // --- Vault master password ---
    VaultPasswordInput(String),
    VaultConfirmInput(String),
    VaultSubmit,
    VaultLock,
    VaultCheckLock,
    /// Result of async unlock attempt: Ok(master_password) or Err(reason).
    VaultUnlockResult(Result<String, String>),
    /// Result of async vault setup: Ok(master_password) or Err(reason).
    VaultSetupResult(Result<String, String>),

    Noop,
}
