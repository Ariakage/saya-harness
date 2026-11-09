# N2 隔离与 MCP 验收

本轮目标为完整交付N2及对应验收。起点为 `547e5fe`，状态由[开发进度](progress.md)维护。以下表格列出需要满足的要求，开发中的代码和研究探针分别记录。

| 任务 | 必须具备的证据 | 当前状态 |
| --- | --- | --- |
| DEV-054 | 本地／外部工具注册、稳定名称、schema校验、宿主副作用分类、三权限与PLAN、版本审批绑定、取消与记录 | 本机阶段通过，752项Rust测试及完整门禁见DEV-054记录 |
| DEV-055 | prepare／execute／文件服务／cleanup的正常入口接线、真实能力等级、显式约束缺失时失败、实际后端上下文 | 本机正常入口通过，最终四平台待验收 |
| DEV-056 | Seatbelt与Bubblewrap／Seccomp／socat、受控网络broker、可用委派资源controller | 本机原生验收通过，Linux委派controller待验收 |
| DEV-075 | 真实shell CST、直接argv、包装器、重定向与动态行为、错误／预算、核查事实与批准版本绑定 | 本机阶段通过，773项Rust测试及CST探针见DEV-075记录 |
| DEV-057 | 路径与链接对抗、缺失保护路径、namespace／syscall、网络旁路、资源与清理故障、性能和恢复的原生证据 | 本机18项与性能通过，最终四平台待验收 |
| DEV-058 | 显式stdio MCP配置、版本与生命周期、启动授权、受控进程与关闭 | 连接组件四平台CI通过，最终整体验收待完成 |
| DEV-059 | 工具schema／结果类型／大小、统一权限与记录、取消、列表变化、三模型协议的外部工具往返 | 正常运行时与CLI／TUI已整合，最终四平台待验收 |
| DEV-060 | 首个凭据后端、引用／撤销、最小化注入、日志／会话／机器输出的凭据边界 | 管理入口与本机边界验证通过，最终四平台待验收 |
| DEV-061 | 固定SDK与协议矩阵、Streamable HTTP、OAuth与续期、断连不重放、额外输入及非交互路径 | 组件开发中 |

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

## 已推送基础阶段

`21a3f6e` 的DEV-054／DEV-075四平台CI全部通过，包含Ubuntu22.04 ARM／x86_64和macOS15 ARM／Intel的完整检查、优化构建及既有正常入口脚本。该源码尚未包含原生后端与MCP的后续整合，证据为 `target/n2/dev075-four-platform-ci.json`。[GitHub Actions记录](https://github.com/Ariakage/saya-harness/actions/runs/42549418601)

## 研究证据

本机macOS27.2 ARM的26项Seatbelt探针通过，原始报告在 `target/n2/research/seatbelt-probe.json`。探针涵盖指定工作区、外部文件、保护路径和符号链接、子进程继承、IPv4／IPv6／Unix socket，以及仅允许指定本地代理端口。产品接线后需要对最终程序重新验收。

MCP采用官方Rust SDK的明确版本进行接口核实，原生后端与远程连接的依赖、协议、资源默认值和平台能力随实现记录。研究源文件保留于 `target/n2-research/`。

## 完成判据

每个DEV阶段在适用格式、静态检查、回归测试和文档同步后独立提交。最终验收应关联固定源码、构建环境、实际程序哈希和测试对象。Linux x86_64／aarch64与macOS15 Intel／ARM分别保留原生结果，编译和替身测试单独记录。

N2还需对本地开发线的Markdown与TUI执行四平台同步检查，沿用项目负责人此前约定。正式发行按单独授权处理。
