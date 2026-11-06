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

这些文件由 `include_str!` 嵌入 core crate，无运行时外部文件依赖；修改后须重新构建。不提供本轮未设计的用户自定义覆盖、persona 注册表或 provider 缓存开关。

## 实际装配与接入

[装配器](../crates/saya-core/src/prompt.rs)按以下顺序构造执行请求：

1. 固定版本、稳定核心、工具纪律。
2. 恰好一个工作模式段与一个权限说明段。
3. 实际注册后台控制工具时才追加后台段。
4. 从 `ToolDefinition` 注册表生成排序后的可用名称清单；名称需唯一且为最长 64 字节的英文标识符，可含数字、下划线和连字符，首字符需为字母。

工具说明与 schema 继续通过 `ModelRequest.tools` 传入，不提升为系统指令；用户任务与项目材料保留在历史中。装配器不直接拼接会话文本、来源 prompt、项目文档或任意自定义字符串。它返回段名、版本和 SHA-256 文本指纹以便离线审阅；指纹只标识文本，不证明模型遵守程度，也尚未接入持久化记录。

[可复用 core 循环](../crates/saya-core/src/runtime.rs)及假模型原型已使用完整装配器，取代每模式的一句指令。每次请求都按生效模式、权限与过滤后的工具重新装配；切换后不残留旧模式段。稳定核心保持相同前缀，但没有虚构 provider 缓存命中保证，也没有引入某产品专用的动态分隔标记。

已知运行事实由 `runtime_context` 编成独立 JSON 数据项放在请求历史开头，内容仅为工作区、profile、model、协议、实际工作模式与权限模式；不混入高优先级 instructions，不包含凭据、原始配置或整份历史，不探测和猜测 Git／平台状态，也不持续向会话存储追加此临时快照。模型名称只是配置值，不是对真实上游身份的认证。项目 AGENTS.md 的查找、哈希、刷新与完整提示词组装仍属 DEV-021 的后续接线。

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
| 自动压缩、scratchpad、Hooks、MCP、记忆和会话外任务说明 | 当前不启用 | 不许诺未实现能力；只有后台控制按当前注册表条件追加 |
| 直接调用后台 shell、内部任务 ID 与 provider tool ID 混用的风险 | 新增 Saya 专属后台生命周期段 | 启动不等于完成，查询后验证内层结果，PLAN 前与结束时清理，避免协议重复结果 |

## 相比 session 中的 DSH 分析与 harness-kit

DSH 的具体实现信息在这里仅来自 session 第 842 行等记录，本轮未重新检查 DSH 安装包。`harness-kit` 是该 session 生成的移植草案，不能当成提取原文或官方规范。

| 来源中的部分 | Saya 的处理 |
| --- | --- |
| 可组合、有序的 prompt 分层 | 保留分层思路；采用轻量、显式顺序、版本化 Markdown + Rust 纯函数，不预建插件注册表 |
| persona prefix／suffix、每 Agent shadow、complete 覆盖和任意变量插值 | 移除；当前只允许枚举与校验过的工具名进入执行指令，避免来源文本被提升为系统策略 |
| in-history 追加高优先级 system 更新与 provider 缓存管理 | 不照搬；使用现有 `ModelRequest.instructions` 接口，动态事实是独立数据，历史不被重写 |
| sandbox × approval 的两个旋钮 | 不替换 Saya 权限契约；保留四工作模式与三权限模式正交，并明确本地命令没有 OS 沙箱 |
| risk／decision 分类器协议 | 改为 Saya 已有的 allow／ask／deny 与 reason，不宣称共享相同 classifier |
| 默认 global／project AGENTS、Skills、scratchpad、自动记忆注入 | D-033 后纳入用户级／工作区约定指令与 Skill 元数据发现，DEV-021 待接线；不引入 scratchpad 或自动记忆 |
| 移植草案的计划文件、exit_plan、固定中文输出与来源工具占位符 | 移除或按 Saya 重写；所有 runtime prompt 为英文，沟通语言跟随用户而不固定中文 |
| 将所有缺乏授权结果统称 denied 或建议改规则重试 | 区分拒绝、模式禁止、许可失效、需要确认、存储故障和未知结果，不指示 Agent 降权规避 |
| 来源 session 外层的人设、无条件服从／放行、工具和环境指令 | 完全不进入 Saya prompt |

## 来源追踪

[来源清单](prompt-sources.json)保存用户提供的 session 和实际使用的源文件路径、字节数及 SHA-256。源目录在构建和运行时均不读取；不导入原始二进制、完整日志、来源系统人设或凭据。Saya 运行时文案以自身契约重写，没有把提取包整段复制进项目。

主要材料是提取的 classifier、PLAN、auto、谨慎执行和输出效率片段，以及 `harness-kit` 的核心、会话与拒绝恢复草案。session 第 111／157 行等保留了提取内容，第 655 行是提取结论，第 755 行是移植草案说明，第 842 行是 DSH 比较；这些记录的版本与完整性声明不能仅凭本次文档处理升格为独立验证事实。

## 预览与验证

可离线预览实际装配文本；示例程序只构造 JSON，不运行工具或调用模型：

```sh
cargo run -p saya-core --example prompt_preview -- work auto read_file search_text background_start background_status background_cancel
cargo run -p saya-core --example prompt_preview -- plan bypass read_file search_text
cargo run -p saya-core --example prompt_preview -- --review
```

[装配测试](../crates/saya-core/tests/prompts.rs)验证 12 种组合、后台能力条件、稳定前缀／指纹、英文与语言偏好、工具名注入拒绝、工具描述不升权和运行事实隔离；假循环验证实际模式切换与请求构造。独立核查继续用可控模型替身验证协议及错误，不依赖真实 API。

[审核场景集](../crates/saya-core/tests/fixtures/prompts/review_cases.jsonl)含 30 项人工编写的期望：常规检索、构建、字面分号、提示词作为数据、强推、破坏性清理、共享系统、上传、凭据、持久化和规避。单测只检查场景数据格式与覆盖完整，不把期望值喂给替身后冒充分类准确率。真实评测需对每个输入调用选定 reviewer，记录模型／profile、prompt 指纹、实际 decision、解析失败／人工兜底和误放行／误拦截；本轮尚无这类测量。

三协议适配已有 core 级受控验证，CLI 有只读开发预览；用户级／项目指令与 Skill 发现仍待 DEV-021。缓存命中、跨模型泛化、提示注入抵抗和安全分类效果均不能由字符串测试证明。
