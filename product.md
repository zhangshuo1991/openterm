可以做，而且这个方向很有价值。我的建议是把它定位成：

# 一个离线优先、本地加密、纯 Rust、开源免费的轻量 SSH 工作台

不是去复制 Termius 的 SaaS，而是反着来：

```text
不强制登录
不依赖云
内网可用
本地资产管理
启动快
占用低
可审计
可自托管扩展
```

技术路线我建议定为：

```text
Rust + iced + russh + alacritty_terminal + redb + RustCrypto
```

其中 iced 负责跨平台桌面 GUI，russh 负责 SSH，alacritty_terminal 负责终端解析核心，redb 做本地嵌入式数据库，RustCrypto 系列负责本地加密。iced 官方定位就是跨平台 Rust GUI 库，russh 是 Rust SSH client/server 库，alacritty_terminal 是 Alacritty 的终端核心 crate，redb 是纯 Rust 嵌入式数据库。([Iced][1])

---

# 1. 产品定位

产品可以暂定名：

```text
OpenTerm
```

或者更有辨识度一点：

```text
RustTerm
LightSSH
OfflineTerm
IronTerm
```

我更推荐 **OpenTerm** 或 **LightSSH**，因为用户一眼能懂。

核心定位：

```text
给开发者、运维、内网环境使用的开源 SSH 客户端。
不登录，不联网，不订阅，本地优先。
```

一句话卖点：

```text
A lightweight, offline-first, open-source SSH client built in Rust.
```

中文：

```text
一个轻量、离线优先、开源免费的 Rust SSH 客户端。
```

---

# 2. 产品原则

这个产品要和 Termius 拉开差异，不要只做“另一个 SSH 工具”。

核心原则：

| 原则    | 含义                      |
| ----- | ----------------------- |
| 离线优先  | 没网也能完整使用                |
| 不强制账号 | 启动、连接、编辑 Host 都不需要登录    |
| 本地加密  | SSH 密钥、密码、Host 信息可加密存储  |
| 轻量流畅  | 不用 Electron，不嵌 Chromium |
| 开源透明  | 用户能审计配置、加密、网络行为         |
| 可迁移   | 支持导入导出，不锁死用户            |
| 可扩展   | 后续可做插件、自托管同步、团队版        |

最重要的是：

```text
基础 SSH 能力永远免费、永远离线可用。
```

这句话应该写进 README。

---

# 3. 技术总方案

## 推荐架构

```text
Desktop App
├─ UI Layer
│  ├─ iced
│  ├─ custom terminal widget
│  └─ command palette
│
├─ Terminal Layer
│  ├─ alacritty_terminal
│  ├─ terminal grid
│  ├─ scrollback buffer
│  ├─ selection / copy / paste
│  └─ keyboard / mouse escape encoding
│
├─ SSH Layer
│  ├─ russh
│  ├─ russh-keys
│  ├─ russh-sftp
│  ├─ port forwarding
│  └─ jump host
│
├─ Storage Layer
│  ├─ redb
│  ├─ serde
│  ├─ migration system
│  └─ import/export
│
├─ Security Layer
│  ├─ argon2
│  ├─ chacha20poly1305
│  ├─ zeroize
│  └─ known_hosts verification
│
└─ Runtime Layer
   ├─ tokio
   ├─ task manager
   ├─ event bus
   └─ cancellation / reconnect
```

Tokio 适合承担 SSH、SFTP、端口转发、后台任务这些异步 I/O；russh 官方文档也说明它是基于 tokio/futures 的异步 SSH client/server 库。([Docs.rs][2])

---

# 4. 技术选型

## GUI：iced

推荐：

```text
iced
```

原因：

* 纯 Rust 生态
* 跨平台
* 架构清晰
* 适合做正式桌面应用
* 比 egui 更适合做复杂业务 UI

iced 官方定位是跨平台 Rust GUI 库，强调 simplicity 和 type-safety。([Iced][1])

备选：

| 方案                 |     适合度 | 评价                  |
| ------------------ | ------: | ------------------- |
| iced               |       高 | 最适合做正式桌面软件          |
| egui               |      中高 | 开发快，但产品感稍弱          |
| winit + wgpu 自研 UI | 高性能但高成本 | 适合后期重构终端渲染          |
| Slint              |       中 | UI 强，但纯 Rust 感没那么彻底 |
| Tauri              |      不选 | 不是纯 Rust UI，虽然轻量    |

最终建议：

```text
第一版：iced + 自定义 Terminal Widget
性能瓶颈出现后：Terminal Surface 抽成独立 wgpu 渲染层
```

---

## SSH：russh

推荐：

```text
russh
russh-keys
russh-sftp
```

russh 是 Rust SSH client/server library；russh-keys 负责 SSH key 文件、加密私钥、agent 等处理；russh-sftp 提供 Russh 生态下的 SFTP client/server 支持。([Docs.rs][3])

需要支持的连接方式：

```text
密码登录
私钥登录
带 passphrase 的私钥
SSH agent
跳板机 ProxyJump
本地端口转发
远程端口转发
动态 SOCKS 转发
SFTP
```

要注意一点：

```text
纯 Rust SSH 生态没有 OpenSSH/libssh2 那么久经战场。
```

所以工程上要做两件事：

```text
1. SSH 兼容性测试矩阵
2. SSH 层抽象接口
```

接口类似：

```rust
trait SshBackend {
    async fn connect(&self, profile: HostProfile) -> Result<Session>;
    async fn open_pty(&self, size: PtySize) -> Result<PtyChannel>;
    async fn open_sftp(&self) -> Result<SftpSession>;
    async fn forward_local(&self, rule: ForwardRule) -> Result<ForwardHandle>;
}
```

这样未来即使 russh 某些场景不够稳，也可以加一个 feature-gated 的 `ssh2` 后端。但默认主线保持纯 Rust。

---

## 终端核心：alacritty_terminal

不要自己从零写终端解析器。

正确路线：

```text
SSH PTY bytes
↓
alacritty_terminal
↓
terminal grid
↓
自定义渲染
```

alacritty_terminal 暴露了终端 grid、term、vte 等模块，适合复用成熟终端模拟器的解析能力。([Docs.rs][4])

终端要支持：

```text
ANSI escape
256 色
TrueColor
光标移动
alternate screen
scrollback
鼠标事件
bracketed paste
vim / nano / tmux / htop
中文宽字符
emoji
复制粘贴
搜索
resize
```

最容易翻车的地方不是 SSH，而是：

```text
终端渲染 + 输入事件 + Unicode 宽度
```

Unicode 宽度可以结合 `unicode-width` 做辅助判断，它的文档说明这个 crate 用于确定 char 和 str 的显示宽度。([Docs.rs][5])

---

## 文本渲染：glyphon / cosmic-text

终端渲染不能简单地一行行创建普通 UI 文本控件，那样性能会很差。

推荐路线：

```text
普通 UI 文本：iced 默认能力
终端文本：自定义 TerminalRenderer
字体 shaping / fallback：cosmic-text
GPU 文本绘制：glyphon
```

glyphon 官方描述是基于 wgpu、cosmic-text 和 etagere 的 2D 文本渲染库；cosmic-text 提供 shaping、font discovery、font fallback、layout、rasterization 等文本处理能力。([Docs.rs][6])

终端渲染策略：

```text
只绘制可视区域
只在 dirty 时重绘
按 cell grid 缓存 glyph
按行合并相同 style 的 run
scrollback 虚拟化
selection 单独 overlay
cursor 单独 overlay
```

不要每帧全量重画整个 scrollback。

---

## 本地存储：redb

推荐：

```text
redb
```

原因：

* 纯 Rust
* 嵌入式
* key-value
* 适合本地配置库
* 事务模型清晰

redb 文档称它是 simple、portable、high-performance、ACID 的嵌入式 key-value store，并且是 pure Rust。([Docs.rs][7])

数据分层：

```text
app_settings
host_profiles
groups
tags
identities
known_hosts
snippets
tunnels
sftp_bookmarks
recent_sessions
encrypted_secrets
migration_state
```

---

## 本地加密

推荐：

```text
argon2
chacha20poly1305
zeroize
```

设计：

```text
用户主密码
↓
Argon2id 派生 vault key
↓
ChaCha20Poly1305 加密 secrets
↓
zeroize 清理内存中的敏感数据
```

argon2 crate 是 Argon2 password hashing 的纯 Rust 实现；chacha20poly1305 crate 是 ChaCha20Poly1305 AEAD 的纯 Rust 实现；zeroize 用于安全清理内存，避免清零操作被编译器优化掉。([Crates][8])

加密范围建议：

| 数据          | 是否加密         |
| ----------- | ------------ |
| SSH 密码      | 必须           |
| 私钥内容        | 必须           |
| passphrase  | 必须           |
| Host 地址     | 可选           |
| 用户名         | 可选           |
| 分组/标签       | 默认明文，可提供隐私模式 |
| known_hosts | 明文即可         |
| snippets    | 可选           |

第一版建议：

```text
默认只加密 secrets。
高级选项提供 “加密全部资产库”。
```

这样用户体验和安全性比较平衡。

---

# 5. 最终产品形态

## 产品主界面

整体结构建议是三栏布局：

```text
┌──────────────────────────────────────────────────────────────┐
│ Top Bar: Quick Connect | Search | Command Palette | Settings │
├───────────────┬──────────────────────────────────┬───────────┤
│ Sidebar       │ Terminal Workspace               │ Inspector │
│               │                                  │           │
│ Hosts         │ Tabs                             │ Host Info │
│ Groups        │ ┌──────────────────────────────┐ │ Snippets  │
│ Tags          │ │ terminal session             │ │ SFTP      │
│ Recent        │ │                              │ │ Tunnels   │
│ Tunnels       │ │                              │ │ Logs      │
│               │ └──────────────────────────────┘ │           │
└───────────────┴──────────────────────────────────┴───────────┘
```

左侧是资产，中间是工作区，右侧是上下文面板。

这个布局比 Termius 更适合桌面重度用户，因为：

```text
Host 管理、终端操作、文件传输、Snippet 不互相抢空间。
```

---

## 首页设计

首次打开不要要求登录，直接进入：

```text
Welcome to OpenTerm

[ Quick Connect ]
Host: 192.168.1.10
User: root
Port: 22

[ Connect ]

[ Import ~/.ssh/config ]
[ Create Host ]
[ Open Local Terminal ]
```

首页底部写清楚：

```text
No account required. No cloud dependency. Your data stays local.
```

这就是产品价值。

---

## Host 管理页

Host 列表要比传统 SSH 客户端好用。

字段设计：

```text
Name
Host
Port
Username
Auth method
Identity
Group
Tags
Jump host
Default directory
Environment
Startup command
SFTP root
Terminal theme
```

Host Card UI：

```text
┌─────────────────────────────┐
│ prod-api-01                 │
│ root@10.20.1.15:22          │
│ Tags: prod api singapore    │
│ Jump: bastion-prod          │
│ Last: 2 hours ago           │
│ [Connect] [SFTP] [Edit]     │
└─────────────────────────────┘
```

Host 支持：

```text
分组
标签
收藏
最近连接
模糊搜索
批量编辑
导入 OpenSSH config
导出 JSON/TOML
```

---

## 连接页

连接时要有清晰状态：

```text
Resolving host...
Connecting TCP...
Negotiating SSH...
Authenticating...
Opening PTY...
Connected.
```

失败时不要只弹错误码，要给人话：

```text
Connection refused
可能原因：
1. 目标机器 22 端口没开
2. 防火墙阻断
3. SSH 服务没启动

[ Retry ]
[ Edit Host ]
[ Open Diagnostics ]
```

---

## 已知主机指纹确认

第一次连接必须出现：

```text
Unknown host key

Host: 10.20.1.15
Algorithm: ssh-ed25519
Fingerprint: SHA256:xxxxxxxx

Do you trust this host?

[ Trust Once ] [ Save and Connect ] [ Cancel ]
```

Host key 变化时必须强警告：

```text
WARNING: Host key changed.
This may indicate a man-in-the-middle attack.
```

这是 SSH 客户端的底线。

---

## Terminal 工作区

中间区域：

```text
┌────────────────────────────────────────────────────┐
│ prod-api-01   prod-db-01   local                  │
├────────────────────────────────────────────────────┤
│ root@prod-api-01:~#                               │
│                                                    │
│                                                    │
└────────────────────────────────────────────────────┘
```

功能：

```text
多 Tab
Split Pane
复制粘贴
搜索
字体大小
主题
当前连接状态
重连
日志保存
快捷命令
```

右键菜单：

```text
Copy
Paste
Select All
Search
Clear Scrollback
Duplicate Session
Open SFTP Here
Save Output
```

---

## Command Palette

一定要做这个。

快捷键：

```text
Ctrl/Cmd + K
```

输入：

```text
> connect prod
> new host
> import ssh config
> open sftp
> forward 8080
> theme dark
```

这是现代工具的核心体验。

命令面板能替代很多复杂菜单。

---

## Snippets 设计

Snippet 不是简单命令收藏，要支持变量。

示例：

```bash
docker logs -f {{container}}
```

执行时弹出参数：

```text
container: nginx
```

支持：

```text
全局 Snippet
分组 Snippet
Host 专属 Snippet
变量
确认执行
危险命令标记
```

危险命令如：

```bash
rm -rf
reboot
shutdown
systemctl restart
```

执行前提醒。

---

## SFTP 设计

右侧或独立 Tab：

```text
┌────────────── Local ─────────────┬──────────── Remote ────────────┐
│ /Users/me/project                │ /var/www/app                   │
│ file1                            │ app.js                         │
│ file2                            │ nginx.conf                     │
└──────────────────────────────────┴────────────────────────────────┘
```

功能优先级：

```text
浏览目录
上传
下载
拖拽上传
新建文件夹
删除
重命名
chmod
复制路径
打开远程目录
传输队列
失败重试
```

不要第一版就做远程编辑器，容易膨胀。

---

## Port Forwarding UI

隧道管理页：

```text
Local Forward
localhost:8080 → prod-api-01:80

Remote Forward
prod-api-01:9000 → localhost:3000

Dynamic SOCKS
localhost:1080
```

状态显示：

```text
Running
Stopped
Error
Bytes in/out
Active connections
```

按钮：

```text
[Start] [Stop] [Edit] [Copy Command]
```

---

# 6. 功能设计

## V1 必须有

第一版必须把基础体验打磨好：

```text
Host 管理
SSH 连接
交互式终端
密码登录
私钥登录
known_hosts
多 Tab
复制粘贴
搜索
本地加密 vault
导入 ~/.ssh/config
导出配置
基础 SFTP
本地端口转发
暗色/亮色主题
离线可用
```

这就是能替代 80% Termius 基础使用的版本。

---

## V1 不建议做

先不要做：

```text
云同步
团队协作
插件系统
AI 命令助手
复杂审计
远程编辑器
移动端
Web 版
Mosh
Kubernetes 面板
```

这些会拖慢核心产品。

第一版的目标不是“大而全”，而是：

```text
轻、稳、快、离线好用。
```

---

## V2 增强功能

```text
Split panes
批量连接
批量命令
高级 SFTP
远程文件预览
端口转发管理器
动态 SOCKS
SSH Agent 集成增强
自托管同步
配置文件 Git 同步
Portable mode
插件 API
```

---

## V3 护城河功能

```text
自托管团队同步
本地审计日志
密钥轮换提醒
堡垒机工作流
Session Recorder
可视化网络拓扑
多环境 Workspace
远程命令模板市场
企业策略配置
```

注意：

```text
企业功能可以收费，但基础 SSH 客户端必须永久免费。
```

这样不会重蹈 Termius 的口碑问题。

---

# 7. 核心数据模型

## HostProfile

```rust
struct HostProfile {
    id: HostId,
    name: String,
    host: String,
    port: u16,
    username: Option<String>,
    group_id: Option<GroupId>,
    tags: Vec<String>,
    auth: AuthRef,
    jump: Option<JumpConfig>,
    terminal: TerminalProfile,
    startup: Option<StartupCommand>,
    sftp: Option<SftpProfile>,
    created_at: Timestamp,
    updated_at: Timestamp,
}
```

## Identity

```rust
enum IdentityKind {
    Password,
    PrivateKeyFile,
    PrivateKeyManaged,
    Agent,
}

struct Identity {
    id: IdentityId,
    name: String,
    kind: IdentityKind,
    username_hint: Option<String>,
    encrypted_secret_ref: Option<SecretId>,
}
```

## Secret

```rust
struct EncryptedSecret {
    id: SecretId,
    version: u32,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    aad: Vec<u8>,
    kdf: KdfParams,
}
```

## Tunnel

```rust
enum TunnelKind {
    Local,
    Remote,
    DynamicSocks,
}

struct TunnelProfile {
    id: TunnelId,
    name: String,
    host_id: HostId,
    kind: TunnelKind,
    bind_addr: String,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
    auto_start: bool,
}
```

## Snippet

```rust
struct Snippet {
    id: SnippetId,
    name: String,
    command: String,
    variables: Vec<SnippetVariable>,
    scope: SnippetScope,
    require_confirm: bool,
    danger_level: DangerLevel,
}
```

---

# 8. 工程模块设计

建议 monorepo：

```text
openterm/
├─ Cargo.toml
├─ crates/
│  ├─ openterm-app/
│  │  └─ 桌面入口
│  │
│  ├─ openterm-ui/
│  │  └─ iced UI、页面、组件
│  │
│  ├─ openterm-terminal/
│  │  └─ alacritty_terminal 适配、grid、selection、renderer
│  │
│  ├─ openterm-ssh/
│  │  └─ russh、PTY、SFTP、tunnel、jump host
│  │
│  ├─ openterm-storage/
│  │  └─ redb、schema、migration
│  │
│  ├─ openterm-crypto/
│  │  └─ vault、KDF、加密、zeroize
│  │
│  ├─ openterm-config/
│  │  └─ import/export、OpenSSH config parser
│  │
│  ├─ openterm-core/
│  │  └─ domain model、event bus、commands
│  │
│  └─ openterm-cli/
│     └─ 命令行工具，用于调试、导入导出、测试连接
│
├─ assets/
├─ docs/
├─ examples/
└─ tests/
```

核心依赖方向必须单向：

```text
ui -> core -> ssh/storage/crypto/terminal
```

不要让 SSH 层依赖 UI。

---

# 9. 事件模型

桌面 app 很容易乱，建议一开始就做 event-driven。

```text
UI Command
↓
App Controller
↓
Domain Service
↓
Async Task
↓
Event Bus
↓
State Update
↓
UI Render
```

示例事件：

```rust
enum AppEvent {
    HostCreated(HostId),
    SessionConnecting(SessionId),
    SessionConnected(SessionId),
    SessionOutput(SessionId, Bytes),
    SessionClosed(SessionId),
    SftpTransferProgress(TransferId, Progress),
    TunnelStarted(TunnelId),
    VaultLocked,
    VaultUnlocked,
}
```

会话状态：

```rust
enum SessionState {
    Idle,
    Connecting,
    Authenticating,
    Connected,
    Reconnecting,
    Closed,
    Failed(String),
}
```

---

# 10. 终端实现细节

终端是这个项目最难的地方。

## 数据流

```text
Keyboard / Mouse
↓
TerminalInputEncoder
↓
SSH PTY Channel
↓
Remote Shell
↓
SSH bytes
↓
alacritty_terminal parser
↓
TerminalGrid
↓
Renderer
```

## 渲染优化

必须做：

```text
可视区域裁剪
dirty line tracking
glyph cache
style run 合并
scrollback ring buffer
后台输出限速刷新
输入优先级高于输出刷新
```

刷新策略：

```text
用户输入：立即刷新
远程输出：批量刷新，最多 60fps
后台大量输出：自动降帧，避免 CPU 飙高
窗口不可见：暂停渲染，只更新 buffer
```

## Scrollback

不要无限 Vec。

使用：

```text
ring buffer
默认 10,000 行
可配置 100,000 行
超过后丢弃最老行
```

## 兼容性测试

必须测试：

```text
bash
zsh
fish
vim
nano
tmux
htop
top
less
systemctl status
docker logs -f
kubectl logs -f
中文输入
emoji
鼠标选择
鼠标滚轮
bracketed paste
```

---

# 11. SSH 实现细节

## 连接流程

```text
load host profile
↓
unlock vault if needed
↓
resolve identity
↓
connect TCP
↓
verify host key
↓
authenticate
↓
request PTY
↓
open shell
↓
bind terminal
```

## Jump Host

第一版优先做 OpenSSH 常见的：

```text
ProxyJump
```

即：

```text
local -> bastion -> target
```

实现方式：

```text
先连接 bastion
在 bastion 上 open direct-tcpip channel 到 target
再在这个 channel 上跑第二层 SSH
```

## Port Forward

```text
Local Forward:
local socket -> ssh channel -> remote host:port

Remote Forward:
remote listener -> ssh channel -> local host:port

Dynamic SOCKS:
local SOCKS5 -> ssh channel -> target host:port
```

V1 先做 Local Forward，V2 再做 Remote 和 Dynamic SOCKS。

---

# 12. 安全设计

## Vault

首次保存密码或托管私钥时提示：

```text
Create Local Vault

Master Password:
Confirm Password:

[Create Vault]
```

解锁后：

```text
15 分钟无操作自动锁定
系统睡眠后自动锁定
手动 Lock Vault
```

## 不做的事

明确承诺：

```text
不上传 Host
不上传密钥
不上传命令
不强制登录
不默认遥测
```

## Host Key 策略

```text
Strict known_hosts
首次连接确认
变更强警告
支持查看 fingerprint
支持删除旧 host key
```

## Secret 内存管理

使用：

```text
zeroize
SecretString / SecretVec
最小化 clone
日志永远不打印 secret
panic report 去敏
```

zeroize 的用途正是安全清理内存，避免清零被优化掉。([Crates][9])

---

# 13. UI 视觉设计

## 风格

建议关键词：

```text
现代
克制
高对比
低干扰
开发者工具感
```

不要做花哨渐变，不要像游戏启动器。

颜色：

```text
背景：深灰 / 近黑
主色：蓝 / 青 / 绿三选一
危险色：红
警告色：黄
成功色：绿
边框：低对比灰
```

字体：

```text
UI 字体：系统默认
Terminal 字体：JetBrains Mono / Fira Code / Cascadia Mono / 用户自选
```

注意：不要把字体打包进发行包，除非确认授权。默认使用系统字体即可。

---

## 信息密度

提供两种密度：

```text
Comfortable
Compact
```

运维用户通常喜欢 Compact。

---

## 主题

内置：

```text
Dark
Light
High Contrast
Solarized Dark
Dracula-like
Gruvbox-like
```

但主题文件要开放：

```toml
[theme]
background = "#101014"
foreground = "#e6e6e6"
cursor = "#ffffff"
selection = "#334155"
```

---

# 14. 快捷键设计

| 快捷键                  | 功能                |
| -------------------- | ----------------- |
| Ctrl/Cmd + K         | Command Palette   |
| Ctrl/Cmd + T         | 新建连接 Tab          |
| Ctrl/Cmd + W         | 关闭 Tab            |
| Ctrl/Cmd + Shift + F | 搜索终端输出            |
| Ctrl/Cmd + Shift + C | 复制                |
| Ctrl/Cmd + Shift + V | 粘贴                |
| Ctrl/Cmd + ,         | 设置                |
| Ctrl/Cmd + 1..9      | 切换 Tab            |
| Ctrl/Cmd + D         | Duplicate Session |
| Ctrl/Cmd + Shift + S | 打开 SFTP           |
| Ctrl/Cmd + Shift + L | 锁定 Vault          |

macOS 上要适配 Cmd，Windows/Linux 用 Ctrl。

---

# 15. 设置页设计

设置分组：

```text
General
Appearance
Terminal
SSH
SFTP
Vault
Import / Export
Advanced
About
```

Terminal 设置：

```text
Font family
Font size
Line height
Cursor style
Blink cursor
Scrollback lines
Copy on select
Right click behavior
Bracketed paste
```

SSH 设置：

```text
Default username
Default port
Connection timeout
Keepalive interval
Known hosts path
Agent integration
Strict host key checking
```

Vault 设置：

```text
Change master password
Auto lock timeout
Lock on sleep
Encrypt host metadata
Export encrypted backup
```

---

# 16. 实现里程碑

不要一上来做完整产品。按下面顺序推进。

---

## P0：技术验证

目标：

```text
证明纯 Rust 路线能跑通。
```

交付物：

```text
iced 窗口
russh 连接 localhost / 内网机器
打开 PTY
接收远端输出
键盘输入写回远端
alacritty_terminal 解析输出
简单绘制 terminal grid
```

验收标准：

```text
能登录 Linux
能运行 ls / top / vim
窗口 resize 后 PTY 尺寸同步
Ctrl+C / Ctrl+D 正常
```

这一步只做一个硬编码 Host，不做 UI 美化。

---

## P1：最小可用 SSH 客户端

目标：

```text
单机离线可用。
```

交付物：

```text
Host CRUD
密码登录
私钥登录
known_hosts
多 Tab
基础终端复制粘贴
配置持久化
错误提示
```

验收标准：

```text
用户可以添加 Host
点击连接
正常使用 shell
关闭重开后配置仍在
无外网也能完整使用
```

这是第一个 dogfood 版本。

---

## P2：本地 Vault 和导入导出

目标：

```text
安全保存资产。
```

交付物：

```text
Vault 创建
Vault 解锁/锁定
密码加密保存
私钥加密保存
导入 ~/.ssh/config
导出 JSON/TOML
备份恢复
```

验收标准：

```text
数据库里看不到明文密码
Vault 锁定后不能连接需要 secret 的 Host
OpenSSH config 能导入常见 Host
```

---

## P3：终端体验打磨

目标：

```text
从“能用”变成“好用”。
```

交付物：

```text
终端搜索
scrollback
主题
字体设置
右键菜单
快捷键
鼠标选择
bracketed paste
中文宽字符处理
性能优化
```

验收标准：

```text
vim/tmux/htop/less 可用
大量日志输出不卡 UI
中文显示正常
复制粘贴体验顺滑
```

---

## P4：SFTP 和端口转发

目标：

```text
覆盖运维高频场景。
```

交付物：

```text
SFTP 文件浏览
上传下载
传输队列
删除/重命名/新建目录
Local Port Forward
Tunnel 状态管理
```

验收标准：

```text
能替代基础 FileZilla/SFTP 操作
能配置 localhost:8080 -> remote:80
断线后状态清晰
```

---

## P5：高级连接能力

目标：

```text
适配真实内网和生产环境。
```

交付物：

```text
ProxyJump
多级跳板
SSH agent
批量编辑 Host
分组/标签增强
连接诊断
自动重连
Session duplicate
```

验收标准：

```text
能通过堡垒机连接内网机器
能快速定位连接失败原因
Host 数量超过 500 仍然搜索流畅
```

---

## P6：公开发布版本

目标：

```text
可以开源推广。
```

交付物：

```text
官网/README
安装包
自动构建
签名
崩溃日志本地化
文档
贡献指南
安全策略
Issue 模板
Roadmap
```

验收标准：

```text
Windows/macOS/Linux 都能安装
普通用户能看 README 上手
开发者能按 CONTRIBUTING 编译
```

---

# 17. 性能目标

这些是产品目标，不是凭空承诺，最终要靠 benchmark 验证。

| 指标         |                 目标 |
| ---------- | -----------------: |
| 冷启动        |           体感接近原生应用 |
| 空闲 CPU     |               接近 0 |
| 单窗口空闲内存    | 明显低于 Electron 类客户端 |
| 终端输出       |           大量日志不卡输入 |
| Tab 数量     |          20 个连接仍可用 |
| Host 数量    |        1,000 条搜索不卡 |
| Scrollback |        默认 10,000 行 |
| 离线启动       |            100% 可用 |

核心优化策略：

```text
不要后台联网
不要全量刷新 UI
不要每行一个 widget
不要把 scrollback 全部渲染
不要在 UI 线程做 SSH I/O
```

---

# 18. 测试方案

## 单元测试

```text
Host model
Vault encryption/decryption
Migration
OpenSSH config import
Snippet variables
Tunnel config validation
```

## 集成测试

用 Docker 或本地 OpenSSH 起测试服务器：

```text
password auth
key auth
wrong password
host key changed
sftp upload/download
port forwarding
jump host
```

OpenSSH 官方文档覆盖了 ssh、sftp、known_hosts、public key 文件等手册资料，测试行为应尽量对齐 OpenSSH 常见语义。([openssh.org][10])

## 终端兼容性测试

```text
vttest
vim
tmux
htop
less
nano
中文
emoji
鼠标
resize
```

## 安全测试

```text
secret 不进日志
panic 不打印敏感信息
Vault 锁定后 secret 清理
数据库无明文密码
host key changed 强警告
```

## 性能测试

```text
1000 hosts 搜索
100000 行 scrollback
持续 docker logs 输出
20 个 session 同时在线
大文件 SFTP 上传下载
```

---

# 19. 开源策略

建议许可证：

```text
MIT / Apache-2.0 双许可
```

这对 Rust 生态比较友好，商业公司也更愿意采用。

仓库结构：

```text
README.md
ROADMAP.md
CONTRIBUTING.md
SECURITY.md
CODE_OF_CONDUCT.md
docs/
examples/
```

README 第一屏要突出：

```text
No login required
Offline-first
Local encrypted vault
Open source
Built with Rust
```

不要写太多技术炫耀，用户更关心：

```text
能不能离线？
稳不稳？
能不能导入配置？
密钥安不安全？
```

---

# 20. 商业化边界

基础版必须坚持：

```text
免费
开源
离线
本地 Host 管理
基础 SSH
基础 SFTP
基础端口转发
```

可以商业化的部分：

```text
企业支持
自托管同步服务器
团队策略管理
审计日志
SSO
堡垒机集成
私有插件市场
企业签名包
```

不要做：

```text
登录后才能 SSH
订阅后才能保存 Host
云不可用导致本地不可用
```

这正是用户讨厌 Termius 的地方。

---

# 21. 最大技术风险

## 风险一：终端渲染复杂

解决：

```text
复用 alacritty_terminal
不要自研 ANSI parser
TerminalRenderer 独立封装
尽早测试 vim/tmux/htop
```

## 风险二：russh 兼容性

解决：

```text
SSH backend 抽象
大量 OpenSSH 兼容测试
先支持主流算法
保留可插拔后端
```

## 风险三：UI 性能

解决：

```text
终端区域不要堆普通 widget
可视区域渲染
dirty line
glyph cache
输出批处理
```

## 风险四：加密设计出错

解决：

```text
只用成熟 crate
不要自创加密算法
schema versioning
安全审计
威胁模型文档
```

## 风险五：功能膨胀

解决：

```text
V1 只做 SSH 核心体验
云同步、团队协作、AI 全部延后
```

---

# 22. 最推荐的最终路线

我会这样定版：

```text
语言：Rust
GUI：iced
异步运行时：tokio
SSH：russh
SSH Key：russh-keys
SFTP：russh-sftp
终端核心：alacritty_terminal
文本渲染：iced 初期，glyphon/cosmic-text 优化终端
数据库：redb
序列化：serde
加密：argon2 + chacha20poly1305 + zeroize
配置导入：OpenSSH config parser 自研或独立 crate
构建：cargo + GitHub Actions
平台：Windows / macOS / Linux
许可证：MIT / Apache-2.0
```

一句话架构：

```text
iced 做产品壳，russh 做连接，alacritty_terminal 做终端脑子，redb 做本地资产库，RustCrypto 做本地保险箱。
```

---

# 23. MVP 的最小闭环

第一版不要贪。只要做到下面这些，就可以开始内测：

```text
打开软件不登录
添加 Host
保存 Host
连接 SSH
交互式 shell 可用
私钥登录可用
known_hosts 可用
复制粘贴可用
断线提示清楚
关闭重开配置还在
```

这就是最小闭环。

然后第二步再加：

```text
Vault
导入 ~/.ssh/config
Tab
主题
SFTP
Port Forward
```

---

# 24. 最终产品应该长什么样

最终它应该给用户这种感觉：

```text
像 Termius 一样好看
像 PuTTY 一样可靠
像 Alacritty 一样轻快
像 OpenSSH 一样朴素可信
```

不要把自己做成“云 SSH 平台”。

应该做成：

```text
开发者电脑上的离线 SSH 工作台。
```

这是它和 Termius 最大的差异，也是最有机会打动用户的点。

[1]: https://iced.rs/?utm_source=chatgpt.com "iced - A cross-platform GUI library for Rust"
[2]: https://docs.rs/tokio/latest/tokio/?utm_source=chatgpt.com "tokio - Rust - Docs.rs"
[3]: https://docs.rs/russh/latest/russh/?utm_source=chatgpt.com "russh - Rust - Docs.rs"
[4]: https://docs.rs/alacritty_terminal/latest/alacritty_terminal/?utm_source=chatgpt.com "alacritty_terminal - Rust - Docs.rs"
[5]: https://docs.rs/unicode-width/latest/unicode_width/?utm_source=chatgpt.com "unicode_width - Rust - Docs.rs"
[6]: https://docs.rs/glyphon/latest/glyphon/index.html?utm_source=chatgpt.com "glyphon - Rust - Docs.rs"
[7]: https://docs.rs/redb/latest/redb/?utm_source=chatgpt.com "redb - Rust - Docs.rs"
[8]: https://crates.io/crates/argon2?utm_source=chatgpt.com "argon2 - crates.io: Rust Package Registry"
[9]: https://crates.io/crates/zeroize?utm_source=chatgpt.com "zeroize - crates.io: Rust Package Registry"
[10]: https://www.openssh.org/manual.html?utm_source=chatgpt.com "Manual Pages - OpenSSH"

