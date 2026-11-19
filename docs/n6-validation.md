# N6 工具发现、步骤验证与程序化编排验收

**2026-11-20：DEV-090 至 DEV-096 已完成本机实现与 macOS Seatbelt 受控验收。联合矩阵（S2 按需发现 × S5 调用样例 × S3 步骤验证 × S1 默认关闭只读 REPL）通过；多文件只读任务开关对照实测完成；deny 动态注入与 check Hook 逐调用门禁均有持久化拒绝证据。四平台远端 CI 尚未在本里程碑执行，不据此声称全平台发行完成。**

任务范围见 [N6 规划](n6-plan.md)；安全契约与拍板见 `D-079`～`D-083`（[决策记录](decisions.md)）；JS 运行时评估见 [DEV-094 报告](reports/n6-js-runtime-eval-2026-11-20.md)。

## 提交与范围

| 任务 | 提交 | 交付 |
| --- | --- | --- |
| DEV-090 | `fbd401e` | S2 `> 8` 阈值按需工具发现（`tool_search`/`tool_describe`） |
| DEV-091 | `fa0bd56` | S3 `StepVerifier` 结构化模型、goals v2 存储、`set-verifier` |
| DEV-092 | `2265fc7` | S3 `run_step` 沙箱验证门禁与 `start --auto` 有界重规划 |
| DEV-093 | `05e2025` | S5 `ToolDefinition.examples` 校验与双路径投影 |
| DEV-094 | `c88c484`、`dfec6b3` | S1 JS 运行时评估报告与 `D-083` 拍板（B1＋rquickjs，保留 B2） |
| DEV-095 | `21c4cd4` | S1 默认关闭只读 `js_repl`：沙箱 `__soyo-js-worker`＋QuickJS＋逐调用门禁 |
| DEV-096 | 本次 | N6 跨能力联合验收、开关对照实测与双语增量说明 |

## 联合验收矩阵（DEV-096）

| 交叉面 | 检查内容 | 证据 |
| --- | --- | --- |
| S1 × 开关默认 | `js_repl` 未启用时对外保持 unknown tool；启用但未配置原生环境时按 `sandbox_capability_unavailable` 失败关闭，不回退 Host | `js_repl_stays_unknown_when_not_enabled`、`js_repl_requires_a_native_environment` |
| S1 × 沙箱 | 脚本在 Seatbelt 沙箱内的 `__soyo-js-worker` 同二进制子进程求值；worker 无文件／网络／进程／模块／凭据面，stdio 之外不接触宿主状态 | `js_repl_runs_scripts_and_bridges_read_only_tools_inside_the_sandbox`、`js_worker_*` stdio 用例（`HOME` 投毒断言未打开应用状态） |
| S1 × 只读边界 | `exec`、写入、`js_repl` 递归与未注册工具在派发前 `permission_denied`；写目标不存在 | `js_repl_bridged_denials_reach_the_script_as_failures` |
| S1 × 动态注入 | 运行时拼接的工具名（`'e'+'xec'`）与越界路径（`'..'` 拼接）仍逐次被拦截：`permission_denied`／`path_denied`，且两次被拦截调用均留有 ToolIntent＋ToolFinished 证据 | `js_repl_dynamically_computed_calls_still_face_the_gate` |
| S1 × Hooks | `tool.before_dispatch` check Hook 按子调用单独裁决：`read_file` 被 `hook_denied` 拦截、`list_dir` 经同一 Hook 放行并执行；两者均有持久化证据 | `js_repl_bridged_calls_pass_check_hooks_individually`（真实插件 Hook 夹具） |
| S1 × 持久化／恢复 | 子调用 ToolIntent／ToolFinished 全部落 store，但合成 `:repl:N` ID 不进入 `History`／模型上下文；外层 `js_repl` 结果正常入历史 | `js_repl_bridged_calls_carry_durable_evidence_without_history`、`js_repl_runs_...` 的 history 断言、`dispatch::tests` 的 `ReplStore` 单测 |
| S1 × 限额／取消 | 畸形参数 `invalid_args`、死循环按 `tool_timeout` 有界回收；引擎内存／栈／抢占、脚本字节、桥消息、调用数均有硬上界 | `js_repl_malformed_arguments_and_timeout_fail_closed`、`js_repl::worker`/`protocol`/`mod` 单测（14 项） |
| S1 × S2 | `tool_search`/`tool_describe`/`output_read`/`output_search` 属 `SessionMetadata` 只读清单可桥接；桥接资格按注册表副作用描述符判定，未知副作用 fail-closed | `mod` 单测 `allowlist` 用例＋运行时 `allowed_sub_call` |
| S1 × S5 | `js_repl` 自身 `ToolDefinition.examples` 经既有样例校验（D-081），随注册生效 | `register_control_definition` 注册路径（定义内含合法样例）＋DEV-093 校验测试 |
| S2 × S5 × S3 | 按需发现、调用样例投影与步骤验证门禁为独立既有验收，本里程碑回归未破坏 | `tool_discovery`、`goal`/`verifier`、`example`/registry 既有套件全绿 |

## 开关对照实测（多文件只读任务）

同一 workspace 三个文件（`alpha.txt`/`beta.txt`/`gamma.txt`）的批量只读任务，`js_repl_multi_file_comparison_against_per_call_dispatch` 实测：

| 维度 | 开启 `js_repl` | 关闭 |
| --- | --- | --- |
| 模型可见 ToolCall | 1 次（`repl-1`） | 3 次（`read-1/2/3`） |
| 模型历史中的中间结果 | 0（合成 `:repl:N` 结果不入 `History`） | 3 条 `ToolResult` |
| 持久化证据 | 外层 1＋子调用 3 组 intent/finished | 3 组 intent/finished |
| 最终内容 | `[CONTENT-ALPHA, CONTENT-BETA, CONTENT-GAMMA]` | 同左，逐项相等 |

按 `D-082`，**不以「37% token 节省」作为收益承诺或验收标准**；对照只记录模型可见调用数、历史条数与结果正确性。

## 未覆盖边界

- 验收环境为本机 macOS Seatbelt；Linux Bubblewrap 端 `internal_worker_argv` 重写已就位但未在本次实测。
- B2 受控外部 Node/Deno 仅为 `D-083` 保留的扩展点，未实现、未验收。
- 脚本内 `Promise.all` 可书写但桥接调用保持串行；写操作、`exec`、交互、多智能体协作显式排除在最小版本之外。

## 验证命令记录

- `cargo test -p soyo-core --lib js_repl`：14 passed
- `cargo test -p soyo-cli --test js_repl`：12 passed（3 项 stdio worker＋9 项原生沙箱全栈）
- `RUST_TEST_THREADS=1 just check`：fmt＋严格 Clippy＋默认成员测试＋release 构建通过
- `just test-tooling`：通过
