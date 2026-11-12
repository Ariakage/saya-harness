# Saya Harness N4 Goal 编排

**开发线版本说明；正式版本号与固定源码发行证据待发行范围确定。**

## 中文

Saya Harness 现已提供持久 Goal 编排。Goal 保存目标、验收条件、步骤、预算、Run 关联和证据，并绑定一个专属 Session 与工作区。CLI 和 TUI 共用同一 GoalStore、规划与执行服务，单次 Step 仍通过现有 AgentRuntime、权限、Sandbox、Compact、检查点、Session 锁和工具派发运行。

主要入口：

```sh
saya goal create "Deliver a verified result" --accept "Tests pass"
saya goal plan GOAL_ID
saya goal confirm GOAL_ID
saya goal continue GOAL_ID --mode build
saya goal start GOAL_ID --auto --mode build
saya goal verify GOAL_ID criterion-1 "Reviewed the test report"
saya goal report GOAL_ID --format markdown --file ./goal.md
saya goal worker
```

TUI 提供 `/goals` 选择器和 `/goal create|plan|confirm|status|continue|start|pause|resume|cancel|verify|report|replan ID`。Goal 面板展示 Step 状态、Run ID、预算、usage、证据和等待原因。

安全与恢复边界：

- 完整生命周期使用独立版本化 SQLite WAL；revision CAS、dispatch intent、lease 和证据发布均持久化。
- worker 只恢复已过期租约。执行结果无法确认时进入 `unknown_side_effect`，不会自动重放。
- 自动继续必须显式 `start --auto`，绑定当前模式、权限、profile、权限规则、Sandbox 和运行上限。配置变化会阻塞自动 Goal，等待重新授权。
- Goal 预算限制 Run、模型请求、工具、活跃时间、token、规划、核验与重规划。
- 模型计划的文字完成或模型核验意见不能替代主机证据。只有全部 Step 完成且全部验收项存在证据时才会完成。
- Manual Approval 的 Goal 执行通过 CLI 终端审批；当前 TUI Goal continue／start 使用 auto 或 bypass，并继续遵守 PLAN、显式 deny 与 Sandbox 硬边界。

已知限制：

- 当前一个 Goal 绑定一个 Session；跨 Session Goal、子 Goal 和多 Agent 分工不在本阶段。
- 自动重规划采用保守的有界失败修正，不开放任意模型改写已完成 Step。
- `goal worker` 是本地持久调度进程；操作系统级 daemon／service 安装、远端执行和跨设备迁移未提供。
- 本阶段完成本机 macOS／Linux Colima 受控验收；正式四平台发行证据待固定版本候选。

## English

Saya Harness now provides durable Goal orchestration. A Goal stores its objective, acceptance criteria, steps, budget, Run associations, and evidence, and owns one dedicated Session and workspace. CLI and TUI use the same GoalStore and orchestration services. Each Step still runs through the existing AgentRuntime, permissions, sandbox, compaction, checkpoints, session lock, and tool dispatch boundaries.

Primary entry points:

```sh
saya goal create "Deliver a verified result" --accept "Tests pass"
saya goal plan GOAL_ID
saya goal confirm GOAL_ID
saya goal continue GOAL_ID --mode build
saya goal start GOAL_ID --auto --mode build
saya goal verify GOAL_ID criterion-1 "Reviewed the test report"
saya goal report GOAL_ID --format markdown --file ./goal.md
saya goal worker
```

The TUI provides a `/goals` selector and `/goal create|plan|confirm|status|continue|start|pause|resume|cancel|verify|report|replan ID`. The Goal panel shows step status, Run IDs, budgets, usage, evidence, and wait reasons.

Safety and recovery boundaries:

- The lifecycle uses an independent versioned SQLite WAL store with revision CAS, dispatch intents, leases, and transactional evidence publication.
- A worker recovers only expired leases. An outcome that cannot be established becomes `unknown_side_effect` and is never replayed automatically.
- Automatic continuation requires an explicit `start --auto` authorization bound to the current mode, permissions, profile, permission rules, sandbox, and runtime limits. Configuration changes block automatic continuation until reauthorized.
- Goal budgets cover Runs, model requests, tools, active time, tokens, planning, verification, and replanning.
- A model's completion prose or verification opinion cannot replace host evidence. Completion requires every Step to finish and every acceptance criterion to have recorded evidence.
- Manual Approval Goal execution uses the CLI terminal approval path. TUI Goal continue/start currently require auto or bypass permissions and still respect PLAN, explicit deny, and hard sandbox boundaries.

Known limitations:

- A Goal currently owns one Session. Cross-Session Goals, child Goals, and multi-Agent delegation are not part of this stage.
- Automatic replanning uses a conservative bounded failure correction and does not allow arbitrary model rewriting of completed Steps.
- `goal worker` is a local durable scheduling process. OS daemon/service installation, remote execution, and cross-device migration are not provided.
- This stage has local macOS and Linux Colima controlled acceptance. Fixed-version four-platform release evidence remains pending.
