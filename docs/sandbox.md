# Sandbox 与命令结构分析

**更新于 2026-11-09。N2 开发线已接入 Linux Bubblewrap + Seccomp + socat、独立 macOS SeatbeltBackend 和受控网络 broker。本文说明当前实现及能力边界，固定源码的四平台整体验收已通过，证据见 [N2 验收记录](n2-validation.md)。**

## 后端边界

core 使用统一 `ExecutionBackend`，当前 `NativeBackend` 按平台选择独立 Seatbelt 实现或 Linux Bubblewrap 实现。`prepare` 返回 `PreparedEnvironment`，Seatbelt profile、Bubblewrap 参数和 Seccomp BPF 留在对应后端中。[执行实现](../crates/saya-core/src/execution.rs)

| 接口职责 | 输入／输出 |
| --- | --- |
| `ExecutionBackend::prepare` | 工作区、读写根、保护路径、网络和资源策略，返回准备环境或明确错误 |
| `PreparedEnvironment::identity`／`capabilities` | 环境 ID、宿主与执行 OS、架构、策略摘要及实际能力等级 |
| `prepare_command`／`file_worker_argv` | 受约束命令及私有文件 worker 启动参数，由共享进程监督器处理 stdio、取消和超时 |
| `wait_for_process_cleanup`／`cleanup` | 已监督进程的结束确认、环境关闭和所属临时资源／cgroup 清理结果 |

默认 `backend = "auto"` 在 macOS 选择 Seatbelt，在 Linux 选择 Bubblewrap。运行开始时实际探测后端，必需约束缺失时返回 `sandbox_capability_unavailable` 等明确错误。后续容器／VM 实现可沿用该接口，并提供实际 guest OS、架构、路径映射、凭据传递和资源语义。配置语法和实际准备分别处理，见[原生隔离配置](configuration.md#n2-原生隔离配置)。

TUI 空闲时可使用 `/sandbox on` 将当前进程的后续 Run 切换为原生 Auto 后端，`/sandbox off` 切换为显式 Host 兼容后端，`/sandbox status` 显示当前选择。该命令不写回配置文件，运行活跃或 UI 操作进行中拒绝执行。关闭沙箱只移除原生文件、网络和 syscall 隔离；权限模式、显式 deny、PLAN 只读边界、凭据存储拒绝及 Host 能力限制继续由原执行链处理。

`host` 是显式选择的兼容后端，文件系统、网络和 syscall 隔离能力报告为 `unsupported`，进程监督按实际能力报告。Host 支持现有单进程 rlimit，聚合资源能力为 `unsupported`。显式提供 `cgroup_root` 或请求任一聚合限制时，准备返回 `sandbox_capability_unavailable`。Host 缺少文件系统边界，同 UID 进程仍可访问其有权写入的其他 cgroup，聚合硬约束要求原生隔离。

底层准备继续拒绝要求保护路径或代理隔离的 Host 策略。共享应用在启用凭据引用或已有私有凭据存储时拒绝 Host 启动，私有存储和默认保护路径见[MCP 与凭据管理](mcp.md#私有存储与执行边界)。权限模式和后端选择分别生效。

模型上下文分别注入宿主信息与实际执行环境身份、能力及 MCP 连接事实。当前原生后端采用规范化宿主路径，策略快照绑定本次 Run 的环境与调用。模式和授权在调用边界复查，进入 PLAN 的清理屏障覆盖后台写入及本地服务。

Linux默认系统视图额外只读映射存在的 `/etc/alternatives`，其目录内容可能包含实际文件。系统工具别名的目标仍通过当前可见根解析。固定系统根保存宿主规范路径与guest路径映射，保护规则覆盖每个别名及受保护祖先下的整个挂载根。遮罩去重并在所有读写根之后应用，避免独立的 `/bin`／`/lib` 视图保留同一保护对象。其余 `/etc` 内容按既有选择性视图提供。

## Linux匿名Unix通道的内核条件

Rust子进程启动使用已连接的匿名AF_UNIX SOCK_SEQPACKET通道，异步运行时还使用SOCK_STREAM通道。payload策略按这两类匿名pair限定协议及flags，命名Unix socket与datagram继续受限制。原peer间的SCM_RIGHTS可以复制发送方已有的FD，宿主描述符通过两层CLOEXEC清理与私有启动协议隔离。

部分旧内核的shutdown及peer-close处理曾改变连接状态，匿名pair可能因此重新connect／listen。当前后端同时采用明确的正式版本范围和私有监听器行为探测，检测到重定向能力、未知行为或无法确认时返回 `sandbox_capability_unavailable`。范围为5.15分支≥5.15.86、6.1分支≥6.1.2，或正式6.2及以后版本。版本条件用于约束已知竞态修复身份，有限行为探测用于核对可观测语义。[peer-close修复](https://github.com/torvalds/linux/commit/3ff8bff704f4de125dca2262e5b5b963a3da1d87)、[shutdown回补](https://github.com/gregkh/linux/commit/80d709875d920f7ca959040457b7393df706fe44)

peer-close分支先确认完整 `POLLHUP`，再执行发送及重定向验证。并行fork可能短暂继承CLOEXEC peer，所有关闭分支共享一秒绝对截止，超时或未知状态返回失败。描述符无效、迟到HUP和中断均按同一截止处理，探测记录实际发送返回值与errno用于诊断。[探针验收记录](n2-validation.md#匿名通道关闭引用的探测同步)

5.4／5.10本轮未纳入内核确认矩阵。厂商 `5.15.0-xxx` 无法仅凭版本串确认对应上游回补，RC和未知版本按必需能力无法确认处理。能力探测、Rust fork路径与原生防重定向回归已在本轮 Linux x86_64／aarch64 的实际程序上通过，见[N2记录](n2-validation.md)。

## 控制链

模型参数经过类型校验、可信操作描述、必要命令结构分析、工作模式／规则／核查／确认，再保存执行意图并交给选定后端。结果和副作用状态由 Store 保存，文件 worker 与命令继续遵循当前输入、输出和时间上限。

AST 分析提供静态风险信息，Sandbox 负责执行期约束。确认绑定规范化 argv、cwd、有效环境摘要、执行环境／策略身份及分析器版本与限额，执行前重查。

## Linux 原生实现

| 组件 | 职责 |
| --- | --- |
| Bubblewrap | user／mount／PID／IPC／network 等命名空间、文件系统视图与只读／可写挂载 |
| Seccomp | 按架构和 profile 约束系统调用及可检查参数 |
| socat | 将隔离环境中的代理入口转接到宿主专用 Unix socket |
| Rust NetworkBroker | HTTP absolute-form／CONNECT 解析、精确 origin、DNS 与目标地址核查、连接／字节／时间额度 |
| 共享进程监督器 | 命令、文件 worker、MCP 与桥接进程的取消、超时及结束确认 |
| 可用的 cgroup v2 | 已委派 controller 支持的 CPU、内存和进程数硬限额 |

Bubblewrap 从私有文件系统视图构建环境，默认绑定系统运行目录、配置的读写根、私有 launcher 和临时目录，PID 命名空间及其 init 管理后代进程。当前视图省略 `/proc`，限制通过 procfs 访问进程内存、描述符和根目录的渠道。依赖 procfs 的程序需要按此执行视图评估兼容性。后端要求 Bubblewrap 0.13.1 或更高版本，实际依赖版本与哈希由验收记录关联。[Linux 后端](../crates/saya-core/src/execution/linux/mod.rs)、[Bubblewrap](https://github.com/containers/bubblewrap)

宿主将基础 BPF 通过文件描述符交给 Bubblewrap，可信 launcher 在执行 payload 前安装更严格的过滤器。过滤器校验 x86_64／aarch64 的 syscall 架构和 ABI，约束 namespace、ptrace、内核／设备操作及 socket 类型。payload 过滤器允许协议为0的 `AF_UNIX`／`SOCK_STREAM` 与 `SOCK_SEQPACKET` 匿名已连接pair，以及 `SOCK_CLOEXEC`／`SOCK_NONBLOCK` flags，用于运行时self-pipe和Rust fork结果通道。命名Unix socket创建及datagram pair受过滤器拒绝，匿名pair同时受前述内核身份与行为探测约束。Seccomp 检查调用号及可见参数，路径访问由文件系统视图承担。[Seccomp BPF](https://www.kernel.org/doc/html/latest/userspace-api/seccomp_filter.html)

启动创建独立 session，执行前规范化未声明描述符的 close-on-exec 标志，并约束 TIOCSTI 等终端相关入口。宿主固定私有 launcher 镜像，通过有界、解除目录链接的文件描述符传入实际环境值。

Linux 的 Browser 专用 profile 保留通用 payload 过滤器，额外提供嵌套 user namespace、私有 `/proc`、`/dev/shm` 与非 root 映射身份，并只读绑定宿主显式给出的 Node 父目录、worker root 与 Chromium 包目录；profile／output／tmp／work／config 五个可写根都位于该 environment 的 Run 私有 scratch，不绑定 workspace、HOME、`.saya` 或凭据目录。Browser profile 的 `mount` 参数检查读取 seccomp_data 的 args[3]（offset 40，即 mountflags），`umount2` 读取 args[1]（offset 24）；这样合法 mount flags 通过，`MS_MOVE` 等被拒绝。Chromium 自身的 sandbox 不能只依赖 Bubblewrap 固定赋予的 `NoNewPrivs=1`、`Seccomp=2`：provider 还必须取得 Chromium 自有的 `InitializeSandbox` stderr 证据，否则在首次真实启动后返回 `browser_sandbox_unavailable`。macOS Seatbelt 无法在 Chromium 重初始化其 sandbox 时同时保持外层限制，因此浏览器环境明确返回 `browser_sandbox_unavailable`。组合行为与平台证据见 [Browser 阶段](browser.md)。

按 D-066，专用 VM 后端不把 Chromium 放进宿主 Seatbelt／Bubblewrap browser profile，而是在 VM 内保留 Chromium 自有 sandbox；宿主只运行受控 `ssh`，Auto 按平台选择 Seatbelt 或 Bubblewrap。`read_files` 精确只读绑定 `ssh`、identity 与 known-hosts 三个文件，`write_roots` 仅一个 0700 Run 私有目录。`LoopbackPort { host, port, proxy_port }` 保留归一化 SSH 字面地址，可额外连接本次 broker 的 `127.0.0.1` 端口；Linux 精确桥接指定地址，Seatbelt 仍按 localhost TCP 端口限制。canonicalize 后的 identity／known-hosts 路径重新接受空白、控制字符与引号校验；`[::1]` 归一化为裸 `::1`，不做 IPv4 回退。通用默认仍为 `Deny`，不增加额外文件；Host 对该用途保持拒绝。

Linux 宿主通道继续使用私有 netns 与通用 Seccomp。固定 socat 镜像在 payload 启动前建立最多两条私有桥，启动记录不超过 16 KiB，IPv6 listener 显式 v6-only。宿主 relay 只转发到固定 SSH／broker SocketAddr，不解析调用方目标；最多 32 个在途连接、每方向 16 KiB 缓冲、3 秒 connect 期限。SSH 桥允许服务端先发 banner；HTTP broker 桥只在收到请求首字节后连接上游，空就绪探测不会占用累计连接额度。socket 为 0600，位于工作区与可写根之外；LoopbackPort 私有目录使用保留完整 UUID 位的 URL-safe 无填充 Base64，socket 采用短名，真实 prepare 已验证 64 字节 TMPDIR，不增加路径或后端回退。显式清理、Drop 和 prepare 后续失败均先停止并回收 relay，再按 socket 类型、device／inode 核对并删除所属路径。路径替换后不直接删除替换目录项；这一身份检查不提供针对任意并发宿主操作的原子 unlink 保证。命名 AF_UNIX 与匿名 pair 的内核／过滤器边界不变，host relay FD 不进入 payload 白名单。SSH 所需的 passwd／group／nsswitch.conf 为只包含当前数值身份的合成只读文件，不绑定宿主账号库；其他通用策略不增加这些文件。受控 Linux 与非 root 验证、环境前提失败记录见 [Linux 宿主通道](n5-validation.md#linux-vm-宿主通道增量)。

VM 内 renderer sandbox 证据由受控远端命令读取 VM `/proc`，不使用宿主 `/proc`；VM 内普通进程没有 `NoNewPrivs`／`Seccomp`，因此 renderer 上的这两个属性在该环境是可判别证据。实现与真实 VM 证据见 [Browser VM 扩展](browser.md#dev-080-vm-后端扩展)。

当前采用用户命名空间的非特权启动，从固定候选路径检查 bwrap／socat 的文件权限与身份，并在 prepare 中探测 namespace、Seccomp 和策略可用性。内核、发行版 LSM 或依赖约束导致的失败返回明确原因。

### 网络代理桥

```text
payload HTTP_PROXY / HTTPS_PROXY
  -> 私有 network namespace 中的 loopback 入口
  -> socat TCP-to-Unix 转接
  -> 仅该环境可见的 Unix socket
  -> Rust NetworkBroker 的目标策略
  -> 获准目标
```

network namespace 隔离外部连接，socat 将该环境的 loopback HTTP 代理入口转接到本 Run 的专用 Unix socket。宿主 socket 位于工作区与可写根之外的私有目录，控制通道与模型／文件 RPC 分开。直连 TCP／UDP、DNS、IPv6、Unix socket 和代理端口旁路属于原生验收范围。

Linux私有network namespace内的单向UDP发送可以返回成功。验收要求已通过宿主正控制的监听器收到零个payload数据包，代理端口分别验证IPv4和IPv6。macOS的UDP探针另外要求Seatbelt拒绝系统调用。外部DNS解析的未测范围按[N2验收记录](n2-validation.md#udp-传输边界判据)维护。

桥接socat在宿主按打开的FD核对root所有、权限与可执行性，固定为私有0500镜像并以只读FD挂载。guest按精确device／inode和只读可执行权限复查，身份校验跨user namespace的UID映射生效。准备和复制失败均关闭该执行路径。[代理镜像验收](n2-validation.md#linux-代理桥接镜像身份)

NetworkBroker 实现 HTTP absolute-form 与 CONNECT。每次新上游连接核对精确 origin、重新解析全部目标地址并固定所连接的 socket 地址，默认拒绝私有／本地／元数据地址。HTTP 请求采用有界 framing，校验 Host 与目标一致性，剔除代理专用认证头，并限制连接数、字节量和时间。客户端跟随重定向后产生的新代理请求继续接受相同核查。[网络实现](../crates/saya-core/src/network.rs)

HTTPS CONNECT 的授权粒度为主机／端口，隧道中的加密方法和路径保持不透明。双栈 loopback listener 占有同一代理端口，进程与桥接收尾先于代理释放。本地开发服务需额外配置单一 origin 例外，并核对 loopback 解析结果。

原生隔离与 Unix socket 桥接的工程实践可参考 Anthropic 的 sandbox-runtime，其默认策略与实现细节需分别核对。[Anthropic sandbox-runtime](https://github.com/anthropics/sandbox-runtime)

## macOS SeatbeltBackend

固定使用系统 `/usr/bin/sandbox-exec`，将类型化策略编译为受控 SBPL 和路径参数。后端处理可读／可写根、保护路径、必要系统库／服务、exec／fork、信号与网络规则，子进程继承实际限制。

网络策略仅允许连接本次受控代理端口的 TCP，再由 NetworkBroker 检查目标。proxy 环境变量由宿主设置，同端口 UDP、直接外联、额外本地服务和 Unix socket 访问分别进入验收。profile 明确拒绝其他进程的 PID 信息读取，保留自身描述符处理所需权限，并为 `/tmp`、`/var`、`/etc` 根 symlink 提供读取规则。[Seatbelt 实现](../crates/saya-core/src/execution/seatbelt.rs)

本机 macOS 27.2 的 `man sandbox-exec` 将该命令标记为 deprecated，当前独立后端使用系统提供的程序。macOS 15 ARM／Intel 的实际集成程序已通过验收，源码与程序证据见 [N2 验收记录](n2-validation.md)，后续系统兼容性通过后端接口继续验证。

## 文件策略与性能

默认使用原生路径约束和按需 bind，运行库、工具链与工作区直接复用。开销来源包含策略准备、进程启动、代理桥及 syscall 检查，实际性能由验收数据记录。

- macOS 将规范化路径及保护规则编译进 Seatbelt profile。
- Linux 根据允许根及更具体的只读／拒绝路径构建 mount 图，系统目录按最小可用集提供；默认继续省略 `/proc` 与宿主 `/etc/passwd`，但当配置显式启用 `sandbox.ca_certs` 或 `sandbox.toolchain_presets`（如 `"auto"`、`"rust"`、`"python"`、`"node"`、`"homebrew"`、`"xcode_clt"`、`"minimal_etc"`）时，运行准备阶段会只读追加存在且安全（排除 `/`、`$HOME`、`.saya`）的工具链／CA 目录，并在 Linux 按需生成仅含当前数值 UID／GID 的合成 `/etc/{passwd,group,nsswitch.conf}`。
- Linux 对既有保护路径建立私有拒绝视图，可写根下缺失的保护路径返回 `unsupported_fs_policy`。macOS 将缺失路径的拒绝规则编译进 profile，规则覆盖随后创建的目标。
- 七种内置文件工具通过同一准备环境启动私有 worker。宿主负责意图、授权和结果记录，worker 复核目标、版本及单文件替换；实际 RPC 请求和响应上限为 32 MiB。通用命令在获准可写根中运行。
- 宿主把启动时已存在的配置、data、state、cache 和工作区 `.saya/local` 加入原生保护集合，活动凭据引用使用的实际私有存储另行强制保护。`.saya/skills` 的正文读取继续经过文件权限与后端根策略，发现过程使用[项目上下文规则](project-context.md)。
- 仓库控制数据与工作区文件分开保护：内置文件工具把 `.git` 下的 `config`、`config.worktree`、`hooks/`、`info/` 及 worktree `gitdir` 链接指向的控制目录整体视为保护路径，任何权限模式下均拒绝读写；原生后端只对同一集合追加 deny-write——Seatbelt 编译对应写入拒绝，Bubblewrap 对这些路径叠加只读重绑定。沙箱内 `git status`、`git commit` 等常规操作保持可用，而 `git config`、hook 改写与 fsmonitor/filter 类自动命令注入被拒。`.git` 之外的普通文件不受影响。`HOME` 本身位于可写根内时，`~/.ssh`、`~/.gitconfig`、`~/.config/git` 及常见 shell 启动文件按同一集合处理。

通用程序写入与外部编辑可能并发，执行记录保存实际结果。DEV-064 增加 Diff、检查点和恢复，具体文件一致性保证在该层验收。

## 资源与清理能力矩阵

| 能力 | Linux Bubblewrap | macOS Seatbelt |
| --- | --- | --- |
| 文件／网络执行约束 | 命名空间、mount、Seccomp 与代理组合 | Seatbelt profile 与代理组合 |
| 单进程 CPU／文件／FD 限额 | 按显式资源设置应用 rlimit | 按显式资源设置应用 rlimit |
| Run 总内存／CPU／进程数硬限额 | 需要原生文件系统边界及可写、controller 完整的显式委派 cgroup v2 子树 | `unsupported`，要求该约束时 prepare 失败 |
| 超时与取消 | 共享进程监督器及所属 PID namespace／cgroup | 共享进程监督器与进程组清理 |
| 后代进程清理能力等级 | Bubblewrap 后端报告 `hard` | `best_effort`，清理依赖所属进程组，脱离进程组的后代结果需单独核对 |

`cpu_seconds`、`file_size_bytes` 与 `open_files` 是可选的单进程设置，在原生及 Host 后端沿现有 rlimit 路径生效，`RLIMIT_CPU` 按进程计 CPU 时间。聚合设置采用 `total_memory_bytes`、`total_processes` 和 `cpu_quota`，资源等级分别报告为 `hard`／`best_effort`／`unsupported`。

Linux 仅在宿主显式提供的 cgroup v2 委派根下创建本次环境的独有子目录，核对 controller 文件并回读已请求的限制，加入其拥有的 payload，结束时 kill、等待空组并移除该目录。系统 controller 的委派和启用由宿主准备。用户请求的聚合硬限制缺少必要委派时 prepare 失败。[资源实现](../crates/saya-core/src/execution/cgroup.rs)、[cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html)

Linux 原生后端依据宿主 mountinfo 拒绝与任何 `cgroup`／`cgroup2` 挂载存在祖先或后代交集的可写根，返回 `unsupported_fs_policy`。检查覆盖工作区、`write_roots`、`permissions.additional_roots`、委派根的兄弟路径及 bind 挂载别名。`read_roots` 可提供只读 cgroup 视图，其他根与保护规则继续检查。[文件系统检查](../crates/saya-core/src/execution/linux/mod.rs)

委派验收保留内核限制读回、活动成员与自有子组清理，并在同一专属委派范围内增加兄弟可写组和 Host 配置的模型派发前拒绝对照。本轮 Linux x86_64／aarch64 的委派正负对照已通过，资源耗尽压力仍未验证。[N2 验收记录](n2-validation.md)

## AST／CST 分析

DEV-075使用固定Tree-sitter Bash处理直接argv、可执行程序、cwd和环境引用。`sh -c`／`bash -c`等显式入口解析脚本文本，将CST转换为风险事实，具体边界及预算见[命令分析](command-analysis.md)。[Tree-sitter基础解析](https://tree-sitter.github.io/tree-sitter/using-parsers/2-basic-parsing.html)

| 结构 | 提取信息 |
| --- | --- |
| 简单命令与包装器 | 实际程序、参数、环境和解释器 |
| 管道、条件、后台、重定向 | 分支、输入／输出目标及组合关系 |
| 变量、命令替换、脚本入口 | 可确认信息与 `effects_unknown` |
| 错误语法、未知方言或分析超限 | `analysis_incomplete` 和原因 |

分析器具有输入、时间及内存上限，解析错误显式进入结果。Python／Node inline code、构建脚本和动态加载仍保留不确定性。仓库级源代码扫描由 F-17 或受控工具／Hook 按需提供。

`auto` 使用规则、结构化事实、独立模型核查和人工兜底，`manual` 逐工具批准，`bypass` 仍经过 PLAN、显式 deny 与选定后端硬边界。脚本及其描述以低信任数据进入审核，解析结果与实际执行共同留有版本关联。

## 取消、恢复与验收

取消先关闭新调度，再收尾所属工具、worker、MCP 与 socat。模式切换按当前调用边界协调，进入 PLAN 前清理后台写入与本地 MCP 服务。执行环境由活动进程租约保留，无法确认清理时保存 `outcome_unknown`，相关环境与代理资源继续按清理屏障管理。

DEV-055 至 DEV-057 及 DEV-075 已完成统一环境、独立原生后端、网络 broker 与命令结构事实，固定源码的四平台源码门禁和原生验收通过。验证覆盖路径越界、链接竞态、缺失保护路径、规则与授权变化、父进程信号、进程脱离、syscall profile、网络旁路、资源缺失和清理失败，实际完成状态由 [N2 验收记录](n2-validation.md)维护。

四平台性能测试已记录无隔离基线与原生后端的冷／热启动、短命令 p50／p95、文件读写、搜索、并发任务、CPU／RSS 和代理延迟。实际 Seatbelt、Bubblewrap、Seccomp 和网络约束在具备相应后端的环境验收，CLI 的跨平台替身测试单独记录。

四平台性能证据v2保留原始样本与nearest-rank分位数。冷启动使用新CLI进程与新Run，保留OS页缓存。热启动定义为同一已准备Run／环境内预热一次后的重复命令，计时覆盖模型响应发送完至后继工具结果到达，包含调度、进程启动、持久化及loopback传输。`file_roundtrip`沿用读取指标名称，写入和搜索分别记录实际内容与匹配证明。四个并发Run分别保留wait4资源，并汇总CPU及各child内核峰值RSS的最大值。

代理延迟由payload的 `CLOCK_MONOTONIC` 测量Host直连、原生HTTP代理与CONNECT转发的完整请求RTT，CONNECT样本采用隧道内明文HTTP。CLI整体资源另行记录。样本数支持1至20，CI固定每组3次，独立[性能核验器](../scripts/n2/verify_performance.py)重算分位数、比较值、资源汇总与实际操作证明。[完整证据范围](n2-validation.md#完整性能证据补充)

任务分解见[后续开发计划](post-mvp-plan.md)。

## N3 多入口共享网络预算

`RunNetworkBudget`为base broker及各插件entry listener提供同一Run的连接并发、累计连接、总字节和固定截止时间。origin、DNS／IP校验及更严格的局部限额继续按各listener生效。字节在转发前预留，失败预留保守计入累计额度，活动连接槽沿Drop归还。6项跨listener预算回归及既有网络回归、严格全目标Clippy通过，实际HTTP／原生平台证据按N3记录维护。

## DEV-081A 专用 VM 浏览器网络

专用 VM 后端的页面流量不经过 VM 默认出口。宿主为每个浏览器 worker 生命周期启动一个 `NetworkBroker`，沿用 `RunNetworkBudget` 的连接、字节和截止时间额度；`[browser.network]` 的 `allowed_origins`、`public_web` 与 `localhost_development_origin` 决定逐连接 origin／DNS／地址检查。空 allowlist 且 `public_web=false` 时拒绝启动，`public_web=true` 也只接受完整检查通过的公开地址。通用 broker 默认值没有改变。

宿主 SSH 通道只增加一条反向 streamlocal 转发，指向本次 broker 的 loopback TCP 端口；Seatbelt 只额外允许该端口，以及原有的 Colima 转发 SSH 端口。VM 内的 `127.0.0.1:31281` 属于私有 netns，不是宿主通道端点。VM 内在 `sudo -n unshare -n` 创建的私有网络命名空间中启用 `lo`，确认没有其他路由，由原用户启动 `socat` 把 `127.0.0.1:31281` 桥接到该 Unix socket，随后 worker 以 `setpriv` 回到原 uid／gid 运行。Chromium 保持自有 sandbox，并通过固定代理、`--proxy-bypass-list=<-loopback>`、禁用 QUIC／非代理 WebRTC UDP、`EXCLUDE 127.0.0.1` 的 host resolver 规则和 `--block-service-workers` 约束所有页面连接。

`sudo -n`、`unshare`、`socat`、`setpriv` 缺失，`lo` 无法启用，出现额外路由、代理未监听、反向 socket 缺失或 sshd 拒绝反向 streamlocal 转发时，启动失败关闭并映射为 `browser_network_unavailable`，不会回退 VM 默认网络；预算耗尽或固定 deadline 到期单独返回 `browser_network_budget`。`provision`／`check` 验证 `nsenter` 与 `sudo -n nsenter`，并用 `sshd -T` 检查服务端有效配置允许该转发。取消、SSH 断连与 shutdown 先终止 SSH 进程使转发消失，再由同一 Run 根的关联扫描终止 worker、socat 与包装进程，最后删除 Run 根；broker 监听在同一次清理中停止。网络观察只保留规范化 origin、结果、字节数和耗时，不记录路径、query 或凭据；取消／shutdown 与真实 timeout 分开记录，容量与共享预算拒绝即使 origin 未知也会记录。
