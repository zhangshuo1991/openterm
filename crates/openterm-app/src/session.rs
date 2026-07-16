//! The session model: one self-contained terminal session per tab.
//!
//! This is the structural fix for the old "god struct". Each [`Session`] owns
//! *its own* terminal grid, connection phase, and command channel. Switching
//! tabs is just changing `App::active` — no state is copied between a shared
//! global buffer and per-tab storage, so a background session keeps rendering
//! into its own grid and is intact when you switch back.

use openterm_core::HostId;
use openterm_ssh::HostKeyChallenge;
use openterm_terminal::{AlacrittyTerminalBuffer, TerminalEngine, TerminalSize, TerminalSnapshot};
use tokio::sync::mpsc;

use crate::connection::{Command, ConnectParams};

/// View-side render caches for the terminal canvas. Interior-mutable because
/// they are refreshed lazily from `view(&App)`.
///
/// Two layers of caching keep an idle frame from re-doing terminal work:
/// - `snapshot` memoizes the materialized cell grid, keyed by the buffer's
///   generation counter, so unrelated messages (ticks, mouse moves, toasts)
///   don't rebuild a `Vec<Vec<TerminalCell>>` per `view()`.
/// - `canvas` stores the grid's tessellated geometry; it is cleared only when
///   the render key (generation + font/theme/search state) changes, so a
///   redraw of an unchanged grid re-uses the GPU geometry wholesale.
#[derive(Default)]
pub struct TerminalRenderCache {
    snapshot: std::cell::RefCell<Option<(u64, std::sync::Arc<TerminalSnapshot>)>>,
    pub canvas: iced::widget::canvas::Cache,
    key: std::cell::Cell<u64>,
}

impl TerminalRenderCache {
    /// The current grid snapshot, rebuilt only when the buffer generation
    /// moved since the last call.
    pub fn snapshot(&self, term: &AlacrittyTerminalBuffer) -> std::sync::Arc<TerminalSnapshot> {
        let generation = term.generation();
        let mut slot = self.snapshot.borrow_mut();
        if let Some((cached_gen, snap)) = slot.as_ref() {
            if *cached_gen == generation {
                return snap.clone();
            }
        }
        let snap = std::sync::Arc::new(term.snapshot());
        *slot = Some((generation, snap.clone()));
        snap
    }

    /// Install the render key for the cached grid geometry; clears the canvas
    /// cache when it differs from the previous frame's key.
    pub fn sync_key(&self, key: u64) {
        if self.key.get() != key {
            self.canvas.clear();
            self.key.set(key);
        }
    }

    /// Drop everything (used when the terminal buffer itself is replaced and
    /// its generation counter restarts).
    pub fn reset(&self) {
        *self.snapshot.borrow_mut() = None;
        self.canvas.clear();
        self.key.set(0);
    }
}

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

// ---------------------------------------------------------------------------
// Smart suggestion engine
// ---------------------------------------------------------------------------

/// Strategy for how to source argument candidates for a given command.
#[derive(Debug, Clone)]
pub enum SuggestStrategy {
    /// List files in the current remote/local directory (universal default).
    Files,
    /// List files, then keep only those whose name ends with one of the given
    /// extensions. Used for `python3 *.py`, `node *.js`, `java -jar *.jar`.
    FilesWithExt(&'static [&'static str]),
    /// List only subdirectories of the current directory (`cd`, `pushd`).
    Directories,
    /// Query the remote process list and suggest PIDs.
    ProcessList,
    /// Run an arbitrary command on the remote and parse each line as a candidate.
    Remote { cmd: &'static str, ttl_ms: u64 },
    /// Built-in subcommand list — no network needed, zero latency.
    Subcommands(&'static [&'static str]),
}

/// Pick a suggestion strategy for a given command name.
pub fn strategy_for(cmd: &str) -> SuggestStrategy {
    match cmd {
        "kill" | "pkill" | "killall" => SuggestStrategy::ProcessList,
        "cd" | "pushd" => SuggestStrategy::Directories,
        "git" => SuggestStrategy::Subcommands(&[
            "push", "pull", "commit", "add", "checkout", "merge", "rebase",
            "clone", "fetch", "log", "diff", "status", "branch", "stash", "reset",
            "tag", "config", "remote", "init", "mv", "rm", "show", "cherry-pick",
            "rev-parse", "blame", "bisect", "reflog",
        ]),
        "docker" => SuggestStrategy::Subcommands(&[
            "ps", "images", "exec", "run", "stop", "start", "rm", "logs", "build",
            "pull", "push", "compose", "volume", "network", "kill", "restart",
            "cp", "inspect", "stats", "top", "attach", "tag",
        ]),
        "npm" | "pnpm" | "yarn" => SuggestStrategy::Subcommands(&[
            "install", "run", "start", "test", "build", "dev", "add", "remove",
            "init", "publish", "update", "audit", "link", "exec", "create",
        ]),
        "pm2" => SuggestStrategy::Subcommands(&[
            "start", "stop", "restart", "delete", "list", "logs", "monit",
            "jlist", "describe", "reload", "save", "resurrect", "kill",
        ]),
        "kubectl" => SuggestStrategy::Subcommands(&[
            "get", "apply", "delete", "describe", "logs", "exec", "port-forward",
            "create", "edit", "scale", "rollout", "config", "namespace", "top",
        ]),
        "systemctl" => SuggestStrategy::Remote {
            cmd: "systemctl list-units --type=service --all --no-legend --plain 2>/dev/null | awk '{print $1}' | head -40",
            ttl_ms: 10_000,
        },
        "python" | "python3" => SuggestStrategy::FilesWithExt(&[".py"]),
        "node" | "nodejs" | "deno" | "bun" => SuggestStrategy::FilesWithExt(&[".js", ".mjs", ".ts", ".cjs"]),
        "java" => SuggestStrategy::FilesWithExt(&[".jar", ".class"]),
        "gcc" | "g++" | "cc" | "clang" | "clang++" => SuggestStrategy::FilesWithExt(&[".c", ".cpp", ".cc", ".cxx"]),
        "rustc" | "cargo" => SuggestStrategy::FilesWithExt(&[".rs"]),
        "go" => SuggestStrategy::FilesWithExt(&[".go"]),
        "ruby" | "ruby3" | "irb" => SuggestStrategy::FilesWithExt(&[".rb"]),
        "lua" => SuggestStrategy::FilesWithExt(&[".lua"]),
        "php" => SuggestStrategy::FilesWithExt(&[".php"]),
        "perl" => SuggestStrategy::FilesWithExt(&[".pl", ".pm"]),
        "awk" | "gawk" => SuggestStrategy::FilesWithExt(&[".awk"]),
        _ => SuggestStrategy::Files,
    }
}

/// Common flags/options for frequently-used system and third-party commands.
/// Consulted when the token currently being typed starts with `-`. Zero
/// network cost — this is a static, hand-curated list of the flags people
/// actually reach for interactively, not full man-page coverage.
pub fn flags_for(cmd: &str) -> &'static [&'static str] {
    match cmd {
        "ps" => &["-ef", "-e", "-eo", "-aux", "-A", "-u", "-fade", "-o", "-p", "--sort"],
        "kill" | "pkill" | "killall" => &[
            "-9", "-15", "-SIGKILL", "-SIGTERM", "-SIGHUP", "-SIGINT", "-KILL",
            "-TERM", "-HUP", "-l", "-signal", "-f",
        ],
        "top" | "htop" => &["-u", "-p", "-n", "-b", "-d", "-c"],
        "grep" | "egrep" | "fgrep" => &[
            "-r", "-rn", "-i", "-v", "-c", "-l", "-n", "-w", "-E", "-o",
            "--color", "-A", "-B", "-C", "--include", "--exclude",
        ],
        "find" => &[
            "-name", "-iname", "-type", "-mtime", "-size", "-exec", "-maxdepth",
            "-newer", "-delete", "-perm",
        ],
        "tar" => &[
            "-xzvf", "-czvf", "-xvf", "-cvf", "-tvf", "-xz", "-cz", "-C",
            "--extract", "--create", "--list",
        ],
        "curl" => &[
            "-X", "-H", "-d", "-o", "-O", "-L", "-I", "-s", "-v", "-k",
            "--data", "--header", "-u", "-F",
        ],
        "wget" => &["-O", "-r", "-c", "-q", "--no-check-certificate", "-P", "-b"],
        "ssh" => &["-i", "-p", "-L", "-R", "-D", "-N", "-v", "-A", "-o"],
        "scp" | "rsync" => &["-r", "-a", "-v", "-z", "-P", "-i", "--delete", "-avz", "-p"],
        "docker" => &[
            "-d", "-it", "--rm", "-p", "-v", "-e", "--name", "--network",
            "--restart", "--entrypoint", "-a", "--all", "-f",
        ],
        "kubectl" => &[
            "-n", "--namespace", "-f", "-o", "-l", "--selector", "-w",
            "--watch", "--all-namespaces",
        ],
        "systemctl" | "service" => &["--now", "--type", "--state", "-l", "--user"],
        "journalctl" => &["-u", "-f", "-n", "-p", "--since", "--until", "-e", "-r"],
        "netstat" => &["-tulpn", "-anp", "-r", "-i", "-s"],
        "ss" => &["-tulpn", "-anp", "-l", "-a", "-s"],
        "lsof" => &["-i", "-p", "-u", "-c", "-n"],
        "df" => &["-h", "-k", "-T", "-i"],
        "du" => &["-h", "-s", "-sh", "-a", "--max-depth"],
        "chmod" => &["-R", "755", "644", "700", "600", "+x", "-x"],
        "chown" => &["-R", "-v"],
        "awk" | "gawk" => &["-F", "-v", "-f"],
        "sed" => &["-i", "-e", "-n", "-r", "-E"],
        "sort" => &["-n", "-r", "-k", "-u", "-t"],
        "uniq" => &["-c", "-d", "-u", "-i"],
        "xargs" => &["-I", "-n", "-P", "-0", "-r"],
        "diff" => &["-u", "-r", "-N", "-q", "-y"],
        "git" => &[
            "-m", "-a", "-am", "--force", "-f", "--all", "-v", "-b", "-d",
            "--global", "-u", "--set-upstream",
        ],
        "java" => &[
            "-jar", "-cp", "-classpath", "-Xmx", "-Xms", "-version", "-D",
            "-server", "-verbose", "-agentlib",
        ],
        "python" | "python3" => &["-m", "-c", "-u", "-i", "-v", "--version"],
        "node" => &["-e", "-v", "--version", "--inspect", "--experimental-modules"],
        "npm" | "pnpm" | "yarn" => &["-g", "--save", "--save-dev", "-D", "--force", "-v"],
        "ls" => &["-la", "-l", "-a", "-lh", "-lah", "-t", "-r", "-S"],
        "rm" => &["-rf", "-r", "-f", "-i", "-v"],
        "cp" | "mv" => &["-r", "-v", "-i", "-f", "-a", "-p"],
        "mkdir" => &["-p", "-v", "-m"],
        "iptables" => &["-L", "-A", "-D", "-I", "-F", "-t", "-p", "-j", "-s", "-d"],
        "crontab" => &["-e", "-l", "-r", "-u"],
        "less" | "more" => &["-N", "-S", "+F"],
        "head" | "tail" => &["-n", "-f", "-c"],
        "wc" => &["-l", "-w", "-c"],
        "ping" => &["-c", "-i", "-s", "-t", "-W"],
        _ => &[],
    }
}

/// Common command names across Linux distros and popular third-party tools.
/// Used for command-name completion when session history has nothing to
/// offer yet (cold start) — e.g. suggest "docker" after typing "doc" even
/// before the user has ever run it in this session.
const KNOWN_COMMANDS: &[&str] = &[
    "ls", "cd", "pwd", "cat", "less", "more", "head", "tail", "grep", "egrep",
    "fgrep", "find", "locate", "which", "whereis", "file", "stat", "du", "df",
    "mount", "umount", "ps", "top", "htop", "kill", "pkill", "killall",
    "nice", "renice", "nohup", "jobs", "bg", "fg", "screen", "tmux", "free",
    "uptime", "uname", "hostname", "whoami", "id", "who", "w", "last",
    "history", "chmod", "chown", "chgrp", "umask", "ln", "cp", "mv", "rm",
    "rmdir", "mkdir", "touch", "tar", "gzip", "gunzip", "zip", "unzip",
    "curl", "wget", "scp", "rsync", "ssh", "sftp", "ping", "traceroute",
    "dig", "nslookup", "netstat", "ss", "ip", "ifconfig", "iptables",
    "firewall-cmd", "ufw", "systemctl", "service", "journalctl", "dmesg",
    "crontab", "at", "sed", "awk", "cut", "sort", "uniq", "wc", "tee",
    "xargs", "diff", "patch", "tr", "echo", "printf", "export", "env",
    "alias", "source", "bash", "sh", "zsh", "exit", "logout", "su", "sudo",
    "passwd", "useradd", "userdel", "usermod", "groupadd", "git", "docker",
    "docker-compose", "kubectl", "helm", "npm", "pnpm", "yarn", "node",
    "python", "python3", "pip", "pip3", "java", "javac", "mvn", "gradle",
    "go", "cargo", "rustc", "gcc", "g++", "make", "cmake", "php", "ruby",
    "perl", "lua", "psql", "mysql", "redis-cli", "mongo", "nc", "telnet",
    "vim", "vi", "nano", "emacs", "man", "apt", "apt-get", "yum", "dnf",
    "pacman", "brew", "snap", "lsof", "strace", "ltrace", "gdb", "valgrind",
    "iostat", "vmstat", "sar", "pm2", "supervisorctl", "nginx", "apache2",
    "certbot",
];

/// SSH command string for strategies that need remote data.
/// Returns `(command, ttl_ms)`. `None` for Subcommands (no network).
pub fn strategy_query(strategy: &SuggestStrategy) -> Option<(&'static str, u64)> {
    match strategy {
        SuggestStrategy::Files => Some(("ls -1A 2>/dev/null | head -80", 10_000)),
        SuggestStrategy::FilesWithExt(_) => Some(("ls -1A 2>/dev/null | head -80", 10_000)),
        SuggestStrategy::Directories => Some(("ls -d */ 2>/dev/null", 10_000)),
        SuggestStrategy::ProcessList => Some((
            "ps -eo pid,comm --no-headers 2>/dev/null | sort -rn | head -40",
            5_000,
        )),
        SuggestStrategy::Remote { cmd, ttl_ms } => Some((cmd, *ttl_ms)),
        SuggestStrategy::Subcommands(_) => None,
    }
}

/// Cached directory listing (files + dirs separated for different strategies).
#[derive(Debug, Clone)]
pub struct DirCache {
    pub all_entries: Vec<String>,
    pub dirs: Vec<String>,
    pub fetched_at: std::time::Instant,
}

impl DirCache {
    pub fn is_fresh(&self, ttl_ms: u64) -> bool {
        self.fetched_at.elapsed().as_millis() < ttl_ms as u128
    }
}

/// Per-session smart-suggestion state.
#[derive(Debug, Default)]
pub struct SessionSuggestionState {
    /// Cache of `ls -1A` results (shared by Files / FilesWithExt / Directories).
    pub dir_cache: Option<DirCache>,
    /// Cache of PID list (for kill/pkill/killall).
    pub pid_cache: Option<(Vec<String>, std::time::Instant)>,
    /// Caches for Remote-strategy commands (keyed by command name).
    pub remote_caches: std::collections::HashMap<String, (Vec<String>, std::time::Instant)>,
    /// Tags currently being fetched with their start time (prevents duplicate
    /// requests and allows expiry if the response never arrives).
    pub pending: std::collections::HashMap<String, std::time::Instant>,
}

/// How long to wait before allowing a re-query for a pending tag.
const PENDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

impl SessionSuggestionState {
    /// Invalidate directory cache (called after `cd` is committed).
    pub fn invalidate_dir(&mut self) {
        self.dir_cache = None;
    }

    /// Check if a tag is already being fetched (within the timeout window).
    pub fn is_pending(&self, tag: &str) -> bool {
        if let Some(&start) = self.pending.get(tag) {
            if start.elapsed() < PENDING_TIMEOUT {
                return true;
            }
        }
        false
    }

    /// Mark a tag as being fetched.
    pub fn mark_pending(&mut self, tag: &str) {
        self.pending.insert(tag.to_string(), std::time::Instant::now());
    }

    /// Clear pending state (called when a query completes or fails).
    pub fn clear_pending(&mut self, tag: &str) {
        self.pending.remove(tag);
    }

    /// Clear all state (called on disconnect).
    pub fn clear_all(&mut self) {
        self.dir_cache = None;
        self.pid_cache = None;
        self.remote_caches.clear();
        self.pending.clear();
    }
}

/// Parse raw command output into a list of candidate strings.
pub fn parse_query_output(tag: &str, output: &str) -> Vec<String> {
    // Dynamic `--help` scrape: tag is "help:<cmd>". Extract flags from the
    // help text so any third-party tool that prints a conventional --help gets
    // flag completion without being in the static table.
    if let Some(_cmd) = tag.strip_prefix("help:") {
        return parse_help_flags(output);
    }
    match tag {
        "kill" | "pkill" | "killall" => {
            // ps output: "1234 nginx\n5678 python3" → ["1234", "5678"]
            output
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| l.split_whitespace().next())
                .map(String::from)
                .collect()
        }
        "cd" | "pushd" => {
            // ls -d */ output: "bin/\netc/\nhome/" → ["bin/", "etc/", "home/"]
            output
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .collect()
        }
        _ => {
            // Generic: each non-empty line is a candidate.
            output
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .collect()
        }
    }
}

/// Extract option flags from a command's `--help` text. Handles the common
/// conventions (GNU getopt, argparse, Go's flag pkg, clap): lines that begin
/// (after whitespace) with `-x` or `--long`, possibly `-x, --long`. Returns
/// each distinct flag token (`-x`, `--long`), stripped of any `=VALUE` /
/// `<ARG>` suffix. Best-effort and defensive — junk lines just yield nothing.
pub fn parse_help_flags(output: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim_start();
        // Only consider lines whose first non-space char starts an option.
        if !trimmed.starts_with('-') {
            continue;
        }
        // Split off the description: two+ spaces or a tab usually separate the
        // option column from its help text.
        let opt_col = trimmed
            .split("  ")
            .next()
            .unwrap_or(trimmed)
            .split('\t')
            .next()
            .unwrap_or(trimmed);
        for raw in opt_col.split([',', ' ', '[', ']']) {
            let tok = raw.trim();
            if !tok.starts_with('-') || tok.len() < 2 || tok == "--" {
                continue;
            }
            // Strip an attached value: --foo=BAR → --foo, --foo<n> → --foo.
            let flag: String = tok
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .collect();
            if flag.len() < 2 || flag == "--" || !flag.starts_with('-') {
                continue;
            }
            if seen.insert(flag.clone()) {
                out.push(flag);
                if out.len() >= 80 {
                    return out;
                }
            }
        }
    }
    out
}

/// Whether `cmd` is safe to probe with `<cmd> --help` for flag discovery. Only
/// word-like command names (no paths, no shell metacharacters) that the user
/// has actually run before should be probed — see the caller, which gates on
/// history membership too.
pub fn is_help_probe_safe(cmd: &str) -> bool {
    !cmd.is_empty()
        && cmd.len() <= 32
        && cmd
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && cmd.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Filter file candidates by extension (for FilesWithExt strategy).
pub fn filter_by_ext<'a>(files: &'a [String], exts: &'static [&'static str]) -> Vec<&'a str> {
    files
        .iter()
        .filter(|f| {
            let lower = f.to_lowercase();
            exts.iter().any(|ext| lower.ends_with(ext))
        })
        .map(String::as_str)
        .collect()
}

/// Split a typed line into (command_name, arg_prefix).
/// Returns (cmd_lowercased, arg_part) where arg_part is everything after the
/// first space. If no space, arg_part is empty (still in command-name phase).
pub fn split_command_and_arg(line: &str) -> (String, &str) {
    let trimmed = line.trim_start();
    if let Some(space_pos) = trimmed.find(char::is_whitespace) {
        let cmd = trimmed[..space_pos].to_lowercase();
        let arg = trimmed[space_pos..].trim_start();
        (cmd, arg)
    } else {
        (trimmed.to_lowercase(), "")
    }
}

/// Return the last pipeline/list segment of a typed line, so completion
/// tracks the command actually being typed right now rather than the whole
/// line. `ps -ef | grep java` → `"grep java"`; `cd /tmp && ls -l` → `"ls -l"`.
/// A trailing separator (still no command typed after it) yields `""`.
pub fn last_segment(line: &str) -> &str {
    line.rsplit(['|', ';', '&']).next().unwrap_or(line).trim_start()
}

/// Return the whitespace-delimited token currently being typed: the part of
/// `arg` after its last space (or all of `arg` if it has none). This is the
/// token completion should actually match against — `ps -ef` → arg="-ef",
/// `java -Xmx512m -jar app` → arg last token = "app".
pub fn last_token(arg: &str) -> &str {
    // Find the last whitespace *char* and slice after it by its real UTF-8
    // length. `char::is_whitespace` matches multi-byte spaces (full-width
    // U+3000, no-break U+00A0) common in CJK IME / pasted text, so a naive
    // `pos + 1` would land mid-codepoint and panic — and this runs on every
    // keystroke.
    match arg.char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        Some((pos, c)) => &arg[pos + c.len_utf8()..],
        None => arg,
    }
}

/// The token immediately before the one currently being typed, within `arg`.
/// `-Xmx512m -jar ` → "-jar"; used to look up "what usually follows this
/// token" in the learned bigram model. Returns `""` when there is no prior
/// token (still typing the first argument).
pub fn prev_token(arg: &str) -> &str {
    let cur = last_token(arg);
    // `cur` is a suffix of `arg`, so this byte split is on a char boundary.
    let head = arg[..arg.len() - cur.len()].trim_end();
    // Same multi-byte-whitespace hazard as `last_token`: advance by the real
    // char length, not a hard-coded +1.
    match head.char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        Some((pos, c)) => &head[pos + c.len_utf8()..],
        None => head,
    }
}

// ---------------------------------------------------------------------------
// Self-learning token model
//
// Complements the static `flags_for` / `KNOWN_COMMANDS` tables with per-host
// statistics learned from the user's own command history. This is what gives
// completion coverage for THIRD-PARTY / custom commands the static tables
// never heard of: if you run `mytool --deploy-env prod` a few times, the model
// learns both that `--deploy-env` follows `mytool` and that `prod` follows
// `--deploy-env`, so next time `mytool --dep` completes and `mytool
// --deploy-env ` predicts `prod`. Ranking is frecency (frequency + recency),
// like atuin / McFly, so your daily-driver commands beat one-off ones.
// ---------------------------------------------------------------------------

/// Frecency counter for one learned token: how often it's been seen and the
/// most recent "tick" (a monotonically increasing counter, not wall-clock, so
/// it's deterministic and needs no time source).
#[derive(Debug, Clone, Default)]
struct TokenStat {
    count: u32,
    last_seen: u64,
}

impl TokenStat {
    /// Higher is better. Frequency dominates; recency breaks ties and gently
    /// lifts things you've touched lately. Kept in f64 to avoid overflow.
    fn score(&self, now: u64) -> f64 {
        let recency = 1.0 / (1.0 + (now.saturating_sub(self.last_seen)) as f64);
        self.count as f64 + recency
    }
}

/// Per-command learned statistics: which tokens follow a command name, and
/// which tokens follow a given previous token (bigrams).
#[derive(Debug, Clone, Default)]
pub struct TokenModel {
    /// Monotonic tick, bumped once per learned command line.
    tick: u64,
    /// command → (token → stat). Tokens are flags and stable sub-tokens seen
    /// anywhere in that command's arguments.
    per_command: std::collections::HashMap<String, std::collections::HashMap<String, TokenStat>>,
    /// (command, previous_token) → (next_token → stat). Captures "value that
    /// usually follows this flag/word".
    bigrams: std::collections::HashMap<(String, String), std::collections::HashMap<String, TokenStat>>,
    /// command name → stat, for command-name-phase frecency ranking.
    commands: std::collections::HashMap<String, TokenStat>,
}

/// Commands whose *argument values* must never be learned/suggested (stale
/// PIDs, paths, and destructive targets are misleading). Flags for these are
/// still fine to learn — only positional/value tokens are suppressed.
const NO_VALUE_LEARN_COMMANDS: &[&str] = &[
    "kill", "pkill", "killall", "rm", "rmdir", "shutdown", "reboot",
];

/// Whether a token is stable enough to learn as a value/subcommand. Flags
/// (`-x`, `--long`) are always learnable. Otherwise we require a "word-like"
/// token: no path separators, no leading digit (skip PIDs/ports), reasonable
/// length, and only sane identifier characters. This keeps volatile junk
/// (file paths, PIDs, quoted strings, URLs) out of the model.
fn is_learnable_token(tok: &str) -> bool {
    if tok.is_empty() || tok.len() > 40 {
        return false;
    }
    if tok.starts_with('-') {
        // A flag: learnable as long as it's not a bare "-" / "--".
        return tok.len() > 1 && tok != "--";
    }
    if tok.contains('/') || tok.contains('\\') || tok.contains('$') || tok.contains('=') {
        return false;
    }
    let mut chars = tok.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() {
        return false; // skip numbers (PIDs/ports), globs, etc.
    }
    tok.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

impl TokenModel {
    /// Learn from one committed command line (may contain pipes — each segment
    /// is learned independently so `ps -ef | grep java` teaches both `ps` and
    /// `grep`).
    pub fn learn(&mut self, line: &str) {
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        for segment in line.split(['|', ';', '&']) {
            self.learn_segment(segment.trim(), tick);
        }
    }

    fn learn_segment(&mut self, segment: &str, tick: u64) {
        let mut it = segment.split_whitespace();
        let Some(cmd_raw) = it.next() else { return };
        let cmd = cmd_raw.to_lowercase();
        if cmd.starts_with('-') || cmd.contains('/') {
            return; // not a command name (a flag or a path invocation)
        }
        bump(&mut self.commands, cmd.clone(), tick);

        let allow_values = !NO_VALUE_LEARN_COMMANDS.contains(&cmd.as_str());
        let mut prev = String::new();
        for tok in it {
            let is_flag = tok.starts_with('-');
            if is_learnable_token(tok) && (is_flag || allow_values) {
                // Per-command token frequency.
                let entry = self.per_command.entry(cmd.clone()).or_default();
                bump(entry, tok.to_string(), tick);
                // Bigram: this token follows `prev`.
                if !prev.is_empty() {
                    let key = (cmd.clone(), prev.clone());
                    let entry = self.bigrams.entry(key).or_default();
                    bump(entry, tok.to_string(), tick);
                }
            }
            prev = tok.to_string();
        }
    }

    /// Learned command names extending `prefix`, best-frecency first.
    pub fn command_candidates(&self, prefix: &str) -> Vec<String> {
        rank_matching(&self.commands, prefix, self.tick)
    }

    /// Learned tokens for `cmd` extending `prefix` (flags or values depending
    /// on the prefix), best-frecency first.
    pub fn token_candidates(&self, cmd: &str, prefix: &str) -> Vec<String> {
        match self.per_command.get(cmd) {
            Some(map) => rank_matching(map, prefix, self.tick),
            None => Vec::new(),
        }
    }

    /// Learned tokens that usually follow `prev` under `cmd`, extending
    /// `prefix`, best-frecency first. Powers "value after a flag" prediction.
    pub fn bigram_candidates(&self, cmd: &str, prev: &str, prefix: &str) -> Vec<String> {
        match self.bigrams.get(&(cmd.to_string(), prev.to_string())) {
            Some(map) => rank_matching(map, prefix, self.tick),
            None => Vec::new(),
        }
    }
}

/// Bump a token's stat in a frecency map.
fn bump(map: &mut std::collections::HashMap<String, TokenStat>, key: String, tick: u64) {
    let stat = map.entry(key).or_default();
    stat.count = stat.count.saturating_add(1);
    stat.last_seen = tick;
}

/// Return every key that strictly extends `prefix`, sorted best-frecency
/// first. An empty prefix matches everything (useful for "what comes next").
fn rank_matching(
    map: &std::collections::HashMap<String, TokenStat>,
    prefix: &str,
    now: u64,
) -> Vec<String> {
    let mut hits: Vec<(&String, f64)> = map
        .iter()
        .filter(|(k, _)| k.len() > prefix.len() && k.starts_with(prefix))
        .map(|(k, s)| (k, s.score(now)))
        .collect();
    hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    hits.into_iter().map(|(k, _)| k.clone()).collect()
}

/// Cache tag for a given strategy (used to match query results back).
pub fn strategy_tag(strategy: &SuggestStrategy) -> &'static str {
    match strategy {
        SuggestStrategy::Files => "__files__",
        SuggestStrategy::FilesWithExt(_) => "__files__",
        SuggestStrategy::Directories => "cd",
        SuggestStrategy::ProcessList => "kill",
        SuggestStrategy::Remote { .. } => "",
        SuggestStrategy::Subcommands(_) => "",
    }
}

/// Find the best suffix to suggest from candidates, given what's already typed.
/// Returns only the part *after* the prefix.
pub fn match_suffix<'a>(prefix: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    for cand in candidates {
        if cand.len() > prefix.len() && cand.starts_with(prefix) {
            return Some(cand[prefix.len()..].to_string());
        }
        // Also match if candidate equals prefix exactly → no suffix needed.
    }
    None
}

/// One terminal session = one tab.
pub struct Session {
    pub id: u64,
    pub kind: SessionKind,
    pub config: SessionConfig,
    /// This session's own terminal grid. Never shared.
    pub terminal: AlacrittyTerminalBuffer,
    /// Cached snapshot + canvas geometry for `terminal` (see the type docs).
    pub render: TerminalRenderCache,
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
    /// Smart suggestion state: caches for remote directory listings, PID
    /// lists, and custom query results. Drives context-aware suggestions.
    pub suggestion_state: SessionSuggestionState,
    /// Self-learning token model: per-host statistics (learned from command
    /// history) of which flags/subcommands follow each command and which
    /// values follow each flag. Powers completion for third-party/custom
    /// commands the static tables don't know about. Seeded from persisted
    /// history on connect, then updated live as commands are committed.
    pub token_model: TokenModel,
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

    /// Lowercase file extension (e.g. "py", "rs"), used as the syntect token
    /// for the editor's syntax highlighter. Empty when the name has no ext.
    pub fn ext(&self) -> String {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        match name.rsplit_once('.') {
            Some((_, ext)) => ext.to_ascii_lowercase(),
            None => String::new(),
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
    /// When the transfer began (for elapsed time + ETA).
    pub started_at: std::time::Instant,
    /// When it finished (Done/Failed) — freezes the elapsed clock. `None`
    /// while still active.
    pub finished_at: Option<std::time::Instant>,
    /// Resolved remote path — kept so a paused transfer can be resumed by
    /// re-issuing the original download/upload command.
    pub remote: String,
    /// Resolved local path (same purpose as `remote`).
    pub local: String,
    /// Whether this transfer is a whole directory tree.
    pub is_dir: bool,
    /// True between clicking Pause and the worker confirming the pause (the
    /// worker must drain in-flight chunks first). Shows "Pausing…" so the row
    /// gives instant feedback instead of looking unresponsive.
    pub pause_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    Active,
    /// Paused at a safe point; the `.part` is preserved. Holds the byte count
    /// reached, so resume can show where it left off.
    Paused,
    Done,
    Failed(String),
}

impl Transfer {
    /// Wall-clock time the transfer has run: live while active, frozen once
    /// finished.
    pub fn elapsed(&self) -> std::time::Duration {
        let end = self.finished_at.unwrap_or_else(std::time::Instant::now);
        end.saturating_duration_since(self.started_at)
    }

    /// Estimated seconds remaining, based on *average* throughput so far
    /// (steadier than the instantaneous speed). `None` when the total is
    /// unknown, nothing has transferred yet, or the transfer is complete.
    pub fn eta_secs(&self) -> Option<f64> {
        if self.total == 0 || self.transferred >= self.total || self.transferred == 0 {
            return None;
        }
        let elapsed = self.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return None;
        }
        let avg_bps = self.transferred as f64 / elapsed;
        if avg_bps <= 0.0 {
            return None;
        }
        let remaining = (self.total - self.transferred) as f64;
        Some(remaining / avg_bps)
    }
}

impl Session {
    pub fn new(id: u64, config: SessionConfig, cols: u16, rows: u16) -> Self {
        Self {
            id,
            kind: SessionKind::Ssh,
            config,
            terminal: AlacrittyTerminalBuffer::new(TerminalSize { cols, rows }),
            render: TerminalRenderCache::default(),
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
            suggestion_state: SessionSuggestionState::default(),
            token_model: TokenModel::default(),
        }
    }

    /// Seed the learned token model from prior command lines for this host,
    /// passed oldest-first so the frecency ticks line up with real recency.
    /// Called each time a session reaches `Connected`; resets first so a
    /// reconnect rebuilds cleanly instead of double-counting history (live
    /// commands from the previous connection are already persisted and thus
    /// included in the passed-in history).
    pub fn seed_token_model<'a>(&mut self, commands: impl Iterator<Item = &'a str>) {
        self.token_model = TokenModel::default();
        for cmd in commands {
            self.token_model.learn(cmd);
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
        // Invalidate directory cache if the user ran cd/pushd/popd — the
        // listing will be stale on the next prompt.
        let first_word = line.split_whitespace().next().unwrap_or("");
        if matches!(first_word, "cd" | "pushd" | "popd") {
            self.suggestion_state.invalidate_dir();
        }
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
        // The new buffer's generation restarts, so the old cached snapshot
        // would otherwise look "current" — drop it explicitly.
        self.render.reset();
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

/// Commands whose arguments are context-specific (PIDs, file paths, ports)
/// and should not be suggested from history — the stale value is misleading
/// rather than helpful.
const NO_ARG_SUGGEST_COMMANDS: &[&str] = &[
    "kill", "pkill", "killall", "rm", "rmdir", "del", "shutdown", "reboot",
    "systemctl", "service", "docker", "kubectl",
];

/// Returns the first whitespace-delimited token of `s` (the command name),
/// lowercased for comparison.
fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

/// True if `line` has progressed past the command name into arguments,
/// and the command is in the no-arg-suggest blocklist.
fn is_no_arg_suggest_command(line: &str) -> bool {
    let cmd = first_token(line).to_lowercase();
    if cmd.is_empty() {
        return false;
    }
    let has_space = line.chars().any(|c| c == ' ');
    has_space && NO_ARG_SUGGEST_COMMANDS.contains(&cmd.as_str())
}

/// Find the suffix to suggest after `line`: the first candidate (iterated
/// newest-first) that strictly extends `line` as a prefix. Returns `None` when
/// the line is too short, empty/whitespace, or nothing matches. The returned
/// string is only the part *after* what's already typed.
///
/// Context safety: if the user has typed past the command name (a space
/// follows the first token) and the command is in the no-arg-suggest blocklist
/// (e.g. `kill`, `rm`), no suggestion is produced — stale PIDs and file paths
/// from history are misleading, not helpful.
pub fn suggestion_suffix<'a>(
    line: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<String> {
    // Avoid noise: don't suggest until at least 2 non-space chars are typed.
    if line.trim().len() < 2 {
        return None;
    }
    // Don't suggest arguments for context-specific commands like `kill <pid>`.
    if is_no_arg_suggest_command(line) {
        return None;
    }
    for cand in candidates {
        if cand.len() > line.len() && cand.starts_with(line) {
            return Some(cand[line.len()..].to_string());
        }
    }
    None
}

/// Command-name-phase suggestion: try the session's own history first (it
/// wins, since it's a full remembered command, not just a bare name), then
/// fall back to the static [`KNOWN_COMMANDS`] list so completion works even
/// on a command that has never been run in this session (e.g. typing "doc"
/// for the first time still suggests "ker" to reach "docker").
pub fn command_name_suggestion<'a>(
    line: &str,
    history: impl Iterator<Item = &'a str>,
) -> Option<String> {
    if let Some(s) = suggestion_suffix(line, history) {
        return Some(s);
    }
    let trimmed = line.trim();
    if trimmed.len() < 2 {
        return None;
    }
    let lower = trimmed.to_lowercase();
    for cand in KNOWN_COMMANDS {
        if cand.len() > lower.len() && cand.starts_with(&lower) {
            return Some(cand[lower.len()..].to_string());
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
/// Human-friendly name ordering: case-insensitive, with runs of digits
/// compared as numbers so `item2` sorts before `item10` (the way Finder /
/// Explorer order files) instead of byte-lexicographically. Non-digit
/// characters compare by lowercased char; a purely lexicographic tie falls
/// back to the raw bytes so distinct names never compare equal.
pub fn natural_name_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b), // equal so far → stable raw tiebreak
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    // Compare the two digit runs as numbers, ignoring leading
                    // zeros; the longer run (after trimming zeros) is larger.
                    let na: String = collect_digits(&mut ai);
                    let nb: String = collect_digits(&mut bi);
                    let ta = na.trim_start_matches('0');
                    let tb = nb.trim_start_matches('0');
                    let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    // Numerically equal (e.g. "01" vs "1") → shorter first.
                    let zord = na.len().cmp(&nb.len());
                    if zord != Ordering::Equal {
                        return zord;
                    }
                } else {
                    let la = ca.to_ascii_lowercase();
                    let lb = cb.to_ascii_lowercase();
                    let ord = la.cmp(&lb);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

/// Pull the leading run of ASCII digits off `it` into a string.
fn collect_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s
}

pub fn sort_local(entries: &mut [LocalEntry], sort: SortField, ascending: bool) {
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            let ord = match sort {
                SortField::Name => natural_name_cmp(&a.name, &b.name),
                SortField::Size => a.size.cmp(&b.size).then_with(|| natural_name_cmp(&a.name, &b.name)),
                SortField::Modified => a.modified.cmp(&b.modified).then_with(|| natural_name_cmp(&a.name, &b.name)),
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
                SortField::Name => natural_name_cmp(&a.name, &b.name),
                SortField::Size => a
                    .size
                    .unwrap_or(0)
                    .cmp(&b.size.unwrap_or(0))
                    .then_with(|| natural_name_cmp(&a.name, &b.name)),
                SortField::Modified => a
                    .modified
                    .unwrap_or(0)
                    .cmp(&b.modified.unwrap_or(0))
                    .then_with(|| natural_name_cmp(&a.name, &b.name)),
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
    fn suggestion_blocked_for_kill_args() {
        // kill with arguments: stale PIDs from history are misleading.
        let cands = ["kill 12345", "kill -9 99876"];
        assert_eq!(suggestion_suffix("kill 1", cands.iter().copied()), None);
        assert_eq!(suggestion_suffix("kill ", cands.iter().copied()), None);
        // But the command name itself can still be suggested.
        let cands2 = ["killall", "kill"];
        assert_eq!(
            suggestion_suffix("ki", cands2.iter().copied()),
            Some("llall".to_string())
        );
    }

    #[test]
    fn suggestion_blocked_for_rm_args() {
        let cands = ["rm -rf /tmp/old", "rm /var/log/app.log"];
        assert_eq!(suggestion_suffix("rm ", cands.iter().copied()), None);
        assert_eq!(suggestion_suffix("rm /v", cands.iter().copied()), None);
    }

    #[test]
    fn suggestion_allowed_for_safe_commands_with_args() {
        // cd, git, ls — argument suggestions are useful for these.
        let cands = ["cd /usr/local/bin", "git push origin main"];
        assert_eq!(
            suggestion_suffix("cd /us", cands.iter().copied()),
            Some("r/local/bin".to_string())
        );
        assert_eq!(
            suggestion_suffix("git p", cands.iter().copied()),
            Some("ush origin main".to_string())
        );
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

    // --- Smart suggestion engine tests ---

    #[test]
    fn strategy_for_known_commands() {
        assert!(matches!(
            strategy_for("kill"),
            SuggestStrategy::ProcessList
        ));
        assert!(matches!(
            strategy_for("cd"),
            SuggestStrategy::Directories
        ));
        assert!(matches!(
            strategy_for("python3"),
            SuggestStrategy::FilesWithExt(&[".py"])
        ));
        assert!(matches!(
            strategy_for("git"),
            SuggestStrategy::Subcommands(_)
        ));
        assert!(matches!(
            strategy_for("foobar"),
            SuggestStrategy::Files
        ));
    }

    #[test]
    fn split_command_and_arg_works() {
        assert_eq!(
            split_command_and_arg("kill 123"),
            ("kill".to_string(), "123")
        );
        assert_eq!(
            split_command_and_arg("cd /usr/"),
            ("cd".to_string(), "/usr/")
        );
        assert_eq!(
            split_command_and_arg("git"),
            ("git".to_string(), "")
        );
        assert_eq!(
            split_command_and_arg("  ls  -la"),
            ("ls".to_string(), "-la")
        );
    }

    #[test]
    fn parse_kill_output_extracts_pids() {
        let output = "1234 nginx\n5678 python3\n  9012 sshd\n";
        let pids = parse_query_output("kill", output);
        assert_eq!(pids, vec!["1234", "5678", "9012"]);
    }

    #[test]
    fn parse_cd_output_preserves_slashes() {
        let output = "bin/\netc/\nhome/\n";
        let dirs = parse_query_output("cd", output);
        assert_eq!(dirs, vec!["bin/", "etc/", "home/"]);
    }

    #[test]
    fn parse_generic_output_splits_lines() {
        let output = "nginx.service\nssh.service\n";
        let units = parse_query_output("systemctl", output);
        assert_eq!(units, vec!["nginx.service", "ssh.service"]);
    }

    #[test]
    fn match_suffix_finds_extending_candidate() {
        let cands = ["app.py", "test.py", "utils.py"];
        assert_eq!(
            match_suffix("ap", cands.iter().copied()),
            Some("p.py".to_string())
        );
    }

    #[test]
    fn match_suffix_none_for_exact_match() {
        let cands = ["app.py"];
        assert_eq!(match_suffix("app.py", cands.iter().copied()), None);
    }

    #[test]
    fn match_suffix_none_for_no_match() {
        let cands = ["app.py"];
        assert_eq!(match_suffix("zzz", cands.iter().copied()), None);
    }

    #[test]
    fn match_suffix_empty_prefix_returns_none() {
        let cands = ["app.py"];
        assert_eq!(match_suffix("", cands.iter().copied()), None);
    }

    #[test]
    fn filter_by_ext_keeps_matching_extensions() {
        let files = vec![
            "app.py".to_string(),
            "server.js".to_string(),
            "test.py".to_string(),
            "README.md".to_string(),
        ];
        let py_files = filter_by_ext(&files, &[".py"]);
        assert_eq!(py_files, vec!["app.py", "test.py"]);
    }

    #[test]
    fn filter_by_ext_case_insensitive() {
        let files = vec![
            "app.PY".to_string(),
            "test.py".to_string(),
        ];
        let py_files = filter_by_ext(&files, &[".py"]);
        assert_eq!(py_files, vec!["app.PY", "test.py"]);
    }

    #[test]
    fn dir_cache_freshness_check() {
        let cache = DirCache {
            all_entries: vec!["foo".to_string()],
            dirs: vec![],
            fetched_at: std::time::Instant::now(),
        };
        assert!(cache.is_fresh(10_000));
    }

    #[test]
    fn dir_cache_expires_after_ttl() {
        let cache = DirCache {
            all_entries: vec!["foo".to_string()],
            dirs: vec![],
            fetched_at: std::time::Instant::now() - std::time::Duration::from_millis(15_000),
        };
        assert!(!cache.is_fresh(10_000));
    }

    #[test]
    fn suggestion_state_invalidates_dir_cache() {
        let mut state = SessionSuggestionState::default();
        state.dir_cache = Some(DirCache {
            all_entries: vec!["foo".to_string()],
            dirs: vec![],
            fetched_at: std::time::Instant::now(),
        });
        state.invalidate_dir();
        assert!(state.dir_cache.is_none());
    }

    #[test]
    fn suggestion_state_pending_tracking() {
        let mut state = SessionSuggestionState::default();
        assert!(!state.is_pending("kill"));
        state.mark_pending("kill");
        assert!(state.is_pending("kill"));
        state.clear_pending("kill");
        assert!(!state.is_pending("kill"));
    }

    #[test]
    fn suggestion_state_pending_expires() {
        let mut state = SessionSuggestionState::default();
        // Manually insert an old pending timestamp.
        state.pending.insert(
            "kill".to_string(),
            std::time::Instant::now() - std::time::Duration::from_secs(5),
        );
        // Should be expired (not pending) after PENDING_TIMEOUT.
        assert!(!state.is_pending("kill"));
    }

    #[test]
    fn suggestion_state_clear_all_resets_everything() {
        let mut state = SessionSuggestionState::default();
        state.dir_cache = Some(DirCache {
            all_entries: vec!["foo".to_string()],
            dirs: vec![],
            fetched_at: std::time::Instant::now(),
        });
        state.pid_cache = Some((vec!["123".to_string()], std::time::Instant::now()));
        state.mark_pending("kill");
        state.clear_all();
        assert!(state.dir_cache.is_none());
        assert!(state.pid_cache.is_none());
        assert!(!state.is_pending("kill"));
    }

    #[test]
    fn cd_command_invalidates_dir_cache_on_commit() {
        let mut s = session();
        s.suggestion_state.dir_cache = Some(DirCache {
            all_entries: vec!["foo".to_string()],
            dirs: vec![],
            fetched_at: std::time::Instant::now(),
        });
        // Simulate: user types "cd /tmp" + Enter, then shell responds.
        s.track_input(b"cd /tmp\r");
        s.write_output(b"\r\n");
        // The cache should have been invalidated by commit_input_line.
        assert!(s.suggestion_state.dir_cache.is_none());
    }

    #[test]
    fn non_cd_command_keeps_dir_cache() {
        let mut s = session();
        s.suggestion_state.dir_cache = Some(DirCache {
            all_entries: vec!["foo".to_string()],
            dirs: vec![],
            fetched_at: std::time::Instant::now(),
        });
        s.track_input(b"ls -la\r");
        s.write_output(b"\r\n");
        assert!(s.suggestion_state.dir_cache.is_some());
    }

    // --- Multi-token / pipeline / flags completion (broader command coverage) ---

    #[test]
    fn last_segment_tracks_the_command_after_a_pipe() {
        assert_eq!(last_segment("ps -ef|grep java"), "grep java");
        assert_eq!(last_segment("ps -ef | grep java"), "grep java");
        assert_eq!(last_segment("cd /tmp && ls -l"), "ls -l");
        assert_eq!(last_segment("echo hi; ls"), "ls");
        // No separator: the whole line is the segment.
        assert_eq!(last_segment("ps -ef"), "ps -ef");
        // Trailing separator with nothing typed after it yet.
        assert_eq!(last_segment("ls -la |"), "");
    }

    #[test]
    fn last_token_finds_the_token_being_typed() {
        // Single flag: the whole arg is the token.
        assert_eq!(last_token("-ef"), "-ef");
        // Multiple tokens: only the trailing one matters.
        assert_eq!(last_token("-Xmx512m -jar app"), "app");
        assert_eq!(last_token("-9 123"), "123");
        assert_eq!(last_token(""), "");
    }

    #[test]
    fn flags_for_known_commands_covers_common_flags() {
        assert!(flags_for("ps").contains(&"-ef"));
        assert!(flags_for("kill").contains(&"-9"));
        assert!(flags_for("java").contains(&"-jar"));
        assert!(flags_for("grep").contains(&"-r"));
        assert!(flags_for("docker").contains(&"--rm"));
        assert!(flags_for("totally-unknown-cmd").is_empty());
    }

    #[test]
    fn flag_match_suffix_completes_ps_ef() {
        // Simulates what recompute_active_suggestion does: cur_token starts
        // with '-', so it's matched against flags_for("ps").
        let suffix = match_suffix("-e", flags_for("ps").iter().copied());
        assert_eq!(suffix, Some("f".to_string()));
    }

    #[test]
    fn command_name_suggestion_falls_back_to_known_commands() {
        // No history at all: should still suggest from the static list.
        let empty: Vec<&str> = vec![];
        assert_eq!(
            command_name_suggestion("doc", empty.iter().copied()),
            Some("ker".to_string())
        );
    }

    #[test]
    fn command_name_suggestion_prefers_history_over_static_list() {
        // History has a fuller remembered command; it should win over the
        // bare static command name.
        let hist = ["docker ps -a"];
        assert_eq!(
            command_name_suggestion("doc", hist.iter().copied()),
            Some("ker ps -a".to_string())
        );
    }

    // --- prev_token / learned token model / help scrape ---

    #[test]
    fn prev_token_finds_the_token_before_the_cursor() {
        assert_eq!(prev_token("-jar "), "-jar");
        assert_eq!(prev_token("-Xmx512m -jar app"), "-jar");
        assert_eq!(prev_token("app"), ""); // first arg: no prior token
        assert_eq!(prev_token("--deploy-env "), "--deploy-env");
    }

    #[test]
    fn model_learns_flags_for_third_party_commands() {
        let mut m = TokenModel::default();
        m.learn("mytool --deploy-env prod");
        m.learn("mytool --deploy-env prod");
        // Flag is learned even though `mytool` is in no static table.
        let flags = m.token_candidates("mytool", "--dep");
        assert_eq!(flags.first().map(String::as_str), Some("--deploy-env"));
    }

    #[test]
    fn model_learns_value_after_a_flag_via_bigram() {
        let mut m = TokenModel::default();
        m.learn("mytool --deploy-env prod");
        // What usually follows `--deploy-env`? → "prod".
        let vals = m.bigram_candidates("mytool", "--deploy-env", "");
        assert_eq!(vals, vec!["prod".to_string()]);
        // And prefix-filtered.
        assert_eq!(
            m.bigram_candidates("mytool", "--deploy-env", "pr"),
            vec!["prod".to_string()]
        );
    }

    #[test]
    fn model_ranks_by_frecency() {
        let mut m = TokenModel::default();
        // "build" seen 3×, "bench" seen 1× — build should rank first.
        m.learn("cargo bench");
        m.learn("cargo build");
        m.learn("cargo build");
        m.learn("cargo build");
        let cands = m.token_candidates("cargo", "b");
        assert_eq!(cands.first().map(String::as_str), Some("build"));
    }

    #[test]
    fn model_does_not_learn_volatile_values_for_kill() {
        let mut m = TokenModel::default();
        m.learn("kill -9 12345");
        // The flag is fine to learn…
        assert_eq!(
            m.token_candidates("kill", "-9"),
            Vec::<String>::new(),
            "exact match yields no extending suffix"
        );
        assert!(m.token_candidates("kill", "-").contains(&"-9".to_string()));
        // …but the PID value must NOT be learned.
        assert!(m.bigram_candidates("kill", "-9", "").is_empty());
        assert!(!m.token_candidates("kill", "1").contains(&"12345".to_string()));
    }

    #[test]
    fn model_skips_paths_and_numbers_as_values() {
        let mut m = TokenModel::default();
        m.learn("cat /etc/passwd");
        m.learn("sleep 300");
        // Paths and bare numbers are not learnable values.
        assert!(m.token_candidates("cat", "/").is_empty());
        assert!(m.token_candidates("sleep", "3").is_empty());
    }

    #[test]
    fn model_learns_each_pipeline_segment() {
        let mut m = TokenModel::default();
        m.learn("ps -ef | grep java");
        // Both `ps` and `grep` are learned as commands.
        assert!(m.command_candidates("p").contains(&"ps".to_string()));
        assert!(m.command_candidates("gr").contains(&"grep".to_string()));
    }

    #[test]
    fn parse_help_flags_extracts_options() {
        let help = "\
Usage: mytool [OPTIONS]

Options:
  -h, --help            Print help
  -v, --verbose         Be noisy
      --deploy-env ENV  Target environment
  -o, --output=FILE     Write to FILE
";
        let flags = parse_help_flags(help);
        assert!(flags.contains(&"-h".to_string()));
        assert!(flags.contains(&"--help".to_string()));
        assert!(flags.contains(&"--verbose".to_string()));
        assert!(flags.contains(&"--deploy-env".to_string()));
        // `=FILE` suffix stripped.
        assert!(flags.contains(&"--output".to_string()));
        assert!(!flags.iter().any(|f| f.contains('=')));
    }

    #[test]
    fn parse_help_flags_ignores_prose_lines() {
        let help = "This tool does things.\nRun it carefully.\nNo options here.";
        assert!(parse_help_flags(help).is_empty());
    }

    #[test]
    fn help_probe_safety_gate() {
        assert!(is_help_probe_safe("docker"));
        assert!(is_help_probe_safe("my-tool.sh"));
        assert!(!is_help_probe_safe("./script")); // path
        assert!(!is_help_probe_safe("rm -rf")); // space / metachar
        assert!(!is_help_probe_safe("")); // empty
        assert!(!is_help_probe_safe("2tool")); // leading digit
    }

    #[test]
    fn token_helpers_survive_multibyte_whitespace_and_args() {
        // Full-width space (U+3000, 3 bytes) and no-break space (U+00A0, 2
        // bytes) between tokens used to panic last_token/prev_token via a
        // `pos + 1` slice landing mid-codepoint. These must not panic and must
        // return the right token.
        assert_eq!(last_token("ls\u{3000}foo"), "foo");
        assert_eq!(last_token("git\u{00A0}commit"), "commit");
        assert_eq!(prev_token("ls\u{3000}foo bar"), "foo");
        assert_eq!(prev_token("a\u{00A0}b\u{3000}c"), "b");
        // Multi-byte token content is fine too.
        assert_eq!(last_token("cd 文档"), "文档");
        assert_eq!(prev_token("cd 文档/"), "cd");
        // No whitespace → whole arg; trailing space → empty current token.
        assert_eq!(last_token("solo"), "solo");
        assert_eq!(last_token("cmd "), "");
    }

    #[test]
    fn parse_hex_rejects_multibyte_that_is_six_bytes() {
        use crate::theme::parse_hex;
        // "中中" is 2 chars but 6 bytes — must be rejected, not panic.
        assert!(parse_hex("中中").is_none());
        assert!(parse_hex("#中中").is_none());
        // Valid colors still parse.
        assert!(parse_hex("#3c9e8f").is_some());
        assert!(parse_hex("3c9e8f").is_some());
        assert!(parse_hex("xyzxyz").is_none()); // ascii but not hex digits
    }

    #[test]
    fn natural_name_cmp_orders_numbers_and_case_like_a_file_manager() {
        use std::cmp::Ordering;
        // Numeric runs compare as numbers, not bytes: item2 < item10.
        assert_eq!(natural_name_cmp("item2", "item10"), Ordering::Less);
        assert_eq!(natural_name_cmp("file9", "file100"), Ordering::Less);
        // Case-insensitive: "Apple" and "apple" tie on the letters, and the raw
        // tiebreak keeps them distinct (uppercase byte < lowercase byte).
        assert_eq!(natural_name_cmp("apple", "Banana"), Ordering::Less);
        assert_eq!(natural_name_cmp("Banana", "apple"), Ordering::Greater);

        // A full sort lands in human order, not lexicographic (…1, 10, 2…).
        let mut names = vec!["item10", "item2", "Item1", "item20", "item3"];
        names.sort_by(|a, b| natural_name_cmp(a, b));
        assert_eq!(names, vec!["Item1", "item2", "item3", "item10", "item20"]);

        // Sort is total/consistent: no pair is simultaneously Less both ways.
        let sample = ["a", "A", "a1", "a10", "a2", "b", "10", "2", "ab"];
        for x in sample {
            for y in sample {
                let xy = natural_name_cmp(x, y);
                let yx = natural_name_cmp(y, x);
                assert_eq!(xy, yx.reverse(), "asymmetry between {x:?} and {y:?}");
            }
        }
    }
}
