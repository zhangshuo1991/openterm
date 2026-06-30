# OpenTerm Feature Proposals

> 代码依据：`crates/openterm-app/src/` 实际结构。说明中文，标识符英文。
> 本文档由多 agent 工作流对代码库深度分析后生成，覆盖 UI 美化、键盘快捷键、命令补全、CLI 工具集成四个方向。
> 与 `ROADMAP.md`（项目 P0-P3 基础路线图）互补，本文聚焦体验增强阶段。

---

## 🚀 Quick Wins（立即可做，1-2 天内全部完成）

### 1. 彩色 Tab 药丸

每个 Tab 根据 host 字符串哈希派生固定色调，背景 14% alpha，激活边框 60% alpha。零状态、零消息变更。

**修改**：`theme.rs`（新增 `tab_accent(host: &str) -> Color`）、`ui/tabs.rs`（样式闭包）

```rust
pub fn tab_accent(host: &str) -> Color {
    const P: [(f32, f32, f32); 6] = [
        (0.235, 0.620, 0.560), (0.220, 0.530, 0.940), (0.741, 0.576, 0.976),
        (0.880, 0.480, 0.240), (0.360, 0.720, 0.380), (0.860, 0.660, 0.260),
    ];
    let h = host.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let (r, g, b) = P[h as usize % P.len()];
    Color::from_rgb(r, g, b)
}
```

**工作量**：0.5 天

---

### 2. 未保存文件指示点

`session.rs:334` 的 `FileViewerState.dirty` 已存在，只需在 Tab 标题行插入 4px 琥珀色圆点。

**修改**：`ui/tabs.rs`（`tab()` 函数 body row）  **工作量**：0.25 天

---

### 3. 补全缺失快捷键（Cmd+, / Cmd+B / Cmd+1-9）

三个 Message 已存在，只需在 `subscription.rs::app_shortcut()` 加 15 行：

```rust
Key::Character(v) if v == "," && cmd => Some(Message::OpenSettings),
Key::Character(v) if v.eq_ignore_ascii_case("b") && cmd => Some(Message::ToggleSidebar),
Key::Character(v) if matches!(v.as_ref(), "1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9") && cmd => {
    let n: usize = v.parse::<usize>().unwrap_or(1).saturating_sub(1);
    Some(Message::SelectTab(n))
}
```

**工作量**：0.25 天

---

### 4. History 一键 Re-run

新增 `HistoryRun(String)` 变体，在命令后追加 `\n` 立即执行：

```rust
Message::HistoryRun(cmd) => {
    let bytes = format!("{cmd}\n").into_bytes();
    if let Some(s) = app.active_session_mut() {
        if let Some(tx) = &s.cmd_tx {
            let _ = tx.try_send(crate::connection::Command::Input(bytes));
        }
    }
    Task::none()
}
```

**修改**：`message.rs`（+1 行）、`update.rs`（+5 行）、`ui/history.rs`（hover 时显示 Run 按钮）  **工作量**：0.5 天

---

### 5. Tab 悬停 Tooltip

当 host 名超 22 字符被截断时，用 `tooltip()` 包裹整个 tab button，悬停显示完整标题。

**修改**：`ui/tabs.rs`  **工作量**：0.25 天

---

## 🎨 Sprint 1：UI 美化（约 10 天）

### Tab 栏

**关闭动画**（1.5 天）— Tab 宽度 120ms ease-out 收缩至 0。用 `HashMap<u64, Animation<bool>>` 以 session.id 为键（避免 index 漂移 bug），`any_animating()` 需同步检查。

> ⚠️ 对抗性评审重点：HashMap 键必须用 `session.id: u64`（稳定），不能用 index。两个 Tab 同时关闭时，第一个完成 remove 后第二个的 index 立即失效。

```rust
// main.rs App struct:
closing_tabs: HashMap<u64, Animation<bool>>,
// update.rs CloseTab: 不立即 remove，改为启动动画
let id = app.sessions[idx].id;
let mut anim = Animation::new(true).quick();
anim.go_mut(false, now);
app.closing_tabs.insert(id, anim);
```

**Tab 拖拽重排**（2.5 天）— iced 无 layout query，需在 view pass 时把估算宽度存入 `tab_widths: Vec<f32>`，以计算 insert_before。用 `stack![]` 渲染 ghost tab + 插入线。

```rust
// 3 个新 Message:
TabDragStart(usize, Point), TabDragMove(Point), TabDragEnd,
// 新状态:
tab_drag: Option<TabDragState>, tab_widths: Vec<f32>,
// update.rs TabDragEnd:
Message::TabDragEnd => {
    if let Some(drag) = app.tab_drag.take() {
        let from = drag.from_idx;
        let to = drag.insert_before.min(app.sessions.len().saturating_sub(1));
        if from != to { app.sessions.swap(from, to); app.active = to; }
    }
    Task::none()
}
```

### 侧边栏

**主机头像**（1 天）— 首字母圆形 container（`border_radius: 18.0`）+ 右下角状态点，无需第三方依赖。

**上次连接时间**（0.75 天）— `HostProfile.last_connected_at` 已存在，新增 `relative_time()` 解析为 "3h ago" / "2d ago" 显示。

```rust
fn relative_time(ts_str: &str) -> String {
    let secs: u64 = ts_str.strip_prefix("unix:").and_then(|s| s.parse().ok()).unwrap_or(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0);
    let age = now.saturating_sub(secs);
    match age {
        0..=3599 => format!("{}m ago", age / 60),
        3600..=86399 => format!("{}h ago", age / 3600),
        _ => format!("{}d ago", age / 86400),
    }
}
```

**可折叠分组**（1 天）— 新增 `GroupToggle(String)` 消息、`collapsed_groups: HashSet<String>` 状态，持久化到 `UiSettings`。

### 终端区域

**可配置行高/字间距**（1.5 天）— `UiSettings` 新增 `line_height: f32`（默认 1.2）、`letter_spacing: f32`，Settings Appearance 面板加滑块，`terminal_render::grid_for_viewport()` 引入行高参数。

**光标形状可选**（1 天）— 新增 `CursorShape` enum（Block/Underline/Beam）+ `blink: bool`，PulseTick 驱动闪烁，`terminal_render.rs` 分支绘制。

**复制闪光**（0.75 天）— 复制时 150ms accent 色闪烁，`copy_flash_progress: f32` + Tick 推进，canvas 选区颜色 lerp。

### History 面板

**时间线分组**（0.75 天）— Today / Yesterday / This Week / Older 分组 header，纯渲染逻辑无新状态。

**可展开输出预览**（0.75 天）— 新增 `HistoryToggleExpand(u64)`、`expanded_history: HashSet<u64>`，点击展开最多 1500 字符输出块。

### 底部状态栏

**实时吞吐量**（0.75 天）— Session 新增 `bytes_rx_rate: f32` / `bytes_tx_rate: f32`，MetricsTick 计算，footer 显示 `↓2.4KB/s ↑0.1KB/s`。

**连接时长**（0.5 天）— `connected_at: Option<Instant>`，Phase::Connected 时赋值，footer 显示 `01:23:45`。

**延迟折线图**（1 天）— `ping_history: VecDeque<u16>`（最近 10 次），footer 用 iced Canvas 绘制 sparkline。

### 通知 Toast 系统（2 天）

顶部右侧滑入/淡出，最多 4 条堆叠，点击关闭。用 `padding_left` 插值模拟 translate-x（iced 无 transform）。新建 `ui/toasts.rs`，`stack![]` 放在视图最顶层。

```rust
pub struct Toast { pub id: u64, pub kind: ToastKind, pub msg: String,
                   pub progress: f32, pub dismissed: bool }
pub enum ToastKind { Success, Error, Info, Warning }
// Tick handler:
for t in &mut app.toasts { t.progress += dt / 3.5; }
app.toasts.retain(|t| !t.dismissed && t.progress < 1.3);
```

> ⚠️ `any_animating()` 需增加 `!app.toasts.is_empty()` 判断，否则 Toast 期间不收帧。

### 主题系统（2 天）

6 个预设 accent + 自定义 hex 输入。`UiSettings` 新增 `accent_hex: String`，`theme.rs` 运行时解析覆盖 palette accent。Settings Appearance 面板加色块选择器。

---

## ⌨️ Sprint 2：键盘快捷键体系（约 3 天）

所有拦截点在 `subscription.rs::app_shortcut()` 或 `sftp_shortcut()` 新函数中。

| 快捷键 | 消息 | 备注 |
|--------|------|------|
| **Cmd+,** | `OpenSettings` | macOS 通用习惯，新增 |
| **Cmd+B** | `ToggleSidebar` | VS Code 习惯，新增 |
| **Cmd+1-9** | `SelectTab(n)` | 已有 handler，新增绑定 |
| **Cmd+Shift+[/]** | `CycleTabs(-1/+1)` | 新增 `Message::CycleTabs(i32)` |
| **Cmd+Shift+R** | `StartRenameTab` | 新增 Message + inline input 渲染 |
| **Cmd+Shift+N** | `DuplicateTab` | 原 Cmd+Shift+D 冲突 SFTP 下载，改为 N |
| **Cmd+P** | `TogglePalette` | 第二触发键，VS Code 习惯 |
| **Cmd+G / Cmd+Shift+G** | `TerminalSearchNext/Prev` | 新增 |
| **Cmd+0** | `ResetFontSize` | 新增 `Message::ResetFontSize` |
| **Cmd+Shift+K** | `ClearTerminal` | Cmd+K 已被 palette 占用 |
| **Cmd+Shift+H** | `ToggleHistory` | 新增 |
| **Cmd+Shift+E** | `ToggleSftp` | 新增 |
| **Cmd+Shift+M** | `ToggleMonitor` | 新增 |
| **Opt+←/→** | PTY `\x1bb`/`\x1bf` | 修改 `keys.rs`，在 alt guard 之前处理 |
| **SFTP 激活时 Delete** | `SftpDeleteRemoteSelected` | `sftp_shortcut()` 新函数，仅 sftp_open 时触发 |
| **SFTP 激活时 F2** | `SftpRenameSelected` | 同上 |
| **SFTP 激活时 Cmd+Shift+U/D** | upload/download | 同上 |

**冲突解决**：

- `Cmd+K` 保留为调色板（已发布）；`ClearTerminal` 改为 `Cmd+Shift+K`
- `Cmd+Shift+D` 冲突：DuplicateTab 改为 `Cmd+Shift+N`；SFTP 下载保留 `D`
- `Cmd+Shift+R` 上下文优先：sftp_open 时触发 `SftpRefresh`，否则触发 `StartRenameTab`（须在 subscription filter_map 中按顺序检查）
- `Cmd+Enter` 的 `HistoryRunSelected` 守卫必须在 update.rs 侧：`if !app.history_open || app.history_cursor.is_none() { return Task::none(); }`，否则面板关闭时会静默运行上次选中命令（严重 bug）

**Opt+Left/Right 修改点**（`keys.rs:26` 的 alt guard 之前）：

```rust
if modifiers.alt() && !modifiers.control() && !modifiers.logo() {
    match key.as_ref() {
        Key::Named(key::Named::ArrowLeft)  => return Some(b"\x1bb".to_vec()),
        Key::Named(key::Named::ArrowRight) => return Some(b"\x1bf".to_vec()),
        Key::Named(key::Named::Backspace)  => return Some(vec![0x17]), // word-delete
        _ => {}
    }
}
```

---

## 💡 Sprint 3：命令补全与智能输入（约 8 天）

### Tier 1（纯客户端）

**History 内联 Ghost Text**（3-4 天）— 用户输入时，canvas 在光标后绘制半透明后缀建议（40% alpha），Right/Tab 接受。

关键约束：ghost text 必须在 canvas draw pass 中单独绘制（Alacritty grid 之后），不注入 PTY 流。需维护 `input_shadow: String`（client-side 行缓冲）。

> ⚠️ 已知局限：SSH 远程 readline 重绘 prompt 行时 input_shadow 会漂移（Up-arrow 历史调用、tab 补全后），v1 在 Enter 时 reset 即可，标注为已知限制。

```rust
// update.rs TerminalInput 拦截:
let is_right = bytes == b"\x1b[C";
let is_tab = bytes == &[0x09];
if is_right || is_tab {
    if let Some(sug) = session.inline_suggestion.take() {
        let _ = tx.try_send(Command::Input(sug.into_bytes()));
        return Task::none();
    }
}
if bytes == b"\r" { session.input_shadow.clear(); }
else { /* update shadow */ }
session.inline_suggestion = compute_inline_suggestion(&session.input_shadow, &history);
```

**Ctrl+R 历史搜索弹层**（2 天）— 复用 `ui/palette.rs` 的 overlay 模式，列表数据改为 history，在 `subscription.rs` 中在 PTY 转发之前拦截 Ctrl+R。新增 `history_search_open/query/idx` 字段。

**Snippet 片段展开**（2-3 天）— 新 redb 表 `"snippets"`，Space/Tab 时检查 `input_shadow` 是否匹配，发送退格 erase abbr + expansion bytes。Settings 新增 Snippets 管理面板。

```rust
// 例：gp -> git push origin HEAD
if let Some(expansion) = app.snippets.get(&session.input_shadow) {
    let abbr_len = session.input_shadow.len();
    let mut out: Vec<u8> = vec![0x7f; abbr_len];  // erase abbr
    out.extend_from_slice(expansion.as_bytes());
    out.extend_from_slice(&bytes);  // 原始 space/tab
    let _ = tx.try_send(Command::Input(out));
}
```

### Tier 2（Shell 集成，可选）

**OSC 133 Shell 集成**（2 天）— 用户在远程 `~/.bashrc` / `~/.zshrc` 注入 precmd/preexec hooks，OpenTerm 解析：

- `ESC ] 133 ; A ST` — prompt ready
- `ESC ] 133 ; B ST` — command started
- `ESC ] 133 ; D ; <exit_code> ST` — command done

好处：精确命令时间戳、exit code 着色、cwd 更新（搭配 OSC 7）、tab title 显示当前路径。

> ⚠️ OSC 序列可能跨 `ConnEvent::Data` chunk 分片，需 `osc_buf: Vec<u8>` ring-buffer 处理。

### Tier 3（AI 辅助，可选，需 API Key）

**错误解释器**（1 天）— 命令 exit code 非 0 时，footer 出现 `[Explain]` 按钮，发送命令 + 最后 2KB 输出到 Claude API，回复展示在 history 面板右侧抽屉。Settings 新增 API Key 配置项（存入 vault）。

---

## 🤖 Sprint 4：CLI 工具集成（约 12 天）

### 检测层（前置，2 天）

解析 OSC 133 B 的命令文本，前缀匹配识别 `DevTool` enum：

```rust
pub enum DevTool { ClaudeCode, Codex, Gh, Git, Container, BuildTool }

fn detect_tool(cmd: &str) -> Option<DevTool> {
    let t = cmd.trim_start();
    if t.starts_with("claude")  { return Some(DevTool::ClaudeCode); }
    if t.starts_with("codex ")  { return Some(DevTool::Codex); }
    if t.starts_with("gh ")     { return Some(DevTool::Gh); }
    if t.starts_with("docker") || t.starts_with("kubectl") { return Some(DevTool::Container); }
    if t.starts_with("git ")    { return Some(DevTool::Git); }
    if t.starts_with("cargo") || t.starts_with("npm") || t.starts_with("pip ") {
        return Some(DevTool::BuildTool);
    }
    None
}
```

激活时：侧边栏自动收起，工作区切换为 `terminal | 6px divider | tool_panel`（350px 默认宽）。
Session 新增：`active_tool: Option<DevTool>`、`tool_output_buf: String`、`tool_panel_visible: bool`。

### Claude Code / Codex 深度集成（3-4 天）

解析 claude/codex 输出两种模式：

- **Mode A 文本模式**：行扫描 `"> Read file: ..."` / `"> Write ..."` / `"✓ Done"` 等模式（脆弱，需迭代）
- **Mode B JSON 模式**（推荐）：运行 `claude --output-format stream-json`，解析 NDJSON 事件流（`tool_use` / `tool_result` / `text` / `cost`）

Tool Panel 三分区（新建 `ui/tool_panel.rs`）：

1. **Header**：工具名 + 模型 + 耗时 + 费用
2. **Tool call log**：可滚动列表，每行 `icon + verb + path + 状态点`
3. **Touched files**：按目录分组的文件树，M/A/R 彩色 badge

一键审批注入：检测到 `[y/n]:` 输出时，panel 显示 `[Approve] [Reject]` 按钮，点击发送 `b"y\n"` / `b"n\n"` 到 PTY。
费用计数器：JSON cost 事件出现时，footer 替换字体大小显示为 `$0.0042`（点击重置）。

### Git 工作流（2 天）

触发：OSC 133 B 命令为 `git diff` / `git log` / `git status`。

- **Diff viewer**：OSC 133 D 时捕获输出，`DiffViewerState` 存于 session，tool panel 渲染 unified diff（`+` 绿、`-` 红、`@@` muted）
- **Branch 显示**：连接后异步运行 `git branch --show-current`，存入 `session.git_branch`，tab 标题变为 `name (branch)`
- **git status 视图**：解析 staged / unstaged / untracked 三桶，迷你文件树带 M/A/D/? badge

### Docker / kubectl 日志流（1 天）

触发：`docker logs -f` / `kubectl logs -f` / `kubectl logs --follow`。

Side panel 日志视图中（不修改主终端），行级后处理着色：`ERROR|FATAL` → `status_error`，`WARN` → `status_warn`，`INFO` → `text_muted`，支持 JSON log `"level":"error"` 映射。
Footer 新增 container context badge。Panel 仅缓冲最近 2000 行，避免高吞吐量性能问题。

### Build Tool 输出解析（1-2 天）

触发：`cargo build|test|check`、`npm run|install|ci`、`pip install`。

非阻塞浮动 overlay（右下角，类似现有 delete confirm modal 但不拦截 input）：

```rust
pub struct BuildState {
    pub tool: String, pub steps_done: u32, pub steps_total: Option<u32>,
    pub error_count: u32, pub first_error: Option<BuildError>,
    pub started_at: Instant, pub done: bool,
}
```

显示进度条（cargo 解析 `Compiling X / Y`）、错误计数、耗时。遇 `error[E...]` 时提取文件路径+行号，显示 `[Open in file viewer]` 按钮（复用现有 `OpenFileViewer`）。

### Snippet 工作流库（1 天）

命令模板，支持 `{{placeholder}}` 语法，调色板中可搜索，选中后插入并将光标停在第一个占位符。内置：`kubectl exec -it {{pod}} -- bash`、`docker exec -it {{container}} /bin/sh`、`claude --output-format stream-json {{task}}`。

### Session Profile / Persona（1 天）

一键保存/加载布局配置（字体大小、面板开关、tool panel 宽度），新 redb 表 `"session_profiles"`，Settings → Profiles 面板管理。

---

## 📋 Backlog / 暂缓

- **侧边栏 tag chips 换行**：iced_aw 版本兼容问题（不在 Cargo.toml、常落后一个 iced 版本），用手动 Row + "+N" badge 替代，不添加 `iced_aw` 依赖
- **Remote completion proxy**（Tier 2 Completion）：需 SFTP 注入 bash 函数 + 第二 SSH channel，3-4 天，暂缓
- **Light mode**：`Theme::Light` iced 支持，但需重新校正所有 color token，2-3 天
- **多窗格 terminal split**（Cmd+Shift+T）：iced 无原生 split，需 tiling widget，复杂度高

---

## 🗺️ 实施顺序建议

| 周 | 内容 | 产出 |
|----|------|------|
| **Week 1** | Quick Wins 全部（1.5 天）+ 键盘快捷键补全 Sprint 2（3 天）| 立即可用的快捷键体验提升 |
| **Week 2** | Toast 系统 + Tab 彩色 + 未保存指示点 + History Re-run | 视觉品质大幅提升 |
| **Week 3** | 侧边栏：头像 + 上次连接时间 + 可折叠分组 | 侧边栏完成度 |
| **Week 4** | Terminal：光标形状 + 行高 + 复制闪光 + History 时间线 + 展开预览 | 终端区域完成 |
| **Week 5** | Footer：吞吐量 + 时长 + sparkline + Tab 关闭动画 | 状态栏完成 |
| **Week 6** | Accent 主题 + Ghost text 补全（input_shadow + canvas render）| 主题系统 + 首个补全功能 |
| **Week 7** | OSC 133 检测层 + Claude Code 集成（JSON 模式优先）| CLI 集成核心 |
| **Week 8** | Git diff viewer + Docker log coloring + Build overlay | CLI 集成完成 |
| **Week 9** | Ctrl+R 历史搜索弹层 + Snippet 系统 + Snippet 工作流库 | 智能输入完整 |
| **Week 10** | Session profiles + 测试 + polish | 发布准备 |

---

## ✅ 对抗性评审结论

所有功能均可在 iced 0.14 实现，无绝对 infeasible 项。主要注意点：

1. **Tab 关闭动画**：必须用 `session.id: u64` 做 HashMap 键，避免 index 漂移（两个 Tab 同时关闭时第二个 index 失效）
2. **Ghost text**：在 canvas 层绘制，不注入 PTY；`input_shadow` 在 Enter 时 reset，远程 readline 重绘会导致漂移（v1 已知限制）
3. **OSC 序列解析**：需 ring-buffer 处理 SSH chunk 分片；SSH worker 须从字节流剥离 OSC 序列
4. **`any_animating()`**：每加一个动画来源（closing_tabs、toasts、hover progress）都要更新，否则不收帧
5. **completion popup 定位**：iced 不回传 layout bounds，需在 App 维护 `terminal_canvas_origin: Point`，面板宽度变化时重算
6. **存储兼容**：所有新字段用 `#[serde(default)]`，新表用独立 TableDefinition，现有表不动

