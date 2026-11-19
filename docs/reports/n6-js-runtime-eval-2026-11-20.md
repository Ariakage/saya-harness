# N6/S1（`F-21`）JS 运行时与沙箱接合方案评估

**日期：2026-11-20 · 任务：DEV-094（`D-082` 拍板门禁） · 状态：评估完成，待项目负责人拍板并补录 `D-083+` 后再进入 `DEV-095` 编码**

## 目标与范围

按 `D-082` 与 [N6 规划](../n6-plan.md) S1 节，对比「宿主进程内纯 Rust 嵌入式 JS 引擎」与「原生沙箱（Seatbelt／Bubblewrap）内部 JS 引擎子进程」（含受控外部 Node 对照）在构建／二进制体积／C 依赖／OS 隔离强度／延迟／内存与指令限制／超时与取消／Hooks 与逐底层调用门禁／恢复上的取舍，收敛 `DEV-095` 的只读工具桥接、资源限制、Hooks、恢复与默认关闭（opt-in）设计空间。

本报告只产出评估与待拍板决策点；不实现代码，不以「37% token 节省」作收益承诺或验收线（`D-082`）。

## 已核实的仓库现状（HEAD `05e2025`）

- **执行后端**：`ExecutionBackend::prepare(SandboxPolicy) -> PreparedEnvironment`（`crates/soyo-core/src/execution.rs`）。`backend = "auto"` 在 macOS 选 Seatbelt、Linux 选 Bubblewrap；`SandboxPolicy` 已承载 read/write roots、`read_files` 精确文件、`protected_paths`、`write_denied_paths`、`NetworkPolicy`（`Deny`/`Proxy`/`LoopbackPort`）与 `ResourceLimits`（`cpu_seconds`、`total_memory_bytes`、`total_processes`、`open_files`、`cpu_quota`，可叠加 cgroup v2 委托）。
- **沙箱内子进程先例**：`execution/worker.rs` 的 `__soyo-file-worker`——同一 `soyo` 二进制的私有 argv，在沙箱内运行，经 stdio 有界 JSON RPC（`WORKER_RPC_MAX_BYTES = 32 MiB`）执行**宿主已审批的单次**文件调用；权限决定（`BoundGate`）按宿主传入的 call/context/approved_target 逐字绑定，子进程本身不做权限判断。Linux 侧 `__soyo-sandbox-launch` 是 Bubblewrap PID 2 的可信 launcher，在 exec payload 前安装 Seccomp。`exec` 工具经 `PreparedCommand`＋进程组监督（`killpg`、默认 120s／上限 600s 超时）。
- **逐底层调用门禁链**（`DEV-095` 必须复用的路径）：运行时循环在派发前执行 `tool.before_dispatch` check hooks（可 `hook_denied` 阻断），随后 `dispatcher.authorize` 将调用分类为 `Operation` 并经权限管理器过 deny 规则、作用域根检查、审批（`needs_confirmation` 时 `interactive` 挂起等人工）、预算；`dispatcher.execute` 持 `OneShotPermit` 执行并落 `ToolIntent`/`ToolFinished` 证据与脱敏，`tool.finished` hooks 在循环侧触发。`restrict_to_read_only` 已支持隐藏全部副作用与浏览器工具（PLAN 复用同一机制）。
- **外部 Node 先例**：Browser worker 已把宿主 Node 安装树按 v2 身份 manifest（逐树 sha256）只读挂载进 Bubblewrap（`BrowserSandboxProfile.install_roots`）；沙箱配置 `toolchain_presets` 含 `"node"` 预设可把宿主 Node 工具链目录只读加入沙箱视图。
- **当前无 C 编译依赖**：`soyo-core` 仅 `libc` FFI 调用，无 `cc`/build-script 编译单元。
- **现状确认**：全仓无 `code_mode`/`CodeMode` 或任何 JS 编排实现；`OutputStore` 处理单次大输出持久化，与脚本中间结果驻留执行环境的分层独立（`D-082` 已要求）。

## 方案定义

评估发现「进程拓扑」与「JS 引擎选型」是两个可独立拍板的维度，拆分如下：

| 编号 | 方案 | 说明 |
| --- | --- | --- |
| **A1** | 宿主进程内嵌 Boa | 唯一成熟的纯 Rust JS 引擎（`boa_engine`），零 C 依赖，Test262 符合度约 94%（v0.21，2025-10） |
| **A2** | 宿主进程内嵌 QuickJS（`rquickjs`） | 进程内嵌入但含 vendored C 源码（经 `cc` 编译，约 1 MiB），资源限制 API 最完整；`D-082` 表述的「纯 Rust」字面上不含此项，因其资源治理优势保留在评估中供拍板参考 |
| **B1** | 沙箱内宿主 worker 子进程（同二进制嵌引擎） | 复用 `__soyo-file-worker` 模式新增 `__soyo-js-worker`，在 Seatbelt/Bubblewrap 内运行，引擎仍为 Boa 或 QuickJS——**可兼得进程内引擎选型与内核级隔离** |
| **B2** | 沙箱内外部运行时子进程（Node／Deno） | 复用 `toolchain_presets`/`install_roots` 只读挂载宿主运行时，仓库附带引导 JS 做 RPC 客户端 |
| **C** | 受控外部 Node 子进程（对照基线） | 不强制进沙箱的 `node` 子进程，仅作能力与延迟参照，不作为推荐交付形态 |

`deno_core`/`rusty_v8`（进程内 V8）在初筛即出局：约 40 MiB 静态库、构建默认下载预编译二进制（与离线构建及供应链约定冲突）、Isolate 启动约 10 ms，体量与一个只读编排 REPL 明显不相称。

## 逐轴对比

| 轴 | A1 Boa（进程内·纯 Rust） | A2 QuickJS（进程内·含 C） | B1 沙箱内嵌引擎 worker | B2 沙箱内外部运行时 | C 外部 Node 对照 |
| --- | --- | --- | --- | --- | --- |
| 构建／体积 | 纯 Rust 依赖树（engine/gc/parser/interner，ICU 可选），预计 +2–5 MiB；无 C toolchain | vendored C 经 `cc` 编译 +~1 MiB，无网络获取；引入仓库首个 C 编译单元 | 同 A1/A2（引擎仍在 `soyo` 二进制内），无额外分发物 | 零构建成本；依赖宿主已装 Node(~90 MiB)/Deno(~56 MiB) | 同 B2 |
| C 依赖 | 无 | QuickJS 单一 vendored C 库 | 取决于引擎选型 | 无（外部二进制依赖替代编译依赖） | 无 |
| OS 隔离 | 无内核隔离；能力面收敛于注册的只读桥函数（不注册则无 fs/net 入口），残余风险＝引擎内存安全漏洞与宿主地址空间资源耗尽 | 同 A1 | Seatbelt/Bubblewrap＋RLIMIT/cgroup 内核级隔离；崩溃与越权写被内核与文件系统视图阻断 | 同 B1（另依赖外部运行时自身健壮性） | 无强制隔离（对照） |
| 单次桥接延迟 | 进程内函数调用 µs 级 | 同 A1 | 进程启动一次性 ~10–50 ms＋每次调用管道序列化 ~0.1–1 ms | 同 B1（外部运行时冷启动 50–200 ms，Node 尤甚） | ~50–150 ms 冷启动 |
| 内存限制 | `RuntimeLimits::max_allocation` 限**单次**分配（默认 1.5 GiB）；**无总堆上限**（上游已记录缺口），栈/递归可限 | `set_memory_limit`（总堆硬上限）＋`set_max_stack_size`，引擎级最完整 | 引擎级上限＋`ResourceLimits.total_memory_bytes`/cgroup 双保险 | `ResourceLimits`＋`node --max-old-space-size` | 仅 `ResourceLimits`/命令行参数 |
| 指令／循环限制 | `loop_iteration_limit`（u64，调用边界与循环内检查，超限抛不可捕获 `RuntimeLimitError`）＋递归 512 | 无原生指令计数；以 interrupt handler 周期回调近似 | 引擎级＋`cpu_seconds`/cgroup CPU 带宽 | 仅 OS 级 CPU 限制 | 同 B2 |
| 超时／取消抢占 | 无任意中断回调；墙钟超时只能在循环检查点与宿主桥返回处生效，长同步段（如无循环的大 stringify）粒度受限——`while(true)` 可被迭代上限兜住 | `set_interrupt_handler` 周期性回调→不可捕获中断，墙钟超时与取消可任意抢占 | 墙钟＝监督器 `killpg`，最硬；引擎内限制叠加 | 同 B1 | 同 B1 |
| Hooks／逐调用门禁 | 同进程直接复用 `dispatcher.authorize`/`execute`＋`fire_hooks`；审批挂起需异步桥让出执行器 | 同 A1 | RPC 天然强制「权限决定全部在宿主」：worker 只做发起与结果转译；每条桥消息经同一 authorize/execute/hooks 链 | 同 B1 | 同 B1 |
| 崩溃与恢复 | 引擎 panic/崩溃拖垮宿主进程→run 失败关闭，代价高 | 同 A1（QuickJS 久经 LLRT 等生产嵌入） | 子进程崩溃被环境与租约收敛，run 收到失败结果而非进程级事故 | 同 B1 | 崩溃即调用失败 |
| 恢复语义 | REPL 本身为单次模型级 `ToolCall`，脚本意图/结果按现有 ToolIntent/ToolResult 持久化；只读使重放天然幂等；中间结果不落主上下文（`D-082` 与 `OutputStore` 分层） | 同 A1 | 同 A1；取消/超时 kill 后可按同一 call 重入 | 同 B1 | — |
| 运维面 | 全平台同一份行为；不依赖用户环境 | 同 A1 | 同 A1 | 版本漂移、最小安装缺失、跨平台差异；需沿用 manifest/探测式固定 | 同 B2 |

**延迟结论（如实记录）**：B 系每次桥接约 0.1–1 ms、启动约 10–200 ms；目标负载为多文件只读任务（典型几十次调用、秒级模型往返），该差距在两个数量级以内不构成决定轴；若未来出现万次级桥接的微基准负载再复核。

## 关键分析

### 1. 「逐底层调用门禁」在三类拓扑下语义等价，但 B 系结构性更强

`D-082` 要求脚本内每次底层调用逐次过 deny/根检查/Hooks/审批/预算/取消/脱敏，防止 `auto` 只审脚本文本被动态参数绕过。

- A 系：桥接是宿主函数回调，每次调用以新 `call_id` 构造 `ToolCall` 回到 `dispatcher.authorize`＋`execute`＋`check_pre_dispatch_hooks`/`tool.finished` 路径。审批挂起需要桥返回 Promise/让出执行器（Boa `JobExecutor`／rquickjs `AsyncRuntime` 均支持）。
- B 系：worker 把每次调用序列化为 RPC 请求发回宿主，宿主走完全相同的授权/执行/Hook 链后回传结果。**通道本身保证子进程无权短路**——worker 内不驻留任何权限逻辑，与 `__soyo-file-worker` 的 `BoundGate` 先例一致；这是 B 系相对 A 系的结构性优势，而非语义差异。
- 需要 `DEV-095` 设计的共同新点：桥消息→`ToolCall` 的构造（call_id、summary、interactive 标记、DispatchReview）、`WaitingInput`/审批挂起在脚本侧的呈现、每次桥调用的 ToolIntent/ToolFinished 证据归属（建议归属外层 REPL call 的 run，call_id 派生自脚本调用序号）。

### 2. 资源治理：Boa 的缺口与 QuickJS 的完整度

- **Boa**：`RuntimeLimits` 覆盖循环迭代（可在 `while(true)` 内兜底）、递归、栈、单次分配与 backtrace，超限抛不可捕获错误；但截至 v0.21/0.22 **无总堆上限、无任意中断 handler**——`"a".repeat(0xFFFFFFFF)` 类问题已由 `max_allocation` 修复单次分配，但「小源码膨胀大堆」（如数组持续增长）只能靠 OS 层 OOM 兜底；墙钟超时无法在密集同步段任意抢占。
- **QuickJS（rquickjs）**：`set_memory_limit`（总堆）、`set_max_stack_size`、`set_interrupt_handler`（周期回调→不可捕获中断）构成进程内最完整的内存/指令/超时三元组；AWS LLRT、Svix 等生产嵌入先例（Svix 实测每次运行开销 ~1 ms，替代 V8 后 11 ms→1 ms）。
- **B 系**：`ResourceLimits`（cpu_seconds、total_memory_bytes、total_processes、cpu_quota）＋`killpg` 监督提供内核级硬边界，引擎级限制退化为第二道防线——此时 Boa 的堆上限缺口被内核内存限额实质性兜底，**「纯 Rust」与「强资源治理」不再互斥**。

### 3. 隔离强度：与 `D-040`/`D-050` 原生隔离态势的一致性

- A 系脚本与宿主同地址空间：即便桥函数面收敛为只读，引擎自身漏洞或失控资源消耗直接影响整个 run 进程；定位「实验性、可撤回」时把故障域放大到宿主是不小的心理与实现负担。
- B 系沿用 N2 既有原生后端（D-040 Bubblewrap＋Seccomp、macOS 独立 SeatbeltBackend，D-050 默认原生隔离、不可用明确失败）：worker 仅持有一条回宿主的有界 RPC 通道，文件系统视图/网络/syscall/资源全部由既有策略承载；脚本能造成的最大破坏是 worker 自身崩溃——正好落在「实验性默认关闭」需要的可撤回故障域内。
- B2/C 的外部运行时把「引擎健壮性」外包给用户安装的 Node/Deno，且需在四平台处理发现、版本与固定（browser 侧 manifest v2 是既有先例但为重量级流程）；作为对照有价值，作为最小版本的交付面偏重。

### 4. 桥接与恢复的共同设计面

- **通道**：B 系需要把 `__soyo-file-worker` 的「单请求单进程」扩展为「长驻 worker＋每条消息一次底层调用」的有界 JSON-RPC（stdio 或 Linux loopback 桥接先例）；`WORKER_RPC_MAX_BYTES` 级别的消息上限沿用。
- **幂等与恢复**：最小版本只开放只读工具，脚本内调用无副作用，崩溃后整段脚本可安全重跑；建议 `DEV-095` 将「REPL 单次调用结果」作为唯一持久化单元，脚本内逐调用证据归属同 run、可观测但不单独恢复（与现有 ToolFinished 记录一致）。
- **opt-in 接合**：配置位＋工作模式资格（PLAN 只读语义契合，`restrict_to_read_only` 直接复用）；管理员/配置级关闭开关与「不因已安装 Hook 拒绝启动」按 `D-082` 直接落入设计。

## 待拍板决策点（建议录入 `D-083+`）

1. **进程拓扑**：A（宿主进程内，故障域＝整个 run 进程）还是 B（沙箱 worker 子进程，复用 N2 隔离与崩溃收敛）？本报告**倾向 B1**：与 `D-040`/`D-050` 原生隔离态势一致，通道天然强制逐调用宿主门禁，实验性故障域最小；代价是新增长驻 worker RPC 通道（约一个 worker 模块＋协议测试）。
2. **引擎选型**：若选 B1，Boa（纯 Rust 零 C 依赖；无总堆上限/中断回调，由内核限额兜底）还是 rquickjs（引入首个 C 编译单元；引擎级内存/栈/抢占最完整，可独立降级到 A 拓扑复用）？若坚持「纯 Rust」字面要求则为 Boa；若拍板允许 vendored C 则 rquickjs 资源治理更强。**两案在 B1 下都成立，差异主要在 C 依赖接受度**。
3. **外部运行时**：是否保留 B2（沙箱内 Node/Deno）作为后续可选后端？建议 `DEV-095` 最小版本只做 B1，`toolchain_presets` 先例留作扩展点不实现。
4. **桥协议形状**：脚本侧 API 同步式（顺序 await 语义由引擎 promise 承担）还是显式 `callTool()` 异步；以及中间结果驻留策略（脚本堆内，仅返回值/失败摘要回宿主并落一条 REPL 级 ToolResult）。
5. **默认预算**：`DEV-095` 的默认脚本超时、内存上限、单次脚本内底层调用数上限与最大桥消息字节的具体默认值（建议沿用 exec 的 120s/600s 形态与 worker 32 MiB 量级起评）。

## 证据来源

- 仓库代码：`crates/soyo-core/src/execution.rs`（SandboxPolicy/ResourceLimits/PreparedCommand）、`execution/worker.rs`（`__soyo-file-worker`/`__soyo-sandbox-launch`/`WORKER_RPC_MAX_BYTES`）、`dispatch.rs`（authorize/execute/read_only）、`runtime/mod.rs`（`tool.before_dispatch`/`tool.finished` 触发点）、`tools/exec.rs`（超时与 killpg 监督）、`browser/worker/environment.rs` 与 `docs/configuration.md`（Node install_roots/toolchain_presets）。
- Boa：[v0.21 release（94.12% Test262）](https://boajs.dev/blog/2025/10/22/boa-release-21)、[`RuntimeLimits` API](https://docs.rs/boa_engine/latest/boa_engine/vm/struct.RuntimeLimits.html)、[vm.md](https://github.com/boa-dev/boa/blob/main/docs/vm.md)、单次分配上限修复（`max_allocation`）。
- rquickjs/QuickJS：[`AsyncRuntime`/`Runtime` API（memory_limit/stack_size/interrupt_handler/promise hooks）](https://docs.rs/rquickjs/latest/rquickjs/struct.Runtime.html)、[Svix 迁移实践](https://www.svix.com/blog/improving-transformations/)（V8→QuickJS：~11ms→~1ms）。
- 体量参照：rusty_v8 说明（V8 60 万+行 C++、构建/预编译分发约束）、[deno 二进制构成](https://github.com/denoland/deno/issues/9243)、v8x 引擎体积表（quickjs-ng ~1 MiB 静态库）。
- 决策：`docs/decisions.md`（D-040、D-050、D-082）、`docs/n6-plan.md`（S1 核定边界与验收方式）、`docs/sandbox.md`（后端能力矩阵）。
