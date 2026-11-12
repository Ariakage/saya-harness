# Saya 当前提示词

本目录保存 `saya-prompts-v1` 的一份可阅读快照，包含截至2026-11-11 的核心、工具、四种工作模式、三种权限模式、后台工具与独立审核提示词。正文保持英文，包含任务清单、宿主输出引用、完整性与有界检索，以及N3上下文、记忆证据和扩展边界。

- [核心](v1/core.md)、[工具](v1/tools.md)、[后台调用](v1/background.md)
- 工作模式：[PLAN](v1/modes/plan.md)、[BUILD](v1/modes/build.md)、[CODE](v1/modes/code.md)、[WORK](v1/modes/work.md)
- 权限模式：[auto](v1/permissions/auto.md)、[Manual Approval](v1/permissions/manual.md)、[bypass](v1/permissions/bypass.md)
- [独立安全审核](v1/review.md)
- [Goal结构化规划](v1/goal-plan.md)、[Goal验收核验](v1/goal-verification.md)
- [结构化Compact](v1/compact.md)、[条件式续接](v1/compaction-continuation.md)、[有来源的记忆上下文](v1/memory-context.md)
- 按实际能力选择的[输出检索](v1/output-retrieval.md)、[记忆写入与遗忘](v1/memory-operations.md)、[Skill与扩展](v1/extensions.md)、[会话任务清单](v1/task-tracking.md)和[工作区快照](v1/workspace-snapshots.md)

实际运行时通过 `include_str!` 使用 [`crates/saya-core/prompts/v1`](../../crates/saya-core/prompts/v1) 中的文件；此目录是文档快照，修改它不会改变运行行为。提示词后续修改时应同步本快照。执行时按当前模式、权限及可用工具动态组装，不会同时使用全部模式文本；可用工具名称和运行上下文由代码生成，不属于静态提示词文件。装配规则和来源对照见[提示词定制](../prompts.md)。

N3摘要、续接和记忆读取规则已有运行时版本。只有宿主选中已提交的上下文checkpoint时才把完整续接段放入静态前缀，否则只保留短说明；动态 `checkpoint_selected_by_host` 明确标记本次状态。摘要与当前Memory内容沿来源数据段注入。Compact使用执行profile、空工具和有限共享预算，输出按结构化schema验证。初始英文草案、指定来源和适配差异见[Compact来源记录](drafts/n3/README.md)。

工具规则已说明memory_read／search／evidence／update／forget，独立审核规则解释 `__saya_managed.review`、证据等级、作用域、更正及遗忘。用户任务摘要保持调用方来源身份，实际授权依据当前用户证据与宿主规则。三份记忆参考、原型与验收要求见[记忆提示词来源](drafts/n3/memory-sources.md)。独立memory-consolidation仍保留为设计草案。

当前源码的模型请求、协议、UI和四平台验收结果按[开发进度](../progress.md)与[N3验收](../n3-validation.md)维护。
