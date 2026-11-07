# Saya Harness

使用 Rust 构建的 Agent Runtime Harness，同时支持编程任务与通用任务。Saya 自己驱动模型与工具之间的执行循环，首阶段提供 CLI，后续扩展 TUI 和 GUI。

**0.1.0 已发布。** 提供 macOS 15+、Linux glibc 2.35+ 的 x86_64 与 aarch64 CLI 安装包。中英双语说明、校验和及四平台最低环境验收证据见 [GitHub Release](https://github.com/Ariakage/saya-harness/releases/tag/v0.1.0)。

当前开发线为 **0.2.0-rc.1 候选**，已实现 N0 共享基础及 N1 默认 TUI，包括 `saya-app`、诊断与追踪、日志查询、JSON／Markdown 会话导出及终端对话界面。直接运行 `saya` 进入 TUI，文本交互使用 `saya chat`。本轮用于验收和试用，正式发布安排在后续代码 review 与其他测试之后。候选范围见[双语说明](docs/release-notes-0.2.0-rc.1.md)，实际证据见[开发进度](docs/progress.md)。

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
| [CLI 开发预览](docs/cli-preview.md) | 单次运行、交互、会话和当前限制 |
| [MVP 验收证据](docs/acceptance.md) | AC-01 至 AC-24 的测试与发行验收映射 |
| [0.1.0 双语版本说明](docs/release-notes-0.1.0.md) | 功能、安装、兼容性与已知限制 |
| [项目指令与 Skill 发现](docs/project-context.md) | 用户级／工作区上下文、优先级和安全边界 |
| [SQLite 存储组件](docs/storage.md) | 事务、数据路径、持久化与当前边界 |
| [saya-plugin 规范](docs/saya-plugin.md) | manifest v1 草案、结构 schema、权限与生命周期 |
| [后续能力路线图](docs/capability-roadmap.md) | Sandbox、MCP、Skills／插件、Hooks、记忆、多 Agent 等功能规划 |
| [后续开发计划](docs/post-mvp-plan.md) | 0.2.0 候选范围、六个阶段与 DEV-038 至 DEV-083 |
| [原生 Sandbox](docs/sandbox.md) | Seatbelt 独立后端、Bubblewrap／Seccomp／socat 和 AST |
| [上下文与长输出](docs/context-management.md) | OutputStore、检索、Compact、协议及缓存 |
| [Browser 设计](docs/browser.md) | 托管浏览器、页面操作、来源、权限及产物 |
| [TUI 设计](docs/tui.md) | 默认 saya 入口、共享应用层、终端交互与兼容 |
| [日志与追踪](docs/observability.md) | 五级诊断、关联 span、日志查询与安全边界 |
| [Session 导出](docs/session-export.md) | 一致性快照、Markdown／JSON 与文件保存 |
| [Goal 设计](docs/goal.md) | 目标、步骤、证据、手动与有限自动推进 |
| [MVP 实施路线图](docs/roadmap.md) | 8 个 MVP 里程碑、工作量估算与交付条件 |
| [实施任务](docs/implementation-plan.md) | 37 个任务、依赖、交付与基线／验收覆盖 |
| [开发进度](docs/progress.md) | 当前状态、下一任务与实际验证证据 |
| [提示词定制](docs/prompts.md) | 英文提示词、模式与审核装配、来源取舍对照和离线预览 |
| [决策记录](docs/decisions.md) | 已确认方向、基线和历史变更 |

当前实现和验证证据见[开发进度](docs/progress.md)，[使用指南](docs/usage.md)和[无凭据配置示例](examples/config.example.toml)提供 CLI 入门。

后续交付按[开发计划](docs/post-mvp-plan.md)推进。0.2.0 候选范围为共享应用服务、增强日志、Session Export 和默认 TUI，原生 Sandbox、MCP、Compact、插件、Browser 及 Goal 按依赖交付。具体版本根据验收收敛，GUI 位于 TUI 之后。

## 许可证

项目采用 [MIT 许可证](LICENSE)。
