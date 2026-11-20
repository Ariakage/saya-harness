# N6 工具发现、步骤验证与程序化编排验收

**2026-11-20：DEV-090 至 DEV-096 已完成实现与双平台原生后端实测。联合矩阵（S2 按需发现 × S5 调用样例 × S3 步骤验证 × S1 默认关闭只读 REPL）通过；多文件只读任务开关对照实测完成；deny 动态注入与 check Hook 逐调用门禁均有持久化拒绝证据。S1 在 macOS Seatbelt 与 Linux Bubblewrap（aarch64 Ubuntu 24.04，bwrap 0.13.1）两端实跑通过；B2 受控外部 Node 引擎按 D-084 落地并在两个后端下实测。四平台远端 CI（run `44640448903`）六项任务全部 success。**

任务范围见 [N6 规划](n6-plan.md)；安全契约与拍板见 `D-079`～`D-084`（[决策记录](decisions.md)）；JS 运行时评估见 [DEV-094 报告](reports/n6-js-runtime-eval-2026-11-20.md)。

## 提交与范围

| 任务 | 提交 | 交付 |
| --- | --- | --- |
| DEV-090 | `fbd401e` | S2 `> 8` 阈值按需工具发现（`tool_search`/`tool_describe`） |
| DEV-091 | `fa0bd56` | S3 `StepVerifier` 结构化模型、goals v2 存储、`set-verifier` |
| DEV-092 | `2265fc7` | S3 `run_step` 沙箱验证门禁与 `start --auto` 有界重规划 |
| DEV-093 | `05e2025` | S5 `ToolDefinition.examples` 校验与双路径投影 |
| DEV-094 | `c88c484`、`dfec6b3` | S1 JS 运行时评估报告与 `D-083` 拍板（B1＋rquickjs，保留 B2） |
| DEV-095 | `21c4cd4` | S1 默认关闭只读 `js_repl`：沙箱 `__soyo-js-worker`＋QuickJS＋逐调用门禁 |
| DEV-096 | `f1e6817`＋后续 B2 提交 | N6 跨能力联合验收、开关对照实测、双语增量说明；B2 受控外部 Node 引擎（`D-084`）与 Linux Bubblewrap 实测 |

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
| S1 × B2 引擎（D-084） | `engine = "node"` 显式固定二进制经 `read_files` 只读暴露进沙箱，`vm` 求值面不含 `require`/`process`/Node 内建；桥协议、逐调用门禁、限额与 B1 完全一致 | `js_repl_node_engine_runs_inside_the_sandbox`、`js_repl_node_engine_reports_script_failures`、`js_repl::mod` 引擎解析单测 |
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

## Linux Bubblewrap 实测（aarch64 Ubuntu 24.04）

Colima（macOS VZ）aarch64 Ubuntu 24.04 特权容器内实跑：Bubblewrap 0.13.1 按 `scripts/n2/install-linux-tools.sh` 同源构建（sha256 校验、非 setuid），Node v18.20.4（Debian bookworm 包）。

- `cargo test -p soyo-cli --test js_repl -- --test-threads=1`：**14 passed**（75.72s），覆盖 QuickJS 与 Node 双引擎在 Bubblewrap+Seccomp 下的全栈用例，含 Node 引擎沙箱内执行与桥接。
- 低版本 bwrap 0.8（发行版默认包）按 `sandbox_capability_unavailable` 明确拒绝，符合 D-050 失败关闭约定。

## 四平台远端矩阵（CI run 44640448903）

推送 `1bef2da..89ca997` 触发的 CI run `44640448903` 六项任务全部 success：

| 任务 | 结果 |
| --- | --- |
| macos-15（Apple Silicon） | success：含 `js_repl` 14 项（QuickJS＋Node 引擎 Seatbelt 内执行）、release smoke／journey／review_regressions／tui_journey |
| macos-15-intel | success：同上 |
| ubuntu-22.04（x86_64） | success：含 `js_repl` 14 项（Bubblewrap）与全部 journey 套件 |
| ubuntu-22.04-arm | success：同上 |
| Desktop frontend | success |
| Advisory audit | success |

矩阵收集期间发现并修复的**既有回归**（属 helper 迁移与 D-075/D-077 提交组，与 js_repl 无关，修复提交 `b016463`、`ba881c8`）：

- helper socket 采用 `<state_dir>/helper/<fp>.sock`（D-077）后，journey 深层临时目录下路径超出 macOS `sun_path` 上限，helper 无法绑定 → `chat_via_helper` 回落 in-process 时遗留首个 `Console` reader，两个 tty 读者分尸输入并满 CPU 空转；`chat` 回落前 `drop(console)`，journey 改用 `SOYO_HELPER_DIR` 短路径（既有逃逸口）。
- `tui_journey` 的 `MODE_COLORS` 仍是 D-075 调色板落地前的旧值（`13f2eef` 未同步），动画颜色断言必然失败 → 对齐新色板。
- TUI `/new` 经 helper 的 `new_session` IPC 不携带模式，`--mode` 未持久化（`work_mode` 恒为 `work`）→ 远端建会话后按 `config.mode` 调 `set_work_mode` 对齐。
- TUI `/permissions` 发送参数名 `mode`，与服务端契约 `permissions` 漂移 → 对齐。
- helper `resume_session_for` 对已驻留空闲会话槽直接绑定新连接、不检查 `owner_conn_id` → `session_busy` 语义缺失，第二客户端可静默夺权；补 busy 检查并在接管时转移所有权。
- 远端运行启动即被校验拒绝（如 `invalid_config`）时草稿不回填编辑器，与本地 `start_run` 同步失败语义不一致 → `finish` 在无观察结果失败时恢复草稿。

更早几轮 run（`44614004687` 起）中 js_repl 相关用例在四平台均已通过；期间的 Node keg 依赖暴露与 merged-usr 顶层符号链接问题已分别由 `47dbc48`/`8487cfd`/`06f0e16`/`2ab5a93`/`1bef2da` 修复，ubuntu 的 `crashed_background_command` 时序失败为既有偶发 flake，本地与本轮均通过。

## 未覆盖边界

- Windows 未在矩阵内；Node 引擎在 Windows 平台未实测。
- Node 引擎无 QuickJS 的引擎级抢占／内存硬中断，`--max-old-space-size`/`--stack-size`＋宿主墙钟与 SIGKILL 近似；CPU 死循环仅靠内核限额与宿主超时回收（D-084）。
- 脚本内 `Promise.all` 可书写但桥接调用保持串行；写操作、`exec`、交互、多智能体协作显式排除在最小版本之外。

## 验证命令记录

- `cargo test -p soyo-core --lib js_repl`：15 passed（含引擎解析／Node 配置校验）
- `cargo test -p soyo-cli --test js_repl`：14 passed（3 项 stdio worker＋11 项原生沙箱全栈，含 2 项 Node 引擎）
- macOS Seatbelt：同套 14 项全绿（fnm Node v22.23.0 实测）
- Linux Bubblewrap：同套 14 项全绿（aarch64 Ubuntu 24.04 容器，bwrap 0.13.1＋Node v18.20.4）
- `RUST_TEST_THREADS=1 just check`：fmt＋严格 Clippy＋默认成员测试＋release 构建通过
- `just test-tooling`：通过
