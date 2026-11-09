# 上下文、Compact 与长工具输出

**更新于2026-11-10。N3开发线已接入OutputStore、ContextBuilder、自动／手动Compact和记忆投影。本文描述当前接口与有限预算，实际检查、质量和平台证据见[N3验收](n3-validation.md)及[开发进度](progress.md)。**

## 模块与持久历史

| 模块 | 职责 |
| --- | --- |
| OutputStore | 有界捕获、动态脱敏、首尾预览、分页／搜索和引用租约 |
| ContextBuilder | 完整请求预算、协议原子组、来源绑定、历史投影及压缩信号 |
| CompactionCoordinator | 合法前缀切片、有限摘要／合并调用、schema与目标预算校验 |
| ContextStore | 来源水位CAS、CompactionRecord与ContextView原子检查点 |
| MemoryStore与Exposure记录 | 当前记忆作用域、遗忘约束、失效历史组与摘要版本兼容性 |

Session原始历史保留用户输入、调用及结果。模型请求使用可重建的ContextView，用户历史导出继续读取原始记录。摘要作为显式历史来源数据进入投影，当前模式、权限、工具、预算和用户任务原文由宿主重新提供。

## 长工具输出

```text
exec管道／MCP解码结果
  -> 持续排空、有限捕获、解码与动态脱敏
  -> 私有OutputStore、首尾与覆盖元数据
  -> 小结果／宿主output_refs
  -> output_search定位、output_read分页
```

exec捕获位于管道drain阶段，stdout／stderr原有有效捕获上限默认各64KiB，超限继续排空。MCP在模型正文限额之前捕获文本和structuredContent为NDJSON，二进制内容记录省略描述。Browser产物接入仍按其后续任务推进。[配置与上限](configuration.md#输出捕获与上下文预算)

| 项目 | 当前默认或边界 |
| --- | --- |
| OutputStore单对象／单Run／总库配额 | 16MiB／128MiB／1GiB，原有producer捕获上限同时生效 |
| 对象数、尾部和预览 | 4096项，tail及preview各4096字节 |
| 模型侧output_read | 默认8KiB、100行，最多8KiB、200行，较低宿主上限优先 |
| 模型侧output_search | 最多50项、4行上下文及8KiB，较低宿主上限优先 |
| 大历史结果投影 | 宿主有对应durable引用且原结果超过16KiB时，以引用和必要状态投影 |

元数据包括output_ref、Session／Run／call／stream归属、capture_limit_bytes、total_bytes_seen、retained_bytes、retained_source_bytes、retained_ranges、gaps、artifact_complete、completion、coverage、withheld_reason、preview_head／tail及sha256。退出码、超时、取消和实际执行错误继续保留在工具结果中。

引用通过宿主独立通道登记，并与ToolFinished和历史结果在同一事务关联后发布。正文中相似的output_ref字符串保持普通源数据。无法安全解码或脱敏时返回withheld原因，捕获缺口进入覆盖范围。搜索只覆盖实际保留数据，查不到匹配时仍需考虑缺口。

output_read返回metadata、chunks及next_cursor，output_search返回相应匹配与页游标。游标绑定脱敏视图摘要，凭据集合变化后需要重新查询。Manual Approval覆盖每次查询调用，PLAN允许获准的只读查询，当前显式deny和Session归属检查持续生效。

Run发布对象持有租约，历史引用遵循有界存储和回收策略。缺失对象返回output_unavailable，重新执行producer沿新的明确调用与权限流程处理。磁盘发布失败保留实际工具结果及捕获错误，恢复核对原意图和封存对象，避免重复副作用。

## 检索与grep

优先使用find_files、search_text及有界read_file定位源材料。已有output_ref先output_search，再读取相关页。grep／rg可通过已获准的exec使用，PLAN调查使用内置只读工具。

工具schema、字节上限、条数与完整请求预算共同限制实际返回。模型可以请求更小的范围。截断、部分产物、缺失引用和未知执行状态分别作为证据缺口保留。

## 完整请求预算

定义模型窗口 `C`、安全余量 `R = ceil(C / 10) + 1 + context.extra_safety_tokens`，以及稳定输入基准 `B = C - R - 1`。所有静态／动态system段、工具schema、history、协议封装、opaque状态和缓存前缀都占用输入容量。80%触发与55%目标均按这个稳定B计算。

输出目标desired依次取最具体请求／用途显式值、实际profile显式max_output_tokens、profile.limit.output声明值、32000缺省。ContextBudget保留output_reserve字段作为请求未给覆盖时的目标默认值，输入基准始终保留至少1 token空间。每次请求以完整wire计数 `I_wire`拟合输出，满足 `1 <= O <= min(desired, C - R - I_wire)`，重新序列化最终请求后复核 `I_wire + O <= C - R`。数字宽度变化后的重计允许保留少量额外保守余量。该约束继续满足模型请求严格90%安全保护，空间不足时先处理可压缩来源或返回明确限制。

动态输出空间与压缩水位分别计算。输入接近窗口时降低本次O，触发基准B保持稳定，避免因可用输出减少而把每轮输入占用误算为100%。较大的声明值或覆盖值可用于小窗口模型，请求仍受实际剩余空间限制。配置格式及用途继承见[输出目标](configuration.md#模型输出-token-目标)。

三协议计数复用实际发送请求体的序列化路径，证据绑定完整请求指纹、协议、模型及适配器版本。当前token值使用明确标记为estimated的字符启发式，ASCII每4字符和其他Unicode标量每1字符。缺少可用counter时使用有界字节估算，并记录方法和不确定性。计数保持本地执行，远端计数或经验证tokenizer属于后续适配接缝。

ContextBuilder保留最新原始UserText、未闭合调用、宿主标记的保护组及供应商必留协议内容。工具组以完整调用／结果配对为原子单位。固定前缀或必留内容超限时返回context_limit和原因，完整原历史继续用于恢复。

单个用户任务持续运行时，最新UserText可以从较早已摘要前缀中逐字重放。保留来源范围及hash与摘要切片分别校验，原始工具组继续保持完整。该路径支持没有第二条用户输入的长任务压缩。

## 自动与手动Compact

默认 `context.auto_compact = true`，下一次Execution请求前在80%输入容量触发，成功后的投影必须降至55%以内。较高的固定前缀或必留内容使目标不可达时，协调器返回明确限制原因。配置见[ContextSettings](configuration.md#输出捕获与上下文预算)。

- `saya compact SESSION_ID`独占保存Session的写锁，在当前配置下执行独立Compact，支持显式接受配置引用变更的参数。
- 文本会话与TUI `/compact`在空闲时压缩当前Session，运行中请求在下一模型边界排队，重复请求合并。末轮完成仍有待处理请求时执行独立Compact。
- 独立Compact沿摘要请求路径工作，保留原始用户／工具历史，完成后等待用户继续。自动压缩关闭时仍保留手动入口和请求预算保护。

协调流程如下。

1. 构建当前请求与来源到投影的ordinal映射，先应用宿主输出引用及记忆失效组替换。
2. 选择合法的已闭合协议前缀，保留最新用户原文、当前运行控制和协议必留组。
3. 使用执行profile和空工具集合请求结构化英文摘要。每个单元带来源角色、范围、hash及证据等级。
4. 长输入按完整组分块，默认最多16块、4轮合并，单个不可拆组超过预算时明确失败。
5. 校验schema、来源、引用、必留项、当前版本、严格55%目标及完整请求预算，拒绝摘要tool calls或超大正文。
6. 以完整原历史水位／hash、前缀hash、profile／model／protocol、适配器与上下文版本做CAS，原子写入ContextView和CompactionRecord。
7. 成功后重新构建下一次请求，应用当前模式、记忆版本与工具集合。CAS冲突和摘要错误保留原历史及此前有效检查点。

摘要默认继承执行profile的输出目标，context.summary_max_output_tokens显式值优先，并可高于profile目标。每个分块或合并请求独立按实际wire剩余空间收缩O，summary_budget的输入容量沿用稳定B。生成结果继续通过64KiB结构化正文、schema、来源／引用和必需事实校验，输出截断或校验失败保持原检查点。每次Compact最多32次实际模型调用、600秒操作时间，实际调用、允许的传输重试和活跃时间共用Run剩余预算。单次摘要请求沿当前模型的连接、idle及总超时配置，最多2次允许的传输重试。人工等待沿共享活跃预算暂停规则处理。

取消后停止调度并收敛已开始的调用，摘要调用期间持久事务保持短生命周期。协调器校验源版本后才返回可提交检查点。摘要字段包含objective、constraints、decisions、completed、remaining、uncertainties、required_ids和references，事实携带source IDs、evidence等级及状态。

## 三协议原子组

| 协议 | 分组与切点 |
| --- | --- |
| Chat Completions | assistant调用及全部对应tool结果闭合，调用顺序与call ID保持一致 |
| Responses | typed items、对应镜像和opaque reasoning依照适配器规则分组，保留所需原始后缀 |
| Anthropic Messages | tool_use／tool_result及适用thinking／signature／redacted blocks整组处理 |

先验证原始历史及镜像配对，再验证切点、保留组和重建请求。无法安全裁剪的组保持必留。供应商原生Compact或context editing作为后续协议策略单独验收。

## 缓存与恢复

静态系统规则、规范工具顺序和条件式续接规则保持稳定前缀。动态 `checkpoint_selected_by_host` 表示本次请求是否选择已提交检查点，续接段按该标志适用。时间、模式、任务、记忆、Skill状态和输出水位进入动态来源段。

摘要与保留来源带明确的历史数据envelope。固定前缀、续接规则及动态标志全部参与计数，压缩前后使用同一静态前缀。Anthropic cache_control及各协议现有缓存能力继续沿provider适配器处理。

CompactionRecord保存源范围／hash、保留组、失效组manifest、提示词与适配器版本、用量及目标／实际投影统计。恢复验证当前profile、项目／Skill／记忆版本和原始历史，版本不兼容的视图需要重新构建。会话恢复采用本次权限配置。

## 记忆遗忘与上下文

默认全局索引为 `~/.saya/MEMORY.md`。路由、选中事实、来源与revision进入有界Memory分项。当前记忆和遗忘版本参与检查点兼容性及缓存指纹。[记忆工具与管理](memory.md)

宿主对原生记忆调用及实际使用Memory的模型请求保存durable来源证明。遗忘、来源撤回或策略变化后，ContextBuilder只根据这些证明将失效的完整闭合组替换为明确历史数据envelope，保留有界机械观察。原工具参数、正文和ProviderItem随该组退出模型投影，旧摘要也需通过当前记忆版本校验。

Compact只读取这一经验证的投影，来源到投影ordinal映射参与切片。用户原始输入与当前宿主控制保持各自权威，摘要中的权限或记忆声明继续沿其来源处理。

## 验收

DEV-076验证管道排空、跨块秘密、配额、缺口、查询、权限、取消、发布及GC。DEV-062／063／077验证完整序列化计数、单用户长Run、协议闭合、固定前缀、80／55水位、有限摘要／重试、CAS、恢复、质量和缓存。DEV-085／089联合验证记忆来源、更正、遗忘、旧Session／摘要、凭据轮换及新请求实际字节。

本机受控模型、实际模型质量、PTY和四平台原生隔离分别记录各自证据。Plugin／MCP已接入正常Run组装，HTTP broker与原生后端联合验收仍按当前修复和实际runner结果维护。[实施任务](post-mvp-plan.md)

## DEV-064 文件检查点组件

2026-11-10，已实现选定UTF-8文件的捕获、比较、接受与受控恢复，支持Git及普通目录，保存workspace／Session来源、文件版本和完整性。准备阶段捕获批准所针对的字节及观察，发布再次核对路径、目录句柄和内容版本，恢复按文件原子替换并记录实际部分结果。外部写入不参与内部锁，多文件恢复的部分完成和unknown沿结果明确保留。17项组件回归及全workspace全目标严格Clippy通过，正常模型工具／界面与平台证据继续按[N3验收](n3-validation.md)维护。

## DEV-062 协议组与来源投影

ContextBuilder支持单用户长Run的闭合前缀，覆盖最新用户所在范围时将原始用户消息逐字重放，并保留显式pin及未完成协议组。源范围到请求投影的映射保留输出引用、原文与宿主证明的失效组身份，已退休内容仅保留经验证的机械状态。25项组件、三协议真实wire计数与预算拒绝回归及全workspace全目标严格Clippy通过，正常Compact与记忆联合矩阵继续按N3记录维护。

## DEV-063 Compact协调与持久化组件

结构化Compact提供有限分块／合并、来源引用与必留项验证、共享模型／活跃预算、严格目标水位及单一并发协调。摘要与指针在同一存储事务按Session、原始水位、profile、版本及前指针CAS发布，原始历史保持完整。存储同时支持无正文的MemoryExposure来源绑定，供后续退休投影验证。19项协调器回归、三协议执行／摘要实际wire计数与Anthropic静态cache_control回归及严格全目标Clippy通过。自动／手动入口和平台联合证据继续按N3记录维护。

## DEV-076 捕获派发与投影修复

命令派发前在阻塞worker完成stdout／stderr捕获预留，再次检查取消后才创建子进程。管道排空完成后释放缓冲锁，再由阻塞worker完成密封，保留失败捕获的明确缺口。长结果投影保存真实exit_code、超时、取消及截断状态，模型可凭宿主引用回取正文。13项exec与5项正常OutputStore回归通过，发布故障仍保留已观察结果且不重放副作用，完整本机门禁日志为 `target/n3/integration-workspace-gate.log`。
