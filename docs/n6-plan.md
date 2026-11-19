# N6 目标与提案规划（工具发现、步骤验证与程序化编排）

**状态：建议／规划中（待开始，未实现）· 更新于 2026-11-19。本文整理并收敛 S1、S2、S3、S5 四项新增目标与能力提案，拆分为 `DEV-090` 至 `DEV-096`（归属阶段 N6，新增能力 `F-21` 并扩展 `F-02`、`F-20`）。本文只记录提案基线、当前 HEAD 代码核验事实与最小可行版本约束；进入具体开发时若涉及公共契约、默认策略或安全边界调整，须经项目负责人确认并记入[决策记录](decisions.md)。**

> 进度与状态跟踪入口见[开发进度](progress.md)，任务编号与依赖关系见[后续开发计划](post-mvp-plan.md)，能力定位见[能力路线图](capability-roadmap.md)。

## 总览与实施排序

| 提案 | 名称 | 优先级 | 轨道 | 价值排序 | 成本 | 归属能力 | 对应任务 | 当前状态 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **S2** | MCP／大工具目录的按需发现（`search + describe`） | **P1** | 实现主线 | 第 2 | 中 | `F-02`、`F-06` | `DEV-090` | 待开始（在已有 MCP 上做延迟加载） |
| **S3** | 给 Goal 步骤绑定宿主验证器（不重做 Goal） | **P1** | 可靠性线 | 第 3 | 中 | `F-20` | `DEV-091`、`DEV-092` | 待开始（不阻断按需发现，可与 S2 并行） |
| **S5** | 在工具 schema 里加少量真实调用样例 | **P2** | 可并行附带 | 第 5 | 低 | `F-02` | `DEV-093` | 待开始（与 S2 互补） |
| **S1** | 实验性程序化工具编排（只读 Code Mode 风格 REPL） | **P2** | 实现主线（S2 之后） | 第 1 | 高 | **新增 `F-21`** | `DEV-094`、`DEV-095` | 待开始（默认关闭，位于 S2 之后） |
| **—** | N6 跨能力联合验收与双语增量说明 | — | 联合验收 | — | — | `F-02`、`F-20`、`F-21` | `DEV-096` | 待开始 |

---

## S2 · MCP／大工具目录的按需发现（`DEV-090`）

- **状态／成本／轨道**：建议（待开始） · 成本中 · P1 实现主线（价值第 2）
- **具体机制**：
  - 默认不把全部 MCP／大型外部工具的完整 schema 放进每轮上下文，默认提示词仅保留核心内置读写与 `exec` 工具。
  - 将 MCP／大型外部工具设为默认延迟加载（`defer_loading`），提供 `tool_search` 与 `tool_describe` 入口，由模型先检索候选、再按需加载所需工具定义。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - `soyo-core` 已具备 stdio／Streamable HTTP MCP、Skill 发现与统一工具注册表（`F-02`／`F-03`）。
  - 本次已逐行核对装配路径，确认此前“是否每轮全量注入 `ModelRequest.tools`”的待核实项：
    1. [crates/soyo-core/src/registry.rs](../crates/soyo-core/src/registry.rs#L178-L187) 中 `ToolRegistry::definitions` 遍历 `self.entries.values()`，仅按 `PLAN`（`allowed_in_plan()`）和持久性（`!requires_durability()`）过滤。
    2. [crates/soyo-core/src/dispatch.rs](../crates/soyo-core/src/dispatch.rs#L458-L481) 中 `ToolDispatcher::definitions` 仅追加 `read_only` 模式对 Browser 工具的过滤，随后返回其余全部已注册工具（含全部已发现的 MCP 工具）。
    3. [crates/soyo-core/src/runtime/mod.rs](../crates/soyo-core/src/runtime/mod.rs#L914-L934)（`AgentRuntime::definitions`）与 [runtime/mod.rs](../crates/soyo-core/src/runtime/mod.rs#L1086-L1156)（`prepare_request`）合并记忆／Skill／检查点工具后，每轮将完整 `Vec<ToolDefinition>` 写入 `ModelRequest.tools`；三类协议适配器（[provider/chat.rs](../crates/soyo-core/src/provider/chat.rs#L83-L94)、[provider/responses.rs](../crates/soyo-core/src/provider/responses.rs#L67-L78)、[provider/anthropic.rs](../crates/soyo-core/src/provider/anthropic.rs#L160-L175)）全量序列化发出。
  - 全仓代码搜索未见 `defer_loading`、`tool_search` 或 `tool_describe`，且 [MCP 文档](mcp.md) 仍说明模型看到的是完整注册表快照。
- **还缺什么与设计注意点**：
  - 缺少注册表的延迟标记、按需检索与描述接口，以及动态扩展工具列表时对 Prompt Caching 前缀稳定性（如 [provider/anthropic.rs](../crates/soyo-core/src/provider/anthropic.rs#L130) 的 `cached_tools` 断点）的明确约束：核心工具须保持稳定前缀顺序，按需展开的外部工具应避免破坏既定缓存前缀约定，并保留目录代次与权限／审批身份绑定。
- **建议的最小版本（`DEV-090`）**：
  - 内置读写与 `exec`（含核心只读工具）保持常驻；MCP 工具默认 `defer_loading`。
  - 新增宿主可控的 `tool_search` 与 `tool_describe`，仅返回脱敏后的名称、简要摘要与按需展开的 schema；不绕过 `PLAN` 过滤、目录代次校验或 `KnownSecrets` 检查。
- **收益与成本**：多 MCP 服务器时降低固定工具 schema 上下文占用并减少相似工具名误选；主要涉及装配、检索与缓存前缀控制，不必新建脚本沙箱。
- **主要风险**：搜索漏工具会导致模型静默降级或找不到正确工具；若动态修改 `ModelRequest.tools` 未固定前缀顺序，可能影响多轮缓存命中。
- **验收方式**：
  - 夹具挂载 **20+ 个虚假 MCP 工具**，断言首轮提示里只包含核心内置工具（含 `tool_search`/`tool_describe`）加已命中／展开项，不出现全量 20+ MCP schema，且任务仍能选对并正确调用目标工具。
  - Anthropic 2025-11-24 官方文章中的 Tool Search 数字属于当年内部测试数据，不作为本仓库基准或验收指标。
- **参考与证据**：
  - [Anthropic: Advanced Tool Use (2025-11-24)](https://www.anthropic.com/research/advanced-tool-use)
  - [docs/mcp.md](mcp.md)
  - [crates/soyo-core/src/boundary.rs](../crates/soyo-core/src/boundary.rs)

---

## S3 · 给 Goal 步骤绑定宿主验证器（`DEV-091`、`DEV-092`）

- **状态／成本／轨道**：建议（待开始） · 成本中 · P1 可靠性线（价值第 3，不阻断按需发现）
- **具体机制**：
  - 在不重做现有 N4 Goal 架构的前提下，为步骤引入宿主命令验证器：步骤是否完成由宿主执行验证命令（测试、lint、自定义检查）的退出码和有界输出决定，而不是模型自称完成。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - N4 Goal 已具备独立版本化 SQLite、专属 Session、租约与 dispatch intent、宿主证据、`confirm`/`verify`、有限重规划和 CLI／TUI 入口（[docs/goal.md](goal.md)）。
  - 核查 [crates/soyo-core/src/goal.rs](../crates/soyo-core/src/goal.rs#L172-L176) 与 [goal.rs](../crates/soyo-core/src/goal.rs#L524-L536)：`VerificationSpec` 目前仅支持 `Manual`、`FileExists { path }`、`FileSha256 { path, sha256 }`；`GoalStep` 没有 `verifier` 命令字段。当前核验面向 Goal 目标级文件存在性／摘要或人工摘要，而非步骤级可自动执行的宿主命令，且 [goal.md](goal.md) 写明命令退出码与目标验收分别记录。
- **还缺什么**：
  - 缺少步骤级宿主命令 `verifier` 描述、受控执行与退出码／有界输出证据记录，以及 `start --auto` 对步骤 verifier 的强门禁与重规划闭环。
- **建议的最小版本（`DEV-091`、`DEV-092`）**：
  - `goal plan` 可为 `GoalStep` 附加宿主验证命令（结构化 argv／显式配置，经原生沙箱与现行权限／审批边界校验）。
  - `goal start --auto` 仅在步骤对应的宿主 verifier 退出码通过并落库宿主证据后才前进到下一步；失败则在现有预算与 `max_replans` 上限内进入 `replan`（遵循 unknown 不重放边界）；**Goal 不得仅凭模型文本或自述关闭**。
- **收益与成本**：把「先写验证再让 Agent 改到通过」接入已有 N4 Goal，可复用 N4 存储、预算与有限重规划基础设施。
- **主要风险与待核实**：
  - 风险：测试本身不稳定，或 verifier 命令本身带来非预期副作用／安全性问题，须严格对齐原生沙箱、权限规则与既有证据持久化规则。
  - 待核实：上述结论来自对 `HEAD` `goal.rs` 类型与 `docs/goal.md` 的静态核验，具体接入前需先实测当前 N4 验收基线。
- **验收方式**：
  - 在「模型声称已完成但宿主测试／verifier 命令失败」的故障注入测试中，步骤不得标记完成、自动推进不得跳过，Goal 亦不得进入 `completed`。
- **参考与证据**：
  - [crates/soyo-core/src/goal.rs#L172-L176](../crates/soyo-core/src/goal.rs#L172-L176)
  - [docs/goal.md](goal.md)

---

## S5 · 在工具 schema 里加少量真实调用样例（`DEV-093`）

- **状态／成本／轨道**：建议（待开始） · 成本低 · P2 可并行附带（价值第 5）
- **具体机制**：
  - 依据 Anthropic 2025-11-24 官方文章提出的 *Tool Use Examples*：在 JSON Schema 之外附带 1–5 个真实调用样例，演示可选参数、边界值与常见参数组合。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - 核查 [crates/soyo-core/src/boundary.rs](../crates/soyo-core/src/boundary.rs#L54-L58)：`ToolDefinition` 目前仅包含 `name`、`description`、`input_schema` 三个字段；[docs/tool-registry.md](tool-registry.md) 未定义 `examples` 字段或样例校验契约。
- **还缺什么**：
  - 缺少统一的调用样例字段与规范、样例随 `input_schema` 与已知凭据（`SecretRegistry`）的离线校验，以及向模型侧（含三协议与 S2 的 `tool_describe` 输出）的一致投影。
- **建议的最小版本（`DEV-093`）**：
  - 在 `ToolDefinition`（及插件／MCP 工具规范中可选字段）支持有界调用样例列表，优先为易混淆的 MCP／插件工具补充 **1–3 个真实数据样例**（禁用占位符，受大小／节点数上限约束，并经过已知凭据扫描与 `jsonschema` 验证）。
  - 与 S2 互补：延迟加载工具在 `tool_describe` 展开时一并呈现真实调用样例，避免空占首轮上下文。
- **收益与成本**：成本低（主要是规范、校验与文档化），可减少可选参数误填和相似工具误用。
- **主要风险与待核实**：
  - 风险：样例随工具版本演进过时会误导模型，须在加载或单测阶段强制以同一离线 `jsonschema` 校验样例有效性。
  - 待核实：尚未穷尽全部既有插件清单；具体向 OpenAI Chat／Responses 与 Anthropic Messages 的序列化映射需在实现时对齐当前上游字段支持情况（不支持专用字段时可投影进描述或按需描述输出）。
- **验收方式**：
  - 易混工具均带有经 `jsonschema` 与脱敏校验通过的真实调用样例；不编造未实测的“错误率下降百分比”。
- **参考与证据**：
  - [Anthropic: Advanced Tool Use (2025-11-24)](https://www.anthropic.com/research/advanced-tool-use)
  - [crates/soyo-core/src/boundary.rs](../crates/soyo-core/src/boundary.rs)
  - [docs/tool-registry.md](tool-registry.md)

---

## S1 · 实验性程序化工具编排：只读 Code Mode 风格 REPL（`F-21` · `DEV-094`、`DEV-095`）

- **状态／成本／轨道**：建议（待开始） · 成本高 · P2 实现主线（价值第 1，排在 **S2 之后**）
- **外部机制与演进核实**：
  - 模型编写一段 JavaScript，将宿主允许的工具当作函数调用（支持并行或将上一步结果传给下一步），中间结果停留在脚本执行环境中，只把最终返回值送回模型上下文。
  - 外部参考背景（Claude Code 文档与发布记录核实）：官方定位为「默认关闭、实验性、可能变更或撤回」；在 `auto` 模式下，前置审查看到的是整段脚本，而非脚本内每次运行时计算出的工具参数／路径。2026-11-16 的 `v2.1.326` 允许与 agent teams 同时启用，2026-11-18 的 `v2.1.327` 放开「不再因已安装 mod 会 hook 工具调用而拒绝启动」，且管理员仍可在托管设置中关闭——这属于在实验边界内扩展并收紧，而非撤回，也不改变「默认关闭、实验性」定位。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - 全仓搜索未见 `code_mode`、`CodeMode` 或任何 JS 程序化编排实现；Soyo 主循环仍是按次工具派发（含同轮批处理）。
  - Soyo 已有可复用 Agent 循环、N2 原生沙箱、权限与 Hooks 以及已核验的 `OutputStore` 摘录路径，但 `OutputStore` 处理的是单次大工具输出的持久化与检索，与「模型编写多步脚本、在送入模型上下文前过滤中间工具结果」的脚本执行面不属于同一层能力。
- **还缺什么**：
  - 缺少隔离的只读 JS 脚本执行面、脚本内工具桥接协议，以及「中间工具结果不落入主上下文、仅交回过滤结果」的生命周期／持久化／恢复语义。
  - 尚未评估嵌入式 JS 运行时与 macOS Seatbelt／Linux Bubblewrap 原生后端的接合方式。
- **建议的最小版本（`DEV-094`、`DEV-095`）**：
  - **严格默认关闭（opt-in）**，作为实验性能力（`F-21`）提供显式配置与开关。
  - **只对只读工具开放**：最小版本禁止将 `exec`/Shell 与文件写入放入脚本执行面，也不把多智能体协作（`F-09`）插在前面。
  - **逐底层调用门禁**：脚本每次向下调用底层只读工具时，必须经由宿主侧逐次执行现有的 `deny` 规则、作用域根检查、`Hooks`（`tool.before_dispatch`/`tool.finished`）、审批策略、预算与取消信号——不因为外层脚本通过了检查就放行运行时动态算出的越权路径。
- **收益与成本**：在多文件搜索、过滤和批量读取场景下减少主上下文膨胀与模型往返轮次；但实现成本高，涉及新执行面、权限语义、会话恢复、Hooks 关联与超时／内存预算控制。
- **主要风险与待核实**：
  - 主要风险：若 `auto` 模式仅审查整段脚本文本而不逐次拦截运行时动态参数，可能绕过基于路径或参数文本的策略规则；外部参照本身仍标为实验性，接口须维持隔离与可撤回性。
  - 待核实：尚未在 Soyo 中实测多文件只读负载对比，尚未评估具体 JS 运行时选型与 Seatbelt／Bubblewrap 的兼容及隔离边界（由 `DEV-094` 先行完成评估与方案收敛）。
- **验收方式**：
  - 在同一多文件只读任务上，对比开启与关闭实验性 REPL 的请求轮次与结果正确性。
  - 故障与安全注入测试：在脚本内故意构造触及 `deny` 规则或 `PLAN`／受保护路径的动态调用时，底层调用必须仍被拒绝并留下明确意图／拒绝记录。
  - **明确不以「37% token 节省」作为收益承诺或验收标准。**
- **参考与证据**：
  - [Claude Code: Code Mode](https://code.claude.com/docs/en/code-mode)
  - [Claude Code: Changelog](https://code.claude.com/docs/en/changelog)
  - [Claude Code Release v2.1.327](https://github.com/anthropics/claude-code/releases/tag/v2.1.327)

---

## 联合验收与推进顺序（`DEV-096`）

1. **P1 并行起跑**：先以 `DEV-090`（S2：MCP／大工具目录按需发现）作为实现主线入口，同时可在可靠性线并行推进 `DEV-091`～`DEV-092`（S3：Goal 步骤宿主验证器），互不阻断。
2. **P2 附带与后续**：`DEV-093`（S5：工具调用样例）可与 S2 并行或紧接落地并接入 `tool_describe`；`DEV-094`～`DEV-095`（S1：`F-21` 只读程序化编排 REPL）排在 `DEV-090` 之后推进。
3. **联合验收（`DEV-096`）**：核验 S2 按需发现、S3 步骤验证门禁、S5 调用样例与 S1 默认关闭只读 REPL 的跨模块交互、权限／Hooks／沙箱与恢复契约；形成开关对照实测记录与双语增量说明。
