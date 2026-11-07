# Session 导出设计

**DEV-043 至 DEV-045 已实现只读快照、版本化 JSON 与 Markdown 导出。CLI 文件接入按 DEV-046 推进，以下 CLI 命令在该任务完成前保留为草案。**

## 现有基础与入口

`session list`、`session show` 和运行 JSON／JSONL 已在 0.1.0 实现。后续文件导出从 SQLite 中已保存的历史生成，读取与渲染可由 CLI、TUI 及未来 SDK 复用。

候选 CLI 如下，命令在对应 DEV 完成后开放。

```sh
saya session export SESSION_ID --format markdown --scope transcript --file ./session.md
saya session export SESSION_ID --format json --scope full --include-tools --file ./session.json
saya --output json session export SESSION_ID --format markdown --file ./session.md
```

现有全局 `--output` 选择命令 stdout 的 text／json／jsonl 表达，新增 `--file` 选择导出文件路径。`--format` 选择文件内容，默认 `markdown`，`--scope` 默认 `transcript`，工具正文由 `--include-tools` 显式包含。成功确认返回文件路径、格式、字节数、摘要哈希及快照水位，错误沿用 CLI 的输出与退出码约定。

## 一致性快照

core 提供 `SqliteStore::read_snapshot`、`SessionSnapshot` 与 `SnapshotLimits`。源数据库和 WAL 经稳定性哈希及提交帧校验重建为私有内存 image，再通过一次只读事务取得会话、历史、运行、工具、审批、用量、后台状态、上下文来源、持久事件和变更记录。导出器共享该规范化视图。源目录不会因查询创建数据库、SHM、锁或迁移文件，源正在变化且无法取得稳定 image 时返回 `storage_busy`。[SQLite 隔离说明](https://www.sqlite.org/isolation.html)

默认读取限制为源 DB 加 WAL 合计 128 MiB、规范化原始记录合计 32 MiB／100,000 行、5 秒协作期限。调用者可显式缩小或调整限制，源字节硬上限为 512 MiB。I/O 分块和逐行查询检查取消与期限，限额超出返回明确错误。旧数据库返回 `storage_migration_required`，迁移由显式写入入口执行。

快照保存以下元数据。

- 会话 ID、生成时间、导出格式版本、源数据库 schema 和选择范围。
- 实际已持久化的 `last_change_seq` 与 `last_event_seq`，以及采样时的运行状态。
- 各集合的包含／省略规则、工具正文选择、截断与脱敏范围。
- 历史原顺序、Run ID、调用关联及后台任务父子关联。

水位取已提交记录，事件预留序号中的空洞允许存在。工具关联采用 `(run_id, call_id)`，避免不同 Run 的同名调用混淆。活跃会话显示其快照时状态，`unknown`、未完成和已拒绝结果按记录呈现。

[SqliteStore](../crates/saya-core/src/sqlite.rs) 现有分项查询可复用解码和验证逻辑，统一导出入口需要单一事务。只读打开路径应独立验证 schema 与文件，数据库创建、迁移和中断恢复属于各自的显式操作。遇到需要迁移的旧库时给出迁移提示。

## 范围与内容选择

| 内容 | transcript | full |
| --- | --- | --- |
| 用户与可展示 assistant 文本 | 包含 | 包含 |
| 会话、模式和 profile 的安全元数据 | 包含 | 包含 |
| 工具状态与关联 ID | 保留必要关联 | 包含完整结构化摘要 |
| 工具参数及结果正文 | `--include-tools` 时包含 | `--include-tools` 时包含 |
| Run、用量、后台任务与相关持久事件 | 按对话所需引用 | 包含已保存的结构化记录 |
| 指令来源与已保存哈希 | 简要来源 | 包含已保存的来源元数据 |

导出使用持久化来源记录，文件系统中后续改动的指令内容具有独立时间点。供应商不透明签名／内部状态保存在运行存储的协议范围内，导出文件用省略元数据描述这些字段。会话导入、协议迁移和执行回放分别规划。

## JSON 格式草案

core 的 `export::SessionExport::from_snapshot` 按 `ExportOptions` 生成统一投影，`to_json` 编码为独立 `schema_version = 1`、`export_type = saya.session` 格式。默认 scope 为 transcript，include_tools 为 false，每个字符串保留最多 64 KiB，最终编码最多 32 MiB。身份字段超过单字段限额时返回 `export_limit`，正文按 UTF-8 边界截取，并在 `truncated_fields` 保存 JSON Pointer、原字节数及保留字节数。

投影在截断前对全部已选择内容重新脱敏。Full 增加持久事件、activity、指标和审批元数据，工具参数及结果正文在 history、调用表、后台任务、事件、activity 和审批中统一按 include_tools 选择。供应商 ProviderItem 整体省略，可展示文本保留原 ordinal。旧 messages 表缺少 run_id，历史工具项只在持久调用可唯一匹配时补充 run_id，否则用 null 与 omitted_fields 说明。调用表始终保留独立的 (run_id, call_id)。

以下为空集合形状示意。

```json
{
  "schema_version": 1,
  "export_type": "saya.session",
  "snapshot": {
    "created_at": "2026-11-07T15:00:00Z",
    "source_database_schema": 2,
    "last_change_seq": 0,
    "last_event_seq": 0,
    "scope": "transcript",
    "include_tools": false,
    "redaction_scope": "persisted-markers-and-current-configured-values"
  },
  "session": {
    "id": "session-id",
    "work_mode": "WORK",
    "profile": "configured-profile"
  },
  "history": [],
  "runs": [],
  "tool_calls": [],
  "metrics": [],
  "background_jobs": [],
  "events": [],
  "activities": [],
  "project_context": [],
  "approval_requests": [],
  "omitted_fields": ["provider_internal_state"],
  "truncated_fields": []
}
```

该格式从规范化快照映射。0.1.0 的结果／事件 schema v1、数据库 schema 2 和上述导出 schema 分别演进。

## Markdown 表达

`SessionExport::to_markdown` 使用与 JSON 相同的投影，按原 ordinal 展示用户、assistant、部分响应和选定工具内容，附会话元数据、快照水位、Run／工具复合关联、来源、省略字段及截断说明。Full 范围额外呈现指标、持久事件、activity 和审批元数据。

消息与结构化记录放入动态长度代码围栏，围栏长度大于正文连续反引号最大长度。正文 HTML 字符转义，ANSI、回车及双向控制字符以显式 Unicode 转义呈现，中文和 emoji 保留。文件内容总上限与 JSON 一致，转义使 Markdown 超过编码限额时返回 `export_limit`。

大结果采用一致的大小上限，JSON 和 Markdown 对同一字段采用相同的选择、脱敏与截断策略。读取预算超限时返回明确错误或带声明的有界结果，具体上限在 DEV-043 固定。

## 文件、安全与失败

1. 用户显式选择目标路径，默认私有权限为 0600。已存在目标与符号链接在发布前检查，并使用具有排他创建语义的提交操作。
2. 在目标父目录创建私有临时文件，完成渲染、写入和同步后发布。中途取消、磁盘满或编码失败时清理临时文件。
3. 文件名竞争在最终提交点重新处理，已有文件保持原内容。并发测试覆盖验证与提交之间的路径替换。
4. 当前调用显式选择 PLAN 时，目标及临时文件须位于工作区外，复用现有目录边界检查。运行记录的历史模式作为被导出数据保存。
5. 脱敏再次应用于规范化快照，覆盖当前可解析配置凭据和已持久化遮蔽标记。省略字段及脱敏能力范围写入元数据。
6. 导出由显式用户命令或 TUI 操作发起，模型请求次数为零。对活跃会话的查询完成后释放读事务，并在返回确认前核对最终文件摘要。

## 验收

- 并发写入的活跃会话导出具有一致水位，跨表记录和 `(run_id, call_id)` 关联完整。
- 旧 schema、损坏数据库、未知结果、后台父子记录和部分运行具有明确输出。
- 两种格式覆盖中文、emoji、超长输出、嵌套编码秘密、代码围栏、HTML 与控制字符。
- 文件已存在、链接替换、磁盘失败、取消和 PLAN 目标路径通过故障注入。
- 同一快照的两种表达内容对应，stdout 的命令确认符合当前选择的输出模式。

CLI 和 TUI 共用导出服务，任务定义及依赖见[后续开发计划](post-mvp-plan.md)。
