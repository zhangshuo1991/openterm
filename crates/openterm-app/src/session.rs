//! The session model: one self-contained terminal session per tab.
//!
//! This is the structural fix for the old "god struct". Each [`Session`] owns
//! *its own* terminal grid, connection phase, and command channel. Switching
//! tabs is just changing `App::active` — no state is copied between a shared
//! global buffer and per-tab storage, so a background session keeps rendering
//! into its own grid and is intact when you switch back.

use openterm_core::HostId;
use openterm_ssh::HostKeyChallenge;
use openterm_terminal::{AlacrittyTerminalBuffer, TerminalEngine, TerminalSize};
use tokio::sync::mpsc;

use crate::connection::{Command, ConnectParams};

/// How the user authenticates a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// SSH agent or default key in `~/.ssh`.
    Agent,
    /// Username + password.
    Password,
    /// Explicit private key file (+ optional passphrase).
    Key,
}

impl AuthMode {
    #[allow(dead_code)]
    pub const ALL: [AuthMode; 3] = [AuthMode::Agent, AuthMode::Password, AuthMode::Key];

    pub fn label(self) -> &'static str {
        match self {
            AuthMode::Agent => "SSH agent / default key",
            AuthMode::Password => "Password",
            AuthMode::Key => "Private key file",
        }
    }
}

impl std::fmt::Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Which settings panel is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPanel {
    Terminal,
    Ssh,
    Keys,
    Appearance,
    Snippets,
    Advanced,
}

/// What to do when a connection drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDisconnect {
    Alert,
    AutoReconnect,
    CloseTab,
}

/// Editable connection settings for a session (also used by the host editor).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The saved host this session was launched from, if any.
    pub host_id: Option<HostId>,
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: String,
    pub auth: AuthMode,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,
    /// Group / folder the host belongs to (e.g. "Production").
    pub group: String,
    /// Comma-separated list of tags (e.g. "nginx, k8s").
    pub tags_str: String,
    /// Whether the jump-host (bastion) section is expanded.
    pub show_jump: bool,
    /// Bastion host to proxy through (UI-only, not wired to SSH yet).
    pub jump_host: String,
}

impl SessionConfig {
    pub fn blank(default_user: String, default_key_path: String) -> Self {
        Self {
            host_id: None,
            name: String::new(),
            host: String::new(),
            user: default_user,
            port: "22".to_string(),
            auth: AuthMode::Agent,
            password: String::new(),
            key_path: default_key_path,
            passphrase: String::new(),
            group: String::new(),
            tags_str: String::new(),
            show_jump: false,
            jump_host: String::new(),
        }
    }

    pub fn target_label(&self) -> String {
        let host = self.host.trim();
        let user = self.user.trim();
        if host.is_empty() {
            return "New session".to_string();
        }
        if user.is_empty() {
            host.to_string()
        } else {
            format!("{user}@{host}")
        }
    }
}

/// The connection lifecycle phase of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Never connected, or fully disconnected.
    Idle,
    /// Connection in progress.
    Connecting,
    /// Shell is live.
    Connected,
    /// Last attempt failed; message explains why.
    Failed(String),
}

impl Phase {
    pub fn is_active(&self) -> bool {
        matches!(self, Phase::Connecting | Phase::Connected)
    }
}

/// Whether this session is an SSH connection or a local shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Ssh,
    Local,
}

/// One terminal session = one tab.
pub struct Session {
    pub id: u64,
    pub kind: SessionKind,
    pub config: SessionConfig,
    /// This session's own terminal grid. Never shared.
    pub terminal: AlacrittyTerminalBuffer,
    pub grid_cols: u16,
    pub grid_rows: u16,
    pub phase: Phase,
    /// Command channel to this session's connection worker (set on `Ready`).
    pub cmd_tx: Option<mpsc::Sender<Command>>,
    /// Connect params requested before the worker channel was ready.
    pub pending_connect: Option<ConnectParams>,
    /// A host key awaiting user confirmation.
    pub host_key: Option<HostKeyChallenge>,
    /// Short status line shown in the tab/footer for this session.
    pub status: String,
    // --- SFTP workspace state (Phase 2) ---
    pub sftp_open: bool,
    /// Which metric's process drill-down is shown (None = the rail's process
    /// expander is collapsed).
    pub monitor_panel: Option<MonitorPanel>,
    /// Latest process sample for the drill-down table.
    pub processes: Vec<crate::metrics::ProcessInfo>,
    /// Listening ports sampled from `ss`.
    pub ports: Vec<crate::metrics::PortInfo>,
    /// Recent history for the rail's line charts (cap 60). CPU%/Memory% are
    /// 0..100; network and disk-IO are bytes/s (auto-scaled when charted).
    pub cpu_history: std::collections::VecDeque<f32>,
    pub mem_history: std::collections::VecDeque<f32>,
    pub net_history: std::collections::VecDeque<f32>,
    pub diskio_history: std::collections::VecDeque<f32>,
    /// Latest computed metrics (display-ready) for the monitor view.
    pub metrics: Option<crate::metrics::SessionMetrics>,
    /// Previous raw sample + when it arrived, for rate (CPU/IO) deltas.
    pub prev_sample: Option<(std::time::Instant, crate::metrics::RawSample)>,
    pub remote_path: String,
    pub remote_files: Vec<openterm_ssh::RemoteFileEntry>,
    /// Selected remote rows (multi-select). Empty = nothing selected.
    pub selected_remote: std::collections::BTreeSet<usize>,
    /// Anchor row for shift-range selection in the remote pane.
    pub remote_anchor: Option<usize>,
    pub sftp_status: String,
    /// Local side of the dual-pane file manager.
    pub local_path: String,
    pub local_files: Vec<LocalEntry>,
    /// Selected local rows (multi-select). Empty = nothing selected.
    pub selected_local: std::collections::BTreeSet<usize>,
    /// Anchor row for shift-range selection in the local pane.
    pub local_anchor: Option<usize>,
    /// Transfers for this session (active first, then completed history).
    pub transfers: Vec<Transfer>,
    /// Commands the user has typed this session (most recent last).
    pub command_history: Vec<String>,
    /// Bytes of the line currently being typed (for command capture).
    input_buf: Vec<u8>,
    /// Whether we're skipping an ANSI escape sequence during capture.
    input_escape: bool,
    /// Enter was pressed but we haven't seen the shell's response yet;
    /// commit command history on the next `write_output` call (after tab
    /// completion bytes are guaranteed to be in the terminal).
    enter_pending: bool,
    /// Accumulates output bytes after command commit for Terminal Memory.
    output_buf: Vec<u8>,
    /// Whether output capture is active (between command commit and next Enter).
    capturing_output: bool,
    /// Snapshot of the previous command's output (ANSI-stripped, capped),
    /// finalized when the next command arrives.
    pub committed_output: String,
    /// Whether any remote output has been written (keeps the terminal visible
    /// after disconnect so the final screen stays readable).
    pub has_output: bool,
    /// Active terminal text selection (col1, row1, col2, row2). None = no selection.
    pub selection: Option<(usize, usize, usize, usize)>,
    /// Pending chmod operation (modal open).
    pub sftp_chmod: Option<ChmodState>,
    /// Error from the last local `read_dir` (e.g. macOS TCC permission denied).
    pub local_error: Option<String>,
    /// File viewer panel (shown in the SFTP workspace right column).
    pub file_viewer: Option<FileViewerState>,
    /// When the session most recently reached `Phase::Connected`, for the
    /// footer's live connection-duration clock. Cleared on disconnect.
    pub connected_at: Option<std::time::Instant>,
    /// Sprint 3: inline ghost-text suggestion (the *suffix* after the current
    /// input line) computed from history. Drawn at the cursor at 40% alpha;
    /// Right/Tab accepts it. Never injected into the PTY until accepted.
    pub inline_suggestion: Option<String>,
}

/// A local filesystem entry shown in the SFTP local pane.
#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub name: String,
    pub path: std::path::PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
}

/// How file lists are ordered in the SFTP panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Name,
    Size,
    Modified,
}

/// Which SFTP pane an action targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpSide {
    Local,
    Remote,
}

/// Which monitor metric's process drill-down is being shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorPanel {
    Cpu,
    Memory,
}

impl MonitorPanel {
    pub fn sort(self) -> crate::metrics::ProcessSort {
        match self {
            MonitorPanel::Cpu => crate::metrics::ProcessSort::Cpu,
            MonitorPanel::Memory => crate::metrics::ProcessSort::Memory,
        }
    }
}

/// Pending chmod operation: path being changed + current octal string.
#[derive(Debug, Clone)]
pub struct ChmodState {
    pub path: String,
    pub current_mode: u32,
    pub input: String, // e.g. "755"
}

/// A small modal prompt for entering a name (new folder / rename).
#[derive(Debug, Clone)]
pub struct SftpPrompt {
    pub side: SftpSide,
    pub kind: SftpPromptKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SftpPromptKind {
    /// Create a new directory in `side`'s current path.
    NewFolder,
    /// Rename the entry at `index` (its old name shown as the default).
    Rename { index: usize, old: String },
}

/// Viewing mode for the file viewer panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerMode {
    /// Read-only with syntax highlighting.
    Preview,
    /// Editable text; save writes back to the server.
    Edit,
    /// Large file / log: paged view with search only (no edit).
    Log,
}

/// Content state for the file viewer.
#[derive(Debug, Clone)]
pub enum ViewerContent {
    Loading,
    /// Full text loaded (small files or edit mode).
    Loaded(String),
    /// Streamed view: chunks received so far, total file size, and the current page offset.
    Streaming { text: String, total: u64, page_offset: u64 },
    Error(String),
}

/// The right-column file viewer panel in the SFTP workspace.
/// The right-column file viewer panel in the SFTP workspace.
pub struct FileViewerState {
    pub path: String,
    pub mode: ViewerMode,
    pub content: ViewerContent,
    /// Multi-line editor content — used only in Edit mode.
    pub editor: iced::widget::text_editor::Content,
    pub scroll: f32,
    /// Language/syntax name (empty = plain text).
    pub lang: String,
    /// Pre-computed syntax-highlighted spans; invalidated whenever content changes.
    pub highlight_cache: Vec<(iced::Color, String)>,
    pub search: String,
    pub replace: String,
    /// Byte offsets of search matches in the current text.
    pub matches: Vec<usize>,
    pub match_idx: usize,
    pub dirty: bool,
    pub saving: bool,
}

impl FileViewerState {
    pub const PAGE_SIZE: u64 = 64 * 1024; // 64 KB per page
    pub const SMALL_FILE_LIMIT: u64 = 256 * 1024; // below this: load all

    pub fn new_loading(path: String) -> Self {
        let lang = crate::highlight::lang_from_ext(&path).to_owned();
        Self {
            path,
            mode: ViewerMode::Preview,
            content: ViewerContent::Loading,
            editor: iced::widget::text_editor::Content::new(),
            scroll: 0.0,
            lang,
            highlight_cache: Vec::new(),
            search: String::new(),
            replace: String::new(),
            matches: Vec::new(),
            match_idx: 0,
            dirty: false,
            saving: false,
        }
    }

    /// Current displayable text (if any).
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            ViewerContent::Loaded(s) => Some(s.as_str()),
            ViewerContent::Streaming { text, .. } => Some(text.as_str()),
            _ => None,
        }
    }

    /// Recompute match offsets for the current search query.
    pub fn refresh_matches(&mut self) {
        self.matches.clear();
        self.match_idx = 0;
        let q = self.search.to_ascii_lowercase();
        if q.is_empty() { return; }
        let text = match &self.content {
            ViewerContent::Loaded(s) => s.clone(),
            ViewerContent::Streaming { text, .. } => text.clone(),
            _ => return,
        };
        let lower = text.to_ascii_lowercase();
        let mut start = 0;
        while let Some(pos) = lower[start..].find(&q) {
            self.matches.push(start + pos);
            start += pos + q.len().max(1);
        }
    }
}

/// A pending delete awaiting the user's confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpConfirm {
    pub side: SftpSide,
    /// What to show: a single name, or "N items" for a multi-select.
    pub label: String,
    /// Number of items the delete will affect.
    pub count: usize,
    /// True when at least one target is a directory (stronger warning).
    pub any_dir: bool,
}

/// A file transfer (in progress or completed), shown in the transfers panel.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: u64,
    pub name: String,
    pub direction: crate::connection::Direction,
    pub total: u64,
    pub transferred: u64,
    pub speed_bps: f64,
    pub status: TransferStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    Active,
    Done,
    Failed(String),
}

impl Session {
    pub fn new(id: u64, config: SessionConfig, cols: u16, rows: u16) -> Self {
        Self {
            id,
            kind: SessionKind::Ssh,
            config,
            terminal: AlacrittyTerminalBuffer::new(TerminalSize { cols, rows }),
            grid_cols: cols,
            grid_rows: rows,
            phase: Phase::Idle,
            cmd_tx: None,
            pending_connect: None,
            host_key: None,
            status: "Not connected".to_string(),
            sftp_open: false,
            monitor_panel: None,
            processes: Vec::new(),
            ports: Vec::new(),
            cpu_history: std::collections::VecDeque::new(),
            mem_history: std::collections::VecDeque::new(),
            net_history: std::collections::VecDeque::new(),
            diskio_history: std::collections::VecDeque::new(),
            metrics: None,
            prev_sample: None,
            remote_path: ".".to_string(),
            remote_files: Vec::new(),
            selected_remote: std::collections::BTreeSet::new(),
            remote_anchor: None,
            sftp_status: String::new(),
            local_path: default_local_dir(),
            local_files: Vec::new(),
            selected_local: std::collections::BTreeSet::new(),
            local_anchor: None,
            transfers: Vec::new(),
            command_history: Vec::new(),
            input_buf: Vec::new(),
            input_escape: false,
            enter_pending: false,
            output_buf: Vec::new(),
            capturing_output: false,
            committed_output: String::new(),
            has_output: false,
            selection: None,
            sftp_chmod: None,
            local_error: None,
            file_viewer: None,
            connected_at: None,
            inline_suggestion: None,
        }
    }

    /// Tab title: prefer the saved name, then user@host, then a generic label.
    pub fn title(&self) -> String {
        if self.kind == SessionKind::Local {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            let name = shell.rsplit('/').next().unwrap_or("shell");
            return format!("Local — {name}");
        }
        let name = self.config.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        let label = self.config.target_label();
        if label == "New session" {
            format!("Session {}", self.id)
        } else {
            label
        }
    }

    /// Resize this session's grid (and request a PTY resize if connected).
    pub fn resize_grid(&mut self, cols: u16, rows: u16) {
        if cols == self.grid_cols && rows == self.grid_rows {
            return;
        }
        self.grid_cols = cols;
        self.grid_rows = rows;
        self.terminal.resize(TerminalSize { cols, rows });
    }

    /// Feed remote bytes into this session's own grid.
    pub fn write_output(&mut self, bytes: &[u8]) {
        // Commit deferred history entry before writing new output.
        // At this point all tab-completion bytes are already in the terminal
        // (SSH stream is ordered), so cursor_line_text() sees the full command.
        if self.enter_pending {
            // Snapshot previous command's output and start a fresh capture.
            self.committed_output = strip_ansi_truncated(&self.output_buf, 5_000);
            self.output_buf.clear();
            self.capturing_output = true;

            let tline = self.terminal.cursor_line_text();
            self.commit_input_line(Some(&tline));
            self.enter_pending = false;
        } else if self.capturing_output {
            self.output_buf.extend_from_slice(bytes);
            // Hard cap: stop accumulating after 5 KB to bound memory usage.
            if self.output_buf.len() >= 5_000 {
                self.capturing_output = false;
            }
        }
        let _ = self.terminal.write_remote_output(bytes);
        self.has_output = true;
    }

    /// Observe bytes the user is sending to the shell to reconstruct typed
    /// commands. This is a best-effort line tracker: it ignores ANSI escape
    /// sequences (arrows, etc.), handles backspace and line-kill, and records a
    /// command when Enter is pressed. It won't perfectly mirror shell history
    /// recall or tab-completion, but gives a useful "commands you ran" list.
    pub fn track_input(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.input_escape {
                // CSI/escape sequences end on a letter or '~'.
                if b.is_ascii_alphabetic() || b == b'~' {
                    self.input_escape = false;
                }
                continue;
            }
            match b {
                0x1b => self.input_escape = true, // ESC
                b'\r' | b'\n' => {
                    // Don't commit yet: tab-completion bytes from the remote
                    // shell may still be in flight. Defer to write_output so
                    // the terminal is fully updated before we read it.
                    // input_buf is kept as fallback (cleared by commit_input_line).
                    self.enter_pending = true;
                }
                0x7f | 0x08 => {
                    // Backspace: drop the last full UTF-8 char.
                    while self.input_buf.pop().is_some_and(|c| (c & 0xc0) == 0x80) {}
                }
                0x03 | 0x15 | 0x0c => self.input_buf.clear(), // Ctrl-C / Ctrl-U / Ctrl-L
                b'\t' => {}                                   // ignore tab-completion triggers
                _ if b >= 0x20 => self.input_buf.push(b),
                _ => {}
            }
        }
    }

    /// The line the user is currently typing, reconstructed from tracked input
    /// bytes (best-effort; see `track_input`). Used to compute ghost-text
    /// suggestions and match snippet abbreviations.
    pub fn input_line(&self) -> String {
        String::from_utf8_lossy(&self.input_buf).into_owned()
    }

    /// Append already-accepted bytes (a ghost suggestion or snippet expansion)
    /// to the tracked input line, so the shadow stays in sync with what the
    /// remote shell now shows. ANSI control bytes are ignored.
    pub fn extend_input(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b >= 0x20 {
                self.input_buf.push(b);
            }
        }
    }

    /// Reset the tracked input line (used after a snippet erases its abbr).
    pub fn clear_input_line(&mut self) {
        self.input_buf.clear();
    }

    fn commit_input_line(&mut self, terminal_line: Option<&str>) {
        // strip_prompt is a module-level fn defined below.
        // Prefer the terminal's current line (post-tab-completion) over the raw
        // typed bytes. Fall back to input_buf if no recognisable prompt is found.
        let line = terminal_line
            .and_then(strip_prompt)
            .map(str::to_string)
            .or_else(|| {
                let s = String::from_utf8_lossy(&self.input_buf).trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            });
        self.input_buf.clear();
        let Some(line) = line else { return };
        // Skip consecutive duplicates.
        if self.command_history.last().map(String::as_str) == Some(line.as_str()) {
            return;
        }
        self.command_history.push(line);
        if self.command_history.len() > 200 {
            let overflow = self.command_history.len() - 200;
            self.command_history.drain(0..overflow);
        }
    }

    pub fn terminal_has_content(&self) -> bool {
        self.has_output
    }

    /// Reset the grid to a clean state (used on (re)connect).
    pub fn clear_grid(&mut self) {
        self.terminal = AlacrittyTerminalBuffer::new(TerminalSize {
            cols: self.grid_cols,
            rows: self.grid_rows,
        });
        self.has_output = false;
    }

    /// Re-read the local directory into `local_files`, applying `sort`.
    pub fn refresh_local(&mut self, sort: SortField, ascending: bool) {
        let mut entries = Vec::new();
        match std::fs::read_dir(&self.local_path) {
            Ok(read) => {
                for entry in read.flatten() {
                    let path = entry.path();
                    let meta = entry.metadata().ok();
                    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    entries.push(LocalEntry {
                        name: entry.file_name().to_string_lossy().to_string(),
                        path,
                        is_dir,
                        size,
                        modified,
                    });
                }
                self.local_error = None;
            }
            Err(e) => {
                self.local_error = Some(format!("Cannot read directory: {e}"));
            }
        }
        sort_local(&mut entries, sort, ascending);
        self.local_files = entries;
        self.selected_local.clear();
        self.local_anchor = None;
    }

    /// Apply a click on row `index` in `side`, honoring multi-select modifiers:
    /// plain = select only this; toggle (Cmd/Ctrl) = add/remove; range (Shift) =
    /// select from the anchor to here. Returns nothing; mutates the set + anchor.
    pub fn select_click(&mut self, side: SftpSide, index: usize, toggle: bool, range: bool) {
        let (set, anchor) = match side {
            SftpSide::Remote => (&mut self.selected_remote, &mut self.remote_anchor),
            SftpSide::Local => (&mut self.selected_local, &mut self.local_anchor),
        };
        if range {
            if let Some(a) = *anchor {
                set.clear();
                let (lo, hi) = if a <= index { (a, index) } else { (index, a) };
                set.extend(lo..=hi);
                return;
            }
            // No anchor yet: fall through to a plain select.
        }
        if toggle {
            if !set.remove(&index) {
                set.insert(index);
            }
            *anchor = Some(index);
        } else {
            set.clear();
            set.insert(index);
            *anchor = Some(index);
        }
    }

    /// Push the latest CPU% / Memory% / net-bps / disk-IO-bps into the rail's
    /// chart history (cap 60).
    pub fn push_metrics_history(&mut self, cpu: f32, mem: f32, net: f32, diskio: f32) {
        const CAP: usize = 60;
        for (buf, v) in [
            (&mut self.cpu_history, cpu),
            (&mut self.mem_history, mem),
            (&mut self.net_history, net),
            (&mut self.diskio_history, diskio),
        ] {
            buf.push_back(v);
            while buf.len() > CAP {
                buf.pop_front();
            }
        }
    }
}

/// Strip ANSI escape sequences from terminal output and truncate to max_bytes.
fn strip_ansi_truncated(bytes: &[u8], max_bytes: usize) -> String {
    let raw = String::from_utf8_lossy(bytes);
    let mut result = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ch.is_ascii_alphabetic() || ch == '~' {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
            if result.len() >= max_bytes {
                break;
            }
        }
    }
    result.trim_end().to_string()
}

/// Find the suffix to suggest after `line`: the first candidate (iterated
/// newest-first) that strictly extends `line` as a prefix. Returns `None` when
/// the line is too short, empty/whitespace, or nothing matches. The returned
/// string is only the part *after* what's already typed.
pub fn suggestion_suffix<'a>(
    line: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<String> {
    // Avoid noise: don't suggest until at least 2 non-space chars are typed.
    if line.trim().len() < 2 {
        return None;
    }
    for cand in candidates {
        if cand.len() > line.len() && cand.starts_with(line) {
            return Some(cand[line.len()..].to_string());
        }
    }
    None
}

/// Strip a shell prompt from `line` and return the command portion.
/// Finds the rightmost occurrence of `$ `, `# `, or `% ` and returns everything after it.
fn strip_prompt(line: &str) -> Option<&str> {
    ["$ ", "# ", "% "]
        .iter()
        .filter_map(|t| line.rfind(t).map(|pos| (pos, line[pos + t.len()..].trim())))
        .max_by_key(|(pos, _)| *pos)
        .map(|(_, cmd)| cmd)
        .filter(|s| !s.is_empty())
}

/// Order local entries: directories always first, then by the chosen field.
pub fn sort_local(entries: &mut [LocalEntry], sort: SortField, ascending: bool) {
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            let ord = match sort {
                SortField::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortField::Size => a.size.cmp(&b.size),
                SortField::Modified => a.modified.cmp(&b.modified),
            };
            if ascending {
                ord
            } else {
                ord.reverse()
            }
        })
    });
}

/// Order remote entries: directories first, then by the chosen field.
pub fn sort_remote(
    entries: &mut [openterm_ssh::RemoteFileEntry],
    sort: SortField,
    ascending: bool,
) {
    use openterm_ssh::RemoteFileKind;
    entries.sort_by(|a, b| {
        let a_dir = matches!(a.kind, RemoteFileKind::Directory);
        let b_dir = matches!(b.kind, RemoteFileKind::Directory);
        b_dir.cmp(&a_dir).then_with(|| {
            let ord = match sort {
                SortField::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortField::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
                SortField::Modified => a.modified.unwrap_or(0).cmp(&b.modified.unwrap_or(0)),
            };
            if ascending {
                ord
            } else {
                ord.reverse()
            }
        })
    });
}

/// The directory the local SFTP pane starts in (the user's home).
fn default_local_dir() -> String {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(1, SessionConfig::blank("u".into(), "k".into()), 80, 24)
    }

    #[test]
    fn captures_typed_commands_on_enter() {
        let mut s = session();
        s.track_input(b"ls -la\r");
        s.write_output(b"\r\n");
        s.track_input(b"whoami\n");
        s.write_output(b"\r\n");
        assert_eq!(s.command_history, vec!["ls -la", "whoami"]);
    }

    #[test]
    fn multi_select_click_modes() {
        let mut s = session();
        let side = SftpSide::Remote;
        // Plain click selects only that row.
        s.select_click(side, 2, false, false);
        assert_eq!(s.selected_remote.iter().copied().collect::<Vec<_>>(), [2]);
        // Cmd/Ctrl-click adds another.
        s.select_click(side, 5, true, false);
        assert_eq!(
            s.selected_remote.iter().copied().collect::<Vec<_>>(),
            [2, 5]
        );
        // Cmd/Ctrl-click again toggles it off (anchor moves to 5).
        s.select_click(side, 5, true, false);
        assert_eq!(s.selected_remote.iter().copied().collect::<Vec<_>>(), [2]);
        // Shift-click selects the range from the anchor (now 5) to 6.
        s.select_click(side, 6, false, true);
        assert_eq!(
            s.selected_remote.iter().copied().collect::<Vec<_>>(),
            [5, 6]
        );
        // Plain click collapses back to one.
        s.select_click(side, 0, false, false);
        assert_eq!(s.selected_remote.iter().copied().collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn shift_without_anchor_falls_back_to_single() {
        let mut s = session();
        s.select_click(SftpSide::Local, 4, false, true);
        assert_eq!(s.selected_local.iter().copied().collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn backspace_edits_the_line() {
        let mut s = session();
        s.track_input(b"echoo"); // trailing typo
        s.track_input(&[0x7f]); // erase the extra 'o'
        s.track_input(b" hi\r");
        s.write_output(b"\r\n");
        assert_eq!(s.command_history, vec!["echo hi"]);
    }

    #[test]
    fn ignores_arrow_escape_sequences() {
        let mut s = session();
        // Up-arrow (ESC [ A) in the middle should be skipped, not recorded.
        s.track_input(b"cd \x1b[A/tmp\r");
        s.write_output(b"\r\n");
        assert_eq!(s.command_history, vec!["cd /tmp"]);
    }

    #[test]
    fn ctrl_c_clears_the_line_without_recording() {
        let mut s = session();
        s.track_input(b"rm -rf /"); // oops
        s.track_input(&[0x03]); // Ctrl-C
        s.track_input(b"ls\r");
        s.write_output(b"\r\n");
        assert_eq!(s.command_history, vec!["ls"]);
    }

    #[test]
    fn skips_consecutive_duplicates() {
        let mut s = session();
        s.track_input(b"ls\r");
        s.write_output(b"\r\n");
        s.track_input(b"ls\r");
        s.write_output(b"\r\n");
        assert_eq!(s.command_history, vec!["ls"]);
    }

    #[test]
    fn suggestion_extends_prefix_newest_first() {
        // Candidates iterated newest-first: the first match that extends wins.
        let cands = ["git push origin main", "git pull", "git status"];
        assert_eq!(
            suggestion_suffix("git p", cands.iter().copied()),
            Some("ush origin main".to_string())
        );
    }

    #[test]
    fn suggestion_requires_two_chars() {
        let cands = ["ls -la"];
        assert_eq!(suggestion_suffix("l", cands.iter().copied()), None);
        assert_eq!(
            suggestion_suffix("ls", cands.iter().copied()),
            Some(" -la".to_string())
        );
    }

    #[test]
    fn suggestion_none_when_exact_or_no_match() {
        let cands = ["ls -la", "cd /tmp"];
        // Exact match (no remaining suffix) → no suggestion.
        assert_eq!(suggestion_suffix("ls -la", cands.iter().copied()), None);
        // No candidate starts with the line.
        assert_eq!(suggestion_suffix("xyz", cands.iter().copied()), None);
    }

    #[test]
    fn input_line_tracks_typing() {
        let mut s = session();
        s.track_input(b"git p");
        assert_eq!(s.input_line(), "git p");
        // Enter commits the line; the shadow resets for the next command.
        s.track_input(b"\r");
        s.write_output(b"\r\n");
        assert_eq!(s.input_line(), "");
    }
}
