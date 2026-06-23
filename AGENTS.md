# Repository Guidelines

<!-- BEGIN COMPOUND CODEX TOOL MAP -->
## Compound Codex Tool Mapping (Claude Compatibility)

This section maps Claude Code plugin tool references to Codex behavior.
Only this block is managed automatically.

Tool mapping:
- Read: use shell reads (cat/sed) or rg
- Write: create files via shell redirection or apply_patch
- Edit/MultiEdit: use apply_patch
- Bash: use shell_command
- Grep: use rg (fallback: grep)
- Glob: use rg --files or find
- LS: use ls via shell_command
- WebFetch/WebSearch: use curl or Context7 for library docs
- AskUserQuestion/Question: ask the user in chat
- Task/Subagent/Parallel: run sequentially in main thread; use multi_tool_use.parallel for tool calls
- TodoWrite/TodoRead: use file-based todos in todos/ with file-todos skill
- Skill: open the referenced SKILL.md and follow it
- ExitPlanMode: ignore
<!-- END COMPOUND CODEX TOOL MAP -->

<!-- context7 -->
Use Context7 MCP to fetch current documentation whenever the user asks about a library, framework, SDK, API, CLI tool, or cloud service -- even well-known ones like React, Next.js, Prisma, Express, Tailwind, Django, or Spring Boot. This includes API syntax, configuration, version migration, library-specific debugging, setup instructions, and CLI tool usage. Use even when you think you know the answer -- your training data may not reflect recent changes. Prefer this over web search for library docs.

Do not use for: refactoring, writing scripts from scratch, debugging business logic, code review, or general programming concepts.

## Steps

1. Always start with `resolve-library-id` using the library name and the user's question, unless the user provides an exact library ID in `/org/project` format
2. Pick the best match (ID format: `/org/project`) by: exact name match, description relevance, code snippet count, source reputation (High/Medium preferred), and benchmark score (higher is better). If results don't look right, try alternate names or queries (e.g., "next.js" not "nextjs", or rephrase the question). Use version-specific IDs when the user mentions a version
3. `query-docs` with the selected library ID and the user's full question (not single words)
4. If you weren't satisfied with the answer, call `query-docs` again for the same library with `researchMode: true`. This retries with sandboxed agents that git-pull the actual source repos plus a live web search, then synthesizes a fresh answer. More costly than the default
5. Answer using the fetched docs
<!-- context7 -->

## Project Structure & Module Organization

This repository is a Rust workspace for OpenTerm, an offline-first SSH workbench.
Source code lives under `crates/`:

- `openterm-core`: domain models and validation.
- `openterm-crypto`: local vault encryption primitives.
- `openterm-storage`: redb-backed local persistence.
- `openterm-config`: OpenSSH import and TOML export.
- `openterm-ssh`: SSH backend traits and transport boundary.
- `openterm-terminal`: terminal buffer/engine boundary.
- `openterm-ui`: UI state and product shell logic.
- `openterm-app` and `openterm-cli`: executable entrypoints.

Product direction is in `product.md`; supporting docs are in `README.md`,
`ROADMAP.md`, `SECURITY.md`, and `CONTRIBUTING.md`. Do not edit generated
`target/` output.

## Build, Test, and Development Commands

- `cargo fmt --all`: format every crate.
- `cargo check --workspace`: type-check the full workspace quickly.
- `cargo test --workspace`: run all unit and doc tests.
- `cargo run -p openterm-app`: run the current app bootstrap shell.
- `cargo run -p openterm-cli -- list-hosts`: inspect local saved hosts.
- `cargo run -p openterm-cli -- add-host local 127.0.0.1 --user "$USER"`:
  add a host profile.
- `./scripts/test_ui_smoke_real.sh`: launch the GUI, capture a screenshot, and
  verify the app window rendered.

Use `--db /path/to/test.redb` for storage tests that should not touch the
default local database.

## Coding Style & Naming Conventions

Use standard Rust style enforced by `rustfmt`: four-space indentation,
snake_case functions/modules, PascalCase types, and SCREAMING_SNAKE_CASE
constants. Keep crate boundaries clean: UI depends on domain contracts, not SSH
internals; SSH and terminal crates expose interfaces for future `russh` and
`alacritty_terminal` adapters.

Prefer explicit `thiserror` enums in libraries. Use `anyhow` only in binaries.

## Testing Guidelines

Unit tests live beside implementation files in `#[cfg(test)] mod tests`. Name
tests by behavior, for example `imports_basic_openssh_hosts` or
`wrong_password_fails`. Cover model validation, vault behavior, storage round
trips, import/export parsing, and terminal buffer logic. Run
`cargo test --workspace` before handoff.

## Commit & Pull Request Guidelines

This directory currently has no Git history, so no existing commit convention can
be inferred. Use concise imperative commits such as `Add redb host storage` or
`Fix vault decrypt error handling`.

Pull requests should include a short summary, verification commands, related
issue or roadmap item, and screenshots only for real GUI changes. Call out
security-sensitive changes to vault, secrets, known_hosts, or SSH auth paths.

## Security & Configuration Tips

Do not log passwords, private keys, passphrases, or decrypted vault contents.
Keep local databases and smoke-test files out of commits. Preserve offline,
account-free basic SSH and host management.

## Most Important Goal

OpenTerm 最重要的目标是：先成为一个可靠、可直接操作远端机器的
SSH 终端软件。所有开发顺序必须服从这个最短真实闭环：

`服务器列表 -> 连接配置 -> 用户名/密码 -> 保存 -> 连接 -> 进入主终端 -> 输入命令 -> 断开/重连 -> 新 tab 仍可用`

这个闭环没有被真实服务器验证通过前，不要优先扩展 SFTP、日志、
端口转发、诊断、vault、安全设置、导入导出或其它外围功能。

硬性验收：

- 主工作区必须 terminal-first，终端不是页面里的卡片、日志框或小 div。
- 用户名、密码、保存服务器、保存后重连必须是一条直觉路径；不要拆成多个含义不清的保存按钮。
- 已保存服务器必须能一键连接；新 tab 必须复用当前 host/session 的连接配置。
- 断开、认证失败、channel close、重连必须有清晰状态，不得用黄色横幅替代正确连接状态模型。
- 每次涉及连接、认证、PTY、tab、保存服务器的修改，都必须优先跑真实 SSH 验证，而不是只跑单元测试。

第一性原理：这个产品的底层价值不是“SSH 相关功能很多”，而是“用户能稳定控制远端 shell”。凡是不能让这个目标更真实的功能，默认延后或删除。

## Product UI Target

像素级模仿 Termius。OpenTerm 的桌面端交互目标不是“把所有 SSH 功能堆到一个页面”，而是形成类似 Termius 的会话型 SSH 工作台：左侧资产/主机导航，中间以终端为主工作区，SFTP、端口转发、命令、诊断、设置作为当前会话下的独立工具区或设置页。不要用解释性文案弥补信息架构问题；应通过布局、层级、状态和导航自然表达当前连接、工具归属和操作范围。

### Terminal Client IA Rules

基于 Termius / electerm 的桌面端模式，后续 UI 修改必须遵守：

- **禁止功能 rail**：不要在主界面常驻 `Term / Files / Fwd / Cmd / Act / Diag / Set` 这类功能清单。用户已经明确反对“按钮太多、功能太复杂”的界面。
- **Terminal-first**：连接后主区域只服务 terminal session。不要把 terminal 做成页面里的卡片、输出框或 div；它应是获得焦点后直接接收键盘事件的 PTY surface。
- **Horizontal session tabs**：会话切换应优先做成顶部/工作区 horizontal tabs，接近 Termius 新桌面版，而不是左侧功能导航。
- **Hosts are data, sessions are work**：左侧只承载 Hosts / Groups / Vault-like 资产导航，不混入 active connections、SFTP、日志、诊断等工作区功能。
- **SFTP is a separate workspace/tab**：SFTP 应作为当前 host/session 的独立 tab 或 dual-pane 文件管理界面；不要在 terminal 页面常驻一个 `Files` 按钮或说明文字来表达关系。
- **Progressive disclosure**：Forward、Diagnostics、Settings、Activity、Snippets 等非主路径能力应进入 command palette、session menu、host settings 或 terminal side panel；不要作为大按钮常驻。
- **Command palette is shortcut-first**：`Command K` 是快捷入口，不应作为顶部大文字按钮占据主界面。
- **Connection details belong in host/session editing**：`Details` 不应常驻 terminal 顶栏；连接配置、认证、跳板机、known_hosts 等应在 Host 编辑或连接弹层中处理。
- **No explanatory crutches**：禁止用 “same SSH session” 这类文案弥补信息架构问题。关系必须由 tab 层级、选中 host、session 标题和状态自然表达。
- **When in doubt, remove chrome**：如果一个控件不是连接、切换会话、输入 terminal、或当前上下文的必要操作，默认收起，不默认展示。

## Reasoning & Response Protocol

运用第一性原理思考，拒绝经验主义和路径盲从。不要假设用户完全清楚目标，保持审慎，从原始需求和问题出发。若目标模糊，请停下和用户讨论；若目标清晰但路径非最优，请直接建议更短、更低成本的办法。

所有回答必须分为两个部分：

- **直接执行**：按照用户当前的要求和逻辑，直接给出任务结果。
- **深度交互**：基于底层逻辑对用户的原始需求进行“审慎挑战”。包括但不限于：质疑用户的动机是否偏离目标（XY 问题）、分析当前路径的弊端、并给出更优雅的替代方案。

## SSH Test Target

For manual SSH smoke tests, use host `82.157.57.178` with user `ubuntu`.
Do not commit the password. Pass it through an environment variable, password
manager, or interactive prompt when testing:

```sh
cargo run -p openterm-cli -- exec 82.157.57.178 "hostname" --user ubuntu --password-env OPENTERM_TEST_PASSWORD
```

If no password environment variable is available, prefer `--password-stdin`
over `--password` so the secret does not appear in process arguments.
Use the full real smoke script first, or run individual slices when debugging:

```sh
./scripts/test_all_real.sh
./scripts/test_real_exec.sh
./scripts/test_real_sftp.sh
./scripts/test_real_forward.sh
./scripts/test_ui_smoke_real.sh
./scripts/test_ui_real_known_hosts_reuse.sh
```
