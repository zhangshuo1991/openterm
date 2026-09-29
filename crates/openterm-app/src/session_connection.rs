//! Which connection a session's work runs on.
//!
//! Two roles, deliberately separated:
//!
//! - **Primary**: one long-lived connection carrying the terminal PTY and the
//!   interactive SFTP operations (list, mkdir, chmod, rename, metadata) that have
//!   to feel instant while the user navigates.
//! - **Data connections**: bulk uploads/downloads run on their own connection,
//!   borrowed from [`SshConnectionPool`] for the duration of the transfer and
//!   returned when it ends.
//!
//! The separation is not about throughput. A transfer is the one operation that
//! runs for minutes over a link the user does not control, and before this split
//! it shared the terminal's connection — so a transfer that stalled, timed out or
//! lost its transport took the interactive session down with it, which is what
//! users reported as "the SFTP transfer dropped my connection".

use openterm_ssh::connection_pool::{DataConnection, PoolConfig, SshConnectionPool};
use openterm_ssh::{ConnectRoute, RusshSession, SshError};
use std::sync::Arc;

/// 会话连接的两种模式
pub enum SessionConnection {
    /// 单连接模式：所有操作共用一条连接（调试/回退用）
    Legacy { session: Arc<RusshSession> },
    /// 连接池模式：主连接 + 数据连接池
    Pooled { pool: Arc<SshConnectionPool> },
}

impl SessionConnection {
    /// 创建传统单连接模式
    pub async fn new_legacy(route: ConnectRoute) -> Result<Self, SshError> {
        let session = Arc::new(openterm_ssh::RusshBackend.connect_with_route(route).await?);
        Ok(Self::Legacy { session })
    }

    /// 创建连接池模式
    pub async fn new_pooled(
        route: ConnectRoute,
        pool_config: PoolConfig,
    ) -> Result<Self, SshError> {
        let pool = Arc::new(SshConnectionPool::new(route, pool_config).await?);
        Ok(Self::Pooled { pool })
    }

    /// 获取用于 terminal PTY 的连接
    pub fn terminal_session(&self) -> Arc<RusshSession> {
        match self {
            Self::Legacy { session } => session.clone(),
            Self::Pooled { pool } => pool.primary(),
        }
    }

    /// 获取用于快速 SFTP 操作的连接（ls/mkdir/chmod/rename）
    pub fn quick_sftp_session(&self) -> Arc<RusshSession> {
        self.terminal_session()
    }

    /// 租借一条专用连接用于大文件传输
    ///
    /// 池模式下会新建或复用一条数据连接（Drop 时自动归还）；单连接模式下
    /// 复用主连接，行为与改动前一致，便于对比排查。
    pub async fn acquire_transfer_connection(&self) -> Result<TransferConnection, SshError> {
        match self {
            Self::Legacy { session } => Ok(TransferConnection::Legacy(session.clone())),
            Self::Pooled { pool } => Ok(TransferConnection::Pooled(
                pool.acquire_data_connection().await?,
            )),
        }
    }

    /// 优雅关闭：断开池中所有连接（含主连接）
    pub async fn shutdown(&self) {
        match self {
            Self::Legacy { session } => {
                let _ = session.disconnect().await;
            }
            Self::Pooled { pool } => pool.shutdown().await,
        }
    }
}

/// 一次大文件传输持有的连接
pub enum TransferConnection {
    Legacy(Arc<RusshSession>),
    /// 持有数据连接，Drop 时归还池中
    Pooled(DataConnection),
}

impl TransferConnection {
    /// 获取底层 session
    pub fn session(&self) -> &Arc<RusshSession> {
        match self {
            Self::Legacy(session) => session,
            Self::Pooled(data) => data.session(),
        }
    }
}

/// 连接配置：决定使用哪种模式
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// 是否启用连接池模式
    pub enable_pool: bool,

    /// 连接池配置（仅当 enable_pool=true 时生效）
    pub pool_config: PoolConfig,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            enable_pool: true,
            // Three concurrent bulk transfers is already more than a single
            // upstream link can serve; the cap exists to bound the number of SSH
            // connections one session opens on the server.
            pool_config: PoolConfig::default(),
        }
    }
}
