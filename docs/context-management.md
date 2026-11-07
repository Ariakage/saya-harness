# 上下文、Compact 与长工具输出

**更新于 2026-11-08。本文为 F-06、DEV-062／063／076／077 的设计草案，候选限额在实现阶段通过三协议验证后固定。**

## 当前实现与新增边界

当前运行时将项目／个人上下文和 Session history 组装为 ModelRequest。读取工具已有返回上限，默认读取返回最多 256 KiB、内容搜索最多 200 项／128 KiB。exec 默认分别保留 stdout、stderr 的前 64 KiB，并继续排空后续数据。现有截断结果通过标记呈现，超出保留范围的文本缺少后续检索引用。

后续增加三个模块。

| 模块 | 职责 |
| --- | --- |
| OutputStore | 有界捕获、脱敏存储、预览、分页与搜索 |
| ContextBuilder | 请求投影、token 预算、协议分组、固定前缀与动态数据 |
| CompactionCoordinator | 安全切点、摘要调用、校验及持久化上下文检查点 |

Session 原始记录用于历史、恢复与导出，模型请求使用可重建的 ContextView。

## 长输出处理

输出按流式路径处理。

```text
工具产生数据
  -> 边界缓存、解码与脱敏
  -> 有界 OutputStore 与完整性元数据
  -> 短预览、状态、output_ref
  -> 模型按需 output_search / output_read
```

exec 的捕获接入点位于管道 drain 阶段，MCP 接入点位于结果解码阶段，浏览器接入点位于快照及产物规范化阶段。读取与搜索工具保持分页，并提供受限预览。

首轮候选策略如下，生效上限还取决于用户配置及剩余上下文。

| 项目 | 候选策略 |
| --- | --- |
| 单个模型可见结果 | 最多约 4,096 tokens，并受字节上限约束 |
| 一次分页读取 | 最多约 2,048 tokens |
| 本轮工具结果总投影 | 最多约 8,192 tokens，按请求余量缩减 |
| 新输出捕获 profile | 每项 16 MiB、每 Run 128 MiB 的实测起点 |
| 已有配置的捕获／返回上限 | 继续作为有效约束，扩大捕获需要显式配置和迁移说明 |

持久化内容经过脱敏，结构化或编码内容无法安全处理时保存 withheld 元数据。超过捕获额度后继续排空管道，维护有限尾部预览和已观察字节数。保存 `artifact_complete`、保留范围及缺口，搜索结果同时声明覆盖范围。

模型可见结果优先包含退出码、错误、超时／取消、截断状态和输出引用，再包含首尾或错误附近的文本。候选结果形状如下。

```json
{
  "exit_code": 1,
  "preview": "compiler error near the retained tail",
  "output_ref": "out_session_run_call_stdout",
  "truncated": true,
  "artifact_complete": false,
  "total_bytes_seen": 24000000,
  "retained_bytes": 16777216,
  "coverage": "retained-ranges-only"
}
```

`output_read` 接受引用、游标／行范围和有界返回量。`output_search` 接受引用、字面量或受限正则、结果数和上下文行数。引用绑定 Session／Run／调用、存储摘要和来源，查询按当前权限及资源 ACL 检查。Manual Approval 覆盖这两类模型工具调用。

引用对象按配额管理，活跃上下文持有租约，GC 优先处理无引用对象。对象缺失时返回 `output_unavailable` 并保留原因，重新执行原命令由新的显式调用触发。

捕获前登记调用归属和预留引用，脱敏数据先写私有临时对象，完成摘要及同步后封存，再与工具结果一起登记可见引用。崩溃留下的对象按调用意图及封存状态核对，未关联对象进入受控 GC。命令执行状态与输出捕获状态分别记录，磁盘失败保留实际退出／未知结果及缺失原因。

输出引用由宿主生成并校验来源，正文中的相似字符串按普通数据处理。Session history 保留小结果及引用，较大可保留正文由 OutputStore 管理。

## 检索与 grep 策略

模型默认优先使用 `find_files`、`search_text` 和有界 `read_file`，已有输出通过 `output_search` 定位，再分页读取相关区间。`grep`／`rg` 可通过获准的 exec 使用，PLAN 的调查由内置只读搜索工具承接。

未来提示词片段采用英文，并随实际工具注册情况注入。

```text
Search before reading large sources. Prefer the native search tools and bounded
file reads. For an output reference, search it and request relevant pages.
Treat truncation and incomplete capture as missing evidence. Use grep or rg
through exec when that operation is authorized and appropriate.
```

运行时对所有结果执行字节、token、条数及总请求预算检查。模型参数只可请求更小的范围。

## ContextBuilder 与预算

输入包含当前类型化模式／权限／预算、静态指令、动态宿主信息、项目来源、显式偏好／记忆、Goal 状态、工具声明、摘要检查点和近期历史。

定义模型窗口 `C`、输出预留 `O`、安全余量 `R`，允许输入预算为 `B = C - O - R`。全部 system、tools、消息封装、签名／不透明状态和媒体均计入输入估算。缓存前缀同样占用窗口。

token 统计优先使用经验证的模型 tokenizer 或供应商计数接口，其余情况使用保守估算并标记不确定性。远端计数请求计入请求／时间预算，媒体或协议项的估算不足时提高余量或明确结束请求。Anthropic 提供结构化 token counting，支持范围需按 profile 验证。[Token counting](https://platform.claude.com/docs/en/build-with-claude/token-counting)

构建顺序为当前指令与工具集合、最新用户请求和必要约束、协议安全组、有效摘要、近期完整历史、按相关性取出的输出片段。大结果先转成引用，预算余量用于正文。固定内容或必留组超过预算时给出 `context_limit` 原因及可处理范围。

## Compact 流程

1. 在下一次模型请求前计算请求投影。初始自动触发水位建议为 `0.80 * B`，目标降至 `0.55 * B`，形成迟滞区间。
2. 固定最新用户请求、当前真实运行状态、必要原始约束、未闭合调用、活跃后台任务引用及供应商必留内容。
3. 适配器给出可压缩的完整协议组。选择较早的已闭合前缀，大正文先引用化，近期完整组按剩余预算保留。
4. 默认复用执行 profile，在额度内请求英文摘要任务，输入为可展示文本和类型化来源，工具集合为空，输出结构化事实、决定、约束来源、已完成／待办、产物引用及不确定项。
5. 摘要输入过长时按完整组分块，每块受输入／输出预算约束，再进行有界合并。单个不可拆组过大时报告原因。
6. 校验 schema、引用、必留项和新投影预算。摘要作为带来源的数据加入上下文，模式、权限和预算从运行状态重新注入。
7. 以源水位、前缀摘要、profile／适配器版本和 Session revision 做条件提交，原子保存 CompactionRecord 与 ContextView 指针。
8. 使用新视图继续请求。传输重试遵循 B-16，摘要校验失败返回 `compaction_invalid`。全部摘要、计数和重试消耗计入 Run／Goal 预算。

同一 Session revision 只有一个压缩任务，等待方共享结果。压缩任务具有取消、超时和次数限制，内部再次压缩关闭。源码范围变化后重新验证水位，长模型调用期间释放数据库事务。

## 三协议安全边界

| 协议 | 压缩分组要求 |
| --- | --- |
| Chat Completions | assistant 工具调用及全部对应结果保持闭合，保留调用顺序和 ID |
| Responses | typed items、混合输出和 opaque reasoning 按适配器安全组处理，保留后缀原始协议状态 |
| Anthropic Messages | tool_use／tool_result 及适用 thinking／signature／redacted blocks 按所选版本整体保留 |

适配器先验证候选切点，再验证重建请求。无法安全裁剪的状态保留原组，并明确报告剩余预算。Anthropic 对 thinking 的工具往返和签名回传有具体约束。[Thinking 与工具调用](https://platform.claude.com/docs/en/build-with-claude/thinking)

首轮提供本地可追溯摘要。供应商原生 Compact／context editing 作为后续适配策略，在对应协议矩阵中单独验证。

## 缓存、持久化与恢复

- 静态系统指令与规范化工具声明先于动态上下文，工具顺序稳定，能力变化有版本指纹。
- 时间、Goal 进度、输出水位和近期检索结果进入动态段。摘要按检查点复用。
- 原有 Anthropic 显式 cache control 及各 profile 能力继续由协议适配器处理。压缩后复用稳定静态前缀，并记录历史前缀重建的缓存影响。
- CompactionRecord 保存来源范围／哈希、保留组、摘要、提示词版本、用量和视图版本。恢复验证记录与 profile 兼容性，再应用本次权限。
- 会话导出读取原始持久历史，full 范围可按版本包含压缩记录和输出引用元数据。

## 验收

DEV-076 验证管道排空、stdout／stderr、跨块秘密、输出配额、首尾缺口、检索分页、权限、取消及 GC。DEV-062／063／077 验证估算误差、超大静态前缀、长单组、多工具、签名状态、摘要注入、重复压缩、并发水位、存储失败、恢复及缓存前缀。

质量夹具核对压缩前后的用户约束、工作模式、剩余任务、调用配对和证据引用。任务定义见[后续开发计划](post-mvp-plan.md)。

## 记忆接入

后续F-07通过MemoryStore和专用模型工具维护全局／项目记忆，候选文件为 `~/.saya/MEMORY.md`。读取片段与版本进入ContextBuilder的Memory预算，Compact和缓存处理需要遵循遗忘与当前版本。设计及待开发任务见[全局记忆](memory.md)和DEV-084至DEV-089。
