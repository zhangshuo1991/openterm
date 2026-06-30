//! OpenTerm desktop app — session-centric SSH workbench.
//!
//! Architecture (see `session.rs` / `connection.rs`):
//! * `App` owns `Vec<Session>` + `active`. Switching tabs changes an index;
//!   no state is copied, so background sessions keep their own terminal grids.
//! * Each session has one connection worker (an iced subscription) that holds
//!   the live SSH connection and multiplexes shell + SFTP over it.

mod connection;
mod highlight;
mod keys;
mod message;
mod metrics;
mod palette;
mod session;
mod smoke;
mod terminal_render;
mod theme;
mod ui;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::animation::Animation;
use iced::{Element, Size, Subscription, Task, Theme};
use openterm_core::{AuthRef, HostId, HostProfile};
use openterm_crypto::{LocalVault, VaultConfig};
use openterm_ssh::{AuthMethod, ConnectOptions, ConnectRoute, HostKeyPolicy};
use openterm_storage::{UiSettings, WorkspaceStore};
use std::collections::HashMap;

use crate::message::Message;
use crate::session::{AuthMode, Session, SessionConfig};

/// Built-in key used to encrypt saved secrets while the credential vault is
/// disabled (the default). It keeps stored passwords from being plain text
/// without requiring a master password; it is not a security boundary against
/// someone with local file access. Enabling the vault re-encrypts everything
/// under the user's master password instead.
const VAULT_DEFAULT_KEY: &[u8] = b"openterm-local-default-vault-v1";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

fn main() -> iced::Result {
    // Fuse the native macOS titlebar into our chrome: the content fills the full
    // window height and the traffic lights float over our tab bar, reclaiming the
    // ~28px the system titlebar used to take.
    let window = iced::window::Settings {
        size: iced::Size::new(1200.0, 780.0),
        platform_specific: iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        ..Default::default()
    };

    iced::application(App::new, App::update, App::view)
        .title("OpenTerm")
        .theme(app_theme)
        .subscription(App::subscription)
        .window(window)
        .centered()
        .run()
}

fn app_theme(_app: &App) -> Theme {
    Theme::Dark
}

pub struct App {
    db_path: PathBuf,
    hosts: Vec<HostProfile>,
    host_search: String,
    sessions: Vec<Session>,
    active: usize,
    next_session_id: u64,
    /// Sidebar width (resizable) and whether it's collapsed (VS Code style).
    sidebar_width: f32,
    sidebar_collapsed: bool,
    sidebar_dragging: bool,
    /// Which host row is hovered, so edit/delete icons only show on that row.
    hovered_host: Option<usize>,
    /// Saved-host index pending a delete confirmation (sidebar ✕).
    pending_host_delete: Option<usize>,
    /// Latest keyboard modifiers, for multi-select clicks in the SFTP panes.
    modifiers: iced::keyboard::Modifiers,
    /// Current window size, used to derive the terminal grid.
    window_size: Size,
    font_size: u16,
    color_scheme: crate::theme::ColorScheme,
    /// Default username/port pre-filled for new sessions (from settings).
    default_user: String,
    default_port: String,
    /// SFTP sort field + direction (shared by both panes).
    sftp_sort: session::SortField,
    sftp_sort_asc: bool,
    sftp_sort_local: session::SortField,
    sftp_sort_asc_local: bool,
    /// Which SFTP row (if any) has its context menu open: (side, index).
    sftp_menu: Option<(session::SftpSide, usize)>,
    /// Last SFTP row click (side, index, time) for double-click detection:
    /// a single click selects a row, a double click on a folder enters it.
    last_sftp_click: Option<(session::SftpSide, usize, std::time::Instant)>,
    /// Whether the always-on resource rail is collapsed (hidden) to reclaim
    /// width. Shown by default whenever the active session is connected.
    rail_collapsed: bool,
    /// Current width of the resource rail (user-resizable by dragging its left edge).
    rail_width: f32,
    rail_dragging: bool,
    /// Last press on the title/drag strip, for double-click-to-zoom detection.
    last_title_click: Option<std::time::Instant>,
    /// Active name prompt (new folder / rename), shown as an overlay.
    sftp_prompt: Option<session::SftpPrompt>,
    /// Pending delete awaiting confirmation, shown as an overlay.
    sftp_confirm: Option<session::SftpConfirm>,
    /// Monotonic id for transfers.
    next_transfer_id: u64,
    /// Command-history side panel (right of the terminal).
    history_open: bool,
    history_width: f32,
    history_dragging: bool,
    /// All persisted history entries (newest-first), loaded at startup.
    pub all_history: std::collections::VecDeque<openterm_storage::HistoryEntry>,
    /// Live keyword filter for the history panel (not persisted).
    pub history_filter: String,
    /// Whether the settings panel overlay is open.
    settings_open: bool,
    /// Ping latency per saved host (None = unreachable / not yet measured).
    pub ping_results: HashMap<HostId, Option<u32>>,
    /// Which settings panel is active.
    settings_panel: session::SettingsPanel,
    /// SSH keepalive interval (seconds).
    server_alive_interval: String,
    /// What to do on disconnect.
    on_disconnect: session::OnDisconnect,
    palette_open: bool,
    palette_query: String,
    palette_selected: usize,
    status: String,
    /// Smoke-test status file path (set only under OPENTERM_SMOKE_CONNECT).
    pub smoke_status: Option<PathBuf>,
    /// When set, the smoke run should open SFTP once output arrives.
    pub smoke_sftp_pending: bool,
    /// When set, the smoke run downloads the first remote file after listing.
    pub smoke_download_pending: bool,
    /// When set, re-open a remote context menu after the first listing (QA).
    pub smoke_open_menu: bool,
    /// Entrance animation for the connection card; `now` is the latest frame.
    card_anim: Animation<bool>,
    /// Sidebar collapse/expand slide animation (false = collapsed / 0 px).
    sidebar_anim: Animation<bool>,
    /// History panel slide animation (false = hidden / 0 px).
    history_anim: Animation<bool>,
    /// Resource rail slide animation (false = hidden / 0 px). Driven by
    /// `sync_rail()` whenever connection/tab state changes.
    rail_anim: Animation<bool>,
    /// Last-applied rail visibility target (so `sync_rail` only re-triggers the
    /// animation on an actual change).
    rail_shown: bool,
    /// Settings overlay slide-in (false = hidden, true = fully shown).
    settings_anim: Animation<bool>,
    /// Toggled every ~700 ms while any session is connecting, drives the pulse.
    pub connecting_pulse: bool,
    now: Instant,

    // --- Terminal search (Cmd+F) ---
    /// Current search query; None when the bar is closed.
    pub terminal_search: Option<String>,
    /// Which visible match is the "current" one (cycles mod match count).
    pub terminal_search_idx: usize,

    // --- Vault master password ---
    /// Whether the credential vault is enabled. When false, secrets use the
    /// built-in default key and no master password is required.
    pub vault_enabled: bool,
    /// In-memory master password. None while locked.
    pub vault_master: Option<String>,
    /// Whether a canary exists in storage (i.e. password was set up before).
    pub vault_has_canary: bool,
    /// Password field in the lock / setup dialog.
    pub vault_pw: String,
    /// Confirm field (only used during first-time setup).
    pub vault_confirm: String,
    /// Error string shown inside the vault dialog.
    pub vault_err: Option<String>,
    /// Time the vault was last used (encrypt/decrypt). Drives the 15 min auto-lock.
    pub vault_last_use: Instant,
    /// Time of the last VaultCheckLock tick. Large gaps imply the system slept.
    pub vault_last_check: Instant,
    /// When set, show the create-master-password dialog (triggered by enabling
    /// the vault from settings).
    pub vault_setup_prompt: bool,
    /// Set while a vault enable/disable re-encryption is in flight, to disable
    /// the toggle and show progress.
    pub vault_busy: bool,

    // --- Sprint 1 UI polish ---
    /// Sidebar group names the user has collapsed.
    pub collapsed_groups: std::collections::HashSet<String>,
    /// Terminal line-height multiplier (persisted).
    pub line_height: f32,
    /// Extra per-cell horizontal spacing in pixels (persisted).
    pub letter_spacing: f32,
    /// Terminal cursor shape (persisted).
    pub cursor_shape: crate::theme::CursorShape,
    /// Accent color override as "#rrggbb" (empty = scheme default).
    pub accent_hex: String,
    /// Per-tab close animations, keyed by stable `session.id` (not index).
    pub closing_tabs: HashMap<u64, Animation<bool>>,
    /// Recent TCP-ping samples per saved host, for the footer latency sparkline.
    pub ping_history: HashMap<HostId, std::collections::VecDeque<u16>>,
    /// Active toast notifications (top-right), newest last.
    pub toasts: Vec<crate::ui::toasts::Toast>,
    /// Monotonic id for toasts.
    pub next_toast_id: u64,
    /// History rows whose output preview is expanded (keyed by entry ts_ms).
    pub expanded_history: std::collections::HashSet<u64>,
    /// While set and in the future, the terminal selection flashes to confirm a
    /// copy. Drives a brief burst of frames via `any_animating`.
    pub copy_flash_until: Option<Instant>,

    // --- Sprint 3: command completion & smart input ---
    /// Saved input snippets (abbr → expansion), loaded at startup.
    pub snippets: Vec<openterm_storage::Snippet>,
    /// Draft abbreviation in the Snippets settings panel.
    pub snippet_draft_abbr: String,
    /// Draft expansion in the Snippets settings panel.
    pub snippet_draft_expansion: String,
    /// Whether the Ctrl+R history-search overlay is open.
    pub history_search_open: bool,
    /// Live query in the history-search overlay.
    pub history_search_query: String,
    /// Selected row in the history-search overlay (index into filtered list).
    pub history_search_idx: usize,
    /// Whether the connect card reveals the password/passphrase in plain text.
    pub reveal_password: bool,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let db_path = openterm_ui::default_db_path();
        let (hosts, settings, all_history, snippets) = WorkspaceStore::open(&db_path)
            .map(|store| {
                (
                    store.list_hosts().unwrap_or_default(),
                    store.get_ui_settings().ok().flatten().unwrap_or_default(),
                    store.load_history().unwrap_or_default().into(),
                    store.list_snippets().unwrap_or_default(),
                )
            })
            .unwrap_or_default();

        let font_size = settings
            .terminal_font_size
            .clamp(theme::MIN_FONT_SIZE, theme::MAX_FONT_SIZE);
        let color_scheme = crate::theme::ColorScheme::from_str(&settings.color_scheme);
        crate::theme::set_scheme(color_scheme);
        crate::theme::set_accent_override(&settings.accent_hex);
        let cursor_shape = crate::theme::CursorShape::from_str(&settings.cursor_shape);
        let line_height = if settings.line_height.is_finite() {
            settings.line_height.clamp(1.0, 2.0)
        } else {
            1.32
        };
        crate::terminal_render::set_line_height(line_height);
        let letter_spacing = if settings.letter_spacing.is_finite() {
            settings.letter_spacing.clamp(0.0, 6.0)
        } else {
            0.0
        };
        crate::terminal_render::set_letter_spacing(letter_spacing);
        let collapsed_groups: std::collections::HashSet<String> =
            settings.collapsed_groups.iter().cloned().collect();
        let accent_hex = settings.accent_hex.clone();
        let window_size = Size::new(1200.0, 780.0);
        let (cols, rows) = terminal_render::grid_for_viewport(
            terminal_area(window_size, theme::SIDEBAR_WIDTH, 0.0, 0.0).width,
            terminal_area(window_size, theme::SIDEBAR_WIDTH, 0.0, 0.0).height,
            font_size,
        );

        let mut app = App {
            db_path,
            hosts,
            host_search: String::new(),
            sessions: Vec::new(),
            active: 0,
            next_session_id: 1,
            sidebar_width: theme::SIDEBAR_WIDTH,
            sidebar_collapsed: false,
            sidebar_dragging: false,
            hovered_host: None,
            pending_host_delete: None,
            modifiers: iced::keyboard::Modifiers::default(),
            window_size,
            font_size,
            color_scheme,
            default_user: settings.default_user.clone(),
            default_port: settings.default_port.to_string(),
            sftp_sort: session::SortField::Name,
            sftp_sort_asc: true,
            sftp_sort_local: session::SortField::Name,
            sftp_sort_asc_local: true,
            sftp_menu: None,
            last_sftp_click: None,
            rail_collapsed: false,
            rail_width: ui::RAIL_WIDTH,
            rail_dragging: false,
            last_title_click: None,
            sftp_prompt: None,
            sftp_confirm: None,
            next_transfer_id: 1,
            history_open: false,
            history_width: 280.0,
            history_dragging: false,
            all_history,
            history_filter: String::new(),
            settings_open: false,
            ping_results: HashMap::new(),
            settings_panel: session::SettingsPanel::Terminal,
            server_alive_interval: "60".to_string(),
            on_disconnect: session::OnDisconnect::AutoReconnect,
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            status: "Ready. Pick a saved host or start a new session.".to_string(),
            smoke_status: None,
            smoke_sftp_pending: false,
            smoke_download_pending: false,
            smoke_open_menu: false,
            card_anim: Animation::new(false).quick(),
            sidebar_anim: Animation::new(true).quick(),
            history_anim: Animation::new(false).quick(),
            rail_anim: Animation::new(false).quick(),
            rail_shown: false,
            settings_anim: Animation::new(false).quick(),
            connecting_pulse: false,
            now: Instant::now(),
            terminal_search: None,
            terminal_search_idx: 0,
            vault_enabled: settings.vault_enabled,
            vault_master: None,
            vault_has_canary: false, // will be set below
            vault_pw: String::new(),
            vault_confirm: String::new(),
            vault_err: None,
            vault_last_use: Instant::now(),
            vault_last_check: Instant::now(),
            vault_setup_prompt: false,
            vault_busy: false,
            collapsed_groups,
            line_height,
            letter_spacing,
            cursor_shape,
            accent_hex,
            closing_tabs: HashMap::new(),
            ping_history: HashMap::new(),
            toasts: Vec::new(),
            next_toast_id: 1,
            expanded_history: std::collections::HashSet::new(),
            copy_flash_until: None,
            snippets,
            snippet_draft_abbr: String::new(),
            snippet_draft_expansion: String::new(),
            history_search_open: false,
            history_search_query: String::new(),
            history_search_idx: 0,
            reveal_password: false,
        };
        // Check whether a master-password canary already exists.
        app.vault_has_canary = WorkspaceStore::open(&app.db_path)
            .ok()
            .and_then(|s| s.get_master_canary().ok())
            .flatten()
            .is_some();
        // Always start with one blank session so the workspace is never empty.
        app.spawn_session(app.blank_config(), cols, rows);
        // Kick off the entrance animation for the first card.
        app.start_card_entrance();

        // Debug/QA hook: open the settings panel on launch for screenshot tests.
        if std::env::var_os("OPENTERM_SMOKE_OPEN_SETTINGS").is_some() {
            app.settings_open = true;
        }
        if std::env::var_os("OPENTERM_SMOKE_OPEN_HISTORY").is_some() {
            app.history_open = true;
        }
        if std::env::var_os("OPENTERM_SMOKE_OPEN_MENU").is_some() {
            // Re-applied after the first remote listing (the SFTP auto-open
            // clears menu state), so the screenshot shows a real context menu.
            app.smoke_open_menu = true;
        }

        // Optional smoke automation: auto-connect to a real server and record
        // lifecycle milestones for the verification scripts.
        if let Some((config, smoke_cfg)) = smoke::SmokeConfig::from_env() {
            app.smoke_status = smoke_cfg.status_path.clone();
            app.smoke_sftp_pending = std::env::var_os("OPENTERM_SMOKE_SFTP").is_some();
            app.smoke_download_pending = std::env::var_os("OPENTERM_SMOKE_SFTP_DOWNLOAD").is_some();
            smoke::record(&app.smoke_status, "loaded");
            if let Some(session) = app.active_session_mut() {
                session.config = config;
            }
            let task = crate::update::update(&mut app, Message::Connect);
            return (app, task);
        }

        // Fire an initial ping immediately so the sidebar shows latency on first open.
        let ping = crate::update::update(&mut app, Message::PingTick);
        (app, ping)
    }

    // --- session/config helpers ---

    fn blank_config(&self) -> SessionConfig {
        // Prefer the configured default user, else fall back to $USER.
        let user = if self.default_user.trim().is_empty() {
            std::env::var("USER").unwrap_or_default()
        } else {
            self.default_user.trim().to_string()
        };
        let mut config = SessionConfig::blank(user, default_key_path());
        let port = self.default_port.trim();
        if !port.is_empty() {
            config.port = port.to_string();
        }
        config
    }

    fn spawn_session(&mut self, config: SessionConfig, cols: u16, rows: u16) -> usize {
        let id = self.next_session_id;
        self.next_session_id += 1;
        self.sessions.push(Session::new(id, config, cols, rows));
        let index = self.sessions.len() - 1;
        self.active = index;
        index
    }

    fn spawn_local_session(&mut self, cols: u16, rows: u16) -> usize {
        let id = self.next_session_id;
        self.next_session_id += 1;
        let mut s = Session::new(id, self.blank_config(), cols, rows);
        s.kind = crate::session::SessionKind::Local;
        self.sessions.push(s);
        let index = self.sessions.len() - 1;
        self.active = index;
        index
    }

    fn active_session(&self) -> Option<&Session> {
        self.sessions.get(self.active)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    fn active_session_mut(&mut self) -> Option<&mut Session> {
        self.sessions.get_mut(self.active)
    }

    fn session_index_by_id(&self, id: u64) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    fn current_grid(&self) -> (u16, u16) {
        let area = terminal_area(
            self.window_size,
            self.effective_sidebar_width(),
            self.reserved_right(),
            self.reserved_top(),
        );
        terminal_render::grid_for_viewport(area.width, area.height, self.font_size)
    }

    /// Sidebar width actually painted: 0 when collapsed, the resizable width
    /// (plus its divider) otherwise. Used by `terminal_area` so the grid always
    /// matches the space left for the terminal.
    pub fn effective_sidebar_width(&self) -> f32 {
        if self.sidebar_collapsed {
            0.0
        } else {
            self.sidebar_width + theme::SIDEBAR_DIVIDER_WIDTH
        }
    }

    pub fn color_scheme(&self) -> crate::theme::ColorScheme { self.color_scheme }

    pub fn cursor_shape(&self) -> crate::theme::CursorShape { self.cursor_shape }

    pub fn letter_spacing(&self) -> f32 { self.letter_spacing }

    /// The latest animation-frame instant (drives interpolation in views).
    pub fn now(&self) -> Instant {
        self.now
    }

    pub fn sidebar_width_value(&self) -> f32 {
        self.sidebar_width
    }

    pub fn sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    pub fn sidebar_dragging(&self) -> bool {
        self.sidebar_dragging
    }

    pub fn hovered_host(&self) -> Option<usize> {
        self.hovered_host
    }

    /// The saved host pending a delete confirmation, if any: (index, name).
    pub fn pending_host_delete(&self) -> Option<(usize, &str)> {
        self.pending_host_delete
            .and_then(|i| self.hosts.get(i).map(|h| (i, h.name.as_str())))
    }

    pub fn modifiers(&self) -> iced::keyboard::Modifiers {
        self.modifiers
    }

    pub fn set_modifiers(&mut self, m: iced::keyboard::Modifiers) {
        self.modifiers = m;
    }

    pub fn rail_collapsed(&self) -> bool {
        self.rail_collapsed
    }

    pub fn rail_dragging(&self) -> bool {
        self.rail_dragging
    }

    pub fn rail_width_value(&self) -> f32 {
        self.rail_width
    }

    /// Whether the resource rail should be shown: the active session is
    /// connected and the user hasn't collapsed it.
    pub fn rail_visible(&self) -> bool {
        !self.rail_collapsed
            && self
                .active_session()
                .map(|s| s.phase == session::Phase::Connected && s.kind == session::SessionKind::Ssh)
                .unwrap_or(false)
    }

    /// Vertical space taken above the terminal by the Terminal|Files sub-tab
    /// bar, which is shown only while the active session is connected.
    fn reserved_top(&self) -> f32 {
        let connected = self
            .active_session()
            .map(|s| s.phase == session::Phase::Connected)
            .unwrap_or(false);
        if connected {
            ui::SUBTAB_HEIGHT
        } else {
            0.0
        }
    }

    /// Horizontal space taken by panels on the right (command-history panel +
    /// its divider, and the resource rail) so the terminal grid is sized to
    /// what's actually left.
    fn reserved_right(&self) -> f32 {
        let history = if self.history_open {
            self.history_width + ui::HISTORY_DIVIDER_WIDTH
        } else {
            0.0
        };
        // Use the animated width so the terminal grid grows/shrinks smoothly as
        // the rail slides, rather than snapping when it appears.
        let rail = if self.rail_rendered() {
            self.rail_visual_width()
        } else {
            0.0
        };
        history + rail
    }

    /// Current width of the command-history panel (for the view).
    pub fn history_width_value(&self) -> f32 {
        self.history_width
    }

    pub fn sftp_prompt_active(&self) -> bool {
        self.sftp_prompt.is_some()
    }

    pub fn sftp_confirm(&self) -> Option<&session::SftpConfirm> {
        self.sftp_confirm.as_ref()
    }

    /// (Re)start the connection-card entrance animation from hidden to shown.
    fn start_card_entrance(&mut self) {
        let now = Instant::now();
        self.now = now;
        // Reset to false instantly, then animate to true so it always plays.
        self.card_anim = Animation::new(false).quick();
        self.card_anim.go_mut(true, now);
    }

    /// Entrance progress in 0.0..=1.0 for the current frame.
    pub fn card_progress(&self) -> f32 {
        self.card_anim.interpolate(0.0_f32, 1.0_f32, self.now)
    }

    /// Whether the card animation still needs frames (drives the subscription).
    pub fn card_animating(&self) -> bool {
        self.card_anim.is_animating(self.now)
    }

    /// Visually-rendered sidebar width (0 → full during collapse/expand).
    pub fn sidebar_visual_width(&self) -> f32 {
        self.sidebar_anim.interpolate(0.0_f32, self.sidebar_width, self.now)
    }

    /// True while the sidebar slide is in progress (frames still needed).
    pub fn sidebar_animating(&self) -> bool {
        self.sidebar_anim.is_animating(self.now)
    }

    /// Visually-rendered history panel width (0 → full during open/close).
    pub fn history_visual_width(&self) -> f32 {
        self.history_anim.interpolate(0.0_f32, self.history_width, self.now)
    }

    /// True while the history slide is in progress.
    pub fn history_animating(&self) -> bool {
        self.history_anim.is_animating(self.now)
    }

    /// Visually-rendered rail width (0 → full during open/close). The rail's
    /// fixed content width stays constant; the container clips it as this
    /// shrinks, so the rail appears to slide in/out from the right.
    pub fn rail_visual_width(&self) -> f32 {
        self.rail_anim.interpolate(0.0_f32, self.rail_width, self.now)
    }

    /// True while the rail slide is in progress (frames still needed).
    pub fn rail_animating(&self) -> bool {
        self.rail_anim.is_animating(self.now)
    }

    /// Whether the rail should be painted at all this frame: either logically
    /// visible, or still animating out.
    pub fn rail_rendered(&self) -> bool {
        self.rail_visible() || self.rail_animating()
    }

    /// Re-point the rail animation at the current logical visibility. Called
    /// after any state change that can flip `rail_visible()` (connect, close,
    /// tab switch, monitor toggle). A no-op when the target is unchanged, so it
    /// never restarts an in-flight slide.
    pub fn sync_rail(&mut self) {
        let target = self.rail_visible();
        if target != self.rail_shown {
            self.rail_shown = target;
            let now = std::time::Instant::now();
            self.now = now;
            self.rail_anim.go_mut(target, now);
        }
    }

    /// Settings overlay entrance progress 0.0→1.0.
    pub fn settings_progress(&self) -> f32 {
        self.settings_anim.interpolate(0.0_f32, 1.0_f32, self.now)
    }

    /// True while any animation needs frames.
    pub fn any_animating(&self) -> bool {
        self.card_anim.is_animating(self.now)
            || self.sidebar_anim.is_animating(self.now)
            || self.history_anim.is_animating(self.now)
            || self.rail_anim.is_animating(self.now)
            || self.settings_anim.is_animating(self.now)
            || self.closing_tabs.values().any(|a| a.is_animating(self.now))
            || !self.toasts.is_empty()
            || self.copy_flash_until.is_some_and(|t| self.now < t)
    }

    /// Copy-flash intensity in 0.0..=1.0 (1.0 right after copy, fading to 0).
    pub fn copy_flash(&self) -> f32 {
        const DUR: f32 = 0.45;
        match self.copy_flash_until {
            Some(until) if self.now < until => {
                let remaining = until.duration_since(self.now).as_secs_f32();
                (remaining / DUR).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    // --- vault: encrypt/decrypt saved passwords ---

    /// Key bytes used for encrypting/decrypting saved secrets: the master
    /// password when the vault is enabled, else the built-in default key.
    pub fn vault_key_bytes(&self) -> &[u8] {
        if self.vault_enabled {
            self.vault_master.as_deref().unwrap_or("").as_bytes()
        } else {
            VAULT_DEFAULT_KEY
        }
    }

    /// True when the vault is enabled but no master password is in memory, so
    /// saved secrets can't be decrypted until the user unlocks.
    pub fn vault_locked(&self) -> bool {
        self.vault_enabled && self.vault_master.is_none()
    }

    /// Whether the blocking vault overlay should be shown: the vault is enabled
    /// and locked (must unlock), or the create-master-password dialog is up.
    pub fn vault_overlay_active(&self) -> bool {
        (self.vault_enabled && self.vault_has_canary && self.vault_master.is_none())
            || self.vault_setup_prompt
    }

    /// Reset the 15-minute inactivity timer.
    pub fn touch_vault(&mut self) {
        self.vault_last_use = Instant::now();
    }

    /// Queue a toast notification (caps the visible stack at 4, dropping oldest).
    pub fn push_toast(&mut self, kind: crate::ui::toasts::ToastKind, msg: impl Into<String>) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts
            .push(crate::ui::toasts::Toast::new(id, kind, msg));
        // Keep the stack bounded: drop the oldest non-dismissed extras.
        while self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn vault(&self) -> LocalVault {
        LocalVault::new(VaultConfig::default())
    }

    /// Look up a snippet expansion by its exact abbreviation.
    pub fn snippet_for(&self, abbr: &str) -> Option<&str> {
        self.snippets
            .iter()
            .find(|s| s.abbr == abbr)
            .map(|s| s.expansion.as_str())
    }

    fn decrypt_secret(&self, id: openterm_core::SecretId) -> Option<String> {
        if self.vault_locked() { return None; }
        let store = WorkspaceStore::open(&self.db_path).ok()?;
        let secret = store.get_secret(id).ok().flatten()?;
        let bytes = self
            .vault()
            .decrypt_secret(self.vault_key_bytes(), &secret)
            .ok()?;
        String::from_utf8(bytes).ok()
    }

    /// Build a `SessionConfig` from a saved host, decrypting its password.
    fn config_from_host(&self, host: &HostProfile) -> SessionConfig {
        let mut config = self.blank_config();
        config.host_id = Some(host.id);
        config.name = host.name.clone();
        config.host = host.host.clone();
        config.user = host.username.clone().unwrap_or_default();
        config.port = host.port.to_string();
        config.group = host.group.clone().unwrap_or_default();
        config.tags_str = host.tags.join(", ");
        match &host.auth {
            AuthRef::PasswordSecret(secret_id) => {
                config.auth = AuthMode::Password;
                if let Some(password) = self.decrypt_secret(*secret_id) {
                    config.password = password;
                }
            }
            AuthRef::PrivateKeyFile { path, passphrase } => {
                config.auth = AuthMode::Key;
                config.key_path = path.clone();
                if let Some(secret_id) = passphrase {
                    if let Some(phrase) = self.decrypt_secret(*secret_id) {
                        config.passphrase = phrase;
                    }
                }
            }
            AuthRef::AgentOrDefault | AuthRef::ManagedPrivateKey(_) => {
                config.auth = AuthMode::Agent;
            }
        }
        config
    }

    /// Translate a session config into a connect route, validating inputs.
    fn build_route(config: &SessionConfig) -> Result<ConnectRoute, String> {
        let host = config.host.trim();
        if host.is_empty() {
            return Err("Host is required.".to_string());
        }
        let user = config.user.trim();
        if user.is_empty() {
            return Err("Username is required.".to_string());
        }
        let port: u16 = config
            .port
            .trim()
            .parse()
            .map_err(|_| "Port must be a number between 1 and 65535.".to_string())?;
        if port == 0 {
            return Err("Port must be greater than zero.".to_string());
        }

        let auth = match config.auth {
            AuthMode::Agent => AuthMethod::AgentOrDefault,
            AuthMode::Password => {
                if config.password.is_empty() {
                    return Err("Password is required.".to_string());
                }
                AuthMethod::Password(config.password.clone())
            }
            AuthMode::Key => {
                let path = config.key_path.trim();
                if path.is_empty() {
                    return Err("Key path is required.".to_string());
                }
                let passphrase = if config.passphrase.is_empty() {
                    None
                } else {
                    Some(config.passphrase.clone())
                };
                AuthMethod::PrivateKey {
                    path: PathBuf::from(path),
                    passphrase,
                }
            }
        };

        let mut profile = HostProfile::new(
            if config.name.trim().is_empty() {
                host.to_string()
            } else {
                config.name.trim().to_string()
            },
            host.to_string(),
        );
        profile.port = port;
        profile.username = Some(user.to_string());

        let options = ConnectOptions {
            username: user.to_string(),
            auth,
            trust_unknown_host_keys: false,
            host_key_policy: HostKeyPolicy::ConfirmNew {
                known_hosts: default_known_hosts_path(),
            },
            timeout: CONNECT_TIMEOUT,
        };

        let jump = parse_jump_host(config.jump_host.trim(), user)?;

        Ok(ConnectRoute {
            target: profile,
            target_options: options,
            jump,
        })
    }

    // update / subscription / view live in the modules below.
    fn update(&mut self, message: Message) -> Task<Message> {
        let task = crate::update::update(self, message);
        // Keep the rail slide animation pointed at the current logical
        // visibility after every message (connect/close/tab-switch/toggle can
        // all flip it). No-op when nothing changed.
        self.sync_rail();
        task
    }

    fn subscription(&self) -> Subscription<Message> {
        crate::subscription::subscription(self)
    }

    fn view(&self) -> Element<'_, Message> {
        crate::ui::view(self)
    }

    /// Persist the current UI settings (font size, theme).
    fn persist_settings(&self) {
        if let Ok(store) = WorkspaceStore::open(&self.db_path) {
            let _ = store.save_ui_settings(&UiSettings {
                theme_mode: "dark".to_string(),
                terminal_font_size: self.font_size,
                default_user: self.default_user.trim().to_string(),
                default_port: self.default_port.trim().parse().unwrap_or(22),
                color_scheme: self.color_scheme.to_str().to_string(),
                vault_enabled: self.vault_enabled,
                collapsed_groups: self.collapsed_groups.iter().cloned().collect(),
                line_height: self.line_height,
                letter_spacing: self.letter_spacing,
                cursor_shape: self.cursor_shape.to_str().to_string(),
                accent_hex: self.accent_hex.clone(),
            });
        }
    }

    /// Update a saved host's last-connected timestamp.
    fn touch_host(&mut self, host_id: HostId) {
        let Ok(store) = WorkspaceStore::open(&self.db_path) else {
            return;
        };
        if let Ok(Some(mut host)) = store.get_host(host_id) {
            host.last_connected_at = Some(current_timestamp());
            let _ = store.save_host(&host);
            self.hosts = store.list_hosts().unwrap_or_default();
        }
    }
}

/// Save (or update) a host from a session config, encrypting any password or
/// passphrase into the local vault. Returns the host id.
fn persist_host(app: &App, config: &SessionConfig) -> Result<HostId, String> {
    // Validate by building a route first (re-uses the same checks).
    App::build_route(config).map_err(|e| e)?;

    let store = WorkspaceStore::open(&app.db_path).map_err(|e| e.to_string())?;
    let vault = app.vault();

    // Reuse the existing host id when editing, so we update in place.
    let mut profile = if let Some(id) = config.host_id {
        store
            .get_host(id)
            .ok()
            .flatten()
            .unwrap_or_else(|| HostProfile::new(config.name.clone(), config.host.clone()))
    } else {
        HostProfile::new(config.name.clone(), config.host.clone())
    };

    profile.name = if config.name.trim().is_empty() {
        config.host.trim().to_string()
    } else {
        config.name.trim().to_string()
    };
    profile.host = config.host.trim().to_string();
    profile.port = config.port.trim().parse().unwrap_or(22);
    profile.username = Some(config.user.trim().to_string());
    profile.group = if config.group.trim().is_empty() { None } else { Some(config.group.trim().to_string()) };
    profile.tags = config.tags_str.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();

    profile.auth = match config.auth {
        AuthMode::Agent => AuthRef::AgentOrDefault,
        AuthMode::Password => {
            let secret = vault
                .encrypt_secret(app.vault_key_bytes(), config.password.as_bytes())
                .map_err(|e| e.to_string())?;
            let id = secret.id;
            store.save_secret(&secret).map_err(|e| e.to_string())?;
            AuthRef::PasswordSecret(id)
        }
        AuthMode::Key => {
            let passphrase = if config.passphrase.is_empty() {
                None
            } else {
                let secret = vault
                    .encrypt_secret(app.vault_key_bytes(), config.passphrase.as_bytes())
                    .map_err(|e| e.to_string())?;
                let id = secret.id;
                store.save_secret(&secret).map_err(|e| e.to_string())?;
                Some(id)
            };
            AuthRef::PrivateKeyFile {
                path: config.key_path.trim().to_string(),
                passphrase,
            }
        }
    };

    store.save_host(&profile).map_err(|e| e.to_string())?;
    let id = profile.id;
    // No mutation of app.hosts here (called with &App); caller refreshes.
    Ok(id)
}

mod subscription;
mod update;

/// The pixel area available to the terminal canvas for a given window size.
/// Mirrors the layout in `ui`: sidebar on the left, tab bar + a connection
/// header band on top, footer at the bottom, with fixed paddings. Because the
/// grid is derived from this exact area, the rendered grid never overflows.
fn terminal_area(window: Size, sidebar_width: f32, reserved_right: f32, reserved_top: f32) -> Size {
    let width =
        (window.width - sidebar_width - reserved_right - ui::WORKSPACE_H_PADDING * 2.0).max(120.0);
    let height = (window.height
        - theme::TAB_BAR_HEIGHT
        - ui::FOOTER_HEIGHT
        - reserved_top
        - ui::TERMINAL_V_PADDING * 2.0)
        .max(80.0);
    Size::new(width, height)
}

fn default_key_path() -> String {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("id_ed25519")
        .display()
        .to_string()
}

fn default_known_hosts_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("known_hosts")
}

/// Parse a jump-host string of the form `[user@]host[:port]` into a
/// `(HostProfile, ConnectOptions)` pair ready for `ConnectRoute::jump`.
/// Returns `Ok(None)` when the string is empty (no jump host configured).
fn parse_jump_host(
    s: &str,
    default_user: &str,
) -> Result<Option<(HostProfile, ConnectOptions)>, String> {
    if s.is_empty() {
        return Ok(None);
    }
    // Split user@rest
    let (user, rest) = match s.split_once('@') {
        Some((u, r)) => (u.to_string(), r),
        None => (default_user.to_string(), s),
    };
    // Split host:port (use rsplit to handle IPv6 like [::1]:22)
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| "Jump host port must be a number between 1 and 65535.".to_string())?;
            (h.to_string(), port)
        }
        None => (rest.to_string(), 22u16),
    };
    if host.is_empty() {
        return Err("Jump host address is required.".to_string());
    }
    if user.is_empty() {
        return Err("Jump host username is required.".to_string());
    }
    let mut profile = HostProfile::new(host.clone(), host);
    profile.port = port;
    profile.username = Some(user.clone());
    let options = ConnectOptions {
        username: user,
        auth: AuthMethod::AgentOrDefault,
        trust_unknown_host_keys: false,
        host_key_policy: HostKeyPolicy::ConfirmNew {
            known_hosts: default_known_hosts_path(),
        },
        timeout: CONNECT_TIMEOUT,
    };
    Ok(Some((profile, options)))
}

fn current_timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("unix:{seconds}")
}
