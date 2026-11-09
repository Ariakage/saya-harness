# Saya Harness

使用 Rust 构建的 Agent Runtime Harness，同时支持编程任务与通用任务。Saya 自己驱动模型与工具之间的执行循环，CLI 与默认 TUI 复用运行时，GUI 按后续计划推进。

**Saya Harness 0.2.0** 提供 CLI 和默认 TUI，支持 macOS 15+、Linux glibc 2.35+ 的 x86_64 与 aarch64。包含 N0 共享基础、N1 终端界面、交互完善及两轮审查整改。直接运行 `saya` 进入 TUI，文本交互使用 `saya chat`。[GitHub Release](https://github.com/Ariakage/saya-harness/releases/tag/v0.2.0)、[中英双语说明](docs/release-notes-0.2.0.md)、[开发进度](docs/progress.md)

0.1.0 与 0.2.0-rc.2 的历史发行及验收保留各自源码和产物归属。

当前开发线已完成 N2 原生隔离、命令 CST 风险事实、stdio／Streamable HTTP MCP 和凭据管理，并接入 CLI 与 TUI。固定源码 `aaec8c5` 已通过 Ubuntu 22.04 x86_64／aarch64 与 macOS 15 Intel／ARM 的源码 CI、原生／MCP／Markdown／TUI 和完整性能验收。0.2.0 的功能与限制继续归属已发布产物。Linux 原生后端另要求可确认的内核修复身份与能力探测。[隔离配置](docs/configuration.md#n2-原生隔离配置)、[原生隔离](docs/sandbox.md)、[MCP 与凭据](docs/mcp.md)、[N2 最终验收记录](docs/n2-validation.md)

## 已确认方向

- 实现语言：Rust。
- 交付路线：CLI MVP → TUI → GUI。
- 核心职责：接收任务，调用模型，执行模型请求的工具，将工具结果交回模型，继续运行至结束。
- 任务范围：同时支持编程任务与通用任务。
- 工作模式：PLAN、BUILD、CODE、WORK，四者可切换，新会话默认 WORK。
- 模式分工：PLAN 负责需求分析与规划；BUILD 负责端到端实现和交付；CODE 聚焦代码编写、修改与调试；WORK 负责通用任务执行。
- CLI 交互：同时支持连续对话的交互会话和用于脚本调用的单次命令。
- 模式规则：用户手动切换，Agent 可建议；当前调用完成后、下一次调用前生效，同一会话保留上下文。
- 权限模式：`auto`、`bypass permissions`、`Manual Approval`，默认 `auto`。Manual Approval 对每次工具调用要求人工批准；PLAN 在三种权限模式下均只读，不修改工作区文件。
- 模型协议：OpenAI Chat Completions、OpenAI Responses API、Anthropic Messages。
- 首批工具：本地文件读写、目录／内容搜索、命令执行；可显式在当前运行内后台启动、查询与取消。运行结束时清理未完成任务。
- 输出：流式交互、JSON 最终结果与 JSONL 事件输出。
- 单次命令：允许显式预授权命令执行；无法询问且未获授权时，返回需要确认并退出。
- 会话：本地保存、列出和恢复。恢复历史、原工作模式与配置引用后等待新输入，不自动重放工具；权限采用本次启动设置，不恢复历史授权。

## MVP 基线

项目负责人已整套确认[完整基线](docs/mvp-baseline.md) B-01 至 B-24，并在 D-029 补充运行内后台工具调用、D-033 纳入用户级与工作区的指令及 Skill 发现，D-035 将最低支持系统调整为 macOS 15；当前没有尚待选择的 MVP 产品阻塞项。

0.1.0 提供以下执行路径。

1. 用户通过交互会话或单次命令选择模式、输入任务，并提供模型配置。
2. 运行时调用模型，处理最终回答或工具调用。
3. 工具执行结果返回模型，运行时继续下一轮。
4. CLI 展示结果并保存会话，后续可列出或恢复；失败、取消或达到运行限制时明确结束。

已确定命令名 saya、Rust 1.99.1／2024、core＋CLI workspace、TOML 配置、SQLite WAL、schema v1、macOS／Linux 首批平台和 MIT 许可证。核心运行时、CLI、本地会话持久化、并发与中断恢复已通过本机受控验证；四类任务与三协议已通过本机受控端到端验证，P7 验收已完成，正式发行状态见开发进度。完整接口、默认值和验证条件见基线、[需求规格](docs/requirements.md)与[执行契约](docs/execution.md)。

## 文档

| 文档 | 内容 |
| --- | --- |
| [AGENTS.md](AGENTS.md) | 编码 Agent 的工作约定 |
| [文档索引](docs/README.md) | 文档用途与状态说明 |
| [项目范围](docs/vision.md) | 已确定的产品目标、MVP 范围与演进路线 |
| [需求规格](docs/requirements.md) | 已确认需求、任务流程与验收条件 |
| [完整基线](docs/mvp-baseline.md) | 已整套确认的 24 组决定与具体契约 |
| [工作模式](docs/modes.md) | PLAN、BUILD、CODE、WORK 的职责与切换规则 |
| [执行与恢复契约](docs/execution.md) | 三种权限模式、切换边界、预授权、输出与会话恢复 |
| [运行时接口](docs/runtime.md) | 可复用 Agent 循环、调用边界和验证范围 |
| [架构设计](docs/architecture.md) | Agent 循环、模块边界与实现约束 |
| [开发指南](docs/development.md) | 开发流程与验证方式 |
| [CLI 使用指南](docs/usage.md) | 构建、配置、使用示例与故障处理 |
| [配置组件](docs/configuration.md) | 配置来源、模型 profile、凭据引用和兼容选项 |
| [模型协议](docs/model-protocols.md) | 三协议适配、流式边界和验证范围 |
| [CLI 入口与实现记录](docs/cli-preview.md) | 单次运行、文本会话与默认 TUI 入口 |
| [MVP 验收证据](docs/acceptance.md) | AC-01 至 AC-24 的测试与发行验收映射 |
| [0.1.0 双语版本说明](docs/release-notes-0.1.0.md) | 功能、安装、兼容性与已知限制 |
| [0.2.0 双语版本说明](docs/release-notes-0.2.0.md) | 已发布的 N0／N1、交互完善、审查修复与发行限制 |
| [0.2.0-rc.1 与开发线双语说明](docs/release-notes-0.2.0-rc.1.md) | 候选与后续交互增量、迁移和限制 |
| [发行验证](docs/release-validation.md) | 历史产物、源码关联、最低环境与下载复核 |
| [项目指令与 Skill 发现](docs/project-context.md) | 用户级／工作区上下文、优先级和安全边界 |
| [SQLite 存储组件](docs/storage.md) | 事务、数据路径、持久化与当前边界 |
| [saya-plugin 规范](docs/saya-plugin.md) | manifest v1 草案、结构 schema、权限与生命周期 |
| [后续能力路线图](docs/capability-roadmap.md) | 已有扩展基础与 Skills／插件、Hooks、记忆、多 Agent 等后续规划 |
| [后续开发计划](docs/post-mvp-plan.md) | 0.2.0 发行范围、当前 N2 与后续阶段，DEV-038 至 DEV-089 |
| [全局记忆](docs/memory.md) | 可读 MEMORY.md、模型编辑、检索、遗忘与回滚的后续设计 |
| [原生 Sandbox](docs/sandbox.md) | Seatbelt、Bubblewrap／Seccomp／socat、HTTP／CONNECT broker 和 CST 边界 |
| [MCP 与凭据管理](docs/mcp.md) | stdio／HTTP 工具、服务生命周期、私有凭据与 OAuth |
| [N2 验收](docs/n2-validation.md) | 固定源码、四平台原生／MCP／Markdown／TUI 和性能证据 |
| [上下文与长输出](docs/context-management.md) | OutputStore、检索、Compact、协议及缓存 |
| [Browser 设计](docs/browser.md) | 托管浏览器、页面操作、来源、权限及产物 |
| [TUI 设计](docs/tui.md) | 默认 saya 入口、共享应用层、终端交互与兼容 |
| [TUI 布局](docs/tui-design.md) | 参考布局、侧栏、输入区与预览 |
| [TUI 验证](docs/tui-validation.md) | 候选及后续源码的真实终端验收 |
| [日志与追踪](docs/observability.md) | 五级诊断、关联 span、日志查询与安全边界 |
| [Session 导出](docs/session-export.md) | 一致性快照、Markdown／JSON 与文件保存 |
| [Goal 设计](docs/goal.md) | 目标、步骤、证据、手动与有限自动推进 |
| [MVP 实施路线图](docs/roadmap.md) | 8 个 MVP 里程碑、工作量估算与交付条件 |
| [实施任务](docs/implementation-plan.md) | 37 个任务、依赖、交付与基线／验收覆盖 |
| [开发进度](docs/progress.md) | 当前状态、下一任务与实际验证证据 |
| [提示词定制](docs/prompts.md) | 英文提示词、模式与审核装配、来源取舍对照和离线预览 |
| [决策记录](docs/decisions.md) | 已确认方向、基线和历史变更 |

当前实现和验证证据见[开发进度](docs/progress.md)，[使用指南](docs/usage.md)和[无凭据配置示例](examples/config.example.toml)提供 CLI 入门。

后续交付按[开发计划](docs/post-mvp-plan.md)推进。0.2.0 已交付共享应用服务、增强日志、Session Export 和默认 TUI。当前 N2 已完成默认原生隔离、MCP 与对应四平台验收。N3 的上下文、OutputStore、Compact、检查点、插件／Hooks 与全局记忆，以及后续 Goal、Browser 和 GUI 按依赖规划，后续版本号在发行范围收敛时确定。

## 许可证

项目采用 [MIT 许可证](LICENSE)。
