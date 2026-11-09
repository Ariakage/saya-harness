# Saya 当前提示词

本目录保存 `saya-prompts-v1` 的一份可阅读快照，包含截至 2026-11-08 的核心、工具、四种工作模式、三种权限模式、后台工具与独立审核提示词。正文保持英文，工具与PLAN段已纳入会话任务清单规则。

- [核心](v1/core.md)、[工具](v1/tools.md)、[后台调用](v1/background.md)
- 工作模式：[PLAN](v1/modes/plan.md)、[BUILD](v1/modes/build.md)、[CODE](v1/modes/code.md)、[WORK](v1/modes/work.md)
- 权限模式：[auto](v1/permissions/auto.md)、[Manual Approval](v1/permissions/manual.md)、[bypass](v1/permissions/bypass.md)
- [独立安全审核](v1/review.md)

实际运行时通过 `include_str!` 使用 [`crates/saya-core/prompts/v1`](../../crates/saya-core/prompts/v1) 中的文件；此目录是文档快照，修改它不会改变运行行为。提示词后续修改时应同步本快照。执行时按当前模式、权限及可用工具动态组装，不会同时使用全部模式文本；可用工具名称和运行上下文由代码生成，不属于静态提示词文件。装配规则和来源对照见[提示词定制](../prompts.md)。

N3新增的英文摘要与续接草案、指定来源及取舍见[Compact提示词草案](drafts/n3/README.md)。实际运行时接入状态在对应草案说明与开发进度中维护。

N3记忆读取与整理的英文草案、三份指定参考及DEV-084至DEV-089验收要求见[记忆提示词来源](drafts/n3/memory-sources.md)。
