//! Command palette actions. The palette (Cmd/Ctrl+K) is the single place every
//! secondary capability lives, so the main UI stays free of permanent buttons.
//! Each action is context-aware: only the ones that make sense for the active
//! session's state are offered.

use crate::message::Message;
use crate::session::Phase;
use crate::App;

/// A runnable palette entry.
#[derive(Debug, Clone)]
pub struct PaletteAction {
    pub title: &'static str,
    pub hint: &'static str,
    /// Lowercase keywords for fuzzy matching.
    pub keywords: &'static str,
    pub message: Message,
}

/// Build the list of actions available right now, filtered by `query`.
pub fn actions_for(app: &App, query: &str) -> Vec<PaletteAction> {
    let phase = app
        .active_session()
        .map(|s| s.phase.clone())
        .unwrap_or(Phase::Idle);
    let connected = phase == Phase::Connected;
    let sftp_open = app.active_session().map(|s| s.sftp_open).unwrap_or(false);

    let mut all: Vec<PaletteAction> = Vec::new();

    // Always available.
    all.push(PaletteAction {
        title: "New session",
        hint: "Open a new terminal tab",
        keywords: "new session tab open create",
        message: Message::NewTab,
    });

    if !phase.is_active() {
        all.push(PaletteAction {
            title: "Connect",
            hint: "Connect this session",
            keywords: "connect ssh start",
            message: Message::Connect,
        });
        all.push(PaletteAction {
            title: "Save host",
            hint: "Save these details to the sidebar",
            keywords: "save host store bookmark",
            message: Message::SaveHost,
        });
    }

    if connected {
        all.push(PaletteAction {
            title: if sftp_open {
                "Hide files"
            } else {
                "Browse files (SFTP)"
            },
            hint: "Toggle the dual-pane file manager",
            keywords: "files sftp upload download transfer browse",
            message: Message::ToggleSftp,
        });
        all.push(PaletteAction {
            title: "Disconnect",
            hint: "Close this shell",
            keywords: "disconnect close quit exit",
            message: Message::Disconnect,
        });
        all.push(PaletteAction {
            title: "Command history",
            hint: "Toggle the command-history panel",
            keywords: "history commands recent panel side",
            message: Message::ToggleHistory,
        });
        all.push(PaletteAction {
            title: "Search history",
            hint: "Fuzzy-search past commands (Ctrl+R)",
            keywords: "search history reverse ctrl-r find command",
            message: Message::HistorySearchOpen,
        });
        all.push(PaletteAction {
            title: "Clear terminal",
            hint: "Wipe the current screen",
            keywords: "clear cls wipe terminal",
            message: Message::ClearTerminal,
        });
    }

    if app.session_count() > 1 {
        all.push(PaletteAction {
            title: "Close tab",
            hint: "Close the active session tab",
            keywords: "close tab remove",
            message: Message::CloseTab(usize::MAX),
        });
    }

    // Font size.
    all.push(PaletteAction {
        title: "Increase font size",
        hint: "Make terminal text larger",
        keywords: "font size larger bigger zoom in increase",
        message: Message::FontSizeDelta(1),
    });
    all.push(PaletteAction {
        title: "Decrease font size",
        hint: "Make terminal text smaller",
        keywords: "font size smaller zoom out decrease",
        message: Message::FontSizeDelta(-1),
    });
    all.push(PaletteAction {
        title: "Settings",
        hint: "Open preferences",
        keywords: "settings preferences options config defaults font user port",
        message: Message::OpenSettings,
    });

    // Filter by fuzzy query.
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return all;
    }
    all.into_iter()
        .filter(|a| {
            a.title.to_lowercase().contains(&q)
                || a.hint.to_lowercase().contains(&q)
                || a.keywords.contains(&q)
                || fuzzy(&a.title.to_lowercase(), &q)
        })
        .collect()
}

/// Loose subsequence match (characters of `needle` appear in order in `hay`).
fn fuzzy(hay: &str, needle: &str) -> bool {
    let mut chars = hay.chars();
    needle.chars().all(|c| chars.any(|h| h == c))
}
