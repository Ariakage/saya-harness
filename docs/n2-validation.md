# N2 隔离与 MCP 验收

本轮目标为完整交付N2及对应验收。起点为 `547e5fe`，状态由[开发进度](progress.md)维护。以下表格列出需要满足的要求，开发中的代码和研究探针分别记录。

| 任务 | 必须具备的证据 | 当前状态 |
| --- | --- | --- |
| DEV-054 | 本地／外部工具注册、稳定名称、schema校验、宿主副作用分类、三权限与PLAN、版本审批绑定、取消与记录 | 本机阶段通过，752项Rust测试及完整门禁见DEV-054记录 |
| DEV-055 | prepare／execute／文件服务／cleanup的正常入口接线、真实能力等级、显式约束缺失时失败、实际后端上下文 | 开发中 |
| DEV-056 | Seatbelt与Bubblewrap／Seccomp／socat、受控网络broker、可用委派资源controller | 开发中 |
| DEV-075 | 真实shell CST、直接argv、包装器、重定向与动态行为、错误／预算、核查事实与批准版本绑定 | 本机阶段通过，773项Rust测试及CST探针见DEV-075记录 |
| DEV-057 | 路径与链接对抗、缺失保护路径、namespace／syscall、网络旁路、资源与清理故障、性能和恢复的原生证据 | 待执行 |
| DEV-058 | 显式stdio MCP配置、版本与生命周期、启动授权、受控进程与关闭 | 组件开发中 |
| DEV-059 | 工具schema／结果类型／大小、统一权限与记录、取消、列表变化、三模型协议的外部工具往返 | 待整合 |
| DEV-060 | 首个凭据后端、引用／撤销、最小化注入、日志／会话／机器输出的凭据边界 | 待实现 |
| DEV-061 | 固定SDK与协议矩阵、Streamable HTTP、OAuth与续期、断连不重放、额外输入及非交互路径 | 组件开发中 |

## DEV-055 原生执行组件

core已提供统一准备环境、能力等级、固定策略身份和清理接口，包含独立Seatbelt与Bubblewrap后端、Seccomp、私有launcher、文件worker和进程监督。六种文件工具复用统一审批与持久化入口，执行前保存意图并复查许可，异常退出保留未知状态及可确认的观测。文件RPC以实际请求／响应的32MiB作为传输上限，较大的处理限额允许用于小结果。

本机隔离源码快照完成格式、workspace全目标严格Clippy、803项Rust测试和优化构建，日志为 `target/n2/dev055-workspace-final2.log`。包含真实CLI文件worker、原生路径与网络拒绝、环境FD、资源缺失、取消与清理回归。本机为macOS27.2 ARM，部署目标15.0。初次环境FD测试的描述符号复用竞态已通过保留inode见证修正，原失败记录继续保留。

本阶段提交执行组件和显式 `ExecConfig.execution` 接线。正常CLI默认策略、运行期代理及MCP服务继续整合，DEV-055／056的完整完成状态依据最终正常入口与四平台验收更新。Linux CI通过固定SHA-256校验安装非setuid的Bubblewrap0.13.1及socat，后端缺失时明确失败。

## DEV-056 网络与原生约束组件

每Run网络broker支持HTTP absolute-form与CONNECT、精确origin、每连接DNS地址核查及固定目标地址、双栈loopback端口占有、解析／字节／连接／时间预算和关闭等待。受控真实TCP／Unix socket的18项broker回归包含请求走私、认证目标、DNS变化、取消、限额与监听器生命周期。

隔离源码快照完成格式、workspace严格Clippy、823项Rust测试与优化构建，日志为 `target/n2/dev056-workspace-final2.log`。macOS回归确认宿主进程参数查询被显式拒绝、私有launcher保留自身FD处理能力、标准根symlink可解析，代理仅允许TCP。正常CLI的UDP同端口及宿主进程查询对抗将随DEV-057重跑。

首次Linux原生CI已保留实际失败证据。原Seccomp阻止匿名socketpair，Tokio信号驱动初始化因此panic，原始日志在 `target/n2/research/dev055-direct-linux-{arm,x86}-ci.log`。修复仅放行AF_UNIX的已连接stream pair与必要flags，其余Unix socket创建保持受限，并补充真实Linux重定向／SCM_RIGHTS探针。候选通过两架构类型与严格静态检查，实际Linux修复结果继续由后续CI验证。

上述823项本机测试的操作系统为macOS27.2 ARM，Linux修复、委派cgroup和最终默认入口仍待相应原生验收。

## 已推送基础阶段

`21a3f6e` 的DEV-054／DEV-075四平台CI全部通过，包含Ubuntu22.04 ARM／x86_64和macOS15 ARM／Intel的完整检查、优化构建及既有正常入口脚本。该源码尚未包含原生后端与MCP的后续整合，证据为 `target/n2/dev075-four-platform-ci.json`。[GitHub Actions记录](https://github.com/Ariakage/saya-harness/actions/runs/42549418601)

## 研究证据

本机macOS27.2 ARM的26项Seatbelt探针通过，原始报告在 `target/n2/research/seatbelt-probe.json`。探针涵盖指定工作区、外部文件、保护路径和符号链接、子进程继承、IPv4／IPv6／Unix socket，以及仅允许指定本地代理端口。产品接线后需要对最终程序重新验收。

MCP采用官方Rust SDK的明确版本进行接口核实，原生后端与远程连接的依赖、协议、资源默认值和平台能力随实现记录。研究源文件保留于 `target/n2-research/`。

## 完成判据

每个DEV阶段在适用格式、静态检查、回归测试和文档同步后独立提交。最终验收应关联固定源码、构建环境、实际程序哈希和测试对象。Linux x86_64／aarch64与macOS15 Intel／ARM分别保留原生结果，编译和替身测试单独记录。

N2还需对本地开发线的Markdown与TUI执行四平台同步检查，沿用项目负责人此前约定。正式发行按单独授权处理。
