# Sandbox 与命令结构分析

**更新于 2026-11-08。Linux 技术组合已由项目负责人指定为 Bubblewrap + Seccomp + socat，macOS 将 sandbox-exec 封装为独立 SeatbeltBackend。具体 profile、接口与资源默认值仍为实现草案。**

## 后端边界

core 使用统一 `ExecutionBackend`，后端负责平台相关的准备、执行、文件操作与清理。Seatbelt profile、Bubblewrap 参数和 Seccomp BPF 留在各自实现中。

| 接口职责 | 输入／输出 |
| --- | --- |
| capabilities | 实际文件、网络、资源、清理及执行平台能力 |
| prepare | 已批准策略、工作区映射、凭据引用、有限资源要求，返回 EnvironmentHandle |
| execute／files | 结构化 argv 或文件操作、cwd、取消及调用归属，返回结果与输出引用 |
| inspect／terminate／cleanup | 环境身份、拥有的资源、实际终止与清理状态 |

初始后端为 `SeatbeltBackend` 和 `BubblewrapBackend`，模块可放在 core 的 execution 边界。后续 `ContainerBackend`／`VmBackend` 可实现同一接口，明确 guest OS、架构、路径映射、凭据传递和资源语义。后端选择与降级依据显式配置及能力要求，必需约束缺失时返回 `sandbox_capability_unavailable`。

模型上下文分别注入宿主环境与实际执行环境，工具使用逻辑工作区路径，由后端完成受控映射。策略快照绑定环境及调用版本，新操作按当前版本准备，旧写入任务在模式切换边界终止或结束，后续权限通过新的环境快照生效。

## 控制链

模型参数经过类型校验、OperationDescriptor、必要命令结构分析、工作模式／规则／核查／确认，再保存执行意图并交给选定后端。结果和清理状态经 Store／OutputStore 保存。

AST 分析提供静态风险信息，Sandbox 负责执行期约束。确认绑定 argv、cwd、脚本身份、策略及分析器版本，执行前重查。

## Linux 原生实现

| 组件 | 职责 |
| --- | --- |
| Bubblewrap | user／mount／PID／IPC／network 等命名空间、文件系统视图与只读／可写挂载 |
| Seccomp | 按架构和 profile 约束系统调用及可检查参数 |
| socat | 将隔离环境中的代理入口转接到宿主专用 Unix socket |
| Rust NetworkBroker | HTTP CONNECT／SOCKS 请求解析、目标和域名策略、认证引用、连接与时间额度 |
| ProcessSupervisor | 子进程、取消、超时、桥接生命周期与实际清理 |
| 可用的 cgroup v2 | 已委派 controller 支持的 CPU、内存和进程数硬限额 |

Bubblewrap 从最小文件系统视图构建环境，按需要绑定运行库、工具链、工作区和临时目录，PID 命名空间及其 init 管理后代进程。实际约束由具体配置决定。[Bubblewrap](https://github.com/containers/bubblewrap)

宿主将受控 BPF 通过文件描述符交给 Bubblewrap，payload 在过滤生效后启动。过滤器校验 x86_64／aarch64 的 syscall 架构，处理 ABI 差异，限制 namespace 逃逸、ptrace、内核／设备操作及其他高风险入口。Chromium 所需调用使用单独版本化 profile 验证。Seccomp 处理调用号及可检查参数，文件路径由文件系统策略承担。[Seccomp BPF](https://www.kernel.org/doc/html/latest/userspace-api/seccomp_filter.html)

启动创建独立 session，关闭未声明文件描述符，覆盖 TIOCSTI 等终端相关入口。浏览器 profile 允许其施加自身更严格的 Seccomp，必要的 namespace 调用与外层限制共同验收。

首轮采用用户命名空间的非特权启动，安装的 bwrap／socat 路径、版本及文件身份记入诊断。内核、发行版 LSM 或用户命名空间限制在 prepare 中探测，明确失败原因。

### 网络代理桥

```text
payload HTTP_PROXY / SOCKS
  -> 私有 network namespace 中的 loopback 入口
  -> socat TCP-to-Unix 转接
  -> 仅该环境可见的 Unix socket
  -> Rust NetworkBroker 的目标策略
  -> 获准目标
```

network namespace 的外部连接被隔离，socat 仅转发到该 Run 的代理 socket。宿主代理 socket 位于私有目录，控制通道与模型／文件 RPC 分开。直连 TCP／UDP、DNS、QUIC、IPv6、Unix socket 与代理旁路均纳入测试。

NetworkBroker 对规范化 origin、解析后的目标 IP、重定向、DNS 变化、内部／元数据地址、认证绑定和连接限额逐项检查。HTTPS CONNECT 的基本粒度为主机／端口，HTTP 方法和路径规则需要相应应用层处理。本地开发服务使用用户显式绑定的单一转发目标。

原生隔离与 Unix socket 桥接的工程实践可参考 Anthropic 的 sandbox-runtime，其默认策略与实现细节需分别核对。[Anthropic sandbox-runtime](https://github.com/anthropics/sandbox-runtime)

## macOS SeatbeltBackend

固定使用系统 `/usr/bin/sandbox-exec`，将类型化策略编译为受控 SBPL 和路径参数。后端处理可读／可写根、保护路径、必要系统库／服务、exec／fork、信号与网络规则，子进程继承实际限制。

网络策略仅允许连接本次受控代理入口，再由 NetworkBroker 检查外部目标。proxy 环境变量由宿主设置，直接外联、额外本地服务和任意 Unix socket 分别测试。Rust 应用与其他后端只依赖统一策略与能力接口。[Codex Seatbelt 实现](https://github.com/openai/codex/blob/main/codex-rs/sandboxing/src/seatbelt.rs)

2026-11-08 在本机 macOS 27.2 读取的 `man sandbox-exec` 将该命令标记为 deprecated，系统当前提供该文件。macOS 15 ARM／Intel 的可用性、SBPL 行为和后续系统兼容性需单独实测。保留后端替换接口用于未来经验证的容器、VM 或平台方案。

## 文件策略与性能

默认使用原生路径约束和按需 bind，运行库、工具链与工作区直接复用。开销来源包含策略准备、进程启动、代理桥及 syscall 检查，实际性能由验收数据记录。

- macOS 将规范化路径及保护规则编译进 Seatbelt profile。
- Linux 根据允许根及更具体的只读／拒绝路径构建 mount 图，系统目录按最小可用集提供。
- 缺失保护路径的 mask 在私有视图中处理。需要修改宿主工作区才能建立 mask、或无法表达动态拒绝规则时，返回 `unsupported_fs_policy`，明确约束原因。
- 内置读写／搜索通过同一 ExecutionBackend 的文件服务执行。文件操作继续验证预期版本和单文件替换，通用命令在获准可写根中运行。
- 受限文件服务、插件目录、凭据目录、应用数据及控制 socket 的范围由宿主策略生成，模型参数只选择已允许资源。

通用程序写入与外部编辑可能并发，执行记录保存实际结果。DEV-064 增加 Diff、检查点和恢复，具体文件一致性保证在该层验收。

## 资源与清理能力矩阵

| 能力 | Linux | macOS |
| --- | --- | --- |
| 文件／网络执行约束 | 命名空间、mount、Seccomp 与代理组合 | Seatbelt profile 与代理组合 |
| 单进程 CPU／文件／FD 限额 | 可验证 rlimit | 可验证 rlimit |
| Run 总内存／CPU／进程数硬限额 | 依赖已委派 cgroup v2 controller | 初始后端标记为未提供 |
| 超时与超额监测 | supervisor，结合可用 cgroup | supervisor 监测与终止，标记为尽力控制 |
| 脱离进程组的后代清理 | 私有 PID namespace／cgroup 归属验证 | 进程追踪与进程组清理，无法确认的结果保留 unknown |

`RLIMIT_CPU` 按进程计 CPU 时间，`RLIMIT_NPROC` 按 UID 计数，跨进程汇总能力独立报告。资源属性带 `hard`／`best_effort`／`unsupported`，用户要求的硬限制缺失时 prepare 失败。[cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html)

## AST／CST 分析

普通 exec 首先分析直接 argv、可执行程序、cwd 和环境引用。`sh -c`／`bash -c` 等显式入口再解析脚本文本，首轮候选为 Tree-sitter Bash，将 CST 转换为风险事实。[Tree-sitter 基础解析](https://tree-sitter.github.io/tree-sitter/using-parsers/2-basic-parsing.html)

| 结构 | 提取信息 |
| --- | --- |
| 简单命令与包装器 | 实际程序、参数、环境和解释器 |
| 管道、条件、后台、重定向 | 分支、输入／输出目标及组合关系 |
| 变量、命令替换、脚本入口 | 可确认信息与 `effects_unknown` |
| 错误语法、未知方言或分析超限 | `analysis_incomplete` 和原因 |

分析器具有输入、时间及内存上限，解析错误显式进入结果。Python／Node inline code、构建脚本和动态加载仍保留不确定性。仓库级源代码扫描由 F-17 或受控工具／Hook 按需提供。

`auto` 使用规则、结构化事实、独立模型核查和人工兜底，`manual` 逐工具批准，`bypass` 仍经过 PLAN、显式 deny 与选定后端硬边界。脚本及其描述以低信任数据进入审核，解析结果与实际执行共同留有版本关联。

## 取消、恢复与验收

取消先关闭新调度，再终止所属工具、worker 与 socat。模式切换按当前调用边界协调，进入 PLAN 前清理后台写入。记录环境身份、PID／cgroup、调用结果及实际清理，未知副作用等待核对。

DEV-055／056 先建立能力探测、Seatbelt 独立实现及 Linux 组合后端，DEV-057／075 覆盖路径越界、链接竞态、缺失保护路径、动态规则、父进程信号、进程脱离、syscall profile、网络旁路、资源缺失和清理失败。

性能测试记录无隔离基线与原生后端的冷／热启动、短命令 p50／p95、文件读写、搜索、并发任务、CPU／RSS 和代理延迟。实际 Seatbelt、Bubblewrap、Seccomp 和网络约束在具备相应后端的环境验收，CLI 的跨平台替身测试单独记录。

任务分解见[后续开发计划](post-mvp-plan.md)。
