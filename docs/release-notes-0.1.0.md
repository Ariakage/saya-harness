# Saya Harness 0.1.0

[English](#english) · [简体中文](#简体中文)

## English

Saya Harness is a Rust agent runtime that drives model requests, local tool execution, and subsequent turns. Version 0.1.0 delivers the CLI for programming and local general-purpose tasks. [User guide](usage.md)

### Features

- Four work modes, PLAN, BUILD, CODE, and WORK, with WORK as the default. Users can switch modes within a session, and PLAN enforces read-only tool dispatch.
- OpenAI Chat Completions, OpenAI Responses API, and Anthropic Messages, including streaming text and tool-result round trips. Users configure the model and API endpoint.
- File reading, directory and content search, single-file writes and patches, direct argv command execution, and background jobs scoped to the current run.
- auto, Manual Approval, and bypass permission modes. auto combines rules, isolated model review, and human confirmation. Manual Approval covers every tool call. PLAN and explicit deny rules continue to apply in bypass mode.
- Interactive sessions and one-shot tasks, with text output, final JSON results, and JSONL events.
- Local SQLite sessions, listing and resume, cross-process write locks, execution intent and result persistence, and interrupted-operation records.
- Workspace and user-level project instructions, Skill metadata discovery, explicit preferences and memory input, dynamic host context, and protocol-specific prompt-cache options.
- Known-credential redaction, rotating structured logs, execution and review usage records, and estimated costs based on explicit user-provided rates.

### Platforms and downloads

| Archive | Supported environment |
| --- | --- |
| `saya-0.1.0-linux-x86_64.tar.gz` | Linux x86_64 with glibc 2.35+ |
| `saya-0.1.0-linux-aarch64.tar.gz` | Linux aarch64 with glibc 2.35+ |
| `saya-0.1.0-macos-x86_64.tar.gz` | macOS 15+ on Intel |
| `saya-0.1.0-macos-aarch64.tar.gz` | macOS 15+ on Apple Silicon |

Each archive contains `saya`, the MIT license, README, and `BUILD-INFO.json` with its source commit and binary hash. The release includes `SHA256SUMS` and per-target environment and verification evidence. Builds use pinned Rust 1.99.1 and edition 2024. Minimum-environment checks cover both architectures on Ubuntu 22.04 and macOS 15.

The following example selects Linux x86_64. Select the archive for your platform from the table above. Run in an empty directory with GitHub CLI and `shasum` available.

```sh
archive=saya-0.1.0-linux-x86_64.tar.gz
gh release download v0.1.0 --repo Ariakage/saya-harness --pattern "$archive" --pattern SHA256SUMS
grep -F "  $archive" SHA256SUMS | shasum -a 256 -c -
mkdir saya-0.1.0
tar -xzf "$archive" -C saya-0.1.0
./saya-0.1.0/saya --version
mkdir -p "$HOME/.local/bin"
install -m 755 saya-0.1.0/saya "$HOME/.local/bin/saya"
```

Add `$HOME/.local/bin` to PATH. Configure a model profile and provide its API key through the environment variable referenced in the config, then run `saya config check` before starting a task. Source builds and protocol-specific settings are documented in the user guide and configuration reference. [User guide](usage.md), [Configuration](configuration.md)

### Review and compatibility

Pre-release review fixed explicit PLAN overrides during session resume, encoded credentials reaching review or approval data, configured write limits, Responses mixed-output history, and invalid foreground/background tool-path error handling. Regression checks exercise the real CLI. The review record includes reproductions and independent verification. [Review record](review-2026-11-07.md)

Configuration and result/event formats use schema v1. The internal session database uses schema 2, with a backup followed by a transactional migration from schema 1. Resume applies the current invocation's permissions, waits for new input, and preserves recorded outcomes and interruption states. [Session storage](storage.md)

### Known limitations

- Commands execute with host permissions. OS sandboxing is planned for a later version. Process groups cannot constrain descendants that detach themselves, and forcibly terminated runs may leave processes with unknown outcomes.
- External concurrent file changes can race with version checking and replacement. File operations provide single-file guarantees.
- Credential protection recognizes configured known values. Duplicate JSON keys, excessive nesting, and numbers that cannot be preserved safely use fail-closed handling and may cause some inputs to be rejected.
- Costs are estimates based on provider token usage and explicit rates. Live model tests cover a limited sample of one configured service and model.
- macOS archives have no developer-identity signature or Apple notarization. Fully reproducible builds, automatic updates, persistent jobs across runs, and cross-protocol session migration remain future work.

Sandbox, MCP, executable plugins, automatic context compaction, TUI, and GUI follow the capability roadmap. [Capability roadmap](capability-roadmap.md)

## 简体中文

Saya Harness 的首个 CLI 版本，自行驱动模型调用、工具执行和后续轮次，面向编程与本地通用任务。详细用法见[使用指南](usage.md)。

### 主要能力

- PLAN、BUILD、CODE、WORK 四种工作模式，默认 WORK。用户可在同一会话切换模式，PLAN 在工具派发处保持只读。
- OpenAI Chat Completions、OpenAI Responses API、Anthropic Messages 三种模型协议，支持流式文本和工具结果回传。模型与服务地址由使用者配置。
- 文件读取、目录与内容搜索、单文件写入及补丁、直接 argv 命令执行，以及当前运行内的后台启动、查询与取消。
- auto、Manual Approval 与 bypass 三种权限模式。auto 结合规则、独立模型审核和人工确认，Manual Approval 对每次工具调用确认，bypass 保留 PLAN 与显式 deny。
- 交互会话和脚本用单次任务，提供文本、JSON 最终结果及 JSONL 事件。
- SQLite 本地会话、列出与恢复、跨进程写锁、执行前后保存及中断结果记录。
- 工作区与用户级项目指令、Skill 元数据发现、显式偏好／记忆输入、动态宿主上下文和按协议配置的提示词缓存优化。
- 已知配置凭据遮蔽、结构化轮转日志、执行与审核用量记录，以及显式费率下的成本估算。

### 平台与产物

| 文件 | 支持环境 |
| --- | --- |
| `saya-0.1.0-linux-x86_64.tar.gz` | Linux x86_64，glibc 2.35+ |
| `saya-0.1.0-linux-aarch64.tar.gz` | Linux aarch64，glibc 2.35+ |
| `saya-0.1.0-macos-x86_64.tar.gz` | macOS 15+，Intel |
| `saya-0.1.0-macos-aarch64.tar.gz` | macOS 15+，Apple Silicon |

每个包包含 `saya`、MIT 许可证、README 和记录源码提交及二进制哈希的 `BUILD-INFO.json`。发行同时提供 `SHA256SUMS` 和按目标记录的实际环境与解包验证证据。构建使用锁定的 Rust 1.99.1／edition 2024，最低环境验证覆盖 Ubuntu 22.04 和 macOS 15 的两个架构。

以下示例使用 Linux x86_64 文件名，其他目标可按上表替换。需要 GitHub CLI 和 `shasum`，应在空目录执行。

```sh
archive=saya-0.1.0-linux-x86_64.tar.gz
gh release download v0.1.0 --repo Ariakage/saya-harness --pattern "$archive" --pattern SHA256SUMS
grep -F "  $archive" SHA256SUMS | shasum -a 256 -c -
mkdir saya-0.1.0
tar -xzf "$archive" -C saya-0.1.0
./saya-0.1.0/saya --version
mkdir -p "$HOME/.local/bin"
install -m 755 saya-0.1.0/saya "$HOME/.local/bin/saya"
```

将 `$HOME/.local/bin` 加入 PATH 后可使用 `saya`。从源码构建、配置模板和三协议参数见[使用指南](usage.md)与[配置说明](configuration.md)。API 密钥通过配置引用的环境变量提供。完成 profile 配置后执行 `saya config check`，再运行任务。

### 审查与兼容性

发布前审查修复了恢复会话时的显式 PLAN、嵌套凭据进入核查／确认、写入上限传递、Responses 混合输出历史，以及无效前后台工具路径的错误分类。回归通过实际 CLI 检查，审查记录包含问题复现和独立复核结果。[风险优先审查](review-2026-11-07.md)

配置与结果／事件采用 schema v1。会话内部数据库 schema 为 2，schema 1 迁移先生成备份，再事务更新。恢复使用本次启动权限，等待新输入，保持旧工具调用的执行结果和中断状态。[会话存储](storage.md)

### 已知限制

- 命令使用宿主权限执行，当前没有 OS 沙箱。进程组清理无法约束主动脱离进程组的后代，强制终止后未知任务可能仍在运行。
- 文件版本检查与替换之间仍可能存在外部并发修改，单文件操作缺少跨文件事务。
- 凭据保护识别已配置的已知值。重复 JSON 键、过深嵌套和无法保真处理的数值采用失败关闭，部分输入可能因此被拒绝。
- 成本依赖服务返回的 token 用量和使用者显式费率，记录为估算。真实模型联调覆盖指定服务和模型的有限样本。
- macOS 包未作开发者身份签名或 Apple 公证。完整可复现构建、自动更新、跨运行持久任务和跨协议会话迁移仍属后续工作。

Sandbox、MCP、插件执行、自动上下文压缩、TUI 和 GUI 按后续版本规划推进。[能力路线图](capability-roadmap.md)
