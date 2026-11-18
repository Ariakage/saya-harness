# 项目文档

Soyo Harness 的说明文档默认使用中文，发行说明提供中英双语。CLI MVP 已完成 P0 至 P7 验收并发布 [v0.1.0](https://github.com/Ariakage/soyo-harness/releases/tag/v0.1.0)，**0.2.0** 已发布默认 TUI、增强日志与会话导出。具体行为以[完整基线](mvp-baseline.md) B-01 至 B-24 及后续[决策记录](decisions.md)为准。

本目录按职责分为四类：**核心规范与契约**、**子系统设计与使用文档**、**进度与阶段验收**，以及置于子目录的**历史档案**（[`reviews/`](reviews/review-2026-11-11.md)、[`releases/`](releases/release-notes-0.2.0.md)、[`reports/`](reports/live-model-validation.md)）。

## 核心规范与契约

| 文档 | 用途 | 当前状态 |
| --- | --- | --- |
| [项目范围](vision.md) | 目标、交付阶段与 MVP 验收入口 | 已确认 |
| [需求规格](requirements.md) | 已确认需求、用户流程与验收条件 | 已确认；实现和验证范围见开发进度 |
| [完整基线](mvp-baseline.md) | 全部 24 组决定及具体契约 | 已整套确认（D-027） |
| [工作模式](modes.md) | PLAN、BUILD、CODE、WORK 职责与切换规则 | 已实现；边界按完整基线验证 |
| [执行与恢复契约](execution.md) | 权限模式、切换边界、输出与崩溃恢复契约 | 已实现；验证范围见开发进度 |
| [架构设计](architecture.md) | 运行循环、分层 crate 与扩展模块边界 | 已实现；与当前代码模块一致 |
| [决策记录](decisions.md) | 已确认方向、基线与演进历史 | 持续维护 |
| [根目录 AGENTS.md](../AGENTS.md) | 编码 Agent 的协作、边界与提交流程约定 | 当前有效 |
| [MIT 许可证](../LICENSE) | 项目开源许可与版权声明 | 已生效 |

## 子系统设计与使用文档

| 文档 | 用途 | 当前状态 |
| --- | --- | --- |
| [CLI 使用指南](usage.md) | 构建、配置、TUI／CLI 交互、恢复与导出入门 | 已实现；覆盖 `0.2.0` 与当前开发线 |
| [配置组件](configuration.md) | 配置来源、模型 profile、沙箱预设与模块边界 | 已实现；接入 CLI 与默认 TUI |
| [CLI 入口与实现记录](cli-preview.md) | 单次运行、文本会话、后台任务与原生入口边界 | 已实现；细节见使用指南 |
| [运行时接口](runtime.md) | 可复用 core 循环、`runtime/` 模块与并发批处理边界 | 已实现；受控及模型联调已验证 |
| [模型协议接入](model-protocols.md) | OpenAI／Anthropic 协议映射、思考摘要、缓存与续写边界 | 已实现；三协议受控验证通过 |
| [SQLite 存储组件](storage.md) | SQLite WAL、Schema v2、事务、写锁与迁移边界 | 已实现；受控与崩溃恢复验证通过 |
| [原生 Sandbox](sandbox.md) | Seatbelt、Linux Bubblewrap／Seccomp／socat、网络代理与 CST | 已实现（N2）；完成四平台验收 |
| [命令结构分析](command-analysis.md) | Tree-sitter Bash CST 风险事实、预算与独立核查绑定 | 已实现（N2）；完成四平台验收 |
| [工具注册](tool-registry.md) | 本地／外部工具身份、宿主分类、代次与派发 | 已实现（N2）；完成四平台验收 |
| [MCP 与凭据管理](mcp.md) | stdio／Streamable HTTP MCP、私有凭据与 OAuth 绑定 | 已实现（N2）；完成四平台验收 |
| [上下文与长输出](context-management.md) | OutputStore、检索、Compact、检查点与缓存前缀 | 已实现（N3）；完成四平台验收 |
| [项目指令与 Skill 发现](project-context.md) | 用户级／工作区指令与 Skill 发现优先级和安全边界 | 已实现；受控验证通过 |
| [提示词定制](prompts.md) | 英文 prompt 装配、模式／核查分段与前缀缓存设计 | 已实现；受控接线与评估通过 |
| [提示词来源清单](prompt-sources.json) | 提示词模板来源、相对路径与 SHA-256 核查记录 | 历史来源核查清单 |
| [当前提示词快照](prompt/README.md) | `XLFMTPsoyo-prompts-v1` 正式正文与 `drafts/n3/` 草稿说明 | 运行时嵌入正文（`v1/`） |
| [日志与追踪](observability.md) | 五级日志、关联 span、脱敏记录与 `soyo logs` 查询 | 已实现（N0）；本机验收通过 |
| [Session 导出](session-export.md) | 一致性快照、Markdown／JSON 导出与私有文件写入 | 已实现（N0）；本机验收通过 |
| [soyo-plugin v1 规范](soyo-plugin.md) | 插件 manifest v1、权限、lock、安装与 Hook 生命周期 | 已实现（N3）；完成四平台验收 |
| [插件结构 schema](schemas/soyo-plugin-v1.schema.json) | `soyo-plugin.toml` 的 JSON Schema 校验定义 | 运行时集成（N3） |
| [插件 manifest 示例](../examples/soyo-plugin.example.toml) | 声明 Skills／MCP／Hooks 的结构示例 | 格式示例 |
| [全局记忆](memory.md) | `MEMORY.md`、专用工具、审阅、更正／遗忘与回滚 | 已实现（N3）；完成四平台验收 |
| [Goal 设计](goal.md) | 独立持久 Goal、结构化计划、预算与人工／自动推进 | 已实现（N4）；完成本机受控验收 |
| [Browser 设计](browser.md) | 受控 Web、Playwright worker、VM、文件／网络 broker 与截图遮蔽 | 已实现（N5）；VM 与 ARM64 实测通过 |
| [TUI 设计](tui.md) | 默认 `soyo` 终端入口、分层控制、附件与恢复路径 | 已实现（N1／后续交互）；持续演进 |
| [TUI 布局设计](tui-design.md) | 配色、侧栏、输入区、状态反馈与 `view/` 模块布局 | 已实现；与 `view/` 一致 |
| [Markdown 与调试预览](markdown-rendering.md) | CommonMark／GFM、数学／图表高亮与 `tui-preview` | 已实现；离线及 PTY 验证通过 |
| [桌面端设计](desktop.md) | `soyo-desktop`（Tauri 2 + in-process）IPC、安全边界与分发设计 | 已确认（D-072）；分阶段实施中 |
| [桌面端 UI/UX 设计](design/desktop-ui.md) | 深色主题令牌、pill 组件规范、消息流／审批／Composer／用量等界面规则 | 设计稿已确认（Figma Page 3）；实现未完成 |

## 开发、进度与阶段验收

| 文档 | 用途 | 当前状态 |
| --- | --- | --- |
| [开发指南](development.md) | 协作流程、`justfile` 任务、Rust 检查与 CI 构建 | 当前有效 |
| [开发进度](progress.md) | 各阶段、DEV 任务状态与最新本地／CI 实测日志 | 持续维护 |
| [MVP 实施路线图](roadmap.md) | 0.1.0 P0 至 P7 里程碑定义与交付条件 | 已交付（0.1.0 历史基线） |
| [实施任务](implementation-plan.md) | DEV-001 至 DEV-037 任务拆解与验收映射 | 已完成（0.1.0 历史基线） |
| [后续能力路线图](capability-roadmap.md) | F-01 至 F-20 功能模块规划、已实现范围与未实现项（含 GUI 等） | 规划与现状对照 |
| [后续开发计划](post-mvp-plan.md) | 0.2.0、N0 至 N5 阶段拆解（DEV-038 至 DEV-089）与待规划项 | 迭代主计划 |
| [MVP 验收证据](acceptance.md) | AC-01 至 AC-24 对应测试模块与发行证据索引 | 已验收（0.1.0） |
| [TUI 验证](tui-validation.md) | 候选及后续源码的终端布局、渲染与 PTY 验收记录 | 已验证 |
| [N2 验收](n2-validation.md) | 原生隔离、CST、MCP 与性能四平台验收证据（`aaec8c5`） | 完成四平台验收 |
| [N3 验收](n3-validation.md) | 上下文、插件／Hooks 与全局记忆四平台验收证据（`e1da373`） | 完成四平台验收 |
| [N4 Goal 验收](n4-validation.md) | Goal 存储、推进、worker、预算与故障矩阵验证 | 完成本机受控验收 |
| [N5 Browser 验收](n5-validation.md) | 定制引擎、Colima VM、ARM64 原生实测与 `5fff1c4` 四平台远端终验 | 完成本机／VM／`5fff1c4` 四平台验收（非 ARM64 定制引擎待分发） |

## 历史档案（`reviews/`、`releases/`、`reports/`）

> 下述文档记录特定历史提交或发行候选时点的静态审查、双语发行说明与专题实验报告；如需了解**当前仓库实际行为**，请以上文核心规范与子系统文档为准。

### 代码审查与整改记录（`reviews/`）

| 文档 | 内容 |
| --- | --- |
| [2026-11-07 风险审查](reviews/review-2026-11-07.md) | 历史问题、修复与候选验证记录 |
| [2026-11-09 首轮审查](reviews/review-2026-11-09.md) | RC1 后 6 项问题修复与交叉复核报告 |
| [2026-11-09 第二轮审查](reviews/review-2026-11-09_02.md) | 仓库深度审查、安全耦合分析与文档整理建议 |
| [2026-11-10 首轮审查](reviews/review-2026-11-10.md) | N0–N2 阶段审查与终端背压／调度修复记录 |
| [2026-11-10 第二轮审查](reviews/review-2026-11-10_02.md) | N2 隔离与存储路径第二轮复核与回归证据 |
| [2026-11-10 第三轮审查](reviews/review-2026-11-10_03.md) | N2 收尾一致性复核记录 |
| [2026-11-11 N4 审查与整改](reviews/review-2026-11-11.md) | Goal 模块 15 项契约问题复现、修复与回归证据 |

### 发行说明与校验记录（`releases/`）

| 文档 | 内容 |
| --- | --- |
| [MVP 发行检查清单](releases/release-checklist.md) | B-01 至 B-24 检查项与 CLI 测试映射 |
| [发行验证记录](releases/release-validation.md) | 历史构建产物、校验和、最低环境与下载复核 |
| [0.1.0 双语版本说明](releases/release-notes-0.1.0.md) | 0.1.0 功能、安装、迁移与平台限制 |
| [0.2.0-rc.1 与开发线双语说明](releases/release-notes-0.2.0-rc.1.md) | 候选迁移、后续交互增量与限制 |
| [0.2.0-rc.2 双语版本说明](releases/release-notes-0.2.0-rc.2.md) | RC2 候选范围与四平台验收归属 |
| [0.2.0 双语版本说明](releases/release-notes-0.2.0.md) | 已发布 0.2.0 正式版范围、安装与限制 |
| [N4 中英双语说明](releases/release-notes-n4.md) | Goal 功能、命令、安全边界与开发线说明 |
| [N5 中英双语说明](releases/release-notes-n5.md) | Browser 开发线功能、权限、VM 与兼容性说明 |

### 专题实验与模型验证报告（`reports/`）

| 文档 | 内容 |
| --- | --- |
| [真实模型联调记录](reports/live-model-validation.md) | DEV-030 兼容端点小规模样本联调记录 |
| [提示词多轮缓存验证报告](reports/prompt-evaluation-2026-11-11.md) | DeepSeek 10 场景多轮前缀缓存率对比评估 |
| [缓存 95% 与工具错误联合验证](reports/prompt-cache-95-2026-11-11.md) | `saya_runtime_context_v1` 后移后 95.05%／Turn 2+ 97.20% 联合验证 |
| [提示词与模型任务能力联合验证](reports/prompt-capability-2026-11-11.md) | 10 场景任务能力、命中率与工具修复联合验证报告 |

## 状态含义

- **已实现**：已有代码、受控回归或平台验证证据支持的能力；未支持场景在各自文档注明限制。
- **已确认**：项目负责人明确确定的产品与工程约束，记录于[完整基线](mvp-baseline.md)与[决策记录](decisions.md)。
- **规划中（未实现）**：如 GUI、多 Agent 协作等在[后续能力路线图](capability-roadmap.md)或 `prompt/drafts/` 中维护的设计方案。
- **历史档案**：记录过往里程碑审查、历史发行说明与专题实验快照，保留原始上下文与结论。

## 维护方式

产品范围变化时更新[项目范围](vision.md)与[决策记录](decisions.md)；接口、行为或模块边界变化时同步更新对应设计文档，确保所有链接与代码示例可验证。新增文档时在本索引对应分类中添加条目，历史一次性报告或版本说明放入 `reviews/`、`releases/` 或 `reports/`。
