# N6 目标与提案规划（工具发现、步骤验证与程序化编排）

**状态：核心契约已由 `D-079` 至 `D-083` 确认；`DEV-090`（S2）、`DEV-091`／`DEV-092`（S3：步骤验证器模型、v2 存储、沙箱执行、自动推进门禁与有界重规划）与 `DEV-093`（S5：调用样例校验与双路径投影）已完成本机验证，`DEV-094` 已产出 [JS 运行时与沙箱接合评估报告](reports/n6-js-runtime-eval-2026-11-20.md) 并经 `D-083` 拍板（B1 沙箱 worker＋rquickjs，保留 B2 扩展点），`DEV-095`（S1 只读 JS REPL）与 `DEV-096`（N6 联合验收）已完成本机验证，见[N6 验收记录](n6-validation.md) · 更新于 2026-11-20。本文整理并收敛 S1、S2、S3、S5 四项新增目标与能力，拆分为 `DEV-090` 至 `DEV-096`（归属阶段 N6，新增能力 `F-21` 并扩展 `F-02`、`F-06`、`F-20`）。其中 S1 的 JS 运行时与沙箱接合方案按 `D-082` 保留为 `DEV-094` 评估后的拍板门禁；其余实施中若需变更已确认契约或默认值，须另行记入[决策记录](decisions.md)。**

> 进度与状态跟踪入口见[开发进度](progress.md)，任务编号与依赖关系见[后续开发计划](post-mvp-plan.md)，能力定位见[能力路线图](capability-roadmap.md)，已确认决策见[决策记录](decisions.md#当前基线与变更管理)。

## 总览与实施排序

| 提案 | 名称 | 优先级 | 轨道 | 价值排序 | 成本 | 归属能力 | 对应决策 | 对应任务 | 当前状态 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **S2** | MCP／大工具目录的按需发现（`search + describe`） | **P1** | 实现主线 | 第 2 | 中 | `F-02`、`F-06` | `D-079` | `DEV-090` | 已完成本机验证（已发现 MCP `> 8` 自动延迟） |
| **S3** | 给 Goal 步骤绑定宿主验证器（不重做 Goal） | **P1** | 可靠性线 | 第 3 | 中 | `F-20` | `D-080` | `DEV-091`、`DEV-092` | `DEV-091`、`DEV-092` 已完成本机验证 |
| **S5** | 在工具 schema 里加少量真实调用样例 | **P2** | 可并行附带 | 第 5 | 低 | `F-02` | `D-081` | `DEV-093` | 已完成本机验证（`ToolDefinition.examples` + 双路径投影） |
| **S1** | 实验性程序化工具编排（只读 Code Mode 风格 REPL） | **P2** | 实现主线（S2 之后） | 第 1 | 高 | **新增 `F-21`** | `D-082`、`D-083` | `DEV-094`、`DEV-095` | `D-083` 已定案 B1＋rquickjs，`DEV-095` 本机验证完成 |
| **—** | N6 跨能力联合验收与双语增量说明 | — | 联合验收 | — | — | `F-02`、`F-20`、`F-21` | `D-079`–`D-082` | `DEV-096` | 完成，见[N6 验收记录](n6-validation.md) |

---

## S2 · MCP／大工具目录的按需发现（`DEV-090` · `D-079`）

- **状态／成本／轨道**：已完成本机验证（`D-079`） · 成本中 · P1 实现主线（价值第 2）
- **具体机制**：
  - **按阈值自动触发（`D-079`）**：仅当已发现的 MCP 工具总数超过阈值（默认 `> 8`，支持显式配置覆盖）时自动启用延迟加载；小目录（`≤ 8`）维持原有全量注入行为，避免为少量工具引入额外检索轮次并保持既有夹具兼容。
  - 核心内置读写与 `exec` 工具保持常驻；触发延迟加载后，将 MCP／大型外部工具标记为 `defer_loading`，向模型提供 `tool_search` 与 `tool_describe`，由模型先检索候选、再按需展开所需工具定义。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - `soyo-core` 已具备 stdio／Streamable HTTP MCP、Skill 发现与统一工具注册表（`F-02`／`F-03`）。
  - 本次已逐行核对装配路径，确认此前“是否每轮全量注入 `ModelRequest.tools`”的待核实项：
    1. [crates/soyo-core/src/registry.rs](../crates/soyo-core/src/registry.rs#L178-L187) 中 `ToolRegistry::definitions` 遍历 `self.entries.values()`，仅按 `PLAN`（`allowed_in_plan()`）和持久性（`!requires_durability()`）过滤。
    2. [crates/soyo-core/src/dispatch.rs](../crates/soyo-core/src/dispatch.rs#L458-L481) 中 `ToolDispatcher::definitions` 仅追加 `read_only` 模式对 Browser 工具的过滤，随后返回其余全部已注册工具（含全部已发现的 MCP 工具）。
    3. [crates/soyo-core/src/runtime/mod.rs](../crates/soyo-core/src/runtime/mod.rs#L914-L934)（`AgentRuntime::definitions`）与 [runtime/mod.rs](../crates/soyo-core/src/runtime/mod.rs#L1086-L1156)（`prepare_request`）合并记忆／Skill／检查点工具后，每轮将完整 `Vec<ToolDefinition>` 写入 `ModelRequest.tools`；三类协议适配器（[provider/chat.rs](../crates/soyo-core/src/provider/chat.rs#L83-L94)、[provider/responses.rs](../crates/soyo-core/src/provider/responses.rs#L67-L78)、[provider/anthropic.rs](../crates/soyo-core/src/provider/anthropic.rs#L160-L175)）全量序列化发出。
  - 全仓代码搜索未见 `defer_loading`、`tool_search` 或 `tool_describe`，且 [MCP 文档](mcp.md) 仍说明模型看到的是完整注册表快照。
- **还缺什么与设计注意点**：
  - 缺少注册表的延迟标记、按需检索与描述接口，以及动态扩展工具列表时对 Prompt Caching 前缀稳定性（如 [provider/anthropic.rs](../crates/soyo-core/src/provider/anthropic.rs#L130) 的 `cached_tools` 断点）的明确约束：核心工具须保持稳定前缀顺序，按需展开的外部工具应避免破坏既定缓存前缀约定，并保留目录代次与权限／审批身份绑定。
- **已核定最小版本（`DEV-090` · `D-079`）**：
  - 内置读写与 `exec`（含核心只读工具）保持常驻且顺序稳定；已发现 MCP 工具总数超过阈值（默认 `> 8`，可配置覆盖）时自动启用延迟加载，小目录（`≤ 8`）保留全量注入。
  - 新增宿主可控的 `tool_search` 与 `tool_describe`，仅返回脱敏后的名称、简要摘要与按需展开的 schema；展开工具按稳定顺序追加而不重排核心缓存前缀，且不绕过 `PLAN` 过滤、目录代次校验或 `KnownSecrets` 检查。
- **收益与成本**：多 MCP 服务器时降低固定工具 schema 上下文占用并减少相似工具名误选；主要涉及装配、检索与缓存前缀控制，不必新建脚本沙箱。
- **主要风险**：搜索漏工具会导致模型静默降级或找不到正确工具；若动态修改 `ModelRequest.tools` 未固定前缀顺序，可能影响多轮缓存命中。
- **验收方式**：
  - 小目录（`≤ 8` 个 MCP 工具）回归维持既有全量注入行为；
  - 夹具挂载 **20+ 个虚假 MCP 工具**，断言自动触发延迟加载、首轮提示里只包含核心内置工具（含 `tool_search`/`tool_describe`）加已命中／展开项，不出现全量 20+ MCP schema，且任务仍能选对并正确调用目标工具。
  - Anthropic 2025-11-24 官方文章中的 Tool Search 数字属于当年内部测试数据，不作为本仓库基准或验收指标。
- **参考与证据**：
  - [Anthropic: Advanced Tool Use (2025-11-24)](https://www.anthropic.com/research/advanced-tool-use)
  - [docs/mcp.md](mcp.md)
  - [crates/soyo-core/src/boundary.rs](../crates/soyo-core/src/boundary.rs)

---

## S3 · 给 Goal 步骤绑定宿主验证器（`DEV-091`、`DEV-092` · `D-080`）

- **状态／成本／轨道**：`DEV-091`、`DEV-092` 已完成本机验证（`D-080`） · 成本中 · P1 可靠性线（价值第 3，不阻断按需发现）
- **具体机制**：
  - 在不重做现有 N4 Goal 架构的前提下，为步骤引入宿主命令验证器：步骤是否完成由宿主执行验证命令（测试、lint、自定义检查）的退出码和有界输出决定，而不是模型自称完成。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - N4 Goal 已具备独立版本化 SQLite、专属 Session、租约与 dispatch intent、宿主证据、`confirm`/`verify`、有限重规划和 CLI／TUI 入口（[docs/goal.md](goal.md)）。
  - 核查 [crates/soyo-core/src/goal.rs](../crates/soyo-core/src/goal.rs#L172-L176) 与 [goal.rs](../crates/soyo-core/src/goal.rs#L524-L536)：`VerificationSpec` 目前仅支持 `Manual`、`FileExists { path }`、`FileSha256 { path, sha256 }`；`GoalStep` 没有 `verifier` 命令字段。当前核验面向 Goal 目标级文件存在性／摘要或人工摘要，而非步骤级可自动执行的宿主命令，且 [goal.md](goal.md) 写明命令退出码与目标验收分别记录。
- **还缺什么**：
  - 缺少步骤级宿主命令 `verifier` 描述、受控执行与退出码／有界输出证据记录，以及 `start --auto` 对步骤 verifier 的强门禁与重规划闭环。
- **已核定最小版本（`DEV-091`、`DEV-092` · `D-080`）**：
  - 为 `GoalStep` 与版本化 `goals.sqlite3` 增加可选 `verifier`（仅结构化 argv，禁止隐式 shell 字符串拼接，附带相对工作目录、超时与输出上限）。
  - 允许 `goal plan` 模型草案提议或通过 `--plan-file`／CLI 显式指定步骤 `verifier`，但所有 `verifier` 必须经 `goal confirm` 逐条人工展示并锁定后方可执行。
  - 执行走现有 `ExecutionBackend`（原生沙箱）、现行 `deny`／权限规则、预算、取消与 `SecretRegistry` 脱敏，`PLAN` 模式不执行；`goal start --auto` 仅在步骤对应的宿主 verifier 退出码为 0 并落库宿主证据（退出码 + 有界输出摘录，与模型文本严格分离）后才前进到下一步；失败则在现有预算与 `max_replans` 上限内进入 `replan`，unknown／取消／超时保守停止（不自动重放）；**Goal 不得仅凭模型文本或自述关闭**。
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

## S5 · 在工具 schema 里加少量真实调用样例（`DEV-093` · `D-081`）

- **状态／成本／轨道**：`DEV-093` 已完成本机验证（`D-081`） · 成本低 · P2 可并行附带（价值第 5）
- **具体机制**：
  - 依据 Anthropic 2025-11-24 官方文章提出的 *Tool Use Examples*：在 JSON Schema 之外附带 1–5 个真实调用样例，演示可选参数、边界值与常见参数组合。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - 核查 [crates/soyo-core/src/boundary.rs](../crates/soyo-core/src/boundary.rs#L54-L58)：`ToolDefinition` 目前仅包含 `name`、`description`、`input_schema` 三个字段；[docs/tool-registry.md](tool-registry.md) 未定义 `examples` 字段或样例校验契约。
- **还缺什么**：
  - 缺少统一的调用样例字段与规范、样例随 `input_schema` 与已知凭据（`SecretRegistry`）的离线校验，以及向模型侧（含三协议与 S2 的 `tool_describe` 输出）的一致投影。
- **已核定最小版本（`DEV-093` · `D-081`）**：
  - 在 `ToolDefinition` 中新增 `examples: Vec<Value>`（及插件／MCP 工具规范对应可选字段），优先为易混淆的 MCP／插件／内置工具补充 **1–3 条真实数据样例**（建议 1–5 条，禁用占位符，受大小／节点数上限约束）。
  - 所有样例在加载／校验期必须经过工具自身离线 `jsonschema` 校验与 `SecretRegistry` 脱敏检查，防止 schema 变更后样例过期误导或泄露凭据。
  - **双路径投影（`D-081`）**：配合 `D-079`，延迟加载工具在 `tool_describe` 返回结果中展示样例（零首轮上下文占用）；常驻工具安全投影至三协议（兼容 `strict_tools` 与第三方兼容端点，不向 OpenAI strict schema 注入非法关键字）。
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

## S1 · 实验性程序化工具编排：只读 Code Mode 风格 REPL（`F-21` · `DEV-094`、`DEV-095` · `D-082`）

- **状态／成本／轨道**：`D-082` 已定案安全边界与评估停靠门禁；`DEV-094` 评估报告已产出（[报告](reports/n6-js-runtime-eval-2026-11-20.md)），`D-083` 已定案 B1 沙箱 worker＋rquickjs（B2 外部运行时保留为扩展点）；`DEV-095` 已完成本机验证 · 成本高 · P2 实现主线（价值第 1，排在 **S2 之后**）
- **`DEV-095` 落地摘要**：新增 `js_repl` 模块——`__soyo-js-worker` 同二进制私有 argv 经 `internal_worker_argv` 进入 Seatbelt／Bubblewrap 沙箱，worker 内以 rquickjs（QuickJS）求值，脚本经 `await soyo.callTool(name, args)` 逐次经有界换行 JSON RPC 回宿主；宿主为每个桥接调用生成 `外层:repl:N` 合成调用，完整过 `tool.before_dispatch` Hooks → `authorize`（含人工确认）→ 一次性许可 → `execute` → `tool.finished` 与脱敏，证据经 `ReplStore` 持久化但不写 `History`；只读 allowlist（`FileRead`、`WebRead`、发现与输出查询、`ExternalReadOnly`）外的工具、`exec`、写入、交互、多智能体与 `js_repl` 自身在授权前 fail-closed；`[js_repl]` 配置默认关闭，引擎内存／栈／抢占、脚本字节、桥消息、底层调用数与墙钟超时均有硬上界。验证：`js_repl` 模块单测 14 项、`soyo-cli --test js_repl` 12 项（真二进制 stdio 协议＋原生 Seatbelt 沙箱内桥接 `read_file`、deny 注入、check Hook 逐调用门禁、动态构造调用拦截、多文件开关对照、超时清理、无历史污染）与 `RUST_TEST_THREADS=1 just check`。
- **外部机制与演进核实**：
  - 模型编写一段 JavaScript，将宿主允许的工具当作函数调用（支持并行或将上一步结果传给下一步），中间结果停留在脚本执行环境中，只把最终返回值送回模型上下文。
  - 外部参考背景（Claude Code 文档与发布记录核实）：官方定位为「默认关闭、实验性、可能变更或撤回」；在 `auto` 模式下，前置审查看到的是整段脚本，而非脚本内每次运行时计算出的工具参数／路径。2026-11-16 的 `v2.1.326` 允许与 agent teams 同时启用，2026-11-18 的 `v2.1.327` 放开「不再因已安装 mod 会 hook 工具调用而拒绝启动」，且管理员仍可在托管设置中关闭——这属于在实验边界内扩展并收紧，而非撤回，也不改变「默认关闭、实验性」定位。
- **已核实当前代码与文档现状（HEAD `47996e1`）**：
  - 全仓搜索未见 `code_mode`、`CodeMode` 或任何 JS 程序化编排实现；Soyo 主循环仍是按次工具派发（含同轮批处理）。
  - Soyo 已有可复用 Agent 循环、N2 原生沙箱、权限与 Hooks 以及已核验的 `OutputStore` 摘录路径，但 `OutputStore` 处理的是单次大工具输出的持久化与检索，与「模型编写多步脚本、在送入模型上下文前过滤中间工具结果」的脚本执行面不属于同一层能力。
- **还缺什么**：
  - 缺少隔离的只读 JS 脚本执行面、脚本内工具桥接协议，以及「中间工具结果不落入主上下文、仅交回过滤结果」的生命周期／持久化／恢复语义。
  - 尚未评估嵌入式 JS 运行时与 macOS Seatbelt／Linux Bubblewrap 原生后端的接合方式。
- **已核定最小版本与停靠门禁（`DEV-094`、`DEV-095` · `D-082`）**：
  - **严格默认关闭（opt-in）**，定位为实验性能力（`F-21`，保留可修改与可撤回性），且排在 S2（`DEV-090`）之后推进。
  - **只对只读工具开放**：最小版本禁止将 `exec`/Shell 与文件写入放入脚本执行面，也不把多智能体协作（`F-09`）插在前面。
  - **逐底层调用门禁**：脚本每次向下调用底层只读工具时，必须经由宿主 `ToolDispatcher` 逐次执行现有的 `deny` 规则、作用域根检查、`Hooks`（`tool.before_dispatch`/`tool.finished`，不因已安装 Hook 拒绝启动且保留管理员／配置显式关闭开关）、审批策略、预算、取消与脱敏——不因为外层脚本通过了 `auto` 检查就放行运行时动态算出的越权路径。
  - **`DEV-094` 拍板门禁**：先完成「宿主进程内纯 Rust 嵌入式 JS 引擎 vs 原生沙箱（Seatbelt／Bubblewrap）内部 JS 引擎子进程（及受控 Node 对照）」的对比评估报告（构建／二进制体积／C 依赖／OS 隔离强度／延迟／内存与指令限制），**停下来经项目负责人拍板并补录后续决策后，再进入 `DEV-095` 编码**。
- **收益与成本**：在多文件搜索、过滤和批量读取场景下减少主上下文膨胀与模型往返轮次；但实现成本高，涉及新执行面、权限语义、会话恢复、Hooks 关联与超时／内存预算控制。
- **主要风险与待核实**：
  - 主要风险：若 `auto` 模式仅审查整段脚本文本而不逐次拦截运行时动态参数，可能绕过基于路径或参数文本的策略规则；外部参照本身仍标为实验性，接口须维持隔离与可撤回性。
  - 待核实：尚未在 Soyo 中实测多文件只读负载对比，JS 运行时选型与 Seatbelt／Bubblewrap 的接合方案待 `DEV-094` 评估后定案。
- **验收方式**：
  - 在同一多文件只读任务上，对比开启与关闭实验性 REPL 的请求轮次与结果正确性。
  - 故障与安全注入测试：在脚本内故意构造触及 `deny` 规则或 `PLAN`／受保护路径的动态调用时，底层调用必须仍被拒绝并留下明确意图／拒绝记录。
  - **明确不以「37% token 节省」作为收益承诺或验收标准。**
- **参考与证据**：
  - [Claude Code: Code Mode](https://code.claude.com/docs/en/code-mode)
  - [Claude Code: Changelog](https://code.claude.com/docs/en/changelog)
  - [Claude Code Release v2.1.327](https://github.com/anthropics/claude-code/releases/tag/v2.1.327)

---

## 联合验收与 `/goal` 分批推进建议（`DEV-096`）

1. **第一轮 `/goal`（`DEV-090`～`DEV-093` · `D-079`/`D-080`/`D-081`）**：先以 `DEV-090`（S2：`> 8` 阈值按需工具发现）作为实现主线，并行推进 `DEV-091`～`DEV-092`（S3：Goal 步骤宿主验证器）与 `DEV-093`（S5：`ToolDefinition.examples` 校验与双路径投影）。
2. **第二轮 `/goal`（`DEV-094` 评估拍板 → `DEV-095`～`DEV-096` · `D-082`）**：先执行 `DEV-094`（S1 JS 运行时与沙箱对比评估）并停靠等待决策拍板与 `D-083+` 落盘，确认后再实施 `DEV-095`（S1 只读 REPL）与 `DEV-096`（N6 联合验收）。
3. **联合验收（`DEV-096`）**：核验 S2 按需发现、S5 调用样例、S3 步骤验证门禁与 S1 默认关闭只读 REPL 的跨模块交互、权限／Hooks／沙箱与恢复契约；形成开关对照实测记录与双语增量说明。
