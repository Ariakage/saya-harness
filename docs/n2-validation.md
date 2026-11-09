# N2 隔离与 MCP 验收

本轮N2及对应验收已完成。起点为 `547e5fe`，最终实现与验收源码为 `aaec8c5b81ddb464320fd46f6dc6dd00a0a547f2`。下表与[最终四平台验收](#最终四平台验收)记录当前结果，后续各节保留逐阶段开发、失败、修复和本机研究历史，原有“待验收”表述归属各节当时源码。状态由[开发进度](progress.md)维护。

| 任务 | 必须具备的证据 | 当前状态 |
| --- | --- | --- |
| DEV-054 | 本地／外部工具注册、稳定名称、schema校验、宿主副作用分类、三权限与PLAN、版本审批绑定、取消与记录 | 完成，最终四平台完整门禁及外部工具矩阵通过 |
| DEV-055 | prepare／execute／文件服务／cleanup的正常入口接线、真实能力等级、显式约束缺失时失败、实际后端上下文 | 完成，四平台正常入口和能力正负对照通过 |
| DEV-056 | Seatbelt与Bubblewrap／Seccomp／socat、受控网络broker、可用委派资源controller | 完成，两种原生后端及Linux两架构实际委派验收通过 |
| DEV-075 | 真实shell CST、直接argv、包装器、重定向与动态行为、错误／预算、核查事实与批准版本绑定 | 完成，最终四平台回归及既有CST研究探针通过 |
| DEV-057 | 路径与链接对抗、缺失保护路径、namespace／syscall、网络旁路、资源与清理故障、性能和恢复的原生证据 | 完成，Linux各19项／macOS各18项及完整性能v2通过 |
| DEV-058 | 显式stdio MCP配置、版本与生命周期、启动授权、受控进程与关闭 | 完成，四平台正常CLI／PTY／TUI生命周期通过 |
| DEV-059 | 工具schema／结果类型／大小、统一权限与记录、取消、列表变化、三模型协议的外部工具往返 | 完成，四平台统一运行时和三模型协议矩阵通过 |
| DEV-060 | 首个凭据后端、引用／撤销、最小化注入、日志／会话／机器输出的凭据边界 | 完成，四平台私有文件后端与凭据边界通过 |
| DEV-061 | 固定SDK与协议矩阵、Streamable HTTP、OAuth与续期、断连不重放、额外输入及非交互路径 | 完成，四平台各25项联合验收通过 |

## 最终四平台验收

2026-11-09，固定源码 `aaec8c5b81ddb464320fd46f6dc6dd00a0a547f2` 的源码CI与N2验收均以四个原生job成功结束。所有平台完整workspace格式、严格Clippy、Rust测试、优化构建及49项Python工具回归通过。验收使用各平台从该固定源码构建的程序。[源码CI](https://github.com/Ariakage/saya-harness/actions/runs/42607697386)、[N2原生验收](https://github.com/Ariakage/saya-harness/actions/runs/42607698976)

| 实际环境 | Rust测试 | 原生用例 | MCP／OAuth | TUI旅程 | 性能v2判据 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Ubuntu22.04.5 x86_64／glibc2.35 | 923 | 19 | 25 | 24 | 21／21 |
| Ubuntu22.04.5 aarch64／glibc2.35 | 923 | 19 | 25 | 24 | 21／21 |
| macOS15.8.2 ARM／24H232 | 907 | 18 | 25 | 24 | 21／21 |
| macOS15.7.9 Intel／24G830 | 907 | 18 | 25 | 24 | 21／21 |

Linux实际内核均为 `6.8.0-1070-azure`，ELF最高GLIBC符号为2.34，运行库为2.35。macOS两端均在实际15系主机执行，Mach-O架构分别与主机匹配且 `minos=15.0`。Linux新增加的STREAM／SEQPACKET真实fork／exec关闭引用回归与原长路径能力探测在源码CI和N2门禁均通过。

| Runner | 验收程序SHA-256 |
| --- | --- |
| `ubuntu-22.04` | `b41ed794e168479dad471b0720aaecff661bc205e72b37eca3476c03a3bb3106` |
| `ubuntu-22.04-arm` | `66626971e33befe42d49ccbd2f809f9f72dc81783c2d8529aedde287c80bdfc2` |
| `macos-15` | `2d9af57dc8d5ebb96e640bdc16819750109be03c41e0895a6baa815a3ab5f2f1` |
| `macos-15-intel` | `93e746a43259c370e1d5640c5d88ef708c246b265344e8f5d52d2504a8923a93` |

下载后的独立核验将固定提交归档的209项源码／构建／脚本清单和全树摘要分别与native／MCP记录比对，并检查干净源码、workflow身份、架构、基线、实际测试前后哈希及两份保留程序副本。各端TUI通过固定工作流的 `target/release/saya` 入口执行，MCP保存的末帧诊断为complete／exit0，raw长度及程序归属均核对通过。两Linux的系统别名保护、controller读回、活动cgroup成员、结束清理及四项模型派发前拒绝对照全部通过。

最终记录位于 `target/n2/remote-review/aaec8c5b/`，索引为 `monitoring-complete.json`，包含每端完整job日志、原始artifact、独立报告与性能重算。原生清单摘要为 `6333707a372bad367551e5fdb3bba4a65fa5749d075f8031af45cea2e45668b2`，全树摘要为 `c180bc6e53f946d2f2b5478343dcb7c5c9465d776dfab991bbd6944e3853b5cf`。

Actions保留期为14天，本轮artifact于2026-11-23到期。下表记录API声明的归档digest，程序摘要已在下载解包后独立核对。

| Runner | Artifact ID | API归档SHA-256 |
| --- | --- | --- |
| `ubuntu-22.04` | [13669876303](https://github.com/Ariakage/saya-harness/actions/runs/42607698976/artifacts/13669876303) | `dd2b97c65dfd146e064470510f80ccfa715d723eebf5db8d80d53d74f1b2b5c6` |
| `ubuntu-22.04-arm` | [13668814442](https://github.com/Ariakage/saya-harness/actions/runs/42607698976/artifacts/13668814442) | `82d4e4733bf4952a8e5209c312a2bd85e8908bd36bec6aa22323597d3ca5f7df` |
| `macos-15` | [13669387523](https://github.com/Ariakage/saya-harness/actions/runs/42607698976/artifacts/13669387523) | `5dd7670089ab3170e340b793fbfe7f2a66fa9233b0f3d7ccf800b9e42f2c2cf3` |
| `macos-15-intel` | [13669744661](https://github.com/Ariakage/saya-harness/actions/runs/42607698976/artifacts/13669744661) | `4cc70cbb1491372d476cb4c084960c20e2f1e2de26dc55b11aa557b9835d13a0` |

### 最终性能测量

完整v2报告覆盖Host／原生两后端的冷启动、同Run热命令、短命令、文件读取、写入、搜索与四Run并发七项，以及直连、HTTP代理和CONNECT三条网络路径。每端在同一次原生脚本运行取齐所有指标，再由固定源码的 `scripts/n2/verify_performance.py` 传入实际报告路径及 `--samples 3`，在CI与下载后分别校验全部原始样本、成功操作证明、nearest-rank分位数和比较值，21项判据均通过。

每格为原生／Host 的p50毫秒。冷启动、读写、搜索和并发批次包含CLI与夹具开销，热调用在同一Run的一次预热后单独计时。每项三个样本，采用nearest-rank，用于本轮开销观测。

| Runner | Cold | Warm | Short | Read | Write | Search | Four-Run batch |
|---|---:|---:|---:|---:|---:|---:|---:|
| ubuntu-22.04 | 262.424 / 30.531 | 15.049 / 15.498 | 322.721 / 60.877 | 272.219 / 272.191 | 302.603 / 262.164 | 272.343 / 292.419 | 894.126 / 77.373 |
| ubuntu-22.04-arm | 201.738 / 20.588 | 16.357 / 15.984 | 221.936 / 40.754 | 222.008 / 211.947 | 221.976 / 221.882 | 222.009 / 211.868 | 679.519 / 54.172 |
| macos-15 | 164.697 / 79.943 | 32.291 / 19.639 | 170.241 / 103.903 | 152.909 / 151.467 | 161.027 / 146.604 | 163.678 / 175.841 | 305.403 / 259.574 |
| macos-15-intel | 270.174 / 169.483 | 194.364 / 25.906 | 333.046 / 184.128 | 352.568 / 325.174 | 373.258 / 296.311 | 402.883 / 272.199 | 820.964 / 265.912 |

代理表单独测量payload内完整socket请求／响应／关闭的p50毫秒，包含CONNECT建链。CLI整体资源另行记录。CONNECT内使用明文HTTP。

| Runner | Host direct | Native HTTP | Native CONNECT |
|---|---:|---:|---:|
| ubuntu-22.04 | 0.426 | 0.975 | 1.046 |
| ubuntu-22.04-arm | 0.582 | 1.228 | 1.219 |
| macos-15 | 1.023 | 0.931 | 1.066 |
| macos-15-intel | 1.438 | 2.380 | 2.074 |

CPU与RSS来自内核wait4。热命令间隔从模型响应发送完成计至后继工具结果请求，包含调度、持久化与本地夹具开销，CLI整体CPU／RSS单列。四Run并发记录各child资源、CPU之和及最大child峰值RSS，同时聚合内存峰值未测。冷启动指新CLI／新Run且保留OS页缓存。代理RTT由payload内单调时钟测量，CONNECT在隧道中使用明文HTTP。全部p95、样本、作用域与操作证明保留于各端 `native-evidence.json`。

### 验收范围与后续边界

- 凭据首个后端使用0700／0600私有明文文件，系统凭据库属于后续扩展。MCP额外输入以 `needs_input` 结束当前Run，原调用内交互续接另行实现。
- macOS后代清理能力为best_effort，Linux使用所属PID namespace。Linux聚合硬限制要求显式委派cgroup，委派验收包含内核读回、成员、清理与配置负对照，资源耗尽压力未测。
- OAuth、三模型协议和代理使用受控本地服务。真实厂商互操作、外部DNS与TLS加密内容检查保留独立未测范围。
- 历史Intel启动与JSONL偶发失败缺少足够原始观测，唯一根因保留未确认状态。已确认的终端计时和FD所有权问题有永久回归，最终四端重新执行全部门禁和正常入口验收通过。
- 本机完整门禁采用四测试线程控制本机I/O争用，最终四平台门禁沿各runner默认测试并发执行。后续N3从DEV-076 OutputStore进入DEV-062 Context Builder，发行版本按单独准备流程确定。

## DEV-055 原生执行组件

core已提供统一准备环境、能力等级、固定策略身份和清理接口，包含独立Seatbelt与Bubblewrap后端、Seccomp、私有launcher、文件worker和进程监督。六种文件工具复用统一审批与持久化入口，执行前保存意图并复查许可，异常退出保留未知状态及可确认的观测。文件RPC以实际请求／响应的32MiB作为传输上限，较大的处理限额允许用于小结果。

本机隔离源码快照完成格式、workspace全目标严格Clippy、803项Rust测试和优化构建，日志为 `target/n2/dev055-workspace-final2.log`。包含真实CLI文件worker、原生路径与网络拒绝、环境FD、资源缺失、取消与清理回归。本机为macOS27.2 ARM，部署目标15.0。初次环境FD测试的描述符号复用竞态已通过保留inode见证修正，原失败记录继续保留。

本阶段提交执行组件和显式 `ExecConfig.execution` 接线。正常CLI默认策略、运行期代理及MCP服务继续整合，DEV-055／056的完整完成状态依据最终正常入口与四平台验收更新。Linux CI通过固定SHA-256校验安装非setuid的Bubblewrap0.13.1及socat，后端缺失时明确失败。

## DEV-056 网络与原生约束组件

每Run网络broker支持HTTP absolute-form与CONNECT、精确origin、每连接DNS地址核查及固定目标地址、双栈loopback端口占有、解析／字节／连接／时间预算和关闭等待。受控真实TCP／Unix socket的18项broker回归包含请求走私、认证目标、DNS变化、取消、限额与监听器生命周期。

隔离源码快照完成格式、workspace严格Clippy、823项Rust测试与优化构建，日志为 `target/n2/dev056-workspace-final2.log`。macOS回归确认宿主进程参数查询被显式拒绝、私有launcher保留自身FD处理能力、标准根symlink可解析，代理仅允许TCP。正常CLI的UDP同端口及宿主进程查询对抗将随DEV-057重跑。

首次Linux原生CI已保留实际失败证据。原Seccomp阻止匿名socketpair，Tokio信号驱动初始化因此panic，原始日志在 `target/n2/research/dev055-direct-linux-{arm,x86}-ci.log`。修复仅放行AF_UNIX的已连接stream pair与必要flags，其余Unix socket创建保持受限，并补充真实Linux重定向／SCM_RIGHTS探针。后续源码 `b15bbe8` 四平台CI全部通过，包含两种Linux架构的真实原生回归，证据为 `target/n2/dev056-four-platform-ci.json`。[CI记录](https://github.com/Ariakage/saya-harness/actions/runs/42555064671)

上述823项本机测试的操作系统为macOS27.2 ARM。Linux修复已有CI回归证据，委派cgroup及最终默认入口仍待完整原生验收。

## DEV-058 MCP 连接与认证组件

固定官方Rust SDK rmcp3.9.0，核心连接支持stdio与Streamable HTTP，兼容2026-07-28及2025-11-25协议。stdio接收宿主已经监督的管道，SDK适配器与进程创建分别处理。目录、schema、JSON／SSE与工具结果具有输入、大小和时间边界，工具发送一次，断连、认证、额外输入与不明确结果经类型化错误返回。

本阶段同时提供连接所需的私有凭据组件及OAuth基础，DEV-060／061继续负责公共管理入口与整体验收。凭据后端使用私有明文文件、原子发布和撤销记录。OAuth具有PKCE、state、issuer、明确scope与资源绑定，并区分精确transport endpoint和合法canonical resource。动态轮换保留旧／新值供宿主脱敏，恢复和更新再次核对绑定。

SDK需要的reqwest0.13单独服务于MCP，原有模型接入继续使用其已验证的0.12依赖。async-trait用于SDK凭据存储接口，futures和sse-stream用于有界流处理。

隔离源码快照完成格式、workspace严格Clippy、873项Rust测试及优化构建，日志为 `target/n2/dev058-workspace.log`。组件提交 `e69e6c1` 四平台CI全部通过，证据为 `target/n2/dev058-four-platform-ci.json`。[CI记录](https://github.com/Ariakage/saya-harness/actions/runs/42557002296)

正常应用配置、默认后端、服务生命周期和界面接线在DEV-059阶段整合，最终四平台程序随后统一验收。

## DEV-059 正常入口整合

应用层默认采用原生后端，执行环境身份与能力进入实际系统上下文。文件 worker 复用意图持久化、许可复查与未知结果记录，额外逻辑权限根和 OS 视图分别落实。显式 Host 兼容模式保留能力报告，活动凭据存储要求原生保护。

MCP 服务连接、启动授权、目录发现和刷新共享运行预算。人工确认暂停活跃计时，清理在预算结束后继续执行。PLAN 切换通过生命周期屏障等待本地服务及副作用收尾。动态凭据集合持续覆盖旧值与新值，进入模型、存储、事件、正文和 Thinking 显示前使用当前集合检查。

整合工作区在本机 macOS27.2 ARM／Rust1.99.1 执行完整门禁，904项 Rust 测试及优化构建通过，日志为 `target/n2/integration-workspace-5.log`。独立提交快照完成901项Rust测试和完整门禁，日志为 `target/n2/dev059-workspace-final.log`。首次并行负载下的CLI启动就绪等待超时保留于 `target/n2/dev059-workspace-before-readiness.log`，停止其他验收进程后完整重跑通过，测试超时与产品行为保持原有约定。最终四平台验收继续分别记录。旧 TUI 脚本要求 MCP 显示“未实现”的断言已依据新功能改为实际空服务状态，真实服务连接另由 MCP 验收脚本覆盖。

## DEV-060 私有凭据与管理入口

CLI 提供 `mcp credentials import/list/revoke` 与交互 `mcp login`。导入值从显式环境变量取得，列表仅返回元数据，引用撤销永久生效。批准绑定服务、精确 endpoint 和明确 scope，OAuth 另保存 issuer 与 canonical resource。登录发现、回调等待和令牌交换共享有效期，进入私有存储提交点后等待真实写入结果。

独立复核复现管理审批中的已知凭据泄露后，入口在人工提示、授权 URL 输出及浏览器导航前完成凭据检查，包含未配置的导入来源值和有界百分号／form 编码视图。修复后的真实终端保持空输出，token POST 与存储引用数均为0。三项永久回归覆盖导入、授权元数据和B-17退出码，全部10项CLI bin测试与严格Clippy通过。

正常优化程序的受控 OAuth 验证覆盖PKCE／state、明确scope、一次刷新、动态脱敏、一次工具POST与撤销，完整矩阵由DEV-061记录。当前Rust源码清单与904项完整门禁和优化程序一致。本后端为文件系统私有明文存储，最终各平台使用同一源码重新构建并验证。

## DEV-057 原生普通入口与性能

`scripts/n2/native_acceptance.py` 使用固定的实际CLI副本、独立模型夹具和编译后的本地系统调用probe，逐项验证操作结果及实际后端能力。错误和超时保留有界诊断，证据逐阶段原子发布。PLAN要求完整的两项拒绝结果，性能样本要求实际工具成功，8项纯Python回归检查这些验收判据。

最终本机脚本SHA-256为 `5f136fd04cff6a80e0c52da03955d0b521d69b2114613b48fbbb63eb350d9db9`，所测优化程序SHA-256为 `8f7810773f90b81b91d3ee7a4916ced17e15ef945b4050f96a3210c2d4f15585`。macOS27.2 ARM的18项实际原生用例全部通过，证据为 `target/n2/final-local/native-reviewed-evidence.json`。同一程序另通过工具smoke、文本会话、既有审查回归与24组正常TUI旅程。

本机每variant三次样本测得原生相对Host的P50附加耗时约为冷启动37ms、文件读取18ms、短命令399ms、四任务批次455ms。样本为新进程／新Run，保留OS页缓存；RSS为内核wait4的ru_maxrss。平台、样本量及聚合内存测量边界保留于报告，Linux和macOS15重新测量。

Linux委派用例要求提供专属cgroup v2子树，检查memory.max、pids.max、cpu.max内核读回、运行中实际成员和结束后自有组移除。该用例覆盖内核配置与成员，资源耗尽压力未测试。四平台工作流对源码、主机与二进制架构、程序SHA分别留证，并在所有结果下上传14天Actions artifacts。Linux包装器只在当前systemd委派服务内创建和清理子树，验收强制要求该用例。

```sh
MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/n2/test_native_acceptance.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/n2/native_acceptance.py target/release/saya
```

macOS本机未执行Linux委派用例。最终四平台固定源码结果在后续记录，正式发行按独立授权处理。

## DEV-061 MCP 协议与正常入口矩阵

| 边界 | 固定实现与验证 |
| --- | --- |
| SDK／协议 | rmcp3.9.0，首选2026-07-28，兼容2025-11-25，核心受控连接测试包含协商与legacy初始化 |
| 模型协议 | Chat Completions、Responses、Anthropic Messages各自完成stdio及HTTP外部工具往返 |
| HTTP | 有界JSON／SSE、单次工具POST、目录变化、schema失效、401和断连保留明确状态 |
| 授权 | 明确scope、PKCE／state／issuer／resource绑定、轮换刷新、撤销与动态凭据脱敏 |
| 交互 | 两次stdio人工批准、非交互终止、PLAN本地服务停止与宿主只读HTTP工具 |
| 生命周期 | 四项连接／目录预算边界、恢复单次派发、同Run状态事件、真实TUI及终端恢复 |

永久 `scripts/n2/mcp_acceptance.py` 使用实际普通CLI、控制终端与默认路由TUI。相同本机优化程序通过25／25项，报告为 `target/n2/final-local/mcp/mcp-report.json`，运行前后SHA-256均为 `8f7810773f90b81b91d3ee7a4916ced17e15ef945b4050f96a3210c2d4f15585`。实际界面验证Configured、Connected／HTTP／2工具、Stopped／0工具，SQLite事件归属同一个Run，退出恢复termios、文件flags与alternate screen。

报告将本机macOS27.2 ARM、当时源码提交及dirty状态分别保存，本地记录对应该优化程序。最终四平台工作流将从固定提交重新构建，逐平台保存新的程序哈希。OAuth／MCP均使用本地可控服务，真实厂商互操作与付费模型联调保留独立证据层级。

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/n2/mcp_acceptance.py target/release/saya
```

## 首次最终源码验证与Linux工具链兼容

`97b40f96aad20751e3f803d9e0b0904fee90e175` 已正常推送并启动源码CI与N2四平台验收。Linux两架构的完整门禁均在BUILD／CODE夹具失败，Rust编译器在启动时报告无法加载`librustc_driver`并退出127，后续原生／MCP步骤因门禁失败跳过。原始日志为 `target/n2/remote-review/linux-arm-job-128982907369.log` 与 `linux-x86-job-128982907119.log`。[首次验收](https://github.com/Ariakage/saya-harness/actions/runs/42562872298)

工具链目录已进入可读视图且argv已指定`--sysroot`。根据glibc通过`/proc/self/exe`解析origin的实现，缺少该路径可导致相对动态库定位失败。当前候选为隔离内编译命令显式设置工具链`lib`路径，后端策略保留。原因推断与Linux修复结果分别记录。[glibc实现记录](https://sourceware.org/pipermail/glibc-cvs/2022q4/081599.html)

## Linux Rust子进程兼容复验

`1c988470` 的Linux两架构复验证明显式库路径已使编译器进入链接阶段，随后创建`cc`子进程返回EPERM。Rust标准库的Unix fork路径使用匿名AF_UNIX SOCK_SEQPACKET通道报告exec结果，当前payload过滤器仅放行STREAM，因此需要运行时兼容修复。候选仅扩展已连接的匿名pair类型，保留命名Unix socket与datagram限制，并扩展实际防重定向／FD传递及带pre_exec的Rust子进程回归。当前候选已加入精确pair放行、内核正式版本身份门禁及行为探测。确认范围为5.15分支≥5.15.86、6.1分支≥6.1.2或正式6.2及以后版本，其他身份按未确认必需能力处理。有限探测不能证明历史竞态修复，因此与版本条件共同使用。原peer间允许传递已有FD，宿主FD继承边界继续由CLOEXEC与私有通道隔离。该项在Linux实际复验完成后更新状态。[Rust标准库实现](https://doc.rust-lang.org/src/std/sys/process/unix/unix.rs.html)

候选在本机通过完整格式、workspace全目标严格Clippy、904项Rust测试及优化构建，日志为 `target/n2/seqpacket-workspace.log`。两Linux目标在独立后端harness完成全目标严格Clippy，实际版本函数的21个样本通过。交叉检查作为类型证据，Linux native/fork/内核探测仍待真实runner。Mac优化程序SHA仍为 `8f7810773f90b81b91d3ee7a4916ced17e15ef945b4050f96a3210c2d4f15585`，与本机18原生、25MCP及24TUI已测程序逐字节一致。

## 原生依赖下载故障与校验

`2b353dc2` 的N2 Linux ARM在固定Bubblewrap release下载连续HTTP504后退出22，源码CI两Linux任务同样在安装阶段终止。失败日志和ARM首次产物保留于 `target/n2/remote-review/2b353dc2/`。任务尚未执行产品构建／测试的范围按工作流实际skipped状态记录。

安装脚本保留0.13.1及SHA-256 `0e85c8702248b8dd64144f594147553650cdd0e4c951690345faace79c217c4d`。原下载地址失败后访问官方release asset API的已锁定640310180对象，最终执行相同摘要核查。官方元数据的文件名、129472字节大小与digest已核实，真实API下载摘要一致。四项离线控制流回归验证主地址成功、API兜底覆盖部分下载、坏摘要拒绝解包与双失败，记录为 `target/n2/research/installer-fallback-tests.log`。[官方asset接口](https://docs.github.com/en/rest/releases/assets#get-a-release-asset)

## Linux系统别名与保护映射复验

`2b353dc2` 的N2 Linux x86已越过动态库与fork通道问题，BUILD工具结果变为 `linker cc not found`／ENOENT。宿主Meson实际识别cc与ld，隔离视图缺少包管理器别名目录是当前原因推断。候选将存在的 `/etc/alternatives` 只读映射，原生回归会记录真实 `/usr/bin/cc` 链、最终规范目标、只读标志和whole-directory保护结果。

独立源码审查确认系统目录独立bind产生的guest别名没有全部应用原canonical保护mask。候选保存固定6个system roots的source／destination映射，保护路径投射到全部别名，受保护祖先覆盖整个子挂载，遮罩去重／父路径消除冗余并保留4096上限。所有遮罩在根挂载后应用。

Linux原生unit增加 `/usr/bin/readlink` 与 `/bin/readlink` 同对象双路径拒绝、cc正控制和其余 `/etc` 隐藏检查。成功时保存 `system-alternatives.json`，实际host链接与防护结果由runner证据确认。本机core及两架构交叉严格检查、2项实际映射函数测试和独立代码复核通过，原生Linux结果继续记录。

当前候选在本机完成904项Rust测试、格式、workspace全目标严格Clippy与优化构建，日志为 `target/n2/alternatives-workspace.log`。Mac程序与既有完整本机验收程序逐字节一致，Linux源码变更继续由原生runner验证。

## x86 GNU默认LLD与无proc视图

`340499db` 的N2 x86 BUILD已找到cc并启动链接流程，工具stderr为 `lld-wrapper: could not get the path of the current executable: no /proc/self/exe available`，原日志保留于 `340499db/ubuntu-22.04-job-129001686806.log`。同源ARM的12项编译／工具acceptance已全过，随后进入chat就绪等待的独立失败。

当前候选仅为Linux x86_64 GNU夹具增加官方稳定 `-C linker-features=-lld` 选项，使用系统链接器。ARM与macOS原命令保持，本机12项acceptance、格式、定向严格Clippy及Rust1.99.1目标参数检查通过，日志为 `target/n2/research/linux-x86-system-linker-*`。真实x86链接仍待runner。[官方Rust选项](https://doc.rust-lang.org/rustc/codegen-options/index.html#linker-features)

## 模型进入信号与终端等待

ARM的三个chat超时分别等待首条模型请求或后台启动后的第二条模型请求，旧夹具的3／4秒等待尚未执行取消／模式断言。新增PTY就绪等待在10秒边界内排空输出并检查子进程，失败返回状态与有界记录。单次CLI取消夹具的同类就绪等待增加明确的进程回收与输出诊断。

本机18项chat测试及同一已编译程序的3轮54项重复通过，新增实际CLI提前退出用例验证诊断。最终完整格式、workspace严格Clippy、905项Rust测试与优化构建通过，记录为 `target/n2/readiness-workspace.log`。实际Linux性能／同步结果在新runner中验证。

## 独立验收编排与Linux基线

完整源码门禁与优化候选构建分别留证，候选按同一固定源码生成。架构／平台基线通过后，原生、MCP与界面检查分别记录结果。完整门禁失败使job失败，所有必需步骤成功才满足N2退出条件。`!cancelled()`与前置outcome控制阶段启动，取消后的always步骤保留证据。

Linux见证记录实际Ubuntu22.04／glibc2.35、ELF最大GLIBC符号≤2.35及ldd依赖，并核对同一程序与架构见证的SHA。最终下载还需将provenance、baseline、native、MCP及保留的程序副本逐项比对。

静态验证涵盖YAML、14个Shell片段、所有内嵌Python、失败／取消依赖场景，12项native／installer离线回归及独立只读复核通过。实际Actions运行结果在固定源码后记录，源CI仍执行完整workspace门禁。[条件状态函数](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#status-check-functions)

## Linux 长路径与资源控制修复

`72e79a0` 的四平台记录中，macOS15 ARM完整源码门禁及18项原生、25项MCP、24项TUI通过，Intel同样通过三个程序验收套件，其源码结构夹具受到解析截止时间影响。Linux两架构在原生准备时暴露私有TMPDIR超过Unix socket地址长度的问题。宿主能力探测现持有私有目录FD，以短 `/proc/self/fd` 地址执行同一STREAM／SEQPACKET语义探测，payload继续使用既有文件系统视图。长于108字节的真实scratch路径加入回归。[该轮验收](https://github.com/Ariakage/saya-harness/actions/runs/42571348707)

资源约束检查拒绝可写cgroup／cgroup2挂载及其bind别名和重叠根。显式Host请求委派根或聚合硬限制时准备失败。Linux委派验收保留实际内核读回、活动成员与清理，并加入兄弟controller可写根及Host配置负对照。该用例验证配置与成员，资源耗尽压力仍未测试。

本机macOS27.2 ARM完成905项Rust测试、完整格式／严格Clippy及优化构建，记录为 `target/n2/ci-profile-workspace.log`。最终Linux模块另通过两架构严格交叉检查和mountinfo函数探针，真实Linux行为待新runner验证。

## 命名空间生命周期见证

Linux的MCP与TUI旧夹具将guest PID用于宿主存活查询，原始记录包含EPERM。共享FIFO见证现要求子进程写入READY并保持writer，宿主观察现场writer后才发送ACK；stdio服务随后发布阶段标记并进入协议。取消或关闭后要求EOF，继承writer的后代也纳入结束判定。未启动负对照检查任何启动活动，保留真实宿主CLI信号操作。

13项脚本回归覆盖历史READY／EOF、从未启动、writer继承及READY／ACK顺序。固定优化程序 `fedcddbec295183a4bfcb6fac1a9d7552bba163b850ccd86a5013e0eafe19bdf` 在本机通过18项原生场景、每variant三次性能样本、25项MCP和24项TUI，报告位于 `target/n2/validated-local/`。该记录的源码工作区包含本轮待提交修复，Linux PID命名空间的实际见证由四平台工作流重新确认。

`1a932cc` 的两Linux架构已通过25项MCP、24项TUI及委派资源正负对照。源码门禁继续到Rust生命周期夹具，五项旧断言同样把guest PID交给宿主查询。该夹具改为FIFO现场READY／writer及CLI退出后的立即EOF，shell和sleep后代共同持有writer，保留真实CLI信号与原退出码。新增未启动、历史READY与继承writer回归，本机三轮共18项测试、定向严格Clippy及格式检查通过，日志为 `target/n2/research/lifecycle-namespace-*.log`。Linux重新执行仍待新源码runner确认。[该轮验收](https://github.com/Ariakage/saya-harness/actions/runs/42579723866)

`551acc26` 的Linux源码门禁中原五项生命周期场景全部通过，新增继承writer夹具在dash后台命令的stdin重定向处返回BrokenPipe。夹具在异步列表之前以FD8保存控制管道，后代显式从FD8读取，FD9继续见证生命周期。本机真实dash对照复现旧路径提前EOF／BrokenPipe并确认新路径收到release后结束，6项生命周期测试、定向严格Clippy、格式及独立复核通过，记录为 `target/n2/research/lifecycle-{dash-release-control.json,async-release-fd.log}`。最终Linux完整门禁仍由新固定源码执行。

## Linux 代理桥接镜像身份

`1a932cc` 的Linux HTTP／CONNECT用例均在launcher返回exit74／EPERM。桥接启动仍以guest的 `metadata.uid() == 0` 判断宿主root所有的socat，而当前user namespace仅映射runner UID。修复在宿主打开源FD并复验root所有、无group／other写权限及可执行性，复用有界复制和前后fingerprint校验，固定为私有0500镜像并通过只读FD挂载。guest核对该镜像的精确device／inode、无写权限与可执行性。PID2入口、清洁环境、payload过滤和桥接进程清理继续由既有边界执行。

两Linux目标的实际模块严格Clippy通过，四项新增Linux回归覆盖host信任、镜像替换、权限和完整内部身份字段。本机同源码函数的三个metadata控制通过，独立只读复核无新增阻断，证据为 `target/n2/research/proxy-linux-*-clippy.log` 与 `target/n2/research/proxy-oracle/report.json`。该控制未执行Linux命名空间，实际代理往返与最终四平台验收等待新源码runner。

最终本机macOS27.2 ARM完整格式、workspace全目标严格Clippy、906项Rust测试及优化构建通过，日志为 `target/n2/proxy-final-workspace.log`。33项Python回归及全部脚本AST检查通过，日志为 `target/n2/research/proxy-final-python.log`。本次优化程序SHA-256为 `9d90b4bf6b82a174e5930d80203e5a23b1584e63044b8a147622a888366ec019`，四平台使用新固定源码重新构建验证。

## UDP 传输边界判据

`1a932cc` 的Linux原生套件15／19项通过，其raw UDP探针调用单向 `sendto`，在私有network namespace内返回 `SENT`。原判据错误地要求系统调用必须失败，修订后核对完整的 `SENT` 或正整数errno输出，同时必须确认宿主监听器收到零个payload数据包。macOS继续要求Seatbelt拒绝系统调用。代理端口的IPv4／IPv6监听器均先执行实际宿主发送／接收正控制并排空，再检查payload发送后的零收包。

11项验收oracle回归覆盖Linux私有发送、宿主实际收包的失败反例、macOS拒绝及无效输出。本机既有固定程序通过18项原生场景，记录为 `target/n2/udp-oracle-local/native-evidence.json`。该探针验证指定host listener的传输隔离，外部DNS解析仍列于未测范围。Linux代理启动失败另行修复并重新执行完整套件。

## CI 调试镜像与结构夹具

源码CI和N2工作流设置 `CARGO_PROFILE_DEV_DEBUG=line-tables-only`，保留行表并记录实际debug／优化镜像大小。结构夹具仅在唯一未完成原因为 `DeadlineExceeded` 时最多重试两次，每次沿用生产50ms限制，显式预算测试继续使用独立入口。19项结构测试及最终全目标严格Clippy通过，生产解析预算和运行预算保持现有值。最终完成仍要求新固定源码的全部四平台源码门禁和程序验收成功。

## 文件遮罩挂载的独立描述符

`b1f58e2` 的两Linux源码门禁已通过生命周期夹具，核心库145项测试仅系统别名保护回归失败。Bubblewrap0.13.1在每个 `--ro-bind-fd` 操作完成后关闭该FD，旧实现将同一个文件遮罩FD用于多个目标。文件遮罩现为每个挂载独立 `try_clone`，继续固定同一私有遮罩inode，目录、保护顺序与权限要求沿已有入口执行。真实系统别名回归保持强制运行并生成独立见证。[Bubblewrap描述符消费实现](https://github.com/containers/bubblewrap/blob/v0.13.1/bubblewrap.c#L1290)

源码门禁使用 `cargo test --workspace --locked --no-fail-fast` 收集其余测试目标结果，任一失败仍使完整门禁返回失败。最终Linux挂载与完整四平台结果等待修复后的固定源码执行。

两Linux目标的实际模块全目标严格Clippy通过，本机真实FD消费对照复现旧实现第二次EBADF，独立复制后两次成功且inode／000权限相同，原描述符继续有效。独立复核无新增阻断，证据为 `target/n2/research/mask-fd-linux-*-clippy.log` 和 `target/n2/research/mask-fd-review/oracle.json`。最终完整本机门禁906项Rust测试、格式、严格Clippy和优化构建通过，日志为 `target/n2/mount-fds-workspace-final.log`。

## JSONL 审批夹具端点收尾

本机完整门禁在交互JSONL夹具中收到 `run.finished` 后等待CLI退出超时，原始记录为 `target/n2/mount-fds-workspace.log`。该夹具为PTY master设置close-on-exec，启动后释放Command持有的slave副本，并让stdout reader持续排空至EOF。终端master继续在原有边界关闭，保留8秒退出检查、全部审批／澄清事件和成功退出码断言。

候选曾把master保留到子进程退出之后，在本机产生退出等待停滞，关闭master后立即释放。该变体及进程采样保存在 `target/n2/research/jsonl-tty-after-*`，最终夹具沿用原关闭顺序。该观察记录实际受控进程状态，具体内核路径尚未确认。

最终夹具11次定向运行、完整61项CLI、定向严格Clippy、格式及独立复核通过，记录为 `target/n2/research/jsonl-tty-owned-after-*.log`。完整workspace重新通过906项测试。原偶发超时的唯一根因尚未确认，已收口的端点所有权和实际复验结果分别记录。

## 人工确认与后台完成的测试同步

`a6f3d9996764e9ca42163c627f989a707419a5bd` 的N2四平台工作流与artifact独立核验全部通过。Linux两端各920项Rust、19项原生、25项MCP和24项TUI，macOS15两端各906项Rust、18项原生、25项MCP和24项TUI。实际源码双清单、架构、平台基线、程序前后哈希、系统别名保护及资源正负对照均通过。报告为 `target/n2/remote-review/a6f3d999/monitoring-complete.json`。[四平台原生基线](https://github.com/Ariakage/saya-harness/actions/runs/42588642806)

独立源码CI的Intel正向人工确认用例使用250ms活跃预算，包含真实Python／MCP启动时偶发达到限制。测试现仅在人工回调内虚拟推进60秒，返回前恢复实时时钟，验证10秒活跃预算下任务完成及模型请求一次。同样推进60秒的未暂停预算负对照必须耗尽。真实进程和协议I/O继续按实时时钟执行，日志为 `target/n2/research/human-budget-*.log`。[该轮源码CI](https://github.com/Ariakage/saya-harness/actions/runs/42588641334)

本机门禁还复现后台文件已可见、job结果尚未完成保存时夹具提前输出final，运行结束按契约取消任务，artifact列表为空。夹具现通过正常 `background_status` 查询实际job及唯一call ID，收到 `finished` 与成功结果后才结束模型任务，保留请求／轮询上限和原artifact断言。11次定向运行、完整61项CLI、严格Clippy及独立复核通过。生命周期8秒等待补充阶段标签，便于区分marker、writer和CLI退出。

最终本机以 `RUST_TEST_THREADS=4` 执行完整workspace门禁，906项Rust测试、格式、全目标严格Clippy及优化构建通过，记录为 `target/n2/human-budget-workspace-final.log`。此前默认线程数下的CLI／生命周期等待失败保留于 `target/n2/human-budget-workspace.log`，定向生命周期6项复验通过。四平台门禁继续使用各runner默认测试并发，最终新源码验收另行记录。

## 完整性能证据补充

终审对照DEV-057起点设计确认，已有四指标记录缺少独立热启动、文件写入、搜索和代理延迟。性能证据v2现记录每个Host／原生后端的冷启动、读取、写入、搜索、短命令、同Run热命令及四Run并发七项，逐样本保留实际成功操作与后端证明。热命令预热一次后取样，使用实际模型响应发送完至后继工具结果请求的单调时间戳，并验证同一Run／环境。CPU和内存按整次CLI及每个并发child的内核数据记录。

代理组分别测量Host直连、原生HTTP代理与CONNECT，payload单调时钟覆盖socket建立、发送、完整16字节响应及关闭。每个样本要求真实目标只收到一次授权路径请求，CLI整体资源与网络RTT分别存储。CONNECT采用隧道内明文HTTP，操作与资源的测量范围随报告记录。

固定本机程序 `9d90b4bf6b82a174e5930d80203e5a23b1584e63044b8a147622a888366ec019` 在同一次脚本执行通过18项原生场景、两后端七指标及三种网络路径各3样本，记录为 `target/n2/performance-final-local/native-evidence.json`。47项Python离线回归通过，独立核验器的21项判据及25项坏数据反例通过。核验器支持1／3／20样本，并已接入四平台工作流，强制 `measurement_version=2`、完整单次报告、精确样本数、分位数／比较值重算及资源范围。

本机观察到热命令P50原生30.817ms、Host15.465ms，网络RTT P50直连0.797ms、HTTP代理1.521ms、CONNECT1.510ms。这些值对应上述三样本与明确测量范围，四平台实际值按新固定源码重新记录。

## 终端输出预算与启动诊断

`b091dee` 的四平台源码CI全部通过，N2三端完整通过，Intel仅MCP侧栏用例在启动0.505秒后退出1。旧报告保留了正确的Configured首屏，但缺少失败raw，具体原因无法唯一确认。MCP夹具现在在清理前保存有界raw、末帧、阶段、进程退出码及ANSI／UTF-8解析状态。受控150ms消费者停顿实测触发error35并成功保留诊断，正常启动、缩放及20ms停顿均通过。[该轮源码CI](https://github.com/Ariakage/saya-harness/actions/runs/42594301076)、[该轮N2记录](https://github.com/Ariakage/saya-harness/actions/runs/42594302822)

独立阴性对照确认原帧I/O截止时间在渲染前开始，纯300ms渲染延迟后即使PTY持续可写也会报WouldBlock。`TerminalGuard`复用Ratatui `try_draw`，在渲染完成后开始250ms帧输出计时。旧代码在新回归失败，修复后8项终端测试及3轮重复通过，持续慢读、关闭和恢复的原有边界断言继续执行。此独立问题与旧Intel缺raw失败分别留证。

最终本机4线程完整门禁通过907项Rust测试、格式、workspace严格Clippy及优化构建，日志为 `target/n2/terminal-final-workspace.log`。49项Python离线回归通过。新程序 `d2b0f84585ac0e66cf8953547bd9eac8b36cbd7e21b28024c0da469a8aa9479e` 在同一固定副本通过18项原生、完整性能v2的21项核验、25项MCP和24项TUI，记录位于 `target/n2/terminal-final-local/`。新源码的四平台验收继续单独执行。

诊断捕获失败时保留原TUI异常，成功运行中的捕获失败仍使验收失败，终端与服务清理均经finally执行。永久脚本回归覆盖退出尾部、raw／屏幕边界、阶段和进程状态。异常优先级另外以实际夹具函数的三项对照验证，记录为 `target/n2/research/tui-independent-review/lifecycle-error-probes-after.json`。

## 匿名通道关闭引用的探测同步

`e7bbc7e` 的四平台源码CI全部通过，N2的Linux x86源码门禁在长路径匿名pair用例返回“dead-peer send did not report a known closed state”。该端正常原生19项、MCP25项、TUI24项及完整性能v2均通过，失败门禁独立保留。[源码CI](https://github.com/Ariakage/saya-harness/actions/runs/42602868114)、[该轮N2](https://github.com/Ariakage/saya-harness/actions/runs/42602868260)

同源码STREAM控制证明，真实fork中的子进程在exec前可暂时持有CLOEXEC peer，父进程关闭后发送仍可合法成功，exec关闭后原严格检查通过。探测现为所有peer-close分支共享一秒绝对截止，确认Linux完整POLLHUP后才执行发送和重定向验证。无效描述符、迟到事件、未知状态和超时失败关闭，中断按同一截止重试。固定内核门槛与完整行为负对照继续共同约束能力。

新增Linux模块回归包含STREAM／SEQPACKET的真实fork／exec窗口、保留peer超时、过期截止及无继承正控制。两Linux目标严格Clippy、格式与独立复核通过，本机同源码STREAM控制通过，实际Linux HUP路径待新runner执行。原CI失败的唯一原因仍无法从旧错误信息确认，新的错误包含安全的返回值及errno。证据位于 `target/n2/research/pair-peer-close/`。

本机4线程完整门禁907项Rust测试、workspace严格Clippy、格式及优化构建通过，日志为 `target/n2/peer-close-workspace.log`。Mac优化程序与已完整本机验收的 `d2b0f84585ac0e66cf8953547bd9eac8b36cbd7e21b28024c0da469a8aa9479e` 逐字节一致。最终四平台仍按新固定源码验收。

## 归档CI的规范模式采样

文档归档提交 `6da36ac` 的CI在macOS ARM仅有空闲输入回收夹具失败，位置为完整 `c_lflag` 比较，实际536872395、原始1483，差值为Darwin的 `PENDIN`。先执行的400ms回收断言通过。子进程先发布报告再进入canonical `read_line`，父进程读取termios与该读取清除PENDIN之间存在竞态。[该轮CI](https://github.com/Ariakage/saya-harness/actions/runs/42613791184)

独立真实PTY对照连续12次复现相同数字，空的非阻塞canonical read后全部termios字段与初始值一致。官方XNU在规范模式重入时设置PENDIN，后续输入处理清除此内核状态，与上述观测相符。[XNU规范模式状态转换](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/tty.c#L1358)、[输入状态处理](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/tty.c#L2073)

夹具在终端恢复后、报告发布前执行一次空非阻塞读取。每种读取结果均先恢复原文件标志，只有WouldBlock通过，EOF、实际字节及其他错误使夹具失败。完整termios比较、文件标志前后检查、400ms回收及原等待时限继续执行。新增文件屏障固定父进程在子进程 `read_line` 前观察；ECHO错误配置负对照验证空读取后仍由精确local flags断言拒绝。

受控移除发布前读取时，新屏障回归复现原标志位失败。修复后的全部10项input测试连续3轮通过，独立复核通过，记录位于 `target/n2/research/input-idle-fix/`。本机4线程完整门禁通过909项Rust测试、格式、workspace严格Clippy与优化构建，日志为 `target/n2/input-snapshot-workspace.log`。程序SHA仍为 `d2b0f84585ac0e66cf8953547bd9eac8b36cbd7e21b28024c0da469a8aa9479e`，与本机完整原生／MCP／TUI验收程序逐字节相同。该改动位于测试夹具，N2原生程序和实际验收基线继续归属上方 `aaec8c5`，修复提交对应的源码CI记录四平台复验结果。

## 已推送基础阶段

`21a3f6e` 的DEV-054／DEV-075四平台CI全部通过，包含Ubuntu22.04 ARM／x86_64和macOS15 ARM／Intel的完整检查、优化构建及既有正常入口脚本。该源码尚未包含原生后端与MCP的后续整合，证据为 `target/n2/dev075-four-platform-ci.json`。[GitHub Actions记录](https://github.com/Ariakage/saya-harness/actions/runs/42549418601)

## 研究证据

本机macOS27.2 ARM的26项Seatbelt探针通过，原始报告在 `target/n2/research/seatbelt-probe.json`。探针涵盖指定工作区、外部文件、保护路径和符号链接、子进程继承、IPv4／IPv6／Unix socket，以及仅允许指定本地代理端口。产品接线后需要对最终程序重新验收。

MCP采用官方Rust SDK的明确版本进行接口核实，原生后端与远程连接的依赖、协议、资源默认值和平台能力随实现记录。研究源文件保留于 `target/n2-research/`。

## 完成判据

每个DEV阶段在适用格式、静态检查、回归测试和文档同步后独立提交。最终验收应关联固定源码、构建环境、实际程序哈希和测试对象。Linux x86_64／aarch64与macOS15 Intel／ARM分别保留原生结果，编译和替身测试单独记录。

本轮已对相同固定源码的Markdown与TUI执行四平台同步检查，结果见[最终四平台验收](#最终四平台验收)。正式发行按单独授权处理。
