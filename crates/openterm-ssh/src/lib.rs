use openterm_core::HostProfile;
use russh::client;
use russh::keys::agent::client::AgentClient;
use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, FileType, OpenFlags};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

pub mod connection_pool;

pub use connection_pool::{DataConnection, PoolConfig, PoolStats, SshConnectionPool};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyChallenge {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    pub known_hosts: PathBuf,
    public_key: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("authentication failed")]
    Authentication,
    #[error("host key verification required")]
    HostKeyVerificationRequired(Box<HostKeyChallenge>),
    #[error("username is required")]
    MissingUsername,
    #[error("remote command did not report an exit status")]
    MissingExitStatus,
    #[error("SSH protocol error: {0}")]
    Protocol(#[from] russh::Error),
    #[error("SFTP error: {0}")]
    Sftp(#[from] russh_sftp::client::error::Error),
    #[error("SSH key error: {0}")]
    Key(#[from] russh::keys::Error),
    #[error("SSH agent authentication failed: {0}")]
    Agent(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("operation timed out")]
    Timeout,
}

impl SshError {
    /// Whether re-running the same operation could plausibly succeed.
    ///
    /// Drives transfer retries: a timeout or a dropped transport on a congested
    /// link is worth another attempt (the `.part` prefix makes it cheap), while
    /// a server status reply ("no such file", "permission denied") is a verdict
    /// that another attempt cannot change.
    pub fn is_transient(&self) -> bool {
        match self {
            SshError::Timeout | SshError::Io(_) | SshError::Protocol(_) => true,
            SshError::Sftp(error) => !matches!(
                error,
                russh_sftp::client::error::Error::Status(_)
                    | russh_sftp::client::error::Error::Limited(_)
            ),
            _ => false,
        }
    }
}

/// Upper bound for short remote operations (exec samples, SFTP metadata,
/// directory listings). Chosen well above normal round-trip times: its job is
/// to release the channel-semaphore permit when a server stops responding
/// (frozen disk, cgroup freeze), not to police slow-but-alive links.
const OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// How many bulk transfers (uploads/downloads) may run at once. Must stay
/// strictly below the channel semaphore's 6 so directory listings and other
/// interactive ops always have channel permits available mid-transfer.
const MAX_BULK_TRANSFERS: usize = 3;

/// Bulk-content bound (whole-file read/write, recursive deletes): more
/// generous, since legitimately large payloads on slow links take a while.
const CONTENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Read size for one positioned read in a bulk download.
const TRANSFER_CHUNK: u64 = 256 * 1024;

/// How many [`TRANSFER_CHUNK`] reads may be in flight at once. The window is
/// sized from measured throughput between these bounds rather than fixed: a
/// deep window on a slow link is what queues a request behind tens of seconds
/// of data (see [`TRANSFER_TAIL_TARGET`]), and a shallow one caps throughput on
/// a fast link.
const TRANSFER_WINDOW_MIN: usize = 2;
const TRANSFER_WINDOW_MAX: usize = 16;

/// Largest queueing delay we are willing to create for the deepest in-flight
/// read, at the currently measured rate. The window is derived from this, so a
/// slow link shrinks it automatically instead of building an unservable queue.
const TRANSFER_TAIL_TARGET: Duration = Duration::from_secs(8);

/// Per-request deadline for bulk SFTP traffic.
///
/// `russh-sftp` defaults to **10 s**, which is not a timeout for the *request*
/// so much as a budget for the *whole queue in front of it*: a deep read window
/// on a link slower than `window_bytes / 10 s` times out the tail of every
/// window. The request is then dropped from the dispatch table, its late reply
/// is discarded ("packet for unknown recipient" — the bytes are pulled off the
/// wire and thrown away), and the transfer aborts with `SFTP error: Timeout`
/// while the server keeps streaming. Measured on a 100 KiB/s link: a 4 MiB
/// transfer died after 20 s, losing everything it had fetched.
///
/// Bulk transfers therefore get a deadline generous enough that only a genuinely
/// stalled peer trips it; the adaptive window above is what keeps real latency
/// far away from it. Interactive operations keep the tight default, so a hung
/// server still fails `ls`/`stat` quickly.
const TRANSFER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// In-flight writes `russh-sftp` keeps queued for a bulk upload.
const TRANSFER_WRITE_WINDOW: usize = 4;

/// Attempts for one bulk transfer. Every attempt after the first resumes from
/// the `.part` prefix, so a retry costs one backoff instead of the whole file.
const TRANSFER_ATTEMPTS: usize = 4;
const TRANSFER_RETRY_BACKOFF: Duration = Duration::from_millis(750);
const TRANSFER_RETRY_BACKOFF_MAX: Duration = Duration::from_secs(8);

/// Run `fut` with a deadline, mapping expiry to [`SshError::Timeout`]. On
/// timeout the in-flight future is dropped, which closes its channel and
/// releases the channel-semaphore permit it held.
async fn bounded<T>(
    limit: std::time::Duration,
    fut: impl std::future::Future<Output = Result<T, SshError>>,
) -> Result<T, SshError> {
    tokio::time::timeout(limit, fut)
        .await
        .unwrap_or(Err(SshError::Timeout))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    Password(String),
    AgentOrDefault,
    DefaultKey,
    PrivateKey {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOptions {
    pub username: String,
    pub auth: AuthMethod,
    pub trust_unknown_host_keys: bool,
    pub host_key_policy: HostKeyPolicy,
    pub timeout: Duration,
    /// How often to send an SSH keepalive while the link is otherwise quiet.
    /// `None` disables SSH-level keepalives; TCP keepalives stay enabled
    /// regardless, because they are the ones that survive a throttled app.
    pub keepalive_interval: Option<Duration>,
    /// Unanswered keepalives tolerated before the session is declared dead.
    pub keepalive_max: usize,
}

/// Idle time before the kernel starts TCP keepalive probes.
///
/// Applies whenever SSH-level keepalives are disabled or late, which is the
/// point: these probes are sent by the kernel, so a throttled or descheduled
/// app cannot stop them.
const TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(30);

impl ConnectOptions {
    /// Default SSH keepalive interval.
    pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

    /// Unanswered keepalives tolerated before the session is declared dead.
    ///
    /// Looser than russh's default of 3 on purpose. At a 30s interval that
    /// default gives a session only ~2 minutes of grace, which a throttled app
    /// (macOS App Nap) or a badly congested link can exceed while the session is
    /// perfectly healthy — and killing a live session is far worse than noticing
    /// a dead one a minute later. TCP keepalives still detect a truly dead peer.
    pub const DEFAULT_KEEPALIVE_MAX: usize = 5;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyPolicy {
    TrustAll,
    Strict { known_hosts: PathBuf },
    AcceptNew { known_hosts: PathBuf },
    ConfirmNew { known_hosts: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRoute {
    pub target: HostProfile,
    pub target_options: ConnectOptions,
    pub jump: Option<(HostProfile, ConnectOptions)>,
}

impl HostKeyChallenge {
    pub fn accept(&self) -> Result<(), SshError> {
        let public_key = russh::keys::ssh_key::PublicKey::from_openssh(&self.public_key)
            .map_err(|error| SshError::Connection(error.to_string()))?;
        learn_known_hosts_path(&self.host, self.port, &public_key, &self.known_hosts)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum ClientHandlerError {
    #[error(transparent)]
    Russh(#[from] russh::Error),
    #[error("host key verification required")]
    HostKeyVerificationRequired(Box<HostKeyChallenge>),
}

impl ConnectOptions {
    fn effective_host_key_policy(&self) -> HostKeyPolicy {
        if self.trust_unknown_host_keys {
            HostKeyPolicy::TrustAll
        } else {
            self.host_key_policy.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_status: u32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteFileKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFileEntry {
    pub name: String,
    pub path: String,
    pub kind: RemoteFileKind,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalForwardOptions {
    pub bind_host: String,
    pub bind_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardEvent {
    Listening { bind_host: String, bind_port: u16 },
    ConnectionAccepted { peer: String },
    ConnectionClosed { peer: String },
    Failed(String),
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOptions {
    pub term: String,
    pub size: PtySize,
    /// Run this command on the PTY instead of the login shell. Used for
    /// session persistence (`tmux new -A …`): the PTY semantics are
    /// identical, so resize/input/output all work unchanged.
    pub command: Option<String>,
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            term: "xterm-256color".to_string(),
            size: PtySize { cols: 80, rows: 24 },
            command: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    ExitStatus(u32),
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyInput {
    Write(Vec<u8>),
    Resize(PtySize),
}

#[derive(Debug, Default)]
pub struct RusshBackend;

impl RusshBackend {
    pub async fn connect_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
    ) -> Result<RusshSession, SshError> {
        let config = Arc::new(client::Config {
            // No inactivity timeout: an idle terminal must not drop the session.
            // Liveness is handled by the caller's keepalive policy, backed by TCP
            // keepalives on the socket below.
            inactivity_timeout: None,
            keepalive_interval: options.keepalive_interval,
            keepalive_max: options.keepalive_max,
            nodelay: true,
            ..Default::default()
        });
        let handler = ClientHandler {
            host: profile.host.clone(),
            port: profile.port,
            policy: options.effective_host_key_policy(),
        };
        // `options.timeout` guards the initial connect only, not the live session.
        //
        // `client::connect` would build its own socket with `nodelay` and nothing
        // else — no TCP keepalives — so the socket is built here instead and
        // handed to `client::connect_stream`.
        let stream = tokio::time::timeout(
            options.timeout,
            tokio::net::TcpStream::connect((profile.host.as_str(), profile.port)),
        )
        .await
        .map_err(|_| SshError::Connection("connection timed out".to_string()))??;
        if let Err(error) = stream.set_nodelay(true) {
            log::warn!("could not disable Nagle on {}: {error}", profile.host);
        }
        if let Err(error) = configure_tcp_keepalive(&stream) {
            log::warn!(
                "could not enable TCP keepalives for {} (idle SSH sessions may drop): {error}",
                profile.host
            );
        }
        let mut handle = tokio::time::timeout(
            options.timeout,
            client::connect_stream(config, stream, handler),
        )
        .await
        .map_err(|_| SshError::Connection("connection timed out".to_string()))?
        .map_err(map_handler_error)?;

        let auth_result = authenticate_handle(&mut handle, options.username, options.auth).await?;

        if !auth_result.success() {
            return Err(SshError::Authentication);
        }

        Ok(RusshSession {
            handle,
            jump_handle: None,
            channel_sem: Arc::new(tokio::sync::Semaphore::new(6)),
            bulk_sem: Arc::new(tokio::sync::Semaphore::new(MAX_BULK_TRANSFERS)),
        })
    }

    pub async fn connect_with_route(&self, route: ConnectRoute) -> Result<RusshSession, SshError> {
        match route.jump {
            None => {
                self.connect_with_options(route.target, route.target_options)
                    .await
            }
            Some((jump_profile, jump_options)) => {
                self.connect_via_jump_with_options(
                    jump_profile,
                    jump_options,
                    route.target,
                    route.target_options,
                )
                .await
            }
        }
    }

    pub async fn connect_via_jump_with_options(
        &self,
        jump_profile: HostProfile,
        jump_options: ConnectOptions,
        target_profile: HostProfile,
        target_options: ConnectOptions,
    ) -> Result<RusshSession, SshError> {
        let jump = self
            .connect_with_options(jump_profile.clone(), jump_options)
            .await?;
        let channel = jump
            .handle
            .channel_open_direct_tcpip(
                target_profile.host.as_str(),
                u32::from(target_profile.port),
                jump_profile.host.as_str(),
                u32::from(jump_profile.port),
            )
            .await?;

        let config = Arc::new(client::Config {
            // See `connect_with_options`: idle sessions stay alive via the
            // caller's keepalive policy (the outer connection's TCP keepalives
            // already cover the tunnel itself).
            inactivity_timeout: None,
            keepalive_interval: target_options.keepalive_interval,
            keepalive_max: target_options.keepalive_max,
            nodelay: true,
            ..Default::default()
        });
        let handler = ClientHandler {
            host: target_profile.host.clone(),
            port: target_profile.port,
            policy: target_options.effective_host_key_policy(),
        };
        let mut handle = tokio::time::timeout(
            target_options.timeout,
            client::connect_stream(config, channel.into_stream(), handler),
        )
        .await
        .map_err(|_| SshError::Connection("connection timed out".to_string()))?
        .map_err(map_handler_error)?;
        let auth_result =
            authenticate_handle(&mut handle, target_options.username, target_options.auth).await?;
        if !auth_result.success() {
            return Err(SshError::Authentication);
        }

        Ok(RusshSession {
            handle,
            jump_handle: Some(jump.handle),
            channel_sem: Arc::new(tokio::sync::Semaphore::new(6)),
            bulk_sem: Arc::new(tokio::sync::Semaphore::new(MAX_BULK_TRANSFERS)),
        })
    }

    pub async fn exec_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        command: &str,
    ) -> Result<ExecOutput, SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let output = session.exec(command).await;
        let close_result = session.close().await;
        match (output, close_result) {
            (Ok(output), Ok(())) => Ok(output),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn exec_with_route(
        &self,
        route: ConnectRoute,
        command: &str,
    ) -> Result<ExecOutput, SshError> {
        let mut session = self.connect_with_route(route).await?;
        let output = session.exec(command).await;
        let close_result = session.close().await;
        match (output, close_result) {
            (Ok(output), Ok(())) => Ok(output),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn shell_with_options<I, O>(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        shell: ShellOptions,
        stdin: I,
        stdout: O,
    ) -> Result<u32, SshError>
    where
        I: AsyncRead + Send + Unpin + 'static,
        O: AsyncWrite + Send + Unpin + 'static,
    {
        let mut session = self.connect_with_options(profile, options).await?;
        let exit_status = session.interactive_shell(shell, stdin, stdout).await;
        let close_result = session.close().await;
        match (exit_status, close_result) {
            (Ok(exit_status), Ok(())) => Ok(exit_status),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn list_dir_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        path: &str,
    ) -> Result<Vec<RemoteFileEntry>, SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let result = session.list_dir(path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(entries), Ok(())) => Ok(entries),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn list_dir_with_route(
        &self,
        route: ConnectRoute,
        path: &str,
    ) -> Result<Vec<RemoteFileEntry>, SshError> {
        let mut session = self.connect_with_route(route).await?;
        let result = session.list_dir(path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(entries), Ok(())) => Ok(entries),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn read_file_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        remote_path: &str,
    ) -> Result<Vec<u8>, SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let result = session.read_file(remote_path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(bytes), Ok(())) => Ok(bytes),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn read_file_with_route(
        &self,
        route: ConnectRoute,
        remote_path: &str,
    ) -> Result<Vec<u8>, SshError> {
        let mut session = self.connect_with_route(route).await?;
        let result = session.read_file(remote_path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(bytes), Ok(())) => Ok(bytes),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn write_file_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        remote_path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let result = session.write_file(remote_path, bytes).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn write_file_with_route(
        &self,
        route: ConnectRoute,
        remote_path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_route(route).await?;
        let result = session.write_file(remote_path, bytes).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn create_dir_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        remote_path: &str,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let result = session.create_dir(remote_path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn create_dir_with_route(
        &self,
        route: ConnectRoute,
        remote_path: &str,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_route(route).await?;
        let result = session.create_dir(remote_path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn remove_path_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        remote_path: &str,
        kind: RemoteFileKind,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let result = session.remove_path(remote_path, kind).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn remove_path_with_route(
        &self,
        route: ConnectRoute,
        remote_path: &str,
        kind: RemoteFileKind,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_route(route).await?;
        let result = session.remove_path(remote_path, kind).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn rename_path_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        old_path: &str,
        new_path: &str,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let result = session.rename_path(old_path, new_path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn rename_path_with_route(
        &self,
        route: ConnectRoute,
        old_path: &str,
        new_path: &str,
    ) -> Result<(), SshError> {
        let mut session = self.connect_with_route(route).await?;
        let result = session.rename_path(old_path, new_path).await;
        let close_result = session.close().await;
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn run_local_forward_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        forward: LocalForwardOptions,
        events: mpsc::Sender<ForwardEvent>,
        mut stop: mpsc::Receiver<()>,
    ) -> Result<(), SshError> {
        let session = self.connect_with_options(profile, options).await?;
        session.run_local_forward(forward, events, &mut stop).await
    }
}

pub struct RusshSession {
    handle: client::Handle<ClientHandler>,
    jump_handle: Option<client::Handle<ClientHandler>>,
    /// Limits concurrent SSH channels so we never exceed the server's
    /// MaxSessions cap (OpenSSH default = 10). Every method that opens a
    /// channel acquires a permit for the channel's lifetime.
    channel_sem: Arc<tokio::sync::Semaphore>,
    /// Caps concurrent *bulk transfers* (upload/download), strictly below the
    /// channel budget. Transfers hold a channel for minutes; without this cap a
    /// multi-file batch drains `channel_sem` completely and — because tokio
    /// semaphores are FIFO — a directory listing then queues behind every
    /// pending transfer, freezing SFTP navigation for the whole batch. Queued
    /// transfers wait HERE instead, leaving channel permits for interactive ops.
    bulk_sem: Arc<tokio::sync::Semaphore>,
}

/// An SFTP session paired with the channel semaphore permit that guards it.
/// Dropping (or calling `.close()`) releases both the SFTP session and the
/// permit, making the slot available for the next caller.
struct SftpHandle {
    inner: SftpSession,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl SftpHandle {
    async fn close(self) -> Result<(), russh_sftp::client::error::Error> {
        self.inner.close().await
        // _permit drops here, freeing the channel slot
    }
}

impl std::ops::Deref for SftpHandle {
    type Target = SftpSession;
    fn deref(&self) -> &SftpSession {
        &self.inner
    }
}

impl RusshSession {
    /// Run one command and collect its output over a dedicated exec channel.
    /// Used by the one-shot backend helpers (connect → exec → close).
    pub async fn exec(&mut self, command: &str) -> Result<ExecOutput, SshError> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status = None;

        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status: code } => exit_status = Some(code),
                ChannelMsg::Close => break,
                _ => {}
            }
        }

        Ok(ExecOutput {
            exit_status: exit_status.ok_or(SshError::MissingExitStatus)?,
            stdout,
            stderr,
        })
    }

    /// Disconnect and close the session. Unlike [`RusshSession::disconnect`],
    /// this takes `&mut self` and reclaims the jump-host handle.
    pub async fn close(&mut self) -> Result<(), SshError> {
        self.handle
            .disconnect(Disconnect::ByApplication, "", "English")
            .await?;
        if let Some(jump_handle) = self.jump_handle.take() {
            jump_handle
                .disconnect(Disconnect::ByApplication, "", "English")
                .await?;
        }
        Ok(())
    }

    /// Disconnect the session through a shared reference. Unlike [`RusshSession::close`],
    /// this does not require ownership, so an actor holding `Arc<RusshSession>` can tear
    /// the connection down while shell and SFTP channels are multiplexed over it.
    pub async fn disconnect(&self) -> Result<(), SshError> {
        let timeout = std::time::Duration::from_secs(5);
        let _ = tokio::time::timeout(
            timeout,
            self.handle
                .disconnect(Disconnect::ByApplication, "", "English"),
        )
        .await;
        if let Some(jump_handle) = &self.jump_handle {
            let _ = tokio::time::timeout(
                timeout,
                jump_handle.disconnect(Disconnect::ByApplication, "", "English"),
            )
            .await;
        }
        Ok(())
    }

    /// Check if the connection is still alive (used by the pool's health checks).
    ///
    /// Sends one SSH global-request ping and returns true if the server answers
    /// within a short timeout.
    pub async fn is_alive(&self) -> bool {
        // A global-request ping: one round trip, no channel opened, and the
        // reply is resolved by the session loop itself. The earlier `echo 1`
        // probe opened an exec channel and could therefore be refused by the
        // channel semaphore while a transfer was running — reporting a healthy
        // connection as dead, and (through the pool's health check) throwing the
        // connection away.
        let probe_timeout = std::time::Duration::from_secs(5);
        tokio::time::timeout(probe_timeout, self.handle.send_ping())
            .await
            .is_ok_and(|result| result.is_ok())
    }

    /// Run a command and capture its output through a shared reference. Opens a
    /// fresh exec channel on the live connection (multiplexed alongside the shell
    /// and SFTP), so an `Arc<RusshSession>` can sample remote state — e.g. the
    /// resource monitor reading `/proc` — without disturbing the interactive PTY.
    ///
    /// Bounded by [`OP_TIMEOUT`]: a hung remote (frozen disk, cgroup freeze)
    /// would otherwise hold this permit forever, and six stuck samples would
    /// starve SFTP/metrics/completion for the whole connection.
    pub async fn exec_capture(&self, command: &str) -> Result<ExecOutput, SshError> {
        let _permit = self
            .channel_sem
            .clone()
            .acquire_owned()
            .await
            .expect("channel semaphore closed");
        bounded(OP_TIMEOUT, async {
            let mut channel = self.handle.channel_open_session().await?;
            channel.exec(true, command).await?;

            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut exit_status = None;

            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                    ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status: code } => exit_status = Some(code),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }

            Ok(ExecOutput {
                // Some servers close the channel without an explicit exit-status for
                // piped commands; default to 0 rather than failing the sample.
                exit_status: exit_status.unwrap_or(0),
                stdout,
                stderr,
            })
        })
        .await
    }

    pub async fn interactive_shell<I, O>(
        &mut self,
        options: ShellOptions,
        mut stdin: I,
        mut stdout: O,
    ) -> Result<u32, SshError>
    where
        I: AsyncRead + Send + Unpin + 'static,
        O: AsyncWrite + Send + Unpin + 'static,
    {
        let channel = self.handle.channel_open_session().await?;
        channel
            .request_pty(
                true,
                &options.term,
                u32::from(options.size.cols),
                u32::from(options.size.rows),
                0,
                0,
                &[],
            )
            .await?;
        channel.request_shell(true).await?;

        let (mut reader, writer) = channel.split();
        let input_task = tokio::spawn(async move {
            let mut buffer = [0_u8; 8192];
            loop {
                let read = stdin.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                writer.data_bytes(buffer[..read].to_vec()).await?;
            }
            Ok::<(), SshError>(())
        });

        let mut exit_status = 0;
        while let Some(message) = reader.wait().await {
            match message {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                    stdout.write_all(&data).await?;
                    stdout.flush().await?;
                }
                ChannelMsg::ExitStatus { exit_status: code } => exit_status = code,
                ChannelMsg::Close => break,
                _ => {}
            }
        }

        input_task.abort();
        Ok(exit_status)
    }

    pub async fn event_shell(
        &self,
        options: ShellOptions,
        input: &mut mpsc::Receiver<PtyInput>,
        events: mpsc::Sender<PtyEvent>,
    ) -> Result<u32, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel
            .request_pty(
                true,
                &options.term,
                u32::from(options.size.cols),
                u32::from(options.size.rows),
                0,
                0,
                &[],
            )
            .await?;
        match &options.command {
            Some(command) => channel.exec(true, command.as_str()).await?,
            None => channel.request_shell(true).await?,
        }

        let (mut reader, writer) = channel.split();
        let mut exit_status = 0;

        loop {
            tokio::select! {
                maybe_input = input.recv() => {
                    match maybe_input {
                        Some(PtyInput::Write(bytes)) => writer.data_bytes(bytes).await?,
                        Some(PtyInput::Resize(size)) => {
                            writer
                                .window_change(u32::from(size.cols), u32::from(size.rows), 0, 0)
                                .await?;
                        }
                        None => break,
                    }
                }
                maybe_message = reader.wait() => {
                    match maybe_message {
                        Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                            if events.send(PtyEvent::Output(data.to_vec())).await.is_err() {
                                break;
                            }
                        }
                        Some(ChannelMsg::ExitStatus { exit_status: code }) => {
                            exit_status = code;
                            let _ = events.send(PtyEvent::ExitStatus(code)).await;
                        }
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                        _ => {}
                    }
                }
            }
        }

        let _ = events.send(PtyEvent::Closed).await;
        Ok(exit_status)
    }

    /// Open an SFTP subsystem channel, acquiring one slot from the connection-level
    /// channel semaphore. The permit is released when the returned `SftpHandle` is
    /// dropped (or closed), so callers MUST call `.close()` or let it drop when done.
    async fn open_sftp_guarded(&self) -> Result<SftpHandle, SshError> {
        self.open_sftp_guarded_with(russh_sftp::client::Config::default())
            .await
    }

    /// Same as [`Self::open_sftp_guarded`] but with explicit protocol settings,
    /// so bulk transfers can trade the interactive request deadline for one that
    /// a slow link cannot trip (see [`TRANSFER_REQUEST_TIMEOUT`]).
    async fn open_sftp_guarded_with(
        &self,
        config: russh_sftp::client::Config,
    ) -> Result<SftpHandle, SshError> {
        let _permit = self
            .channel_sem
            .clone()
            .acquire_owned()
            .await
            .expect("channel semaphore closed");
        let channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        let inner = SftpSession::new_with_config(channel.into_stream(), config).await?;
        Ok(SftpHandle { inner, _permit })
    }

    /// Protocol settings for bulk SFTP traffic.
    fn transfer_sftp_config() -> russh_sftp::client::Config {
        russh_sftp::client::Config {
            request_timeout_secs: TRANSFER_REQUEST_TIMEOUT.as_secs(),
            max_concurrent_writes: TRANSFER_WRITE_WINDOW,
            ..Default::default()
        }
    }

    /// Public entry point kept for external users (e.g. tests). Internally
    /// prefer `open_sftp_guarded` so the semaphore is always respected.
    pub async fn open_sftp(&self) -> Result<SftpSession, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        Ok(SftpSession::new(channel.into_stream()).await?)
    }

    /// Resolve a (possibly relative) remote path to its absolute form.
    pub async fn canonicalize(&self, path: &str) -> Result<String, SshError> {
        bounded(OP_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result = sftp.canonicalize(path).await.map_err(SshError::from);
            let _ = sftp.close().await;
            result
        })
        .await
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<RemoteFileEntry>, SshError> {
        Ok(self.list_dir_resolved(path).await?.1)
    }

    /// List `path`, canonicalizing it first *only when needed* (relative paths
    /// like the initial "."), reusing a single SFTP channel for both the
    /// canonicalize and the read_dir. Returns `(resolved_path, entries)`.
    ///
    /// Navigation always builds absolute paths (`join_remote`/`parent_remote`),
    /// so after the first connect the canonicalize round-trip is pure overhead;
    /// skipping it — and not opening a second channel — removes the per-folder
    /// stall.
    pub async fn list_dir_resolved(
        &self,
        path: &str,
    ) -> Result<(String, Vec<RemoteFileEntry>), SshError> {
        bounded(OP_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result: Result<(String, Vec<RemoteFileEntry>), SshError> = async {
                // Absolute paths are already resolved; only relative ones (".",
                // "..", "foo/bar") need a canonicalize round-trip.
                let resolved = if path.starts_with('/') {
                    path.to_string()
                } else {
                    sftp.canonicalize(path)
                        .await
                        .unwrap_or_else(|_| path.to_string())
                };
                let mut entries = sftp
                    .read_dir(&resolved)
                    .await?
                    .map(|entry| {
                        let metadata = entry.metadata();
                        RemoteFileEntry {
                            name: entry.file_name(),
                            path: entry.path(),
                            kind: remote_file_kind(metadata.file_type()),
                            size: metadata.size,
                            permissions: metadata.permissions,
                            modified: metadata.mtime,
                        }
                    })
                    .collect::<Vec<_>>();
                entries.sort_by(|a, b| {
                    let a_dir = matches!(a.kind, RemoteFileKind::Directory);
                    let b_dir = matches!(b.kind, RemoteFileKind::Directory);
                    b_dir.cmp(&a_dir).then_with(|| a.name.cmp(&b.name))
                });
                Ok((resolved, entries))
            }
            .await;
            let _ = sftp.close().await;
            result
        })
        .await
    }

    pub async fn read_file(&self, remote_path: &str) -> Result<Vec<u8>, SshError> {
        bounded(CONTENT_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result = sftp.read(remote_path).await.map_err(SshError::from);
            let _ = sftp.close().await;
            result
        })
        .await
    }

    /// Read `len` bytes starting at `offset` from a remote file.
    pub async fn read_file_range(
        &self,
        remote_path: &str,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, u64), SshError> {
        bounded(OP_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result: Result<(Vec<u8>, u64), SshError> = async {
                let total = sftp.metadata(remote_path).await?.size.unwrap_or(0);
                let mut file = sftp.open(remote_path).await?;
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                let cap = len.min(total.saturating_sub(offset)) as usize;
                let mut buf = vec![0u8; cap];
                let mut pos = 0;
                while pos < cap {
                    let n = file.read(&mut buf[pos..]).await?;
                    if n == 0 {
                        break;
                    }
                    pos += n;
                }
                buf.truncate(pos);
                Ok((buf, total))
            }
            .await;
            let _ = sftp.close().await;
            result
        })
        .await
    }

    pub async fn write_file(&self, remote_path: &str, bytes: Vec<u8>) -> Result<(), SshError> {
        bounded(CONTENT_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result: Result<(), SshError> = async {
                let mut file = sftp.create(remote_path).await?;
                file.write_all(&bytes).await?;
                file.shutdown().await?;
                Ok(())
            }
            .await;
            let _ = sftp.close().await;
            result
        })
        .await
    }

    /// Size of a remote file in bytes (0 if unknown).
    pub async fn remote_file_size(&self, remote_path: &str) -> Result<u64, SshError> {
        bounded(OP_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result = sftp
                .metadata(remote_path)
                .await
                .map(|m| m.size.unwrap_or(0))
                .map_err(SshError::from);
            let _ = sftp.close().await;
            result
        })
        .await
    }

    /// Download a remote file to a local path, resumably, with an adaptive read
    /// window and automatic retry.
    ///
    /// Bytes land in `<local>.part`; on success it is renamed over `<local>`.
    /// A `.part` that is a valid prefix is resumed rather than restarted, and so
    /// is every retry — a link that breaks mid-file costs one backoff instead of
    /// the whole transfer. Cumulative bytes (including any resumed prefix) are
    /// reported over `progress`. Returns the file size.
    pub async fn download_file(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
        progress: mpsc::Sender<u64>,
        stop: Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<u64, SshError> {
        let mut attempt = 0;
        let mut backoff = TRANSFER_RETRY_BACKOFF;
        loop {
            attempt += 1;
            let outcome = self
                .download_attempt(remote_path, local_path, progress.clone(), stop.clone())
                .await;
            let error = match outcome {
                Ok(bytes) => return Ok(bytes),
                Err(error) => error,
            };
            let cancelled = stop.load(std::sync::atomic::Ordering::Relaxed) != 0;
            if cancelled || !error.is_transient() || attempt >= TRANSFER_ATTEMPTS {
                return Err(error);
            }
            // `download_attempt` left the `.part` as a contiguous prefix, so the
            // next attempt continues from a file that is exactly right.
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(TRANSFER_RETRY_BACKOFF_MAX);
        }
    }

    /// One pass of [`Self::download_file`]: stream `[resume, total)` into the
    /// `.part` file, then either promote it or cut it back to the longest
    /// *contiguous* completed prefix so a later resume can never skip a hole.
    async fn download_attempt(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
        progress: mpsc::Sender<u64>,
        stop: Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<u64, SshError> {
        use std::os::unix::fs::FileExt;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        // Queue behind other bulk transfers FIRST (see `bulk_sem`): waiting
        // here keeps channel permits free for interactive SFTP navigation.
        let _bulk_permit = self
            .bulk_sem
            .clone()
            .acquire_owned()
            .await
            .expect("bulk semaphore closed");

        // Acquire a channel slot before opening the raw SFTP subsystem.
        let _chan_permit = self
            .channel_sem
            .clone()
            .acquire_owned()
            .await
            .expect("channel semaphore closed");

        // A dedicated SFTP channel as a RawSftpSession for positioned reads,
        // carrying the bulk request deadline rather than the interactive default.
        let channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        let raw = Arc::new(RawSftpSession::new_with_config(
            channel.into_stream(),
            Self::transfer_sftp_config(),
        ));
        raw.init().await?;

        let total = raw.stat(remote_path).await?.attrs.size.unwrap_or(0);

        // Resume from an existing `.part` when it is a valid prefix; otherwise
        // start fresh (a stale/oversized `.part` is truncated).
        let part = part_path(local_path);
        let existing = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let resume = if existing > 0 && existing <= total {
            existing
        } else {
            0
        };

        let file = Arc::new(
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(resume == 0)
                .open(&part)?,
        );

        let handle = raw
            .open(remote_path, OpenFlags::READ, FileAttributes::default())
            .await?
            .handle;

        // Single throttled reporter task → monotonic cumulative progress even
        // though chunks complete out of order across the window.
        let counter = Arc::new(AtomicU64::new(resume));
        let done = Arc::new(AtomicBool::new(false));
        let reporter = {
            let (counter, done, progress) = (counter.clone(), done.clone(), progress.clone());
            tokio::spawn(async move {
                loop {
                    let _ = progress.send(counter.load(Ordering::Relaxed)).await;
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let _ = progress.send(counter.load(Ordering::Relaxed)).await;
            })
        };

        // One flag per chunk records what is *known* written. Windowed writes
        // land at absolute offsets and finish out of order, so the file length
        // alone would let a later resume skip a hole and silently corrupt the
        // file; the flags give the longest prefix that is genuinely contiguous.
        let remaining = total - resume;
        let chunk_count = remaining.div_ceil(TRANSFER_CHUNK) as usize;
        let completed: Arc<Vec<AtomicBool>> =
            Arc::new((0..chunk_count).map(|_| AtomicBool::new(false)).collect());
        let watermark = |completed: &[AtomicBool]| -> u64 {
            let mut contiguous = 0_usize;
            while contiguous < completed.len() && completed[contiguous].load(Ordering::Acquire) {
                contiguous += 1;
            }
            resume + (contiguous as u64 * TRANSFER_CHUNK).min(remaining)
        };

        let mut stopped = false;
        let mut failure: Option<SshError> = None;
        let mut offset = resume;
        // Start shallow: the first window has no measured rate to size it from,
        // and guessing deep is precisely what queues an unservable backlog on a
        // slow link.
        let mut window = TRANSFER_WINDOW_MIN;

        while offset < total {
            // Cooperative stop: quit spawning new chunks, then fall through to
            // drain whatever is already in flight. A nonzero stop token means
            // pause or cancel (both stop here; the caller decides what to do with
            // the `.part`).
            if stop.load(Ordering::Relaxed) != 0 {
                stopped = true;
                break;
            }

            let batch_start = offset;
            let batch_end = (offset + window as u64 * TRANSFER_CHUNK).min(total);
            let started = std::time::Instant::now();
            let mut set = tokio::task::JoinSet::new();
            let mut index = ((offset - resume) / TRANSFER_CHUNK) as usize;
            while offset < batch_end {
                let end = (offset + TRANSFER_CHUNK).min(total);
                let chunk_start = offset;
                let chunk_index = index;
                let (raw, handle, file, counter, completed) = (
                    raw.clone(),
                    handle.clone(),
                    file.clone(),
                    counter.clone(),
                    completed.clone(),
                );
                set.spawn(async move {
                    let mut cur = chunk_start;
                    while cur < end {
                        let want = (end - cur) as u32;
                        let data = raw.read(handle.clone(), cur, want).await?;
                        if data.data.is_empty() {
                            break; // unexpected early EOF
                        }
                        let read = data.data.len();
                        file.write_all_at(&data.data, cur)?;
                        cur += read as u64;
                        counter.fetch_add(read as u64, Ordering::Relaxed);
                    }
                    // Only a fully fetched chunk is safe to resume past.
                    completed[chunk_index].store(true, Ordering::Release);
                    Ok::<(), SshError>(())
                });
                offset = end;
                index += 1;
            }

            let mut aborted = false;
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        if failure.is_none() {
                            failure = Some(error);
                        }
                        set.abort_all();
                    }
                    // Aborted or panicked chunk: it never set its flag, so the
                    // watermark stays before it.
                    Err(_) => aborted = true,
                }
            }
            if failure.is_some() {
                break;
            }
            if aborted {
                if stop.load(Ordering::Relaxed) != 0 {
                    stopped = true;
                    break;
                }
                // A chunk vanished without reporting an error. The bytes it owed
                // are missing, so this must not be treated as a finished file:
                // fail (and let the retry resume from the watermark) rather than
                // promote a `.part` with a hole in it.
                failure = Some(SshError::Io(std::io::Error::other(
                    "download chunk ended without completing",
                )));
                break;
            }

            // Size the next window from what this one actually achieved: keep the
            // deepest queued read inside `TRANSFER_TAIL_TARGET`. Shrink at once
            // (safety), grow at most 2x per window (stability).
            let elapsed = started.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let bytes_per_sec = (batch_end - batch_start) as f64 / elapsed;
                let affordable = (bytes_per_sec * TRANSFER_TAIL_TARGET.as_secs_f64()
                    / TRANSFER_CHUNK as f64) as usize;
                window = affordable
                    .clamp(TRANSFER_WINDOW_MIN, TRANSFER_WINDOW_MAX)
                    .min(window.saturating_mul(2));
            }
        }

        done.store(true, Ordering::Relaxed);
        let _ = reporter.await;
        // Bounded teardown: on an unresponsive session a plain `close` would wait
        // out the bulk request deadline before returning.
        let _ = bounded(OP_TIMEOUT, async {
            let _ = raw.close(handle).await;
            Ok::<(), SshError>(())
        })
        .await;
        let _ = raw.close_session();

        if failure.is_none() && !stopped {
            // Flush to disk and promote `.part` → final (overwriting any old file).
            file.sync_all()?;
            drop(file);
            tokio::fs::rename(&part, local_path).await?;
            return Ok(total);
        }

        // Paused, cancelled or failed: keep the `.part` as a clean resumable
        // prefix, truncated to the watermark so no hole survives.
        let keep = watermark(&completed[..]);
        file.set_len(keep)?;
        file.sync_all()?;
        drop(file);
        match failure {
            Some(error) => Err(error),
            None => Ok(keep),
        }
    }

    /// Upload a local file to a remote path, resumably and with automatic retry.
    ///
    /// Bytes land in `<remote>.part`; on success it is renamed over `<remote>`.
    /// A remote `.part` that is a valid prefix is resumed rather than restarted,
    /// and so is every retry. Writes are pipelined by russh-sftp's `File` (up to
    /// [`TRANSFER_WRITE_WINDOW`] WRITE packets in flight) under the bulk request
    /// deadline. Reports cumulative bytes (including any resumed prefix).
    pub async fn upload_file(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
        progress: mpsc::Sender<u64>,
        stop: Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<u64, SshError> {
        let mut attempt = 0;
        let mut backoff = TRANSFER_RETRY_BACKOFF;
        loop {
            attempt += 1;
            let outcome = self
                .upload_attempt(local_path, remote_path, progress.clone(), stop.clone())
                .await;
            let error = match outcome {
                Ok(bytes) => return Ok(bytes),
                Err(error) => error,
            };
            let cancelled = stop.load(std::sync::atomic::Ordering::Relaxed) != 0;
            if cancelled || !error.is_transient() || attempt >= TRANSFER_ATTEMPTS {
                return Err(error);
            }
            // The remote `.part` size is authoritative, so the next attempt
            // continues from exactly the bytes the server acknowledged.
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(TRANSFER_RETRY_BACKOFF_MAX);
        }
    }

    /// One pass of [`Self::upload_file`].
    async fn upload_attempt(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
        progress: mpsc::Sender<u64>,
        stop: Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<u64, SshError> {
        use std::sync::atomic::Ordering;
        // Queue behind other bulk transfers FIRST (see `bulk_sem`): waiting
        // here keeps channel permits free for interactive SFTP navigation.
        let _bulk_permit = self
            .bulk_sem
            .clone()
            .acquire_owned()
            .await
            .expect("bulk semaphore closed");
        let sftp = self
            .open_sftp_guarded_with(Self::transfer_sftp_config())
            .await?;
        let result: Result<u64, SshError> = async {
            let total = tokio::fs::metadata(local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let part = format!("{remote_path}.part");
            let existing = match sftp.metadata(&part).await {
                Ok(m) => m.size.unwrap_or(0),
                Err(_) => 0,
            };
            let resume = if existing > 0 && existing <= total {
                existing
            } else {
                0
            };
            let mut local = tokio::fs::File::open(local_path).await?;
            let mut remote = if resume > 0 {
                let mut f = sftp
                    .open_with_flags(&part, OpenFlags::WRITE | OpenFlags::CREATE)
                    .await?;
                local.seek(std::io::SeekFrom::Start(resume)).await?;
                f.seek(std::io::SeekFrom::Start(resume)).await?;
                f
            } else {
                sftp.open_with_flags(
                    &part,
                    OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
                )
                .await?
            };
            let mut transferred = resume;
            let _ = progress.send(transferred).await;
            let mut buffer = vec![0_u8; 256 * 1024];
            let mut stopped = false;
            loop {
                // Cooperative stop: the sequential loop breaks at a clean byte
                // boundary; the `.part` on the remote is a valid prefix that a
                // later resume continues from. Nonzero token = pause or cancel.
                if stop.load(Ordering::Relaxed) != 0 {
                    stopped = true;
                    break;
                }
                let read = local.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                remote.write_all(&buffer[..read]).await?;
                transferred += read as u64;
                let _ = progress.send(transferred).await;
            }
            // Bounded, but NOT ignored: promoting the `.part` is only safe once
            // the server acknowledged every write, so a failed or stalled flush
            // must fail the attempt (the retry then resumes from the remote
            // `.part` size) instead of renaming a short file into place. Bounding
            // it keeps a dead session from waiting out the bulk deadline.
            bounded(OP_TIMEOUT, async {
                remote.flush().await?;
                remote.shutdown().await?;
                Ok::<(), SshError>(())
            })
            .await?;
            if stopped {
                // Leave `.part` in place for resume; don't promote to final.
                return Ok(transferred);
            }
            let _ = sftp.remove_file(remote_path).await;
            sftp.rename(&part, remote_path).await?;
            Ok(transferred)
        }
        .await;
        let _ = sftp.close().await;
        result
    }

    pub async fn create_dir(&self, remote_path: &str) -> Result<(), SshError> {
        bounded(OP_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result = sftp.create_dir(remote_path).await.map_err(SshError::from);
            let _ = sftp.close().await;
            result
        })
        .await
    }

    pub async fn remove_path(
        &self,
        remote_path: &str,
        kind: RemoteFileKind,
    ) -> Result<(), SshError> {
        bounded(CONTENT_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result = match kind {
                RemoteFileKind::Directory => remove_dir_recursive(&sftp, remote_path).await,
                RemoteFileKind::File | RemoteFileKind::Symlink | RemoteFileKind::Other => {
                    sftp.remove_file(remote_path).await.map_err(SshError::from)
                }
            };
            let _ = sftp.close().await;
            result
        })
        .await
    }

    pub async fn rename_path(&self, old_path: &str, new_path: &str) -> Result<(), SshError> {
        bounded(OP_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let result = sftp
                .rename(old_path, new_path)
                .await
                .map_err(SshError::from);
            let _ = sftp.close().await;
            result
        })
        .await
    }

    /// Change the permissions of a remote file (SFTP setstat).
    pub async fn chmod_path(&self, path: &str, mode: u32) -> Result<(), SshError> {
        bounded(OP_TIMEOUT, async {
            let sftp = self.open_sftp_guarded().await?;
            let mut attrs = FileAttributes::default();
            attrs.permissions = Some(mode);
            let result = sftp.set_metadata(path, attrs).await.map_err(SshError::from);
            let _ = sftp.close().await;
            result
        })
        .await
    }

    pub async fn run_local_forward(
        self,
        forward: LocalForwardOptions,
        events: mpsc::Sender<ForwardEvent>,
        stop: &mut mpsc::Receiver<()>,
    ) -> Result<(), SshError> {
        let listener = TcpListener::bind((forward.bind_host.as_str(), forward.bind_port)).await?;
        let bind_port = listener.local_addr()?.port();
        let _ = events
            .send(ForwardEvent::Listening {
                bind_host: forward.bind_host.clone(),
                bind_port,
            })
            .await;

        let session = Arc::new(Mutex::new(self.handle));
        loop {
            tokio::select! {
                _ = stop.recv() => {
                    let _ = session.lock().await
                        .disconnect(Disconnect::ByApplication, "local forward stopped", "English")
                        .await;
                    let _ = events.send(ForwardEvent::Stopped).await;
                    return Ok(());
                }
                accepted = listener.accept() => {
                    let (stream, peer_addr) = accepted?;
                    let peer = peer_addr.to_string();
                    let _ = events
                        .send(ForwardEvent::ConnectionAccepted { peer: peer.clone() })
                        .await;

                    let session = session.clone();
                    let events = events.clone();
                    let remote_host = forward.remote_host.clone();
                    let remote_port = forward.remote_port;
                    tokio::spawn(async move {
                        let result = forward_tcp_stream(
                            session,
                            stream,
                            peer.clone(),
                            remote_host,
                            remote_port,
                        )
                        .await;
                        match result {
                            Ok(()) => {
                                let _ = events.send(ForwardEvent::ConnectionClosed { peer }).await;
                            }
                            Err(error) => {
                                let _ = events.send(ForwardEvent::Failed(error.to_string())).await;
                            }
                        }
                    });
                }
            }
        }
    }

}

async fn forward_tcp_stream(
    handle: Arc<Mutex<client::Handle<ClientHandler>>>,
    mut stream: TcpStream,
    peer: String,
    remote_host: String,
    remote_port: u16,
) -> Result<(), SshError> {
    let originator = stream.peer_addr().ok();
    let mut channel = handle
        .lock()
        .await
        .channel_open_direct_tcpip(
            remote_host,
            u32::from(remote_port),
            originator
                .map(|addr| addr.ip().to_string())
                .unwrap_or_else(|| peer.clone()),
            originator.map(|addr| u32::from(addr.port())).unwrap_or(0),
        )
        .await?;

    let mut stream_closed = false;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        tokio::select! {
            read = stream.read(&mut buffer), if !stream_closed => {
                match read {
                    Ok(0) => {
                        stream_closed = true;
                        channel.eof().await?;
                    }
                    Ok(n) => channel.data(&buffer[..n]).await?,
                    Err(error) => return Err(SshError::Io(error)),
                }
            }
            maybe_message = channel.wait() => {
                match maybe_message {
                    Some(ChannelMsg::Data { data }) => stream.write_all(&data).await?,
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    Some(ChannelMsg::WindowAdjusted { .. }) => {}
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn remote_file_kind(kind: FileType) -> RemoteFileKind {
    match kind {
        FileType::Dir => RemoteFileKind::Directory,
        FileType::File => RemoteFileKind::File,
        FileType::Symlink => RemoteFileKind::Symlink,
        FileType::Other => RemoteFileKind::Other,
    }
}

/// The temporary `.part` sibling a resumable download streams into before it is
/// renamed over the final destination.
fn part_path(local_path: &std::path::Path) -> PathBuf {
    let mut name = local_path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    local_path.with_file_name(name)
}

/// Recursively delete a remote directory and everything inside it. SFTP's
/// `rmdir` only removes *empty* directories, so we must walk the tree: delete
/// each child (recursing into subdirectories) before removing the dir itself.
/// Boxed because async fns can't recurse directly.
fn remove_dir_recursive<'a>(
    sftp: &'a SftpSession,
    path: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SshError>> + Send + 'a>> {
    Box::pin(async move {
        let entries = sftp.read_dir(path).await?;
        for entry in entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            // `path()` joins the dir we passed to read_dir with the entry name.
            let child = entry.path();
            match entry.file_type() {
                FileType::Dir => remove_dir_recursive(sftp, &child).await?,
                _ => sftp.remove_file(&child).await?,
            }
        }
        sftp.remove_dir(path).await?;
        Ok(())
    })
}

#[derive(Debug, Clone)]
struct ClientHandler {
    host: String,
    port: u16,
    policy: HostKeyPolicy,
}

impl client::Handler for ClientHandler {
    type Error = ClientHandlerError;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match &self.policy {
            HostKeyPolicy::TrustAll => Ok(true),
            HostKeyPolicy::Strict { known_hosts } => {
                Ok(
                    check_known_hosts_path(&self.host, self.port, server_public_key, known_hosts)
                        .unwrap_or(false),
                )
            }
            HostKeyPolicy::AcceptNew { known_hosts } => {
                match check_known_hosts_path(&self.host, self.port, server_public_key, known_hosts)
                {
                    Ok(true) => Ok(true),
                    Ok(false) => {
                        let _ = learn_known_hosts_path(
                            &self.host,
                            self.port,
                            server_public_key,
                            known_hosts,
                        );
                        Ok(true)
                    }
                    Err(_) => Ok(false),
                }
            }
            HostKeyPolicy::ConfirmNew { known_hosts } => {
                match check_known_hosts_path(&self.host, self.port, server_public_key, known_hosts)
                {
                    Ok(true) => Ok(true),
                    Ok(false) => Err(ClientHandlerError::HostKeyVerificationRequired(Box::new(
                        HostKeyChallenge {
                            host: self.host.clone(),
                            port: self.port,
                            algorithm: server_public_key.algorithm().to_string(),
                            fingerprint: server_public_key
                                .fingerprint(Default::default())
                                .to_string(),
                            known_hosts: known_hosts.clone(),
                            public_key: server_public_key.to_openssh().map_err(|error| {
                                ClientHandlerError::Russh(russh::Error::Keys(error.into()))
                            })?,
                        },
                    ))),
                    Err(_) => Ok(false),
                }
            }
        }
    }
}

/// Turn on TCP keepalives for a freshly connected socket.
///
/// The SSH keepalive is a *user-space* timer. If the app's timers are throttled
/// (macOS App Nap once the window is left alone) or the process is descheduled,
/// they stop firing, and a stateful firewall or NAT on the path is then free to
/// forget the idle flow — which is exactly the "left the terminal alone and SSH
/// dropped by itself" report. TCP keepalives are emitted by the kernel, so they
/// keep that flow warm no matter what the app does.
///
/// The platform defaults cannot be relied on: `SO_KEEPALIVE` is off unless it is
/// set explicitly, and macOS' idle time defaults to two hours.
#[cfg(unix)]
fn configure_tcp_keepalive(stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    unsafe fn set(
        fd: libc::c_int,
        level: libc::c_int,
        name: libc::c_int,
        value: libc::c_int,
    ) -> std::io::Result<()> {
        // SAFETY: `fd` is a live socket owned by `stream` for the duration of the
        // call, and `value` is a correctly sized `c_int` for these options.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                level,
                name,
                std::ptr::addr_of!(value).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    let fd = stream.as_raw_fd();
    unsafe {
        set(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1)?;
        // Only macOS and Linux name the idle-time knob in libc; elsewhere
        // `SO_KEEPALIVE` alone is still better than nothing.
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
        {
            #[cfg(target_os = "macos")]
            let idle_option = libc::TCP_KEEPALIVE;
            #[cfg(not(target_os = "macos"))]
            let idle_option = libc::TCP_KEEPIDLE;
            set(
                fd,
                libc::IPPROTO_TCP,
                idle_option,
                TCP_KEEPALIVE_IDLE.as_secs() as libc::c_int,
            )?;
            set(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 15)?;
            set(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 4)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn configure_tcp_keepalive(_stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    Ok(())
}

fn map_handler_error(error: ClientHandlerError) -> SshError {
    match error {
        ClientHandlerError::Russh(error) => SshError::Connection(error.to_string()),
        ClientHandlerError::HostKeyVerificationRequired(challenge) => {
            SshError::HostKeyVerificationRequired(challenge)
        }
    }
}

fn default_private_key_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("id_ed25519")
}

async fn authenticate_private_key(
    handle: &mut client::Handle<ClientHandler>,
    username: String,
    path: PathBuf,
    passphrase: Option<String>,
) -> Result<client::AuthResult, SshError> {
    let key = load_secret_key(path, passphrase.as_deref())?;
    let rsa_hash = handle.best_supported_rsa_hash().await?.flatten();
    Ok(handle
        .authenticate_publickey(
            username,
            PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash),
        )
        .await?)
}

async fn authenticate_handle(
    handle: &mut client::Handle<ClientHandler>,
    username: String,
    auth: AuthMethod,
) -> Result<client::AuthResult, SshError> {
    match auth {
        AuthMethod::Password(password) => {
            Ok(handle.authenticate_password(username, password).await?)
        }
        AuthMethod::AgentOrDefault => authenticate_agent_or_default_key(handle, username).await,
        AuthMethod::DefaultKey => {
            authenticate_private_key(handle, username, default_private_key_path(), None).await
        }
        AuthMethod::PrivateKey { path, passphrase } => {
            authenticate_private_key(handle, username, path, passphrase).await
        }
    }
}

async fn authenticate_agent_or_default_key(
    handle: &mut client::Handle<ClientHandler>,
    username: String,
) -> Result<client::AuthResult, SshError> {
    match authenticate_agent(handle, &username).await {
        Ok(result) if result.success() => return Ok(result),
        Ok(_) => {}
        Err(_) => {}
    }

    authenticate_private_key(handle, username, default_private_key_path(), None).await
}

#[cfg(unix)]
async fn authenticate_agent(
    handle: &mut client::Handle<ClientHandler>,
    username: &str,
) -> Result<client::AuthResult, SshError> {
    let mut agent = AgentClient::connect_env()
        .await
        .map_err(|error| SshError::Agent(error.to_string()))?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| SshError::Agent(error.to_string()))?;
    let rsa_hash = handle.best_supported_rsa_hash().await?.flatten();

    for identity in identities {
        let result = handle
            .authenticate_publickey_with(
                username.to_string(),
                identity.public_key().into_owned(),
                rsa_hash,
                &mut agent,
            )
            .await
            .map_err(|error| SshError::Agent(error.to_string()))?;
        if result.success() {
            return Ok(result);
        }
    }

    Err(SshError::Authentication)
}

#[cfg(not(unix))]
async fn authenticate_agent(
    _handle: &mut client::Handle<ClientHandler>,
    _username: &str,
) -> Result<client::AuthResult, SshError> {
    Err(SshError::Agent(
        "ssh-agent authentication is not supported on this platform yet".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read an integer socket option back from a live socket.
    #[cfg(unix)]
    fn socket_option(
        stream: &tokio::net::TcpStream,
        level: libc::c_int,
        name: libc::c_int,
    ) -> libc::c_int {
        use std::os::unix::io::AsRawFd;
        let mut value: libc::c_int = -1;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `value`/`len` describe a `c_int` buffer that `getsockopt`
        // fills in, and the fd is live for the duration of the call.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                level,
                name,
                std::ptr::addr_of_mut!(value).cast(),
                &mut len,
            )
        };
        assert_eq!(
            rc,
            0,
            "getsockopt failed: {}",
            std::io::Error::last_os_error()
        );
        value
    }

    /// The kernel has to be what keeps an idle connection warm, because the SSH
    /// keepalive is a user-space timer that macOS App Nap can throttle once the
    /// window is left alone. Both settings are off by default (`SO_KEEPALIVE` is
    /// opt-in and macOS' idle time is two hours), so assert they are really set
    /// rather than merely intended.
    #[cfg(unix)]
    #[tokio::test]
    async fn tcp_keepalives_are_enabled_on_the_connect_socket() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let dialing = tokio::spawn(async move { tokio::net::TcpStream::connect(addr).await });
        let (_server_side, _) = listener.accept().await.expect("accept");
        let client = dialing
            .await
            .expect("join dial task")
            .expect("connect client socket");

        // BSD reports SO_KEEPALIVE as a bitmask rather than a boolean: off reads
        // back 0 and on reads back a nonzero bit (8 on macOS), so "changed from
        // off to on" is the assertion that means something.
        assert_eq!(
            socket_option(&client, libc::SOL_SOCKET, libc::SO_KEEPALIVE),
            0,
            "a fresh socket must start with keepalives off, or this test proves nothing"
        );
        configure_tcp_keepalive(&client).expect("enable TCP keepalives");
        assert_ne!(
            socket_option(&client, libc::SOL_SOCKET, libc::SO_KEEPALIVE),
            0,
            "SO_KEEPALIVE must be on, otherwise the kernel sends nothing while idle"
        );
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
        {
            #[cfg(target_os = "macos")]
            let idle_option = libc::TCP_KEEPALIVE;
            #[cfg(not(target_os = "macos"))]
            let idle_option = libc::TCP_KEEPIDLE;
            assert_eq!(
                socket_option(&client, libc::IPPROTO_TCP, idle_option) as u64,
                TCP_KEEPALIVE_IDLE.as_secs(),
                "keepalive idle time must be set, not left at the OS default"
            );
            assert_eq!(
                socket_option(&client, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL),
                15
            );
            assert_eq!(
                socket_option(&client, libc::IPPROTO_TCP, libc::TCP_KEEPCNT),
                4
            );
        }
    }

    /// A server verdict is final; a broken link is worth another attempt.
    #[test]
    fn transient_errors_are_distinguished_from_server_verdicts() {
        assert!(SshError::Timeout.is_transient());
        assert!(SshError::Io(std::io::Error::other("reset")).is_transient());
        assert!(
            SshError::Sftp(russh_sftp::client::error::Error::Timeout).is_transient(),
            "an SFTP timeout is what a congested link produces"
        );
        assert!(
            !SshError::Sftp(russh_sftp::client::error::Error::Limited("too big".into()))
                .is_transient()
        );
        assert!(!SshError::Authentication.is_transient());
    }

    /// The bulk deadline exists because russh-sftp's 10s default is a queue
    /// budget, not a stall detector: a deep read window on a slow link times out
    /// the tail of every window and the transfer dies while the server keeps
    /// streaming. Interactive operations keep the tight default so a hung server
    /// still fails `ls` quickly.
    #[test]
    fn bulk_sftp_traffic_gets_a_deadline_a_slow_link_cannot_trip() {
        let bulk = RusshSession::transfer_sftp_config();
        let interactive = russh_sftp::client::Config::default();
        assert_eq!(
            bulk.request_timeout_secs,
            TRANSFER_REQUEST_TIMEOUT.as_secs()
        );
        assert!(
            bulk.request_timeout_secs >= 10 * interactive.request_timeout_secs,
            "bulk deadline must be far looser than the interactive one"
        );
        assert!(bulk.max_concurrent_writes <= TRANSFER_WINDOW_MAX);
    }

    #[test]
    fn exec_output_keeps_stdout_and_stderr_separate() {
        let output = ExecOutput {
            exit_status: 7,
            stdout: b"ok\n".to_vec(),
            stderr: b"warn\n".to_vec(),
        };

        assert_eq!(output.exit_status, 7);
        assert_eq!(output.stdout, b"ok\n");
        assert_eq!(output.stderr, b"warn\n");
    }

    #[test]
    fn pty_input_can_represent_resize_events() {
        assert_eq!(
            PtyInput::Resize(PtySize {
                cols: 120,
                rows: 40
            }),
            PtyInput::Resize(PtySize {
                cols: 120,
                rows: 40
            })
        );
    }

    #[test]
    fn agent_or_default_and_default_key_auth_methods_are_explicit() {
        assert_eq!(AuthMethod::AgentOrDefault, AuthMethod::AgentOrDefault);
        assert_eq!(AuthMethod::DefaultKey, AuthMethod::DefaultKey);
        assert_ne!(AuthMethod::AgentOrDefault, AuthMethod::DefaultKey);
        assert_ne!(
            AuthMethod::AgentOrDefault,
            AuthMethod::PrivateKey {
                path: default_private_key_path(),
                passphrase: None,
            }
        );
    }

    #[test]
    fn connect_options_legacy_trust_flag_overrides_host_key_policy() {
        let options = ConnectOptions {
            username: "ubuntu".to_string(),
            auth: AuthMethod::AgentOrDefault,
            trust_unknown_host_keys: true,
            host_key_policy: HostKeyPolicy::Strict {
                known_hosts: PathBuf::from("/tmp/known_hosts"),
            },
            timeout: Duration::from_secs(10),
            keepalive_interval: Some(ConnectOptions::DEFAULT_KEEPALIVE_INTERVAL),
            keepalive_max: ConnectOptions::DEFAULT_KEEPALIVE_MAX,
        };

        assert_eq!(options.effective_host_key_policy(), HostKeyPolicy::TrustAll);
    }
}
