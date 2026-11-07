# 项目文档

Saya Harness 的说明文档默认使用中文，发行说明提供中英双语。CLI MVP 已完成 P0 至 P7 验收并发布 [v0.1.0](https://github.com/Ariakage/saya-harness/releases/tag/v0.1.0)。具体行为以[完整基线](mvp-baseline.md) B-01 至 B-24 及后续决策为准。

| 文档 | 用途 | 当前状态 |
| --- | --- | --- |
| [项目范围](vision.md) | 目标、交付阶段与 MVP 验收入口 | 已确认 |
| [需求规格](requirements.md) | 已确认需求、用户流程与验收条件 | 已确认；实现和验证范围见开发进度 |
| [完整基线](mvp-baseline.md) | 全部 24 组决定及具体契约 | 已整套确认，D-027 |
| [工作模式](modes.md) | 四种工作模式的职责与切换规则 | 已确认；实现和验证范围见开发进度 |
| [执行与恢复契约](execution.md) | 权限模式、切换边界、脚本输出与恢复策略 | 已确认，按完整基线实现 |
| [运行时接口](runtime.md) | 可复用 core 循环和调用边界 | 受控验证通过；真实模型小规模联调通过 |
| [真实模型联调记录](live-model-validation.md) | 测试服务、模型、日期与结果 | DEV-030 单模型小规模联调 |
| [模型协议接入](model-protocols.md) | HTTP／SSE、协议适配与验证边界 | 三协议受控验证及指定模型小规模联调通过 |
| [架构设计](architecture.md) | 运行流程、职责划分与实现约束 | 已接入运行时；范围见开发进度 |
| [开发指南](development.md) | 协作流程、Rust 检查与文档维护 | 初版约定 |
| [CLI 使用指南](usage.md) | 构建、配置、示例与故障处理 | 0.1.0 使用指南，示例经验证 |
| [CLI 开发预览](cli-preview.md) | 单次运行、持久会话和当前限制 | 实现记录，当前发行行为见使用指南 |
| [项目指令与 Skill 发现](project-context.md) | 来源、优先级与刷新规则 | 受控验证通过 |
| [SQLite 存储组件](storage.md) | 事务、事件序号与持久化边界 | 本机受控验证，恢复见开发进度 |
| [saya-plugin v1 规范](saya-plugin.md) | manifest、权限、lock、生命周期和验收 | 规范草案，loader 待实现 |
| [插件结构 schema](schemas/saya-plugin-v1.schema.json) | TOML 解析结果的 JSON Schema | v1 草案，含语义验证要求 |
| [插件 manifest 示例](../examples/saya-plugin.example.toml) | 声明 Skills／MCP／Hook 的结构示例 | 格式示例 |
| [后续能力路线图](capability-roadmap.md) | F-01 至 F-20 的功能模块、优先级与依赖 | MVP 基础与待开发增量分别记录 |
| [后续开发计划](post-mvp-plan.md) | 0.2.0 候选、N0 至 N5、DEV-038 至 DEV-089 | 任务定义与设计草案 |
| [全局记忆](memory.md) | MEMORY.md、模型编辑／遗忘、审阅与回滚 | 设计草案，DEV-084 至 DEV-089 待开发 |
| [原生 Sandbox](sandbox.md) | Seatbelt、Linux 原生组合、网络、AST 与能力等级 | 后端组合已明确，接口待实现 |
| [上下文与长输出](context-management.md) | 预算、引用／检索、Compact、协议及缓存 | 设计草案 |
| [Browser 设计](browser.md) | 托管浏览器、页面／文件／网络与恢复 | 设计草案 |
| [TUI 设计](tui.md) | 默认入口、应用分层、交互与终端恢复 | DEV-047 至 DEV-053 已完成候选验收，后续交互完善见开发进度 |
| [日志与追踪](observability.md) | 五级日志、span、安全记录与本地查询 | DEV-040 至 DEV-042 已实现，本机验收通过 |
| [Session 导出](session-export.md) | 一致性快照、Markdown／JSON 与文件保存 | DEV-043 至 DEV-046 已实现，本机验收通过 |
| [Goal 设计](goal.md) | 目标、证据、预算、恢复与分层交付 | 设计草案，DEV-069 至 DEV-074 待开发 |
| [MVP 实施路线图](roadmap.md) | MVP 里程碑、估算与交付条件 | P0 至 P7 验收完成 |
| [实施任务](implementation-plan.md) | 37 项任务、依赖与验收覆盖 | 状态见开发进度 |
| [发行检查](release-checklist.md) | 24 组基线、验收与已知限制 | P7 验收完成 |
| [0.1.0 双语版本说明](release-notes-0.1.0.md) | 功能、安装、迁移与平台限制 | 已发布，中英双语 |
| [发行验证记录](release-validation.md) | 构建产物、校验和、最低环境与下载复核 | v0.1.0 发布及复核完成 |
| [MVP 验收证据](acceptance.md) | AC-01 至 AC-24 的测试与发行验收映射 | 受控测试与四平台发行验收通过 |
| [开发进度](progress.md) | 阶段／任务状态与证据 | 持续维护 |
| [提示词定制](prompts.md) | 英文 prompt、装配与来源对照 | 组件与受控接线，真实模型效果未评测 |
| [当前提示词快照](prompt/README.md) | 核心、工具、模式、权限与审核提示词正文 | `saya-prompts-v1`，运行时来源见目录说明 |
| [决策记录](decisions.md) | 方向确认、基线与变更历史 | 持续维护 |
| [根目录 AGENTS.md](../AGENTS.md) | 编码 Agent 的执行约定 | 初版约定 |
| [MIT 许可证](../LICENSE) | 项目许可与版权声明 | 已添加 |

## 状态含义

- **已确认**：项目负责人明确确定的方向，记录在决策文档中。
- **草案**：供讨论的建议，允许调整，不代表已经定案。
- **已实现**：已有代码与相应验证证据支持的能力。
- **待决定**：尚缺产品选择或技术决策。

项目目标以最新确认的需求为准；现有行为需要查验代码与验证结果。二者出现偏差时应明确记录，不能只修改描述来掩盖差异。

当前 MVP 无待决产品阻塞项。后续 TUI／GUI、网络工具与其他扩展的范围已明确留到后续阶段，不与首版实现混在一起。

查看后续功能全景时使用能力路线图，选择下一个实现任务时读取后续开发计划和开发进度。MVP 路线图与任务表保留 0.1.0 的实施定义及验收依据。

## 维护方式

产品范围变化时更新项目范围与决策记录；接口、行为或模块边界变化时更新对应设计文档。只有实现并验证后，才将能力标记为已实现。

新增文档时在此添加入口；各文档使用相对链接，方便在仓库和托管平台阅读。
