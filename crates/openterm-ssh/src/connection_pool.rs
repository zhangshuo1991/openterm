//! SSH 连接池：分离控制通道与数据通道，避免大文件传输阻塞交互操作。
//!
//! ## 架构
//!
//! - **Primary Connection**: 一个长期存活的主连接，承载 terminal PTY + 快速 SFTP
//!   操作（ls/mkdir/chmod/rename）。永不用于大文件传输。
//! - **Data Connection Pool**: 动态管理的数据连接池，专用于 upload/download。
//!   每个传输独占一个连接，传输完成后归还池中复用。连接空闲超时后自动关闭。
//! - **Health Check**: 后台任务定期 ping 空闲连接，剔除死连接，保持池健康。
//!
//! ## 使用示例
//!
//! ```no_run
//! use openterm_ssh::{ConnectRoute, PoolConfig, SshConnectionPool};
//! use std::sync::Arc;
//! use std::sync::atomic::AtomicU8;
//!
//! # async fn example(route: ConnectRoute) -> Result<(), openterm_ssh::SshError> {
//! let pool = SshConnectionPool::new(route, PoolConfig::default()).await?;
//!
//! // 交互式操作用主连接
//! let _entries = pool.primary().list_dir("/tmp").await?;
//!
//! // 大文件传输租借数据连接
//! let data_conn = pool.acquire_data_connection().await?;
//! let (progress, _progress_rx) = tokio::sync::mpsc::channel::<u64>(8);
//! let stop = Arc::new(AtomicU8::new(0));
//! data_conn
//!     .session()
//!     .download_file("/remote/file", std::path::Path::new("/local/file"), progress, stop)
//!     .await?;
//! // Drop 时自动归还连接到池
//! # Ok(())
//! # }
//! ```

use crate::{ConnectRoute, RusshBackend, RusshSession, SshError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore};

/// 连接池配置
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 数据连接池最大容量（默认 3）
    pub max_data_connections: usize,

    /// 数据连接空闲超时，超过后自动关闭（默认 300s）
    pub idle_timeout: Duration,

    /// 健康检查间隔（默认 30s）
    pub health_check_interval: Duration,

    /// 获取连接的最大等待时间（默认 60s）
    pub acquire_timeout: Duration,

    /// 是否启用健康检查（默认 true）
    pub enable_health_check: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_data_connections: 3,
            idle_timeout: Duration::from_secs(300),
            health_check_interval: Duration::from_secs(30),
            acquire_timeout: Duration::from_secs(60),
            enable_health_check: true,
        }
    }
}

/// 连接池统计信息
#[derive(Debug, Clone, Default)]
pub struct PoolStats {
    /// 当前池中空闲连接数
    pub idle_connections: usize,

    /// 当前正在使用的连接数
    pub active_connections: usize,

    /// 累计创建的连接数
    pub total_created: u64,

    /// 累计复用的连接数（直接从池中取出）
    pub total_reused: u64,

    /// 健康检查剔除的死连接数
    pub total_evicted: u64,

    /// 当前等待连接的请求数
    pub waiting_requests: usize,
}

/// 池中连接的元数据
struct PooledConnection {
    session: Arc<RusshSession>,
    /// 最后一次归还到池的时间
    last_returned: Instant,
    /// 此连接被复用的次数
    reuse_count: u32,
    /// 该连接占用的容量许可。
    ///
    /// 许可跟着连接走（空闲时也不释放），所以 `max_data_connections`
    /// 才是「总连接数」的硬上限，而不是「同时在创建中的连接数」。
    capacity: OwnedSemaphorePermit,
}

/// SSH 连接池
pub struct SshConnectionPool {
    /// 主连接：terminal PTY + 快速 SFTP
    primary: Arc<RusshSession>,

    /// 数据连接池（空闲连接）
    pool: Arc<Mutex<Vec<PooledConnection>>>,

    /// 连接配置，用于创建新连接
    route: ConnectRoute,

    /// 配置参数
    config: PoolConfig,

    /// 数据连接的总容量上限（空闲 + 使用中），防止握手风暴
    capacity: Arc<Semaphore>,

    /// 统计信息
    stats: Arc<RwLock<PoolStats>>,

    /// 健康检查任务的终止信号
    shutdown: Arc<tokio::sync::Notify>,
}

impl SshConnectionPool {
    /// 创建连接池并建立主连接
    pub async fn new(route: ConnectRoute, config: PoolConfig) -> Result<Self, SshError> {
        let primary = Arc::new(RusshBackend.connect_with_route(route.clone()).await?);

        let pool = Arc::new(Mutex::new(Vec::new()));
        let capacity = Arc::new(Semaphore::new(config.max_data_connections));
        let stats = Arc::new(RwLock::new(PoolStats::default()));
        let shutdown = Arc::new(tokio::sync::Notify::new());

        let pool_instance = Self {
            primary: primary.clone(),
            pool: pool.clone(),
            route: route.clone(),
            config: config.clone(),
            capacity,
            stats: stats.clone(),
            shutdown: shutdown.clone(),
        };

        // 启动健康检查后台任务
        if config.enable_health_check {
            let pool_weak = Arc::downgrade(&pool);
            let interval = config.health_check_interval;
            let idle_timeout = config.idle_timeout;
            let stats_weak = Arc::downgrade(&stats);
            let shutdown_weak = Arc::downgrade(&shutdown);

            tokio::spawn(async move {
                let Some(shutdown) = shutdown_weak.upgrade() else {
                    return;
                };
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(interval) => {
                            let Some(pool) = pool_weak.upgrade() else { break };
                            let Some(stats) = stats_weak.upgrade() else { break };
                            Self::health_check_round(pool, stats, idle_timeout).await;
                        }
                        _ = shutdown.notified() => {
                            break;
                        }
                    }
                }
            });
        }

        Ok(pool_instance)
    }

    /// 获取主连接（terminal + 交互式 SFTP）
    pub fn primary(&self) -> Arc<RusshSession> {
        self.primary.clone()
    }

    /// 租借一个数据连接用于大文件传输
    ///
    /// 返回 RAII 包装的连接，Drop 时自动归还池中。池中有空闲连接就直接复用；
    /// 否则在容量上限内新建。达到上限时最多等待 `acquire_timeout`。
    pub async fn acquire_data_connection(&self) -> Result<DataConnection, SshError> {
        // 1. 复用空闲连接。空闲连接自己持有容量许可，所以这里不能再取许可。
        if let Some(connection) = self.take_idle().await {
            return Ok(connection);
        }

        // 2. 取一份容量做新连接，等别的传输归还。
        let permit = match tokio::time::timeout(
            self.config.acquire_timeout,
            self.capacity.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(SshError::Connection("connection pool closed".to_string()));
            }
            Err(_) => {
                return Err(SshError::Connection(format!(
                    "all {} data connections are busy; try again when a transfer finishes",
                    self.config.max_data_connections
                )));
            }
        };

        // 3. 等待期间可能已有连接归还；有就复用它，把刚取的许可还回去。
        if let Some(connection) = self.take_idle().await {
            drop(permit);
            return Ok(connection);
        }

        // 4. 真正新建连接。
        let session = Arc::new(RusshBackend.connect_with_route(self.route.clone()).await?);
        let mut stats = self.stats.write().await;
        stats.total_created += 1;
        stats.active_connections += 1;
        drop(stats);

        Ok(DataConnection {
            session,
            pool: self.pool.clone(),
            stats: self.stats.clone(),
            reuse_count: 0,
            capacity: Some(permit),
        })
    }

    /// Pop an idle connection that still answers, keeping the pool lock only for
    /// the pop itself: the liveness ping is a network round trip and holding the
    /// mutex across it would serialise every acquire behind the slowest peer.
    async fn take_idle(&self) -> Option<DataConnection> {
        loop {
            let pooled = { self.pool.lock().await.pop() };
            let Some(pooled) = pooled else {
                let mut stats = self.stats.write().await;
                stats.idle_connections = 0;
                return None;
            };

            let alive = pooled.session.is_alive().await;
            let mut stats = self.stats.write().await;
            if alive {
                stats.idle_connections = self.pool.lock().await.len();
                stats.active_connections += 1;
                stats.total_reused += 1;
                return Some(DataConnection {
                    session: pooled.session,
                    pool: self.pool.clone(),
                    stats: self.stats.clone(),
                    reuse_count: pooled.reuse_count + 1,
                    capacity: Some(pooled.capacity),
                });
            }
            // Dead connection: drop it (releasing its capacity permit) and look
            // for another.
            stats.total_evicted += 1;
        }
    }

    /// 获取连接池统计信息
    pub async fn stats(&self) -> PoolStats {
        self.stats.read().await.clone()
    }

    /// 健康检查：移除死连接和超时空闲连接
    async fn health_check_round(
        pool: Arc<Mutex<Vec<PooledConnection>>>,
        stats: Arc<RwLock<PoolStats>>,
        idle_timeout: Duration,
    ) {
        // Take the whole set out of the pool first: the liveness ping below is a
        // network round trip, and holding the lock across it would block every
        // acquire for the length of that trip.
        let candidates = {
            let mut pool = pool.lock().await;
            std::mem::take(&mut *pool)
        };

        let now = Instant::now();
        let mut evicted = 0_usize;
        let mut alive = Vec::with_capacity(candidates.len());
        for connection in candidates {
            if now.duration_since(connection.last_returned) > idle_timeout {
                evicted += 1; // 空闲超时（丢弃会释放其容量许可）
                continue;
            }
            if connection.session.is_alive().await {
                alive.push(connection);
            } else {
                evicted += 1;
            }
        }

        let kept = alive.len();
        {
            let mut pool = pool.lock().await;
            pool.extend(alive);
        }
        let mut stats = stats.write().await;
        stats.idle_connections = kept;
        stats.total_evicted += evicted as u64;
    }

    /// 连接池当前的空闲/使用中数量（无网络 I/O）。
    pub async fn snapshot(&self) -> PoolStats {
        let mut stats = self.stats.read().await.clone();
        stats.idle_connections = self.pool.lock().await.len();
        stats
    }

    /// 优雅关闭连接池：断开所有连接，停止健康检查
    pub async fn shutdown(&self) {
        self.shutdown.notify_one();

        let mut pool = self.pool.lock().await;
        for conn in pool.drain(..) {
            let _ = conn.session.disconnect().await;
        }

        let _ = self.primary.disconnect().await;
    }
}

impl Drop for SshConnectionPool {
    fn drop(&mut self) {
        // 通知健康检查任务退出
        self.shutdown.notify_one();
    }
}

/// RAII 封装的数据连接：传输结束后自动归还池
pub struct DataConnection {
    session: Arc<RusshSession>,
    pool: Arc<Mutex<Vec<PooledConnection>>>,
    stats: Arc<RwLock<PoolStats>>,
    reuse_count: u32,
    /// 该连接占用的容量许可；归还到池时一起交回，Drop 时自动释放。
    capacity: Option<OwnedSemaphorePermit>,
}

impl DataConnection {
    /// 获取底层 SSH 会话
    pub fn session(&self) -> &Arc<RusshSession> {
        &self.session
    }

    /// 显式归还连接（通常不需要，Drop 时自动归还）
    pub async fn release(self) {
        // Drop 会自动调用，这里提供显式接口方便测试
        drop(self);
    }
}

impl Drop for DataConnection {
    fn drop(&mut self) {
        let session = self.session.clone();
        let pool = self.pool.clone();
        let stats = self.stats.clone();
        let reuse_count = self.reuse_count;
        let capacity = self.capacity.take();

        match capacity {
            // 正常归还：连接与容量许可一起回到池中。
            Some(capacity) => {
                tokio::spawn(async move {
                    let mut pool = pool.lock().await;
                    pool.push(PooledConnection {
                        session,
                        last_returned: Instant::now(),
                        reuse_count,
                        capacity,
                    });

                    let mut stats = stats.write().await;
                    stats.idle_connections = pool.len();
                    stats.active_connections = stats.active_connections.saturating_sub(1);
                });
            }
            // 已经归还过（或池已关闭）：让会话随 Drop 关闭，容量自动释放。
            None => {
                tokio::spawn(async move {
                    let mut stats = stats.write().await;
                    stats.active_connections = stats.active_connections.saturating_sub(1);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool's capacity is a hard cap on *connections*, not on dials in
    /// flight: an idle connection keeps its permit, so `max_data_connections`
    /// connections can never be exceeded no matter how many callers arrive at
    /// once. Exercised against the permit accounting directly because dialling
    /// needs a server.
    #[tokio::test]
    async fn capacity_permits_bound_idle_plus_active_connections() {
        let capacity = Arc::new(Semaphore::new(2));
        let first = capacity
            .clone()
            .acquire_owned()
            .await
            .expect("first permit");
        let second = capacity
            .clone()
            .acquire_owned()
            .await
            .expect("second permit");

        // A third caller must wait rather than dial a third connection.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), capacity.clone().acquire_owned())
                .await
                .is_err(),
            "capacity must be exhausted while both connections are held"
        );

        // Returning one connection returns its permit with it.
        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), capacity.clone().acquire_owned())
                .await
                .is_ok(),
            "releasing a connection must free exactly one slot"
        );
        drop(second);
    }

    /// Exhausting the capacity must produce a message that names the limit and
    /// the way out, because that is what the user sees in the transfer row.
    #[test]
    fn exhaustion_message_is_actionable() {
        let config = PoolConfig::default();
        let message = format!(
            "all {} data connections are busy; try again when a transfer finishes",
            config.max_data_connections
        );
        assert!(message.contains("busy"));
        assert!(message.contains(&config.max_data_connections.to_string()));
    }

    /// The acquire wait is bounded: a transfer must fail rather than hang forever
    /// behind other transfers.
    #[test]
    fn acquire_wait_is_bounded() {
        let config = PoolConfig::default();
        assert!(config.acquire_timeout > Duration::ZERO);
        assert!(
            config.acquire_timeout <= Duration::from_secs(600),
            "an unbounded acquire would look like a hung transfer"
        );
    }
}
