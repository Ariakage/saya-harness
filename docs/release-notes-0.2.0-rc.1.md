# Saya Harness 0.2.0-rc.1

[简体中文](#简体中文) · [English](#english)

## 简体中文

**0.2.0-rc.1 候选说明，更新于 2026-11-08。本次范围为 N0 共享基础与 N1 默认 TUI，供 CI 验收和本机测试。正式发布安排在后续代码 review 与其他测试之后。当前实现及验证证据见[开发进度](progress.md)和[TUI 验证](tui-validation.md)。**

### 默认 TUI 与入口迁移

- stdin 和 stdout 均连接终端时，直接运行 `saya` 进入全屏 TUI，可继续使用 `--config`、`--cwd`、`--mode` 等全局参数。
- 原有文本交互使用显式 `saya chat`。`saya run`、`saya resume`、`saya session`、`saya logs` 与 `saya config` 继续使用各自的 CLI 入口。
- 无子命令且缺少终端，或显式选择 JSON／JSONL 输出时，返回用法错误和退出码2。脚本可使用 `saya run --stdin` 或其他显式子命令。
- TUI 复用共享应用层的模型、会话、权限和运行控制。PLAN、BUILD、CODE、WORK 四工作模式与 auto、Manual Approval、bypass permissions 三权限模式继续生效。

### 对话与运行控制

- 流式聊天、多行 UTF-8／grapheme 编辑、粘贴、代码围栏、历史滚动及工作区／模型／模式／权限／用量顶栏。
- Enter 发送，Alt+Enter 或 Ctrl+J 换行，Ctrl+P 打开命令面板，PgUp／PgDown 滚动，End 恢复跟随输出，Ctrl+C 取消，输入为空时 Ctrl+D 请求退出。
- `/mode plan|build|code|work` 请求切换模式，界面同时显示当前与待生效模式。`/permissions auto|manual|bypass` 在运行和 UI 任务均空闲时修改权限。
- 人工审批显示操作、参数、工作区及关联 ID。详情可见且输入为空时，新的普通／Shift `y` 批准，`n` 或 Esc 拒绝。澄清通过输入框回复，`//` 可发送字面量斜杠。
- 100列起对话与详情并列，较窄终端的详情占用主区。草稿64 KiB、单条消息256 KiB、聊天正文合计2 MiB／512条，详情缓存2 MiB／10,000逻辑行，省略和截断保留可见提示。界面布局及预览见[TUI 设计说明](tui-design.md)。

### 会话、工具与导出

- `/new`、`/sessions [PAGE]`、`/resume ID` 提供新建、分页和恢复。`/profiles` 与 `/profile NAME` 查看和选择已配置 profile，兼容配置变化通过 `/resume-accept ID` 显式接受。
- 恢复会话使用本次权限设置，等待新输入。中断响应、工具 `unknown`、错误与截断按已保存记录呈现。
- `/tools` 与 `/jobs` 查看一致性快照及持久水位，工具调用通过 `run_id` 和 `call_id` 关联。`/cancel-job ID` 请求取消当前运行拥有的后台任务，后台任务生命周期继续限于当前运行。
- `/export json|markdown PATH` 导出对话，`/export-full json|markdown PATH` 显式包含完整范围与工具正文。相对路径基于当前会话工作区，目标父目录须已存在，已有文件受保护。
- CLI 可离线执行 `saya session export SESSION_ID --file ./session.md`，JSON 文件使用 `--format json`，完整工具记录使用 `--scope full --include-tools`。两种格式复用脱敏投影，回执包含摘要与快照水位，每字段64 KiB、每导出32 MiB。PLAN 导出目标须位于工作区外。[会话导出说明](session-export.md)

### 本地日志与共享基础

- trace、debug、info、warn、error 五级诊断，默认 warn，可使用 `--log-level` 或 `SAYA_LOG_LEVEL` 选择。类型化字段及会话／运行／请求／调用／后台 ID 用于关联，诊断与运行机器输出分别处理。
- `saya logs show --session SESSION_ID --level info --limit 50` 查询本地记录，`saya --output jsonl logs tail --session SESSION_ID` 跟随日志，Ctrl+C 退出 tail。
- 日志默认位于 `$XDG_STATE_HOME/saya/saya.log` 或 `~/.local/state/saya/saya.log`，单文件10 MiB、最多五份备份。显示、查询与导出重新应用当前配置已知凭据脱敏。[日志说明](observability.md)

### 取消与终端恢复

Ctrl+C 请求取消所属运行或 UI 任务，并等待收尾。导出已经完成发布时，取消回复仍报告实际发布结果。正常退出、可恢复错误、panic unwind 与受处理的 SIGINT／SIGTERM／SIGHUP 进入终端恢复路径，处理 raw mode、光标、括号粘贴与 alternate screen。终端输出阻塞或连接失效时，屏幕恢复使用有界尝试。

SIGKILL、进程崩溃或断电可能留下终端状态与未知执行结果。可在 shell 中恢复终端，然后检查会话记录和实际文件，确认操作结果。

```sh
stty sane
printf '\033[?2004l\033[?25h\033[?1049l'
```

### 平台与候选验收

| 支持基线 | 架构 |
| --- | --- |
| Linux，glibc 2.35+ | x86_64、aarch64 |
| macOS 15+ | Intel x86_64、Apple Silicon aarch64 |

构建继续使用 Rust 1.99.1／edition 2024。N0 已有本机验收证据，N1 的最终正常入口、解包程序及四平台验收按开发进度继续记录。本草案尚未确认候选下载、哈希、签名或正式发行状态。

### 已知限制与后续范围

- OS Sandbox、MCP、自动上下文压缩和可执行插件仍属后续开发范围。命令继续使用宿主权限，进程组清理无法约束主动脱离的后代进程。
- TUI 的有界显示窗口可能省略较早或较长内容，导出也有明确限额。恢复和工具快照保留未知结果，需要结合工作区核查实际副作用。
- 凭据脱敏依据当前可解析的已知配置值及持久化标记。文件操作保持单文件保证，外部并发改动仍可能竞争。
- macOS 候选程序的开发者签名、公证与实际产物状态待发行记录确认。跨运行持久任务、跨协议会话迁移、GUI、Goal 和 Browser 按后续计划推进。[后续开发计划](post-mvp-plan.md)

## English

**0.2.0-rc.1 candidate notes, updated on 2026-11-08. This candidate covers N0 shared foundations and N1 default TUI for CI acceptance and local testing. Formal publication follows subsequent code review and additional testing. Current implementation and evidence are tracked in [development progress](progress.md) and [TUI validation](tui-validation.md).**

### Default TUI and entry-point migration

- Running `saya` opens the full-screen TUI when both stdin and stdout are terminals. Global options including `--config`, `--cwd`, and `--mode` continue to apply.
- Use explicit `saya chat` for the existing text conversation interface. `saya run`, `saya resume`, `saya session`, `saya logs`, and `saya config` retain their CLI entry points.
- With no subcommand, a missing terminal or explicit JSON／JSONL output returns a usage error with exit code 2. Scripts can use `saya run --stdin` or another explicit subcommand.
- The TUI reuses shared model, session, permission, and runtime controls. PLAN, BUILD, CODE, and WORK modes and auto, Manual Approval, and bypass permissions continue to apply.

### Conversation and runtime controls

- Streaming conversation, multiline UTF-8／grapheme editing, paste, code fences, history scrolling, and a workspace／model／mode／permissions／usage header.
- Enter sends, Alt+Enter or Ctrl+J adds a newline, Ctrl+P opens commands, PgUp／PgDown scroll, End follows new output, Ctrl+C cancels, and Ctrl+D requests exit when the draft is empty.
- `/mode plan|build|code|work` requests a mode change while the header shows current and pending modes. `/permissions auto|manual|bypass` changes permissions while both runtime and UI operations are idle.
- Approval details include the operation, arguments, workspace, and correlation IDs. With approval details visible and the draft empty, a fresh ordinary／Shift `y` approves, and `n` or Esc denies. Questions accept typed replies, and `//` sends a literal leading slash.
- At 100 columns, conversation and details appear side by side. Narrower terminals show details in the main area. Budgets are 64 KiB for a draft, 256 KiB per message, 2 MiB／512 messages for chat bodies, and 2 MiB／10,000 logical lines for cached details, with visible omission and truncation markers. [TUI design and previews](tui-design.md)

### Sessions, tools, and export

- `/new`, `/sessions [PAGE]`, and `/resume ID` create, browse, and restore sessions. `/profiles` and `/profile NAME` inspect and select configured profiles. `/resume-accept ID` explicitly accepts compatible configuration changes.
- Restored sessions use the current invocation's permissions and wait for new input. Interrupted responses, tool `unknown` outcomes, errors, and truncation remain visible according to stored records.
- `/tools` and `/jobs` display consistent snapshots and committed watermarks. Tool calls are correlated by `run_id` and `call_id`. `/cancel-job ID` requests cancellation of a background job owned by the active run. Background jobs remain scoped to that run.
- `/export json|markdown PATH` exports a transcript. `/export-full json|markdown PATH` explicitly includes full scope and tool bodies. Relative paths resolve from the session workspace, the parent directory must exist, and existing files are protected.
- The offline CLI supports `saya session export SESSION_ID --file ./session.md`. Use `--format json` for JSON and `--scope full --include-tools` for full tool records. Both formats share redacted projections and receipts with hashes and snapshot watermarks. Limits are 64 KiB per field and 32 MiB per export. PLAN export targets must be outside the workspace. [Session export](session-export.md)

### Local logs and shared foundations

- Five diagnostic levels, trace, debug, info, warn, and error, defaulting to warn. Select a level with `--log-level` or `SAYA_LOG_LEVEL`. Typed fields and session／run／request／call／job IDs provide correlation, and diagnostics are handled separately from runtime machine output.
- `saya logs show --session SESSION_ID --level info --limit 50` queries local records. `saya --output jsonl logs tail --session SESSION_ID` follows logs, with Ctrl+C stopping tail.
- Logs default to `$XDG_STATE_HOME/saya/saya.log` or `~/.local/state/saya/saya.log`, rotating at 10 MiB with up to five backups. Display, queries, and exports reapply redaction for currently configured known credentials. [Logging](observability.md)

### Cancellation and terminal recovery

Ctrl+C requests cancellation of owned runtime or UI work and waits for cleanup. An export that has already been published still reports its actual publication result after cancellation. Normal exits, recoverable errors, panic unwind, and handled SIGINT／SIGTERM／SIGHUP enter terminal restoration for raw mode, cursor visibility, bracketed paste, and the alternate screen. Blocked output or a lost connection permits bounded attempts to restore screen state.

SIGKILL, a process crash, or power loss may leave terminal state and execution outcomes unresolved. Restore the terminal from the shell, then inspect session records and actual files to establish operation outcomes.

```sh
stty sane
printf '\033[?2004l\033[?25h\033[?1049l'
```

### Platforms and candidate acceptance

| Supported baseline | Architectures |
| --- | --- |
| Linux with glibc 2.35+ | x86_64, aarch64 |
| macOS 15+ | Intel x86_64, Apple Silicon aarch64 |

Builds continue to use Rust 1.99.1 and edition 2024. N0 has local acceptance evidence. Final N1 entry-point, unpacked-binary, and four-platform checks continue to be recorded in development progress. Candidate downloads, hashes, signatures, and final publication status remain unconfirmed in this draft.

### Known limitations and subsequent scope

- OS sandboxing, MCP, automatic context compaction, and executable plugins remain subsequent work. Commands continue to run with host permissions, and process groups cannot constrain descendants that detach themselves.
- Bounded TUI windows can omit earlier or longer content, and exports also have explicit limits. Restored sessions and tool snapshots retain unknown outcomes, which require checking actual workspace effects.
- Credential redaction depends on currently resolvable configured values and persisted markers. File operations provide single-file guarantees and may race with external concurrent changes.
- Developer signing, notarization, and actual macOS candidate artifacts remain subject to release records. Persistent jobs across runs, cross-protocol session migration, GUI, Goal, and Browser follow the subsequent development plan. [Post-MVP plan](post-mvp-plan.md)
