# Saya Harness 0.2.0-rc.2

[简体中文](#简体中文) · [English](#english)

## 简体中文

0.2.0-rc.2 汇集 N0 共享应用基础、N1 默认 TUI、RC1 之后的终端交互完善和两轮审查整改。Saya 继续自行驱动模型调用、本地工具执行和后续轮次，支持编程与通用任务。版本对应的源码、产物与平台证据见下方验收记录。[使用指南](usage.md)

### 默认 TUI 与入口迁移

- stdin 和 stdout 均连接终端时，直接运行 `saya` 进入全屏 TUI。全局 `--config`、`--cwd`、`--profile`、`--mode` 和 `--permissions` 继续适用。
- 原有文本交互使用 `saya chat`。单次任务、文本恢复、会话查询、日志和配置继续使用 `saya run`、`saya resume`、`saya session`、`saya logs` 和 `saya config`。
- 无子命令的非终端调用，或无子命令且选择 JSON／JSONL 时，返回用法错误2。脚本使用 `saya run --stdin` 等显式入口。文本、JSON 最终结果和 JSONL 事件继续保留各自的输出契约。
- CLI 与 TUI 复用 `saya-app` 和 `saya-core` 的模型、会话、权限及运行控制。[入口与运行说明](tui.md)

### 对话、补全与运行控制

- 统一深色会话布局支持多行 Unicode／grapheme 编辑、粘贴和滚动，最低窗口为20列×6行。达到120列×24行显示会话标题、Context、Tasks、扩展状态及工作目录侧栏。窗口标题采用经归一化和已知凭据检查的首条任务概括名。
- Assistant 正文显示有界 Markdown 子集，支持标题、列表、引用、强调、行内代码和代码围栏。URL／HTML 作为可见文本呈现。Thinking 和结构化工具结果使用可折叠卡片，工具摘要来自实际结果字段，展开可查看保留的正文。
- Enter 发送，Alt+Enter 或 Ctrl+J 换行。PgUp／PgDn 滚动，End 跟随最新输出，Ctrl+O 切换最近保留的可折叠卡片，点击标题可单独展开。鼠标滚动按对话和详情区域处理。
- 空草稿按 Tab 循环 WORK → CODE → PLAN → BUILD。输入 `/` 显示命令建议，↑／↓选择，Tab 或末尾→补全，Enter执行草稿，Esc隐藏建议。带参数命令保留前缀，供用户填写 profile、ID 或路径。
- Ctrl+P 打开居中命令面板，方向键、鼠标滚轮或点击选择，Enter执行高亮项。Esc或再次Ctrl+P关闭后保留草稿和真实详情。面板接管背景编辑，待审批／澄清的回复需要对应请求详情可见。
- Ctrl+C 取消所属运行或界面任务，空闲时清空草稿。空草稿 Ctrl+D 请求退出，活跃任务先取消并等待收尾。运行前的可恢复校验错误保留草稿，异步准备失败时恢复未发送任务，已有新草稿时单独显示旧的 Unsent draft。[界面与显示限额](tui-design.md)

### 工作模式、权限与模型

- PLAN 分析与规划，BUILD 端到端实现和交付，CODE 编写、修改与调试代码，WORK 处理通用任务。新会话默认 WORK，切换由用户发起，在调用边界生效，界面分别显示实际与待生效模式。进入 PLAN 前清理后台写入和命令。
- 权限默认 `auto`，依次使用规则、必要的独立模型核查和人工确认。`Manual Approval` 对每次 Agent 工具调用要求批准，包括读取、搜索和后台查询。`bypass permissions` 跳过人工确认与路径范围限制，PLAN 和显式 deny 继续生效。工作模式与权限分别控制。[执行契约](execution.md)
- 继续支持 OpenAI Chat Completions、OpenAI Responses 和 Anthropic Messages。profile 可配置 `thinking_effort` 为 `low`、`medium`、`high`、`xhigh` 或 `max`，分别透传至 `reasoning_effort`、`reasoning.effort` 和 `output_config.effort`。级别支持范围由模型服务决定，独立核查使用自己的 profile 设置。[配置说明](configuration.md)
- Thinking 展示服务实际返回的可读推理或摘要，审核推理与不透明签名／加密字段分别处理。Context 分项标为 Estimated，依据归一化模型请求估算，服务端报告的输入用量单独显示。[Thinking 与上下文](tui.md#thinking任务清单与折叠)
- 模型可通过 `todo_write` 更新会话任务清单，Tasks 展示最近成功保存的列表。PLAN 允许更新这些会话元数据，工作区保持只读。Manual Approval 对该工具逐次批准，完成标记仍需对应执行证据。[任务清单](runtime.md#会话任务清单)

### 会话、日志与导出

- `/new`、`/sessions [PAGE]`、`/resume ID` 提供新建、浏览和恢复。`/profiles`、`/profile NAME` 选择已配置模型，兼容配置变化通过 `/resume-accept ID` 显式接受。恢复后等待新输入，使用本次权限设置，工具历史保留原执行结果和 unknown 状态。
- `/tools` 和 `/jobs` 显示带持久水位的快照，`/cancel-job ID` 取消当前运行拥有的后台任务。正常结束、取消和错误均等待所属任务清理，再释放会话锁。
- `trace`、`debug`、`info`、`warn`、`error` 五级本地诊断默认 `warn`，通过 `--log-level` 或 `SAYA_LOG_LEVEL` 设置。`saya logs show` 查询，`saya logs tail` 跟随，支持会话／运行关联及文本、JSON／JSONL 的适用输出。日志10 MiB轮转、保留五份备份。[日志说明](observability.md)
- TUI 使用 `/export json|markdown PATH` 或 `/export-full json|markdown PATH`。离线 CLI 使用 `saya session export SESSION_ID --file ./session.md`，完整 JSON 使用 `--format json --scope full --include-tools`。导出复用脱敏投影、独占文件发布和带 SHA-256／水位的回执，每字段64 KiB、总计32 MiB。目标父目录须存在，已有文件受保护，PLAN 同时保护本次与保存会话的工作区。[会话导出](session-export.md)
- 配置、结果与事件继续采用 schema v1。既有会话沿持久记录恢复，配置指纹变化需要显式接受，模型／协议变更需要新会话。[会话存储](storage.md)

### 审查修复

- 统一已知凭据的检测、净化与保存边界，覆盖普通正文、Markdown 和混合／嵌套 JSON 转义。无法安全清除的正文扣留，无凭据的歧义 JSON 保持原文。标题在空白与控制字符归一化后再次检查，文本终端输出统一处理控制和双向字符。
- 修复命令环境覆盖沿用旧 argv／cwd 授权、符号链接路径规则失配、后台项目上下文误杀整轮运行、模式切换与后台接纳竞态，以及非命令后台任务超时。许可在派发前重查，模式请求和生效事件随会话保存。
- 修复文本与 TUI 异常退出的后台清理和会话锁次序、HUP退出码、异步草稿丢失、恢复覆盖项粘滞及超大组合字符显示。INT／TERM／HUP 触发取消与收尾，绘制失败仍等待所属任务结束。
- 改善 Responses 增量／完成一致性与 completed-only 输出、Anthropic 空文本回放和缓存用量、WAL有效提交前缀、导出身份关联、日志退出排空及目录权限。文本 CLI 按请求补齐最终正文，合法交错顺序不一致时可换行重显完整回答。

逐项处理、保留契约和此前源码的验证记录见[首轮审查](review-2026-11-09.md)与[第二轮整改](review-2026-11-09_02.md)。RC2 的产物验收在下方独立记录。

### 平台与安装

| 文件 | 支持环境 |
| --- | --- |
| `saya-0.2.0-rc.2-linux-x86_64.tar.gz` | Linux x86_64，glibc 2.35+ |
| `saya-0.2.0-rc.2-linux-aarch64.tar.gz` | Linux aarch64，glibc 2.35+ |
| `saya-0.2.0-rc.2-macos-x86_64.tar.gz` | macOS 15+，Intel |
| `saya-0.2.0-rc.2-macos-aarch64.tar.gz` | macOS 15+，Apple Silicon |

归档包含 `saya`、MIT 许可证、README 和记录源码提交及二进制哈希的 `BUILD-INFO.json`。从源码构建使用 Rust 1.99.1／edition 2024，执行 `cargo build --workspace --release --locked`。

以下安装示例选择 Linux x86_64，需要 GitHub CLI 和 `shasum`，在空目录执行，其他平台按上表替换文件名。

```sh
archive=saya-0.2.0-rc.2-linux-x86_64.tar.gz
gh release download v0.2.0-rc.2 --repo Ariakage/saya-harness --pattern "$archive" --pattern SHA256SUMS
grep -F "  $archive" SHA256SUMS | shasum -a 256 -c -
mkdir saya-0.2.0-rc.2
tar -xzf "$archive" -C saya-0.2.0-rc.2
./saya-0.2.0-rc.2/saya --version
mkdir -p "$HOME/.local/bin"
install -m 755 saya-0.2.0-rc.2/saya "$HOME/.local/bin/saya"
```

将 `$HOME/.local/bin` 加入 PATH。模型 profile、API 根地址和实际上下文窗口由用户配置，API 密钥通过 `api_key_env` 引用的环境变量提供。先执行 `saya config check`，再启动 TUI 或任务。配置检查验证本地配置及环境引用，实际服务连接由运行验证。[使用与配置](usage.md)

### 已知限制与终端恢复

- 命令以宿主权限执行，OS Sandbox 仍属后续能力。进程组清理无法约束主动脱离的后代，强制终止后可能保留未知任务与副作用。文件修改提供单文件保证，外部并发修改仍可能竞争。
- 凭据保护依据当前配置可解析的已知值及保存标记，未知秘密需由用户管理。个人偏好／记忆文件会发送给模型，项目指令和 Skill 元数据也来自本地来源。应核查内容与来源信任，权限和模式由运行时独立执行。发现 Skill 元数据后，正文读取仍受工具权限控制。[项目上下文](project-context.md)
- TUI 显示缓存有界，超长内容使用截断或省略提示。单个显示字素簇最多128字节，视觉行最多4 KiB。协议仅保存不透明推理时，恢复后明文可能不可用。Context 和显式费率成本均为估算。
- 模型兼容性取决于服务实际支持的协议事件和参数。后台任务限当前运行，自动上下文压缩、MCP、插件执行、Browser、Goal 和 GUI 按后续计划推进。[后续开发计划](post-mvp-plan.md)
- 本候选没有 Developer ID 签名或 Apple 公证。ad-hoc等具体签名状态以各macOS产物的发行证据为准。
- 终端标题恢复依赖 xterm 标题栈支持。正常退出、panic unwind 与受处理信号走统一恢复路径，断连／慢输出使用有界尝试。SIGKILL、进程崩溃或断电可能留下终端状态。可在 shell 中执行下列命令，再核查会话及实际操作结果。

```sh
stty sane
printf '\033\\\033[?1006l\033[?1000l\033[?2004l\033[?25h\033[?1049l\033[23;2t'
```

### RC2 验收记录

- 候选源码由 `v0.2.0-rc.2` 标签固定，构建和解包验收使用发行工作流。[标签源码](https://github.com/Ariakage/saya-harness/tree/v0.2.0-rc.2)、[发行工作流](https://github.com/Ariakage/saya-harness/actions/workflows/release-candidates.yml)
- 各平台实际通过范围、最低运行环境、源码／脚本／二进制摘要及下载复核以随本版本发布的结构化证据为准。历史 P7／RC1 记录继续归属原产物。[RC2 发行证据](https://github.com/Ariakage/saya-harness/releases/download/v0.2.0-rc.2/release-evidence.json)
- 下载后核对对应归档的 SHA-256，并核对包内 `BUILD-INFO.json` 的源码和二进制记录。[RC2 校验和](https://github.com/Ariakage/saya-harness/releases/download/v0.2.0-rc.2/SHA256SUMS)

## English

0.2.0-rc.2 brings together N0 shared application foundations, the N1 default TUI, post-RC1 terminal interactions, and two rounds of review repairs. Saya drives model requests, local tool execution, and subsequent turns for programming and general-purpose tasks. Source, artifact, and platform evidence for this version is linked in the acceptance record below. [User guide](usage.md)

### Default TUI and entry-point migration

- Running `saya` opens the full-screen TUI when stdin and stdout are terminals. Global `--config`, `--cwd`, `--profile`, `--mode`, and `--permissions` options continue to apply.
- Use `saya chat` for the existing text conversation interface. One-shot tasks, text resume, sessions, logs, and configuration retain the explicit `saya run`, `saya resume`, `saya session`, `saya logs`, and `saya config` entry points.
- With no subcommand, non-terminal invocation or JSON／JSONL selection returns usage error 2. Scripts use an explicit entry point such as `saya run --stdin`. Text, final JSON results, and JSONL events retain their output contracts.
- CLI and TUI share model, session, permission, and run controls through `saya-app` and `saya-core`. [Entry points and runtime behavior](tui.md)

### Conversation, completion, and run controls

- A unified dark layout supports multiline Unicode／grapheme editing, paste, and scrolling, with a minimum window of 20 columns × 6 rows. At 120 columns × 24 rows, a sidebar shows the session title, Context, Tasks, extension status, and workspace. The window title uses a normalized, known-credential-checked summary of the first task.
- Assistant messages render a bounded Markdown subset with headings, lists, quotes, emphasis, inline code, and fenced code. URLs and HTML remain visible text. Thinking and structured tool results have foldable cards. Tool summaries come from actual result fields, with retained bodies available when expanded.
- Enter sends, and Alt+Enter or Ctrl+J adds a newline. PgUp／PgDn scroll, End follows output, Ctrl+O toggles the latest retained foldable card, and header clicks toggle individual cards. Mouse scrolling follows the conversation or details region.
- With an empty draft, Tab cycles WORK → CODE → PLAN → BUILD. Typing `/` opens suggestions, ↑／↓ selects, Tab or Right at the end completes, Enter submits the draft, and Esc hides suggestions. Parameterized commands retain a prefix for the user to supply a profile, ID, or path.
- Ctrl+P opens a centered command modal. Arrow keys, mouse wheel, or clicks select, and Enter invokes the highlighted item. Esc or Ctrl+P closes it while preserving the draft and actual details. The modal captures background editing, and approval／question replies require the corresponding request details to be visible.
- Ctrl+C cancels owned runtime or UI work and clears the draft while idle. Ctrl+D with an empty draft requests exit, cancelling and awaiting active work first. Recoverable pre-run validation errors preserve the draft. Failed asynchronous preparation restores unsent input, retaining a newer draft and displaying the older Unsent draft separately. [Layout and display budgets](tui-design.md)

### Work modes, permissions, and models

- PLAN analyzes and plans, BUILD implements and delivers end to end, CODE writes, changes, and debugs code, and WORK handles general tasks. New sessions default to WORK. Users request changes at call boundaries, with actual and pending modes shown separately. Background writes and commands are cleaned up before entering PLAN.
- Permissions default to `auto`, using rules, separate model review when needed, and human confirmation. `Manual Approval` requires approval for every Agent tool call, including reads, searches, and background queries. `bypass permissions` skips human confirmation and path-scope restrictions while retaining PLAN and explicit deny rules. Work modes and permission modes remain separate controls. [Execution contract](execution.md)
- OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages remain supported. Profiles accept `thinking_effort` values `low`, `medium`, `high`, `xhigh`, and `max`, passed through as `reasoning_effort`, `reasoning.effort`, and `output_config.effort`, respectively. Model services determine accepted levels, and independent review uses its own profile settings. [Configuration](configuration.md)
- Thinking displays readable reasoning or summaries actually returned by the service. Review reasoning and opaque signatures／encrypted fields are handled separately. Context components are labeled Estimated and derived from normalized model requests, alongside separately reported provider input usage. [Thinking and context](tui.md#thinking任务清单与折叠)
- Models can update session task lists through `todo_write`, and Tasks reflects the latest successfully persisted list. PLAN permits this session metadata while keeping the workspace read-only. Manual Approval applies to every call, and completion claims still require execution evidence. [Task lists](runtime.md#会话任务清单)

### Sessions, logs, and export

- `/new`, `/sessions [PAGE]`, and `/resume ID` create, browse, and restore sessions. `/profiles` and `/profile NAME` select configured models. `/resume-accept ID` explicitly accepts compatible configuration changes. Resume waits for new input and uses the current invocation's permissions, retaining recorded outcomes and unknown states.
- `/tools` and `/jobs` show snapshots with committed watermarks. `/cancel-job ID` cancels a background job owned by the active run. Completion, cancellation, and errors await owned cleanup before releasing the session lock.
- Local diagnostics provide `trace`, `debug`, `info`, `warn`, and `error`, defaulting to `warn`, selected with `--log-level` or `SAYA_LOG_LEVEL`. `saya logs show` queries records and `saya logs tail` follows them, with session／run correlation and applicable text, JSON／JSONL formats. Logs rotate at 10 MiB with five backups. [Logging](observability.md)
- TUI export uses `/export json|markdown PATH` or `/export-full json|markdown PATH`. The offline CLI uses `saya session export SESSION_ID --file ./session.md`, with `--format json --scope full --include-tools` for full JSON records. Both share redacted projections, exclusive file publication, and receipts with SHA-256 and watermarks. Limits are 64 KiB per field and 32 MiB in total. The parent directory must exist, existing files are protected, and PLAN protects both current and saved-session workspaces. [Session export](session-export.md)
- Configuration, results, and events retain schema v1. Existing sessions resume from persisted records. Fingerprint changes require explicit acceptance, and model／protocol changes require a new session. [Session storage](storage.md)

### Review repairs

- Unified known-credential detection, sanitization, and persistence across prose, Markdown, and mixed／nested JSON escapes. Unsafe residual bodies are withheld, while non-secret ambiguous JSON remains verbatim. Titles are checked again after whitespace and control normalization, and text terminal output consistently handles control and bidirectional characters.
- Fixed command environment overrides reusing argv／cwd authorization, symlink path-rule mismatches, background project-context changes failing an entire run, mode-switch／background-admission races, and timeouts for non-command background tools. Permits are rechecked before dispatch, and requested／effective mode events persist with the session.
- Fixed background cleanup and session-lock ordering on text／TUI errors, HUP exit status, asynchronous draft loss, stale resume overrides, and oversized combining-character rendering. INT／TERM／HUP request cancellation and cleanup. Drawing failures still await owned work.
- Improved Responses incremental／final consistency and completed-only output, Anthropic empty-text replay and cached usage, valid WAL commit-prefix recovery, export identity correlation, shutdown log draining, and directory permissions. Text CLI fills in missing final text per request. Valid interleaving that differs from final order can print the complete answer again on a new line.

Itemized dispositions, retained contracts, and validation for earlier sources are recorded in the [first review](review-2026-11-09.md) and [second review repairs](review-2026-11-09_02.md). RC2 artifact acceptance is recorded independently below.

### Platforms and installation

| Archive | Supported environment |
| --- | --- |
| `saya-0.2.0-rc.2-linux-x86_64.tar.gz` | Linux x86_64 with glibc 2.35+ |
| `saya-0.2.0-rc.2-linux-aarch64.tar.gz` | Linux aarch64 with glibc 2.35+ |
| `saya-0.2.0-rc.2-macos-x86_64.tar.gz` | macOS 15+ on Intel |
| `saya-0.2.0-rc.2-macos-aarch64.tar.gz` | macOS 15+ on Apple Silicon |

Archives contain `saya`, the MIT license, README, and `BUILD-INFO.json` with the source commit and binary hash. Source builds use Rust 1.99.1／edition 2024 and `cargo build --workspace --release --locked`.

The following example selects Linux x86_64. Use GitHub CLI and `shasum` in an empty directory, replacing the archive name for other platforms.

```sh
archive=saya-0.2.0-rc.2-linux-x86_64.tar.gz
gh release download v0.2.0-rc.2 --repo Ariakage/saya-harness --pattern "$archive" --pattern SHA256SUMS
grep -F "  $archive" SHA256SUMS | shasum -a 256 -c -
mkdir saya-0.2.0-rc.2
tar -xzf "$archive" -C saya-0.2.0-rc.2
./saya-0.2.0-rc.2/saya --version
mkdir -p "$HOME/.local/bin"
install -m 755 saya-0.2.0-rc.2/saya "$HOME/.local/bin/saya"
```

Add `$HOME/.local/bin` to PATH. Configure the model profile, API root, and actual context window, supplying the API key through the environment variable named by `api_key_env`. Run `saya config check` before starting the TUI or a task. Configuration checks validate local settings and environment references, with service connectivity established by actual execution. [Usage and configuration](usage.md)

### Known limitations and terminal recovery

- Commands execute with host permissions, with OS sandboxing planned for subsequent work. Process groups cannot constrain descendants that detach themselves, and forced termination can leave unknown jobs and effects. File changes provide single-file guarantees and can race with external changes.
- Credential protection depends on currently resolvable configured values and persisted markers. Users manage unknown secrets. Personal preference／memory files are sent to the model, and project instructions and Skill metadata also come from local sources. Review their contents and source trust. Runtime permissions and modes are enforced independently, and Skill body reads remain subject to tool permissions. [Project context](project-context.md)
- TUI caches are bounded and show omission／truncation markers. Rendered graphemes are limited to 128 bytes and visual rows to 4 KiB. Plaintext reasoning can be unavailable after resume when the protocol persists only opaque data. Context and costs based on explicit rates are estimates.
- Model compatibility depends on the service's supported protocol events and parameters. Background jobs remain scoped to the current run. Automatic compaction, MCP, executable plugins, Browser, Goal, and GUI follow the development plan. [Post-MVP plan](post-mvp-plan.md)
- This candidate has no Developer ID signature or Apple notarization. Per-artifact release evidence records specific macOS signature details, including ad-hoc status where applicable.
- Title restoration depends on xterm title-stack support. Normal exits, panic unwind, and handled signals share terminal restoration, with bounded attempts for disconnected or slow output. SIGKILL, a process crash, or power loss can leave terminal state behind. Run the following from the shell, then inspect session records and actual effects.

```sh
stty sane
printf '\033\\\033[?1006l\033[?1000l\033[?2004l\033[?25h\033[?1049l\033[23;2t'
```

### RC2 acceptance record

- The `v0.2.0-rc.2` tag fixes the candidate source. The release workflow builds and verifies the unpacked programs. [Tagged source](https://github.com/Ariakage/saya-harness/tree/v0.2.0-rc.2), [Release workflow](https://github.com/Ariakage/saya-harness/actions/workflows/release-candidates.yml)
- Per-platform checks actually passed, minimum runtime environments, source／script／binary hashes, and download verification are defined by the structured evidence shipped with this version. Historical P7／RC1 records retain their original artifact scope. [RC2 release evidence](https://github.com/Ariakage/saya-harness/releases/download/v0.2.0-rc.2/release-evidence.json)
- After downloading, verify the archive's SHA-256 and the source and binary records in `BUILD-INFO.json`. [RC2 checksums](https://github.com/Ariakage/saya-harness/releases/download/v0.2.0-rc.2/SHA256SUMS)
