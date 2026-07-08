use async_trait::async_trait;
use openterm_core::HostProfile;
use russh::client;
use russh::keys::agent::client::AgentClient;
use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, FileType, OpenFlags};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

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
    #[error("SSH backend is not implemented yet")]
    NotImplemented,
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
pub struct DynamicForwardOptions {
    pub bind_host: String,
    pub bind_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteForwardOptions {
    pub bind_host: String,
    pub bind_port: u16,
    pub local_host: String,
    pub local_port: u16,
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
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            term: "xterm-256color".to_string(),
            size: PtySize { cols: 80, rows: 24 },
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

#[async_trait]
pub trait SshBackend: Send + Sync {
    async fn connect(&self, profile: HostProfile) -> Result<Box<dyn SshSession>, SshError>;
}

#[async_trait]
pub trait SshSession: Send + Sync {
    async fn open_pty(&self, size: PtySize) -> Result<Box<dyn PtyChannel>, SshError>;
    async fn exec(&mut self, command: &str) -> Result<ExecOutput, SshError>;
    async fn close(&mut self) -> Result<(), SshError>;
}

#[async_trait]
pub trait PtyChannel: Send + Sync {
    async fn write(&self, bytes: &[u8]) -> Result<(), SshError>;
    async fn resize(&self, size: PtySize) -> Result<(), SshError>;
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
            // No inactivity timeout: an idle terminal must not drop the
            // session. Liveness is handled by keepalives (every 30s, dead peer
            // declared after `keepalive_max` unanswered pings ~= 90s).
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(30)),
            nodelay: true,
            ..Default::default()
        });
        let remote_forward_sender = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            host: profile.host.clone(),
            port: profile.port,
            policy: options.effective_host_key_policy(),
            remote_forward_sender: remote_forward_sender.clone(),
        };
        // `options.timeout` guards the initial connect only, not the live session.
        let mut handle = tokio::time::timeout(
            options.timeout,
            client::connect(config, (profile.host.as_str(), profile.port), handler),
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
            remote_forward_sender,
            channel_sem: Arc::new(tokio::sync::Semaphore::new(6)),
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
            // See `connect_with_options`: idle sessions stay alive via keepalives.
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(30)),
            nodelay: true,
            ..Default::default()
        });
        let remote_forward_sender = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            host: target_profile.host.clone(),
            port: target_profile.port,
            policy: target_options.effective_host_key_policy(),
            remote_forward_sender: remote_forward_sender.clone(),
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
            remote_forward_sender,
            channel_sem: Arc::new(tokio::sync::Semaphore::new(6)),
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

    pub async fn event_shell_with_options(
        &self,
        profile: HostProfile,
        options: ConnectOptions,
        shell: ShellOptions,
        mut input: mpsc::Receiver<PtyInput>,
        events: mpsc::Sender<PtyEvent>,
    ) -> Result<u32, SshError> {
        let mut session = self.connect_with_options(profile, options).await?;
        let exit_status = session.event_shell(shell, &mut input, events).await;
        let close_result = session.close().await;
        match (exit_status, close_result) {
            (Ok(exit_status), Ok(())) => Ok(exit_status),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn event_shell_with_route(
        &self,
        route: ConnectRoute,
        shell: ShellOptions,
        mut input: mpsc::Receiver<PtyInput>,
        events: mpsc::Sender<PtyEvent>,
    ) -> Result<u32, SshError> {
        let mut session = self.connect_with_route(route).await?;
        let exit_status = session.event_shell(shell, &mut input, events).await;
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

    pub async fn run_local_forward_with_route(
        &self,
        route: ConnectRoute,
        forward: LocalForwardOptions,
        events: mpsc::Sender<ForwardEvent>,
        mut stop: mpsc::Receiver<()>,
    ) -> Result<(), SshError> {
        let session = self.connect_with_route(route).await?;
        session.run_local_forward(forward, events, &mut stop).await
    }

    pub async fn run_dynamic_forward_with_route(
        &self,
        route: ConnectRoute,
        forward: DynamicForwardOptions,
        events: mpsc::Sender<ForwardEvent>,
        mut stop: mpsc::Receiver<()>,
    ) -> Result<(), SshError> {
        let session = self.connect_with_route(route).await?;
        session
            .run_dynamic_forward(forward, events, &mut stop)
            .await
    }

    pub async fn run_remote_forward_with_route(
        &self,
        route: ConnectRoute,
        forward: RemoteForwardOptions,
        events: mpsc::Sender<ForwardEvent>,
        mut stop: mpsc::Receiver<()>,
    ) -> Result<(), SshError> {
        let session = self.connect_with_route(route).await?;
        session.run_remote_forward(forward, events, &mut stop).await
    }
}

#[async_trait]
impl SshBackend for RusshBackend {
    async fn connect(&self, profile: HostProfile) -> Result<Box<dyn SshSession>, SshError> {
        let username = profile.username.clone().ok_or(SshError::MissingUsername)?;
        let options = ConnectOptions {
            username,
            auth: AuthMethod::PrivateKey {
                path: default_private_key_path(),
                passphrase: None,
            },
            trust_unknown_host_keys: true,
            host_key_policy: HostKeyPolicy::TrustAll,
            timeout: Duration::from_secs(10),
        };
        let session = self.connect_with_options(profile, options).await?;
        Ok(Box::new(session))
    }
}

pub struct RusshSession {
    handle: client::Handle<ClientHandler>,
    jump_handle: Option<client::Handle<ClientHandler>>,
    remote_forward_sender: Arc<Mutex<Option<mpsc::Sender<RemoteForwardChannel>>>>,
    /// Limits concurrent SSH channels so we never exceed the server's
    /// MaxSessions cap (OpenSSH default = 10). Every method that opens a
    /// channel acquires a permit for the channel's lifetime.
    channel_sem: Arc<tokio::sync::Semaphore>,
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

pub struct RusshPtyChannel {
    writer: russh::ChannelWriteHalf<client::Msg>,
}

struct RemoteForwardChannel {
    channel: russh::Channel<client::Msg>,
    peer: String,
}

#[async_trait]
impl SshSession for RusshSession {
    async fn open_pty(&self, size: PtySize) -> Result<Box<dyn PtyChannel>, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel
            .request_pty(
                true,
                "xterm-256color",
                u32::from(size.cols),
                u32::from(size.rows),
                0,
                0,
                &[],
            )
            .await?;
        let (_reader, writer) = channel.split();
        Ok(Box::new(RusshPtyChannel { writer }))
    }

    async fn exec(&mut self, command: &str) -> Result<ExecOutput, SshError> {
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

    async fn close(&mut self) -> Result<(), SshError> {
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
}

impl RusshSession {
    /// Disconnect the session through a shared reference. Unlike [`SshSession::close`],
    /// this does not require ownership, so an actor holding `Arc<RusshSession>` can tear
    /// the connection down while shell and SFTP channels are multiplexed over it.
    pub async fn disconnect(&self) -> Result<(), SshError> {
        let timeout = std::time::Duration::from_secs(5);
        let _ = tokio::time::timeout(
            timeout,
            self.handle.disconnect(Disconnect::ByApplication, "", "English"),
        ).await;
        if let Some(jump_handle) = &self.jump_handle {
            let _ = tokio::time::timeout(
                timeout,
                jump_handle.disconnect(Disconnect::ByApplication, "", "English"),
            ).await;
        }
        Ok(())
    }

    /// Run a command and capture its output through a shared reference. Opens a
    /// fresh exec channel on the live connection (multiplexed alongside the shell
    /// and SFTP), so an `Arc<RusshSession>` can sample remote state — e.g. the
    /// resource monitor reading `/proc` — without disturbing the interactive PTY.
    pub async fn exec_capture(&self, command: &str) -> Result<ExecOutput, SshError> {
        let _permit = self
            .channel_sem
            .clone()
            .acquire_owned()
            .await
            .expect("channel semaphore closed");
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
        channel.request_shell(true).await?;

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
        let _permit = self
            .channel_sem
            .clone()
            .acquire_owned()
            .await
            .expect("channel semaphore closed");
        let channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        let inner = SftpSession::new(channel.into_stream()).await?;
        Ok(SftpHandle { inner, _permit })
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
        let sftp = self.open_sftp_guarded().await?;
        let result = sftp.canonicalize(path).await.map_err(SshError::from);
        let _ = sftp.close().await;
        result
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
        let sftp = self.open_sftp_guarded().await?;
        let result: Result<(String, Vec<RemoteFileEntry>), SshError> = async {
            // Absolute paths are already resolved; only relative ones (".",
            // "..", "foo/bar") need a canonicalize round-trip.
            let resolved = if path.starts_with('/') {
                path.to_string()
            } else {
                sftp.canonicalize(path).await.unwrap_or_else(|_| path.to_string())
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
        }.await;
        let _ = sftp.close().await;
        result
    }

    pub async fn read_file(&self, remote_path: &str) -> Result<Vec<u8>, SshError> {
        let sftp = self.open_sftp_guarded().await?;
        let result = sftp.read(remote_path).await.map_err(SshError::from);
        let _ = sftp.close().await;
        result
    }

    /// Read `len` bytes starting at `offset` from a remote file.
    pub async fn read_file_range(&self, remote_path: &str, offset: u64, len: u64) -> Result<(Vec<u8>, u64), SshError> {
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
                if n == 0 { break; }
                pos += n;
            }
            buf.truncate(pos);
            Ok((buf, total))
        }.await;
        let _ = sftp.close().await;
        result
    }

    pub async fn write_file(&self, remote_path: &str, bytes: Vec<u8>) -> Result<(), SshError> {
        let sftp = self.open_sftp_guarded().await?;
        let result: Result<(), SshError> = async {
            let mut file = sftp.create(remote_path).await?;
            file.write_all(&bytes).await?;
            file.shutdown().await?;
            Ok(())
        }.await;
        let _ = sftp.close().await;
        result
    }

    /// Size of a remote file in bytes (0 if unknown).
    pub async fn remote_file_size(&self, remote_path: &str) -> Result<u64, SshError> {
        let sftp = self.open_sftp_guarded().await?;
        let result = sftp.metadata(remote_path).await.map(|m| m.size.unwrap_or(0)).map_err(SshError::from);
        let _ = sftp.close().await;
        result
    }

    /// Download a remote file to a local path, streaming in chunks and reporting
    /// cumulative bytes transferred over `progress`. Returns the total bytes
    /// written. The same live SSH connection is reused (no redial).
    /// Download a remote file to a local path, resumably and with pipelining.
    ///
    /// Bytes land in `<local>.part`; on success it is renamed over `<local>`.
    /// If a `.part` already exists, the transfer **resumes** from its length.
    /// Reads are pipelined: up to `WINDOW` positioned `read(offset,len)` requests
    /// are in flight at once (each carries its own absolute offset), and every
    /// chunk is written to the local file at its offset — so throughput scales
    /// with bandwidth, not round-trip latency. Cumulative bytes (including any
    /// resumed prefix) are reported over `progress`. Returns the file size.
    pub async fn download_file(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
        progress: mpsc::Sender<u64>,
        stop: Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<u64, SshError> {
        use std::os::unix::fs::FileExt;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        const CHUNK: u64 = 256 * 1024;
        const WINDOW: usize = 16;

        // Acquire a channel slot before opening the raw SFTP subsystem.
        let _chan_permit = self
            .channel_sem
            .clone()
            .acquire_owned()
            .await
            .expect("channel semaphore closed");

        // A dedicated SFTP channel as a RawSftpSession for positioned reads.
        let channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        let raw = Arc::new(RawSftpSession::new(channel.into_stream()));
        raw.init().await?;

        let total = raw.stat(remote_path).await?.attrs.size.unwrap_or(0);

        // Resume from an existing `.part` when it is a valid prefix; otherwise
        // start fresh (a stale/oversized `.part` is truncated).
        let part = part_path(local_path);
        let existing = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
        let resume = if existing > 0 && existing <= total { existing } else { 0 };

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

        // The highest contiguous offset we've finished spawning work for. When
        // a stop is requested we drain the in-flight window, so everything in
        // `[resume, contiguous_end)` is guaranteed written — the `.part` is
        // then truncated to that watermark so a later resume starts clean
        // (windowed writes could otherwise leave holes past this point).
        let mut contiguous_end = resume;
        let mut stopped = false;

        let outcome: Result<(), SshError> = async {
            if total > resume {
                let sem = Arc::new(tokio::sync::Semaphore::new(WINDOW));
                let mut set = tokio::task::JoinSet::new();
                let mut off = resume;
                while off < total {
                    // Cooperative stop: quit spawning new chunks, then fall
                    // through to drain whatever is already in flight. A nonzero
                    // stop token means pause or cancel (both stop here; the
                    // caller decides what to do with the `.part`).
                    if stop.load(Ordering::Relaxed) != 0 {
                        stopped = true;
                        break;
                    }
                    let end = (off + CHUNK).min(total);
                    let permit = sem.clone().acquire_owned().await.expect("semaphore");
                    let (raw, handle, file, counter) =
                        (raw.clone(), handle.clone(), file.clone(), counter.clone());
                    set.spawn(async move {
                        let _permit = permit;
                        let mut cur = off;
                        while cur < end {
                            let want = (end - cur) as u32;
                            let data = raw.read(handle.clone(), cur, want).await?;
                            if data.data.is_empty() {
                                break; // unexpected early EOF
                            }
                            let n = data.data.len();
                            file.write_all_at(&data.data, cur)?;
                            cur += n as u64;
                            counter.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Ok::<(), SshError>(())
                    });
                    off = end;
                    contiguous_end = off;
                }
                while let Some(joined) = set.join_next().await {
                    joined.map_err(|e| std::io::Error::other(format!("download task failed: {e}")))??;
                }
            }
            Ok(())
        }
        .await;

        done.store(true, Ordering::Relaxed);
        let _ = reporter.await;
        let _ = raw.close(handle).await;
        let _ = raw.close_session();
        outcome?;

        if stopped {
            // Paused/cancelled: keep the `.part` as a clean resumable prefix
            // (truncate off any holes past the contiguous watermark). Do NOT
            // promote to the final name.
            file.set_len(contiguous_end)?;
            file.sync_all()?;
            drop(file);
            return Ok(contiguous_end);
        }

        // Flush to disk and promote `.part` → final (overwriting any old file).
        file.sync_all()?;
        drop(file);
        tokio::fs::rename(&part, local_path).await?;
        Ok(total)
    }

    /// Upload a local file to a remote path, resumably. Bytes land in
    /// `<remote>.part`; on success it is renamed over `<remote>`. If a remote
    /// `.part` already exists, the transfer **resumes** from its length. Writes
    /// are already pipelined by russh-sftp's `File` (it keeps up to
    /// `max_concurrent_writes` WRITE packets in flight), so this streams with a
    /// large buffer. Reports cumulative bytes (including any resumed prefix).
    pub async fn upload_file(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
        progress: mpsc::Sender<u64>,
        stop: Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<u64, SshError> {
        use std::sync::atomic::Ordering;
        let sftp = self.open_sftp_guarded().await?;
        let result: Result<u64, SshError> = async {
            let total = tokio::fs::metadata(local_path).await.map(|m| m.len()).unwrap_or(0);
            let part = format!("{remote_path}.part");
            let existing = match sftp.metadata(&part).await {
                Ok(m) => m.size.unwrap_or(0),
                Err(_) => 0,
            };
            let resume = if existing > 0 && existing <= total { existing } else { 0 };
            let mut local = tokio::fs::File::open(local_path).await?;
            let mut remote = if resume > 0 {
                let mut f = sftp.open_with_flags(&part, OpenFlags::WRITE | OpenFlags::CREATE).await?;
                local.seek(std::io::SeekFrom::Start(resume)).await?;
                f.seek(std::io::SeekFrom::Start(resume)).await?;
                f
            } else {
                sftp.open_with_flags(&part, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE).await?
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
                if read == 0 { break; }
                remote.write_all(&buffer[..read]).await?;
                transferred += read as u64;
                let _ = progress.send(transferred).await;
            }
            remote.flush().await?;
            remote.shutdown().await?;
            if stopped {
                // Leave `.part` in place for resume; don't promote to final.
                return Ok(transferred);
            }
            let _ = sftp.remove_file(remote_path).await;
            sftp.rename(&part, remote_path).await?;
            Ok(transferred)
        }.await;
        let _ = sftp.close().await;
        result
    }

    pub async fn create_dir(&self, remote_path: &str) -> Result<(), SshError> {
        let sftp = self.open_sftp_guarded().await?;
        let result = sftp.create_dir(remote_path).await.map_err(SshError::from);
        let _ = sftp.close().await;
        result
    }

    pub async fn remove_path(
        &self,
        remote_path: &str,
        kind: RemoteFileKind,
    ) -> Result<(), SshError> {
        let sftp = self.open_sftp_guarded().await?;
        let result = match kind {
            RemoteFileKind::Directory => remove_dir_recursive(&sftp, remote_path).await,
            RemoteFileKind::File | RemoteFileKind::Symlink | RemoteFileKind::Other => {
                sftp.remove_file(remote_path).await.map_err(SshError::from)
            }
        };
        let _ = sftp.close().await;
        result
    }

    pub async fn rename_path(&self, old_path: &str, new_path: &str) -> Result<(), SshError> {
        let sftp = self.open_sftp_guarded().await?;
        let result = sftp.rename(old_path, new_path).await.map_err(SshError::from);
        let _ = sftp.close().await;
        result
    }

    /// Change the permissions of a remote file (SFTP setstat).
    pub async fn chmod_path(&self, path: &str, mode: u32) -> Result<(), SshError> {
        let sftp = self.open_sftp_guarded().await?;
        let mut attrs = FileAttributes::default();
        attrs.permissions = Some(mode);
        let result = sftp.set_metadata(path, attrs).await.map_err(SshError::from);
        let _ = sftp.close().await;
        result
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

    pub async fn run_dynamic_forward(
        self,
        forward: DynamicForwardOptions,
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
                        .disconnect(Disconnect::ByApplication, "dynamic forward stopped", "English")
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
                    tokio::spawn(async move {
                        let result = forward_socks5_stream(session, stream, peer.clone()).await;
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

    pub async fn run_remote_forward(
        self,
        forward: RemoteForwardOptions,
        events: mpsc::Sender<ForwardEvent>,
        stop: &mut mpsc::Receiver<()>,
    ) -> Result<(), SshError> {
        let (remote_sender, mut remote_receiver) = mpsc::channel::<RemoteForwardChannel>(100);
        *self.remote_forward_sender.lock().await = Some(remote_sender);
        let allocated_port = self
            .handle
            .tcpip_forward(forward.bind_host.clone(), u32::from(forward.bind_port))
            .await?;
        let bind_port = if forward.bind_port == 0 {
            u16::try_from(allocated_port).map_err(|_| {
                SshError::Connection(format!(
                    "server returned invalid remote port {allocated_port}"
                ))
            })?
        } else {
            forward.bind_port
        };
        let _ = events
            .send(ForwardEvent::Listening {
                bind_host: forward.bind_host.clone(),
                bind_port,
            })
            .await;

        loop {
            tokio::select! {
                _ = stop.recv() => {
                    let _ = self.handle
                        .cancel_tcpip_forward(forward.bind_host.clone(), u32::from(bind_port))
                        .await;
                    *self.remote_forward_sender.lock().await = None;
                    let _ = self.handle
                        .disconnect(Disconnect::ByApplication, "remote forward stopped", "English")
                        .await;
                    let _ = events.send(ForwardEvent::Stopped).await;
                    return Ok(());
                }
                maybe_channel = remote_receiver.recv() => {
                    let Some(remote) = maybe_channel else {
                        let _ = events.send(ForwardEvent::Stopped).await;
                        return Ok(());
                    };
                    let peer = remote.peer.clone();
                    let _ = events
                        .send(ForwardEvent::ConnectionAccepted { peer: peer.clone() })
                        .await;
                    let events = events.clone();
                    let local_host = forward.local_host.clone();
                    let local_port = forward.local_port;
                    tokio::spawn(async move {
                        let result = forward_remote_channel(
                            remote.channel,
                            local_host,
                            local_port,
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

async fn forward_remote_channel(
    mut channel: russh::Channel<client::Msg>,
    local_host: String,
    local_port: u16,
) -> Result<(), SshError> {
    let mut stream = TcpStream::connect((local_host.as_str(), local_port)).await?;
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

async fn forward_socks5_stream(
    handle: Arc<Mutex<client::Handle<ClientHandler>>>,
    mut stream: TcpStream,
    peer: String,
) -> Result<(), SshError> {
    let destination = read_socks5_connect_request(&mut stream).await?;
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    forward_tcp_stream(handle, stream, peer, destination.host, destination.port).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Socks5Destination {
    host: String,
    port: u16,
}

async fn read_socks5_connect_request(
    stream: &mut TcpStream,
) -> Result<Socks5Destination, SshError> {
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting[0] != 0x05 {
        return Err(SshError::Connection(
            "SOCKS5 client sent an unsupported version".to_string(),
        ));
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0x00) {
        stream.write_all(&[0x05, 0xff]).await?;
        return Err(SshError::Authentication);
    }
    stream.write_all(&[0x05, 0x00]).await?;

    read_socks5_destination(stream).await
}

async fn read_socks5_destination<R>(reader: &mut R) -> Result<Socks5Destination, SshError>
where
    R: AsyncRead + Unpin,
{
    let mut request = [0u8; 4];
    reader.read_exact(&mut request).await?;
    if request[0] != 0x05 || request[1] != 0x01 {
        return Err(SshError::Connection(
            "SOCKS5 only supports CONNECT requests".to_string(),
        ));
    }

    let host = match request[3] {
        0x01 => {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes).await?;
            Ipv4Addr::from(bytes).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            reader.read_exact(&mut len).await?;
            let mut bytes = vec![0u8; len[0] as usize];
            reader.read_exact(&mut bytes).await?;
            String::from_utf8(bytes)
                .map_err(|_| SshError::Connection("SOCKS5 domain is not UTF-8".to_string()))?
        }
        0x04 => {
            let mut bytes = [0u8; 16];
            reader.read_exact(&mut bytes).await?;
            Ipv6Addr::from(bytes).to_string()
        }
        _ => {
            return Err(SshError::Connection(
                "SOCKS5 address type is unsupported".to_string(),
            ))
        }
    };
    let mut port = [0u8; 2];
    reader.read_exact(&mut port).await?;

    Ok(Socks5Destination {
        host,
        port: u16::from_be_bytes(port),
    })
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

#[async_trait]
impl PtyChannel for RusshPtyChannel {
    async fn write(&self, bytes: &[u8]) -> Result<(), SshError> {
        self.writer.data_bytes(bytes.to_vec()).await?;
        Ok(())
    }

    async fn resize(&self, size: PtySize) -> Result<(), SshError> {
        self.writer
            .window_change(u32::from(size.cols), u32::from(size.rows), 0, 0)
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ClientHandler {
    host: String,
    port: u16,
    policy: HostKeyPolicy,
    remote_forward_sender: Arc<Mutex<Option<mpsc::Sender<RemoteForwardChannel>>>>,
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

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let sender = self.remote_forward_sender.lock().await.clone();
        if let Some(sender) = sender {
            let _ = sender
                .send(RemoteForwardChannel {
                    channel,
                    peer: format!("{originator_address}:{originator_port}"),
                })
                .await;
        }
        Ok(())
    }
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
        };

        assert_eq!(options.effective_host_key_policy(), HostKeyPolicy::TrustAll);
    }

    #[tokio::test]
    async fn socks5_destination_parses_domain_connect_request() {
        let mut bytes = &[
            0x05, 0x01, 0x00, 0x03, 11, b'e', b'x', b'a', b'm', b'p', b'l', b'e', b'.', b'c', b'o',
            b'm', 0x01, 0xbb,
        ][..];

        let destination = read_socks5_destination(&mut bytes).await.unwrap();

        assert_eq!(
            destination,
            Socks5Destination {
                host: "example.com".to_string(),
                port: 443
            }
        );
    }

    #[tokio::test]
    async fn socks5_destination_parses_ipv4_connect_request() {
        let mut bytes = &[0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1, 0x04, 0x38][..];

        let destination = read_socks5_destination(&mut bytes).await.unwrap();

        assert_eq!(
            destination,
            Socks5Destination {
                host: "127.0.0.1".to_string(),
                port: 1080
            }
        );
    }
}
