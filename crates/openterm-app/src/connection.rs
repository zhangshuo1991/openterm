//! Per-session connection actor.
//!
//! Each terminal session owns exactly one connection worker, driven by an iced
//! subscription keyed on the session id. The worker holds the live
//! [`RusshSession`] in an `Arc` and multiplexes everything over that single SSH
//! connection:
//!
//! * an interactive shell channel (PTY), pumped continuously, and
//! * on-demand SFTP channels for the file workspace.
//!
//! The old app redialed a brand-new SSH connection for every SFTP operation;
//! here SFTP reuses the connection the shell is already running on, which is
//! how real SSH clients behave.
//!
//! Communication is message-based: the UI sends [`Command`]s through a channel
//! the worker hands back on startup, and the worker streams [`Event`]s tagged
//! with the originating `session_id` back into the iced runtime.

use std::sync::Arc;

use iced::futures::{SinkExt, Stream};
use openterm_ssh::{
    ConnectRoute, HostKeyChallenge, PtyEvent, PtyInput, PtySize, RemoteFileEntry, RemoteFileKind,
    RusshSession, ShellOptions, SshError,
};
use tokio::sync::mpsc;

use crate::session_connection::{ConnectionConfig, SessionConnection};

/// Parameters needed to open a shell on a route.
#[derive(Debug, Clone)]
pub struct ConnectParams {
    pub route: ConnectRoute,
    pub cols: u16,
    pub rows: u16,
    pub term: String,
}

/// Direction of an SFTP transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

/// Quiet period that flushes a small (interactive-scale) burst. Two
/// milliseconds is far below a frame (16 ms) yet long enough for a shell
/// echo's escape-sequence fragments to arrive and merge.
const QUIESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(2);
/// Bursts up to this size are treated as interactive and get the quiescence
/// flush; anything bigger is a bulk stream and waits for the frame tick.
const QUIESCE_MAX_BYTES: usize = 4 * 1024;
/// Hard cap on one merged output message, so memory stays bounded even if the
/// producer outruns the UI runtime for a long time.
const OUTPUT_BATCH_CAP: usize = 256 * 1024;
/// Frame-paced flush for continuous streams (`cat`, build logs), matching a
/// 60 Hz display: more redraws than that cannot be seen anyway.
const OUTPUT_FRAME_TICK: std::time::Duration = std::time::Duration::from_millis(16);

/// Merges PTY output bursts before they cross into the UI runtime.
///
/// A keystroke echo flushes as soon as the stream has been quiet for
/// [`QUIESCE_WINDOW`], so typing stays snappy. A continuous stream keeps
/// merging into frame-sized messages instead of dispatching one Elm message
/// per socket read — during `cat` that previously meant hundreds of message
/// dispatches and full-grid re-snapshots per second.
pub(crate) struct OutputCoalescer {
    buf: Vec<u8>,
    deadline: Option<tokio::time::Instant>,
}

impl OutputCoalescer {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            deadline: None,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() >= OUTPUT_BATCH_CAP {
            // Caller flushes on size (see `len`); no timer needed.
            self.deadline = None;
        } else if self.buf.len() <= QUIESCE_MAX_BYTES {
            self.deadline = Some(tokio::time::Instant::now() + QUIESCE_WINDOW);
        } else {
            // Bulk stream: batch until the frame tick.
            self.deadline = None;
        }
    }

    fn flush(&mut self) -> Vec<u8> {
        self.deadline = None;
        std::mem::take(&mut self.buf)
    }

    fn len(&self) -> usize {
        self.buf.len()
    }
}

/// Commands the UI sends to a session's connection worker.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Command {
    /// Open (or reopen) the shell using these parameters.
    Connect(ConnectParams),
    /// Forward bytes typed by the user to the PTY.
    Write(Vec<u8>),
    /// Tell the remote PTY the viewport changed.
    Resize { cols: u16, rows: u16 },
    /// List a remote directory over the live connection.
    SftpList(String),
    /// Download a remote file to a local path (streamed, with progress).
    SftpDownload {
        id: u64,
        name: String,
        remote: String,
        local: String,
        /// Size known from the listing (0 = unknown, query it).
        size: u64,
        /// True when `remote` is a directory: transfer the whole tree.
        is_dir: bool,
    },
    /// Upload a local file to a remote path (streamed, with progress).
    SftpUpload {
        id: u64,
        name: String,
        local: String,
        remote: String,
        /// Size known from local metadata (0 = unknown, query it).
        size: u64,
        /// True when `local` is a directory: transfer the whole tree.
        is_dir: bool,
    },
    /// Create a remote directory.
    SftpMkdir(String),
    /// Pause an in-flight transfer at a safe point, keeping its `.part` so it
    /// can be resumed later by re-issuing the original download/upload command.
    SftpPauseTransfer { id: u64 },
    /// Cancel an in-flight transfer and delete its `.part` scratch file(s).
    SftpCancelTransfer { id: u64 },
    /// Remove a remote file or directory.
    SftpRemove { path: String, is_dir: bool },
    /// Rename / move a remote path.
    SftpRename { from: String, to: String },
    /// Sample remote resource usage (CPU/mem/disk/net) over the live connection.
    SampleMetrics,
    /// Sample the remote process list (for the monitor's CPU/Memory drill-down).
    SampleProcesses,
    /// Sample listening TCP/UDP ports via `ss`.
    SamplePorts,
    /// Change permissions of a remote file.
    SftpChmod { path: String, mode: u32 },
    /// Read a byte range of a remote file for the file viewer.
    ReadFileRange { path: String, offset: u64, len: u64 },
    /// Write (overwrite) a remote file from the editor.
    WriteFile { path: String, data: Vec<u8> },
    /// Close the shell and disconnect.
    Disconnect,
    /// Execute a one-shot command on the remote (non-interactive) and return
    /// its stdout for the smart-suggestion engine. `tag` identifies which
    /// strategy the result belongs to (e.g. "kill", "cd", "__files__").
    ExecQuery { command: String, tag: String },
}

/// Events the worker streams back to the UI. Every variant carries the
/// `session_id` so the dispatcher can route it to the right session even when
/// it is not the active tab.
#[derive(Debug, Clone)]
pub enum Event {
    /// First event: hands the UI the channel used to send [`Command`]s.
    Ready {
        session_id: u64,
        sender: mpsc::Sender<Command>,
    },
    Connecting {
        session_id: u64,
    },
    Connected {
        session_id: u64,
    },
    Output {
        session_id: u64,
        bytes: Vec<u8>,
    },
    /// The server's host key is not yet trusted; the UI must confirm it.
    HostKeyRequired {
        session_id: u64,
        challenge: Box<HostKeyChallenge>,
    },
    SftpListed {
        session_id: u64,
        path: String,
        result: Result<Vec<RemoteFileEntry>, String>,
    },
    SftpDone {
        session_id: u64,
        message: Result<String, String>,
    },
    /// A streamed transfer has started.
    TransferStarted {
        session_id: u64,
        id: u64,
        name: String,
        direction: Direction,
        total: u64,
        /// Resolved remote path (for rebuilding a resume command).
        remote: String,
        /// Resolved local path (for rebuilding a resume command).
        local: String,
        /// Whether this transfer is a whole directory tree.
        is_dir: bool,
    },
    /// Progress update for a streamed transfer (throttled).
    TransferProgress {
        session_id: u64,
        id: u64,
        transferred: u64,
        speed_bps: f64,
    },
    /// A streamed transfer finished (ok = bytes transferred, err = message).
    TransferFinished {
        session_id: u64,
        id: u64,
        result: Result<u64, String>,
    },
    /// A transfer was paused at a safe point (its `.part` is preserved).
    TransferPaused {
        session_id: u64,
        id: u64,
        transferred: u64,
    },
    /// A transfer was cancelled and its `.part` scratch removed.
    TransferCanceled {
        session_id: u64,
        id: u64,
    },
    /// Raw stdout of one resource-monitor sample, parsed by the UI side.
    Metrics {
        session_id: u64,
        raw: String,
    },
    /// Raw stdout of one `ps` process sample, parsed by the UI side.
    Processes {
        session_id: u64,
        raw: String,
    },
    /// Raw stdout of `ss` port sample, parsed by the UI side.
    Ports {
        session_id: u64,
        raw: String,
    },
    /// A chunk of a remote file for the file viewer (offset, data, file total size).
    FileChunk {
        session_id: u64,
        path: String,
        offset: u64,
        data: Vec<u8>,
        total: u64,
    },
    /// Result of writing a remote file from the editor.
    FileSaved {
        session_id: u64,
        result: Result<(), String>,
    },
    Exit {
        session_id: u64,
        code: u32,
    },
    Closed {
        session_id: u64,
    },
    Failed {
        session_id: u64,
        error: String,
        /// True for failures another attempt cannot fix (bad credentials,
        /// missing username). Auto-reconnect must stop on these — retrying a
        /// wrong password forever just locks the account.
        fatal: bool,
    },
    /// Result of a smart-suggestion query (stdout parsed into candidates).
    SuggestionData {
        session_id: u64,
        tag: String,
        candidates: Vec<String>,
    },
}

impl Event {
    pub fn session_id(&self) -> u64 {
        match self {
            Event::Ready { session_id, .. }
            | Event::Connecting { session_id }
            | Event::Connected { session_id }
            | Event::Output { session_id, .. }
            | Event::HostKeyRequired { session_id, .. }
            | Event::SftpListed { session_id, .. }
            | Event::SftpDone { session_id, .. }
            | Event::TransferStarted { session_id, .. }
            | Event::TransferProgress { session_id, .. }
            | Event::TransferFinished { session_id, .. }
            | Event::TransferPaused { session_id, .. }
            | Event::TransferCanceled { session_id, .. }
            | Event::Metrics { session_id, .. }
            | Event::Processes { session_id, .. }
            | Event::Ports { session_id, .. }
            | Event::FileChunk { session_id, .. }
            | Event::FileSaved { session_id, .. }
            | Event::Exit { session_id, .. }
            | Event::Closed { session_id }
            | Event::Failed { session_id, .. }
            | Event::SuggestionData { session_id, .. } => *session_id,
        }
    }
}

/// The subscription worker for one session. Lives as long as the session
/// exists; reconnects are handled in-place by re-sending [`Command::Connect`].
pub fn worker(session_id: u64) -> impl Stream<Item = Event> {
    iced::stream::channel(256, move |mut out: OutSink| async move {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(256);

        // Hand the command channel to the UI.
        if out
            .send(Event::Ready {
                session_id,
                sender: cmd_tx,
            })
            .await
            .is_err()
        {
            return;
        }

        // Outer loop: wait for a Connect, run a shell, then wait again for a
        // reconnect. Exits only when the command channel is dropped (the
        // session was closed) or a Disconnect arrives before connecting.
        loop {
            // Drain commands until a Connect (ignore stray writes pre-connect).
            let params = loop {
                match cmd_rx.recv().await {
                    Some(Command::Connect(params)) => break params,
                    Some(Command::Disconnect) | None => return,
                    _ => continue,
                }
            };

            if run_connection(session_id, &mut out, &mut cmd_rx, params)
                .await
                .is_break()
            {
                return;
            }
        }
    })
}

use std::ops::ControlFlow;

/// Run a single connection lifecycle: connect, pump the shell + handle SFTP,
/// until the shell closes or a Disconnect arrives. Returns `Break` if the whole
/// worker should terminate (UI dropped the channel).
async fn run_connection(
    session_id: u64,
    out: &mut iced::futures::channel::mpsc::Sender<Event>,
    cmd_rx: &mut mpsc::Receiver<Command>,
    params: ConnectParams,
) -> ControlFlow<()> {
    let _ = out.send(Event::Connecting { session_id }).await;

    // Primary connection + data-connection pool for bulk transfers.
    let conn_config = ConnectionConfig::default();
    let session_conn = if conn_config.enable_pool {
        match SessionConnection::new_pooled(params.route.clone(), conn_config.pool_config).await {
            Ok(conn) => conn,
            Err(SshError::HostKeyVerificationRequired(challenge)) => {
                let _ = out
                    .send(Event::HostKeyRequired {
                        session_id,
                        challenge,
                    })
                    .await;
                return ControlFlow::Continue(());
            }
            Err(error) => {
                let fatal = matches!(error, SshError::Authentication | SshError::MissingUsername);
                let _ = out
                    .send(Event::Failed {
                        session_id,
                        error: error.to_string(),
                        fatal,
                    })
                    .await;
                return ControlFlow::Continue(());
            }
        }
    } else {
        match SessionConnection::new_legacy(params.route.clone()).await {
            Ok(conn) => conn,
            Err(SshError::HostKeyVerificationRequired(challenge)) => {
                let _ = out
                    .send(Event::HostKeyRequired {
                        session_id,
                        challenge,
                    })
                    .await;
                return ControlFlow::Continue(());
            }
            Err(error) => {
                let fatal = matches!(error, SshError::Authentication | SshError::MissingUsername);
                let _ = out
                    .send(Event::Failed {
                        session_id,
                        error: error.to_string(),
                        fatal,
                    })
                    .await;
                return ControlFlow::Continue(());
            }
        }
    };

    let session_conn = Arc::new(session_conn);

    let _ = out.send(Event::Connected { session_id }).await;

    // Terminal 使用主连接
    let shell_session = session_conn.terminal_session();

    // Spawn the shell pump on its own task so SFTP work never stalls it.
    let (pty_in_tx, mut pty_in_rx) = mpsc::channel::<PtyInput>(256);
    let (pty_ev_tx, mut pty_ev_rx) = mpsc::channel::<PtyEvent>(256);
    let shell_opts = ShellOptions {
        term: params.term.clone(),
        size: PtySize {
            cols: params.cols,
            rows: params.rows,
        },
    };
    let shell_task = tokio::spawn(async move {
        shell_session
            .event_shell(shell_opts, &mut pty_in_rx, pty_ev_tx)
            .await
    });
    // #16: track all fire-and-forget tasks so they're aborted on disconnect.
    let mut bg_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // Per-transfer control, keyed by transfer id, so pause/cancel can act on an
    // in-flight transfer — or, once the transfer task has exited (e.g. after a
    // pause), so a later cancel can still clean up the `.part` scratch itself.
    let mut transfers: std::collections::HashMap<u64, TransferCtl> =
        std::collections::HashMap::new();
    // #5: accumulate output bytes and flush at ~60fps to reduce per-byte overhead.
    let mut flush_tick = tokio::time::interval(OUTPUT_FRAME_TICK);
    flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut output = OutputCoalescer::new();

    // Main multiplexing loop.
    let outcome = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Write(bytes)) => {
                    // Non-blocking: drop keystrokes rather than stalling the select loop
                    // under SSH back-pressure.
                    let _ = pty_in_tx.try_send(PtyInput::Write(bytes));
                }
                Some(Command::Resize { cols, rows }) => {
                    let _ = pty_in_tx.try_send(PtyInput::Resize(PtySize { cols, rows }));
                }
                Some(Command::Connect(_)) => {
                    // Already connected; ignore duplicate connects.
                }
                Some(Command::SftpList(path)) => {
                    // 快速 SFTP 操作使用主连接
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_sftp_list(session_id, session, out.clone(), path));
                }
                Some(Command::SftpDownload { id, name, remote, local, size, is_dir }) => {
                    let mode = Arc::new(std::sync::atomic::AtomicU8::new(0));
                    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
                    let finalized = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    transfers.insert(id, TransferCtl {
                        mode: mode.clone(),
                        running: running.clone(),
                        finalized: finalized.clone(),
                        remote: remote.clone(),
                        local: local.clone(),
                        direction: Direction::Download,
                    });
                    // Bulk transfers run on the pool's data connection, never on
                    // the terminal's.
                    bg_tasks.push(spawn_transfer(
                        session_id,
                        session_conn.clone(),
                        out.clone(),
                        Transfer { id, name, direction: Direction::Download, remote, local, size, is_dir },
                        mode,
                        running,
                        finalized,
                    ));
                }
                Some(Command::SftpUpload { id, name, local, remote, size, is_dir }) => {
                    let mode = Arc::new(std::sync::atomic::AtomicU8::new(0));
                    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
                    let finalized = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    transfers.insert(id, TransferCtl {
                        mode: mode.clone(),
                        running: running.clone(),
                        finalized: finalized.clone(),
                        remote: remote.clone(),
                        local: local.clone(),
                        direction: Direction::Upload,
                    });
                    // Bulk transfers run on the pool's data connection, never on
                    // the terminal's.
                    bg_tasks.push(spawn_transfer(
                        session_id,
                        session_conn.clone(),
                        out.clone(),
                        Transfer { id, name, direction: Direction::Upload, remote, local, size, is_dir },
                        mode,
                        running,
                        finalized,
                    ));
                }
                Some(Command::SftpPauseTransfer { id }) => {
                    // Cooperative pause: mode=1 → the running task stops at its
                    // next safe point and keeps the `.part` for a later resume.
                    if let Some(ctl) = transfers.get(&id) {
                        ctl.mode.store(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                Some(Command::SftpCancelTransfer { id }) => {
                    if let Some(ctl) = transfers.get(&id) {
                        ctl.mode.store(2, std::sync::atomic::Ordering::SeqCst);
                        // If the task is still running it will observe mode=2 and
                        // handle its own cleanup on exit — we must NOT claim
                        // `finalized` here or the task would lose the claim and
                        // skip cleanup. Only take over when the task has already
                        // exited (e.g. a paused transfer, whose task is gone).
                        let task_done = !ctl.running.load(std::sync::atomic::Ordering::SeqCst);
                        if task_done
                            && ctl
                                .finalized
                                .compare_exchange(
                                    false,
                                    true,
                                    std::sync::atomic::Ordering::SeqCst,
                                    std::sync::atomic::Ordering::SeqCst,
                                )
                                .is_ok()
                        {
                            let files = vec![(
                                ctl.remote.clone(),
                                std::path::PathBuf::from(&ctl.local),
                                0u64,
                            )];
                            let sess = session_conn.quick_sftp_session();
                            let mut o = out.clone();
                            let dir = ctl.direction;
                            bg_tasks.push(tokio::spawn(async move {
                                cleanup_part_files(&sess, &files, dir).await;
                                let _ = o.send(Event::TransferCanceled { session_id, id }).await;
                            }));
                        }
                    }
                }
                Some(Command::SftpMkdir(path)) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_sftp_simple(session_id, session, out.clone(),
                        SftpOp::Mkdir(path)));
                }
                Some(Command::SftpRemove { path, is_dir }) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_sftp_simple(session_id, session, out.clone(),
                        SftpOp::Remove { path, is_dir }));
                }
                Some(Command::SftpRename { from, to }) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_sftp_simple(session_id, session, out.clone(),
                        SftpOp::Rename { from, to }));
                }
                Some(Command::SftpChmod { path, mode }) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_sftp_simple(session_id, session, out.clone(),
                        SftpOp::Chmod { path, mode }));
                }
                Some(Command::SampleMetrics) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_metrics(session_id, session, out.clone()));
                }
                Some(Command::SampleProcesses) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_processes(session_id, session, out.clone()));
                }
                Some(Command::SamplePorts) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_ports(session_id, session, out.clone()));
                }
                Some(Command::ReadFileRange { path, offset, len }) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_read_file(session_id, session, out.clone(), path, offset, len));
                }
                Some(Command::WriteFile { path, data }) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_write_file(session_id, session, out.clone(), path, data));
                }
                Some(Command::Disconnect) => break ShellOutcome::Disconnected,
                Some(Command::ExecQuery { command, tag }) => {
                    let session = session_conn.quick_sftp_session();
                    bg_tasks.push(spawn_suggestion_query(
                        session_id, session, out.clone(), command, tag,
                    ));
                }
                None => break ShellOutcome::WorkerDropped,
            },
            ev = pty_ev_rx.recv() => match ev {
                Some(PtyEvent::Output(bytes)) => {
                    output.push(&bytes);
                    if output.len() >= OUTPUT_BATCH_CAP {
                        let bytes = output.flush();
                        if out.try_send(Event::Output { session_id, bytes })
                            .is_err_and(|e| e.is_disconnected())
                        {
                            break ShellOutcome::WorkerDropped;
                        }
                    }
                }
                Some(PtyEvent::ExitStatus(code)) => {
                    // Flush buffered bytes before the exit event so order is preserved.
                    let bytes = output.flush();
                    if !bytes.is_empty() {
                        let _ = out.try_send(Event::Output { session_id, bytes });
                    }
                    let _ = out.send(Event::Exit { session_id, code }).await;
                }
                Some(PtyEvent::Closed) | None => {
                    let bytes = output.flush();
                    if !bytes.is_empty() {
                        let _ = out.try_send(Event::Output { session_id, bytes });
                    }
                    break ShellOutcome::Closed;
                }
            },
            _ = async {
                match output.deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let bytes = output.flush();
                if !bytes.is_empty()
                    && out
                        .try_send(Event::Output { session_id, bytes })
                        .is_err_and(|e| e.is_disconnected())
                {
                    break ShellOutcome::WorkerDropped;
                }
            }
            _ = flush_tick.tick() => {
                let bytes = output.flush();
                if !bytes.is_empty() {
                    match out.try_send(Event::Output { session_id, bytes }) {
                        Ok(()) => {}
                        Err(e) if e.is_disconnected() => break ShellOutcome::WorkerDropped,
                        Err(_) => {}
                    }
                }
                // Prune finished background tasks to avoid unbounded memory growth.
                // Over a long session with many SFTP ops, bg_tasks could accumulate
                // thousands of completed JoinHandles (~72 bytes each) if not cleaned.
                bg_tasks.retain(|h| !h.is_finished());
            }
        }
    };

    // Tear down this connection. Abort the background work first so no task is
    // still using a borrowed data connection, then close explicitly: dropping
    // the pool would only close sockets, leaving the server to log an aborted
    // connection per tab instead of an SSH disconnect.
    shell_task.abort();
    for h in bg_tasks {
        h.abort();
    }
    session_conn.shutdown().await;
    drop(session_conn);

    match outcome {
        ShellOutcome::WorkerDropped => ControlFlow::Break(()),
        ShellOutcome::Disconnected | ShellOutcome::Closed => {
            let _ = out.send(Event::Closed { session_id }).await;
            ControlFlow::Continue(())
        }
    }
}

enum ShellOutcome {
    /// Shell channel closed (remote exit, EOF, or error).
    Closed,
    /// User requested disconnect.
    Disconnected,
    /// UI dropped the command channel (session closed) — stop the worker.
    WorkerDropped,
}

enum SftpOp {
    Mkdir(String),
    Remove { path: String, is_dir: bool },
    Rename { from: String, to: String },
    Chmod { path: String, mode: u32 },
}

type OutSink = iced::futures::channel::mpsc::Sender<Event>;

fn spawn_sftp_list(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
    path: String,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        // Resolve + list over a single SFTP channel. `list_dir_resolved` only
        // canonicalizes relative paths (e.g. "." on connect), so ordinary
        // navigation into absolute paths pays for just one channel + one
        // round-trip instead of a canonicalize channel plus a list channel.
        let (path, result) = match session.list_dir_resolved(&path).await {
            Ok((resolved, entries)) => (resolved, Ok(entries)),
            Err(e) => (path, Err(e.to_string())),
        };
        let _ = out
            .send(Event::SftpListed {
                session_id,
                path,
                result,
            })
            .await;
    });
}

struct Transfer {
    id: u64,
    name: String,
    direction: Direction,
    remote: String,
    local: String,
    /// Known size from the caller (0 = unknown).
    size: u64,
    /// True when this transfer is a whole directory tree.
    is_dir: bool,
}

/// Actor-side control handle for one transfer. `mode` is the cooperative stop
/// token shared with the streaming task (0 = run, 1 = pause, 2 = cancel).
/// `running` is set false by the task as it exits, so a cancel arriving after
/// the task is gone (e.g. on a paused transfer) can still remove the `.part`.
struct TransferCtl {
    mode: Arc<std::sync::atomic::AtomicU8>,
    running: Arc<std::sync::atomic::AtomicBool>,
    /// Claimed (false→true) by whichever of the task or the worker handles the
    /// terminal cancel, so the cleanup + `TransferCanceled` fire exactly once
    /// even if both race to it.
    finalized: Arc<std::sync::atomic::AtomicBool>,
    remote: String,
    local: String,
    direction: Direction,
}

/// Run a streamed transfer (a single file or a whole directory tree), emitting
/// Started → throttled Progress (≈12/s, with instantaneous speed) → Finished
/// (or Paused / Canceled if `stop` is signalled — 1 = pause, 2 = cancel).
/// A directory is walked first to build a flat file list and a true total, then
/// every file streams with progress accumulated across the whole tree, so a
/// folder appears as one transfer with one aggregate progress bar.
fn spawn_transfer(
    session_id: u64,
    conn: Arc<SessionConnection>,
    mut out: OutSink,
    t: Transfer,
    stop: Arc<std::sync::atomic::AtomicU8>,
    running: Arc<std::sync::atomic::AtomicBool>,
    finalized: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        // Borrow a dedicated connection for this transfer. Done here rather than
        // in the worker's select loop: dialling can take seconds (handshake,
        // auth) and would otherwise stall the terminal's own I/O. The connection
        // is returned to the pool when `transfer` drops at the end of this task,
        // and because it is not the terminal's connection, a transfer that dies
        // cannot take the interactive session with it.
        let transfer = match conn.acquire_transfer_connection().await {
            Ok(transfer) => transfer,
            Err(error) => {
                let _ = out
                    .send(Event::TransferStarted {
                        session_id,
                        id: t.id,
                        name: t.name.clone(),
                        direction: t.direction,
                        total: 0,
                        remote: t.remote.clone(),
                        local: t.local.clone(),
                        is_dir: t.is_dir,
                    })
                    .await;
                let _ = out
                    .send(Event::TransferFinished {
                        session_id,
                        id: t.id,
                        result: Err(error.to_string()),
                    })
                    .await;
                running.store(false, std::sync::atomic::Ordering::SeqCst);
                return;
            }
        };
        let session = transfer.session().clone();

        // Build the work list of (remote, local, size) and the overall total.
        // A directory is expanded into its files (creating the destination
        // directory skeleton as a side effect); a single file is its own list.
        let (files, total): (Vec<(String, std::path::PathBuf, u64)>, u64) = if t.is_dir {
            let walked = match t.direction {
                Direction::Download => {
                    collect_remote_tree(&session, &t.remote, std::path::PathBuf::from(&t.local))
                        .await
                }
                Direction::Upload => {
                    collect_local_tree(&session, std::path::Path::new(&t.local), &t.remote).await
                }
            };
            match walked {
                Ok(list) => {
                    let total = list.iter().map(|(_, _, s)| *s).sum();
                    (list, total)
                }
                Err(e) => {
                    // Surface the failure as a started-then-failed transfer row.
                    let _ = out
                        .send(Event::TransferStarted {
                            session_id,
                            id: t.id,
                            name: t.name.clone(),
                            direction: t.direction,
                            total: 0,
                            remote: t.remote.clone(),
                            local: t.local.clone(),
                            is_dir: t.is_dir,
                        })
                        .await;
                    let _ = out
                        .send(Event::TransferFinished {
                            session_id,
                            id: t.id,
                            result: Err(e.to_string()),
                        })
                        .await;
                    return;
                }
            }
        } else {
            // Prefer the size the caller already knew (from the listing / local
            // metadata); only query when it wasn't supplied, so the progress bar
            // has a correct total from the first frame.
            let total = if t.size > 0 {
                t.size
            } else {
                match t.direction {
                    Direction::Download => session.remote_file_size(&t.remote).await.unwrap_or(0),
                    Direction::Upload => tokio::fs::metadata(&t.local)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0),
                }
            };
            (
                vec![(t.remote.clone(), std::path::PathBuf::from(&t.local), total)],
                total,
            )
        };

        let _ = out
            .send(Event::TransferStarted {
                session_id,
                id: t.id,
                name: t.name.clone(),
                direction: t.direction,
                total,
                remote: t.remote.clone(),
                local: t.local.clone(),
                is_dir: t.is_dir,
            })
            .await;

        // Raw byte counts from the streaming methods.
        let (ptx, mut prx) = mpsc::channel::<u64>(256);

        // Forwarder: throttle to ~12 Hz and compute EMA-smoothed speed.
        let mut progress_out = out.clone();
        let id = t.id;
        let forwarder = tokio::spawn(async move {
            let mut last_emit = tokio::time::Instant::now();
            let mut last_bytes = 0_u64;
            let mut ema_speed: f64 = 0.0;
            while let Some(transferred) = prx.recv().await {
                let now = tokio::time::Instant::now();
                let dt = now.duration_since(last_emit).as_secs_f64();
                if dt >= 0.08 {
                    let instant = if dt > 0.0 {
                        transferred.saturating_sub(last_bytes) as f64 / dt
                    } else {
                        0.0
                    };
                    // EMA α=0.25: smooth enough to stop flickering, fast enough to track real changes.
                    ema_speed = if ema_speed == 0.0 {
                        instant
                    } else {
                        0.25 * instant + 0.75 * ema_speed
                    };
                    let _ = progress_out
                        .send(Event::TransferProgress {
                            session_id,
                            id,
                            transferred,
                            speed_bps: ema_speed,
                        })
                        .await;
                    last_emit = now;
                    last_bytes = transferred;
                }
            }
        });

        // Stream every file, accumulating bytes across the whole tree so the
        // one progress bar advances continuously over a folder.
        let result = transfer_files(&session, t.direction, &files, ptx, stop.clone()).await;
        // ptx was consumed by transfer_files → throttle forwarder ends.
        let _ = forwarder.await;

        // Mark the task as no longer running BEFORE re-reading the mode, so a
        // cancel that arrives concurrently is guaranteed to be handled by
        // exactly one side (see the worker's SftpCancelTransfer arm).
        running.store(false, std::sync::atomic::Ordering::SeqCst);

        // If a stop was requested, emit Paused/Canceled instead of Finished.
        let mode = stop.load(std::sync::atomic::Ordering::SeqCst);
        if mode == 1 {
            // Pause is NOT terminal: do not claim `finalized`, so a later cancel
            // on the paused row can still take over and clean up.
            let transferred = result.unwrap_or(0);
            let _ = out
                .send(Event::TransferPaused {
                    session_id,
                    id: t.id,
                    transferred,
                })
                .await;
            return;
        }

        // Terminal (finish or cancel): claim `finalized` so exactly one side
        // emits a terminal event. If we lose the claim, a concurrent cancel in
        // the worker already took over — stay silent.
        let won = finalized
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok();
        if !won {
            return;
        }
        if mode == 2 {
            cleanup_part_files(&session, &files, t.direction).await;
            let _ = out
                .send(Event::TransferCanceled {
                    session_id,
                    id: t.id,
                })
                .await;
            return;
        }

        let _ = out
            .send(Event::TransferFinished {
                session_id,
                id: t.id,
                result: result.map_err(|e| e.to_string()),
            })
            .await;
    });
}

/// Remove the `.part` scratch file(s) for a cancelled transfer. For a single
/// file this is exactly one path; for a directory tree the current in-flight
/// file's `.part` is the one that matters (completed files were already
/// promoted, un-started ones never created a `.part`), but we sweep every
/// candidate best-effort since a stale `.part` is harmless.
async fn cleanup_part_files(
    session: &RusshSession,
    files: &[(String, std::path::PathBuf, u64)],
    direction: Direction,
) {
    for (remote, local, _sz) in files {
        match direction {
            Direction::Download => {
                let mut name = local.file_name().unwrap_or_default().to_os_string();
                name.push(".part");
                let part = local.with_file_name(name);
                let _ = tokio::fs::remove_file(&part).await;
            }
            Direction::Upload => {
                let part = format!("{remote}.part");
                let _ = session.remove_path(&part, RemoteFileKind::File).await;
            }
        }
    }
}

/// Stream every file in `files` (already-resolved `(remote, local, size)`),
/// reporting cumulative bytes across the whole list over `progress`, and return
/// the total bytes transferred. Each file's own 0..size progress is offset by
/// the bytes already done so the combined stream is monotonic. The first
/// failure aborts the rest, matching single-file semantics. A nonzero `stop`
/// token stops the per-file streaming at a safe point (the ssh method leaves a
/// resumable `.part`) and prevents starting the next file.
async fn transfer_files(
    session: &RusshSession,
    direction: Direction,
    files: &[(String, std::path::PathBuf, u64)],
    progress: mpsc::Sender<u64>,
    stop: Arc<std::sync::atomic::AtomicU8>,
) -> Result<u64, SshError> {
    let mut base = 0_u64;
    for (remote, local, _sz) in files {
        if stop.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            break; // don't begin another file once a stop is requested
        }
        let (fptx, mut fprx) = mpsc::channel::<u64>(256);
        let agg = progress.clone();
        let base_now = base;
        let fwd = tokio::spawn(async move {
            while let Some(n) = fprx.recv().await {
                let _ = agg.send(base_now + n).await;
            }
        });
        let one = match direction {
            Direction::Download => {
                session
                    .download_file(remote, local, fptx, stop.clone())
                    .await
            }
            Direction::Upload => session.upload_file(local, remote, fptx, stop.clone()).await,
        };
        // fptx dropped → inner forwarder ends.
        let _ = fwd.await;
        base += one?;
    }
    Ok(base)
}

/// Walk a remote directory tree, creating the mirroring local directories as it
/// descends, and return a flat list of every file as (remote, local, size).
fn collect_remote_tree<'a>(
    session: &'a RusshSession,
    remote_dir: &'a str,
    local_dir: std::path::PathBuf,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<Vec<(String, std::path::PathBuf, u64)>, SshError>>
            + Send
            + 'a,
    >,
> {
    Box::pin(async move {
        tokio::fs::create_dir_all(&local_dir).await?;
        let mut files = Vec::new();
        for entry in session.list_dir(remote_dir).await? {
            let child_remote = join_remote(remote_dir, &entry.name);
            let child_local = local_dir.join(&entry.name);
            match entry.kind {
                RemoteFileKind::Directory => {
                    let mut sub = collect_remote_tree(session, &child_remote, child_local).await?;
                    files.append(&mut sub);
                }
                // Symlinks/other are taken as files; directory symlinks are not
                // followed (their kind isn't Directory), avoiding cycles.
                _ => files.push((child_remote, child_local, entry.size.unwrap_or(0))),
            }
        }
        Ok(files)
    })
}

/// Walk a local directory tree, creating the mirroring remote directories as it
/// descends, and return a flat list of every file as (remote, local, size).
fn collect_local_tree<'a>(
    session: &'a RusshSession,
    local_dir: &'a std::path::Path,
    remote_dir: &'a str,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<Vec<(String, std::path::PathBuf, u64)>, SshError>>
            + Send
            + 'a,
    >,
> {
    Box::pin(async move {
        // SFTP has no "already exists" status code — servers return generic
        // Failure(4) for an existing dir, so we can't distinguish it from a
        // real error here. Silently ignore the result: if the dir truly can't
        // be created, the subsequent file uploads will fail with a clear error.
        // Channel exhaustion (the historic cause of silent failures here) is now
        // prevented by the connection-level `channel_sem` semaphore.
        let _ = session.create_dir(remote_dir).await;
        let mut files = Vec::new();
        let mut rd = tokio::fs::read_dir(local_dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let meta = entry.metadata().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            let child_remote = join_remote(remote_dir, &name);
            let child_local = entry.path();
            if meta.is_dir() {
                let mut sub = collect_local_tree(session, &child_local, &child_remote).await?;
                files.append(&mut sub);
            } else {
                files.push((child_remote, child_local, meta.len()));
            }
        }
        Ok(files)
    })
}

/// Join a remote base path and a child name (POSIX semantics).
fn join_remote(base: &str, name: &str) -> String {
    if base == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", base.trim_end_matches('/'))
    }
}

fn spawn_sftp_simple(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
    op: SftpOp,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        let message = match op {
            SftpOp::Mkdir(path) => session
                .create_dir(&path)
                .await
                .map(|_| format!("Created {path}"))
                .map_err(|e| e.to_string()),
            SftpOp::Remove { path, is_dir } => {
                let kind = if is_dir {
                    RemoteFileKind::Directory
                } else {
                    RemoteFileKind::File
                };
                session
                    .remove_path(&path, kind)
                    .await
                    .map(|_| format!("Removed {path}"))
                    .map_err(|e| e.to_string())
            }
            SftpOp::Rename { from, to } => session
                .rename_path(&from, &to)
                .await
                .map(|_| format!("Renamed {from} -> {to}"))
                .map_err(|e| e.to_string()),
            SftpOp::Chmod { path, mode } => session
                .chmod_path(&path, mode)
                .await
                .map(|_| format!("chmod {mode:o} {path}"))
                .map_err(|e| e.to_string()),
        };
        let _ = out
            .send(Event::SftpDone {
                session_id,
                message,
            })
            .await;
    });
}

/// Sample remote resource usage in one round-trip and ship the raw stdout to
/// the UI, which parses it (see `metrics.rs`). Errors are swallowed: a failed
/// sample just means the monitor keeps its last values until the next tick.
fn spawn_metrics(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        if let Ok(output) = session.exec_capture(crate::metrics::SAMPLE_COMMAND).await {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let _ = out.send(Event::Metrics { session_id, raw }).await;
        }
    });
}

/// Sample the remote process list (`ps`) for the monitor's drill-down. Errors
/// are swallowed: a failed sample keeps the last list until the next tick.
fn spawn_processes(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        if let Ok(output) = session.exec_capture(crate::metrics::PROCESS_COMMAND).await {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let _ = out.send(Event::Processes { session_id, raw }).await;
        }
    });
}

fn spawn_read_file(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
    path: String,
    offset: u64,
    len: u64,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        match session.read_file_range(&path, offset, len).await {
            Ok((data, total)) => {
                let _ = out
                    .send(Event::FileChunk {
                        session_id,
                        path,
                        offset,
                        data,
                        total,
                    })
                    .await;
            }
            Err(e) => {
                let _ = out
                    .send(Event::FileChunk {
                        session_id,
                        path,
                        offset,
                        data: Vec::new(),
                        total: 0,
                    })
                    .await;
                let _ = out
                    .send(Event::FileSaved {
                        session_id,
                        result: Err(e.to_string()),
                    })
                    .await;
            }
        }
    });
}

fn spawn_write_file(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
    path: String,
    data: Vec<u8>,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        let result = session
            .write_file(&path, data)
            .await
            .map_err(|e| e.to_string());
        let _ = out.send(Event::FileSaved { session_id, result }).await;
    });
}

fn spawn_ports(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        if let Ok(output) = session.exec_capture(crate::metrics::PORT_COMMAND).await {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let _ = out.send(Event::Ports { session_id, raw }).await;
        }
    });
}

/// Execute a one-shot command for the smart-suggestion engine and stream the
/// parsed candidates back. Errors are swallowed (empty candidate list sent back
/// so the UI can clear its pending state and fallback to history).
fn spawn_suggestion_query(
    session_id: u64,
    session: Arc<RusshSession>,
    mut out: OutSink,
    command: String,
    tag: String,
) -> tokio::task::JoinHandle<()> {
    return tokio::spawn(async move {
        let candidates = match session.exec_capture(&command).await {
            Ok(output) => {
                let raw = String::from_utf8_lossy(&output.stdout).into_owned();
                crate::session::parse_query_output(&tag, &raw)
            }
            Err(_) => Vec::new(),
        };
        let _ = out
            .send(Event::SuggestionData {
                session_id,
                tag,
                candidates,
            })
            .await;
    });
}

/// Subscription worker for a local PTY shell (macOS/Linux).
/// Speaks the same `Event` protocol as the SSH `worker` so the rest of the
/// app needs no special-casing beyond picking which worker to run.
#[cfg(unix)]
pub fn local_worker(session_id: u64) -> impl iced::futures::Stream<Item = Event> {
    iced::stream::channel(256, move |mut out: OutSink| async move {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(256);
        if out
            .send(Event::Ready {
                session_id,
                sender: cmd_tx,
            })
            .await
            .is_err()
        {
            return;
        }

        // Wait for the first Connect to learn the initial cols/rows.
        let params = loop {
            match cmd_rx.recv().await {
                Some(Command::Connect(p)) => break p,
                Some(Command::Disconnect) | None => return,
                _ => continue,
            }
        };
        let _ = out.send(Event::Connecting { session_id }).await;

        // Open a PTY pair and spawn $SHELL.
        let (master_fd, mut child) = match spawn_pty_shell(params.cols, params.rows) {
            Ok(x) => x,
            Err(e) => {
                let _ = out
                    .send(Event::Failed {
                        session_id,
                        error: e.to_string(),
                        fatal: true,
                    })
                    .await;
                return;
            }
        };
        let _ = out.send(Event::Connected { session_id }).await;

        // Bridge the blocking PTY read into async via a dedicated thread.
        //
        // The reader thread OWNS the fd from here on: on macOS, close() on a
        // PTY master blocks while another thread sits inside read() on it
        // (the teardown path used to close it here, deadlocking exactly when
        // the shell was still echoing — i.e. right after heavy output). The
        // reaper's SIGHUP/SIGKILL makes the shell release the slave, read()
        // then returns <= 0, and the thread closes the master itself.
        let (pty_tx, mut pty_rx) = mpsc::channel::<Vec<u8>>(256);
        let read_fd = master_fd;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe {
                    libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };
                if n <= 0 {
                    break;
                }
                if pty_tx.blocking_send(buf[..n as usize].to_vec()).is_err() {
                    break;
                }
            }
            unsafe {
                libc::close(read_fd);
            }
        });

        // Same output batching as the SSH worker: interactive echoes flush on
        // quiescence, continuous streams merge into frame-sized messages.
        let mut output = OutputCoalescer::new();
        let mut flush_tick = tokio::time::interval(OUTPUT_FRAME_TICK);
        flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(Command::Write(bytes)) => {
                        // Write the full buffer: a PTY can take a partial
                        // write under load (large pastes), and silently
                        // dropping the tail would corrupt the shell's input.
                        let mut off = 0;
                        while off < bytes.len() {
                            let n = unsafe {
                                libc::write(
                                    master_fd,
                                    bytes[off..].as_ptr() as *const libc::c_void,
                                    bytes.len() - off,
                                )
                            };
                            if n < 0 {
                                if std::io::Error::last_os_error().kind()
                                    == std::io::ErrorKind::Interrupted
                                {
                                    continue;
                                }
                                break;
                            }
                            off += n as usize;
                        }
                    }
                    Some(Command::Resize { cols, rows }) => {
                        let ws = libc::winsize { ws_col: cols, ws_row: rows, ws_xpixel: 0, ws_ypixel: 0 };
                        unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws); }
                    }
                    Some(Command::Disconnect) | None => break,
                    Some(Command::ExecQuery { command, tag }) => {
                        let mut out = out.clone();
                        tokio::task::spawn_blocking(move || {
                            let candidates = std::process::Command::new("sh")
                                .arg("-c")
                                .arg(&command)
                                .output()
                                .ok()
                                .map(|o| {
                                    let raw = String::from_utf8_lossy(&o.stdout).into_owned();
                                    crate::session::parse_query_output(&tag, &raw)
                                })
                                .unwrap_or_default();
                            let _ = out.try_send(Event::SuggestionData {
                                session_id,
                                tag,
                                candidates,
                            });
                        });
                    }
                    _ => {}
                },
                bytes = pty_rx.recv() => match bytes {
                    Some(b) => {
                        output.push(&b);
                        if output.len() >= OUTPUT_BATCH_CAP {
                            let bytes = output.flush();
                            if out.send(Event::Output { session_id, bytes }).await.is_err() {
                                break;
                            }
                        }
                    }
                    None => {
                        let bytes = output.flush();
                        if !bytes.is_empty() {
                            let _ = out.send(Event::Output { session_id, bytes }).await;
                        }
                        break;
                    }
                },
                _ = async {
                    match output.deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let bytes = output.flush();
                    if !bytes.is_empty()
                        && out.send(Event::Output { session_id, bytes }).await.is_err()
                    {
                        break;
                    }
                }
                _ = flush_tick.tick() => {
                    let bytes = output.flush();
                    if !bytes.is_empty()
                        && out.send(Event::Output { session_id, bytes }).await.is_err()
                    {
                        break;
                    }
                }
            }
        }

        // The master fd is NOT closed here — the reader thread owns it (see
        // the spawn above); closing under a blocked read() deadlocks on macOS.
        // Reap the shell on a blocking thread so it never lingers as a
        // zombie: HUP first (the normal terminal hang-up), escalate to KILL
        // if it hasn't exited after a short grace period. Detached — the
        // Closed event goes out immediately.
        tokio::task::spawn_blocking(move || {
            let pid = child.id() as libc::pid_t;
            unsafe {
                libc::kill(pid, libc::SIGHUP);
            }
            for _ in 0..40 {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                    Err(_) => return,
                }
            }
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = child.wait();
        });
        let _ = out.send(Event::Closed { session_id }).await;
    })
}

/// Open a PTY pair and spawn `$SHELL`. Returns `(master_fd, child)`.
/// Uses `std::process::Command` + `pre_exec` instead of raw `fork()` so the
/// call is safe from a multi-threaded tokio runtime. The caller must reap
/// the returned `Child` on disconnect or it becomes a zombie.
#[cfg(unix)]
fn spawn_pty_shell(cols: u16, rows: u16) -> std::io::Result<(libc::c_int, std::process::Child)> {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;

    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut libc::winsize {
                ws_col: cols,
                ws_row: rows,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

    // Dup slave so each stdio slot gets its own fd (File takes ownership).
    let (s1, s2) = unsafe { (libc::dup(slave), libc::dup(slave)) };
    let stdin = unsafe { std::fs::File::from_raw_fd(slave) };
    let stdout = unsafe { std::fs::File::from_raw_fd(s1) };
    let stderr = unsafe { std::fs::File::from_raw_fd(s2) };

    let mut cmd = std::process::Command::new(&shell);
    cmd.arg("-l")
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor");

    // pre_exec runs after fork but before exec — only async-signal-safe calls here.
    // At this point stdin/stdout/stderr are already dup2'd to fd 0/1/2, so
    // we use fd 0 to set the controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0 as libc::c_int);
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    Ok((master, child))
}

#[cfg(test)]
mod tests {
    use super::*;
    // The end-to-end transfer tests below dial a real server, so they need the
    // backend entry point as well as the session type the outer module imports.
    use openterm_ssh::RusshBackend;

    /// Whether this environment can allocate a PTY at all.
    ///
    /// A sandbox that denies `openpty` (the DSH file sandbox does: EPERM) cannot
    /// run the local-shell test, and that is an environment fact rather than a
    /// product regression. Checked directly so the skip cannot mask a real
    /// failure in the worker itself.
    #[cfg(unix)]
    fn pty_available() -> Result<(), String> {
        let (mut master, mut slave) = (-1, -1);
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        Ok(())
    }

    /// (length, FNV-1a hash) — cheap end-to-end integrity check for a transfer.
    fn fingerprint_file(path: &std::path::Path) -> (u64, u64) {
        use std::io::Read;
        let mut file = std::fs::File::open(path).expect("open for fingerprint");
        let mut buf = vec![0u8; 1 << 20];
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut len = 0_u64;
        loop {
            let read = file.read(&mut buf).expect("read for fingerprint");
            if read == 0 {
                break;
            }
            len += read as u64;
            for byte in &buf[..read] {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
        (len, hash)
    }

    /// Write `len` bytes of deterministic, position-dependent content.
    fn write_probe_file(path: &std::path::Path, len: u64) {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create probe dir");
        }
        let file = std::fs::File::create(path).expect("create probe file");
        let mut writer = std::io::BufWriter::with_capacity(1 << 20, file);
        let mut block = vec![0u8; 64 * 1024];
        let mut written = 0_u64;
        let mut index = 0_u64;
        while written < len {
            for (i, byte) in block.iter_mut().enumerate() {
                *byte = ((index as usize + i) % 251) as u8;
            }
            let take = ((len - written) as usize).min(block.len());
            writer.write_all(&block[..take]).expect("write probe file");
            written += take as u64;
            index += 1;
        }
        writer.flush().expect("flush probe file");
    }

    /// End-to-end verification against a **real server with a saved host**,
    /// driving the same path the GUI does: the pooled `SessionConnection`, a
    /// dedicated transfer connection, and the real network. Nothing is typed
    /// into the test — the auth method comes from the saved host: a stored
    /// password/passphrase (vault key), a private-key file, or the agent /
    /// default key (`~/.ssh/id_ed25519`) for key-based login.
    ///
    /// ```sh
    /// cargo test -p openterm-app --bin openterm-app real_server -- --ignored --nocapture
    ///
    /// # optional overrides
    /// OPENTERM_REAL_DB=/path/to/openterm.redb   (default: the app's own database)
    /// OPENTERM_REAL_HOST=82.157.57.178          (default)
    /// OPENTERM_REAL_MB=64                       (payload size, default 48)
    /// OPENTERM_REAL_IDLE_SECS=180               (idle window to survive, default 120)
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "dials a real server; needs a saved host in a workspace database"]
    async fn real_server_transfer_through_the_pool() {
        use openterm_core::AuthRef;
        use openterm_crypto::{LocalVault, VaultConfig};
        use openterm_ssh::{AuthMethod, ConnectOptions, ConnectRoute, HostKeyPolicy, PoolConfig};
        use openterm_storage::WorkspaceStore;

        let db = std::env::var("OPENTERM_REAL_DB")
            .unwrap_or_else(|_| openterm_ui::default_db_path().display().to_string());
        let wanted =
            std::env::var("OPENTERM_REAL_HOST").unwrap_or_else(|_| "82.157.57.178".to_string());
        let megabytes: u64 = std::env::var("OPENTERM_REAL_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(48);
        let idle_secs: u64 = std::env::var("OPENTERM_REAL_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120);

        let store = WorkspaceStore::open(&db).expect("open workspace database");
        let host = store
            .list_hosts()
            .expect("list hosts")
            .into_iter()
            .find(|host| host.host == wanted || host.name == wanted)
            .unwrap_or_else(|| panic!("no saved host matching {wanted}"));
        let username = host.username.clone().expect("saved host has a username");
        // Decrypting a stored secret requires the vault to be off (the probe
        // has no master password); key-based hosts don't need the vault.
        let auth = match &host.auth {
            AuthRef::PasswordSecret(id) => {
                let settings = store.get_ui_settings().ok().flatten().unwrap_or_default();
                assert!(
                    !settings.vault_enabled,
                    "the vault is enabled, so the saved password cannot be read without the \
                     master password; unlock it in the GUI or disable the vault first"
                );
                let secret = store
                    .get_secret(*id)
                    .expect("secret lookup")
                    .expect("saved password secret");
                let vault = LocalVault::new(VaultConfig::default());
                let password = String::from_utf8(
                    vault
                        .decrypt_secret(crate::VAULT_DEFAULT_KEY, &secret)
                        .expect("decrypt saved password"),
                )
                .expect("password is utf-8");
                AuthMethod::Password(password)
            }
            AuthRef::PrivateKeyFile { path, passphrase } => {
                let passphrase = passphrase.map(|id| {
                    let settings = store.get_ui_settings().ok().flatten().unwrap_or_default();
                    assert!(
                        !settings.vault_enabled,
                        "the vault is enabled, so the saved passphrase cannot be read without \
                         the master password; unlock it in the GUI or disable the vault first"
                    );
                    let secret = store
                        .get_secret(id)
                        .expect("secret lookup")
                        .expect("saved passphrase secret");
                    let vault = LocalVault::new(VaultConfig::default());
                    String::from_utf8(
                        vault
                            .decrypt_secret(crate::VAULT_DEFAULT_KEY, &secret)
                            .expect("decrypt saved passphrase"),
                    )
                    .expect("passphrase is utf-8")
                });
                AuthMethod::PrivateKey {
                    path: std::path::PathBuf::from(path),
                    passphrase,
                }
            }
            // Agent identities or the default key (~/.ssh/id_ed25519): plain
            // key-based login, no stored secret involved.
            AuthRef::AgentOrDefault | AuthRef::ManagedPrivateKey(_) => AuthMethod::AgentOrDefault,
        };
        eprintln!("connecting to {}@{wanted} (auth from saved host)", username);

        let route = ConnectRoute {
            target: host.clone(),
            target_options: ConnectOptions {
                username: username.clone(),
                auth,
                trust_unknown_host_keys: false,
                host_key_policy: HostKeyPolicy::AcceptNew {
                    known_hosts: crate::default_known_hosts_path(),
                },
                timeout: std::time::Duration::from_secs(20),
                keepalive_interval: Some(ConnectOptions::DEFAULT_KEEPALIVE_INTERVAL),
                keepalive_max: ConnectOptions::DEFAULT_KEEPALIVE_MAX,
            },
            jump: None,
        };

        let started = std::time::Instant::now();
        let conn = SessionConnection::new_pooled(route, PoolConfig::default())
            .await
            .expect("connect through SessionConnection");
        eprintln!("connected in {:.1}s", started.elapsed().as_secs_f64());
        let primary = conn.terminal_session();

        // --- terminal on the primary connection -----------------------------
        let (in_tx, mut in_rx) = mpsc::channel::<PtyInput>(64);
        let (ev_tx, mut ev_rx) = mpsc::channel::<PtyEvent>(256);
        let shell_session = primary.clone();
        let mut shell = tokio::spawn(async move {
            shell_session
                .event_shell(
                    ShellOptions {
                        term: "xterm-256color".to_string(),
                        size: PtySize {
                            cols: 100,
                            rows: 30,
                        },
                    },
                    &mut in_rx,
                    ev_tx,
                )
                .await
        });
        let marker = format!("OPENTERM_REAL_{}", std::process::id());
        in_tx
            .send(PtyInput::Write(
                format!("echo {marker}; hostname\n").into_bytes(),
            ))
            .await
            .expect("write to shell");
        let mut output = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while !output.contains(&marker) && tokio::time::Instant::now() < deadline {
            if let Ok(Some(PtyEvent::Output(bytes))) =
                tokio::time::timeout(std::time::Duration::from_secs(3), ev_rx.recv()).await
            {
                output.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        assert!(output.contains(&marker), "shell never answered: {output:?}");
        eprintln!("terminal answered on the primary connection");

        // --- a bulk transfer uses its own connection ------------------------
        let transfer = conn
            .acquire_transfer_connection()
            .await
            .expect("acquire transfer connection");
        assert!(
            !Arc::ptr_eq(&primary, transfer.session()),
            "a bulk transfer must not run on the terminal's connection"
        );
        let tsession = transfer.session().clone();

        let dir = std::env::temp_dir().join(format!("openterm-real-{}", std::process::id()));
        let source = dir.join("real-source.bin");
        let downloaded = dir.join("real-downloaded.bin");
        write_probe_file(&source, megabytes * 1024 * 1024);
        let expected = fingerprint_file(&source);
        let remote = format!("/tmp/openterm-real-{}.bin", std::process::id());

        let (utx, mut urx) = mpsc::channel::<u64>(64);
        let up_progress = tokio::spawn(async move {
            let mut last = 0;
            while let Some(n) = urx.recv().await {
                last = n;
            }
            last
        });
        let started = std::time::Instant::now();
        let uploaded = tsession
            .upload_file(
                &source,
                &remote,
                utx,
                Arc::new(std::sync::atomic::AtomicU8::new(0)),
            )
            .await
            .unwrap_or_else(|error| panic!("upload of {megabytes} MiB failed: {error}"));
        let up_secs = started.elapsed().as_secs_f64();
        let _ = up_progress.await;
        assert_eq!(uploaded, expected.0, "uploaded byte count");
        assert_eq!(
            tsession
                .remote_file_size(&remote)
                .await
                .expect("remote size"),
            expected.0,
            "remote file size differs from the source"
        );
        eprintln!(
            "upload OK: {} bytes in {:.1}s ({:.0} KiB/s)",
            uploaded,
            up_secs,
            uploaded as f64 / 1024.0 / up_secs.max(1e-9)
        );

        let (dtx, mut drx) = mpsc::channel::<u64>(64);
        let down_progress = tokio::spawn(async move {
            let mut last = 0;
            while let Some(n) = drx.recv().await {
                last = n;
            }
            last
        });
        let started = std::time::Instant::now();
        let downloaded_bytes = tsession
            .download_file(
                &remote,
                &downloaded,
                dtx,
                Arc::new(std::sync::atomic::AtomicU8::new(0)),
            )
            .await
            .unwrap_or_else(|error| panic!("download of {megabytes} MiB failed: {error}"));
        let down_secs = started.elapsed().as_secs_f64();
        let _ = down_progress.await;
        assert_eq!(downloaded_bytes, expected.0);
        assert_eq!(
            fingerprint_file(&downloaded),
            expected,
            "downloaded bytes differ from the uploaded source"
        );
        eprintln!(
            "download OK: {} bytes in {:.1}s ({:.0} KiB/s), bytes verified",
            downloaded_bytes,
            down_secs,
            downloaded_bytes as f64 / 1024.0 / down_secs.max(1e-9)
        );

        // --- the reported symptom: an idle terminal must stay usable --------
        if idle_secs > 0 {
            eprintln!("idling {idle_secs}s to test the reported keepalive drop ...");
            in_tx
                .send(PtyInput::Write(format!("echo BEFORE_IDLE\n").into_bytes()))
                .await
                .expect("write before idle");
            let before = std::time::Instant::now();
            match tokio::time::timeout(std::time::Duration::from_secs(idle_secs), &mut shell).await
            {
                Ok(joined) => panic!(
                    "session died after {:.1}s idle (shell returned {joined:?})",
                    before.elapsed().as_secs_f64()
                ),
                Err(_) => eprintln!("still connected after {idle_secs}s idle"),
            }
            in_tx
                .send(PtyInput::Write(format!("echo AFTER_IDLE\n").into_bytes()))
                .await
                .expect("write after idle");
            let mut after = String::new();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            while !after.contains("AFTER_IDLE") && tokio::time::Instant::now() < deadline {
                if let Ok(Some(PtyEvent::Output(bytes))) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), ev_rx.recv()).await
                {
                    after.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            assert!(
                after.contains("AFTER_IDLE"),
                "shell stopped responding after idle"
            );
            eprintln!("terminal still usable after {idle_secs}s idle");
        }

        // --- the transfer connection survived too ---------------------------
        assert!(
            tsession.is_alive().await,
            "the transfer connection died during the run"
        );

        let _ = tsession.remove_path(&remote, RemoteFileKind::File).await;
        let _ = std::fs::remove_dir_all(&dir);
        let _ = conn.shutdown().await;
        shell.abort();
    }

    /// The local PTY worker must reap its shell after Disconnect — the old
    /// `std::mem::forget(child)` path left one zombie process behind per
    /// closed local tab, for the lifetime of the app.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_worker_reaps_shell_on_disconnect() {
        use iced::futures::StreamExt;

        if let Err(error) = pty_available() {
            eprintln!("skipping: this environment cannot allocate a PTY ({error})");
            return;
        }

        let mut stream = Box::pin(local_worker(7));
        let sender = match stream.next().await {
            Some(Event::Ready { sender, .. }) => sender,
            _ => panic!("expected Ready as the first worker event"),
        };
        sender
            .send(Command::Connect(ConnectParams {
                route: test_route(),
                cols: 80,
                rows: 24,
                term: "xterm-256color".to_string(),
            }))
            .await
            .unwrap();

        // Drive the stream to Connected (skipping Connecting/Output).
        loop {
            match stream.next().await {
                Some(Event::Connected { .. }) => break,
                Some(Event::Failed { error, .. }) => panic!("local shell failed: {error}"),
                Some(_) => continue,
                None => panic!("worker stream ended before Connected"),
            }
        }

        sender.send(Command::Disconnect).await.unwrap();
        loop {
            match stream.next().await {
                Some(Event::Closed { .. }) | None => break,
                Some(_) => continue,
            }
        }

        // The reaper runs on a detached blocking task: SIGHUP, up to 2 s of
        // grace, then SIGKILL + wait. Poll the process table until no zombie
        // child of this test process remains.
        let me = std::process::id().to_string();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            let out = std::process::Command::new("ps")
                .args(["-axo", "ppid=,stat="])
                .output()
                .expect("run ps");
            let zombies = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| {
                    let mut it = l.split_whitespace();
                    it.next() == Some(me.as_str()) && it.next().unwrap_or("").starts_with('Z')
                })
                .count();
            if zombies == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "shell child still a zombie {zombies} after disconnect"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// The SSH key used by the live tests (`OPENTERM_TEST_KEY`, default
    /// `~/.ssh/id_ed25519`), without checking that it exists. The local-shell
    /// tests pass a dummy route they never dial.
    fn live_key_path() -> std::path::PathBuf {
        std::env::var_os("OPENTERM_TEST_KEY")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                home.unwrap_or_default().join(".ssh").join("id_ed25519")
            })
    }

    /// [`live_key_path`] when the key file actually exists, else None (with a
    /// loud skip note) so live tests skip instead of failing offline.
    fn live_test_key() -> Option<std::path::PathBuf> {
        let path = live_key_path();
        if path.exists() {
            Some(path)
        } else {
            eprintln!(
                "skipping: no SSH key at {} (set OPENTERM_TEST_KEY)",
                path.display()
            );
            None
        }
    }

    /// Build a route to the live test server (mirrors openterm-ssh's harness).
    fn test_route() -> ConnectRoute {
        use openterm_core::HostProfile;
        use openterm_ssh::{AuthMethod, ConnectOptions, HostKeyPolicy};
        let key = live_key_path();
        let mut profile = HostProfile::new("live", "82.157.57.178");
        profile.port = 22;
        profile.username = Some("ubuntu".to_string());
        ConnectRoute {
            target: profile,
            target_options: ConnectOptions {
                username: "ubuntu".to_string(),
                auth: AuthMethod::PrivateKey {
                    path: key,
                    passphrase: None,
                },
                trust_unknown_host_keys: true,
                host_key_policy: HostKeyPolicy::TrustAll,
                timeout: std::time::Duration::from_secs(15),
                keepalive_interval: Some(ConnectOptions::DEFAULT_KEEPALIVE_INTERVAL),
                keepalive_max: ConnectOptions::DEFAULT_KEEPALIVE_MAX,
            },
            jump: None,
        }
    }

    /// End-to-end recursive folder transfer: upload a nested local tree, verify
    /// it appears remotely, download it back into a fresh dir, and check the
    /// bytes round-trip. Exercises the exact walk + aggregate-progress path the
    /// SFTP UI uses when a folder is selected. Gated on an available SSH key.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recursive_dir_transfer_round_trip() {
        if live_test_key().is_none() {
            return;
        }
        let session = RusshBackend
            .connect_with_route(test_route())
            .await
            .expect("connect");

        // Local source tree: a top-level file + a nested subdir with a file.
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("openterm_tx_src_{pid}"));
        let nested = src.join("nested");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(src.join("top.txt"), b"top-level\n")
            .await
            .unwrap();
        tokio::fs::write(nested.join("inner.bin"), vec![7u8; 4096])
            .await
            .unwrap();
        let expected_total = ("top-level\n".len() + 4096) as u64;
        let remote_dir = format!("/tmp/openterm_tx_{pid}");

        // --- Upload the whole tree ---
        let up_files = collect_local_tree(&session, &src, &remote_dir)
            .await
            .expect("walk local tree");
        assert_eq!(up_files.len(), 2, "expected 2 files in the local tree");
        let (uptx, mut uprx) = mpsc::channel::<u64>(64);
        let up_progress = tokio::spawn(async move {
            let (mut last, mut monotonic) = (0_u64, true);
            while let Some(n) = uprx.recv().await {
                if n < last {
                    monotonic = false;
                }
                last = n;
            }
            (last, monotonic)
        });
        let uploaded = transfer_files(
            &session,
            Direction::Upload,
            &up_files,
            uptx,
            Arc::new(std::sync::atomic::AtomicU8::new(0)),
        )
        .await
        .expect("upload tree");
        let (up_last, up_monotonic) = up_progress.await.unwrap();
        assert_eq!(uploaded, expected_total, "uploaded byte total");
        assert_eq!(up_last, expected_total, "final progress reached the total");
        assert!(up_monotonic, "aggregate upload progress was not monotonic");

        // Confirm the remote tree exists.
        let listed = session.list_dir(&remote_dir).await.expect("list remote");
        assert!(
            listed.iter().any(|e| e.name == "top.txt"),
            "top.txt missing"
        );
        assert!(
            listed
                .iter()
                .any(|e| e.name == "nested" && matches!(e.kind, RemoteFileKind::Directory)),
            "nested subdir missing remotely"
        );

        // --- Download the whole tree back into a fresh local dir ---
        let dst = std::env::temp_dir().join(format!("openterm_tx_dst_{pid}"));
        let _ = tokio::fs::remove_dir_all(&dst).await;
        let down_files = collect_remote_tree(&session, &remote_dir, dst.clone())
            .await
            .expect("walk remote tree");
        assert_eq!(down_files.len(), 2, "expected 2 files in the remote tree");
        let (dntx, mut dnrx) = mpsc::channel::<u64>(64);
        let dn_progress = tokio::spawn(async move {
            let mut last = 0_u64;
            while let Some(n) = dnrx.recv().await {
                last = n;
            }
            last
        });
        let downloaded = transfer_files(
            &session,
            Direction::Download,
            &down_files,
            dntx,
            Arc::new(std::sync::atomic::AtomicU8::new(0)),
        )
        .await
        .expect("download tree");
        let dn_last = dn_progress.await.unwrap();
        assert_eq!(downloaded, expected_total, "downloaded byte total");
        assert_eq!(dn_last, expected_total);

        // Verify the bytes round-tripped at both levels.
        let inner = tokio::fs::read(dst.join("nested").join("inner.bin"))
            .await
            .expect("read inner");
        assert_eq!(inner, vec![7u8; 4096], "nested file bytes differ");
        let top = tokio::fs::read(dst.join("top.txt"))
            .await
            .expect("read top");
        assert_eq!(top, b"top-level\n", "top file bytes differ");

        // Clean up remote tree + local temp dirs.
        session
            .remove_path(&remote_dir, RemoteFileKind::Directory)
            .await
            .expect("cleanup remote");
        let _ = tokio::fs::remove_dir_all(&src).await;
        let _ = tokio::fs::remove_dir_all(&dst).await;
        session.disconnect().await.expect("disconnect");
        eprintln!(
            "OK: recursive upload+download round-trip of {expected_total} bytes across {} files",
            down_files.len()
        );
    }
    mod output_coalescer_tests {
        use super::*;

        /// Interactive-scale bursts arm the quiescence timer so a keystroke echo
        /// flushes after ~2 ms instead of waiting a full frame tick.
        #[test]
        fn small_bursts_arm_the_quiescence_deadline() {
            let mut c = OutputCoalescer::new();
            c.push(b"hi");
            assert!(
                c.deadline.is_some(),
                "interactive burst must arm quiescence"
            );
            assert_eq!(c.flush(), b"hi".to_vec());
            assert!(c.deadline.is_none());
            assert_eq!(c.flush(), Vec::<u8>::new(), "flush is idempotent");
        }

        /// Bulk streams must NOT flush every 2 ms — they wait for the frame tick,
        /// otherwise `cat` would still produce ~500 messages/second.
        #[test]
        fn bulk_streams_wait_for_the_frame_tick() {
            let mut c = OutputCoalescer::new();
            c.push(&vec![b'a'; QUIESCE_MAX_BYTES + 1]);
            assert!(c.deadline.is_none(), "bulk stream must not arm quiescence");
            assert_eq!(c.len(), QUIESCE_MAX_BYTES + 1);
        }

        /// A batch at the cap signals the caller (via len) to flush immediately.
        #[test]
        fn capped_batches_report_their_size_for_immediate_flush() {
            let mut c = OutputCoalescer::new();
            c.push(&vec![b'a'; OUTPUT_BATCH_CAP]);
            assert!(c.len() >= OUTPUT_BATCH_CAP);
            assert_eq!(c.flush().len(), OUTPUT_BATCH_CAP);
        }
    }

    #[cfg(unix)]
    mod latency_benchmark {
        use super::*;
        use iced::futures::StreamExt;

        /// Keystroke→echo latency through the whole UI pipeline: PTY write →
        /// shell echo → read thread → coalescer → Event stream. Prints p50/p95 so
        /// output-path regressions show up as numbers; the assert is a generous
        /// sanity bound (50 ms), not a target.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn keystroke_echo_latency_profile() {
            if let Err(error) = pty_available() {
                eprintln!("skipping: {error}");
                return;
            }

            let mut stream = Box::pin(local_worker(11));
            let sender = match stream.next().await {
                Some(Event::Ready { sender, .. }) => sender,
                _ => panic!("expected Ready as the first worker event"),
            };
            sender
                .send(Command::Connect(ConnectParams {
                    route: test_route(),
                    cols: 80,
                    rows: 24,
                    term: "xterm-256color".to_string(),
                }))
                .await
                .unwrap();
            loop {
                match stream.next().await {
                    Some(Event::Connected { .. }) => break,
                    Some(Event::Failed { error, .. }) => panic!("local shell failed: {error}"),
                    Some(_) => continue,
                    None => panic!("worker stream ended before Connected"),
                }
            }

            // Let the shell's banner/prompt drain before measuring.
            let settle_until = tokio::time::Instant::now() + std::time::Duration::from_millis(400);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(settle_until) => break,
                    ev = stream.next() => { if ev.is_none() { panic!("stream ended during settle"); } }
                }
            }

            const SAMPLES: usize = 50;
            let mut latencies = Vec::with_capacity(SAMPLES);
            for _ in 0..SAMPLES {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                let t0 = std::time::Instant::now();
                sender.send(Command::Write(vec![b'x'])).await.unwrap();
                loop {
                    match tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                        .await
                    {
                        Ok(Some(Event::Output { .. })) => break,
                        Ok(Some(_)) => continue,
                        Ok(None) => panic!("stream ended mid-benchmark"),
                        Err(_) => panic!("no echo within 2 s"),
                    }
                }
                latencies.push(t0.elapsed());
            }
            latencies.sort();

            let at = |q: usize| latencies[q.min(SAMPLES - 1)].as_secs_f64() * 1000.0;
            eprintln!(
                "keystroke→echo over {SAMPLES} keys: min {:.1} ms  p50 {:.1} ms  p95 {:.1} ms",
                at(0),
                at(SAMPLES / 2),
                at(SAMPLES * 95 / 100)
            );
            assert!(
                latencies[SAMPLES / 2] < std::time::Duration::from_millis(50),
                "median keystroke→echo latency {:.1} ms exceeds the 50 ms sanity bound",
                at(SAMPLES / 2)
            );

            sender.send(Command::Disconnect).await.unwrap();
            loop {
                match stream.next().await {
                    Some(Event::Closed { .. }) | None => break,
                    Some(_) => continue,
                }
            }
        }
    }
    mod reconnect_worker_tests {
        use super::*;
        use iced::futures::StreamExt;

        /// The plumbing auto-reconnect rides on: the worker must survive a shell
        /// death (Event::Closed) and accept a fresh Connect on the SAME channel,
        /// reaching Connected again. This is exactly what dispatch_reconnect does
        /// after a dropped connection. Runs against the real test server with
        /// key auth; skips loudly without the key.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn worker_accepts_reconnect_after_shell_death() {
            if live_test_key().is_none() {
                return;
            }

            let mut stream = Box::pin(worker(21));
            let sender = match stream.next().await {
                Some(Event::Ready { sender, .. }) => sender,
                _ => panic!("expected Ready"),
            };

            // First connection.
            sender
                .send(Command::Connect(ConnectParams {
                    route: test_route(),
                    cols: 80,
                    rows: 24,
                    term: "xterm-256color".to_string(),
                }))
                .await
                .unwrap();
            let mut saw_connected = false;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await {
                    Ok(Some(Event::Connected { .. })) => {
                        saw_connected = true;
                        break;
                    }
                    Ok(Some(_)) => continue,
                    _ => panic!("first connect did not reach Connected"),
                }
            }
            assert!(saw_connected);

            // Kill the shell from inside — the connection drops, worker reports
            // Closed and stays alive.
            sender
                .send(Command::Write(b"exit\r\n".to_vec()))
                .await
                .unwrap();
            let mut saw_closed = false;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await {
                    Ok(Some(Event::Closed { .. })) => {
                        saw_closed = true;
                        break;
                    }
                    Ok(Some(_)) => continue,
                    _ => panic!("shell exit did not produce Closed"),
                }
            }
            assert!(saw_closed, "worker should report Closed but keep running");

            // Reconnect on the same channel — this is the auto-reconnect path.
            sender
                .send(Command::Connect(ConnectParams {
                    route: test_route(),
                    cols: 80,
                    rows: 24,
                    term: "xterm-256color".to_string(),
                }))
                .await
                .unwrap();
            let mut reconnected = false;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await {
                    Ok(Some(Event::Connected { .. })) => {
                        reconnected = true;
                        break;
                    }
                    Ok(Some(_)) => continue,
                    _ => panic!("reconnect did not reach Connected"),
                }
            }
            assert!(reconnected, "worker must reconnect after Closed");

            let _ = sender.send(Command::Disconnect).await;
        }
    }
}
