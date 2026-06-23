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
    RusshBackend, RusshSession, ShellOptions, SshError,
};
use tokio::sync::mpsc;

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
    /// Remove a remote file or directory.
    SftpRemove { path: String, is_dir: bool },
    /// Rename / move a remote path.
    SftpRename { from: String, to: String },
    /// Sample remote resource usage (CPU/mem/disk/net) over the live connection.
    SampleMetrics,
    /// Sample the remote process list (for the monitor's CPU/Memory drill-down).
    SampleProcesses,
    /// Change permissions of a remote file.
    SftpChmod { path: String, mode: u32 },
    /// Close the shell and disconnect.
    Disconnect,
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
            | Event::Metrics { session_id, .. }
            | Event::Processes { session_id, .. }
            | Event::Exit { session_id, .. }
            | Event::Closed { session_id }
            | Event::Failed { session_id, .. } => *session_id,
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

    let session = match RusshBackend.connect_with_route(params.route.clone()).await {
        Ok(session) => Arc::new(session),
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
            let _ = out
                .send(Event::Failed {
                    session_id,
                    error: error.to_string(),
                })
                .await;
            return ControlFlow::Continue(());
        }
    };

    let _ = out.send(Event::Connected { session_id }).await;

    // Spawn the shell pump on its own task so SFTP work never stalls it.
    let (pty_in_tx, mut pty_in_rx) = mpsc::channel::<PtyInput>(256);
    let (pty_ev_tx, mut pty_ev_rx) = mpsc::channel::<PtyEvent>(256);
    let shell_session = session.clone();
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

    // Main multiplexing loop.
    let outcome = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Write(bytes)) => {
                    let _ = pty_in_tx.send(PtyInput::Write(bytes)).await;
                }
                Some(Command::Resize { cols, rows }) => {
                    let _ = pty_in_tx.send(PtyInput::Resize(PtySize { cols, rows })).await;
                }
                Some(Command::Connect(_)) => {
                    // Already connected; ignore duplicate connects.
                }
                Some(Command::SftpList(path)) => {
                    spawn_sftp_list(session_id, session.clone(), out.clone(), path);
                }
                Some(Command::SftpDownload { id, name, remote, local, size, is_dir }) => {
                    spawn_transfer(
                        session_id,
                        session.clone(),
                        out.clone(),
                        Transfer { id, name, direction: Direction::Download, remote, local, size, is_dir },
                    );
                }
                Some(Command::SftpUpload { id, name, local, remote, size, is_dir }) => {
                    spawn_transfer(
                        session_id,
                        session.clone(),
                        out.clone(),
                        Transfer { id, name, direction: Direction::Upload, remote, local, size, is_dir },
                    );
                }
                Some(Command::SftpMkdir(path)) => {
                    spawn_sftp_simple(session_id, session.clone(), out.clone(),
                        SftpOp::Mkdir(path));
                }
                Some(Command::SftpRemove { path, is_dir }) => {
                    spawn_sftp_simple(session_id, session.clone(), out.clone(),
                        SftpOp::Remove { path, is_dir });
                }
                Some(Command::SftpRename { from, to }) => {
                    spawn_sftp_simple(session_id, session.clone(), out.clone(),
                        SftpOp::Rename { from, to });
                }
                Some(Command::SftpChmod { path, mode }) => {
                    spawn_sftp_simple(session_id, session.clone(), out.clone(),
                        SftpOp::Chmod { path, mode });
                }
                Some(Command::SampleMetrics) => {
                    spawn_metrics(session_id, session.clone(), out.clone());
                }
                Some(Command::SampleProcesses) => {
                    spawn_processes(session_id, session.clone(), out.clone());
                }
                Some(Command::Disconnect) => break ShellOutcome::Disconnected,
                None => break ShellOutcome::WorkerDropped,
            },
            ev = pty_ev_rx.recv() => match ev {
                Some(PtyEvent::Output(bytes)) => {
                    if out.send(Event::Output { session_id, bytes }).await.is_err() {
                        break ShellOutcome::WorkerDropped;
                    }
                }
                Some(PtyEvent::ExitStatus(code)) => {
                    let _ = out.send(Event::Exit { session_id, code }).await;
                }
                Some(PtyEvent::Closed) | None => break ShellOutcome::Closed,
            }
        }
    };

    // Tear down this connection.
    let _ = session.disconnect().await;
    shell_task.abort();

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

fn spawn_sftp_list(session_id: u64, session: Arc<RusshSession>, mut out: OutSink, path: String) {
    tokio::spawn(async move {
        // Resolve relative paths (e.g. "." on connect) to an absolute path so
        // "Up" navigation has somewhere to go.
        let resolved = session
            .canonicalize(&path)
            .await
            .unwrap_or_else(|_| path.clone());
        let result = session.list_dir(&resolved).await.map_err(|e| e.to_string());
        let _ = out
            .send(Event::SftpListed {
                session_id,
                path: resolved,
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

/// Run a streamed transfer (a single file or a whole directory tree), emitting
/// Started → throttled Progress (≈12/s, with instantaneous speed) → Finished.
/// A directory is walked first to build a flat file list and a true total, then
/// every file streams with progress accumulated across the whole tree, so a
/// folder appears as one transfer with one aggregate progress bar.
fn spawn_transfer(session_id: u64, session: Arc<RusshSession>, mut out: OutSink, t: Transfer) {
    tokio::spawn(async move {
        // Build the work list of (remote, local, size) and the overall total.
        // A directory is expanded into its files (creating the destination
        // directory skeleton as a side effect); a single file is its own list.
        let (files, total): (Vec<(String, std::path::PathBuf, u64)>, u64) = if t.is_dir {
            let walked = match t.direction {
                Direction::Download => {
                    collect_remote_tree(&session, &t.remote, std::path::PathBuf::from(&t.local)).await
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
                    ema_speed = if ema_speed == 0.0 { instant } else { 0.25 * instant + 0.75 * ema_speed };
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
        let result = transfer_files(&session, t.direction, &files, ptx).await;
        // ptx was consumed by transfer_files → throttle forwarder ends.
        let _ = forwarder.await;

        let _ = out
            .send(Event::TransferFinished {
                session_id,
                id: t.id,
                result: result.map_err(|e| e.to_string()),
            })
            .await;
    });
}

/// Stream every file in `files` (already-resolved `(remote, local, size)`),
/// reporting cumulative bytes across the whole list over `progress`, and return
/// the total bytes transferred. Each file's own 0..size progress is offset by
/// the bytes already done so the combined stream is monotonic. The first
/// failure aborts the rest, matching single-file semantics.
async fn transfer_files(
    session: &RusshSession,
    direction: Direction,
    files: &[(String, std::path::PathBuf, u64)],
    progress: mpsc::Sender<u64>,
) -> Result<u64, SshError> {
    let mut base = 0_u64;
    for (remote, local, _sz) in files {
        let (fptx, mut fprx) = mpsc::channel::<u64>(256);
        let agg = progress.clone();
        let base_now = base;
        let fwd = tokio::spawn(async move {
            while let Some(n) = fprx.recv().await {
                let _ = agg.send(base_now + n).await;
            }
        });
        let one = match direction {
            Direction::Download => session.download_file(remote, local, fptx).await,
            Direction::Upload => session.upload_file(local, remote, fptx).await,
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
        // Best-effort: the directory may already exist on the remote.
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

fn spawn_sftp_simple(session_id: u64, session: Arc<RusshSession>, mut out: OutSink, op: SftpOp) {
    tokio::spawn(async move {
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
fn spawn_metrics(session_id: u64, session: Arc<RusshSession>, mut out: OutSink) {
    tokio::spawn(async move {
        if let Ok(output) = session.exec_capture(crate::metrics::SAMPLE_COMMAND).await {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let _ = out.send(Event::Metrics { session_id, raw }).await;
        }
    });
}

/// Sample the remote process list (`ps`) for the monitor's drill-down. Errors
/// are swallowed: a failed sample keeps the last list until the next tick.
fn spawn_processes(session_id: u64, session: Arc<RusshSession>, mut out: OutSink) {
    tokio::spawn(async move {
        if let Ok(output) = session.exec_capture(crate::metrics::PROCESS_COMMAND).await {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let _ = out.send(Event::Processes { session_id, raw }).await;
        }
    });
}

/// Subscription worker for a local PTY shell (macOS/Linux).
/// Speaks the same `Event` protocol as the SSH `worker` so the rest of the
/// app needs no special-casing beyond picking which worker to run.
#[cfg(unix)]
pub fn local_worker(session_id: u64) -> impl iced::futures::Stream<Item = Event> {
    use std::os::unix::io::RawFd;
    iced::stream::channel(256, move |mut out: OutSink| async move {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(256);
        if out.send(Event::Ready { session_id, sender: cmd_tx }).await.is_err() {
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
        let (master_fd, child_pid) = match spawn_pty_shell(params.cols, params.rows) {
            Ok(x) => x,
            Err(e) => {
                let _ = out.send(Event::Failed { session_id, error: e.to_string() }).await;
                return;
            }
        };
        let _ = out.send(Event::Connected { session_id }).await;

        // Bridge the blocking PTY read into async via a dedicated thread.
        let (pty_tx, mut pty_rx) = mpsc::channel::<Vec<u8>>(256);
        let read_fd = master_fd;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n <= 0 { break; }
                if pty_tx.blocking_send(buf[..n as usize].to_vec()).is_err() { break; }
            }
        });

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(Command::Write(bytes)) => {
                        unsafe { libc::write(master_fd, bytes.as_ptr() as *const libc::c_void, bytes.len()); }
                    }
                    Some(Command::Resize { cols, rows }) => {
                        let ws = libc::winsize { ws_col: cols, ws_row: rows, ws_xpixel: 0, ws_ypixel: 0 };
                        unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws); }
                    }
                    Some(Command::Disconnect) | None => break,
                    _ => {}
                },
                bytes = pty_rx.recv() => match bytes {
                    Some(b) => { if out.send(Event::Output { session_id, bytes: b }).await.is_err() { break; } }
                    None => break,
                },
            }
        }

        unsafe {
            libc::close(master_fd);
            libc::kill(child_pid, libc::SIGHUP);
        }
        let _ = out.send(Event::Closed { session_id }).await;
    })
}

/// Open a PTY pair and spawn `$SHELL`. Returns `(master_fd, child_pid)`.
/// Uses `std::process::Command` + `pre_exec` instead of raw `fork()` so the
/// call is safe from a multi-threaded tokio runtime.
#[cfg(unix)]
fn spawn_pty_shell(cols: u16, rows: u16) -> std::io::Result<(libc::c_int, libc::pid_t)> {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;

    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master, &mut slave,
            std::ptr::null_mut(), std::ptr::null_mut(),
            &mut libc::winsize { ws_col: cols, ws_row: rows, ws_xpixel: 0, ws_ypixel: 0 },
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

    // Dup slave so each stdio slot gets its own fd (File takes ownership).
    let (s1, s2) = unsafe { (libc::dup(slave), libc::dup(slave)) };
    let stdin  = unsafe { std::fs::File::from_raw_fd(slave) };
    let stdout = unsafe { std::fs::File::from_raw_fd(s1) };
    let stderr = unsafe { std::fs::File::from_raw_fd(s2) };

    let mut cmd = std::process::Command::new(&shell);
    cmd.arg("-l")
        .stdin(stdin).stdout(stdout).stderr(stderr)
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
    let pid = child.id() as libc::pid_t;
    // Leak the Child handle — we manage lifetime via master fd + SIGHUP on disconnect.
    std::mem::forget(child);

    Ok((master, pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a route to the live test server (mirrors openterm-ssh's harness).
    fn test_route(password: String) -> ConnectRoute {
        use openterm_core::HostProfile;
        use openterm_ssh::{AuthMethod, ConnectOptions, HostKeyPolicy};
        let mut profile = HostProfile::new("live", "82.157.57.178");
        profile.port = 22;
        profile.username = Some("ubuntu".to_string());
        ConnectRoute {
            target: profile,
            target_options: ConnectOptions {
                username: "ubuntu".to_string(),
                auth: AuthMethod::Password(password),
                trust_unknown_host_keys: true,
                host_key_policy: HostKeyPolicy::TrustAll,
                timeout: std::time::Duration::from_secs(15),
            },
            jump: None,
        }
    }

    /// End-to-end recursive folder transfer: upload a nested local tree, verify
    /// it appears remotely, download it back into a fresh dir, and check the
    /// bytes round-trip. Exercises the exact walk + aggregate-progress path the
    /// SFTP UI uses when a folder is selected. Gated on OPENTERM_TEST_PASSWORD.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recursive_dir_transfer_round_trip() {
        let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
            eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
            return;
        };
        let session = RusshBackend
            .connect_with_route(test_route(password))
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
        let uploaded = transfer_files(&session, Direction::Upload, &up_files, uptx)
            .await
            .expect("upload tree");
        let (up_last, up_monotonic) = up_progress.await.unwrap();
        assert_eq!(uploaded, expected_total, "uploaded byte total");
        assert_eq!(up_last, expected_total, "final progress reached the total");
        assert!(up_monotonic, "aggregate upload progress was not monotonic");

        // Confirm the remote tree exists.
        let listed = session.list_dir(&remote_dir).await.expect("list remote");
        assert!(listed.iter().any(|e| e.name == "top.txt"), "top.txt missing");
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
        let downloaded = transfer_files(&session, Direction::Download, &down_files, dntx)
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
        let top = tokio::fs::read(dst.join("top.txt")).await.expect("read top");
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
}
