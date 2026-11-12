# N4 Goal 编排验收

**状态：DEV-069 至 DEV-074 已实现，本机 macOS 与 Linux 受控验收完成。跨平台固定源码 CI／发行证据在版本候选确定后单独冻结。**

## 交付范围

| 任务 | 交付 | 主要实现 |
| --- | --- | --- |
| DEV-069 | Goal／Step／证据模型与存储 | `goal.rs`、`goal/store.rs`、独立 SQLite schema v1、预算、CAS、dispatch intent 与 lease |
| DEV-070 | 手动 Goal 与 CLI／TUI | `saya goal` 子命令、`/goals` 面板与 `/goal` 生命周期命令、专属 Session、结构化计划与人工确认 |
| DEV-071 | 持久 worker 与恢复 | 心跳租约、过期 dispatch recovery、unknown 不重放、`goal worker [--once]` |
| DEV-072 | 有限自动续跑、核验与重规划 | 显式 auto authorization、运行／请求／工具／时间／token 预算、无工具核验、已知失败有界修正 |
| DEV-073 | TUI 与报告 | Goal 选择器、步骤／证据／预算面板、continue／start／pause／resume／cancel／verify／replan、JSON／Markdown report |
| DEV-074 | 生命周期与验收 | 权限漂移、worker 崩溃、unknown 副作用、预算停止、虚假完成、CLI／TUI／Bash PTY 与本地平台矩阵 |

## 数据与安全边界

- `data_dir/goals/goals.sqlite3` 使用独立 application ID、`user_version=1`、WAL、私有目录0700和数据库0600。
- Goal 文本与结构化记录在落库前应用当前配置凭据脱敏；发现配置凭据时拒绝保存。
- Goal revision 使用 CAS；dispatch intent、Run 关联、usage、evidence 和状态转换在 SQLite 事务内发布。
- 同一 Goal 同时只有一个未过期 worker lease；同一工作区另由 `workspace_leases` 排他，防止不同 Goal 并发写入。lease 心跳覆盖前台 Goal Run；worker 只恢复已过期 dispatch。
- intent／running 未取得终态时标记 `unknown_side_effect`，不重放。显式 `goal resolve --retry` 才会重新进入调度。
- 自动 Goal 绑定模式、权限模式、profile、权限规则、Sandbox、命令环境名与运行上限指纹（SHA-256）。权限或配置变化进入 `blocked / authorization`。
- file-exists／file-sha256 验收路径必须相对工作区，逐级使用目录能力且拒绝链接、非普通文件、超限和摘要不符。
- 模型计划、Run 完成和模型核验意见都是数据；Goal 只有在全部 Step 完成且每个验收项存在主机记录证据时才进入 `completed`。

## 验收矩阵

| 要求 | 证据 |
| --- | --- |
| 创建、计划、确认、状态、暂停、恢复、取消 | 暂停／恢复保留计划确认与验收等待；真实双进程用例验证活动 Run 接收取消并以 130 收尾 |
| 结构计划与依赖 | 严格 JSON、未知字段拒绝、重复／悬空／循环依赖、前缀校验测试 |
| 手动一步与 Run 关联 | 本地三协议兼容 fixture、专属 Session 加锁、Step Run 与 usage 持久化 |
| 有限自动续跑 | 自动模式逐 Step 执行，完成全部 Step 后等待显式验收证据 |
| 虚假完成 | 模型文本完成但证据缺失时保持 `waiting / verification` |
| 已知失败重规划 | 不完整模型流产生已知 failed Run，`max_replans` 内修正一次并重试；unknown／未持久化失败不重试 |
| 预算 | `max_runs=1` 阻止第二步；规划耗尽 `max_model_requests=1` 后手动 continue 不产生新请求；成功调用缺失 token usage 时保守停止后续 dispatch |
| worker 崩溃 | 过期 dispatch 恢复为 unknown，Goal 等待人工处理，恢复过程不发起重放请求 |
| 同工作区并发 | 第二个 Goal 获取工作区租约时返回 `goal_workspace_busy`，释放后可按序继续 |
| 权限漂移 | auto authorization 后改为 Manual，worker 进入 `blocked / authorization` 且不执行 Run |
| 文件证据 | 文件存在、SHA-256、错误摘要和中间链接拒绝；中间 Step 暂未满足不中断，最终变化使旧证据失效，主机重验不调用模型 |
| TUI | 实际 PTY 完成 `/goal create`、模型计划、确认、Goal 选择器、暂停／恢复、自动 Step、核验、report 与终端恢复 |
| 本地平台 | macOS ARM 完整 Rust／smoke；Linux ARM64 Colima 定向 Goal／SQLite／TUI 测试与 workspace 编译 |

## 审查整改

2026-11-12 对原提交 `56ad4f0` 的 15 项发现完成整改，详见[N4 审查与整改报告](review-2026-11-11.md)。新增 core 反例覆盖状态恢复、证据生命周期、缺失 persistence、全部预算 dispatch、活动暂停／取消和实际读取字节上限；新增 CLI 反例覆盖 PLAN 写库与双工作区报告、人工证据类型、Draft／Cancelled、单次预授权、终端控制字符、文件最终重验及活动模型请求取消。

定向验收中 Goal core 14/14、Goal CLI 15/15 通过。完整整改门禁和 release 证据在报告第 6 节与[开发进度](progress.md)中单独记录；原提交的验收如下保留为历史基线。

## 原提交本机验证（`56ad4f0`）

原提交 release 程序 SHA-256 为
`bd24cedf78d235edcf5b07e63b4575243edb4a86c8ea7562399d79feeb6e32c2`。

macOS 27.2 ARM／Rust 1.99.1：

```sh
RUST_TEST_THREADS=1 ./scripts/check.sh
just test-tooling
just smoke target/release/saya
```

- `scripts/check.sh` 通过格式、workspace 全目标严格 Clippy、1414 项 Rust 测试、8 项条件忽略和 release 构建，日志为 `target/n4-check.log`。
- `just test-tooling` 的 81 项 Python 回归全部通过，日志为 `target/n4-tooling.log`。
- `just smoke` 通过离线工具、文本 journey、5 项审查回归和 28 组 TUI PTY；其中 Goal 场景完成创建、模型计划、确认、选择器、暂停／恢复、自动 Step、核验、report 和终端恢复，日志为 `target/n4-smoke.log` 与 `target/n4-goal-pty.log`。
- Goal CLI fixture 显式选择 Host sandbox，使本组测试只验证编排且不冒充 N2 native Sandbox 验收。修改后的 Goal CLI 7/7 与严格 Clippy 通过，日志为 `target/n4-goal-cli-macos-host.log` 和 `target/n4-goal-cli-clippy.log`。

Linux ARM64：

- 本机 Colima `default` 为 Ubuntu 24.04.4 ARM64／8 GiB VM，测试容器为 Ubuntu 22.04 ARM64、Rust 1.99.1。为适配 VM 内存，容器内使用单并发、关闭测试 debug info 和 incremental。
- Goal core 8/8、Goal CLI 7/7、TUI Goal service／preview／picker 各 1/1 及 workspace `cargo check --locked` 全部通过，日志为 `target/n4-linux.log`。
- 首次未限制并发的容器编译因 8 GiB VM OOM 被 SIGKILL，日志保留为 `target/n4-linux-initial.log`。另一次枚举发现 Goal CLI fixture 隐式依赖缺失的 native 工具，日志保留为 `target/n4-linux-sandbox-missing.log`；显式 Host fixture 后的最终运行通过。
- 临时 Docker source volume 和测试容器已删除，Colima 已停止。

未运行远端 CI 或四平台固定源码发行验收。
