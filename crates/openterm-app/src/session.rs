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
    /// Whether any remote output has been written (keeps the terminal visible
    /// after disconnect so the final screen stays readable).
    pub has_output: bool,
    /// Active terminal text selection (col1, row1, col2, row2). None = no selection.
    pub selection: Option<(usize, usize, usize, usize)>,
    /// Pending chmod operation (modal open).
    pub sftp_chmod: Option<ChmodState>,
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

/// A pending delete awaiting the user's confirmation. Captured when the user
/// picks "Delete" from the context menu; the actual delete runs only on confirm.
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
            has_output: false,
            selection: None,
            sftp_chmod: None,
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
                b'\r' | b'\n' => self.commit_input_line(),
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

    fn commit_input_line(&mut self) {
        if self.input_buf.is_empty() {
            return;
        }
        let line = String::from_utf8_lossy(&self.input_buf).trim().to_string();
        self.input_buf.clear();
        if line.is_empty() {
            return;
        }
        // Skip consecutive duplicates.
        if self.command_history.last().map(String::as_str) == Some(line.as_str()) {
            return;
        }
        self.command_history.push(line);
        // Bound memory.
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
        if let Ok(read) = std::fs::read_dir(&self.local_path) {
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
        s.track_input(b"whoami\n");
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
        assert_eq!(s.command_history, vec!["echo hi"]);
    }

    #[test]
    fn ignores_arrow_escape_sequences() {
        let mut s = session();
        // Up-arrow (ESC [ A) in the middle should be skipped, not recorded.
        s.track_input(b"cd \x1b[A/tmp\r");
        assert_eq!(s.command_history, vec!["cd /tmp"]);
    }

    #[test]
    fn ctrl_c_clears_the_line_without_recording() {
        let mut s = session();
        s.track_input(b"rm -rf /"); // oops
        s.track_input(&[0x03]); // Ctrl-C
        s.track_input(b"ls\r");
        assert_eq!(s.command_history, vec!["ls"]);
    }

    #[test]
    fn skips_consecutive_duplicates() {
        let mut s = session();
        s.track_input(b"ls\r");
        s.track_input(b"ls\r");
        assert_eq!(s.command_history, vec!["ls"]);
    }
}
