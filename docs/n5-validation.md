# N5 Browser 实现与验收

**2026-11-15：DEV-079 至 DEV-082 已提交；DEV-083 本机／专用 VM 联合阶段完成 21 passed、1 disabled，三协议 CLI／TUI 回归通过。Linux ARM 独立 VM 已通过完整原生 Rust 门禁、工具回归、CLI／TUI smoke 及宿主通道定向验收；这不是生产 Chromium 或四平台证据。生产截图、native Browser 及新增四平台证据仍未满足，DEV-083 全范围与 N5 不标为完成。**

任务范围来自[后续开发计划](post-mvp-plan.md#n5-browser)；实现与安全契约见 [Browser](browser.md)，对外范围见[中英双语说明](release-notes-n5.md)。

## 提交与范围

| 阶段 | 提交 | 已有实现与证据 |
| --- | --- | --- |
| DEV-079 | `5a61152` | BrowserProvider／BrowserState、受控 Web、操作描述、引用、权限与取消组件 |
| DEV-080 | `058bb9e` | 固定 worker、身份校验、生产装配、专用 VM、双层 sandbox 与生命周期 |
| DEV-081 网络阶段 | `e2587fe` | 共享 Run 网络预算、broker、反向 SSH、私有 netns、旁路 canary |
| DEV-081 凭据／文件／后台 | `f1bbc5c` | origin 凭据槽、下载／上传 artifact、GC／lease、取消与 unknown |
| DEV-082 | `1443291` | CLI／TUI 审批与产物、三协议图片、有效上下文媒体加载、来源保真与导出 |
| DEV-083 | 本次本机验收阶段 | 正常 CLI／TUI／专用 VM 联合矩阵通过；保留截图 disabled、native Browser 与新增四平台缺口，不标全范围完成 |

## 固定程序与本机门禁

宿主为 macOS 27.2 ARM，Rust 1.99.1，部署目标 15.0。固定 release 程序 SHA-256：

```text
86c08738d627d3b54314957675f82f1e23398fab212b4a02fe1410a9f82a0135
```

| 检查 | 实际结果 | 本机证据 |
| --- | --- | --- |
| `RUST_TEST_THREADS=1 just check` | 70 个测试二进制、1650 passed、0 failed、12 ignored；格式、严格 Clippy 和 release 构建通过 | `target/n5/dev082-signoff-check.log` |
| 独立 CLI PTY 回归 | 21 passed；此前并行压力下的终端等待超时未在串行复跑重现 | `target/n5/dev082-signoff-chat.log` |
| Python 工具回归 | 门禁当时 118 项通过，含工作区中的联合验收工具用例；工具后续增量单独核对 | `target/n5/dev082-signoff-tooling.log` |
| smoke／journey／审查回归／TUI | 通过，含 5 项审查回归及 28 组真实 TUI PTY | `target/n5/dev082-signoff-smoke.log` |
| 定向独立复核 | 来源、同页锁、有效媒体投影、图片计数、GC 与后台显示未发现新增实质缺陷 | 对应回归位于 core／CLI／TUI 测试 |

`ignored` 是显式条件入口，不计为已执行的平台测试。工具单测通过也不代表真实 VM 或 UI 旅程通过。

## 真实 Browser 后端

专用 Colima profile `saya-browser`，Linux ARM VM，宿主只使用严格 SSH 通道。显式 manifest 摘要：

```text
e6af5a229682028f9be237a96c1e1f4699292d08b3b5bce88822e56a3d679b46
```

固定版本为 `@playwright/mcp 0.0.85`、Playwright `1.66.0-alpha-2026-11-09`、Chromium `158.0.8127.18`（revision 1254）。这些身份约束不等于验证整个第三方安装树。

`target/n5/dev082-signoff-vm.log` 对同一源码实际重跑三个入口，各 1 passed、0 failed、0 ignored：

| 入口 | 断言与边界 |
| --- | --- |
| `browser_vm_real` | renderer 合法 sandbox argv、`NoNewPrivs=1`、`Seccomp=2`，真实 worker／Chromium 生命周期 |
| `browser_vm_network_real` | 只经 broker 出口、来源、redirect、内网／metadata 拒绝、浏览器实际 netns 内 TCP／UDP 正控制与旁路零命中、STUN 执行标记、取消与清理 |
| `browser_vm_files_real` | 合成凭据、origin／frame／stale／次数限制、实际文件字节与上传原名、后台 artifact 跨 GC、unknown 恢复、截图禁用拒绝及清理 |

运行后另行检查 `/tmp/saya-runs` 与 worker／Chromium／socat 进程，无残留。canary 端口与 broker 观察数量随运行变化，不固定为验收常数。

这些结果只覆盖显式预置 VM，不是 native Browser 或四平台证据。`stderr_initialize_sandbox=false` 是观测值，不作为成功断言；VM renderer 的证据与 native 后端所需的内层 sandbox 证据分别处理。

## 生命周期核验与启动清理增量

2026-11-14，provider 不再永久复用首次 renderer 的 sandbox 证据：成功页面 MCP 调用后重新采样，close／shutdown 作废旧证据。核验开始即隔离生命周期，核验失败、外层 timeout 丢弃 future 或清理失败时禁止继续派发；显式 close／shutdown 确认清理后才允许重建。该证据只表示最近一次完成的采样，不声称持续或全部 renderer 的瞬时保证。

启动进程在首次 catalogue await 前注册 connection／workspace，隔离覆盖初始导航和导航后的 catalogue 刷新，直到 sandbox 核验完成；协议／目录拒绝和外层丢弃不会遗失进程清理句柄。初始导航后目录或 sandbox 核验失败，且清理未确认时返回 `outcome_unknown`，不因 provider session 尚未建立而吞掉未知状态。新增启动回归分别先在连接复用、丢失 shutdown 重试和错误的已知失败断言上失败；证据保留为 `target/n5/fullscope-sandbox-startup-red.log`、`fullscope-sandbox-catalog-timeout-red.log` 和 `fullscope-sandbox-post-navigation-timeout-red.log`。

本轮运行时增量使用新的标准 release，SHA-256 为 `c1e20194d690efae1a9403a1c0892b62d78c146fa38c6da3365902305c2c7a91`，Mach-O 最低部署目标为 15.0；不复用本文历史固定程序的验收结论。

| 检查 | 实际结果 | 证据 |
| --- | --- | --- |
| worker 定向回归 | 48 passed、0 failed | `target/n5/fullscope-lifecycle-worker-green.log` |
| 完整 Rust 门禁 | 71 个测试二进制、1660 passed、0 failed、13 ignored，格式、严格 Clippy、release 构建通过 | `target/n5/fullscope-lifecycle-check.log` |
| Python 工具回归 | 11 个 suite、144 项通过 | `target/n5/fullscope-lifecycle-tooling.log` |
| 真实专用 VM | 生命周期、网络、凭据／文件／后台与遮蔽原语四入口各 1 passed、0 failed、0 ignored；运行前后程序摘要一致 | `target/n5/fullscope-lifecycle-vm.log` |
| CLI／TUI smoke | 离线 PLAN、文本会话、5 项审查回归及 28 组真实 TUI 旅程通过 | `target/n5/fullscope-lifecycle-smoke.log` |

真实生命周期入口实际执行 close／reopen／snapshot；本轮采样 renderer PID 分别为 133754、133857，均为 `NoNewPrivs=1`、`Seccomp=2` 且 argv 未禁用 sandbox，不把 PID 必须改变作为通过条件。前置独立空基线与最终独立清理检查分别保存在 `target/n5/fullscope-sandbox-final-inventory-after.log`、`target/n5/fullscope-lifecycle-inventory-after.log`，远端 Run 根及关联进程均为空。截图入口仍只验证合成页面原语，生产截图 facts 保持 false。此前 1659 项门禁及 `3f0b7bd4…` 程序的复跑日志继续保留，不冒充最终增量证据。

这一增量只在本地验证与提交；按项目负责人最新要求，不推送，新增四平台远端验收等待再次授权。N5 全范围仍未完成。

## Linux VM 宿主通道增量

2026-11-14，VM 宿主通道改为按 Auto 选择 macOS Seatbelt／Linux Bubblewrap；Linux 保留私有 netns、通用 Seccomp、匿名 pair 的内核身份／行为探测与固定 socat 镜像校验。`LoopbackPort` 增加精确 `host`，最多桥接 SSH 与本次 broker 两个固定目标；没有 Host、宿主 netns 或 IPv4 回退。账号解析只使用合成只读 passwd／group／NSS 文件，三项 SSH `read_files` 不扩大为目录授权。实现边界见 [Browser](browser.md#dev-080-vm-后端扩展) 与 [Sandbox](sandbox.md#linux-原生实现)。

离线 Linux 输入归档 `target/n5/linux-host-channel/source-native-green.tar` 的 SHA-256 为 `94fca3dbfafe3bc824b96fce5964254dbcb6d096b3e3eef23aa4945edafbdda4`，显式包含新增 relay 与 Linux 入口，不包含用户既有 `CONTRIBUTING.md` 或实际凭据。Ubuntu 22.04 ARM 用户空间使用 Rust 1.99.1、单并发、关闭 dev debug／incremental，在现有 Colima default 的 `6.8.0-117-generic` 内核中断网构建；这是受控 VM／容器证据，不是 Ubuntu 22.04 原生宿主四平台验收。固定 Linux dev 程序摘要为 `12ac0f03d1c586928c8989911df6d45c5a2c8c9a14f12a059aa21daacabbc770`。

| 检查 | 实际结果 | 本机证据 |
| --- | --- | --- |
| 受管 relay | 双目标转发、IPv6 活动连接关闭、部分启动失败、重复清理后的路径重用均通过 | `target/n5/linux-host-channel/relay-green-reuse.log` |
| Linux 启动协议与 prepare | 两条有界记录、无效／重复目标拒绝、私有通道 prepare 各 1 passed | `target/n5/linux-host-channel/native-green.log` |
| Linux payload 与真实 SSH | 两入口各 1 passed；均覆盖 `127.0.0.2` 与 `::1`。SSH 公钥认证成功、错误 host key 拒绝；payload 只达 SSH／broker，其他 TCP／UDP 无命中，命名 Unix 拒绝，精确文件只读且父目录内容隐藏，512 及以上故意设为可继承的宿主 UDP FD 未进入 payload | `target/n5/linux-host-channel/native-green.log` |
| Linux core 全量 | 同一测试程序在带 init 的夹具中 689 passed、0 failed、1 ignored；显式 prepare 入口另行运行，不把 ignored 算通过 | `target/n5/linux-host-channel/linux-core-init.log` |
| 非 root 原生通道 | 专用 VM 中 uid 501、CapEff=0、NoNewPrivs=1 的宿主进程执行两种地址的真实 payload 回归，1 passed；fixture 已删除 | `target/n5/linux-host-channel/native-nonroot-vm.log` |
| macOS 完整 Rust 门禁 | 1668 passed、0 failed、13 ignored，格式、workspace 全目标严格 Clippy 与 release 构建通过 | `target/n5/linux-host-channel/macos-check.log` |
| Python 工具回归 | 11 个 suite、144 项通过 | `target/n5/linux-host-channel/tooling.log` |

非 root 验证复用已预置专用 VM 的 namespace 前提，仅在私有 mount namespace 的临时 `/usr/local/bin` 视图安装固定 Bubblewrap 0.13.1，未改变 VM 全局工具或 sysctl。程序、测试入口和工具的转移前后摘要一致；宿主进程的 CapEff／NoNewPrivs 观测不是 Chromium sandbox 证据。

失败记录保留：初始 `prepare-red.log` 明确只支持 Seatbelt；`relay-red-final.log` 三项因 Unsupported 失败；`relay-reuse-red.log` 复现清理误删复用路径；`protocol-red-fixedenv.log` 复现未支持双桥记录；`native-red.log` 的 payload 因缺账号解析失败。SSH 夹具最初正控制失败不计产品红灯，改用私有账号目录后正控制通过，`nss-green-ssh-red.log` 随后才在 Linux preflight 平台拒绝处失败。缺 vendor 的 `protocol-red.log` 是准备失败，不是行为验收。

默认 Colima 的非 root 最小 `unshare`／Bubblewrap 正控制同样因 namespace 能力被拒绝，记录于 `rootless-preflight.log`／`native-rootless.log`，未修改其 `apparmor_restrict_unprivileged_userns=1`。无 init 的首轮 Linux core 有三项 orphan 清理失败，记录于 `linux-core.log`；增加实际 init 回收前提后，同一程序全量通过，不修改或放宽清理断言。原有容器外层的测试权限不作为产品隔离能力；产品缺少所需内核／LSM／工具条件时仍失败关闭。

### 初次平台门禁阻塞（历史输入）

系统卷空间耗尽时，本阶段没有标为完成，也没有创建新的阶段 commit 或推送。当时独立测量系统卷仅约 100 MiB 可用，Akari 工作区卷约 34 GiB；本会话可丢弃的临时输出仅约 1.6 MiB，不能据此恢复系统卷空间。没有删除归属不明的缓存、其他会话数据或共享 VM 磁盘，也没有重启／调整共享 Colima VM。

该轮 Linux 全 workspace 的源码输入归档为 `source-platform-final.tar`（SHA-256 `996302652b1dde41e1d81a5202452155a83cbb077a3897db728c819a844890ef`），不包含随后补强的源码。全部失败测试名称、失败 target 与分轮计数另存于 `target/n5/linux-host-channel/verification-summary.json`，保留原始日志，不把无法执行的后续入口算作成功。`linux-workspace-check.log`／`linux-workspace-final.log` 暴露仅供 macOS 使用的测试辅助类型与 AtomicBool 导入未做平台门控，已按实际使用范围修复，未关闭 lint。随后 `linux-platform-final.log` 的格式与全目标严格 Clippy 通过，重新构建的 dev 程序仍为 `12ac0f03…`；但 workspace 测试出现 `Input/output error (os error 5)`、临时 SQLite 数据不可读等失败，完整 Linux 门禁**没有通过**，不能以此前 core／定向入口成功替代。

macOS 中间轮 `macos-check-final.log` 的 `queued_compaction_at_the_last_execution_call_runs_after_completion_without_a_second_user_turn` 曾因 sandbox prepare 的 `Stale NFS file handle (os error 70)` 失败；随后 `macos-platform-final.log` 完整门禁通过，未放宽断言或增加隐式重试。该轮固定 `73df58171170d80ecc73f44d81671f25265c6c74f4a8770af5525d4af0ba2862` 程序通过 smoke／文本 journey、5 项审查回归及 28 组 TUI；真实 VM 四入口各 1 passed，前后独立 Run 根／关联进程均为空，程序摘要一致，日志为 `macos-smoke.log`／`macos-real-vm.log`。截图仍只验证原语，不是生产截图事务。

固定旧测试程序复现了三个真实 VM 入口在缺少显式输入时仍以成功返回；`entrypoints-baseline/missing-input-results.json` 记录 `browser_vm_real`、`browser_vm_network_real`、`browser_vm_files_real` 的 exit=0，而原本已严格检查输入的 `browser_screenshot_real` 为 exit=101。旧程序与失败记录保留；修复及最终源码的重验见下节，不把旧归档或旧测试程序当作后续增量的身份。

### 通道与真实验收断言补强

闲置临时项已按授权迁移到工作区私有归档；后续构建、日志与可控临时输出使用工作区 TMPDIR，不改变系统临时目录链接或全局配置。活动 IPC、工具运行目录及带 Git 元数据的目录保留，不能把迁移描述为删除整个 `/private/tmp`。

受管 relay 新增两项实际 IO 红绿回归：活跃期间替换 socket 路径后，清理按类型及 device／inode 核对，不直接删除替换物；HTTP broker 桥收到首字节后才连接上游，空就绪探测不会先消耗 `max_total_connections=1` 的唯一额度，SSH 服务端先发 banner 仍正常。旧实现确实在两个新断言处失败，修复后 6 项通过，证据为 `relay-review-red.log`／`relay-review-green.log`。

三个旧 VM 入口不再缺前提时成功返回：必须提供并固定 `SAYA_TEST_EXECUTABLE`，必需 VM 字段和端口严格解析，provider 装配失败也失败；不使用默认 release 路径，不按 macOS 之外的平台成功跳过。最新四个测试程序在 `entrypoints-shortpaths/harnesses.json` 记录程序与 Rust 源码 SHA-256，其源码摘要与真实 VM 运行一致；四入口各自的缺输入、缺 VM 设置、程序不存在、程序不可固定共 16 项负控均以 exit=101 在对应前提边界拒绝，实际错误和失败计数均经核对，证据为 `entrypoints-shortpaths/negative/results.json`。较早的 `entrypoints-final/` 冻结轮保留自己的程序身份。截图入口原本已严格拒绝缺前提，本次仅移除 macOS-only 限制。

凭据／文件入口先证明 fill 未自动提交，再显式点击 submit，核对 fixture 实收 `user=alice-secret`（合成测试值），保留 origin／frame／stale／次数拒绝。网络入口以 oneshot 确认慢请求实际到达后才取消，要求派发后的 `outcome_unknown`，不再以未 poll 的入口取消代替。清理扫描使用有界受控 SSH、结构化目录／进程清单和完成标记，不吞权限或 I/O 错误；新增不可列目录的默认回归先在旧扫描上失败，修复后通过，证据为 `cleanup-scan-red.log`／`cleanup-scan-green.log`。

| 最新本机检查 | 实际结果 | `target/n5/linux-host-channel/` 下证据 |
| --- | --- | --- |
| 完整 macOS Rust 门禁 | 1672 passed、0 failed、13 ignored；格式、全目标严格 Clippy、release 构建通过 | `shortpaths-macos-check.log` |
| Python 工具回归 | 11 个 suite、144 项通过 | `shortpaths-tooling.log` |
| CLI／TUI smoke | 离线 PLAN、文本 journey、5 项审查及 28 组 TUI 旅程通过 | `shortpaths-smoke.log` |
| 强化后的真实专用 VM | sandbox 生命周期、网络、凭据／文件／后台、遮蔽原语四入口各 1 passed、0 failed、0 ignored | `shortpaths-macos-real-and-joint.log` |
| 完整 Browser 联合矩阵 | 21 passed、1 disabled、0 failed；Execution 135、Review 8、Compact 1；11 组 PTY 退出无强制 SIGKILL | `shortpaths-browser-joint/run-7wcz2abb/evidence.json` |
| 独立清理与身份 | 前后 Run 根和关联进程均为空；四份测试源码与固定程序前后摘要一致 | `shortpaths-macos-vm-inventory-{before,after}.json`、`shortpaths-macos-vm-identity.json` |
| 前提负控 | 四个新固定测试程序的 16 项均以 exit=101 在预期边界拒绝 | `entrypoints-shortpaths/negative/results.json` |

本轮 release SHA-256 为 `d3c9e061cc5fed584fac42cb9ecd7c3513cf2505c9ab8ff7c6a91287f633583f`，Mach-O 最低部署目标 15.0、构建 SDK 27.0；程序由标准门禁构建，冻结后复跑真实 VM、完整联合矩阵与 smoke，最终再次核对身份。`final-macos-check.log`／`strict-*` 的 1671 项与 `73df5817…` 程序保留为较早输入，不用于替代本轮结果。最新源码归档与逐文件摘要另存于 `source-shortpaths-final.tar`／`source-shortpaths-final.json`，汇总为 `shortpaths-stage-signoff.json`；它们记录当前工作树，不伪称独立构建来源证明，也不复用旧归档摘要。上述完整 Browser 结果仍是 macOS ARM 宿主与显式 Linux ARM 专用 VM，不是 Linux 宿主或四平台 Browser 成功证据。

### Linux 交叉链接与定向实跑

共享 default Colima 的 image／overlayfs 读写仍出现 I/O 错误，`linux-sysroot-probe/copy.log` 记录只读复制 CRT 也因 overlay mount 失败；没有重启、调整或修复共享 VM。健康专用 VM 缺少 `/usr/lib/gcc/aarch64-linux-gnu`，无法直接导出完整 GNU linker 输入，失败记录为 `cross-tools/sysroot-list.log`。

最初 Zig linker 不支持必需 `--fix-cortex-a53-843419` 的失败保留在 `cross-tests.jsonl`／`cross-tests-errors.json`。最终继续用 Zig 编译 C，以 Clang 和 GNU-flavor LLD 链接，保留该 ARM 勘误参数；补入同一 Zig 工具链实际使用的 ARM64 UBSAN 与 compiler runtime，不关闭 C 检查。工作区私有 linker sysroot 来自 Ubuntu Jammy 官方索引，经独立公钥 GPG 验签及 Size／SHA-256 核对，仅提取 28 项所需输入，绝对库别名重定位为私有根内的相对链接；16 项离线工具回归通过，不执行包脚本或安装全局工具。最终 manifest 为 `apt-link-sysroot-20261114-4/manifest.json`（SHA-256 `6c1d9303acb411fde13b5b86c93c315f5820b26c53e57a1af5430b00dac6c846`）。最新 GNU target workspace 全目标严格 Clippy 与完整测试程序 `--no-run` 链接均通过，日志为 `shortpaths-linux-clippy.log`、`cross-tests-shortpaths.jsonl`／`.log`；离线 vendor、Cargo 配置和编译缓存均位于工作区。

第一次链接成功后的真实 prepare 暴露 Unix socket 路径超长，失败轮保留在 `cross-native-vm.log`，当时失败后也确认清理。新增 94 字节私有目录的实际转发回归在旧实现复现 `SUN_LEN` 错误；改为保留完整 UUID 位的 URL-safe 无填充 Base64 目录名及 `r0`／`r1` 短 socket 名，红绿证据为 `relay-longpath-{red,green}.log`。Linux prepare 入口在独立子进程中强制 64 字节 TMPDIR，不改父进程全局环境；broker canary 使用先发 HTTP 请求再读取响应的真实 client-first 协议，SSH 仍允许服务端先发。

| 最新 Linux 实际执行 | 结果 | `target/n5/linux-host-channel/` 下证据 |
| --- | --- | --- |
| relay、启动协议与长路径 prepare | 7＋1＋1 passed，0 failed、0 ignored | `shortpaths-native-vm.log` |
| root payload 与真实 SSH | 两入口各 1 passed，保留 `127.0.0.2`／`::1`、双目标和 TCP／UDP／Unix／高位 FD 负控；公钥认证成功、错误 host key 拒绝 | 同上 |
| uid 501 payload | CapEff=0、NoNewPrivs=1 的宿主进程实际执行，1 passed | 同上 |
| 身份与清理 | 共 12 passed；转移前后程序 SHA-256 一致、root 临时目录基线恢复、关联进程为空、fixture 已删除；独立 Browser inventory 前后为空 | `cross-native-shortpaths/{inputs,remote-identity-before,outcome}.json`、`cleanup.log`，`shortpaths-native-inventory-{before,after}.json` |

Linux dev 程序 SHA-256 为 `8a9f6b0eb735eeb0c4f3d01870aed55d9e53473c16ee6e9d24db8a916dc8a4a6`。实际执行环境为专用 VM 的 Ubuntu 24.04／glibc 2.39／ARM64／`6.8.0-117-generic`，不是 Ubuntu 22.04 原生宿主验收；Bubblewrap 0.13.1 只安装到本次私有 mount namespace 的临时视图，未改变全局工具、sshd 或 sysctl。该历史环境当时没有完整原生 Rust／C 编译工具链及编译期绝对 workspace 路径，上述定向阶段没有完成 Linux workspace 全量运行；后续独立环境的完整结果见下一节。不使用假编译器、隐藏失败入口或放宽清理断言凑成功，宿主进程属性也不作为 Chromium renderer 证据。

本阶段只交付已验证的宿主通道与真实验收加固增量，不提供生产捕获事务、Linux 侧完整 Chromium 联合矩阵或新增四平台证据；不改变截图 facts，N5 全范围仍未完成。所有推送与新增远端验收继续等待项目负责人再次授权。

## Linux ARM 完整原生门禁

2026-11-15，在工作区卷创建独立 Lima `n5arm22` VM：Ubuntu 22.04.5／glibc 2.35／ARM64，4 CPU、4 GiB RAM、24 GiB 虚拟磁盘；无宿主目录挂载、不转发 SSH agent，磁盘实际位于 `target/n5/vmf1/_lima/n5arm22/disk`。原有 Ubuntu 24.04 Browser Colima VM 未重启、调整或改全局工具。新环境使用真实 Rust 1.99.1、GCC 11.4、Just 1.59.0 与 Bubblewrap 0.13.1，Rust／C canary 均实际编译并执行；不是交叉链接或编译器替身。

源码基线为 `557f865a1b884b43d228bf48f591770899b21620`，源码归档 SHA-256 为 `6d42adf72f66aa98e09e7cc80cb71a86362565de354bafd34f20cdd98940b878`。新增的 `execution_backend.rs` 测试夹具补丁 SHA-256 为 `94356a9cb186bc7b2e1a411469422107bc4b774ab43da2567d2a570b042f140f`，仅让 Linux Browser 环境测试在独立进程中先固定真实 CLI，再准备并实际执行 payload；缺少程序或能力时不再成功跳过。生产 Rust 源码、权限和 Seccomp 规则未改。原生构建仅关闭 dev 调试信息与增量缓存、使用单编译／测试并发，release 优化配置未覆盖。完整 Git bundle 补齐归档不包含的真实 Git 元数据；没有伪造 revision、覆盖工作区文件或复制宿主 Git 配置。输入、Git tree 与实际工作树补丁分别记录于下表对应证据，仍不构成独立构建来源证明。

| 本轮检查 | 实际结果 | `target/n5/linux-full-20261115/` 下证据 |
| --- | --- | --- |
| Linux 完整 Rust 门禁 | 1686 passed、0 failed、17 ignored；格式、全目标严格 Clippy、标准 release 构建通过 | `logs/ubuntu22-arm-full-workspace-04.log` |
| Linux Python 工具回归 | 11 个 suite、144 项通过 | 同上 |
| Linux CLI／TUI smoke | PLAN 往返、会话 journey、5 项审查、28 组真实 TUI 旅程通过 | 同上，`evidence/ubuntu22-arm-native-workspace-result-04.json` |
| Linux 原生对抗／性能 | 18 passed、1 not_run（未提供 delegated cgroup）；性能 v2 的 21 项核对通过，每指标 3 次采样 | `evidence/ubuntu22-arm-native-acceptance-06.json`、`logs/ubuntu22-arm-native-acceptance-06.log` |
| Linux 宿主通道定向实跑 | relay 7、启动协议 1、64 字节 TMPDIR 准备 1、root payload 1、root SSH 1、uid 501／CapEff=0／NoNewPrivs=1 payload 1；合计 12 passed、0 failed、0 ignored；私有临时根和关联进程恢复基线 | `evidence/ubuntu22-arm-host-channel-07.json` |
| 同期 macOS 完整门禁 | 1672 passed、0 failed、13 ignored，含格式、严格 Clippy 与 release；工具 144 项及完整 CLI／TUI smoke 通过 | `logs/macos-fixture-{check,tooling,smoke}-03.log` |
| 输入与程序身份 | 基线、测试补丁、环境／配方逐文件 SHA-256，真实 Git tree 与工作树一致；固定程序前后相同 | `evidence/ubuntu22-arm-source-inputs-03.json`、`ubuntu22-arm-git-source-04.json`、`ubuntu22-arm-workspace-runner-result-04.json` |

Linux release SHA-256 为 `9b1634ec7c3cb1b115dee6b9301959bd7e8b86b7574fd7c14ee02d4192660d30`，66,611,320 字节；已复制保全于 `frozen-linux/saya`。macOS release 仍为 `d3c9e061cc5fed584fac42cb9ecd7c3513cf2505c9ab8ff7c6a91287f633583f`，标准重建后与冻结程序相同。两端 `just check` 的 ignored 数量与单独显式入口分别记录，不把忽略、缺前提或 fixture 宿主属性计为 Chromium 成功。密封汇总 `evidence/linux-native-stage-signoff-20261115.json` 核对输入、脚本、日志及报告的逐文件 SHA-256；`stage_verified=true` 仅确认这一阶段，`n5_full_acceptance=false` 保留全部未满足范围。

保留的失败轮次与整改如下：

- 初始 Ubuntu `5.15.0-198-generic` 未满足已确认的内核修复身份门槛，完整 Rust 记录 1540 passed、146 failed、17 ignored；仅在新 VM 安装官方 HWE 后实际启动 `6.8.0-149-generic`，没有放宽 gate 或伪造 uname。第二轮仍有 4 个失败 target、47 项失败，原始日志均保留。
- `umask=0002` 使普通测试夹具产生组可写目录。内存、日志与 CLI 的三组同程序对照均为 0002 失败、0022 通过；只固定验收 Shell 的 umask，不修改生产权限拒绝。对照为 `evidence/ubuntu22-arm-umask-red-green.json`。
- 测试 harness 被误当成 payload launcher 时，实际返回 `Unrecognized option: 'env-fd'`。真实 CLI 提前 pin 与独立进程修正后，原失败测试和完整 workspace 均通过；红测为 `logs/ubuntu22-arm-browser-launcher-red.log`。
- 第三轮 Rust 与 release 已通过，但工具回归在编译器调用之前因归档缺少 Git 元数据失败，未执行 smoke；第四轮补入经过核验的真实 bundle 后整链通过。没有修改编译失败证据测试或伪造 Git 输出。
- 宿主通道编排的第 04 轮缺少显式 `--ignored` 被实际计数检查拒绝，第 05 轮错误地给 root 使用 uid501／0700 的 socket 祖先，第 06 轮切换用户时被 sudo 清除了测试程序变量。分别修正显式调用、按 UID 分离临时根及精确重建测试环境；未放宽命名空间的目录访问或继承任意宿主环境。第 07 轮全部实跑并核对清理，失败报告未改写为成功。

本轮完成 Linux ARM 的完整原生 workspace 门禁和这一阶段回归，不代表 native Chromium、生产截图、macOS 15 运行环境或四平台联合完成。截图 facts 保持 false，新增远端验收与推送仍未进行。

## DEV-083 联合矩阵

[真实入口工具](../scripts/n5/acceptance.py)只使用显式程序、本地模型与本地网页，不 provision、不下载、不访问付费服务。缺少 `SAYA_TEST_VM_*` 前提必须失败并留证，不能把未运行算为通过。本轮新增 Browser 场景使用受控 Chat Completions fixture；三协议正常入口回归单列，不扩称为三协议 Browser 联合实跑。

| 矩阵 | 当前状态 |
| --- | --- |
| CLI／TUI：PLAN × 三权限、auto 核查、Manual 批准／拒绝、bypass | 正常入口通过；11 组真实 PTY、48 帧逐调用审批，填充正文不出现在审批中，终端完整属性与 fd flags 恢复 |
| Browser：长快照、OutputRef 检索、文件、后台慢页、unknown 与恢复 | 正常 CLI／TUI 通过；全量分页到 EOF、下载实际字节／同 Session 上传、后台及前台取消、unknown 不重放与新快照恢复 |
| Browser × 插件 × 显式 Compact × Session 恢复 | 实际安装／激活／读取插件资源，Compact 仅摘要调用；同 Session 恢复后检索原 OutputRef，原始历史与工具配对不改写 |
| 三协议 N3 CLI／TUI 回归 | 修复夹具后重跑通过 34 条 CLI、15 条真实 TUI 旅程；CLI 实际请求 105 个工具、观察 102 个结果，Manual 等待单列 |
| 原生隔离与性能 | 固定程序记录 18 passed、1 not_run（未提供 Linux delegated cgroup）；性能 v2 全部 21 项核对通过，每指标 3 次采样 |
| Python 工具回归 | `just test-tooling` 144 项通过，含 36 项 N5 验收工具回归；日志 `target/n5/dev083-tooling-final.log` |
| 固定程序完整 smoke | 离线 smoke、文本 journey、5 项审查回归和 28 组 TUI 再次通过；日志 `target/n5/dev083-smoke-lifecycle.log` |
| 生产截图 | disabled；不计为截图验收通过 |

最终 Browser 报告为 `target/n5/dev083-browser-final/run-jfraywh6/evidence.json`：21 passed、1 disabled、0 failed，实际模型 fixture 请求为 Execution 135、Review 8、Compact 1，未使用付费模型。两条长快照链路各读取 6 页、45,261 字节，完整视图摘要与宿主一致，同时单独记录正文摘要及 200 个有序源标记。前后远端 Run 根／关联进程列表均为空，11 组 PTY 清理均正常退出且未强制 SIGKILL；SQLite integrity 为 `ok`，前台取消意图持久化为 `unknown`／`outcome_unknown`。另行严格 SSH 只读核对仍为空，日志 `target/n5/dev083-final-inventory.log`。

冻结记录为 `target/n5/dev083-browser-view-identity.json`，8 个验收脚本的前后 SHA-256 一致，程序前后均为上述固定值。验收报告的 workspace manifest 不是构建证明；程序由标准 `just release` 生成，日志 `target/n5/dev083-build.log`，对应 Rust 源码 `1443291511a294a141908633296e0d304f68f8bb`、Mach-O 最低部署目标 15.0。本阶段未修改 Rust 源码，曾通过标准构建入口重建并核对固定哈希；最终冻结矩阵期间未重建或替换程序。

修复夹具后的三协议复跑报告为 `target/n5/dev083-n3-cli-lifecycle.json`、`dev083-n3-tui-lifecycle.json`；此前的 `dev083-n3-cli.json`、`dev083-n3-tui.json` 作为早期复跑保留。原生与性能报告为 `target/n5/dev083-native-final.json`、`dev083-performance-final.json`，均保持固定程序身份；不以历史 N2／N3 四平台结果替代当前 N5 的平台证据。

逐项汇总与上述证据文件的 SHA-256 保存在 `target/n5/dev083-stage-signoff.json`；`stage_verified=true` 仅代表这一可验证的本机阶段，`n5_full_acceptance=false` 明确保留未满足范围。

首轮原生证据为 `target/n5/dev083-native-evidence.json` 与 `dev083-performance-verification.json`，明确记录当时工作区源码 manifest 而非伪造固定构建来源。性能报告覆盖实际采样、分位数、HTTP／CONNECT 与 CLI 进程资源；不建立性能 SLO，也不测量并发 Run 的同时总内存。

固定提交的采样摘要如下，单位为毫秒，每指标 3 次，分位数用 nearest-rank；此时 P95 即样本最大值，不代表统计稳定的尾延迟。采样未隔离同机其它验收负载，不能据小差异推断性能提升；冷启动指新 CLI 进程，保留 OS 页缓存。warm command 是同一环境内从模型响应发出到下一工具结果请求到达的区间，其它表中主指标包含相应正常 CLI 入口成本。

| 指标 | Host P50／P95 | Seatbelt P50／P95 |
| --- | --- | --- |
| 冷启动 | 40.38／49.78 | 89.19／89.19 |
| 同 Run 暖命令 | 82.16／82.65 | 88.01／90.23 |
| 短命令 | 134.19／137.28 | 622.89／676.70 |
| 文件写入 | 541.59／577.37 | 600.12／698.63 |
| 文本检索 | 539.99／549.41 | 520.34／552.30 |
| HTTP RTT | 0.887／0.911（直连） | 1.692／1.697（broker） |
| CONNECT RTT | 同上直连基线 | 1.507／1.530（broker） |

### 失败轮次与判据整改

- 普通 `cargo build` 未继承项目部署目标，产物哈希变化后被候选身份校验拒绝，实际旅程数为 0。拒绝证据保留为 `target/n5/dev083-n3-cli-identity-rejected.json`；改用标准构建入口后恢复原哈希与 15.0 部署目标，没有修改预期哈希放行。
- Browser 首轮报告 `target/n5/dev083-browser/run-pgv61cvb/evidence.json` 为 6 passed、16 failed、1 disabled。800 段页面触及 512 元素上限，宿主正确返回 `truncated`／`limit_reached`／`retained_ranges_only`，但脚本错误要求完整；未清除的历史 fixture 错误又污染后续独立场景。这些失败不等于 16 个独立运行时缺陷。
- 长后台 job ID 在真实面板中换行，脚本不能要求单行完整 ID；面板打开时必须先关闭，再通过命令执行取消。前台工具可见性应等待真实 UI 帧，不以 HTTP 命中瞬间的旧画面判断。
- 失败夹具强制结束 TUI 后留下两个无进程的 Run 目录，已仅对本次已知目录调用生产清理路径并保留 `target/n5/dev083-fixture-failure-cleanup-final.log`。强制退出不作为正常 Run 清理通过证据，修正后的矩阵需独立证明正常取消与退出。
- canonical CLI 不复用 TUI 的 raw-mode 预热，完整 `tcgetattr`／`F_GETFL` 基线在候选进程启动前捕获，退出时不掩码 PENDIN 或重采样。真实故障程序在启动时关闭 ECHO，负向回归先失败、修复后通过，防止把错误的启动状态当作基线。
- 冻结轮 `target/n5/dev083-browser-final/run-sgqb_t36/evidence.json` 为 19 passed、3 failed、1 disabled，脚本和程序前后摘要一致，远端前后均为空。新增长快照全量分页校验错误地把密封记录摘要当作正文摘要，导致两条旅程与全局夹具断言失败；实际持久化的 45,261 字节、200 个有序标记均完整，按记录格式重建的视图摘要与宿主一致。验收工具现分别核对完整视图摘要并记录正文摘要，不修改运行时或放宽完整性要求。
- 持续 PTY 读取失败与父进程退出后残留进程组均以真实受控子进程复现并补修；清理只作用于夹具自己创建的进程。相关失败仍判为失败，不能以强制回收冒充正常退出。独立回收探针见 `target/n5/dev083-process-cleanup-probe.log`。

上述诊断轮保持失败记录；最终修正轮为 `run-jfraywh6`，只有冻结后的完整实跑结果计入本机联合阶段通过。

## 截图与媒体边界

### 真实遮蔽原语调查（尚未生产接入）

新增[专用 VM 像素回归](../crates/saya-core/tests/browser_screenshot_real.rs)，仅使用合成字段与 `about:blank`。实际固定 Chromium 返回 `HeadlessChrome/158.0.8127.18`、revision `6449d9f9a6a906edff724b7b220199b3fde4615c`。在已确认的双层 sandbox 内，暂停 renderer 后通过原生 DOM/CSS 遍历并隐藏普通、密码和 closed-shadow 输入；宿主独立解码实际 PNG，逐点核对三处敏感区域为白色、非敏感绿色区域仍保留，同时核对暂停期间计时不变、原始 style 精确恢复后计时继续。PNG 解码依赖仅用于测试。

显式入口实际为 1 passed、0 failed、0 ignored，日志 `target/n5/fullscope-primitives-real-final.log`；此前原语轮次及独立空 inventory 分别保留在 `target/n5/fullscope-cdp-real.log`、`target/n5/fullscope-primitives-inventory.log`。该入口在默认 Rust 门禁中仍为 ignored，缺少显式 VM 输入或平台不符不得返回通过。单 renderer 的特征回归不证明流式文档、导航提交、新 target、OOPIF 或取消恢复的生产事务屏障，不能据此开启截图 facts 或计为 N5 截图验收完成。

这一测试增量的最终 `RUST_TEST_THREADS=1 just check` 通过 71 个测试二进制、1650 passed、0 failed、13 ignored，含格式、严格 Clippy 与 release 构建，日志 `target/n5/fullscope-primitives-check-final.log`；`just test-tooling` 144 项通过，日志 `target/n5/fullscope-primitives-tooling.log`。新增的 ignored 入口已按上一段单独显式执行，不把其余 ignored 算作实跑。初次门禁中的常量平台断言被严格 Clippy 拒绝，已改用仍会在错误平台失败的 OS 断言；初始日志保留在 `target/n5/fullscope-primitives-check.log`。运行时源码未改，release 程序仍为本文固定 SHA-256。

### 生产边界

- 生产 worker 缺少截图前真实遮蔽契约，`browser_screenshot` 不注册；直接调用为 `browser_capability_disabled`，不保存未遮蔽图片。
- 受控 provider 已验证截图宿主成功路径、lease／GC／恢复和三协议原生图片块。它不是生产 worker 的像素级遮蔽证据。
- 开启 profile 图片能力时，只从有效请求历史选择宿主 Image 引用，最多 8 个，raw 与 base64 聚合各不超过 64 MiB；File 永不注入，Compact／Review 不扩展媒体。
- 截图 URL 使用 `last_confirmed` 语义。公开 artifact 句柄不授予跨 Session 权限；页面伪造引用不产生可信 GC pin 或宿主授权。

## 定制捕获引擎的构建前提

[D-068](decisions.md) 已确认允许扩展托管捕获引擎与固定身份契约，必要时采用定制 Chromium；它不是完成证明。[引擎实施边界](browser.md#托管捕获引擎扩展d-068待实现)保留预捕获遮蔽、事务屏障与双层 sandbox。固定源码的 Linux 构建文档要求 x86-64 构建机与至少 100 GB 空闲空间。2026-11-14 的约 31 GiB 余量保留为历史测量，不能当成后续清理后的现值；Linux ARM VM 的运行时验收也不证明标准 Linux 构建工具链支持 ARM 宿主。

### 固定宿主工具链与跨目标执行

2026-11-15，按唯一 `(device, inode)` 核对四个 Cargo profile 的可再生中间产物，并在 `target/` 内定位全部相关硬链接。实际回收 169,521 个 inode、525,390 个路径，候选唯一分配量为 64,388,321,280 字节；回收期间工作区可用量由 41,713,999,872 升至 106,195,374,080 字节。后者是当时的可用空间实测变化，包含并行 I/O 与文件系统影响，不把 `st_blocks` 计量直接当作空间承诺。四个 `.cargo-lock` 均以非阻塞独占锁持有，重新检查未见 Cargo／rustc 或打开的候选文件；分析器仍运行，不宣称取得所有用户进程的未来使用保证。841 个未知或保护 inode 未提升为可回收项；全部程序、动态库、源码、日志、证据和用户 `CONTRIBUTING.md` 保留。149 个受保护程序路径（81 个唯一 inode）的前后摘要一致；已重建的当前 `execution_backend` 与历史冻结副本分别核对，不要求二者冒充同一程序。证据为 `target/n5/engine-cache-inventory/reclaim-applied-01/result.json`。

从固定 Chromium `6449d9f9a6a906edff724b7b220199b3fde4615c` 的 DEPS 绑定官方对象、generation、精确大小与完整 SHA-256，实际取得 Mac ARM64 Clang／LLD 24、同修订的 Rust 工具链、Linux ARM64 sysroot 和 Linux 编译运行库，共 324,108,436 字节。大请求超时与不完整响应保留；续传使用有界范围、核对 `Content-Range`，拼合后仍要求完整固定摘要一致。下载与解包各使用工作区内独立数据目录，未执行归档内安装脚本；工具、sysroot 的文件／链接清单与摘要在 `target/n5/capture-toolchain-20261115/evidence/`。POSIX sysroot 中 systemd 单元名的字面反斜线不作为路径分隔符，仍保留绝对路径、`..`、特殊文件、越界链接、权限与资源上限检查。

固定宿主 Clang／LLD 24 与 Rust 1.101.0-nightly 已实际启动；项目自身仍使用 Rust 1.99.1。独立 C++／Rust ABI 探针经 `aarch64-linux-gnu` 目标与固定 sysroot、运行库链接，保留 `--fix-cortex-a53-843419`；ELF SHA-256 为 `0008bb6a83b52960ac93dfcb97903d889844a54f87fb0b68b0db782ad2c091d9`。同一字节在独立 Ubuntu 22.04 ARM VM 的 uid501、`6.8.0-149-generic` 内核上实跑，输出 `SAYA_CAPTURE_TOOLCHAIN_ARM64_OK`，远端临时程序与目录已清理。Rust 部分明确使用 `no_core`，**不证明目标 std 或 Chromium 链接成功**；固定 GN 配置的目标 std 从源码构建，不能把宿主包缺少预编译 Linux std 误判为默认阻断。完整报告为 `target/n5/capture-toolchain-20261115/evidence/standalone-canary-06.json`。

构建写入约束的真实正负控制已通过：允许两个任务目录内写入，拒绝修改目录外的、由当前用户拥有且模式为 0600 的可写对照文件。后续构建使用任务私有 HOME／TMPDIR／缓存与独立写入限制，不仅依赖缓存环境变量；该对照不是 CIPD、vpython 或完整构建已经通过的证明。固定 depot_tools `b86fc981b72ab966f8260f73b4eca600b3b12677` 已取得；完整源码展开、GN 生成、目标 std 与 headless_shell 的端到端构建仍不属于上述探针成功范围。

尚未运行或部署定制 Chromium，未实现生产捕获事务或身份 v2。本阶段已重新核对四份完整归档、25,515 个文件／链接条目与实际 ELF，密封汇总为 `target/n5/capture-toolchain-20261115/evidence/toolchain-stage-signoff.json`；汇总明确 `target_stdlib_verified`、`gn_generation_verified`、`chromium_build_verified`、`production_capture_verified` 与 `n5_full_acceptance` 均为 false。生产截图继续禁用；四平台远端验收按项目负责人最新要求等待再次授权，不以工具链探针、TCG 可行性调查或交叉产物代替实际平台成功证据。

### 固定 bootstrap 与 GN

2026-11-15，固定 depot_tools `b86fc981b72ab966f8260f73b4eca600b3b12677` 的原生 CIPD 客户端 2.9.2（SHA-256 `4ac2fa37f23486d249a2d71ff5084b6207c00cfa55928dad577fe99f0376ab71`）完成 15 个 Mac ARM bootstrap 包的正式部署。包共 268,172,212 字节，逐包核对 instance、完整摘要、ZIP 路径与展开大小，并由原客户端 `pkg-inspect`／`pkg-deploy` 验证；完整离线 `ensure` 通过，固定入口的 Python 为 3.11.8。实际执行了 gclient 入口；其 `--version` 输出为帮助，不把它记作数字版本，身份仍由固定提交决定。报告为 `target/n5/capture-toolchain-20261115/evidence/{cipd-packages-17,depot-deploy-18}.json`。

同一 DEPS 的 `gn/gn/mac-arm64` 已通过原客户端正式获取、检查与部署；instance 为 `phtG5eQKjHsVLHUJ3vtBlBU6H_pRwldzL4kqKWh1LOkC`，1,087,354 字节的包 SHA-256 为 `a61b46e5e40a8c7b152c7509defb4194153a1ffa51c257732f892a2968752ce9`，实际 `gn --version` 为 `2603 (938087829fcb)`。已用该程序格式化捕获草案的 GN 文件；**未执行 GN 生成或定制 headless_shell 编译**。原始报告为 `evidence/gn-input-23.json`，重新核对全部 bootstrap 包、客户端、depot_tools 提交及实际 GN 版本的汇总为 `evidence/bootstrap-stage-signoff-28.json`，两者均位于上述工具链目录。

完整 Chromium 工作树仍在补齐；逐批 Git 传输超时和官方固定快照在 1200 秒仅取得 23,363,584 字节的失败均保留。快照的 `download_complete` 为 false，未调用导入器，不将部分归档当成完整源码。原生捕获的 C++／Mojo／PDL 草案仅保留于工作区 `target/`，尚未编译、部署或提交为生产实现；既有引擎的六项显式原生用例均在缺少私有命令处失败，不能证明草案正确。生产截图 facts、身份 v1 和已发布能力均未改变。此阶段的成功仅限固定 bootstrap 与 GN；N5 全范围仍未完成，未推送或新增远端验收。

## 复跑入口

先按 [Browser 配置与 VM 前提](browser.md)显式设置全部 `SAYA_TEST_VM_*` 输入，程序身份与源码归属另行保留。

```bash
python3 -I -B scripts/n5/acceptance.py target/release/saya --evidence-dir target/n5-acceptance
```

要求真实生产截图时必须严格运行；当前固定 worker 预期不能满足该要求：

```bash
python3 -I -B scripts/n5/acceptance.py target/release/saya --evidence-dir target/n5-acceptance-screenshot --require-screenshot
```

```bash
python3 -B scripts/n2/native_acceptance.py target/release/saya --benchmark-samples 3 --output target/n5/native-evidence.json
```

```bash
python3 -B scripts/n2/verify_performance.py target/n5/native-evidence.json --samples 3
```

## 尚未满足的退出条件

1. 固定生产 Browser 的可靠预捕获遮蔽及真实截图成功证据。
2. 当前 N5 源码的 native Browser 与新增四平台固定源码联合／发行证据；既有 N2／N3 四平台报告不得移作本轮证据。

本轮未推送、未创建发行，Browser 验收只访问本地网页 fixture，未使用真实外网页面或付费模型；构建准备中的官方依赖下载不算 Browser 外网验收。版本号未变，完成声明以实际证据为准，不以模型文字或安全拒绝代替尚缺功能。
