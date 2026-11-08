# Saya Harness 0.2.0-rc.1

[简体中文](#简体中文) · [English](#english)

## 简体中文

**0.2.0-rc.1 与后续开发线说明，更新于 2026-11-09。RC1 候选源码为 `a6c172d`，范围为 N0 共享基础与 N1 默认 TUI。下文单独记录 RC1 后的交互增量，审查整改与补充验收见[开发进度](progress.md)和[TUI 验证](tui-validation.md)。正式发布另行安排。**

### 默认 TUI 与入口迁移

- stdin 和 stdout 均连接终端时，直接运行 `saya` 进入全屏 TUI，可继续使用 `--config`、`--cwd`、`--mode` 等全局参数。
- 原有文本交互使用显式 `saya chat`。`saya run`、`saya resume`、`saya session`、`saya logs` 与 `saya config` 继续使用各自的 CLI 入口。
- 无子命令且缺少终端，或显式选择 JSON／JSONL 输出时，返回用法错误和退出码2。脚本可使用 `saya run --stdin` 或其他显式子命令。
- TUI 复用共享应用层的模型、会话、权限和运行控制。PLAN、BUILD、CODE、WORK 四工作模式与 auto、Manual Approval、bypass permissions 三权限模式继续生效。

### RC1 的对话与运行控制

- 流式聊天、多行 UTF-8／grapheme 编辑、粘贴、代码围栏、历史滚动及工作区／模型／模式／权限／用量顶栏。
- Enter 发送，Alt+Enter 或 Ctrl+J 换行，Ctrl+P 打开命令面板，PgUp／PgDown 滚动，End 恢复跟随输出，Ctrl+C 取消，输入为空时 Ctrl+D 请求退出。
- `/mode plan|build|code|work` 请求切换模式，界面同时显示当前与待生效模式。`/permissions auto|manual|bypass` 在运行和 UI 任务均空闲时修改权限。
- 人工审批显示操作、参数、工作区及关联 ID。详情可见且输入为空时，新的普通／Shift `y` 批准，`n` 或 Esc 拒绝。澄清通过输入框回复，`//` 可发送字面量斜杠。
- 100列起对话与详情并列，较窄终端的详情占用主区。草稿64 KiB、单条消息256 KiB、聊天正文合计2 MiB／512条，详情缓存2 MiB／10,000逻辑行，省略和截断保留可见提示。界面布局及预览见[TUI 设计说明](tui-design.md)。

### RC1 之后的 TUI 增量

- 各尺寸采用统一会话布局，达到120列×24行时显示右侧栏，展示会话标题、Context、Tasks、扩展状态及工作目录。四工作模式使用各自主题色，窗口标题使用经过归一化和凭据检查的首条任务概括名。
- Assistant 正文支持有界 Markdown 子集，包括标题、列表、引用、强调、行内代码和代码围栏。URL／HTML 显示为可见文本。当前渲染对单个字符簇设128字节、视觉行设4 KiB上限，超限位置显示省略提示，持久化与导出保留消息数据。
- 空草稿按 Tab 循环 WORK、CODE、PLAN、BUILD。输入 `/` 显示命令建议，Tab／右方向键补全。Ctrl+P 打开居中命令面板，通过键盘或鼠标选择，Enter 执行高亮项，Esc 关闭并保留草稿与真实详情。
- profile 可配置 `thinking_effort`，当前支持 `low`、`medium`、`high`、`xhigh`、`max`，按三协议透传。Context 分项显示 System、Instructions、Skills、Preferences、Memory、Conversation、Files、Tools、Protocol 的估算值，并单独呈现服务端报告的输入用量。
- 模型通过 `todo_write` 维护任务清单，Tasks 展示实际保存状态。PLAN 可更新会话任务元数据，Manual Approval 对该工具逐次确认。Thinking 与较长工具正文可折叠，支持点击标题、Ctrl+O 和区域鼠标滚动。工具标题从结构化结果提取短摘要，展开后可查看正文。

这些增量按各自源码记录本机验证。历史 RC1 的四平台验收归属于 `a6c172d`，当前开发线的跨平台验证随新产物单独执行。

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

Ctrl+C 请求取消所属运行或 UI 任务，并等待收尾。导出已经完成发布时，取消回复仍报告实际发布结果。正常退出、可恢复错误、panic unwind 与受处理的 SIGINT／SIGTERM／SIGHUP 进入终端恢复路径，处理 raw mode、光标、括号粘贴与 alternate screen。当前开发线还恢复鼠标报告及支持 xterm 标题栈的终端标题。终端输出阻塞或连接失效时，屏幕恢复使用有界尝试。

SIGKILL、进程崩溃或断电可能留下终端状态与未知执行结果。可在 shell 中恢复终端，然后检查会话记录和实际文件，确认操作结果。

```sh
stty sane
printf '\033\\\033[?1006l\033[?1000l\033[?2004l\033[?25h\033[?1049l\033[23;2t'
```

### 平台与候选验收

| 支持基线 | 架构 |
| --- | --- |
| Linux，glibc 2.35+ | x86_64、aarch64 |
| macOS 15+ | Intel x86_64、Apple Silicon aarch64 |

构建使用 Rust 1.99.1／edition 2024。候选源码 `a6c172d` 已通过四平台 CI 与原生最低环境解包验收，每个平台执行445项Rust测试及13组TUI场景。四个归档下载后的SHA-256、二进制、源码声明与脚本摘要均已核对，环境和校验和见[TUI验证](tui-validation.md)。候选保存在Actions artifacts，保留14天。正式发布安排在后续代码review与其他测试之后。

### 已知限制与后续范围

- OS Sandbox、MCP、自动上下文压缩和可执行插件仍属后续开发范围。命令继续使用宿主权限，进程组清理无法约束主动脱离的后代进程。
- TUI 的有界显示窗口可能省略较早或较长内容，导出也有明确限额。恢复和工具快照保留未知结果，需要结合工作区核查实际副作用。
- 凭据脱敏依据当前可解析的已知配置值及持久化标记。文件操作保持单文件保证，外部并发改动仍可能竞争。
- macOS开发者身份签名与Apple公证仍属后续发行工作。跨运行持久任务、跨协议会话迁移、GUI、Goal 和 Browser 按后续计划推进。[后续开发计划](post-mvp-plan.md)

### RC1 之后的开发线审查

2026-11-09，开发线完成文本 CLI 终端控制序列、个人文件编码凭据外发、Todo 脱敏后失效仍成功、Responses 推理流冲突、TUI 异常后台清理和异步失败覆盖草稿六项问题的首轮修复及交叉复核。首轮源码为 `727832b`，历史 RC1 的 `a6c172d` 四平台验收保留原有归属。各轮程序的实际验证见[本轮审查](review-2026-11-09.md)。共享凭据检测的失败关闭可能拒绝部分无秘密歧义 JSON。仅流式明文、最终仅 opaque 的推理响应，恢复后无法重建明文。

后续R-02补修覆盖Markdown和普通正文中的Unicode、混合及嵌套JSON转义凭据，通过检测的个人文本保持原文。检测视图受256层与累计64 MiB扫描限制，超限失败关闭。最终程序通过600项Rust测试、20组本机PTY及原六例零模型请求回归，源码和程序关联见审查记录。

## English

**0.2.0-rc.1 and subsequent development-line notes, updated on 2026-11-09. RC1 source `a6c172d` covers N0 shared foundations and N1 default TUI. Post-RC1 interaction features are identified separately below. Review repairs and additional acceptance are tracked in [development progress](progress.md) and [TUI validation](tui-validation.md). Formal publication will be scheduled separately.**

### Default TUI and entry-point migration

- Running `saya` opens the full-screen TUI when both stdin and stdout are terminals. Global options including `--config`, `--cwd`, and `--mode` continue to apply.
- Use explicit `saya chat` for the existing text conversation interface. `saya run`, `saya resume`, `saya session`, `saya logs`, and `saya config` retain their CLI entry points.
- With no subcommand, a missing terminal or explicit JSON／JSONL output returns a usage error with exit code 2. Scripts can use `saya run --stdin` or another explicit subcommand.
- The TUI reuses shared model, session, permission, and runtime controls. PLAN, BUILD, CODE, and WORK modes and auto, Manual Approval, and bypass permissions continue to apply.

### RC1 conversation and runtime controls

- Streaming conversation, multiline UTF-8／grapheme editing, paste, code fences, history scrolling, and a workspace／model／mode／permissions／usage header.
- Enter sends, Alt+Enter or Ctrl+J adds a newline, Ctrl+P opens commands, PgUp／PgDown scroll, End follows new output, Ctrl+C cancels, and Ctrl+D requests exit when the draft is empty.
- `/mode plan|build|code|work` requests a mode change while the header shows current and pending modes. `/permissions auto|manual|bypass` changes permissions while both runtime and UI operations are idle.
- Approval details include the operation, arguments, workspace, and correlation IDs. With approval details visible and the draft empty, a fresh ordinary／Shift `y` approves, and `n` or Esc denies. Questions accept typed replies, and `//` sends a literal leading slash.
- At 100 columns, conversation and details appear side by side. Narrower terminals show details in the main area. Budgets are 64 KiB for a draft, 256 KiB per message, 2 MiB／512 messages for chat bodies, and 2 MiB／10,000 logical lines for cached details, with visible omission and truncation markers. [TUI design and previews](tui-design.md)

### Post-RC1 TUI additions

- All sizes use a unified session layout. At 120 columns × 24 rows, a sidebar shows the session title, Context, Tasks, extension status, and workspace. Each work mode has its own theme color. The window title uses a normalized, credential-checked summary of the first task.
- Assistant messages support a bounded Markdown subset with headings, lists, quotes, emphasis, inline code, and fenced code. URLs and HTML render as visible text. Current rendering limits a grapheme to 128 bytes and a visual row to 4 KiB, with explicit omission markers. Persisted history and exports retain message data.
- With an empty draft, Tab cycles WORK, CODE, PLAN, and BUILD. Typing `/` opens command suggestions, and Tab or Right completes the selection. Ctrl+P opens a centered command modal with keyboard and mouse selection. Enter invokes the highlighted item, and Esc closes it while preserving the draft and actual details.
- Profiles accept `thinking_effort` values `low`, `medium`, `high`, `xhigh`, and `max`, passed through the three protocols. Context displays estimated System, Instructions, Skills, Preferences, Memory, Conversation, Files, Tools, and Protocol components, alongside separately reported provider input usage.
- Models maintain task lists through `todo_write`, and Tasks reflects persisted state. PLAN can update session task metadata, and Manual Approval requires approval for each call. Thinking and longer tool bodies can be folded using header clicks or Ctrl+O, with mouse scrolling scoped to the active region. Tool headers derive short summaries from structured results, and expanded cards expose their bodies.

Validation for these additions is recorded against each development source. Historical four-platform RC1 acceptance belongs to `a6c172d`. Platform acceptance for current development artifacts is tracked against each new artifact.

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

Ctrl+C requests cancellation of owned runtime or UI work and waits for cleanup. An export that has already been published still reports its actual publication result after cancellation. Normal exits, recoverable errors, panic unwind, and handled SIGINT／SIGTERM／SIGHUP enter terminal restoration for raw mode, cursor visibility, bracketed paste, and the alternate screen. The current development line also restores mouse reporting and the previous title on terminals supporting the xterm title stack. Blocked output or a lost connection permits bounded attempts to restore screen state.

SIGKILL, a process crash, or power loss may leave terminal state and execution outcomes unresolved. Restore the terminal from the shell, then inspect session records and actual files to establish operation outcomes.

```sh
stty sane
printf '\033\\\033[?1006l\033[?1000l\033[?2004l\033[?25h\033[?1049l\033[23;2t'
```

### Platforms and candidate acceptance

| Supported baseline | Architectures |
| --- | --- |
| Linux with glibc 2.35+ | x86_64, aarch64 |
| macOS 15+ | Intel x86_64, Apple Silicon aarch64 |

Builds use Rust 1.99.1 and edition 2024. Candidate source `a6c172d` passed four-platform CI and native minimum-environment validation of the unpacked programs, with 445 Rust tests and 13 TUI scenarios on each platform. Downloaded archives were checked against their SHA-256 values, binaries, source declarations, and script hashes. Environments and checksums are recorded in [TUI validation](tui-validation.md). Candidates are retained as Actions artifacts for 14 days. Formal publication follows subsequent code review and additional testing.

### Known limitations and subsequent scope

- OS sandboxing, MCP, automatic context compaction, and executable plugins remain subsequent work. Commands continue to run with host permissions, and process groups cannot constrain descendants that detach themselves.
- Bounded TUI windows can omit earlier or longer content, and exports also have explicit limits. Restored sessions and tool snapshots retain unknown outcomes, which require checking actual workspace effects.
- Credential redaction depends on currently resolvable configured values and persisted markers. File operations provide single-file guarantees and may race with external concurrent changes.
- macOS developer identity signing and Apple notarization remain release-preparation work. Persistent jobs across runs, cross-protocol session migration, GUI, Goal, and Browser follow the subsequent development plan. [Post-MVP plan](post-mvp-plan.md)

### Development-line review after RC1

On 2026-11-09, the development line completed initial repairs for six confirmed issues involving CLI terminal controls, encoded credentials in personal files, invalid Todo snapshots after redaction, conflicting Responses reasoning streams, background cleanup during TUI errors, and newer drafts overwritten after asynchronous failures. Cross-review by agents who did not implement the respective fixes passed. The initial repairs are recorded at `727832b`. The historical four-platform RC1 evidence remains associated with `a6c172d`. Validation for each program is recorded in the [review report](review-2026-11-09.md). Shared credential detection fails closed and may reject some non-secret ambiguous JSON. Reasoning returned as stream-only plaintext with an opaque final item cannot be reconstructed as plaintext after restore.

The subsequent R-02 repair covers Unicode, mixed, and nested JSON escapes inside Markdown and prose. Personal text that passes inspection is sent verbatim. Detection views are bounded to 256 decoding layers and 64 MiB of cumulative scanning, with exhausted budgets failing closed. The final program passed 600 Rust tests, 20 local PTY groups, and all six original credential probes with zero model requests. Source and binary provenance is recorded in the review report.
