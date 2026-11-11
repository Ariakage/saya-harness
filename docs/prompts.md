# Saya 提示词定制与来源对照

本版 prompt 全部使用英文，不固定用户沟通语言：遵循用户明确偏好，否则跟随当前请求的语言。产品说明和本文件使用中文。代码版本标识为 `saya-prompts-v1`；实现状态见[开发进度](progress.md)。

各段正文的当前可阅读快照见[当前提示词](prompt/README.md)。运行时仍使用下表所列的 core crate 内源文件，修改时应同步快照。

## 可直接审阅的文件

| 层 | 文件 | 内容 |
| --- | --- | --- |
| 稳定核心 | [core.md](../crates/saya-core/prompts/v1/core.md) | 任务授权、材料信任、证据、范围与交付 |
| 工具纪律 | [tools.md](../crates/saya-core/prompts/v1/tools.md) | schema、版本冲突、直接 argv、错误与确认语义 |
| PLAN | [plan.md](../crates/saya-core/prompts/v1/modes/plan.md) | 只读调查与对话内计划；没有文件写入例外 |
| BUILD | [build.md](../crates/saya-core/prompts/v1/modes/build.md) | 端到端实现、验证、产物与交付 |
| CODE | [code.md](../crates/saya-core/prompts/v1/modes/code.md) | 聚焦代码修改、诊断、回归验证 |
| WORK | [work.md](../crates/saya-core/prompts/v1/modes/work.md) | 本地通用任务、原件保留、结果核对 |
| auto | [auto.md](../crates/saya-core/prompts/v1/permissions/auto.md) | 常规推进、规则优先、独立核查和人工兜底 |
| manual | [manual.md](../crates/saya-core/prompts/v1/permissions/manual.md) | 每次工具调用确认和精确命令预授权 |
| bypass | [bypass.md](../crates/saya-core/prompts/v1/permissions/bypass.md) | 说明跳过项及仍有效的硬限制 |
| 后台任务 | [background.md](../crates/saya-core/prompts/v1/background.md) | 启动回执、依赖与冲突、查询、取消及清理 |
| 独立审核器 | [review.md](../crates/saya-core/prompts/v1/review.md) | 数据边界、完整效果、allow／ask／deny 和输出契约 |
| 能力片段 | [capabilities.md](../crates/saya-core/prompts/v1/capabilities.md) | 输出检索、记忆写入、Skill／扩展、任务清单与工作区快照的按需注入 |

这些文件由 `include_str!` 嵌入 core crate，无运行时外部文件依赖；修改后须重新构建。不提供用户自定义 prompt 覆盖或 persona 注册表。Anthropic 缓存断点通过 CLI 参数显式开启，见下文。

## 实际装配与接入

[装配器](../crates/saya-core/src/prompt.rs)按以下顺序构造执行请求：

1. 固定版本、稳定核心、工具纪律。
2. 恰好一个工作模式段与一个权限说明段。
3. 实际注册后台控制工具时才追加后台段；按当前工具注册情况选择输出检索、记忆写入／遗忘、Skill／扩展、任务清单与工作区快照段。未注册能力不注入其指导。
4. 记忆读取能力实际可用时追加 Memory Context；关闭读取或仅有写入能力时不注入。
5. 从 `ToolDefinition` 注册表生成排序后的可用名称清单；名称需唯一且为最长 64 字节的英文标识符，可含数字、下划线和连字符，首字符需为字母。

工具说明与 schema 继续通过 `ModelRequest.tools` 传入，不提升为系统指令；内置描述明确补丁版本、单文件修改、读取分页和直接 argv 等调用语义。用户任务、项目材料与显式提供的偏好／记忆保留在历史中。静态装配器不直接拼接来源 prompt、项目文档或任意自定义字符串。它返回段名、版本和 SHA-256 文本指纹以便离线审阅；指纹只标识静态文本，不证明模型遵守程度，也尚未接入持久化记录。

[可复用 core 循环](../crates/saya-core/src/runtime.rs)及假模型原型已使用完整装配器，取代每模式的一句指令。每次请求都按生效模式、权限与过滤后的工具重新装配；切换后不残留旧模式段。稳定核心保持相同前缀，但没有虚构 provider 缓存命中保证，也没有引入某产品专用的动态分隔标记。

每次执行模型请求在静态提示词之后加入动态系统段，但只放入相同有效配置下可复用的事实：操作系统与架构、模型配置引用、实际工作／权限模式，以及项目上下文和显式偏好／记忆是否提供。Runtime 创建时的精确时间快照供后续轮次复用，并与工作区一起放在 `saya_runtime_context_v1` 历史数据项；执行环境实例身份、策略摘要和能力放在其后的环境数据项。这些运行实例数据位于 provider 工具 schema 之后，不再截断不同工作区之间的系统／工具共享前缀。宿主 checkpoint 标志仍位于 `Dynamic request context` 之前。所有字符串序列化为 JSON 并标记为数据，不包含配置凭据或完整历史；临时快照不持久化。用户级和工作区指令、Skill 元数据作为独立 `saya_project_context_v1` 数据项放在模型历史中，来源、哈希、范围与刷新见[项目上下文](project-context.md)。显式偏好和记忆放在 `saya_personal_context_v1` 数据项，不自动发现；它们不是系统授权，模型仍可能受内容影响。

上下文管理未选中历史checkpoint时只注入短续接说明；宿主选中已提交的上下文checkpoint时才追加完整续接规则，并在动态系统段设置 `checkpoint_selected_by_host`。完整摘要与来源引用仍是历史数据，不恢复旧授权。

## 前缀缓存

静态提示词先于动态系统段，动态时钟不会改变静态文本本身。Anthropic Messages 通过 `--anthropic-prompt-cache` 显式启用时，将静态文本放在带 `cache_control: {type: ephemeral}` 的系统块中，动态内容放在不带标记的下一块；其他兼容端点默认不发送该字段。安全审核请求不带执行上下文或该缓存标记。Anthropic 缓存以工具、系统、消息的顺序构成前缀，工具定义或模式变化仍可能使缓存失效；阈值和缓存创建费用因模型而异。参见 [Anthropic 缓存文档](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)。

启用 Anthropic 缓存时，同一模型实例内相同静态前缀和工具定义的并发冷请求会等待首个响应事件；失败、取消或丢弃流时释放等待者，四分钟后重新尝试。不同模型实例和进程不共享协调状态，超过内部键数量上限时可能不协调，因此不保证杜绝重复写入。该协调不复用模型输出，不保证服务端确实创建了缓存。

OpenAI 兼容协议保留相同的静态字节前缀并依赖服务端自动缓存；DeepSeek 也自行处理缓存，不额外发送 `cache_control` 或其不支持的 `prompt_cache_key`。系统段只保留相同配置下稳定的内容；运行时间、工作区和环境实例数据位于工具 schema 之后的历史项，因此不同工作区也能复用系统与工具前缀。新增工具结果和用户任务仍是首次出现的未缓存输入，不能被旧前缀复用。参见 [OpenAI 缓存文档](https://developers.openai.com/api/docs/guides/prompt-caching)、[DeepSeek 上下文缓存](https://api-docs.deepseek.com/guides/kv_cache/)和 [Responses 兼容说明](https://api-docs.deepseek.com/guides/responses_api/)。

`Usage.cached_input_tokens` 以可选字段记录 provider 实际报告的缓存输入量，未上报时保持未知且维持既有 JSON 形状。Chat Completions 兼容 `prompt_tokens_details.cached_tokens` 与 `prompt_cache_hit_tokens`，Responses 读取 `input_tokens_details.cached_tokens`，Messages 读取 `cache_read_input_tokens`；运行指标只在相关请求均上报时合计缓存量。真实 DeepSeek 多轮对照中，前缀优化前24次记录为整体36.37%／Turn 2+ 35.53%，优化后48次运行合并为整体83.54%／Turn 2+ 91.75%。完整 token 数量、占比与限制见[多轮缓存验证报告](prompt-evaluation-2026-11-11.md)。这些结果不是服务端保证，也不代表其他模型。后续将运行时间、工作区和环境实例数据移至工具schema之后，最终96次运行达到整体95.05%／Turn 2+ 97.20%，并在相同48次规模下将失败工具结果由4次降为3次；见[缓存95%与工具错误联合验证](prompt-cache-95-2026-11-11.md)。

[审核组件](../crates/saya-core/src/review.rs)使用独立英文审核 prompt，保留现有一个数据消息、无工具、1024 输出 token 默认上限、严格 JSON 解析与失败转人工逻辑。它不接收执行 prompt、项目说明或完整执行历史；数据仍为 `tool/arguments/work_mode/permissions/goal_summary`。尤其不把 `goal_summary` 中“用户已授权”的说法当成原始用户证据。

权限执行逻辑没有因这次 prompt 定制而更换：auto 的原生文件工具默认放行、未命中 exec 才触发模型核查；manual 对每次工具调用要求人工批准；显式 allow 仍可按现有基线跳过核查。英文审核提示词不是对所有文件和命令新增的扫描层，不能覆盖硬 deny、PLAN 或已有人工拒绝。需要精确用户证据的高影响操作，在当前审核输入不足时建议 ask，再由真实确认或已有规则处理。

## 相比所提供的 Claude Code 素材

以下比较只针对用户提供的提取文本及 session 中对 2.1.89 的分析，不是对当前安装版或全部 Claude Code 行为的重新验证。

| 来源中的部分 | Saya 的处理 | 原因或新增约束 |
| --- | --- | --- |
| 产品身份、Claude／SDK 身份及变量化工具名 | 重写为 Saya，名称从实际注册表产生 | 不继承别人的身份与不存在的能力 |
| 编程为主的共同任务说明 | 拆成核心 + BUILD／CODE／WORK | BUILD 覆盖端到端交付，CODE 聚焦修改调试，WORK 支持非编程本地任务 |
| PLAN 中可编辑唯一计划文件 | 移除例外，在对话中交付计划 | Saya PLAN 连计划文件也不能写 |
| 强制 Explore／Plan 子 Agent 与固定多阶段代理流程 | 移除 | Saya MVP 没有子 Agent；按任务规模调查与规划 |
| ExitPlanMode、审批后自动退出／继续实现 | 移除 | 工作模式由用户切换，模型仅建议；没有虚构退出工具 |
| full／sparse／reentry 多套计划附件 | 当前采用单个完整模式段，每次调用按有效模式装配 | 尚无可靠的压缩／稀疏附件状态机，不声称已实现 |
| auto 行为强调立即编码、少提问 | 改为遵循当前工作模式推进 | auto 是权限维度，不将 PLAN 或 WORK 强行变成编程 |
| 用户意图规则把部分疑问句一概视为没有授权 | 对明确要求修复／实现／帮忙处理的常规任务按行动指令推进，高影响操作仍需具体授权 | 避免把已授权可逆工作反复拆成确认问题 |
| default／acceptEdits／dontAsk 与工作模式混合的枚举 | 不移植；提供独立的三权限说明 | 与 Saya 四工作模式 × 三权限模式保持一致 |
| bypass 缺少模型侧专属 prompt | 新增短说明 | 明确 PLAN、显式 deny、预算和取消依旧有效；不让模型误解为无限任务范围 |
| classify_result、shouldBlock、thinking／XML 两阶段分类 | 移除，改为一个严格 JSON 对象 | 对齐现有 `allow/ask/deny + reason`；不索取私有推理、不注册审核工具 |
| 完整 transcript 核查及用户意图规则 | 保留意图与动作分离，按现有可见输入重写 | 只有操作和摘要时不声称看过原始用户授权、脚本、钩子或页面 |
| 默认允许，再按 BLOCK 与 ALLOW 例外判定 | 改为可见证据支持 allow，风险／未知转 ask，明确规避或泄露转 deny | 对齐三态输出及没有完整 transcript 的现实输入 |
| 大量设施专属 BLOCK／ALLOW 项和强制豁免 | 改为按效果检查与三个结果 | 不硬编码 main 分支绝对拒绝，不把工具链安装、工作分支推送等一概免检 |
| 进入 auto 时摘除／恢复过宽 allow 规则 | 不移植 | 会改变已定案的规则优先级，需要独立产品决策；prompt 不暗改权限配置 |
| 临时核查故障允许稍后重试的文案 | 按 Saya 改为有限内部重试后转人工；无交互则 needs_confirmation | 不让主 Agent 反复提交来刷出放行 |
| 自动压缩、scratchpad、Hooks、MCP、记忆和会话外任务说明 | 按实际启用条件分别注入 | Compact使用独立无工具请求；Hooks不提升为模型指令；MCP与Plugin工具来自实际注册表；Memory使用独立低信任上下文。离线预览用具名fixture展示完整装配，不表示每次运行都启用全部能力 |
| 直接调用后台 shell、内部任务 ID 与 provider tool ID 混用的风险 | 新增 Saya 专属后台生命周期段 | 启动不等于完成，查询后验证内层结果，PLAN 前与结束时清理，避免协议重复结果 |

## 相比 session 中的 DSH 分析与 harness-kit

DSH 的具体实现信息在这里仅来自 session 第 842 行等记录，本轮未重新检查 DSH 安装包。`harness-kit` 是该 session 生成的移植草案，不能当成提取原文或官方规范。

| 来源中的部分 | Saya 的处理 |
| --- | --- |
| 可组合、有序的 prompt 分层 | 保留分层思路；采用轻量、显式顺序、版本化 Markdown + Rust 纯函数，不预建插件注册表 |
| persona prefix／suffix、每 Agent shadow、complete 覆盖和任意变量插值 | 不引入自定义覆盖；系统段中的运行事实以标记数据的 JSON 注入，来源文件与用户内容保留为低信任数据 |
| in-history 追加高优先级 system 更新与 provider 缓存管理 | 不重写历史；静态指令与动态系统段分开，Anthropic 可选显式缓存断点，其他兼容协议保持自动缓存前缀 |
| sandbox × approval 的两个旋钮 | 不替换 Saya 权限契约；保留四工作模式与三权限模式正交，并明确本地命令没有 OS 沙箱 |
| risk／decision 分类器协议 | 改为 Saya 已有的 allow／ask／deny 与 reason，不宣称共享相同 classifier |
| 默认 global／project AGENTS、Skills、scratchpad、自动记忆注入 | D-033 的用户级／工作区约定指令与 Skill 元数据发现已接入；Skill正文仅在显式激活后进入低信任上下文。全局记忆读取、自动注入、维护和遗忘按全局记忆设计运行 |
| 移植草案的计划文件、exit_plan、固定中文输出与来源工具占位符 | 移除或按 Saya 重写；所有 runtime prompt 为英文，沟通语言跟随用户而不固定中文 |
| 将所有缺乏授权结果统称 denied 或建议改规则重试 | 区分拒绝、模式禁止、许可失效、需要确认、存储故障和未知结果，不指示 Agent 降权规避 |
| 来源 session 外层的人设、无条件服从／放行、工具和环境指令 | 完全不进入 Saya prompt |

## 来源追踪

[来源清单](prompt-sources.json)保存用户提供的 session 与源文件的来源标识、来源内相对路径、字节数及 SHA-256。核查者可在本机将来源标识映射到原始材料目录，按相对路径和摘要复核。清单保留历史提取证据，Saya 运行时文案按自身契约重写。

主要材料是提取的 classifier、PLAN、auto、谨慎执行和输出效率片段，以及 `harness-kit` 的核心、会话与拒绝恢复草案。session 第 111／157 行等保留了提取内容，第 655 行是提取结论，第 755 行是移植草案说明，第 842 行是 DSH 比较；这些记录的版本与完整性声明不能仅凭本次文档处理升格为独立验证事实。

## 预览与验证

可离线预览完整 `ModelRequest`；示例程序只构造 JSON，不运行工具、MCP Server、Plugin、Skill脚本或调用模型：

```sh
just prompt code
just prompt build
just prompt compact
just prompt memory
just prompt review
just prompt all
just prompt all | jq '.requests.code.request.combined_system'
```

可用类型为 `work`、`plan`、`code`、`build`、`compact`、`memory`、`review` 和 `all`；省略类型等同 `all`。Code、Build、Work与Plan场景包含静态core／tools／mode／permission段、条件后台段、动态宿主与运行事实、项目上下文、显式偏好、Memory上下文、已激活Skill正文、当前任务，以及生产运行时生成的内置、Skill、Memory与工作区checkpoint工具schema。外部MCP与Plugin只使用明确的离线fixture，Compact场景使用实际Compact instructions与输出schema；Review场景使用实际独立审核instructions和样例操作数据。

输出中的 `instructions`、`dynamic_instructions`、`history` 与 `tools` 对应运行时 `ModelRequest` 字段，`combined_system` 是组合后的系统文本，`instructions_fingerprint` 与 `system_fingerprint` 分别标识静态文本和本次组合文本，`tool_layers` 按builtin／Skill／Memory／MCP／Plugin归类工具名。预览使用与生产相同的 `AgentRuntime::model_tool_definitions`，包含工作区checkpoint和按模式过滤的工具定义；Memory、Skill和checkpoint后端位于临时目录，预览不会在工作区创建这些存储。执行场景再经过ContextBuilder，未选中checkpoint时显示短续接说明。项目上下文从当前工作区和HOME发现；Skill激活、Memory投影、MCP和Plugin条目是明确的离线fixture。`fixture_notice` 会保留在JSON中，不能把fixture工具名或已激活Skill误认为当前运行配置。

预览输出包含实际工作区路径、项目指令和发现的Skill元数据。它只应在本地审阅，不应直接提交或粘贴到公开位置；需要分享时先人工脱敏。

[装配测试](../crates/saya-core/tests/prompts.rs)验证 12 种组合、后台与新增能力片段的可见性、稳定前缀／指纹、英文与语言偏好、工具名注入拒绝、工具描述不升权和运行事实隔离；缓存前缀的跨工作区稳定性、运行时间数据位置、工具 schema、补丁错误提示及三协议缓存用量另有定向回归。假循环验证实际模式切换与请求构造。独立核查继续用可控模型替身验证协议及错误，不依赖真实 API。

[真实提示词场景脚本](../scripts/prompt_eval.py)提供显式启用的DeepSeek基线／候选对照，覆盖PLAN注入下的只读调查、带版本的已有文件补丁、Skill选择、通用文件交付、多hunk补丁、范围注入、CSV聚合、长日志检索、无序区间合并和事件状态聚合。`--repeat` 与 `--jobs` 支持独立多轮及并发，`--diagnostics` 保存夹具衍生的失败调用，`--compare` 离线比较报告。每个场景在独立临时工作区运行并检查实际文件、工具调用和终态；测试配置拒绝exec，因此不会执行模型生成的命令。报告记录程序SHA、模型、窗口、场景错误、请求次数、逐轮输入／缓存输入／输出token、整体与Turn 1／Turn 2+命中率及token占比；旧报告未采集缓存字段时显示未知而不是0。运行方式见[开发指南](development.md)，实际多轮结果见[DeepSeek对比验证报告](prompt-evaluation-2026-11-11.md)；最新10场景能力对照见[提示词与模型任务能力联合验证](prompt-capability-2026-11-11.md)。

[审核场景集](../crates/saya-core/tests/fixtures/prompts/review_cases.jsonl)含 30 项人工编写的期望：常规检索、构建、字面分号、提示词作为数据、强推、破坏性清理、共享系统、上传、凭据、持久化和规避。单测只检查场景数据格式与覆盖完整，不把期望值喂给替身后冒充分类准确率。真实评测需对每个输入调用选定 reviewer，记录模型／profile、prompt 指纹、实际 decision、解析失败／人工兜底和误放行／误拦截；本轮尚无这类测量。

三协议适配、持久会话、受权限控制的工具以及用户级／项目指令与 Skill 发现已接入 CLI 和共享应用层。当前默认 TUI 使用同一运行时，实际验证范围见[开发进度](progress.md)。跨模型泛化、其他服务的缓存命中、提示注入抵抗和安全分类效果仍需要对应评测证据。

## 会话任务清单提示

开发线工具纪律新增英文Session task tracking段。实际暴露 `todo_write` 时，模型应在多步骤任务前建立简明清单，保留稳定ID，在范围或进度变化时提交完整列表，同时保持至多一项进行中。成功保存的列表通过 `runtime_context.todos` 注入后续执行请求，PLAN提示明确允许修改会话任务元数据。

提示词要求基于实际验收结果标记完成，工具参数和条目内容按任务数据处理。清单更新的权限、持久化和失败处理由[运行时工具](runtime.md#会话任务清单)执行。
