# Saya Harness

使用 Rust 构建的 Agent Runtime Harness，同时支持编程任务与通用任务。Saya 自己驱动模型与工具之间的执行循环，首阶段提供 CLI，后续扩展 TUI 和 GUI。

**当前状态：可复用 core 运行循环、本地工具、权限与后台任务已在模型替身和受控夹具下验证；真实模型协议、持久化与 CLI 功能尚未实现。**

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

项目负责人已整套确认[完整基线](docs/mvp-baseline.md) B-01 至 B-24，并在 D-029 补充运行内后台工具调用，包括 CLI、配置、核查、工具、输出、存储、预算、验收与工程基础；当前没有尚待选择的 MVP 产品阻塞项。

首阶段拟完成一条可验证的执行路径：

1. 用户通过交互会话或单次命令选择模式、输入任务，并提供模型配置。
2. 运行时调用模型，处理最终回答或工具调用。
3. 工具执行结果返回模型，运行时继续下一轮。
4. CLI 展示结果并保存会话，后续可列出或恢复；失败、取消或达到运行限制时明确结束。

已确定命令名 saya、Rust 1.99.1／2024、core＋CLI workspace、TOML 配置、SQLite WAL、schema v1、macOS／Linux 首批平台和 MIT 许可证。核心数据与接口及受控测试循环已实现，真实产品行为尚未实现；完整接口、默认值和验证条件见基线、[需求规格](docs/requirements.md)与[执行契约](docs/execution.md)。

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
| [saya-plugin 规划](docs/saya-plugin.md) | 未来插件系统的边界、实施阶段与验收方向 |
| [后续能力路线图](docs/capability-roadmap.md) | Sandbox、MCP、Skills／插件、Hooks、记忆、多 Agent 等功能规划 |
| [MVP 实施路线图](docs/roadmap.md) | 8 个 MVP 里程碑、工作量估算与交付条件 |
| [实施任务](docs/implementation-plan.md) | 37 个任务、依赖、交付与基线／验收覆盖 |
| [开发进度](docs/progress.md) | 当前状态、下一任务与实际验证证据 |
| [提示词定制](docs/prompts.md) | 英文提示词、模式与审核装配、来源取舍对照和离线预览 |
| [决策记录](docs/decisions.md) | 已确认方向、基线和历史变更 |

需求基线与开发计划已准备。后续按[路线图](docs/roadmap.md)推进，并在[进度记录](docs/progress.md)维护实际状态；DEV-006 的模式状态机已在普通构建提供，切换行为已在测试专用原型中验证；可复用 core 循环已在受控测试中完成 P1；真实协议和 CLI 仍属后续阶段。

CLI 之后的新增功能按[后续能力路线图](docs/capability-roadmap.md)规划，优先 Sandbox、MCP Client、Skills／插件与 Hooks，观测评估同步建设；界面继续按 TUI → GUI 演进。

## 许可证

项目采用 [MIT 许可证](LICENSE)。
