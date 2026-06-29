# OpenTerm 性能与稳定性优化方案

根据代码库的全面审计，以下是按优先级排列的优化建议。

---

## 一、流畅性与资源占用

### P0 — 终端渲染（影响最大，改动收益最高）

**当前问题**：终端渲染采用「全量快照」模式，每帧分配 `rows × cols` 个 `TerminalCell`（~56字节/单元），80×24 窗口每帧 ~107KB 分配。SSH 每来一个数据块就调用一次 `snapshot()` → `refresh_visible_lines()`，造成大量重复分配。

**建议方案**：

| # | 措施 | 位置 |
|---|------|------|
| 1 | **脏行追踪**：在 `AlacrittyTerminalBuffer` 中维护一个 `HashSet<usize>` 记录变动的行号，`snapshot()` 只重建脏行 | `openterm-terminal/src/lib.rs:103-108` |
| 2 | **渲染时不再调 `snapshot()`**：改为直接持有 `TerminalGrid` 的 `Arc`，渲染 Canvas 时读已有的 grid，只在 SSH 数据到达时才更新 | `openterm-app/src/ui/terminal.rs:24` |
| 3 | **样式运行合并**：将连续相同样式的 cell 合并为 `StyledRun { text: String, fg, bg, bold, underline }`，减少 Canvas 的 `fill_text()` 调用次数 | `openterm-app/src/terminal_render.rs:182-253` |
| 4 | **`cursor_line_text()` 不再调 `snapshot()`**：直接从 `term.grid()` 读光标行，避免分配整个 grid | `openterm-terminal/src/lib.rs:161-174` |

**预期收益**：渲染帧率从当前可能的 <30fps 提升到稳定 60fps，内存分配减少 80%+。

---

### P1 — SSH 输出处理

| # | 措施 | 位置 |
|---|------|------|
| 5 | **输出合并写入**：SSH 数据到达后不立即写终端，用 `tokio::time::interval` 做 16ms（60fps）批量合并。多次 `write_remote_output` 合并为一次 | `openterm-app/src/connection.rs:389` 附近 |
| 6 | **无意义数据拷贝消除**：`data.to_vec()` 在 `event_shell` 中多拷贝了一份。改用 `Vec<u8>` 直接转移所有权 | `openterm-ssh/src/lib.rs:902` |
| 7 | **限制 scrollback 上限**：在 `Config` 中显式设置 `scrolling.history = 5000`（默认 10000）。长会话下内存占用减半 | `openterm-terminal/src/lib.rs:113` |

---

### P2 — 内存与分配

| # | 措施 | 位置 |
|---|------|------|
| 8 | **Sidebar 过滤结果缓存**：将 `filtered_hosts` 的计算结果缓存在 `App` 中，仅在 hosts 列表或搜索词变化时重新计算，避免每帧 O(n) 遍历 | `openterm-app/src/ui/sidebar.rs:74-121` |
| 9 | **`SessionConfig` 避免深拷贝**：`SaveHost` 和 `DuplicateTab` 中的 `config.clone()` 改为传引用或移动所有权 | `openterm-app/src/update.rs:136, 191` |
| 10 | **命令历史用 `VecDeque`**：`all_history.insert(0, entry)` 是 O(n) 移位，换 `VecDeque::push_front` 可 O(1) | `openterm-app/src/update.rs:1554` |
| 11 | **拖拽时不 resize 终端**：侧栏拖拽时每帧都在 resize 并重新 snapshot，应改为仅在 `DragEnd` 时 resize | `openterm-app/src/update.rs:322-393` |

---

### P3 — 动画策略（保留但按需）

| # | 措施 | 位置 |
|---|------|------|
| 12 | **连接卡片动画**：当前仅 `card_animating()` 时订阅帧事件，策略正确。可进一步将动画时长从默认值缩短到 200ms，减少 GPU 开销 | `openterm-app/src/subscription.rs:116-120` |
| 13 | **Ping 状态动画**：sidebar 的 ping 延迟点可改为 CSS transition 效果，不走 Canvas 渲染。如果当前是轮询渲染，改为仅在 latency 值变化时触发 | `openterm-app/src/ui/sidebar.rs` |

---

## 二、稳定性

### P0 — SSH 通道泄漏（高危）

| # | 措施 | 位置 |
|---|------|------|
| 14 | **SFTP 错误时关闭 channel**：所有 SFTP 方法（`read_file`、`write_file`、`list_dir` 等）在 `?` 提前返回前必须调用 `sftp.close()`。参考 `remove_path` 的实现模式：先保存 result，再 close，最后返回 result | `openterm-ssh/src/lib.rs:929-1229` |
| 15 | **验证 russh-sftp 的 `Drop` 行为**：如果 `SftpSession::Drop` 不关闭底层 channel，每个 SFTP 错误会泄漏一个 SSH channel。服务端 `MaxSessions` 默认 10，泄漏 10 次后无法新建 channel | 同上 |

### P1 — 异步任务管理

| # | 措施 | 位置 |
|---|------|------|
| 16 | **`JoinHandle` 不能丢弃**：`tokio::spawn` 的返回值必须存入 `JoinSet` 或在 drop 时 abort。当前 `spawn_sftp_list`、`spawn_transfer`、`spawn_metrics` 等 8 处 fire-and-forget 任务无法取消，其 panic 也会被静默吞掉 | `openterm-app/src/connection.rs:435, 470, 700, 744, 754, 763, 780` |
| 17 | **转发连接任务跟踪**：`run_local_forward` 中每个 TCP 连接 spawn 的任务无人跟踪。应存入 `JoinSet`，在 shutdown 时统一 abort | `openterm-ssh/src/lib.rs:1268, 1325, 1394` |

### P2 — 超时与断连

| # | 措施 | 位置 |
|---|------|------|
| 18 | **`disconnect()` 加超时**：服务器无响应时 `handle.disconnect().await` 可能永久挂起。用 `tokio::time::timeout(Duration::from_secs(5), ...)` 包裹，超时后强行 drop handle | `openterm-ssh/src/lib.rs:766-776`、`openterm-app/src/connection.rs:404` |
| 19 | **显式设置 `keepalive_max`**：当前依赖 russh 内部默认值（3），应显式设置 `keepalive_max: 3` 确保断线检测在 ~90s 内生效 | `openterm-ssh/src/lib.rs:238` |
| 20 | **SFTP 操作加超时**：`sftp.read_dir()`、`sftp.read()` 等无超时，服务器卡死时永久阻塞。包装 `tokio::time::timeout(30s, ...)` | `openterm-ssh/src/lib.rs` 各 SFTP 方法 |

### P3 — 错误恢复

| # | 措施 | 位置 |
|---|------|------|
| 21 | **注册全局 panic hook**：至少把 panic 信息写入日志文件，避免静默崩溃 | `openterm-app/src/main.rs:39` |
| 22 | **Shell task 错误不静默丢失**：`shell_task.abort()` 改为先 `shell_task.await` 获取结果再决定是否 abort | `openterm-app/src/connection.rs:405` |
| 23 | **History 持久化不要在热路径上**：`Output` 事件中 `WorkspaceStore::open()` 是阻塞 I/O（redb），极度影响 UI 流畅。改为异步写入或批量攒 500ms 再写一次 | `openterm-app/src/update.rs:1534-1536` |

### P4 — 文件 I/O 异步化

| # | 措施 | 位置 |
|---|------|------|
| 24 | **`download_file` 用 `tokio::fs`**：`std::fs::File::write_all_at` 和 `sync_all` 在 tokio worker 线程上阻塞。改用 `tokio::fs::File` + `write_at` | `openterm-ssh/src/lib.rs:1042-1047, 1093, 1116` |
| 25 | **`input_task` 的 abort 改进**：stdin 读取任务 abort 后如果 stdin 阻塞在内核 `read()`，可能无限不退出。改用 `tokio::io::stdin()` 异步读取 | `openterm-ssh/src/lib.rs:834, 859` |

---

## 三、实施优先级矩阵

| 优先级 | 编号 | 类型 | 改动量 | 收益 |
|--------|------|------|--------|------|
| **立即** | 14, 15 | 稳定性 (channel 泄漏) | 小 | 防止 SSH session 耗尽 |
| **立即** | 23 | 流畅性 (阻塞 I/O) | 中 | 解决最严重的 UI 卡顿 |
| **本周** | 1, 2, 3, 4 | 流畅性 (终端渲染) | 中-大 | 核心体验提升最大 |
| **本周** | 16, 17 | 稳定性 (任务泄漏) | 小 | 防止 panic 静默丢失 |
| **本周** | 18, 19 | 稳定性 (超时) | 小 | 防止断连卡死 |
| **下周** | 5, 6, 7 | 流畅性 (SSH 输出) | 小 | 减少无效分配 |
| **下周** | 8, 9, 10, 11 | 流畅性 (内存) | 小 | 降低常驻内存 |
| **迭代** | 20, 21, 22, 24, 25 | 健壮性 | 小-中 | 长期稳定性 |
| **迭代** | 12, 13 | 动画微调 | 小 | 锦上添花 |

---

## 四、关键代码位置索引

### 终端渲染链

```
SSH 数据 → connection.rs:389 (try_send Event::Output)
         → update.rs:1524 (session.write_output)
         → openterm-terminal/src/lib.rs:187 (write_remote_output)
           → refresh_visible_lines() → snapshot() ← 全量分配
         → ui/terminal.rs:24 (session.terminal.snapshot())
         → terminal_render.rs:182-253 (draw 每个 cell)
```

### SSH 通道生命周期

```
connection.rs:223 (stream::channel + worker spawn)
  → connection.rs:275 (connect_active)
  → openterm-ssh/src/lib.rs:228 (connect_with_options)
  → openterm-ssh/src/lib.rs:863 (event_shell) ← PTY 泵
  → openterm-ssh/src/lib.rs:921 (open_sftp) ← SFTP 通道
```

### 事件循环

```
update.rs:12 (update 入口)
  → 直接处理 (HostSearchChanged, SelectTab, ...)
  → handle_conn_event:1490 (Worker 事件路由)
  → 递归 (Palette dispatch)
  → 返回 Task<Message>
```

---

## 五、验证方法

每次改动完成后，按以下顺序验证：

1. `cargo check --workspace` — 类型检查
2. `cargo test --workspace` — 单元测试
3. 真实 SSH 连接验证 — 连接 `82.157.57.178`，执行 `cat /var/log/syslog` 触发大量输出，观察：
   - 终端是否流畅滚动
   - 断连重连是否正常
   - SFTP 操作是否正常
   - 新建 Tab 是否复用连接配置
4. `./scripts/test_all_real.sh` — 完整烟雾测试
