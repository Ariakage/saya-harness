# Soyo Harness N6 工具发现、步骤验证与程序化编排

**开发线增量说明，当前保持 `version = 0.3.0` 且未创建／发布新版本标签。N6（DEV-090 至 DEV-096）已完成实现并在 macOS Seatbelt 与 Linux Bubblewrap（aarch64 Ubuntu 24.04）两端实测；四平台远端矩阵以推送后 CI 为准。**

## 中文

N6 为共享运行时增加按需工具发现、Goal 步骤宿主验证、工具调用样例与默认关闭的实验性只读 JavaScript 编排面。全部能力复用既有权限、审批、Hooks、沙箱与恢复契约，不提供绕过运行时的独立入口。

### 已实现

- **S2 按需工具发现（`D-079`）**：MCP 工具数超过可配置阈值（默认 `> 8`）时延迟注册为延迟定义，模型经 `tool_search`/`tool_describe` 发现并展开，首批筛选保持有界。
- **S3 Goal 步骤验证（`D-080`）**：`StepVerifier` 结构化 argv（非 shell 字符串）在沙箱中执行；`goal confirm` 人工确认与宿主证据齐备前不自动推进，`start --auto` 的重规划有界。
- **S5 调用样例（`D-081`）**：`ToolDefinition.examples` 按 JSON Schema 与已知密钥校验，安全投影到支持的模型协议，供模型参考合法参数形态。
- **S1 实验性只读 JS REPL（`D-082`/`D-083`）**：`[js_repl]` 严格默认关闭。启用后 `js_repl` 工具把模型脚本送入 Seatbelt／Bubblewrap 沙箱内的 `__soyo-js-worker` 同二进制子进程，由 QuickJS（rquickjs）求值；脚本经 `await soyo.callTool(name, args)` 逐次经有界 RPC 回宿主，每一次底层调用独立过 deny、根路径检查、`tool.before_dispatch`/`tool.finished` Hooks、审批、预算、取消与脱敏。仅只读工具可桥接，`exec`、写入、交互、多智能体与递归一律拒绝；中间结果与合成调用 ID 不进入模型上下文，只保留持久化证据；内存、栈、脚本字节、桥消息、调用数与墙钟超时均有硬上界。
- **B2 受控外部 Node 引擎（`D-084`）**：`[js_repl] engine = "node"`＋`[js_repl.node] path` 显式固定二进制（默认仍为 `quickjs`，不做隐式发现、不自动安装）。Node 经 `read_files` 精确只读暴露进同一沙箱，`node_bootstrap.js` 在 `vm` 上下文求值且不向脚本暴露 `require`/`process`/Node 内建；桥协议、逐调用门禁、Hooks、审批与只读边界与 QuickJS 完全一致，引擎级内存/栈限制由 `node` 参数＋宿主墙钟与回收近似。
- **联合验收（DEV-096）**：跨模块矩阵、同一多文件只读任务开关对照（1 次模型可见调用＋3 组持久化证据 vs 3 次模型可见调用，结果一致）、动态构造调用拦截与 check Hook 逐调用门禁均有测试证据，见 [N6 验收记录](../n6-validation.md)。

### 兼容性与限制

- `js_repl` 默认关闭且为实验性能力，保留修改与撤回性；开启需要原生 Seatbelt／Bubblewrap 后端，显式 Host 兼容后端拒绝执行。
- 脚本编排面当前仅只读：不支持 `exec`/Shell、文件写入、浏览器写操作、交互确认、后台控制、多智能体协作与 `js_repl` 递归；桥接调用串行执行，`Promise.all` 不并行派发。
- B2 Node 引擎无 QuickJS 的引擎级抢占/内存中断，死循环仅靠宿主超时与内核限额回收；`engine` 默认保持 `quickjs`，Node 仅为可选对照路径。外部 Deno 仍未实现。
- 验收证据覆盖 macOS Seatbelt 与 aarch64 Linux Bubblewrap（bwrap 0.13.1）两端实跑；Windows 与其余平台以远端 CI 矩阵为准。
- 不以「37% token 节省」等外部指标作为收益承诺或验收标准。

测试结果与未满足项以 [N6 验收记录](../n6-validation.md) 为准；本说明不创建发行或修改版本号。

## English

**Development-line incremental notes; `version = 0.3.0` is unchanged and no new release tag was created or published. N6 (DEV-090 to DEV-096) is implemented and exercised on both macOS Seatbelt and Linux Bubblewrap (aarch64 Ubuntu 24.04); the four-platform remote matrix is governed by post-push CI results.**

N6 adds on-demand tool discovery, host-side Goal step verification, validated call examples, and a default-off experimental read-only JavaScript orchestration surface to the shared runtime. All capabilities reuse the existing permission, approval, hook, sandbox, and recovery contracts; no entry point bypasses the runtime.

### Delivered

- **S2 on-demand discovery (`D-079`)**: when the discovered MCP tool count exceeds the configurable threshold (default `> 8`), tools register as deferred definitions that the model expands through `tool_search`/`tool_describe`, keeping the first projection bounded.
- **S3 Goal step verification (`D-080`)**: `StepVerifier` runs structured argv (never a shell string) inside the sandbox; automatic continuation requires `goal confirm` plus host evidence, and `start --auto` re-planning is bounded.
- **S5 call examples (`D-081`)**: `ToolDefinition.examples` are validated against the JSON Schema and known secrets, then safely projected to supported model protocols.
- **S1 experimental read-only JS REPL (`D-082`/`D-083`)**: `[js_repl]` is strictly default-off. When enabled, `js_repl` evaluates model scripts with QuickJS (rquickjs) inside a same-binary `__soyo-js-worker` subprocess under the Seatbelt/Bubblewrap sandbox. `await soyo.callTool(name, args)` crosses a bounded RPC back to the host, and every bridged call independently passes deny rules, root-path checks, `tool.before_dispatch`/`tool.finished` hooks, approval, budgets, cancellation, and redaction. Only read-only tools may be bridged; `exec`, writes, interactive calls, multi-agent control, and recursion are denied. Intermediate results and synthetic call ids never enter model context while durable evidence persists; memory, stack, script size, wire frames, call count, and wall-clock timeout are all bounded.
- **B2 controlled external Node engine (`D-084`)**: `[js_repl] engine = "node"` plus an explicitly pinned `[js_repl.node] path` binary (`quickjs` stays the default; no implicit discovery or auto-install). Node enters the same sandbox read-only via `read_files`, and `node_bootstrap.js` evaluates scripts in a `vm` context without `require`/`process`/Node builtins. The bridge protocol, per-call gating, hooks, approval, and read-only boundary are identical to QuickJS; engine-level memory/stack limits are approximated by `node` flags plus host wall-clock timeout and teardown.
- **Joint acceptance (DEV-096)**: cross-module matrix, an on/off comparison on the same multi-file read-only task (one model-visible call plus three persisted child-evidence pairs vs three model-visible calls, identical results), dynamic-call deny injection, and per-call check-hook gating are all covered by tests; see the [N6 validation record](../n6-validation.md).

### Compatibility and limits

- `js_repl` is default-off and experimental, and may change or be withdrawn; enabling it requires a native Seatbelt/Bubblewrap backend and explicitly refuses Host compatibility mode.
- The orchestration surface is read-only: no `exec`/shell, file writes, browser writes, interactive confirmations, background control, multi-agent orchestration, or `js_repl` recursion. Bridged calls run sequentially; `Promise.all` does not dispatch in parallel.
- The Node engine lacks QuickJS-style engine preemption/memory interrupts; runaway loops are bounded only by host timeout and kernel limits. `engine` stays `quickjs` by default and Node is an optional comparison path; external Deno remains unimplemented.
- Acceptance evidence covers live runs on macOS Seatbelt and aarch64 Linux Bubblewrap (bwrap 0.13.1); Windows and the remaining platforms are governed by the remote CI matrix.
- External figures such as "37% token savings" are not used as a benefit claim or an acceptance criterion.

Test results and open items are governed by the [N6 validation record](../n6-validation.md); this note creates no release and changes no version number.
